"""M2 benchmark: layouts, filter evaluation and view rendering on a large synthetic tree.

    uv run python tools/bench_m2.py --nodes 3000000
    uv run python tools/bench_m2.py --visual flame,flame_polar --nodes 1000000

The tree mimics a real system disk: a few huge folders, deep nesting, many small files and
common extensions. Everything runs in memory (nothing is read from or written to disk). Prints rich
tables; the M2 target is a 4K layout under 120 ms. ``--visual`` picks the visualizations to lay
out (the treemap adds the filter and view tables); ``--budget`` caps the run time in seconds:
scenarios not started by then are skipped.
"""

from __future__ import annotations

import argparse
import random
import time
from collections.abc import Callable

import numpy as np
from rich.console import Console
from rich.table import Table

from spaceknife.filter import FilterContext, evaluate
from spaceknife.filter.names import name_index
from spaceknife.layout import (  # noqa: F401  (register on import)
    Basis,
    Layout,
    LayoutParams,
    RectKind,
    RootSpec,
    StoreSizes,
    flame,
    flame_polar,
    registry,
)
from spaceknife.store.nodes import ENTRY_COLUMNS, Kind, NodeStore
from spaceknife.view import View
from spaceknife.view.state import RootInfoLike
from spaceknife_protocol.filter_syntax import parse

EXTS = [
    "dll",
    "exe",
    "txt",
    "jpg",
    "png",
    "mp4",
    "log",
    "json",
    "js",
    "py",
    "zip",
    "dat",
    "sys",
    "mp3",
    "pdf",
    "",
]
WEIGHTS = [8, 3, 6, 6, 5, 1, 4, 6, 9, 5, 1, 4, 1, 1, 1, 6]
WORDS = [
    "cache",
    "temp",
    "node_modules",
    "src",
    "lib",
    "assets",
    "Program Files",
    "Windows",
    "Users",
    "AppData",
    "Local",
    "Microsoft",
    "build",
    "dist",
    "data",
    "logs",
    "img",
    "docs",
    "pkg",
    "bin",
]


def synth(nodes: int, seed: int = 7) -> NodeStore:
    rng = random.Random(seed)
    store = NodeStore(capacity=nodes + 1024)
    root = store.add_root("C:\\")
    queue = [(root, 1)]
    while queue and len(store) < nodes:
        d, depth = queue.pop(rng.randrange(len(queue)) if len(queue) > 64 else 0)
        n_dirs = rng.choice([0, 1, 2, 3, 5, 8]) if depth < 12 else 0
        n_files = int(rng.paretovariate(1.1) * 6) if rng.random() < 0.9 else rng.randint(500, 5000)
        count = min(n_dirs + n_files, nodes - len(store))
        if count <= 0:
            continue
        names: list[bytes] = []
        kinds = np.zeros(count, dtype=np.int64)
        sizes = np.zeros(count, dtype=np.int64)
        exts = np.zeros(count, dtype=np.uint16)
        for i in range(count):
            if i < n_dirs:
                names.append(f"{rng.choice(WORDS)}{i}".encode())
                kinds[i] = Kind.DIR
            else:
                ext = rng.choices(EXTS, WEIGHTS)[0]
                stem = f"{rng.choice(WORDS)}_{rng.randrange(10**6)}"
                names.append((stem + ("." + ext if ext else "")).encode())
                sizes[i] = int(rng.lognormvariate(9, 2.5))
                exts[i] = store.ext_id(ext)
        entries = {k: np.zeros(count, dtype=np.int64) for k in ENTRY_COLUMNS}
        entries["kind"] = kinds
        entries["size"] = sizes
        entries["alloc"] = (sizes + 4095) // 4096 * 4096
        now = int(time.time())
        entries["mtime"] = now - np.array([int(rng.expovariate(1 / (90 * 86400))) for _ in range(count)])
        entries["flags"] = np.where(np.array([rng.random() < 0.03 for _ in range(count)]), 32, 0)
        first = store.ingest_names(d, names, entries, exts)
        for i in range(n_dirs):
            queue.append((first + i, depth + 1))
    return store


def ms(fn, repeat: int = 3) -> tuple[float, object]:  # type: ignore[no-untyped-def]
    best, out = float("inf"), None
    for _ in range(repeat):
        t0 = time.perf_counter()
        out = fn()
        best = min(best, time.perf_counter() - t0)
    return best * 1000, out


PARTITION_GRID = [(1920, 1080, 1.0, 5), (1920, 1080, 1.0, 15), (3840, 2160, 1.0, 5), (3840, 2160, 1.0, 15)]
PARTITION_MIN_ELEMENT = 10.0  # the app's default minimum element (setting treemap.min_element)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--nodes", type=int, default=3_000_000)
    ap.add_argument("--visual", default="treemap", help="comma-separated: treemap, flame, flame_polar")
    ap.add_argument("--budget", type=float, default=120.0, help="seconds; later scenarios are skipped")
    args = ap.parse_args()
    visuals = [v.strip() for v in args.visual.split(",") if v.strip()]
    known = {"treemap"} | {v.id for v in registry.all_visuals()}
    if unknown := [v for v in visuals if v not in known]:
        ap.error(f"unknown visualization {', '.join(unknown)} (known: {', '.join(sorted(known))})")
    con = Console()
    deadline: float = time.perf_counter() + args.budget
    t0 = time.perf_counter()
    store = synth(args.nodes)
    mib = store.memory_bytes() / 2**20
    con.print(f"合成树：{len(store):,} 个节点，{mib:.0f} MiB，用时 {time.perf_counter() - t0:.1f} 秒")

    def late() -> bool:
        return time.perf_counter() > deadline

    if "treemap" in visuals:
        treemap(con, store, late)
    basis = Basis(store)
    for vid in visuals:
        if vid != "treemap":
            partition(con, basis, vid, late)
    if late():
        con.print(f"[yellow]超出 {args.budget:.0f} 秒的时间上限，后面的场景已跳过[/]")


def partition(con: Console, basis: Basis, vid: str, late: Callable[[], bool]) -> None:
    vis = registry.get(vid)
    engine = vis.engine(basis)
    t = Table(title=f"{vis.title}布局（{vis.layout}，取 3 次最好成绩）", title_justify="left")
    for col in ("视口", "层级", "画出层数", "行数", "展开", "毫秒"):
        t.add_column(col, justify="right")
    for w, h, dpr, detail in PARTITION_GRID:
        if late():
            t.add_row(f"{w}×{h} @{dpr}", str(detail), "", "", "", "跳过")
            continue
        p = LayoutParams(width=w, height=h, dpr=dpr, detail=detail, min_element=PARTITION_MIN_ELEMENT)
        dl = engine.run(p, RootSpec(node=1), StoreSizes(basis))
        took, _ = ms(lambda p=p: engine.run(p, RootSpec(node=1), StoreSizes(basis)))
        folders = f"{int((dl.kind == RectKind.FOLDER).sum()):,}"
        t.add_row(f"{w}×{h} @{dpr}", str(detail), str(dl.levels), f"{len(dl):,}", folders, f"{took:.1f}")
    con.print(t)


def treemap(con: Console, store: NodeStore, late: Callable[[], bool]) -> None:
    lay = Layout(store)
    t = Table(title="树图布局（取 3 次最好成绩）", title_justify="left")
    for col in ("视口", "层级", "矩形数", "毫秒"):
        t.add_column(col, justify="right")
    for w, h, dpr, detail in [
        (1920, 1080, 1.0, 5),
        (2560, 1440, 1.5, 5),
        (3840, 2160, 1.0, 5),
        (3840, 2160, 1.0, 8),
        (3840, 2160, 1.0, 15),
        (1920, 1080, 2.0, 15),
    ]:
        if late():
            t.add_row(f"{w}×{h} @{dpr}", str(detail), "", "跳过")
            continue
        p = LayoutParams(width=w, height=h, dpr=dpr, detail=detail)
        lay.run(p, RootSpec(node=1))
        took, dl = ms(lambda p=p: lay.run(p, RootSpec(node=1)))
        t.add_row(f"{w}×{h} @{dpr}", str(detail), f"{len(dl):,}", f"{took:.1f}")  # type: ignore[arg-type]
    con.print(t)
    if late():
        return

    t0 = time.perf_counter()
    name_index(store).update(len(store))
    con.print(f"名字索引：{(time.perf_counter() - t0) * 1000:.0f} 毫秒（扫描结束后在后台建好）")
    t = Table(title="过滤求值（整棵树，含逐级汇总）", title_justify="left")
    for col in ("表达式", "匹配", "首次毫秒", "再次毫秒"):
        t.add_column(col, justify="right")
    ctx = FilterContext.current()
    for text in [
        "*.dll",
        "*.mp4;>10mb",
        "*cache*",
        "?*e*",
        "\\*temp*",
        ":class:image;<30d",
        ":attr:h",
        "|*.js;s>1mb",
    ]:
        if late():
            t.add_row(text, "", "", "跳过")
            continue
        nf = parse(text).normal
        assert nf is not None
        t0 = time.perf_counter()
        res = evaluate(store, nf, ctx)
        first = (time.perf_counter() - t0) * 1000
        again, _ = ms(lambda nf=nf: evaluate(store, nf, ctx))
        t.add_row(text, f"{res.totals(0)[0]:,}", f"{first:.0f}", f"{again:.0f}")
    con.print(t)
    if late():
        return

    view = View(
        "bench",
        store,
        [RootInfoLike(node=1, whole_volume=True, free=10**11, used=int(store.cols["tot_alloc"][1]) + 10**9)],
    )
    view.set_viewport(3840, 2160, 1.0)
    took, _ = ms(view.render)
    view.set_filter("*.jpg;*.png;>100kb")
    took_f, _ = ms(view.render)
    con.print(f"视图渲染 4K（布局 + 标记）：{took:.1f} 毫秒；带过滤暗化：{took_f:.1f} 毫秒")


if __name__ == "__main__":
    main()
