//! `ingest_slice` folds a whole slice of listings exactly like `ingest_listing` per listing
//! (test_store_ingest_slice.py).

use sk_store::testing::{self, Rng, append_batch};
use sk_store::*;

/// The reference: one listing at a time; mount points' PENDING is what ingest_listing leaves.
fn fold_listings(
    store: &Store,
    refs: &[SliceRef],
    bounds: &[u32],
    batch: &EntryBatch,
) -> Vec<Option<NodeId>> {
    let mut a = store.appender();
    let mut starts: Vec<NodeId> = Vec::new();
    for (k, r) in refs.iter().enumerate() {
        let dir = match *r {
            SliceRef::Node(d) => d,
            SliceRef::Entry { listing, entry } => starts[listing as usize] + entry,
        };
        let (lo, hi) = (bounds[k] as usize, bounds[k + 1] as usize);
        let part = batch.select(std::slice::from_ref(&(lo..hi)));
        starts.push(a.ingest_listing(dir, &part).unwrap());
    }
    starts.into_iter().map(Some).collect()
}

fn two_stores() -> (Store, Store) {
    two_stores_with(StoreOptions::default())
}

fn two_stores_with(opts: StoreOptions) -> (Store, Store) {
    let stores = (Store::with_options(opts.clone()), Store::with_options(opts));
    for s in [&stores.0, &stores.1] {
        let mut a = s.appender();
        a.add_root_dir("C:\\a").unwrap();
        a.add_root_dir("D:\\b").unwrap();
        a.ext_id("x");
        a.ext_id("y");
    }
    stores
}

/// Listings over `pending` store nodes and over folders found earlier in the slice.
type Slice = (Vec<SliceRef>, Vec<u32>, EntryBatch, Vec<(u32, u32)>);

fn random_slice(rng: &mut Rng, pending: &mut Vec<NodeId>) -> Slice {
    let mut refs = Vec::new();
    let mut found: Vec<(u32, u32)> = Vec::new();
    let mut bounds = vec![0u32];
    let mut all = EntryBatch::new();
    for k in 0..rng.below(14) as u32 {
        if !found.is_empty() && (pending.is_empty() || rng.chance(0.7)) {
            let (j, i) = found.swap_remove(rng.below(found.len() as u64) as usize);
            refs.push(SliceRef::Entry {
                listing: j,
                entry: i,
            });
        } else if !pending.is_empty() {
            refs.push(SliceRef::Node(
                pending.swap_remove(rng.below(pending.len() as u64) as usize),
            ));
        } else {
            break;
        }
        let mut b = EntryBatch::new();
        for i in 0..rng.below(7) as u32 {
            let kind = *rng.pick(&[Kind::File, Kind::File, Kind::Dir, Kind::Link, Kind::Other]);
            let mut flags = *rng.pick(&[
                Flags::NONE,
                Flags::HIDDEN,
                Flags::HARDLINK_DUP,
                Flags::HIDDEN | Flags::HARDLINK_DUP,
            ]);
            if kind == Kind::Dir && rng.chance(0.15) {
                flags |= Flags::MOUNT;
            }
            let size = if rng.chance(0.3) {
                rng.range(0, 1 << 45)
            } else {
                rng.range(0, 5000)
            };
            let name = format!("n{k}.{i}{}", "é".repeat(rng.below(3) as usize));
            b.push(Entry {
                flags,
                size: if kind == Kind::Dir { 0 } else { size },
                alloc: if rng.chance(0.2) {
                    rng.range(0, 1 << 40)
                } else {
                    size / 2
                },
                mtime: rng.next_u64() as u32,
                ctime: rng.next_u64() as u32,
                atime: rng.next_u64() as u32,
                ino: rng.next_u64() >> 1,
                nlink: rng.range(1, 4) as u16,
                ext: rng.below(3) as u16,
                ..Entry::new(name.as_bytes(), kind)
            });
            if kind == Kind::Dir && !flags.intersects(Flags::MOUNT) {
                found.push((k, i));
            }
        }
        append_batch(&mut all, &b);
        bounds.push(all.len() as u32);
    }
    (refs, bounds, all, found)
}

/// The reference leaves PENDING on mount points (ingest_listing does too); the slice never sets
/// it. Compare after clearing it on the reference's mount points.
fn clear_mount_pending(store: &Store) {
    let mut a = store.appender();
    for i in 0..a.len() as NodeId {
        if a.flags(i).contains(Flags::MOUNT | Flags::PENDING) {
            a.clear(i, Flags::PENDING);
        }
    }
}

#[test]
fn random_slices() {
    random_slices_with(0..300, StoreOptions::default);
}

/// The same with names in many small heap chunks and `name_off` widening on the way (the rows
/// sk-native writes are lent over chunk ends and both offset widths).
#[test]
fn random_slices_over_small_chunks_and_wide_offsets() {
    let (wide, chunked) = random_slices_with(300..700, || StoreOptions {
        first_chunk: 16,
        max_chunk: 48,
        off_narrow_limit: 150,
        ..StoreOptions::default()
    });
    assert!(
        wide > 40 && chunked > 30,
        "{wide} stores widened, {chunked} in 3+ chunks"
    );
}

/// Random rounds of slices against ingest_listing; returns how many stores ended with wide
/// name offsets and how many with names in three chunks or more.
fn random_slices_with(
    cases: std::ops::Range<u64>,
    opts: impl Fn() -> StoreOptions,
) -> (usize, usize) {
    let (mut wide, mut chunked) = (0, 0);
    for case in cases {
        let mut rng = Rng::new(case);
        let rounds = 1 + rng.below(4);
        let (vec, refst) = two_stores_with(opts());
        let mut pending = vec![1, 2];
        for _ in 0..rounds {
            let (refs, bounds, batch, left) = random_slice(&mut rng, &mut pending);
            let starts = vec.appender().ingest_slice(&refs, &bounds, &batch).unwrap();
            assert_eq!(
                starts,
                fold_listings(&refst, &refs, &bounds, &batch),
                "case {case}"
            );
            clear_mount_pending(&refst);
            testing::assert_same_store(&vec.read(), &refst.read());
            pending.extend(left.iter().map(|&(j, i)| starts[j as usize].unwrap() + i));
        }
        // the incremental totals agree with a full recount as well
        testing::check_invariants(&vec.read()).unwrap();
        wide += vec.read().wide_offsets() as usize;
        chunked += (vec.read().name_chunks() >= 3) as usize;
    }
    (wide, chunked)
}

#[test]
fn spec_scans_agree_for_every_slice_size() {
    let mut rng = Rng::new(9);
    let mut spec = testing::d([]);
    // a random spec tree of ~400 nodes
    let mut folders = vec![String::new()];
    for i in 0..400 {
        let parent = rng.pick(&folders).clone();
        let name = format!("e{i}");
        let child = if rng.chance(0.3) {
            testing::d([])
        } else {
            testing::f(rng.range(0, 100_000))
        };
        let is_dir = matches!(child, testing::Spec::Dir(_));
        spec.get_mut(&parent)
            .unwrap()
            .children_mut()
            .push((name.clone(), child));
        if is_dir {
            folders.push(format!("{parent}/{name}"));
        }
    }
    let one = testing::build_with(&spec, "/r", 1, &[]);
    let facts = testing::facts(&one.read());
    for per_slice in [1, 3, 25, 100_000] {
        let s = testing::scan_spec(&spec, "/r", per_slice);
        assert_eq!(testing::facts(&s.read()), facts, "per_slice {per_slice}");
        testing::check_invariants(&s.read()).unwrap();
    }
    assert_eq!(one.read().tot_files(1) as usize, spec.counts().0);
}

#[test]
fn bad_slices_change_nothing() {
    let store = Store::new();
    let mut a = store.appender();
    let root = a.add_root_dir("C:\\r").unwrap();
    let one = testing::batch(&[Kind::File], &[1], &[]);
    let two = {
        let mut b = one.clone();
        append_batch(&mut b, &one);
        b
    };
    let cases: Vec<(Vec<SliceRef>, Vec<u32>, &EntryBatch, StoreError)> = vec![
        (
            vec![SliceRef::Entry {
                listing: 0,
                entry: 0,
            }],
            vec![0, 1],
            &one,
            StoreError::BadSlice("a listing can only name an entry of an earlier listing"),
        ),
        (
            vec![SliceRef::Node(99)],
            vec![0, 1],
            &one,
            StoreError::BadSlice("a listing names a node that is not in the store"),
        ),
        (
            vec![
                SliceRef::Node(root),
                SliceRef::Entry {
                    listing: 0,
                    entry: 0,
                },
            ],
            vec![0, 1, 2],
            &two,
            StoreError::NotADirectory(root + 1), // entry 0 of listing 0 is a file
        ),
        (
            vec![SliceRef::Node(root), SliceRef::Node(root)],
            vec![0, 1, 2],
            &two,
            StoreError::AlreadyListed(root),
        ),
        (
            vec![SliceRef::Node(root)],
            vec![0, 2],
            &one,
            StoreError::BadSlice("bounds do not match the entries"),
        ),
    ];
    for (refs, bounds, batch, want) in cases {
        assert_eq!(a.ingest_slice(&refs, &bounds, batch), Err(want));
        assert_eq!(a.len(), 2);
        assert_eq!(a.child_count(root), 0);
    }
    a.ingest_slice(&[SliceRef::Node(root)], &[0, 1], &one)
        .unwrap();
    assert_eq!(
        a.ingest_slice(&[SliceRef::Node(root)], &[0, 1], &one),
        Err(StoreError::AlreadyListed(root))
    );
}

/// The fold (sk-native) writes the store's values: its kinds and flag bits are the store's.
#[test]
fn the_native_fold_writes_the_stores_values() {
    use sk_native::columns::{flag, kind};
    assert_eq!(
        [kind::FILE, kind::DIR, kind::LINK, kind::OTHER],
        [Kind::File, Kind::Dir, Kind::Link, Kind::Other].map(|k| k as u8)
    );
    let pairs = [
        (flag::ACCESS_DENIED, Flags::ACCESS_DENIED),
        (flag::MOUNT, Flags::MOUNT),
        (flag::HARDLINK_DUP, Flags::HARDLINK_DUP),
        (flag::COMPRESSED, Flags::COMPRESSED),
        (flag::SPARSE, Flags::SPARSE),
        (flag::HIDDEN, Flags::HIDDEN),
        (flag::SYSTEM, Flags::SYSTEM),
        (flag::READONLY, Flags::READONLY),
        (flag::REPARSE, Flags::REPARSE),
        (flag::CLOUD, Flags::CLOUD),
        (flag::ERROR, Flags::ERROR),
        (flag::PENDING, Flags::PENDING),
        (flag::ARCHIVE, Flags::ARCHIVE),
        (flag::ENCRYPTED, Flags::ENCRYPTED),
        (flag::TEMPORARY, Flags::TEMPORARY),
        (flag::NOT_INDEXED, Flags::NOT_INDEXED),
        (flag::OFFLINE, Flags::OFFLINE),
        (flag::VIRTUAL, Flags::VIRTUAL),
        (flag::ADS, Flags::ADS),
        (flag::GONE, Flags::GONE),
    ];
    for (native, store) in pairs {
        assert_eq!(native, store.0, "{store:?}");
    }
}

/// A slice that lists a folder which left the tree and one that did not: the first is skipped
/// with the listing inside it, the second folds as `ingest_listing` folds it.
#[test]
fn a_slice_over_a_folder_that_left_the_tree_folds_the_rest() {
    let (vec, refst) = two_stores();
    let mut ids = Vec::new();
    for s in [&vec, &refst] {
        let mut a = s.appender();
        ids = vec![
            a.ingest_listing(1, &testing::dirs_and_files("k", 1, 2))
                .unwrap(),
            a.ingest_listing(2, &testing::dirs_and_files("g", 1, 2))
                .unwrap(),
        ];
    }
    let (kept, gone) = (ids[0], ids[1]);
    for s in [&vec, &refst] {
        let mut w = s.write();
        let f = w.flags(gone).0;
        w.set_raw(Col::Flags, gone, f | Flags::GONE.0);
        w.touch();
    }
    let inner = testing::dirs_and_files("i", 0, 3);
    let outer = testing::dirs_and_files("o", 1, 1);
    let mut all = EntryBatch::new();
    for b in [&outer, &inner, &outer, &inner] {
        append_batch(&mut all, b);
    }
    let refs = [
        SliceRef::Node(gone),
        SliceRef::Entry {
            listing: 0,
            entry: 0,
        },
        SliceRef::Node(kept),
        SliceRef::Entry {
            listing: 2,
            entry: 0,
        },
    ];
    let bounds = [0, 2, 5, 7, 10];
    let starts = vec.appender().ingest_slice(&refs, &bounds, &all).unwrap();
    let n = refst.len() as NodeId;
    assert_eq!(starts, vec![None, None, Some(n), Some(n + 2)]);
    {
        let mut a = refst.appender();
        let s = a.ingest_listing(kept, &outer).unwrap();
        a.ingest_listing(s, &inner).unwrap();
    }
    // (no check_invariants: the GONE row above was marked by hand, not moved out of its range)
    testing::assert_same_store(&vec.read(), &refst.read());
}
