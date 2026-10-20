"""Command palette (W4): fuzzy scoring, command registry, path search, recents."""

from __future__ import annotations

import os
import sys
from pathlib import Path

import numpy as np
import pytest

from spaceknife import i18n
from spaceknife.layout import registry as visuals
from spaceknife.layout.api import ENGINE_API, Visualization
from spaceknife.layout.engine import Layout, TreemapEngine
from spaceknife.palette import paths as ppaths
from spaceknife.palette.commands import Context, evaluate, match, registry
from spaceknife.palette.fuzzy import score, utf16_spans
from spaceknife.palette.recents import Recents
from spaceknife.store.nodes import ENTRY_COLUMNS, Kind, NodeStore
from spaceknife.view import TagStore, View
from spaceknife.view.state import RootInfoLike
from spaceknife_protocol import ALL_CAPABILITIES

from .store_builder import build, find

MB = 1 << 20
ROOT = "C:\\" if sys.platform == "win32" else "/"  # a volume root of this platform

SPEC: dict[str, object] = {
    "Program Files": {"Docker": {"dockerd.exe": 30 * MB}, "Git": {"git.exe": 5 * MB}},
    "ProgramData": {"Docker": {"state.db": 10 * MB}},
    "Users": {
        "lenny": {
            "AppData": {"Local": {"Docker": {"wsl": {"disk": {"docker_data.vhdx": 600 * MB}}}}},
            "Videos": {"trip.mp4": 90 * MB, "旅行 记录.mov": 20 * MB},
            "dark.txt": 1 * MB,
        }
    },
}


# ------------------------------------------------------------------ fuzzy


def test_fuzzy_subsequence_and_hits() -> None:
    got = score("dkr", "Docker")
    assert got is not None and got[1] == [(0, 1), (3, 4), (5, 6)]
    assert score("dkx", "Docker") is None
    assert score("", "anything") == (0, [])


def test_fuzzy_prefers_exact_prefix_runs_and_word_starts() -> None:
    def s(q: str, t: str) -> int:
        got = score(q, t)
        assert got is not None
        return got[0]

    assert s("docker", "Docker") > s("docker", "dockerd.exe") > s("docker", "my_docker_data")
    assert s("sn", "save snapshot") > s("sn", "versions")  # word start beats a middle hit
    assert s("abc", "abc_x") > s("abc", "a_b_c")  # a run beats scattered letters


def test_utf16_spans_count_astral_characters_twice() -> None:
    assert utf16_spans("a😀b", [(2, 3)]) == [(3, 4)]
    assert utf16_spans("plain", [(1, 3)]) == [(1, 3)]


# ------------------------------------------------------------------ commands


def test_pinned_commands_in_spec_order() -> None:
    pinned = sorted((c for c in registry() if c.pinned), key=lambda c: c.pinned)
    assert [c.name for c in pinned] == [
        "重新扫描当前视图",
        "全盘重新扫描",
        "切换配色",
        "显示/隐藏可用空间",
        "打开删除篮",
        "保存快照",
        "与快照对比…",
        "导出报告…",
        "设置",
    ]
    assert len({c.id for c in registry()}) == len(registry())


def test_command_matching_by_name_pinyin_and_alias() -> None:
    by_id = {c.id: c for c in registry()}
    hit = match(by_id["view.cycle_color"], "qhps")
    assert hit is not None and hit[1] == [(0, 4)]  # initials light up the four characters
    hit = match(by_id["view.cycle_color"], "peise")
    assert hit is not None and hit[1] == [(2, 4)]
    assert match(by_id["view.toggle_unknown"], "未知") is not None
    assert match(by_id["app.settings"], "settings") == (match(by_id["app.settings"], "settings") or (0, []))
    assert match(by_id["app.settings"], "zzz") is None
    found = [c.id for c in registry() if match(c, "快照")]
    assert {"snapshot.save", "snapshot.compare", "snapshot.open"} <= set(found)


def test_availability_and_status(tmp_path: Path) -> None:
    by_id = {c.id: c for c in registry()}
    start = Context(tab="start")
    assert evaluate(by_id["snapshot.save"], start) == (False, "需要先完成扫描")
    assert evaluate(by_id["app.settings"], start) == (True, "")
    store = build(SPEC, root=ROOT)
    view = View("v", store, [RootInfoLike(node=1, whole_volume=False)], tags=TagStore(tmp_path / "t.json"))
    running = Context(tab="scan", scanning=True, has_scan=True, view=view)
    assert evaluate(by_id["snapshot.save"], running) == (False, "扫描进行中")
    assert evaluate(by_id["scan.stop"], running)[0]
    assert evaluate(by_id["scan.pause"], running) == (True, "")
    assert evaluate(by_id["scan.resume"], running) == (False, "扫描没有暂停")
    paused = Context(tab="scan", scanning=True, paused=True, has_scan=True, view=view)
    assert evaluate(by_id["scan.pause"], paused) == (False, "已暂停")
    assert evaluate(by_id["scan.resume"], paused) == (True, "")
    reading_mft = Context(tab="scan", scanning=True, method="mft", has_scan=True, view=view)
    assert evaluate(by_id["scan.pause"], reading_mft) == (False, "MFT 快速扫描几秒内完成，无需暂停")
    assert evaluate(by_id["scan.pause"], start) == (False, "没有进行中的扫描")
    done = Context(tab="scan", has_scan=True, view=view, elapsed_s=4.8, basket=2)
    assert evaluate(by_id["scan.rescan_all"], done) == (True, "硬扫描 · 约 5 秒")
    mft_done = Context(tab="scan", has_scan=True, method="mft", view=view, elapsed_s=9.3)
    assert evaluate(by_id["scan.rescan_all"], mft_done) == (True, "MFT 快速扫描 · 约 9 秒")
    assert evaluate(by_id["view.cycle_color"], done) == (True, "当前：文件类别")
    assert evaluate(by_id["view.toggle_free"], done) == (True, "当前：显示")
    assert evaluate(by_id["basket.open"], done) == (True, "2 项")
    assert evaluate(by_id["snapshot.compare"], done) == (False, "没有快照")
    assert evaluate(by_id["item.copy_path"], done) == (False, "没有选中项")
    assert evaluate(by_id["nav.back"], done) == (False, "没有可后退的位置")
    view.select(find(store, "Users"))
    assert evaluate(by_id["item.copy_path"], done) == (True, "")


def test_visualization_commands(tmp_path: Path) -> None:
    by_id = {c.id: c for c in registry()}
    picks = ("view.visual.treemap", "view.visual.flame", "view.visual.flame_polar", "view.color.branch")
    for cid in ("view.cycle_visual", *picks):
        assert not by_id[cid].pinned, cid
    assert all(by_id[cid].typed_only for cid in picks)
    cycle = by_id["view.cycle_visual"]
    assert cycle.keys == ("V",) and not cycle.typed_only and cycle.icon == "vizFlamePolar"
    assert match(cycle, "qhksh") is not None and match(cycle, "flame") is not None
    assert match(by_id["view.visual.flame_polar"], "polar") is not None
    store = build(SPEC, root=ROOT)
    view = View("v", store, [RootInfoLike(node=1, whole_volume=False)], tags=TagStore(tmp_path / "t.json"))
    full = Context(tab="scan", has_scan=True, view=view, caps=frozenset(ALL_CAPABILITIES))
    old = Context(tab="scan", has_scan=True, view=view)  # a frontend without visuals.v1
    assert evaluate(by_id["view.visual.treemap"], full) == (True, "当前")
    assert evaluate(by_id["view.visual.treemap"], old) == (True, "当前")
    assert evaluate(by_id["view.color.branch"], full) == (True, "")
    assert evaluate(by_id["view.color.branch"], old) == (False, "这种配色在当前界面不可用")
    assert evaluate(by_id["view.visual.flame"], old) == (False, "这种可视化在当前界面不可用")
    flame = "flame" in [v.id for v in visuals.all_visuals()]  # registered by its own module
    assert evaluate(by_id["view.visual.flame"], full)[0] is flame
    assert evaluate(by_id["view.cycle_visual"], old) == (False, "这种可视化在当前界面不可用")
    assert evaluate(by_id["view.color.class"], Context(tab="start")) == (False, "需要先完成扫描")
    fake = Visualization(
        id="test.pie",
        api=ENGINE_API,
        layout="drawlist.v1",
        capability="",
        title_id="测试图",
        icon="vizTreemap",
        order=90,
        engine=TreemapEngine,
    )
    visuals.register(fake)
    try:
        assert evaluate(by_id["view.cycle_visual"], full) == (True, "当前：树图")
        view.set_visual("test.pie")
        assert evaluate(by_id["view.cycle_visual"], full) == (True, "当前：测试图")
        assert evaluate(by_id["view.visual.treemap"], full) == (True, "")
        view.color_mode = "branch"
        assert evaluate(by_id["view.color.branch"], full) == (True, "当前")
        assert evaluate(by_id["view.cycle_color"], full) == (True, "当前：顶层分支")
        view.set_visual("treemap")
        try:
            i18n.set_language("en")
            assert evaluate(by_id["view.cycle_visual"], full) == (True, "Current: Treemap")
        finally:
            i18n.set_language("zh-CN")
    finally:
        view.set_visual("treemap")
        visuals.unregister(fake.id)


# ------------------------------------------------------------------ paths


@pytest.fixture
def tree() -> tuple[object, np.ndarray]:
    store = build(SPEC, root=ROOT)
    return store, Layout(store).sizes()


def test_search_tiers_then_size(tree: tuple[object, np.ndarray]) -> None:
    store, sizes = tree
    hits, total = ppaths.search(store, sizes, "docker", 10)  # type: ignore[arg-type]
    names = [(h.name, h.tier) for h in hits]
    # exact names first (largest first), then prefix, then inside the name
    assert names[:3] == [("Docker", 0), ("Docker", 0), ("Docker", 0)]
    assert [h.alloc for h in hits[:3]] == sorted((h.alloc for h in hits[:3]), reverse=True)
    assert ("dockerd.exe", 1) in names and ("docker_data.vhdx", 1) in names
    assert total == len(hits) == 5
    vhdx = next(h for h in hits if h.name == "docker_data.vhdx")
    assert vhdx.hits == [(0, 6)] and not vhdx.is_dir and vhdx.complete.endswith("docker_data.vhdx")
    docker = hits[0]
    assert docker.is_dir and docker.complete.endswith(os.sep)


def test_search_scattered_letters_and_exclusion(tree: tuple[object, np.ndarray]) -> None:
    store, sizes = tree
    hits, _ = ppaths.search(store, sizes, "dkr", 10)  # type: ignore[arg-type]
    assert hits and all(h.tier == 3 for h in hits)
    assert hits[0].alloc == 600 * MB  # the biggest scattered hits (that Docker folder holds the vhdx)
    seed = find(store, "Users/lenny/AppData/Local/Docker")  # type: ignore[arg-type]
    hits, total = ppaths.search(store, sizes, "Docker", 5, exclude=seed, contains_only=True)  # type: ignore[arg-type]
    assert seed not in [h.node for h in hits] and total == 4
    assert all(h.tier <= 2 for h in hits)


def test_search_non_ascii_and_glob(tree: tuple[object, np.ndarray]) -> None:
    store, sizes = tree
    hits, _ = ppaths.search(store, sizes, "旅行", 5)  # type: ignore[arg-type]
    assert [h.name for h in hits] == ["旅行 记录.mov"] and hits[0].hits == [(0, 2)]
    hits, _ = ppaths.search(store, sizes, "*.exe", 5)  # type: ignore[arg-type]
    assert sorted(h.name for h in hits) == ["dockerd.exe", "git.exe"]


def test_inside_hits_keep_offsets_of_the_original_name() -> None:
    # "İ".lower() is two characters: offsets found in a Unicode-lowered name would drift
    store = build({"İİ notes docs.txt": 1, "İ😀docs": 2}, root=ROOT)
    hits, _ = ppaths.search(store, Layout(store).sizes(), "DOCS", 10)
    got = {h.name: (h.tier, h.hits) for h in hits}
    assert got == {"İİ notes docs.txt": (2, [(9, 13)]), "İ😀docs": (2, [(2, 6)])}
    assert utf16_spans("İ😀docs", got["İ😀docs"][1]) == [(3, 7)]


def test_seed_is_searched_literally() -> None:
    store = build({"what?.txt": 1, "whatX.txt": 2, "what?.txt.bak": 3, "中?.txt": 4, "中x.txt": 5}, root=ROOT)
    sizes = Layout(store).sizes()
    hits, total = ppaths.search(store, sizes, "what?.txt", 10, contains_only=True, literal=True)
    assert [(h.name, h.tier, h.hits) for h in hits] == [
        ("what?.txt", 0, [(0, 9)]),
        ("what?.txt.bak", 1, [(0, 9)]),
    ]
    assert total == 2
    hits, _ = ppaths.search(store, sizes, "中?.txt", 10, contains_only=True, literal=True)
    assert [h.name for h in hits] == ["中?.txt"]
    hits, _ = ppaths.search(store, sizes, "what?.txt", 10)  # typed: a glob
    assert sorted(h.name for h in hits) == ["what?.txt", "whatX.txt"]


def test_seed_text_of_scan_roots() -> None:
    photos, music = ("D:\\Photos", "E:\\Music") if sys.platform == "win32" else ("/data/Photos", "/mnt/Music")
    store = NodeStore()
    for path in (photos, music, ROOT):
        store.add_root(path)
    entries = {k: np.zeros(1, dtype=np.int64) for k in ENTRY_COLUMNS}
    first = store.ingest_names(1, [b"a.jpg"], entries)
    assert [ppaths.seed_text(store, r) for r in store.roots()] == ["Photos", "Music", ""]
    assert ppaths.seed_text(store, first) == "a.jpg"


def test_children_listing_and_completion(tree: tuple[object, np.ndarray]) -> None:
    store, sizes = tree
    lenny = find(store, "Users/lenny")  # type: ignore[arg-type]
    hits, total = ppaths.children(store, sizes, lenny, "", 10)  # type: ignore[arg-type]
    assert [h.name for h in hits] == ["AppData", "Videos", "dark.txt"] and total == 3
    hits, _ = ppaths.children(store, sizes, lenny, "vi", 10)  # type: ignore[arg-type]
    assert [h.name for h in hits] == ["Videos"]
    sep = os.sep
    base = os.path.join(ROOT, "Users", "lenny") + sep  # C:\Users\lenny\ or /Users/lenny/
    assert ppaths.completion(store, sizes, base + "Vi") == base + "Videos" + sep  # type: ignore[arg-type]
    assert ppaths.completion(store, sizes, base + "da") == base + "dark.txt"  # type: ignore[arg-type]
    assert ppaths.completion(store, sizes, base + "zz") is None  # type: ignore[arg-type]


def test_nodes_listed_after_the_sizes_count_as_not_scanned(tmp_path: Path) -> None:
    (tmp_path / "Videos" / "new").mkdir(parents=True)
    store = build({"Videos": {}, "old.txt": 5}, root=str(tmp_path), pending=("Videos",))
    sizes = Layout(store).sizes()
    videos = find(store, "Videos")
    # the scan lists Videos after the sizes were taken
    entries = {k: np.zeros(2, dtype=np.int64) for k in ENTRY_COLUMNS}
    entries["kind"][0] = Kind.DIR
    store.ingest_names(videos, [b"new", b"trip.mp4"], entries)
    assert len(store) > len(sizes)
    base = str(tmp_path / "Videos") + os.sep
    assert ppaths.children(store, sizes, videos, "", 10) == ([], 0)
    assert ppaths.children(store, sizes, videos, "tr", 10) == ([], 0)
    assert ppaths.completion(store, sizes, base + "tr") is None
    r = ppaths.resolve(store, sizes, base + "new")  # on disk, not sized yet
    assert (r.state, r.node, r.is_dir, r.alloc) == ("outside", None, True, None)
    r = ppaths.resolve(store, sizes, base + "trip.mp4")
    assert (r.state, r.node) == ("missing", None)
    sizes = Layout(store).sizes()  # sized: in the scan
    assert {h.name for h in ppaths.children(store, sizes, videos, "", 10)[0]} == {"new", "trip.mp4"}
    assert ppaths.completion(store, sizes, base + "tr") == base + "trip.mp4"
    assert ppaths.resolve(store, sizes, base + "new").state == "in_scan"


def test_path_input(tmp_path: Path, tree: tuple[object, np.ndarray]) -> None:
    store, sizes = tree
    for text in ("C:\\x", "c:/", "\\\\server\\share", "/usr", "~"):
        assert ppaths.looks_like_path(text), text
    for text in ("docker", "\\temp", "*.iso", ">1gb"):
        assert not ppaths.looks_like_path(text), text
    r = ppaths.resolve(store, sizes, os.path.join(ROOT, "Users", "lenny", "Videos"))  # type: ignore[arg-type]
    assert r.state == "in_scan" and r.is_dir and r.alloc == 110 * MB
    r = ppaths.resolve(store, sizes, str(tmp_path))  # type: ignore[arg-type]
    assert r.state == "outside" and r.is_dir
    r = ppaths.resolve(store, sizes, str(tmp_path / "nope"))  # type: ignore[arg-type]
    assert r.state == "missing"
    (tmp_path / "Videos").mkdir()
    assert ppaths.completion(None, None, str(tmp_path) + os.sep + "vi") == str(tmp_path / "Videos") + os.sep


# ------------------------------------------------------------------ recents


def test_recents_order_dedupe_clear_and_persistence(tmp_path: Path) -> None:
    file = tmp_path / "recent.json"
    r = Recents(file)
    r.add_filter("*.iso;>1gb")
    r.add_jump("C-abc", "C:\\Users\\lenny\\Videos")
    r.add_filter("*.mp4")
    r.add_filter("*.iso;>1gb")  # again: moves to the front, no duplicate
    texts = [e.text for e in Recents(file).entries("C-abc")]
    assert texts == ["*.iso;>1gb", "*.mp4", "C:\\Users\\lenny\\Videos"]
    assert [e.text for e in r.entries("D-xyz")] == ["*.iso;>1gb", "*.mp4"]  # filters are shared
    assert [e.text for e in r.entries(None)] == ["*.iso;>1gb", "*.mp4"]
    r.clear("C-abc")
    assert Recents(file).entries("C-abc") == []
    file.write_text("{broken", encoding="utf-8")
    assert Recents(file).entries("C-abc") == []
