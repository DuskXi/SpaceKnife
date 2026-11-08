//! Port of 0.3.4's `tests/test_memory.py` (docs/memory.md): bounded compute, latest-wins calls,
//! bounded view history and name-index cache, closed scans freed and remembered by the memory
//! watchdog, the idle trim rule. 0.3.4 replaced the process-memory reader and the heap calls with
//! fakes; the Rust watchdog reads the real process, so its warning path is not reachable from a
//! test (a small test process stays far below the 1.5 GiB slack): what is ported is the rule that
//! keeps it quiet and the accounting it judges by.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;
use sk_server::compute;

/// The compute pool is process-wide: these tests take it one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compute_pool_bounds_concurrency() {
    let _g = SERIAL.lock().await;
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let jobs: Vec<_> = (0..compute::workers() * 4)
        .map(|_| {
            let (running, peak) = (running.clone(), peak.clone());
            tokio::spawn(compute::compute(move || {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(50));
                running.fetch_sub(1, Ordering::SeqCst);
            }))
        })
        .collect();
    for j in jobs {
        j.await.unwrap().unwrap();
    }
    let peak = peak.load(Ordering::SeqCst);
    assert!(peak >= 1 && peak <= compute::workers(), "peak {peak}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn latest_drops_superseded_queued_calls() {
    let _g = SERIAL.lock().await;
    // occupy every worker so the next calls queue
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let blockers: Vec<_> = (0..compute::workers())
        .map(|_| {
            let gate = gate.clone();
            tokio::spawn(compute::compute(move || {
                let (lock, cv) = &*gate;
                let mut open = lock.lock().unwrap();
                let end = Instant::now() + Duration::from_secs(5);
                while !*open && Instant::now() < end {
                    open = cv.wait_timeout(open, Duration::from_millis(50)).unwrap().0;
                }
            }))
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let latest: Arc<compute::Latest<&'static str>> = Arc::new(compute::Latest::new());
    let ran = Arc::new(Mutex::new(Vec::new()));
    let mut calls = Vec::new();
    for i in 0..5 {
        let (latest, ran) = (latest.clone(), ran.clone());
        calls.push(tokio::spawn(async move {
            latest.run("k", move || ran.lock().unwrap().push(i)).await
        }));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    {
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    let mut results = Vec::new();
    for c in calls {
        results.push(c.await.unwrap());
    }
    for b in blockers {
        b.await.unwrap().unwrap();
    }
    assert_eq!(*ran.lock().unwrap(), vec![4], "only the newest queued call runs");
    for r in &results[..4] {
        assert_eq!(r.as_ref().err().map(|e| e.code), Some(-32800), "superseded");
    }
    assert!(results[4].is_ok());
    assert_eq!(latest.len(), 0);
}

/// Zooming in and out more often than the history keeps: the way back is HISTORY steps long.
#[tokio::test]
async fn view_history_is_bounded() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    // no monitor: a folder it lists again (Linux: changed within the scan's first second) gets a
    // new id, and the zooms below go by id
    let s = c.ok("settings.get", json!({})).await;
    c.ok(
        "settings.update",
        json!({"values": {"scan.monitor": false}, "version": s["version"]}),
    )
    .await;
    let st = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 0, "auto_snapshot": false}),
        )
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    wait_done(&c, &id).await;
    let a = c
        .ok("tree.find", json!({"scan_id": id, "path": facts.root.join("a")}))
        .await["node"]["id"]
        .clone();
    let v = c
        .ok(
            "view.open",
            json!({"scan_id": id, "viewport": {"width": 400, "height": 300}}),
        )
        .await;
    let view = v["state"]["view_id"].as_str().unwrap().to_owned();
    let history = sk_core::view::state::HISTORY;
    for i in 0..history + 50 {
        let r = if i % 2 == 0 {
            c.ok("view.zoom", json!({"view_id": view, "key": a})).await
        } else {
            c.ok("view.home", json!({"view_id": view})).await
        };
        let want = if i % 2 == 0 { &a } else { &r["state"]["zoom_node"] };
        assert_eq!(&r["state"]["zoom_node"], want, "step {i}: {}", r["state"]);
        assert_eq!(r["state"]["can_back"], true, "step {i}");
    }
    let mut back = 0;
    loop {
        let r = c.ok("view.back", json!({"view_id": view})).await;
        back += 1;
        if r["state"]["can_back"] != true {
            break;
        }
        assert!(back <= history + 1, "more than {history} steps back");
    }
    assert_eq!(back, history);
    c.drain_binary().await;
    c.ok("settings.reset", json!({"category": "scan"})).await;
    srv.shutdown();
}

/// The name index takes the store per call and holds none of it.
#[test]
fn name_index_does_not_keep_its_store_alive() {
    let store = sk_store::testing::random_store(3, 50);
    let index = sk_core::filter::names::NameIndex::new();
    {
        let nodes = store.read();
        index.update(&nodes, nodes.len());
        assert!(index.len() > 0);
    }
    let n = index.len();
    drop(store);
    assert_eq!(index.len(), n);
    assert!(index.memory_bytes() > 0);
}

/// The cache of query results is bounded (by count, and by bytes): asking ever new questions does
/// not grow it. (0.3.4 lowered the byte bound to see it; the Rust one, 256 MiB, is a constant.)
#[test]
fn name_index_cache_is_bounded() {
    let store = sk_store::testing::random_store(7, 400);
    let nodes = store.read();
    let index = sk_core::filter::names::NameIndex::new();
    index.update(&nodes, nodes.len());
    let ask = |q: &str| {
        let _ = index.ascii_tiers(&nodes, q, nodes.len(), true);
    };
    let queries: Vec<String> = (0..200).map(|i| format!("{}{}", (b'a' + (i % 26) as u8) as char, i)).collect();
    for q in &queries[..20] {
        ask(q);
    }
    let settled = index.memory_bytes();
    for q in &queries[20..] {
        ask(q);
    }
    let after = index.memory_bytes();
    assert!(after <= settled * 2, "the cache grew from {settled} to {after} bytes");
    assert!(index.memory_bytes() > 0);
}

/// Closing a scan frees it, and its size is remembered for the watchdog (the heap may keep that
/// much for the next scan).
#[tokio::test]
async fn closing_a_scan_gives_the_heap_back() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let st = c
        .ok("scan.start", json!({"roots": [facts.root], "auto_snapshot": false}))
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    wait_done(&c, &id).await;
    let h = srv.server.scans.get(&id).unwrap();
    h.finished().await;
    let size = h.store.ram_bytes();
    let weak = Arc::downgrade(&h);
    drop(h);
    srv.server.scans.closed.lock().clear();
    c.ok("scan.close", json!({"scan_id": id})).await;
    let closed = srv.server.scans.closed.lock().clone();
    assert_eq!(closed.len(), 1);
    assert!(closed[0].1 > 0 && closed[0].1 == size, "{closed:?} vs {size}");
    assert_eq!(sk_server::diag::retained(&srv.server), size as u64);
    for _ in 0..100 {
        if weak.upgrade().is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(weak.upgrade().is_none(), "the closed scan is still referenced");
    sk_core::memory::release(); // best effort: never fails
    srv.shutdown();
}

#[tokio::test]
async fn watchdog_allows_for_what_closed_scans_leave_in_the_heap() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let two = 2usize << 30;
    // a 2 GiB scan closed a minute ago counts
    *srv.server.scans.closed.lock() = vec![(Instant::now() - Duration::from_secs(60), two)];
    assert_eq!(sk_server::diag::retained(&srv.server), two as u64);
    // an hour later it no longer does
    if let Some(old) = Instant::now().checked_sub(Duration::from_secs(2 * 3600)) {
        *srv.server.scans.closed.lock() = vec![(old, two)];
        assert_eq!(sk_server::diag::retained(&srv.server), 0);
    }
    srv.server.scans.closed.lock().clear();
    srv.shutdown();
}

/// A process within what its scans account for plus the slack is not reported; the report names
/// the accounting.
#[tokio::test]
async fn watchdog_stays_quiet_within_its_allowance() {
    let _g = SERIAL.lock().await;
    let srv = server().await;
    let mem = sk_core::memory::process_memory();
    if cfg!(any(windows, target_os = "linux")) {
        assert!(mem.private.is_some_and(|p| p > 0) && mem.resident.is_some_and(|r| r > 0));
    }
    if mem.private.is_some_and(|p| p < sk_server::diag::SLACK) {
        let mut last = 0u64;
        assert!(!sk_server::diag::check(&srv.server, &mut last));
    }
    let text = sk_server::diag::report(&srv.server);
    assert!(text.contains("accounted") && text.contains("compute"), "{text}");
    srv.shutdown();
}

/// Idle: the heap is trimmed once, then again only after it grew by TRIM_GROWTH.
#[test]
fn the_heap_is_trimmed_while_idle_once_it_grew() {
    use sk_server::diag::{TRIM_GROWTH, trim_due};
    let gib = 1u64 << 30;
    let mut trimmed = None;
    assert!(trim_due(gib, &mut trimmed), "never trimmed yet");
    trimmed = Some(gib);
    assert!(!trim_due(gib, &mut trimmed), "no growth since");
    assert!(!trim_due(gib + TRIM_GROWTH, &mut trimmed));
    assert!(trim_due(gib + TRIM_GROWTH + 1, &mut trimmed));
    // growth counts from the lowest point since the last trim
    let mut trimmed = Some(gib);
    assert!(!trim_due(gib - (100 << 20), &mut trimmed));
    assert_eq!(trimmed, Some(gib - (100 << 20)));
    assert!(trim_due(gib - (100 << 20) + TRIM_GROWTH + 1, &mut trimmed));
    sk_core::memory::trim(); // glibc: malloc_trim; elsewhere nothing; never fails
}
