"""Case-only siblings (docs/native.md 2.2) on the Python side: the worker's checks of folders whose
names differ only in case, the monitor's relists, path lookups and the guard of deletions.

Windows opens names case-insensitively, so where a folder stores names case-sensitively (WinBtrfs,
NTFS names in the POSIX namespace) the name of ``a`` can open its sibling ``A``. These tests stand
in for that with a fake lister and a fake stored-name lookup, so they run on every system; the
Windows tests in packages/native/tests/test_hardscan.py check the real thing.
"""

from __future__ import annotations

import asyncio
import dataclasses
import os
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import numpy as np
import pytest

from spaceknife.monitor import Change
from spaceknife.ops import trash
from spaceknife.ops.basket import Basket
from spaceknife.scan import worker
from spaceknife.scan.worker import RootOptions, SliceOptions, SliceResult, Task
from spaceknife.server.compares import CompareManager
from spaceknife.server.monitors import ScanMonitor
from spaceknife.store.lookup import PathIndex
from spaceknife.store.nodes import ENTRY_COLUMNS, Kind, NodeStore
from spaceknife_protocol import compare as pc

# --------------------------------------------------------------------------- the worker

R = "I:\\x"  # a WinBtrfs-like root; the fake lister below needs no disk

# folder -> its entries (name, is a folder): `a` and `A` differ only in case, file `B` wins the
# name of folder `b`, two entries show as the very same name (U+FFFD), one name has a backslash
TREE: dict[str, list[tuple[str, bool]]] = {
    R: [
        ("a", True),
        ("A", True),
        ("b", True),
        ("B", False),
        ("n\ufffd", True),
        ("n\ufffd", True),
        ("s\\l", True),
        ("plain", True),
    ],
    R + "\\a": [("loser", False)],  # what an open by the name `a` would never list on WinBtrfs
    R + "\\A": [("inner", True)],
    R + "\\A\\inner": [],
    R + "\\b": [],
    R + "\\n\ufffd": [],
    R + "\\s\\l": [],
    R + "\\plain": [],
}
# what the file system stores for the folder each path opens (WinBtrfs: the first whose name
# matches in any case, so `a` opens `A`)
STORED = {
    R: "\\X",  # the root typed in another case: never compared
    R + "\\a": "\\x\\A",
    R + "\\A": "\\x\\A",
    R + "\\A\\inner": "\\x\\A\\inner",
    R + "\\b": "\\x\\b",
    R + "\\n\ufffd": "\\x\\n\ufffd",
    R + "\\plain": "\\x\\plain",
}


@pytest.fixture
def fake_volume(monkeypatch: pytest.MonkeyPatch) -> None:
    """worker._list_dir and worker.final_name over TREE and STORED, the marks as _list_windows sets them."""

    def lister(path: str, root: RootOptions, acc: Any) -> list[tuple[int, str, str]]:
        entries = TREE[path]
        base = len(acc.kind)
        subdirs: list[tuple[int, str, str]] = []
        for name, is_dir in entries:
            acc.names.append(name.encode("utf-8", "surrogatepass"))
            acc.kind.append(worker.KIND_DIR if is_dir else worker.KIND_FILE)
            for col in (
                acc.flags,
                acc.size,
                acc.alloc,
                acc.mtime,
                acc.ctime,
                acc.atime,
                acc.ino,
                acc.dev,
                acc.ext,
            ):
                col.append(0)
            acc.nlink.append(1)
            if is_dir:
                subdirs.append((len(acc.kind) - 1 - base, path + "\\" + name, ""))
        if root.verify_names and subdirs and len(entries) >= 2:
            marks = worker.case_marks([n for n, _ in entries], [j for j, _, _ in subdirs])
            subdirs = [(j, p, marks.get(j, "")) for j, p, _ in subdirs]
        return subdirs

    monkeypatch.setattr(worker, "_list_dir", lister)
    monkeypatch.setattr(worker, "final_name", STORED.get)


def run(tasks: list[Task], budget_s: float = 1e9, *, verify: bool = True) -> SliceResult:
    opts = SliceOptions(
        roots=(RootOptions(path=R, verify_names=verify),), budget_s=budget_s, max_entries=10**9
    )
    return worker.scan_slice(tasks, opts)


def test_case_marks_group_names_that_upper_case_alike() -> None:
    names = ["a", "A", "b", "B", "x\ufffd", "x\ufffd", "s\\l", "s/m", "ß", "SS", "only"]
    dirs = [0, 1, 2, 4, 5, 6, 7, 8, 10]
    assert worker.case_marks(names, dirs) == {
        0: "strict",
        1: "strict",
        2: "strict",  # file B could win its name
        4: "strict",
        5: "dup",  # after the first entry of the very same name
        6: "bad",
        7: "bad",
        8: "strict",  # str.upper() groups more than Windows does: the stored name decides
    }
    assert worker.case_marks(["a", "A"], []) == {}
    assert worker.case_marks(["A", "a"], [1]) == {1: "strict"}


def test_names_below_the_root_and_stored_tails() -> None:
    assert worker.names_below(R + "\\a\\b", R) == ["a", "b"]
    assert worker.names_below("I:\\x\\\\a\\", "I:\\x\\") == ["a"]
    assert worker.names_below(R, R) is None  # the root itself
    assert worker.names_below("I:\\xy\\a", R) is None
    assert worker.names_below(R + "\\a", "") is None
    assert worker.tail_is("\\x\\A", ["a"]) is False
    assert worker.tail_is("\\mnt\\x\\a", ["x", "a"]) is True
    assert worker.tail_is("\\a", ["x", "a"]) is None


@pytest.mark.usefixtures("fake_volume")
def test_a_loser_is_a_listing_error_when_its_turn_comes() -> None:
    res = run([((0, 1, 0), R, 0)])
    assert res.paths == [R, R + "\\A", R + "\\A\\inner", R + "\\b", R + "\\n\ufffd", R + "\\plain"]
    assert sorted((path, msg) for _ref, path, msg in res.errors) == sorted(
        [
            (R + "\\a", worker.CASE_ERROR),  # its name opens A
            (R + "\\n\ufffd", worker.DUP_ERROR),  # the second of the same name
            (R + "\\s\\l", worker.BADNAME_ERROR),
        ]
    )
    assert not res.frontier
    # without the root option nothing is checked: the loser is listed under its name
    res = run([((0, 1, 0), R, 0)], verify=False)
    assert R + "\\a" in res.paths and not res.errors


@pytest.mark.usefixtures("fake_volume")
def test_a_relist_alone_keeps_its_children_out_of_the_errors() -> None:
    """At budget 0 (the monitor relists one folder) a twin goes to the frontier: as a task it is
    checked when it comes back. Names no path can hold are errors, never tasks."""
    res = run([((0, 1, 0), R, 0)], budget_s=0.0)
    assert res.paths == [R]
    assert {path for _ref, path, _idx in res.frontier} == {
        R + "\\a",
        R + "\\A",
        R + "\\b",
        R + "\\n\ufffd",
        R + "\\plain",
    }
    assert sorted(msg for _ref, _path, msg in res.errors) == sorted([worker.DUP_ERROR, worker.BADNAME_ERROR])
    assert all(ref[0] == 1 for ref, _path, _msg in res.errors)
    # the frontier as the next slice's tasks: the loser fails its check, the rest list
    again = run(res.frontier)
    assert [path for _ref, path, _msg in again.errors] == [R + "\\a"]
    assert R + "\\A\\inner" in again.paths


@pytest.mark.usefixtures("fake_volume")
def test_tasks_whose_stored_names_differ_are_errors() -> None:
    res = run([((0, 7, 0), R + "\\a", 0), ((0, 8, 0), R + "\\A\\inner", 0)])
    assert res.errors == [((0, 7, 0), R + "\\a", worker.CASE_ERROR)]
    assert res.paths == [R + "\\A\\inner"]


# --------------------------------------------------------------------------- the monitor's relists


def test_a_relist_stands_with_errors_of_its_subfolders_only(tmp_path: Path) -> None:
    (tmp_path / "top" / "sub").mkdir(parents=True)
    top = str(tmp_path / "top")
    listed = worker.scan_slice([((0, 5, 0), top, 0)], SliceOptions(roots=(RootOptions(),), budget_s=0.0))
    assert len(listed.refs) == 1
    mon: Any = ScanMonitor.__new__(ScanMonitor)
    mon.handle = SimpleNamespace(roots=[SimpleNamespace(path=str(tmp_path), volume=None)], scanner=None)
    answer: list[SliceResult] = []

    async def fake_slice(tasks: list[Task], opts: SliceOptions) -> SliceResult:
        assert tasks == [((0, 5, 0), top, 0)] and opts.budget_s == 0.0
        assert opts.roots == (RootOptions(path=str(tmp_path)),)  # no scanner: the root's own options
        return answer[0]

    mon._slice = fake_slice

    def relist(errors: list[tuple[tuple[int, int, int], str, str]], **kw: Any) -> SliceResult | None:
        answer[:] = [dataclasses.replace(listed, errors=errors, **kw)]
        got: SliceResult | None = asyncio.run(mon._list_one(5, top))
        return got

    assert relist([]) is not None
    assert relist([((1, 0, 0), os.path.join(top, "sub"), worker.DUP_ERROR)]) is not None
    assert relist([((0, 5, 0), top, worker.CASE_ERROR)]) is None  # the folder itself: stays as scanned
    assert relist([], refs=np.zeros((0, 3), dtype=np.int64), paths=[]) is None  # gone


# --------------------------------------------------------------------------- lookups by path

ROOT = "C:\\r" if os.name == "nt" else "/r"


def entries(kinds: list[int], sizes: list[int] | None = None) -> dict[str, np.ndarray]:
    out = {k: np.zeros(len(kinds), dtype=np.int64) for k in ENTRY_COLUMNS}
    out["kind"][:] = kinds
    if sizes is not None:
        out["size"][:] = sizes
        out["alloc"][:] = sizes
    return out


def twin_store() -> tuple[NodeStore, dict[str, int]]:
    """ROOT holds folders `a` and `A`, `b` alone, two entries named `dup`, and `ab` beside `AB`;
    `a` and `A` each hold a file `f` (sizes 1 and 2)."""
    store = NodeStore()
    root = store.add_root(ROOT)
    names = ["a", "A", "b", "dup", "dup", "ab", "AB"]
    first = store.ingest_names(root, [n.encode() for n in names], entries([Kind.DIR] * len(names)))
    ids = {n: first + i for i, n in enumerate(names) if n != "dup"}
    ids["dup"] = first + 3
    for name, size in (("a", 1), ("A", 2)):
        ids[name + "/f"] = store.ingest_names(ids[name], [b"f"], entries([Kind.FILE], [size]))
    return store, ids


def path(*names: str) -> str:
    return os.path.join(ROOT, *names)


@pytest.fixture
def folding(monkeypatch: pytest.MonkeyPatch) -> None:
    """normcase as Windows has it, on every system (it is the identity on POSIX)."""
    monkeypatch.setattr(os.path, "normcase", lambda s: s.replace("/", os.sep).lower())


@pytest.mark.usefixtures("folding")
def test_lookups_take_the_exact_case_then_a_unique_fold() -> None:
    store, ids = twin_store()
    root = next(iter(store.roots()))
    index = PathIndex(store)
    want: dict[str, int | None] = {
        path("a", "f"): ids["a/f"],
        path("A", "f"): ids["A/f"],
        path("A"): ids["A"],
        path("B"): ids["b"],  # only one entry has that name in any case
        path("dup"): ids["dup"],  # exact duplicates: the first
        path("DUP"): None,  # several in another case: none of them is meant
        path("Ab"): None,
        path("ab"): ids["ab"],
        path("c"): None,
        os.path.join(ROOT.upper(), "A"): ids["A"],  # the root typed in another case is the root
    }
    for p, node in want.items():
        assert index.locate(p)[0] == node, p
        assert store.find(p) == node, p
    assert index.locate(path("DUP")) == (None, root)  # the deepest one there is


@pytest.mark.usefixtures("folding")
def test_file_changes_land_in_the_folder_they_name() -> None:
    store, ids = twin_store()
    mon: Any = ScanMonitor.__new__(ScanMonitor)
    mon.handle = SimpleNamespace(store=store, method="quick", roots=[])
    mon.index = PathIndex(store)
    folders: set[str] = set()
    change = Change("modified", path("A", "f"), size=77, alloc=4096, mtime=5)
    asyncio.run(mon._apply_stats({change.path: change}, folders))
    assert not folders
    assert int(store.cols["size"][ids["A/f"]]) == 77
    assert int(store.cols["size"][ids["a/f"]]) == 1


@pytest.mark.usefixtures("folding")
def test_a_compare_view_finds_the_twin_it_shows() -> None:
    live, ids = twin_store()
    old, old_ids = twin_store()

    class Tree:
        store = old
        status = np.zeros(len(old), dtype=np.uint8)

        def __len__(self) -> int:
            return len(old)

    scans: Any = SimpleNamespace(scans={"s1": SimpleNamespace(id="s1", store=live, source="scan")})
    mgr = CompareManager(scans, SimpleNamespace())  # type: ignore[arg-type]
    compares: Any = mgr.compares
    compares["c1"] = SimpleNamespace(tree=Tree(), target_spec=pc.NOW, scan_id="s1")
    assert mgr.live_node("c1", old_ids["A/f"]) == ("s1", ids["A/f"])
    assert mgr.live_node("c1", old_ids["a"]) == ("s1", ids["a"])


# --------------------------------------------------------------------------- deleting


@pytest.fixture
def twins(tmp_path: Path) -> tuple[Path, Path, Path]:
    """A scan root with folders `a` and `A` side by side (a case-sensitive folder on Windows)."""
    root = tmp_path / "root"
    root.mkdir()
    if sys.platform == "win32":
        done = subprocess.run(
            ["fsutil", "file", "setCaseSensitiveInfo", str(root), "enable"], capture_output=True, check=False
        )
        if done.returncode != 0:
            pytest.skip("cannot make a case-sensitive folder here (needs the WSL feature)")
    (root / "a").mkdir()
    try:
        (root / "A").mkdir()
    except FileExistsError:
        pytest.skip("this file system folds case (APFS by default)")
    (root / "a" / "f").write_bytes(b"a")
    (root / "A" / "g").write_bytes(b"A")
    return root, root / "a", root / "A"


def stored_as(mapping: dict[str, str]) -> Callable[[str], str | None]:
    return lambda p: mapping.get(p)


def test_a_path_that_opens_another_entry_is_never_deleted(
    twins: tuple[Path, Path, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    root, a, big_a = twins
    # as on WinBtrfs: the path of `a` opens `A`
    monkeypatch.setattr(trash, "_final_name", stored_as({str(a): "\\somewhere\\root\\A"}))
    roots = {str(a): str(root)}
    (res,) = trash.delete_permanently([str(a)], roots=roots, check_locks=False)
    assert (res.code, res.message, res.ok) == ("ambiguous_name", trash.TEXT["ambiguous_name"], False)
    (res,) = trash.move_to_trash([str(a)], roots=roots, check_locks=False)
    assert res.code == "ambiguous_name"
    assert (a / "f").exists() and (big_a / "g").exists()


def test_other_stored_names_do_not_stop_a_deletion(
    twins: tuple[Path, Path, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    root, a, big_a = twins
    # a hard link's other name, a short name: not a case-only difference
    monkeypatch.setattr(trash, "_final_name", stored_as({str(a): "\\elsewhere\\other"}))
    (res,) = trash.delete_permanently([str(a)], roots={str(a): str(root)}, check_locks=False)
    assert res.ok and not a.exists() and (big_a / "g").exists()


def test_case_twins_on_disk_each_delete_their_own(twins: tuple[Path, Path, Path]) -> None:
    root, a, big_a = twins
    roots = {str(a): str(root), str(big_a): str(root)}
    (res,) = trash.delete_permanently([str(a)], roots=roots, check_locks=False)
    assert res.ok and not a.exists() and (big_a / "g").exists()
    (res,) = trash.delete_permanently([str(big_a)], roots=roots, check_locks=False)
    assert res.ok and not big_a.exists()


def test_names_with_a_separator_cannot_go_into_the_basket() -> None:
    store = NodeStore()
    root = store.add_root(ROOT)
    names = ["ok", "x/y", "x\\y"]
    first = store.ingest_names(root, [n.encode() for n in names], entries([Kind.DIR] * 3))
    inner = store.ingest_names(first + 1, [b"f"], entries([Kind.FILE], [1]))
    b = Basket(volume_of=lambda root: "VOL")
    res = b.add(store, "s1", [first, first + 1, first + 2, inner])
    refused = {r.path for r in res.refused}
    bad = {store.path(first + 1), store.path(inner)}  # the name, and anything below it
    if os.name == "nt":
        bad.add(store.path(first + 2))  # WinBtrfs shows Linux names with a backslash as they are
    assert refused == bad
    assert len(res.added) == 4 - len(bad)
