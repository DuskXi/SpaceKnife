//! The node store: ingest, totals, queries, growth, names (test_store.py, test_store_growth.py,
//! test_store_layout.py, test_memory*.py).

use std::sync::Arc;

use sk_store::testing::{self, Rng, batch, d, dirs_and_files, f, named_batch};
use sk_store::*;

fn plain() -> StoreOptions {
    let policy = Arc::new(Policy::new());
    policy.set_physical_memory(Some(None)); // no budget: plain memory
    StoreOptions {
        policy,
        ..StoreOptions::default()
    }
}

#[test]
fn ingest_builds_contiguous_children_and_totals() {
    let s = Store::new();
    let mut a = s.appender();
    let root = a.add_root_dir("/data").unwrap();
    let docs = a
        .ingest_listing(
            root,
            &named_batch(
                &["docs", "a.txt", "b.bin"],
                &[Kind::Dir, Kind::File, Kind::File],
                &[0, 10, 5000],
                &[],
            ),
        )
        .unwrap();
    assert_eq!(
        a.children(root).collect::<Vec<_>>(),
        vec![docs, docs + 1, docs + 2]
    );
    a.ingest_listing(docs, &named_batch(&["x.pdf"], &[Kind::File], &[9000], &[]))
        .unwrap();
    assert_eq!(a.effective_size(root), (14010, 4096 + 8192 + 12288));
    assert_eq!(a.effective_size(docs), (9000, 12288));
    let v = a.view(root);
    assert_eq!((v.tot_files, v.tot_dirs), (3, 1));
    assert!(
        a.path_lossy(docs + 3)
            .replace('\\', "/")
            .ends_with("/data/docs/x.pdf")
    );
    assert!(!a.view(docs).flags.intersects(Flags::PENDING));
    assert_eq!(a.view(root).flags, Flags::NONE);
    assert_eq!(a.view(0).tot_dirs, 2); // the root and docs
}

#[test]
fn hardlink_duplicates_do_not_count() {
    let s = Store::new();
    let mut a = s.appender();
    let root = a.add_root_dir("/r").unwrap();
    a.ingest_listing(
        root,
        &named_batch(
            &["a", "a-link"],
            &[Kind::File; 2],
            &[8192, 8192],
            &[Flags::NONE, Flags::HARDLINK_DUP],
        ),
    )
    .unwrap();
    assert_eq!(a.effective_size(root), (8192, 8192));
    assert_eq!(a.effective_size(root + 2), (0, 0));
}

#[test]
fn double_listing_and_non_dir_are_rejected() {
    let s = Store::new();
    let mut a = s.appender();
    let root = a.add_root_dir("/r").unwrap();
    a.ingest_listing(root, &batch(&[Kind::File], &[1], &[]))
        .unwrap();
    assert_eq!(
        a.ingest_listing(root, &batch(&[Kind::File], &[1], &[])),
        Err(StoreError::AlreadyListed(root))
    );
    assert_eq!(
        a.ingest_listing(root + 1, &batch(&[Kind::File], &[1], &[])),
        Err(StoreError::NotADirectory(root + 1))
    );
    assert_eq!(a.add_root_dir("/late"), Err(StoreError::RootsAfterScan));
}

#[test]
fn mount_points_are_never_pending() {
    let s = Store::new();
    let mut a = s.appender();
    let root = a.add_root_dir("/r").unwrap();
    let first = a
        .ingest_listing(
            root,
            &named_batch(
                &["m", "d"],
                &[Kind::Dir, Kind::Dir],
                &[0, 0],
                &[Flags::MOUNT, Flags::NONE],
            ),
        )
        .unwrap();
    assert!(!a.flags(first).intersects(Flags::PENDING));
    assert!(a.flags(first + 1).intersects(Flags::PENDING));
}

#[test]
fn reaggregate_matches_incremental() {
    for seed in 1..=3 {
        let s = testing::random_store(seed, 300);
        let mut a = s.appender();
        let before: Vec<_> = (0..a.len() as NodeId)
            .map(|i| (a.tot_size(i), a.tot_alloc(i), a.tot_files(i), a.tot_dirs(i)))
            .collect();
        a.reaggregate();
        let after: Vec<_> = (0..a.len() as NodeId)
            .map(|i| (a.tot_size(i), a.tot_alloc(i), a.tot_files(i), a.tot_dirs(i)))
            .collect();
        assert_eq!(before, after, "seed {seed}");
        testing::check_invariants(&a).unwrap();
    }
}

#[test]
fn sorted_children_and_find() {
    let s = Store::new();
    let mut a = s.appender();
    let root = a.add_root_dir("/r").unwrap();
    a.ingest_listing(
        root,
        &named_batch(
            &["b", "A", "c"],
            &[Kind::File; 3],
            &[10, 9000, 100_000],
            &[],
        ),
    )
    .unwrap();
    let by_size: Vec<_> = a
        .sorted_children(root, SortBy::Size, true)
        .into_iter()
        .map(|i| a.name(i).into_owned())
        .collect();
    assert_eq!(by_size, ["c", "A", "b"]);
    let by_name: Vec<_> = a
        .sorted_children(root, SortBy::Name, false)
        .into_iter()
        .map(|i| a.name(i).into_owned())
        .collect();
    assert_eq!(by_name, ["A", "b", "c"]);
    assert_eq!(a.find(&a.path_bytes(root + 2)), Some(root + 2));
    assert_eq!(a.find_path(&a.path(root + 2)), Some(root + 2));
    let mut missing = a.path_bytes(root);
    missing.extend_from_slice(b"/missing");
    assert_eq!(a.find(&missing), None);
}

#[test]
fn find_prefers_exact_case_and_refuses_ambiguity() {
    let s = Store::new();
    let mut a = s.appender();
    let root = a.add_root_dir("/r").unwrap();
    let first = a
        .ingest_listing(
            root,
            &named_batch(&["A", "a", "Solo"], &[Kind::File; 3], &[1, 2, 3], &[]),
        )
        .unwrap();
    assert_eq!(a.find(b"/r/A"), Some(first));
    assert_eq!(a.find(b"/r/a"), Some(first + 1));
    if cfg!(windows) {
        assert_eq!(a.find(b"/r/solo"), Some(first + 2)); // one entry under normcase
    }
}

#[test]
fn builder_roundtrip() {
    let s = testing::random_store(7, 50);
    let r = s.read();
    let n = r.len();
    let image: Vec<u8> = r.name_runs().flatten().copied().collect();
    let mut b = StoreBuilder::with_image(n, image, StoreOptions::default());
    b.set_ext_table(r.ext_table().iter().map(|e| e.to_string()).collect());
    {
        let c = b.columns();
        for i in 0..n {
            let id = i as NodeId;
            c.parent[i] = r.parent(id);
            c.depth[i] = r.depth(id);
            c.kind[i] = r.kind_raw(id);
            c.flags[i] = r.flags_raw(id);
            c.name_len[i] = r.name_len(id);
            c.size[i] = r.size(id);
            c.alloc[i] = r.alloc(id);
            c.child_start[i] = r.child_start(id);
            c.child_count[i] = r.child_count(id);
            c.tot_size[i] = r.tot_size(id);
            c.tot_alloc[i] = r.tot_alloc(id);
            c.tot_files[i] = r.tot_files(id);
            c.tot_dirs[i] = r.tot_dirs(id);
            c.nlink[i] = r.nlink(id);
        }
    }
    let clone = b.finish().unwrap();
    let c = clone.read();
    testing::assert_same_store(&r, &c);
    assert_eq!(c.path_bytes(n as NodeId - 1), r.path_bytes(n as NodeId - 1));
    assert!(clone.appender().is_settled());
}

#[test]
fn builder_rejects_inconsistent_columns() {
    let mut b = StoreBuilder::new(2, 3, StoreOptions::default());
    {
        let c = b.columns();
        c.parent[0] = -1;
        c.parent[1] = 7; // outside
        c.name_len[1] = 1;
    }
    b.image().copy_from_slice(b"\0\0x");
    assert!(matches!(b.finish(), Err(StoreError::Inconsistent(_))));
}

#[test]
fn names_in_any_order_are_laid_out() {
    let s = testing::random_store(3, 100);
    let r = s.read();
    let names = testing::all_names(&r);
    let mut order: Vec<usize> = (0..names.len()).collect();
    let mut rng = Rng::new(3);
    for i in (1..order.len()).rev() {
        order.swap(i, rng.below(i as u64 + 1) as usize);
    }
    let mut blob = Vec::new();
    let mut off = vec![0u64; names.len()];
    for &i in &order {
        off[i] = blob.len() as u64;
        blob.extend_from_slice(&names[i]);
    }
    let lens: Vec<u16> = names.iter().map(|n| n.len() as u16).collect();
    let image = separate_names(&blob, &off, &lens);
    assert_eq!(image, r.name_runs().flatten().copied().collect::<Vec<u8>>());
}

// ------------------------------------------------------------------ the name heap

fn small_chunks() -> StoreOptions {
    StoreOptions {
        first_chunk: 64,
        max_chunk: 128,
        ..plain()
    }
}

fn grow_names(store: &Store, files: usize, seed: u64) {
    let names = [
        "readme.TXT",
        "Photo.jpg",
        "数据.csv",
        "a",
        "cache",
        "Cache.db",
        "node_modules",
        "Ünïcode.md",
    ];
    let mut rng = Rng::new(seed);
    let mut a = store.appender();
    let root = a.add_root_dir("/r").unwrap();
    let mut dirs = std::collections::VecDeque::from([root]);
    let mut made = 0;
    while made < files {
        let Some(d) = dirs.pop_front() else { break };
        let m = 40.min(files - made);
        let mut b = EntryBatch::new();
        for i in 0..m {
            let name = format!("{}{}", rng.pick(&names), made + i);
            b.push(if i < m / 4 {
                Entry::dir(name.as_bytes())
            } else {
                Entry::file(name.as_bytes(), 1, 4096)
            });
        }
        let start = a.ingest_listing(d, &b).unwrap();
        dirs.extend(start..start + (m / 4) as NodeId);
        made += m;
    }
}

#[test]
fn names_spread_over_many_chunks_read_the_same() {
    let one = Store::with_options(plain());
    grow_names(&one, 3000, 1);
    let many = Store::with_options(small_chunks());
    grow_names(&many, 3000, 1);
    let (r1, r2) = (one.read(), many.read());
    assert!(r2.name_chunks() > 20);
    assert_eq!(testing::all_names(&r1), testing::all_names(&r2));
    // runs: every node exactly once, each run within a chunk, NUL-separated
    let mut a = 0;
    let mut count = 0;
    while (a as usize) < r2.len() {
        let run = r2.names_run(a, r2.len() as NodeId);
        assert!(run.stop > a);
        let seps = run.seps();
        assert_eq!(seps.len(), (run.stop - a) as usize);
        for (k, &s) in seps.iter().enumerate() {
            let id = a + k as NodeId;
            let len = r2.name_len(id) as usize;
            assert_eq!(
                &run.run[s as usize + 1..s as usize + 1 + len],
                r2.name_bytes(id)
            );
        }
        count += 1;
        a = run.stop;
    }
    assert!(count > 20);
    let image1: Vec<u8> = r1.name_runs().flatten().copied().collect();
    let image2: Vec<u8> = r2.name_runs().flatten().copied().collect();
    assert_eq!(image1, image2);
    assert_eq!(image1.len(), r1.names_size());
}

#[test]
fn a_listing_larger_than_a_chunk_gets_its_own() {
    let s = Store::with_options(small_chunks());
    let mut a = s.appender();
    let root = a.add_root_dir("/r").unwrap();
    let names: Vec<String> = (0..50).map(|i| format!("long-name-{i:04}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    a.ingest_listing(root, &named_batch(&refs, &[Kind::File; 50], &[1; 50], &[]))
        .unwrap();
    for (i, n) in names.iter().enumerate() {
        assert_eq!(a.name_bytes(root + 1 + i as NodeId), n.as_bytes());
    }
}

#[test]
fn name_offsets_are_narrow_then_widen() {
    let s = Store::with_options(StoreOptions {
        off_narrow_limit: 2000,
        ..plain()
    });
    let mut a = s.appender();
    let root = a.add_root_dir("/r").unwrap();
    a.ingest_listing(root, &dirs_and_files("d", 3, 0)).unwrap();
    assert!(!a.wide_offsets());
    let before = testing::all_names(&a);
    let mut leaf = root + 1;
    for k in 0..40 {
        let start = a
            .ingest_listing(
                leaf,
                &dirs_and_files(&format!("x{k:02}-padding-padding-"), 1, 2),
            )
            .unwrap();
        leaf = start;
    }
    assert!(a.wide_offsets());
    assert_eq!(testing::all_names(&a)[..before.len()], before[..]);
    assert_eq!(a.name(a.len() as NodeId - 1), "x39-padding-padding-2");
    testing::check_invariants(&a).unwrap();
}

// ------------------------------------------------------------------ growth (docs/memory.md §3)

/// Folders of 7 files and 3 subfolders breadth first until `rows` nodes; the capacities seen.
fn grow_tree(a: &mut Appender<'_>, root: NodeId, rows: usize) -> Vec<usize> {
    let mut caps = vec![a.capacity()];
    let mut pending = std::collections::VecDeque::from([root]);
    let mut k = 0;
    while a.len() < rows {
        let d = pending.pop_front().unwrap();
        let start = a
            .ingest_listing(d, &dirs_and_files(&format!("n{k}_"), 3, 7))
            .unwrap();
        pending.extend(start..start + 3);
        k += 1;
        if a.capacity() != *caps.last().unwrap() {
            caps.push(a.capacity());
        }
    }
    caps
}

fn leaf(a: &Nodes) -> NodeId {
    (0..a.len() as NodeId)
        .find(|&i| a.is_dir(i) && a.child_count(i) == 0)
        .unwrap()
}

#[test]
fn a_scan_grows_the_columns_by_half() {
    let s = Store::with_options(plain());
    let mut a = s.appender();
    let root = a.add_root_dir("/r").unwrap();
    let caps = grow_tree(&mut a, root, 200_000);
    assert!(caps.len() > 5); // from 16 Ki rows
    for w in caps.windows(2) {
        assert!(w[1] <= (w[0] as f64 * store::GROWTH) as usize + 1 + 10);
    }
    assert!(a.capacity() as f64 <= store::GROWTH * a.len() as f64 + 16.0);
}

#[test]
fn a_complete_tree_grows_a_little_at_a_time() {
    let s = Store::with_options(StoreOptions {
        settled_min_step: 64,
        ..plain()
    });
    let mut a = s.appender();
    let root = a.add_root_dir("/r").unwrap();
    grow_tree(&mut a, root, 20_000);
    let alloc = a.allocs().to_vec();
    assert!(a.compact() > 0);
    let n = a.len();
    assert_eq!(a.capacity(), n);
    assert_eq!(a.compact(), 0); // nothing left to give back
    let l = leaf(&a);
    a.ingest_listing(l, &dirs_and_files("new", 0, 1)).unwrap(); // a file created after the scan
    assert_eq!(a.capacity(), n + 1 + (n + 1) / store::SETTLED_SHARE); // not 2 * n
    assert_eq!(&a.allocs()[..n], &alloc[..]);
    let cap = a.capacity();
    for i in 0..cap - a.len() {
        let l = leaf(&a);
        a.ingest_listing(l, &dirs_and_files(&format!("x{i}_"), 0, 1))
            .unwrap();
        assert_eq!(a.capacity(), cap);
    }
    let l = leaf(&a);
    a.ingest_listing(l, &dirs_and_files("over", 0, 1)).unwrap();
    assert_eq!(a.capacity(), a.len() + a.len() / store::SETTLED_SHARE);
    testing::check_invariants(&a).unwrap();
}

#[test]
fn small_complete_trees_get_a_minimum_step() {
    let s = Store::with_options(plain());
    let mut a = s.appender();
    let root = a.add_root_dir("/r").unwrap();
    grow_tree(&mut a, root, 1000);
    a.compact();
    let l = leaf(&a);
    a.ingest_listing(l, &dirs_and_files("new", 0, 1)).unwrap();
    assert_eq!(a.capacity(), a.len() + store::SETTLED_MIN_STEP);
}

#[test]
fn loaded_and_adopted_stores_are_complete() {
    let built = Store::with_options(plain());
    {
        let mut a = built.appender();
        let root = a.add_root_dir("/r").unwrap();
        grow_tree(&mut a, root, 5000);
    }
    let (copy, _) = mutate::compact(&built.read()).unwrap();
    let n = copy.len();
    {
        let mut a = copy.appender();
        assert!(a.is_settled());
        let l = leaf(&a);
        a.ingest_listing(l, &dirs_and_files("new", 0, 1)).unwrap();
        assert_eq!(a.capacity(), n + 1 + store::SETTLED_MIN_STEP);
        assert_eq!(a.name(n as NodeId), "new0");
    }
    let target = Store::new();
    let (other, _) = mutate::compact(&built.read()).unwrap();
    let v = target.version().max(other.version());
    target.write().adopt(&other);
    assert!(target.version() > v);
    let mut a = target.appender();
    assert_eq!(a.len(), n);
    let l = leaf(&a);
    a.ingest_listing(l, &dirs_and_files("new", 0, 1)).unwrap();
    assert_eq!(a.capacity(), n + 1 + store::SETTLED_MIN_STEP);
}

#[test]
fn a_scan_reserves_its_columns_once() {
    let s = Store::new();
    let mut a = s.appender();
    a.add_root_dir("/r").unwrap();
    a.reserve(100_000);
    assert_eq!(a.capacity(), 100_000);
    a.reserve(10); // never shrinks
    assert_eq!(a.capacity(), 100_000);
    a.ingest_listing(1, &dirs_and_files("f", 0, 99_000))
        .unwrap();
    assert_eq!(a.capacity(), 100_000); // the expected rows fit: no growth, no copy
}

#[test]
fn compact_gives_back_headroom_and_keeps_the_data() {
    let s = Store::with_options(plain());
    let mut a = s.appender();
    let root = a.add_root_dir("/r").unwrap();
    grow_tree(&mut a, root, 5000);
    let before: Vec<Vec<u8>> = Col::ALL
        .iter()
        .map(|&c| a.column_bytes(c, 0..a.len()).into_owned())
        .collect();
    let ram = s.ram_bytes();
    assert!(a.capacity() > a.len());
    assert!(a.compact() > 0);
    assert!(s.ram_bytes() < ram);
    let after: Vec<Vec<u8>> = Col::ALL
        .iter()
        .map(|&c| a.column_bytes(c, 0..a.len()).into_owned())
        .collect();
    assert_eq!(before, after);
}

#[test]
fn readers_keep_their_generation_while_columns_grow() {
    let s = Store::with_options(plain());
    {
        let mut a = s.appender();
        let root = a.add_root_dir("/r").unwrap();
        a.ingest_listing(root, &dirs_and_files("d", 2, 2)).unwrap();
    }
    let r = s.read();
    let n = r.len();
    let names = testing::all_names(&r);
    {
        let mut a = s.appender(); // an appender coexists with the reader
        let root = 1;
        let first = a.children(root).start;
        a.ingest_listing(first, &dirs_and_files("big", 0, 50_000))
            .unwrap(); // grows every column
    }
    assert_eq!(r.len(), n);
    assert_eq!(testing::all_names(&r), names);
    assert_eq!(r.child_range(1).1 as usize, n); // ranges past the view read as empty ...
    assert_eq!(r.child_range(r.children(1).start), (0, 0)); // ... the folder listed meanwhile
    drop(r);
    assert_eq!(s.read().len(), n + 50_000);
}

#[test]
fn closing_a_store_frees_it() {
    let s = testing::random_store(5, 50);
    let weak_count = {
        let r = s.read();
        r.len()
    };
    assert!(weak_count > 1);
    let clone = s.clone();
    drop(s);
    assert!(clone.len() > 1); // the handle is shared
}

#[test]
fn spec_builds_and_finds() {
    let spec = d([
        ("a", d([("x.txt", f(10)), ("y", d([]))])),
        ("b.bin", f(5000)),
    ]);
    let s = testing::build_with(&spec, "/root", 4096, &["a/y"]);
    let r = s.read();
    let x = testing::find(&r, "a/x.txt");
    assert_eq!(r.name(x), "x.txt");
    assert_eq!(r.ext_name(r.ext(x)).as_ref(), "txt");
    assert_eq!(r.alloc(x), 4096);
    assert!(r.flags(testing::find(&r, "a/y")).intersects(Flags::PENDING));
    assert_eq!(r.tot_files(1), 2);
    testing::check_invariants(&r).unwrap();
}
