"""Case-only siblings (docs/native.md 2.2) on the Python side: the worker's checks of folders whose
names differ only in case, the monitor's relists, path lookups and the guard of deletions.

Windows opens names case-insensitively, so where a folder stores names case-sensitively (WinBtrfs,
NTFS names in the POSIX namespace) the name of ``a`` can open its sibling ``A``. These tests stand
in for that with a fake lister and a fake stored-name lookup, so they run on every system; the
Windows tests in packages/native/tests/test_hardscan.py check the real thing.
"""

from __future__ import annotations

import asyncio
import contextlib
import dataclasses
import os
import re
import shutil
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import numpy as np
import pytest

from spaceknife.events import denied_reason
from spaceknife.monitor import Change
from spaceknife.ops import basket, trash
from spaceknife.ops.basket import Basket
from spaceknife.scan import worker
from spaceknife.scan.worker import RootOptions, SliceOptions, SliceResult, Task
from spaceknife.server.compares import CompareManager
from spaceknife.server.monitors import ScanMonitor
from spaceknife.store import mutate
from spaceknife.store.lookup import PathIndex
from spaceknife.store.nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore
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
    R: "\\x",  # the root's own stored name: what lies below it must follow it
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


def test_both_walkers_word_these_errors_alike() -> None:
    """The native walker's listing errors (hardscan/case.rs ``msg``) are the worker's, word for
    word: the log panel finds its wording by the text (events.denied_reason)."""
    src = Path(__file__).parents[2] / "native" / "src" / "hardscan" / "case.rs"
    body = src.read_text(encoding="utf-8").split("pub mod msg {", 1)[1].split("\n}", 1)[0]
    texts = dict(re.findall(r'pub const (\w+): &str =\s*"([^"]*)";', body))
    wording = {
        "CASE": (worker.CASE_ERROR, "同一文件夹里另有一项与它只差大小写，按名字打开的是那一项"),
        "DUP": (worker.DUP_ERROR, "同一文件夹里另有一项在 Windows 上显示为同一个名字，按名字分不开"),
        "BADNAME": (worker.BADNAME_ERROR, "名字里有 \\ 或 /，按名字打不开"),
        "CAP": (worker.CAP_ERROR, "只能按文件 ID 打开的文件夹一次要读的项太多，这次没有读"),
        "INCOMPLETE": (worker.INCOMPLETE_ERROR, "按文件 ID 打开时没有列全"),
    }
    assert texts == {name: text for name, (text, _words) in wording.items()}
    for text, words in wording.values():
        assert denied_reason(text) == words


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


def test_names_below_the_root_and_stored_names() -> None:
    assert worker.names_below(R + "\\a\\b", R) == ["a", "b"]
    assert worker.names_below("I:\\x\\\\a\\", "I:\\x\\") == ["a"]
    assert worker.names_below(R, R) is None  # the root itself
    assert worker.names_below("I:\\xy\\a", R) is None
    assert worker.names_below(R + "\\a", "") is None
    # after the root's own stored name, whatever the root was typed as (hardscan/case.rs task_is)
    assert not worker.task_is("\\x\\A", "\\x", ["a"])
    assert worker.task_is("\\x\\a", "\\x", ["a"])
    assert worker.task_is("\\a", "\\", ["a"])  # a root that is a mount folder of the volume's root
    assert not worker.task_is("\\elsewhere\\x\\a", "\\x", ["a"])  # through a symlink that won the name
    # without the root's: the last names, never fewer
    assert worker.task_is("\\mnt\\x\\a", None, ["x", "a"])
    assert not worker.task_is("\\x\\A", None, ["a"])
    assert not worker.task_is("\\a", None, ["x", "a"])


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
def test_a_relist_alone_leaves_twins_to_the_frontier_and_unnamable_children_as_errors() -> None:
    """At budget 0 (the monitor relists one folder) a twin goes to the frontier: as a task it is
    checked when it comes back. Names no path can hold (a second entry of the very same name, a
    backslash) are errors of those subfolders, never tasks."""
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


def test_tasks_a_symlink_led_elsewhere_are_errors(fake_volume: None, monkeypatch: pytest.MonkeyPatch) -> None:
    """A twin that is a symlink wins the name and leads to a folder with the same last names, or
    to one with fewer names than the path has below the root: not the folder the path names."""
    stored = {**STORED, R + "\\b": "\\y\\x\\b", R + "\\A\\inner": "\\inner"}
    monkeypatch.setattr(worker, "final_name", stored.get)
    tasks: list[Task] = [
        ((0, 1, 0), R + "\\b", 0),
        ((0, 2, 0), R + "\\A\\inner", 0),
        ((0, 3, 0), R + "\\A", 0),
    ]
    res = run(tasks)
    assert [path for _ref, path, msg in res.errors if msg == worker.CASE_ERROR] == [
        R + "\\b",
        R + "\\A\\inner",
    ]
    assert res.paths == [R + "\\A", R + "\\A\\inner"]  # A itself, then the subfolder it holds
    # without the root's own stored name only the last names tell, and never fewer of them
    del stored[R]
    res = run(tasks)
    assert [path for _ref, path, _msg in res.errors] == [R + "\\A\\inner"]


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
    # more than this one folder: its listing is all the relist takes as its children
    two = np.array([[0, 5, 0], [1, 0, 0]], dtype=np.int64)
    assert relist([], refs=two, paths=[top, os.path.join(top, "sub")]) is None


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


def case_sensitive(folder: Path) -> None:
    """Makes ``folder`` hold names that differ only in case, or skips. On Windows it becomes a
    case-sensitive NTFS folder (fsutil's exit code says nothing: 0 also when that failed, so the
    folder is tried); elsewhere the file system decides (APFS folds case by default). As
    packages/native/tests/test_hardscan.py ``_case_sensitive`` does."""
    folder.mkdir(parents=True, exist_ok=True)
    if sys.platform == "win32":
        with contextlib.suppress(OSError):
            subprocess.run(
                ["fsutil", "file", "setCaseSensitiveInfo", str(folder), "enable"],
                capture_output=True,
                check=False,
            )
    (folder / "probe").mkdir()
    try:
        (folder / "PROBE").mkdir()
    except FileExistsError:
        pytest.skip(f"{folder} cannot hold names that differ only in case")
    finally:
        (folder / "probe").rmdir()
    (folder / "PROBE").rmdir()


@pytest.fixture
def twins(tmp_path: Path) -> tuple[Path, Path, Path]:
    """A scan root with folders `a` and `A` side by side (a case-sensitive folder on Windows)."""
    root = tmp_path / "root"
    case_sensitive(root)
    (root / "a").mkdir()
    (root / "A").mkdir()
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
    """A stored name elsewhere (another name of a hard link, a redirect) with no name on the way
    that another entry has but for case: the path names the item. With such a pair on the way it
    may not: `A` can be a symlink that won the name of `a` and led elsewhere."""
    root, a, big_a = twins
    plain = root / "plain"
    plain.mkdir()
    (plain / "f").write_bytes(b"p")
    elsewhere = {str(plain): "\\elsewhere\\other", str(a / "f"): "\\elsewhere\\y\\f"}
    monkeypatch.setattr(trash, "_final_name", stored_as(elsewhere))
    (res,) = trash.delete_permanently([str(a / "f")], roots={str(a / "f"): str(root)}, check_locks=False)
    assert res.code == "ambiguous_name" and (a / "f").exists()
    (res,) = trash.delete_permanently([str(plain)], roots={str(plain): str(root)}, check_locks=False)
    assert res.ok and not plain.exists() and (a / "f").exists() and (big_a / "g").exists()


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


def test_names_held_twice_cannot_go_into_the_basket() -> None:
    """The scan lists each of two entries of the very same name (WinBtrfs shows invalid UTF-8 as
    U+FFFD) with its own contents; a path opens one of them, so neither is taken, nor anything
    below them."""
    store = NodeStore()
    root = store.add_root(ROOT)
    names = ["n�", "n�", "ok"]
    first = store.ingest_names(root, [n.encode() for n in names], entries([Kind.DIR] * 3))
    inner = store.ingest_names(first + 1, [b"f"], entries([Kind.FILE], [1]))
    b = Basket(volume_of=lambda root: "VOL")
    res = b.add(store, "s1", [first, first + 1, inner, first + 2])
    assert [(r.path, r.reason) for r in res.refused] == [
        (store.path(first), basket.TEXT["dup_name"]),
        (store.path(first + 1), basket.TEXT["dup_name"]),
        (store.path(inner), basket.TEXT["dup_name"]),
    ]
    assert len(res.added) == 1


def test_items_keep_the_scan_root_they_were_found_under() -> None:
    """Deleting checks a path's names below its scan root (``reaches_another``): the basket keeps
    the root with each item, so the check holds once the scan is closed too."""
    store, ids = twin_store()
    b = Basket(volume_of=lambda root: "VOL")
    b.add(store, "s1", [ids["a/f"]])
    b.add(store, "s1", [next(iter(store.roots()))], members=[ids["b"]])  # a group of small items
    assert {(t.path, t.root) for t in b.targets()} == {(path("a", "f"), ROOT), (path("b"), ROOT)}


def test_a_name_whose_case_changed_since_the_scan_still_reaches_the_item(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The scan saw `Photos`, the folder is `photos` now: no entry has the old name, so the path
    still opens the item (Windows) and is not refused. A stored name somewhere else altogether
    (another name of a hard link, a redirect) is no sibling either."""
    root = tmp_path / "root"
    (root / "photos").mkdir(parents=True)
    (root / "d1").mkdir()
    (root / "d2").mkdir()
    (root / "d2" / "file.txt").write_bytes(b"f")
    stale = str(root / "Photos")
    linked = str(root / "d2" / "file.txt")
    stored = {str(root): "\\x\\root", stale: "\\x\\root\\photos", linked: "\\x\\root\\d1\\File.txt"}
    monkeypatch.setattr(trash, "_final_name", stored_as(stored))
    assert not trash.reaches_another(stale, str(root))
    assert not trash.reaches_another(linked, str(root))
    # with both names in the folder, the stored one is another entry's, or a link that one is
    # led elsewhere
    (root / "x").mkdir()
    case_sensitive(root / "x" / "cs")
    (root / "x" / "cs" / "Photos").mkdir()
    (root / "x" / "cs" / "photos").mkdir()
    twin = str(root / "x" / "cs" / "Photos")
    below = twin + os.sep + "f"
    stored |= {twin: "\\x\\root\\x\\cs\\photos", below: "\\x\\root\\x\\f"}
    assert trash.reaches_another(twin, str(root))
    assert trash.reaches_another(below, str(root))
    stored[twin] = "\\x\\root\\x\\cs\\Photos"  # what it is stored as: nothing to look at
    assert not trash.reaches_another(twin, str(root))


@pytest.mark.skipif(sys.platform != "win32", reason="Windows opens names case-insensitively")
def test_windows_a_stale_case_is_deleted_by_its_path(tmp_path: Path) -> None:
    root = tmp_path / "root"
    (root / "photos").mkdir(parents=True)
    (root / "photos" / "big.mov").write_bytes(b"m")
    stale = str(root / "Photos" / "big.mov")
    (res,) = trash.delete_permanently([stale], roots={stale: str(root)}, check_locks=False)
    assert res.ok and not (root / "photos" / "big.mov").exists()


@pytest.mark.skipif(sys.platform != "win32", reason="Win32 path rules")
def test_windows_names_ending_in_a_dot_are_never_deleted_by_their_path(tmp_path: Path) -> None:
    """Win32 trims a name's trailing dot or space: by its path the folder `odd.` is `odd`. The scan
    lists both as they are; deleting or trashing `odd.` would touch `odd`, so it is refused, as is
    adding such an item to the basket."""
    root = tmp_path / "root"
    root.mkdir()
    long_root = "\\\\?\\" + str(root)
    for name in ("odd", "odd."):
        os.mkdir(long_root + "\\" + name)
        with open(long_root + "\\" + name + "\\in.txt", "wb") as f:
            f.write(name.encode())
    try:
        target = str(root / "odd.")
        for op in (trash.delete_permanently, trash.move_to_trash):
            (res,) = op([target], roots={target: str(root)}, check_locks=False)
            assert (res.code, res.message) == ("ambiguous_name", trash.TEXT["trailing_name"]), op
        assert sorted(os.listdir(long_root)) == ["odd", "odd."]
        assert trash.win32_trims(str(root / "x." / "f")) and trash.win32_trims(str(root / "sp "))
        assert not trash.win32_trims(str(root / "a.b" / "c"))
        store = NodeStore()
        top = store.add_root(str(root))
        first = store.ingest_names(top, [b"odd.", b"odd"], entries([Kind.DIR] * 2))
        res_add = Basket(volume_of=lambda root: "VOL").add(store, "s1", [first, first + 1])
        assert [r.reason for r in res_add.refused] == [basket.TEXT["trailing_name"]]
    finally:
        for name in ("odd", "odd."):
            shutil.rmtree(long_root + "\\" + name, ignore_errors=True)


# --------------------------------------------------------------------------- relisting by path


def test_paths_that_name_no_folder_alone() -> None:
    """The monitor relists folders by path: not those whose path opens another one too."""
    store, ids = twin_store()
    root = next(iter(store.roots()))
    index = PathIndex(store)
    dup = ids["dup"]
    inner = store.ingest_names(dup + 1, [b"x"], entries([Kind.DIR]))
    slashed = store.ingest_names(ids["b"], [b"s/l", b"t"], entries([Kind.DIR] * 2))
    assert index.unnamed(dup) and index.unnamed(dup + 1) and index.unnamed(inner)
    assert index.unnamed(slashed) and not index.unnamed(slashed + 1)
    assert not index.unnamed(ids["a"]) and not index.unnamed(ids["a/f"]) and not index.unnamed(root)


def test_relisting_pairs_names_held_twice_in_order() -> None:
    """A folder relisted with two entries of the very same name keeps each one's own subtree."""
    store = NodeStore()
    root = store.add_root(ROOT)
    first = store.ingest_names(root, [b"n\xef\xbf\xbd", b"n\xef\xbf\xbd"], entries([Kind.DIR] * 2))
    store.ingest_names(first, [b"one"], entries([Kind.FILE], [1]))
    store.ingest_names(first + 1, [b"two"], entries([Kind.FILE], [2]))
    blob = b"n\xef\xbf\xbd" * 2
    rel = mutate.relist(store, root, blob, np.array([4, 4]), entries([Kind.DIR] * 2))
    assert len(rel.moved_to) == 2 and not len(rel.removed)
    kept = [int(rel.moved_to[i]) for i in np.argsort(rel.moved_from)]
    assert [[store.name(c) for c in store.children(k)] for k in kept] == [["one"], ["two"]]


async def test_a_relist_leaves_new_subfolders_no_path_reaches_unscanned(
    fake_volume: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    """New subfolders a relist finds that no path can open (the second of the very same name, a
    backslash) are not scanned by their path: they are unreadable, not pending. On a relist of the
    whole subtree (F5) folders of a name held twice are not listed again by path either."""
    import spaceknife.server.monitors as monitors_mod

    monkeypatch.setattr(monitors_mod, "verify_names", lambda volume: True)
    store = NodeStore()
    root = store.add_root(R)
    plain = store.ingest_names(root, [b"plain"], entries([Kind.DIR]))
    store.clear(plain, Flag.PENDING)  # listed already
    mon: Any = ScanMonitor.__new__(ScanMonitor)
    mon.handle = SimpleNamespace(
        store=store, roots=[SimpleNamespace(path=R, volume=None)], scanner=None, id="s1"
    )
    mon.owner = SimpleNamespace(views=SimpleNamespace(chains=lambda _id: None, remap=lambda *_a: None))

    async def fake_slice(tasks: list[Task], opts: SliceOptions) -> SliceResult:
        return worker.scan_slice(tasks, opts)

    async def permit() -> None:
        return None

    mon._slice = fake_slice
    mon._permit = permit
    applied = await mon._relist({root: R})
    c = store.cols
    rows = [(store.name(i), int(c["flags"][i])) for i in store.children(root)]
    denied, pending = np.uint32(Flag.ACCESS_DENIED), np.uint32(Flag.PENDING)
    assert [(n, bool(f & denied), bool(f & pending)) for n, f in rows] == [
        ("a", False, True),
        ("A", False, True),
        ("b", False, True),
        ("B", False, False),  # a file
        ("n\ufffd", False, True),  # the first of the same name: scanned by its path, checked
        ("n\ufffd", True, False),
        ("s\\l", True, False),
        ("plain", False, False),  # listed already: kept
    ]
    assert sorted(store.name(i) for i in applied.pending) == sorted(["a", "A", "b", "n\ufffd"])
    for i in store.children(root):
        store.clear(i, Flag.PENDING)
    again = await mon._relist({root: R})
    names = sorted(store.name(i) for i in again.kept_folders)
    assert names == sorted(["a", "A", "b", "plain"] + ([] if os.name == "nt" else ["s\\l"]))
