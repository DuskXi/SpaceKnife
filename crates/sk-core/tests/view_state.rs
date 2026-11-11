//! View state machine (Python `test_view.py`, `test_view_results.py`, `test_view_visuals.py`,
//! `test_trash_view.py`, the registry parts of `test_visual_registry.py`): navigation history,
//! detail, free/unknown, filter dim/hide, tags, colours, selection, results, visualizations,
//! branch colours, recycle bins, the binary frame.

mod view_support;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Once};

use sk_core::layout::api::ENGINE_API;
use sk_core::layout::basis::Basis;
use sk_core::layout::{
    DrawList, Engine, KEY_FREE, KEY_MORE, KEY_SMALL, KEY_UNKNOWN, LayoutParams, RootSpec, Row,
    SizeSource, StoreSizes, TreemapEngine, Visualization, kind, registry,
};
use sk_core::view::branch::{BranchHues, LIMIT, SLOTS};
use sk_core::view::results::{
    ResultsCache, effective_tag, relative_path, result_rows, sort_rows, sorted_rows,
};
use sk_core::view::trash::{TrashCache, WithoutBins, is_trash_path, subtree};
use sk_core::view::{
    ArrayChanges, Frame, RootInfo, TagStore, View, ViewContext, ViewSettings, encode_frame, flag,
    frame_info, showable,
};
use sk_protocol::compare::CompareDirection;
use sk_protocol::filter::TagColor;
use sk_protocol::view::{
    ColorModeName, DRAWLIST_LAYOUT, FLAME_LAYOUT, FilterModeName, FlameGeometry, FlameOrigin,
    FrameGeometry, PixelRect, ResultKind, ResultsSortName,
};
use sk_protocol::{CAP_BINARY_FRAMES, CAP_LAYOUT_FLAME, CAP_VISUALS};
use sk_store::{Col, Flags, NodeId, Store};
use view_support::{S, build_spec, dir, find};

const MB: i64 = 1 << 20;
const GB: i64 = 1 << 30;

// --------------------------------------------------------------------------- fixtures

fn files(prefix: &str, n: usize, ext: &str, size: i64) -> Vec<(String, S)> {
    (0..n)
        .map(|i| (format!("{prefix}{i}{ext}"), S::F(size)))
        .collect()
}

fn spec(extra_videos: bool, deep: bool) -> S {
    let mut videos = vec![
        ("trip.mp4".to_owned(), S::F(900 * MB)),
        ("wedding.mov".to_owned(), S::F(600 * MB)),
    ];
    if extra_videos {
        videos.push(("clip.mp4".to_owned(), S::F(0)));
    }
    let mut temp = files("t", 400, ".tmp", 5_000);
    temp.push(("big.log".to_owned(), S::F(80 * MB)));
    let mut lenny = vec![
        ("Videos".to_owned(), S::D(videos)),
        (
            "Music".to_owned(),
            dir([("a.mp3", S::F(40 * MB)), ("b.flac", S::F(60 * MB))]),
        ),
        ("Temp".to_owned(), S::D(temp)),
    ];
    if deep {
        let mut d = dir([("far.mp4", S::F(50 * MB))]);
        for name in ["d", "c", "b", "a"] {
            d = S::D(vec![(name.to_owned(), d)]);
        }
        lenny.push(("Deep".to_owned(), d));
    }
    dir([
        ("Users", dir([("lenny", S::D(lenny))])),
        (
            "Windows",
            dir([(
                "System32",
                dir([
                    ("kernel.dll", S::F(30 * MB)),
                    ("drivers", dir([("x.sys", S::F(5 * MB))])),
                ]),
            )]),
        ),
        ("pagefile.sys", S::F(300 * MB)),
    ])
}

fn tmp_tags() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tags.json");
    (dir, file)
}

fn view_on(store: Store, roots: Vec<RootInfo>, tags: Option<PathBuf>) -> View {
    let ctx = ViewContext::new(store, roots, TagStore::new(tags));
    let mut v = View::new("v1", ctx, ViewSettings::default()).unwrap();
    v.set_viewport(1200.0, 800.0, 1.0);
    v
}

fn volume_view(used: i64, s: &S, tags: Option<PathBuf>) -> View {
    let store = build_spec(s, "C:\\", 1, &[]);
    view_on(
        store,
        vec![RootInfo {
            node: 1,
            whole_volume: true,
            free: Some(500 * MB),
            used: Some(used),
        }],
        tags,
    )
}

fn plain(s: &S, pending: &[&str]) -> View {
    view_on(
        build_spec(s, "/root", 1, pending),
        vec![RootInfo::folder(1)],
        None,
    )
}

fn node(v: &View, path: &str) -> NodeId {
    find(&v.store().read(), path)
}

fn row_of(v: &View, path: &str) -> usize {
    let n = node(v, path) as i32;
    let f = v.last.as_ref().unwrap();
    f.draw
        .node
        .iter()
        .position(|&x| x == n)
        .unwrap_or_else(|| panic!("{path}"))
}

fn keys(f: &Frame) -> HashSet<u32> {
    f.draw.key.iter().copied().collect()
}

// --------------------------------------------------------------------------- test-only visuals

/// The treemap's rows handed out as `flame.v1` (the zoom root is drawn, flashes may land on it).
struct AsFlame;

impl Engine for AsFlame {
    fn run(
        &self,
        nodes: &sk_store::Nodes,
        basis: &Basis,
        params: &LayoutParams,
        root: &RootSpec,
        sizes: &dyn SizeSource,
        out: &mut DrawList,
    ) {
        TreemapEngine.run(nodes, basis, params, root, sizes, out);
        out.layout = FLAME_LAYOUT;
        out.meta = Some(FrameGeometry::Flame(FlameGeometry {
            kind: Default::default(),
            origin: FlameOrigin::Bottom,
            row_h: 20,
            rows: params.detail as i32,
            gap: 2,
            root: PixelRect {
                x: 0,
                y: 0,
                w: out.width,
                h: 18,
            },
        }));
    }
    fn levels(&self, params: &LayoutParams) -> u32 {
        params.detail
    }
}

/// A treemap that fits one level whatever the detail level asks for.
struct Shallow;

impl Engine for Shallow {
    fn run(
        &self,
        nodes: &sk_store::Nodes,
        basis: &Basis,
        params: &LayoutParams,
        root: &RootSpec,
        sizes: &dyn SizeSource,
        out: &mut DrawList,
    ) {
        let mut p = params.clone();
        p.detail = 1;
        TreemapEngine.run(nodes, basis, &p, root, sizes, out);
    }
    fn levels(&self, _: &LayoutParams) -> u32 {
        1
    }
}

fn fake(
    id: &str,
    engine: Arc<dyn Engine>,
    layout: &'static str,
    color: ColorModeName,
) -> Visualization {
    Visualization {
        id: id.to_owned(),
        api: ENGINE_API,
        layout,
        capability: String::new(),
        title_id: "树图".into(),
        icon: "vizTreemap".into(),
        order: 90,
        engine,
        default_color: color,
        compare: true,
        free_space: true,
    }
}

fn fakes() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        registry::register(fake(
            "test.fake",
            Arc::new(TreemapEngine),
            DRAWLIST_LAYOUT,
            ColorModeName::Flat,
        ))
        .unwrap();
        registry::register(fake(
            "test.rooted",
            Arc::new(AsFlame),
            FLAME_LAYOUT,
            ColorModeName::Branch,
        ))
        .unwrap();
        registry::register(fake(
            "test.shallow",
            Arc::new(Shallow),
            DRAWLIST_LAYOUT,
            ColorModeName::Class,
        ))
        .unwrap();
        let mut nofree = fake(
            "test.nofree",
            Arc::new(TreemapEngine),
            DRAWLIST_LAYOUT,
            ColorModeName::Class,
        );
        nofree.free_space = false;
        registry::register(nofree).unwrap();
    });
}

// --------------------------------------------------------------------------- test_view.py

#[test]
fn volume_root_shows_free_and_unknown_space() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    let f = v.render();
    assert!(keys(f).contains(&KEY_FREE) && keys(f).contains(&KEY_UNKNOWN));
    let unknown = f.draw.size[f.draw.row(KEY_UNKNOWN).unwrap()];
    assert_eq!(unknown, 2200 * MB - v.store().read().tot_alloc(1));
    v.show_free = false;
    assert!(!keys(v.render()).contains(&KEY_FREE));
    v.show_unknown = false;
    assert!(!keys(v.render()).contains(&KEY_UNKNOWN));
    v.show_free = true;
    v.show_unknown = true;
    let users = node(&v, "Users");
    v.zoom_to_node(users);
    assert!(!keys(v.render()).contains(&KEY_FREE)); // only at the volume root
}

#[test]
fn zoom_back_forward_up_home() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    v.render();
    let users = node(&v, "Users");
    assert!(v.zoom_into(users));
    v.render();
    let lenny = node(&v, "Users/lenny");
    v.select(Some(lenny));
    assert!(v.zoom_into(lenny));
    v.render();
    let crumbs: Vec<u32> = v.breadcrumb().iter().map(|c| c.0).collect();
    assert_eq!(crumbs, [1, users, lenny]);
    assert!(v.go_back());
    assert_eq!((v.zoom.node, v.selected), (users, Some(lenny)));
    assert!(v.go_forward());
    assert_eq!(v.zoom.node, lenny);
    assert!(v.go_up());
    assert_eq!((v.zoom.node, v.selected), (users, Some(lenny)));
    assert!(v.go_home());
    assert_eq!(v.zoom.node, 1);
    assert!(!v.go_home() && !v.go_up() && !v.can_go_up());
    while v.go_back() {}
    assert!(v.zoom.node == 1 && v.back.is_empty());
}

#[test]
fn free_space_cannot_be_zoomed() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    v.render();
    assert!(!v.zoom_into(KEY_FREE) && !v.zoom_into(KEY_UNKNOWN));
}

#[test]
fn zoom_into_small_items_and_back_up() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    let temp = node(&v, "Users/lenny/Temp");
    v.zoom_to_node(temp);
    let key = KEY_SMALL | temp;
    let f = v.render();
    let row = f.draw.row(key).unwrap();
    assert_eq!(f.draw.count[row], 400);
    assert!(v.zoom_into(key));
    assert!(v.render().draw.len() > 100);
    assert_eq!(
        *v.breadcrumb().last().unwrap(),
        (key, "small:400".to_owned())
    );
    assert!(v.go_up());
    assert_eq!((v.zoom.node, v.selected), (temp, Some(key)));
    assert_eq!(v.render().selected_row, Some(row));
}

#[test]
fn zooming_into_a_huge_small_items_block_always_makes_progress() {
    let mut big = vec![("big.iso".to_owned(), S::F(4 * GB))];
    big.extend(files("f", 20_000, ".bin", 4096));
    let mut v = plain(&dir([("Big", S::D(big))]), &[]);
    let bign = node(&v, "Big");
    v.render();
    let mut key = Some(KEY_SMALL | bign);
    let mut keys_seen = Vec::new();
    let mut counts = vec![20_000u32];
    while let Some(k) = key {
        assert!(v.zoom_into(k));
        keys_seen.push(k);
        let f = v.render();
        assert!(f.draw.kind.iter().filter(|&&x| x == kind::FILE).count() > 1000);
        let aggs: Vec<usize> = (0..f.draw.len())
            .filter(|&i| f.draw.kind[i] != kind::FILE)
            .collect();
        key = None;
        if let [row] = aggs[..] {
            let nk = f.draw.key[row];
            assert!(!keys_seen.contains(&nk) && f.draw.count[row] < *counts.last().unwrap());
            counts.push(f.draw.count[row]);
            key = Some(nk);
        }
    }
    assert!(keys_seen.len() == 5 && *counts.last().unwrap() < 4000);
    let crumbs = v.breadcrumb();
    let mut want = vec![1, bign];
    want.extend(&keys_seen);
    assert_eq!(crumbs.iter().map(|c| c.0).collect::<Vec<_>>(), want);
    let mut labels = vec!["small:20000".to_owned()];
    labels.extend(counts[1..].iter().map(|n| format!("more:{n}")));
    assert_eq!(
        crumbs[2..].iter().map(|c| c.1.clone()).collect::<Vec<_>>(),
        labels
    );
    for &k in keys_seen.iter().rev() {
        assert!(v.go_up());
        let f = v.render();
        let row = f.draw.row(k);
        assert!(row.is_some() && f.selected_row == row);
        assert_eq!(v.selected, Some(k));
    }
    assert!(v.zoom.node == bign && v.zoom.members.is_none());
}

#[test]
fn selection_in_a_small_items_block_too_small_to_draw_marks_its_folder() {
    for at in ["", "Data/"] {
        let x = dir([
            ("big.bin", S::F(40 * GB)),
            ("a.txt", S::F(10)),
            ("b.txt", S::F(10)),
        ]);
        let mut items = if at.is_empty() {
            vec![("X".to_owned(), x)]
        } else {
            vec![("Data".to_owned(), dir([("X", x)]))]
        };
        items.push(("Y".to_owned(), dir([("y.bin", S::F(20 * GB))])));
        let mut v = plain(&S::D(items), &[]);
        let folder = node(&v, &format!("{at}X"));
        let a = node(&v, &format!("{at}X/a.txt"));
        let f = v.render();
        assert!(f.draw.row(KEY_SMALL | folder).is_none());
        assert!(f.draw.members[&(KEY_SMALL | folder)].contains(&a));
        v.select(Some(a));
        let f = v.render();
        let sel = f.selected_row.unwrap();
        assert_eq!(Some(sel), f.draw.row(folder));
        assert!(f.flags[sel] & flag::SELECTED != 0);
        let on_path: HashSet<i32> = (0..f.draw.len())
            .filter(|&r| f.flags[r] & flag::ON_PATH != 0)
            .map(|r| f.draw.node[r])
            .collect();
        let want: HashSet<i32> = if at.is_empty() {
            HashSet::new()
        } else {
            [node(&v, "Data") as i32].into()
        };
        assert_eq!(on_path, want);
    }
}

#[test]
fn detail_level_is_clamped() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    assert_eq!(v.set_detail(0), 1);
    assert_eq!(v.set_detail(99), 15);
    v.set_detail(1);
    assert!(!v.render().draw.kind.contains(&kind::FOLDER));
}

#[test]
fn selection_inside_a_collapsed_folder_marks_the_visible_ancestor() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    v.set_detail(2);
    v.render();
    let x_sys = node(&v, "Windows/System32/drivers/x.sys");
    v.select(Some(x_sys));
    let f = v.render();
    let sel = f.selected_row.unwrap();
    let sel_node = f.draw.node[sel] as NodeId;
    let flags = f.flags.clone();
    assert_eq!(sel_node, node(&v, "Windows/System32"));
    assert!(flags[sel] & flag::SELECTED != 0);
    assert!(flags[row_of(&v, "Windows")] & flag::ON_PATH != 0);
}

#[test]
fn filter_dim_keeps_the_layout() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    let plain_draw = v.render().draw.clone();
    v.set_filter("*.mp4;*.mov", None);
    let f = v.render();
    assert_eq!(plain_draw.key, f.draw.key);
    assert_eq!(plain_draw.geo, f.draw.geo);
    assert_eq!(plain_draw.kind, f.draw.kind);
    let flags = f.flags.clone();
    let totals = f.filter_totals;
    let trip = row_of(&v, "Users/lenny/Videos/trip.mp4");
    let music = row_of(&v, "Users/lenny/Music");
    let users = row_of(&v, "Users");
    assert!(flags[trip] & flag::MATCH != 0 && flags[trip] & flag::DIM == 0);
    assert!(flags[music] & flag::DIM != 0);
    assert!(flags[users] & flag::MATCH != 0);
    assert_eq!(totals, Some((2, 1500 * MB, 1500 * MB)));
}

#[test]
fn filter_hide_lays_out_matches_only() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    v.filter_mode = FilterModeName::Hide;
    v.set_filter(":class:video", None);
    let music = node(&v, "Users/lenny/Music") as i32;
    let f = v.render();
    assert!(!f.draw.node.contains(&music));
    assert!(!keys(f).contains(&KEY_FREE));
    let _ = f;
    let size = v.last.as_ref().unwrap().draw.size[row_of(&v, "Users")];
    assert_eq!(size, 1500 * MB);
}

#[test]
fn filter_with_errors_keeps_the_last_valid_result() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    v.set_filter("*.mp4", None);
    let good = v.filter.result.clone().unwrap();
    let state = v.set_filter("*.mp4;>500xb", None);
    assert!(state.stale);
    assert_eq!(state.diagnostics[0].code.as_str(), "unknown_unit");
    assert!(Arc::ptr_eq(v.filter.result.as_ref().unwrap(), &good));
    v.set_filter("", None);
    assert!(!v.filter.active());
    assert!(
        v.render()
            .flags
            .iter()
            .all(|&f| f & (flag::MATCH | flag::DIM) == 0)
    );
}

#[test]
fn folder_mask_hits_are_flagged() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    v.set_filter("\\temp", None);
    let flags = v.render().flags.clone();
    assert!(flags[row_of(&v, "Users/lenny/Temp")] & flag::FOLDER_HIT != 0);
    assert!(flags[row_of(&v, "Users/lenny/Music")] & flag::FOLDER_HIT == 0);
}

#[test]
fn tags_mark_inherit_persist_and_clear() {
    let (_dir, file) = tmp_tags();
    let mut v = volume_view(2200 * MB, &spec(false, false), Some(file.clone()));
    let videos = node(&v, "Users/lenny/Videos");
    v.select(Some(videos));
    assert!(v.tag_selected(Some(TagColor::Green)));
    let f = v.render();
    let (tag, flags) = (f.tag.clone(), f.flags.clone());
    let row = row_of(&v, "Users/lenny/Videos");
    assert!(tag[row] == 3 && flags[row] & flag::TAGGED != 0);
    assert_eq!(tag[row_of(&v, "Users/lenny/Videos/trip.mp4")], 0);
    v.set_filter(":green", None);
    assert_eq!(v.filter.result.as_ref().unwrap().totals(1).0, 2);
    let mut again = TagStore::new(Some(file));
    assert_eq!(again.of(&v.store().read(), videos), Some(TagColor::Green));
    v.select(Some(node(&v, "Users")));
    assert!(v.tag_selected(None));
    assert_eq!(v.ctx.tags.lock().of(&v.store().read(), videos), None);
    assert_eq!(v.filter.result.as_ref().unwrap().totals(1).0, 0);
}

#[test]
fn time_tones() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    let now = sk_core::filter::unix_now();
    {
        let ids: Vec<(NodeId, i64)> = [
            ("pagefile.sys", 3600),
            ("Windows/System32/kernel.dll", 3 * 86_400),
            ("Users/lenny/Videos/trip.mp4", 400 * 86_400),
        ]
        .iter()
        .map(|(p, age)| (node(&v, p), *age))
        .collect();
        let mut w = v.store().write();
        for (id, age) in ids {
            w.set_raw(Col::Mtime, id, (now - age) as u32);
        }
        w.touch();
    }
    v.set_color_mode(ColorModeName::Time);
    let tone = v.render().tone.clone();
    assert_eq!(tone[row_of(&v, "pagefile.sys")], 0);
    v.zoom_to_node(node(&v, "Windows/System32"));
    let tone = v.render().tone.clone();
    assert_eq!(tone[row_of(&v, "Windows/System32/kernel.dll")], 1);
    v.zoom_to_node(node(&v, "Users/lenny/Videos"));
    let tone = v.render().tone.clone();
    assert_eq!(tone[row_of(&v, "Users/lenny/Videos/trip.mp4")], 4);
}

#[test]
fn views_of_one_scan_are_independent() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    let mut other = View::new("v2", v.ctx.clone(), ViewSettings::default()).unwrap();
    other.set_viewport(600.0, 400.0, 1.0);
    v.zoom_to_node(node(&v, "Users"));
    v.set_detail(3);
    v.set_filter("*.mp4", None);
    assert!(other.zoom.node == 1 && other.detail == 5 && !other.filter.active());
    assert!(!other.render().draw.is_empty());
}

#[test]
fn double_clicking_a_file_zooms_to_its_folder_and_selects_it() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    let trip = node(&v, "Users/lenny/Videos/trip.mp4");
    assert!(v.zoom_to_node(trip));
    assert_eq!(
        (v.zoom.node, v.selected),
        (node(&v, "Users/lenny/Videos"), Some(trip))
    );
    let f = v.render();
    assert_eq!(f.draw.node[f.selected_row.unwrap()], trip as i32);
    let depth = v.back.len();
    v.select(None);
    assert!(v.zoom_into(trip));
    assert_eq!((v.selected, v.back.len()), (Some(trip), depth));
}

fn scan_spec() -> S {
    dir([
        (
            "a",
            dir([(
                "b",
                dir([(
                    "c",
                    dir([
                        (
                            "d",
                            dir([("e", dir([("f.bin", S::F(1))])), ("h.bin", S::F(20 * MB))]),
                        ),
                        ("g.bin", S::F(30 * MB)),
                    ]),
                )]),
            )]),
        ),
        ("done", dir([("x.bin", S::F(50 * MB))])),
    ])
}

fn marked(v: &mut View) -> (HashSet<String>, bool) {
    let f = v.render();
    let rows: Vec<i32> = (0..f.draw.len())
        .filter(|&r| f.flags[r] & flag::SCANNING != 0)
        .map(|r| f.draw.node[r])
        .collect();
    let root = f.root.flags & flag::SCANNING != 0;
    let r = v.store().read();
    (
        rows.iter()
            .map(|&n| r.name(n as NodeId).into_owned())
            .collect(),
        root,
    )
}

fn set(names: &[&str]) -> HashSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn folders_still_being_scanned_are_marked_on_the_top_three_levels() {
    fakes();
    let mut v = plain(&scan_spec(), &["a/b/c/d/e"]);
    assert_eq!(marked(&mut v), (set(&[]), false));
    v.scanning = true;
    assert_eq!(marked(&mut v), (set(&["a", "b", "c"]), true));
    for visual in ["test.rooted", "test.fake"] {
        v.set_visual(visual);
        assert_eq!(marked(&mut v), (set(&["a", "b", "c"]), true));
    }
    v.set_visual("treemap");
    assert!(v.zoom_into(node(&v, "a")));
    assert_eq!(marked(&mut v).0, set(&["b", "c", "d"]));
    v.zoom_to_node(node(&v, "done"));
    assert_eq!(marked(&mut v), (set(&[]), false));
    v.zoom_to_node(node(&v, "a/b/c/d/e"));
    let f = v.render();
    assert!(f.root.kind == kind::PENDING && f.root.flags & flag::SCANNING != 0);
    let e = node(&v, "a/b/c/d/e");
    v.store().appender().clear(e, Flags::PENDING);
    v.zoom_to_node(1);
    assert_eq!(marked(&mut v), (set(&[]), false));
}

// --------------------------------------------------------------------------- test_view_results.py

fn results_view() -> View {
    volume_view(2600 * MB, &spec(true, true), None)
}

fn filtered(v: &mut View, text: &str) {
    v.set_filter(text, None);
    assert!(v.filter.active());
    v.render();
}

fn check_totals(v: &View) {
    let (files, alloc, size) = v.last.as_ref().unwrap().filter_totals.unwrap();
    let rows = result_rows(v);
    assert_eq!(rows.iter().map(|r| r.count).sum::<i64>(), files);
    assert_eq!(rows.iter().map(|r| r.alloc).sum::<i64>(), alloc);
    assert_eq!(rows.iter().map(|r| r.size).sum::<i64>(), size);
}

#[test]
fn drawn_files_are_their_own_rows() {
    let mut v = results_view();
    filtered(&mut v, "*.mov;trip.mp4");
    let rows = sort_rows(result_rows(&v), ResultsSortName::Size);
    let got: Vec<(ResultKind, &str, i64)> = rows
        .iter()
        .map(|r| (r.kind, r.name.as_str(), r.count))
        .collect();
    assert_eq!(
        got,
        [
            (ResultKind::File, "trip.mp4", 1),
            (ResultKind::File, "wedding.mov", 1)
        ]
    );
    let sep = std::path::MAIN_SEPARATOR_STR;
    assert_eq!(
        relative_path(&v, rows[0].parent),
        ["Users", "lenny", "Videos"].join(sep)
    );
    assert_eq!(rows[0].key, node(&v, "Users/lenny/Videos/trip.mp4"));
    check_totals(&v);
}

#[test]
fn collapsed_folder_aggregates_its_matches() {
    let mut v = results_view();
    v.set_detail(1);
    filtered(&mut v, "*.mp4;*.mov");
    let rows = result_rows(&v);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].kind, rows[0].name.as_str(), rows[0].count),
        (ResultKind::Folder, "Users", 4)
    );
    assert_eq!(rows[0].alloc, (900 + 600 + 50) * MB);
    assert_eq!(relative_path(&v, rows[0].parent), "");
    check_totals(&v);
}

#[test]
fn small_items_become_one_group_row() {
    let mut v = results_view();
    v.zoom_to_node(node(&v, "Users/lenny/Temp"));
    filtered(&mut v, "*.tmp");
    let rows = result_rows(&v);
    let groups: Vec<_> = rows
        .iter()
        .filter(|r| r.kind == ResultKind::Group)
        .collect();
    assert!(!groups.is_empty() && groups.iter().all(|r| r.key >= KEY_SMALL));
    assert_eq!(rows.iter().map(|r| r.count).sum::<i64>(), 400);
    check_totals(&v);
}

#[test]
fn zero_size_and_undrawn_matches_still_count() {
    let mut v = results_view();
    filtered(&mut v, "clip.mp4");
    assert_eq!(result_rows(&v).iter().map(|r| r.count).sum::<i64>(), 1);
    check_totals(&v);
}

#[test]
fn rows_add_up_to_the_totals() {
    for detail in [1, 2, 3, 5, 9] {
        for text in ["*.mp4", "*", ">1mb", "*.tmp;*.sys", "\\temp", "|*.tmp"] {
            for mode in [FilterModeName::Dim, FilterModeName::Hide] {
                let mut v = results_view();
                v.set_detail(detail);
                v.filter_mode = mode;
                filtered(&mut v, text);
                check_totals(&v);
                v.zoom_to_node(node(&v, "Users/lenny"));
                v.render();
                check_totals(&v);
            }
        }
    }
}

#[test]
fn rows_inside_an_aggregate_zoom() {
    let mut v = results_view();
    v.zoom_to_node(node(&v, "Users/lenny/Temp"));
    let f = v.render();
    let small = (0..f.draw.len())
        .find(|&i| f.draw.kind[i] == kind::SMALL)
        .map(|i| f.draw.key[i])
        .unwrap();
    assert!(v.zoom_into(small));
    filtered(&mut v, "*.tmp");
    let rows = result_rows(&v);
    assert!(!rows.is_empty());
    check_totals(&v);
}

#[test]
fn a_huge_aggregate_lists_its_members_and_opens_its_rest() {
    let mut big = vec![("big.iso".to_owned(), S::F(4 << 30))];
    big.extend(files("f", 20_000, ".bin", 4096));
    let mut v = plain(&dir([("Big", S::D(big))]), &[]);
    v.render();
    assert!(v.zoom_into(KEY_SMALL | node(&v, "Big")));
    filtered(&mut v, "*.bin");
    let rows = result_rows(&v);
    let nfiles = rows.iter().filter(|r| r.kind == ResultKind::File).count();
    let groups: Vec<_> = rows
        .iter()
        .filter(|r| r.kind == ResultKind::Group)
        .collect();
    assert_eq!(groups.len(), 1);
    let group = groups[0].clone();
    assert!(nfiles > 1000 && nfiles as i64 + group.count == 20_000);
    check_totals(&v);
    let member = *v.last.as_ref().unwrap().draw.members[&group.key]
        .last()
        .unwrap();
    let depth = v.back.len();
    assert!(v.reveal(member));
    let f = v.render();
    assert_eq!(f.selected_row, f.draw.row(group.key));
    assert_eq!(v.back.len(), depth);
    assert!(v.zoom_into(group.key));
    v.render();
    let rows = result_rows(&v);
    assert_eq!(rows.iter().map(|r| r.count).sum::<i64>(), group.count);
    assert!(rows.iter().any(|r| r.kind == ResultKind::File));
    check_totals(&v);
}

#[test]
fn sorting_and_cache() {
    let mut v = results_view();
    filtered(&mut v, "*.mp4;*.mov;*.mp3;*.flac");
    let mut cache = ResultsCache::default();
    let by_size = sorted_rows(&v, &mut cache);
    assert!(by_size.windows(2).all(|w| w[0].alloc >= w[1].alloc));
    assert!(Arc::ptr_eq(&sorted_rows(&v, &mut cache), &by_size));
    v.results_sort = ResultsSortName::Name;
    let names: Vec<String> = sorted_rows(&v, &mut cache)
        .iter()
        .map(|r| r.name.to_lowercase())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    v.results_sort = ResultsSortName::Count;
    let counts: Vec<i64> = sorted_rows(&v, &mut cache)
        .iter()
        .map(|r| r.count)
        .collect();
    assert!(counts.windows(2).all(|w| w[0] >= w[1]));
    v.results_sort = ResultsSortName::Size;
    v.render();
    assert!(!Arc::ptr_eq(&sorted_rows(&v, &mut cache), &by_size));
}

#[test]
fn effective_tag_inherits_from_folders() {
    let v = results_view();
    let videos = node(&v, "Users/lenny/Videos");
    let trip = node(&v, "Users/lenny/Videos/trip.mp4");
    {
        let r = v.store().read();
        v.ctx.tags.lock().set(&r, videos, Some(TagColor::Green));
    }
    assert_eq!(effective_tag(&v, trip), (Some(TagColor::Green), true));
    assert_eq!(effective_tag(&v, videos), (Some(TagColor::Green), false));
    {
        let r = v.store().read();
        v.ctx.tags.lock().set(&r, trip, Some(TagColor::Red));
    }
    assert_eq!(effective_tag(&v, trip), (Some(TagColor::Red), false));
}

#[test]
fn reveal_a_drawn_block_only_selects() {
    let mut v = results_view();
    v.render();
    let users = node(&v, "Users");
    assert!(v.reveal(users));
    assert!(v.selected == Some(users) && v.back.is_empty());
    assert!(!v.reveal(users));
}

#[test]
fn reveal_navigates_to_show_a_collapsed_file() {
    let mut v = results_view();
    v.set_detail(2);
    v.render();
    let far = node(&v, "Users/lenny/Deep/a/b/c/d/far.mp4");
    assert!(v.reveal(far));
    let f = v.render();
    assert!(f.draw.row(far).is_some() && f.selected_row == f.draw.row(far));
    assert_eq!(v.selected, Some(far));
    assert!(v.go_back() && v.zoom.node == 1);
}

#[test]
fn reveal_selects_a_member_of_a_small_block() {
    let mut v = results_view();
    let temp = node(&v, "Users/lenny/Temp");
    v.zoom_to_node(temp);
    let t0 = node(&v, "Users/lenny/Temp/t0.tmp");
    assert!(v.render().draw.row(t0).is_none());
    let depth = v.back.len();
    assert!(v.reveal(t0));
    let f = v.render();
    let sel = f.selected_row.unwrap();
    assert_eq!(f.draw.kind[sel], kind::SMALL);
    assert!(v.zoom.node == temp && v.back.len() == depth);
}

#[test]
fn reveal_outside_the_zoom_root() {
    let mut v = results_view();
    v.zoom_to_node(node(&v, "Windows"));
    v.render();
    let trip = node(&v, "Users/lenny/Videos/trip.mp4");
    assert!(v.reveal(trip));
    assert!(v.render().draw.row(trip).is_some());
    assert_eq!(v.selected, Some(trip));
}

#[test]
fn reveal_uses_the_levels_the_visualization_draws() {
    fakes();
    let mut deep = dir([("f.bin", S::F(10 * GB))]);
    for name in ["e", "d", "c", "b", "a"] {
        deep = S::D(vec![(name.to_owned(), deep)]);
    }
    let mut v = plain(&deep, &[]);
    let f = node(&v, "a/b/c/d/e/f.bin");
    v.render();
    assert!(v.reveal(f) && v.zoom.node == node(&v, "a"));
    assert!(v.render().draw.row(f).is_some());
    v.go_home();
    v.set_visual("test.shallow");
    v.render();
    assert!(v.reveal(f) && v.zoom.node == node(&v, "a/b/c/d/e"));
    assert!(v.render().draw.row(f).is_some());
}

// --------------------------------------------------------------------------- test_view_visuals.py

#[test]
fn switching_keeps_zoom_history_selection_and_filter() {
    fakes();
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    let (users, lenny) = (node(&v, "Users"), node(&v, "Users/lenny"));
    v.render();
    assert!(v.zoom_into(users));
    v.render();
    assert!(v.zoom_into(lenny));
    v.select(Some(node(&v, "Users/lenny/Videos")));
    v.set_filter("*.mp4", None);
    v.set_detail(3);
    let before = (
        v.zoom.clone(),
        v.back.len(),
        v.selected,
        v.filter.normal.clone(),
        v.detail,
    );
    let first = v.render();
    let (first_keys, first_sel, first_totals) = (
        first.draw.key.clone(),
        first.selected_row,
        first.filter_totals,
    );
    assert_eq!(v.set_visual("test.fake"), Some(true));
    assert_eq!(v.set_visual("test.fake"), Some(false));
    assert_eq!(v.set_visual("pie"), None);
    assert_eq!(
        (
            v.zoom.clone(),
            v.back.len(),
            v.selected,
            v.filter.normal.clone(),
            v.detail
        ),
        before
    );
    let f = v.render();
    assert_eq!(
        (f.draw.visual.as_str(), f.draw.layout),
        ("test.fake", DRAWLIST_LAYOUT)
    );
    assert_eq!(f.draw.key, first_keys);
    assert_eq!((f.selected_row, f.filter_totals), (first_sel, first_totals));
    assert!(v.go_back() && v.zoom.node == users && v.visual == "test.fake");
    v.set_visual("treemap");
    assert_eq!(v.render().draw.visual, "treemap");
}

#[test]
fn each_visualization_remembers_its_colour_mode() {
    fakes();
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    assert_eq!(v.color_mode(), ColorModeName::Class);
    v.set_color_mode(ColorModeName::Time);
    v.set_visual("test.fake");
    assert_eq!(v.color_mode(), ColorModeName::Flat);
    v.set_color_mode(ColorModeName::Branch);
    v.set_visual("test.rooted");
    assert_eq!(v.color_mode(), ColorModeName::Branch);
    v.set_visual("treemap");
    assert_eq!(v.color_mode(), ColorModeName::Time);
    v.set_visual("test.fake");
    assert_eq!(v.color_mode(), ColorModeName::Branch);
    let s = ViewSettings {
        visual: "test.fake".into(),
        color_mode: ColorModeName::Class,
        colors: BTreeMap::from([
            ("treemap".to_owned(), ColorModeName::Flat),
            ("test.fake".to_owned(), ColorModeName::Time),
        ]),
        ..ViewSettings::default()
    };
    let mut v2 = View::new("v2", v.ctx.clone(), s).unwrap();
    assert!(v2.visual == "test.fake" && v2.color_mode() == ColorModeName::Class);
    v2.set_visual("treemap");
    assert_eq!(v2.color_mode(), ColorModeName::Flat);
    let bad = ViewSettings {
        visual: "pie".into(),
        ..ViewSettings::default()
    };
    assert!(View::new("v3", v.ctx.clone(), bad).is_none());
}

#[test]
fn free_space_only_where_the_visualization_draws_it() {
    fakes();
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    assert!(keys(v.render()).contains(&KEY_FREE));
    v.set_visual("test.nofree");
    let k = keys(v.render());
    assert!(!k.contains(&KEY_FREE) && !k.contains(&KEY_UNKNOWN));
}

#[test]
fn compare_views_switch_too() {
    fakes();
    let mut v = plain(
        &dir([
            ("a", dir([("one.bin", S::F(10_000))])),
            ("new", dir([("big.iso", S::F(300_000))])),
        ]),
        &[],
    );
    let n = v.store().len();
    let delta: Vec<i64> = (0..n as i64).map(|i| i * 100 - 300).collect();
    let status: Vec<u8> = (0..n).map(|i| (i % 4) as u8).collect();
    let grow: Vec<i64> = {
        let r = v.store().read();
        (0..n as NodeId)
            .map(|i| r.effective_size(i).1 / 2)
            .collect()
    };
    let g2 = grow.clone();
    v.compare = Some(Arc::new(ArrayChanges::new(
        delta.clone(),
        status,
        move |d, _| {
            if d == CompareDirection::Grow {
                g2.clone()
            } else {
                vec![0; g2.len()]
            }
        },
    )));
    v.set_viewport(800.0, 600.0, 1.0);
    let first_keys = v.render().draw.key.clone();
    assert!(v.last.as_ref().unwrap().delta.is_some());
    for visual in ["test.rooted", "test.fake"] {
        v.direction = CompareDirection::All;
        v.set_visual(visual);
        let f = v.render();
        assert!(f.delta.is_some() && f.dstat.is_some());
        assert_eq!(f.draw.key, first_keys);
        assert_eq!(f.root.delta, Some(delta[1]));
        v.direction = CompareDirection::Grow;
        assert_eq!(v.render().root.size, grow[1]);
    }
}

#[test]
fn the_zoom_root_as_a_row() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    let total = v.store().read().tot_alloc(1);
    let root = v.render().root.clone();
    assert_eq!((root.key, root.node, root.kind), (1, 1, kind::FOLDER));
    assert!(root.size == total && root.flags == 0 && root.branch == 0);
    v.set_filter("*.mp4", None);
    assert!(v.render().root.flags & flag::MATCH != 0);
    v.filter_mode = FilterModeName::Hide;
    assert_eq!(v.render().root.size, 900 * MB);
    v.set_filter("*.nothing", None);
    assert!(v.render().root.flags & flag::DIM != 0);
    v.set_filter("", None);
    v.filter_mode = FilterModeName::Dim;
    let videos = node(&v, "Users/lenny/Videos");
    v.select(Some(videos));
    assert!(v.tag_selected(Some(TagColor::Red)));
    v.zoom_to_node(videos);
    let root = v.render().root.clone();
    assert!(root.tag == 1 && root.flags & flag::TAGGED != 0 && root.size == 1500 * MB);
    let temp = node(&v, "Users/lenny/Temp");
    v.zoom_to_node(temp);
    v.render();
    assert!(v.zoom_into(KEY_SMALL | temp));
    let root = v.render().root.clone();
    assert_eq!(
        (root.key, root.node, root.kind, root.count),
        (KEY_SMALL | temp, temp, kind::SMALL, 400)
    );
    assert_eq!(root.size, 400 * 5000);
}

#[test]
fn flashes_land_on_the_zoom_root_in_partition_layouts() {
    fakes();
    let mut v = plain(
        &dir([
            ("big.bin", S::F(40 * GB)),
            ("tiny.txt", S::F(1)),
            ("Other", dir([("o.bin", S::F(10 * GB))])),
        ]),
        &[],
    );
    let (tiny, other, o_bin) = (
        node(&v, "tiny.txt"),
        node(&v, "Other"),
        node(&v, "Other/o.bin"),
    );
    v.render();
    v.add_flash(&HashMap::from([(tiny, 4096)]));
    assert!(v.render().flash.is_empty());
    v.add_flash(&HashMap::from([(tiny, 4096), (o_bin, 10)]));
    v.set_visual("test.rooted");
    let f = v.render();
    let row = f.draw.row(o_bin).unwrap() as i64;
    assert_eq!(f.flash, [(-1, 4096), (row, 10)]);
    v.zoom_to_node(other);
    v.render();
    v.add_flash(&HashMap::from([(tiny, 5), (other, 7)]));
    assert_eq!(v.render().flash, [(-1, 7)]);
}

#[test]
fn a_selected_aggregate_falls_back_to_its_node() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    let (lenny, temp) = (node(&v, "Users/lenny"), node(&v, "Users/lenny/Temp"));
    v.zoom_to_node(lenny);
    let small = KEY_SMALL | temp;
    let row = v.render().draw.row(small);
    assert!(row.is_some());
    v.select(Some(small));
    assert_eq!(v.render().selected_row, row);
    v.set_detail(1);
    let f = v.render();
    assert!(f.draw.row(small).is_none() && f.selected_row == f.draw.row(temp));
    v.set_detail(5);
    v.zoom_to_node(temp);
    v.select(Some(small));
    v.min_element = 1.0;
    let f = v.render();
    assert!(f.draw.row(small).is_none() && f.selected_row.is_none());
    v.min_element = 14.0;
    v.zoom_to_node(lenny);
    v.select(Some(KEY_MORE | temp));
    let f = v.render();
    assert_eq!(f.selected_row, f.draw.row(temp));
    v.zoom_to_node(node(&v, "Users"));
    v.select(Some(KEY_FREE));
    assert!(v.render().selected_row.is_none());
}

fn top_slots(f: &Frame) -> BTreeMap<u32, u8> {
    (0..f.draw.len())
        .filter(|&r| f.draw.parent[r] < 0)
        .map(|r| (f.draw.key[r], f.branch[r]))
        .collect()
}

#[test]
fn branch_slots_are_stable_and_inherited() {
    fakes();
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    let f = v.render();
    let slots = top_slots(f);
    let real: BTreeMap<u32, u8> = slots
        .iter()
        .filter(|(k, _)| **k < KEY_SMALL)
        .map(|(k, s)| (*k, *s))
        .collect();
    assert!(!real.is_empty() && real.values().all(|&s| (1..=SLOTS).contains(&s)));
    assert_eq!((slots[&KEY_FREE], slots[&KEY_UNKNOWN]), (0, 0));
    let order: Vec<u8> = (0..f.draw.len())
        .filter(|&r| f.draw.parent[r] < 0 && f.draw.key[r] < KEY_SMALL)
        .map(|r| f.branch[r])
        .collect();
    assert!(order.windows(2).all(|w| w[0] != w[1]));
    for i in 0..f.draw.len() {
        let p = f.draw.parent[i];
        if p >= 0 {
            assert_eq!(f.branch[i], f.branch[p as usize]);
        }
    }
    v.set_viewport(700.0, 900.0, 1.5);
    v.set_visual("test.rooted");
    let again = top_slots(v.render());
    let kept: BTreeMap<u32, u8> = again
        .into_iter()
        .filter(|(k, _)| real.contains_key(k))
        .collect();
    assert_eq!(kept, real);
}

#[test]
fn branch_slots_reset_with_the_zoom_root_and_follow_remap() {
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    let users = node(&v, "Users");
    let first = top_slots(v.render());
    v.zoom_to_node(users);
    assert_eq!(
        top_slots(v.render()),
        BTreeMap::from([(node(&v, "Users/lenny"), 1)])
    );
    v.go_back();
    assert_eq!(top_slots(v.render()), first);
    let before = v.hues.slots();
    v.remap(&|i| Some(if i >= 2 { i + 1 } else { i }), 1);
    let want: HashMap<u32, u8> = before.iter().map(|(k, s)| (k + 1, *s)).collect();
    assert_eq!(v.hues.slots(), want);
    v.remap(&|_| None, 1);
    assert!(v.hues.slots().is_empty());
}

fn make_draw(keys: &[u32], kinds: &[u8]) -> DrawList {
    let mut d = DrawList::new();
    for (i, &k) in keys.iter().enumerate() {
        d.push(
            Row {
                key: k,
                node: k as i32,
                parent: -1,
                kind: kinds.get(i).copied().unwrap_or(0),
                depth: 1,
                cls: 0,
                label: 0,
                size: 1,
                count: 0,
                geo: [0; 4],
            },
            b"",
        );
    }
    d
}

#[test]
fn branch_slot_choice() {
    let mut hues = BranchHues::new();
    assert_eq!(
        hues.hues(
            1,
            &make_draw(&[10, 11, 12, KEY_SMALL | 1, 13], &[0, 1, 2, 4, 0])
        ),
        [1, 2, 3, 0, 4]
    );
    assert_eq!(
        hues.hues(1, &make_draw(&[10, 20, 11, 12, 13], &[])),
        [1, 5, 2, 3, 4]
    );
    let many: Vec<u32> = (30..30 + SLOTS as u32 + 2).collect();
    let got = hues.hues(1, &make_draw(&many, &[]));
    assert_eq!(got[..SLOTS as usize], (1..=SLOTS).collect::<Vec<_>>()[..]);
    assert_eq!(got[SLOTS as usize..], [1, 2]);
    let lots: Vec<u32> = (100..100 + LIMIT as u32 + 50).collect();
    hues.hues(1, &make_draw(&lots, &[]));
    let slots = hues.slots();
    assert_eq!(slots.len(), LIMIT);
    assert!(!slots.contains_key(&100) && slots.contains_key(&(100 + LIMIT as u32 + 49)));
    assert_eq!(hues.hues(2, &make_draw(&[100], &[])), [1]);
}

#[test]
fn branch_slots_wrap_around_like_the_rings() {
    let ks: Vec<u32> = (10..10 + SLOTS as u32 + 1).collect();
    let got = BranchHues::new().hues(1, &make_draw(&ks, &[]));
    let last = *got.last().unwrap();
    assert!(last != got[0] && last != got[got.len() - 2]);
}

#[test]
fn row_lookup_scans_then_indexes() {
    let ks = [7, 3, KEY_SMALL | 3, 9, 3, 1];
    let draw = make_draw(&ks, &[]);
    let probes = [3, 9, 1, 7, KEY_SMALL | 3, 4, 0, 1 << 31, 3, 9, 1, 5];
    let expect: Vec<Option<usize>> = probes
        .iter()
        .map(|p| ks.iter().position(|k| k == p))
        .collect();
    let got: Vec<Option<usize>> = probes.iter().map(|&p| draw.row(p)).collect();
    assert_eq!(got, expect);
    let again: Vec<Option<usize>> = probes.iter().map(|&p| draw.row(p)).collect();
    assert_eq!(again, expect);
}

// --------------------------------------------------------------------------- the registry

#[test]
fn the_treemap_is_built_in_and_registration_is_checked() {
    let tree = registry::get("treemap").unwrap();
    assert_eq!(
        (
            tree.layout,
            tree.capability.as_str(),
            tree.icon.as_str(),
            tree.default_color
        ),
        (DRAWLIST_LAYOUT, "", "vizTreemap", ColorModeName::Class)
    );
    assert_eq!(registry::all_visuals()[0].id, "treemap");
    assert!(registry::get("pie").is_none());
    let base = fake(
        "reg.base",
        Arc::new(TreemapEngine),
        DRAWLIST_LAYOUT,
        ColorModeName::Class,
    );
    let mut bad = base.clone();
    bad.id = "treemap".into();
    assert!(
        registry::register(bad)
            .unwrap_err()
            .to_string()
            .contains("already registered")
    );
    let mut bad = base.clone();
    bad.api = ENGINE_API + 1;
    assert!(
        registry::register(bad)
            .unwrap_err()
            .to_string()
            .contains("engine API")
    );
    for id in ["Test", &"a".repeat(49)] {
        let mut bad = base.clone();
        bad.id = id.to_owned();
        assert!(
            registry::register(bad)
                .unwrap_err()
                .to_string()
                .contains("lowercase")
        );
    }
    let mut bad = base.clone();
    bad.layout = "drawlist";
    assert!(
        registry::register(bad)
            .unwrap_err()
            .to_string()
            .contains("layout")
    );
}

#[test]
fn availability_follows_the_capabilities() {
    let plain_v = fake(
        "reg.plain",
        Arc::new(TreemapEngine),
        DRAWLIST_LAYOUT,
        ColorModeName::Class,
    );
    let mut gated = fake(
        "reg.gated",
        Arc::new(TreemapEngine),
        FLAME_LAYOUT,
        ColorModeName::Class,
    );
    gated.capability = CAP_LAYOUT_FLAME.into();
    gated.order = 91;
    registry::register(plain_v).unwrap();
    registry::register(gated).unwrap();
    let ids = |v: Vec<Visualization>| v.into_iter().map(|v| v.id).collect::<Vec<_>>();
    let base = ids(registry::available(&[CAP_BINARY_FRAMES]));
    assert_eq!(base[0], "treemap");
    assert!(base.contains(&"reg.plain".to_owned()) && !base.contains(&"reg.gated".to_owned()));
    let flame = ids(registry::available(&[CAP_LAYOUT_FLAME]));
    let (a, b) = (
        flame.iter().position(|i| i == "reg.plain").unwrap(),
        flame.iter().position(|i| i == "reg.gated").unwrap(),
    );
    assert!(a < b);
    assert_eq!(showable::<&str>("treemap", &[]).unwrap().id, "treemap");
    assert!(showable("reg.plain", &[CAP_LAYOUT_FLAME]).is_none());
    assert_eq!(
        showable("reg.plain", &[CAP_VISUALS]).unwrap().id,
        "reg.plain"
    );
    assert!(showable("reg.gated", &[CAP_VISUALS]).is_none());
    assert_eq!(
        showable("reg.gated", &[CAP_VISUALS, CAP_LAYOUT_FLAME])
            .unwrap()
            .id,
        "reg.gated"
    );
    assert!(showable("pie", &[CAP_VISUALS, CAP_LAYOUT_FLAME]).is_none());
    registry::unregister("reg.plain");
    registry::unregister("reg.gated");
}

// --------------------------------------------------------------------------- test_trash_view.py

#[test]
fn trash_paths() {
    for p in [
        r"C:\$Recycle.Bin",
        "D:/$RECYCLE.BIN",
        r"\\server\share\$Recycle.Bin",
        r"E:\RECYCLER",
        "/Users/lenny/.Trash",
        "/Volumes/USB/.Trashes",
        "/.Trashes",
        "/home/lenny/.local/share/Trash",
        "/media/lenny/USB/.Trash-1000",
    ] {
        assert!(is_trash_path(p, false), "{p}");
    }
    for p in [
        r"C:\Users\lenny\$Recycle.Bin",
        "/home/lenny/Trash",
        "/home/lenny/.Trash-abc",
        "/Users/lenny/Documents/.Trash",
        "/home/lenny/.local/share/Trash/files",
        "/mnt/usb/.Trash",
    ] {
        assert!(!is_trash_path(p, false), "{p}");
    }
    assert!(is_trash_path("/mnt/usb/.Trash", true));
}

fn in_trash(store: &Store, paths: &[&str]) -> Vec<bool> {
    let r = store.read();
    let ids: Vec<i64> = paths.iter().map(|p| find(&r, p) as i64).collect();
    TrashCache::new().in_trash(&r, &ids)
}

#[test]
fn nodes_in_the_bins_of_each_system() {
    let win = build_spec(
        &dir([
            (
                "$Recycle.Bin",
                dir([(
                    "S-1-5-21",
                    dir([("$RX1.mp4", S::F(700 * MB)), ("$IX1.mp4", S::F(544))]),
                )]),
            ),
            (
                "Users",
                dir([(
                    "lenny",
                    dir([
                        ("$Recycle.Bin", dir([("a.txt", S::F(10))])),
                        ("b.txt", S::F(10)),
                    ]),
                )]),
            ),
        ]),
        "C:\\",
        1,
        &[],
    );
    assert_eq!(
        in_trash(
            &win,
            &[
                "$Recycle.Bin",
                "$Recycle.Bin/S-1-5-21",
                "$Recycle.Bin/S-1-5-21/$RX1.mp4"
            ]
        ),
        [true; 3]
    );
    assert_eq!(
        in_trash(
            &win,
            &[
                "Users",
                "Users/lenny/$Recycle.Bin/a.txt",
                "Users/lenny/b.txt"
            ]
        ),
        [false; 3]
    );
    let posix = build_spec(
        &dir([
            (
                "Users",
                dir([(
                    "lenny",
                    dir([
                        (".Trash", dir([("old.dmg", S::F(5 * MB))])),
                        ("Documents", dir([(".Trash", dir([("x", S::F(1))]))])),
                    ]),
                )]),
            ),
            (
                "home",
                dir([(
                    "lenny",
                    dir([(
                        ".local",
                        dir([(
                            "share",
                            dir([(
                                "Trash",
                                dir([
                                    ("files", dir([("y", S::F(1))])),
                                    ("info", dir([("y.trashinfo", S::F(1))])),
                                ]),
                            )]),
                        )]),
                    )]),
                )]),
            ),
            (
                "media",
                dir([(
                    "usb",
                    dir([(".Trash-1000", dir([("files", dir([("z", S::F(1))]))]))]),
                )]),
            ),
        ]),
        "/",
        1,
        &[],
    );
    assert_eq!(
        in_trash(
            &posix,
            &[
                "Users/lenny/.Trash/old.dmg",
                "home/lenny/.local/share/Trash/files/y",
                "home/lenny/.local/share/Trash/info/y.trashinfo",
                "media/usb/.Trash-1000/files/z",
            ]
        ),
        [true; 4]
    );
    assert_eq!(
        in_trash(
            &posix,
            &[
                "Users/lenny/Documents/.Trash/x",
                "home/lenny/.local/share",
                "media/usb"
            ]
        ),
        [false; 3]
    );
    let r = posix.read();
    assert_eq!(
        TrashCache::new().in_trash(&r, &[-1, 0, r.len() as i64 + 5]),
        [false; 3]
    );
}

#[test]
fn a_scanned_bin_and_the_shared_trash_of_a_scanned_mount() {
    let bin_only = build_spec(
        &dir([("files", dir([("y", S::F(1))]))]),
        "/home/lenny/.local/share/Trash",
        1,
        &[],
    );
    assert_eq!(in_trash(&bin_only, &["files", "files/y"]), [true, true]);
    let mount = build_spec(
        &dir([
            (".Trash", dir([("1000", dir([("f", S::F(1))]))])),
            ("data", dir([("g", S::F(1))])),
        ]),
        "/mnt/usb",
        1,
        &[],
    );
    assert_eq!(
        in_trash(&mount, &[".Trash/1000/f", "data/g"]),
        [true, false]
    );
}

fn bin_spec() -> S {
    dir([
        (
            "$Recycle.Bin",
            dir([(
                "S-1-5-21",
                dir([("$RX1.mp4", S::F(700 * MB)), ("$RX2.iso", S::F(400 * MB))]),
            )]),
        ),
        (
            "Users",
            dir([(
                "lenny",
                dir([("Videos", dir([("trip.mp4", S::F(900 * MB))]))]),
            )]),
        ),
    ])
}

#[test]
fn frames_flag_the_bin_and_what_is_in_it() {
    let mut v = volume_view(2100 * MB, &bin_spec(), None);
    v.render();
    let f = v.last.as_ref().unwrap();
    let flagged = |path: &str, fl: u16| -> bool {
        let n = find(&v.store().read(), path) as i32;
        let f = v.last.as_ref().unwrap();
        let r = f.draw.node.iter().position(|&x| x == n).unwrap();
        f.flags[r] & fl != 0
    };
    let free_rows_clear = (0..f.draw.len())
        .filter(|&i| f.draw.node[i] < 0)
        .all(|i| f.flags[i] & flag::TRASH == 0);
    let root_clear = f.root.flags & flag::TRASH == 0;
    assert!(free_rows_clear && root_clear);
    assert!(
        flagged("$Recycle.Bin", flag::TRASH)
            && flagged("$Recycle.Bin/S-1-5-21/$RX1.mp4", flag::TRASH)
    );
    assert!(!flagged("Users", flag::TRASH) && !flagged("Users/lenny/Videos/trip.mp4", flag::TRASH));
    assert!(
        flagged("$Recycle.Bin", flag::TRASH_BIN)
            && !flagged("$Recycle.Bin/S-1-5-21", flag::TRASH_BIN)
    );
    assert!(v.zoom_to_node(node(&v, "$Recycle.Bin")));
    assert_eq!(
        v.render().root.flags & (flag::TRASH | flag::TRASH_BIN),
        flag::TRASH | flag::TRASH_BIN
    );
}

#[test]
fn every_bin_of_a_store_is_found() {
    let store = build_spec(
        &dir([
            (
                "$Recycle.Bin",
                dir([("S-1-5-21", dir([("$R1.mp4", S::F(700 * MB))]))]),
            ),
            (
                "Users",
                dir([(
                    "lenny",
                    dir([
                        ("$Recycle.Bin", dir([("a", S::F(1))])),
                        (
                            ".local",
                            dir([(
                                "share",
                                dir([("Trash", dir([("files", dir([("y", S::F(1))]))]))]),
                            )]),
                        ),
                        ("Videos", dir([("trip.mp4", S::F(900 * MB))])),
                    ]),
                )]),
            ),
            ("Trash", dir([("x", S::F(1))])),
        ]),
        "C:\\",
        1,
        &[],
    );
    let r = store.read();
    let found = TrashCache::new().find_bins(&r);
    let want = {
        let mut w = vec![
            find(&r, "$Recycle.Bin"),
            find(&r, "Users/lenny/.local/share/Trash"),
        ];
        w.sort_unstable();
        w
    };
    assert_eq!(found, want);
    let none = build_spec(
        &dir([("Videos", dir([("a.mp4", S::F(1))]))]),
        "D:\\",
        1,
        &[],
    );
    assert!(TrashCache::new().find_bins(&none.read()).is_empty());
}

#[test]
fn without_bins_takes_their_bytes_out() {
    let store = build_spec(
        &dir([
            (
                "$Recycle.Bin",
                dir([(
                    "S-1",
                    dir([
                        ("a.mp4", S::F(700 * MB)),
                        ("b", dir([("c.iso", S::F(300 * MB))])),
                    ]),
                )]),
            ),
            ("Users", dir([("v.mp4", S::F(900 * MB))])),
        ]),
        "C:\\",
        1,
        &[],
    );
    let r = store.read();
    let base = StoreSizes::of(&r);
    let bins = TrashCache::new().find_bins(&r);
    let sizes = WithoutBins::new(&base, &r, &bins);
    let bin = find(&r, "$Recycle.Bin");
    let inside: Vec<NodeId> = [
        "$Recycle.Bin/S-1",
        "$Recycle.Bin/S-1/a.mp4",
        "$Recycle.Bin/S-1/b/c.iso",
    ]
    .iter()
    .map(|p| find(&r, p))
    .collect();
    assert_eq!(sizes.size(&r, 1), base.size(&r, 1) - 1000 * MB);
    assert_eq!(sizes.size(&r, bin), 0);
    assert_eq!(sizes.size(&r, find(&r, "Users")), 900 * MB);
    assert!(inside.iter().all(|&i| sizes.size(&r, i) == 0));
    let mut want = vec![bin, find(&r, "$Recycle.Bin/S-1/b")];
    want.extend(&inside);
    want.sort_unstable();
    assert_eq!(subtree(&r, &[bin]), want);
}

#[test]
fn hiding_the_bin_takes_it_out_of_the_picture() {
    let s = dir([
        (
            "$Recycle.Bin",
            dir([("S-1-5-21", dir([("$RX1.mp4", S::F(700 * MB))]))]),
        ),
        (
            "Users",
            dir([("lenny", dir([("trip.mp4", S::F(900 * MB))]))]),
        ),
    ]);
    let mut v = volume_view(1700 * MB, &s, None);
    assert!(v.has_trash() && v.show_trash);
    let bin = node(&v, "$Recycle.Bin") as i32;
    assert!(v.render().draw.node.contains(&bin));
    v.show_trash = false;
    let f = v.render();
    assert!(!f.draw.node.contains(&bin));
    assert!(f.flags.iter().all(|&x| x & flag::TRASH == 0));
    let unknown = f.draw.size[f.draw.row(KEY_UNKNOWN).unwrap()];
    assert_eq!(unknown, 1700 * MB - v.store().read().tot_alloc(1));
    assert!(v.zoom_to_node(bin as NodeId));
    assert!(!v.render().draw.is_empty());
    let plain_v = volume_view(1700 * MB, &dir([("Users", dir([("a", S::F(1))]))]), None);
    assert!(!plain_v.has_trash());
}

/// The path of a node as the system writes it (what the user lists in 「隐藏的文件夹」).
fn path_of(v: &View, path: &str) -> String {
    let r = v.store().read();
    r.path_lossy(if path.is_empty() { 1 } else { find(&r, path) })
}

#[test]
fn hidden_folders_are_taken_out_of_the_picture() {
    let s = dir([
        (
            "$Recycle.Bin",
            dir([("S-1-5-21", dir([("$RX1.mp4", S::F(100 * MB))]))]),
        ),
        (
            "Users",
            dir([(
                "lenny",
                dir([
                    ("Music", dir([("a.flac", S::F(600 * MB))])),
                    (
                        "Games",
                        dir([
                            ("g.pak", S::F(300 * MB)),
                            ("Saves", dir([("s.dat", S::F(50 * MB))])),
                        ]),
                    ),
                    ("Library", dir([("cache.db", S::F(100 * MB))])),
                ]),
            )]),
        ),
    ]);
    let mut v = volume_view(1300 * MB, &s, None);
    let music = node(&v, "Users/lenny/Music");
    let games = node(&v, "Users/lenny/Games");
    // nothing listed: nothing offered, all drawn
    assert_eq!(v.hidden_folder_count(), 0);
    assert!(v.render().draw.node.contains(&(music as i32)));
    let drawn = |v: &View, n: NodeId| v.last.as_ref().unwrap().draw.node.contains(&(n as i32));
    let size_of = |v: &View, p: &str| v.last.as_ref().unwrap().draw.size[row_of(v, p)];
    let lenny_all = size_of(&v, "Users/lenny");

    // a folder inside a listed one counts once; a path the scan doesn't hold, and the scan's
    // root (hiding it would leave nothing), not at all
    let list = vec![
        path_of(&v, "Users/lenny/Music"),
        path_of(&v, "Users/lenny/Games/Saves"),
        path_of(&v, "Users/lenny/Games"),
        format!("{}-elsewhere", path_of(&v, "Users")),
        path_of(&v, ""),
    ];
    assert!(v.set_hidden_folders(list.clone()));
    assert!(!v.set_hidden_folders(list));
    assert_eq!(v.hidden_folder_count(), 2);
    assert!(v.hide_folders, "a view opens with them hidden");
    v.render();
    assert!(!drawn(&v, music) && !drawn(&v, games));
    // their bytes are off their ancestors once (Saves is inside Games)
    assert_eq!(size_of(&v, "Users/lenny"), lenny_all - 950 * MB);
    assert_eq!(
        size_of(&v, "Users/lenny"),
        size_of(&v, "Users/lenny/Library")
    );

    // the toggle brings them back
    v.hide_folders = false;
    v.render();
    assert!(drawn(&v, music) && drawn(&v, games));
    assert_eq!(size_of(&v, "Users/lenny"), lenny_all);

    // a view zoomed into a hidden folder still shows it
    v.hide_folders = true;
    assert!(v.zoom_to_node(music));
    v.render();
    assert!(drawn(&v, node(&v, "Users/lenny/Music/a.flac")));
    assert!(v.zoom_to_node(1));

    // with the recycle bin hidden as well, and listed too, it is taken out once
    let mut both = vec![
        path_of(&v, "$Recycle.Bin"),
        path_of(&v, "Users/lenny/Music"),
    ];
    both.push(path_of(&v, "Users/lenny/Music"));
    v.set_hidden_folders(both);
    v.show_trash = false;
    v.render();
    assert!(!drawn(&v, node(&v, "$Recycle.Bin")) && !drawn(&v, music));
    assert!(drawn(&v, games));
    assert_eq!(size_of(&v, "Users/lenny"), lenny_all - 600 * MB);

    // no list, nothing offered
    v.set_hidden_folders(Vec::new());
    assert_eq!(v.hidden_folder_count(), 0);
}

// --------------------------------------------------------------------------- the wire

#[test]
fn frames_encode_with_every_column() {
    use sk_protocol::binary::decode_frame;
    let mut v = volume_view(2200 * MB, &spec(false, false), None);
    v.set_filter("*.mp4", None);
    v.render();
    let f = v.last.as_ref().unwrap();
    let bytes = encode_frame(f, "v1", 7, None).unwrap();
    let d = decode_frame(&bytes).unwrap();
    assert_eq!(
        (
            d.header.stream.as_str(),
            d.header.layout.as_str(),
            d.header.seq
        ),
        ("view:v1", DRAWLIST_LAYOUT, 7)
    );
    for name in [
        "key", "node", "parent", "kind", "depth", "cls", "label", "size", "count", "x", "y", "w",
        "h", "flags", "tag", "tone", "branch", "name_len",
    ] {
        assert_eq!(d.column(name).unwrap().count, f.draw.len(), "{name}");
    }
    assert_eq!(
        d.column("size").unwrap().to_vec::<i64>().unwrap(),
        f.draw.size
    );
    let names = d.blob("names").unwrap();
    let lens = d.column("name_len").unwrap().to_vec::<u32>().unwrap();
    assert_eq!(lens.iter().sum::<u32>() as usize, names.len());
    let info = frame_info(&v, f, 7, "test");
    assert_eq!(
        (info.rows as usize, info.width, info.layout.as_str()),
        (f.draw.len(), 1200, DRAWLIST_LAYOUT)
    );
    assert!(info.root.is_none() && info.geometry.is_none());
    assert_eq!(info.filter.as_ref().unwrap().files, 1);
}
