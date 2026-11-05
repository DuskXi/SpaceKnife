//! Port of tests/test_start_page.py: start page (W1) backend — path checks, volume media and flags.

mod common;

use common::*;
use serde_json::json;
use sk_core::media::{classify, system_mount};
use sk_protocol::scan::{MediaName, PathCheckState};
use sk_server::handlers::scan::check_paths;

#[test]
fn check_paths_states_and_merging() {
    init();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("a").join("b")).unwrap();
    std::fs::create_dir(tmp.path().join("c")).unwrap();
    std::fs::write(tmp.path().join("f.txt"), "x").unwrap();
    let s = |p: std::path::PathBuf| p.to_string_lossy().into_owned();
    let (a, b, c) = (s(tmp.path().join("a")), s(tmp.path().join("a").join("b")), s(tmp.path().join("c")));
    let res = check_paths(&[
        format!(" {b} "),
        a.clone(),
        String::new(),
        s(tmp.path().join("f.txt")),
        s(tmp.path().join("nope")),
        c.clone(),
        a.clone(),
    ]);
    let states: Vec<(PathCheckState, Option<String>)> = res.items.iter().map(|i| (i.state, i.merged_into.clone())).collect();
    assert_eq!(
        states,
        [
            (PathCheckState::Ok, Some(a.clone())), // inside a: merged into it
            (PathCheckState::Ok, None),
            (PathCheckState::NotDir, None),
            (PathCheckState::Missing, None),
            (PathCheckState::Ok, None),
            (PathCheckState::Ok, Some(a.clone())), // duplicate
        ]
    );
    assert!(res.roots == [a.clone(), c.clone()] || res.roots == [c, a]);
    assert_eq!((res.items[0].path.as_str(), res.items[0].normalized.as_str()), (b.as_str(), b.as_str())); // trimmed
    assert!(!res.items[1].volume.is_empty()); // the volume holding the folder
    assert_eq!(res.items[3].message, "路径不存在");
    assert_eq!(res.items[2].message, "不是文件夹");
}

#[test]
fn media_classification() {
    assert_eq!(classify("nvme", Some(false), false), MediaName::NvmeSsd);
    assert_eq!(classify("sata", Some(false), false), MediaName::SataSsd);
    assert_eq!(classify("other", Some(true), false), MediaName::Hdd);
    assert_eq!(classify("usb", Some(true), false), MediaName::UsbHdd);
    assert_eq!(classify("usb", Some(false), true), MediaName::UsbFlash);
    assert_eq!(classify("usb", Some(false), false), MediaName::UsbSsd);
    assert_eq!(classify("sd", None, false), MediaName::SdCard);
    assert_eq!(classify("other", None, false), MediaName::Unknown);
    let m = system_mount();
    assert!(m.ends_with(std::path::MAIN_SEPARATOR) || m == "/");
}

#[tokio::test]
async fn volumes_and_checks_over_the_protocol() {
    let srv = server().await;
    let client = Client::open(&srv.url).await;
    let vols = client.ok("volume.list", json!({})).await;
    let vols = vols["volumes"].as_array().unwrap();
    assert!(!vols.is_empty());
    assert_eq!(vols.iter().filter(|v| v["is_system"] == true).count(), 1);
    assert!(vols.iter().all(|v| MediaName::from_str(v["media"].as_str().unwrap()).is_some()));
    let tmp = tempfile::tempdir().unwrap();
    let res = client
        .ok("scan.check_paths", json!({"paths": [tmp.path().to_string_lossy(), tmp.path().join("x").to_string_lossy()]}))
        .await;
    let states: Vec<&str> = res["items"].as_array().unwrap().iter().map(|i| i["state"].as_str().unwrap()).collect();
    assert_eq!(states, ["ok", "missing"]);
    assert_eq!(res["roots"], json!([tmp.path().to_string_lossy()]));
}
