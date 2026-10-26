"""How the node store's columns grow (docs/memory.md §3): by half while a scan adds rows, a little
at a time once the tree is complete, so a file change never doubles a whole volume's columns."""

from __future__ import annotations

import itertools

import numpy as np
import pytest

from spaceknife.store import mapped, nodes
from spaceknife.store.nodes import COLUMNS, ENTRY_COLUMNS, Kind, NodeStore


@pytest.fixture(autouse=True)
def plain_memory(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(mapped.POLICY, "force_bytes", None)  # mapped columns keep their tail when compacted


def entries(dirs: int, files: int) -> dict[str, np.ndarray]:
    m = dirs + files
    out = {key: np.zeros(m, dtype=COLUMNS[key]) for key in ENTRY_COLUMNS}
    out["kind"][:dirs] = Kind.DIR
    out["kind"][dirs:] = Kind.FILE
    out["alloc"][dirs:] = np.arange(files) + 1
    return out


def grow_tree(store: NodeStore, root: int, rows: int) -> list[int]:
    """Add folders of 7 files and 3 subfolders breadth first until ``rows`` nodes; the capacities seen."""
    caps = [store._cap]
    pending = [root]
    k = 0
    while len(store) < rows:
        d = pending.pop(0)
        start = store.ingest_names(d, [f"n{k}_{i}".encode() for i in range(10)], entries(3, 7))
        pending.extend(range(start, start + 3))
        k += 1
        if store._cap != caps[-1]:
            caps.append(store._cap)
    return caps


def leaf(store: NodeStore) -> int:
    c = store.cols
    n = len(store)
    return int(np.flatnonzero((c["kind"][:n] == Kind.DIR) & (c["child_count"][:n] == 0))[0])


def test_a_scan_grows_the_columns_by_half() -> None:
    store = NodeStore()
    root = store.add_root("/r")
    caps = grow_tree(store, root, 200_000)
    assert len(caps) > 5  # from 16 Ki rows
    for before, after in itertools.pairwise(caps):
        assert after <= int(before * nodes.GROWTH) + 1 + 10  # one listing more at most
    assert store._cap <= nodes.GROWTH * len(store) + 16  # at most a third unused, not a half
    assert all(len(col) == store._cap for col in store.cols.values())


def test_a_complete_tree_grows_a_little_at_a_time(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(nodes, "SETTLED_MIN_STEP", 64)
    store = NodeStore()
    root = store.add_root("/r")
    grow_tree(store, root, 20_000)
    alloc = store.col("alloc").copy()
    store.compact()
    n = len(store)
    assert store._cap == n
    store.ingest_names(leaf(store), [b"new"], entries(0, 1))  # a file created after the scan
    assert store._cap == n + 1 + (n + 1) // nodes.SETTLED_SHARE  # not 2 * n
    assert store.col("alloc")[:n].tolist() == alloc.tolist()  # nothing lost on the way
    cap = store._cap
    for i in range(cap - len(store)):  # the headroom fills before anything is copied again
        store.ingest_names(leaf(store), [f"x{i}".encode()], entries(0, 1))
        assert store._cap == cap
    store.ingest_names(leaf(store), [b"over"], entries(0, 1))
    assert store._cap == len(store) + len(store) // nodes.SETTLED_SHARE


def test_small_complete_trees_get_a_minimum_step() -> None:
    store = NodeStore()
    root = store.add_root("/r")
    grow_tree(store, root, 1000)
    store.compact()
    store.ingest_names(leaf(store), [b"new"], entries(0, 1))
    assert store._cap == len(store) + nodes.SETTLED_MIN_STEP


def test_loaded_and_adopted_stores_are_complete() -> None:
    built = NodeStore()
    root = built.add_root("/r")
    grow_tree(built, root, 5000)
    loaded = NodeStore.from_columns(built.export_columns(), built.names_blob, built.ext_table, separated=True)
    n = len(loaded)
    loaded.ingest_names(leaf(loaded), [b"new"], entries(0, 1))
    assert loaded._cap == n + 1 + nodes.SETTLED_MIN_STEP  # a snapshot or an MFT tree: not doubled
    assert loaded.name(n) == "new"

    target = NodeStore()
    target.adopt(
        NodeStore.from_columns(built.export_columns(), built.names_blob, built.ext_table, separated=True)
    )
    target.ingest_names(leaf(target), [b"new"], entries(0, 1))
    assert target._cap == n + 1 + nodes.SETTLED_MIN_STEP
