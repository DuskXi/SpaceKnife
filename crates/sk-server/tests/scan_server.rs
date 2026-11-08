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
    let started = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": workers}),
        )
        .await;
    assert!(["queued", "scanning", "done"].contains(&started["state"].as_str().unwrap()));
    let id = started["scan_id"].as_str().unwrap();
    let status = wait_final(&c, id, true).await;
    assert_eq!(status["state"], "done");
    assert_eq!(status["files"], facts.files);
    assert_eq!(status["dirs"].as_i64().unwrap() - 1, facts.dirs);
    assert_eq!(status["bytes_size"], facts.size);
    assert!(Path::new(status["snapshot"].as_str().unwrap()).exists());

    let root = status["roots"][0]["node"].as_u64().unwrap();
    let children = c
        .ok(
            "tree.children",
            json!({"scan_id": id, "node": root, "limit": 3}),
        )
        .await;
    // top level: a, c.txt, 中文 文件夹, deep, empty.dat, many, b
    assert_eq!(children["total"], 7);
    let allocs: Vec<i64> = children["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["alloc"].as_i64().unwrap())
        .collect();
    let mut sorted = allocs.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(allocs, sorted);
    assert_eq!(allocs.len(), 3);

    let movie = facts.root.join("a").join("sub").join("movie.mp4");
    let found = c
        .ok("tree.find", json!({"scan_id": id, "path": movie}))
        .await;
    let node = &found["node"];
    assert_eq!(node["ext"], "mp4");
    assert_eq!(node["size"], 123456);
    let info = c
        .ok("tree.node", json!({"scan_id": id, "node": node["id"]}))
        .await;
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
    let started = c
        .ok("scan.start", json!({"roots": [facts.root], "workers": 0}))
        .await;
    let status = wait_final(&c, started["scan_id"].as_str().unwrap(), true).await;
    let listed = c.ok("snapshot.list", json!({})).await;
    let snaps = listed["snapshots"].as_array().unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0]["path"], status["snapshot"]);
    assert_eq!(snaps[0]["files"], facts.files);
    let loaded = c
        .ok("snapshot.load", json!({"path": status["snapshot"]}))
        .await;
    assert_eq!(loaded["source"], "snapshot");
    assert_eq!(loaded["state"], "done");
    assert_ne!(loaded["scan_id"], started["scan_id"]);
    assert_eq!(
        (loaded["files"].as_i64(), loaded["bytes_size"].as_i64()),
        (Some(facts.files), Some(facts.size))
    );
    // the next scan of the same folder knows how many items to expect (UI-W2-10, UI-W2-15)
    assert!(
        status["expected_items"].is_null()
            && status["progress"].is_null()
            && status["remaining_s"].is_null()
    );
    let again = c
        .ok("scan.start", json!({"roots": [facts.root], "workers": 0}))
        .await;
    assert_eq!(again["expected_items"], status["items"]);
    let done = wait_final(&c, again["scan_id"].as_str().unwrap(), true).await;
    assert_eq!(done["progress"], 1.0);
    assert!(done["rate"].as_f64().unwrap() > 0.0);
    // saved by hand somewhere else, and listed scans
    let target = tmp.path().join("hand.sks");
    let saved = c
        .ok(
            "snapshot.save",
            json!({"scan_id": again["scan_id"], "path": target}),
        )
        .await;
    assert_eq!(
        saved["node_count"].as_i64().unwrap(),
        done["items"].as_i64().unwrap() + 1
    );
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
    let started = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let id = started["scan_id"].clone();
    c.ok("scan.close", json!({"scan_id": id})).await;
    assert_eq!(
        c.err("scan.status", json!({"scan_id": id})).await.code,
        code(ErrorCode::UnknownScan)
    );
    assert_eq!(
        c.err("scan.close", json!({"scan_id": id})).await.code,
        code(ErrorCode::UnknownScan)
    );
    let missing = tmp.path().join("missing");
    assert_eq!(
        c.err("scan.start", json!({"roots": [missing]})).await.code,
        code(ErrorCode::ScanFailed)
    );
    let nope = tmp.path().join("nope.sks");
    assert_eq!(
        c.err("snapshot.load", json!({"path": nope})).await.code,
        code(ErrorCode::SnapshotError)
    );
    srv.shutdown();
}

#[tokio::test]
async fn unknown_node() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let started = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let e = c
        .err(
            "tree.node",
            json!({"scan_id": started["scan_id"], "node": 1_000_000_000}),
        )
        .await;
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
    assert!(
        vols.iter()
            .all(|v| v["total"].as_i64().unwrap() > 0 || !v["reason"].as_str().unwrap().is_empty())
    );
    assert!(
        vols.iter()
            .any(|v| v["total"].as_i64().unwrap() > 0 && v["reason"] == "")
    );
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
    assert_eq!(
        items[0]["merged_into"].as_str().map(Path::new),
        Some(facts.root.as_path())
    );
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
    let started = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let id = started["scan_id"].as_str().unwrap().to_owned();
    wait_final(&c, &id, false).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let bus = sk_core::log::LogBus::global();
    let events: Vec<_> = bus
        .recent(5000, sk_protocol::streams::LogLevel::Trace)
        .into_iter()
        .filter(|e| {
            e.extra.get("event").is_some_and(|v| !v.is_empty())
                && e.extra.get("scan_id") == Some(&id)
        })
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
    let started = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let id = started["scan_id"].as_str().unwrap();
    let st = wait_final(&c, id, false).await;
    let root = st["roots"][0]["node"].clone();
    let s = c
        .ok(
            "tree.summary",
            json!({"scan_id": id, "node": root, "top": 5}),
        )
        .await;
    let tops = s["top_files"].as_array().unwrap();
    assert_eq!(tops.len(), 5);
    assert_eq!(tops[0]["node"]["name"], "movie.mp4");
    assert_eq!(
        tops[0]["rel_parent"],
        format!("a{0}sub{0}", std::path::MAIN_SEPARATOR)
    );
    let files: i64 = s["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["files"].as_i64().unwrap())
        .sum();
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
    let started = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let id = started["scan_id"].as_str().unwrap().to_owned();
    let sub = c.subscribe("scan", json!({"scan_id": id})).await;
    // the current status at once
    let first = c.event("scan.progress", 5.0).await;
    assert_eq!(first["sub"], sub.as_str());
    assert_eq!(first["status"]["scan_id"], id.as_str());
    c.ok("scan.cancel", json!({"scan_id": id})).await;
    let fin = c
        .event_where("scan.progress", 30.0, |p| {
            !["scanning", "queued"].contains(&p["status"]["state"].as_str().unwrap())
        })
        .await;
    assert!(["cancelled", "done"].contains(&fin["status"]["state"].as_str().unwrap()));
    // a scan topic for an unknown scan
    let e = c
        .err(
            "$/subscribe",
            json!({"topic": "scan", "params": {"scan_id": "scan-nope"}}),
        )
        .await;
    assert_eq!(e.code, code(ErrorCode::UnknownScan));
    let e = c
        .err("$/subscribe", json!({"topic": "scan", "params": {}}))
        .await;
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
        let st = c
            .ok(
                "scan.start",
                json!({"roots": [tmp.path()], "workers": 0, "auto_snapshot": false}),
            )
            .await;
        let id = st["scan_id"].as_str().unwrap().to_owned();
        wait_final(&c, &id, false).await;
        ids.push(id);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    // every scan has a view: no room
    for id in &ids {
        c.ok(
            "view.open",
            json!({"scan_id": id, "viewport": {"width": 200, "height": 100, "dpr": 1}}),
        )
        .await;
    }
    let e = c
        .err(
            "scan.start",
            json!({"roots": [tmp.path()], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    assert_eq!(e.code, code(ErrorCode::ScanBusy));
    // one view closed: its scan is evicted for the new one
    let views = c.ok("view.list", json!({"scan_id": ids[3]})).await;
    let vid = views["views"][0]["view_id"].clone();
    c.ok("view.close", json!({"view_id": vid})).await;
    let st = c
        .ok(
            "scan.start",
            json!({"roots": [tmp.path()], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    assert!(st["scan_id"].is_string());
    assert_eq!(
        c.err("scan.status", json!({"scan_id": ids[3]})).await.code,
        code(ErrorCode::UnknownScan)
    );
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
    let started = c
        .ok("scan.start", json!({"roots": [facts.root], "workers": 0}))
        .await;
    let st = wait_final(&c, started["scan_id"].as_str().unwrap(), true).await;
    assert_eq!(
        sk_server::scans::last_item_count(&roots),
        st["items"].as_i64()
    );
    // a stopped scan saved by hand in the same folder is skipped
    let partial = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let pid = partial["scan_id"].as_str().unwrap().to_owned();
    c.ok("scan.cancel", json!({"scan_id": pid})).await;
    let fin = wait_final(&c, &pid, false).await;
    if fin["state"] == "cancelled" {
        // snapshot names have a one-second stamp: not the same file as the complete one
        tokio::time::sleep(Duration::from_millis(1100)).await;
        c.ok("snapshot.save", json!({"scan_id": pid})).await;
        assert_eq!(
            sk_server::scans::last_item_count(&roots),
            st["items"].as_i64()
        );
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
    let started = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let id = started["scan_id"].as_str().unwrap().to_owned();
    let st = wait_final(&c, &id, false).await;
    assert_eq!(st["state"], "done");
    tokio::time::sleep(Duration::from_millis(100)).await;
    // nothing left to continue
    assert_eq!(
        c.err("scan.continue", json!({"scan_id": id})).await.code,
        code(ErrorCode::InvalidRequest)
    );
    // no disk load control without a governor; MFT only for whole NTFS volumes
    let e = c
        .err("scan.set_load", json!({"scan_id": id, "level": "quiet"}))
        .await;
    assert_eq!(e.code, code(ErrorCode::InvalidRequest));
    assert_eq!(
        c.err("scan.use_mft", json!({"scan_id": id})).await.code,
        code(ErrorCode::InvalidRequest)
    );
    // pause / resume of a finished scan answer with its status
    c.ok("scan.pause", json!({"scan_id": id})).await;
    c.ok("scan.resume", json!({"scan_id": id})).await;
    // rescanning a folder in place (F5) with a monitor or without
    std::fs::write(facts.root.join("a").join("new.bin"), vec![0u8; 4096]).unwrap();
    let a = c
        .ok(
            "tree.find",
            json!({"scan_id": id, "path": facts.root.join("a")}),
        )
        .await;
    c.ok(
        "scan.rescan",
        json!({"scan_id": id, "node": a["node"]["id"]}),
    )
    .await;
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

#[tokio::test]
async fn a_panic_in_a_scan_fails_that_scan_and_the_server_goes_on() {
    // a bug while listing (a listing thread) or folding (the scan's own thread): that scan ends as
    // failed with the reason, instead of «scanning» for good; the next scan works
    use std::sync::{Arc, Mutex};
    let _g = SERIAL.lock().await;
    init();
    let plan: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let steps = plan.clone();
    let mut tuning = sk_server::app::Tuning::for_tests();
    tuning.scan_fault = Some(Arc::new(move |step: &str| {
        let mut p = steps.lock().unwrap();
        if p.first() == Some(&step) {
            p.remove(0);
            drop(p);
            panic!("injected {step} bug");
        }
    }));
    let srv = sk_server::start_with(
        sk_server::ServeOptions {
            token: TOKEN.to_owned(),
            ..sk_server::ServeOptions::default()
        },
        tuning,
    )
    .await
    .unwrap();
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    for step in ["list", "fold", ""] {
        if !step.is_empty() {
            plan.lock().unwrap().push(step);
        }
        let started = c
            .ok(
                "scan.start",
                json!({"roots": [facts.root], "workers": 2, "auto_snapshot": false}),
            )
            .await;
        let id = started["scan_id"].as_str().unwrap().to_owned();
        c.subscribe("scan", json!({"scan_id": id})).await;
        let want = id.clone();
        let ev = c
            .event_where("scan.progress", 20.0, move |p| {
                let s = &p["status"];
                s["scan_id"] == want.as_str() && s["state"] != "scanning" && s["state"] != "queued"
            })
            .await;
        let status = c.ok("scan.status", json!({"scan_id": id})).await;
        if step.is_empty() {
            assert_eq!(status["state"], "done", "{status}");
            assert_eq!(status["files"], facts.files);
        } else {
            assert_eq!(ev["status"]["state"], "failed", "{step}: {ev}");
            assert_eq!(status["state"], "failed", "{step}: {status}");
            let message = status["message"].as_str().unwrap_or("");
            assert!(
                message.contains(&format!("injected {step} bug")),
                "{step}: {status}"
            );
        }
        c.ok("scan.close", json!({"scan_id": id})).await;
    }
    assert!(plan.lock().unwrap().is_empty());
    srv.shutdown();
}

/// The state of the scan's monitor in a status (null: not watched).
fn monitor_state(status: &serde_json::Value) -> serde_json::Value {
    status["monitor"]["state"].clone()
}

#[tokio::test]
async fn a_watched_scan_says_so_from_its_final_status_on() {
    // the status bar reads live monitoring only from the `scan` topic's pushes: the final status
    // of a scan already names its running monitor, as in 0.3.4 (which started the watchers before
    // the scan), never null («live monitoring off») because the watchers were still starting
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let dir = sample_tree();
    let started = c
        .ok(
            "scan.start",
            json!({"roots": [dir.path()], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let id = started["scan_id"].as_str().unwrap().to_owned();
    let last = wait_final(&c, &id, false).await;
    assert_eq!(last["state"], "done", "{last}");
    assert_eq!(monitor_state(&last), "running", "{last}");
    let status = c.ok("scan.status", json!({"scan_id": id})).await;
    assert_eq!(monitor_state(&status), "running", "{status}");
    c.ok("scan.close", json!({"scan_id": id})).await;
    srv.shutdown();
}

#[tokio::test]
async fn applied_changes_and_the_monitor_switch_push_the_scan_status() {
    // 0.3.4 pushed scan.progress whenever the monitor applied changes (the status bar's counts and
    // its monitor state come from these pushes only); switching live monitoring off and on again
    // pushes too, so the status bar never keeps a state the scan left
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let dir = sample_tree();
    let started = c
        .ok(
            "scan.start",
            json!({"roots": [dir.path()], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let id = started["scan_id"].as_str().unwrap().to_owned();
    let done = wait_final(&c, &id, false).await;
    let files = done["files"].as_i64().unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    c.drain_events().await;
    std::fs::write(dir.path().join("live.bin"), vec![0u8; 7000]).unwrap();
    let want = id.clone();
    let ev = c
        .event_where("scan.progress", 20.0, move |p| {
            p["status"]["scan_id"] == want.as_str()
                && p["status"]["files"].as_i64() == Some(files + 1)
        })
        .await;
    assert_eq!(monitor_state(&ev["status"]), "running", "{ev}");
    assert!(
        ev["status"]["monitor"]["changes"].as_i64().unwrap() >= 1,
        "the counts are the monitor's own after the change: {ev}"
    );

    let set = |on: bool| {
        let c = &c;
        async move {
            let v = c.ok("settings.get", json!({})).await["version"].clone();
            c.ok(
                "settings.update",
                json!({"values": {"scan.monitor": on}, "version": v}),
            )
            .await;
        }
    };
    c.drain_events().await;
    set(false).await;
    let want = id.clone();
    c.event_where("scan.progress", 10.0, move |p| {
        p["status"]["scan_id"] == want.as_str() && p["status"]["monitor"].is_null()
    })
    .await;
    set(true).await;
    let want = id.clone();
    c.event_where("scan.progress", 10.0, move |p| {
        p["status"]["scan_id"] == want.as_str() && p["status"]["monitor"]["state"] == "running"
    })
    .await;
    c.ok("scan.close", json!({"scan_id": id})).await;
    srv.shutdown();
}

/// A disk sampler that knows one disk and takes `delay` to start (PDH, sysfs on a busy machine).
struct SlowSampler(sk_core::io::sampler::RefSet);

impl sk_core::io::DiskSampler for SlowSampler {
    fn name(&self) -> &'static str {
        "slow"
    }
    fn sample(
        &mut self,
    ) -> std::collections::HashMap<sk_core::io::DiskId, sk_core::io::DiskSample> {
        std::collections::HashMap::new()
    }
    fn refs(&self) -> Vec<sk_core::io::DiskRef> {
        self.0.refs()
    }
    fn disk_ref(&self, disk: &str) -> Option<sk_core::io::DiskRef> {
        self.0.get(disk)
    }
    fn watch(&mut self, r: sk_core::io::DiskRef) {
        self.0.watch(r)
    }
}

#[tokio::test]
async fn an_io_subscription_made_at_once_gets_the_disks_of_the_started_registry() {
    // disk sampling starts with the backend (on a blocking thread); an `io` subscription made
    // before it is through still gets the load of every disk it knows then, as in 0.3.4 (which had
    // started it before the first session), not nothing for good
    let srv = server_tuned(
        sk_server::ServeOptions {
            token: TOKEN.to_owned(),
            ..sk_server::ServeOptions::default()
        },
        |t| {
            t.io_sampling = true;
            t.io_sampler = Some(std::sync::Arc::new(|| {
                std::thread::sleep(Duration::from_millis(800));
                let mut refs = sk_core::io::sampler::RefSet::default();
                refs.watch(sk_core::io::DiskRef::physical("test-disk-0"));
                Box::new(SlowSampler(refs)) as Box<dyn sk_core::io::DiskSampler>
            }));
        },
    )
    .await;
    let c = Client::open(&srv.url).await;
    // (not `running()`: it waits for the start)
    assert!(
        srv.server.scans.io.governors().is_empty(),
        "subscribed before the sampler started"
    );
    c.subscribe("io", json!({})).await;
    let ev = c
        .event_where("io.load", 10.0, |p| p["status"]["disk"] == "test-disk-0")
        .await;
    assert!(ev["sub"].is_string(), "{ev}");
    srv.shutdown();
}

#[tokio::test]
async fn a_panic_while_the_helper_starts_leaves_the_hard_scan_going() {
    // scan.use_mft starts the helper on a thread of its own (the administrator prompt). A bug
    // there is that scan's error, told like a helper that would not start: the switch is not left
    // 芦starting the helper禄 for good (so it can be asked for again), and the hard scan goes on to
    // the end (0.3.4 cleared it in a `finally` and kept scanning)
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let _g = SERIAL.lock().await;
    init();
    // the listing threads wait here, so the scan is still running when the switch is asked for
    let hold = Arc::new(AtomicBool::new(true));
    let held = hold.clone();
    let srv = server_tuned(
        sk_server::ServeOptions {
            token: TOKEN.to_owned(),
            ..sk_server::ServeOptions::default()
        },
        move |t| {
            t.scan_fault = Some(Arc::new(move |step: &str| {
                while step == "list" && held.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }));
        },
    )
    .await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let started = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 2, "auto_snapshot": false}),
        )
        .await;
    let id = started["scan_id"].as_str().unwrap().to_owned();
    let h = srv.server.scans.get(&id).unwrap();
    for (attempt, why) in ["injected helper bug", "no helper here"]
        .into_iter()
        .enumerate()
    {
        let get = move || -> Result<_, sk_core::helper::HelperError> {
            if attempt == 0 {
                panic!("{why}");
            }
            Err(sk_core::helper::HelperError::error(why, "test"))
        };
        srv.server.scans.upgrade_with(&srv.server, &h, get).unwrap();
        let mut status = json!(null);
        for _ in 0..200 {
            status = c.ok("scan.status", json!({"scan_id": id})).await;
            if !h.st.lock().upgrading && status["phase"] != "helper" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(!h.st.lock().upgrading, "{why}: still starting the helper");
        assert_ne!(status["phase"], "helper", "{why}: {status}");
        assert_eq!(status["state"], "scanning", "{why}: {status}");
        assert_eq!(status["method"], "hard", "{why}: {status}");
        let fallback = status["fallback"].as_str().unwrap_or("");
        assert!(fallback.contains(why), "{why}: {status}");
    }
    hold.store(false, Ordering::SeqCst);
    let status = wait_done(&c, &id).await;
    assert_eq!(status["state"], "done", "{status}");
    assert_eq!(status["files"], facts.files);
    srv.shutdown();
}
