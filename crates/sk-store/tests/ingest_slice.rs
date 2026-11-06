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
    let stores = (Store::new(), Store::new());
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
    for case in 0..300u64 {
        let mut rng = Rng::new(case);
        let rounds = 1 + rng.below(4);
        let (vec, refst) = two_stores();
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
    }
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
