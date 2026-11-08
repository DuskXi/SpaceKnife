//! The `$MFT` tree build (`sk_native::mft::build_tree`) into plain vectors: on parsed images and
//! random tables, the columns form the tree the store needs — node 0, the root, every other node
//! after its parent in breadth-first order inside its parent's child range, metafiles under the
//! metafile folder, every row of the table exactly once (the root's own entries excepted), the
//! `\0name` image in node order, extensions numbered from a table of distinct lower-cased ones,
//! totals that add up. sk-core's `scan_mft_build` compares the whole store with a serial
//! reference build, column by column.

mod mft_image;

use std::collections::HashMap;

use mft_image as im;
use sk_native::columns::{NodeColumns, flag, kind};
use sk_native::mft::build::{CYCLE_LIMIT, FIRST_USER_RECORD, ROOT_RECORD};
use sk_native::mft::{
    BuildError, BuildReport, MftTable, PseudoNames, TreeOptions, TreeSink, build_tree, parse_mft,
};

const RS: usize = 1024;
const CS: usize = 4096;
const NAMES: PseudoNames<'static> = PseudoNames {
    root: b"C:\\",
    meta: b"$META",
    orphans: b"$ORPHANS",
};

/// The store's columns as vectors.
#[derive(Default)]
struct Cols {
    parent: Vec<i32>,
    depth: Vec<u16>,
    kind: Vec<u8>,
    flags: Vec<u32>,
    name_len: Vec<u16>,
    ext: Vec<u16>,
    size: Vec<i64>,
    alloc: Vec<i64>,
    mtime: Vec<u32>,
    ctime: Vec<u32>,
    atime: Vec<u32>,
    ino: Vec<u64>,
    nlink: Vec<u16>,
    child_start: Vec<i32>,
    child_count: Vec<i32>,
    tot_size: Vec<i64>,
    tot_alloc: Vec<i64>,
    tot_files: Vec<i32>,
    tot_dirs: Vec<i32>,
    image: Vec<u8>,
    ext_table: Vec<String>,
    asked: usize,
}

impl TreeSink for Cols {
    type Error = String;

    fn columns(
        &mut self,
        nodes: usize,
        image_len: usize,
        ext_table: Vec<String>,
    ) -> Result<(NodeColumns<'_>, &mut [u8]), String> {
        self.asked += 1;
        self.parent = vec![0; nodes];
        self.depth = vec![0; nodes];
        self.kind = vec![0; nodes];
        self.flags = vec![0; nodes];
        self.name_len = vec![0; nodes];
        self.ext = vec![0; nodes];
        self.size = vec![0; nodes];
        self.alloc = vec![0; nodes];
        self.mtime = vec![0; nodes];
        self.ctime = vec![0; nodes];
        self.atime = vec![0; nodes];
        self.ino = vec![0; nodes];
        self.nlink = vec![0; nodes];
        self.child_start = vec![0; nodes];
        self.child_count = vec![0; nodes];
        self.tot_size = vec![0; nodes];
        self.tot_alloc = vec![0; nodes];
        self.tot_files = vec![0; nodes];
        self.tot_dirs = vec![0; nodes];
        self.image = vec![0; image_len];
        self.ext_table = ext_table;
        Ok((
            NodeColumns {
                parent: &mut self.parent,
                depth: &mut self.depth,
                kind: &mut self.kind,
                flags: &mut self.flags,
                name_len: &mut self.name_len,
                ext: &mut self.ext,
                size: &mut self.size,
                alloc: &mut self.alloc,
                mtime: &mut self.mtime,
                ctime: &mut self.ctime,
                atime: &mut self.atime,
                ino: &mut self.ino,
                nlink: &mut self.nlink,
                child_start: &mut self.child_start,
                child_count: &mut self.child_count,
                tot_size: &mut self.tot_size,
                tot_alloc: &mut self.tot_alloc,
                tot_files: &mut self.tot_files,
                tot_dirs: &mut self.tot_dirs,
            },
            &mut self.image,
        ))
    }
}

impl Cols {
    fn len(&self) -> usize {
        self.parent.len()
    }

    /// Every node's name, read from the image (which must be `\0name` per node, in order).
    fn names(&self) -> Vec<&[u8]> {
        let mut out = Vec::with_capacity(self.len());
        let mut at = 0usize;
        for i in 0..self.len() {
            assert_eq!(self.image[at], 0, "the NUL before node {i}'s name");
            let l = self.name_len[i] as usize;
            out.push(&self.image[at + 1..at + 1 + l]);
            at += l + 1;
        }
        assert_eq!(at, self.image.len(), "the names fill the image");
        out
    }
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

/// Builds a copy of `t` and checks what the build makes; returns the columns.
fn check(t: &MftTable, opts: TreeOptions) -> (Cols, BuildReport) {
    let mut c = Cols::default();
    let report = build_tree(t.clone(), &NAMES, &opts, &mut c).unwrap();
    assert_eq!(c.asked, 1, "the sink is asked once");
    let n = c.len();
    assert_eq!((report.names, report.nodes), (t.rows(), n));
    let names = c.names();
    // node 0 and the root
    assert_eq!(
        (c.parent[0], c.depth[0], c.kind[0], c.flags[0], c.nlink[0]),
        (-1, 0, kind::DIR, flag::VIRTUAL, 1)
    );
    assert_eq!((c.child_start[0], c.child_count[0]), (1, 1));
    assert_eq!(names[1], NAMES.root);
    assert_eq!(
        (c.parent[1], c.depth[1], c.kind[1], c.flags[1], c.ino[1]),
        (0, 1, kind::DIR, 0, ROOT_RECORD)
    );
    // every other node: after its parent, a level below it, inside its child range
    let mut kids = 0i64;
    for i in 1..n {
        if c.child_count[i] > 0 {
            assert_eq!(c.kind[i], kind::DIR, "children of node {i}");
            kids += c.child_count[i] as i64;
        }
        if i == 1 {
            continue;
        }
        let p = c.parent[i];
        assert!(p >= 1 && (p as usize) < i, "parent of {i}: {p}");
        let p = p as usize;
        assert_eq!(c.depth[i], c.depth[p].wrapping_add(1), "depth of {i}");
        let (s, m) = (c.child_start[p] as usize, c.child_count[p] as usize);
        assert!(s <= i && i < s + m, "node {i} in the range of {p}");
    }
    assert_eq!(
        kids as usize,
        n - 2,
        "every node but node 0 and the root is a child"
    );
    // the pseudo folders, when they are used
    let pseudo: Vec<usize> = (2..n)
        .filter(|&i| c.flags[i] == flag::VIRTUAL && c.parent[i] == 1 && c.ino[i] == 0)
        .collect();
    for &i in &pseudo {
        assert!(names[i] == NAMES.meta || names[i] == NAMES.orphans);
        assert!(
            c.child_count[i] > 0,
            "a pseudo folder only when it holds something"
        );
    }
    let meta = pseudo.iter().copied().find(|&i| names[i] == NAMES.meta);
    let orphans = pseudo.iter().copied().find(|&i| names[i] == NAMES.orphans);
    assert_eq!(
        report.orphans,
        orphans.map_or(0, |o| c.child_count[o] as usize)
    );
    // every row of the table once (the root's own entries are the root), by record and name
    let mut want: HashMap<(u64, Vec<u8>), i64> = HashMap::new();
    for r in 0..t.rows() {
        if t.record[r] != ROOT_RECORD {
            *want.entry((t.record[r], t.name(r).to_vec())).or_default() += 1;
        }
    }
    let mut got: HashMap<(u64, Vec<u8>), i64> = HashMap::new();
    for (i, name) in names.iter().enumerate().skip(2) {
        if !pseudo.contains(&i) {
            *got.entry((c.ino[i], name.to_vec())).or_default() += 1;
        }
    }
    assert!(got == want, "every row once");
    // metafiles under the metafile folder
    for i in 2..n {
        let rec = c.ino[i];
        if !pseudo.contains(&i) && rec < FIRST_USER_RECORD {
            assert_eq!(Some(c.parent[i] as usize), meta, "metafile {i}");
        }
    }
    // extensions: a table of distinct lower-cased extensions, "" first, files only
    assert_eq!(c.ext_table.first().map(String::as_str), Some(""));
    let mut uniq = c.ext_table.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), c.ext_table.len());
    for (i, name) in names.iter().enumerate() {
        let e = c.ext[i] as usize;
        assert!(e < c.ext_table.len());
        match raw_ext(name) {
            Some(raw) if c.kind[i] == kind::FILE && i > 1 && !pseudo.contains(&i) => {
                let lower =
                    String::from_utf8_lossy(&sk_native::hardscan::lower_ext(raw)).into_owned();
                assert_eq!(c.ext_table[e], lower, "extension of {i}");
            }
            _ => assert_eq!(e, 0, "no extension for node {i}"),
        }
    }
    // further names of a file (where the table itself sets no such bit)
    if t.flags.iter().all(|f| f & flag::HARDLINK_DUP == 0) {
        let mut first: HashMap<u64, usize> = HashMap::new();
        let dups = (0..t.rows())
            .filter(|&r| *first.entry(t.record[r]).or_insert(r) != r && t.record[r] != ROOT_RECORD)
            .count();
        let flagged = (0..n)
            .filter(|&i| c.flags[i] & flag::HARDLINK_DUP != 0)
            .count();
        let want = if opts.count_links_each { 0 } else { dups };
        assert_eq!(flagged, want, "HARDLINK_DUP");
    } // totals: own bytes (but further links') and the subtrees', files and folders
    let mut tot = vec![(0i64, 0i64, 0i32, 0i32); n];
    for i in (1..n).rev() {
        let counted = c.flags[i] & flag::HARDLINK_DUP == 0;
        let (s, a, f, d) = tot[i];
        let p = &mut tot[c.parent[i] as usize];
        p.0 =
            p.0.wrapping_add(s.wrapping_add(if counted { c.size[i] } else { 0 }));
        p.1 =
            p.1.wrapping_add(a.wrapping_add(if counted { c.alloc[i] } else { 0 }));
        p.2 = p.2.wrapping_add(f + (c.kind[i] == kind::FILE) as i32);
        p.3 = p.3.wrapping_add(d + (c.kind[i] == kind::DIR) as i32);
    }
    for (i, &(s, a, f, d)) in tot.iter().enumerate() {
        assert_eq!(
            (c.tot_size[i], c.tot_alloc[i], c.tot_files[i], c.tot_dirs[i]),
            (s, a, f, d),
            "totals of {i}"
        );
    }
    (c, report)
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

fn parsed(recs: &[im::Rec]) -> MftTable {
    parse_mft(&im::build_mft(recs, RS, CS, 0), RS, CS).unwrap()
}

/// `n` folders, each the parent of the next, the last the parent of the first.
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
        nlink: vec![1; n],
        names: vec![b'a'; n],
        bad_records: 0,
    }
}

#[test]
fn sample_image() {
    let t = parsed(&im::sample_records());
    for (ads, each) in [(false, false), (true, false), (false, true), (true, true)] {
        let (c, _) = check(
            &t,
            TreeOptions {
                include_ads: ads,
                count_links_each: each,
            },
        );
        assert!(c.names().contains(&NAMES.meta), "metafiles");
    }
}

#[test]
fn cycles_and_orphans() {
    let mut recs = im::metafile_records();
    recs.extend([
        rec(70, 72, "c1", true, 0),
        rec(71, 70, "c2", true, 0),
        rec(72, 71, "c3", true, 0),
        rec(73, 71, "below.txt", false, 5000),
        rec(74, 74, "self", true, 0),
        rec(76, 66, "under-a-file", false, 0),
        rec(66, im::ROOT, "file", false, 1),
        rec(77, 1 << 40, "far-parent", false, 0),
    ]);
    let (c, report) = check(&parsed(&recs), TreeOptions::default());
    assert_eq!(report.cycles, 1, "the ring c1 -> c3 -> c2 breaks once");
    // the ring's smallest row, the folder in itself, the child of a file, the far parent
    assert_eq!(report.orphans, 4);
    let names = c.names();
    let o = (0..c.len()).find(|&i| names[i] == NAMES.orphans).unwrap();
    let under: Vec<&[u8]> = (0..c.len())
        .filter(|&i| c.parent[i] == o as i32)
        .map(|i| names[i])
        .collect();
    for n in [&b"self"[..], b"under-a-file", b"far-parent"] {
        assert!(
            under.contains(&n),
            "{} under the orphans",
            String::from_utf8_lossy(n)
        );
    }
}

#[test]
fn a_cycle_longer_than_the_limit_breaks_every_member() {
    let (_, r) = check(&ring(CYCLE_LIMIT + 1), TreeOptions::default());
    assert_eq!((r.cycles, r.orphans), (CYCLE_LIMIT + 1, CYCLE_LIMIT + 1));
    let (_, r) = check(&ring(CYCLE_LIMIT), TreeOptions::default());
    assert_eq!((r.cycles, r.orphans), (1, 1));
}

#[test]
fn empty_and_synthetic_tables() {
    let (c, r) = check(&parse_mft(&[], RS, CS).unwrap(), TreeOptions::default());
    assert_eq!((c.len(), r.orphans), (2, 0), "node 0 and the root only");
    let t = parse_mft(&im::synthetic_mft(20_000, 5, RS), RS, CS).unwrap();
    check(&t, TreeOptions::default());
    check(
        &t,
        TreeOptions {
            include_ads: true,
            count_links_each: false,
        },
    );
}

#[test]
fn random_tables() {
    let mut seed = 0x5eed_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for case in 0..60 {
        let n = (next() % 300) as usize;
        let top = (n / 2 + 20) as u64;
        let mut t = MftTable::default();
        for i in 0..n {
            let name: Vec<u8> = (0..next() % 7)
                .map(|_| b"a.Bx"[(next() % 4) as usize])
                .collect();
            t.record.push(next() % top);
            t.parent.push(if next() % 10 == 0 {
                next() >> 20
            } else {
                next() % top
            });
            t.name_off.push(t.names.len() as u32);
            t.name_len.push(name.len() as u16);
            t.names.extend_from_slice(&name);
            t.kind
                .push([kind::FILE, kind::DIR, kind::DIR, kind::LINK][(next() % 4) as usize]);
            t.flags.push((next() % (1 << 19)) as u32);
            for v in [&mut t.size, &mut t.alloc, &mut t.ads_size, &mut t.ads_alloc] {
                v.push((next() % (1 << 40)) as i64 - (1 << 39));
            }
            for v in [&mut t.mtime, &mut t.ctime, &mut t.atime] {
                v.push((next() % (1 << 34)) as i64 - 5);
            }
            t.nlink.push((i % 3) as u16 + 1);
        }
        check(
            &t,
            TreeOptions {
                include_ads: case % 2 == 1,
                count_links_each: case % 4 >= 2,
            },
        );
    }
}

#[test]
fn bad_tables_and_refusing_sinks() {
    let t = parsed(&im::sample_records());
    let mut bad = t.clone();
    bad.name_off[3] = bad.names.len() as u32 + 10;
    let mut c = Cols::default();
    assert_eq!(
        build_tree(bad, &NAMES, &TreeOptions::default(), &mut c).unwrap_err(),
        BuildError::Table("name offsets out of range")
    );
    let mut short = t.clone();
    short.kind.pop();
    assert_eq!(
        build_tree(short, &NAMES, &TreeOptions::default(), &mut c).unwrap_err(),
        BuildError::Table("the table's columns differ in rows")
    );
    let long = vec![b'x'; 70_000];
    let names = PseudoNames {
        orphans: &long,
        ..NAMES
    };
    assert_eq!(
        build_tree(t.clone(), &names, &TreeOptions::default(), &mut c).unwrap_err(),
        BuildError::NameTooLong(70_000)
    );
    assert_eq!(c.asked, 0, "nothing asked of the sink for a bad table");

    struct Full;
    impl TreeSink for Full {
        type Error = &'static str;
        fn columns(
            &mut self,
            _: usize,
            _: usize,
            _: Vec<String>,
        ) -> Result<(NodeColumns<'_>, &mut [u8]), &'static str> {
            Err("no room")
        }
    }
    assert_eq!(
        build_tree(t, &NAMES, &TreeOptions::default(), &mut Full).unwrap_err(),
        BuildError::Sink("no room")
    );
}

#[test]
#[should_panic(expected = "shape")]
fn columns_of_another_shape_are_refused() {
    struct Short(Vec<i32>, Vec<u8>);
    impl TreeSink for Short {
        type Error = ();
        fn columns(
            &mut self,
            nodes: usize,
            image_len: usize,
            _: Vec<String>,
        ) -> Result<(NodeColumns<'_>, &mut [u8]), ()> {
            self.0 = vec![0; nodes - 1];
            self.1 = vec![0; image_len];
            Ok((
                NodeColumns {
                    parent: &mut self.0,
                    ..NodeColumns::default()
                },
                &mut self.1,
            ))
        }
    }
    let _ = build_tree(
        parsed(&im::sample_records()),
        &NAMES,
        &TreeOptions::default(),
        &mut Short(Vec::new(), Vec::new()),
    );
}

/// The build runs on a pool of its own: its threads are named for it, and go with it.
#[test]
fn the_build_runs_on_a_pool_of_its_own() {
    struct Spy(Option<String>, Cols);
    impl TreeSink for Spy {
        type Error = String;
        fn columns(
            &mut self,
            nodes: usize,
            image_len: usize,
            ext: Vec<String>,
        ) -> Result<(NodeColumns<'_>, &mut [u8]), String> {
            self.0 = std::thread::current().name().map(str::to_owned);
            self.1.columns(nodes, image_len, ext)
        }
    }
    let mut spy = Spy(None, Cols::default());
    build_tree(
        parsed(&im::sample_records()),
        &NAMES,
        &TreeOptions::default(),
        &mut spy,
    )
    .unwrap();
    let name = spy.0.expect("a named thread");
    assert!(name.starts_with("sk-mft-build-"), "{name}");
}

/// An extension's text in the table is the sink's ([`TreeSink::ext_text`]): by default U+FFFD
/// for what is not valid Unicode, so two such extensions share one id; a sink that escapes gets
/// its own text, and an id per distinct extension.
#[test]
fn extensions_take_the_sinks_text() {
    struct Escaping(Cols);
    impl TreeSink for Escaping {
        type Error = String;
        fn columns(
            &mut self,
            nodes: usize,
            image_len: usize,
            ext: Vec<String>,
        ) -> Result<(NodeColumns<'_>, &mut [u8]), String> {
            self.0.columns(nodes, image_len, ext)
        }
        fn ext_text(raw: &[u8]) -> String {
            raw.iter()
                .map(|&b| match b.is_ascii() {
                    true => char::from(b).to_string(),
                    false => format!(r"\x{b:02x}"),
                })
                .collect()
        }
    }
    let file = |n: u64, name: Vec<u16>| im::Rec {
        streams: vec![im::stream(10)],
        ..im::rec(n, vec![im::nm16(im::ROOT, name, im::WIN32)])
    };
    // `a.X` + a lone surrogate (WTF-8 ED B2 80), `b.x` + another one (ED B2 81), `c.TXT`
    let mut recs = im::metafile_records();
    recs.extend([
        file(100, [im::w("a.X"), vec![0xDC80]].concat()),
        file(101, [im::w("b.x"), vec![0xDC81]].concat()),
        file(102, im::w("c.TXT")),
    ]);
    let table = || parsed(&recs);
    let ext_of = |c: &Cols, name: &[u8]| {
        let i = c
            .names()
            .iter()
            .position(|n| *n == name)
            .expect("a node of that name");
        c.ext_table[c.ext[i] as usize].clone()
    };
    let (a, b) = (b"a.X\xED\xB2\x80".as_slice(), b"b.x\xED\xB2\x81".as_slice());

    let mut plain = Cols::default();
    build_tree(table(), &NAMES, &TreeOptions::default(), &mut plain).unwrap();
    assert_eq!(ext_of(&plain, a), "x\u{FFFD}\u{FFFD}\u{FFFD}");
    assert_eq!(ext_of(&plain, b), ext_of(&plain, a));
    assert_eq!(ext_of(&plain, b"c.TXT"), "txt");
    assert_eq!(plain.ext_table.len(), 3); // "", the U+FFFD one, "txt"

    let mut escaping = Escaping(Cols::default());
    build_tree(table(), &NAMES, &TreeOptions::default(), &mut escaping).unwrap();
    let c = &escaping.0;
    assert_eq!(ext_of(c, a), r"x\xed\xb2\x80");
    assert_eq!(ext_of(c, b), r"x\xed\xb2\x81");
    assert_eq!(ext_of(c, b"c.TXT"), "txt");
    assert_eq!(c.ext_table.len(), 4);
}
