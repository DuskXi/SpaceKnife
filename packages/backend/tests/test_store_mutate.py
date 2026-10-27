"""store.mutate: a tree changed in place by relists and stat updates equals a fresh scan of the disk."""

from __future__ import annotations

import asyncio
import os
import shutil
from pathlib import Path

import numpy as np
import pytest

from spaceknife.scan import worker
from spaceknife.scan.scanner import Scanner, ScanState
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


# ------------------------------------------------------------------ folders deleted while being read


def gone_tree(tmp_path: Path) -> Path:
    root = tmp_path / "r"
    (root / "gone" / "a" / "b").mkdir(parents=True)
    (root / "gone" / "a" / "b" / "f.bin").write_bytes(b"x" * 100)
    (root / "gone" / "a" / "g.txt").write_bytes(b"y" * 10)
    (root / "keep").mkdir()
    (root / "keep" / "k.txt").write_bytes(b"z" * 10)
    return root


def test_listings_of_folders_deleted_meanwhile_are_skipped(tmp_path: Path) -> None:
    """A worker reads a/ while a/ still waits in the queue; then gone/ is deleted and the monitor
    relists the root, which marks gone/'s subtree GONE. The stale listing of a/ (and of b/ inside
    it) is skipped instead of failing with "not a directory"."""
    root = gone_tree(tmp_path)
    store = NodeStore()
    node = store.add_root(str(root))

    def one_level(task: worker.Task) -> list[worker.Task]:
        res = worker.scan_slice([task], opts(root, budget=0.0))
        starts = store.ingest_slice(
            res.refs, res.bounds, res.name_blob, res.name_lens, cols(res), ext_ids(store, res)
        ).tolist()
        return [((0, a if k == 0 else starts[a] + b, 0), p, i) for (k, a, b), p, i in res.frontier]

    queued = one_level(((0, node, 0), str(root), 0))
    one_level(next(t for t in queued if t[1].endswith("keep")))
    (a_task,) = one_level(next(t for t in queued if t[1].endswith("gone")))
    stale = worker.scan_slice([a_task], opts(root))  # a/ and b/ inside it, in one slice
    assert len(stale.refs) == 2
    shutil.rmtree(root / "gone")
    assert mutate.can_relist(store, node)  # the monitor may relist the root: no child is pending
    relist_path(store, root, node)
    size = len(store)
    starts = store.ingest_slice(
        stale.refs, stale.bounds, stale.name_blob, stale.name_lens, cols(stale), ext_ids(store, stale)
    )
    assert starts.tolist() == [-1, -1] and len(store) == size
    assert facts(store) == facts(scan(root))


def test_scanner_finishes_when_queued_folders_are_deleted(tmp_path: Path) -> None:
    """The same race through Scanner._apply: the stale result's pending folders and errors are
    dropped, nothing of the deleted subtree is queued, and the scan completes."""
    root = gone_tree(tmp_path)
    scanner = Scanner([str(root)], budget_s=0.0)
    store = scanner.store
    root_opts = SliceOptions(roots=scanner._root_opts, budget_s=0.0)
    scanner._apply(worker.scan_slice([scanner._queue.popleft()], root_opts))
    for name in ("keep", "gone"):
        task = next(t for t in scanner._queue if t[1].endswith(name))
        scanner._queue.remove(task)
        scanner._apply(worker.scan_slice([task], root_opts))
    a_task = next(t for t in scanner._queue if t[1].endswith(os.path.join("gone", "a")))
    scanner._queue.remove(a_task)
    stale = worker.scan_slice([a_task], SliceOptions(roots=scanner._root_opts, budget_s=0.0))
    shutil.rmtree(root / "gone")
    assert mutate.can_relist(store, scanner.roots[0].node)
    relist_path(store, root, scanner.roots[0].node)
    scanner._apply(stale)  # b/ was its pending subfolder: not queued, no error
    assert all("gone" not in path for _, path, _ in scanner._queue)
    result = asyncio.run(scanner.run())
    assert result.state == ScanState.DONE
    fresh = Scanner([str(root)])  # same options (cluster size, resident files) as the scanner above
    asyncio.run(fresh.run())
    assert facts(store) == facts(fresh.store)
