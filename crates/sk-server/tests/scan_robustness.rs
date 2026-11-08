//! Port of the rest of 0.3.4's `tests/test_robust_scan_manager.py`: a failing status listener,
//! auto-save failures (prune and save), the summary of folders past the denied list, and the
//! state of saved snapshots (expected items, a loaded snapshot's state). 0.3.4 patched the
//! functions; here the failures are made on disk.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::*;
use serde_json::{Value, json};
use sk_store::snapshot::{self, SaveOptions};

/// The tests share this binary's snapshot folder.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A tree root by its long, absolute name (the name the server files snapshots under).
fn tree(tmp: &Path) -> PathBuf {
    let facts = build_tree(&tmp.join("root"));
    let p = std::fs::canonicalize(&facts.root).unwrap();
    PathBuf::from(
        p.to_string_lossy()
            .strip_prefix(r"\\?\")
            .map(str::to_owned)
            .unwrap_or_else(|| p.to_string_lossy().into_owned()),
    )
}

fn roots_of(root: &Path) -> Vec<String> {
    vec![root.to_string_lossy().into_owned()]
}

/// The auto-snapshot folder of these roots.
fn folder_of(roots: &[String]) -> PathBuf {
    sk_server::scans::auto_path(roots).parent().unwrap().to_path_buf()
}

/// The log-panel events of a scan (records with `event` and its `scan_id`), from the log bus.
async fn events_of(scan_id: &str) -> Vec<String> {
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    sk_core::log::LogBus::global()
        .recent(5000, sk_protocol::streams::LogLevel::Trace)
        .into_iter()
        .filter(|e| {
            e.extra.get("event").is_some_and(|v| !v.is_empty())
                && e.extra.get("scan_id").map(String::as_str) == Some(scan_id)
        })
        .map(|e| e.message.clone())
        .collect()
}

#[tokio::test]
async fn failing_listener_never_fails_the_scan() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tree(tmp.path());
    let st = c
        .ok(
            "scan.start",
            json!({"roots": [root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    let h = srv.server.scans.get(&id).unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    h.add_listener("a-broken", Arc::new(|_| panic!("subscriber bug")));
    {
        let seen = seen.clone();
        h.add_listener(
            "b-fine",
            Arc::new(move |s| {
                let v = serde_json::to_value(s).unwrap();
                seen.lock()
                    .unwrap()
                    .push(v["state"].as_str().unwrap_or("").to_owned());
            }),
        );
    }
    let done = wait_done(&c, &id).await;
    assert_eq!(done["state"], "done");
    h.publish(&srv.server); // the scan may have ended before the listeners came
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.last().map(String::as_str), Some("done"), "{seen:?}");
    assert_eq!(c.ok("scan.status", json!({"scan_id": id})).await["state"], "done");
    srv.shutdown();
}

/// Pruning the old snapshots fails (an entry it cannot remove): the new snapshot stays, no
/// warning.
#[tokio::test]
async fn prune_failure_keeps_the_saved_snapshot() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tree(tmp.path());
    let folder = folder_of(&roots_of(&root));
    // keep one: the older entries of a week are pruned, and one of them is a folder
    let s = c.ok("settings.get", json!({})).await;
    c.ok(
        "settings.update",
        json!({"values": {"advanced.snapshot_keep": 1}, "version": s["version"]}),
    )
    .await;
    std::fs::create_dir_all(folder.join("20200106-000000.sks")).unwrap();
    std::fs::write(folder.join("20200107-000000.sks"), b"old").unwrap();
    let st = c
        .ok("scan.start", json!({"roots": [root], "workers": 0}))
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    let fin = wait_final(&c, &id, true).await;
    let saved = PathBuf::from(fin["snapshot"].as_str().unwrap());
    assert!(saved.exists());
    assert_eq!(saved.parent().unwrap(), folder);
    assert!(folder.join("20200106-000000.sks").is_dir(), "prune could not remove it");
    let msgs = events_of(&id).await;
    assert!(msgs.iter().any(|m| m.starts_with("硬扫描完成")), "{msgs:?}");
    assert!(
        !msgs.iter().any(|m| m.contains("自动保存快照失败")),
        "{msgs:?}"
    );
    c.ok("settings.reset", json!({"category": "advanced"}))
        .await;
    srv.shutdown();
}

/// Saving fails (a file where the snapshot folder goes): the scan still ends `done` and is
/// published, without a snapshot, with a warning in the log panel.
#[tokio::test]
async fn unexpected_save_error_still_publishes() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tree(tmp.path());
    let folder = folder_of(&roots_of(&root));
    std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
    std::fs::write(&folder, b"not a folder").unwrap();
    let st = c
        .ok("scan.start", json!({"roots": [root], "workers": 0}))
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    let fin = wait_final(&c, &id, false).await;
    assert_eq!(fin["state"], "done");
    let h = srv.server.scans.get(&id).unwrap();
    h.finished().await;
    let again = c.ok("scan.status", json!({"scan_id": id})).await;
    assert_eq!(again["state"], "done");
    assert!(again["snapshot"].is_null(), "{again}");
    assert_eq!(folder_of(&h.root_paths()), folder, "the failure hit the scan's folder");
    let msgs = events_of(&id).await;
    assert!(
        msgs.iter().any(|m| m.contains("自动保存快照失败")),
        "{msgs:?}"
    );
    std::fs::remove_file(&folder).unwrap();
    srv.shutdown();
}

/// More folders that cannot be read than the scan records (200): one summary line for the rest,
/// only while 「记录扫描警告」 is on. POSIX: folders without permissions (not as root).
#[cfg(unix)]
#[tokio::test]
async fn unlisted_denied_summary_follows_the_warning_setting() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: plain syscall.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root reads every folder");
        return;
    }
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let denied: Vec<PathBuf> = (0..260).map(|i| root.join(format!("d{i:03}"))).collect();
    for d in &denied {
        std::fs::create_dir_all(d).unwrap();
        std::fs::write(d.join("f"), b"x").unwrap();
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    for enabled in [true, false] {
        let s = c.ok("settings.get", json!({})).await;
        c.ok(
            "settings.update",
            json!({"values": {"general.log_scan_warnings": enabled}, "version": s["version"]}),
        )
        .await;
        let st = c
            .ok(
                "scan.start",
                json!({"roots": [root], "workers": 0, "auto_snapshot": false}),
            )
            .await;
        let id = st["scan_id"].as_str().unwrap().to_owned();
        let fin = wait_done(&c, &id).await;
        assert_eq!(fin["errors"], 260);
        let summary: Vec<String> = events_of(&id)
            .await
            .into_iter()
            .filter(|m| m.starts_with("另有"))
            .collect();
        let want = if enabled {
            vec!["另有 60 个文件夹无法访问，已计入未知空间".to_owned()]
        } else {
            vec![]
        };
        assert_eq!(summary, want, "log_scan_warnings = {enabled}");
    }
    for d in &denied {
        let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755));
    }
    c.ok("settings.reset", json!({"category": "general"})).await;
    srv.shutdown();
}

/// A snapshot holding only its root, saved in `state` (None: a snapshot without the field).
fn save_snapshot(path: &Path, root: &str, state: Option<&str>) {
    let store = sk_store::testing::build_with(&sk_store::testing::d([]), root, 4096, &[]);
    let mut stats = serde_json::Map::new();
    if let Some(s) = state {
        stats.insert("state".into(), Value::from(s));
    }
    let opts = SaveOptions {
        roots: vec![root.to_owned()],
        stats,
        ..SaveOptions::default()
    };
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    snapshot::save(&store.read(), path, &opts).unwrap();
}

#[tokio::test]
async fn expected_items_none_without_complete_snapshot() {
    let _g = SERIAL.lock().await;
    init();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    std::fs::create_dir(&root).unwrap();
    let roots = roots_of(&root);
    save_snapshot(&sk_server::scans::auto_path(&roots), &roots[0], Some("failed"));
    assert_eq!(sk_server::scans::last_item_count(&roots), None);
}

#[tokio::test]
async fn loaded_snapshot_keeps_its_state() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    std::fs::create_dir(&root).unwrap();
    let root = root.to_string_lossy().into_owned();
    for (i, (state, want, progress)) in [
        (Some("done"), "done", json!(1.0)),
        (Some("cancelled"), "cancelled", Value::Null),
        (Some("failed"), "failed", Value::Null),
        (None, "done", json!(1.0)),
    ]
    .into_iter()
    .enumerate()
    {
        let path = tmp.path().join(format!("s{i}.sks"));
        save_snapshot(&path, &root, state);
        let st = c
            .ok("snapshot.load", json!({"path": path.to_string_lossy()}))
            .await;
        assert_eq!(
            (st["state"].as_str().unwrap(), &st["progress"]),
            (want, &progress),
            "saved as {state:?}"
        );
    }
    srv.shutdown();
}
