"""Plugin system costs on a synthetic tree (docs/memory.md, 「插件」): images, queries, processes.

    uv run python tools/bench_plugins.py [--nodes 1000000] [--json]

Builds a tree of ``--nodes`` nodes (1000 files a folder), then measures, in a plugin process:
the start (and its memory), a call's round trip, exporting the image of seven columns, a host-side
query, a code visualization's weights (one call per tree version) and a flame graph frame drawn
with them.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import statistics
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

import numpy as np

PLUGIN_TOML = """id = "bench.plugin"
version = "0.1.0"
name = { zh = "基准", en = "Bench" }
entry = "plugin.py"
permissions = ["tree.read"]
[limits]
timeout_s = 600
"""
PLUGIN_PY = """
import numpy as np
from spaceknife_sdk import Context, Payload, Plugin, T, VisualInput

plugin = Plugin()


class Out(Payload):
    n: int
    total: int = 0


@plugin.method
async def ping(ctx: Context) -> Out:
    return Out(n=0)


@plugin.method(timeout=600)
async def image(ctx: Context) -> Out:
    t = ctx.tree
    cols = t.cols("parent", "kind", "size", "alloc", "tot_alloc", "child_start", "child_count")
    return Out(n=t.n, total=int(cols["alloc"].sum()))


@plugin.method(timeout=600)
async def query(ctx: Context) -> Out:
    rows = ctx.tree.query("*.bin", kind="file", columns=("bytes", "path"), sort="-bytes", limit=1000)
    return Out(n=len(rows), total=rows.total)


@plugin.visualization("weights", T("权重", "Weights"), base="flame")
def weights(v: VisualInput) -> np.ndarray:
    return v.tree.col("size").astype(np.float64)
"""


def build_tree(nodes: int) -> Any:
    from spaceknife.store.nodes import ENTRY_COLUMNS, Kind, NodeStore

    per = 1000
    folders = max(1, nodes // (per + 1))
    store = NodeStore(capacity=folders * (per + 1) + 16)
    root = store.add_root("/bench")
    entries = {k: np.zeros(folders, dtype=np.int64) for k in ENTRY_COLUMNS}
    entries["kind"][:] = Kind.DIR
    first = store.ingest_names(root, [f"d{i:05d}".encode() for i in range(folders)], entries)
    rng = np.random.default_rng(1)
    ext = store.ext_id("bin")
    names = [f"f{i:04d}.bin".encode() for i in range(per)]
    for d in range(folders):
        e = {k: np.zeros(per, dtype=np.int64) for k in ENTRY_COLUMNS}
        size = rng.integers(1, 1 << 20, per)
        e["size"][:] = size
        e["alloc"][:] = (size + 4095) // 4096 * 4096
        e["mtime"][:] = 1_700_000_000
        store.ingest_names(first + d, names, e, np.full(per, ext, dtype=np.uint16))
    return store


async def run(nodes: int) -> dict[str, Any]:
    from spaceknife_sdk.testing import MockHost

    out: dict[str, Any] = {"nodes": 0}
    with tempfile.TemporaryDirectory() as tmp:
        folder = Path(tmp) / "bench.plugin"
        folder.mkdir()
        (folder / "plugin.toml").write_text(PLUGIN_TOML, encoding="utf-8")
        (folder / "plugin.py").write_text(PLUGIN_PY, encoding="utf-8")
        t0 = time.perf_counter()
        store = build_tree(nodes)
        out["nodes"] = len(store)
        out["build_s"] = round(time.perf_counter() - t0, 2)
        host = MockHost.load(folder, process=True)
        host.scan(store)
        try:
            t0 = time.perf_counter()
            hp = await host._ready()
            await host.call("ping")
            out["process_start_s"] = round(time.perf_counter() - t0, 3)
            rec = hp.records["bench.plugin"]
            out["process_rss_mb"] = round(rec.runtime.info().memory / 2**20, 1)
            rtts = []
            for _ in range(200):
                t = time.perf_counter()
                await host.call("ping")
                rtts.append(time.perf_counter() - t)
            out["call_round_trip_ms"] = round(statistics.median(rtts) * 1000, 3)
            # the image: exported by the host (seven columns), mapped by the plugin
            from spaceknife.plugins.image import export

            t = time.perf_counter()
            seven = ["parent", "kind", "size", "alloc", "tot_alloc", "child_start", "child_count"]
            e = export("bench", store, seven)
            out["image_export_ms"] = round((time.perf_counter() - t) * 1000, 1)
            out["image_mb"] = round(e.size / 2**20, 1)
            e.release()
            t = time.perf_counter()
            await host.call("image")
            out["image_first_use_ms"] = round((time.perf_counter() - t) * 1000, 1)
            t = time.perf_counter()
            await host.call("image")
            out["image_cached_ms"] = round((time.perf_counter() - t) * 1000, 1)
            t = time.perf_counter()
            res = await host.call("query")
            out["query_ms"] = round((time.perf_counter() - t) * 1000, 1)
            out["query_matches"] = res["total"]
            # a code visualization: the weights once per tree version, then frames on the host
            t = time.perf_counter()
            await host.visual("weights", width=3840, height=2160)
            out["code_visual_first_frame_ms"] = round((time.perf_counter() - t) * 1000, 1)
            frames = []
            for _ in range(5):
                t = time.perf_counter()
                await host.visual("weights", width=3840, height=2160)
                frames.append(time.perf_counter() - t)
            out["code_visual_frame_ms"] = round(statistics.median(frames) * 1000, 1)
        finally:
            await host.close()
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--nodes", type=int, default=1_000_000)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)
    res = asyncio.run(run(args.nodes))
    if args.json:
        print(json.dumps(res))
    else:
        for k, v in res.items():
            print(f"{k:28} {v}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
