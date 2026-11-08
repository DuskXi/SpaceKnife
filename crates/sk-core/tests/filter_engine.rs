//! Filter engine against an independent node-by-node reference evaluator on random trees (Python
//! `test_filter_engine.py`).

mod view_support;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sk_core::filter::classes::ClassMap;
use sk_core::filter::{FilterContext, NameIndex, attr_flag, evaluate, tag_bit};
use sk_protocol::filter::{AttrSet, FilterNormal};
use sk_protocol::filter_syntax::parse;
use sk_store::testing::ext_of;
use sk_store::{Entry, EntryBatch, Flags, Kind, NodeId, Nodes, Store};
use view_support::PyRandom;

const NOW: i64 = 1_800_000_000;
const DAY: i64 = 86_400;
const ROOT: &str = if cfg!(windows) {
    "C:\\Data\\Scan Root"
} else {
    "/data/Scan Root"
};

const STEMS: &[&str] = &[
    "a",
    "B",
    "notes",
    "Temp",
    "temp",
    "x",
    "Ä",
    "ä",
    "视频",
    "Program Files",
    "tmp",
    "abc",
    "Abc",
];
const EXTS: &[&str] = &[
    "", ".txt", ".TXT", ".jpg", ".mp4", ".log", ".tar.gz", ".", ".Mp3", ".zip", ".ts", ".MTS",
];
const ATTRS: &[Flags] = &[
    Flags::HIDDEN,
    Flags::READONLY,
    Flags::SYSTEM,
    Flags::ARCHIVE,
    Flags::COMPRESSED,
    Flags::SPARSE,
];

fn build_random_store(seed: u64, max_nodes: usize) -> (Store, Vec<u8>) {
    let mut rng = PyRandom::new(seed);
    let store = Store::new();
    {
        let mut app = store.appender();
        let root = app.add_root_dir(ROOT).unwrap();
        let mut queue: std::collections::VecDeque<(NodeId, u32)> = [(root, 1)].into();
        while let Some((d, depth)) = queue.pop_front() {
            if app.len() >= max_nodes {
                break;
            }
            let count = if depth < 7 {
                rng.randint(4, 12)
            } else {
                rng.randint(0, 4)
            };
            let mut b = EntryBatch::new();
            for slot in 0..count {
                let mut r = rng.random();
                if depth < 4 && slot < 2 {
                    r = 0.0;
                }
                let kind = if r < 0.35 && depth < 7 {
                    Kind::Dir
                } else if r > 0.95 {
                    Kind::Link
                } else {
                    Kind::File
                };
                let stem = *rng.choice(STEMS);
                let mut name = if kind == Kind::Dir {
                    stem.to_owned()
                } else {
                    let pre = if rng.random() < 0.05 {
                        *rng.choice(&["", "."])
                    } else {
                        ""
                    };
                    format!("{pre}{stem}")
                };
                if kind != Kind::Dir {
                    name.push_str(rng.choice(EXTS));
                }
                let mut flags = Flags::NONE;
                for &f in ATTRS {
                    if rng.random() < 0.2 {
                        flags |= f;
                    }
                }
                if kind == Kind::File && rng.random() < 0.05 {
                    flags |= Flags::HARDLINK_DUP;
                }
                let size = if kind == Kind::Dir {
                    0
                } else {
                    *rng.choice(&[0i64, 10, 4000, 4096, 5000, 1 << 20, 3 << 20, 50 << 20])
                };
                let mut times = [0u32; 3];
                for t in &mut times {
                    *t = if rng.random() < 0.05 {
                        0
                    } else {
                        (NOW - *rng.choice(&[1i64, 2, 10, 40, 400, 800]) * DAY - 7) as u32
                    };
                }
                let ext = if kind == Kind::File {
                    app.ext_id(&ext_of(&name))
                } else {
                    0
                };
                b.push(Entry {
                    flags,
                    size,
                    alloc: (size + 4095) / 4096 * 4096,
                    mtime: times[0],
                    ctime: times[1],
                    atime: times[2],
                    ext,
                    ..Entry::new(name.as_bytes(), kind)
                });
            }
            let first = app.ingest_listing(d, &b).unwrap();
            for i in 0..count as NodeId {
                if app.is_dir(first + i) {
                    queue.push_back((first + i, depth + 1));
                }
            }
        }
    }
    let n = store.len();
    let mut tags = vec![0u8; n];
    for _ in 0..40 {
        let i = 1 + rng.randrange(n as u64 - 1) as usize;
        tags[i] = *rng.choice(&[1u8, 2, 4, 8]);
    }
    (store, tags)
}

fn random_store(seed: u64) -> &'static (Store, Vec<u8>) {
    use std::sync::OnceLock;
    static S: [OnceLock<(Store, Vec<u8>)>; 4] = [const { OnceLock::new() }; 4];
    S[seed as usize].get_or_init(|| {
        let s = build_random_store(seed, 2500);
        {
            let r = s.0.read();
            assert!(r.len() > 1500 && r.depths().iter().copied().max().unwrap() >= 5);
        }
        s
    })
}

// --------------------------------------------------------------------------- reference

fn variants(c: char) -> Vec<char> {
    let mut out = vec![c];
    let one = |mut it: std::iter::Peekable<std::vec::IntoIter<char>>| {
        let x = it.next()?;
        it.peek().is_none().then_some(x)
    };
    for v in [
        one(c.to_lowercase().collect::<Vec<_>>().into_iter().peekable()),
        one(c.to_uppercase().collect::<Vec<_>>().into_iter().peekable()),
    ]
    .into_iter()
    .flatten()
    {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

fn ref_glob(pattern: &str, name: &str) -> bool {
    // None = '*', Some(None) = '?', Some(Some(c)) = literal
    let p: Vec<char> = pattern.chars().collect();
    let mut toks: Vec<Option<Option<char>>> = Vec::new();
    let mut i = 0;
    while i < p.len() {
        if p[i] == '\\' && i + 1 < p.len() {
            toks.push(Some(Some(p[i + 1])));
            i += 2;
            continue;
        }
        toks.push(match p[i] {
            '*' => None,
            '?' => Some(None),
            c => Some(Some(c)),
        });
        i += 1;
    }
    let s: Vec<char> = name.chars().collect();
    let mut memo = HashMap::new();
    fn go(
        t: usize,
        k: usize,
        toks: &[Option<Option<char>>],
        s: &[char],
        memo: &mut HashMap<(usize, usize), bool>,
    ) -> bool {
        if let Some(&v) = memo.get(&(t, k)) {
            return v;
        }
        let v = if t == toks.len() {
            k == s.len()
        } else {
            match toks[t] {
                None => go(t + 1, k, toks, s, memo) || (k < s.len() && go(t, k + 1, toks, s, memo)),
                Some(_) if k == s.len() => false,
                Some(None) => go(t + 1, k + 1, toks, s, memo),
                Some(Some(c)) => variants(c).contains(&s[k]) && go(t + 1, k + 1, toks, s, memo),
            }
        };
        memo.insert((t, k), v);
        v
    }
    go(0, 0, &toks, &s, &mut memo)
}

fn attrs_hold(flags: Flags, s: &AttrSet) -> bool {
    s.required.iter().all(|&a| flags.intersects(attr_flag(a)))
        && s.forbidden.iter().all(|&a| !flags.intersects(attr_flag(a)))
}

fn base_name(raw: &str) -> String {
    let t = raw.trim_end_matches(['\\', '/']);
    t.rsplit(|c| c == '/' || (cfg!(windows) && c == '\\'))
        .next()
        .unwrap_or("")
        .to_owned()
}

fn reference(nodes: &Nodes, nf: &FilterNormal, now: i64, tags: &[u8]) -> Vec<bool> {
    let n = nodes.len();
    let map = ClassMap::builtin();
    let table = nodes.ext_table();
    let mut out = vec![false; n];
    for i in 1..n as NodeId {
        if nodes.is_dir(i) {
            continue;
        }
        let name = nodes.name(i).into_owned();
        let chain: Vec<NodeId> = nodes.ancestors(i).collect();
        let folder_names: Vec<String> = chain
            .iter()
            .map(|&a| {
                if nodes.depth(a) == 1 {
                    base_name(&nodes.name(a))
                } else {
                    nodes.name(a).into_owned()
                }
            })
            .collect();
        let ext = &table[nodes.ext(i) as usize];
        let mut classes: HashSet<String> = map
            .file_classes(ext, nodes.size(i))
            .into_iter()
            .map(|c| map.ids()[c].clone())
            .collect();
        if classes.is_empty() {
            classes.insert("other".into());
        }
        let own_tags = std::iter::once(i)
            .chain(chain.iter().copied())
            .fold(0u8, |a, x| a | tags.get(x as usize).copied().unwrap_or(0));
        let flags = nodes.flags(i);
        let mut ok = true;
        if !nf.masks.is_empty() || !nf.classes.is_empty() {
            ok &= nf.masks.iter().any(|m| ref_glob(m, &name))
                || nf.classes.iter().any(|c| classes.contains(c));
        }
        ok &= !nf.not_masks.iter().any(|m| ref_glob(m, &name));
        ok &= !nf.not_classes.iter().any(|c| classes.contains(c));
        if !nf.folders.is_empty() {
            ok &= nf
                .folders
                .iter()
                .any(|f| folder_names.iter().any(|fname| ref_glob(f, fname)));
        }
        ok &= !nf
            .not_folders
            .iter()
            .any(|f| folder_names.iter().any(|fname| ref_glob(f, fname)));
        if let Some(t) = &nf.tags {
            ok &= own_tags & t.iter().fold(0, |a, &c| a | tag_bit(c)) != 0;
        }
        ok &= own_tags & nf.not_tags.iter().fold(0, |a, &c| a | tag_bit(c)) == 0;
        for r in &nf.sizes {
            let v = if r.field == "disk" {
                nodes.alloc(i)
            } else {
                nodes.size(i)
            };
            ok &= r.lo.is_none_or(|lo| v >= lo) && r.hi.is_none_or(|hi| v <= hi);
        }
        for r in &nf.ages {
            let t = match r.field.as_str() {
                "modify" => nodes.mtime(i),
                "create" => nodes.ctime(i),
                _ => nodes.atime(i),
            } as i64;
            let age = now - t;
            ok &= t != 0 && r.lo.is_none_or(|lo| age >= lo) && r.hi.is_none_or(|hi| age <= hi);
        }
        if let Some(a) = &nf.attrs {
            ok &= attrs_hold(flags, a);
        }
        for s in &nf.not_attrs {
            ok &= !attrs_hold(flags, s);
        }
        out[i as usize] = ok;
    }
    out
}

fn check_with(store: &Store, tags: &[u8], text: &str, names: Option<Arc<NameIndex>>) {
    let nf = parse(text, None, None)
        .normal
        .unwrap_or_else(|| panic!("{text}"));
    let nodes = store.read();
    let mut ctx = FilterContext::at(NOW);
    ctx.tags = Some(tags.to_vec().into());
    ctx.names = names;
    let res = evaluate(&nodes, &nf, &ctx);
    let want = reference(&nodes, &nf, NOW, tags);
    let diff: Vec<(usize, String)> = (0..nodes.len())
        .filter(|&i| res.matched[i] != want[i])
        .take(5)
        .map(|i| (i, nodes.path_lossy(i as NodeId)))
        .collect();
    assert!(diff.is_empty(), "{text}: {diff:?}");
    // bottom-up aggregation: every directory's numbers equal a direct sum over its subtree
    let mut dirs: Vec<NodeId> = vec![0];
    dirs.extend(nodes.roots());
    dirs.extend(
        (0..nodes.len() as NodeId)
            .filter(|&i| nodes.is_dir(i))
            .take(30),
    );
    for d in dirs {
        let mut sub = vec![d];
        let mut k = 0;
        while k < sub.len() {
            let c: Vec<NodeId> = nodes.children(sub[k]).collect();
            sub.extend(c);
            k += 1;
        }
        let m: Vec<NodeId> = sub.iter().copied().filter(|&x| want[x as usize]).collect();
        let counted = |x: NodeId| !nodes.flags(x).intersects(Flags::HARDLINK_DUP);
        assert_eq!(res.count[d as usize] as usize, m.len(), "{text} count {d}");
        let alloc: i64 = m
            .iter()
            .filter(|&&x| counted(x))
            .map(|&x| nodes.alloc(x))
            .sum();
        let size: i64 = m
            .iter()
            .filter(|&&x| counted(x))
            .map(|&x| nodes.size(x))
            .sum();
        assert_eq!(res.alloc[d as usize], alloc, "{text} alloc {d}");
        assert_eq!(res.size[d as usize], size, "{text} size {d}");
    }
}

fn check(store: &Store, tags: &[u8], text: &str) {
    check_with(store, tags, text, None)
}

const EXPRESSIONS: &[&str] = &[
    "",
    "*",
    "*.txt",
    "*.TXT;*.jpg",
    "a*",
    "*a*",
    "?",
    "?*",
    "*.tar.gz",
    "*.",
    ".txt",
    "ä*",
    "Ä.txt",
    "视频*",
    "|*.txt",
    "|*.txt;|*.jpg;|a*",
    ">1mb",
    ">1mb;<10mb",
    "|>1mb",
    "s>4000b;s<=4096b",
    ">=4kb",
    "<5d",
    "m>30d;m<500d",
    "c<5d",
    "a>1y",
    "|<5d",
    ":attr:h",
    ":attr:h+r-s",
    "|:attr:a",
    ":attr:c;|:attr:sp",
    ":red",
    ":tag:r+y",
    "|:all",
    ":all",
    ":class:video",
    ":class:other",
    "|:class:doc",
    "*.log;:class:image",
    "\\temp",
    "\\*temp*",
    "|\\temp",
    "\\scan root",
    "\\*",
    "\\temp;\\program files",
    "\\temp;|*.zip",
    "*.txt;\\temp;>1kb",
    "*.jpg;>1mb;\\a;:attr:-h",
    "\\x;|\\a;:tag:all-red;c>10d",
];

#[test]
fn engine_matches_reference() {
    for seed in [1, 2, 3] {
        let (store, tags) = random_store(seed);
        for text in EXPRESSIONS {
            check(store, tags, text);
        }
    }
}

#[test]
fn engine_matches_reference_on_random_combinations() {
    let words: Vec<&str> = EXPRESSIONS
        .iter()
        .flat_map(|e| e.split(';'))
        .filter(|w| !w.is_empty())
        .collect();
    let mut rng = PyRandom::new(99);
    for _ in 0..150 {
        let k = 1 + rng.randrange(5) as usize;
        let text: Vec<&str> = (0..k).map(|_| *rng.choice(&words)).collect();
        let seed = 1 + rng.randrange(3);
        let (store, tags) = random_store(seed);
        check(store, tags, &text.join(";"));
    }
}

#[test]
fn growing_store_reuses_the_name_index() {
    let (full, _) = build_random_store(7, 1500);
    let f = full.read();
    let store = Store::new();
    let index = Arc::new(NameIndex::new());
    let dirs: Vec<NodeId> = (1..f.len() as NodeId).filter(|&i| f.is_dir(i)).collect();
    let mut mapping: HashMap<NodeId, NodeId> = HashMap::new();
    {
        let mut app = store.appender();
        app.add_root_dir(f.name(1).as_ref()).unwrap();
    }
    mapping.insert(1, 1);
    for (step, &d) in dirs.iter().enumerate() {
        {
            let mut app = store.appender();
            let kids: Vec<NodeId> = f.children(d).collect();
            let mut b = EntryBatch::new();
            for &k in &kids {
                b.push(Entry {
                    flags: f.flags(k) & !Flags::PENDING,
                    size: f.size(k),
                    alloc: f.alloc(k),
                    mtime: f.mtime(k),
                    ctime: f.ctime(k),
                    atime: f.atime(k),
                    ext: app.ext_id(&f.ext_name(f.ext(k))),
                    ..Entry::new(f.name_bytes(k), f.kind(k))
                });
            }
            let first = app.ingest_listing(mapping[&d], &b).unwrap();
            for (off, &k) in kids.iter().enumerate() {
                mapping.insert(k, first + off as NodeId);
            }
        }
        if step == dirs.len() / 2 {
            let zeros = vec![0u8; store.len()];
            check_with(&store, &zeros, "*a*;\\temp", Some(index.clone()));
        }
    }
    let zeros = vec![0u8; store.len()];
    check_with(&store, &zeros, "*a*;\\temp", Some(index.clone()));
    check_with(&store, &zeros, "*.txt;|\\x", Some(index.clone()));
    assert_eq!(index.len(), store.len());
}

fn flat(names: &[&str], kinds: &[Kind], sizes: &[i64], flags: &[Flags], mtimes: &[u32]) -> Store {
    let store = Store::new();
    {
        let mut app = store.appender();
        let root = app.add_root_dir(ROOT).unwrap();
        let mut b = EntryBatch::new();
        for (i, name) in names.iter().enumerate() {
            let kind = kinds.get(i).copied().unwrap_or(Kind::File);
            b.push(Entry {
                flags: flags.get(i).copied().unwrap_or_default(),
                size: sizes.get(i).copied().unwrap_or(0),
                alloc: sizes.get(i).map_or(0, |s| (s + 4095) / 4096 * 4096),
                mtime: mtimes.get(i).copied().unwrap_or(0),
                ext: if kind == Kind::File {
                    app.ext_id(&ext_of(name))
                } else {
                    0
                },
                ..Entry::new(name.as_bytes(), kind)
            });
        }
        assert_eq!(app.ingest_listing(root, &b).unwrap(), 2);
    }
    store
}

fn matched_names(store: &Store, text: &str) -> Vec<String> {
    let nf = parse(text, None, None).normal.unwrap();
    let r = store.read();
    let res = evaluate(&r, &nf, &FilterContext::at(NOW));
    let mut out: Vec<String> = (0..r.len())
        .filter(|&i| res.matched[i])
        .map(|i| r.name(i as NodeId).into_owned())
        .collect();
    out.sort();
    out
}

#[test]
fn extension_fast_path_handles_dotfiles_links_and_case() {
    let names = [
        "a.TXT",
        ".txt",
        "b.txt.bak",
        "link.txt",
        "c.Txt",
        "txt",
        "d.tXt",
    ];
    let kinds = [
        Kind::File,
        Kind::File,
        Kind::File,
        Kind::Link,
        Kind::File,
        Kind::File,
        Kind::Dir,
    ];
    let store = flat(&names, &kinds, &[], &[], &[]);
    // the directory "d.tXt" never matches
    assert_eq!(
        matched_names(&store, "*.txt"),
        [".txt", "a.TXT", "c.Txt", "link.txt"]
    );
}

#[test]
fn hardlink_duplicates_match_but_add_no_bytes() {
    let store = flat(
        &["a.bin", "b.bin"],
        &[],
        &[1000, 1000],
        &[Flags::NONE, Flags::HARDLINK_DUP],
        &[],
    );
    let nf = parse("*.bin", None, None).normal.unwrap();
    let r = store.read();
    let res = evaluate(&r, &nf, &FilterContext::at(NOW));
    assert_eq!(res.totals(1), (2, 4096, 1000));
}

#[test]
fn unknown_timestamps_satisfy_no_age_condition() {
    let store = flat(
        &["unknown", "known"],
        &[],
        &[],
        &[],
        &[0, (NOW - 3 * DAY) as u32],
    );
    for (text, want) in [
        ("<5d", vec!["known"]),
        (">5d", vec![]),
        ("|<5d", vec![]),
        ("|>5d", vec!["known"]),
        ("*", vec!["known", "unknown"]),
    ] {
        assert_eq!(matched_names(&store, text), want, "{text}");
    }
}

#[test]
fn folder_hits_mark_bold_folders_including_the_root() {
    let (store, tags) = random_store(1);
    let nf = parse("\\scan root;\\temp", None, None).normal.unwrap();
    let r = store.read();
    let mut ctx = FilterContext::at(NOW);
    ctx.tags = Some(tags.clone().into());
    let res = evaluate(&r, &nf, &ctx);
    let hits: HashSet<String> = (0..r.len())
        .filter(|&i| res.folder_hit[i])
        .map(|i| r.name(i as NodeId).into_owned())
        .collect();
    assert!(hits.contains(ROOT));
    let allowed: HashSet<String> = [ROOT, "Temp", "temp"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(hits.is_subset(&allowed), "{hits:?}");
}

#[test]
fn question_mark_counts_characters() {
    for (pattern, expected) in [
        ("?", vec!["é", "中"]),
        ("??", vec!["ab", "xé", "中é"]),
        ("???", vec![]),
        ("?.txt", vec!["中.txt"]),
        ("???.txt", vec![]),
        ("中?", vec!["中é"]),
        ("中??", vec![]),
    ] {
        let store = flat(
            &["é", "ab", "中", "中.txt", "中é", "xé"],
            &[],
            &[],
            &[],
            &[],
        );
        let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(matched_names(&store, pattern), want, "{pattern}");
    }
}
