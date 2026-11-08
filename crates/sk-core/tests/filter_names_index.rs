//! NameIndex (Python `test_names_index.py`): whole-character `?` and concurrent use from several
//! threads.

mod view_support;

use std::sync::{Arc, Barrier};

use sk_core::filter::NameIndex;
use sk_protocol::filter_syntax::normalize_glob;
use sk_store::{Entry, EntryBatch, NodeId, Store};
use view_support::PyRandom;

fn flat_store(names: &[String]) -> Store {
    let store = Store::new();
    {
        let mut app = store.appender();
        let root = app.add_root_dir("root").unwrap();
        let mut b = EntryBatch::new();
        for n in names {
            b.push(Entry::file(n.as_bytes(), 0, 0));
        }
        app.ingest_listing(root, &b).unwrap();
    }
    store
}

#[test]
fn question_mark_never_matches_part_of_a_character() {
    let names: Vec<String> = ["é", "ab", "中", "中.txt", "中é", "xé"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for (pattern, expected) in [
        ("?", vec!["é", "中"]),
        ("??", vec!["ab", "xé", "中é"]),
        ("???", vec![]),
        ("?.txt", vec!["中.txt"]),
        ("???.txt", vec![]),
        ("中?", vec!["中é"]),
        ("中??", vec![]),
    ] {
        let store = flat_store(&names);
        let r = store.read();
        let ids = NameIndex::new().match_glob(&r, &normalize_glob(pattern), r.len());
        let mut got: Vec<String> = ids.iter().map(|&i| r.name(i).into_owned()).collect();
        got.sort();
        let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(got, want, "{pattern}");
    }
}

const STEMS: &[&str] = &[
    "f", "F", "file", "Fig", "a9", "x7", "log", "Q", "ff9", "notes",
];
const EXTS: &[&str] = &[".txt", ".TXT", ".log", "", ".f9"];
const TIERS: &[&str] = &["f12", "fi9", "x7"];

/// fnmatch on lower-cased names (`*` and literal characters only).
fn fnmatch(name: &str, pat: &str) -> bool {
    let (n, p): (Vec<char>, Vec<char>) = (name.chars().collect(), pat.chars().collect());
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while i < n.len() {
        if j < p.len() && p[j] == '*' {
            star = Some(j);
            j += 1;
            mark = i;
        } else if j < p.len() && p[j] == n[i] {
            i += 1;
            j += 1;
        } else if let Some(s) = star {
            j = s + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    while j < p.len() && p[j] == '*' {
        j += 1;
    }
    j == p.len()
}

fn tiers(names: &[String], q: &str) -> [Vec<NodeId>; 4] {
    let mut out: [Vec<NodeId>; 4] = Default::default();
    for (i, raw) in names.iter().enumerate() {
        let name = raw.to_lowercase();
        let sub = {
            let mut it = name.chars();
            q.chars().all(|c| it.by_ref().any(|x| x == c))
        };
        let t = if name == q {
            0
        } else if name.starts_with(q) {
            1
        } else if name.contains(q) {
            2
        } else if q.len() >= 2 && sub {
            3
        } else {
            continue;
        };
        out[t].push(i as NodeId);
    }
    out
}

#[test]
fn concurrent_update_and_queries_stay_complete() {
    let mut rng = PyRandom::new(7);
    let mut names = Vec::new();
    for _ in 0..12_000 {
        let stem = *rng.choice(STEMS);
        let digits = rng.randrange(4) + 1;
        let num = rng.randrange(10u64.pow(digits as u32));
        names.push(format!("{stem}{num}{}", rng.choice(EXTS)));
    }
    let store = flat_store(&names);
    let total = store.len();
    let all: Vec<String> = {
        let r = store.read();
        (0..total as NodeId)
            .map(|i| r.name(i).into_owned())
            .collect()
    };
    let lowered: Vec<String> = all.iter().map(|n| n.to_lowercase()).collect();
    let masks: Vec<String> = (0..10)
        .map(|d| format!("*{d}*.txt"))
        .chain((0..10).map(|d| format!("f{d}*")))
        .collect();
    let want_masks: Vec<Vec<NodeId>> = masks
        .iter()
        .map(|m| {
            (0..total as NodeId)
                .filter(|&i| fnmatch(&lowered[i as usize], m))
                .collect()
        })
        .collect();
    let seq: Vec<NodeId> = (0..total as NodeId)
        .filter(|&i| {
            let l = &lowered[i as usize];
            l.find('f').is_some_and(|at| l[at..].contains('9'))
        })
        .collect();
    let want_tiers: Vec<[Vec<NodeId>; 4]> = TIERS.iter().map(|q| tiers(&all, q)).collect();
    let index = Arc::new(NameIndex::with_segment_nodes(61)); // many segments: many interleavings
    let workers = 8;
    let barrier = Arc::new(Barrier::new(workers));
    let below = |v: &[NodeId], n: usize| -> Vec<NodeId> {
        v.iter().copied().filter(|&i| (i as usize) < n).collect()
    };
    std::thread::scope(|s| {
        for t in 0..workers {
            let (index, barrier, store) = (index.clone(), barrier.clone(), store.clone());
            let (masks, want_masks, seq, want_tiers) = (&masks, &want_masks, &seq, &want_tiers);
            s.spawn(move || {
                barrier.wait();
                let mut steps: Vec<usize> = (200 + 37 * t..total).step_by(900 + 50 * t).collect();
                steps.push(total);
                for (k, &n) in steps.iter().enumerate() {
                    let r = store.read();
                    index.update(&r, n);
                    let m = (t + k) % masks.len();
                    assert_eq!(index.match_glob(&r, &masks[m], n), below(&want_masks[m], n));
                    let q = (t + k) % TIERS.len();
                    let got = index.ascii_tiers(&r, TIERS[q], n, true);
                    for (g, w) in got.iter().zip(&want_tiers[q]) {
                        assert_eq!(g, &below(w, n), "{} {n}", TIERS[q]);
                    }
                    let pred = |name: &[u8]| {
                        let l = name.to_ascii_lowercase();
                        l.iter()
                            .position(|&b| b == b'f')
                            .is_some_and(|at| l[at..].contains(&b'9'))
                    };
                    assert_eq!(index.search(&r, "seq:f9", &pred, n), below(seq, n));
                }
            });
        }
    });
    // every node indexed exactly once, in order
    let mut end = 0;
    for (first, len) in index.segments() {
        assert_eq!(first as usize, end);
        end += len;
    }
    assert_eq!(end, total);
    assert_eq!(index.len(), total);
    let r = store.read();
    for (m, want) in masks.iter().zip(&want_masks) {
        assert_eq!(&index.match_glob(&r, m, total), want);
    }
}
