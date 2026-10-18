"""Memory per item and search latency of a scan, in plain memory or memory-mapped.

    uv run python tools/bench_memory.py --snapshot PATH.sks [--mapped]

Loads a snapshot, builds the name index the way the palette and filters use it, and prints bytes
per item for each structure, the private memory the process gained, and palette / glob latency.
``--mapped`` forces the memory-mapped mode (columns and names in self-deleting files).
"""

from __future__ import annotations

import argparse
import gc
import os
import sys
import tempfile
import time
from pathlib import Path


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--snapshot", required=True, type=Path)
    ap.add_argument("--mapped", action="store_true", help="force the memory-mapped mode")
    args = ap.parse_args()
    os.environ.setdefault("SPACEKNIFE_HOME", tempfile.mkdtemp(prefix="sk-bench-"))
    if args.mapped:
        os.environ["SPACEKNIFE_MAP_ABOVE"] = "1"

    from spaceknife.filter.names import name_index
    from spaceknife.layout.engine import Layout
    from spaceknife.memory import process_memory
    from spaceknife.palette import paths as ppaths
    from spaceknife.store import snapshot

    def private_mb() -> float:
        gc.collect()
        return (process_memory().private or 0) / 2**20

    def best_ms(fn: object, reps: int = 3) -> tuple[float, object]:
        best, out = 1e9, None
        for _ in range(reps):
            t = time.perf_counter()
            out = fn()  # type: ignore[operator]
            best = min(best, time.perf_counter() - t)
        return best * 1000, out

    base = private_mb()
    t = time.perf_counter()
    store, header = snapshot.load(args.snapshot)
    load_s = time.perf_counter() - t
    n = len(store)
    after_load = private_mb()
    cols = sum(a.nbytes for a in store.cols.values())
    index = name_index(store)
    index.update(n)
    for q in ("win", "cache"):
        index.ascii_tiers(q, n, scattered=True)
    sizes = Layout(store).sizes()
    after_index = private_mb()
    lower = sum(a.nbytes for a in index._lower.values())
    mode = "memory-mapped" if store.mapped else "plain memory"
    print(f"{n:,} items, snapshot format {header.format}, {mode}, loaded in {load_s:.2f} s")
    print(f"  columns      {cols / n:6.1f} B/item")
    print(f"  names        {store.names_size() / n:6.1f} B/item (with the NUL)")
    core = (index.memory_bytes() - lower) / n
    print(f"  name index   {core:6.1f} B/item + lower-cased cache {lower / 2**20:.0f} MB")
    print(f"  layout sizes {sizes.nbytes / n:6.1f} B/item")
    print(
        f"  private memory: +{after_load - base:.0f} MB after loading, +{after_index - base:.0f} MB with the "
        f"index ({(after_index - base) * 2**20 / n:.0f} B/item)"
    )
    for q in ("d", "dll", "node_modules"):
        ms, (_, total) = best_ms(lambda q=q: ppaths.search(store, sizes, q, 50))  # type: ignore[misc]
        print(f"  palette {q!r:16} {ms:7.1f} ms  ({total:,} hits)")
    for pat in ("*.dll", "*cache*", "win*"):
        index._cache.clear()
        ms, found = best_ms(lambda pat=pat: index.match(pat, n), reps=1)  # type: ignore[misc]
        print(f"  glob {pat!r:11} {ms:7.1f} ms  ({len(found):,} matches, uncached)")  # type: ignore[arg-type]
    return 0


if __name__ == "__main__":
    sys.exit(main())
