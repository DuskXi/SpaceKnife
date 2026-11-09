//! Replay SpaceSniffer 2.0.5.18's recorded filter results (Python `test_filter_compat.py`).
//!
//! `data/spacesniffer_filter_oracle.json` holds what SpaceSniffer returned for every probe on the
//! fixture tree of the Python `filter_fixture.py`. SpaceKnife must return the same files, or
//! SpaceSniffer must have rejected the expression where SpaceKnife does too — except for the
//! deliberate differences in [`differences`] (docs/filter-syntax.md §9). The Python test scanned
//! the fixture from disk; here the same tree is built in memory with the same sizes, times and
//! attributes and SpaceSniffer's allocation model, so this measures the filter language only.

mod view_support;

use std::collections::{BTreeSet, HashMap};

use serde_json::Value;
use sk_core::filter::{FilterContext, evaluate};
use sk_protocol::filter_syntax::parse;
use sk_store::testing::ext_of;
use sk_store::{Entry, EntryBatch, Flags, Kind, NodeId, Store};

const ORACLE: &str = include_str!("data/spacesniffer_filter_oracle.json");
const DAY: f64 = 86_400.0;
const NOW: f64 = 1_800_000_000.25;

struct F {
    path: &'static str,
    size: i64,
    created: f64,
    modified: f64,
    accessed: f64,
    hidden: bool,
    readonly: bool,
    system: bool,
}

const fn f(path: &'static str, size: i64, c: f64, m: f64, a: f64) -> F {
    F {
        path,
        size,
        created: c,
        modified: m,
        accessed: a,
        hidden: false,
        readonly: false,
        system: false,
    }
}

fn files() -> Vec<F> {
    vec![
        f("notes.txt", 100, 2.0, 2.0, 1.0),
        f("big.txt", 3_000_000, 40.0, 10.0, 2.0),
        f("photo.jpg", 20_000_000, 400.0, 40.0, 10.0),
        f("app.log", 5000, 400.0, 400.0, 400.0),
        f("exact4000.bin", 4000, 2.0, 2.0, 2.0),
        f("README", 10, 10.0, 10.0, 10.0),
        f("UPPER.TXT", 2048, 10.0, 10.0, 10.0),
        f("a1.txt", 1024, 10.0, 10.0, 10.0),
        F {
            hidden: true,
            readonly: true,
            ..f("music.mp3", 4_000_000, 10.0, 10.0, 10.0)
        },
        f("sub/movie.mp4", 50_000_000, 10.0, 2.0, 1.0),
        f("sub/tiny.jpg", 200, 2.0, 10.0, 2.0),
        f("sub/deeper/temp/x.txt", 300, 10.0, 10.0, 10.0),
        F {
            hidden: true,
            ..f("temp/cache.txt", 1500, 2.0, 1.0, 1.0)
        },
        F {
            readonly: true,
            ..f("temp/data.zip", 8_000_000, 2.0, 40.0, 40.0)
        },
        F {
            hidden: true,
            system: true,
            ..f("Program Files/x/setup.exe", 2_000_000, 10.0, 10.0, 10.0)
        },
        f("zero.dat", 0, 10.0, 10.0, 10.0),
    ]
}

fn unit_files() -> Vec<F> {
    vec![
        f("k1010.bin", 1010, 10.0, 10.0, 10.0),
        f("m1030k.bin", 1_030_000, 10.0, 10.0, 10.0),
        f("age30_5.bin", 10, 400.0, 30.5, 400.0),
        f("age31_5.bin", 10, 400.0, 31.5, 400.0),
        f("age365_5.bin", 10, 400.0, 365.5, 400.0),
        f("age366_5.bin", 10, 400.0, 366.5, 400.0),
    ]
}

/// The fixture as a scan of `<base>/<name>` would store it.
/// A folder of the fixture: its path and its entries (file index, or None for a sub-folder).
type Folder = (String, Vec<(String, Option<usize>)>);

fn tree(name: &str, files: &[F]) -> Store {
    // folders in order of first appearance
    let mut folders: Vec<Folder> = vec![(String::new(), vec![])];
    let mut index: HashMap<String, usize> = HashMap::from([(String::new(), 0)]);
    for (i, file) in files.iter().enumerate() {
        let parts: Vec<&str> = file.path.split('/').collect();
        let mut at = String::new();
        for (k, part) in parts.iter().enumerate() {
            let child = if at.is_empty() {
                part.to_string()
            } else {
                format!("{at}/{part}")
            };
            let is_file = k + 1 == parts.len();
            let slot = index[&at];
            if is_file {
                folders[slot].1.push((part.to_string(), Some(i)));
            } else if !index.contains_key(&child) {
                folders[slot].1.push((part.to_string(), None));
                index.insert(child.clone(), folders.len());
                folders.push((child.clone(), vec![]));
            }
            at = child;
        }
    }
    let root_path = if cfg!(windows) {
        format!("C:\\Temp\\ss-compat0\\{name}")
    } else {
        format!("/tmp/ss-compat0/{name}")
    };
    let store = Store::new();
    {
        let mut app = store.appender();
        let root = app.add_root_dir(&root_path).unwrap();
        let mut queue = std::collections::VecDeque::from([(root, String::new())]);
        let time = |days: f64| (NOW - days * DAY).floor() as u32;
        while let Some((node, path)) = queue.pop_front() {
            let entries = &folders[index[&path]].1;
            let mut b = EntryBatch::new();
            for (nm, file) in entries {
                match file {
                    Some(i) => {
                        let fx = &files[*i];
                        let mut flags = Flags::ARCHIVE;
                        if fx.hidden {
                            flags |= Flags::HIDDEN;
                        }
                        if fx.readonly {
                            flags |= Flags::READONLY;
                        }
                        if fx.system {
                            flags |= Flags::SYSTEM;
                        }
                        b.push(Entry {
                            flags,
                            size: fx.size,
                            alloc: (fx.size + 4095) / 4096 * 4096, // SpaceSniffer's model
                            ctime: time(fx.created),
                            mtime: time(fx.modified),
                            atime: time(fx.accessed),
                            ext: app.ext_id(&ext_of(nm)),
                            ..Entry::new(nm.as_bytes(), Kind::File)
                        })
                    }
                    None => b.push(Entry::dir(nm.as_bytes())),
                }
            }
            let first = app.ingest_listing(node, &b).unwrap();
            for (k, (nm, file)) in entries.iter().enumerate() {
                if file.is_none() {
                    let child = if path.is_empty() {
                        nm.clone()
                    } else {
                        format!("{path}/{nm}")
                    };
                    queue.push_back((first + k as NodeId, child));
                }
            }
        }
    }
    store
}

fn ours(store: &Store, text: &str) -> Option<BTreeSet<String>> {
    let nf = parse(text, None, None).normal?;
    let r = store.read();
    // SpaceSniffer evaluated some seconds after building; never exactly on an age boundary
    let res = evaluate(&r, &nf, &FilterContext::at(NOW as i64 + 30));
    let root = r.roots().next().unwrap();
    let prefix = r.path_lossy(root).len() + 1;
    Some(
        (0..r.len())
            .filter(|&i| res.matched[i])
            .map(|i| r.path_lossy(i as NodeId)[prefix..].replace('\\', "/"))
            .collect(),
    )
}

enum Diff {
    Error,
    Files(Vec<&'static str>),
    Minus(Vec<&'static str>),
    Plus(Vec<&'static str>),
}

const BIG: [&str; 6] = [
    "Program Files/x/setup.exe",
    "big.txt",
    "music.mp3",
    "photo.jpg",
    "sub/movie.mp4",
    "temp/data.zip",
];

fn all() -> Vec<&'static str> {
    files().iter().map(|f| f.path).collect()
}

fn all_but(x: &[&str]) -> Vec<&'static str> {
    all().into_iter().filter(|p| !x.contains(p)).collect()
}

/// SpaceKnife's deliberate differences (docs/filter-syntax.md §9).
fn differences() -> HashMap<&'static str, Diff> {
    let txt: Vec<&'static str> = all()
        .into_iter()
        .filter(|p| p.to_lowercase().ends_with(".txt"))
        .collect();
    HashMap::from([
        (
            "m<5d;m>1.5d",
            Diff::Files(vec!["exact4000.bin", "notes.txt", "sub/movie.mp4"]),
        ),
        ("m>1.5d", Diff::Files(all_but(&["temp/cache.txt"]))),
        (">=1mb", Diff::Files(BIG.to_vec())),
        (">1tb", Diff::Files(vec![])),
        (">1kib", Diff::Files(all_but(&["zero.dat"]))),
        (
            ">1mon",
            Diff::Files(vec!["app.log", "photo.jpg", "temp/data.zip"]),
        ),
        (">1minute", Diff::Files(all())),
        (">1second", Diff::Files(all())),
        (
            ":class:Pictures",
            Diff::Files(vec!["photo.jpg", "sub/tiny.jpg"]),
        ),
        ("\"notes.txt\"", Diff::Files(vec!["notes.txt"])),
        ("s>4000b", Diff::Minus(vec!["exact4000.bin"])),
        ("s<4000b", Diff::Minus(vec!["exact4000.bin"])),
        ("s>4000b;s<4000b", Diff::Files(vec![])),
        (">0b", Diff::Minus(vec!["zero.dat"])),
        ("|>1mb", Diff::Minus(BIG.to_vec())),
        (
            "|<5d",
            Diff::Minus(vec![
                "exact4000.bin",
                "notes.txt",
                "sub/movie.mp4",
                "temp/cache.txt",
            ]),
        ),
        (
            ">1mb;|>10mb",
            Diff::Minus(vec!["photo.jpg", "sub/movie.mp4"]),
        ),
        (">1mb;>2mb", Diff::Minus(vec!["Program Files/x/setup.exe"])),
        ("| *.txt", Diff::Minus(txt)),
        (
            "\\ temp",
            Diff::Plus(vec![
                "sub/deeper/temp/x.txt",
                "temp/cache.txt",
                "temp/data.zip",
            ]),
        ),
        (
            "created<5d",
            Diff::Plus(vec![
                "exact4000.bin",
                "notes.txt",
                "sub/tiny.jpg",
                "temp/cache.txt",
                "temp/data.zip",
            ]),
        ),
        ("\\fixture", Diff::Files(all())),
        (
            "|*.exe;|*.dll;\\*temp*;|\\*windows*",
            Diff::Files(vec![
                "sub/deeper/temp/x.txt",
                "temp/cache.txt",
                "temp/data.zip",
            ]),
        ),
        ("\\temp\\x", Diff::Error),
        (":foo", Diff::Error),
    ])
}

fn as_set(v: &Value) -> Option<BTreeSet<String>> {
    v.as_array()
        .map(|a| a.iter().map(|s| s.as_str().unwrap().to_owned()).collect())
}

#[test]
fn same_result_as_spacesniffer() {
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle["fixture_version"], 2);
    let fixture = tree("fixture", &files());
    let units = tree("units", &unit_files());
    let diffs = differences();
    let mut failures = Vec::new();
    let mut cases = 0;
    for (tree_name, key, store) in [
        ("fixture", "results", &fixture),
        ("units", "unit_results", &units),
    ] {
        for (text, ss) in oracle[key].as_object().unwrap() {
            cases += 1;
            let ss = as_set(ss);
            let got = ours(store, text);
            let diff = if tree_name == "fixture" {
                diffs.get(text.as_str())
            } else {
                None
            };
            let want = match diff {
                None => ss.clone(),
                Some(Diff::Error) => None,
                Some(Diff::Files(v)) => Some(v.iter().map(|s| s.to_string()).collect()),
                Some(Diff::Minus(v)) => ss
                    .clone()
                    .map(|s| s.into_iter().filter(|p| !v.contains(&p.as_str())).collect()),
                Some(Diff::Plus(v)) => ss.clone().map(|s| {
                    s.into_iter()
                        .chain(v.iter().map(|p| p.to_string()))
                        .collect()
                }),
            };
            if got != want {
                failures.push(format!("{tree_name}:{text:?}: got {got:?}, want {want:?}"));
            }
            if diff.is_some() && got == ss {
                failures.push(format!(
                    "{text:?} matches SpaceSniffer now: drop it from the differences"
                ));
            }
        }
    }
    assert!(cases > 180, "{cases}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
