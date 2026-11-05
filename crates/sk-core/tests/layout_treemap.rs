//! Treemap layout (Python `test_layout.py` and `test_layout_contract.py`): tiling, proportions,
//! pixel snapping, expansion rules, G5 merging, special blocks, the row contract of every
//! registered visualization.

mod view_support;

use std::collections::HashSet;
use std::sync::Arc;

use sk_core::layout::contract::check_draw;
use sk_core::layout::squarify::squarify;
use sk_core::layout::treemap::Geom;
use sk_core::layout::{
    ArraySizes, Basis, DrawList, Engine, KEY_FREE, KEY_MORE, KEY_NODE_MASK, KEY_SMALL, KEY_UNKNOWN,
    LayoutParams, NO_CLASS, PAGE_REST, RootSpec, SizeSource, StoreSizes, effective_alloc, kind,
    label, registry, treemap,
};
use sk_store::{NodeId, Store};
use view_support::{PyRandom, S, build, build_spec, dir, find, golden_tree, hide_sizes};

const MB: i64 = 1 << 20;
const GB: i64 = 1 << 30;

fn files(prefix: &str, n: usize, size: impl Fn(usize) -> i64) -> Vec<(String, S)> {
    (0..n)
        .map(|i| (format!("{prefix}{i}"), S::F(size(i))))
        .collect()
}

fn lay_with(store: &Store, p: &LayoutParams, root: &RootSpec, sizes: Option<Vec<i64>>) -> DrawList {
    let r = store.read();
    let basis = Basis::default();
    let own = StoreSizes::of(&r);
    let arr = sizes.map(|s| ArraySizes(Arc::new(s)));
    let src: &dyn SizeSource = match &arr {
        Some(a) => a,
        None => &own,
    };
    let mut out = DrawList::new();
    treemap(&r, &basis, p, root, src, &mut out);
    let problems = check_draw(&out, &r, root, true, Some(src));
    assert!(problems.is_empty(), "{problems:#?}");
    out
}

fn lay(store: &Store, p: &LayoutParams, root: &RootSpec) -> DrawList {
    lay_with(store, p, root, None)
}

fn params(w: f64, h: f64, f: impl FnOnce(&mut LayoutParams)) -> LayoutParams {
    let mut p = LayoutParams::new(w, h);
    f(&mut p);
    p
}

// --------------------------------------------------------------------------- squarify

#[test]
fn squarify_tiles_the_rectangle_exactly() {
    let mut rng = PyRandom::new(3);
    for _ in 0..300 {
        let k = 1 + rng.randrange(60) as usize;
        let mut sizes: Vec<f64> = (0..k).map(|_| 0.01 + rng.random() * 1e6).collect();
        sizes.sort_by(|a, b| b.partial_cmp(a).unwrap());
        let (w, h) = (1.0 + rng.random() * 3999.0, 1.0 + rng.random() * 3999.0);
        let cells = squarify(&sizes, 0.0, 0.0, w, h, 1.0);
        let areas: Vec<f64> = cells.iter().map(|c| (c.2 - c.0) * (c.3 - c.1)).collect();
        let sum: f64 = areas.iter().sum();
        assert!((sum - w * h).abs() <= 1e-9 * w * h);
        let total: f64 = sizes.iter().sum();
        for (a, s) in areas.iter().zip(&sizes) {
            let want = s / total * w * h;
            assert!((a - want).abs() <= 1e-6 * want.max(1.0) + 1e-6);
        }
        for c in &cells {
            assert!(-1e-9 <= c.0 && c.0 <= c.2 && c.2 <= w + 1e-9);
            assert!(-1e-9 <= c.1 && c.1 <= c.3 && c.3 <= h + 1e-9);
        }
        for (i, a) in cells.iter().enumerate() {
            for b in &cells[i + 1..] {
                let ox = a.2.min(b.2) - a.0.max(b.0);
                let oy = a.3.min(b.3) - a.1.max(b.1);
                assert!(ox <= 1e-6 || oy <= 1e-6);
            }
        }
    }
}

#[test]
fn squarify_prefers_square_cells() {
    let cells = squarify(
        &[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0],
        0.0,
        0.0,
        6.0,
        4.0,
        1.0,
    );
    let worst = cells
        .iter()
        .map(|c| {
            let (w, h) = (c.2 - c.0, c.3 - c.1);
            (w / h).max(h / w)
        })
        .fold(0.0, f64::max);
    assert!(worst < 3.0);
}

// --------------------------------------------------------------------------- layout

pub fn random_spec(rng: &mut PyRandom, depth: u32) -> S {
    let mut items = Vec::new();
    let n = rng.randint(2, if depth < 3 { 9 } else { 4 });
    for i in 0..n {
        if depth < 4 && rng.random() < 0.3 {
            items.push((format!("d{depth}_{i}"), random_spec(rng, depth + 1)));
        } else {
            let ext = *rng.choice(&[".mp4", ".jpg", ".txt", ".zip", ".exe", ".bin", ""]);
            let size = *rng.choice(&[1, 100, 4096, 50_000, MB, 20 * MB, 700 * MB]);
            items.push((format!("f{depth}_{i}{ext}"), S::F(size)));
        }
    }
    S::D(items)
}

fn content_box(dl: &DrawList, row: usize, dpr: f64) -> (i32, i32, i32, i32) {
    let r = |x: f64| x.round_ties_even() as i32;
    let (b, pad, header) = (r(dpr).max(1), r(2.0 * dpr), r(18.0 * dpr));
    let (x, y, w, h) = (dl.x()[row], dl.y()[row], dl.w()[row], dl.h()[row]);
    (
        x + b + pad,
        y + b + header,
        x + w - b - pad,
        y + h - b - pad,
    )
}

fn check_geometry(dl: &DrawList, dpr: f64) {
    let gap = ((2.0 * dpr).round_ties_even() as i32).max(1);
    let mut by_parent: std::collections::BTreeMap<i32, Vec<usize>> = Default::default();
    for i in 0..dl.len() {
        let p = dl.parent[i];
        by_parent.entry(p).or_default().push(i);
        assert!(p < i as i32);
        let (x, y, w, h) = (dl.x()[i], dl.y()[i], dl.w()[i], dl.h()[i]);
        assert!(w as f64 >= 2.0 * dpr && h as f64 >= 2.0 * dpr);
        let (bx0, by0, bx1, by1) = if p < 0 {
            (0, 0, dl.width, dl.height)
        } else {
            assert_eq!(dl.kind[p as usize], kind::FOLDER);
            content_box(dl, p as usize, dpr)
        };
        assert!(bx0 <= x && x + w <= bx1 && by0 <= y && y + h <= by1, "{i}");
    }
    for rows in by_parent.values() {
        for (k, &a) in rows.iter().enumerate() {
            for &b in &rows[k + 1..] {
                let (ax, ay, aw, ah) = (dl.x()[a], dl.y()[a], dl.w()[a], dl.h()[a]);
                let (bx, by, bw, bh) = (dl.x()[b], dl.y()[b], dl.w()[b], dl.h()[b]);
                let sx = (bx - (ax + aw)).max(ax - (bx + bw));
                let sy = (by - (ay + ah)).max(ay - (by + bh));
                assert!(sx.max(sy) >= gap);
            }
        }
    }
}

#[test]
fn random_trees_tile_without_overlap() {
    for seed in 0..6 {
        for dpr in [1.0, 1.25, 1.5, 2.0] {
            let spec = random_spec(&mut PyRandom::new(seed), 0);
            let store = build_spec(&spec, "/root", 4096, &[]);
            let dl = lay(
                &store,
                &params(900.0, 640.0, |p| {
                    p.dpr = dpr;
                    p.detail = 6
                }),
                &RootSpec::node(1),
            );
            assert!(!dl.is_empty());
            check_geometry(&dl, dpr);
        }
    }
}

#[test]
fn areas_follow_sizes() {
    let sizes = [400, 300, 120, 90, 50, 25, 15];
    let store = build(&S::D(
        sizes
            .iter()
            .enumerate()
            .map(|(i, s)| (format!("f{i}.bin"), S::F(s * MB)))
            .collect(),
    ));
    let dl = lay(
        &store,
        &params(1200.0, 800.0, |p| p.min_element = 1.0),
        &RootSpec::node(1),
    );
    let total: i64 = dl.size.iter().sum();
    for i in 0..dl.len() {
        let cell = ((dl.w()[i] + 2) * (dl.h()[i] + 2)) as f64 / (1202.0 * 802.0);
        assert!((cell - dl.size[i] as f64 / total as f64).abs() < 0.01);
    }
}

#[test]
fn largest_first_in_the_top_left() {
    let store = build(&dir([
        ("small.bin", S::F(MB)),
        ("big.bin", S::F(9 * MB)),
        ("mid.bin", S::F(3 * MB)),
    ]));
    let dl = lay(&store, &LayoutParams::new(600.0, 400.0), &RootSpec::node(1));
    assert_eq!(dl.name_list()[0], "big.bin");
    assert_eq!((dl.x()[0], dl.y()[0]), (0, 0));
    let by_name = lay(
        &store,
        &params(600.0, 400.0, |p| p.sort_by_size = false),
        &RootSpec::node(1),
    );
    assert_eq!(by_name.name_list(), ["big.bin", "mid.bin", "small.bin"]);
}

fn nesting_spec() -> S {
    dir([
        (
            "a",
            dir([(
                "b",
                dir([(
                    "c",
                    dir([(
                        "d",
                        dir([("x.bin", S::F(50 * MB)), ("y.bin", S::F(40 * MB))]),
                    )]),
                )]),
            )]),
        ),
        ("z.bin", S::F(10 * MB)),
    ])
}

#[test]
fn detail_level_controls_nesting() {
    let store = build(&nesting_spec());
    for detail in 1..6 {
        let dl = lay(
            &store,
            &params(1000.0, 800.0, |p| p.detail = detail),
            &RootSpec::node(1),
        );
        let names = dl.name_list();
        let of = |k: u8| -> Vec<&str> {
            (0..dl.len())
                .filter(|&i| dl.kind[i] == k)
                .map(|i| names[i])
                .collect()
        };
        let abcd = ["a", "b", "c", "d"];
        assert_eq!(of(kind::FOLDER), abcd[..detail as usize - 1]);
        let want: Vec<&str> = if detail <= 4 {
            vec![abcd[detail as usize - 1]]
        } else {
            vec![]
        };
        assert_eq!(of(kind::COLLAPSED), want);
        assert_eq!(*dl.depth.iter().max().unwrap() as u32, detail);
    }
}

#[test]
fn folders_too_small_to_hold_content_collapse() {
    let store = build(&dir([
        ("big.bin", S::F(100 * MB)),
        (
            "tiny",
            dir([("a.bin", S::F(3 * MB)), ("b.bin", S::F(3 * MB))]),
        ),
    ]));
    let dl = lay(
        &store,
        &params(400.0, 300.0, |p| p.min_element = 1.0),
        &RootSpec::node(1),
    );
    let tiny = find(&store.read(), "tiny");
    let row = dl.node.iter().position(|&n| n == tiny as i32).unwrap();
    assert_eq!(dl.kind[row], kind::COLLAPSED);
    assert!(dl.w()[row] < 48 || dl.h()[row] < 40);
}

#[test]
fn small_files_merge_into_one_block_with_the_dominant_class() {
    let mut items = vec![("movie.mp4".to_owned(), S::F(900 * MB))];
    items.extend(
        files("p", 300, |_| 60_000)
            .into_iter()
            .map(|(n, s)| (n + ".jpg", s)),
    );
    items.extend(
        files("t", 100, |_| 10_000)
            .into_iter()
            .map(|(n, s)| (n + ".txt", s)),
    );
    let store = build(&S::D(items));
    let dl = lay(&store, &LayoutParams::new(800.0, 600.0), &RootSpec::node(1));
    let row = dl.row(KEY_SMALL | 1).unwrap();
    assert_eq!(dl.kind[row], kind::SMALL);
    assert_eq!(dl.count[row], 400);
    assert_eq!(dl.size[row], 300 * 60_000 + 100 * 10_000);
    assert_eq!(Basis::default().class_ids()[dl.cls[row] as usize], "image");
    let members = &dl.members[&(KEY_SMALL | 1)];
    let r = store.read();
    assert_eq!(members.len(), 400);
    assert_eq!(
        dl.size[row],
        members.iter().map(|&m| effective_alloc(&r, m)).sum::<i64>()
    );
    // mixed bytes without a majority: in no one class
    let mut mixed = vec![("movie.mp4".to_owned(), S::F(900 * MB))];
    for ext in ["jpg", "zip", "txt"] {
        mixed.extend(
            files(&ext[..1], 100, |_| 30_000)
                .into_iter()
                .map(|(n, s)| (format!("{n}.{ext}"), s)),
        );
    }
    let store = build(&S::D(mixed));
    let dm = lay(&store, &LayoutParams::new(800.0, 600.0), &RootSpec::node(1));
    assert_eq!(dm.cls[dm.row(KEY_SMALL | 1).unwrap()], NO_CLASS);
}

#[test]
fn tiny_siblings_merge_into_plus_n() {
    let mut items = vec![("big.bin".to_owned(), S::F(100 * MB))];
    items.extend((0..50).map(|i| (format!("dir{i}"), dir([("x.bin", S::F(10_000))]))));
    items.extend(
        files("f", 30, |_| 10_000)
            .into_iter()
            .map(|(n, s)| (n + ".bin", s)),
    );
    let store = build(&S::D(items));
    let dl = lay(&store, &LayoutParams::new(800.0, 600.0), &RootSpec::node(1));
    let more = dl.row(KEY_MORE | 1).unwrap();
    assert_eq!(dl.kind[more], kind::MORE);
    assert_eq!(more, dl.len() - 1); // placed last
    assert_eq!((dl.count[more], dl.size[more]), (50, 50 * 10_000));
    let small = dl.row(KEY_SMALL | 1).unwrap();
    assert_eq!(dl.count[small], 30);
}

#[test]
fn a_too_small_small_items_block_joins_plus_n() {
    let mut items = vec![("big.bin".to_owned(), S::F(100 * MB))];
    items.extend((0..50).map(|i| (format!("dir{i}"), dir([("x.bin", S::F(10_000))]))));
    items.extend(
        files("f", 30, |_| 500)
            .into_iter()
            .map(|(n, s)| (n + ".bin", s)),
    );
    let store = build(&S::D(items));
    let dl = lay(&store, &LayoutParams::new(800.0, 600.0), &RootSpec::node(1));
    let more = dl.row(KEY_MORE | 1).unwrap();
    assert!(dl.row(KEY_SMALL | 1).is_none());
    assert_eq!(dl.count[more], 80);
    assert_eq!(dl.members[&(KEY_MORE | 1)].len(), 80);
    assert_eq!(dl.size[more], 50 * 10_000 + 30 * 500);
}

#[test]
fn aggregate_block_can_be_the_zoom_root() {
    let mut items = vec![("big.bin".to_owned(), S::F(10_000 * MB))];
    items.extend(
        files("f", 200, |i| (i as i64 + 1) * 1000)
            .into_iter()
            .map(|(n, s)| (n + ".bin", s)),
    );
    let store = build(&S::D(items));
    let dl = lay(&store, &LayoutParams::new(800.0, 600.0), &RootSpec::node(1));
    let members = dl.members[&(KEY_SMALL | 1)].clone();
    let root = RootSpec {
        node: 1,
        members: Some(Arc::new(members.clone())),
        ..RootSpec::default()
    };
    let zoomed = lay(
        &store,
        &params(800.0, 600.0, |p| p.min_element = 1.0),
        &root,
    );
    let set: HashSet<i32> = members.iter().map(|&m| m as i32).collect();
    assert!(zoomed.node.iter().all(|n| set.contains(n)));
    assert!(zoomed.len() > 150);
}

fn big_folder(n: usize, with_iso: bool) -> Store {
    let mut items = Vec::new();
    if with_iso {
        items.push(("big.iso".to_owned(), S::F(4 * GB)));
    }
    items.extend(
        files("f", n, |_| 4096)
            .into_iter()
            .map(|(n, s)| (n + ".bin", s)),
    );
    build(&dir([("Big", S::D(items))]))
}

#[test]
fn zooming_into_a_huge_small_items_block_pages_through_its_members() {
    for dpr in [1.0, 1.25, 2.0] {
        let store = big_folder(20_000, true);
        let big = find(&store.read(), "Big");
        let p = params(1200.0, 800.0, |p| p.dpr = dpr);
        let mut key = KEY_SMALL | big;
        let everything = lay(&store, &p, &RootSpec::node(1)).members[&key].clone();
        let mut members = everything.clone();
        assert_eq!(members.len(), 20_000);
        let thr = (14.0 * dpr).powi(2);
        let gap = ((2.0 * dpr).round_ties_even() as i32).max(1);
        let mut seen: Vec<i32> = Vec::new();
        let mut keys = vec![key];
        loop {
            let root = RootSpec {
                node: big,
                members: Some(Arc::new(members.clone())),
                key: Some(key),
                ..RootSpec::default()
            };
            let dl = lay(&store, &p, &root);
            let file_rows: Vec<usize> = (0..dl.len())
                .filter(|&i| dl.kind[i] == kind::FILE)
                .collect();
            assert!(file_rows.len() > 1000);
            for &i in &file_rows {
                assert!(((dl.w()[i] + gap) * (dl.h()[i] + gap)) as f64 >= 0.85 * thr);
            }
            seen.extend(file_rows.iter().map(|&i| dl.node[i]));
            let rest: Vec<usize> = (0..dl.len())
                .filter(|&i| dl.kind[i] != kind::FILE)
                .collect();
            if rest.is_empty() {
                break;
            }
            assert_eq!(rest.len(), 1);
            let row = rest[0];
            key = dl.key[row];
            assert_eq!(dl.kind[row], kind::MORE);
            assert_eq!(row, dl.len() - 1);
            assert_eq!(key >> 30, KEY_MORE >> 30);
            assert!(!keys.contains(&key));
            let share = ((dl.w()[row] + gap) * (dl.h()[row] + gap)) as f64
                / ((dl.width + gap) * (dl.height + gap)) as f64;
            assert!((share - PAGE_REST).abs() <= 0.01, "{share}");
            let inner = dl.members[&key].clone();
            assert_eq!(dl.count[row] as usize, inner.len());
            assert_eq!(inner.len(), members.len() - file_rows.len());
            assert_eq!(dl.size[row], 4096 * inner.len() as i64);
            let mut reversed = members.clone();
            reversed.reverse();
            let again = lay(
                &store,
                &p,
                &RootSpec {
                    node: big,
                    members: Some(Arc::new(reversed)),
                    key: Some(*keys.last().unwrap()),
                    ..RootSpec::default()
                },
            );
            assert_eq!(*again.key.last().unwrap(), key);
            keys.push(key);
            members = inner;
        }
        assert_eq!(keys.len(), 5);
        let mut all: Vec<i32> = everything.iter().map(|&m| m as i32).collect();
        all.sort_unstable();
        seen.sort_unstable();
        assert_eq!(seen, all);
    }
}

#[test]
fn the_leftover_never_takes_the_key_of_the_block_zoomed_into() {
    let store = big_folder(20_000, false);
    let big = find(&store.read(), "Big");
    let members: Vec<NodeId> = store.read().children(big).collect();
    let p = LayoutParams::new(1200.0, 800.0);
    let root = |key| RootSpec {
        node: big,
        members: Some(Arc::new(members.clone())),
        key,
        ..RootSpec::default()
    };
    let first = lay(&store, &p, &root(None));
    let taken = *first.key.last().unwrap();
    let clash = lay(&store, &p, &root(Some(taken)));
    let other = *clash.key.last().unwrap();
    assert!(other != taken && other >> 30 == KEY_MORE >> 30);
    assert_eq!(clash.members[&other], first.members[&taken]);
}

#[test]
fn free_and_unknown_space_sort_with_the_top_level() {
    let store = build(&dir([
        ("Users", dir([("a.bin", S::F(400 * MB))])),
        ("Windows", dir([("b.bin", S::F(100 * MB))])),
    ]));
    let root = RootSpec {
        node: 1,
        free: Some(200 * MB),
        unknown: Some(50 * MB),
        ..RootSpec::default()
    };
    let dl = lay(&store, &LayoutParams::new(900.0, 600.0), &root);
    let top: Vec<i64> = (0..dl.len())
        .filter(|&i| dl.parent[i] == -1)
        .map(|i| dl.size[i])
        .collect();
    let mut sorted = top.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(top, sorted);
    assert_eq!(dl.kind[dl.row(KEY_FREE).unwrap()], kind::FREE);
    assert_eq!(dl.kind[dl.row(KEY_UNKNOWN).unwrap()], kind::UNKNOWN);
    let none = lay(
        &store,
        &LayoutParams::new(900.0, 600.0),
        &RootSpec {
            unknown: Some(-5),
            ..root
        },
    );
    assert!(none.row(KEY_UNKNOWN).is_none());
}

#[test]
fn pending_folders_are_not_expanded() {
    let spec = dir([
        ("done", dir([("a.bin", S::F(300 * MB))])),
        ("todo", dir([("b.bin", S::F(1))])),
    ]);
    let store = build_spec(&spec, "/root", 1, &["todo"]);
    let r = store.read();
    let node = find(&r, "todo");
    let mut sizes: Vec<i64> = (0..r.len() as NodeId)
        .map(|i| effective_alloc(&r, i))
        .collect();
    sizes[node as usize] = 200 * MB;
    drop(r);
    let dl = lay_with(
        &store,
        &LayoutParams::new(800.0, 600.0),
        &RootSpec::node(1),
        Some(sizes),
    );
    let row = dl.name_list().iter().position(|&n| n == "todo").unwrap();
    assert_eq!(dl.kind[row], kind::PENDING);
}

#[test]
fn a_file_can_be_the_zoom_root() {
    let store = build(&dir([("one.iso", S::F(5 * MB))]));
    let dl = lay(&store, &LayoutParams::new(500.0, 300.0), &RootSpec::node(2));
    assert_eq!(dl.len(), 1);
    assert_eq!(
        (dl.x()[0], dl.y()[0], dl.w()[0], dl.h()[0]),
        (0, 0, 500, 300)
    );
}

#[test]
fn empty_folder_draws_nothing() {
    let store = build(&dir([("empty", dir::<String>([])), ("zero.bin", S::F(0))]));
    assert!(lay(&store, &LayoutParams::new(500.0, 300.0), &RootSpec::node(1)).is_empty());
}

#[test]
fn labels_follow_block_size() {
    let g = Geom::new(&LayoutParams::new(10.0, 10.0));
    assert_eq!(g.label(24, 22), 0);
    assert_eq!(g.label(48, 22), label::NAME);
    assert_eq!(g.label(66, 17), label::NAME | label::SHORT);
    assert_eq!(g.label(72, 46), label::NAME | label::SIZE);
    assert_eq!(g.label(303, 548), label::NAME | label::SIZE | label::BIG);
    let hi = Geom::new(&params(10.0, 10.0, |p| p.dpr = 2.0));
    assert_eq!(hi.label(48, 44), 0);
}

#[test]
fn gaps_scale_with_the_device_pixel_ratio() {
    let store = build(&dir([("a.bin", S::F(3 * MB)), ("b.bin", S::F(2 * MB))]));
    let dl = lay(
        &store,
        &params(400.0, 300.0, |p| p.dpr = 1.5),
        &RootSpec::node(1),
    );
    assert_eq!((dl.width, dl.height), (600, 450));
    let (ax, ay, aw, ah) = (dl.x()[0], dl.y()[0], dl.w()[0], dl.h()[0]);
    let (bx, by) = (dl.x()[1], dl.y()[1]);
    assert_eq!((bx - (ax + aw)).max(by - (ay + ah)), 3);
}

#[test]
fn layout_is_deterministic() {
    let spec = random_spec(&mut PyRandom::new(42), 0);
    let store = build_spec(&spec, "/root", 4096, &[]);
    let p = params(1280.0, 720.0, |p| p.dpr = 1.25);
    let a = lay(&store, &p, &RootSpec::node(1));
    let b = lay(&store, &p, &RootSpec::node(1));
    assert_eq!(a.key, b.key);
    assert_eq!(a.geo, b.geo);
    assert_eq!(
        (&a.kind, &a.label, &a.size, &a.count),
        (&b.kind, &b.label, &b.size, &b.count)
    );
    assert_eq!(a.names, b.names);
}

// --------------------------------------------------------------------------- the row contract

fn largest(store: &Store, want_dir: bool) -> NodeId {
    let r = store.read();
    let mut best = (i64::MIN, 0);
    for i in 2..r.len() as NodeId {
        if r.is_dir(i) == want_dir && effective_alloc(&r, i) > best.0 {
            best = (effective_alloc(&r, i), i);
        }
    }
    best.1
}

fn aggregate(draw: &DrawList, tag: Option<u32>) -> Option<RootSpec> {
    let row = (0..draw.len())
        .filter(|&r| matches!(draw.kind[r], kind::SMALL | kind::MORE))
        .filter(|&r| tag.is_none_or(|t| draw.key[r] & !KEY_NODE_MASK == t))
        .max_by_key(|&r| (draw.count[r], std::cmp::Reverse(draw.key[r])))?;
    let key = draw.key[row];
    Some(RootSpec {
        node: draw.node[row] as NodeId,
        members: Some(Arc::new(draw.members[&key].clone())),
        key: Some(key),
        ..RootSpec::default()
    })
}

fn run_engine(
    e: &dyn Engine,
    store: &Store,
    p: &LayoutParams,
    root: &RootSpec,
    sizes: Option<&dyn SizeSource>,
) -> DrawList {
    let r = store.read();
    let own = StoreSizes::of(&r);
    let src = sizes.unwrap_or(&own);
    let mut out = DrawList::new();
    e.run(&r, &Basis::default(), p, root, src, &mut out);
    let problems = check_draw(&out, &r, root, true, Some(src));
    assert!(problems.is_empty(), "{root:?}: {problems:#?}");
    out
}

#[test]
fn the_row_contract_holds_for_every_visualization() {
    let disk = golden_tree(20_000, 7, 0);
    let pending = golden_tree(20_000, 7, 3);
    let hide = ArraySizes(Arc::new(hide_sizes(&disk.read())));
    let fhd = LayoutParams::new(1920.0, 1080.0);
    let uhd = params(2560.0, 1440.0, |p| {
        p.dpr = 1.5;
        p.detail = 15
    });
    let small_view = params(640.0, 400.0, |p| {
        p.dpr = 1.25;
        p.detail = 8;
        p.sort_by_size = false
    });
    let tiny = LayoutParams::new(30.0, 30.0);
    for vis in registry::all_visuals() {
        let e = &*vis.engine;
        let volume = if vis.free_space {
            RootSpec {
                node: 1,
                free: Some(2 << 30),
                unknown: Some(500 << 20),
                ..RootSpec::default()
            }
        } else {
            RootSpec::node(1)
        };
        let dl = run_engine(e, &disk, &fhd, &RootSpec::node(1), None);
        assert_eq!(dl.layout, vis.layout);
        run_engine(e, &disk, &fhd, &volume, None);
        run_engine(e, &disk, &small_view, &volume, None);
        run_engine(e, &disk, &uhd, &RootSpec::node(largest(&disk, true)), None);
        run_engine(e, &disk, &fhd, &RootSpec::node(largest(&disk, false)), None);
        run_engine(e, &disk, &fhd, &RootSpec::node(1), Some(&hide));
        let p = run_engine(e, &pending, &uhd, &RootSpec::node(1), None);
        assert!(p.kind.contains(&kind::PENDING), "{}", vis.id);
        run_engine(e, &disk, &tiny, &volume, None);
        // aggregate zooms, and the "+N items" inside them
        let first = run_engine(e, &disk, &fhd, &RootSpec::node(1), None);
        for tag in [KEY_SMALL, KEY_MORE] {
            let Some(zoom) = aggregate(&first, Some(tag)) else {
                continue;
            };
            let inner = run_engine(e, &disk, &fhd, &zoom, None);
            assert!(
                (0..inner.len()).any(|i| inner.kind[i] <= kind::PENDING && inner.depth[i] == 1)
            );
            if let Some(left) = aggregate(&inner, None) {
                run_engine(e, &disk, &fhd, &left, None);
            }
        }
        // levels never exceed the detail
        for p in [
            &fhd,
            &uhd,
            &small_view,
            &params(120.0, 90.0, |p| p.detail = 15),
        ] {
            let levels = e.levels(p);
            assert!(levels <= p.detail);
            assert_eq!(
                run_engine(e, &disk, p, &RootSpec::node(1), None).levels,
                levels
            );
        }
    }
}
