//! Command palette: fuzzy scoring, command registry, path search, recents (port of the non-server
//! parts of packages/backend/tests/test_palette.py).

use sk_core::filter::NameIndex;
use sk_core::i18n::{self, Language};
use sk_core::palette::commands::{Context, ShownVisual, ViewFacts, command, evaluate, match_command, registry, syllables};
use sk_core::palette::fuzzy::{score, utf16_spans};
use sk_core::palette::paths::{self as ppaths, SearchOptions};
use sk_core::layout::{ArraySizes, SizeSource};
use sk_core::palette::recents::Recents;
use sk_protocol::palette::{PaletteTab, PathState};
use sk_store::testing::{Spec, build_with, d, f, find};
use sk_store::{Entry, EntryBatch, Flags, Kind, Nodes, Store, Times};

const MB: i64 = 1 << 20;
const ROOT: &str = if cfg!(windows) { "C:\\" } else { "/" };
const SEP: char = std::path::MAIN_SEPARATOR;

fn spec() -> Spec {
    d([
        ("Program Files", d([("Docker", d([("dockerd.exe", f(30 * MB))])), ("Git", d([("git.exe", f(5 * MB))]))])),
        ("ProgramData", d([("Docker", d([("state.db", f(10 * MB))]))])),
        (
            "Users",
            d([(
                "lenny",
                d([
                    ("AppData", d([("Local", d([("Docker", d([("wsl", d([("disk", d([("docker_data.vhdx", f(600 * MB))]))]))]))]))])),
                    ("Videos", d([("trip.mp4", f(90 * MB)), ("旅行 记录.mov", f(20 * MB))])),
                    ("dark.txt", f(MB)),
                ]),
            )]),
        ),
    ])
}

/// The area basis of every node, as the view's layout sizes it.
fn sizes(n: &Nodes) -> ArraySizes {
    ArraySizes(std::sync::Arc::new((0..n.len() as u32).map(|i| n.effective_size(i).1).collect()))
}

fn tree() -> Store {
    build_with(&spec(), ROOT, 1, &[])
}

fn cmd(id: &str) -> &'static sk_core::palette::Command {
    command(id).unwrap()
}

fn s(ok: bool, text: &str) -> (bool, String) {
    (ok, text.to_owned())
}

// ------------------------------------------------------------------ fuzzy

#[test]
fn fuzzy_subsequence_and_hits() {
    assert_eq!(score("dkr", "Docker").unwrap().1, [(0, 1), (3, 4), (5, 6)]);
    assert!(score("dkx", "Docker").is_none());
    assert_eq!(score("", "anything"), Some((0, vec![])));
}

#[test]
fn fuzzy_prefers_exact_prefix_runs_and_word_starts() {
    let s = |q: &str, t: &str| score(q, t).unwrap().0;
    assert!(s("docker", "Docker") > s("docker", "dockerd.exe"));
    assert!(s("docker", "dockerd.exe") > s("docker", "my_docker_data"));
    assert!(s("sn", "save snapshot") > s("sn", "versions"));
    assert!(s("abc", "abc_x") > s("abc", "a_b_c"));
}

#[test]
fn utf16_spans_count_astral_characters_twice() {
    assert_eq!(utf16_spans("a😀b", &[(2, 3)]), [(3, 4)]);
    assert_eq!(utf16_spans("plain", &[(1, 3)]), [(1, 3)]);
}

// ------------------------------------------------------------------ commands

#[test]
fn pinned_commands_in_spec_order() {
    let mut pinned: Vec<_> = registry().iter().filter(|c| c.pinned > 0).collect();
    pinned.sort_by_key(|c| c.pinned);
    let names: Vec<&str> = pinned.iter().map(|c| c.name).collect();
    assert_eq!(
        names,
        ["重新扫描当前视图", "全盘重新扫描", "切换配色", "显示/隐藏可用空间", "打开删除篮", "保存快照", "与快照对比…", "导出报告…", "设置"]
    );
    let mut ids: Vec<&str> = registry().iter().map(|c| c.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), registry().len());
}

#[test]
fn every_name_has_its_pinyin() {
    for c in registry() {
        let syl = syllables(c.name).unwrap_or_else(|| panic!("{} is missing from pinyin.json", c.name));
        assert_eq!(syl.len(), c.name.chars().count(), "{}", c.name);
    }
}

#[test]
fn command_matching_by_name_pinyin_and_alias() {
    let _g = i18n::TEST_LOCK.lock();
    assert_eq!(match_command(cmd("view.cycle_color"), "qhps").unwrap().1, [(0, 4)]);
    assert_eq!(match_command(cmd("view.cycle_color"), "peise").unwrap().1, [(2, 4)]);
    assert!(match_command(cmd("view.toggle_unknown"), "未知").is_some());
    let settings = match_command(cmd("app.settings"), "settings").unwrap();
    assert!(settings.1.is_empty()); // an alias: no hits on the name
    assert!(match_command(cmd("app.settings"), "zzz").is_none());
    let found: Vec<&str> = registry().iter().filter(|c| match_command(c, "快照").is_some()).map(|c| c.id).collect();
    for id in ["snapshot.save", "snapshot.compare", "snapshot.open"] {
        assert!(found.contains(&id), "{id}");
    }
}

#[test]
fn moving_tabs_is_always_available() {
    let (left, right) = (cmd("tab.move_left"), cmd("tab.move_right"));
    assert_eq!((left.keys, right.keys), (&["Ctrl+Shift+PageUp"][..], &["Ctrl+Shift+PageDown"][..]));
    assert!(evaluate(left, &Context::new(PaletteTab::Start)).0);
    assert!(evaluate(right, &Context::new(PaletteTab::Scan)).0);
}

fn view() -> ViewFacts {
    ViewFacts {
        color_mode: "class".into(),
        visual: "treemap".into(),
        show_free: true,
        can_go_up: false,
        ..ViewFacts::default()
    }
}

fn scan_ctx(view: ViewFacts) -> Context {
    Context { has_scan: true, view: Some(view), ..Context::new(PaletteTab::Scan) }
}

#[test]
fn availability_and_status() {
    let _g = i18n::TEST_LOCK.lock();
    let start = Context::new(PaletteTab::Start);
    assert_eq!(evaluate(cmd("snapshot.save"), &start), s(false, "需要先完成扫描"));
    assert_eq!(evaluate(cmd("app.settings"), &start), s(true, ""));
    let running = Context { scanning: true, ..scan_ctx(view()) };
    assert_eq!(evaluate(cmd("snapshot.save"), &running), s(false, "扫描进行中"));
    assert!(evaluate(cmd("scan.stop"), &running).0);
    assert_eq!(evaluate(cmd("scan.pause"), &running), s(true, ""));
    assert_eq!(evaluate(cmd("scan.resume"), &running), s(false, "扫描没有暂停"));
    let paused = Context { paused: true, ..running.clone() };
    assert_eq!(evaluate(cmd("scan.pause"), &paused), s(false, "已暂停"));
    assert_eq!(evaluate(cmd("scan.resume"), &paused), s(true, ""));
    let mft = Context { method: "mft".into(), ..running.clone() };
    assert_eq!(evaluate(cmd("scan.pause"), &mft), s(false, "MFT 快速扫描几秒内完成，无需暂停"));
    assert_eq!(evaluate(cmd("scan.pause"), &start), s(false, "没有进行中的扫描"));
    let done = Context { elapsed_s: Some(4.8), basket: 2, ..scan_ctx(view()) };
    assert_eq!(evaluate(cmd("scan.rescan_all"), &done), s(true, "硬扫描 · 约 5 秒"));
    let mft_done = Context { method: "mft".into(), elapsed_s: Some(9.3), ..scan_ctx(view()) };
    assert_eq!(evaluate(cmd("scan.rescan_all"), &mft_done), s(true, "MFT 快速扫描 · 约 9 秒"));
    assert_eq!(evaluate(cmd("view.cycle_color"), &done), s(true, "当前：文件类别"));
    assert_eq!(evaluate(cmd("view.toggle_free"), &done), s(true, "当前：显示"));
    assert_eq!(evaluate(cmd("basket.open"), &done), s(true, "2 项"));
    assert_eq!(evaluate(cmd("snapshot.compare"), &done), s(false, "没有快照"));
    assert_eq!(evaluate(cmd("item.copy_path"), &done), s(false, "没有选中项"));
    assert_eq!(evaluate(cmd("nav.back"), &done), s(false, "没有可后退的位置"));
    assert_eq!(evaluate(cmd("item.preview"), &Context { view: Some(ViewFacts { selected: Some(3), ..view() }), ..done.clone() }), s(false, "还没有实现（见 TODO）"));
    let selected = Context { view: Some(ViewFacts { selected: Some(3), ..view() }), ..done.clone() };
    assert_eq!(evaluate(cmd("item.copy_path"), &selected), s(true, ""));
    assert_eq!(evaluate(cmd("view.rescan"), &selected), s(true, ""));
    let comparing = Context { view: Some(ViewFacts { compare: true, ..view() }), ..done.clone() };
    assert_eq!(evaluate(cmd("view.rescan"), &comparing), s(false, "对比时不可用，先退出对比"));
    assert_eq!(evaluate(cmd("snapshot.save"), &comparing), s(true, ""));
}

fn visual(id: &str, title: &str) -> ShownVisual {
    ShownVisual { id: id.into(), title: title.into(), compare: true }
}

#[test]
fn visualization_commands() {
    let _g = i18n::TEST_LOCK.lock();
    let picks = ["view.visual.treemap", "view.visual.flame", "view.visual.flame_polar", "view.color.branch"];
    for id in std::iter::once("view.cycle_visual").chain(picks) {
        assert_eq!(cmd(id).pinned, 0, "{id}");
    }
    assert!(picks.iter().all(|id| cmd(id).typed_only));
    let cycle = cmd("view.cycle_visual");
    assert!(cycle.keys == ["V"] && !cycle.typed_only && cycle.icon == "vizFlamePolar");
    assert!(match_command(cycle, "qhksh").is_some() && match_command(cycle, "flame").is_some());
    assert!(match_command(cmd("view.visual.flame_polar"), "polar").is_some());
    let all = vec![visual("treemap", "树图"), visual("flame", "火焰图"), visual("flame_polar", "火焰-极坐标图")];
    let full = Context { caps: vec![sk_protocol::CAP_VISUALS.into()], visuals: all.clone(), ..scan_ctx(view()) };
    let old = Context { visuals: vec![visual("treemap", "树图")], ..scan_ctx(view()) }; // without visuals.v1
    assert_eq!(evaluate(cmd("view.visual.treemap"), &full), s(true, "当前"));
    assert_eq!(evaluate(cmd("view.visual.treemap"), &old), s(true, "当前"));
    assert_eq!(evaluate(cmd("view.color.branch"), &full), s(true, ""));
    assert_eq!(evaluate(cmd("view.color.branch"), &old), s(false, "这种配色在当前界面不可用"));
    assert_eq!(evaluate(cmd("view.visual.flame"), &old), s(false, "这种可视化在当前界面不可用"));
    assert!(evaluate(cmd("view.visual.flame"), &full).0);
    assert_eq!(evaluate(cmd("view.cycle_visual"), &old), s(false, "这种可视化在当前界面不可用"));
    assert_eq!(evaluate(cmd("view.color.class"), &Context::new(PaletteTab::Start)), s(false, "需要先完成扫描"));
    // a visualization registered by a plug-in: its own title
    let mut with_pie = full.clone();
    with_pie.visuals.push(visual("test.pie", "测试图"));
    assert_eq!(evaluate(cmd("view.cycle_visual"), &with_pie), s(true, "当前：树图"));
    with_pie.view.as_mut().unwrap().visual = "test.pie".into();
    assert_eq!(evaluate(cmd("view.cycle_visual"), &with_pie), s(true, "当前：测试图"));
    assert_eq!(evaluate(cmd("view.visual.treemap"), &with_pie), s(true, ""));
    with_pie.view.as_mut().unwrap().color_mode = "branch".into();
    assert_eq!(evaluate(cmd("view.color.branch"), &with_pie), s(true, "当前"));
    assert_eq!(evaluate(cmd("view.cycle_color"), &with_pie), s(true, "当前：顶层分支"));
    // only visualizations that can compare while comparing
    let comparing = Context {
        view: Some(ViewFacts { compare: true, ..view() }),
        visuals: vec![visual("treemap", "树图"), ShownVisual { compare: false, ..visual("flame", "火焰图") }],
        ..full.clone()
    };
    assert_eq!(evaluate(cmd("view.cycle_visual"), &comparing), s(false, "这种可视化在当前界面不可用"));
    i18n::set_language(Language::En);
    let english = evaluate(cmd("view.cycle_visual"), &full);
    let hit = match_command(cmd("view.cycle_color"), "qhps");
    i18n::set_language(Language::ZhCn);
    assert_eq!(english, s(true, "Current: Treemap"));
    assert!(hit.is_some_and(|h| h.1.is_empty()), "pinyin still finds it, no hits on the English name");
}

// ------------------------------------------------------------------ paths

#[test]
fn search_tiers_then_size() {
    let store = tree();
    let n = store.read();
    let (sz, index) = (sizes(&n), NameIndex::new());
    let (hits, total) = ppaths::search(&n, &index, &sz, "docker", 10, SearchOptions::default());
    let names: Vec<(&str, u8)> = hits.iter().map(|h| (h.name.as_str(), h.tier)).collect();
    assert_eq!(names[..3], [("Docker", 0), ("Docker", 0), ("Docker", 0)]);
    let allocs: Vec<i64> = hits[..3].iter().map(|h| h.alloc).collect();
    let mut sorted = allocs.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(allocs, sorted);
    assert!(names.contains(&("dockerd.exe", 1)) && names.contains(&("docker_data.vhdx", 1)));
    assert!(total == hits.len() && total == 5);
    let vhdx = hits.iter().find(|h| h.name == "docker_data.vhdx").unwrap();
    assert!(vhdx.hits == [(0, 6)] && !vhdx.is_dir && vhdx.complete.ends_with("docker_data.vhdx"));
    assert!(hits[0].is_dir && hits[0].complete.ends_with(SEP));
}

#[test]
fn search_scattered_letters_and_exclusion() {
    let store = tree();
    let n = store.read();
    let (sz, index) = (sizes(&n), NameIndex::new());
    let (hits, _) = ppaths::search(&n, &index, &sz, "dkr", 10, SearchOptions::default());
    assert!(!hits.is_empty() && hits.iter().all(|h| h.tier == 3));
    assert_eq!(hits[0].alloc, 600 * MB);
    let seed = find(&n, "Users/lenny/AppData/Local/Docker");
    let opts = SearchOptions { exclude: Some(seed), contains_only: true, literal: false };
    let (hits, total) = ppaths::search(&n, &index, &sz, "Docker", 5, opts);
    assert!(!hits.iter().any(|h| h.node == seed) && total == 4);
    assert!(hits.iter().all(|h| h.tier <= 2));
}

#[test]
fn search_non_ascii_and_glob() {
    let store = tree();
    let n = store.read();
    let (sz, index) = (sizes(&n), NameIndex::new());
    let (hits, _) = ppaths::search(&n, &index, &sz, "旅行", 5, SearchOptions::default());
    assert_eq!(hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["旅行 记录.mov"]);
    assert_eq!(hits[0].hits, [(0, 2)]);
    let (hits, _) = ppaths::search(&n, &index, &sz, "*.exe", 5, SearchOptions::default());
    let mut names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["dockerd.exe", "git.exe"]);
    let (hits, _) = ppaths::search(&n, &index, &sz, "旅记", 5, SearchOptions::default());
    assert_eq!(hits.iter().map(|h| (h.name.as_str(), h.tier)).collect::<Vec<_>>(), [("旅行 记录.mov", 3)]);
}

#[test]
fn inside_hits_keep_offsets_of_the_original_name() {
    // "İ".lower() is two characters: offsets found in a Unicode-lowered name would drift
    let store = build_with(&d([("İİ notes docs.txt", f(1)), ("İ😀docs", f(2))]), ROOT, 1, &[]);
    let n = store.read();
    let (hits, _) = ppaths::search(&n, &NameIndex::new(), &sizes(&n), "DOCS", 10, SearchOptions::default());
    type Got = Vec<(String, u8, Vec<(usize, usize)>)>;
    let mut got: Got = hits.into_iter().map(|h| (h.name, h.tier, h.hits)).collect();
    got.sort();
    assert_eq!(got, [("İİ notes docs.txt".into(), 2, vec![(9, 13)]), ("İ😀docs".into(), 2, vec![(2, 6)])]);
    assert_eq!(utf16_spans("İ😀docs", &[(2, 6)]), [(3, 7)]);
}

#[test]
fn seed_is_searched_literally() {
    let names = [("what?.txt", 1), ("whatX.txt", 2), ("what?.txt.bak", 3), ("中?.txt", 4), ("中x.txt", 5)];
    let spec = Spec::Dir(names.iter().map(|(n, s)| (n.to_string(), Spec::File(*s))).collect());
    // '?' cannot be in a Windows file name, but the store does not care
    let store = build_with(&spec, ROOT, 1, &[]);
    let n = store.read();
    let (sz, index) = (sizes(&n), NameIndex::new());
    let lit = SearchOptions { contains_only: true, literal: true, exclude: None };
    let (hits, total) = ppaths::search(&n, &index, &sz, "what?.txt", 10, lit);
    type Got<'a> = Vec<(&'a str, u8, Vec<(usize, usize)>)>;
    let got: Got = hits.iter().map(|h| (h.name.as_str(), h.tier, h.hits.clone())).collect();
    assert_eq!(got, [("what?.txt", 0, vec![(0, 9)]), ("what?.txt.bak", 1, vec![(0, 9)])]);
    assert_eq!(total, 2);
    let (hits, _) = ppaths::search(&n, &index, &sz, "中?.txt", 10, lit);
    assert_eq!(hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["中?.txt"]);
    let (hits, _) = ppaths::search(&n, &index, &sz, "what?.txt", 10, SearchOptions::default());
    let mut names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["what?.txt", "whatX.txt"]);
}

#[test]
fn seed_text_of_scan_roots() {
    let roots: [&str; 3] = if cfg!(windows) { ["D:\\Photos", "E:\\Music", ROOT] } else { ["/data/Photos", "/mnt/Music", ROOT] };
    let store = Store::new();
    let first = {
        let mut app = store.appender();
        for r in roots {
            app.add_root(r.as_bytes(), Kind::Dir, Flags::NONE, Times::default()).unwrap();
        }
        let mut b = EntryBatch::new();
        b.push(Entry::file(b"a.jpg", 0, 0));
        app.ingest_listing(1, &b).unwrap()
    };
    let n = store.read();
    assert_eq!(n.roots().map(|r| ppaths::seed_text(&n, r)).collect::<Vec<_>>(), ["Photos", "Music", ""]);
    assert_eq!(ppaths::seed_text(&n, first), "a.jpg");
}

#[test]
fn children_listing_and_completion() {
    let store = tree();
    let n = store.read();
    let sz = sizes(&n);
    let lenny = find(&n, "Users/lenny");
    let (hits, total) = ppaths::children(&n, &sz, lenny, "", 10);
    assert_eq!(hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["AppData", "Videos", "dark.txt"]);
    assert_eq!(total, 3);
    let (hits, _) = ppaths::children(&n, &sz, lenny, "vi", 10);
    assert_eq!(hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["Videos"]);
    let base = format!("{ROOT}Users{SEP}lenny{SEP}");
    let some = Some(&sz as &dyn ppaths::Sizes);
    assert_eq!(ppaths::completion(Some(&n), some, &format!("{base}Vi")), Some(format!("{base}Videos{SEP}")));
    assert_eq!(ppaths::completion(Some(&n), some, &format!("{base}da")), Some(format!("{base}dark.txt")));
    assert_eq!(ppaths::completion(Some(&n), some, &format!("{base}zz")), None);
}

#[test]
fn nodes_listed_after_the_sizes_count_as_not_scanned() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("Videos").join("new")).unwrap();
    let root = tmp.path().to_str().unwrap();
    let store = build_with(&d([("Videos", d([])), ("old.txt", f(5))]), root, 1, &["Videos"]);
    let (sz, videos) = {
        let n = store.read();
        (sizes(&n), find(&n, "Videos"))
    };
    {
        // the scan lists Videos after the sizes were taken
        let mut app = store.appender();
        let mut b = EntryBatch::new();
        b.push(Entry::dir(b"new"));
        b.push(Entry::file(b"trip.mp4", 0, 0));
        app.ingest_listing(videos, &b).unwrap();
    }
    let n = store.read();
    assert!(n.len() > sz.n());
    let base = format!("{}{SEP}", tmp.path().join("Videos").display());
    let some = Some(&sz as &dyn ppaths::Sizes);
    assert_eq!(ppaths::children(&n, &sz, videos, "", 10), (vec![], 0));
    assert_eq!(ppaths::children(&n, &sz, videos, "tr", 10), (vec![], 0));
    assert_eq!(ppaths::completion(Some(&n), some, &format!("{base}tr")), None);
    let r = ppaths::resolve(Some(&n), some, &format!("{base}new"));
    assert_eq!((r.state, r.node, r.is_dir, r.alloc), (PathState::Outside, None, true, None));
    let r = ppaths::resolve(Some(&n), some, &format!("{base}trip.mp4"));
    assert_eq!((r.state, r.node), (PathState::Missing, None));
    let sz = sizes(&n); // sized: in the scan
    let some = Some(&sz as &dyn ppaths::Sizes);
    let mut names: Vec<String> = ppaths::children(&n, &sz, videos, "", 10).0.into_iter().map(|h| h.name).collect();
    names.sort();
    assert_eq!(names, ["new", "trip.mp4"]);
    assert_eq!(ppaths::completion(Some(&n), some, &format!("{base}tr")), Some(format!("{base}trip.mp4")));
    assert_eq!(ppaths::resolve(Some(&n), some, &format!("{base}new")).state, PathState::InScan);
}

#[test]
fn path_input() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tree();
    let n = store.read();
    let sz = sizes(&n);
    let some = Some(&sz as &dyn ppaths::Sizes);
    for text in ["C:\\x", "c:/", "\\\\server\\share", "/usr", "~"] {
        assert!(ppaths::looks_like_path(text), "{text}");
    }
    for text in ["docker", "\\temp", "*.iso", ">1gb"] {
        assert!(!ppaths::looks_like_path(text), "{text}");
    }
    let r = ppaths::resolve(Some(&n), some, &format!("{ROOT}Users{SEP}lenny{SEP}Videos"));
    assert!(r.state == PathState::InScan && r.is_dir && r.alloc == Some(110 * MB));
    let r = ppaths::resolve(Some(&n), some, tmp.path().to_str().unwrap());
    assert!(r.state == PathState::Outside && r.is_dir);
    let r = ppaths::resolve(Some(&n), some, tmp.path().join("nope").to_str().unwrap());
    assert_eq!(r.state, PathState::Missing);
    std::fs::create_dir(tmp.path().join("Videos")).unwrap();
    let typed = format!("{}{SEP}vi", tmp.path().display());
    assert_eq!(ppaths::completion(None, None, &typed), Some(format!("{}{SEP}", tmp.path().join("Videos").display())));
    assert_eq!(ppaths::trimmed("C:\\Users\\"), if cfg!(windows) { "C:\\Users" } else { "C:\\Users\\" });
    assert_eq!(ppaths::trimmed("/"), "/");
    assert_eq!(ppaths::split_last(&format!("a{SEP}b{SEP}Vi")), (format!("a{SEP}b{SEP}").as_str(), "Vi"));
}

// ------------------------------------------------------------------ recents

#[test]
fn recents_order_dedupe_clear_and_persistence() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("recent.json");
    let r = Recents::open(&file);
    r.add_filter("*.iso;>1gb").unwrap();
    r.add_jump("C-abc", "C:\\Users\\lenny\\Videos").unwrap();
    r.add_filter("*.mp4").unwrap();
    r.add_filter("*.iso;>1gb").unwrap(); // again: moves to the front, no duplicate
    let texts = |e: Vec<sk_core::palette::recents::Entry>| e.into_iter().map(|e| e.text).collect::<Vec<_>>();
    assert_eq!(texts(Recents::open(&file).entries(Some("C-abc"))), ["*.iso;>1gb", "*.mp4", "C:\\Users\\lenny\\Videos"]);
    assert_eq!(texts(r.entries(Some("D-xyz"))), ["*.iso;>1gb", "*.mp4"]);
    assert_eq!(texts(r.entries(None)), ["*.iso;>1gb", "*.mp4"]);
    if cfg!(windows) {
        r.add_jump("C-abc", "c:\\users\\lenny\\Videos\\").unwrap(); // the same place
        assert_eq!(texts(r.entries(Some("C-abc"))).len(), 3);
    }
    r.clear(Some("C-abc")).unwrap();
    assert!(Recents::open(&file).entries(Some("C-abc")).is_empty());
    std::fs::write(&file, "{broken").unwrap();
    assert!(Recents::open(&file).entries(Some("C-abc")).is_empty());
}
