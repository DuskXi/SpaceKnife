"""Reading a node store while a scan adds listings in another thread.

A side-panel summary walked a subtree while the scan listed folders; reading a folder's child
range start before its count could pair the old start 0 with a fresh count — a range over the top
of the tree — and the walk revisited the tree again and again (tens of GB within a minute).
"""

from __future__ import annotations

import sys
import threading

import numpy as np

from spaceknife import pathclass
from spaceknife.classes import DEFAULT_CLASSES
from spaceknife.layout.basis import Basis
from spaceknife.layout.engine import Layout, LayoutParams, RootSpec
from spaceknife.store.nodes import COLUMNS, ENTRY_COLUMNS, Kind, NodeStore
from spaceknife.store.subtree import subtree_ids, summarize


def _entries(dirs: int, files: int) -> dict[str, np.ndarray]:
    m = dirs + files
    entries = {key: np.zeros(m, dtype=COLUMNS[key]) for key in ENTRY_COLUMNS}
    entries["kind"][:dirs] = Kind.DIR
    entries["kind"][dirs:] = Kind.FILE
    entries["alloc"][dirs:] = 4096
    return entries


def test_a_half_published_range_reads_as_not_listed() -> None:
    store = NodeStore()
    root = store.add_root("/r")
    first = store.ingest_names(root, [b"a", b"b", b"c"], _entries(2, 1))
    folder = first  # listed below: simulate the moment its count is written but its start is not
    store.cols["child_count"][folder] = 2
    assert store.child_range(folder) == (0, 0)
    assert list(store.children(folder)) == []
    assert set(subtree_ids(store, 0).tolist()) == {root, first, first + 1, first + 2}
    store.cols["child_count"][folder] = 0
    kids = store.ingest_names(folder, [b"x", b"y"], _entries(0, 2))
    assert store.child_range(folder) == (kids, kids + 2)


def test_subtree_walks_stay_exact_while_a_scan_ingests() -> None:
    store = NodeStore()
    root = store.add_root("/r")
    pending = [root]
    stop = threading.Event()
    problems: list[str] = []

    def writer() -> None:
        k = 0
        while pending and len(store) < 150_000 and not stop.is_set():
            d = pending.pop(0)
            start = store.ingest_names(d, [f"n{k}_{i}".encode() for i in range(8)], _entries(4, 4))
            pending.extend(range(start, start + 4))
            k += 1

    def reader() -> None:
        while not stop.is_set():
            ids = subtree_ids(store, 0)
            n = len(store)
            if ids.size != np.unique(ids).size or ids.size > n:
                problems.append(f"{ids.size} ids, {np.unique(ids).size} distinct, {n} nodes")
                return

    old = sys.getswitchinterval()
    sys.setswitchinterval(1e-6)  # switch threads as often as possible: provoke the race
    try:
        threads = [threading.Thread(target=writer)] + [threading.Thread(target=reader) for _ in range(2)]
        for t in threads:
            t.start()
        threads[0].join(30)
        stop.set()
        for t in threads:
            t.join()
    finally:
        sys.setswitchinterval(old)
    assert not problems, problems[0]
    assert subtree_ids(store, 0).size == len(store) - 1  # every node below the virtual root, once


def test_summaries_classify_every_node_while_a_scan_ingests() -> None:
    """The details panel's summaries during a hard scan: each one classifies the nodes of its walk
    with the folder class column, which these threads work out while the writer folds listings."""
    store = NodeStore()
    root = store.add_root("/r")
    pending = [root]
    stop = threading.Event()
    problems: list[str] = []
    basis = Basis(store, DEFAULT_CLASSES)

    def writer() -> None:
        k = 0
        while pending and len(store) < 40_000 and not stop.is_set():
            d = pending.pop(0)
            names = [f"n{k}_{i}".encode() for i in range(8)]
            if k % 5 == 0:
                names[4] = b"Cargo.toml"  # a project: its folder's subtree is worked out again
            start = store.ingest_names(d, names, _entries(4, 4))
            pending.extend(range(start, start + 4))
            k += 1

    def reader() -> None:
        while not stop.is_set():
            try:
                summarize(store, root, basis.cls_of, len(basis.class_ids), 5)
            except Exception as exc:
                problems.append(f"{type(exc).__name__}: {exc}")
                return

    old = sys.getswitchinterval()
    sys.setswitchinterval(1e-6)  # switch threads as often as possible: provoke the race
    try:
        threads = [threading.Thread(target=writer)] + [threading.Thread(target=reader) for _ in range(2)]
        for t in threads:
            t.start()
        threads[0].join(30)
        stop.set()
        for t in threads:
            t.join()
    finally:
        sys.setswitchinterval(old)
    assert not problems, problems[0]
    kept = pathclass.column(store, DEFAULT_CLASSES).copy()
    pathclass._entries.pop(store, None)
    assert np.array_equal(kept, pathclass.column(store, DEFAULT_CLASSES))  # as if worked out at the end


def test_layout_skips_a_half_published_range() -> None:
    store = NodeStore()
    root = store.add_root("/r")
    first = store.ingest_names(root, [b"a", b"b"], _entries(1, 1))
    store.cols["child_count"][first] = 5  # count written, start still 0
    layout = Layout(store)
    draw = layout.run(LayoutParams(width=400, height=300), RootSpec(node=root))
    assert 0 not in draw.node.tolist()  # the virtual root never shows up inside a folder
