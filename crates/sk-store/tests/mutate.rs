//! A tree changed in place by relists and stat updates equals a fresh scan of the disk
//! (test_store_mutate.py, test_compact.py). The "disk" is an in-memory spec tree.

use sk_store::mutate::{self, can_relist, compact, gone_count};
use sk_store::testing::{self, Rng, Spec, d, f, facts, relist_from_spec, scan_spec};
use sk_store::*;

fn disk() -> Spec {
    let many: Vec<(String, Spec)> = (0..30)
        .map(|i| (format!("f{i:03}.log"), f(100 + i)))
        .collect();
    let mut spec = d([
        (
            "a",
            d([
                ("one.bin", f(1000)),
                ("two.bin", f(5000)),
                ("sub", d([("movie.mp4", f(123456))])),
            ]),
        ),
        ("b", d([("inner", d([]))])),
        ("c.txt", f(10)),
        ("中文 文件夹", d([("照片.jpg", f(2048))])),
        (
            "deep",
            d([("l1", d([("l2", d([("l3", d([("x.dat", f(777))]))]))]))]),
        ),
        ("empty.dat", f(0)),
    ]);
    spec.children_mut().push(("many".into(), Spec::Dir(many)));
    spec
}

fn relist_path(store: &Store, spec: &Spec, path: &str) -> mutate::Relist {
    let mut w = store.write();
    let folder = if path.is_empty() {
        w.roots().next().unwrap()
    } else {
        testing::find(&w, path)
    };
    relist_from_spec(&mut w, folder, spec.get(path).unwrap())
}

#[test]
fn relist_follows_the_disk() {
    let mut spec = disk();
    let store = scan_spec(&spec, "/root", 4);
    // create, grow, delete, rename, a new folder with content, a file turned into a folder
    spec.get_mut("deep/l1/l2/l3")
        .unwrap()
        .children_mut()
        .push(("new.bin".into(), f(5000)));
    *spec.get_mut("deep/l1/l2/l3/x.dat").unwrap() = f(70_000);
    spec.get_mut("deep/l1/l2")
        .unwrap()
        .children_mut()
        .push(("fresh".into(), d([("deep", d([("x.txt", f(1234))]))])));
    let a = spec.get_mut("").unwrap().children_mut();
    let pos = a.iter().position(|(n, _)| n == "a").unwrap();
    a[pos].0 = "renamed-folder".into();
    let pos = a.iter().position(|(n, _)| n == "c.txt").unwrap();
    a[pos].1 = d([("now-a-folder.txt", f(3))]);
    for path in ["deep/l1/l2/l3", "deep/l1/l2", ""] {
        relist_path(&store, &spec, path);
    }
    let r = store.read();
    testing::check_invariants(&r).unwrap();
    assert_eq!(facts(&r), facts(&scan_spec(&spec, "/root", 3).read()));
    assert!(gone_count(&r) > 0);
}

#[test]
fn removed_subtree_and_kept_subtree() {
    let mut spec = disk();
    let store = scan_spec(&spec, "/root", 2);
    let kept_below: Vec<String> = {
        let r = store.read();
        let b = testing::find(&r, "b");
        r.children(b).map(|n| r.path_lossy(n)).collect()
    };
    let root = spec.get_mut("").unwrap().children_mut();
    root.retain(|(n, _)| n != "a");
    let rel = relist_path(&store, &spec, "");
    let r = store.read();
    let b = testing::find(&r, "b");
    assert!(rel.moved_to.contains(&b));
    assert_eq!(
        r.children(b).map(|n| r.path_lossy(n)).collect::<Vec<_>>(),
        kept_below
    ); // moved with it
    assert_eq!(rel.removed.len(), 1);
    assert!(rel.removed_alloc[0] > 0);
    assert!(rel.gone > 4); // a/, its old copy, and everything below it
    testing::check_invariants(&r).unwrap();
    assert_eq!(facts(&r), facts(&scan_spec(&spec, "/root", 3).read()));
}

#[test]
fn update_stats_climbs() {
    let spec = disk();
    let store = scan_spec(&spec, "/root", 4);
    let mut w = store.write();
    let node = testing::find(&w, "a/sub/movie.mp4");
    let before_root = w.tot_alloc(1);
    let old = w.alloc(node);
    let delta = mutate::update_stats(&mut w, &[node], &[1_000_000], &[old + 8192], Some(&[5]));
    assert_eq!(delta, vec![8192]);
    assert_eq!(w.tot_alloc(1), before_root + 8192);
    assert_eq!(w.mtime(node), 5);
    testing::check_invariants(&w).unwrap();
}

#[test]
fn grown_files_are_reported() {
    let mut spec = disk();
    let store = scan_spec(&spec, "/root", 4);
    *spec.get_mut("a/one.bin").unwrap() = f(9999);
    let rel = relist_path(&store, &spec, "a");
    assert_eq!(rel.grown.len(), 1);
    assert_eq!(rel.grown_by, vec![9999 - 1000]);
    assert!(rel.added.is_empty() && rel.removed.is_empty());
}

#[test]
fn compact_renumbers_the_live_tree() {
    let mut spec = disk();
    let store = scan_spec(&spec, "/root", 4);
    for path in ["a", "a/sub", "b", "deep/l1", "many", ""] {
        spec.get_mut(path)
            .unwrap()
            .children_mut()
            .push(("added.dat".into(), f(777)));
        relist_path(&store, &spec, path);
    }
    let r = store.read();
    let before = facts(&r);
    let (compacted, remap) = compact(&r).unwrap();
    let c = compacted.read();
    assert_eq!(gone_count(&c), 0);
    assert_eq!(c.len(), r.len() - gone_count(&r));
    assert_eq!(facts(&c), before);
    for (old, &new) in remap.iter().enumerate() {
        if new >= 0 {
            assert_eq!(c.name_bytes(new as NodeId), r.name_bytes(old as NodeId));
        }
    }
    for dd in 0..c.len() as NodeId {
        if c.is_dir(dd) {
            let (lo, hi) = c.child_range(dd);
            assert!(lo > dd || hi == lo); // every range after its folder again
        }
    }
    testing::check_invariants(&c).unwrap();
    drop(c);
    // adopting it
    let v = store.version();
    drop(r);
    store.write().adopt(&compacted);
    assert!(store.version() > v);
    assert_eq!(facts(&store.read()), before);
}

#[test]
fn relist_waits_for_pending_children() {
    let spec = d([("sub", d([]))]);
    let store = Store::new();
    let mut a = store.appender();
    let root = a.add_root_dir("/r").unwrap();
    let b = testing::listing_of(spec.children(), 1, &mut |_| 0);
    a.ingest_listing(root, &b).unwrap();
    assert!(!can_relist(&a, root)); // "sub" is still queued by its id
    let sub = a.children(root).next().unwrap();
    assert!(!can_relist(&a, sub)); // not listed yet itself
}

#[test]
fn a_vanished_root_lists_empty() {
    let spec = disk();
    let store = scan_spec(&spec, "/root", 4);
    let mut w = store.write();
    let rel = mutate::relist(&mut w, 1, &EntryBatch::new()).unwrap();
    assert!(!rel.removed.is_empty());
    assert_eq!(w.children(1).count(), 0);
    assert_eq!(
        (w.tot_size(1), w.tot_alloc(1), w.tot_files(1), w.tot_dirs(1)),
        (0, 0, 0, 0)
    );
    assert_eq!((w.tot_size(0), w.tot_alloc(0), w.tot_files(0)), (0, 0, 0));
    testing::check_invariants(&w).unwrap();
}

#[test]
fn listings_of_folders_deleted_meanwhile_are_skipped() {
    // a worker reads gone/a while gone/ is deleted and the monitor relists the root, marking
    // gone/'s subtree GONE: the stale listing of a/ (and of b/ inside it) is skipped
    let mut spec = d([
        (
            "gone",
            d([("a", d([("b", d([("f.bin", f(100))])), ("g.txt", f(10))]))]),
        ),
        ("keep", d([("k.txt", f(10))])),
    ]);
    let store = Store::new();
    let a_id;
    {
        let mut a = store.appender();
        let root = a.add_root_dir("/r").unwrap();
        let first = a
            .ingest_listing(root, &testing::listing_of(spec.children(), 1, &mut |_| 0))
            .unwrap();
        a.ingest_listing(
            first + 1,
            &testing::listing_of(spec.get("keep").unwrap().children(), 1, &mut |_| 0),
        )
        .unwrap();
        a_id = a
            .ingest_listing(
                first,
                &testing::listing_of(spec.get("gone").unwrap().children(), 1, &mut |_| 0),
            )
            .unwrap();
    }
    // the stale slice: a/ and b/ inside it
    let a_list = testing::listing_of(spec.get("gone/a").unwrap().children(), 1, &mut |_| 0);
    let b_list = testing::listing_of(spec.get("gone/a/b").unwrap().children(), 1, &mut |_| 0);
    let mut all = a_list.clone();
    testing::append_batch(&mut all, &b_list);
    let refs = [
        SliceRef::Node(a_id),
        SliceRef::Entry {
            listing: 0,
            entry: 0,
        },
    ];
    let bounds = [0, a_list.len() as u32, all.len() as u32];

    spec.children_mut().retain(|(n, _)| n != "gone");
    assert!(can_relist(&store.read(), 1)); // the monitor may relist the root: no child is pending
    relist_path(&store, &spec, "");
    let size = store.len();
    let starts = store.appender().ingest_slice(&refs, &bounds, &all).unwrap();
    assert_eq!(starts, vec![None, None]);
    assert_eq!(store.len(), size);
    assert_eq!(
        facts(&store.read()),
        facts(&scan_spec(&spec, "/r", 2).read())
    );
}

// ------------------------------------------------------------------ compaction (test_compact.py)

/// A tree whose folders were listed again many times: renamed, deleted and new entries.
fn churned_store(seed: u64, folders: usize, relists: usize, opts: StoreOptions) -> Store {
    let mut rng = Rng::new(seed);
    let store = Store::with_options(opts);
    let mut a = store.appender();
    let root = a.add_root_dir("/r").unwrap();
    let mut todo = vec![root];
    let mut dirs = vec![root];
    let mut k = 0;
    while let Some(dd) = todo.pop() {
        let m = rng.below(30) as usize;
        let sub = if dirs.len() < folders {
            (rng.below(3) as usize).min(m)
        } else {
            0
        };
        let mut b = EntryBatch::new();
        for i in 0..m {
            let name = format!("名字{k}-{i}.{}", if i % 3 != 0 { "txt" } else { "mp4" });
            if i < sub {
                b.push(Entry::dir(name.as_bytes()));
            } else {
                let size = rng.range(0, 1 << 30);
                b.push(Entry {
                    mtime: rng.next_u64() as u32 >> 1,
                    ..Entry::file(name.as_bytes(), size, size / 4096 * 4096)
                });
            }
        }
        k += 1;
        let start = a.ingest_listing(dd, &b).unwrap();
        let new: Vec<NodeId> = (start..start + sub as NodeId).collect();
        todo.extend(&new);
        dirs.extend(&new);
    }
    drop(a);
    for r in 0..relists {
        let mut w = store.write();
        let live: Vec<NodeId> = dirs
            .iter()
            .copied()
            .filter(|&x| can_relist(&w, x))
            .collect();
        let dd = *rng.pick(&live);
        let mut b = EntryBatch::new();
        for c in w.children(dd) {
            if rng.chance(0.7) {
                let name = w.name_bytes(c).to_vec();
                b.push(Entry {
                    kind: w.kind(c),
                    ..Entry::file(&name, w.size(c), w.alloc(c))
                });
            }
        }
        for i in 0..rng.below(12) {
            let name = format!("new{r}-{i}");
            b.push(if i % 4 != 0 {
                Entry::file(name.as_bytes(), 10, 4096)
            } else {
                Entry::dir(name.as_bytes())
            });
        }
        let rel = mutate::relist(&mut w, dd, &b).unwrap();
        for &p in &rel.pending {
            w.ingest_listing(p, &EntryBatch::new()).unwrap();
        }
        dirs.retain(|&x| !w.flags(x).intersects(Flags::GONE));
        dirs.extend(
            rel.added
                .iter()
                .chain(&rel.moved_to)
                .copied()
                .filter(|&x| w.is_dir(x)),
        );
    }
    store
}

#[test]
fn compact_of_churned_stores() {
    for seed in 0..4 {
        let store = churned_store(seed, 60, 120, StoreOptions::default());
        let r = store.read();
        assert!(gone_count(&r) > 0);
        testing::check_invariants(&r).unwrap();
        let (got, remap) = compact(&r).unwrap();
        let g = got.read();
        assert_eq!(gone_count(&g), 0);
        assert_eq!(facts(&g), facts(&r));
        assert_eq!(remap.iter().filter(|&&x| x >= 0).count(), g.len());
        testing::check_invariants(&g).unwrap();
        // the image is `\0name` per node in the new order
        let image: Vec<u8> = g.name_runs().flatten().copied().collect();
        let mut want = Vec::new();
        for i in 0..g.len() as NodeId {
            want.push(0);
            want.extend_from_slice(g.name_bytes(i));
        }
        assert_eq!(image, want);
    }
}

#[test]
fn compact_reads_names_from_every_heap_chunk() {
    let opts = StoreOptions {
        first_chunk: 256,
        max_chunk: 512,
        ..StoreOptions::default()
    };
    let store = churned_store(9, 30, 60, opts);
    let r = store.read();
    assert!(r.name_chunks() > 5);
    let (got, remap) = compact(&r).unwrap();
    let g = got.read();
    for (old, &new) in remap.iter().enumerate() {
        if new >= 0 {
            assert_eq!(g.name_bytes(new as NodeId), r.name_bytes(old as NodeId));
        }
    }
}

#[test]
fn needs_compaction_counts_gone_rows() {
    let store = Store::new();
    {
        let mut a = store.appender();
        let root = a.add_root_dir("/r").unwrap();
        a.ingest_listing(root, &testing::dirs_and_files("f", 0, 300_000))
            .unwrap();
    }
    assert!(!mutate::needs_compaction(&store.read()));
    let mut w = store.write();
    let half = testing::dirs_and_files("g", 0, 10);
    mutate::relist(&mut w, 1, &half).unwrap(); // every old row is GONE now
    assert!(mutate::needs_compaction(&w));
}

/// A folder relisted with two entries of the very same name (WinBtrfs shows invalid UTF-8 as
/// U+FFFD; the scan lists each by its id) keeps each one's own subtree: they pair in listing
/// order, never both with one old row's children (port of test_case_twins.py
/// `relisting_pairs_names_held_twice_in_order`).
#[test]
fn relisting_pairs_names_held_twice_in_order() {
    let spec = d([
        ("n\u{FFFD}", d([("one", f(1))])),
        ("n\u{FFFD}", d([("two", f(2))])),
    ]);
    let store = scan_spec(&spec, "/root", 1);
    let rel = relist_path(&store, &spec, "");
    assert!(rel.removed.is_empty(), "{rel:?}");
    let r = store.read();
    let root = r.roots().next().unwrap();
    let kept: Vec<Vec<String>> = r
        .children(root)
        .filter(|&c| !r.flags(c).intersects(Flags::GONE))
        .map(|c| r.children(c).map(|g| r.name(g).into_owned()).collect())
        .collect();
    assert_eq!(kept, [vec!["one".to_string()], vec!["two".to_string()]]);
}
