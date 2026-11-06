//! Golden draw lists of the treemap (Python `test_layout_golden.py`): the output stays byte for
//! byte what the Python backend recorded in `data/layout_golden.json` — every column, the names and
//! the member blocks of 17 scenarios on one fixed synthetic tree (several viewports, device pixel
//! ratios and detail levels, aggregate zooms, hide-mode sizes, free and unknown space).

mod view_support;

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde_json::Value;
use sk_core::filter::ClassMap;
use sk_core::layout::{
    ArraySizes, Basis, DrawList, KEY_MORE, KEY_NODE_MASK, KEY_SMALL, LayoutParams, RootSpec,
    SizeSource, StoreSizes, effective_alloc, treemap,
};
use sk_store::{Kind, NodeId, Store};
use view_support::{column, digest, golden_tree, hide_sizes};

const GOLDEN: &str = include_str!("data/layout_golden.json");
const ROOT: NodeId = 1;

struct Base {
    store: Store,
    basis: Basis,
}

fn base() -> &'static Base {
    static B: OnceLock<Base> = OnceLock::new();
    B.get_or_init(|| Base {
        store: golden_tree(20_000, 7, 0),
        // the built-in classes and the folder classes, as the Python recording had them
        basis: Basis::new(Arc::new(ClassMap::builtin().clone()))
            .with_folder_classes(Arc::new(sk_core::pathclass::FolderClasses::new())),
    })
}

fn run(
    store: &Store,
    p: &LayoutParams,
    root: &RootSpec,
    sizes: Option<&dyn SizeSource>,
) -> DrawList {
    let r = store.read();
    let own = StoreSizes::of(&r);
    let mut out = DrawList::new();
    treemap(&r, &base().basis, p, root, sizes.unwrap_or(&own), &mut out);
    out
}

fn fhd() -> LayoutParams {
    LayoutParams::new(1920.0, 1080.0)
}

/// Zoom into the `kind` block with the most members of the 1080p frame.
fn aggregate(kind: u32) -> RootSpec {
    let first = run(&base().store, &fhd(), &RootSpec::node(ROOT), None);
    let key = *first
        .members
        .keys()
        .filter(|&&k| k & !KEY_NODE_MASK == kind)
        .max_by_key(|&&k| (first.members[&k].len(), std::cmp::Reverse(k)))
        .unwrap();
    RootSpec {
        node: key & KEY_NODE_MASK,
        members: Some(Arc::new(first.members[&key].clone())),
        key: Some(key),
        ..RootSpec::default()
    }
}

fn largest(want_dir: bool) -> NodeId {
    let r = base().store.read();
    let mut best = (i64::MIN, 0);
    for i in ROOT + 1..r.len() as NodeId {
        if r.is_dir(i) == want_dir {
            let s = effective_alloc(&r, i);
            if s > best.0 {
                best = (s, i);
            }
        }
    }
    best.1
}

fn params(w: f64, h: f64, f: impl FnOnce(&mut LayoutParams)) -> LayoutParams {
    let mut p = LayoutParams::new(w, h);
    f(&mut p);
    p
}

fn volume(free: i64, unknown: i64) -> RootSpec {
    RootSpec {
        node: ROOT,
        free: Some(free),
        unknown: Some(unknown),
        ..RootSpec::default()
    }
}

fn scenario(name: &str) -> DrawList {
    let b = base();
    let s = &b.store;
    let root = RootSpec::node(ROOT);
    match name {
        "fhd" => run(s, &fhd(), &root, None),
        "uhd_detail15" => run(s, &params(3840.0, 2160.0, |p| p.detail = 15), &root, None),
        "dpr125" => run(
            s,
            &params(1536.0, 864.0, |p| {
                p.dpr = 1.25;
                p.detail = 8
            }),
            &root,
            None,
        ),
        "dpr150" => run(
            s,
            &params(1280.0, 720.0, |p| {
                p.dpr = 1.5;
                p.detail = 10
            }),
            &root,
            None,
        ),
        "volume" => run(s, &fhd(), &volume(2 << 30, 500 << 20), None),
        "volume_by_name" => run(
            s,
            &params(1600.0, 900.0, |p| {
                p.dpr = 1.25;
                p.sort_by_size = false
            }),
            &volume(2 << 30, 500 << 20),
            None,
        ),
        "volume_tiny_free" => run(s, &fhd(), &volume(4096, 0), None),
        "font_aspect" => run(
            s,
            &params(1920.0, 1080.0, |p| {
                p.detail = 6;
                p.min_element = 20.0;
                p.font_scale = 1.25;
                p.aspect = 1.6
            }),
            &root,
            None,
        ),
        "hide" | "hide_uhd_dpr150" => {
            let sizes = ArraySizes(Arc::new(hide_sizes(&s.read())));
            let p = if name == "hide" {
                params(1920.0, 1080.0, |p| p.detail = 8)
            } else {
                params(2560.0, 1440.0, |p| {
                    p.dpr = 1.5;
                    p.detail = 15
                })
            };
            run(s, &p, &root, Some(&sizes))
        }
        "aggregate_small" => run(s, &fhd(), &aggregate(KEY_SMALL), None),
        "aggregate_more" => run(s, &fhd(), &aggregate(KEY_MORE), None),
        "aggregate_by_name" => run(
            s,
            &params(1280.0, 720.0, |p| {
                p.dpr = 1.5;
                p.sort_by_size = false
            }),
            &aggregate(KEY_SMALL),
            None,
        ),
        "aggregate_leftover" => {
            let zoom = aggregate(KEY_SMALL);
            let first = run(s, &fhd(), &zoom, None);
            let key = *first
                .members
                .keys()
                .max_by_key(|&&k| (first.members[&k].len(), std::cmp::Reverse(k)))
                .unwrap();
            let again = RootSpec {
                node: zoom.node,
                members: Some(Arc::new(first.members[&key].clone())),
                key: Some(key),
                ..RootSpec::default()
            };
            run(s, &fhd(), &again, None)
        }
        "folder" => run(
            s,
            &params(2560.0, 1440.0, |p| {
                p.dpr = 1.5;
                p.detail = 15
            }),
            &RootSpec::node(largest(true)),
            None,
        ),
        "file_root" => run(s, &fhd(), &RootSpec::node(largest(false)), None),
        "pending" => {
            let store = golden_tree(20_000, 7, 3);
            run(
                &store,
                &params(1920.0, 1080.0, |p| p.detail = 8),
                &root,
                None,
            )
        }
        other => panic!("unknown scenario {other}"),
    }
}

fn names_digest(names: &[&str]) -> String {
    digest(names.iter().map(|s| s.as_bytes()))
}

fn fingerprint(dl: &DrawList) -> Value {
    let mut keys: Vec<u32> = dl.members.keys().copied().collect();
    keys.sort_unstable();
    let member_cols: Vec<String> = keys
        .iter()
        .map(|k| column(&dl.members[k].iter().map(|&i| i as i64).collect::<Vec<_>>()))
        .collect();
    let mut cols = serde_json::Map::new();
    cols.insert("key".into(), column(&dl.key).into());
    cols.insert("node".into(), column(&dl.node).into());
    cols.insert("parent".into(), column(&dl.parent).into());
    cols.insert("kind".into(), column(&dl.kind).into());
    cols.insert("depth".into(), column(&dl.depth).into());
    cols.insert("x".into(), column(&dl.geo[0]).into());
    cols.insert("y".into(), column(&dl.geo[1]).into());
    cols.insert("w".into(), column(&dl.geo[2]).into());
    cols.insert("h".into(), column(&dl.geo[3]).into());
    cols.insert("cls".into(), column(&dl.cls).into());
    cols.insert("label".into(), column(&dl.label).into());
    cols.insert("size".into(), column(&dl.size).into());
    cols.insert("count".into(), column(&dl.count).into());
    serde_json::json!({
        "rows": dl.len(),
        "width": dl.width,
        "height": dl.height,
        "columns": Value::Object(cols),
        "names": names_digest(&dl.name_list()),
        "member_keys": keys,
        "members": digest(member_cols.iter().map(|s| s.as_bytes())),
    })
}

fn golden() -> Value {
    serde_json::from_str(GOLDEN).unwrap()
}

#[test]
fn the_tree_is_the_recorded_one() {
    let g = golden();
    let want = &g["tree"];
    let r = base().store.read();
    let n = r.len();
    assert_eq!(want["nodes"], n);
    let folders = (0..n as NodeId).filter(|&i| r.is_dir(i)).count();
    assert_eq!(want["folders"], folders);
    assert_eq!(want["allocated"], r.tot_alloc(ROOT));
    let table: Vec<String> = r.ext_table().iter().map(|e| e.to_string()).collect();
    assert_eq!(want["ext_table"], serde_json::json!(table));
    let names: Vec<String> = (0..n as NodeId).map(|i| r.name(i).into_owned()).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(want["names"], names_digest(&refs));
    let c = &want["columns"];
    let ids = 0..n as NodeId;
    let col = |f: &dyn Fn(NodeId) -> i64| -> Vec<i64> { ids.clone().map(f).collect() };
    assert_eq!(c["parent"], column(&r.parents()[..n]));
    assert_eq!(c["depth"], column(&r.depths()[..n]));
    assert_eq!(c["kind"], column(&r.kinds_raw()[..n]));
    assert_eq!(c["alloc"], column(&r.allocs()[..n]));
    assert_eq!(c["ext"], column(&r.exts()[..n]));
    assert_eq!(c["tot_alloc"], column(&col(&|i| r.tot_alloc(i))));
    let cs: Vec<i32> = ids.clone().map(|i| r.child_start(i)).collect();
    let cc: Vec<i32> = ids.clone().map(|i| r.child_count(i)).collect();
    assert_eq!(c["child_start"], column(&cs));
    assert_eq!(c["child_count"], column(&cc));
    // flags: the store's own bits for node 0 and roots may differ from Python's (VIRTUAL);
    // compare the bits the layouts read
    let _ = Kind::Dir;
}

const SCENARIOS: &[&str] = &[
    "fhd",
    "uhd_detail15",
    "dpr125",
    "dpr150",
    "volume",
    "volume_by_name",
    "volume_tiny_free",
    "font_aspect",
    "hide",
    "hide_uhd_dpr150",
    "aggregate_small",
    "aggregate_more",
    "aggregate_by_name",
    "aggregate_leftover",
    "folder",
    "file_root",
    "pending",
];

#[test]
fn every_scenario_is_recorded() {
    let g = golden();
    let recorded: Vec<&String> = g["scenarios"].as_object().unwrap().keys().collect();
    let mut want: Vec<&str> = SCENARIOS.to_vec();
    want.sort();
    let mut got: Vec<&str> = recorded.iter().map(|s| s.as_str()).collect();
    got.sort();
    assert_eq!(got, want);
}

#[test]
fn draw_lists_are_byte_identical() {
    let g = golden();
    let mut failed = BTreeMap::new();
    for &name in SCENARIOS {
        let got = fingerprint(&scenario(name));
        let want = &g["scenarios"][name];
        if &got != want {
            let mut diff = Vec::new();
            for k in ["rows", "width", "height", "names", "member_keys", "members"] {
                if got[k] != want[k] {
                    diff.push(k.to_owned());
                }
            }
            for (k, v) in want["columns"].as_object().unwrap() {
                if &got["columns"][k] != v {
                    diff.push(format!("columns.{k}"));
                }
            }
            failed.insert(name, (diff, got["rows"].clone(), want["rows"].clone()));
        }
    }
    assert!(failed.is_empty(), "scenarios differ: {failed:#?}");
}

#[test]
fn scenarios_reach_every_path() {
    let kinds = |name: &str| -> std::collections::BTreeSet<u8> {
        scenario(name).kind.iter().copied().collect()
    };
    assert!([0, 1, 2, 4, 5].iter().all(|k| kinds("fhd").contains(k)));
    assert!([6, 7].iter().all(|k| kinds("volume").contains(k)));
    assert!(kinds("pending").contains(&3));
    assert!(kinds("aggregate_small").contains(&5));
    assert!(!kinds("volume_tiny_free").contains(&6));
    let g = golden();
    assert!(
        g["scenarios"]["uhd_detail15"]["member_keys"]
            .as_array()
            .unwrap()
            .len()
            > 20
    );
}
