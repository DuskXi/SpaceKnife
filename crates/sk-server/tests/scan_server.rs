//! Port of tests/test_scan_server.py (the CLI parts belong to the cli task) and the server parts
//! of test_robust_scan_manager.py.

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use serde_json::json;
use sk_protocol::ErrorCode;

fn code(e: ErrorCode) -> i64 {
    i64::from(e.code())
}

/// Tests here change process-wide state (the snapshots folder, settings): one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn snapshots_root() -> std::path::PathBuf {
    init();
    sk_core::paths::snapshots_dir()
}

fn clear_snapshots() {
    let _ = std::fs::remove_dir_all(snapshots_root());
}

async fn scan_over_the_protocol(workers: u32) {
    let _g = SERIAL.lock().await;
    clear_snapshots();
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let started = c.ok("scan.start", json!({"roots": [facts.root], "workers": workers})).await;
    assert!(["queued", "scanning", "done"].contains(&started["state"].as_str().unwrap()));
    let id = started["scan_id"].as_str().unwrap();
    let status = wait_final(&c, id, true).await;
    assert_eq!(status["state"], "done");
    assert_eq!(status["files"], facts.files);
    assert_eq!(status["dirs"].as_i64().unwrap() - 1, facts.dirs);
    assert_eq!(status["bytes_size"], facts.size);
    assert!(Path::new(status["snapshot"].as_str().unwrap()).exists());

    let root = status["roots"][0]["node"].as_u64().unwrap();
    let children = c.ok("tree.children", json!({"scan_id": id, "node": root, "limit": 3})).await;
    // top level: a, c.txt, 中文 文件夹, deep, empty.dat, many, b
    assert_eq!(children["total"], 7);
    let allocs: Vec<i64> = children["items"].as_array().unwrap().iter().map(|i| i["alloc"].as_i64().unwrap()).collect();
    let mut sorted = allocs.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(allocs, sorted);
    assert_eq!(allocs.len(), 3);

    let movie = facts.root.join("a").join("sub").join("movie.mp4");
    let found = c.ok("tree.find", json!({"scan_id": id, "path": movie})).await;
    let node = &found["node"];
    assert_eq!(node["ext"], "mp4");
    assert_eq!(node["size"], 123456);
    let info = c.ok("tree.node", json!({"scan_id": id, "node": node["id"]})).await;
    assert_eq!(Path::new(info["path"].as_str().unwrap()), movie);
    assert_eq!(info["on_disk"], true);
    srv.shutdown();
}

#[tokio::test]
async fn scan_over_the_protocol_in_process() {
    scan_over_the_protocol(0).await;
}

#[tokio::test]
async fn scan_over_the_protocol_threads() {
    scan_over_the_protocol(2).await;
}

#[tokio::test]
async fn snapshot_list_and_load() {
    let _g = SERIAL.lock().await;
    clear_snapshots();
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let started = c.ok("scan.start", json!({"roots": [facts.root], "workers": 0})).await;
    let status = wait_final(&c, started["scan_id"].as_str().unwrap(), true).await;
    let listed = c.ok("snapshot.list", json!({})).await;
    let snaps = listed["snapshots"].as_array().unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0]["path"], status["snapshot"]);
    assert_eq!(snaps[0]["files"], facts.files);
    let loaded = c.ok("snapshot.load", json!({"path": status["snapshot"]})).await;
    assert_eq!(loaded["source"], "snapshot");
    assert_eq!(loaded["state"], "done");
    assert_ne!(loaded["scan_id"], started["scan_id"]);
    assert_eq!((loaded["files"].as_i64(), loaded["bytes_size"].as_i64()), (Some(facts.files), Some(facts.size)));
    // the next scan of the same folder knows how many items to expect (UI-W2-10, UI-W2-15)
    assert!(status["expected_items"].is_null() && status["progress"].is_null() && status["remaining_s"].is_null());
    let again = c.ok("scan.start", json!({"roots": [facts.root], "workers": 0})).await;
    assert_eq!(again["expected_items"], status["items"]);
    let done = wait_final(&c, again["scan_id"].as_str().unwrap(), true).await;
    assert_eq!(done["progress"], 1.0);
    assert!(done["rate"].as_f64().unwrap() > 0.0);
    // saved by hand somewhere else, and listed scans
    let target = tmp.path().join("hand.sks");
    let saved = c.ok("snapshot.save", json!({"scan_id": again["scan_id"], "path": target})).await;
    assert_eq!(saved["node_count"].as_i64().unwrap(), done["items"].as_i64().unwrap() + 1);
    assert!(target.exists());
    let list = c.ok("scan.list", json!({})).await;
    assert_eq!(list["scans"].as_array().unwrap().len(), 3);
    srv.shutdown();
}

#[tokio::test]
async fn close_and_errors() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let started = c.ok("scan.start", json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false})).await;
    let id = started["scan_id"].clone();
    c.ok("scan.close", json!({"scan_id": id})).await;
    assert_eq!(c.err("scan.status", json!({"scan_id": id})).await.code, code(ErrorCode::UnknownScan));
    assert_eq!(c.err("scan.close", json!({"scan_id": id})).await.code, code(ErrorCode::UnknownScan));
    let missing = tmp.path().join("missing");
    assert_eq!(c.err("scan.start", json!({"roots": [missing]})).await.code, code(ErrorCode::ScanFailed));
    let nope = tmp.path().join("nope.sks");
    assert_eq!(c.err("snapshot.load", json!({"path": nope})).await.code, code(ErrorCode::SnapshotError));
    srv.shutdown();
}

#[tokio::test]
async fn unknown_node() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let started = c.ok("scan.start", json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false})).await;
    let e = c.err("tree.node", json!({"scan_id": started["scan_id"], "node": 1_000_000_000})).await;
    assert_eq!(e.code, code(ErrorCode::UnknownNode));
    srv.shutdown();
}

#[tokio::test]
async fn volume_list() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let vols = c.ok("volume.list", json!({})).await;
    let vols = vols["volumes"].as_array().unwrap();
    assert!(!vols.is_empty());
    // unavailable drives are listed too, with a reason instead of a size (UI-W1-22)
    assert!(vols.iter().all(|v| v["total"].as_i64().unwrap() > 0 || !v["reason"].as_str().unwrap().is_empty()));
    assert!(vols.iter().any(|v| v["total"].as_i64().unwrap() > 0 && v["reason"] == ""));
}

#[tokio::test]
async fn check_paths_merges_nested_roots() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let file = facts.root.join("c.txt");
    let r = c
        .ok(
            "scan.check_paths",
            json!({"paths": [facts.root.join("a"), facts.root, tmp.path().join("missing"), file, "  "]}),
        )
        .await;
    let items = r["items"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    assert_eq!(items[0]["state"], "ok");
    assert_eq!(items[0]["merged_into"].as_str().map(Path::new), Some(facts.root.as_path()));
    assert_eq!(items[1]["state"], "ok");
    assert!(items[1]["merged_into"].is_null());
    assert_eq!(items[2]["state"], "missing");
    assert_eq!(items[3]["state"], "not_dir");
    assert_eq!(r["roots"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn scan_events_for_the_log_panel() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let started = c.ok("scan.start", json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false})).await;
    let id = started["scan_id"].as_str().unwrap().to_owned();
    wait_final(&c, &id, false).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let bus = sk_core::log::LogBus::global();
    let events: Vec<_> = bus
        .recent(5000, sk_protocol::streams::LogLevel::Trace)
        .into_iter()
        .filter(|e| e.extra.get("event").is_some_and(|v| !v.is_empty()) && e.extra.get("scan_id") == Some(&id))
        .collect();
    let first = events.first().expect("events of the scan");
    assert_eq!(first.extra["event"], "scan");
    assert!(first.message.starts_with("开始扫描 "), "{}", first.message);
    let last = events.last().unwrap();
    assert_eq!(last.extra["event"], "scan");
    assert!(last.message.starts_with("硬扫描完成："), "{}", last.message);
    assert_eq!(Path::new(&last.extra["path"]), facts.root.as_path());
    srv.shutdown();
}

#[tokio::test]
async fn tree_summary_of_a_folder() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let started = c.ok("scan.start", json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false})).await;
    let id = started["scan_id"].as_str().unwrap();
    let st = wait_final(&c, id, false).await;
    let root = st["roots"][0]["node"].clone();
    let s = c.ok("tree.summary", json!({"scan_id": id, "node": root, "top": 5})).await;
    let tops = s["top_files"].as_array().unwrap();
    assert_eq!(tops.len(), 5);
    assert_eq!(tops[0]["node"]["name"], "movie.mp4");
    assert_eq!(tops[0]["rel_parent"], format!("a{0}sub{0}", std::path::MAIN_SEPARATOR));
    let files: i64 = s["classes"].as_array().unwrap().iter().map(|c| c["files"].as_i64().unwrap()).sum();
    assert_eq!(files, facts.files);
    assert!(s["parent"].is_null() || s["parent"]["id"] == 0 || s["parent"].is_object());
    assert_eq!(s["on_disk"], true);
    srv.shutdown();
}

#[tokio::test]
async fn scan_topic_pushes_status_and_cancel_keeps_the_tree() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let started = c.ok("scan.start", json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false})).await;
    let id = started["scan_id"].as_str().unwrap().to_owned();
    let sub = c.subscribe("scan", json!({"scan_id": id})).await;
    // the current status at once
    let first = c.event("scan.progress", 5.0).await;
    assert_eq!(first["sub"], sub.as_str());
    assert_eq!(first["status"]["scan_id"], id.as_str());
    c.ok("scan.cancel", json!({"scan_id": id})).await;
    let fin = c
        .event_where("scan.progress", 30.0, |p| !["scanning", "queued"].contains(&p["status"]["state"].as_str().unwrap()))
        .await;
    assert!(["cancelled", "done"].contains(&fin["status"]["state"].as_str().unwrap()));
    // a scan topic for an unknown scan
    let e = c.err("$/subscribe", json!({"topic": "scan", "params": {"scan_id": "scan-nope"}})).await;
    assert_eq!(e.code, code(ErrorCode::UnknownScan));
    let e = c.err("$/subscribe", json!({"topic": "scan", "params": {}})).await;
    assert_eq!(e.code, code(ErrorCode::InvalidParams));
    srv.shutdown();
}

#[tokio::test]
async fn eviction_skips_scans_with_open_views_and_refuses_when_all_are_in_use() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    make_tree(tmp.path(), &[("x/f.bin", 10)]);
    let mut ids = Vec::new();
    for _ in 0..sk_server::scans::MAX_SCANS {
        let st = c.ok("scan.start", json!({"roots": [tmp.path()], "workers": 0, "auto_snapshot": false})).await;
        let id = st["scan_id"].as_str().unwrap().to_owned();
        wait_final(&c, &id, false).await;
        ids.push(id);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    // every scan has a view: no room
    for id in &ids {
        c.ok("view.open", json!({"scan_id": id, "viewport": {"width": 200, "height": 100, "dpr": 1}})).await;
    }
    let e = c.err("scan.start", json!({"roots": [tmp.path()], "workers": 0, "auto_snapshot": false})).await;
    assert_eq!(e.code, code(ErrorCode::ScanBusy));
    // one view closed: its scan is evicted for the new one
    let views = c.ok("view.list", json!({"scan_id": ids[3]})).await;
    let vid = views["views"][0]["view_id"].clone();
    c.ok("view.close", json!({"view_id": vid})).await;
    let st = c.ok("scan.start", json!({"roots": [tmp.path()], "workers": 0, "auto_snapshot": false})).await;
    assert!(st["scan_id"].is_string());
    assert_eq!(c.err("scan.status", json!({"scan_id": ids[3]})).await.code, code(ErrorCode::UnknownScan));
    c.ok("scan.status", json!({"scan_id": ids[0]})).await;
    srv.shutdown();
}

#[tokio::test]
async fn expected_items_ignore_partial_snapshots() {
    let _g = SERIAL.lock().await;
    clear_snapshots();
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let roots = vec![facts.root.to_string_lossy().into_owned()];
    assert_eq!(sk_server::scans::last_item_count(&roots), None);
    let started = c.ok("scan.start", json!({"roots": [facts.root], "workers": 0})).await;
    let st = wait_final(&c, started["scan_id"].as_str().unwrap(), true).await;
    assert_eq!(sk_server::scans::last_item_count(&roots), st["items"].as_i64());
    // a stopped scan saved by hand in the same folder is skipped
    let partial = c.ok("scan.start", json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false})).await;
    let pid = partial["scan_id"].as_str().unwrap().to_owned();
    c.ok("scan.cancel", json!({"scan_id": pid})).await;
    let fin = wait_final(&c, &pid, false).await;
    if fin["state"] == "cancelled" {
        // snapshot names have a one-second stamp: not the same file as the complete one
        tokio::time::sleep(Duration::from_millis(1100)).await;
        c.ok("snapshot.save", json!({"scan_id": pid})).await;
        assert_eq!(sk_server::scans::last_item_count(&roots), st["items"].as_i64());
        // and a loaded stopped snapshot keeps its state
        let list = c.ok("snapshot.list", json!({})).await;
        let newest = list["snapshots"][0]["path"].clone();
        let loaded = c.ok("snapshot.load", json!({"path": newest})).await;
        assert_eq!(loaded["state"], "cancelled");
        assert!(loaded["progress"].is_null());
    }
    srv.shutdown();
}

#[tokio::test]
async fn continue_and_rescan_errors() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let started = c.ok("scan.start", json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false})).await;
    let id = started["scan_id"].as_str().unwrap().to_owned();
    let st = wait_final(&c, &id, false).await;
    assert_eq!(st["state"], "done");
    tokio::time::sleep(Duration::from_millis(100)).await;
    // nothing left to continue
    assert_eq!(c.err("scan.continue", json!({"scan_id": id})).await.code, code(ErrorCode::InvalidRequest));
    // no disk load control without a governor; MFT only for whole NTFS volumes
    let e = c.err("scan.set_load", json!({"scan_id": id, "level": "quiet"})).await;
    assert_eq!(e.code, code(ErrorCode::InvalidRequest));
    assert_eq!(c.err("scan.use_mft", json!({"scan_id": id})).await.code, code(ErrorCode::InvalidRequest));
    // pause / resume of a finished scan answer with its status
    c.ok("scan.pause", json!({"scan_id": id})).await;
    c.ok("scan.resume", json!({"scan_id": id})).await;
    // rescanning a folder in place (F5) with a monitor or without
    std::fs::write(facts.root.join("a").join("new.bin"), vec![0u8; 4096]).unwrap();
    let a = c.ok("tree.find", json!({"scan_id": id, "path": facts.root.join("a")})).await;
    c.ok("scan.rescan", json!({"scan_id": id, "node": a["node"]["id"]})).await;
    let new = facts.root.join("a").join("new.bin");
    let mut found = false;
    for _ in 0..100 {
        let r = c.ok("tree.find", json!({"scan_id": id, "path": new})).await;
        if !r["node"].is_null() {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "the rescan found the new file");
    srv.shutdown();
}
