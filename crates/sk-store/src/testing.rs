//! Fixture trees for tests (feature `test-support`): stores from nested specs, random stores, an
//! in-memory "disk" that can be scanned in slices and changed for relists, a tree on a real
//! disk, and invariant checks. Other crates use it with
//! `sk-store = { workspace = true, features = ["test-support"] }` in their dev-dependencies.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

use crate::mutate;
use crate::store::{Nodes, Store, Writer};
use crate::types::*;

// --------------------------------------------------------------------------- random numbers

/// A small deterministic generator (SplitMix64).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `0..n` (n > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo).max(1) as u64) as i64
    }
    pub fn chance(&mut self, p: f64) -> bool {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 * 1.0 < p
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

// --------------------------------------------------------------------------- spec trees

/// A tree to build: a file of a size, or a folder of named children (in listing order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Spec {
    File(i64),
    Dir(Vec<(String, Spec)>),
}

/// A file of `size` bytes.
pub fn f(size: i64) -> Spec {
    Spec::File(size)
}

/// A folder.
pub fn d<'a>(children: impl IntoIterator<Item = (&'a str, Spec)>) -> Spec {
    Spec::Dir(
        children
            .into_iter()
            .map(|(n, s)| (n.to_string(), s))
            .collect(),
    )
}

impl Spec {
    pub fn children(&self) -> &[(String, Spec)] {
        match self {
            Spec::Dir(c) => c,
            Spec::File(_) => &[],
        }
    }

    pub fn children_mut(&mut self) -> &mut Vec<(String, Spec)> {
        match self {
            Spec::Dir(c) => c,
            Spec::File(_) => panic!("not a folder"),
        }
    }

    /// The node at a `/`-separated relative path.
    pub fn get(&self, path: &str) -> Option<&Spec> {
        let mut node = self;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            node = &node.children().iter().find(|(n, _)| n == part)?.1;
        }
        Some(node)
    }

    pub fn get_mut(&mut self, path: &str) -> Option<&mut Spec> {
        let mut node = self;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let Spec::Dir(c) = node else { return None };
            node = &mut c.iter_mut().find(|(n, _)| n == part)?.1;
        }
        Some(node)
    }

    /// Files and folders below (not counting itself).
    pub fn counts(&self) -> (usize, usize) {
        let mut files = 0;
        let mut dirs = 0;
        for (_, c) in self.children() {
            match c {
                Spec::File(_) => files += 1,
                Spec::Dir(_) => {
                    dirs += 1;
                    let (a, b) = c.counts();
                    files += a;
                    dirs += b;
                }
            }
        }
        (files, dirs)
    }
}

/// Lower-case extension of a name ("" if none, or longer than 16).
pub fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 && name.len() - dot <= 17 => name[dot + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Allocation of a file of `size` bytes in clusters of `cluster`.
pub fn round_up(size: i64, cluster: i64) -> i64 {
    if cluster <= 1 {
        size
    } else {
        (size + cluster - 1) / cluster * cluster
    }
}

/// The entries of a spec folder as a scan would list them (extensions from `ext`).
pub fn listing_of(
    children: &[(String, Spec)],
    cluster: i64,
    ext: &mut dyn FnMut(&str) -> u16,
) -> EntryBatch {
    let mut b = EntryBatch::new();
    for (name, spec) in children {
        let e = match spec {
            Spec::File(size) => Entry {
                ext: ext(&ext_of(name)),
                ..Entry::file(name.as_bytes(), *size, round_up(*size, cluster))
            },
            Spec::Dir(_) => Entry::dir(name.as_bytes()),
        };
        b.push(e);
    }
    b
}

/// A store with one scan root built from `spec` with `ingest_listing`, breadth first; file
/// allocation = size rounded up to `cluster`. Folders at the `pending` paths stay unlisted.
pub fn build_with(spec: &Spec, root: &str, cluster: i64, pending: &[&str]) -> Store {
    let store = Store::new();
    {
        let mut app = store.appender();
        let root_id = app.add_root_dir(root).unwrap();
        let mut queue: VecDeque<(NodeId, &Spec, String)> =
            VecDeque::from([(root_id, spec, String::new())]);
        while let Some((node, s, prefix)) = queue.pop_front() {
            let batch = listing_of(s.children(), cluster, &mut |e| app.ext_id(e));
            let first = app.ingest_listing(node, &batch).unwrap();
            for (i, (name, child)) in s.children().iter().enumerate() {
                if let Spec::Dir(_) = child {
                    let path = format!("{prefix}/{name}");
                    if pending.contains(&path.trim_start_matches('/')) {
                        continue;
                    }
                    queue.push_back((first + i as NodeId, child, path));
                }
            }
        }
    }
    store
}

/// [`build_with`] with no rounding and nothing pending, root `/root`.
pub fn build(spec: &Spec) -> Store {
    build_with(spec, "/root", 1, &[])
}

/// The node at a `/`-separated path below the first root.
pub fn find(nodes: &Nodes, path: &str) -> NodeId {
    let mut node = nodes.roots().next().expect("a root");
    for part in path.split('/').filter(|p| !p.is_empty()) {
        node = nodes
            .children(node)
            .find(|&c| nodes.name_bytes(c) == part.as_bytes())
            .unwrap_or_else(|| panic!("no {part} in {path}"));
    }
    node
}

/// A batch named `n0, n1, …` of the given kinds and sizes (allocation rounded to 4096).
pub fn batch(kinds: &[Kind], sizes: &[i64], flags: &[Flags]) -> EntryBatch {
    let names: Vec<String> = (0..kinds.len()).map(|i| format!("n{i}")).collect();
    named_batch(
        &names.iter().map(String::as_str).collect::<Vec<_>>(),
        kinds,
        sizes,
        flags,
    )
}

/// A batch of named entries (allocation rounded to 4096; `flags` may be empty).
pub fn named_batch(names: &[&str], kinds: &[Kind], sizes: &[i64], flags: &[Flags]) -> EntryBatch {
    let mut b = EntryBatch::new();
    for (i, name) in names.iter().enumerate() {
        b.push(Entry {
            flags: flags.get(i).copied().unwrap_or_default(),
            size: sizes[i],
            alloc: round_up(sizes[i], 4096),
            ..Entry::new(name.as_bytes(), kinds[i])
        });
    }
    b
}

/// `dirs` folders then `files` files (allocation 4096 each), named `{prefix}{i}`.
pub fn dirs_and_files(prefix: &str, dirs: usize, files: usize) -> EntryBatch {
    let mut b = EntryBatch::new();
    for i in 0..dirs + files {
        let name = format!("{prefix}{i}");
        b.push(if i < dirs {
            Entry::dir(name.as_bytes())
        } else {
            Entry::file(name.as_bytes(), 0, 4096)
        });
    }
    b
}

/// A random tree listed one folder at a time (files, folders, links; 5% hard-link duplicates).
pub fn random_store(seed: u64, n_dirs: usize) -> Store {
    let mut rng = Rng::new(seed);
    let store = Store::new();
    {
        let mut app = store.appender();
        let mut pending = vec![app.add_root_dir("/root").unwrap()];
        let mut listed = 0;
        while !pending.is_empty() && listed < n_dirs {
            let d = pending.swap_remove(rng.below(pending.len() as u64) as usize);
            let k = rng.below(8) as usize;
            let kinds: Vec<Kind> = (0..k)
                .map(|_| *rng.pick(&[Kind::File, Kind::File, Kind::Dir, Kind::Link]))
                .collect();
            let sizes: Vec<i64> = kinds
                .iter()
                .map(|&kd| {
                    if kd == Kind::File {
                        rng.range(0, 10_000_000)
                    } else {
                        0
                    }
                })
                .collect();
            let flags: Vec<Flags> = kinds
                .iter()
                .map(|&kd| {
                    if kd == Kind::File && rng.chance(0.05) {
                        Flags::HARDLINK_DUP
                    } else {
                        Flags::NONE
                    }
                })
                .collect();
            let start = app
                .ingest_listing(d, &batch(&kinds, &sizes, &flags))
                .unwrap();
            pending.extend(
                kinds
                    .iter()
                    .enumerate()
                    .filter(|(_, k)| **k == Kind::Dir)
                    .map(|(i, _)| start + i as NodeId),
            );
            listed += 1;
        }
    }
    store
}

// --------------------------------------------------------------------------- an in-memory disk

/// Scans `spec` into a new store as a scanner does: slices of up to `per_slice` listings, a
/// listing naming entries of earlier listings of its slice, the rest queued by id.
pub fn scan_spec(spec: &Spec, root: &str, per_slice: usize) -> Store {
    let store = Store::new();
    {
        let mut app = store.appender();
        let root_id = app.add_root_dir(root).unwrap();
        let mut queue: VecDeque<(NodeId, &Spec)> = VecDeque::from([(root_id, spec)]);
        ingest_queue(&mut app, &mut queue, per_slice, 1);
    }
    store
}

/// Lists every folder of the queue (and below) with `ingest_slice`.
pub fn ingest_queue<'s>(
    app: &mut crate::Appender<'_>,
    queue: &mut VecDeque<(NodeId, &'s Spec)>,
    per_slice: usize,
    cluster: i64,
) {
    while !queue.is_empty() {
        let mut refs = Vec::new();
        let mut bounds = vec![0u32];
        let mut all = EntryBatch::new();
        let mut inner: VecDeque<(SliceRef, &'s Spec)> = VecDeque::new();
        let mut later: Vec<(u32, u32, &'s Spec)> = Vec::new(); // (listing, entry) for the frontier
        if let Some((id, s)) = queue.pop_front() {
            inner.push_back((SliceRef::Node(id), s));
        }
        while let Some((r, s)) = inner.pop_front() {
            if refs.len() >= per_slice.max(1) {
                match r {
                    SliceRef::Entry { listing, entry } => later.push((listing, entry, s)),
                    SliceRef::Node(_) => unreachable!(),
                }
                continue;
            }
            let k = refs.len() as u32;
            refs.push(r);
            let b = listing_of(s.children(), cluster, &mut |e| app.ext_id(e));
            for (i, (_, child)) in s.children().iter().enumerate() {
                if let Spec::Dir(_) = child {
                    inner.push_back((
                        SliceRef::Entry {
                            listing: k,
                            entry: i as u32,
                        },
                        child,
                    ));
                }
            }
            append_batch(&mut all, &b);
            bounds.push(all.len() as u32);
        }
        let starts = app.ingest_slice(&refs, &bounds, &all).unwrap();
        for (listing, entry, s) in later {
            queue.push_back((starts[listing as usize].unwrap() + entry, s));
        }
    }
}

/// Appends `b`'s rows to `all`.
pub fn append_batch(all: &mut EntryBatch, b: &EntryBatch) {
    all.names.extend_from_slice(&b.names);
    all.name_lens.extend_from_slice(&b.name_lens);
    all.ext.extend_from_slice(&b.ext);
    all.kind.extend_from_slice(&b.kind);
    all.flags.extend_from_slice(&b.flags);
    all.size.extend_from_slice(&b.size);
    all.alloc.extend_from_slice(&b.alloc);
    all.mtime.extend_from_slice(&b.mtime);
    all.ctime.extend_from_slice(&b.ctime);
    all.atime.extend_from_slice(&b.atime);
    all.ino.extend_from_slice(&b.ino);
    all.nlink.extend_from_slice(&b.nlink);
}

/// Lists `folder` again from `spec` (its current contents) as the monitor does, and scans the
/// folders that are new.
pub fn relist_from_spec(w: &mut Writer<'_>, folder: NodeId, spec: &Spec) -> mutate::Relist {
    let batch = listing_of(spec.children(), 1, &mut |e| w.ext_id(e));
    let rel = mutate::relist(w, folder, &batch).unwrap();
    let mut queue = VecDeque::new();
    for &p in &rel.pending {
        let name = w.name(p).into_owned();
        let child = &spec.children().iter().find(|(n, _)| *n == name).unwrap().1;
        queue.push_back((p, child));
    }
    ingest_queue(w, &mut queue, 4, 1);
    rel
}

/// Path → (kind, size, alloc, tot_size, tot_alloc, tot_files, tot_dirs, depth) of every node
/// reachable from the roots.
pub fn facts(nodes: &Nodes) -> BTreeMap<String, [i64; 8]> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<NodeId> = nodes.roots().collect();
    while let Some(node) = stack.pop() {
        assert!(
            !nodes.flags(node).intersects(Flags::GONE),
            "a GONE row in the tree"
        );
        out.insert(
            nodes.path_lossy(node).replace('\\', "/"),
            [
                nodes.kind_raw(node) as i64,
                nodes.size(node),
                nodes.alloc(node),
                nodes.tot_size(node),
                nodes.tot_alloc(node),
                nodes.tot_files(node) as i64,
                nodes.tot_dirs(node) as i64,
                nodes.depth(node) as i64,
            ],
        );
        stack.extend(nodes.children(node));
    }
    out
}

/// Every live node is exactly one folder's child with the right parent and depth; `GONE` rows
/// are in no range; the totals equal a recount.
pub fn check_invariants(nodes: &Nodes) -> Result<(), String> {
    let n = nodes.len();
    let mut seen = vec![0u32; n];
    for d in 0..n as NodeId {
        let gone = nodes.flags(d).intersects(Flags::GONE);
        if gone || !(nodes.is_dir(d) || d == 0) {
            if nodes.child_count(d) > 0 && gone {
                return Err(format!("GONE row {d} has children"));
            }
            continue;
        }
        for c in nodes.children(d) {
            if nodes.parent(c) != d as i32 {
                return Err(format!(
                    "node {c} in the range of {d} has parent {}",
                    nodes.parent(c)
                ));
            }
            if nodes.depth(c) != nodes.depth(d) + 1 {
                return Err(format!(
                    "node {c} has depth {} below depth {}",
                    nodes.depth(c),
                    nodes.depth(d)
                ));
            }
            seen[c as usize] += 1;
        }
    }
    for (i, &times) in seen.iter().enumerate().skip(1) {
        let gone = nodes.flags(i as NodeId).intersects(Flags::GONE);
        let want = if gone { 0 } else { 1 };
        if times != want {
            return Err(format!("node {i} is in {times} ranges (gone: {gone})"));
        }
    }
    // totals: a recount over a breadth-first order
    let order = crate::store::Appender::bfs_order(nodes);
    let mut sub = vec![[0i64; 4]; n];
    for &x in order.iter().rev() {
        let p = nodes.parent(x);
        if x == 0 || p < 0 {
            continue;
        }
        let counted = !nodes.flags(x).intersects(Flags::HARDLINK_DUP);
        let own = sub[x as usize];
        let s = &mut sub[p as usize];
        s[0] += own[0] + if counted { nodes.size(x) } else { 0 };
        s[1] += own[1] + if counted { nodes.alloc(x) } else { 0 };
        s[2] += own[2] + (nodes.kind(x) == Kind::File) as i64;
        s[3] += own[3] + nodes.is_dir(x) as i64;
    }
    for &x in &order {
        let have = [
            nodes.tot_size(x),
            nodes.tot_alloc(x),
            nodes.tot_files(x) as i64,
            nodes.tot_dirs(x) as i64,
        ];
        if have != sub[x as usize] {
            return Err(format!(
                "totals of {x}: {have:?}, recount {:?}",
                sub[x as usize]
            ));
        }
    }
    Ok(())
}

/// Two views hold the same rows: every column but `name_off`, and the same names.
pub fn assert_same_store(a: &Nodes, b: &Nodes) {
    assert_eq!(a.len(), b.len(), "node counts");
    for c in Col::ALL {
        if c == Col::NameOff {
            continue;
        }
        assert!(
            a.column_bytes(c, 0..a.len()) == b.column_bytes(c, 0..b.len()),
            "column {}",
            c.name()
        );
    }
    for i in 0..a.len() as NodeId {
        assert_eq!(a.name_bytes(i), b.name_bytes(i), "name of {i}");
    }
}

/// Every node's name, in id order.
pub fn all_names(nodes: &Nodes) -> Vec<Vec<u8>> {
    (0..nodes.len() as NodeId)
        .map(|i| nodes.name_bytes(i).to_vec())
        .collect()
}

// --------------------------------------------------------------------------- a tree on disk

/// What [`build_disk_tree`] made.
#[derive(Clone, Debug)]
pub struct TreeFacts {
    pub files: usize,
    /// excluding the root itself
    pub dirs: usize,
    pub size: u64,
}

/// The files of the synthetic disk tree (relative path, size).
pub fn disk_tree_files() -> Vec<(String, u64)> {
    let mut files: Vec<(String, u64)> = vec![
        ("a/one.bin".into(), 1000),
        ("a/two.bin".into(), 5000),
        ("a/sub/movie.mp4".into(), 123456),
        ("c.txt".into(), 10),
        ("中文 文件夹/照片.jpg".into(), 2048),
        ("deep/l1/l2/l3/l4/x.dat".into(), 777),
        ("empty.dat".into(), 0),
    ];
    files.extend((0..300).map(|i| (format!("many/f{i:03}.log"), 100 + i)));
    files
}

/// A synthetic directory tree with exactly known contents under `root` (for scanner tests).
pub fn build_disk_tree(root: &Path) -> std::io::Result<TreeFacts> {
    let mut dirs = std::collections::BTreeSet::from(["b".to_string(), "b/inner".to_string()]);
    let files = disk_tree_files();
    for (rel, size) in &files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, vec![b'x'; *size as usize])?;
        let mut parent = Path::new(rel).parent();
        while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
            dirs.insert(p.to_string_lossy().replace('\\', "/"));
            parent = p.parent();
        }
    }
    for dir in ["b", "b/inner"] {
        std::fs::create_dir_all(root.join(dir))?;
    }
    Ok(TreeFacts {
        files: files.len(),
        dirs: dirs.len(),
        size: files.iter().map(|f| f.1).sum(),
    })
}
