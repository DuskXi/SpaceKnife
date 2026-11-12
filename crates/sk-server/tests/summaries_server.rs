//! Port of the cache half of 0.3.4's `tests/test_summary.py`: a scan computes the summary of a
//! node (details panel, Disks page, end of a scan) once per store version, concurrent callers share
//! one walk, and the cache does not keep a closed scan alive. 0.3.4 counted the walks with a spy;
//! here a walk is a new `Arc<Summary>` and a cached answer is the same one.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use serde_json::json;
use sk_core::summary::Walk;
use sk_server::scans::ScanHandle;
use sk_store::subtree::Summary;

fn full(w: Walk) -> Arc<Summary> {
    match w {
        Walk::Full(s) => s,
        Walk::ByExtension(..) => panic!("a summary by extension only"),
        Walk::Failed(_) => panic!("the walk failed"),
    }
}

async fn scanned(
    c: &Client,
    srv: &sk_server::ServerHandle,
) -> (Arc<ScanHandle>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let st = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    wait_done(c, &id).await;
    let h = srv.server.scans.get(&id).unwrap();
    h.finished().await;
    (h, tmp)
}

fn root_of(h: &ScanHandle) -> u32 {
    h.roots.read()[0].node
}

#[tokio::test]
async fn the_server_computes_a_summary_once_per_store_version() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let (h, tmp) = scanned(&c, &srv).await;
    let root = root_of(&h);
    // the details panel's two pages and the Disks page: one walk
    let five = full(h.summary(root, 5, true));
    let fifty = full(h.summary(root, 50, true));
    let none = full(h.summary(root, 0, true));
    assert!(Arc::ptr_eq(&five, &fifty) && Arc::ptr_eq(&five, &none));
    // more than was kept: computed again
    let more = full(h.summary(root, 500, true));
    assert!(!Arc::ptr_eq(&five, &more));
    let again = full(h.summary(root, 5, true)); // still a cached walk
    assert!(Arc::ptr_eq(&again, &five) || Arc::ptr_eq(&again, &more));
    // the store changed (the monitor applied a new file): a new version, a new walk
    let added = tmp.path().join("root").join("a").join("new.bin");
    std::fs::write(&added, vec![b'n'; 4096]).unwrap();
    let mut seen = false;
    for _ in 0..200 {
        let f = c
            .ok("tree.find", json!({"scan_id": h.id, "path": added}))
            .await;
        if !f["node"].is_null() {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "the monitor applied the change");
    let after = full(h.summary(root, 5, true));
    assert!(!Arc::ptr_eq(&after, &more));
    // the user gave a folder a class: computed again
    c.ok(
        "classes.folder",
        json!({"path": tmp.path().join("root").join("a"), "class_id": "video"}),
    )
    .await;
    let classed = full(h.summary(root, 5, true));
    assert!(!Arc::ptr_eq(&classed, &after));
    // a running scan's summaries are never kept
    let a = full(h.summary(root, 5, false));
    let b = full(h.summary(root, 5, false));
    assert!(!Arc::ptr_eq(&a, &b));
    c.ok(
        "classes.folder",
        json!({"path": tmp.path().join("root").join("a")}),
    )
    .await;
    srv.shutdown();
}

#[tokio::test]
async fn concurrent_callers_share_one_walk() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let (h, _tmp) = scanned(&c, &srv).await;
    let root = root_of(&h);
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let h = h.clone();
            std::thread::spawn(move || full(h.summary(root, 5, true)))
        })
        .collect();
    let got: Vec<Arc<Summary>> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(
        got.iter().all(|g| Arc::ptr_eq(g, &got[0])),
        "one walk for all four"
    );
    srv.shutdown();
}

/// 0.3.4 counted at most one walk at a time per scan with a spy; the Rust cache holds the scan's
/// summary lock across the walk. What is observable: concurrent calls on different nodes give
/// what the same calls give one by one.
#[tokio::test]
async fn summaries_of_different_nodes_run_one_at_a_time() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let (h, tmp) = scanned(&c, &srv).await;
    let dirs: Vec<u32> = ["a", "a/sub", "deep", "deep/l1", "many", "b"]
        .iter()
        .map(|p| {
            let nodes = h.store.read();
            let path = p
                .split('/')
                .fold(tmp.path().join("root"), |acc, part| acc.join(part));
            (0..nodes.len() as u32)
                .find(|&n| nodes.path(n) == path)
                .unwrap()
        })
        .collect();
    let alone: Vec<(u32, usize)> = dirs
        .iter()
        .map(|&d| {
            let s = full(h.summary(d, 5, false));
            (s.latest_mtime, s.top_files.len())
        })
        .collect();
    let threads: Vec<_> = dirs
        .iter()
        .map(|&d| {
            let h = h.clone();
            std::thread::spawn(move || {
                let s = full(h.summary(d, 5, true));
                (s.latest_mtime, s.top_files.len())
            })
        })
        .collect();
    let together: Vec<(u32, usize)> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(together, alone);
    srv.shutdown();
}

#[tokio::test]
async fn the_summary_cache_does_not_keep_a_closed_scan_alive() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let (h, _tmp) = scanned(&c, &srv).await;
    let root = root_of(&h);
    let _ = full(h.summary(root, 5, true));
    let id = h.id.clone();
    let weak = Arc::downgrade(&h);
    drop(h);
    c.ok("scan.close", json!({"scan_id": id})).await;
    let mut gone = false;
    for _ in 0..100 {
        if weak.upgrade().is_none() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone, "the closed scan is still referenced");
    srv.shutdown();
}

/// A summary reads the scan's roots (its volume) while its caller may hold them already (the Disks
/// page, the end of a whole-volume scan); the monitor's write of them (free space, once a second)
/// queued in between must not hold that read back, or both wait for ever.
#[tokio::test]
async fn the_roots_are_read_again_while_a_writer_waits() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let (h, _tmp) = scanned(&c, &srv).await;
    let root = root_of(&h);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let reader = {
        let h = h.clone();
        std::thread::spawn(move || {
            let held = h.roots.read();
            held_tx.send(()).unwrap();
            go_rx.recv().unwrap(); // a writer waits now
            let found = h.root_of(&h.store.read(), root).map(|r| r.node);
            drop(held);
            done_tx.send(found).unwrap();
        })
    };
    held_rx.recv().unwrap();
    let writer = {
        let h = h.clone();
        std::thread::spawn(move || h.roots.write().len())
    };
    std::thread::sleep(Duration::from_millis(300)); // parked behind the reader's guard
    go_tx.send(()).unwrap();
    let found = done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("root_of waited behind the queued writer");
    assert_eq!(found, Some(root));
    reader.join().unwrap();
    assert_eq!(writer.join().unwrap(), 1);
    srv.shutdown();
}
