//! Subtree walks, summaries and the path index.

use std::collections::HashSet;

use sk_store::lookup::PathIndex;
use sk_store::subtree::{SubtreeChunks, ranges_to_ids, subtree_ids, summarize};
use sk_store::testing::{self, d, f, relist_from_spec, scan_spec};
use sk_store::*;

#[test]
fn ranges_to_ids_concatenates() {
    assert_eq!(ranges_to_ids(&[5, 1], &[2, 3]), vec![5, 6, 1, 2, 3]);
    assert!(ranges_to_ids(&[], &[]).is_empty());
}

#[test]
fn chunked_walks_visit_every_node_once() {
    let store = testing::random_store(11, 2000);
    let r = store.read();
    let all: HashSet<NodeId> = subtree_ids(&r, 1).unwrap().into_iter().collect();
    assert_eq!(
        all.len(),
        r.tot_files(1) as usize + r.tot_dirs(1) as usize + count_links(&r)
    );
    for chunk in [1, 7, 100, 1 << 18] {
        let mut seen = HashSet::new();
        for piece in SubtreeChunks::new(&r, 1, chunk) {
            let piece = piece.unwrap();
            assert!(piece.len() <= chunk);
            for id in piece {
                assert!(seen.insert(id));
            }
        }
        assert_eq!(seen, all, "chunk {chunk}");
    }
}

fn count_links(r: &Nodes) -> usize {
    subtree_ids(r, 1)
        .unwrap()
        .iter()
        .filter(|&&i| matches!(r.kind(i), Kind::Link | Kind::Other))
        .count()
}

#[test]
fn summaries_do_not_depend_on_the_chunk() {
    let store = testing::random_store(12, 1500);
    let r = store.read();
    let cls = |id: NodeId| (r.size(id) % 5) as usize;
    let want = summarize(&r, 1, cls, 5, 10, None, 1 << 18).unwrap();
    assert_eq!(want.top_files.len(), 10);
    for w in want.top_files.windows(2) {
        assert!(r.alloc(w[0]) > r.alloc(w[1]) || (r.alloc(w[0]) == r.alloc(w[1]) && w[0] < w[1]));
    }
    // brute force: the largest counted files
    let mut files: Vec<NodeId> = subtree_ids(&r, 1)
        .unwrap()
        .into_iter()
        .filter(|&i| !r.is_dir(i) && !r.flags(i).intersects(Flags::HARDLINK_DUP))
        .collect();
    files.sort_by(|&a, &b| r.alloc(b).cmp(&r.alloc(a)).then(a.cmp(&b)));
    assert_eq!(want.top_files, files[..10]);
    assert_eq!(
        want.class_bytes.iter().sum::<i64>(),
        files.iter().map(|&i| r.alloc(i)).sum::<i64>()
    );
    for chunk in [1, 3, 64] {
        let mut base = |id: NodeId| (r.size(id) % 2) as usize;
        let got = summarize(&r, 1, cls, 5, 10, Some(&mut base), chunk).unwrap();
        assert_eq!(got.top_files, want.top_files);
        assert_eq!(got.class_bytes, want.class_bytes);
        assert_eq!(got.class_files, want.class_files);
        assert_eq!(got.latest_mtime, want.latest_mtime);
        assert!(got.class_parts.is_some());
    }
    // a file summarises itself
    let one = files[0];
    let s = summarize(&r, one, cls, 5, 3, None, 16).unwrap();
    assert_eq!(s.top_files, vec![one]);
}

#[test]
fn path_index_locates_and_follows_relists() {
    let mut spec = d([
        (
            "a",
            d([("x.txt", f(1)), ("X.txt", f(2)), ("Only.bin", f(3))]),
        ),
        ("b", d([("deep", d([("y", f(4))]))])),
    ]);
    let store = scan_spec(&spec, "/root", 2);
    let mut index = PathIndex::new();
    {
        let r = store.read();
        let x = testing::find(&r, "a/x.txt");
        assert_eq!(index.locate(&r, b"/root/a/x.txt"), (Some(x), Some(x)));
        assert_eq!(
            index.locate(&r, b"/root/a/X.txt"),
            (Some(x + 1), Some(x + 1))
        );
        let a = testing::find(&r, "a");
        assert_eq!(index.locate(&r, b"/root/a/missing"), (None, Some(a)));
        assert_eq!(index.locate(&r, b"/elsewhere/a"), (None, None));
        let only = testing::find(&r, "a/Only.bin");
        if cfg!(windows) {
            assert_eq!(index.locate(&r, b"/root/a/only.BIN").0, Some(only));
            assert_eq!(index.locate(&r, b"/root/a/X.TXT").0, None); // two under normcase: neither
        } else {
            assert_eq!(index.locate(&r, b"/root/a/only.BIN").0, None);
        }
        assert_eq!(index.locate(&r, b"/root/a/x.txt/below"), (None, Some(x))); // a file is a leaf
        assert!(!index.unnamed(&r, x));
    }
    spec.get_mut("a")
        .unwrap()
        .children_mut()
        .push(("new.txt".into(), f(9)));
    {
        let mut w = store.write();
        let a = testing::find(&w, "a");
        relist_from_spec(&mut w, a, spec.get("a").unwrap());
    }
    let r = store.read();
    let new = testing::find(&r, "a/new.txt");
    assert_eq!(index.locate(&r, b"/root/a/new.txt").0, Some(new)); // the folder's range changed
    assert_eq!(
        index.locate(&r, b"/root/a/x.txt").0,
        Some(testing::find(&r, "a/x.txt"))
    );
}

#[test]
fn names_held_twice_are_unnamed() {
    let store = Store::new();
    let mut a = store.appender();
    let root = a.add_root_dir("/r").unwrap();
    let first = a
        .ingest_listing(
            root,
            &testing::named_batch(
                &["dup", "dup", "ok", "s/x"],
                &[Kind::File; 4],
                &[1, 2, 3, 4],
                &[],
            ),
        )
        .unwrap();
    let mut index = PathIndex::new();
    assert!(index.unnamed(&a, first));
    assert!(index.unnamed(&a, first + 1));
    assert!(!index.unnamed(&a, first + 2));
    assert!(index.unnamed(&a, first + 3));
    assert_eq!(index.locate(&a, b"/r/dup").0, Some(first)); // the first of them
}

#[test]
fn bounded_number_of_folder_maps() {
    let store = testing::random_store(2, 3000);
    let r = store.read();
    let mut index = PathIndex::new();
    for id in 1..r.len() as NodeId {
        if r.is_dir(id) {
            index.locate(&r, &r.path_bytes(id));
        }
    }
    assert!(index.len() <= sk_store::lookup::MAX_FOLDERS);
    index.forget();
    assert!(index.is_empty());
}
