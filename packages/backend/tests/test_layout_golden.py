"""Golden draw lists of the treemap: its output stays byte for byte what was recorded.

``data/layout_golden.json`` holds, per scenario, a sha256 of every column of the draw list, of the
names and of the member blocks, all laid out on one fixed synthetic tree: several viewports,
device pixel ratios and detail levels, aggregate zooms, hide-mode sizes, free and unknown space.
Refactoring the layout engine must not move a single byte. Record again only for an intended
change of the treemap's output:

    SPACEKNIFE_TEST_RECORD_GOLDEN=1 uv run pytest packages/backend/tests/test_layout_golden.py
"""

from __future__ import annotations

import hashlib
import json
import os
import random
from collections.abc import Callable
from functools import cache
from pathlib import Path

import numpy as np
import pytest

from spaceknife.classes import DEFAULT_CLASSES
from spaceknife.layout import DrawList, Layout, LayoutParams, RootSpec
from spaceknife.layout.engine import KEY_MORE, KEY_NODE_MASK, KEY_SMALL
from spaceknife.store.nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore

FIXTURE = Path(__file__).parent / "data" / "layout_golden.json"
RECORD = os.environ.get("SPACEKNIFE_TEST_RECORD_GOLDEN") == "1"

COLUMNS = ("key", "node", "parent", "kind", "depth", "x", "y", "w", "h", "cls", "label", "size", "count")
STORE_COLUMNS = (
    "parent",
    "depth",
    "kind",
    "flags",
    "alloc",
    "tot_alloc",
    "child_start",
    "child_count",
    "ext",
)

WORDS = ["cache", "Temp", "node_modules", "src", "lib", "Program Files", "Windows", "Users", "AppData"]
WORDS += ["Local", "build", "dist", "data", "logs", "img", "docs", "pkg", "bin", "文档", "Ünïcode"]
EXTS = ["dll"] * 8 + ["exe"] * 3 + ["txt"] * 6 + ["jpg"] * 6 + ["png"] * 5 + ["mp4", "zip", "sys", "pdf"]
EXTS += ["log"] * 4 + ["json"] * 6 + ["js"] * 9 + ["py"] * 5 + ["dat"] * 4 + [""] * 6
FILE_COUNTS = (0, 1, 2, 3, 4, 6, 8, 12, 20, 40, 90)

ROOT = 1  # the scan root of tree()


# --------------------------------------------------------------------------- the tree


def tree(nodes: int = 20_000, seed: int = 7, *, pending_every: int = 0) -> NodeStore:
    """A system-disk-like tree in the manner of ``tools/bench_m2.synth``, drawn with integer
    operations only: every platform builds the same tree, and edits to the benchmark don't move
    the recording. Folders still queued when the budget runs out stay PENDING with nothing in
    them; ``pending_every`` also marks every n-th listed folder below the top level PENDING."""
    rng = random.Random(seed)
    store = NodeStore(capacity=nodes + 1024)
    queue = [(store.add_root("C:\\"), 1)]
    while queue and len(store) < nodes:
        d, depth = queue.pop(rng.randrange(len(queue)) if len(queue) > 64 else 0)
        n_dirs = rng.choice((0, 1, 2, 3, 5, 8)) if depth < 12 else 0
        n_files = rng.randint(500, 5000) if rng.randrange(10) == 0 else rng.choice(FILE_COUNTS)
        count = min(n_dirs + n_files, nodes - len(store))
        if count <= 0:
            continue
        names: list[bytes] = []
        entries = {k: np.zeros(count, dtype=np.int64) for k in ENTRY_COLUMNS}
        exts = np.zeros(count, dtype=np.uint16)
        for i in range(count):
            if i < n_dirs:
                names.append(f"{rng.choice(WORDS)}{i}".encode())
                entries["kind"][i] = Kind.DIR
                continue
            ext = rng.choice(EXTS)
            names.append(f"{rng.choice(WORDS)}_{rng.randrange(10**6)}{'.' + ext if ext else ''}".encode())
            bits = 4 + rng.randrange(12) + rng.randrange(12)  # log size peaks around 32 KB ...
            if rng.randrange(300) == 0:
                bits = 24 + rng.randrange(6)  # ... with a few files of 16 to 512 MB
            size = 0 if rng.randrange(50) == 0 else rng.randrange(1 << bits)
            entries["size"][i] = size
            entries["alloc"][i] = (size + 4095) // 4096 * 4096
            entries["flags"][i] = Flag.HARDLINK_DUP if rng.randrange(40) == 0 else 0
            exts[i] = store.ext_id(ext)
        first = store.ingest_names(d, names, entries, exts)
        queue.extend((first + i, depth + 1) for i in range(n_dirs))
    if pending_every:
        c = store.cols
        n = len(store)
        listed = np.nonzero((c["kind"][:n] == Kind.DIR) & (c["child_count"][:n] > 0) & (c["depth"][:n] > 2))[
            0
        ]
        c["flags"][listed[::pending_every]] |= np.uint32(Flag.PENDING)
    return store


@cache
def base() -> Layout:
    return Layout(tree(), DEFAULT_CLASSES)  # the built-in class table: user edits don't matter


def hide_sizes(store: NodeStore) -> np.ndarray:
    """Hide mode's area basis for ``*.dll;*.js``: matching files, folders holding their matches."""
    n = len(store)
    c = store.cols
    wanted = [i for i, e in enumerate(store.ext_table) if e in ("dll", "js")]
    match = (c["kind"][:n] != Kind.DIR) & np.isin(c["ext"][:n], wanted)
    sizes: np.ndarray = np.where(match, Layout.effective_sizes(store), 0).astype(np.int64)
    depth, parent = c["depth"][:n], c["parent"][:n]
    for d in range(int(depth.max()), 1, -1):  # deepest first: a folder is complete before it counts
        ids = np.nonzero(depth == d)[0]
        np.add.at(sizes, parent[ids], sizes[ids])
    return sizes


FHD = LayoutParams(width=1920, height=1080)


def aggregate(kind: int) -> RootSpec:
    """Zoom into the ``kind`` block (KEY_SMALL / KEY_MORE) with the most members of the 1080p frame."""
    first = base().run(FHD, RootSpec(node=ROOT))
    keys = [k for k in first.members if (k & ~KEY_NODE_MASK) == kind]
    key = max(keys, key=lambda k: (len(first.members[k]), -k))
    return RootSpec(node=key & KEY_NODE_MASK, members=first.members[key], key=key)


def largest(kind: Kind) -> int:
    """The largest node of ``kind`` below the root (ties: lowest id)."""
    store = base().store
    n = len(store)
    sizes = np.where(store.cols["kind"][:n] == kind, base().sizes(), -1)
    sizes[: ROOT + 1] = -1
    return int(np.argmax(sizes))


def _fhd() -> DrawList:
    return base().run(FHD, RootSpec(node=ROOT))


def _uhd_detail15() -> DrawList:
    return base().run(LayoutParams(width=3840, height=2160, detail=15), RootSpec(node=ROOT))


def _dpr125() -> DrawList:
    return base().run(LayoutParams(width=1536, height=864, dpr=1.25, detail=8), RootSpec(node=ROOT))


def _dpr150() -> DrawList:
    return base().run(LayoutParams(width=1280, height=720, dpr=1.5, detail=10), RootSpec(node=ROOT))


def _volume() -> DrawList:
    return base().run(FHD, RootSpec(node=ROOT, free=2 << 30, unknown=500 << 20))


def _volume_by_name() -> DrawList:
    p = LayoutParams(width=1600, height=900, dpr=1.25, sort_by_size=False)
    return base().run(p, RootSpec(node=ROOT, free=2 << 30, unknown=500 << 20))


def _volume_tiny_free() -> DrawList:  # free space too small to draw: its cell is dropped
    return base().run(FHD, RootSpec(node=ROOT, free=4096, unknown=0))


def _font_aspect() -> DrawList:
    p = LayoutParams(width=1920, height=1080, detail=6, min_element=20, font_scale=1.25, aspect=1.6)
    return base().run(p, RootSpec(node=ROOT))


def _hide() -> DrawList:
    p = LayoutParams(width=1920, height=1080, detail=8)
    return base().run(p, RootSpec(node=ROOT), hide_sizes(base().store))


def _hide_uhd_dpr150() -> DrawList:
    p = LayoutParams(width=2560, height=1440, dpr=1.5, detail=15)
    return base().run(p, RootSpec(node=ROOT), hide_sizes(base().store))


def _aggregate_small() -> DrawList:
    return base().run(FHD, aggregate(KEY_SMALL))


def _aggregate_more() -> DrawList:
    return base().run(FHD, aggregate(KEY_MORE))


def _aggregate_by_name() -> DrawList:
    p = LayoutParams(width=1280, height=720, dpr=1.5, sort_by_size=False)
    return base().run(p, aggregate(KEY_SMALL))


def _aggregate_leftover() -> DrawList:  # the "+N items" of an aggregate zoom, zoomed into again
    zoom = aggregate(KEY_SMALL)
    first = base().run(FHD, zoom)
    key = max(first.members, key=lambda k: (len(first.members[k]), -k))
    return base().run(FHD, RootSpec(node=zoom.node, members=first.members[key], key=key))


def _folder() -> DrawList:
    p = LayoutParams(width=2560, height=1440, dpr=1.5, detail=15)
    return base().run(p, RootSpec(node=largest(Kind.DIR)))


def _file_root() -> DrawList:
    return base().run(FHD, RootSpec(node=largest(Kind.FILE)))


def _pending() -> DrawList:
    lay = Layout(tree(pending_every=3), DEFAULT_CLASSES)
    return lay.run(LayoutParams(width=1920, height=1080, detail=8), RootSpec(node=ROOT))


SCENARIOS: dict[str, Callable[[], DrawList]] = {
    "fhd": _fhd,
    "uhd_detail15": _uhd_detail15,
    "dpr125": _dpr125,
    "dpr150": _dpr150,
    "volume": _volume,
    "volume_by_name": _volume_by_name,
    "volume_tiny_free": _volume_tiny_free,
    "font_aspect": _font_aspect,
    "hide": _hide,
    "hide_uhd_dpr150": _hide_uhd_dpr150,
    "aggregate_small": _aggregate_small,
    "aggregate_more": _aggregate_more,
    "aggregate_by_name": _aggregate_by_name,
    "aggregate_leftover": _aggregate_leftover,
    "folder": _folder,
    "file_root": _file_root,
    "pending": _pending,
}


# --------------------------------------------------------------------------- fingerprints


def digest(*parts: bytes) -> str:
    h = hashlib.sha256()
    for part in parts:
        h.update(len(part).to_bytes(8, "little"))
        h.update(part)
    return h.hexdigest()


def column(a: np.ndarray) -> str:
    return f"{a.dtype.str}:{digest(np.ascontiguousarray(a).tobytes())}"


def names(items: list[str]) -> str:
    return digest(*(s.encode("utf-8", "surrogatepass") for s in items))


def fingerprint(dl: DrawList) -> dict[str, object]:
    keys = sorted(dl.members)
    return {
        "rows": len(dl),
        "width": dl.width,
        "height": dl.height,
        "columns": {name: column(getattr(dl, name)) for name in COLUMNS},
        "names": names(dl.names),
        "member_keys": keys,
        "members": digest(*(column(dl.members[k]).encode() for k in keys)),
    }


def tree_fingerprint(store: NodeStore) -> dict[str, object]:
    n = len(store)
    c = store.cols
    return {
        "nodes": n,
        "folders": int(np.count_nonzero(c["kind"][:n] == Kind.DIR)),
        "allocated": int(c["tot_alloc"][ROOT]),
        "columns": {name: column(c[name][:n]) for name in STORE_COLUMNS},
        "names": names([store.name(i) for i in range(n)]),
        "ext_table": list(store.ext_table),
    }


def record() -> dict[str, object]:
    return {
        "tree": tree_fingerprint(base().store),
        "scenarios": {name: fingerprint(run()) for name, run in SCENARIOS.items()},
    }


@pytest.fixture(scope="module")
def golden() -> dict[str, dict[str, object]]:
    if RECORD:
        FIXTURE.parent.mkdir(parents=True, exist_ok=True)
        text = json.dumps(record(), indent=1, ensure_ascii=False) + "\n"
        FIXTURE.write_text(text, encoding="utf-8", newline="\n")
    data: dict[str, dict[str, object]] = json.loads(FIXTURE.read_text(encoding="utf-8"))
    return data


# --------------------------------------------------------------------------- tests


def test_the_tree_is_the_recorded_one(golden: dict[str, dict[str, object]]) -> None:
    # fails first when the input moved: then the scenarios below say nothing about the engine
    assert tree_fingerprint(base().store) == golden["tree"]


def test_every_scenario_is_recorded(golden: dict[str, dict[str, object]]) -> None:
    assert list(golden["scenarios"]) == list(SCENARIOS)


@pytest.mark.parametrize("name", list(SCENARIOS))
def test_draw_list_is_byte_identical(name: str, golden: dict[str, dict[str, object]]) -> None:
    assert fingerprint(SCENARIOS[name]()) == golden["scenarios"][name]


def test_scenarios_reach_every_path(golden: dict[str, dict[str, object]]) -> None:
    """The recording means something only if the scenarios draw every kind of block."""
    kinds = {
        name: set(np.unique(run().kind).tolist())
        for name, run in SCENARIOS.items()
        if name in ("fhd", "volume", "aggregate_small", "pending", "volume_tiny_free")
    }
    assert {0, 1, 2, 4, 5} <= kinds["fhd"]  # files, folders, collapsed, small items, "+N items"
    assert {6, 7} <= kinds["volume"]  # free and unknown space
    assert 3 in kinds["pending"]
    assert 5 in kinds["aggregate_small"]  # the leftover of an aggregate zoom
    assert 6 not in kinds["volume_tiny_free"]
    assert len(golden["scenarios"]["uhd_detail15"]["member_keys"]) > 20  # type: ignore[arg-type]
