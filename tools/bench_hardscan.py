"""M4 benchmark: native parallel hard scan (spaceknife_native.scan_slice) vs the Python worker.

    uv run python tools/bench_hardscan.py "C:\\Program Files" C:\\Windows\\WinSxS
    uv run python tools/bench_hardscan.py /usr --repeat 5 --pool 4,8

Read-only: it only lists directories. Every mode runs ``--repeat`` times after one warm-up pass
(warm cache) and reports the best and median items/s:

- ``py-slices``   worker.scan_slice in this process, frontier fed back (no node store)
- ``native-slices`` the same loop with the native walker (whole queue per call)
- ``py-scanner``  Scanner inline (one thread, today's small-scan path)
- ``pool-N``      Scanner on a spawn pool of N processes (today's parallel hard scan)
- ``native-scanner`` Scanner with worker.scan_slice swapped for the native walker (threads)

The ``*-scanner`` modes include folding results into the NodeStore on the asyncio thread, so they
are the end-to-end numbers; the ``*-slices`` modes show what the walkers alone can do.
"""

from __future__ import annotations

import argparse
import asyncio
import os
import statistics
import sys
import time
from collections import deque
from collections.abc import Callable
from concurrent.futures import Executor, ThreadPoolExecutor
from functools import partial
from typing import Any

from loguru import logger
from rich.console import Console
from rich.table import Table

from spaceknife.scan import worker
from spaceknife.scan.scanner import Scanner, ScanState, default_workers, make_pool, normalise_root
from spaceknife.scan.volume import volume_for
from spaceknife.scan.worker import RootOptions, SliceOptions, SliceResult, Task

console = Console()

try:
    import spaceknife_native as _native

    native_scan: Callable[..., dict[str, Any]] | None = _native.scan_slice
except (ImportError, AttributeError):
    native_scan = None


def root_options(path: str) -> RootOptions:
    st = os.stat(path)
    try:
        vol = volume_for(path)
    except OSError:
        vol = None
    ntfs = sys.platform == "win32" and vol is not None and vol.fs_type.upper() == "NTFS"
    return RootOptions(
        dev=st.st_dev,
        cluster_size=vol.cluster_size if vol else 4096,
        resident_max=512 if ntfs else 0,
    )


def native_opts(opts: SliceOptions, threads: int) -> dict[str, Any]:
    roots = [
        {
            "dev": r.dev,
            "cluster_size": r.cluster_size,
            "resident_max": r.resident_max,
            "cross_mounts": r.cross_mounts,
        }
        for r in opts.roots
    ]
    return {"roots": roots, "budget_s": opts.budget_s, "max_entries": opts.max_entries, "threads": threads}


def slices_loop(
    roots: list[str], scan: Callable[[list[Task], SliceOptions], SliceResult], batch: int, budget: float
) -> int:
    """A coordinator without the store: feed the frontier back until nothing is left."""
    opts = SliceOptions(roots=tuple(root_options(r) for r in roots), budget_s=budget)
    queue: deque[Task] = deque(((0, i, 0), r, i) for i, r in enumerate(roots))
    items = 0
    while queue:
        tasks = [queue.popleft() for _ in range(min(batch, len(queue)))]
        res = scan(tasks, opts)
        items += len(res.name_lens)
        queue.extend(((0, 0, 0), path, idx) for _, path, idx in res.frontier)
    return items


def native_adapter(threads: int) -> Callable[[list[Task], SliceOptions], SliceResult]:
    assert native_scan is not None
    scan = native_scan

    def run(tasks: list[Task], opts: SliceOptions) -> SliceResult:
        return SliceResult(**scan(tasks, native_opts(opts, threads)))

    return run


async def scanner_run(roots: list[str], executor: Executor | None, concurrency: int, budget: float) -> int:
    scanner = Scanner(roots, executor=executor, concurrency=concurrency, budget_s=budget)
    result = await scanner.run()
    if result.state != ScanState.DONE:
        raise RuntimeError(f"scan ended {result.state}: {result.message}")
    return result.items


def scanner_items(roots: list[str], executor: Executor | None, concurrency: int, budget: float) -> int:
    return asyncio.run(scanner_run(roots, executor, concurrency, budget))


def measure(name: str, fn: Callable[[], int], repeat: int) -> dict[str, Any]:
    fn()  # warm-up: caches, pools, thread pools
    rates: list[float] = []
    items = 0
    for _ in range(repeat):
        t0 = time.perf_counter()
        items = fn()
        rates.append(items / (time.perf_counter() - t0))
    console.print(
        f"  {name:<18} best {max(rates):>12,.0f} items/s   median {statistics.median(rates):>12,.0f}"
    )
    return {"mode": name, "items": items, "best": max(rates), "median": statistics.median(rates)}


def bench_root(
    root: str, args: argparse.Namespace, modes: set[str], pools: list[int]
) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    if "py-slices" in modes:
        rows.append(
            measure("py-slices", partial(slices_loop, [root], worker.scan_slice, 32, 0.03), args.repeat)
        )
    if "native-slices" in modes:
        run = partial(
            slices_loop, [root], native_adapter(args.threads), args.native_batch, args.native_budget
        )
        rows.append(measure("native-slices", run, args.repeat))
    if "py-scanner" in modes:
        rows.append(measure("py-scanner", partial(scanner_items, [root], None, 1, 0.03), args.repeat))
    if "pool" in modes:
        for n in pools:
            pool = make_pool(n)
            try:
                rows.append(measure(f"pool-{n}", partial(scanner_items, [root], pool, n, 0.03), args.repeat))
            finally:
                pool.shutdown()
    if "native-scanner" in modes:
        saved = worker.scan_slice
        threads = ThreadPoolExecutor(max_workers=args.native_concurrency, thread_name_prefix="native-slice")
        worker.scan_slice = native_adapter(args.threads)  # type: ignore[assignment]
        try:
            run = partial(scanner_items, [root], threads, args.native_concurrency, args.native_budget)
            rows.append(measure("native-scanner", run, args.repeat))
        finally:
            worker.scan_slice = saved
            threads.shutdown()
    return rows


def main() -> None:
    default_roots = [r"C:\Program Files", r"C:\Windows\WinSxS"] if sys.platform == "win32" else ["/usr"]
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("roots", nargs="*", default=default_roots)
    ap.add_argument("--repeat", type=int, default=3)
    ap.add_argument("--pool", default=str(default_workers()), help="comma-separated spawn pool sizes")
    ap.add_argument("--threads", type=int, default=0, help="native threads per call (0 = default)")
    ap.add_argument("--native-budget", type=float, default=0.1, help="native slice budget, seconds")
    ap.add_argument("--native-batch", type=int, default=4096, help="tasks per native slice (slices mode)")
    ap.add_argument("--native-concurrency", type=int, default=2, help="native slices in flight (scanner)")
    ap.add_argument("--modes", default="py-slices,native-slices,py-scanner,pool,native-scanner")
    args = ap.parse_args()
    logger.disable("spaceknife")
    modes = set(args.modes.split(","))
    pools = [int(x) for x in args.pool.split(",") if x]
    if native_scan is None and modes & {"native-slices", "native-scanner"}:
        console.print("[yellow]spaceknife_native.scan_slice not available: native modes skipped[/]")
        modes -= {"native-slices", "native-scanner"}

    summary = Table(title=f"hard scan, warm cache, {os.cpu_count()} logical CPUs")
    for col in ("root", "mode", "items", "best items/s", "median items/s", "vs best pool"):
        summary.add_column(col, justify="left" if col in ("root", "mode") else "right")
    for raw in args.roots:
        root = normalise_root(raw)
        console.rule(root)
        rows = bench_root(root, args, modes, pools)
        pool_best = max((r["best"] for r in rows if r["mode"].startswith("pool-")), default=0.0)
        for r in rows:
            ratio = f"{r['best'] / pool_best:.2f}x" if pool_best else "-"
            summary.add_row(
                root, r["mode"], f"{r['items']:,}", f"{r['best']:,.0f}", f"{r['median']:,.0f}", ratio
            )
    console.print(summary)


if __name__ == "__main__":
    main()
