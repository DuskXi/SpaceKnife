"""ops.trash: recycle bin / freedesktop trash round trips, refusals, permanent deletion.

Only files these tests create under ``tmp_path`` are touched. Everything a test moves to the real
recycle bin is restored by the test itself (and, should an assertion fail first, by the
``trashed`` fixture), so the bin is left as it was. On Linux the home trash is pointed into
``tmp_path`` through ``XDG_DATA_HOME``, exactly as the specification allows.
"""

from __future__ import annotations

import os
import re
import shutil
import stat
import struct
import sys
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from spaceknife.ops import trash as T
from spaceknife.ops.basket import Basket
from spaceknife.ops.trash import (
    TEXT,
    OpResult,
    delete_permanently,
    move_to_trash,
    parse_recycle_info,
    parse_trashinfo,
    rename_noreplace,
    restore,
    trash_available,
    trash_status,
)

from .store_builder import build, find
from .test_ops_locks import held_open

WIN = sys.platform == "win32"
MAC = sys.platform == "darwin"
LP = "\\\\?\\"
needs_trash = pytest.mark.skipif(MAC, reason="macOS trash: implemented best effort, not tested here")
windows_only = pytest.mark.skipif(not WIN, reason="Windows recycle bin")
linux_only = pytest.mark.skipif(WIN or MAC, reason="freedesktop trash")


@pytest.fixture
def trashed() -> Iterator[list[str]]:
    """Refs a test put into the real trash; whatever is still there at the end goes back."""
    refs: list[str] = []
    yield refs
    left = [r for r in refs if r]
    if left:
        restore(left)


@pytest.fixture(autouse=True)
def isolated(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "skhome"))
    if not WIN:
        monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path / "xdg"))  # the home trash, inside tmp_path


def make(tmp_path: Path) -> tuple[Path, Path]:
    data = tmp_path / "data"
    (data / "folder a" / "sub").mkdir(parents=True)
    f = data / "报告 final.txt"
    f.write_bytes(b"hello")
    (data / "folder a" / "sub" / "x.bin").write_bytes(b"x" * 10)
    return f, data / "folder a"


def same(a: str, b: str) -> bool:
    return os.path.normcase(os.path.normpath(a)) == os.path.normcase(os.path.normpath(b))


# --------------------------------------------------------------------------- round trips


@needs_trash
def test_trash_and_restore(tmp_path: Path, trashed: list[str]) -> None:
    f, d = make(tmp_path)
    seen: list[tuple[int, int, str]] = []
    res = move_to_trash([str(f), str(d)], progress=lambda done, total, r: seen.append((done, total, r.code)))
    trashed.extend(r.trash_ref for r in res)
    assert [(r.code, r.ok, r.message) for r in res] == [("ok", True, TEXT["trashed"])] * 2
    assert [r.path for r in res] == [str(f), str(d)]
    assert not f.exists() and not d.exists()
    assert seen == [(1, 2, "ok"), (2, 2, "ok")]
    if WIN:
        for r, original, size in zip(res, (f, d), (5, 10), strict=True):
            assert os.path.basename(r.trash_ref).startswith("$I")
            assert os.path.basename(os.path.dirname(os.path.dirname(r.trash_ref))).lower() == "$recycle.bin"
            got_size, when, path = parse_recycle_info(Path(r.trash_ref).read_bytes())
            assert same(path, str(original)) and got_size == size and when > 1.7e9
    else:
        info = Path(res[0].trash_ref)
        assert info.parent == tmp_path / "xdg" / "Trash" / "info"
        text = info.read_text()
        assert text.startswith("[Trash Info]\n")
        assert f"Path={T.quote(os.fsencode(str(f)), safe='/')}\n" in text
        assert re.search(r"^DeletionDate=\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d$", text, re.M)
        assert (tmp_path / "xdg" / "Trash" / "files" / f.name).read_bytes() == b"hello"
    back = restore([r.trash_ref for r in res])
    assert [(b.code, b.message) for b in back] == [("ok", TEXT["restored"])] * 2
    trashed.clear()
    assert [same(b.path, str(p)) for b, p in zip(back, (f, d), strict=True)] == [True, True]
    assert f.read_bytes() == b"hello" and (d / "sub" / "x.bin").read_bytes() == b"x" * 10


@needs_trash
def test_restore_never_overwrites(tmp_path: Path, trashed: list[str]) -> None:
    f = tmp_path / "gone" / "a.txt"
    f.parent.mkdir()
    f.write_bytes(b"old")
    (r,) = move_to_trash([str(f)])
    trashed.append(r.trash_ref)
    f.write_bytes(b"new")  # something new took the name
    (back,) = restore([r.trash_ref])
    assert (back.code, back.message) == ("exists", TEXT["exists"]) and f.read_bytes() == b"new"
    f.unlink()
    f.parent.rmdir()  # the folder is gone too: restoring makes it again
    (back,) = restore([r.trash_ref])
    assert back.code == "ok"
    trashed.clear()
    assert f.read_bytes() == b"old"
    (again,) = restore([r.trash_ref])
    assert again.code == "missing"


@needs_trash
def test_prechecks_skip_items(tmp_path: Path, trashed: list[str]) -> None:
    own = tmp_path / "skhome" / "data" / "x.sks"
    own.parent.mkdir(parents=True)
    own.write_bytes(b"x")
    held = tmp_path / "held.txt"
    held.write_bytes(b"x")
    paths = [str(tmp_path / "nope"), str(own), str(held)]
    with held_open(held):
        res = move_to_trash(paths)
        trashed.extend(r.trash_ref for r in res)
        assert [r.code for r in res] == ["missing", "protected", "locked"]
        assert "使用" in res[2].message
        res = delete_permanently(paths)
        assert [r.code for r in res] == ["missing", "protected", "locked"]
    assert own.exists() and held.exists()


@needs_trash
def test_cancel_midway(tmp_path: Path, trashed: list[str]) -> None:
    files = []
    for i in range(3):
        p = tmp_path / f"c{i}.txt"
        p.write_bytes(b"c")
        files.append(str(p))
    seen: list[OpResult] = []
    res = move_to_trash(files, progress=lambda done, total, r: seen.append(r), cancel=lambda: bool(seen))
    trashed.extend(r.trash_ref for r in res)
    assert [r.code for r in res] == ["ok", "cancelled", "cancelled"]
    assert [os.path.exists(p) for p in files] == [False, True, True]
    assert len(seen) == 3  # every item gets its final result, the cancelled ones too
    assert [b.code for b in restore([res[0].trash_ref])] == ["ok"]
    trashed.clear()


@needs_trash
def test_trash_status(tmp_path: Path) -> None:
    status = trash_status(str(tmp_path))
    assert status.available and trash_available(str(tmp_path))
    if WIN:
        assert "$Recycle.Bin" in status.location and status.used >= 0 and status.capacity > 0
    else:
        assert status.location == str(tmp_path / "xdg" / "Trash")


def test_bad_refs_are_refused(tmp_path: Path) -> None:
    fake = tmp_path / "info" / "x.trashinfo"
    fake.parent.mkdir()
    fake.write_text("[Trash Info]\nPath=/etc/passwd\n")
    res = restore([str(fake), str(tmp_path / "$Iabc.txt"), "not even a path"])
    assert [r.code for r in res] == ["error"] * 3
    assert {r.message for r in res} == {TEXT["bad_ref"]}


# --------------------------------------------------------------------------- Windows refusals


@windows_only
def test_no_trash_on_a_volume_that_destroys(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    from spaceknife.ops._win import BinSettings

    f, d = make(tmp_path)
    monkeypatch.setattr(
        T, "_bin_settings", lambda guid, total: BinSettings(nuke=True, max_bytes=2**40, configured=True)
    )
    res = move_to_trash([str(f), str(d)])
    assert [(r.code, r.message) for r in res] == [("no_trash", TEXT["no_bin_nuke"])] * 2
    assert f.exists() and (d / "sub" / "x.bin").exists()
    assert not trash_available(str(f))
    assert trash_status(str(f)).reason == TEXT["no_bin_nuke"]


@windows_only
def test_no_trash_when_too_big_for_the_bin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    from spaceknife.ops._win import BinSettings

    big = tmp_path / "big.bin"
    big.write_bytes(b"b" * 2000)
    folder = tmp_path / "many"
    folder.mkdir()
    for i in range(3):
        (folder / f"{i}.bin").write_bytes(b"m" * 500)
    monkeypatch.setattr(
        T, "_bin_settings", lambda guid, total: BinSettings(nuke=False, max_bytes=1000, configured=True)
    )
    res = move_to_trash([str(big), str(folder)], sizes={str(folder): 900})  # a hint near the limit: measured
    assert [r.code for r in res] == ["no_trash", "no_trash"]
    assert all(r.message.startswith("超出回收站容量") for r in res)
    assert big.stat().st_size == 2000 and len(list(folder.iterdir())) == 3


def deep_tree(top: Path) -> Path:
    top.mkdir()
    p = str(top)
    while len(p) < 300:
        p = os.path.join(p, "n" * 40)
        os.mkdir(LP + p)
    with open(LP + os.path.join(p, "leaf.txt"), "wb") as fh:
        fh.write(b"x")
    return Path(p) / "leaf.txt"


@windows_only
def test_no_trash_when_paths_get_too_long(tmp_path: Path) -> None:
    top = tmp_path / "deep"
    leaf = deep_tree(top)
    try:
        (r,) = move_to_trash([str(top)])
        assert (r.code, r.message) == ("no_trash", TEXT["too_long"])
        assert os.path.exists(LP + str(leaf))
    finally:
        shutil.rmtree(LP + str(top))


@windows_only
def test_engine_guard_refuses_destruction(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, trashed: list[str]
) -> None:
    """With the checks out of the way, the shell itself wants to destroy a tree that cannot fit
    the bin (paths too long): the sink refuses, nothing is lost, the next item still goes."""
    top = tmp_path / "deep"
    leaf = deep_tree(top)
    other = tmp_path / "other.txt"
    other.write_bytes(b"o")
    monkeypatch.setattr(T, "WIN_MAX_PATH", 100_000)  # skip the length check: only the guard is left
    try:
        res = move_to_trash([str(top), str(other)])
        trashed.extend(r.trash_ref for r in res)
        assert [(r.code, r.message) for r in res] == [("no_trash", TEXT["refused"]), ("ok", TEXT["trashed"])]
        assert os.path.exists(LP + str(leaf))
        assert [b.code for b in restore([res[1].trash_ref])] == ["ok"]
        trashed.clear()
        assert other.read_bytes() == b"o"
    finally:
        shutil.rmtree(LP + str(top))


@windows_only
def test_engine_reports_a_held_file(tmp_path: Path, trashed: list[str]) -> None:
    held = tmp_path / "held.txt"
    held.write_bytes(b"h")
    with held_open(held):
        (r,) = move_to_trash([str(held)], check_locks=False)  # the shell meets the open handle itself
        trashed.append(r.trash_ref)
    assert r.code == "locked" and held.exists()


@windows_only
def test_hresults_map_to_codes() -> None:
    cases = {
        -2144927705: "locked",  # COPYENGINE_E_SHARING_VIOLATION_SRC
        -2147024864: "locked",  # HRESULT_FROM_WIN32(ERROR_SHARING_VIOLATION)
        -2147024891: "denied",  # E_ACCESSDENIED
        -2147024894: "missing",  # ERROR_FILE_NOT_FOUND
        -2144927689: "no_trash",  # COPYENGINE_E_RECYCLE_SIZE_TOO_BIG
        -2147467260: "cancelled",  # E_ABORT
        -2147467259: "error",  # E_FAIL
    }
    assert {hr: T._hr_result("p", hr).code for hr in cases} == cases


# --------------------------------------------------------------------------- freedesktop details


@linux_only
def test_same_names_get_numbered(tmp_path: Path) -> None:
    a = tmp_path / "one" / "same.txt"
    b = tmp_path / "two" / "same.txt"
    for p, body in ((a, b"1"), (b, b"2")):
        p.parent.mkdir()
        p.write_bytes(body)
    res = move_to_trash([str(a), str(b)])
    assert [os.path.basename(r.trash_ref) for r in res] == ["same.txt.trashinfo", "same.2.txt.trashinfo"]
    assert [x.code for x in restore([r.trash_ref for r in res])] == ["ok", "ok"]
    assert (a.read_bytes(), b.read_bytes()) == (b"1", b"2")


@linux_only
@pytest.mark.parametrize("admin", [False, True])
def test_topdir_trash(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, admin: bool) -> None:
    """Another file system: $topdir/.Trash/$uid (sticky admin trash) or $topdir/.Trash-$uid."""
    vol = tmp_path / "vol"
    f = vol / "dir" / "a b.txt"
    f.parent.mkdir(parents=True)
    f.write_bytes(b"t")
    if admin:
        (vol / ".Trash").mkdir()
        os.chmod(vol / ".Trash", 0o1777)
    monkeypatch.setattr(T, "_existing_dev", lambda path: -1)  # the home trash is elsewhere
    monkeypatch.setattr(T, "_mount_top", lambda path, dev: str(vol))
    (r,) = move_to_trash([str(f)])
    uid = os.getuid()  # type: ignore[attr-defined,unused-ignore]
    root = vol / ".Trash" / str(uid) if admin else vol / f".Trash-{uid}"
    assert r.code == "ok" and Path(r.trash_ref) == root / "info" / "a b.txt.trashinfo"
    assert stat.S_IMODE(os.stat(root).st_mode) == 0o700
    assert "Path=dir/a%20b.txt\n" in Path(r.trash_ref).read_text()  # relative to the topdir
    assert [x.code for x in restore([r.trash_ref])] == ["ok"]
    assert f.read_bytes() == b"t"


@linux_only
def test_no_usable_trash_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    f = tmp_path / "vol" / "a.txt"
    f.parent.mkdir()
    f.write_bytes(b"a")
    monkeypatch.setattr(T, "_existing_dev", lambda path: -1)
    monkeypatch.setattr(T, "_mount_top", lambda path, dev: str(tmp_path / "vol"))
    monkeypatch.setattr(T, "_usable_dir", lambda path, uid, create: False)
    (r,) = move_to_trash([str(f)])
    assert (r.code, r.message) == ("no_trash", TEXT["no_trash_dir"]) and f.exists()
    assert not trash_available(str(f))


# --------------------------------------------------------------------------- pure helpers


def test_parse_recycle_info_versions() -> None:
    path = r"C:\Users\u\报告.txt"
    filetime = 133_000_000_000_000_000
    v2 = struct.pack("<qqqi", 2, 1234, filetime, len(path) + 1) + (path + "\0").encode("utf-16-le")
    v1 = struct.pack("<qqq", 1, 99, filetime) + (path + "\0").encode("utf-16-le").ljust(520, b"\0")
    assert parse_recycle_info(v2) == (1234, (filetime - 116444736000000000) / 1e7, path)
    assert parse_recycle_info(v1)[::2] == (99, path)
    for bad in (b"", struct.pack("<qqq", 7, 0, 0), struct.pack("<qqqi", 2, 0, 0, 1) + b"\0\0"):
        with pytest.raises(ValueError, match="record"):
            parse_recycle_info(bad)


def test_parse_trashinfo() -> None:
    text = "[Trash Info]\nPath=/home/u/a%20b/%E6%96%87.txt\nDeletionDate=2026-10-18T12:00:00\n"
    assert parse_trashinfo(text) == "/home/u/a b/文.txt"
    assert parse_trashinfo("[Other]\nPath=/x\n[Trash Info]\nPath=rel/y\n") == "rel/y"
    with pytest.raises(ValueError, match="no Path"):
        parse_trashinfo("[Other]\nPath=/x\n")


def test_rename_noreplace(tmp_path: Path) -> None:
    a, b, c = tmp_path / "a", tmp_path / "b", tmp_path / "c"
    a.write_bytes(b"a")
    b.write_bytes(b"b")
    with pytest.raises(FileExistsError):
        rename_noreplace(str(a), str(b))
    assert (a.read_bytes(), b.read_bytes()) == (b"a", b"b")
    rename_noreplace(str(a), str(c))
    assert c.read_bytes() == b"a" and not a.exists()


# --------------------------------------------------------------------------- permanent deletion


def make_link(target: Path, link: Path) -> None:
    if WIN:
        import _winapi

        _winapi.CreateJunction(str(target), str(link))
    else:
        link.symlink_to(target, target_is_directory=True)


def test_delete_permanently(tmp_path: Path) -> None:
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "keep.txt").write_bytes(b"k")
    tree = tmp_path / "tree"
    (tree / "a b" / "深").mkdir(parents=True)
    (tree / "a b" / "深" / "x.txt").write_bytes(b"x")
    ro = tree / "readonly.txt"
    ro.write_bytes(b"r")
    os.chmod(ro, stat.S_IREAD)
    make_link(outside, tree / "a b" / "link")  # inside the tree: removed, never followed
    top_link = tmp_path / "top-link"
    make_link(outside, top_link)
    single = tmp_path / "single.bin"
    single.write_bytes(b"s")
    seen: list[str] = []
    res = delete_permanently(
        [str(tree), str(top_link), str(single)], progress=lambda d, t, r: seen.append(r.code)
    )
    assert [(r.code, r.message) for r in res] == [("ok", TEXT["deleted"])] * 3
    assert seen == ["ok"] * 3
    assert not tree.exists() and not os.path.lexists(top_link) and not single.exists()
    assert (outside / "keep.txt").read_bytes() == b"k"


def test_delete_permanently_cancel(tmp_path: Path) -> None:
    files = []
    for i in range(3):
        p = tmp_path / f"d{i}.txt"
        p.write_bytes(b"d")
        files.append(str(p))
    seen: list[Any] = []
    res = delete_permanently(files, progress=lambda d, t, r: seen.append(r), cancel=lambda: bool(seen))
    assert [r.code for r in res] == ["ok", "cancelled", "cancelled"]
    assert [os.path.exists(p) for p in files] == [False, True, True]


def test_delete_inside_a_folder_can_be_cancelled(tmp_path: Path) -> None:
    tree = tmp_path / "t"
    for i in range(3):
        (tree / f"s{i}").mkdir(parents=True)
        (tree / f"s{i}" / "f.txt").write_bytes(b"f")
    polls = iter([False, False, False, True])  # the item, the tree, its first folder; then stop
    (r,) = delete_permanently([str(tree)], cancel=lambda: next(polls, True), check_locks=False)
    assert (r.code, r.message) == ("cancelled", TEXT["cancelled_partly"])
    assert tree.exists()


# --------------------------------------------------------------------------- with the basket


@needs_trash
def test_basket_execution_and_undo(tmp_path: Path, trashed: list[str]) -> None:
    """The flow the service runs: targets → move_to_trash → apply_results; undo → restore → reinstate."""
    root = tmp_path / "scan"
    spec = {"keep.txt": 3, "small": {"s0.log": 1, "s1.log": 2, "s2.log": 3}, "old.iso": 4}
    for rel, size in (
        ("keep.txt", 3),
        ("small/s0.log", 1),
        ("small/s1.log", 2),
        ("small/s2.log", 3),
        ("old.iso", 4),
    ):
        (root / rel).parent.mkdir(parents=True, exist_ok=True)
        (root / rel).write_bytes(b"z" * size)
    store = build(spec, root=str(root))
    basket = Basket(volume_of=lambda r: "VOL")
    iso = basket.add(store, "s", [find(store, "old.iso")]).added[0]
    group = basket.add(
        store, "s", [find(store, "small")], members=[find(store, "small/s0.log"), find(store, "small/s1.log")]
    ).added[0]
    targets = basket.targets()
    res = move_to_trash([t.path for t in targets], sizes={t.path: t.size for t in targets})
    trashed.extend(r.trash_ref for r in res)
    assert [r.code for r in res] == ["ok"] * 3
    left = basket.apply_results(res)
    assert len(basket) == 0 and sorted(i.id for i in left) == sorted([iso, group])
    assert (root / "keep.txt").exists() and (root / "small" / "s2.log").exists()  # only what was captured
    back = restore([r.trash_ref for r in res])
    assert [b.code for b in back] == ["ok"] * 3
    trashed.clear()
    basket.reinstate(left)
    basket.refresh(lambda scan_id: store)
    assert sorted(i.id for i in basket.items()) == sorted([iso, group])
    assert all(i.status == "ready" for i in basket.items())
    assert (root / "old.iso").read_bytes() == b"zzzz"
