//! Port of tests/test_ops_server.py (path.forms, basket.*, ops.* over the real WebSocket) plus the
//! export.* methods.
//!
//! Only files of each test's temporary folder are touched. Whatever goes to the real recycle bin
//! is put back by the test (undo, or a cleanup when an assertion failed first); the recycle bin is
//! never emptied.

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use serde_json::{Value, json};

async fn setup() -> (sk_server::ServerHandle, Client) {
    let srv = server().await;
    srv.server.ops.set_delete_delay(Duration::from_millis(300)); // the undo window, shortened
    let c = Client::open(&srv.url).await;
    (srv, c)
}

async fn scanned(c: &Client, root: &Path) -> String {
    scan(c, root).await["scan_id"].as_str().unwrap().to_owned()
}

async fn node_of(c: &Client, scan_id: &str, path: &Path) -> u64 {
    let found = c.ok("tree.find", json!({"scan_id": scan_id, "path": path.to_string_lossy()})).await;
    found["node"]["id"].as_u64().unwrap_or_else(|| panic!("{} not found", path.display()))
}

async fn phase(c: &Client, task: &str, phases: &[&str]) -> Value {
    c.event_where("ops.progress", 60.0, |e| e["task_id"] == task && phases.contains(&e["phase"].as_str().unwrap()))
        .await
}

fn names(state: &Value) -> std::collections::BTreeSet<String> {
    state["items"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap().to_owned()).collect()
}

#[tokio::test]
async fn path_forms() {
    let (_srv, c) = setup().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    make_tree(&root, &[("a b/文件 1.txt", 1)]);
    let scan_id = scanned(&c, &root).await;
    let file = root.join("a b").join("文件 1.txt");
    let node = node_of(&c, &scan_id, &file).await;
    let f = c.ok("path.forms", json!({"scan_id": scan_id, "node": node})).await;
    assert_eq!(f["raw"].as_str().unwrap(), file.to_string_lossy());
    assert_eq!(f["on_disk"], true);
    let uri = f["uri"].as_str().unwrap();
    assert!(uri.starts_with("file://") && uri.contains("%20") && uri.contains("%E6%96%87"), "{uri}");
    let quoted = f["quoted"].as_str().unwrap();
    assert!(quoted.contains(' ') && (quoted.starts_with('"') || quoted.starts_with('\'')));
}

#[tokio::test]
async fn trash_and_undo() {
    let (srv, c) = setup().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(root.join("keep")).unwrap();
    let victim = root.join("victim.bin");
    std::fs::write(&victim, vec![b'v'; 40_000]).unwrap();
    let folder = root.join("old");
    make_tree(&folder, &[("sub/x.dat", 10_000)]);
    let scan_id = scanned(&c, &root).await;
    c.ok("basket.clear", json!({})).await;
    let v = node_of(&c, &scan_id, &victim).await;
    let f = node_of(&c, &scan_id, &folder).await;
    let sub = node_of(&c, &scan_id, &folder.join("sub")).await;
    let added = c.ok("basket.add", json!({"scan_id": scan_id, "nodes": [sub]})).await;
    assert_eq!(added["added"], 1);
    let added = c.ok("basket.add", json!({"scan_id": scan_id, "nodes": [v, f]})).await;
    assert_eq!((added["added"].as_i64(), added["absorbed"].as_i64()), (Some(2), Some(1))); // the folder took in its sub folder
    let state = c.ok("basket.list", json!({})).await;
    assert_eq!(names(&state), ["old", "victim.bin"].iter().map(|s| s.to_string()).collect());
    assert!(state["freeable"].as_i64().unwrap() >= 50_000 && !state["volumes"].as_array().unwrap().is_empty());
    let refused = c.ok("basket.add", json!({"scan_id": scan_id, "nodes": [0]})).await;
    assert_eq!(refused["added"], 0);
    assert!(!refused["refused"].as_array().unwrap().is_empty());

    let task = c.ok("basket.execute", json!({"mode": "trash"})).await;
    let id = task["task_id"].as_str().unwrap().to_owned();
    let done = phase(&c, &id, &["done", "cancelled"]).await;
    let all_ok = done["results"].as_array().unwrap().iter().all(|r| r["ok"] == true);
    if !all_ok {
        eprintln!("skipped: no usable recycle bin here: {:?}", done["results"]);
        return;
    }
    let check = async {
        assert!(!victim.exists() && !folder.exists());
        assert_eq!(done["can_undo"], true);
        assert!(done["freed"].as_i64().unwrap() >= 50_000);
        assert_eq!(done["mode"], "trash");
        // executed items left the basket (the closing broadcast may still be on its way)
        assert!(c.ok("basket.list", json!({})).await["items"].as_array().unwrap().is_empty());

        c.ok("ops.undo", json!({"task_id": id})).await;
        phase(&c, &id, &["undone"]).await;
        assert_eq!(std::fs::read(&victim).unwrap(), vec![b'v'; 40_000]);
        assert!(folder.join("sub").join("x.dat").exists());
        let back = c.ok("basket.list", json!({})).await;
        assert_eq!(names(&back), ["old", "victim.bin"].iter().map(|s| s.to_string()).collect()); // back in the basket
    };
    let outcome = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(check)).await;
    if outcome.is_err() {
        // an assertion failed before undo: put them back anyway
        if let Some(run) = srv.server.ops.run(&id) {
            let refs = run.refs();
            if !refs.is_empty() {
                sk_core::ops::restore(&refs);
            }
        }
        panic!("trash_and_undo failed (restored what went to the recycle bin)");
    }
}

#[tokio::test]
async fn delete_waits_and_can_be_called_off() {
    let (_srv, c) = setup().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    make_tree(&root, &[("doomed.tmp", 1000)]);
    let doomed = root.join("doomed.tmp");
    let scan_id = scanned(&c, &root).await;
    c.ok("basket.clear", json!({})).await;
    let node = node_of(&c, &scan_id, &doomed).await;
    c.ok("basket.add", json!({"scan_id": scan_id, "nodes": [node]})).await;

    // called off inside the undo window: nothing touched, still in the basket
    let task = c.ok("basket.execute", json!({"mode": "delete"})).await;
    assert!(task["delay_s"].as_f64().unwrap() > 0.0);
    let id = task["task_id"].as_str().unwrap().to_owned();
    let waiting = phase(&c, &id, &["waiting"]).await;
    assert_eq!(waiting["can_undo"], true);
    c.ok("ops.undo", json!({"task_id": id})).await;
    phase(&c, &id, &["undone"]).await;
    assert!(doomed.exists());
    assert_eq!(c.ok("basket.list", json!({})).await["items"].as_array().unwrap().len(), 1);

    // and for real
    let task = c.ok("basket.execute", json!({"mode": "delete"})).await;
    let id = task["task_id"].as_str().unwrap().to_owned();
    let done = phase(&c, &id, &["done"]).await;
    let codes: Vec<&str> = done["results"].as_array().unwrap().iter().map(|r| r["code"].as_str().unwrap()).collect();
    assert_eq!(codes, ["ok"]);
    assert!(!doomed.exists());
    assert_eq!(done["can_undo"], false);
    assert_eq!(done["mode"], "delete");
    assert!(c.ok("basket.list", json!({})).await["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn busy_and_unknown_tasks() {
    let (_srv, c) = setup().await;
    c.ok("basket.clear", json!({})).await;
    let e = c.err("basket.execute", json!({"mode": "trash"})).await;
    assert_eq!(e.code, i64::from(sk_protocol::ErrorCode::InvalidRequest.code())); // nothing to execute
    let e = c.err("ops.undo", json!({"task_id": "op-999"})).await;
    assert_eq!(e.code, i64::from(sk_protocol::ErrorCode::InvalidParams.code()));
}

#[tokio::test]
async fn tree_follows_without_monitoring() {
    let (srv, c) = setup().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    make_tree(&root, &[("gone.tmp", 5000), ("stay.tmp", 1)]);
    let gone = root.join("gone.tmp");
    let scan_id = scanned(&c, &root).await;
    srv.server.monitors.stop(&scan_id);
    c.ok("basket.clear", json!({})).await;
    let node = node_of(&c, &scan_id, &gone).await;
    c.ok("basket.add", json!({"scan_id": scan_id, "nodes": [node]})).await;
    let task = c.ok("basket.execute", json!({"mode": "delete"})).await;
    phase(&c, task["task_id"].as_str().unwrap(), &["done"]).await;
    let find = |p: &Path| json!({"scan_id": scan_id, "path": p.to_string_lossy()});
    let mut found = Value::Null;
    for _ in 0..100 {
        // the parent folder is read again
        found = c.ok("tree.find", find(&gone)).await["node"].clone();
        if found.is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(found.is_null());
    assert!(!c.ok("tree.find", find(&root.join("stay.tmp"))).await["node"].is_null());
    assert!(root.join("stay.tmp").exists());
}

// ---------------------------------------------------------------------------------- export.*

#[tokio::test]
async fn export_targets_check_and_run() {
    let (_srv, c) = setup().await;
    let targets = c.ok("export.targets", json!({})).await;
    let list = targets["targets"].as_array().unwrap();
    assert!(list.iter().any(|t| t["name"] == "Simple file list" && t["kind"] == "template"));
    assert!(list.iter().any(|t| t["name"] == "csv" && t["kind"] == "format"));

    let ok = c.ok("export.check", json!({"source": "<%path%>"})).await;
    assert_eq!(ok["ok"], true, "{ok}");
    let bad = c.ok("export.check", json!({"source": "x<%nosuch%>"})).await;
    assert_eq!(bad["ok"], false, "{bad}");
    assert_eq!(bad["issues"][0]["code"], "unknown_tag");

    let tree = sample_tree();
    let scan_id = scanned(&c, tree.path()).await;
    let opened = c
        .ok("view.open", json!({"scan_id": scan_id, "viewport": {"width": 800, "height": 600, "dpr": 1}}))
        .await;
    let view_id = opened["state"]["view_id"].as_str().unwrap().to_owned();
    let out_dir = tempfile::tempdir().unwrap();
    let out = out_dir.path().join("list.txt");
    let r = c
        .ok("export.run", json!({"view_id": view_id, "target": "Simple file list", "path": out.to_string_lossy(), "scope": "all"}))
        .await;
    assert_eq!(r["bytes"].as_i64().unwrap() as u64, std::fs::metadata(&out).unwrap().len());
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("three.mp4") && text.contains("one.txt"), "{text}");

    // filtered CSV: only the matching file
    c.ok("view.filter", json!({"view_id": view_id, "text": "*.mp4"})).await;
    let csv = out_dir.path().join("list.csv");
    c.ok("export.run", json!({"view_id": view_id, "target": "csv", "path": csv.to_string_lossy(), "scope": "all", "use_filter": true}))
        .await;
    let text = std::fs::read_to_string(&csv).unwrap();
    assert!(text.contains("three.mp4") && !text.contains("one.txt"), "{text}");

    let e = c
        .err("export.run", json!({"view_id": view_id, "target": "csv", "path": out_dir.path().join("no/such/x.csv").to_string_lossy()}))
        .await;
    assert_eq!(e.code, i64::from(sk_protocol::ErrorCode::InvalidParams.code()));
    let e = c.err("export.run", json!({"view_id": view_id, "target": "", "path": csv.to_string_lossy()})).await;
    assert_eq!(e.code, i64::from(sk_protocol::ErrorCode::InvalidParams.code()));
}
