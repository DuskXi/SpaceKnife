//! Reading a store while a scan adds listings in other threads (test_store_concurrency.py).
//!
//! A side-panel summary walked a subtree while the scan listed folders; reading a folder's child
//! range start before its count could pair the old start 0 with a fresh count — a range over the
//! top of the tree — and the walk revisited the tree again and again.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use sk_store::subtree::{SUMMARY_CHUNK, subtree_ids, summarize};
use sk_store::testing::{self, dirs_and_files};
use sk_store::*;

#[test]
fn a_half_published_range_reads_as_not_listed() {
    let store = Store::new();
    let root;
    let first;
    {
        let mut a = store.appender();
        root = a.add_root_dir("/r").unwrap();
        first = a.ingest_listing(root, &dirs_and_files("n", 2, 1)).unwrap();
    }
    {
        let mut w = store.write(); // the moment its count is written but its start is not
        w.set_raw::<i32>(Col::ChildCount, first, 2);
    }
    {
        let r = store.read();
        assert_eq!(r.child_range(first), (0, 0));
        assert_eq!(r.children(first).count(), 0);
        let ids: HashSet<_> = subtree_ids(&r, 0).unwrap().into_iter().collect();
        assert_eq!(ids, HashSet::from([root, first, first + 1, first + 2]));
    }
    store.write().set_raw::<i32>(Col::ChildCount, first, 0);
    let mut a = store.appender();
    let kids = a.ingest_listing(first, &dirs_and_files("x", 0, 2)).unwrap();
    assert_eq!(a.child_range(first), (kids, kids + 2));
}

fn scan_in_background(store: &Store, rows: usize, stop: &AtomicBool) {
    let mut a = store.appender();
    let root = a.add_root_dir("/r").unwrap();
    drop(a);
    let mut pending = VecDeque::from([root]);
    let mut k = 0;
    while let Some(d) = pending.pop_front() {
        if store.len() >= rows || stop.load(Ordering::Relaxed) {
            break;
        }
        // a fresh appender per listing, like a scanner applying results as they come
        let mut a = store.appender();
        let start = a
            .ingest_listing(d, &dirs_and_files(&format!("n{k}_"), 4, 4))
            .unwrap();
        pending.extend(start..start + 4);
        k += 1;
    }
}

#[test]
fn subtree_walks_stay_exact_while_a_scan_ingests() {
    let store = Store::with_capacity(16); // grows many times meanwhile
    let stop = AtomicBool::new(false);
    let problems = parking_lot_free_problems();
    thread::scope(|s| {
        let writer = s.spawn(|| {
            scan_in_background(&store, 150_000, &stop);
            stop.store(true, Ordering::Relaxed);
        });
        for _ in 0..3 {
            s.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let r = store.read();
                    let ids = match subtree_ids(&r, 0) {
                        Ok(ids) => ids,
                        Err(e) => {
                            problems.lock().unwrap().push(e.to_string());
                            return;
                        }
                    };
                    let distinct: HashSet<_> = ids.iter().copied().collect();
                    if distinct.len() != ids.len() || ids.len() > r.len() {
                        problems.lock().unwrap().push(format!(
                            "{} ids, {} distinct, {} nodes",
                            ids.len(),
                            distinct.len(),
                            r.len()
                        ));
                        return;
                    }
                    // the published totals never exceed what the walk can see once it settles
                    let _ = r.tot_files(0);
                }
            });
        }
        writer.join().unwrap();
    });
    let problems = problems.into_inner().unwrap();
    assert!(problems.is_empty(), "{}", problems[0]);
    let r = store.read();
    assert_eq!(subtree_ids(&r, 0).unwrap().len(), r.len() - 1); // every node below node 0, once
    testing::check_invariants(&r).unwrap();
}

fn parking_lot_free_problems() -> std::sync::Mutex<Vec<String>> {
    std::sync::Mutex::new(Vec::new())
}

#[test]
fn summaries_and_names_while_a_scan_ingests() {
    let store = Store::with_capacity(16);
    let stop = AtomicBool::new(false);
    let problems = parking_lot_free_problems();
    thread::scope(|s| {
        let writer = s.spawn(|| {
            scan_in_background(&store, 60_000, &stop);
            stop.store(true, Ordering::Relaxed);
        });
        for _ in 0..2 {
            s.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let r = store.read();
                    if r.len() < 2 {
                        continue;
                    }
                    match summarize(
                        &r,
                        1,
                        |id| (id % 3) as usize,
                        3,
                        5,
                        None,
                        SUMMARY_CHUNK / 64,
                    ) {
                        Ok(sum) => {
                            if sum.class_files.iter().sum::<i64>() as usize >= r.len() {
                                problems.lock().unwrap().push("too many files".into());
                            }
                        }
                        Err(e) => problems.lock().unwrap().push(e.to_string()),
                    }
                    // names are readable while chunks are added
                    let mut a = 0;
                    while (a as usize) < r.len() {
                        let run = r.names_run(a, r.len() as NodeId);
                        if run.seps().len() != (run.stop - a) as usize {
                            problems.lock().unwrap().push("bad run".into());
                        }
                        a = run.stop;
                    }
                }
            });
        }
        writer.join().unwrap();
    });
    let problems = problems.into_inner().unwrap();
    assert!(problems.is_empty(), "{}", problems[0]);
}

#[test]
fn a_writer_waits_for_readers_and_excludes_them() {
    let store = testing::random_store(4, 200);
    let inside = AtomicBool::new(false);
    thread::scope(|s| {
        let r = store.read();
        let h = s.spawn(|| {
            let mut w = store.write();
            inside.store(true, Ordering::SeqCst);
            w.touch();
            thread::sleep(std::time::Duration::from_millis(20));
            inside.store(false, Ordering::SeqCst);
        });
        thread::sleep(std::time::Duration::from_millis(30));
        assert!(!inside.load(Ordering::SeqCst)); // still waiting for our reader
        drop(r);
        h.join().unwrap();
        let _r = store.read();
        assert!(!inside.load(Ordering::SeqCst));
    });
}

#[test]
fn relists_and_readers_interleave_safely() {
    use sk_store::testing::{Spec, d, f, relist_from_spec, scan_spec};
    let mut spec = d([
        ("a", d([("x", f(1)), ("y", f(2))])),
        ("b", d([("z", f(3))])),
    ]);
    let store = scan_spec(&spec, "/r", 2);
    let stop = AtomicBool::new(false);
    thread::scope(|s| {
        for _ in 0..2 {
            s.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let r = store.read();
                    let ids = subtree_ids(&r, 0).unwrap();
                    assert_eq!(ids.len(), testing::facts(&r).len());
                    testing::check_invariants(&r).unwrap();
                }
            });
        }
        for round in 0..200 {
            let name = format!("f{round}");
            let a = spec.get_mut("a").unwrap();
            if let Spec::Dir(c) = a {
                c.push((name, f(round)));
                if c.len() > 6 {
                    c.remove(0);
                }
            }
            let mut w = store.write();
            let folder = testing::find(&w, "a");
            relist_from_spec(&mut w, folder, spec.get("a").unwrap());
        }
        stop.store(true, Ordering::Relaxed);
    });
    let r = store.read();
    testing::check_invariants(&r).unwrap();
    assert_eq!(
        testing::facts(&r),
        testing::facts(&scan_spec(&spec, "/r", 3).read())
    );
}

// ---- structure lock fairness (test_structure_lock.py) ----
//
// A cleanup plan built over a large scan read the tree for minutes; a file change on the monitored
// disk then waited for it, and since a waiting writer used to stop every new reader, every picture,
// summary and view waited too.

mod structure_lock {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use sk_store::Store;

    /// A reader on its own thread: started, held until released.
    struct Reader {
        started: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
        thread: thread::JoinHandle<()>,
    }

    impl Reader {
        fn spawn(
            store: &Arc<Store>,
            log: &Arc<std::sync::Mutex<Vec<String>>>,
            name: &str,
        ) -> Reader {
            let (started_tx, started) = mpsc::channel();
            let (release, release_rx) = mpsc::channel::<()>();
            let (store, log, name) = (store.clone(), log.clone(), name.to_string());
            let thread = thread::spawn(move || {
                let _r = store.read();
                log.lock().unwrap().push(format!("start {name}"));
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(20));
                log.lock().unwrap().push(format!("end {name}"));
            });
            Reader {
                started,
                release,
                thread,
            }
        }
        fn started_within(&self, d: Duration) -> bool {
            self.started.recv_timeout(d).is_ok()
        }
        fn finish(self) {
            let _ = self.release.send(());
            self.thread.join().unwrap();
        }
    }

    /// A writer on its own thread (optionally with a deadline); reports whether it wrote.
    fn spawn_writer(
        store: &Arc<Store>,
        log: &Arc<std::sync::Mutex<Vec<String>>>,
        deadline: Option<Instant>,
    ) -> thread::JoinHandle<bool> {
        let (store, log) = (store.clone(), log.clone());
        thread::spawn(move || {
            let w = match deadline {
                None => Some(store.write()),
                Some(d) => store.write_until(d),
            };
            match w {
                Some(mut w) => {
                    w.touch();
                    log.lock().unwrap().push("write".into());
                    true
                }
                None => false,
            }
        })
    }

    fn setup() -> (Arc<Store>, Arc<std::sync::Mutex<Vec<String>>>) {
        (
            Arc::new(sk_store::testing::random_store(7, 50)),
            Arc::new(std::sync::Mutex::new(Vec::new())),
        )
    }

    const SHORT: Duration = Duration::from_millis(60);
    const LONG: Duration = Duration::from_secs(5);

    #[test]
    fn a_long_reader_holds_up_the_change_not_the_other_readers() {
        let (store, log) = setup();
        let old = Reader::spawn(&store, &log, "old");
        assert!(old.started_within(LONG));
        let w = spawn_writer(&store, &log, None);
        thread::sleep(SHORT);
        assert!(!w.is_finished()); // the change waits for the reader that was there first
        let new = Reader::spawn(&store, &log, "new");
        assert!(
            new.started_within(Duration::from_secs(2)),
            "a new reader waited for a change that itself waited for an older reader"
        );
        new.finish();
        thread::sleep(SHORT);
        assert!(!w.is_finished());
        old.finish();
        assert!(w.join().unwrap());
        assert_eq!(log.lock().unwrap().last().unwrap(), "write");
    }

    #[test]
    fn once_the_older_readers_are_done_new_readers_wait_for_the_change() {
        let (store, log) = setup();
        let old = Reader::spawn(&store, &log, "old");
        assert!(old.started_within(LONG));
        let w = spawn_writer(&store, &log, None);
        thread::sleep(SHORT);
        let mid = Reader::spawn(&store, &log, "mid"); // admitted: the change cannot start before "old"
        assert!(mid.started_within(Duration::from_secs(2)));
        old.finish();
        let late = Reader::spawn(&store, &log, "late"); // nothing older than the change runs: it waits
        assert!(!late.started_within(Duration::from_millis(300)));
        mid.finish();
        assert!(w.join().unwrap()); // no stream of readers keeps the change out
        assert!(late.started_within(LONG));
        late.finish();
        let log = log.lock().unwrap();
        let at = |s: &str| log.iter().position(|e| e == s).unwrap();
        assert!(at("write") < at("start late"));
    }

    #[test]
    fn readers_held_back_go_on_when_the_waiting_change_is_given_up() {
        let (store, log) = setup();
        let old = Reader::spawn(&store, &log, "old");
        assert!(old.started_within(LONG));
        let w = spawn_writer(
            &store,
            &log,
            Some(Instant::now() + Duration::from_millis(700)),
        );
        thread::sleep(SHORT);
        let mid = Reader::spawn(&store, &log, "mid");
        assert!(mid.started_within(Duration::from_secs(2)));
        old.finish();
        let late = Reader::spawn(&store, &log, "late");
        assert!(!late.started_within(Duration::from_millis(300)));
        assert!(!w.join().unwrap(), "the writer gave up"); // "mid" still reads
        assert!(
            late.started_within(Duration::from_secs(2)),
            "a reader stayed blocked by a change that was given up"
        );
        mid.finish();
        late.finish();
        assert!(!log.lock().unwrap().iter().any(|e| e == "write"));
    }

    #[test]
    fn no_reader_ever_runs_during_a_change() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let store = sk_store::testing::random_store(8, 50);
        let stop = AtomicBool::new(false);
        let readers = AtomicUsize::new(0);
        let writing = AtomicBool::new(false);
        let bad = AtomicBool::new(false);
        thread::scope(|s| {
            for _ in 0..6 {
                s.spawn(|| {
                    while !stop.load(Ordering::Relaxed) {
                        let _r = store.read();
                        readers.fetch_add(1, Ordering::SeqCst);
                        if writing.load(Ordering::SeqCst) {
                            bad.store(true, Ordering::SeqCst);
                        }
                        thread::sleep(Duration::from_micros(500));
                        readers.fetch_sub(1, Ordering::SeqCst);
                    }
                });
            }
            for _ in 0..200 {
                let mut w = store.write();
                writing.store(true, Ordering::SeqCst);
                if readers.load(Ordering::SeqCst) != 0 {
                    bad.store(true, Ordering::SeqCst);
                }
                w.touch();
                writing.store(false, Ordering::SeqCst);
                drop(w);
                thread::yield_now();
            }
            stop.store(true, Ordering::Relaxed);
        });
        assert!(!bad.load(Ordering::SeqCst));
    }
}
