"""MFT fast-scan benchmark: Rust vs numpy parsing, and building the node store.

    uv run python tools/bench_mft.py --records 2000000 --rows 6500000
    uv run python tools/bench_mft.py --volume C:      # real $MFT: needs an elevated shell

Synthetic mode builds a $MFT image of ``--records`` records (packages/backend/tests/mft_image.py:
metafiles, a random folder tree, hard links, DOS aliases, streams) and a synthetic ``MftTable``
of ``--rows`` rows for ``table_to_store``. Volume mode reads the real $MFT once (read only) and
times reading, parsing and building. M4 targets: Rust parse >= 2 M records/s, table_to_store of
6.5 M rows well under 5 s, a C:-sized volume under 10 s end to end.
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import sys
import time
from collections.abc import Callable
from pathlib import Path
from types import ModuleType
from typing import Any

import numpy as np
from rich.console import Console
from rich.table import Table

from spaceknife import native
from spaceknife.log import setup_logging
from spaceknife.scan import mft
from spaceknife.store.nodes import Kind

console = Console()


def _image_module() -> ModuleType:
    path = Path(__file__).resolve().parents[1] / "packages" / "backend" / "tests" / "mft_image.py"
    spec = importlib.util.spec_from_file_location("mft_image", path)
    assert spec is not None
    assert spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    sys.modules["mft_image"] = mod
    spec.loader.exec_module(mod)
    return mod


def timed(fn: Callable[[], Any], repeat: int = 1) -> tuple[Any, float]:
    best = float("inf")
    result: Any = None
    for _ in range(repeat):
        t0 = time.perf_counter()
        result = fn()
        best = min(best, time.perf_counter() - t0)
    return result, best


def synthetic_table(rows: int, seed: int = 0) -> dict[str, Any]:
    """An ``MftTable``-shaped dict of ``rows`` rows (one name per record, ~6 % folders)."""
    rng = np.random.default_rng(seed)
    first = 32
    record = np.arange(first, first + rows, dtype=np.uint64)
    is_dir = rng.random(rows) < 0.06
    is_dir[0] = True
    dir_records = record[is_dir].astype(np.int64)
    rank = np.searchsorted(dir_records, record.astype(np.int64)) - 1
    pick = (rng.random(rows) * (rank + 2)).astype(np.int64) - 1
    parent = np.where(pick >= 0, dir_records[np.maximum(pick, 0)], 5).astype(np.uint64)
    exts = [b".dll", b".txt", b".jpg", b".png", b".json", b".js", b".manifest", b".mui", b"", b".log"]
    stem = rng.integers(4, 20, rows)
    ext_pick = rng.integers(0, len(exts), rows)
    ext_len = np.array([len(e) for e in exts])[ext_pick]
    name_len = (stem + np.where(is_dir, 0, ext_len)).astype(np.int64)
    name_off = np.cumsum(name_len) - name_len
    blob = rng.integers(ord("a"), ord("z") + 1, int(name_len.sum()), dtype=np.uint8)
    for k, e in enumerate(exts):
        if not e:
            continue
        rows_k = np.flatnonzero((ext_pick == k) & ~is_dir)
        for i, ch in enumerate(e):
            blob[name_off[rows_k] + stem[rows_k] + i] = ch
    size = np.where(is_dir, 0, rng.lognormal(9, 2.5, rows).astype(np.int64))
    alloc = np.where(size > 700, (size + 4095) // 4096 * 4096, 0)
    t = np.full(rows, 1_700_000_000, dtype=np.int64)
    return {
        "record": record,
        "parent": parent,
        "name_off": name_off.astype(np.uint32),
        "name_len": name_len.astype(np.uint16),
        "kind": np.where(is_dir, Kind.DIR, Kind.FILE).astype(np.uint8),
        "flags": np.zeros(rows, dtype=np.uint32),
        "size": size,
        "alloc": alloc,
        "ads_size": np.zeros(rows, dtype=np.int64),
        "ads_alloc": np.zeros(rows, dtype=np.int64),
        "mtime": t,
        "ctime": t,
        "atime": t,
        "nlink": np.ones(rows, dtype=np.uint16),
        "names": blob.tobytes(),
        "bad_records": 0,
    }


def bench_synthetic(records: int, fallback_records: int, rows: int) -> None:
    im = _image_module()
    out = Table(title=f"MFT fast scan ({os.cpu_count()} CPUs)")
    for col in ("step", "input", "time", "rate"):
        out.add_column(col, justify="right" if col != "step" else "left")
    arr, t = timed(lambda: im.synthetic_mft(records))
    out.add_row("build synthetic image", f"{records:,} records", f"{t:.2f} s", "")
    data = memoryview(arr)
    mod = native.module()
    table: dict[str, Any] | None = None
    if mod is not None and native.has("parse_mft"):
        table, t = timed(lambda: mod.parse_mft(data, 1024, 4096), repeat=3)
        out.add_row(
            "parse_mft (Rust)",
            f"{records:,} records",
            f"{t * 1e3:.0f} ms",
            f"{records / t / 1e6:.1f} M rec/s",
        )
    else:
        console.print("[yellow]spaceknife_native.parse_mft not available: Rust rows skipped[/]")
    if fallback_records:
        sub = memoryview(arr[: min(fallback_records, records)])
        n = sub.nbytes // 1024
        fb, t = timed(lambda: mft.parse_mft_numpy(sub, 1024, 4096))
        out.add_row(
            "parse_mft_numpy (fallback)", f"{n:,} records", f"{t:.2f} s", f"{n / t / 1e6:.2f} M rec/s"
        )
        table = table or fb
    if table is not None:
        nrows = len(table["record"])
        store, t = timed(lambda: mft.table_to_store(table, "C:\\"))
        out.add_row(
            "table_to_store (parsed)", f"{nrows:,} rows", f"{t:.2f} s", f"{nrows / t / 1e6:.1f} M rows/s"
        )
    if rows:
        syn, t = timed(lambda: synthetic_table(rows))
        out.add_row("build synthetic table", f"{rows:,} rows", f"{t:.2f} s", "")
        store, t = timed(lambda: mft.table_to_store(syn, "C:\\"))
        depth = int(store.cols["depth"][: len(store)].max())
        out.add_row(
            "table_to_store", f"{rows:,} rows, depth {depth}", f"{t:.2f} s", f"{rows / t / 1e6:.1f} M rows/s"
        )
    console.print(out)


def bench_volume(volume: str) -> None:
    out = Table(title=f"MFT fast scan of {volume}")
    for col in ("step", "time", "detail"):
        out.add_column(col)
    t0 = time.perf_counter()
    (data, info), t = timed(lambda: mft.read_raw_mft(volume))
    out.add_row(
        "read $MFT", f"{t:.2f} s", f"{len(data) / 2**30:.2f} GiB, {info.get('mft_fragments')} fragments"
    )
    table, t = timed(lambda: mft.parse_mft_table(data, info["record_size"], info["cluster_size"]))
    records = len(data) // info["record_size"]
    out.add_row(
        "parse",
        f"{t:.2f} s",
        f"{records:,} records, {len(table['record']):,} rows, {table['bad_records']} bad, "
        f"{records / t / 1e6:.1f} M rec/s",
    )
    data = b""  # free the raw $MFT before building the store
    root = volume.rstrip("\\/") + "\\"
    store, t = timed(lambda: mft.table_to_store(table, root, info))
    r = next(iter(store.roots()))
    out.add_row(
        "table_to_store",
        f"{t:.2f} s",
        f"{len(store):,} nodes, {int(store.cols['tot_alloc'][r]) / 2**30:.1f} GiB allocated, "
        f"{int(store.cols['tot_files'][r]):,} files",
    )
    out.add_row("total", f"{time.perf_counter() - t0:.2f} s", "")
    console.print(out)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--records", type=int, default=2_000_000, help="records in the synthetic $MFT image")
    ap.add_argument(
        "--fallback-records", type=int, default=500_000, help="records parsed by the numpy fallback (0: skip)"
    )
    ap.add_argument("--rows", type=int, default=6_500_000, help="rows of the synthetic table (0: skip)")
    ap.add_argument("--volume", help="read and parse this volume's real $MFT (elevated shell only)")
    args = ap.parse_args()
    setup_logging("INFO")
    if args.volume:
        try:
            bench_volume(args.volume)
        except PermissionError:
            console.print("[red]reading the $MFT needs an elevated (administrator) shell[/]")
            raise SystemExit(1) from None
    else:
        bench_synthetic(args.records, args.fallback_records, args.rows)


if __name__ == "__main__":
    main()
