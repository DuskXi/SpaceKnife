//! Snapshot compare (sk_core::diff, port of tests/test_diff.py): hand-built trees, a slow
//! reference implementation on random trees, the union store's invariants and the change ranking.

use std::collections::{HashMap, HashSet, VecDeque};

use sk_core::diff::{
    ChangeTree, CompareOptions, DOMINANT, DiffError, DiffTree, Direction, MINOR, Status, compare,
    compare_with, short_label,
};
use sk_store::testing::{Rng, check_invariants};
use sk_store::{Entry, EntryBatch, Flags, Kind, NodeId, Nodes, Store, StoreOptions, Times, mutate};

// --------------------------------------------------------------------------- building trees

#[derive(Clone, Debug)]
enum S {
    /// a non-folder entry: size, allocation (None: the size), an additional hard link, kind
    F(i64, Option<i64>, bool, Kind),
    /// a folder with its own allocation (POSIX directory blocks)
    D(Vec<(String, S)>, i64),
}

fn f(size: i64) -> S {
    S::F(size, None, false, Kind::File)
}

fn d(kids: Vec<(&str, S)>) -> S {
    S::D(
        kids.into_iter().map(|(n, s)| (n.to_string(), s)).collect(),
        0,
    )
}

fn kids(spec: &S) -> &[(String, S)] {
    match spec {
        S::D(k, _) => k,
        S::F(..) => &[],
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Order {
    Bfs,
    Dfs,
}

fn build_roots(roots: &[(&str, &S)], order: Order) -> Store {
    let store = Store::new();
    {
        let mut app = store.appender();
        let mut todo: VecDeque<(NodeId, &S)> = VecDeque::new();
        for (path, spec) in roots {
            let r = app
                .add_root(path.as_bytes(), Kind::Dir, Flags::PENDING, Times::default())
                .unwrap();
            todo.push_back((r, *spec));
        }
        while let Some((node, spec)) = if order == Order::Bfs {
            todo.pop_front()
        } else {
            todo.pop_back()
        } {
            let mut batch = EntryBatch::new();
            for (name, s) in kids(spec) {
                let e = match s {
                    S::D(_, alloc) => Entry {
                        alloc: *alloc,
                        ..Entry::dir(name.as_bytes())
                    },
                    S::F(size, alloc, dup, kind) => Entry {
                        size: *size,
                        alloc: alloc.unwrap_or(*size),
                        flags: if *dup {
                            Flags::HARDLINK_DUP
                        } else {
                            Flags::NONE
                        },
                        nlink: if *dup { 2 } else { 1 },
                        ..Entry::new(name.as_bytes(), *kind)
                    },
                };
                batch.push(e);
            }
            let first = app.ingest_listing(node, &batch).unwrap();
            for (i, (_, s)) in kids(spec).iter().enumerate() {
                if let S::D(..) = s {
                    todo.push_back((first + i as NodeId, s));
                }
            }
        }
    }
    store
}

fn build(spec: &S) -> Store {
    build_roots(&[("/r", spec)], Order::Bfs)
}

fn build_at(spec: &S, root: &str) -> Store {
    build_roots(&[(root, spec)], Order::Bfs)
}

/// List `folder` again with the same entries: its children move to new rows, the old ones GONE.
fn relist_same(store: &Store, folder: NodeId) {
    let mut w = store.write();
    let mut batch = EntryBatch::new();
    let kids: Vec<NodeId> = w.children(folder).collect();
    let names: Vec<Vec<u8>> = kids.iter().map(|&k| w.name_bytes(k).to_vec()).collect();
    for (k, name) in kids.iter().zip(&names) {
        let k = *k;
        batch.push(Entry {
            name,
            kind: w.kind(k),
            flags: w.flags(k) & Flags::HARDLINK_DUP,
            size: w.size(k),
            alloc: w.alloc(k),
            mtime: w.mtime(k),
            ctime: w.ctime(k),
            atime: w.atime(k),
            ino: w.ino(k),
            nlink: w.nlink(k),
            ext: w.ext(k),
        });
    }
    mutate::relist(&mut w, folder, &batch).unwrap();
}

/// Row of `a/b` below the first root; `kind` picks among equal names.
fn find(nodes: &Nodes, path: &str, kind: Option<Kind>) -> NodeId {
    let mut node = nodes.roots().next().unwrap();
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    for (i, part) in parts.iter().enumerate() {
        let hits: Vec<NodeId> = nodes
            .children(node)
            .filter(|&c| nodes.name(c) == *part)
            .filter(|&c| i + 1 < parts.len() || kind.is_none_or(|k| nodes.kind(c) == k))
            .collect();
        assert!(!hits.is_empty(), "{path}");
        node = hits[0];
    }
    node
}

fn row(tree: &DiffTree, path: &str) -> usize {
    find(&tree.store.read(), path, None) as usize
}

fn row_k(tree: &DiffTree, path: &str, kind: Kind) -> usize {
    find(&tree.store.read(), path, Some(kind)) as usize
}

fn top(tree: &DiffTree) -> usize {
    tree.store.read().roots().next().unwrap() as usize
}

// --------------------------------------------------------------------------- reference

fn live(nodes: &Nodes, node: i32) -> Vec<i32> {
    if node < 0 || !nodes.is_dir(node as u32) {
        return vec![];
    }
    nodes
        .children(node as u32)
        .filter(|&c| !nodes.flags(c).intersects(Flags::GONE))
        .map(|c| c as i32)
        .collect()
}

fn eff(nodes: &Nodes, r: i32, alloc: bool) -> i64 {
    if r < 0 {
        return 0;
    }
    let r = r as u32;
    if nodes.is_dir(r) {
        return if alloc {
            nodes.tot_alloc(r)
        } else {
            nodes.tot_size(r)
        };
    }
    if nodes.flags(r).intersects(Flags::HARDLINK_DUP) {
        return 0;
    }
    if alloc { nodes.alloc(r) } else { nodes.size(r) }
}

fn own(nodes: &Nodes, r: i32) -> i64 {
    if r < 0 || nodes.flags(r as u32).intersects(Flags::HARDLINK_DUP) {
        return 0;
    }
    nodes.alloc(r as u32)
}

fn ref_fold(s: &str) -> String {
    s.to_lowercase()
}

fn normpath(s: &str) -> String {
    String::from_utf8(sk_store::names::normpath(s.as_bytes())).unwrap()
}

fn pairs(base: &Nodes, target: &Nodes, b: i32, t: i32, fold: bool, top: bool) -> Vec<(i32, i32)> {
    let (kb, kt) = (live(base, b), live(target, t));
    let key = |nodes: &Nodes, r: i32, folded: bool| -> (String, bool) {
        let mut name = nodes.name(r as u32).into_owned();
        if top {
            name = normpath(&name);
        }
        (
            if folded { ref_fold(&name) } else { name },
            nodes.is_dir(r as u32),
        )
    };
    let mut matched: HashMap<i32, i32> = HashMap::new();
    let mut taken: HashSet<i32> = HashSet::new();
    let passes: Vec<bool> = if top {
        vec![fold]
    } else if fold {
        vec![false, true]
    } else {
        vec![false]
    };
    for folded in passes {
        let mut waiting: HashMap<(String, bool), VecDeque<i32>> = HashMap::new();
        for &cb in &kb {
            if !taken.contains(&cb) {
                waiting
                    .entry(key(base, cb, folded))
                    .or_default()
                    .push_back(cb);
            }
        }
        for &ct in &kt {
            if matched.contains_key(&ct) {
                continue;
            }
            if let Some(cb) = waiting
                .get_mut(&key(target, ct, folded))
                .and_then(|q| q.pop_front())
            {
                matched.insert(ct, cb);
                taken.insert(cb);
            }
        }
    }
    let mut out: Vec<(i32, i32)> = kt
        .iter()
        .map(|&ct| (*matched.get(&ct).unwrap_or(&-1), ct))
        .collect();
    out.extend(
        kb.iter()
            .filter(|cb| !taken.contains(cb))
            .map(|&cb| (cb, -1)),
    );
    out
}

type Row = (Status, i64, i64, i64, i64, i64, i64);

fn reference(base: &Nodes, target: &Nodes, fold: bool) -> HashMap<(i32, i32), Row> {
    fn visit(
        base: &Nodes,
        target: &Nodes,
        fold: bool,
        b: i32,
        t: i32,
        out: &mut HashMap<(i32, i32), Row>,
    ) -> (i64, i64, Status) {
        let (mut grow, mut shrink, mut changed) = (0i64, 0i64, false);
        for (cb, ct) in pairs(base, target, b, t, fold, (b, t) == (0, 0)) {
            let (mut g, mut s, status) = visit(base, target, fold, cb, ct, out);
            let od = own(target, ct) - own(base, cb);
            let is_dir = if ct >= 0 {
                target.is_dir(ct as u32)
            } else {
                base.is_dir(cb as u32)
            };
            if is_dir {
                g += od.max(0);
                s += (-od).max(0);
            }
            grow += g;
            shrink += s;
            changed |= status != Status::Unchanged;
        }
        let is_dir = if t >= 0 {
            target.is_dir(t as u32)
        } else {
            base.is_dir(b as u32)
        };
        if !is_dir {
            let od = own(target, t) - own(base, b);
            grow = od.max(0);
            shrink = (-od).max(0);
        }
        let ends = (
            eff(base, b, true),
            eff(target, t, true),
            eff(base, b, false),
            eff(target, t, false),
        );
        let status = if b < 0 {
            Status::Added
        } else if t < 0 {
            Status::Removed
        } else {
            let (bu, tu) = (b as u32, t as u32);
            if changed
                || ends.0 != ends.1
                || ends.2 != ends.3
                || base.size(bu) != target.size(tu)
                || base.alloc(bu) != target.alloc(tu)
                || base.kind(bu) != target.kind(tu)
                || (base.flags(bu).0 ^ target.flags(tu).0) & Flags::HARDLINK_DUP.0 != 0
            {
                Status::Changed
            } else {
                Status::Unchanged
            }
        };
        out.insert(
            (b, t),
            (status, ends.0, ends.1, ends.2, ends.3, grow, shrink),
        );
        (grow, shrink, status)
    }
    let mut out = HashMap::new();
    visit(base, target, fold, 0, 0, &mut out);
    out
}

fn got(tree: &DiffTree) -> HashMap<(i32, i32), Row> {
    (0..tree.len())
        .map(|u| {
            (
                (tree.base_id[u], tree.target_id[u]),
                (
                    tree.status[u],
                    tree.base_alloc[u],
                    tree.target_alloc[u],
                    tree.base_size[u],
                    tree.target_size[u],
                    tree.grow[u],
                    tree.shrink[u],
                ),
            )
        })
        .collect()
}

fn reachable(nodes: &Nodes) -> HashSet<i32> {
    let mut seen = HashSet::new();
    let mut todo = vec![0i32];
    while let Some(node) = todo.pop() {
        seen.insert(node);
        todo.extend(live(nodes, node));
    }
    seen
}

/// The union store keeps every store invariant; the arrays agree with each other and the union
/// covers every live row of both trees exactly once.
fn check_union(tree: &DiffTree, base: &Nodes, target: &Nodes) {
    let s = tree.store.read();
    let n = s.len();
    assert_eq!(tree.len(), n);
    assert_eq!(tree.grow.len(), n);
    assert_eq!(s.parent(0), -1);
    assert_eq!(s.depth(0), 0);
    assert_eq!(s.flags(0), Flags::VIRTUAL);
    check_invariants(&s).unwrap();
    for u in 1..n as NodeId {
        assert!(
            (s.parent(u) as i64) < u as i64,
            "ids grow below their parent"
        );
        assert!(!s.flags(u).intersects(Flags::GONE));
        if !s.is_dir(u) {
            assert_eq!(s.child_count(u), 0);
        }
        let (side, r) = if tree.target_id[u as usize] >= 0 {
            (target, tree.target_id[u as usize])
        } else {
            (base, tree.base_id[u as usize])
        };
        assert_eq!(s.name_bytes(u), side.name_bytes(r as u32));
    }
    for (ids, nodes) in [(&tree.base_id, base), (&tree.target_id, target)] {
        let live: Vec<i32> = ids.iter().copied().filter(|&i| i >= 0).collect();
        let set: HashSet<i32> = live.iter().copied().collect();
        assert_eq!(set.len(), live.len());
        assert_eq!(set, reachable(nodes));
    }
    for u in 0..n {
        assert_eq!(tree.status[u] == Status::Added, tree.base_id[u] < 0);
        assert_eq!(tree.status[u] == Status::Removed, tree.target_id[u] < 0);
        assert_eq!(tree.delta[u], tree.target_alloc[u] - tree.base_alloc[u]);
        assert_eq!(tree.grow[u] - tree.shrink[u], tree.delta[u]);
        assert!(tree.grow[u] >= 0 && tree.shrink[u] >= 0);
    }
    let area = tree.area();
    for dnode in 0..n as NodeId {
        if s.is_dir(dnode) || dnode == 0 {
            let below: i64 = s.children(dnode).map(|c| area[c as usize]).sum();
            assert!(area[dnode as usize] >= below);
        }
    }
}

fn check(base: &Store, target: &Store, fold: bool) -> DiffTree {
    let (b, t) = (base.read(), target.read());
    let tree = compare(&b, &t, Some(fold)).unwrap();
    check_union(&tree, &b, &t);
    assert_eq!(got(&tree), reference(&b, &t, fold));
    tree
}

fn name(tree: &DiffTree, u: usize) -> String {
    tree.store.read().name(u as NodeId).into_owned()
}

// --------------------------------------------------------------------------- hand-built trees

#[test]
fn added_removed_changed_unchanged() {
    let base = build(&d(vec![
        (
            "a",
            d(vec![
                ("x.txt", f(10)),
                ("y.bin", f(100)),
                ("old.log", f(30)),
            ]),
        ),
        ("keep.dat", f(5)),
        ("same", d(vec![("z", f(1))])),
    ]));
    let target = build(&d(vec![
        (
            "a",
            d(vec![("x.txt", f(10)), ("y.bin", f(300)), ("new.log", f(7))]),
        ),
        ("keep.dat", f(5)),
        ("same", d(vec![("z", f(1))])),
        ("fresh", d(vec![("q", f(2))])),
    ]));
    let tree = check(&base, &target, false);
    let st = |p: &str| tree.status[row(&tree, p)];
    assert_eq!(st("a"), Status::Changed);
    assert_eq!(st("a/x.txt"), Status::Unchanged);
    assert_eq!(st("a/y.bin"), Status::Changed);
    assert_eq!(st("a/old.log"), Status::Removed);
    assert_eq!(st("a/new.log"), Status::Added);
    assert_eq!(st("same"), Status::Unchanged);
    assert_eq!(st("fresh"), Status::Added);
    assert_eq!(st("fresh/q"), Status::Added);
    let a = row(&tree, "a");
    assert_eq!(
        (tree.base_alloc[a], tree.target_alloc[a], tree.delta[a]),
        (140, 317, 177)
    );
    assert_eq!((tree.grow[a], tree.shrink[a]), (207, 30));
    let r = top(&tree);
    assert_eq!(
        (tree.delta[r], tree.grow[r], tree.shrink[r]),
        (179, 209, 30)
    );
    let s = tree.store.read();
    assert_eq!(s.alloc(row(&tree, "a/y.bin") as NodeId), 300);
    assert_eq!(s.alloc(row(&tree, "a/old.log") as NodeId), 30);
    let names: Vec<String> = s
        .children(a as NodeId)
        .map(|c| s.name(c).into_owned())
        .collect();
    assert_eq!(names, ["x.txt", "y.bin", "new.log", "old.log"]);
}

#[test]
fn identical_trees_are_unchanged() {
    let spec = d(vec![
        ("a", d(vec![("b", d(vec![("c.txt", f(3))])), ("d", f(4))])),
        ("e", d(vec![])),
    ]);
    let tree = check(&build(&spec), &build(&spec), false);
    assert!(tree.status.iter().all(|&s| s == Status::Unchanged));
    assert!(tree.delta.iter().all(|&d| d == 0) && tree.area().iter().all(|&a| a == 0));
    let store = build(&spec);
    let r = store.read();
    let same = compare(&r, &r, Some(false)).unwrap();
    assert_eq!(same.base_id, same.target_id);
}

#[test]
fn rename_is_removed_and_added() {
    let tree = check(
        &build(&d(vec![("photos", d(vec![("a.jpg", f(50))]))])),
        &build(&d(vec![("pictures", d(vec![("a.jpg", f(50))]))])),
        false,
    );
    assert_eq!(tree.status[row(&tree, "photos")], Status::Removed);
    assert_eq!(tree.status[row(&tree, "photos/a.jpg")], Status::Removed);
    assert_eq!(tree.status[row(&tree, "pictures")], Status::Added);
    let r = top(&tree);
    assert_eq!((tree.delta[r], tree.grow[r], tree.shrink[r]), (0, 50, 50));
}

#[test]
fn windows_rule_folds_case() {
    let base = build_at(
        &d(vec![
            (
                "Docs",
                d(vec![
                    ("Report.TXT", f(10)),
                    ("Ärger.txt", f(4)),
                    ("naïve", f(2)),
                ]),
            ),
            ("MiXeD", f(1)),
        ]),
        if cfg!(windows) { "C:\\Data" } else { "/Data" },
    );
    let target = build_at(
        &d(vec![
            (
                "docs",
                d(vec![
                    ("report.txt", f(12)),
                    ("ärger.TXT", f(4)),
                    ("NAÏVE", f(2)),
                ]),
            ),
            ("mixed", f(1)),
        ]),
        if cfg!(windows) { "c:\\data" } else { "/data" },
    );
    let tree = check(&base, &target, true);
    assert_eq!(tree.len(), target.read().len()); // everything matched, roots included
    assert_eq!(tree.status[row(&tree, "docs/report.txt")], Status::Changed);
    assert_eq!(tree.status[row(&tree, "docs/ärger.TXT")], Status::Unchanged);
    assert!(name(&tree, top(&tree)).ends_with("data")); // the target's spelling
    let exact = check(&base, &target, false);
    assert_eq!(exact.len(), base.read().len() + target.read().len() - 1);
}

#[test]
fn windows_rule_prefers_the_exact_spelling() {
    let base = build(&d(vec![
        ("A", f(1)),
        ("a", f(2)),
        ("Bb", f(3)),
        ("bB", f(4)),
    ]));
    let target = build(&d(vec![("a", f(2)), ("A", f(1)), ("bb", f(30))]));
    let tree = check(&base, &target, true);
    let got: HashSet<(String, Status)> = (2..tree.len())
        .map(|u| (name(&tree, u), tree.status[u]))
        .collect();
    assert!(got.contains(&("a".into(), Status::Unchanged)));
    assert!(got.contains(&("A".into(), Status::Unchanged)));
    assert!(got.contains(&("bb".into(), Status::Changed)));
    assert!(got.contains(&("bB".into(), Status::Removed)));
}

#[test]
fn posix_rule_is_exact() {
    let tree = check(
        &build(&d(vec![("Readme", f(1))])),
        &build(&d(vec![("README", f(1))])),
        false,
    );
    let mut st: Vec<u8> = tree.status[2..].iter().map(|&s| s as u8).collect();
    st.sort();
    assert_eq!(st, [Status::Added as u8, Status::Removed as u8]);
}

#[test]
fn default_rule_follows_the_platform() {
    let (a, b) = (build(&d(vec![])), build(&d(vec![])));
    let tree = compare(&a.read(), &b.read(), None).unwrap();
    assert_eq!(tree.fold_case, cfg!(windows));
}

#[test]
fn kind_change_is_removed_and_added() {
    let base = build(&d(vec![
        ("x", f(30)),
        ("y", d(vec![("in", f(5))])),
        ("link", S::F(10, None, false, Kind::Link)),
    ]));
    let target = build(&d(vec![
        ("x", d(vec![("inner", f(7))])),
        ("y", f(9)),
        ("link", f(10)),
    ]));
    let tree = check(&base, &target, false);
    assert_eq!(tree.status[row_k(&tree, "x", Kind::File)], Status::Removed);
    assert_eq!(tree.status[row_k(&tree, "x", Kind::Dir)], Status::Added);
    assert_eq!(tree.status[row(&tree, "x/inner")], Status::Added);
    assert_eq!(tree.status[row_k(&tree, "y", Kind::Dir)], Status::Removed);
    assert_eq!(tree.status[row_k(&tree, "y", Kind::File)], Status::Added);
    let l = row(&tree, "link");
    assert_eq!((tree.status[l], tree.delta[l]), (Status::Changed, 0));
}

#[test]
fn hard_link_duplicates_count_zero() {
    let base = build(&d(vec![
        ("a", f(100)),
        ("b", S::F(100, None, true, Kind::File)),
        ("c", S::F(40, None, true, Kind::File)),
    ]));
    let target = build(&d(vec![
        ("a", S::F(100, None, true, Kind::File)),
        ("b", f(100)),
        ("c", S::F(70, None, true, Kind::File)),
    ]));
    let tree = check(&base, &target, false);
    let (a, b, c) = (row(&tree, "a"), row(&tree, "b"), row(&tree, "c"));
    assert_eq!((tree.base_alloc[a], tree.target_alloc[a]), (100, 0));
    assert_eq!((tree.base_alloc[b], tree.target_alloc[b]), (0, 100));
    assert_eq!((tree.delta[c], tree.status[c]), (0, Status::Changed));
    assert_eq!(tree.delta[top(&tree)], 0);
}

#[test]
fn folders_own_blocks() {
    let base = build(&d(vec![("d", S::D(vec![("f".into(), f(10))], 4096))]));
    let target = build(&d(vec![("d", S::D(vec![("f".into(), f(10))], 8192))]));
    let tree = check(&base, &target, false);
    let dn = row(&tree, "d");
    assert_eq!((tree.status[dn], tree.delta[dn]), (Status::Changed, 0));
    let r = top(&tree);
    assert_eq!((tree.delta[r], tree.grow[r]), (4096, 4096));
}

#[test]
fn empty_trees() {
    let (e1, e2) = (Store::new(), Store::new());
    let tree = compare(&e1.read(), &e2.read(), Some(false)).unwrap();
    assert_eq!(tree.len(), 1);
    assert_eq!(tree.status[0], Status::Unchanged);
    assert!(
        tree.changes(Direction::Grow, 10, 0, None, DOMINANT, MINOR)
            .is_empty()
    );
    let full = build(&d(vec![("a", d(vec![("b", f(3))]))]));
    let added = check(&e1, &full, false);
    assert!(added.status[1..].iter().all(|&s| s == Status::Added));
    assert_eq!(added.delta[0], 3);
    let removed = check(&full, &e1, false);
    assert!(removed.status[1..].iter().all(|&s| s == Status::Removed));
    assert_eq!(removed.shrink[0], 3);
    check(&build(&d(vec![])), &build(&d(vec![])), true);
}

#[test]
fn deep_trees() {
    let chain = |depth: usize, leaf: i64| {
        let mut spec = d(vec![("leaf.bin", f(leaf))]);
        for i in 0..depth {
            spec = S::D(vec![(format!("level{i}"), spec)], 0);
        }
        spec
    };
    let (base, target) = (build(&chain(400, 10)), build(&chain(400, 25)));
    let tree = compare(&base.read(), &target.read(), Some(false)).unwrap();
    check_union(&tree, &base.read(), &target.read());
    assert_eq!(*tree.store.read().depths().iter().max().unwrap(), 402);
    assert!(tree.status[1..].iter().all(|&s| s == Status::Changed));
    let units = tree.changes(Direction::Grow, 5, 0, None, DOMINANT, MINOR);
    assert_eq!(units.len(), 1);
    assert!(units[0].path.ends_with("leaf.bin"));
    assert_eq!((units[0].amount, units[0].delta), (15, 15));
}

#[test]
fn multiple_roots() {
    let (x, y, y5, z) = (
        d(vec![("x", f(1))]),
        d(vec![("y", f(2))]),
        d(vec![("y", f(5))]),
        d(vec![("z", f(3))]),
    );
    let base = build_roots(&[("/a", &x), ("/b", &y)], Order::Bfs);
    let target = build_roots(&[("/b/", &y5), ("/c", &z)], Order::Bfs); // "/b/" normalises to "/b"
    let tree = check(&base, &target, false);
    let s = tree.store.read();
    let roots: HashMap<String, Status> = s
        .roots()
        .map(|r| (s.name(r).into_owned(), tree.status[r as usize]))
        .collect();
    assert_eq!(roots["/b/"], Status::Changed);
    assert_eq!(roots["/c"], Status::Added);
    assert_eq!(roots["/a"], Status::Removed);
}

#[test]
fn other_layouts_and_gone_rows() {
    let spec = d(vec![
        (
            "a",
            d(vec![("b", d(vec![("c", f(1)), ("d", f(2))])), ("e", f(3))]),
        ),
        ("f", d(vec![("g", d(vec![("h", f(4))]))])),
        ("i", f(5)),
    ]);
    let base = build(&spec);
    let target = build_roots(&[("/r", &spec)], Order::Dfs);
    for folder in ["a", "a/b", "f/g", ""] {
        let node = {
            let t = target.read();
            if folder.is_empty() {
                t.roots().next().unwrap()
            } else {
                find(&t, folder, None)
            }
        };
        relist_same(&target, node);
    }
    assert!(mutate::gone_count(&target.read()) > 0);
    let tree = check(&base, &target, false);
    assert!(tree.status.iter().all(|&s| s == Status::Unchanged));
    {
        let t = target.read();
        for &r in tree.target_id.iter().filter(|&&r| r >= 0) {
            assert!(!t.flags(r as u32).intersects(Flags::GONE));
        }
    }
    let e = find(&target.read(), "a/e", None);
    {
        let mut w = target.write();
        mutate::update_stats(&mut w, &[e], &[30], &[30], None);
    }
    let tree = check(&base, &target, false);
    assert_eq!(tree.delta[row(&tree, "a/e")], 27);
}

#[test]
fn names_from_several_heap_chunks() {
    let long = "x".repeat(120);
    let mk = |rev: bool| -> S {
        let folders: Vec<(String, S)> = (0..60)
            .map(|i| {
                let mut files: Vec<(String, S)> = (0..30)
                    .map(|j| (format!("{long}{j:04}.dat"), f(i + j)))
                    .collect();
                if rev {
                    files.reverse();
                    if i == 7 {
                        files.push((format!("{long}9999.dat"), f(5)));
                    }
                }
                (format!("dir{i:03}"), S::D(files, 0))
            })
            .collect();
        S::D(folders, 0)
    };
    let (bs, ts) = (mk(false), mk(true));
    let base = build(&bs);
    let target = build_roots(&[("/r", &ts)], Order::Dfs);
    assert!(base.read().name_chunks() > 1 && target.read().name_chunks() > 1);
    let tree = check(&base, &target, false);
    assert_eq!(
        tree.status.iter().filter(|&&s| s == Status::Added).count(),
        1
    );
}

#[test]
fn progress_and_cancel() {
    let base = build(&d(vec![("a", d(vec![("b", f(1))])), ("c", f(2))]));
    let target = build(&d(vec![("a", d(vec![("b", f(3))])), ("d", f(2))]));
    let mut seen: Vec<f64> = Vec::new();
    let mut record = |x: f64| seen.push(x);
    compare_with(
        &base.read(),
        &target.read(),
        CompareOptions {
            fold_case: Some(false),
            progress: Some(&mut record),
            ..CompareOptions::default()
        },
    )
    .unwrap();
    assert!(!seen.is_empty() && seen.windows(2).all(|w| w[0] <= w[1]));
    assert!(seen[0] >= 0.0 && *seen.last().unwrap() == 1.0);
    let calls = std::cell::Cell::new(0);
    let cancel = || {
        calls.set(calls.get() + 1);
        calls.get() > 2
    };
    let err = compare_with(
        &base.read(),
        &target.read(),
        CompareOptions {
            fold_case: Some(false),
            cancel: Some(&cancel),
            ..CompareOptions::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, DiffError::Cancelled));
    assert_eq!(err.to_string(), "对比已取消");
}

#[test]
fn the_layout_engine_draws_the_union() {
    use sk_core::layout::contract::check_draw;
    use sk_core::layout::{
        ArraySizes, Basis, DrawList, LayoutParams, RootSpec, SizeSource, StoreSizes, treemap,
    };
    let names = |n: usize, size: &dyn Fn(usize) -> i64| -> S {
        S::D((0..n).map(|i| (format!("f{i}"), f(size(i)))).collect(), 0)
    };
    let base = build(&d(vec![
        ("a", names(20, &|i| 100 * i as i64)),
        ("b", d(vec![("g", f(5000))])),
    ]));
    let target = build(&d(vec![
        (
            "a",
            names(25, &|i| 100 * i as i64 + if i % 3 != 0 { 50 } else { 0 }),
        ),
        ("c", d(vec![("h", f(800))])),
    ]));
    let tree = check(&base, &target, false);
    let nodes = tree.store.read();
    let root = RootSpec::node(nodes.roots().next().unwrap());
    let params = LayoutParams {
        width: 600.0,
        height: 400.0,
        min_element: 1.0,
        ..LayoutParams::default()
    };
    let basis = Basis::default();
    let lay = |sizes: &dyn SizeSource| {
        let mut out = DrawList::new();
        treemap(&nodes, &basis, &params, &root, sizes, &mut out);
        let problems = check_draw(&out, &nodes, &root, true, Some(sizes));
        assert!(problems.is_empty(), "{problems:#?}");
        out
    };
    let normal = lay(&StoreSizes::of(&nodes));
    let changed = lay(&ArraySizes(std::sync::Arc::new(tree.area())));
    assert!(!normal.is_empty() && !changed.is_empty());
    let drawn: HashSet<i32> = changed.node.iter().copied().collect();
    assert!(
        drawn.contains(&(row(&tree, "b/g") as i32)) && drawn.contains(&(row(&tree, "c/h") as i32))
    );
    assert!(!drawn.contains(&(row(&tree, "a/f0") as i32))); // unchanged: no area
}

// --------------------------------------------------------------------------- change units

fn rel(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("/r/")
        .to_string()
}

fn changes(tree: &DiffTree, dir: Direction, limit: usize) -> Vec<sk_core::diff::Change> {
    tree.changes(dir, limit, 0, None, DOMINANT, MINOR)
}

#[test]
fn changes_ranking() {
    let base = build(&d(vec![
        ("one", d(vec![("keep", f(1000)), ("tiny", f(1))])),
        ("many", d(vec![("old", f(5))])),
        ("two", d(vec![("p", f(10)), ("q", f(10)), ("r", f(1))])),
        ("gone", d(vec![("g", f(400))])),
    ]));
    let mut many: Vec<(String, S)> = vec![("old".into(), f(5))];
    many.extend((0..100).map(|i| (format!("n{i:03}"), f(9))));
    let target = build(&S::D(
        vec![
            (
                "one".into(),
                d(vec![
                    ("keep", f(1000)),
                    ("tiny", f(2)),
                    ("big.iso", f(1000)),
                ]),
            ),
            ("many".into(), S::D(many, 0)),
            (
                "two".into(),
                d(vec![("p", f(310)), ("q", f(260)), ("r", f(2))]),
            ),
        ],
        0,
    ));
    let tree = check(&base, &target, false);
    let grow = changes(&tree, Direction::Grow, 10);
    let got: Vec<(String, i64, bool)> = grow
        .iter()
        .map(|c| (rel(&c.path), c.amount, c.rest))
        .collect();
    let want: Vec<(String, i64, bool)> = [
        ("one/big.iso", 1000, false),
        ("many", 900, false),
        ("two/p", 300, false),
        ("two/q", 250, false),
        ("one", 1, true),
        ("two", 1, true),
    ]
    .iter()
    .map(|(p, a, r)| (p.to_string(), *a, *r))
    .collect();
    assert_eq!(got, want);
    assert_eq!((grow[1].members, grow[4].members), (0, 1));
    assert_eq!(grow.iter().map(|c| c.amount).sum::<i64>(), tree.grow[0]);
    let unit = &grow[0];
    assert_eq!(
        (
            unit.status,
            unit.kind,
            unit.base_alloc,
            unit.target_alloc,
            unit.delta
        ),
        (Status::Added, Kind::File, 0, 1000, 1000)
    );
    let shrink = changes(&tree, Direction::Shrink, 10);
    assert_eq!(shrink.len(), 1);
    assert_eq!(
        (rel(&shrink[0].path), shrink[0].amount, shrink[0].status),
        ("gone/g".into(), 400, Status::Removed)
    );
    assert_eq!(changes(&tree, Direction::Grow, 2).len(), 2);
    assert!(changes(&tree, Direction::Grow, 0).is_empty());
    let whole = tree.changes(Direction::Grow, 10, 0, None, 2.0, MINOR);
    assert_eq!(whole.len(), 1);
    assert_eq!(
        (whole[0].path.as_str(), whole[0].amount),
        ("/r", tree.grow[0])
    );
    assert!(Direction::parse("bigger").is_err());
}

#[test]
fn changes_in_a_scope_and_with_other_amounts() {
    let base = build(&d(vec![
        ("one", d(vec![("keep", f(1000))])),
        ("two", d(vec![("p", f(10)), ("q", f(10))])),
        ("three", d(vec![("x", f(50))])),
    ]));
    let target = build(&d(vec![
        ("one", d(vec![("keep", f(1000)), ("big.iso", f(1000))])),
        ("two", d(vec![("p", f(310)), ("q", f(260))])),
        ("three", d(vec![])),
    ]));
    let tree = check(&base, &target, false);
    let two = row(&tree, "two") as NodeId;
    let inside = tree.changes(Direction::Grow, 10, two, None, DOMINANT, MINOR);
    let got: Vec<(String, i64)> = inside.iter().map(|c| (rel(&c.path), c.amount)).collect();
    assert_eq!(
        got,
        [("two/p".to_string(), 300), ("two/q".to_string(), 250)]
    );
    let p = row(&tree, "two/p") as NodeId;
    assert_eq!(
        tree.changes(Direction::Grow, 10, p, None, DOMINANT, MINOR)[0].amount,
        300
    );
    let three = row(&tree, "three") as NodeId;
    assert!(
        tree.changes(Direction::Grow, 10, three, None, DOMINANT, MINOR)
            .is_empty()
    );
    let mut amount = vec![0i64; tree.len()];
    {
        let s = tree.store.read();
        amount[two as usize] = tree.grow[two as usize];
        for c in s.children(two) {
            amount[c as usize] = tree.grow[c as usize];
        }
        let anc: Vec<NodeId> = s.ancestors(two).chain([0]).collect();
        for a in anc {
            amount[a as usize] = tree.grow[two as usize];
        }
    }
    let only_two = tree.changes(Direction::Grow, 10, 0, Some(&amount), DOMINANT, MINOR);
    let got: Vec<(String, i64)> = only_two.iter().map(|c| (rel(&c.path), c.amount)).collect();
    assert_eq!(
        got,
        [("two/p".to_string(), 300), ("two/q".to_string(), 250)]
    );
    assert_eq!((only_two[0].delta, only_two[0].target_alloc), (300, 310));
}

// --------------------------------------------------------------------------- random trees

const NAMES: &[&str] = &[
    "a",
    "A",
    "b",
    "ab",
    "Ab",
    "aB",
    "x.txt",
    "X.TXT",
    "ä",
    "Ä",
    "中文",
    "long-name-0123456789",
    "LONG-name-0123456789",
    "é",
    "\u{212a}",
];

fn leaf(rng: &mut Rng) -> S {
    if rng.chance(0.5) {
        return f(rng.range(0, 2000));
    }
    let alloc = if rng.chance(0.5) {
        None
    } else {
        Some(rng.range(0, 4097))
    };
    let kind = if rng.chance(0.5) {
        Kind::File
    } else {
        Kind::Link
    };
    S::F(rng.range(0, 2000), alloc, rng.chance(0.5), kind)
}

fn random_tree(rng: &mut Rng, depth: u32, budget: &mut i32) -> S {
    let n = rng.below(if depth == 0 { 5 } else { 7 }) as usize;
    let mut out: Vec<(String, S)> = Vec::new();
    for _ in 0..n {
        if *budget <= 0 {
            break;
        }
        let name = rng.pick(NAMES).to_string();
        if out.iter().any(|(k, _)| *k == name) {
            continue;
        }
        *budget -= 1;
        let v = if depth > 0 && rng.chance(0.35) {
            let inner = random_tree(rng, depth - 1, budget);
            if rng.chance(0.3) {
                if let S::D(k, _) = inner {
                    S::D(k, rng.range(0, 65))
                } else {
                    inner
                }
            } else {
                inner
            }
        } else {
            leaf(rng)
        };
        out.push((name, v));
    }
    S::D(out, 0)
}

fn swapcase(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_lowercase() {
                c.to_uppercase().collect::<Vec<_>>()
            } else {
                c.to_lowercase().collect::<Vec<_>>()
            }
        })
        .collect()
}

/// `spec` a while later: entries kept, dropped, resized, re-cased or turned into the other kind,
/// a few added, listed in another order.
fn mutated(rng: &mut Rng, spec: &S) -> S {
    let mut items: Vec<(String, S)> = kids(spec).to_vec();
    if rng.chance(0.5) {
        for i in (1..items.len()).rev() {
            let j = rng.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }
    let mut out: Vec<(String, S)> = Vec::new();
    for (name, value) in items {
        let op = *rng.pick(&["keep", "keep", "keep", "drop", "resize", "recase", "flip"]);
        if op == "drop" {
            continue;
        }
        let mut name = name;
        let sw = swapcase(&name);
        if op == "recase"
            && !kids(spec).iter().any(|(k, _)| *k == sw)
            && !out.iter().any(|(k, _)| *k == sw)
        {
            name = sw;
        }
        let is_dir = matches!(value, S::D(..));
        let value = if op == "flip" {
            if is_dir { f(7) } else { S::D(vec![], 0) }
        } else if op == "resize" && !is_dir {
            leaf(rng)
        } else if is_dir {
            match mutated(rng, &value) {
                S::D(k, _) => S::D(k, if let S::D(_, a) = value { a } else { 0 }),
                other => other,
            }
        } else {
            value
        };
        if !out.iter().any(|(k, _)| *k == name) {
            out.push((name, value));
        }
    }
    for _ in 0..rng.below(3) {
        let name = rng.pick(NAMES).to_string();
        if !out.iter().any(|(k, _)| *k == name) {
            out.push((name, leaf(rng)));
        }
    }
    S::D(out, 0)
}

#[test]
fn random_trees_match_the_reference() {
    let mut rng = Rng::new(20_251_105);
    for _ in 0..300 {
        let mut budget = 40;
        let spec = random_tree(&mut rng, 3, &mut budget);
        let other = if rng.chance(0.5) {
            mutated(&mut rng, &spec)
        } else {
            let mut budget = 40;
            random_tree(&mut rng, 3, &mut budget)
        };
        let fold = rng.chance(0.5);
        let order = if rng.chance(0.5) {
            Order::Bfs
        } else {
            Order::Dfs
        };
        let base = build(&spec);
        let target = build_roots(&[("/r", &other)], order);
        if rng.chance(0.5) {
            let folders: Vec<NodeId> = {
                let t = target.read();
                (1..t.len() as NodeId).filter(|&r| t.is_dir(r)).collect()
            };
            if !folders.is_empty() {
                for _ in 0..rng.below(4) {
                    let folder = *rng.pick(&folders);
                    let ok = {
                        let t = target.read();
                        mutate::can_relist(&t, folder) && !t.flags(folder).intersects(Flags::GONE)
                    };
                    if ok {
                        relist_same(&target, folder);
                    }
                }
            }
        }
        check(&base, &target, fold);
    }
}

fn big_tree(rng: &mut Rng, n_dirs: usize) -> S {
    fn grow(rng: &mut Rng, depth: u32, dirs: &mut usize) -> S {
        let mut out: Vec<(String, S)> = Vec::new();
        let n = rng.below(12) as usize;
        for i in 0..n {
            let name = format!("{}{i}", if rng.chance(0.2) { "Item" } else { "item" });
            if depth > 0 && *dirs > 0 && rng.chance(0.3) {
                *dirs -= 1;
                out.push((name, grow(rng, depth - 1, dirs)));
            } else {
                out.push((name, f(rng.range(0, 1_000_000))));
            }
        }
        S::D(out, 0)
    }
    let mut dirs = n_dirs;
    let mut kids = Vec::new();
    let mut i = 0;
    while dirs > 0 {
        dirs -= 1;
        kids.push((format!("top{i}"), grow(rng, 6, &mut dirs)));
        i += 1;
    }
    S::D(kids, 0)
}

#[test]
fn changes_partition_random_trees() {
    let mut rng = Rng::new(3);
    let spec = big_tree(&mut rng, 2000);
    let other = mutated(&mut rng, &spec);
    let base = build(&spec);
    let target = build_roots(&[("/r", &other)], Order::Dfs);
    for fold in [false, true] {
        let tree = check(&base, &target, fold);
        let s = tree.store.read();
        for (dir, total) in [
            (Direction::Grow, tree.grow[0]),
            (Direction::Shrink, tree.shrink[0]),
        ] {
            let units = changes(&tree, dir, 1_000_000);
            assert_eq!(units.iter().map(|c| c.amount).sum::<i64>(), total);
            let amounts: Vec<i64> = units.iter().map(|c| c.amount).collect();
            assert!(amounts.windows(2).all(|w| w[0] >= w[1]));
            assert!(amounts.iter().all(|&a| a > 0));
            let whole: Vec<NodeId> = units.iter().filter(|c| !c.rest).map(|c| c.node).collect();
            let set: HashSet<NodeId> = whole.iter().copied().collect();
            assert_eq!(set.len(), whole.len());
            for &node in &whole {
                assert!(!s.ancestors(node).any(|a| set.contains(&a)));
            }
        }
    }
}

// --------------------------------------------------------------------------- change tree

#[test]
fn change_tree_keeps_what_changed() {
    let base = build(&d(vec![
        ("a", d(vec![("x", f(10)), ("y", f(100))])),
        ("same", d(vec![("z", f(1))])),
    ]));
    let target = build(&d(vec![
        ("a", d(vec![("x", f(10)), ("y", f(40)), ("new", f(7))])),
        ("same", d(vec![("z", f(1))])),
    ]));
    let tree = check(&base, &target, false);
    let ct = ChangeTree::of(&tree, StoreOptions::default()).unwrap();
    let s = ct.store().read();
    check_invariants(&s).unwrap();
    let names: Vec<String> = (0..s.len() as NodeId)
        .map(|i| s.path_lossy(i).replace('\\', "/"))
        .collect();
    assert_eq!(names, ["", "/r", "/r/a", "/r/a/y", "/r/a/new"]);
    let a = 2;
    assert_eq!(s.tot_alloc(a), 67);
    assert_eq!(s.alloc(3), 60);
    assert_eq!(ct.new_files[1], 1);
    assert_eq!(ct.diff.delta[a as usize], -53);
    assert_eq!(
        ct.amounts(true, false, Some(&[false, false, false, false, true])),
        vec![7, 7, 7, 0, 7]
    );
    assert_eq!(short_label(&s, 3, 0), "y");
}

#[test]
fn short_labels() {
    let spec = d(vec![(
        "WeChat Files",
        d(vec![(
            "wxid_abc12345",
            d(vec![("Video", d(vec![("v.mp4", f(5))]))]),
        )]),
    )]);
    let store = build(&spec);
    let s = store.read();
    let video = find(&s, "WeChat Files/wxid_abc12345/Video", None);
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
        short_label(&s, video, 0),
        format!("WeChat Files{sep}…{sep}Video")
    );
    let file = find(&s, "WeChat Files/wxid_abc12345/Video/v.mp4", None);
    assert_eq!(short_label(&s, file, 0), "v.mp4");
}

/// `cargo test --release -p sk-core --test diff_compare -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_large_compare() {
    let mut rng = Rng::new(11);
    let spec = big_tree(&mut rng, 150_000);
    let other = mutated(&mut rng, &spec);
    let base = build(&spec);
    let target = build_roots(&[("/r", &other)], Order::Dfs);
    let (b, t) = (base.read(), target.read());
    for fold in [false, true] {
        let t0 = std::time::Instant::now();
        let tree = compare(&b, &t, Some(fold)).unwrap();
        eprintln!(
            "compare {} + {} rows -> {} (fold {fold}) in {:.3} s",
            b.len(),
            t.len(),
            tree.len(),
            t0.elapsed().as_secs_f64()
        );
    }
    let t0 = std::time::Instant::now();
    let same = compare(&b, &b, Some(true)).unwrap();
    eprintln!(
        "self-compare {} rows in {:.3} s",
        same.len(),
        t0.elapsed().as_secs_f64()
    );
}
