//! Port of 0.3.4's `tests/test_robust_view_push.py` `closing_the_view_drops_a_pending_final` (the
//! other two cases are view_server.rs `final_status_during_a_push_renders_the_final_frame_once`),
//! and the palette's 「显示/隐藏回收站」 needing a bin (`test_trash_view.py`
//! `the_palette_toggle_needs_a_bin`), both over the protocol.

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use sk_protocol::scan::ScanStateName;

async fn scanned(c: &Client, root: &std::path::Path) -> String {
    let st = c
        .ok(
            "scan.start",
            json!({"roots": [root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    wait_done(c, &id).await;
    id
}

async fn view(c: &Client, scan_id: &str) -> String {
    let v = c
        .ok(
            "view.open",
            json!({"scan_id": scan_id, "viewport": {"width": 800, "height": 600}}),
        )
        .await;
    v["state"]["view_id"].as_str().unwrap().to_owned()
}

/// The scan ends while a progress push waits for the view; the view is closed before the push
/// renders: no frame of the closed view is pushed after its close, neither that progress frame nor
/// the pending final one (0.3.4 cancelled the push).
#[tokio::test]
async fn closing_the_view_drops_a_pending_final() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let scan_id = scanned(&c, &facts.root).await;
    let view_id = view(&c, &scan_id).await;
    let host = srv.server.views.lookup(&view_id).unwrap();
    let server = srv.server.clone();
    tokio::time::sleep(Duration::from_millis(200)).await;
    c.drain_events().await;
    c.drain_binary().await;
    // hold the view: the progress push waits for it, the end of the scan arrives meanwhile
    let guard = host.lock.lock().await;
    server
        .views
        .on_scan(&server, &scan_id, ScanStateName::Scanning);
    server
        .views
        .on_scan(&server, &scan_id, ScanStateName::Cancelled);
    c.ok("view.close", json!({"view_id": view_id})).await;
    drop(guard);
    tokio::time::sleep(Duration::from_millis(600)).await;
    let reasons: Vec<String> = c
        .drain_events()
        .await
        .into_iter()
        .filter(|(m, p)| m == "view.frame" && p["frame"]["view_id"] == view_id.as_str())
        .map(|(_, p)| p["frame"]["reason"].as_str().unwrap_or("").to_owned())
        .collect();
    assert!(reasons.is_empty(), "frames of the closed view: {reasons:?}");
    assert!(
        c.drain_binary().await.is_empty(),
        "binary frames of the closed view"
    );
    srv.shutdown();
}

#[tokio::test]
async fn the_palette_toggle_needs_a_bin() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    // the shared trash at the top of a scanned folder is a bin; a plain folder has none
    make_tree(
        &tmp.path().join("with"),
        &[(".Trash/1000/f.bin", 100), ("data/g.bin", 100)],
    );
    make_tree(&tmp.path().join("without"), &[("data/g.bin", 100)]);
    let mut got = Vec::new();
    for name in ["with", "without"] {
        let scan_id = scanned(&c, &tmp.path().join(name)).await;
        let view_id = view(&c, &scan_id).await;
        let cmd = c
            .ok(
                "palette.check",
                json!({"id": "view.toggle_trash",
                       "context": {"tab": "scan", "scan_id": scan_id, "view_id": view_id}}),
            )
            .await;
        got.push(cmd);
    }
    assert_eq!(got[0]["enabled"], true, "{}", got[0]);
    assert_eq!(got[1]["enabled"], false, "{}", got[1]);
    assert_eq!(got[1]["status"], "这次扫描里没有回收站", "{}", got[1]);
    srv.shutdown();
}
