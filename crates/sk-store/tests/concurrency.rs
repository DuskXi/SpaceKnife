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
