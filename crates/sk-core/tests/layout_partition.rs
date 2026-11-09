//! Partition engine and its projections (Python `test_layout_partition.py`,
//! `test_layout_flame.py`, `test_layout_flame_polar.py`): merges, aggregate zoom, scans, the row
//! budget; bands, bar widths, origin, labels; rings, angles, thresholds.

mod view_support;

use std::f64::consts::PI;
use std::sync::Arc;

use sk_core::layout::contract::check_draw;
use sk_core::layout::flame::{FlameProjection, GAP, ROW_MAX, ROW_MIN, flame_engine};
use sk_core::layout::flame_polar::{
    FlamePolarProjection, HOLE_MAX_RATIO, HOLE_MIN, HOLE_RATIO, MARGIN, RING_MIN, UNIT,
    flame_polar_engine,
};
use sk_core::layout::partition::{Cell, PartitionEngine, Projection};
use sk_core::layout::{
    ArraySizes, Basis, DrawList, Engine, KEY_FREE, KEY_MORE, KEY_NODE_MASK, KEY_SMALL, KEY_UNKNOWN,
    LayoutParams, OptValue, RootSpec, SizeSource, StoreSizes, effective_alloc, kind, label,
    label_box, registry,
};
use sk_protocol::view::{FLAME_LAYOUT, FlameGeometry, FlameOrigin, FrameGeometry, PixelRect};
use sk_protocol::{CAP_LAYOUT_FLAME, CAP_LAYOUT_POLAR};
use sk_store::{Col, NodeId, Store};
use view_support::{PyRandom, S, build, build_spec, dir, find, golden_tree, hide_sizes};

const MB: i64 = 1 << 20;
const GB: i64 = 1 << 30;
const BAND: i32 = 24;
const U: i32 = UNIT as i32;

fn pr(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Level k is the band [k·24, k·24 + 24); [a, b) is [round(a·W), round(b·W)) along x.
struct Line {
    width: i64,
    levels: u32,
    min_expand: f64,
}

impl Projection for Line {
    fn levels(&self) -> u32 {
        self.levels
    }
    fn unit_px(&self, _: u32) -> f64 {
        self.width as f64
    }
    fn cell(&self, a: f64, b: f64, level: u32) -> Option<Cell> {
        let x = pr(a * self.width as f64);
        let w = pr(b * self.width as f64) - x;
        (w >= 1).then_some([x as i32, level as i32 * BAND, w as i32, BAND])
    }
    fn expandable(&self, cell: &Cell, _: u32) -> bool {
        cell[2] as f64 >= self.min_expand
    }
    fn label(&self, cell: &Cell, _: u8) -> u8 {
        label_box(cell[2] as f64, cell[3] as f64, false)
    }
    fn meta(&self) -> FrameGeometry {
        FrameGeometry::Flame(FlameGeometry {
            kind: Default::default(),
            origin: FlameOrigin::Top,
            row_h: BAND,
            rows: self.levels as i32,
            gap: 0,
            root: PixelRect {
                x: 0,
                y: 0,
                w: self.width as i32,
                h: BAND,
            },
        })
    }
}

fn make_line(p: &LayoutParams) -> Box<dyn Projection> {
    Box::new(Line {
        width: pr(p.width * p.dpr),
        levels: (p.detail as i64)
            .min(pr(p.height * p.dpr).div_euclid(BAND as i64) - 1)
            .max(0) as u32,
        min_expand: p.min_element * p.dpr,
    })
}

fn line() -> PartitionEngine {
    PartitionEngine::new(make_line, FLAME_LAYOUT, "test.line")
}

fn params(w: f64, h: f64, f: impl FnOnce(&mut LayoutParams)) -> LayoutParams {
    let mut p = LayoutParams::new(w, h);
    f(&mut p);
    p
}

fn go(
    e: &dyn Engine,
    store: &Store,
    p: &LayoutParams,
    root: &RootSpec,
    sizes: Option<Vec<i64>>,
) -> DrawList {
    let r = store.read();
    let own = StoreSizes::of(&r);
    let arr = sizes.map(|s| ArraySizes(Arc::new(s)));
    let src: &dyn SizeSource = match &arr {
        Some(a) => a,
        None => &own,
    };
    let mut out = DrawList::new();
    e.run(&r, &Basis::default(), p, root, src, &mut out);
    let problems = check_draw(&out, &r, root, true, Some(src));
    assert!(problems.is_empty(), "{problems:#?}");
    out
}

/// The line engine on 1000 × 240 px.
fn run(store: &Store, f: impl FnOnce(&mut LayoutParams)) -> DrawList {
    go(
        &line(),
        store,
        &params(1000.0, 240.0, f),
        &RootSpec::node(1),
        None,
    )
}

fn top(dl: &DrawList) -> Vec<usize> {
    (0..dl.len()).filter(|&i| dl.parent[i] == -1).collect()
}

fn under(dl: &DrawList, row: i32) -> Vec<usize> {
    (0..dl.len()).filter(|&i| dl.parent[i] == row).collect()
}

fn named(prefix: &str, n: usize, ext: &str, size: impl Fn(usize) -> i64) -> Vec<(String, S)> {
    (0..n)
        .map(|i| (format!("{prefix}{i}{ext}"), S::F(size(i))))
        .collect()
}

fn random_spec(rng: &mut PyRandom, depth: u32) -> S {
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

fn random_store(seed: u64) -> Store {
    build_spec(
        &random_spec(&mut PyRandom::new(seed), 0),
        "/root",
        4096,
        &[],
    )
}

// --------------------------------------------------------------------------- partition: merges

#[test]
fn small_files_merge_into_one_block() {
    let mut items = vec![("movie.mp4".to_owned(), S::F(900 * MB))];
    items.extend(named("p", 300, ".jpg", |_| 60_000));
    let store = build(&S::D(items));
    let dl = run(&store, |_| {});
    let row = dl.row(KEY_SMALL | 1).unwrap();
    assert_eq!(dl.kind[row], kind::SMALL);
    assert_eq!((dl.count[row], dl.size[row]), (300, 300 * 60_000));
    assert_eq!(dl.members[&(KEY_SMALL | 1)].len(), 300);
    assert_eq!(dl.name_list()[row], "");
}

#[test]
fn a_single_small_file_is_not_a_block_of_its_own_kind() {
    let store = build(&dir([
        ("movie.mp4", S::F(900 * MB)),
        ("note.txt", S::F(10_000)),
        ("pic.jpg", S::F(20 * MB)),
    ]));
    let dl = run(&store, |_| {});
    assert!(dl.row(KEY_SMALL | 1).is_none() && dl.row(KEY_MORE | 1).is_none());
    assert!(dl.kind.iter().all(|&k| k == kind::FILE));
}

#[test]
fn tiny_siblings_merge_into_plus_n_placed_last() {
    let mut items = vec![("big.bin".to_owned(), S::F(100 * MB))];
    items.extend((0..50).map(|i| (format!("dir{i}"), dir([("x.bin", S::F(10_000))]))));
    items.extend(named("f", 3, ".bin", |_| 5 * MB));
    let store = build(&S::D(items));
    for by_size in [true, false] {
        let dl = run(&store, |p| p.sort_by_size = by_size);
        let more = dl.row(KEY_MORE | 1).unwrap();
        let t = top(&dl);
        assert_eq!(more, *t.last().unwrap());
        assert_eq!(dl.x()[more], t.iter().map(|&i| dl.x()[i]).max().unwrap());
        assert_eq!((dl.count[more], dl.size[more]), (50, 50 * 10_000));
    }
}

#[test]
fn sorting_by_name_runs_left_to_right() {
    let store = build(&dir([
        ("b.bin", S::F(MB)),
        ("C.bin", S::F(3 * MB)),
        ("a.bin", S::F(2 * MB)),
    ]));
    let by_name = run(&store, |p| p.sort_by_size = false);
    assert_eq!(by_name.name_list(), ["a.bin", "b.bin", "C.bin"]);
    assert!(by_name.x().windows(2).all(|w| w[1] > w[0]));
    assert_eq!(run(&store, |_| {}).name_list(), ["C.bin", "a.bin", "b.bin"]);
}

#[test]
fn free_and_unknown_space_join_the_first_level() {
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
    let dl = go(
        &line(),
        &store,
        &LayoutParams::new(1000.0, 240.0),
        &root,
        None,
    );
    let (free, unknown) = (dl.row(KEY_FREE).unwrap(), dl.row(KEY_UNKNOWN).unwrap());
    assert_eq!((dl.depth[free], dl.depth[unknown]), (1, 1));
    assert_eq!(
        (dl.kind[free], dl.kind[unknown]),
        (kind::FREE, kind::UNKNOWN)
    );
    let sizes: Vec<i64> = top(&dl).iter().map(|&i| dl.size[i]).collect();
    assert!(sizes.windows(2).all(|w| w[0] >= w[1]));
    let neg = RootSpec {
        unknown: Some(-5),
        ..root
    };
    assert!(
        go(
            &line(),
            &store,
            &LayoutParams::new(1000.0, 240.0),
            &neg,
            None
        )
        .row(KEY_UNKNOWN)
        .is_none()
    );
}

// --------------------------------------------------------------------------- partition: zoom roots

#[test]
fn an_aggregate_zoom_always_shows_members_of_its_own() {
    let mut items = vec![("big.iso".to_owned(), S::F(4 * GB))];
    items.extend(named("f", 20_000, ".bin", |_| 4096));
    let store = build(&dir([("Big", S::D(items))]));
    let big = find(&store.read(), "Big");
    let mut key = KEY_SMALL | big;
    let mut members = run(&store, |_| {}).members[&key].clone();
    assert_eq!(members.len(), 20_000);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..3 {
        let root = RootSpec {
            node: big,
            members: Some(Arc::new(members.clone())),
            key: Some(key),
            ..RootSpec::default()
        };
        let dl = go(
            &line(),
            &store,
            &LayoutParams::new(1000.0, 240.0),
            &root,
            None,
        );
        let files: Vec<usize> = (0..dl.len())
            .filter(|&i| dl.kind[i] == kind::FILE)
            .collect();
        assert!(!files.is_empty());
        for &i in &files {
            assert!(members.contains(&(dl.node[i] as NodeId)) && dl.w()[i] >= 13);
            assert!(seen.insert(dl.node[i]));
        }
        let more = *top(&dl).last().unwrap();
        assert_eq!(dl.kind[more], kind::MORE);
        assert_eq!(more, dl.len() - 1);
        key = dl.key[more];
        members = dl.members[&key].clone();
        assert_eq!(dl.count[more] as usize, members.len());
        assert_eq!(dl.size[more], 4096 * members.len() as i64);
    }
}

#[test]
fn a_file_root_or_no_room_draws_nothing() {
    let store = build(&dir([("one.iso", S::F(5 * MB))]));
    let dl = go(
        &line(),
        &store,
        &LayoutParams::new(500.0, 300.0),
        &RootSpec::node(2),
        None,
    );
    assert!(dl.is_empty() && dl.levels == 5 && matches!(dl.meta, Some(FrameGeometry::Flame(_))));
    let dl = go(
        &line(),
        &store,
        &LayoutParams::new(500.0, 30.0),
        &RootSpec::node(1),
        None,
    );
    assert!(dl.is_empty() && dl.levels == 0);
}

#[test]
fn zero_sized_children_are_left_out() {
    let store = build(&dir([
        ("empty", dir::<String>([])),
        ("zero.bin", S::F(0)),
        ("a.bin", S::F(MB)),
    ]));
    assert_eq!(run(&store, |_| {}).name_list(), ["a.bin"]);
    let store = build(&dir([("empty", dir::<String>([])), ("zero.bin", S::F(0))]));
    assert!(run(&store, |_| {}).is_empty());
}

// --------------------------------------------------------------------------- partition: scans

#[test]
fn pending_folders_are_not_expanded() {
    let spec = dir([
        ("done", dir([("a.bin", S::F(300 * MB))])),
        ("todo", dir([("b.bin", S::F(1))])),
    ]);
    let store = build_spec(&spec, "/root", 1, &["todo"]);
    let mut sizes: Vec<i64> = {
        let r = store.read();
        (0..r.len() as NodeId)
            .map(|i| effective_alloc(&r, i))
            .collect()
    };
    let todo = find(&store.read(), "todo");
    sizes[todo as usize] = 200 * MB;
    let dl = go(
        &line(),
        &store,
        &LayoutParams::new(1000.0, 240.0),
        &RootSpec::node(1),
        Some(sizes),
    );
    let names = dl.name_list();
    let row = names.iter().position(|&n| n == "todo").unwrap();
    assert_eq!(dl.kind[row], kind::PENDING);
    assert!(under(&dl, row as i32).is_empty());
    let done = names.iter().position(|&n| n == "done").unwrap();
    assert_eq!(dl.kind[done], kind::FOLDER);
}

#[test]
fn a_folder_being_listed_reads_as_empty() {
    let store = build(&dir([
        (
            "A",
            dir([("x.bin", S::F(300 * MB)), ("y.bin", S::F(100 * MB))]),
        ),
        ("b.bin", S::F(50 * MB)),
    ]));
    let a = find(&store.read(), "A");
    {
        let mut w = store.write();
        w.set_raw(Col::ChildStart, a, 0i32);
        w.touch();
    }
    let dl = run(&store, |_| {});
    let names = dl.name_list();
    let row = names.iter().position(|&n| n == "A").unwrap();
    assert!(under(&dl, row as i32).is_empty());
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(sorted, ["A", "b.bin"]);
}

#[test]
fn nodes_added_after_the_run_started_are_not_read() {
    let store = build(&dir([
        (
            "A",
            dir([("x.bin", S::F(300 * MB)), ("y.bin", S::F(100 * MB))]),
        ),
        ("b.bin", S::F(50 * MB)),
    ]));
    let r = store.read();
    let n = r.len() - 1;
    let sizes: Vec<i64> = (0..n as NodeId).map(|i| effective_alloc(&r, i)).collect();
    drop(r);
    let dl = go(
        &line(),
        &store,
        &LayoutParams::new(1000.0, 240.0),
        &RootSpec::node(1),
        Some(sizes),
    );
    assert!(dl.node.iter().all(|&x| (x as usize) < n));
}

// --------------------------------------------------------------------------- partition: budget

#[test]
fn the_row_budget_collapses_folders_instead_of_dropping_rows() {
    let files = || {
        let mut v = named("f", 6, ".bin", |i| (10 + i as i64) * MB);
        v.extend(named("s", 10, ".txt", |_| 1000));
        S::D(v)
    };
    let spec = S::D(
        (0..6)
            .map(|i| {
                (
                    format!("d{i}"),
                    S::D((0..6).map(|j| (format!("e{j}"), files())).collect()),
                )
            })
            .collect(),
    );
    let store = build(&spec);
    let p = params(1000.0, 240.0, |p| {
        p.detail = 8;
        p.min_element = 3.0
    });
    let full = go(&line(), &store, &p, &RootSpec::node(1), None);
    assert_eq!(full.len(), 6 + 36 + 216);
    assert_eq!(
        full.members
            .keys()
            .filter(|&&k| k & !KEY_NODE_MASK == KEY_SMALL)
            .count(),
        36
    );
    let mut small = line();
    small.max_rows = 40;
    let dl = go(&small, &store, &p, &RootSpec::node(1), None);
    assert!(dl.len() <= 40);
    assert!(dl.key.iter().all(|k| full.key.contains(k)));
    let cut: Vec<i32> = (0..dl.len())
        .filter(|&i| dl.kind[i] == kind::COLLAPSED)
        .map(|i| dl.node[i])
        .filter(|&nd| {
            let r = full.node.iter().position(|&x| x == nd).unwrap();
            full.kind[r] == kind::FOLDER
        })
        .collect();
    assert!(!cut.is_empty());
    assert!(dl.members.keys().all(|k| full.members.contains_key(k)));
    for nd in cut {
        assert!(!dl.members.contains_key(&(KEY_SMALL | nd as u32)));
        assert!(!dl.members.contains_key(&(KEY_MORE | nd as u32)));
    }
}

// --------------------------------------------------------------------------- partition: invariants

#[test]
fn random_trees_keep_the_contract() {
    for seed in 0..6 {
        for dpr in [1.0, 1.25, 2.0] {
            let store = random_store(seed);
            let dl = go(
                &line(),
                &store,
                &params(900.0, 240.0 / dpr, |p| {
                    p.dpr = dpr;
                    p.detail = 8;
                    p.min_element = 4.0
                }),
                &RootSpec::node(1),
                None,
            );
            assert!(!dl.is_empty());
            assert_eq!((dl.layout, dl.visual.as_str()), (FLAME_LAYOUT, "test.line"));
        }
    }
}

fn shares_spec() -> S {
    dir([
        (
            "A",
            dir([
                ("a1.bin", S::F(300 * MB)),
                ("a2.bin", S::F(100 * MB)),
                (
                    "S",
                    dir([("s.bin", S::F(50 * MB)), ("t.bin", S::F(50 * MB))]),
                ),
            ]),
        ),
        (
            "B",
            dir([("b1.bin", S::F(200 * MB)), ("b2.bin", S::F(200 * MB))]),
        ),
        ("c.bin", S::F(100 * MB)),
    ])
}

#[test]
fn children_share_out_their_parent_exactly() {
    let store = build(&shares_spec());
    let dl = run(&store, |p| p.min_element = 1.0);
    let mut containers = vec![-1i32];
    containers.extend(
        (0..dl.len())
            .filter(|&i| dl.kind[i] == kind::FOLDER)
            .map(|i| i as i32),
    );
    for row in containers {
        let kids = under(&dl, row);
        let (x0, x1) = if row < 0 {
            (0, dl.width)
        } else {
            let r = row as usize;
            (dl.x()[r], dl.x()[r] + dl.w()[r])
        };
        let size = if row < 0 {
            store.read().tot_alloc(1)
        } else {
            dl.size[row as usize]
        };
        assert_eq!(kids.iter().map(|&k| dl.size[k]).sum::<i64>(), size);
        let last = *kids.last().unwrap();
        assert_eq!((dl.x()[kids[0]], dl.x()[last] + dl.w()[last]), (x0, x1));
        for w in kids.windows(2) {
            assert_eq!(dl.x()[w[1]], dl.x()[w[0]] + dl.w()[w[0]]);
        }
    }
}

#[test]
fn a_folder_with_bytes_of_its_own_leaves_them_blank() {
    let store = build(&dir([
        (
            "A",
            dir([("x.bin", S::F(100 * MB)), ("y.bin", S::F(100 * MB))]),
        ),
        ("b.bin", S::F(200 * MB)),
    ]));
    let a = find(&store.read(), "A");
    {
        let mut w = store.write();
        let (ta, t1) = (w.tot_alloc(a), w.tot_alloc(1));
        w.set_raw(Col::TotAlloc, a, ta + 200 * MB);
        w.set_raw(Col::TotAlloc, 1, t1 + 200 * MB);
        w.touch();
    }
    let dl = run(&store, |p| p.min_element = 1.0);
    let row = dl.name_list().iter().position(|&n| n == "A").unwrap();
    let kids = under(&dl, row as i32);
    let last = *kids.last().unwrap();
    assert_eq!(dl.w()[row], 667);
    assert_eq!(dl.x()[last] + dl.w()[last], dl.x()[row] + 333);
}

#[test]
fn levels_follow_the_projection() {
    let e = line();
    assert_eq!(e.levels(&params(100.0, 240.0, |p| p.detail = 5)), 5);
    assert_eq!(e.levels(&params(100.0, 80.0, |p| p.detail = 5)), 2);
}

#[test]
fn both_projections_keep_the_contract_on_a_disk_like_tree() {
    let store = golden_tree(20_000, 7, 0);
    let scanning = golden_tree(20_000, 7, 3);
    let hide = hide_sizes(&store.read());
    let used = store.read().tot_alloc(1);
    for e in [flame_engine(), flame_polar_engine()] {
        for (w, h, dpr, detail) in [
            (1920.0, 1080.0, 1.0, 5),
            (3840.0, 2160.0, 1.0, 15),
            (1280.0, 720.0, 1.25, 8),
            (1280.0, 720.0, 1.5, 8),
        ] {
            let p = params(w, h, |p| {
                p.dpr = dpr;
                p.detail = detail
            });
            let mut roots = vec![
                RootSpec::node(1),
                RootSpec {
                    node: 1,
                    free: Some(used / 3),
                    unknown: Some(used / 50),
                    ..RootSpec::default()
                },
            ];
            let first = go(&e, &store, &p, &roots[0], None);
            let mut drawn: Vec<u32> = first
                .members
                .keys()
                .copied()
                .filter(|&k| first.row(k).is_some())
                .collect();
            drawn.sort_unstable();
            assert!(!drawn.is_empty());
            for &k in drawn.iter().take(4) {
                roots.push(RootSpec {
                    node: k & KEY_NODE_MASK,
                    members: Some(Arc::new(first.members[&k].clone())),
                    key: Some(k),
                    ..RootSpec::default()
                });
            }
            for root in &roots {
                for by_size in [true, false] {
                    let mut q = p.clone();
                    q.sort_by_size = by_size;
                    assert!(!go(&e, &store, &q, root, None).is_empty());
                }
            }
            assert!(!go(&e, &store, &p, &roots[0], Some(hide.clone())).is_empty());
            let dl = go(&e, &scanning, &p, &roots[0], None);
            assert!(dl.kind.contains(&kind::PENDING));
        }
    }
}

// --------------------------------------------------------------------------- flame graph

fn flame(store: &Store, f: impl FnOnce(&mut LayoutParams)) -> DrawList {
    go(
        &flame_engine(),
        store,
        &params(1200.0, 800.0, f),
        &RootSpec::node(1),
        None,
    )
}

fn fmeta(dl: &DrawList) -> &FlameGeometry {
    match &dl.meta {
        Some(FrameGeometry::Flame(m)) => m,
        other => panic!("{other:?}"),
    }
}

fn band_y(dl: &DrawList, k: i32) -> i32 {
    let m = fmeta(dl);
    match m.origin {
        FlameOrigin::Top => k * m.row_h,
        FlameOrigin::Bottom => dl.height - k * m.row_h - (m.row_h - m.gap),
    }
}

#[test]
fn flame_levels_and_band_height_stay_within_their_bounds() {
    for (height, dpr, font, detail) in [
        (800.0, 1.0, 1.0, 5),
        (800.0, 1.0, 1.0, 15),
        (2160.0, 1.0, 1.0, 15),
        (300.0, 1.0, 1.0, 15),
        (500.0, 1.5, 1.4, 15),
        (720.0, 1.25, 0.8, 8),
        (60.0, 1.0, 1.0, 5),
        (39.0, 1.0, 1.0, 5),
        (10.0, 2.0, 1.0, 5),
    ] {
        let p = params(1000.0, height, |p| {
            p.dpr = dpr;
            p.font_scale = font;
            p.detail = detail
        });
        let proj = FlameProjection::new(&p);
        let FrameGeometry::Flame(m) = proj.meta() else {
            panic!()
        };
        let lu = dpr * font;
        let hh = pr(height * dpr) as i32;
        let (row_min, row_max) = (
            pr(ROW_MIN as f64 * lu) as i32,
            pr(ROW_MAX as f64 * lu) as i32,
        );
        assert_eq!(m.rows, (detail as i32).min(hh / row_min - 1).max(0));
        assert_eq!(proj.levels as i32, m.rows);
        assert_eq!(m.gap, pr(GAP as f64 * dpr).max(1) as i32);
        if m.rows > 0 {
            assert!(row_min <= m.row_h && m.row_h <= row_max);
            assert!((m.rows + 1) * m.row_h <= hh);
        }
        assert_eq!(flame_engine().levels(&p) as i32, m.rows);
    }
}

#[test]
fn flame_root_bar_and_origin() {
    let store = build(&dir([("a.bin", S::F(MB))]));
    for origin in ["top", "bottom"] {
        let dl = flame(&store, |p| {
            p.options = vec![("origin".into(), OptValue::Str(origin.into()))]
        });
        let m = fmeta(&dl);
        assert_eq!(
            (m.root.x, m.root.y, m.root.w, m.root.h),
            (0, band_y(&dl, 0), 1200, m.row_h - m.gap)
        );
    }
    assert_eq!(fmeta(&flame(&store, |_| {})).origin, FlameOrigin::Bottom);
    let odd = params(500.0, 400.0, |p| {
        p.options = vec![("origin".into(), OptValue::Num(3.0))]
    });
    assert_eq!(FlameProjection::new(&odd).origin, FlameOrigin::Bottom);
}

#[test]
fn flame_bar_widths_follow_sizes() {
    let sizes = [400, 300, 120, 90, 50, 25, 15];
    let store = build(&S::D(
        sizes
            .iter()
            .enumerate()
            .map(|(i, s)| (format!("f{i}.bin"), S::F(s * MB)))
            .collect(),
    ));
    let dl = flame(&store, |p| p.min_element = 1.0);
    let g = fmeta(&dl).gap;
    let total: i64 = dl.size.iter().sum();
    for i in 0..dl.len() {
        let want = dl.size[i] as f64 / total as f64 * (dl.width + g) as f64;
        assert!(((dl.w()[i] + g) as f64 - want).abs() <= 1.0);
    }
    let last = dl.len() - 1;
    assert_eq!((dl.x()[0], dl.x()[last] + dl.w()[last]), (0, dl.width));
}

#[test]
fn flame_children_sit_in_the_next_band_within_their_parent() {
    for seed in 0..6 {
        for dpr in [1.0, 1.25, 1.5, 2.0] {
            let store = random_store(seed);
            let dl = go(
                &flame_engine(),
                &store,
                &params(900.0, 640.0, |p| {
                    p.dpr = dpr;
                    p.detail = 8;
                    p.min_element = 4.0
                }),
                &RootSpec::node(1),
                None,
            );
            assert!(!dl.is_empty());
            let m = fmeta(&dl);
            let g = m.gap;
            for i in 0..dl.len() {
                assert_eq!(dl.y()[i], band_y(&dl, dl.depth[i] as i32));
                assert_eq!(dl.h()[i], m.row_h - g);
                let p = dl.parent[i];
                if p >= 0 {
                    let p = p as usize;
                    assert_eq!(dl.depth[i], dl.depth[p] + 1);
                    assert!(dl.x()[p] <= dl.x()[i] && dl.x()[i] < dl.x()[p] + dl.w()[p] + g);
                    assert!(dl.x()[i] + dl.w()[i] <= dl.x()[p] + dl.w()[p]);
                }
                assert!(dl.w()[i] as i64 >= pr(2.0 * dpr));
            }
        }
    }
}

#[test]
fn flame_siblings_meet_at_one_gap() {
    let store = build(&shares_spec());
    let dl = flame(&store, |p| {
        p.min_element = 1.0;
        p.dpr = 1.5
    });
    let g = fmeta(&dl).gap;
    let mut containers = vec![-1i32];
    containers.extend(
        (0..dl.len())
            .filter(|&i| dl.kind[i] == kind::FOLDER)
            .map(|i| i as i32),
    );
    for row in containers {
        let kids = under(&dl, row);
        let (x0, x1) = if row < 0 {
            (0, dl.width)
        } else {
            let r = row as usize;
            (dl.x()[r], dl.x()[r] + dl.w()[r])
        };
        let last = *kids.last().unwrap();
        assert_eq!((dl.x()[kids[0]], dl.x()[last] + dl.w()[last]), (x0, x1));
        for w in kids.windows(2) {
            assert_eq!(dl.x()[w[1]], dl.x()[w[0]] + dl.w()[w[0]] + g);
        }
    }
}

#[test]
fn flame_merge_threshold_is_the_minimum_bar_width() {
    for (n, merged) in [(100, false), (300, true)] {
        let store = build(&S::D(named("f", n, ".bin", |_| MB)));
        let dl = flame(&store, |p| p.min_element = 10.0);
        assert_eq!(dl.row(KEY_SMALL | 1).is_some(), merged);
        let g = fmeta(&dl).gap;
        for i in 0..dl.len() {
            if dl.kind[i] == kind::FILE {
                assert!(dl.w()[i] + g >= 9);
            }
        }
    }
    let store = build(&dir([
        ("A", S::D(named("f", 100, ".bin", |_| MB))),
        ("B", S::D(named("f", 100, ".bin", |_| MB))),
    ]));
    let dl = flame(&store, |p| p.min_element = 10.0);
    for name in ["A", "B"] {
        let node = find(&store.read(), name);
        let row = dl.node.iter().position(|&x| x == node as i32).unwrap();
        let small = dl.row(KEY_SMALL | node).unwrap();
        assert_eq!(dl.w()[row], 599);
        assert_eq!(dl.parent[small], row as i32);
    }
}

#[test]
fn flame_root_on_top_and_at_the_bottom_mirror_each_other() {
    for dpr in [1.0, 1.25, 1.5] {
        let store = random_store(5);
        let with = |origin: &str| {
            flame(&store, |p| {
                p.dpr = dpr;
                p.detail = 8;
                p.min_element = 4.0;
                p.options = vec![("origin".into(), OptValue::Str(origin.into()))];
            })
        };
        let (t, b) = (with("top"), with("bottom"));
        assert_eq!(t.key, b.key);
        assert_eq!((t.x(), t.w(), t.h()), (b.x(), b.w(), b.h()));
        for i in 0..t.len() {
            assert_eq!(b.y()[i], t.height - t.y()[i] - t.h()[i]);
        }
    }
}

#[test]
fn flame_labels_follow_label_box_without_the_large_type() {
    for (dpr, font) in [(1.0, 1.0), (1.25, 1.0), (2.0, 1.4), (1.0, 0.8)] {
        let store = random_store(7);
        let dl = flame(&store, |p| {
            p.dpr = dpr;
            p.font_scale = font;
            p.detail = 8;
            p.min_element = 4.0
        });
        let lu = dpr * font;
        let want: Vec<u8> = (0..dl.len())
            .map(|i| label_box(dl.w()[i] as f64 / lu, dl.h()[i] as f64 / lu, false))
            .collect();
        assert_eq!(dl.label, want);
        assert!(want.iter().any(|&b| b != 0) && want.iter().all(|&b| b & label::BIG == 0));
    }
}

// --------------------------------------------------------------------------- polar flame graph

fn polar(store: &Store, root: RootSpec, f: impl FnOnce(&mut LayoutParams)) -> DrawList {
    go(
        &flame_polar_engine(),
        store,
        &params(1920.0, 1080.0, f),
        &root,
        None,
    )
}

fn arc(dl: &DrawList, i: usize) -> f64 {
    let g = &dl.geo;
    (g[1][i] - g[0][i]) as f64 / U as f64 * 2.0 * PI * (g[2][i] + g[3][i]) as f64 / 2.0
}

#[test]
fn polar_rings_and_centre_disc_follow_the_formula() {
    for (w, h, dpr, font, detail) in [
        (1920.0, 1080.0, 1.0, 1.0, 5),
        (3840.0, 2160.0, 1.0, 1.0, 15),
        (1280.0, 720.0, 1.25, 1.0, 8),
        (1280.0, 720.0, 1.5, 1.4, 15),
        (800.0, 600.0, 2.0, 0.8, 15),
        (300.0, 200.0, 1.0, 1.0, 15),
        (120.0, 90.0, 1.0, 1.0, 5),
        (10.0, 10.0, 1.0, 1.0, 5),
    ] {
        let p = params(w, h, |p| {
            p.dpr = dpr;
            p.font_scale = font;
            p.detail = detail
        });
        let FrameGeometry::Polar(m) = FlamePolarProjection::new(&p).meta() else {
            panic!()
        };
        let lu = dpr * font;
        let (ww, hh) = (pr(w * dpr), pr(h * dpr));
        let r = (ww.min(hh) / 2 - pr(MARGIN as f64 * dpr)).max(0);
        let hole = pr(HOLE_RATIO * r as f64)
            .max(pr(HOLE_MIN as f64 * lu))
            .min(pr(HOLE_MAX_RATIO * r as f64));
        let n = (detail as i64)
            .min((r - hole).div_euclid(pr(RING_MIN as f64 * lu)))
            .max(0);
        assert_eq!(
            (
                m.cx as i64,
                m.cy as i64,
                m.hole as i64,
                m.rings as i64,
                m.unit
            ),
            (ww / 2, hh / 2, hole, n, UNIT)
        );
        if n > 0 {
            assert_eq!(m.ring_w as i64, (r - hole) / n);
            assert!(m.radius as i64 <= r);
        } else {
            assert_eq!(m.radius as i64, hole);
        }
        assert_eq!(flame_polar_engine().levels(&p) as i64, n);
    }
}

#[test]
fn polar_sectors_start_at_twelve_o_clock_largest_first() {
    let store = build(&dir([
        ("b.bin", S::F(MB)),
        ("C.bin", S::F(3 * MB)),
        ("a.bin", S::F(2 * MB)),
        ("d.bin", S::F(4 * MB)),
    ]));
    let dl = polar(&store, RootSpec::node(1), |p| p.min_element = 1.0);
    let (a0, a1) = (&dl.geo[0], &dl.geo[1]);
    assert_eq!((a0[0], *a1.last().unwrap()), (0, U));
    assert_eq!(dl.name_list(), ["d.bin", "C.bin", "a.bin", "b.bin"]);
    assert_eq!(a0[1..], a1[..a1.len() - 1]);
    let by_name = polar(&store, RootSpec::node(1), |p| {
        p.min_element = 1.0;
        p.sort_by_size = false
    });
    assert_eq!(by_name.name_list(), ["a.bin", "b.bin", "C.bin", "d.bin"]);
}

#[test]
fn polar_a_single_child_is_a_full_turn() {
    let store = build(&dir([(
        "A",
        dir([("B", dir([("x.bin", S::F(3 * MB)), ("y.bin", S::F(MB))]))]),
    )]));
    let dl = polar(&store, RootSpec::node(1), |p| p.min_element = 1.0);
    for name in ["A", "A/B"] {
        let node = find(&store.read(), name);
        let row = dl.node.iter().position(|&x| x == node as i32).unwrap();
        assert_eq!((dl.geo[0][row], dl.geo[1][row]), (0, U));
    }
}

#[test]
fn polar_sectors_nest_within_their_parent() {
    for seed in 0..6 {
        for dpr in [1.0, 1.25, 2.0] {
            let store = random_store(seed);
            let dl = polar(&store, RootSpec::node(1), |p| {
                p.width = 1280.0;
                p.height = 720.0;
                p.dpr = dpr;
                p.detail = 8;
                p.min_element = 4.0
            });
            assert!(!dl.is_empty());
            for i in 0..dl.len() {
                assert!(arc(&dl, i) >= 2.0 * dpr);
                let p = dl.parent[i];
                if p >= 0 {
                    let p = p as usize;
                    assert!(dl.geo[0][i] >= dl.geo[0][p] && dl.geo[1][i] <= dl.geo[1][p]);
                    assert!(dl.geo[2][i] > dl.geo[3][p]);
                }
            }
        }
    }
}

#[test]
fn polar_threshold_shrinks_ring_by_ring() {
    let store = build(&dir([(
        "A",
        dir([("B", S::D(named("f", 140, ".bin", |_| MB)))]),
    )]));
    let far = polar(&store, RootSpec::node(1), |p| p.min_element = 10.0);
    let files: Vec<usize> = (0..far.len())
        .filter(|&i| far.kind[i] == kind::FILE)
        .collect();
    assert_eq!(files.len(), 140);
    assert!(
        files
            .iter()
            .all(|&i| far.depth[i] == 3 && arc(&far, i) >= 10.0)
    );
    let ab = find(&store.read(), "A/B");
    let near = polar(&store, RootSpec::node(ab), |p| p.min_element = 10.0);
    let small = near.row(KEY_SMALL | ab).unwrap();
    assert_eq!((near.len(), near.count[small]), (1, 140));
}

#[test]
fn polar_expanding_needs_the_minimum_element_along_the_ring() {
    let store = build(&dir([
        ("big.bin", S::F(990 * MB)),
        (
            "tiny",
            dir([("a.bin", S::F(5 * MB)), ("b.bin", S::F(5 * MB))]),
        ),
    ]));
    let tiny = find(&store.read(), "tiny");
    for (m, k) in [(14.0, kind::COLLAPSED), (8.0, kind::FOLDER)] {
        let dl = polar(&store, RootSpec::node(1), |p| p.min_element = m);
        let row = dl.node.iter().position(|&x| x == tiny as i32).unwrap();
        assert_eq!(dl.kind[row], k);
    }
}

fn want_label(cell: [i32; 4], dpr: f64, font: f64) -> u8 {
    let [a0, a1, r0, r1] = cell.map(f64::from);
    let (lu, g) = (dpr * font, pr(2.0 * dpr).max(1) as f64);
    let delta = (a1 - a0) / U as f64 * 2.0 * PI;
    let (t, mid) = (r1 - r0, (r0 + r1) / 2.0);
    let length = delta * mid - g;
    if t > length {
        return if label_box(t / lu, (delta * r0 - g) / lu, true) & 1 != 0 {
            1 | 8 | 16
        } else {
            0
        };
    }
    let outer = r1 - pr(2.0 * dpr) as f64;
    let chord = 2.0 * f64::max(0.0, outer * outer - mid * mid).sqrt();
    label_box(length.min(chord) / lu, t / lu, false)
}

#[test]
fn polar_labels_run_along_the_ring_or_along_the_radius() {
    let store = golden_tree(3000, 7, 0);
    for (dpr, font) in [(1.0, 1.0), (1.25, 1.0), (1.5, 1.4), (2.0, 0.8)] {
        let dl = polar(&store, RootSpec::node(1), |p| {
            p.width = 1280.0;
            p.height = 900.0;
            p.dpr = dpr;
            p.font_scale = font;
            p.detail = 6;
            p.min_element = 4.0
        });
        let g = &dl.geo;
        let want: Vec<u8> = (0..dl.len())
            .map(|i| want_label([g[0][i], g[1][i], g[2][i], g[3][i]], dpr, font))
            .collect();
        assert_eq!(dl.label, want);
        assert!(dl.label.iter().any(|&b| b & 16 != 0));
        assert!(dl.label.iter().any(|&b| b & 1 != 0 && b & 16 == 0));
    }
}

#[test]
fn polar_a_thin_spike_names_itself_along_the_radius() {
    let proj = FlamePolarProjection::new(&params(1920.0, 1080.0, |p| p.detail = 5));
    let cell = [0, U / 60, proj.r0[3] as i32, proj.r1[3] as i32];
    assert_eq!(proj.label(&cell, kind::FILE), 1 | 8 | 16);
    let wide = [0, U / 4, proj.r0[1] as i32, proj.r1[1] as i32];
    assert_eq!(proj.label(&wide, kind::FILE), 1 | 2);
    let thin = [0, U / 2000, proj.r0[1] as i32, proj.r1[1] as i32];
    assert_eq!(proj.label(&thin, kind::FILE), 0);
}

#[test]
fn the_partition_visualizations_are_registered() {
    let f = registry::get("flame").unwrap();
    assert_eq!(
        (
            f.layout,
            f.capability.as_str(),
            f.icon.as_str(),
            f.order,
            f.title_id.as_str()
        ),
        (FLAME_LAYOUT, CAP_LAYOUT_FLAME, "vizFlame", 1, "火焰图")
    );
    assert!(
        registry::is_available(&f, &[CAP_LAYOUT_FLAME]) && !registry::is_available::<&str>(&f, &[])
    );
    let p = registry::get("flame_polar").unwrap();
    assert_eq!(
        (
            p.capability.as_str(),
            p.icon.as_str(),
            p.order,
            p.title_id.as_str()
        ),
        (CAP_LAYOUT_POLAR, "vizFlamePolar", 2, "火焰-极坐标图")
    );
    let store = build(&dir([("a.bin", S::F(MB))]));
    let dl = go(
        &*p.engine,
        &store,
        &LayoutParams::new(300.0, 200.0),
        &RootSpec::node(1),
        None,
    );
    assert_eq!(dl.visual, "flame_polar");
}
