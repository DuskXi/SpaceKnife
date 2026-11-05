//! The parallel MFT tree build (`table_to_store`) against a plain serial reference build, column
//! by column (port of backend/tests/test_mft_build.py, where the numpy build was the reference):
//! node order, parents, depths, child ranges, names, extensions, flags, sizes, times, totals, on
//! parsed images and on random tables with every oddity a table can have — unsorted and repeated
//! records, missing, foreign and cyclic parents, metafiles, the root's own entry, streams, huge
//! and negative values.

#[path = "../../sk-native/tests/mft_image/mod.rs"]
#[allow(dead_code, clippy::all)]
mod im;

use std::collections::{HashMap, VecDeque};

use sk_core::scan::mft::{
    BADCLUS_RECORD, FIRST_USER_RECORD, META_FOLDER, MftBuild, ORPHAN_FOLDER, ROOT_RECORD,
    table_to_store,
};
use sk_native::mft::{MftTable, parse_mft};
use sk_store::testing::check_invariants;
use sk_store::{Flags, Kind, Nodes, Store};

const RS: usize = 1024;
const CS: usize = 4096;
const CYCLE_LIMIT: usize = 4096;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo.wrapping_add((self.next() % hi.wrapping_sub(lo) as u64) as i64)
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() % 1_000_000) as f64 / 1e6 < p
    }
}

/// A table with every oddity: records unsorted, repeated (not always next to each other),
/// metafile and root records, parents anywhere (unknown, files, self, cycles), any values, names
/// in the blob in another order than the rows.
fn random_table(rng: &mut Rng, n: usize, names: Option<Vec<Vec<u8>>>) -> MftTable {
    let top = (n / 2 + 20).max(8) as u64;
    let mut record: Vec<u64> = (0..n).map(|_| rng.below(top)).collect();
    if rng.chance(0.5) {
        record.sort_unstable();
    }
    let mut parent: Vec<u64> = (0..n)
        .map(|_| {
            if rng.chance(0.9) {
                rng.below(top)
            } else {
                rng.below(1 << 47)
            }
        })
        .collect();
    let alphabet: Vec<&[u8]> = vec![b"a", b"b", b".", b"c", "文".as_bytes(), b"x", b"T", b"Z"];
    let names: Vec<Vec<u8>> = names.unwrap_or_else(|| {
        (0..n)
            .map(|_| {
                (0..rng.below(9))
                    .flat_map(|_| alphabet[rng.below(8) as usize].to_vec())
                    .collect()
            })
            .collect()
    });
    // names in the blob in a shuffled order
    let mut perm: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        perm.swap(i, rng.below(i as u64 + 1) as usize);
    }
    let mut blob = Vec::new();
    let mut name_off = vec![0u32; n];
    for &i in &perm {
        name_off[i] = blob.len() as u32;
        blob.extend_from_slice(&names[i]);
    }
    let kinds = [0u8, 1, 1, 2];
    let kind: Vec<u8> = (0..n).map(|_| kinds[rng.below(4) as usize]).collect();
    if n > 0 && rng.chance(0.5) {
        // a ring of folders (a parent cycle), sometimes with a tree hanging off it
        let mut firsts: HashMap<u64, usize> = HashMap::new();
        for (i, &r) in record.iter().enumerate() {
            firsts.entry(r).or_insert(i);
        }
        let mut dirs: Vec<usize> = firsts.values().copied().filter(|&i| kind[i] == 1).collect();
        dirs.sort_unstable();
        let len = (2 + rng.below(6) as usize).min(dirs.len());
        if len >= 2 {
            let start = rng.below((dirs.len() - len + 1) as u64) as usize;
            let ring = &dirs[start..start + len];
            for k in 0..len {
                parent[ring[k]] = record[ring[(k + 1) % len]];
            }
        }
    }
    let big = 1i64 << 62;
    let mut ints = |lo: i64, hi: i64| -> Vec<i64> { (0..n).map(|_| rng.range(lo, hi)).collect() };
    let size = ints(-big, big);
    let alloc = ints(-big, big);
    let mtime = ints(-(1 << 40), 1 << 40);
    let ctime = ints(-5, 1 << 33);
    let atime = ints(0, 1 << 32);
    let ads_size = (0..n)
        .map(|_| {
            if rng.chance(0.2) {
                rng.range(-big, big)
            } else {
                0
            }
        })
        .collect();
    let ads_alloc = (0..n)
        .map(|_| {
            if rng.chance(0.2) {
                rng.range(-big, big)
            } else {
                0
            }
        })
        .collect();
    MftTable {
        record,
        parent,
        name_off,
        name_len: names.iter().map(|v| v.len() as u16).collect(),
        kind,
        flags: (0..n).map(|_| rng.below(1 << 19) as u32).collect(),
        size,
        alloc,
        ads_size,
        ads_alloc,
        mtime,
        ctime,
        atime,
        nlink: (0..n).map(|_| rng.below(1 << 16) as u16).collect(),
        names: blob,
        bad_records: 0,
    }
}

// --------------------------------------------------------------------------- the reference

/// One expected node: everything the build writes for it (totals checked separately).
#[derive(Debug, PartialEq, Eq)]
struct Expect {
    name: Vec<u8>,
    parent: i32,
    depth: u16,
    kind: u8,
    flags: u32,
    size: i64,
    alloc: i64,
    times: [u32; 3],
    ino: u64,
    nlink: u16,
    ext: String,
    child_count: i32,
}

fn raw_ext(name: &[u8]) -> Option<&[u8]> {
    let dot = name.iter().rposition(|&b| b == b'.')?;
    let ext = &name[dot + 1..];
    if dot == 0 || ext.is_empty() || ext.len() > 64 {
        return None;
    }
    let chars = ext.iter().filter(|&&b| b & 0xC0 != 0x80).count();
    (chars <= 16).then_some(ext)
}

/// The rules of docs/native.md 2.1, one row at a time: node 0, then the root and the rest in
/// breadth-first order (children in row order).
fn reference(t: &MftTable, root: &str, opts: &MftBuild) -> Vec<Expect> {
    let n = t.rows();
    let (meta, orphan, rootr) = (n, n + 1, n + 2);
    let total = n + 3;
    let mut first: HashMap<u64, usize> = HashMap::new();
    for (i, &r) in t.record.iter().enumerate() {
        first.entry(r).or_insert(i);
    }
    let dup: Vec<bool> = (0..n).map(|i| first[&t.record[i]] != i).collect();
    let container: Vec<bool> = (0..total)
        .map(|i| i >= n || (t.kind[i] == Kind::Dir as u8 && !dup[i]))
        .collect();
    let dir_of = |rec: u64| -> Option<usize> {
        if rec == ROOT_RECORD {
            return Some(rootr);
        }
        first.get(&rec).copied().filter(|&f| container[f])
    };
    let mut pdir: Vec<i64> = vec![-1; total];
    for i in 0..n {
        let r = t.record[i];
        pdir[i] = if r == ROOT_RECORD {
            -2
        } else if r < FIRST_USER_RECORD {
            meta as i64
        } else {
            match dir_of(t.parent[i]) {
                Some(d) if !(container[i] && d == i) => d as i64,
                _ => orphan as i64,
            }
        };
    }
    for row in [meta, orphan] {
        if pdir[..n].contains(&(row as i64)) {
            pdir[row] = rootr as i64;
        }
    }
    // reachable from the root?
    let bfs = |pdir: &[i64]| -> Vec<usize> {
        let mut kids: Vec<Vec<usize>> = vec![Vec::new(); total];
        for (i, &p) in pdir.iter().enumerate() {
            if p >= 0 {
                kids[p as usize].push(i);
            }
        }
        let mut order = vec![rootr];
        let mut queue = VecDeque::from([rootr]);
        while let Some(f) = queue.pop_front() {
            for &k in &kids[f] {
                order.push(k);
                if container[k] {
                    queue.push_back(k);
                }
            }
        }
        order
    };
    let mut order = bfs(&pdir);
    let mut seen = vec![false; total];
    for &r in &order {
        seen[r] = true;
    }
    // every cycle not reachable from the root: its smallest row breaks out (every row of every
    // cycle when one is longer than 4096)
    let mut broken = Vec::new();
    let mut members = Vec::new();
    let mut longest = 0usize;
    let mut done = vec![false; total];
    for start in 0..total {
        if seen[start] || pdir[start] < 0 || done[start] {
            continue;
        }
        let mut path = Vec::new();
        let mut on_path: HashMap<usize, usize> = HashMap::new();
        let mut v = start;
        loop {
            if let Some(&at) = on_path.get(&v) {
                let cycle = &path[at..];
                longest = longest.max(cycle.len());
                members.extend_from_slice(cycle);
                broken.push(*cycle.iter().min().unwrap());
                break;
            }
            if done[v] || seen[v] || pdir[v] < 0 {
                break;
            }
            on_path.insert(v, path.len());
            path.push(v);
            v = pdir[v] as usize;
        }
        for p in path {
            done[p] = true;
        }
    }
    if longest > CYCLE_LIMIT {
        broken = members;
    }
    if !broken.is_empty() {
        for b in broken {
            pdir[b] = orphan as i64;
        }
        pdir[orphan] = rootr as i64;
        order = bfs(&pdir);
    }
    assert_eq!(order.len(), 1 + pdir.iter().filter(|&&p| p >= 0).count());
    let mut node_of = vec![0i32; total];
    for (k, &r) in order.iter().enumerate() {
        node_of[r] = k as i32 + 1;
    }
    let mut depth = vec![0u16; total];
    depth[rootr] = 1;
    for &r in &order[1..] {
        depth[r] = depth[pdir[r] as usize].wrapping_add(1);
    }
    let mut kids = vec![0i32; total];
    for &p in &pdir {
        if p >= 0 {
            kids[p as usize] += 1;
        }
    }
    let mut in_meta = vec![false; total];
    in_meta[meta] = true;
    for &r in &order[1..] {
        if in_meta[pdir[r] as usize] {
            in_meta[r] = true;
        }
    }
    let root_first = (0..n).find(|&i| t.record[i] == ROOT_RECORD);
    let clip = |v: i64| v.clamp(0, u32::MAX as i64) as u32;
    let mut out = vec![Expect {
        name: Vec::new(),
        parent: -1,
        depth: 0,
        kind: Kind::Dir as u8,
        flags: Flags::VIRTUAL.0,
        size: 0,
        alloc: 0,
        times: [0; 3],
        ino: 0,
        nlink: 1,
        ext: String::new(),
        child_count: 1,
    }];
    for &r in &order {
        let parent = if pdir[r] < 0 {
            0
        } else {
            node_of[pdir[r] as usize]
        };
        let child_count = if container[r] { kids[r] } else { 0 };
        if r >= n {
            let name = match r {
                x if x == rootr => root.as_bytes().to_vec(),
                x if x == meta => META_FOLDER.as_bytes().to_vec(),
                _ => ORPHAN_FOLDER.as_bytes().to_vec(),
            };
            let times = match (r == rootr, root_first) {
                (true, Some(i)) => [clip(t.mtime[i]), clip(t.ctime[i]), clip(t.atime[i])],
                _ => [0; 3],
            };
            out.push(Expect {
                name,
                parent,
                depth: depth[r],
                kind: Kind::Dir as u8,
                flags: if r == rootr { 0 } else { Flags::VIRTUAL.0 },
                size: 0,
                alloc: 0,
                times,
                ino: if r == rootr { ROOT_RECORD } else { 0 },
                nlink: 1,
                ext: String::new(),
                child_count,
            });
            continue;
        }
        let rec = t.record[r];
        let kind = t.kind[r];
        let ads_ok = rec != BADCLUS_RECORD;
        let mut size = if kind == Kind::Dir as u8 {
            0
        } else {
            t.size[r]
        };
        if opts.include_ads && ads_ok {
            size = size.wrapping_add(t.ads_size[r]);
        }
        let mut alloc = if kind == Kind::File as u8 {
            t.alloc[r]
        } else {
            0
        };
        if (opts.include_ads || in_meta[r]) && ads_ok {
            alloc = alloc.wrapping_add(t.ads_alloc[r]);
        }
        let mut flags = t.flags[r];
        if dup[r] && !opts.count_links_each {
            flags |= Flags::HARDLINK_DUP.0;
        }
        let ext = if kind == Kind::File as u8 {
            raw_ext(t.name(r))
                .map(|e| String::from_utf8_lossy(&sk_native::hardscan::lower_ext(e)).into_owned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        out.push(Expect {
            name: t.name(r).to_vec(),
            parent,
            depth: depth[r],
            kind,
            flags,
            size,
            alloc,
            times: [clip(t.mtime[r]), clip(t.ctime[r]), clip(t.atime[r])],
            ino: rec,
            nlink: t.nlink[r],
            ext,
            child_count,
        });
    }
    out
}

/// What the store holds, as [`Expect`]s.
fn actual(s: &Nodes) -> Vec<Expect> {
    (0..s.len() as u32)
        .map(|i| Expect {
            name: s.name_bytes(i).to_vec(),
            parent: s.parent(i),
            depth: s.depth(i),
            kind: s.kind_raw(i),
            flags: s.flags_raw(i),
            size: s.size(i),
            alloc: s.alloc(i),
            times: [s.mtime(i), s.ctime(i), s.atime(i)],
            ino: s.ino(i),
            nlink: s.nlink(i),
            ext: s.ext_name(s.ext(i)).to_string(),
            child_count: s.child_count(i),
        })
        .collect()
}

/// Subtree totals of nodes in breadth-first order (`reaggregate`, wrapping like the build).
fn totals_of(nodes: &[Expect]) -> Vec<(i64, i64, i32, i32)> {
    let mut t = vec![(0i64, 0i64, 0i32, 0i32); nodes.len()];
    for c in (1..nodes.len()).rev() {
        let e = &nodes[c];
        let counted = e.flags & Flags::HARDLINK_DUP.0 == 0;
        let (s, a, f, d) = t[c];
        let p = &mut t[e.parent as usize];
        p.0 =
            p.0.wrapping_add(s.wrapping_add(if counted { e.size } else { 0 }));
        p.1 =
            p.1.wrapping_add(a.wrapping_add(if counted { e.alloc } else { 0 }));
        p.2 = p.2.wrapping_add(f + (e.kind == Kind::File as u8) as i32);
        p.3 = p.3.wrapping_add(d + (e.kind == Kind::Dir as u8) as i32);
    }
    t
}

/// The build of a copy of `t` equals the reference, children are contiguous in order, the
/// extension table is numbered in sorted raw order, and the totals are `reaggregate`'s.
fn check(t: &MftTable, root: &str, opts: &MftBuild) -> Store {
    let want = reference(t, root, opts);
    let store = table_to_store(t.clone(), root, opts).unwrap();
    {
        let s = store.read();
        // the invariants' own sums do not wrap: only for tables of plausible sizes
        let huge = |v: &[i64]| v.iter().any(|x| x.unsigned_abs() > 1 << 40);
        if ![&t.size, &t.alloc, &t.ads_size, &t.ads_alloc]
            .iter()
            .any(|c| huge(c))
        {
            check_invariants(&s).unwrap();
        }
        let got = actual(&s);
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g, w, "node {i}");
        }
        // children of each folder: the next ids in breadth-first order
        let mut next = 2u32;
        for id in 1..s.len() as u32 {
            if s.is_dir(id) || id == 1 {
                let (a, b) = s.child_range(id);
                if b > a {
                    assert_eq!(a, next, "children of {id}");
                    next = b;
                }
            }
        }
    }
    let s = store.read();
    let built: Vec<(i64, i64, i32, i32)> = (0..s.len() as u32)
        .map(|i| (s.tot_size(i), s.tot_alloc(i), s.tot_files(i), s.tot_dirs(i)))
        .collect();
    assert!(built == totals_of(&want), "totals");
    // the extension table: "" first, every entry distinct
    let table: Vec<String> = s.ext_table().iter().map(|e| e.to_string()).collect();
    assert_eq!(table.first().map(String::as_str), Some(""));
    let mut uniq = table.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), table.len(), "extensions are distinct");
    drop(s);
    store
}

fn sample() -> MftTable {
    parse_mft(&im::build_mft(&im::sample_records(), RS, CS, 0), RS, CS).unwrap()
}

fn opts(include_ads: bool, count_links_each: bool) -> MftBuild {
    MftBuild {
        include_ads,
        count_links_each,
        folder_names: Some((META_FOLDER.into(), ORPHAN_FOLDER.into())),
    }
}

fn ring(n: usize) -> MftTable {
    let record: Vec<u64> = (100..100 + n as u64).collect();
    let mut parent = record.clone();
    parent.rotate_right(1);
    MftTable {
        record,
        parent,
        name_off: (0..n as u32).collect(),
        name_len: vec![1; n],
        kind: vec![1; n],
        flags: vec![0; n],
        size: vec![0; n],
        alloc: vec![0; n],
        ads_size: vec![0; n],
        ads_alloc: vec![0; n],
        mtime: vec![0; n],
        ctime: vec![0; n],
        atime: vec![0; n],
        nlink: vec![0; n],
        names: vec![b'a'; n],
        bad_records: 0,
    }
}

fn rec(n: u64, parent: u64, name: &str, dir: bool, size: i64) -> im::Rec {
    let mut r = im::rec(n, vec![im::nm(parent, name, im::WIN32)]);
    if dir {
        r.dir = true;
        r.attrs = 0x10;
    } else {
        r.streams = vec![im::stream(size)];
    }
    r
}

// --------------------------------------------------------------------------- tests

#[test]
fn sample_image() {
    let t = sample();
    for (ads, each) in [(false, false), (true, false), (false, true), (true, true)] {
        check(&t, "C:\\", &opts(ads, each));
    }
}

#[test]
fn cycles_orphans_and_odd_parents() {
    let mut recs = im::metafile_records();
    recs.extend([
        rec(70, 72, "c1", true, 0),
        rec(71, 70, "c2", true, 0),
        rec(72, 71, "c3", true, 0),
        rec(73, 71, "below.txt", false, 5000),
        rec(74, 74, "self", true, 0),
        rec(75, 75, "selfkid", false, 1),
        rec(76, 66, "under-a-file", false, 0),
        rec(66, im::ROOT, "file", false, 1),
        rec(77, 1 << 40, "far-parent", false, 0),
    ]);
    let t = parse_mft(&im::build_mft(&recs, RS, CS, 0), RS, CS).unwrap();
    check(&t, "D:\\", &opts(false, false));
}

#[test]
fn minimal_tables() {
    check(
        &parse_mft(&[], RS, CS).unwrap(),
        "E:\\",
        &opts(false, false),
    );
    let recs = vec![rec(64, im::ROOT, "a", true, 0), rec(65, 64, "b", false, 1)];
    let t = parse_mft(&im::build_mft(&recs, RS, CS, 0), RS, CS).unwrap();
    check(&t, "F:\\", &opts(false, false));
}

#[test]
fn synthetic_volume() {
    let t = parse_mft(&im::synthetic_mft(120_000, 21, RS), RS, CS).unwrap();
    for ads in [false, true] {
        check(&t, "C:\\", &opts(ads, false));
    }
}

#[test]
fn extensions_in_any_mix() {
    let wide = "\u{10000}".repeat(5);
    let many_e = "é".repeat(17);
    let exts = [
        "TXT",
        "zip",
        "Abcdefghij",
        "txt",
        "longerext",
        "",
        "7z",
        &wide,
        &many_e,
        "ÉÉÉ",
        "ĲX",
    ];
    let mut names: Vec<String> = (0..exts.len() * 5)
        .map(|i| format!("f{i}.{}", exts[i % exts.len()]))
        .collect();
    names.extend(
        [
            ".hidden",
            "trailing.",
            "a.b.C",
            "x.SixteenCharsExtX",
            "y.SeventeenCharsExt",
        ]
        .map(String::from),
    );
    let recs: Vec<im::Rec> = names
        .iter()
        .enumerate()
        .map(|(i, n)| rec(64 + i as u64, im::ROOT, n, false, 1))
        .collect();
    let t = parse_mft(&im::build_mft(&recs, RS, CS, 0), RS, CS).unwrap();
    let store = check(&t, "C:\\", &opts(false, false));
    let table: Vec<String> = store
        .read()
        .ext_table()
        .iter()
        .map(|e| e.to_string())
        .collect();
    // numbered in sorted raw order: short ones (up to 7 bytes) as little-endian integers with
    // the length on top ("C" < "7z" < "TXT" < "ĲX" < "zip" < "txt"), then the long ones as bytes;
    // "TXT" and "txt" share one entry
    assert_eq!(table[..6], ["", "c", "7z", "txt", "ĳx", "zip"]);
    assert!(table.contains(&"sixteencharsextx".to_string()));
    assert!(!table.iter().any(|e| e.contains("seventeen")));
}

#[test]
fn a_cycle_longer_than_4096_breaks_every_member() {
    // 5000 folders in one parent cycle: every member breaks out under the orphans
    let store = check(&ring(5000), "C:\\", &opts(false, false));
    let s = store.read();
    let orphans = s
        .children(1)
        .find(|&c| s.name_bytes(c) == ORPHAN_FOLDER.as_bytes())
        .unwrap();
    assert_eq!(s.child_count(orphans), 5000);
    // a cycle of 4096: its smallest row only
    let store = check(&ring(4096), "C:\\", &opts(false, false));
    let s = store.read();
    let orphans = s
        .children(1)
        .find(|&c| s.name_bytes(c) == ORPHAN_FOLDER.as_bytes())
        .unwrap();
    assert_eq!(s.child_count(orphans), 1);
}

#[test]
fn mapped_store() {
    let t = sample();
    let dir = tempfile::tempdir().unwrap();
    let policy = sk_store::Policy::new();
    policy.set_force_bytes(Some(1)); // every store goes to file-backed memory
    policy.set_columns_dir(dir.path().to_path_buf());
    let store_opts = sk_store::StoreOptions {
        policy: std::sync::Arc::new(policy),
        ..Default::default()
    };
    let mapped =
        sk_core::scan::mft::table_to_store_with(t.clone(), "C:\\", &opts(false, false), store_opts)
            .unwrap();
    assert!(mapped.read().columns_mapped());
    let plain = check(&t, "C:\\", &opts(false, false));
    assert!(actual(&mapped.read()) == actual(&plain.read()));
}

#[test]
fn bad_tables_are_refused() {
    let mut bad = sample();
    bad.name_off[3] = bad.names.len() as u32 + 10;
    let e = table_to_store(bad, "C:\\", &opts(false, false)).unwrap_err();
    assert!(e.to_string().contains("name offsets"), "{e}");
    let mut short = sample();
    short.kind.pop();
    let e = table_to_store(short, "C:\\", &opts(false, false)).unwrap_err();
    assert!(e.to_string().contains("rows"), "{e}");
}

#[test]
fn random_tables() {
    let mut rng = Rng(0x5eed);
    for case in 0..80 {
        let n = rng.below(401) as usize;
        let t = random_table(&mut rng, n, None);
        check(&t, "C:\\", &opts(case % 2 == 1, case % 4 >= 2));
    }
}

#[test]
fn random_names() {
    let mut rng = Rng(77);
    let pieces: Vec<Vec<u8>> = [
        "a",
        "b",
        "c",
        "X",
        "Y",
        "Z",
        ".",
        "é",
        "文",
        "\u{1F600}",
        "-",
        "Ĳ",
        "İ",
        "Σ",
    ]
    .iter()
    .map(|p| p.as_bytes().to_vec())
    .chain([vec![0xED, 0xA0, 0x80], vec![0xED, 0xBF, 0xBF]]) // lone surrogates (WTF-8)
    .collect();
    for _ in 0..25 {
        let n = 1 + rng.below(80) as usize;
        let names: Vec<Vec<u8>> = (0..n)
            .map(|_| {
                (0..rng.below(13))
                    .flat_map(|_| pieces[rng.below(pieces.len() as u64) as usize].clone())
                    .collect()
            })
            .collect();
        let mut t = random_table(&mut rng, n, Some(names));
        t.kind = vec![0; n]; // files: every name gets an extension id
        t.parent = vec![5; n];
        check(&t, "C:\\", &opts(false, false));
    }
}

#[test]
fn large_random_table() {
    let mut rng = Rng(99);
    let t = random_table(&mut rng, 60_000, None);
    check(&t, "C:\\", &opts(true, false));
}

/// Build speed on a large synthetic table (`cargo test --release -p sk-core --test
/// scan_mft_build -- --ignored --nocapture build_speed`; `SK_MFT_ROWS` sets the rows, default
/// 3 million).
#[test]
#[ignore = "benchmark"]
fn build_speed() {
    let n: usize = std::env::var("SK_MFT_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3_000_000);
    let t = synthetic_tree(n, 42);
    for _ in 0..5 {
        let table = t.clone();
        let t0 = std::time::Instant::now();
        let store = table_to_store(table, "C:\\", &opts(false, false)).unwrap();
        let took = t0.elapsed();
        eprintln!(
            "build of {n} rows: {:.3} s ({} nodes)",
            took.as_secs_f64(),
            store.read().len()
        );
    }
}

/// A volume-like table: a tree of folders (about one in eight rows), files with extensions, a
/// few hard links, rows in record order.
fn synthetic_tree(n: usize, seed: u64) -> MftTable {
    let mut rng = Rng(seed);
    let exts = [
        "txt", "dll", "exe", "JPG", "png", "cache", "json", "sys", "", "dat",
    ];
    let mut t = MftTable::default();
    let mut dirs: Vec<u64> = vec![ROOT_RECORD];
    let mut rec = 16u64;
    while t.rows() < n {
        let is_dir = rng.below(8) == 0;
        let parent = dirs[rng.below(dirs.len() as u64) as usize];
        let ext = exts[rng.below(exts.len() as u64) as usize];
        let name = match (is_dir, ext) {
            (true, _) => format!("d{rec}"),
            (false, "") => format!("f{rec}"),
            _ => format!("f{rec}.{ext}"),
        };
        let links = if !is_dir && rng.below(50) == 0 { 2 } else { 1 };
        for _ in 0..links {
            t.record.push(rec);
            t.parent.push(parent);
            t.name_off.push(t.names.len() as u32);
            t.name_len.push(name.len() as u16);
            t.names.extend_from_slice(name.as_bytes());
            t.kind.push(is_dir as u8);
            t.flags.push(0);
            let size = if is_dir { 0 } else { rng.below(1 << 20) as i64 };
            t.size.push(size);
            t.alloc.push((size + 4095) / 4096 * 4096);
            t.ads_size.push(0);
            t.ads_alloc.push(0);
            t.mtime.push(1_700_000_000);
            t.ctime.push(1_600_000_000);
            t.atime.push(1_700_000_000);
            t.nlink.push(links);
        }
        if is_dir {
            dirs.push(rec);
        }
        rec += 1;
    }
    t
}
