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
    let found = c
        .ok(
            "tree.find",
            json!({"scan_id": scan_id, "path": path.to_string_lossy()}),
        )
        .await;
    found["node"]["id"]
        .as_u64()
        .unwrap_or_else(|| panic!("{} not found", path.display()))
}

async fn phase(c: &Client, task: &str, phases: &[&str]) -> Value {
    c.event_where("ops.progress", 60.0, |e| {
        e["task_id"] == task && phases.contains(&e["phase"].as_str().unwrap())
    })
    .await
}

fn names(state: &Value) -> std::collections::BTreeSet<String> {
    state["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_owned())
        .collect()
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
    let f = c
        .ok("path.forms", json!({"scan_id": scan_id, "node": node}))
        .await;
    assert_eq!(f["raw"].as_str().unwrap(), file.to_string_lossy());
    assert_eq!(f["on_disk"], true);
    let uri = f["uri"].as_str().unwrap();
    assert!(
        uri.starts_with("file://") && uri.contains("%20") && uri.contains("%E6%96%87"),
        "{uri}"
    );
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
    let added = c
        .ok("basket.add", json!({"scan_id": scan_id, "nodes": [sub]}))
        .await;
    assert_eq!(added["added"], 1);
    let added = c
        .ok("basket.add", json!({"scan_id": scan_id, "nodes": [v, f]}))
        .await;
    assert_eq!(
        (added["added"].as_i64(), added["absorbed"].as_i64()),
        (Some(2), Some(1))
    ); // the folder took in its sub folder
    let state = c.ok("basket.list", json!({})).await;
    assert_eq!(
        names(&state),
        ["old", "victim.bin"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );
    assert!(
        state["freeable"].as_i64().unwrap() >= 50_000
            && !state["volumes"].as_array().unwrap().is_empty()
    );
    let refused = c
        .ok("basket.add", json!({"scan_id": scan_id, "nodes": [0]}))
        .await;
    assert_eq!(refused["added"], 0);
    assert!(!refused["refused"].as_array().unwrap().is_empty());

    let task = c.ok("basket.execute", json!({"mode": "trash"})).await;
    let id = task["task_id"].as_str().unwrap().to_owned();
    let done = phase(&c, &id, &["done", "cancelled"]).await;
    let all_ok = done["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["ok"] == true);
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
        assert!(
            c.ok("basket.list", json!({})).await["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        c.ok("ops.undo", json!({"task_id": id})).await;
        phase(&c, &id, &["undone"]).await;
        assert_eq!(std::fs::read(&victim).unwrap(), vec![b'v'; 40_000]);
        assert!(folder.join("sub").join("x.dat").exists());
        let back = c.ok("basket.list", json!({})).await;
        assert_eq!(
            names(&back),
            ["old", "victim.bin"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        ); // back in the basket
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
    c.ok("basket.add", json!({"scan_id": scan_id, "nodes": [node]}))
        .await;

    // called off inside the undo window: nothing touched, still in the basket
    let task = c.ok("basket.execute", json!({"mode": "delete"})).await;
    assert!(task["delay_s"].as_f64().unwrap() > 0.0);
    let id = task["task_id"].as_str().unwrap().to_owned();
    let waiting = phase(&c, &id, &["waiting"]).await;
    assert_eq!(waiting["can_undo"], true);
    c.ok("ops.undo", json!({"task_id": id})).await;
    phase(&c, &id, &["undone"]).await;
    assert!(doomed.exists());
    assert_eq!(
        c.ok("basket.list", json!({})).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // and for real
    let task = c.ok("basket.execute", json!({"mode": "delete"})).await;
    let id = task["task_id"].as_str().unwrap().to_owned();
    let done = phase(&c, &id, &["done"]).await;
    let codes: Vec<&str> = done["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["ok"]);
    assert!(!doomed.exists());
    assert_eq!(done["can_undo"], false);
    assert_eq!(done["mode"], "delete");
    assert!(
        c.ok("basket.list", json!({})).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn busy_and_unknown_tasks() {
    let (_srv, c) = setup().await;
    c.ok("basket.clear", json!({})).await;
    let e = c.err("basket.execute", json!({"mode": "trash"})).await;
    assert_eq!(
        e.code,
        i64::from(sk_protocol::ErrorCode::InvalidRequest.code())
    ); // nothing to execute
    let e = c.err("ops.undo", json!({"task_id": "op-999"})).await;
    assert_eq!(
        e.code,
        i64::from(sk_protocol::ErrorCode::InvalidParams.code())
    );
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
    c.ok("basket.add", json!({"scan_id": scan_id, "nodes": [node]}))
        .await;
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

// ---------------------------------------------------------------------------------- what runs

use sk_server::ops_service::TestHooks;

/// Two sessions ask to execute at the same moment: one operation starts, the other is told one
/// is running (the busy check and the registration are one step; they were two, and a second
/// request between them started a second operation on the same basket).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_one_of_two_executions_at_once_starts() {
    let (srv, c) = setup().await;
    let c2 = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    make_tree(&root, &[("doomed.tmp", 1000)]);
    let scan_id = scanned(&c, &root).await;
    c.ok("basket.clear", json!({})).await;
    let node = node_of(&c, &scan_id, &root.join("doomed.tmp")).await;
    c.ok("basket.add", json!({"scan_id": scan_id, "nodes": [node]}))
        .await;
    srv.server.ops.set_delete_delay(Duration::from_secs(60)); // both would wait: nothing deleted
    // widen the moment between the busy check and the registration
    srv.server.ops.set_test_hooks(TestHooks {
        registering: Some(std::sync::Arc::new(|| {
            std::thread::sleep(Duration::from_millis(300))
        })),
        ..TestHooks::default()
    });
    let execute = json!({"mode": "delete"});
    let (a, b) = tokio::join!(
        c.call("basket.execute", execute.clone()),
        c2.call("basket.execute", execute.clone())
    );
    srv.server.ops.set_test_hooks(TestHooks::default());
    let started: Vec<String> = [&a, &b]
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .map(|t| t["task_id"].as_str().unwrap().to_owned())
        .collect();
    for id in &started {
        c.ok("ops.cancel", json!({"task_id": id})).await; // called off in the undo window
    }
    assert_eq!(started.len(), 1, "{a:?} {b:?}");
    let refused = [&a, &b].into_iter().find_map(|r| r.as_ref().err()).unwrap();
    assert_eq!(
        refused.code,
        i64::from(sk_protocol::ErrorCode::InvalidRequest.code())
    );
    // its events go to the session that started it
    let owner = if a.is_ok() { &c } else { &c2 };
    phase(owner, &started[0], &["undone"]).await;
    assert!(root.join("doomed.tmp").exists());
}

/// What a confirmation was given for is what runs: the ids the client confirmed, not whatever
/// another session put into the shared basket meanwhile.
#[tokio::test]
async fn execute_takes_only_the_items_confirmed() {
    let (srv, c) = setup().await;
    let other = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    make_tree(&root, &[("confirmed.tmp", 100), ("added later.tmp", 100)]);
    let scan_id = scanned(&c, &root).await;
    c.ok("basket.clear", json!({})).await;
    let first = node_of(&c, &scan_id, &root.join("confirmed.tmp")).await;
    let state = c
        .ok("basket.add", json!({"scan_id": scan_id, "nodes": [first]}))
        .await["state"]
        .clone();
    let confirmed: Vec<String> = state["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect();
    // the confirmation dialog is open; another session adds an item
    let later = node_of(&other, &scan_id, &root.join("added later.tmp")).await;
    other
        .ok("basket.add", json!({"scan_id": scan_id, "nodes": [later]}))
        .await;
    let task = c
        .ok(
            "basket.execute",
            json!({"mode": "delete", "ids": confirmed}),
        )
        .await;
    assert_eq!(task["total"], 1);
    phase(&c, task["task_id"].as_str().unwrap(), &["done"]).await;
    assert!(!root.join("confirmed.tmp").exists());
    assert!(root.join("added later.tmp").exists());
    assert_eq!(
        names(&c.ok("basket.list", json!({})).await),
        ["added later.tmp".to_string()].into()
    );
}

fn confirm_of(state: &Value) -> Value {
    state["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| json!({"id": i["id"], "rev": i["rev"]}))
        .collect()
}

/// `confirm` names each item as the dialog showed it (`rev`): those run, not what another session
/// put beside them meanwhile.
#[tokio::test]
async fn confirmed_items_run_and_nothing_beside_them() {
    let (srv, c) = setup().await;
    let other = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    make_tree(&root, &[("confirmed.tmp", 100), ("added later.tmp", 100)]);
    let scan_id = scanned(&c, &root).await;
    c.ok("basket.clear", json!({})).await;
    let first = node_of(&c, &scan_id, &root.join("confirmed.tmp")).await;
    let state = c
        .ok("basket.add", json!({"scan_id": scan_id, "nodes": [first]}))
        .await["state"]
        .clone();
    assert!(state["items"][0]["rev"].as_i64().unwrap() > 0, "{state}");
    let confirmed = confirm_of(&state);
    let later = node_of(&other, &scan_id, &root.join("added later.tmp")).await;
    other
        .ok("basket.add", json!({"scan_id": scan_id, "nodes": [later]}))
        .await;
    let task = c
        .ok(
            "basket.execute",
            json!({"mode": "delete", "confirm": confirmed}),
        )
        .await;
    assert_eq!(task["total"], 1);
    phase(&c, task["task_id"].as_str().unwrap(), &["done"]).await;
    assert!(!root.join("confirmed.tmp").exists());
    assert!(root.join("added later.tmp").exists());
    // both ways at once is no confirmation
    let e = c
        .err(
            "basket.execute",
            json!({"mode": "delete", "ids": [], "confirm": confirmed}),
        )
        .await;
    assert_eq!(
        e.code,
        i64::from(sk_protocol::ErrorCode::InvalidParams.code())
    );
}

/// The confirmation dialog is open when another session changes what a confirmed item covers
/// (here: adds the folder that holds it, which takes it in): nothing runs and the client is told
/// to ask again (BASKET_CHANGED); confirmed as the basket is now, it runs.
#[tokio::test]
async fn nothing_runs_when_a_confirmed_item_changed() {
    let (srv, c) = setup().await;
    let other = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    make_tree(
        &root,
        &[("sub/a.tmp", 100), ("sub/b.tmp", 100), ("keep.tmp", 1)],
    );
    let scan_id = scanned(&c, &root).await;
    c.ok("basket.clear", json!({})).await;
    let a = node_of(&c, &scan_id, &root.join("sub").join("a.tmp")).await;
    let state = c
        .ok("basket.add", json!({"scan_id": scan_id, "nodes": [a]}))
        .await["state"]
        .clone();
    let confirmed = confirm_of(&state);
    let sub = node_of(&other, &scan_id, &root.join("sub")).await;
    other
        .ok("basket.add", json!({"scan_id": scan_id, "nodes": [sub]}))
        .await;
    let e = c
        .err(
            "basket.execute",
            json!({"mode": "delete", "confirm": confirmed}),
        )
        .await;
    assert_eq!(e.code, -32014, "{e:?}");
    assert_eq!(
        e.code,
        i64::from(sk_protocol::ErrorCode::BasketChanged.code())
    );
    tokio::time::sleep(Duration::from_millis(500)).await; // past the undo window: nothing ran
    assert!(root.join("sub").join("a.tmp").exists());
    assert!(root.join("sub").join("b.tmp").exists());
    // asked again, as it is now
    let now = c.ok("basket.list", json!({})).await;
    let task = c
        .ok(
            "basket.execute",
            json!({"mode": "delete", "confirm": confirm_of(&now)}),
        )
        .await;
    phase(&c, task["task_id"].as_str().unwrap(), &["done"]).await;
    assert!(!root.join("sub").exists());
    assert!(root.join("keep.tmp").exists());
}

/// Renumber the scan's tree as the file-change monitor does once enough rows are gone: a
/// compacted copy adopted (every id changes).
fn renumber(srv: &sk_server::ServerHandle, scan_id: &str) {
    let h = srv.server.scans.lookup(scan_id).unwrap();
    let (compacted, _) = {
        let r = h.store.read();
        sk_store::mutate::compact(&r).unwrap()
    };
    h.store.write().adopt(&compacted);
    h.ctx.names.forget();
}

/// A folder read again (an entry added) and the tree renumbered between the moment the interface
/// read an id and basket.add: the id names another item now. basket.add with the generation it
/// was read in is refused (UNKNOWN_NODE) and adds nothing; read again, the item goes in.
#[tokio::test]
async fn a_node_id_read_before_a_renumbering_is_refused() {
    let (srv, c) = setup().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    make_tree(&root, &[("b.tmp", 100), ("c.tmp", 100), ("d/e.tmp", 10)]);
    let scan_id = scanned(&c, &root).await;
    srv.server.monitors.stop(&scan_id);
    c.ok("basket.clear", json!({})).await;
    let b = node_of(&c, &scan_id, &root.join("b.tmp")).await;
    let read = c
        .ok("tree.summary", json!({"scan_id": scan_id, "node": b}))
        .await;
    assert_eq!(read["node"]["id"], b);
    let g0 = read["generation"].as_u64().unwrap();
    // the folder is read again with a new entry first: its entries move to new rows ...
    std::fs::write(root.join("a.tmp"), b"a").unwrap();
    c.ok("scan.rescan", json!({"scan_id": scan_id, "node": 1}))
        .await;
    for _ in 0..200 {
        let found = c
            .ok(
                "tree.find",
                json!({"scan_id": scan_id, "path": root.join("a.tmp").to_string_lossy()}),
            )
            .await;
        if !found["node"].is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // ... and the tree is renumbered
    renumber(&srv, &scan_id);
    let now = c
        .call("tree.node", json!({"scan_id": scan_id, "node": b}))
        .await;
    eprintln!("the old id now names {now:?}");
    let e = c
        .err(
            "basket.add",
            json!({"scan_id": scan_id, "nodes": [b], "generation": g0}),
        )
        .await;
    assert_eq!(
        e.code,
        i64::from(sk_protocol::ErrorCode::UnknownNode.code()),
        "{e:?}"
    );
    assert!(
        c.ok("basket.list", json!({})).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // read again: the same file, by its new id and numbering
    let b2 = node_of(&c, &scan_id, &root.join("b.tmp")).await;
    let g1 = c
        .ok("tree.summary", json!({"scan_id": scan_id, "node": b2}))
        .await["generation"]
        .as_u64()
        .unwrap();
    assert_ne!(g1, g0);
    let added = c
        .ok(
            "basket.add",
            json!({"scan_id": scan_id, "nodes": [b2], "generation": g1}),
        )
        .await;
    assert_eq!(added["added"], 1, "{added}");
    assert_eq!(names(&added["state"]), ["b.tmp".to_string()].into());
}

/// A block of a frame drawn before the tree was renumbered (the frame's `generation`) is refused
/// too, even where the client does not say which numbering it read: the frame's own decides.
#[tokio::test]
async fn a_block_of_a_frame_from_before_a_renumbering_is_refused() {
    let (srv, c) = setup().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    make_tree(&root, &[("b.tmp", 5000), ("c.tmp", 4000)]);
    let scan_id = scanned(&c, &root).await;
    srv.server.monitors.stop(&scan_id);
    c.ok("basket.clear", json!({})).await;
    let up = c
        .ok(
            "view.open",
            json!({"scan_id": scan_id, "viewport": {"width": 400, "height": 300}}),
        )
        .await;
    let view_id = up["state"]["view_id"].as_str().unwrap().to_owned();
    let g0 = up["frame"]["generation"].as_u64().unwrap();
    let b = node_of(&c, &scan_id, &root.join("b.tmp")).await; // a file's block key is its id
    renumber(&srv, &scan_id);
    for params in [
        json!({"scan_id": scan_id, "view_id": view_id, "key": b, "generation": g0}),
        json!({"scan_id": scan_id, "view_id": view_id, "key": b}),
    ] {
        let e = c.err("basket.add", params).await;
        assert_eq!(
            e.code,
            i64::from(sk_protocol::ErrorCode::UnknownNode.code()),
            "{e:?}"
        );
    }
    assert!(
        c.ok("basket.list", json!({})).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    c.ok("view.close", json!({"view_id": view_id})).await;
}

/// A move to the trash stopped midway can be undone: what went to the trash before the stop
/// comes back (it could not: the undo only took finished runs). The Windows shell takes up to
/// 64 items per operation and stops between operations, so there are more than that.
#[tokio::test]
async fn a_cancelled_trash_run_can_be_undone() {
    let (srv, c) = setup().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let n = sk_core::ops::trash::BATCH_ITEMS + 6;
    let names: Vec<String> = (0..n).map(|i| format!("t{i:03}.tmp")).collect();
    let spec: Vec<(&str, usize)> = names.iter().map(|s| (s.as_str(), 10)).collect();
    make_tree(&root, &spec);
    let files: Vec<std::path::PathBuf> = names.iter().map(|s| root.join(s)).collect();
    let scan_id = scanned(&c, &root).await;
    c.ok("basket.clear", json!({})).await;
    let mut nodes = Vec::new();
    for f in &files {
        nodes.push(node_of(&c, &scan_id, f).await);
    }
    c.ok("basket.add", json!({"scan_id": scan_id, "nodes": nodes}))
        .await;
    // stop once the first item is in the trash
    let server = std::sync::Arc::downgrade(&srv.server);
    srv.server.ops.set_test_hooks(TestHooks {
        progress: Some(std::sync::Arc::new(move |task: &str, done: usize| {
            if done == 1
                && let Some(server) = server.upgrade()
            {
                let _ = server.ops.cancel(task);
            }
        })),
        ..TestHooks::default()
    });
    let task = c.ok("basket.execute", json!({"mode": "trash"})).await;
    let id = task["task_id"].as_str().unwrap().to_owned();
    let end = phase(&c, &id, &["done", "cancelled"]).await;
    srv.server.ops.set_test_hooks(TestHooks::default());
    let gone: Vec<&std::path::PathBuf> = files.iter().filter(|f| !f.exists()).collect();
    if gone.is_empty() {
        eprintln!("skipped: no usable recycle bin here: {:?}", end["results"]);
        return;
    }
    let check = async {
        assert_eq!(end["phase"], "cancelled", "{end}");
        assert!(gone.len() < files.len(), "{end}");
        assert_eq!(end["can_undo"], true, "{end}");
        c.ok("ops.undo", json!({"task_id": id})).await;
        phase(&c, &id, &["undone"]).await;
        assert!(files.iter().all(|f| f.exists()));
    };
    let outcome = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(check)).await;
    if outcome.is_err() {
        // put back what went to the recycle bin anyway
        if let Some(run) = srv.server.ops.run(&id) {
            let refs = run.refs();
            if !refs.is_empty() {
                sk_core::ops::restore(&refs);
            }
        }
        panic!(
            "a_cancelled_trash_run_can_be_undone failed (restored what went to the recycle bin)"
        );
    }
}

// ---------------------------------------------------------------------------------- export.*

#[tokio::test]
async fn export_targets_check_and_run() {
    let (_srv, c) = setup().await;
    let targets = c.ok("export.targets", json!({})).await;
    let list = targets["targets"].as_array().unwrap();
    assert!(
        list.iter()
            .any(|t| t["name"] == "Simple file list" && t["kind"] == "template")
    );
    assert!(
        list.iter()
            .any(|t| t["name"] == "csv" && t["kind"] == "format")
    );

    let ok = c.ok("export.check", json!({"source": "<%path%>"})).await;
    assert_eq!(ok["ok"], true, "{ok}");
    let bad = c.ok("export.check", json!({"source": "x<%nosuch%>"})).await;
    assert_eq!(bad["ok"], false, "{bad}");
    assert_eq!(bad["issues"][0]["code"], "unknown_tag");

    let tree = sample_tree();
    let scan_id = scanned(&c, tree.path()).await;
    let opened = c
        .ok(
            "view.open",
            json!({"scan_id": scan_id, "viewport": {"width": 800, "height": 600, "dpr": 1}}),
        )
        .await;
    let view_id = opened["state"]["view_id"].as_str().unwrap().to_owned();
    let out_dir = tempfile::tempdir().unwrap();
    let out = out_dir.path().join("list.txt");
    let r = c
        .ok("export.run", json!({"view_id": view_id, "target": "Simple file list", "path": out.to_string_lossy(), "scope": "all"}))
        .await;
    assert_eq!(
        r["bytes"].as_i64().unwrap() as u64,
        std::fs::metadata(&out).unwrap().len()
    );
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(
        text.contains("three.mp4") && text.contains("one.txt"),
        "{text}"
    );

    // filtered CSV: only the matching file
    c.ok("view.filter", json!({"view_id": view_id, "text": "*.mp4"}))
        .await;
    let csv = out_dir.path().join("list.csv");
    c.ok("export.run", json!({"view_id": view_id, "target": "csv", "path": csv.to_string_lossy(), "scope": "all", "use_filter": true}))
        .await;
    let text = std::fs::read_to_string(&csv).unwrap();
    assert!(
        text.contains("three.mp4") && !text.contains("one.txt"),
        "{text}"
    );

    let e = c
        .err("export.run", json!({"view_id": view_id, "target": "csv", "path": out_dir.path().join("no/such/x.csv").to_string_lossy()}))
        .await;
    assert_eq!(
        e.code,
        i64::from(sk_protocol::ErrorCode::InvalidParams.code())
    );
    let e = c
        .err(
            "export.run",
            json!({"view_id": view_id, "target": "", "path": csv.to_string_lossy()}),
        )
        .await;
    assert_eq!(
        e.code,
        i64::from(sk_protocol::ErrorCode::InvalidParams.code())
    );
}
