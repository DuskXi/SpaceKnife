"""Subtree summaries: chunked walks give the same result as whole-subtree arrays, and the server
computes one summary per store version, one at a time (docs/memory.md §5)."""

from __future__ import annotations

import gc
import threading
import time
import weakref

import numpy as np
import pytest

from spaceknife import pathclass
from spaceknife.classes import DEFAULT_CLASSES
from spaceknife.layout.basis import Basis
from spaceknife.server import scans
from spaceknife.store import subtree
from spaceknife.store.nodes import COLUMNS, ENTRY_COLUMNS, Flag, Kind, NodeStore
from spaceknife.store.subtree import Summary, subtree_chunks, subtree_ids, summarize

DUP = np.uint32(Flag.HARDLINK_DUP)
EXTS = ("", "mp4", "txt", "dll", "png", "zip")


def random_store(seed: int, size: int, *, wide: int = 12) -> tuple[NodeStore, int]:
    """A random tree of about ``size`` nodes: repeated sizes (ties), hard-link duplicates,
    unlisted folders, empty folders."""
    rng = np.random.default_rng(seed)
    store = NodeStore()
    root = store.add_root("/r")
    ext_ids = np.array([store.ext_id(e) for e in EXTS], dtype=np.uint16)
    pending = [root]
    k = 0
    while pending and len(store) < size:
        d = pending.pop(int(rng.integers(len(pending))))
        m = int(rng.integers(0, wide))
        dirs = int(rng.integers(0, min(m, 3) + 1))
        if not pending:  # keep the tree growing until it has its size
            m, dirs = max(m, 2), max(dirs, 1)
        entries = {key: np.zeros(m, dtype=COLUMNS[key]) for key in ENTRY_COLUMNS}
        entries["kind"][:dirs] = Kind.DIR
        entries["kind"][dirs:] = Kind.FILE
        entries["alloc"][dirs:] = rng.choice([0, 4096, 8192, 1 << 20, 123_456_789], m - dirs)
        entries["mtime"][:] = rng.integers(0, 2**31, m)
        entries["flags"][dirs:] = np.where(rng.random(m - dirs) < 0.1, DUP, 0)
        start = store.ingest_names(
            d, [f"n{k}_{i}".encode() for i in range(m)], entries, rng.choice(ext_ids, m)
        )
        pending.extend(range(start, start + dirs))
        k += 1
    return store, root


def by_ext(store: NodeStore):  # type: ignore[no-untyped-def]
    return lambda ids: (store.cols["ext"][ids] % 4).astype(np.uint8)


def by_id(ids: np.ndarray) -> np.ndarray:
    return (ids % 4).astype(np.uint8)


def reference(store: NodeStore, node: int, top: int) -> Summary:
    """The documented result, the slow and obvious way."""
    c = store.cols
    ids = subtree_ids(store, node) if c["kind"][node] == Kind.DIR else np.array([node], dtype=np.int64)
    files = ids[(c["kind"][ids] != Kind.DIR) & ((c["flags"][ids] & DUP) == 0)]
    alloc = c["alloc"][files].astype(np.int64)
    cls = by_ext(store)(files).astype(np.int64)
    base = by_id(files).astype(np.int64)
    parts = np.zeros((4, 4), dtype=np.int64)
    for a, cl, b in zip(alloc.tolist(), cls.tolist(), base.tolist(), strict=True):
        if cl != b:
            parts[cl, b] += a
    return Summary(
        top_files=files[np.lexsort((files, -alloc))][:top],
        class_bytes=np.array([int(alloc[cls == i].sum()) for i in range(4)], dtype=np.int64),
        class_files=np.array([int((cls == i).sum()) for i in range(4)], dtype=np.int64),
        latest_mtime=int(c["mtime"][ids].max()) if ids.size else 0,
        class_parts=parts if bool(np.any(cls != base)) else None,
    )


def same(a: Summary, b: Summary) -> None:
    assert a.top_files.tolist() == b.top_files.tolist()
    assert a.class_bytes.tolist() == b.class_bytes.tolist()
    assert a.class_files.tolist() == b.class_files.tolist()
    assert a.latest_mtime == b.latest_mtime
    assert (a.class_parts is None) == (b.class_parts is None)
    if a.class_parts is not None and b.class_parts is not None:
        assert a.class_parts.tolist() == b.class_parts.tolist()


@pytest.mark.parametrize("seed", range(4))
def test_chunked_summaries_match_the_whole_subtree(seed: int) -> None:
    store, root = random_store(seed, 3000)
    rng = np.random.default_rng(100 + seed)
    nodes = [0, root, *rng.integers(1, len(store), 8).tolist()]
    for node in nodes:
        for top in (0, 1, 5, 50):
            want = reference(store, node, top)
            for chunk in (1, 3, 64, subtree.SUMMARY_CHUNK) if node == root else (2, 7, subtree.SUMMARY_CHUNK):
                same(summarize(store, node, by_ext(store), 4, top, by_id, chunk=chunk), want)


def test_chunks_visit_every_node_once_and_stay_small() -> None:
    store, _root = random_store(7, 5000, wide=40)
    big = store.ingest_names(  # one folder far larger than a chunk: its range is cut
        int(
            np.flatnonzero(
                (store.cols["kind"][: len(store)] == Kind.DIR)
                & (store.cols["child_count"][: len(store)] == 0)
            )[0]
        ),
        [f"f{i}".encode() for i in range(500)],
        {key: np.zeros(500, dtype=COLUMNS[key]) for key in ENTRY_COLUMNS},
    )
    assert big > 0
    for chunk in (1, 5, 16, 100, 1 << 20):
        pieces = list(subtree_chunks(store, 0, chunk))
        assert all(p.size <= 2 * chunk for p in pieces)
        got = np.concatenate(pieces)
        assert got.size == np.unique(got).size == len(store) - 1  # every node below node 0, once
        assert set(got.tolist()) == set(subtree_ids(store, 0).tolist())


def test_files_and_empty_folders_summarise_themselves() -> None:
    store, _root = random_store(3, 400)
    c = store.cols
    n = len(store)
    file = int(np.flatnonzero((c["kind"][:n] == Kind.FILE) & ((c["flags"][:n] & DUP) == 0))[0])
    s = summarize(store, file, by_ext(store), 4, 5, by_id)
    assert s.top_files.tolist() == [file] and s.latest_mtime == int(c["mtime"][file])
    small = NodeStore()
    entries = {key: np.zeros(1, dtype=COLUMNS[key]) for key in ENTRY_COLUMNS}
    entries["kind"][0] = Kind.DIR
    empty = small.ingest_names(small.add_root("/e"), [b"empty"], entries)
    small.ingest_names(empty, [], {key: np.zeros(0, dtype=COLUMNS[key]) for key in ENTRY_COLUMNS})
    s = summarize(small, empty, by_ext(small), 4, 5, by_id)
    assert s.top_files.size == 0 and s.latest_mtime == 0 and int(s.class_files.sum()) == 0


class Counting:
    """``subtree.summarize`` that counts its calls and how many run at once."""

    def __init__(self, gate: threading.Event | None = None) -> None:
        self.real = subtree.summarize
        self.calls = 0
        self.running = 0
        self.most = 0
        self.gate = gate
        self.lock = threading.Lock()

    def __call__(self, *args, **kwargs):  # type: ignore[no-untyped-def]
        with self.lock:
            self.calls += 1
            self.running += 1
            self.most = max(self.most, self.running)
        try:
            if self.gate is not None:
                self.gate.wait(10)
            else:
                time.sleep(0.02)
            return self.real(*args, **kwargs)
        finally:
            with self.lock:
                self.running -= 1


def test_the_server_computes_a_summary_once_per_store_version(monkeypatch: pytest.MonkeyPatch) -> None:
    store, root = random_store(11, 2000)
    basis = Basis(store, DEFAULT_CLASSES)
    spy = Counting()
    monkeypatch.setattr(subtree, "summarize", spy)
    five = scans.cached_summary(store, basis, root, 5)
    fifty = scans.cached_summary(store, basis, root, 50)
    none = scans.cached_summary(store, basis, root, 0)
    assert spy.calls == 1  # the details panel's two pages and the Disks page: one walk
    assert len(five.top_files) == 5 and fifty.top_files[:5].tolist() == five.top_files.tolist()
    assert none.top_files.size == 0 and none.class_bytes.tolist() == five.class_bytes.tolist()
    same(fifty, spy.real(store, root, basis.cls_of, len(basis.class_ids), 50, basis.ext_cls_of))

    scans.cached_summary(store, basis, root, 500)  # more than was kept: computed again
    assert spy.calls == 2
    entries = {key: np.zeros(1, dtype=COLUMNS[key]) for key in ENTRY_COLUMNS}
    folder = int(
        np.flatnonzero(
            (store.cols["kind"][: len(store)] == Kind.DIR) & (store.cols["child_count"][: len(store)] == 0)
        )[0]
    )
    store.ingest_names(folder, [b"new"], entries)  # the store changed: a new version
    scans.cached_summary(store, basis, root, 5)
    assert spy.calls == 3
    monkeypatch.setattr(pathclass, "folders_version", lambda: 10**9)  # the user moved a folder class
    scans.cached_summary(store, basis, root, 5)
    assert spy.calls == 4
    scans.cached_summary(store, basis, root, 5, cache=False)  # a running scan: never kept
    scans.cached_summary(store, basis, root, 5, cache=False)
    assert spy.calls == 6


def test_concurrent_callers_share_one_walk(monkeypatch: pytest.MonkeyPatch) -> None:
    store, root = random_store(12, 2000)
    basis = Basis(store, DEFAULT_CLASSES)
    gate = threading.Event()
    spy = Counting(gate)
    monkeypatch.setattr(subtree, "summarize", spy)
    got: list[Summary] = []
    threads = [
        threading.Thread(target=lambda: got.append(scans.cached_summary(store, basis, root, 5)))
        for _ in range(4)
    ]
    for t in threads:
        t.start()
    deadline = time.monotonic() + 10
    while not spy.calls and time.monotonic() < deadline:
        time.sleep(0.005)
    time.sleep(0.05)  # the others are at the lock (or come later and find the result)
    gate.set()
    for t in threads:
        t.join(10)
    assert spy.calls == 1 and len(got) == 4
    assert all(g.top_files.tolist() == got[0].top_files.tolist() for g in got)


def test_summaries_of_different_nodes_run_one_at_a_time(monkeypatch: pytest.MonkeyPatch) -> None:
    store, _root = random_store(13, 2000)
    basis = Basis(store, DEFAULT_CLASSES)
    spy = Counting()
    monkeypatch.setattr(subtree, "summarize", spy)
    dirs = np.flatnonzero(store.cols["kind"][: len(store)] == Kind.DIR)[:6].tolist()
    threads = [threading.Thread(target=scans.cached_summary, args=(store, basis, int(d), 5)) for d in dirs]
    for t in threads:
        t.start()
    for t in threads:
        t.join(10)
    assert spy.calls == len(dirs) and spy.most == 1


def test_the_summary_cache_does_not_keep_a_closed_scan_alive() -> None:
    store, root = random_store(14, 500)
    scans.cached_summary(store, Basis(store, DEFAULT_CLASSES), root, 5)
    ref = weakref.ref(store)
    del store
    gc.collect()
    assert ref() is None
