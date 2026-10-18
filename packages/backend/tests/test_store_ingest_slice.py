"""NodeStore.ingest_slice folds a whole SliceResult exactly like ingest_listing per listing."""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any

import numpy as np
import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from spaceknife.scan import worker
from spaceknife.scan.worker import RootOptions, SliceOptions, SliceResult
from spaceknife.store.nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore

from .tree_fixture import build_tree


def fold_listings(store: NodeStore, res: SliceResult, ext: np.ndarray) -> list[int]:
    """The reference: what Scanner._apply did before ingest_slice, one listing at a time."""
    name_off = np.concatenate(([0], np.cumsum(res.name_lens, dtype=np.int64)))
    starts: list[int] = []
    for listing in range(len(res.refs)):
        kind, a, b = (int(x) for x in res.refs[listing])
        dir_id = a if kind == 0 else starts[a] + b
        lo, hi = int(res.bounds[listing]), int(res.bounds[listing + 1])
        start = store.ingest_listing(
            dir_id,
            res.name_blob[int(name_off[lo]) : int(name_off[hi])],
            res.name_lens[lo:hi],
            {key: res.cols[key][lo:hi] for key in ENTRY_COLUMNS},
            ext[lo:hi],
        )
        starts.append(start)
        for j in np.flatnonzero((res.cols["flags"][lo:hi] & np.uint32(Flag.MOUNT)) != 0).tolist():
            store.clear(start + j, Flag.PENDING)
    return starts


def fold_slice(store: NodeStore, res: SliceResult, ext: np.ndarray) -> list[int]:
    cols = {key: res.cols[key] for key in ENTRY_COLUMNS}
    out: list[int] = store.ingest_slice(
        res.refs, res.bounds, res.name_blob, res.name_lens, cols, ext
    ).tolist()
    return out


def assert_same_store(a: NodeStore, b: NodeStore) -> None:
    assert len(a) == len(b)
    n = len(a)
    for key in a.cols:
        if key == "name_off":
            continue  # names are compared by content below (heap chunking may differ)
        assert np.array_equal(a.cols[key][:n], b.cols[key][:n]), key
    assert [a.name_bytes(i) for i in range(n)] == [b.name_bytes(i) for i in range(n)]


def two_stores(roots: list[str]) -> tuple[NodeStore, NodeStore]:
    stores = (NodeStore(), NodeStore())
    for s in stores:
        for r in roots:
            s.add_root(r)
    return stores


# ------------------------------------------------------------------ slices from a real tree


@pytest.mark.parametrize("max_entries", [1, 3, 25, 100_000])
def test_real_worker_slices(tmp_path: Path, max_entries: int) -> None:
    facts = build_tree(tmp_path / "root")
    root = str(facts.root)
    vec, ref = two_stores([root])
    opts = SliceOptions(roots=(RootOptions(dev=os.stat(root).st_dev),), budget_s=0.0, max_entries=max_entries)
    queue: list[worker.Task] = [((0, 1, 0), root, 0)]
    slices = 0
    while queue:
        batch, queue = queue[:4], queue[4:]
        res = worker.scan_slice(batch, opts)
        remap = np.array([vec.ext_id(e) for e in res.ext_table], dtype=np.uint16)
        for e in res.ext_table:
            ref.ext_id(e)
        ext = remap[res.ext_ids] if len(res.ext_ids) else res.ext_ids
        starts = fold_slice(vec, res, ext)
        assert starts == fold_listings(ref, res, ext)
        assert_same_store(vec, ref)
        for r, path, idx in res.frontier:
            kind, a, b = r
            queue.append(((0, a if kind == 0 else starts[a] + b, 0), path, idx))
        slices += 1
    assert slices > 1 or max_entries == 100_000
    assert int(vec.cols["tot_files"][1]) == facts.files


# ------------------------------------------------------------------ random slices


@st.composite
def plans(draw: Any) -> tuple[int, int]:
    """A seed for the slices' contents and how many slices (1–4) to fold one after another."""
    return draw(st.integers(0, 2**32 - 1)), draw(st.integers(1, 4))


def random_slice(rng: np.random.Generator, pending: list[int]) -> tuple[SliceResult, list[tuple[int, int]]]:
    """Listings over ``pending`` store nodes and over directories found earlier in the slice."""
    refs: list[tuple[int, int, int]] = []
    found: list[tuple[int, int]] = []  # (listing, entry) of unlisted, enterable directories
    bounds = [0]
    cols: dict[str, list[int]] = {key: [] for key in ENTRY_COLUMNS}
    names: list[bytes] = []
    for k in range(int(rng.integers(0, 14))):
        if found and (not pending or rng.random() < 0.7):
            j, i = found.pop(int(rng.integers(len(found))))
            refs.append((1, j, i))
        elif pending:
            refs.append((0, pending.pop(int(rng.integers(len(pending)))), 0))
        else:
            break
        for i in range(int(rng.integers(0, 7))):
            kind = int(rng.choice([Kind.FILE, Kind.FILE, Kind.DIR, Kind.LINK, Kind.OTHER]))
            flags = int(rng.choice([0, Flag.HIDDEN, Flag.HARDLINK_DUP, Flag.HIDDEN | Flag.HARDLINK_DUP]))
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
            names.append(f"n{k}.{i}{'é' * int(rng.integers(0, 3))}".encode())
            if kind == Kind.DIR and not flags & Flag.MOUNT:
                found.append((k, i))
        bounds.append(len(names))
    dtypes = {"kind": np.uint8, "flags": np.uint32, "nlink": np.uint16, "ino": np.uint64}
    dtypes |= {t: np.uint32 for t in ("mtime", "ctime", "atime")}
    res = SliceResult(
        refs=np.array(refs, dtype=np.int64).reshape(-1, 3),
        paths=[f"p{k}" for k in range(len(refs))],
        bounds=np.array(bounds, dtype=np.int64),
        name_blob=b"".join(names),
        name_lens=np.array([len(n) for n in names], dtype=np.uint16),
        cols={key: np.array(v, dtype=dtypes.get(key, np.int64)) for key, v in cols.items()},
        ext_table=[""],
        ext_ids=rng.integers(0, 3, len(names)).astype(np.uint16),
        frontier=[],
        errors=[],
    )
    return res, found


@settings(max_examples=150, deadline=None)
@given(plans())
def test_random_slices(plan: tuple[int, int]) -> None:
    seed, rounds = plan
    rng = np.random.default_rng(seed)
    vec, ref = two_stores(["C:\\a", "D:\\b"])
    for s in (vec, ref):
        s.ext_id("x")
        s.ext_id("y")
    pending = [1, 2]
    for _ in range(rounds):
        res, left = random_slice(rng, pending)
        starts = fold_slice(vec, res, res.ext_ids)
        assert starts == fold_listings(ref, res, res.ext_ids)
        assert_same_store(vec, ref)
        pending += [starts[j] + i for j, i in left]  # frontier: listed in a later slice
    # the incremental totals agree with a full recount as well
    n = len(vec)
    before = {k: vec.cols[k][:n].copy() for k in ("tot_size", "tot_alloc", "tot_files", "tot_dirs")}
    vec.reaggregate()
    for k, col in before.items():
        assert np.array_equal(vec.cols[k][:n], col), k


def test_bad_slices_change_nothing() -> None:
    store = NodeStore()
    root = store.add_root("C:\\r")
    entries = {key: np.zeros(1, dtype=np.int64) for key in ENTRY_COLUMNS}
    entries["kind"] = np.array([Kind.FILE], dtype=np.uint8)
    one = (np.array([0, 1]), b"f", np.array([1]), entries)
    bad = [
        (np.array([[1, 0, 0]]), "earlier listing"),  # names an entry of itself
        (np.array([[0, 99, 0]]), "not in the store"),
        (np.array([[0, root, 0], [1, 0, 0]]), "not a directory"),  # entry 0 of listing 0 is a file
        (np.array([[0, root, 0], [0, root, 0]]), "already listed"),  # listed twice
    ]
    for refs, why in bad:
        b = np.arange(len(refs) + 1).clip(max=1) if len(refs) > 1 else one[0]
        with pytest.raises(ValueError, match=why):
            store.ingest_slice(refs, b, one[1], one[2], entries)
        assert len(store) == 2 and store.cols["child_count"][root] == 0
    store.ingest_slice(np.array([[0, root, 0]]), *one)
    with pytest.raises(ValueError, match="already listed"):
        store.ingest_slice(np.array([[0, root, 0]]), *one)
