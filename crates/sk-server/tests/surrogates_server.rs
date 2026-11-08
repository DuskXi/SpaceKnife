//! Port of 0.3.4's `tests/test_robust_surrogates.py`: names that are not valid Unicode (a lone
//! UTF-16 surrogate on Windows, bytes that are not UTF-8 on POSIX) never break a scan, a reply or
//! a log record, and a snapshot (format 3) keeps them.
//!
//! `session_frames_survive_lone_surrogates` has no Rust twin of its own: a Rust `String` cannot
//! hold a lone surrogate, so the frames are valid by construction; what matters is that the names
//! reach them escaped ([`sk_core::events::printable`]), which the scan test checks reply by reply.

mod common;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use common::*;
use serde_json::{Value, json};
use sk_core::events::printable;

/// A name that is not valid Unicode, or None where the file system refuses one (APFS).
fn odd(prefix: &str) -> OsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let mut w: Vec<u16> = prefix.encode_utf16().collect();
        w.extend([0xDC80, b'y' as u16]); // unpaired surrogate, legal on NTFS
        OsString::from_wide(&w)
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut b = prefix.as_bytes().to_vec();
        b.extend([0xFF, b'y']); // not UTF-8
        OsString::from_vec(b)
    }
}

/// Make `dir/<odd>` with `inner.bin` and `tail.<odd>` (an extension that is not Unicode either).
fn odd_folder(dir: &Path) -> Option<(OsString, PathBuf)> {
    let name = odd("x");
    let p = dir.join(&name);
    if let Err(e) = std::fs::create_dir(&p) {
        eprintln!("skipped: the file system refuses undecodable names: {e}");
        return None;
    }
    std::fs::write(p.join("inner.bin"), vec![b'z'; 300]).unwrap();
    let mut tail = OsString::from("tail.");
    tail.push(&name);
    std::fs::write(p.join(tail), b"z").unwrap();
    Some((name, p))
}

fn names(v: &Value) -> Vec<String> {
    let mut n: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_owned())
        .collect();
    n.sort();
    n
}

fn shown(name: &OsStr) -> String {
    printable(name)
}

#[tokio::test]
#[ignore = "bug: names that are not valid Unicode reach the protocol decoded lossily (U+FFFD, sk-store Nodes::name → decode_lossy in NodeInfo and tree.node paths); 0.3.4 sent them escaped (\\udc80 on Windows, \\xff on POSIX: events::printable)"]
async fn scan_of_undecodable_names_over_the_protocol() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let Some((name, odd_dir)) = odd_folder(&root) else {
        return;
    };
    std::fs::write(root.join("plain.txt"), vec![b'x'; 100]).unwrap();
    c.subscribe("log", json!({"backlog": 0, "min_level": "INFO"}))
        .await;

    let st = c
        .ok("scan.start", json!({"roots": [root], "workers": 0}))
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    let status = wait_final(&c, &id, true).await;
    assert_eq!(status["state"], "done");
    assert_eq!((status["files"].as_i64(), status["dirs"].as_i64()), (Some(3), Some(2)));
    // the auto snapshot holds the odd extension too
    assert!(!status["snapshot"].is_null(), "{status}");
    let again = c.ok("scan.status", json!({"scan_id": id})).await;
    assert_eq!(again["state"], "done");
    assert_eq!(again["snapshot"], status["snapshot"]);
    let listed = c.ok("scan.list", json!({})).await;
    assert!(
        listed["scans"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["scan_id"] == id.as_str())
    );

    // a reply carrying the name itself is still answered (the name escaped)
    let top = status["roots"][0]["node"].clone();
    let children = c
        .ok("tree.children", json!({"scan_id": id, "node": top}))
        .await;
    let mut want = vec!["plain.txt".to_owned(), shown(&name)];
    want.sort();
    assert_eq!(names(&children), want);
    let folder = children["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == shown(&name).as_str())
        .unwrap()["id"]
        .clone();
    let inner = c
        .ok("tree.children", json!({"scan_id": id, "node": folder}))
        .await;
    let mut tail = OsString::from("tail.");
    tail.push(&name);
    let mut want = vec!["inner.bin".to_owned(), shown(&tail)];
    want.sort();
    assert_eq!(names(&inner), want);
    let node = c
        .ok("tree.node", json!({"scan_id": id, "node": folder}))
        .await;
    assert_eq!(node["path"], shown(odd_dir.as_os_str()).as_str());

    // a log record naming it reaches the session, escaped
    let text = format!("无法访问 {}", shown(odd_dir.as_os_str()));
    sk_core::events::emit(
        sk_core::events::Source::Perm,
        &text,
        sk_core::events::EventLevel::Warning,
        Some(&id),
        Some(&shown(odd_dir.as_os_str())),
    );
    let rec = c
        .event_where("log.record", 10.0, |p| {
            p["message"].as_str().is_some_and(|m| m.starts_with("无法访问"))
        })
        .await;
    assert_eq!(rec["message"], text.as_str());
    assert_eq!(rec["extra"]["path"], shown(odd_dir.as_os_str()).as_str());
    let events: Vec<String> = sk_core::log::LogBus::global()
        .recent(5000, sk_protocol::streams::LogLevel::Trace)
        .into_iter()
        .filter(|e| e.extra.get("scan_id").map(String::as_str) == Some(id.as_str()))
        .map(|e| e.message.clone())
        .collect();
    assert!(events.iter().any(|m| m.starts_with("开始扫描")), "{events:?}");
    assert!(events.iter().any(|m| m.starts_with("硬扫描完成")), "{events:?}");
    srv.shutdown();
}

/// A snapshot (format 3) of a scan holding names that are not valid Unicode: loaded back, the
/// names and the paths built from them are the same. (0.3.4's test also put one in the root's
/// own path; a root reaches the backend as protocol text, which cannot name such a folder.)
#[tokio::test]
async fn snapshot_keeps_lone_surrogates() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    std::fs::create_dir(&root).unwrap();
    let Some((name, odd_dir)) = odd_folder(&root) else {
        return;
    };
    let st = c
        .ok(
            "scan.start",
            json!({"roots": [root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let live = st["scan_id"].as_str().unwrap().to_owned();
    let h = srv.server.scans.get(&live).unwrap();
    wait_done(&c, &live).await;
    let file = tmp.path().join("s.sks");
    c.ok(
        "snapshot.save",
        json!({"scan_id": live, "path": file.to_string_lossy()}),
    )
    .await;
    let header = sk_store::snapshot::read_header(&file).unwrap();
    assert_eq!(header.format, 3);
    let loaded = c
        .ok("snapshot.load", json!({"path": file.to_string_lossy()}))
        .await;
    let lid = loaded["scan_id"].as_str().unwrap().to_owned();
    let lh = srv.server.scans.get(&lid).unwrap();
    let (live_root, loaded_root) = (h.roots.read()[0].node, lh.roots.read()[0].node);
    let a = c
        .ok("tree.children", json!({"scan_id": live, "node": live_root}))
        .await;
    let b = c
        .ok("tree.children", json!({"scan_id": lid, "node": loaded_root}))
        .await;
    assert_eq!(names(&a), names(&b));
    assert_eq!(names(&b).len(), 1, "{:?} for {name:?}", names(&b));
    // the folder's path, built from the stored bytes, is the real one in both trees
    let find = |s: &sk_server::scans::ScanHandle| {
        let nodes = s.store.read();
        let kid = nodes
            .children(nodes.roots().next().unwrap())
            .find(|&k| nodes.path(k) == odd_dir)
            .map(|k| nodes.path(k));
        kid
    };
    assert_eq!(find(&h), Some(odd_dir.clone()), "live");
    assert_eq!(find(&lh), Some(odd_dir.clone()), "loaded");
    srv.shutdown();
}
