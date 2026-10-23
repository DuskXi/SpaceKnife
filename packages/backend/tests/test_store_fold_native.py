"""The native fold of slices (spaceknife_native.SliceFold behind NodeStore.ingest_slices) against
the numpy fold (NodeStore._ingest_slice_py, what ingest_slice did before): column by column, names,
starts, version and row count, on random trees folded in random groups of slices; folders that
leave the tree while they are read (GONE), mount points, hard links, wide name offsets, the
checks that refuse a slice, and readers in another thread while the GIL is released."""

from __future__ import annotations

import threading
import weakref
from collections.abc import Iterator
from contextlib import contextmanager
from typing import Any

import numpy as np
import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from spaceknife.store import nodes
from spaceknife.store.nodes import COLUMNS, ENTRY_COLUMNS, Flag, Kind, NodeStore, SlicePart

NATIVE = nodes._native_fold()
pytestmark = pytest.mark.skipif(NATIVE is None, reason="needs the native extension")


@contextmanager
def fold_with(native: bool) -> Iterator[None]:
    saved = nodes.NATIVE_FOLD
    nodes.NATIVE_FOLD = NATIVE if native else None
    try:
        yield
    finally:
        nodes.NATIVE_FOLD = saved


def ingest(store: NodeStore, parts: list[SlicePart], native: bool) -> list[list[int]]:
    with fold_with(native):
        return [s.tolist() for s in store.ingest_slices(parts)]


def assert_same(a: NodeStore, b: NodeStore) -> None:
    assert (len(a), a.version) == (len(b), b.version)
    n = len(a)
    for key in COLUMNS:
        assert a.cols[key].dtype == b.cols[key].dtype, key
        assert np.array_equal(a.cols[key][:n], b.cols[key][:n]), key
    assert a.names_blob == b.names_blob
    assert a._heap._bases == b._heap._bases and a._heap._used == b._heap._used


def random_part(
    rng: np.random.Generator, pending: list[int], dtypes: str
) -> tuple[SlicePart, list[tuple[int, int]]]:
    """Listings of ``pending`` store nodes and of folders found earlier in the same part; the
    columns in the scanner's types, or (``dtypes == "loose"``) in others the store converts."""
    refs: list[tuple[int, int, int]] = []
    found: list[tuple[int, int]] = []
    bounds = [0]
    cols: dict[str, list[int]] = {key: [] for key in ENTRY_COLUMNS}
    names: list[bytes] = []
    for k in range(int(rng.integers(0, 12))):
        if found and (not pending or rng.random() < 0.7):
            j, i = found.pop(int(rng.integers(len(found))))
            refs.append((1, j, i))
        elif pending:
            refs.append((0, pending.pop(int(rng.integers(len(pending)))), 0))
        else:
            break
        for i in range(int(rng.integers(0, 9))):
            kind = int(rng.choice([Kind.FILE, Kind.FILE, Kind.DIR, Kind.DIR, Kind.LINK, Kind.OTHER]))
            flags = int(rng.choice([0, Flag.HIDDEN, Flag.HARDLINK_DUP, Flag.REPARSE | Flag.HARDLINK_DUP]))
            if kind == Kind.DIR and rng.random() < 0.15:
                flags |= Flag.MOUNT
            size = int(rng.integers(0, 2**45)) if rng.random() < 0.3 else int(rng.integers(0, 5000))
            cols["kind"].append(kind)
            cols["flags"].append(flags)
            cols["size"].append(size if kind != Kind.DIR else 0)
            cols["alloc"].append(int(rng.integers(0, 2**40)) if rng.random() < 0.2 else size // 2)
            for t in ("mtime", "ctime", "atime"):
                cols[t].append(int(rng.integers(0, 2**32)))
            cols["ino"].append(int(rng.integers(0, 2**63)))
            cols["nlink"].append(int(rng.integers(1, 4)))
            names.append(f"n{k}.{i}{'é' * int(rng.integers(0, 3))}{'𝄞' * int(rng.integers(0, 2))}".encode())
            if kind == Kind.DIR and not flags & Flag.MOUNT:
                found.append((k, i))
        bounds.append(len(names))
    if dtypes == "loose":  # what tests and older callers hand in: plain int64 columns, lists
        entries = {key: np.array(v, dtype=np.int64) for key, v in cols.items()}
        entries["ino"] = np.array(cols["ino"], dtype=np.uint64)
        lens: np.ndarray = np.array([len(n) for n in names], dtype=np.int64)
        part_refs: Any = refs
        blob: bytes | memoryview | np.ndarray = memoryview(b"".join(names))
    else:
        entries = {key: np.array(v, dtype=COLUMNS[key]) for key, v in cols.items()}
        lens = np.array([len(n) for n in names], dtype=np.uint16)
        part_refs = np.array(refs, dtype=np.int64).reshape(-1, 3)
        blob = b"".join(names)
    ext: np.ndarray | None = None if rng.random() < 0.2 else rng.integers(0, 3, len(names)).astype(np.uint16)
    part: SlicePart = (part_refs, np.array(bounds, dtype=np.int64), blob, lens, entries, ext)
    return part, found


def two_stores(wide: bool) -> tuple[NodeStore, NodeStore]:
    pair = (NodeStore(capacity=16), NodeStore(capacity=16))  # small: the folds grow the columns
    for s in pair:
        s.add_root("C:\\a")
        s.add_root("D:\\b")
        s.ext_id("x")
        s.ext_id("y")
        if wide:  # as past 4 GiB of names: offsets in int64
            s.cols["name_off"] = s.cols["name_off"].astype(np.int64)
    return pair


@settings(max_examples=250, deadline=None)
@given(st.integers(0, 2**32 - 1), st.integers(1, 6), st.sampled_from(["scanner", "loose"]), st.booleans())
def test_the_native_fold_is_the_numpy_fold(seed: int, rounds: int, dtypes: str, wide: bool) -> None:
    rng = np.random.default_rng(seed)
    native, numpy_ = two_stores(wide)
    pending = [1, 2]
    for _ in range(rounds):
        parts: list[SlicePart] = []
        lefts: list[list[tuple[int, int]]] = []
        for _ in range(int(rng.integers(1, 5))):  # slices landing together
            part, left = random_part(rng, pending, dtypes)
            parts.append(part)
            lefts.append(left)
        # a monitor relisted folders above some of them meanwhile: they left the tree
        listed = [int(r[1]) for p in parts for r in np.asarray(p[0]).reshape(-1, 3) if r[0] == 0]
        for node in listed:
            if rng.random() < 0.15:
                for s in (native, numpy_):
                    s.mark(node, Flag.GONE)
        starts = ingest(native, parts, True)
        assert starts == ingest(numpy_, parts, False)
        assert_same(native, numpy_)
        for st_, left in zip(starts, lefts, strict=True):
            pending += [st_[j] + i for j, i in left if st_[j] >= 0]
    # the totals agree with a full recount (GONE rows aside: a monitor takes them out)
    n = len(native)
    if not (native.cols["flags"][:n] & np.uint32(Flag.GONE)).any():
        before = {k: native.cols[k][:n].copy() for k in ("tot_size", "tot_alloc", "tot_files", "tot_dirs")}
        native.reaggregate()
        for k, col in before.items():
            assert np.array_equal(native.cols[k][:n], col), k


def _entries(kinds: list[int]) -> dict[str, np.ndarray]:
    e = {key: np.zeros(len(kinds), dtype=COLUMNS[key]) for key in ENTRY_COLUMNS}
    e["kind"] = np.array(kinds, dtype=np.uint8)
    return e


def test_edge_cases_agree() -> None:
    for native in (True, False):
        s = NodeStore()
        root = s.add_root("/r")
        v = s.version
        assert ingest(s, [], native) == []
        # no listings at all: nothing changes, not even the version
        empty: SlicePart = (
            np.zeros((0, 3), dtype=np.int64),
            np.zeros(1, dtype=np.int64),
            b"",
            np.zeros(0),
            _entries([]),
            None,
        )
        assert ingest(s, [empty], native) == [[]]
        assert s.version == v
        # an empty folder: listed (not pending), no rows
        one: SlicePart = (np.array([[0, root, 0]]), np.array([0, 0]), b"", np.zeros(0), _entries([]), None)
        assert ingest(s, [one], native) == [[2]]
        assert s.version == v + 1 and len(s) == 2
        assert not s.cols["flags"][root] & Flag.PENDING and s.cols["child_count"][root] == 0
        # every listing of a folder that left the tree: all skipped, nothing written
        t = NodeStore()
        r2 = t.add_root("/r")
        t.mark(r2, Flag.GONE)
        v2 = t.version
        both = (
            np.array([[0, r2, 0], [1, 0, 0]]),
            np.array([0, 1, 2]),
            b"ab",
            np.array([1, 1]),
            _entries([Kind.DIR, 0]),
            None,
        )
        assert ingest(t, [both], native) == [[-1, -1]]
        assert (len(t), t.version) == (2, v2)


@pytest.mark.parametrize("native", [True, False])
def test_bad_slices_are_refused_alike(native: bool) -> None:
    store = NodeStore()
    root = store.add_root("C:\\r")
    entries = {key: np.zeros(1, dtype=np.int64) for key in ENTRY_COLUMNS}
    entries["kind"] = np.array([Kind.FILE], dtype=np.uint8)
    bad = [
        (np.array([[1, 0, 0]]), "earlier listing"),
        (np.array([[0, 99, 0]]), "not in the store"),
        (np.array([[0, root, 0], [1, 0, 0]]), "not a directory"),
        (np.array([[0, root, 0], [1, 0, 5]]), "does not have"),
        (np.array([[0, root, 0], [0, root, 0]]), "already listed"),
    ]
    for refs, why in bad:
        b = np.arange(len(refs) + 1).clip(max=1) if len(refs) > 1 else np.array([0, 1])
        before = {k: store.cols[k][: len(store)].copy() for k in COLUMNS}
        with fold_with(native), pytest.raises(ValueError, match=why):
            store.ingest_slice(refs, b, b"f", np.array([1]), entries)
        assert len(store) == 2 and store.cols["child_count"][root] == 0
        assert all(np.array_equal(store.cols[k][: len(store)], v) for k, v in before.items())
    with fold_with(native):
        store.ingest_slice(np.array([[0, root, 0]]), np.array([0, 1]), b"f", np.array([1]), entries)
        with pytest.raises(ValueError, match="already listed"):
            store.ingest_slice(np.array([[0, root, 0]]), np.array([0, 1]), b"f", np.array([1]), entries)


def test_big_slices_grow_the_columns_and_the_heap_alike() -> None:
    rng = np.random.default_rng(7)
    a, b = NodeStore(capacity=16), NodeStore(capacity=16)
    for s in (a, b):
        s.add_root("/r")
    n = 300_000  # past several heap chunks and column growths
    kinds = rng.choice([Kind.FILE, Kind.DIR], n).astype(np.uint8)
    entries = {key: rng.integers(0, 2**31, n).astype(COLUMNS[key]) for key in ENTRY_COLUMNS}
    entries["kind"] = kinds
    entries["flags"] = (rng.random(n) < 0.1).astype(np.uint32) * np.uint32(Flag.HARDLINK_DUP)
    names = [f"entry-{i}".encode() for i in range(n)]
    part: SlicePart = (
        np.array([[0, 1, 0]]),
        np.array([0, n]),
        b"".join(names),
        np.array([len(x) for x in names], dtype=np.uint16),
        entries,
        None,
    )
    assert ingest(a, [part], True) == ingest(b, [part], False)
    assert_same(a, b)
    assert len(a._heap._bases) > 1


def test_readers_see_whole_listings_while_the_fold_runs_without_the_gil() -> None:
    store = NodeStore()
    root = store.add_root("/r")
    stop = threading.Event()
    problems: list[str] = []

    def reader() -> None:
        while not stop.is_set():
            n = len(store)
            for folder in range(1, n):
                a, b = store.child_range(folder)
                if a == b:
                    continue
                if not (0 < a <= b <= len(store)):
                    problems.append(f"range {a}..{b} of {folder} past the store")
                    return
                if (store.cols["parent"][a:b] != folder).any():
                    problems.append(f"rows {a}..{b} are not {folder}'s")
                    return
                store.name_bytes(b - 1)

    t = threading.Thread(target=reader)
    t.start()
    try:
        pending = [root]
        rng = np.random.default_rng(3)
        with fold_with(True):
            while pending and len(store) < 200_000:
                batch, pending = pending[:64], pending[64:]
                parts: list[SlicePart] = []
                for d in batch:
                    m = int(rng.integers(1, 40))
                    kinds = [Kind.DIR] * (m // 4) + [Kind.FILE] * (m - m // 4)
                    names = [f"{d}-{i}".encode() for i in range(m)]
                    parts.append(
                        (
                            np.array([[0, d, 0]]),
                            np.array([0, m]),
                            b"".join(names),
                            np.array([len(x) for x in names]),
                            _entries(kinds),
                            None,
                        )
                    )
                for st_, p in zip(store.ingest_slices(parts), parts, strict=True):
                    dirs = int(np.count_nonzero(p[4]["kind"] == Kind.DIR))
                    pending.extend(range(int(st_[0]), int(st_[0]) + dirs))
    finally:
        stop.set()
        t.join()
    assert not problems, problems[:3]
    n = len(store)
    before = store.cols["tot_files"][:n].copy()
    store.reaggregate()
    assert np.array_equal(store.cols["tot_files"][:n], before)


def test_a_column_replaced_while_the_fold_writes_stays_alive_until_it_is_done() -> None:
    """NodeStore.compact replaces a column from another thread: the array the fold writes into
    (without the GIL) must outlive the writes, whichever of the two comes first."""
    store = NodeStore()
    root = store.add_root("/r")
    k = 2_000_000
    store.reserve(k + 16)
    entries = {key: np.zeros(k, dtype=COLUMNS[key]) for key in ENTRY_COLUMNS}
    entries["size"][:] = 7
    go = threading.Event()
    seen: list[bool] = []

    class Plan:
        def __init__(self, *args: Any) -> None:
            self.plan = NATIVE(*args)
            self.rows, self.name_bytes = self.plan.rows, self.plan.name_bytes

        def write(self, *args: Any) -> None:
            go.set()
            self.plan.write(*args)

        def publish(self, store: NodeStore) -> Any:
            return self.plan.publish(store)

    def replace() -> None:
        go.wait()
        old = store.cols["size"]
        store.cols["size"] = old.copy()
        alive = weakref.ref(old)
        del old
        seen.append(alive() is not None)

    t = threading.Thread(target=replace)
    t.start()
    saved = nodes.NATIVE_FOLD
    nodes.NATIVE_FOLD = Plan
    try:
        store.ingest_slice(
            np.array([[0, root, 0]]), np.array([0, k]), b"x" * k, np.ones(k, np.uint16), entries
        )
    finally:
        nodes.NATIVE_FOLD = saved
        t.join()
    landed = int((store.cols["size"][2 : len(store)] == 7).sum())
    # replaced before the fold took the column: every row is in the live one; replaced while it
    # wrote: the old one was still alive (the fold holds it), its rows are in the old array
    assert landed == k or seen == [True]
