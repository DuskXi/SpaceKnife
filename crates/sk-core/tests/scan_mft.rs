//! `table_to_store` (port of the store half of backend/tests/test_mft.py), on synthetic `$MFT`
//! images from sk-native's test image builder.

#[path = "../../sk-native/tests/mft_image/mod.rs"]
#[allow(dead_code, clippy::all)]
mod im;

use im::{FA_ARCHIVE, Rec, WIN32, nm, rec, stream};
use sk_core::scan::mft::{META_FOLDER, MftBuild, ORPHAN_FOLDER, table_to_store};
use sk_native::mft::{MftTable, parse_mft};
use sk_store::testing::check_invariants;
use sk_store::{Flags, Kind, NodeId, Nodes, Store};

const RS: usize = 1024;
const CS: usize = 4096;

fn sample() -> MftTable {
    parse_mft(&im::build_mft(&im::sample_records(), RS, CS, 0), RS, CS).unwrap()
}

fn build(t: MftTable, root: &str) -> Store {
    table_to_store(t, root, &MftBuild::default()).unwrap()
}

fn wname(s: &Nodes, id: NodeId) -> String {
    String::from_utf8_lossy(s.name_bytes(id)).into_owned()
}

fn node(s: &Nodes, path: &str) -> NodeId {
    let mut cur = s.roots().next().unwrap();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        cur = s
            .children(cur)
            .find(|&c| wname(s, c) == part)
            .unwrap_or_else(|| panic!("{part} not found in {path}"));
    }
    cur
}

fn child_names(s: &Nodes, path: &str) -> std::collections::BTreeSet<String> {
    s.children(node(s, path)).map(|c| wname(s, c)).collect()
}

fn set(items: &[&str]) -> std::collections::BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn store_shape() {
    let store = build(sample(), "C:\\");
    let s = store.read();
    check_invariants(&s).unwrap();
    let root = s.roots().next().unwrap();
    assert_eq!(s.name(root), "C:\\");
    assert_eq!(s.flags(root), Flags::NONE);
    assert_eq!(s.ino(root), im::ROOT);
    let top = child_names(&s, "");
    assert!(
        set(&[
            "docs",
            "comp.log",
            "x",
            "junction",
            META_FOLDER,
            ORPHAN_FOLDER
        ])
        .is_subset(&top)
    );
    assert!(!top.contains(".") && !top.contains("$MFT"));
    assert_eq!(
        child_names(&s, "docs"),
        set(&[
            "readme.txt",
            "big.iso",
            "hl-b.bin",
            "Y~1",
            "fragmented.dat",
            "文档.docx",
            "sub"
        ])
    );
    assert_eq!(child_names(&s, "docs/sub"), set(&["deep.tar.gz"]));
    let meta = child_names(&s, META_FOLDER);
    assert!(set(&["$MFT", "$LogFile", "$Secure", "$Extend"]).is_subset(&meta));
    assert_eq!(
        child_names(&s, &format!("{META_FOLDER}/$Extend")),
        set(&["$Quota", "$ObjId", "$UsnJrnl", "$RmMetadata"])
    );
    assert_eq!(
        child_names(&s, &format!("{META_FOLDER}/$Extend/$RmMetadata")),
        set(&["$Txf"])
    );
    let orphans = child_names(&s, ORPHAN_FOLDER);
    assert!(orphans.contains("orphan.txt"));
    let cyc: Vec<&str> = ["cycle-a", "cycle-b"]
        .into_iter()
        .filter(|c| orphans.contains(*c))
        .collect();
    assert_eq!(cyc.len(), 1);
    let other = if cyc[0] == "cycle-a" {
        "cycle-b"
    } else {
        "cycle-a"
    };
    assert!(child_names(&s, &format!("{ORPHAN_FOLDER}/{}", cyc[0])).contains(other));
    for pseudo in [META_FOLDER, ORPHAN_FOLDER] {
        assert_eq!(s.flags(node(&s, pseudo)), Flags::VIRTUAL);
    }
}

#[test]
fn store_values() {
    let store = build(sample(), "C:\\");
    let s = store.read();
    let v = s.view(node(&s, "docs/big.iso"));
    assert_eq!(
        (v.size, v.alloc, v.ext.as_str(), v.mtime, v.ctime, v.atime),
        (
            10_000_000,
            10_002_432,
            "iso",
            1_700_000_000,
            1_600_000_000,
            1_700_000_500
        )
    );
    assert_eq!(s.ino(node(&s, "docs/big.iso")), 66);
    assert!(
        !s.flags(node(&s, "hl-a.bin"))
            .intersects(Flags::HARDLINK_DUP)
    );
    assert!(
        s.flags(node(&s, "docs/hl-b.bin"))
            .intersects(Flags::HARDLINK_DUP)
    );
    assert!(
        s.flags(node(&s, "frag-link.dat"))
            .intersects(Flags::HARDLINK_DUP)
    );
    let j = node(&s, "junction");
    assert_eq!((s.kind(j), s.alloc(j)), (Kind::Link, 0));
    assert_eq!(s.alloc(node(&s, "docs")), 0);
    assert_eq!(s.size(node(&s, "with-ads.txt")), 3000);
    let lone = String::from_utf8_lossy(
        &[b"lone".as_slice(), &[0xED, 0xA0, 0x80], b"surrogate.txt"].concat(),
    )
    .into_owned();
    for (name, ext) in [
        ("comp.log", "log"),
        ("\u{1F600}.PNG", "png"),
        ("docs/sub/deep.tar.gz", "gz"),
        (".hidden-dotfile", ""),
        ("trailing.", ""),
        ("x.ThisExtensionIsTooLong", ""),
        ("y.SixteenCharsExtX", "sixteencharsextx"),
        ("z.扩展名", "扩展名"),
        (lone.as_str(), "txt"),
        ("docs/文档.docx", "docx"),
    ] {
        assert_eq!(s.view(node(&s, name)).ext, ext, "{name}");
    }
}

#[test]
fn store_totals() {
    let t = sample();
    let n = t.rows();
    let first: Vec<bool> = (0..n)
        .map(|i| i == 0 || t.record[i] != t.record[i - 1])
        .collect();
    let mut alloc = 0i64;
    let mut size = 0i64;
    let mut files = 0i64;
    #[allow(clippy::needless_range_loop)]
    for i in 0..n {
        let file = t.kind[i] == Kind::File as u8;
        files += file as i64;
        if first[i] && file {
            alloc += t.alloc[i];
            size += t.size[i];
        }
        let meta = (t.record[i] < 16 && t.record[i] != 8) || (24..=28).contains(&t.record[i]);
        if meta {
            alloc += t.ads_alloc[i];
        }
    }
    let store = build(t, "C:\\");
    let s = store.read();
    let root = s.roots().next().unwrap();
    assert_eq!(s.tot_alloc(root), alloc);
    assert_eq!(s.tot_size(root), size);
    assert_eq!(s.tot_files(root) as i64, files);
    assert_eq!(
        s.tot_alloc(node(&s, &format!("{META_FOLDER}/$Extend"))),
        2 << 20
    );
}

#[test]
fn badclus_does_not_count() {
    for include_ads in [false, true] {
        let store = table_to_store(
            sample(),
            "C:\\",
            &MftBuild {
                include_ads,
                ..Default::default()
            },
        )
        .unwrap();
        let s = store.read();
        let v = s.view(node(&s, &format!("{META_FOLDER}/$BadClus")));
        assert_eq!((v.size, v.alloc), (0, 0));
        assert!(s.tot_alloc(s.roots().next().unwrap()) < 1 << 40);
    }
}

#[test]
fn include_ads() {
    let store = table_to_store(
        sample(),
        "C:\\",
        &MftBuild {
            include_ads: true,
            ..Default::default()
        },
    )
    .unwrap();
    let s = store.read();
    check_invariants(&s).unwrap();
    let v = s.view(node(&s, "with-ads.txt"));
    assert_eq!((v.size, v.alloc), (103_026, 106_496));
    assert_eq!(s.alloc(node(&s, "adsdir")), 12_288);
}

#[test]
fn count_links_each() {
    let store = table_to_store(
        sample(),
        "C:\\",
        &MftBuild {
            count_links_each: true,
            ..Default::default()
        },
    )
    .unwrap();
    let s = store.read();
    assert!(
        !s.flags(node(&s, "docs/hl-b.bin"))
            .intersects(Flags::HARDLINK_DUP)
    );
}

fn file(n: u64, parent: u64, name: &str, size: i64) -> Rec {
    Rec {
        streams: vec![stream(size)],
        ..rec(n, vec![nm(parent, name, WIN32)])
    }
}

fn dir(n: u64, parent: u64, name: &str) -> Rec {
    Rec {
        dir: true,
        attrs: 0x10,
        ..rec(n, vec![nm(parent, name, WIN32)])
    }
}

#[test]
fn cycles_and_odd_parents() {
    let mut recs = im::metafile_records();
    recs.extend([
        dir(70, 72, "c1"),
        dir(71, 70, "c2"),
        dir(72, 71, "c3"),
        file(73, 71, "below.txt", 5000),
        dir(74, 74, "self"),
        file(75, 75, "selfkid", 1),
        file(76, 66, "under-a-file", 0),
        file(66, im::ROOT, "file", 1),
        file(77, 1 << 40, "far-parent", 0),
    ]);
    let store = build(
        parse_mft(&im::build_mft(&recs, RS, CS, 0), RS, CS).unwrap(),
        "D:\\",
    );
    let s = store.read();
    check_invariants(&s).unwrap();
    let orphans = child_names(&s, ORPHAN_FOLDER);
    assert!(
        set(&["self", "selfkid", "under-a-file", "far-parent", "c1"]).is_subset(&orphans),
        "{orphans:?}"
    );
    assert_eq!(
        child_names(&s, &format!("{ORPHAN_FOLDER}/c1")),
        set(&["c2"])
    );
    assert_eq!(
        child_names(&s, &format!("{ORPHAN_FOLDER}/c1/c2")),
        set(&["c3", "below.txt"])
    );
    assert_eq!(s.tot_alloc(node(&s, ORPHAN_FOLDER)), 8192);
}

#[test]
fn minimal_tables() {
    let empty = parse_mft(&[], RS, CS).unwrap();
    let store = build(empty, "E:\\");
    let s = store.read();
    check_invariants(&s).unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(s.name(1), "E:\\");
    assert_eq!(s.child_count(1), 0);
    // no root record at all: entries naming record 5 as parent still land under the root
    let recs = vec![dir(64, im::ROOT, "a"), file(65, 64, "b", 1)];
    let store = build(
        parse_mft(&im::build_mft(&recs, RS, CS, 0), RS, CS).unwrap(),
        "F:\\",
    );
    let s = store.read();
    check_invariants(&s).unwrap();
    assert_eq!(child_names(&s, ""), set(&["a"]));
    assert_eq!(child_names(&s, "a"), set(&["b"]));
}

#[test]
fn same_totals_as_a_hard_scan_would_count() {
    // {"a": {"b": {"c.txt": 5000, "d": {}}, "e.bin": 100}, "f.iso": 1 << 30, "g": {"h": 1, "i": 4096}}
    let mut recs = im::metafile_records();
    recs.extend([
        dir(64, im::ROOT, "a"),
        dir(65, 64, "b"),
        file(66, 65, "c.txt", 5000),
        dir(67, 65, "d"),
        file(68, 64, "e.bin", 100),
        file(69, im::ROOT, "f.iso", 1 << 30),
        dir(70, im::ROOT, "g"),
        file(71, 70, "h", 1),
        file(72, 70, "i", 4096),
    ]);
    let _ = FA_ARCHIVE;
    let store = build(
        parse_mft(&im::build_mft(&recs, RS, CS, 0), RS, CS).unwrap(),
        "C:\\",
    );
    let s = store.read();
    check_invariants(&s).unwrap();
    assert!(!child_names(&s, "").contains(ORPHAN_FOLDER));
    let alloc = |size: i64| {
        if size <= im::RESIDENT_MAX {
            0
        } else {
            (size + 4095) / 4096 * 4096
        }
    };
    let a = node(&s, "a");
    assert_eq!(
        (s.tot_size(a), s.tot_alloc(a), s.tot_files(a), s.tot_dirs(a)),
        (5100, alloc(5000), 2, 2)
    );
    let g = node(&s, "g");
    assert_eq!((s.tot_size(g), s.tot_alloc(g)), (4097, alloc(4096)));
    let root = s.roots().next().unwrap();
    let meta = node(&s, META_FOLDER);
    assert_eq!(s.tot_size(root) - s.tot_size(meta), 5100 + (1 << 30) + 4097);
    assert_eq!(s.tot_dirs(root) - s.tot_dirs(meta) - 1, 4);
}

#[test]
fn extension_rule_at_the_end_of_the_blob() {
    let wide = "\u{10000}".repeat(5); // 5 characters, 20 bytes
    let names = [
        "a.TXT".to_string(),
        format!("b.{wide}"),
        "d.Abcdefghij".into(),
        format!("c.{}", "é".repeat(17)),
    ];
    let recs: Vec<Rec> = names
        .iter()
        .enumerate()
        .map(|(i, n)| file(64 + i as u64, im::ROOT, n, 1))
        .collect();
    let store = build(
        parse_mft(&im::build_mft(&recs, RS, CS, 0), RS, CS).unwrap(),
        "C:\\",
    );
    let s = store.read();
    let got: std::collections::BTreeMap<String, String> = s
        .children(1)
        .map(|c| (wname(&s, c), s.view(c).ext))
        .collect();
    assert_eq!(got["a.TXT"], "txt");
    assert_eq!(got[&format!("b.{wide}")], wide);
    assert_eq!(got[&format!("c.{}", "é".repeat(17))], "");
    assert_eq!(got["d.Abcdefghij"], "abcdefghij");
}

#[test]
fn many_names_extensions_across_chunks() {
    // more rows than one extension chunk (65,536): ids stay consistent across chunks
    let n = 70_000u64;
    let exts = ["TXT", "zip", "Abcdefghij", "txt", "longerext", "", "7z"];
    let recs: Vec<Rec> = (0..n)
        .map(|i| {
            let e = exts[i as usize % exts.len()];
            let name = if e.is_empty() {
                format!("f{i}")
            } else {
                format!("f{i}.{e}")
            };
            file(64 + i, im::ROOT, &name, 1)
        })
        .collect();
    let store = build(
        parse_mft(&im::build_mft(&recs, RS, CS, 0), RS, CS).unwrap(),
        "C:\\",
    );
    let s = store.read();
    let mut table: Vec<String> = s.ext_table().iter().map(|e| e.to_string()).collect();
    table.sort();
    assert_eq!(table, ["", "7z", "abcdefghij", "longerext", "txt", "zip"]);
    for c in s.children(1) {
        let name = wname(&s, c);
        let want = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_lowercase())
            .unwrap_or_default();
        assert_eq!(s.view(c).ext, want, "{name}");
    }
}
