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

/// Names that are not valid Unicode reach the protocol escaped as 0.3.4 sent them (`\udc80` on
/// Windows, `\xff` on POSIX: [`printable`]) in NodeInfo (name, extension) and tree.node paths,
/// never as U+FFFD; `path.forms` keeps U+FFFD in its text forms (the interface hands those to the
/// system and refuses such paths: an escaped one could name another item) and the stored bytes in
/// its URI.
#[tokio::test]
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
    assert_eq!(
        (status["files"].as_i64(), status["dirs"].as_i64()),
        (Some(3), Some(2))
    );
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
    let odd_ext = inner["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == shown(&tail).as_str())
        .unwrap()["ext"]
        .clone();
    assert_eq!(odd_ext, shown(&name).as_str());
    let node = c
        .ok("tree.node", json!({"scan_id": id, "node": folder}))
        .await;
    assert_eq!(node["path"], shown(odd_dir.as_os_str()).as_str());
    assert_eq!(node["node"]["name"], shown(&name).as_str());
    // what the interface hands the system to open or reveal: the text forms mark the name as not
    // Unicode (U+FFFD, refused there), the URI keeps the bytes
    let forms = c
        .ok("path.forms", json!({"scan_id": id, "node": folder}))
        .await;
    let raw = forms["raw"].as_str().unwrap();
    assert!(raw.contains('\u{FFFD}'), "{forms}");
    assert!(!raw.contains(&shown(&name)), "{forms}");
    let escaped = if cfg!(windows) {
        "x%ED%B2%80y"
    } else {
        "x%FFy"
    };
    assert!(forms["uri"].as_str().unwrap().ends_with(escaped), "{forms}");

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
            p["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("无法访问"))
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
    assert!(
        events.iter().any(|m| m.starts_with("开始扫描")),
        "{events:?}"
    );
    assert!(
        events.iter().any(|m| m.starts_with("硬扫描完成")),
        "{events:?}"
    );
    srv.shutdown();
}

/// The first root of `root_keys_os.json` (a lone surrogate on Windows, a byte that is not UTF-8 on
/// POSIX) and the key 0.3.4 gave it.
fn odd_root_and_key() -> (OsString, String) {
    let platform = if cfg!(windows) { "windows" } else { "posix" };
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sk-core/tests/data/v034")
        .join(platform)
        .join("root_keys_os.json");
    let cases: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    let case = &cases[0];
    let hex = case["roots_hex"][0].as_str().unwrap();
    let raw: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    #[cfg(windows)]
    let root = {
        use std::os::windows::ffi::OsStringExt;
        let units: Vec<u16> = raw
            .chunks(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        OsString::from_wide(&units)
    };
    #[cfg(unix)]
    let root = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(raw)
    };
    assert!(root.to_str().is_none(), "{root:?}");
    (root, case["key"].as_str().unwrap().to_owned())
}

/// 0.3.4 made snapshots of roots no text can hold (its roots came from the system as
/// `os.fsdecode` text), and named their tag file and auto-save folder with the key of that text.
/// Opened here, such a snapshot's root is the system's string itself, so the tag file and the
/// folder a save goes to are 0.3.4's, not those of a lossy string of the root.
#[tokio::test]
async fn a_snapshot_root_no_text_can_hold_keeps_its_0_3_4_key() {
    use sk_store::{Flags, Kind, Times};
    let (root, key) = odd_root_and_key();
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let store = sk_store::Store::new();
    {
        let mut app = store.appender();
        app.add_root(
            sk_store::names::encode_name(&root),
            Kind::Dir,
            Flags::NONE,
            Times::default(),
        )
        .unwrap();
    }
    let file = tmp.path().join("odd.sks");
    let mut stats = serde_json::Map::new();
    stats.insert("state".into(), json!("done"));
    let opts = sk_store::snapshot::SaveOptions {
        roots: vec![root.to_string_lossy().into_owned()],
        stats,
        ..Default::default()
    };
    sk_store::snapshot::save(&store.read(), &file, &opts).unwrap();

    let loaded = c
        .ok("snapshot.load", json!({"path": file.to_string_lossy()}))
        .await;
    let id = loaded["scan_id"].as_str().unwrap().to_owned();
    let h = srv.server.scans.get(&id).unwrap();
    assert_eq!(h.roots.read()[0].path.as_os_str(), root.as_os_str());
    let tags = h.ctx.tags.lock().file.clone().expect("tags.remember is on");
    assert_eq!(
        tags.file_name().and_then(OsStr::to_str),
        Some(format!("{key}.json").as_str())
    );
    let saved = c.ok("snapshot.save", json!({"scan_id": id})).await;
    let folder = Path::new(saved["path"].as_str().unwrap())
        .parent()
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .map(str::to_owned);
    assert_eq!(folder.as_deref(), Some(key.as_str()), "{saved}");
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
        .ok(
            "tree.children",
            json!({"scan_id": lid, "node": loaded_root}),
        )
        .await;
    assert_eq!(names(&a), names(&b));
    assert_eq!(names(&b).len(), 1, "{:?} for {name:?}", names(&b));
    // the folder's path, built from the stored bytes, is the real one in both trees
    let find = |s: &sk_server::scans::ScanHandle| {
        let nodes = s.store.read();
        nodes
            .children(nodes.roots().next().unwrap())
            .find(|&k| nodes.path(k) == odd_dir)
            .map(|k| nodes.path(k))
    };
    assert_eq!(find(&h), Some(odd_dir.clone()), "live");
    assert_eq!(find(&lh), Some(odd_dir.clone()), "loaded");
    srv.shutdown();
}
