"""Snapshot compare (spaceknife.diff, docs/manage.md §3): hand-built trees, a slow reference
implementation on random trees, the union store's invariants and the change ranking."""

from __future__ import annotations

import ntpath
import os
import sys
from collections import deque
from dataclasses import dataclass, field
from typing import Any

import numpy as np
import pytest
from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st

from spaceknife.diff import CompareCancelled, DiffTree, Status, compare, engine
from spaceknife.layout import Layout, LayoutParams, RootSpec
from spaceknife.store import mutate
from spaceknife.store.nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore

# --------------------------------------------------------------------------- building trees


@dataclass(frozen=True)
class F:
    """A non-folder entry."""

    size: int = 0
    alloc: int | None = None  # default: the size
    dup: bool = False  # an additional hard link (counts 0)
    kind: Kind = Kind.FILE


@dataclass(frozen=True)
class D:
    """A folder with its own allocation (POSIX directory blocks)."""

    children: dict[str, Any] = field(default_factory=dict)
    alloc: int = 0


Spec = dict[str, Any]  # name -> int (file size) | F | dict (folder) | D


def _entry(value: Any) -> tuple[Kind, int, int, int, dict[str, Any] | None]:
    """(kind, size, alloc, flags, children) of a spec value."""
    if isinstance(value, dict):
        return Kind.DIR, 0, 0, 0, value
    if isinstance(value, D):
        return Kind.DIR, 0, value.alloc, 0, value.children
    f = value if isinstance(value, F) else F(int(value))
    alloc = f.size if f.alloc is None else f.alloc
    return f.kind, f.size, alloc, int(Flag.HARDLINK_DUP) if f.dup else 0, None


def build(
    spec: Spec | None = None, root: str = "/r", *, roots: dict[str, Spec] | None = None, order: str = "bfs"
) -> NodeStore:
    """A store from specs; ``order``: listings ingested breadth first or depth first (child ranges
    then lie in another order)."""
    store = NodeStore()
    todo: deque[tuple[int, dict[str, Any]]] = deque()
    for path, sub in (roots if roots is not None else {root: spec or {}}).items():
        todo.append((store.add_root(path), sub))
    while todo:
        node, children = todo.popleft() if order == "bfs" else todo.pop()
        names = list(children)
        entries = {k: np.zeros(len(names), dtype=np.int64) for k in ENTRY_COLUMNS}
        subs: list[tuple[int, dict[str, Any]]] = []
        for i, name in enumerate(names):
            kind, size, alloc, flags, kids = _entry(children[name])
            entries["kind"][i], entries["size"][i], entries["alloc"][i], entries["flags"][i] = (
                kind,
                size,
                alloc,
                flags,
            )
            entries["nlink"][i] = 2 if flags else 1
            if kids is not None:
                subs.append((i, kids))
        first = store.ingest_names(node, [n.encode("utf-8", "surrogatepass") for n in names], entries)
        todo.extend((first + i, kids) for i, kids in subs)
    return store


def relist_same(store: NodeStore, folder: int) -> None:
    """List ``folder`` again with the same entries (a file change hook would): its children move to
    new rows, the old ones become GONE."""
    c = store.cols
    kids = list(store.children(folder))
    names = [store.name_bytes(k) for k in kids]
    entries = {k: np.array([int(c[k][i]) for i in kids], dtype=np.int64) for k in ENTRY_COLUMNS}
    lens = np.array([len(n) for n in names], dtype=np.int64)
    mutate.relist(store, folder, b"".join(names), lens, entries)


def find(store: NodeStore, path: str, kind: Kind | None = None) -> int:
    """Row of ``root/a/b`` (a ``/`` path below the first root); ``kind`` picks among equal names."""
    node = next(iter(store.roots()))
    parts = [p for p in path.split("/") if p]
    for i, part in enumerate(parts):
        hits = [ch for ch in store.children(node) if store.name(ch) == part]
        if kind is not None and i == len(parts) - 1:
            hits = [h for h in hits if store.cols["kind"][h] == kind]
        assert hits, path
        node = hits[0]
    return node


# --------------------------------------------------------------------------- reference


def _live(store: NodeStore, node: int) -> list[int]:
    if node < 0 or store.cols["kind"][node] != Kind.DIR:
        return []
    return [ch for ch in store.children(node) if not store.cols["flags"][ch] & Flag.GONE]


def _eff(store: NodeStore, row: int, key: str) -> int:
    c = store.cols
    if row < 0:
        return 0
    if c["kind"][row] == Kind.DIR:
        return int(c["tot_" + key][row])
    return 0 if c["flags"][row] & Flag.HARDLINK_DUP else int(c[key][row])


def _own(store: NodeStore, row: int) -> int:
    if row < 0 or store.cols["flags"][row] & Flag.HARDLINK_DUP:
        return 0
    return int(store.cols["alloc"][row])


def _is_dir(store: NodeStore, row: int) -> bool:
    return bool(store.cols["kind"][row] == Kind.DIR)


def _pairs(
    base: NodeStore, target: NodeStore, b: int, t: int, fold: bool, top: bool
) -> list[tuple[int, int]]:
    """Children of (b, t) as (base row, target row): exact names first, then folded ones; equal keys
    pair up first come first served. Roots: normalised paths."""
    kb, kt = _live(base, b), _live(target, t)

    def key(store: NodeStore, row: int, folded: bool) -> tuple[str, bool]:
        name = store.name(row)
        if top:
            name = os.path.normpath(name)
        return (ntpath.normcase(name) if folded else name), _is_dir(store, row)

    matched: dict[int, int] = {}
    taken: set[int] = set()
    for folded in [fold] if top else ([False, True] if fold else [False]):
        waiting: dict[tuple[str, bool], deque[int]] = {}
        for cb in kb:
            if cb not in taken:
                waiting.setdefault(key(base, cb, folded), deque()).append(cb)
        for ct in kt:
            if ct in matched:
                continue
            free = waiting.get(key(target, ct, folded))
            if free:
                cb = free.popleft()
                matched[ct] = cb
                taken.add(cb)
    return [(matched.get(ct, -1), ct) for ct in kt] + [(cb, -1) for cb in kb if cb not in taken]


Row = tuple[int, int, int, int, int, int, int]  # status, base/target alloc, base/target size, grow, shrink


def reference(base: NodeStore, target: NodeStore, fold: bool) -> dict[tuple[int, int], Row]:
    """The compare, one Python object per node: (base row, target row) -> expected values."""
    out: dict[tuple[int, int], Row] = {}

    def visit(b: int, t: int) -> tuple[int, int, int]:
        grow = shrink = 0
        changed = False
        for cb, ct in _pairs(base, target, b, t, fold, top=(b, t) == (0, 0)):
            g, s, status = visit(cb, ct)
            od = _own(target, ct) - _own(base, cb)
            if _is_dir(target, ct) if ct >= 0 else _is_dir(base, cb):
                g, s = g + max(od, 0), s + max(-od, 0)
            grow, shrink, changed = grow + g, shrink + s, changed or status != Status.UNCHANGED
        row = (target, t) if t >= 0 else (base, b)
        if not _is_dir(*row):
            od = _own(target, t) - _own(base, b)
            grow, shrink = max(od, 0), max(-od, 0)
        ends = (
            _eff(base, b, "alloc"),
            _eff(target, t, "alloc"),
            _eff(base, b, "size"),
            _eff(target, t, "size"),
        )
        if b < 0:
            status = Status.ADDED
        elif t < 0:
            status = Status.REMOVED
        elif (
            changed
            or ends[0] != ends[1]
            or ends[2] != ends[3]
            or any(int(base.cols[k][b]) != int(target.cols[k][t]) for k in ("size", "alloc", "kind"))
            or (base.cols["flags"][b] ^ target.cols["flags"][t]) & Flag.HARDLINK_DUP
        ):
            status = Status.CHANGED
        else:
            status = Status.UNCHANGED
        out[(b, t)] = (int(status), *ends, grow, shrink)
        return grow, shrink, status

    visit(0, 0)
    return out


def got(tree: DiffTree) -> dict[tuple[int, int], Row]:
    rows = zip(
        tree.base_id.tolist(),
        tree.target_id.tolist(),
        tree.status.tolist(),
        tree.base_alloc.tolist(),
        tree.target_alloc.tolist(),
        tree.base_size.tolist(),
        tree.target_size.tolist(),
        tree.grow.tolist(),
        tree.shrink.tolist(),
        strict=True,
    )
    return {(b, t): (s, ba, ta, bs, ts, g, sh) for b, t, s, ba, ta, bs, ts, g, sh in rows}


def _reachable(store: NodeStore) -> set[int]:
    seen: set[int] = set()
    todo = [0]
    while todo:
        node = todo.pop()
        seen.add(node)
        todo.extend(_live(store, node))
    return seen


def check_union(tree: DiffTree, base: NodeStore, target: NodeStore) -> None:
    """The union store keeps every NodeStore invariant; the arrays agree with each other and the
    union covers every live row of both trees exactly once."""
    s = tree.store
    n = len(s)
    c = s.export_columns()
    assert len(tree.status) == len(tree.grow) == len(tree.base_id) == n
    assert c["parent"][0] == -1 and c["depth"][0] == 0 and c["flags"][0] == Flag.VIRTUAL
    seen = np.zeros(n, dtype=np.int64)
    for d in np.flatnonzero(c["kind"] == Kind.DIR).tolist():
        lo, cnt = int(c["child_start"][d]), int(c["child_count"][d])
        kids = np.arange(lo, lo + cnt)
        assert np.all(c["parent"][kids] == d)
        assert np.all(kids > d)
        assert np.all(c["depth"][kids] == c["depth"][d] + 1)
        seen[kids] += 1
    assert seen[0] == 0 and np.all(seen[1:] == 1)
    assert np.all(c["child_count"][c["kind"] != Kind.DIR] == 0)
    assert not np.any(c["flags"] & np.uint32(Flag.GONE))
    # names: one \0name run per row in id order, equal to the source rows' names
    off, ln = c["name_off"].astype(np.int64), c["name_len"].astype(np.int64)
    assert np.all(off[1:] == off[:-1] + ln[:-1] + 1)
    for u in range(n):
        src, row = (
            (target, int(tree.target_id[u])) if tree.target_id[u] >= 0 else (base, int(tree.base_id[u]))
        )
        assert s.name_bytes(u) == src.name_bytes(row)
    totals = {k: c[k].copy() for k in mutate.TOTALS}
    s.reaggregate()
    for k, v in totals.items():
        assert np.array_equal(s.cols[k][:n], v), k
    # coverage of both trees
    for ids, store in ((tree.base_id, base), (tree.target_id, target)):
        live = ids[ids >= 0]
        assert len(np.unique(live)) == len(live)
        assert set(live.tolist()) == _reachable(store)
    assert np.array_equal(tree.status == Status.ADDED, tree.base_id < 0)
    assert np.array_equal(tree.status == Status.REMOVED, tree.target_id < 0)
    assert np.array_equal(tree.delta, tree.target_alloc - tree.base_alloc)
    assert np.array_equal(tree.grow - tree.shrink, tree.delta)
    assert np.all(tree.grow >= 0) and np.all(tree.shrink >= 0)
    area = tree.area()
    for d in np.flatnonzero(c["kind"] == Kind.DIR).tolist():  # a folder's area covers its children's
        kids = np.arange(int(c["child_start"][d]), int(c["child_start"][d] + c["child_count"][d]))
        assert area[d] >= area[kids].sum()


def check(base: NodeStore, target: NodeStore, fold: bool) -> DiffTree:
    tree = compare(base, target, fold_case=fold)
    check_union(tree, base, target)
    assert got(tree) == reference(base, target, fold)
    return tree


def row(tree: DiffTree, path: str, kind: Kind | None = None) -> int:
    return find(tree.store, path, kind)


# --------------------------------------------------------------------------- hand-built trees


def test_added_removed_changed_unchanged() -> None:
    base = build({"a": {"x.txt": 10, "y.bin": 100, "old.log": 30}, "keep.dat": 5, "same": {"z": 1}})
    target = build(
        {"a": {"x.txt": 10, "y.bin": 300, "new.log": 7}, "keep.dat": 5, "same": {"z": 1}, "fresh": {"q": 2}}
    )
    tree = check(base, target, fold=False)
    st_of = {
        p: Status(int(tree.status[row(tree, p)]))
        for p in ("a", "a/x.txt", "a/y.bin", "a/old.log", "a/new.log")
    }
    assert st_of == {
        "a": Status.CHANGED,
        "a/x.txt": Status.UNCHANGED,
        "a/y.bin": Status.CHANGED,
        "a/old.log": Status.REMOVED,
        "a/new.log": Status.ADDED,
    }
    assert tree.status[row(tree, "same")] == Status.UNCHANGED
    assert (
        tree.status[row(tree, "fresh")] == Status.ADDED and tree.status[row(tree, "fresh/q")] == Status.ADDED
    )
    a = row(tree, "a")
    assert (tree.base_alloc[a], tree.target_alloc[a], tree.delta[a]) == (140, 317, 177)
    assert (tree.grow[a], tree.shrink[a]) == (207, 30)
    top = next(iter(tree.store.roots()))
    assert tree.delta[top] == 2 + 177 and tree.grow[top] == 209 and tree.shrink[top] == 30
    # size columns: the target's, the base's for removed rows
    assert tree.store.cols["alloc"][row(tree, "a/y.bin")] == 300
    assert tree.store.cols["alloc"][row(tree, "a/old.log")] == 30
    # a matched folder lists the target's children in its order, then the removed ones
    names = [tree.store.name(ch) for ch in tree.store.children(a)]
    assert names == ["x.txt", "y.bin", "new.log", "old.log"]


def test_identical_trees_are_unchanged() -> None:
    spec = {"a": {"b": {"c.txt": 3}, "d": 4}, "e": {}}
    tree = check(build(spec), build(spec), fold=False)
    assert np.all(tree.status == Status.UNCHANGED)
    assert np.all(tree.delta == 0) and np.all(tree.area() == 0)
    store = build(spec)
    same = compare(store, store, fold_case=False)
    assert np.array_equal(same.base_id, same.target_id)


def test_rename_is_removed_and_added() -> None:
    tree = check(build({"photos": {"a.jpg": 50}}), build({"pictures": {"a.jpg": 50}}), fold=False)
    assert tree.status[row(tree, "photos")] == Status.REMOVED
    assert tree.status[row(tree, "photos/a.jpg")] == Status.REMOVED
    assert tree.status[row(tree, "pictures")] == Status.ADDED
    top = next(iter(tree.store.roots()))
    assert tree.delta[top] == 0 and tree.grow[top] == 50 and tree.shrink[top] == 50


def test_windows_rule_folds_case() -> None:
    base = build({"Docs": {"Report.TXT": 10, "Ärger.txt": 4, "naïve": 2}, "MiXeD": 1}, root="C:\\Data")
    target = build({"docs": {"report.txt": 12, "ärger.TXT": 4, "NAÏVE": 2}, "mixed": 1}, root="c:\\data")
    tree = check(base, target, fold=True)
    assert len(tree) == len(target)  # everything matched, roots included
    assert tree.status[row(tree, "docs/report.txt")] == Status.CHANGED
    assert tree.status[row(tree, "docs/ärger.TXT")] == Status.UNCHANGED
    assert tree.store.name(next(iter(tree.store.roots()))) == "c:\\data"  # the target's spelling
    exact = check(base, target, fold=False)
    assert len(exact) == len(base) + len(target) - 1  # only the virtual top is shared


def test_windows_rule_prefers_the_exact_spelling() -> None:
    # a case-sensitive folder (WSL, NTFS per-directory flag) with "A" and "a" on both sides
    base = build({"A": 1, "a": 2, "Bb": 3, "bB": 4})
    target = build({"a": 2, "A": 1, "bb": 30})
    tree = check(base, target, fold=True)
    pairs = {(tree.store.name(u), int(tree.status[u])) for u in range(2, len(tree))}
    assert ("a", Status.UNCHANGED) in pairs and ("A", Status.UNCHANGED) in pairs
    # "bb" matches the first of the two spellings that fold to it; the other is removed
    assert ("bb", Status.CHANGED) in pairs and ("bB", Status.REMOVED) in pairs


def test_posix_rule_is_exact() -> None:
    tree = check(build({"Readme": 1}), build({"README": 1}), fold=False)
    assert sorted(int(s) for s in tree.status[2:]) == [Status.ADDED, Status.REMOVED]


def test_default_rule_follows_the_platform() -> None:
    assert compare(build({}), build({})).fold_case == (sys.platform == "win32")


def test_kind_change_is_removed_and_added() -> None:
    base = build({"x": 30, "y": {"in": 5}, "link": F(10, kind=Kind.LINK)})
    target = build({"x": {"inner": 7}, "y": 9, "link": F(10)})
    tree = check(base, target, fold=False)
    assert tree.status[row(tree, "x", Kind.FILE)] == Status.REMOVED
    assert tree.status[row(tree, "x", Kind.DIR)] == Status.ADDED
    assert tree.status[row(tree, "x/inner")] == Status.ADDED
    assert tree.status[row(tree, "y", Kind.DIR)] == Status.REMOVED
    assert tree.status[row(tree, "y", Kind.FILE)] == Status.ADDED
    # a link that became a file is the same item: never descended either way
    assert tree.status[row(tree, "link")] == Status.CHANGED and tree.delta[row(tree, "link")] == 0


def test_hard_link_duplicates_count_zero() -> None:
    base = build({"a": F(100), "b": F(100, dup=True), "c": F(40, dup=True)})
    target = build({"a": F(100, dup=True), "b": F(100), "c": F(70, dup=True)})
    tree = check(base, target, fold=False)
    a, b, c = row(tree, "a"), row(tree, "b"), row(tree, "c")
    assert (tree.base_alloc[a], tree.target_alloc[a]) == (100, 0)  # the counted link moved
    assert (tree.base_alloc[b], tree.target_alloc[b]) == (0, 100)
    assert tree.delta[c] == 0 and tree.status[c] == Status.CHANGED  # its size changed, it counts 0
    top = next(iter(tree.store.roots()))
    assert tree.delta[top] == 0


def test_folders_own_blocks() -> None:
    base = build({"d": D({"f": 10}, alloc=4096)})
    target = build({"d": D({"f": 10}, alloc=8192)})
    tree = check(base, target, fold=False)
    d = row(tree, "d")
    assert tree.status[d] == Status.CHANGED and tree.delta[d] == 0  # its subtree did not change
    top = next(iter(tree.store.roots()))
    assert tree.delta[top] == 4096 and tree.grow[top] == 4096


def test_empty_trees() -> None:
    empty = NodeStore()
    tree = compare(empty, NodeStore(), fold_case=False)
    assert len(tree) == 1 and tree.status[0] == Status.UNCHANGED and tree.changes("grow", 10) == []
    full = build({"a": {"b": 3}})
    added = check(empty, full, fold=False)
    assert np.all(added.status[1:] == Status.ADDED) and added.delta[0] == 3
    removed = check(full, empty, fold=False)
    assert np.all(removed.status[1:] == Status.REMOVED) and removed.shrink[0] == 3
    check(build({}), build({}), fold=True)  # a root without entries


def test_deep_trees() -> None:
    def chain(depth: int, leaf: int) -> Spec:
        spec: Spec = {"leaf.bin": leaf}
        for i in range(depth):
            spec = {f"level{i}": spec}
        return spec

    base, target = build(chain(400, 10)), build(chain(400, 25))
    tree = compare(base, target, fold_case=False)
    check_union(tree, base, target)
    assert int(tree.store.cols["depth"][: len(tree)].max()) == 402
    assert np.all(tree.status[1:] == Status.CHANGED)
    [unit] = tree.changes("grow", 5)
    assert unit.path.endswith("leaf.bin") and unit.amount == 15 and unit.delta == 15


def test_multiple_roots() -> None:
    base = build(roots={"/a": {"x": 1}, "/b": {"y": 2}})
    target = build(roots={"/b/": {"y": 5}, "/c": {"z": 3}})  # "/b/" normalises to "/b"
    tree = check(base, target, fold=False)
    roots = {tree.store.name(r): Status(int(tree.status[r])) for r in tree.store.roots()}
    assert roots == {"/b/": Status.CHANGED, "/c": Status.ADDED, "/a": Status.REMOVED}


def test_other_layouts_and_gone_rows() -> None:
    spec = {"a": {"b": {"c": 1, "d": 2}, "e": 3}, "f": {"g": {"h": 4}}, "i": 5}
    base = build(spec)
    target = build(spec, order="dfs")  # child ranges in another order
    for folder in ("a", "a/b", "f/g", ""):  # moved rows leave GONE copies behind
        relist_same(target, find(target, folder) if folder else next(iter(target.roots())))
    assert mutate.gone_count(target) > 0
    tree = check(base, target, fold=False)
    assert np.all(tree.status == Status.UNCHANGED)
    gone = mutate.is_gone(target.cols["flags"][: len(target)])
    assert not np.any(gone[tree.target_id[tree.target_id >= 0]])
    # GONE rows in a growing tree: change a file after the relist
    mutate.update_stats(target, np.array([find(target, "a/e")]), np.array([30]), np.array([30]))
    tree = check(base, target, fold=False)
    assert tree.delta[row(tree, "a/e")] == 27


def test_names_from_several_heap_chunks() -> None:
    long = "x" * 120
    spec = {f"dir{i:03d}": {f"{long}{j:04d}.dat": i + j for j in range(30)} for i in range(60)}
    other = {k: dict(list(v.items())[::-1]) for k, v in spec.items()}  # every folder reversed
    other["dir007"][f"{long}9999.dat"] = 5
    base, target = build(spec), build(other, order="dfs")
    assert len(base.names_parts()) > 1 and len(target.names_parts()) > 1
    tree = check(base, target, fold=False)
    assert int(np.count_nonzero(tree.status == Status.ADDED)) == 1


def test_hash_collisions_never_pair_different_names(monkeypatch: pytest.MonkeyPatch) -> None:
    real = engine._hash

    def constant(img: Any, pos: np.ndarray, lens: np.ndarray, *, fold: bool) -> tuple[np.ndarray, np.ndarray]:
        h, wide = real(img, pos, lens, fold=fold)
        return np.zeros_like(h), wide  # every leftover name collides

    monkeypatch.setattr(engine, "_hash", constant)
    names = ["b", "a", "Ä", "c", "d", "e"]
    base = build({n: i for i, n in enumerate(names)} | {"sub": {n: 1 for n in names}})
    target = build({n: i for i, n in enumerate(reversed([*names, "new", "ä"]))} | {"sub": {"q": 1, "b": 1}})
    for fold in (False, True):
        check(base, target, fold=fold)


def test_pair_sorted_groups() -> None:
    kb = np.array([5, 7, 9, 9, 11], dtype=np.uint64) << np.uint64(20)
    kt = np.array([9, 7, 13, 11, 11], dtype=np.uint64) << np.uint64(20)
    pb, pt, cb, ct = engine._pair_sorted(kb, kt)
    assert sorted(zip(pb.tolist(), pt.tolist(), strict=True)) == [(1, 1)]
    assert sorted(cb.tolist()) == [2, 3, 4] and sorted(ct.tolist()) == [0, 3, 4]


def test_word_helpers() -> None:
    img, pos, lens = engine._Image.of([b"Hello-World.TXT", b"x", b"ab\xc3\x84"])
    folded, wide = engine._hash(img, pos, lens, fold=True)
    img2, pos2, lens2 = engine._Image.of([b"hello-world.txt", b"X", b"AB\xc3\x84"])
    folded2, _ = engine._hash(img2, pos2, lens2, fold=True)
    assert np.array_equal(folded, folded2) and wide.tolist() == [False, False, True]
    exact, _ = engine._hash(img, pos, lens, fold=False)
    exact2, _ = engine._hash(img2, pos2, lens2, fold=False)
    assert not np.any(exact == exact2)
    assert engine._equal(img, pos, img2, pos2, lens, fold=True).all()
    assert not engine._equal(img, pos, img2, pos2, lens, fold=False).any()
    # every byte value folds like bytes.lower() for ASCII and stays put otherwise
    words = np.arange(256, dtype=np.uint64)
    want = np.frombuffer(bytes(range(256)).lower(), dtype=np.uint8).astype(np.uint64)
    for shift in range(0, 64, 8):
        assert np.array_equal(engine._fold(words << np.uint64(shift)), want << np.uint64(shift))


# --------------------------------------------------------------------------- progress, cancel, layout


def test_progress_and_cancel() -> None:
    base, target = build({"a": {"b": 1}, "c": 2}), build({"a": {"b": 3}, "d": 2})
    seen: list[float] = []
    compare(base, target, progress=seen.append, fold_case=False)
    assert seen and seen == sorted(seen) and seen[0] >= 0 and seen[-1] == 1.0
    calls = 0

    def cancel() -> bool:
        nonlocal calls
        calls += 1
        return calls > 2

    with pytest.raises(CompareCancelled, match="对比已取消"):
        compare(base, target, cancel=cancel, fold_case=False)


def test_the_layout_engine_draws_the_union() -> None:
    base = build({"a": {f"f{i}": 100 * i for i in range(20)}, "b": {"g": 5000}})
    target = build({"a": {f"f{i}": 100 * i + (50 if i % 3 else 0) for i in range(25)}, "c": {"h": 800}})
    tree = check(base, target, fold=False)
    root = next(iter(tree.store.roots()))
    params = LayoutParams(width=600, height=400, min_element=1)
    normal = Layout(tree.store).run(params, RootSpec(node=root))
    changed = Layout(tree.store).run(params, RootSpec(node=root), sizes=tree.area())
    assert len(normal) and len(changed)
    drawn = set(changed.node.tolist())
    assert row(tree, "b/g") in drawn and row(tree, "c/h") in drawn
    assert row(tree, "a/f0") not in drawn  # unchanged: no area


# --------------------------------------------------------------------------- change units


def rel(path: str) -> str:
    return path.replace("\\", "/").removeprefix("/r/")


def test_changes_ranking() -> None:
    base = build(
        {
            "one": {"keep": 1000, "tiny": 1},
            "many": {"old": 5},
            "two": {"p": 10, "q": 10, "r": 1},
            "gone": {"g": 400},
        }
    )
    target = build(
        {
            "one": {"keep": 1000, "tiny": 2, "big.iso": 1000},
            "many": {"old": 5} | {f"n{i:03d}": 9 for i in range(100)},
            "two": {"p": 310, "q": 260, "r": 2},
        }
    )
    tree = check(base, target, fold=False)
    grow = tree.changes("grow", 10)
    assert [(rel(c.path), c.amount, c.rest) for c in grow] == [
        ("one/big.iso", 1000, False),  # one big new file: the file, not its folder
        ("many", 900, False),  # a hundred small ones: the folder
        ("two/p", 300, False),  # a few big changes: each of them
        ("two/q", 250, False),
        ("one", 1, True),  # the rest of "one" ("tiny")
        ("two", 1, True),  # the rest of "two" ("r")
    ]
    assert grow[1].members == 0 and grow[4].members == 1
    assert sum(c.amount for c in grow) == tree.grow[0]  # an exact partition
    unit = grow[0]
    assert (unit.status, unit.kind, unit.base_alloc, unit.target_alloc, unit.delta) == (
        Status.ADDED,
        Kind.FILE,
        0,
        1000,
        1000,
    )
    [gone] = tree.changes("shrink", 10)  # all of "gone" is one file: that file
    assert rel(gone.path) == "gone/g" and gone.amount == 400 and gone.status == Status.REMOVED
    assert len(tree.changes("grow", 2)) == 2 and tree.changes("grow", 0) == []
    # never break folders down: the roots are the units
    [whole] = tree.changes("grow", 10, dominant=2.0)
    assert whole.path == "/r" and whole.amount == tree.grow[0]
    with pytest.raises(ValueError, match="grow"):
        tree.changes("bigger", 3)  # type: ignore[arg-type]


def test_changes_in_a_scope_and_with_other_amounts() -> None:
    base = build({"one": {"keep": 1000}, "two": {"p": 10, "q": 10}, "three": {"x": 50}})
    target = build({"one": {"keep": 1000, "big.iso": 1000}, "two": {"p": 310, "q": 260}, "three": {}})
    tree = check(base, target, fold=False)
    two = row(tree, "two")
    inside = tree.changes("grow", 10, scope=two)
    assert [(rel(c.path), c.amount) for c in inside] == [("two/p", 300), ("two/q", 250)]
    assert tree.changes("grow", 10, scope=row(tree, "two/p"))[0].amount == 300  # a file: itself
    assert tree.changes("grow", 10, scope=row(tree, "three")) == []
    # other amounts: here, only what "two" contributes
    amount = np.zeros(len(tree), dtype=np.int64)
    for node in [two, *tree.store.children(two), *tree.store.ancestors(two)]:
        amount[node] = tree.grow[node] if node in (two, *tree.store.children(two)) else tree.grow[two]
    only_two = tree.changes("grow", 10, amount=amount)
    assert [(rel(c.path), c.amount) for c in only_two] == [("two/p", 300), ("two/q", 250)]
    assert only_two[0].delta == 300 and only_two[0].target_alloc == 310


def test_changes_partition_random_trees() -> None:
    import bench_diff

    base_tree = bench_diff.synth(20_000, seed=3)
    target_tree = bench_diff.mutate(
        base_tree, seed=4, resize=0.1, delete=0.02, add=0.05, recase=0.01, reorder=0.3
    )
    base = bench_diff.to_store(base_tree, scatter=True, seed=5)
    target = bench_diff.to_store(target_tree, scatter=True, seed=6)
    for fold in (False, True):
        tree = check(base, target, fold=fold)
        for direction, total in (("grow", tree.grow[0]), ("shrink", tree.shrink[0])):
            units = tree.changes(direction, 1_000_000)  # type: ignore[arg-type]
            assert sum(c.amount for c in units) == total
            amounts = [c.amount for c in units]
            assert amounts == sorted(amounts, reverse=True) and min(amounts) > 0
            whole = [c.node for c in units if not c.rest]  # whole units never nest
            assert len(set(whole)) == len(whole)
            inside = set(whole)
            for node in whole:
                assert not any(a in inside for a in tree.store.ancestors(node))


# --------------------------------------------------------------------------- random trees


NAMES = [
    "a",
    "A",
    "b",
    "ab",
    "Ab",
    "aB",
    "x.txt",
    "X.TXT",
    "ä",
    "Ä",
    "中文",
    "long-name-0123456789",
    "LONG-name-0123456789",
    "é",
    "\u212a",
]
leaf = st.one_of(
    st.integers(0, 2000),
    st.builds(
        F,
        st.integers(0, 2000),
        st.none() | st.integers(0, 4096),
        st.booleans(),
        st.sampled_from([Kind.FILE, Kind.LINK]),
    ),
)
trees = st.recursive(
    st.dictionaries(st.sampled_from(NAMES), leaf, max_size=4),
    lambda inner: st.dictionaries(
        st.sampled_from(NAMES), st.one_of(leaf, inner, st.builds(D, inner, st.integers(0, 64))), max_size=6
    ),
    max_leaves=40,
)


def _mutated(data: st.DataObject, spec: Spec) -> Spec:
    """``spec`` a while later: entries kept, dropped, resized, re-cased or turned into the other
    kind, a few added, listed in another order."""
    out: Spec = {}
    items = list(spec.items())
    if data.draw(st.booleans()):
        items = data.draw(st.permutations(items))
    for name, value in items:
        op = data.draw(st.sampled_from(["keep", "keep", "keep", "drop", "resize", "recase", "flip"]))
        if op == "drop":
            continue
        if op == "recase" and name.swapcase() not in spec and name.swapcase() not in out:
            name = name.swapcase()
        is_dir = isinstance(value, dict | D)
        if op == "flip":
            value = 7 if is_dir else {}
        elif op == "resize" and not is_dir:
            value = data.draw(leaf)
        elif is_dir:
            kids = value if isinstance(value, dict) else value.children
            value = _mutated(data, kids)
        if name not in out:
            out[name] = value
    for name, value in data.draw(st.dictionaries(st.sampled_from(NAMES), leaf, max_size=2)).items():
        out.setdefault(name, value)
    return out


@settings(max_examples=250, deadline=None, suppress_health_check=[HealthCheck.too_slow])
@given(st.data(), trees, st.booleans(), st.sampled_from(["bfs", "dfs"]), st.booleans())
def test_random_trees_match_the_reference(
    data: st.DataObject, spec: Spec, fold: bool, order: str, relisted: bool
) -> None:
    other = _mutated(data, spec) if data.draw(st.booleans()) else data.draw(trees)
    base = build(spec)
    target = build(other, order=order)
    if relisted:  # GONE rows and moved child ranges
        folders = [r for r in range(1, len(target)) if target.cols["kind"][r] == Kind.DIR]
        for folder in data.draw(st.lists(st.sampled_from(folders), max_size=3, unique=True)):
            if mutate.can_relist(target, folder) and not target.cols["flags"][folder] & Flag.GONE:
                relist_same(target, folder)
    check(base, target, fold)
