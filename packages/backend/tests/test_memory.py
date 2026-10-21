"""Memory: closed scans are freed, work over a scan is bounded, the store gives back headroom."""

from __future__ import annotations

import asyncio
import gc
import os
import subprocess
import sys
import threading
import time
import weakref
from pathlib import Path

import numpy as np
import pytest

from spaceknife.filter import names as names_mod
from spaceknife.filter.names import name_index
from spaceknife.memory import process_memory
from spaceknife.server import Server, compute, diag
from spaceknife.server.compute import Latest, Superseded
from spaceknife.store.nodes import COLUMNS, ENTRY_COLUMNS, Kind, NodeStore
from spaceknife.view.state import HISTORY, View
from spaceknife_client.connection import Connection
from spaceknife_protocol import palette as pp
from spaceknife_protocol import scan as p
from spaceknife_protocol import view as pv
from spaceknife_protocol.methods import PALETTE_PATHS, SCAN_CLOSE, SCAN_START, VIEW_CLOSE, VIEW_OPEN

from .test_scan_server import wait_done
from .tree_fixture import build_tree


@pytest.fixture(autouse=True)
def isolated_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


def _store(files: int) -> NodeStore:
    store = NodeStore(capacity=16)
    root = store.add_root("/r")
    names = [f"file{i:05d}.txt".encode() for i in range(files)]
    entries = {k: np.zeros(files, dtype=COLUMNS[k]) for k in ENTRY_COLUMNS}
    entries["kind"][:] = Kind.FILE
    entries["size"][:] = np.arange(files)
    entries["alloc"][:] = np.arange(files) * 2
    store.ingest_names(root, names, entries)
    return store


# ------------------------------------------------------------------ closed scans are freed


async def test_closing_a_scan_frees_its_store_and_name_index(
    tmp_path: Path, server: Server, client: Connection
) -> None:
    facts = build_tree(tmp_path / "root")
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(facts.root)]))
    await wait_done(client, started.scan_id)
    upd = await client.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=started.scan_id, viewport=pv.Viewport(width=800, height=600))
    )
    # the palette builds the name index (and caches search results in it)
    await client.call(PALETTE_PATHS, pp.PalettePathsParams(scan_id=started.scan_id, text="mov"))
    store_ref = weakref.ref(server.scans.scans[started.scan_id].store)
    index_ref = weakref.ref(name_index(server.scans.scans[started.scan_id].store))
    await client.call(VIEW_CLOSE, pv.ViewIdParams(view_id=upd.state.view_id))
    await client.call(SCAN_CLOSE, p.ScanIdParams(scan_id=started.scan_id))
    for _ in range(20):
        gc.collect()
        if store_ref() is None and index_ref() is None:
            break
        await asyncio.sleep(0.05)
    assert store_ref() is None, "the closed scan's node store is still referenced"
    assert index_ref() is None, "the closed scan's name index is still referenced"


def test_name_index_does_not_keep_its_store_alive() -> None:
    store = _store(100)
    index = name_index(store)
    index.update(len(store))
    ref = weakref.ref(store)
    del store
    gc.collect()
    assert ref() is None
    with pytest.raises(ReferenceError):
        _ = index.store


def test_name_index_cache_is_bounded_by_bytes(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(names_mod, "_CACHE_BYTES", 10_000)
    store = _store(3000)
    index = name_index(store)
    for q in ("f", "fi", "fil", "file", "file0", "file00"):
        index.ascii_tiers(q, len(store), scattered=False)
    cached = sum(names_mod._nbytes(r) for _, r in index._cache.values())
    # the newest two always stay (typing refines the previous query); beyond that the byte bound holds
    assert len(index._cache) == 2 or cached <= 10_000
    assert index.memory_bytes() > 0


# ------------------------------------------------------------------ the node store


def test_compact_gives_back_headroom_and_keeps_the_data(monkeypatch: pytest.MonkeyPatch) -> None:
    from spaceknife.store import mapped

    monkeypatch.setattr(mapped.POLICY, "force_bytes", None)  # plain memory (mapped columns keep their tail)
    store = _store(5000)  # grown from 16 by half: up to a third of every column is headroom
    before = {k: a[: len(store)].copy() for k, a in store.cols.items()}
    cap = len(store.cols["kind"])
    assert cap > len(store)
    freed = store.compact()
    assert freed > 0
    assert all(len(a) == len(store) for a in store.cols.values())
    for k, a in store.cols.items():
        assert np.array_equal(a, before[k]), k
    assert store.compact() == 0  # nothing left to give back
    assert store.name(len(store) - 1) == "file04999.txt"


def test_reaggregate_counts_with_small_temporaries() -> None:
    store = _store(1000)
    want = {k: store.cols[k][: len(store)].copy() for k in ("tot_size", "tot_alloc", "tot_files", "tot_dirs")}
    store.reaggregate()
    for k, a in want.items():
        assert np.array_equal(store.cols[k][: len(store)], a), k


# ------------------------------------------------------------------ bounded work over a scan


async def test_compute_pool_bounds_concurrency() -> None:
    running = 0
    peak = 0
    lock = threading.Lock()

    def job() -> None:
        nonlocal running, peak
        with lock:
            running += 1
            peak = max(peak, running)
        time.sleep(0.05)
        with lock:
            running -= 1

    await asyncio.gather(*(compute.compute(job) for _ in range(compute.WORKERS * 4)))
    assert peak <= compute.WORKERS


async def test_latest_drops_superseded_queued_calls() -> None:
    gate = threading.Event()
    ran: list[int] = []

    def block() -> None:  # occupies every worker so the next calls queue
        gate.wait(5)

    blockers = [asyncio.ensure_future(compute.compute(block)) for _ in range(compute.WORKERS)]
    await asyncio.sleep(0.05)
    latest = Latest()
    calls = [asyncio.ensure_future(latest.run("k", ran.append, i)) for i in range(5)]
    await asyncio.sleep(0.05)
    gate.set()
    results = await asyncio.gather(*calls, return_exceptions=True)
    await asyncio.gather(*blockers)
    assert ran == [4], "only the newest queued call runs"
    assert all(isinstance(r, Superseded) for r in results[:4])
    assert len(latest) == 0


# ------------------------------------------------------------------ views


def test_view_history_is_bounded() -> None:
    store = _store(10)
    view = View("v", store, [])
    for _ in range(HISTORY + 50):
        view._push()
    assert len(view.back) == HISTORY


# ------------------------------------------------------------------ process memory and the watchdog


def test_process_memory_is_readable() -> None:
    mem = process_memory()
    if sys.platform in ("win32", "linux"):
        assert mem.private is not None and mem.private > 0
        assert mem.resident is not None and mem.resident > 0


def test_numpy_starts_without_a_thread_per_core() -> None:
    env = {k: v for k, v in os.environ.items() if k not in ("OPENBLAS_NUM_THREADS", "OMP_NUM_THREADS")}
    code = "import spaceknife, os, numpy; print(os.environ['OPENBLAS_NUM_THREADS'])"
    out = subprocess.run([sys.executable, "-c", code], env=env, capture_output=True, text=True, check=True)
    assert out.stdout.strip() == "1"


async def test_watchdog_reports_unexplained_memory(
    server: Server, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    from spaceknife.memory import ProcessMemory

    monkeypatch.setattr(diag, "PERIOD", 0.01)
    monkeypatch.setattr(diag, "process_memory", lambda: ProcessMemory(40 << 30, 30 << 30))
    seen: list[str] = []
    monkeypatch.setattr(diag, "emit", lambda source, message, **kw: seen.append(message))
    task = asyncio.ensure_future(diag.watch(server))
    try:
        for _ in range(100):
            if seen:
                break
            await asyncio.sleep(0.01)
    finally:
        task.cancel()
    assert seen and "40.0 GB" in seen[0]
    text = diag.report(server)
    assert "accounted" in text and "compute" in text


async def test_closing_a_scan_gives_the_heap_back(
    tmp_path: Path, server: Server, client: Connection, monkeypatch: pytest.MonkeyPatch
) -> None:
    # the C heap keeps freed space for the next scan (docs/memory.md §8): after a close it is asked
    # to give it back, and the closed scan's size is remembered for the watchdog
    from spaceknife.server import scans as scans_mod

    released: list[int] = []
    monkeypatch.setattr(scans_mod, "RELEASE_DELAY_S", 0.0)
    monkeypatch.setattr(scans_mod.memory, "release", lambda: released.append(1))
    facts = build_tree(tmp_path / "root")
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(facts.root)]))
    await wait_done(client, started.scan_id)
    await client.call(SCAN_CLOSE, p.ScanIdParams(scan_id=started.scan_id))
    for _ in range(100):
        if released:
            break
        await asyncio.sleep(0.01)
    assert released
    assert len(server.scans.closed) == 1 and server.scans.closed[0][1] > 0
    assert diag.retained(server) == server.scans.closed[0][1]


def test_release_runs_on_this_platform() -> None:
    from spaceknife.memory import release

    release()  # best effort: never raises


async def _watch_briefly(server: Server) -> None:
    task = asyncio.ensure_future(diag.watch(server))
    try:
        await asyncio.sleep(0.2)
    finally:
        task.cancel()


async def test_watchdog_allows_for_what_closed_scans_leave_in_the_heap(
    server: Server, monkeypatch: pytest.MonkeyPatch
) -> None:
    from spaceknife.memory import ProcessMemory

    monkeypatch.setattr(diag, "PERIOD", 0.01)
    monkeypatch.setattr(diag, "release", lambda: None)
    monkeypatch.setattr(diag, "process_memory", lambda: ProcessMemory(3 << 30, 2 << 30))
    seen: list[str] = []
    monkeypatch.setattr(diag, "emit", lambda source, message, **kw: seen.append(message))
    # a 2 GiB scan closed a minute ago: 3 GiB private is within 1.5 GiB slack + 2 GiB retained
    server.scans.closed = [(time.monotonic() - 60, 2 << 30)]
    await _watch_briefly(server)
    assert not seen
    # an hour later it no longer counts
    server.scans.closed = [(time.monotonic() - 2 * 3600, 2 << 30)]
    await _watch_briefly(server)
    assert seen


async def test_watchdog_gives_memory_back_before_warning(
    server: Server, monkeypatch: pytest.MonkeyPatch
) -> None:
    from spaceknife.memory import ProcessMemory

    monkeypatch.setattr(diag, "PERIOD", 0.01)
    state = {"private": 3 << 30}
    monkeypatch.setattr(diag, "release", lambda: state.update(private=1 << 30))
    monkeypatch.setattr(diag, "process_memory", lambda: ProcessMemory(state["private"], 1 << 30))
    seen: list[str] = []
    monkeypatch.setattr(diag, "emit", lambda source, message, **kw: seen.append(message))
    await _watch_briefly(server)
    assert state["private"] == 1 << 30 and not seen
