"""store.mutate: a tree changed in place by relists and stat updates equals a fresh scan of the disk."""

from __future__ import annotations

import os
import shutil
from pathlib import Path

import numpy as np
import pytest

from spaceknife.scan import worker
from spaceknife.scan.worker import RootOptions, SliceOptions
from spaceknife.store import mutate
from spaceknife.store.nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore

from .tree_fixture import build_tree


def scan(root: Path) -> NodeStore:
    """A plain hard scan of ``root`` with the worker and ingest_slice (no event loop needed)."""
    store = NodeStore()
    node = store.add_root(str(root))
    queue: list[worker.Task] = [((0, node, 0), str(root), 0)]
    while queue:
        batch, queue = queue[:8], queue[8:]
        res = worker.scan_slice(batch, opts(root))
        starts = store.ingest_slice(
            res.refs, res.bounds, res.name_blob, res.name_lens, cols(res), ext_ids(store, res)
        ).tolist()
        for (kind, a, b), path, idx in res.frontier:
            queue.append(((0, a if kind == 0 else starts[a] + b, 0), path, idx))
    return store


def opts(root: Path, budget: float = 0.05) -> SliceOptions:
    return SliceOptions(roots=(RootOptions(dev=os.stat(root).st_dev),), budget_s=budget)


def cols(res: worker.SliceResult) -> dict[str, np.ndarray]:
    return {key: res.cols[key] for key in ENTRY_COLUMNS}


def ext_ids(store: NodeStore, res: worker.SliceResult) -> np.ndarray:
    remap = np.array([store.ext_id(e) for e in res.ext_table], dtype=np.uint16)
    return remap[res.ext_ids] if len(res.ext_ids) else res.ext_ids


def relist_path(store: NodeStore, root: Path, folder: int) -> mutate.Relist:
    """List one folder again (one level, like the monitor) and fold it in; scan what is new."""
    res = worker.scan_slice([((0, folder, 0), store.path(folder), 0)], opts(root, budget=0.0))
    assert len(res.refs) == 1
    rel = mutate.relist(store, folder, res.name_blob, res.name_lens, cols(res), ext_ids(store, res))
    for node in rel.pending.tolist():  # new folders: scanned like the scanner would
        queue: list[worker.Task] = [((0, node, 0), store.path(node), 0)]
        while queue:
            batch, queue = queue[:8], queue[8:]
            sub = worker.scan_slice(batch, opts(root))
            starts = store.ingest_slice(
                sub.refs, sub.bounds, sub.name_blob, sub.name_lens, cols(sub), ext_ids(store, sub)
            ).tolist()
            for (kind, a, b), path, idx in sub.frontier:
                queue.append(((0, a if kind == 0 else starts[a] + b, 0), path, idx))
    return rel


def facts(store: NodeStore) -> dict[str, tuple[int, ...]]:
    """path -> (kind, size, alloc, totals …) of every node reachable from the roots."""
    c = store.cols
    out: dict[str, tuple[int, ...]] = {}
    stack = list(store.roots())
    while stack:
        node = stack.pop()
        assert not c["flags"][node] & np.uint32(Flag.GONE)
        out[store.path(node)] = tuple(
            int(c[k][node])
            for k in ("kind", "size", "alloc", "tot_size", "tot_alloc", "tot_files", "tot_dirs", "depth")
        )
        stack.extend(store.children(node))
    return out


def check_structure(store: NodeStore) -> None:
    """Every live node is exactly one folder's child; GONE rows are in no range; totals add up."""
    n = len(store)
    c = store.cols
    gone = mutate.is_gone(c["flags"][:n])
    seen = np.zeros(n, dtype=np.int64)
    for d in np.flatnonzero((c["kind"][:n] == Kind.DIR) & ~gone).tolist():
        kids = np.arange(*store.child_range(d))
        assert np.all(c["parent"][kids] == d) and np.all(c["depth"][kids] == c["depth"][d] + 1)
        seen[kids] += 1
    assert np.all(seen[1:][~gone[1:]] == 1) and np.all(seen[gone] == 0)
    totals = {k: c[k][:n].copy() for k in mutate.TOTALS}
    store.reaggregate()
    for k, v in totals.items():
        assert np.array_equal(store.cols[k][:n], v), k


def node_of(store: NodeStore, path: Path) -> int:
    found = store.find(str(path))
    assert found is not None, path
    return found


@pytest.fixture
def tree(tmp_path: Path) -> tuple[Path, NodeStore]:
    facts_ = build_tree(tmp_path / "root")
    return facts_.root, scan(facts_.root)


def test_relist_follows_the_disk(tree: tuple[Path, NodeStore]) -> None:
    root, store = tree
    folders = sorted({p.parent for p in root.rglob("*") if p.is_file()} | {root}, key=lambda p: len(p.parts))
    target = folders[-1]  # a deep folder
    up = target.parent
    # create, grow, delete, rename, a new folder with content, a file turned into a folder
    (target / "new.bin").write_bytes(b"n" * 5000)
    first = next(p for p in target.iterdir() if p.is_file() and p.name != "new.bin")
    first.write_bytes(b"g" * 70_000)
    victim = next((p for p in up.iterdir() if p.is_file()), None)
    if victim is not None:
        victim.unlink()
    (up / "fresh").mkdir()
    (up / "fresh" / "deep").mkdir()
    (up / "fresh" / "deep" / "x.txt").write_bytes(b"x" * 1234)
    other = next(p for p in root.iterdir() if p.is_dir() and p not in target.parents)
    other.rename(root / "renamed-folder")
    for folder in (target, up, root):
        relist_path(store, root, node_of(store, folder))
    check_structure(store)
    assert facts(store) == facts(scan(root))
    assert mutate.gone_count(store) > 0


def test_removed_subtree_and_kept_subtree(tree: tuple[Path, NodeStore]) -> None:
    root, store = tree
    a, b = sorted(p for p in root.iterdir() if p.is_dir())[:2]
    kept_below = {store.path(n) for n in store.children(node_of(store, b))}
    shutil.rmtree(a)
    rel = relist_path(store, root, 1)
    assert node_of(store, b) in rel.moved_to.tolist()
    assert {store.path(n) for n in store.children(node_of(store, b))} == kept_below  # moved with it
    assert len(rel.removed) == 1 and rel.removed_alloc[0] > 0
    check_structure(store)
    assert facts(store) == facts(scan(root))


def test_update_stats_climbs(tree: tuple[Path, NodeStore]) -> None:
    root, store = tree
    f = next(p for p in root.rglob("*") if p.is_file())
    node = node_of(store, f)
    before_root = int(store.cols["tot_alloc"][1])
    old = int(store.cols["alloc"][node])
    delta = mutate.update_stats(store, np.array([node]), np.array([10**6]), np.array([old + 8192]))
    assert delta.tolist() == [8192] and int(store.cols["tot_alloc"][1]) == before_root + 8192
    check_structure(store)


def test_compact_renumbers_the_live_tree(tree: tuple[Path, NodeStore]) -> None:
    root, store = tree
    for folder in [*[p for p in root.rglob("*") if p.is_dir()][:5], root]:
        (folder / "added.dat").write_bytes(b"a" * 777)
        relist_path(store, root, node_of(store, folder))
    before = facts(store)
    compacted, remap = mutate.compact(store)
    assert mutate.gone_count(compacted) == 0 and len(compacted) == len(store) - mutate.gone_count(store)
    assert facts(compacted) == before
    live = np.flatnonzero(remap >= 0)
    assert all(compacted.name_bytes(int(remap[i])) == store.name_bytes(int(i)) for i in live.tolist())
    c = compacted.cols
    for d in np.flatnonzero(c["kind"][: len(compacted)] == Kind.DIR).tolist():
        lo, hi = compacted.child_range(d)
        assert lo > d or hi == lo  # every range after its folder again
    check_structure(compacted)


def test_relist_waits_for_pending_children(tmp_path: Path) -> None:
    (tmp_path / "r" / "sub").mkdir(parents=True)
    store = NodeStore()
    root = store.add_root(str(tmp_path / "r"))
    res = worker.scan_slice([((0, root, 0), str(tmp_path / "r"), 0)], opts(tmp_path, budget=0.0))
    store.ingest_slice(res.refs, res.bounds, res.name_blob, res.name_lens, cols(res), ext_ids(store, res))
    assert not mutate.can_relist(store, root)  # "sub" is still queued by its id
    sub = next(iter(store.children(root)))
    assert mutate.can_relist(store, sub) is False  # not listed yet itself


def test_a_vanished_root_lists_empty(tree: tuple[Path, NodeStore]) -> None:
    _, store = tree
    empty = {key: np.zeros(0, dtype=store.cols[key].dtype) for key in ENTRY_COLUMNS}
    rel = mutate.relist(store, 1, b"", np.zeros(0, dtype=np.uint16), empty)
    assert len(rel.removed) > 0 and list(store.children(1)) == []
    assert all(int(store.cols[k][1]) == 0 for k in mutate.TOTALS)
    assert all(int(store.cols[k][0]) == 0 for k in mutate.TOTALS[:3])
    check_structure(store)
