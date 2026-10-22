"""spaceknife_native.scan_slice must equal spaceknife.scan.worker.scan_slice, column by column.

Untruncated slices (huge budget) are compared field by field on varied trees; truncated slices
(budget 0, tiny max_entries) are checked for valid references and for reassembling into the
same tree as the Python worker.
"""

from __future__ import annotations

import asyncio
import contextlib
import ctypes
import dataclasses
import errno
import os
import stat
import struct
import subprocess
import sys
from collections.abc import Iterator
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import numpy as np
import pytest

from spaceknife.scan import worker
from spaceknife.scan.scanner import NATIVE_BATCH, Scanner, ScanState, native_slicer
from spaceknife.scan.worker import RootOptions, SliceOptions, SliceResult, Task
from spaceknife.store.nodes import Flag, NodeStore

native = pytest.importorskip("spaceknife_native")
if not hasattr(native, "scan_slice"):
    pytest.skip("spaceknife_native built without the hardscan feature", allow_module_level=True)

IS_WINDOWS = sys.platform == "win32"
FIELDS = [f.name for f in dataclasses.fields(SliceResult)]
EVERYTHING: dict[str, Any] = {"budget_s": 1e9, "max_entries": 10**12}  # never truncate


def nopts(opts: SliceOptions, **extra: Any) -> dict[str, Any]:
    return {
        "roots": [dataclasses.asdict(r) for r in opts.roots],
        "budget_s": opts.budget_s,
        "max_entries": opts.max_entries,
        **extra,
    }


def root_opts(folder: Path, /, **kw: Any) -> RootOptions:
    kw.setdefault("dev", os.stat(folder).st_dev)
    return RootOptions(**kw)


def describe(res: SliceResult, i: int) -> str:
    off = int(res.name_lens[:i].sum())
    name = res.name_blob[off : off + int(res.name_lens[i])]
    listing = int(np.searchsorted(res.bounds, i, side="right")) - 1
    return f"entry {i} {name!r} in {res.paths[listing]!r}"


def assert_same(w: SliceResult, n: dict[str, Any]) -> None:
    assert sorted(n) == sorted(FIELDS)
    for key in ("refs", "bounds", "name_lens", "ext_ids"):
        a, b = getattr(w, key), n[key]
        assert (b.dtype, b.shape) == (a.dtype, a.shape), key
        assert np.array_equal(a, b), key
    assert n["name_blob"] == w.name_blob
    assert n["paths"] == w.paths
    assert list(n["cols"]) == list(w.cols)
    for key, a in w.cols.items():
        b = n["cols"][key]
        assert b.dtype == a.dtype, key
        bad = np.nonzero(a != b)[0]
        assert not len(bad), f"{key}: {describe(w, int(bad[0]))}: worker {a[bad[0]]} native {b[bad[0]]}"
    assert n["ext_table"] == w.ext_table
    assert n["frontier"] == w.frontier
    assert n["errors"] == w.errors
    assert isinstance(n["elapsed"], float)


def compare(tasks: list[Task], roots: tuple[RootOptions, ...], **extra: Any) -> SliceResult:
    opts = SliceOptions(roots=roots, **EVERYTHING)
    worker.scan_slice(tasks, opts)  # see settle(): a first read can still bump timestamps
    w = worker.scan_slice(tasks, opts)
    assert_same(w, native.scan_slice(tasks, nopts(opts, **extra)))
    return w


def scan_one(root: Path, **kw: Any) -> SliceResult:
    return compare([((0, 1, 0), str(root), 0)], (root_opts(root, **kw),))


# ---------------------------------------------------------------------------- trees

EXT_NAMES = [
    "a.TXT",
    "b.txt",
    ".hidden",
    "noext",
    "trailing.",
    "x.0123456789abcdef",
    "x.0123456789abcdefg",
    "multi.part.tar.GZ",
    "ÜBER.ÄÖÜ",
    "sigma.ΣΑΣ",
    "dotted-İ.İJ",
    "照片.JPG",
    "emoji😀.😀X",
    "MiXeD.JpG",
]


def build_tree(root: Path) -> Path:
    root.mkdir()
    for i, size in enumerate([0, 1, 511, 512, 513, 4095, 4096, 4097, 100_000]):
        (root / f"size{i}.bin").write_bytes(b"x" * size)
    for name in EXT_NAMES:
        (root / name).write_bytes(b"e" * len(name))
    (root / "empty").mkdir()
    (root / "中文 文件夹").mkdir()
    (root / "中文 文件夹" / "照片.jpg").write_bytes(b"p" * 1234)
    deep = root / "deep"
    for i in range(40):
        deep = deep / f"level{i}"
    deep.mkdir(parents=True)
    (deep / "bottom.dat").write_bytes(b"b" * 77)
    big = root / "big"
    big.mkdir()
    for i in range(3000):  # several directory-query buffers
        (big / f"file-with-a-fairly-long-name-{i:05d}.log").write_bytes(b"")
    nest = root / "nest"
    for a in range(4):
        for b in range(3):
            d = nest / f"a{a}" / f"b{b}"
            d.mkdir(parents=True)
            (d / f"f{a}{b}.py").write_text("x" * (a * 10 + b))
    return root


def settle(root: Path) -> None:
    """Read every directory once: the first read of fresh directories can update their
    timestamps (relatime atimes; NTFS refreshes a directory's entry in its parent lazily)."""
    for _ in os.walk(root):
        pass


@pytest.fixture
def tree(tmp_path: Path) -> Path:
    root = build_tree(tmp_path / "root")
    settle(root)
    return root


# ---------------------------------------------------------------------------- equality


def test_varied_tree_is_identical(tree: Path) -> None:
    res = scan_one(tree)
    assert len(res.paths) > 50 and len(res.name_lens) > 3000
    assert "jpg" in res.ext_table and "äöü" in res.ext_table and "σας" in res.ext_table


@pytest.mark.parametrize("threads", [1, 2, 3, 5, 7, 64])  # more sizes than pools are kept
def test_thread_count_does_not_change_output(tree: Path, threads: int) -> None:
    compare([((0, 1, 0), str(tree), 0)], (root_opts(tree),), threads=threads)


def test_root_options_apply(tree: Path) -> None:
    scan_one(tree, cluster_size=8192, resident_max=512)
    scan_one(tree, cluster_size=0, resident_max=0)


def test_several_tasks_roots_and_failures(tmp_path: Path, tree: Path) -> None:
    other = tmp_path / "other"
    other.mkdir()
    (other / "z.Z").write_bytes(b"zz")
    (other / "sub").mkdir()
    tasks: list[Task] = [
        ((0, 5, 0), str(tree / "nest"), 0),
        ((0, 9, 0), str(tmp_path / "missing"), 1),
        ((0, 7, 0), str(other), 1),
        ((0, 8, 0), str(tree / "size1.bin"), 0),
        ((0, 6, 0), str(tree / "中文 文件夹"), 0),
    ]
    roots = (root_opts(tree), root_opts(other, cluster_size=512))
    res = compare(tasks, roots)
    assert len(res.errors) == 2
    assert [e[0] for e in res.errors] == [(0, 9, 0), (0, 8, 0)]


def test_only_failures_give_empty_columns(tmp_path: Path) -> None:
    res = compare([((0, 1, 0), str(tmp_path / "nope"), 0)], (RootOptions(),))
    assert res.refs.shape == (0, 3) and len(res.errors) == 1


def test_empty_task_list() -> None:
    compare([], (RootOptions(),))


def test_symlinks_are_not_followed(tmp_path: Path) -> None:
    root = tmp_path / "links"
    root.mkdir()
    target = root / "target"
    target.mkdir()
    (target / "inside.txt").write_bytes(b"inside")
    (root / "file.txt").write_bytes(b"file")
    made = 0
    for name, dest, is_dir in [
        ("dirlink", target, True),
        ("filelink", root / "file.txt", False),
        ("dangling", root / "gone", False),
    ]:
        try:
            os.symlink(dest, root / name, target_is_directory=is_dir)
            made += 1
        except OSError:
            pass  # unprivileged Windows: no symlinks
    if IS_WINDOWS:
        import _winapi

        _winapi.CreateJunction(str(target), str(root / "junction"))
        made += 1
    if not made:
        pytest.skip("cannot create links here")
    res = scan_one(root)
    assert res.paths == [str(root), str(target)]  # links are entries, never listings


# ---------------------------------------------------------------------------- Windows specifics

if IS_WINDOWS:
    from ctypes import wintypes

    _k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    _k32.SetFileAttributesW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD]
    _k32.DeviceIoControl.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
        ctypes.c_void_p,
    ]


def _set_attrs(path: Path, attrs: int) -> None:
    assert _k32.SetFileAttributesW(str(path), attrs), ctypes.get_last_error()  # type: ignore[attr-defined,unused-ignore]


def _fsctl(path: Path, code: int, payload: bytes | None = None) -> bool:
    import msvcrt

    with open(path, "r+b") as f:
        handle = msvcrt.get_osfhandle(f.fileno())  # type: ignore[attr-defined,unused-ignore]
        buf = ctypes.create_string_buffer(payload) if payload else None
        ret = wintypes.DWORD(0)
        ok = _k32.DeviceIoControl(handle, code, buf, len(payload or b""), None, 0, ctypes.byref(ret), None)
    return bool(ok)


@pytest.mark.skipif(not IS_WINDOWS, reason="Windows file attributes")
def test_windows_attributes_compressed_and_sparse(tmp_path: Path) -> None:
    root = tmp_path / "attrs"
    root.mkdir()
    for name, attrs in [
        ("hidden.txt", 0x2),
        ("system.txt", 0x4),
        ("readonly.txt", 0x1),
        ("plain.txt", 0x80),
        ("notindexed.txt", 0x2000),
        ("temp.tmp", 0x100),
        ("offline.dat", 0x1000),
    ]:
        (root / name).write_bytes(b"a" * 700)
        _set_attrs(root / name, attrs)
    hdir = root / "hiddendir"
    hdir.mkdir()
    (hdir / "in.txt").write_bytes(b"in")
    _set_attrs(hdir, 0x2 | 0x4)
    sparse = root / "sparse.bin"
    sparse.write_bytes(b"")
    compressed = root / "compressed.bin"
    compressed.write_bytes(b"")
    have_sparse = _fsctl(sparse, 0x900C4)  # FSCTL_SET_SPARSE
    have_comp = _fsctl(compressed, 0x9C040, (1).to_bytes(2, "little"))  # FSCTL_SET_COMPRESSION, default
    with open(sparse, "r+b") as f:
        f.seek(5_000_000)
        f.write(b"tail")
        f.flush()
        os.fsync(f.fileno())
    with open(compressed, "r+b") as f:
        f.write(bytes(1_000_000))
        f.flush()
        os.fsync(f.fileno())
    try:
        res = scan_one(root, cluster_size=4096, resident_max=512)
    finally:
        for p in root.iterdir():
            _set_attrs(p, 0x80)
    flags = dict(zip(_names(res), res.cols["flags"].tolist(), strict=True))
    assert flags["hidden.txt"] & Flag.HIDDEN and flags["readonly.txt"] & Flag.READONLY
    assert flags["hiddendir"] & Flag.SYSTEM
    if have_sparse:
        assert flags["sparse.bin"] & Flag.SPARSE
        alloc = dict(zip(_names(res), res.cols["alloc"].tolist(), strict=True))
        assert alloc["sparse.bin"] < 5_000_000
    if have_comp:
        assert flags["compressed.bin"] & Flag.COMPRESSED


def _names(res: SliceResult) -> list[str]:
    out, off = [], 0
    for n in res.name_lens.tolist():
        out.append(res.name_blob[off : off + n].decode("utf-8", worker.NAME_ERRORS))
        off += n
    return out


@pytest.mark.skipif(not IS_WINDOWS, reason="Windows long paths")
def test_windows_long_paths(tmp_path: Path) -> None:
    root = tmp_path / "long"
    deep = "\\\\?\\" + str(root)
    for i in range(30):
        deep += f"\\directory-level-{i:02d}"
    os.makedirs(deep)
    with open(deep + "\\leaf.txt", "wb") as f:
        f.write(b"leaf")
    assert len(deep) > 600
    res = scan_one(root)
    assert len(res.paths) == 31 and not res.errors


@pytest.mark.skipif(not IS_WINDOWS, reason="Windows UTF-16 names")
def test_windows_lone_surrogate_names(tmp_path: Path) -> None:
    root = tmp_path / "surrogates"
    root.mkdir()
    for name in ["lone\ud800.TXT", "ext.\udc00AB", "pair\U0001f600.X"]:
        with open(str(root / name), "wb") as f:
            f.write(b"s")
    (root / "dir\udfff").mkdir()
    scan_one(root)


@pytest.mark.skipif(not IS_WINDOWS, reason="Windows ACLs")
def test_windows_denied_directory(tmp_path: Path) -> None:
    root = tmp_path / "acl"
    denied = root / "denied"
    denied.mkdir(parents=True)
    (denied / "secret.txt").write_bytes(b"s")
    (root / "open.txt").write_bytes(b"o")
    everyone = "*S-1-1-0"
    subprocess.run(["icacls", str(denied), "/deny", f"{everyone}:(RD)"], check=True, capture_output=True)
    try:
        try:
            os.listdir(denied)
            bypassed = True  # this process is not held by the ACE (backup privilege, sandboxing …)
        except PermissionError:
            bypassed = False
        if bypassed:
            pytest.skip("this environment lists directories despite a deny ACE")
        res = scan_one(root)
    finally:
        subprocess.run(["icacls", str(denied), "/remove:d", everyone], check=True, capture_output=True)
    assert len(res.errors) == 1 and res.errors[0][1] == str(denied)


@pytest.mark.skipif(not IS_WINDOWS, reason="Win32 path normalisation")
def test_windows_trailing_dot_names_are_listed(tmp_path: Path) -> None:
    """Win32 trims a name's trailing dot or space: by its plain path the folder `odd.` would list
    its sibling `odd`. Both walkers list each as it is (NT paths natively, `\\\\?\\` paths in the
    worker), also as tasks."""
    root = tmp_path / "dots"
    root.mkdir()
    for name in ("odd.", "odd", "space "):
        os.mkdir("\\\\?\\" + str(root / name))
        with open("\\\\?\\" + str(root / name) + f"\\in-{name.strip('. ')}.txt", "wb") as f:
            f.write(name.encode())
    res = scan_one(root)
    assert sorted(res.paths) == sorted(
        [str(root), str(root / "odd"), str(root / "odd."), str(root / "space ")]
    )
    assert not res.errors and len(res.name_lens) == 6
    res = compare([((0, 1, 0), str(root / "odd."), 0)], (root_opts(root),))
    assert _names(res) == ["in-odd.txt"]


@pytest.mark.skipif(not IS_WINDOWS, reason="Windows relative opens")
def test_windows_relative_opens_list_the_same_tree(tree: Path) -> None:
    """Subdirectories opened by name relative to their parent's handle (and checked against the
    file id the parent's listing gave) list exactly what opens by full path list: the worker's
    tree either way, every folder opened once, and no relative open fell back."""
    tasks: list[Task] = [((0, 1, 0), str(tree), 0)]
    roots = (root_opts(tree),)
    native._open_stats(True)
    compare(tasks, roots, relative_open=False)
    off = native._open_stats(True)
    compare(tasks, roots)  # on by default
    on = native._open_stats(True)
    assert off["relative"] == 0 and off["by_path"] > 10
    assert on["relative"] > 10 and on["by_path"] == 1  # only the task itself goes by path
    assert on["relative"] + on["by_path"] == off["by_path"]
    assert on["relative_failed"] == on["id_mismatch"] == on["id_unknown"] == 0


def _chain_to(top: str, deepest: int) -> list[str]:
    """Folders top, top\\d…, … down to one whose path is exactly `deepest` units long."""
    chain = [top]
    os.mkdir(top)
    while len(chain[-1]) + 241 < deepest:
        chain.append(chain[-1] + "\\" + "d" * 240)
        os.mkdir(chain[-1])
    chain.append(chain[-1] + "\\" + "d" * (deepest - len(chain[-1]) - 1))
    os.mkdir(chain[-1])
    return chain


@pytest.mark.skipif(not IS_WINDOWS, reason="NT path length limit")
def test_windows_relative_opens_stop_where_opens_by_path_do(tmp_path: Path) -> None:
    """A folder an open by path cannot reach because its path is too long can still be opened
    relative to its parent. It must not be: the open by path reports it as an error, so relative
    opens report the same error. Near the limit the kernel may refuse paths too, depending on what
    the drive letter resolves to (a SUBST drive's target makes names longer); relative opens leave
    those to the open by path, so both ways list the same. Renaming the top folder longer puts the
    deepest folders past the limit, and next to it. (The worker is left out: next to the limit its
    os.scandir appends `\\*.*` to the path, so it stops a few units sooner.)"""
    root = tmp_path / "deep"
    root.mkdir()
    grow = 199
    chains = {  # top folder -> deepest NT path once the top is `grow` units longer
        "a": 32_900,  # past the limit of an NT path (32,766 units)
        "b": 32_758,  # just within it
    }
    built = {}
    for name, final in chains.items():
        top = "\\\\?\\" + str(root / name)
        built[name] = (top, _chain_to(top, final - grow))
        os.rename(top, "\\\\?\\" + str(root / (name * (1 + grow))))
    try:
        tasks: list[Task] = [((0, 1, 0), str(root), 0)]
        opts = SliceOptions(roots=(root_opts(root),), **EVERYTHING)
        # see settle(): a first read can still bump timestamps
        native.scan_slice(tasks, nopts(opts, relative_open=False))
        by_path = native.scan_slice(tasks, nopts(opts, relative_open=False))
        native._open_stats(True)
        relative = native.scan_slice(tasks, nopts(opts))
        assert native._open_stats(True)["relative"] > 0
        assert_same(SliceResult(**by_path), relative)
        # the first folder of chain a past the limit; nothing below it is reached
        assert len(by_path["errors"]) == 1, by_path["errors"]
    finally:
        for name, (top, chain) in built.items():
            os.rename("\\\\?\\" + str(root / (name * (1 + grow))), top)
            for d in reversed(chain):
                os.rmdir(d)


@pytest.mark.skipif(not IS_WINDOWS, reason="Win32 path length limit")
def test_windows_folder_past_the_path_limit_is_a_listing_error(tmp_path: Path) -> None:
    """A folder whose path is longer than Win32 allows (32,767 units) is a listing error in the
    worker as in the native walker, with the same message, although os.scandir raises ValueError
    for it instead of OSError. Renaming the top folder longer puts the last folder past the limit,
    its parent well within it."""
    root = tmp_path / "toolong"
    root.mkdir()
    grow = 199
    top = "\\\\?\\" + str(root / "a")
    chain = _chain_to(top, 32_600 - grow)
    chain.append(chain[-1] + "\\" + "d" * 200)  # 32,801 units once the top is longer
    os.mkdir(chain[-1])
    unreached = chain[-1] + "\\unreached.txt"
    with open(unreached, "wb") as f:
        f.write(b"u")
    longer = "\\\\?\\" + str(root / ("a" * (1 + grow)))
    os.rename(top, longer)
    try:
        tasks: list[Task] = [((0, 1, 0), str(root), 0)]
        roots = (root_opts(root),)
        compare(tasks, roots, relative_open=False)
        res = compare(tasks, roots)
    finally:
        os.rename(longer, top)
        os.remove(unreached)
        for d in reversed(chain):
            os.rmdir(d)
    assert [e[1] for e in res.errors] == [longer[4:] + chain[-1][len(top) :]]
    assert len(res.paths) == len(chain)  # the root and every folder above the last


# ---------------------------------------------------------------------------- case-only siblings


def _case_sensitive(folder: Path) -> None:
    """Creates `folder` as a case-sensitive NTFS folder, or skips (fsutil setCaseSensitiveInfo
    needs the WSL feature and an NTFS volume). Its exit code says nothing (0 also when it failed),
    so the folder is tried: `probe` and `PROBE` must both be made. As backend/tests/test_case_twins.py
    ``case_sensitive`` does."""
    folder.mkdir(parents=True, exist_ok=True)
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
        pytest.skip(f"cannot make a case-sensitive folder here ({folder})")
    finally:
        (folder / "probe").rmdir()
    (folder / "PROBE").rmdir()


def build_twins(root: Path) -> Path:
    """root/cs is case-sensitive: `a` and `A` with other contents (a subtree and nested twins
    inside `a`), folder `b` next to file `B`, and a plain folder. root/other makes the first listing
    hold two folders, so `_extd_self_ids` gives them one id and the root switches to FileIdBoth."""
    (root / "other").mkdir(parents=True)
    (root / "other" / "o.txt").write_bytes(b"o")
    cs = root / "cs"
    _case_sensitive(cs)
    _case_sensitive(cs / "a")
    (cs / "a" / "sub" / "deeper").mkdir(parents=True)
    (cs / "a" / "sub" / "deeper" / "d.txt").write_bytes(b"d" * 10)
    (cs / "a" / "x").mkdir()
    (cs / "a" / "X").mkdir()
    (cs / "a" / "x" / "lower.txt").write_bytes(b"x")
    (cs / "a" / "X" / "upper.txt").write_bytes(b"XX")
    (cs / "a" / "f.txt").write_bytes(b"f" * 3)
    (cs / "A").mkdir()
    (cs / "A" / "only.txt").write_bytes(b"A" * 5)
    (cs / "b").mkdir()
    (cs / "b" / "in.txt").write_bytes(b"b")
    (cs / "B").write_bytes(b"B" * 7)
    (cs / "plain").mkdir()
    (cs / "plain" / "p.txt").write_bytes(b"p")
    settle(root)
    return cs


# As WinBtrfs behaves: a strict subfolder is opened by its upcased name (in a case-sensitive NTFS
# folder that reaches the upper twin, the entry WinBtrfs's lookup finds), and FileIdExtd ids are
# the listed folder's own.
WINBTRFS = {"_fold_strict": True, "_extd_self_ids": True}


@pytest.mark.skipif(not IS_WINDOWS, reason="case-sensitive NTFS folders")
def test_windows_case_twins_are_listed_by_their_file_ids(tmp_path: Path) -> None:
    """The open by name of `a`, `b` and `x` reaches another entry (the twin, or file `B`): the
    handle's file id or stored name tells, and the folder is opened by the id its parent's listing
    gave it. The result is the true tree, as the worker lists it by path in a case-sensitive folder."""
    root = tmp_path / "twins"
    build_twins(root)
    tasks: list[Task] = [((0, 1, 0), str(root), 0)]
    native._open_stats(True)
    compare(tasks, (root_opts(root),), threads=1, **WINBTRFS)
    stats = native._open_stats(True)
    assert stats["strict_by_id"] >= 3 and stats["pinned"] >= 5, stats  # a, b, a\x; and below
    assert stats["strict_failed"] == 0 and stats["ids_distrusted"] >= 1, stats
    compare(tasks, (root_opts(root),), _fold_strict=True)  # FileIdExtd ids, 16 bytes by id
    compare(tasks, (root_opts(root),), relative_open=False, _fold_strict=True)


@pytest.mark.skipif(not IS_WINDOWS, reason="case-sensitive NTFS folders")
def test_windows_case_twins_never_wait_in_the_frontier(tmp_path: Path) -> None:
    """Past the budget a folder opened by id, and everything below it, is listed in the same call:
    as a task its path would open the twin. At budget 0 (a relist of one folder) the twins wait
    like any folder. Past the cap on what is listed after the budget, folders below one opened by
    id are errors, never tasks."""
    cs = build_twins(tmp_path / "twins")
    tasks: list[Task] = [((0, 1, 0), str(cs), 0)]
    opts = {"roots": [{}], "max_entries": 1, "threads": 1, "_fold_strict": True}
    res = native.scan_slice(tasks, opts)
    check_slice(res, tasks)
    assert [p for _ref, p, _root in res["frontier"]] == [str(cs / "plain")]
    assert not res["errors"]
    assert {str(cs / n) for n in ("a", "A", "b", "a\\sub", "a\\sub\\deeper", "a\\x", "a\\X")} <= set(
        res["paths"]
    )
    # budget 0: one listing, the twins in the frontier with the plain folder, no errors
    res = native.scan_slice(tasks, {**opts, "budget_s": 0.0, "max_entries": 10**9})
    assert res["paths"] == [str(cs)] and not res["errors"]
    assert sorted(p for _ref, p, _root in res["frontier"]) == sorted(
        str(cs / n) for n in ("A", "a", "b", "plain")
    )
    # a cap of two entries: `a` (and maybe `A` before it) is listed, then the cap is reached. Below
    # `a` (opened by id) nothing more, and nothing left as a task; the twins `A` and `b` can wait
    # in the frontier: as tasks they are checked by stored name on Btrfs
    native._open_stats(True)
    res = native.scan_slice(tasks, {**opts, "_pinned_cap": 2})
    check_slice(res, tasks)
    assert native._open_stats(True)["pinned_cap_hit"] >= 3
    capped = {p for _ref, p, msg in res["errors"] if msg == worker.CAP_ERROR}
    assert capped == {str(cs / "a" / n) for n in ("sub", "x", "X")}
    waiting = {p for _ref, p, _root in res["frontier"]}
    assert {str(cs / "b"), str(cs / "plain")} <= waiting <= {str(cs / n) for n in ("A", "b", "plain")}


@pytest.mark.skipif(not IS_WINDOWS, reason="case-sensitive NTFS folders")
def test_windows_scanner_driven_scans_of_case_twins_build_the_true_tree(tmp_path: Path) -> None:
    """As the scanner drives it, in slices: the same tree as the worker's by path, and no folder a
    task's path could not stand for is ever a task (on WinBtrfs such a path opens the twin): not
    `a`, `A` or `b`, nothing below `a` and `b` (opened by file id), and nothing past the cap."""
    root = tmp_path / "twins"
    cs = build_twins(root)
    py_store = scan_store(root)  # the worker by path: right in a case-sensitive folder
    never = [str(cs / n) for n in ("a", "A", "b")]
    below = [str(cs / n) + "\\" for n in ("a", "b")]

    def slicer(tasks: list[Task], opts: SliceOptions) -> SliceResult:
        for _ref, path, _root in tasks:
            assert path not in never and not path.startswith(tuple(below)), path
        res = SliceResult(**native.scan_slice(tasks, nopts(opts, threads=1, _fold_strict=True)))
        assert not any(msg == worker.CAP_ERROR for _ref, _path, msg in res.errors), res.errors
        return res

    for budget_s in (0.0005, 0.05):
        pool = ThreadPoolExecutor(max_workers=4)
        try:
            scanner = Scanner([str(root)], executor=pool, concurrency=4, slicer=slicer, budget_s=budget_s)
            result = asyncio.run(scanner.run())
        finally:
            pool.shutdown()
        assert result.state == ScanState.DONE and not scanner.progress().denied
        assert tree_snapshot(scanner.store) == tree_snapshot(py_store)


@pytest.mark.skipif(not IS_WINDOWS, reason="case-sensitive NTFS folders")
def test_windows_case_twins_without_ids_are_decided_by_their_stored_names(tmp_path: Path) -> None:
    """A listing whose ids are shared (WinBtrfs's FileIdExtd: every entry has the listed folder's
    own) gives no id to check against: the twins keep their marks, and the stored name decides. The
    open of `a` reaches `A`, stored as `A`: an error, never A's listing under a's name. `b` reaches
    file `B`: the open's own error. `A` is listed, and so is the plain folder."""
    cs = build_twins(tmp_path / "twins")
    res = native.scan_slice(
        [((0, 1, 0), str(cs), 0)], {"roots": [{}], "threads": 1, **WINBTRFS, **EVERYTHING}
    )
    errors = {path: msg for _ref, path, msg in res["errors"]}
    assert errors.keys() == {str(cs / "a"), str(cs / "b")}
    assert errors[str(cs / "a")] == worker.CASE_ERROR
    assert errors[str(cs / "b")] not in (worker.CASE_ERROR, worker.DUP_ERROR)  # the directory name is invalid
    assert {str(cs / "A"), str(cs / "plain")} <= set(res["paths"])
    assert not any(p.startswith(str(cs / "a") + "\\") for p in res["paths"])


@pytest.mark.skipif(not IS_WINDOWS, reason="case-sensitive NTFS folders")
def test_windows_worker_checks_twins_by_their_stored_names(tmp_path: Path) -> None:
    """On a root that verifies names the worker groups a listing's names (str.upper()) and checks
    each twin's stored name when its turn comes. In a case-sensitive folder every path reaches its
    own entry: the true tree, as the native walker lists it opening twins by id."""
    root = tmp_path / "twins"
    build_twins(root)
    compare([((0, 1, 0), str(root), 0)], (root_opts(root, path=str(root), verify_names=True),), **WINBTRFS)


@pytest.mark.skipif(not IS_WINDOWS, reason="case-sensitive NTFS folders, directory symlinks")
def test_windows_a_twin_that_is_a_symlink_is_not_followed(tmp_path: Path) -> None:
    """`x\\A` is a directory symlink to another folder named `a`, and wins the open of `a`'s name
    (as on WinBtrfs, where Linux symlinks are reparse points). The open must reach the link itself,
    which is another entry: `a` goes by its file id and lists its own file, never the target's."""
    (tmp_path / "other" / "a").mkdir(parents=True)
    (tmp_path / "other" / "a" / "OTHER_CONTENT.txt").write_bytes(b"o")
    x = tmp_path / "x"
    _case_sensitive(x)
    (x / "a").mkdir()
    (x / "a" / "real_content.txt").write_bytes(b"r")
    try:
        os.symlink(os.path.join("..", "other", "a"), x / "A", target_is_directory=True)
    except OSError as exc:
        pytest.skip(f"cannot make a directory symlink here: {exc}")
    for extra in ({}, {"relative_open": False}):
        res = SliceResult(
            **native.scan_slice(
                [((0, 1, 0), str(x), 0)], {"roots": [{}], "threads": 1, "_fold_strict": True, **extra}
            )
        )
        names = _names(res)
        bounds = res.bounds.tolist()
        listed = {p: names[bounds[i] : bounds[i + 1]] for i, p in enumerate(res.paths)}
        assert listed.get(str(x / "a")) == ["real_content.txt"], (extra, listed, res.errors)
        assert str(x / "A") not in listed and not res.errors  # a link: listed as an entry, not entered


@pytest.mark.skipif(not IS_WINDOWS, reason="case-sensitive NTFS folders")
def test_windows_a_chain_of_twins_stops_at_the_cap(tmp_path: Path) -> None:
    """Twins at every level, each opened by name (the upper one, which the upcased name reaches):
    past the budget each is listed in the call, but only while the entries listed then stay below
    the cap. Past it the twins go to the frontier (on WinBtrfs their tasks are checked by stored
    name); nothing is listed without bound."""
    top = tmp_path / "t"
    _case_sensitive(top)
    level = top
    for _ in range(8):
        for name in ("q", "Q"):
            _case_sensitive(level / name)
            (level / name / "f.txt").write_bytes(b"x")
        level = level / "Q"
    tasks: list[Task] = [((0, 1, 0), str(top), 0)]
    opts = {"roots": [{}], "threads": 1, "max_entries": 1, "_fold_strict": True}
    res = native.scan_slice(tasks, {**opts, "_pinned_cap": 0})
    check_slice(res, tasks)
    assert res["paths"] == [str(top)] and not res["errors"]
    assert sorted(p for _ref, p, _root in res["frontier"]) == [str(top / "Q"), str(top / "q")]
    res = native.scan_slice(tasks, {**opts, "_pinned_cap": 5})
    check_slice(res, tasks)
    assert 1 < len(res["paths"]) < 6 and res["frontier"] and not res["errors"]
    res = native.scan_slice(tasks, {**opts, "budget_s": 1e9, "max_entries": 10**9})
    assert len(res["paths"]) == 17 and not res["frontier"]  # within the budget: all of it


@pytest.mark.skipif(not IS_WINDOWS, reason="Windows stored names")
def test_windows_tasks_are_checked_by_their_stored_names(tmp_path: Path) -> None:
    """On roots that verify names (Btrfs, where the walker finds it out itself) a task must be the
    folder its path names: in another case than the stored name it is a listing error, in both
    walkers, as a loser that the open of its name does not reach is. The root's own part is never
    compared."""
    root = tmp_path / "names"
    (root / "Mixed" / "inner").mkdir(parents=True)
    (root / "Mixed" / "in.txt").write_bytes(b"i")
    settle(root)
    roots = (root_opts(root, path=str(root), verify_names=True),)
    res = compare([((0, 1, 0), str(root / "MIXED"), 0), ((0, 2, 0), str(root / "Mixed" / "INNER"), 0)], roots)
    assert res.errors == [
        ((0, 1, 0), str(root / "MIXED"), worker.CASE_ERROR),
        ((0, 2, 0), str(root / "Mixed" / "INNER"), worker.CASE_ERROR),
    ]
    res = compare([((0, 1, 0), str(root / "Mixed"), 0)], roots)
    assert res.paths == [str(root / "Mixed"), str(root / "Mixed" / "inner")] and not res.errors
    # the root typed in another case: what lies below it is still compared, the root part not
    upper = str(root).upper()
    roots = (root_opts(root, path=upper, verify_names=True),)
    res = compare([((0, 1, 0), upper, 0), ((0, 2, 0), upper + "\\Mixed", 0)], roots)
    assert not res.errors and len(res.paths) == 5  # the root's listing and the task's
    # without the option NTFS tasks are not checked (no twins outside case-sensitive folders)
    res = compare([((0, 1, 0), str(root / "MIXED"), 0)], (root_opts(root, path=str(root)),))
    assert not res.errors


@pytest.mark.skipif(not IS_WINDOWS, reason="RtlUpcaseUnicodeChar")
def test_windows_upper_groups_every_pair_windows_folds() -> None:
    """The worker groups names by str.upper() and lets the stored name decide: that must find
    every pair the Windows upcase (RtlUpcaseUnicodeChar per UTF-16 unit) takes for the same name.
    It does when upper(c) == upper(Rtl(c)) for every unit; extra groups (ß and SS) cost one check."""
    rtl = ctypes.WinDLL("ntdll").RtlUpcaseUnicodeChar  # type: ignore[attr-defined,unused-ignore]
    rtl.argtypes = [ctypes.c_uint16]
    rtl.restype = ctypes.c_uint16
    folded = 0
    for u in range(0x10000):
        if 0xD800 <= u <= 0xDFFF:
            continue  # surrogates: units of their own, left alone by both
        c, up = chr(u), chr(rtl(u))
        folded += up != c
        assert c.upper() == up.upper(), hex(u)
    assert folded > 900  # the table is there (973 units on Windows 10 and 11)


def test_root_keys_name_what_the_build_accepts(tree: Path) -> None:
    assert {"path", "verify_names"} <= set(native.ROOT_KEYS)
    root = {
        k: v for k, v in dataclasses.asdict(root_opts(tree, path=str(tree))).items() if k in native.ROOT_KEYS
    }
    res = native.scan_slice(
        [((0, 1, 0), str(tree), 0)],
        {"roots": [root], "_fold_strict": False, "_extd_self_ids": False, "_pinned_cap": None, **EVERYTHING},
    )
    assert not res["errors"] and len(res["paths"]) > 50


def test_the_scanner_gives_the_native_walker_each_roots_name_check(
    tree: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A scan of a WinBtrfs volume on Windows (scanner.verify_names) checks every task by its
    stored names: the scanner keeps the root's path and that check with the root, and every slice
    of the native walker gets both. Elsewhere, and on other file systems, nothing is checked."""
    from spaceknife.scan import scanner as scanner_mod

    real_volume_for = scanner_mod.volume_for
    monkeypatch.setattr(scanner_mod, "sys", SimpleNamespace(**{**vars(sys), "platform": "win32"}))
    monkeypatch.setattr(
        scanner_mod, "volume_for", lambda p: dataclasses.replace(real_volume_for(p), fs_type="Btrfs")
    )
    seen: list[list[dict[str, Any]]] = []
    real_scan_slice = native.scan_slice

    def spy(tasks: list[Task], opts: dict[str, Any]) -> dict[str, Any]:
        seen.append(opts["roots"])
        return real_scan_slice(tasks, opts)

    monkeypatch.setattr(native, "scan_slice", spy)
    slicer = native_slicer()
    assert slicer is not None
    pool = ThreadPoolExecutor(max_workers=2)
    try:
        scanner = Scanner([str(tree)], executor=pool, concurrency=2, slicer=slicer, budget_s=0.0005)
        assert asyncio.run(scanner.run()).state == ScanState.DONE
    finally:
        pool.shutdown()
    (opts,) = scanner.root_options
    assert (opts.path, opts.verify_names) == (scanner.roots[0].path, True)
    assert len(seen) > 1 and all(len(roots) == 1 for roots in seen)  # sliced, one root each
    assert {(r[0]["path"], r[0]["verify_names"]) for r in seen} == {(scanner.roots[0].path, True)}
    volume = real_volume_for(str(tree))
    assert not scanner_mod.verify_names(dataclasses.replace(volume, fs_type="NTFS"))
    monkeypatch.setattr(scanner_mod, "sys", sys)
    assert scanner_mod.verify_names(dataclasses.replace(volume, fs_type="Btrfs")) == IS_WINDOWS


class _NtWalk:
    """A reference walker for SPACEKNIFE_TEST_CASE_ROOT, independent of the native one: ntdll through
    ctypes, every folder listed with FileIdBothDirectoryInformation and every subfolder opened by the
    file id its parent's listing gave it, relative to the parent (16 bytes {inode, subvolume} on
    Btrfs), never by its name. Read-only (FILE_LIST_DIRECTORY | SYNCHRONIZE)."""

    def __init__(self) -> None:
        from ctypes import wintypes

        class US(ctypes.Structure):
            _fields_ = [
                ("Length", wintypes.USHORT),
                ("MaximumLength", wintypes.USHORT),
                ("Buffer", ctypes.c_void_p),
            ]

        class OA(ctypes.Structure):
            _fields_ = [
                ("Length", wintypes.ULONG),
                ("RootDirectory", wintypes.HANDLE),
                ("ObjectName", ctypes.POINTER(US)),
                ("Attributes", wintypes.ULONG),
                ("SecurityDescriptor", ctypes.c_void_p),
                ("SecurityQualityOfService", ctypes.c_void_p),
            ]

        class IOSB(ctypes.Structure):
            _fields_ = [("Status", ctypes.c_void_p), ("Information", ctypes.c_size_t)]

        self.US, self.OA, self.IOSB, self.wt = US, OA, IOSB, wintypes
        self.nt = ctypes.WinDLL("ntdll")  # type: ignore[attr-defined,unused-ignore]
        self.buf = ctypes.create_string_buffer(65536)

    def open(self, root: Any, name: bytes, *, by_id: bool = False) -> Any:
        raw = ctypes.create_string_buffer(name, len(name))
        us = self.US(len(name), len(name), ctypes.cast(raw, ctypes.c_void_p))
        case_insensitive = 0x40
        oa = self.OA(ctypes.sizeof(self.OA), root, ctypes.pointer(us), case_insensitive, None, None)
        h = self.wt.HANDLE()
        io = self.IOSB()
        options = 0x1 | 0x20 | 0x4000 | (0x2000 if by_id else 0)  # directory, sync, backup, by id
        status = self.nt.NtOpenFile(ctypes.byref(h), 0x100001, ctypes.byref(oa), ctypes.byref(io), 7, options)
        return h.value if (status & 0xFFFFFFFF) < 0x80000000 else None

    def open_path(self, path: str) -> Any:
        return self.open(None, ("\\??\\" + path).encode("utf-16-le", "surrogatepass"))

    def open_id(self, parent: Any, fid: int, btrfs: bool) -> Any:
        name = struct.pack("<QQ", fid & 0xFF_FFFF_FFFF, fid >> 40) if btrfs else struct.pack("<Q", fid)
        return self.open(parent, name, by_id=True)

    def close(self, h: Any) -> None:
        self.nt.NtClose(self.wt.HANDLE(h))

    def file_id(self, h: Any) -> int | None:
        """FileInternalInformation: the 64-bit id FileIdBothDirectoryInformation lists."""
        io = self.IOSB()
        status = self.nt.NtQueryInformationFile(self.wt.HANDLE(h), ctypes.byref(io), self.buf, 8, 6)
        return None if status & 0x80000000 else int(struct.unpack_from("<Q", self.buf, 0)[0])

    def fs_name(self, h: Any) -> str:
        io = self.IOSB()
        self.nt.NtQueryVolumeInformationFile(self.wt.HANDLE(h), ctypes.byref(io), self.buf, len(self.buf), 5)
        (n,) = struct.unpack_from("<I", self.buf, 8)
        return self.buf.raw[12 : 12 + n].decode("utf-16-le")

    def listing(self, h: Any) -> list[tuple[str, int, int, int, int]] | None:
        """(name, attributes, end of file, file id, reparse tag) per entry, or None when it fails."""
        out: list[tuple[str, int, int, int, int]] = []
        first = True
        while True:
            io = self.IOSB()
            status = self.nt.NtQueryDirectoryFile(
                self.wt.HANDLE(h),
                None,
                None,
                None,
                ctypes.byref(io),
                self.buf,
                len(self.buf),
                37,
                False,
                None,
                first,
            )
            status &= 0xFFFFFFFF
            if status in (0x80000006, 0xC000000F):  # no more files, no such file
                return out
            if status >= 0x80000000:
                return None
            first = False
            off = 0
            while True:
                (nxt,) = struct.unpack_from("<I", self.buf, off)
                (eof,) = struct.unpack_from("<q", self.buf, off + 40)
                attrs, nlen, ea = struct.unpack_from("<III", self.buf, off + 56)
                (fid,) = struct.unpack_from("<Q", self.buf, off + 96)
                name = self.buf.raw[off + 104 : off + 104 + nlen].decode("utf-16-le", "surrogatepass")
                if name not in (".", ".."):
                    out.append((name, attrs, eof, fid, ea if attrs & 0x400 else 0))
                if not nxt:
                    break
                off += nxt


@pytest.mark.skipif(
    not (IS_WINDOWS and os.environ.get("SPACEKNIFE_TEST_CASE_ROOT")),
    reason="set SPACEKNIFE_TEST_CASE_ROOT to a real folder with case-only siblings (Windows, e.g. on WinBtrfs)",
)
def test_case_twins_of_a_real_tree_match_a_reference_that_opens_by_id() -> None:
    """The native walker on a real tree with case-only siblings (WinBtrfs: I:\\usr\\lib\\perl5, a
    flatpak runtime's share\\terminfo) lists each folder as a reference that never opens a folder
    by its name does: per folder the names, kinds and sizes, and no folder the reference does not
    have (apart from reparse points the reference does not enter). On Btrfs every folder the
    reference lists also matches the size its parent's listing gave it (2 bytes per byte of each
    name). A folder whose path reaches it (no name on the way shares its name with another entry)
    lists the same by id as by path: filters that would list folders opened by id in part, as on
    C:, would show here. A folder whose path reaches another entry, given as a task, is a listing
    error on Btrfs (the walker finds out the file system itself), and the entry it reaches is
    listed. No counts are fixed."""
    root = os.path.abspath(os.environ["SPACEKNIFE_TEST_CASE_ROOT"])
    nt = _NtWalk()
    top = nt.open_path(root)
    assert top is not None, root
    btrfs = nt.fs_name(top).lower() == "btrfs"
    ref: dict[str, list[list[tuple[str, int, int]]]] = {}
    entered: list[str] = []  # reparse points the reference does not enter (the native walker may)
    losers: list[tuple[str, str]] = []  # (a folder whose path reaches another entry, that entry)
    problems: list[str] = []

    def kind_of(attrs: int, tag: int) -> int:
        if attrs & 0x400 and tag & 0x20000000:
            return worker.KIND_LINK
        return worker.KIND_DIR if attrs & 0x10 else worker.KIND_FILE

    def walk(path: str, h: Any, listed_size: int | None, by_path: bool) -> None:
        got = nt.listing(h)
        if got is None:
            problems.append(f"{path}: cannot be listed")
            return
        kinds = [kind_of(a, t) for _n, a, _eof, _fid, t in got]
        rows = sorted((e[0], k, 0 if k == worker.KIND_DIR else e[2]) for e, k in zip(got, kinds, strict=True))
        ref.setdefault(path, []).append(rows)
        names = [n for n, *_ in got]
        measurable = btrfs and listed_size is not None and not any("\ufffd" in n for n in names)
        if measurable and 2 * sum(len(n.encode("utf-8", "surrogatepass")) for n in names) != listed_size:
            problems.append(f"{path}: listed size {listed_size} is not that of its names")
        folded: dict[str, int] = {}
        for n in names:
            folded[n.upper()] = folded.get(n.upper(), 0) + 1
        ids = {n: fid for n, _a, _eof, fid, _t in got}
        for name, attrs, eof, fid, tag in got:
            child = os.path.join(path, name)
            if attrs & 0x10 and attrs & 0x400 and not tag & 0x20000000:
                entered.append(child)
            if not attrs & 0x10 or attrs & 0x400:
                continue  # reparse points are not entered here
            ch = nt.open_id(h, fid, btrfs)
            if ch is None:
                problems.append(f"{child}: cannot be opened by its file id")
                continue
            # a path reaches this folder only through names no other entry shares
            reached = by_path and folded[name.upper()] == 1 and "\\" not in name and "/" not in name
            try:
                if reached:
                    named = nt.open_path(child)
                    if named is not None:
                        a, b = nt.listing(named), nt.listing(ch)
                        nt.close(named)
                        if (
                            a is not None
                            and b is not None
                            and sorted(e[0] for e in a) != sorted(e[0] for e in b)
                        ):
                            problems.append(f"{child}: lists other names by id than by path")
                elif by_path and folded[name.upper()] > 1:
                    named = nt.open_path(child)
                    other = nt.file_id(named) if named is not None else None
                    if named is not None:
                        nt.close(named)
                    winner = next((n for n, i in ids.items() if i == other and n != name), None)
                    if winner is not None:
                        losers.append((child, os.path.join(path, winner)))
                walk(child, ch, eof, reached)
            finally:
                nt.close(ch)

    try:
        walk(root, top, None, True)
    finally:
        nt.close(top)
    assert not problems, problems[:10]

    res = native.scan_slice([((0, 1, 0), root, 0)], {"roots": [{"path": root}], **EVERYTHING})
    nat: dict[str, list[list[tuple[str, int, int]]]] = {}
    names = _names(SliceResult(**res))
    bounds = res["bounds"].tolist()
    for i, p in enumerate(res["paths"]):
        lo, hi = bounds[i], bounds[i + 1]
        rows = sorted(
            (names[k], int(res["cols"]["kind"][k]), int(res["cols"]["size"][k])) for k in range(lo, hi)
        )
        nat.setdefault(p, []).append(rows)
    assert not res["errors"], res["errors"][:10]
    differ = [p for p, rows in ref.items() if sorted(nat.get(p, [])) != sorted(rows)]
    assert not differ, f"{len(differ)} of {len(ref)} folders differ from the reference, e.g. {differ[:8]}"
    extra = [p for p in nat if p not in ref and not any(p == q or p.startswith(q + "\\") for q in entered)]
    assert not extra, f"{len(extra)} folders only in the native listing, e.g. {extra[:8]}"

    # as tasks (the frontier, a monitor's relist, a rescan): a loser is an error, its winner listed
    if btrfs and losers:
        tasks: list[Task] = []
        for i, (loser, winner) in enumerate(losers[:20]):
            tasks += [((0, 2 * i + 1, 0), loser, 0), ((0, 2 * i + 2, 0), winner, 0)]
        res = native.scan_slice(tasks, {"roots": [{"path": root}], **EVERYTHING})
        errors = {path: msg for _ref, path, msg in res["errors"] if _ref[0] == 0}
        assert all(errors.get(loser) == worker.CASE_ERROR for loser, _w in losers[:20]), errors
        assert not {w for _l, w in losers[:20]} & errors.keys(), errors


# ---------------------------------------------------------------------------- POSIX specifics


@pytest.mark.skipif(IS_WINDOWS, reason="POSIX names are bytes")
def test_posix_non_utf8_names_fifo_and_hardlinks(tmp_path: Path) -> None:
    root = tmp_path / "posix"
    root.mkdir()
    broot = os.fsencode(root)
    for raw in [b"bad\xff.TXT", b"ext.\xc3\x89\xff", b"\xe4\xb8.\xe4\xb8\xad", b"latin1-\xe9.DAT"]:
        try:
            fd = os.open(os.path.join(broot, raw), os.O_CREAT | os.O_WRONLY, 0o644)
        except OSError as exc:
            if exc.errno == errno.EILSEQ:  # APFS (macOS) only stores valid UTF-8 names
                pytest.skip("this file system rejects names that are not UTF-8")
            raise
        os.write(fd, b"data")
        os.close(fd)
    os.mkdir(os.path.join(broot, b"dir\xfe"))
    (root / "orig.bin").write_bytes(b"h" * 5000)
    os.link(root / "orig.bin", root / "link.bin")
    if sys.platform != "win32":
        os.mkfifo(root / "pipe")
    res = scan_one(root)
    assert any("\udcfe" in p for p in res.paths)


@pytest.mark.skipif(IS_WINDOWS, reason="POSIX PATH_MAX")
def test_posix_folders_past_path_max_are_listed(tmp_path: Path) -> None:
    """A folder whose path is PATH_MAX bytes or longer exists and can be listed, but open() of
    its path fails (ENAMETOOLONG): both walkers used to report it as a listing error and leave out
    everything below it. Now they open such a path a piece at a time. Checked for folders found
    during a call and for a task with such a path (a folder handed back to Python and listed by a
    later call)."""
    root = tmp_path / "long"
    root.mkdir()
    name = b"d" * 200
    path = os.fsencode(root)
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
    try:
        while len(path) <= 5000:  # PATH_MAX: 4,096 on Linux, 1,024 on macOS
            os.mkdir(name, dir_fd=fd)
            below = os.open(name, os.O_RDONLY | os.O_DIRECTORY, dir_fd=fd)
            os.close(fd)
            fd = below
            path += b"/" + name
            out = os.open(b"f.txt", os.O_CREAT | os.O_WRONLY, 0o644, dir_fd=fd)
            os.write(out, b"x" * len(path))
            os.close(out)
    finally:
        os.close(fd)
    deepest = os.fsdecode(path)
    with pytest.raises(OSError, match=os.strerror(errno.ENAMETOOLONG)):
        os.scandir(deepest)
    roots = (root_opts(root),)
    res = scan_one(root)
    assert not res.errors
    assert res.paths[-1] == deepest
    assert len(res.paths) == 1 + path.count(b"/" + name)
    assert int(res.cols["size"].sum()) == sum(range(len(os.fsencode(root)) + 201, len(path) + 1, 201))
    res = compare([((0, 1, 0), deepest, 0)], roots)
    assert not res.errors
    assert res.paths == [deepest]
    assert _names(res) == ["f.txt"]


@pytest.fixture
def restore_modes() -> Iterator[list[Path]]:
    paths: list[Path] = []
    yield paths
    for p in paths:
        os.chmod(p, 0o755)


@pytest.mark.skipif(IS_WINDOWS or (hasattr(os, "geteuid") and os.geteuid() == 0), reason="POSIX permissions")
def test_posix_denied_and_unsearchable_directories(tmp_path: Path, restore_modes: list[Path]) -> None:
    root = tmp_path / "perm"
    denied = root / "denied"
    blind = root / "blind"  # readable, not searchable: names but no stat
    for d in (denied, blind):
        d.mkdir(parents=True)
        (d / "f.txt").write_bytes(b"f")
    (blind / "sub").mkdir()
    os.chmod(denied, 0)
    os.chmod(blind, stat.S_IRUSR)
    restore_modes.extend([denied, blind])
    res = scan_one(root)
    assert [e[1] for e in res.errors] == [str(denied)]
    assert res.errors[0][2] == "Permission denied"
    blind_listing = res.paths.index(str(blind))
    lo, hi = res.bounds[blind_listing], res.bounds[blind_listing + 1]
    assert (res.cols["flags"][lo:hi] & Flag.ERROR).all()


@pytest.mark.skipif(IS_WINDOWS, reason="POSIX devices")
def test_posix_mount_boundaries(tree: Path) -> None:
    real = os.stat(tree).st_dev
    res = scan_one(tree, dev=real + 1)  # every subdirectory looks like another device
    assert res.paths == [str(tree)]
    assert compare([((0, 1, 0), str(tree), 0)], (RootOptions(dev=real + 1, cross_mounts=True),)).paths[1:]


@pytest.mark.skipif(IS_WINDOWS, reason="POSIX walkers")
def test_posix_skipped_folders_are_not_entered(tree: Path) -> None:
    """RootOptions.skip (macOS: the Data volume below "/", already there through the firmlinks):
    the folder is listed as a mount point and not entered, by both walkers alike."""
    subs = sorted(p for p in tree.iterdir() if p.is_dir() and not p.is_symlink())
    skipped, kept = subs[0], subs[1]
    res = scan_one(tree, skip=(str(skipped),))
    assert str(skipped) not in res.paths and any(p.startswith(str(kept)) for p in res.paths)
    assert not any(p.startswith(str(skipped) + "/") for p in res.paths)
    lo, hi = int(res.bounds[0]), int(res.bounds[1])
    off = 0
    flags_by_name = {}
    for i in range(lo, hi):
        n = int(res.name_lens[i])
        flags_by_name[res.name_blob[off : off + n]] = int(res.cols["flags"][i])
        off += n
    assert flags_by_name[os.fsencode(skipped.name)] & Flag.MOUNT
    assert not flags_by_name[os.fsencode(kept.name)] & Flag.MOUNT


@pytest.mark.skipif(IS_WINDOWS, reason="POSIX walkers")
def test_posix_protected_folders_are_not_entered(tree: Path) -> None:
    """RootOptions.protect (macOS without Full Disk Access, by choice): the folder is listed as not
    accessible and not entered, so it asks for nothing; both walkers alike."""
    subs = sorted(p for p in tree.iterdir() if p.is_dir() and not p.is_symlink())
    guarded = subs[0]
    res = scan_one(tree, protect=(str(guarded),))
    assert not any(p == str(guarded) or p.startswith(str(guarded) + "/") for p in res.paths)
    lo, hi, off = int(res.bounds[0]), int(res.bounds[1]), 0
    flags_by_name = {}
    for i in range(lo, hi):
        n = int(res.name_lens[i])
        flags_by_name[res.name_blob[off : off + n]] = int(res.cols["flags"][i])
        off += n
    assert flags_by_name[os.fsencode(guarded.name)] & Flag.ACCESS_DENIED


# ---------------------------------------------------------------------------- slicing


def check_slice(res: dict[str, Any], tasks: list[Task]) -> None:
    refs, bounds, kind = res["refs"], res["bounds"], res["cols"]["kind"]
    listings = len(res["paths"])
    assert refs.shape == (listings, 3) and len(bounds) == listings + 1
    for listing, (k, a, b) in enumerate(refs.tolist()):
        if k == 1:
            assert a < listing and kind[bounds[a] + b] == 1
    for ref, _path, _idx in res["frontier"]:
        if ref[0] == 1:
            assert ref[1] < listings and kind[bounds[ref[1]] + ref[2]] == 1
    for ref, _path, _msg in res["errors"]:
        if ref[0] == 1:
            assert ref[1] < listings
    if tasks:
        assert listings >= 1 or len(res["errors"]) == len(tasks)


def test_zero_budget_still_lists_and_returns_a_valid_frontier(tree: Path) -> None:
    tasks: list[Task] = [((0, 1, 0), str(tree), 0)]
    res = native.scan_slice(tasks, {"roots": [{"dev": os.stat(tree).st_dev}], "budget_s": 0.0})
    check_slice(res, tasks)
    assert res["frontier"]
    res = native.scan_slice(tasks, {"roots": [{"dev": os.stat(tree).st_dev}], "max_entries": 1, "threads": 1})
    check_slice(res, tasks)
    assert len(res["paths"]) == 1 and res["frontier"]


def tree_snapshot(store: NodeStore, *, listed_only: bool = False) -> dict[str, tuple[object, ...]]:
    """Path -> node facts; store ids and extension ids depend on slice order, so they are left out.
    `listed_only`: what each entry's listing said, without which name of a hard-linked file was
    counted (the first one folded in, so it follows the walk's order) and the totals that follow."""
    c = store.cols
    paths: dict[int, str] = {}
    out: dict[str, tuple[object, ...]] = {}
    keys = ("kind", "flags", "size", "alloc", "mtime") + (
        () if listed_only else ("tot_size", "tot_files", "tot_dirs")
    )
    not_dup = ~np.uint32(Flag.HARDLINK_DUP) if listed_only else ~np.uint32(0)
    for i in range(1, len(store)):
        parent = int(c["parent"][i])
        path = (paths[parent] + "/" if parent in paths else "") + store.name(i)
        paths[i] = path
        facts = tuple(int(c[k][i] & not_dup) if k == "flags" else int(c[k][i]) for k in keys)
        out[path] = (*facts, store.ext_table[int(c["ext"][i])])
    return out


def scan_store(root: Path) -> NodeStore:
    scanner = Scanner([str(root)], budget_s=0.0)  # one directory per slice: maximal slicing
    result = asyncio.run(scanner.run())
    assert result.state == ScanState.DONE
    return scanner.store


def test_sliced_native_scan_builds_the_same_tree(tree: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    py_store = scan_store(tree)

    def adapter(tasks: list[Task], opts: SliceOptions) -> SliceResult:
        return SliceResult(**native.scan_slice(tasks, opts))  # a SliceOptions works as opts too

    monkeypatch.setattr(worker, "scan_slice", adapter)
    nat_store = scan_store(tree)
    assert len(nat_store) == len(py_store)
    assert tree_snapshot(nat_store) == tree_snapshot(py_store)


def test_result_arrays_are_writeable(tree: Path) -> None:
    res = native.scan_slice([((0, 1, 0), str(tree), 0)], {"roots": [{}], **EVERYTHING})
    res["cols"]["flags"][0] |= np.uint32(Flag.HARDLINK_DUP)
    assert res["cols"]["flags"][0] & Flag.HARDLINK_DUP


# ---------------------------------------------------------------------------- options


def test_options_are_validated(tree: Path) -> None:
    tasks: list[Task] = [((0, 1, 0), str(tree), 0)]
    with pytest.raises(ValueError, match="unknown"):
        native.scan_slice(tasks, {"roots": [{}], "budget": 1})
    with pytest.raises(ValueError, match="unknown"):
        native.scan_slice(tasks, {"roots": [{"cluster": 1}]})
    with pytest.raises(ValueError, match="out of range"):
        native.scan_slice([((0, 1, 0), str(tree), 1)], {"roots": [{}]})
    with pytest.raises(ValueError, match="roots"):
        native.scan_slice(tasks, {})
    with pytest.raises(TypeError):
        native.scan_slice([((0, 1, 0), b"bytes", 0)], {"roots": [{}]})
    native.scan_slice(tasks, {"roots": [{}], "keep_access_time": None, "relative_open": None})  # known keys
    with pytest.raises(ValueError, match="unknown"):
        native.scan_slice(
            tasks, {"roots": [{}], "open_by_id": True}
        )  # tried and dropped (docs/native.md 2.2)


@pytest.mark.parametrize("budget_s", [0.0005, 0.005, 1.0])
def test_scanner_driven_native_scans_build_the_same_tree(tree: Path, budget_s: float) -> None:
    """The native walker as the app drives it (many tasks per call, calls cut short by their budget,
    the rest handed back and listed by later calls on other threads) builds exactly the tree the
    Python walker builds, whatever the budget: how a scan is cut into calls must never change what
    it finds. (Opening folders by file id relative to a fixed anchor folder once listed some of
    them only in part, in calls that went deep enough: a third of C: went missing without an
    error.)"""
    py_store = scan_store(tree)
    slicer = native_slicer()
    assert slicer is not None
    pool = ThreadPoolExecutor(max_workers=4)
    try:
        scanner = Scanner(
            [str(tree)],
            executor=pool,
            concurrency=4,
            slicer=slicer,
            batch_tasks=NATIVE_BATCH,
            budget_s=budget_s,
        )
        result = asyncio.run(scanner.run())
    finally:
        pool.shutdown()
    assert result.state == ScanState.DONE
    assert len(scanner.store) == len(py_store)
    assert tree_snapshot(scanner.store) == tree_snapshot(py_store)


@pytest.mark.skipif(
    not os.environ.get("SPACEKNIFE_TEST_SCAN_ROOT"),
    reason="set SPACEKNIFE_TEST_SCAN_ROOT to a real folder or drive (minutes, a few GB of memory per million items)",
)
def test_native_scan_of_a_real_tree_matches_the_python_walker(monkeypatch: pytest.MonkeyPatch) -> None:
    """On a real folder or drive (file system filters, ACLs, odd names, files changing meanwhile), the
    native walker as the app drives it finds what the Python walker finds. The Python walker scans
    before and after, and only a path both of its scans agree on counts: a difference there is the
    walker's, not a file that changed. This is how the partial listings of opens by file id showed
    up (a temporary test tree has none of the filters that caused them). Which name of a hard-linked
    file is counted follows the order of the walk, which differs between the two walkers, so that
    and the totals are left out (on Linux, uv's cache and environments share thousands of files).
    Folders the Python walker cannot reach by their name (case-only siblings on WinBtrfs: CASE, DUP,
    BADNAME errors) are opened by file id natively, so they and everything below them are left out
    here; SPACEKNIFE_TEST_CASE_ROOT checks those against a reference that opens by id."""
    from spaceknife.scan import scanner as scanner_mod
    from spaceknife.scan.scanner import NATIVE_BUDGET_S, default_workers, make_pool, native_threads

    root = os.environ["SPACEKNIFE_TEST_SCAN_ROOT"]
    monkeypatch.setattr(scanner_mod, "MAX_RECORDED_DENIED", 10**9)  # every error and its reason
    unreachable: set[str] = set()  # tree_snapshot's keys of the Python walker's no-name folders
    no_name = {worker.CASE_ERROR, worker.DUP_ERROR, worker.BADNAME_ERROR}

    def snapshot_key(scanner: Scanner, path: str) -> str:
        for r in scanner.roots:
            top = r.path.rstrip("\\/")
            if path.startswith(top + os.sep):
                return r.path + "/" + path[len(top) + 1 :].replace(os.sep, "/")
        return path

    def python_scan() -> dict[str, tuple[object, ...]]:
        pool = make_pool()
        try:
            scanner = Scanner([root], executor=pool, concurrency=default_workers())
            asyncio.run(scanner.run())
        finally:
            pool.shutdown()
        for path, why in zip(scanner.progress().denied, scanner.denied_reasons, strict=False):
            if why in no_name:
                unreachable.add(snapshot_key(scanner, path))
        return tree_snapshot(scanner.store, listed_only=True)

    def native_scan() -> dict[str, tuple[object, ...]]:
        threads = native_threads()
        pool = ThreadPoolExecutor(max_workers=threads)
        try:
            scanner = Scanner(
                [root],
                executor=pool,
                concurrency=threads,
                slicer=native_slicer(),
                batch_tasks=NATIVE_BATCH,
                budget_s=NATIVE_BUDGET_S,
            )
            asyncio.run(scanner.run())
        finally:
            pool.shutdown()
        return tree_snapshot(scanner.store, listed_only=True)

    before = python_scan()
    nat = native_scan()
    after = python_scan()
    stable = {p for p in before.keys() & after.keys() if before[p] == after[p]}
    differ = {p for p in stable if nat.get(p) != before[p]}
    # in the native scan only: a file that came and went meanwhile, unless its folder held still
    extra = {p for p in nat.keys() - before.keys() - after.keys() if p.rpartition("/")[0] in stable}
    excused = {p for p in differ | extra if any(p == q or p.startswith(q + "/") for q in unreachable)}
    bad = sorted(
        p for p in differ | extra if p not in excused and not any(q.startswith(p + "/") for q in excused)
    )
    assert not bad, f"{len(bad)} paths differ from the Python walker, e.g. {bad[:8]}"


@pytest.mark.skipif(not (IS_WINDOWS or sys.platform == "linux"), reason="no way to keep access times")
def test_keeping_access_times_changes_nothing(tree: Path) -> None:
    """Opening folders so that they keep their access times lists the same tree."""
    tasks: list[Task] = [((0, 1, 0), str(tree), 0)]
    roots = (root_opts(tree),)
    compare(tasks, roots, keep_access_time=False)
    compare(tasks, roots, keep_access_time=True)
    compare(tasks, roots, threads=1)


@pytest.mark.skipif(not (IS_WINDOWS or sys.platform == "linux"), reason="no way to keep access times")
def test_listing_keeps_folder_access_times(tree: Path) -> None:
    """A scan does not update the last access time of the folders it lists. On NTFS with access
    time updates on, it rewrote every folder's record (about 2 GB of writes for a system volume);
    under Linux's relatime, the first scan of a day rewrites every folder's inode (O_NOATIME, for
    the folders' owner: the test's own folders). With the option off they do change, unless the
    file system keeps no access times at all."""
    old = 978_307_200  # 2001-01-01
    folders = [Path(d) for d, _dirs, _files in os.walk(tree)]

    def age() -> None:
        for d in reversed(folders):  # children first: setting a folder's times touches its parent
            os.utime(d, (old, os.stat(d).st_mtime))
        assert all(abs(os.stat(d).st_atime - old) <= 2 for d in folders)

    def scan(**extra: Any) -> list[Path]:
        opts = nopts(SliceOptions(roots=(root_opts(tree),), **EVERYTHING), **extra)
        res = native.scan_slice([((0, 1, 0), str(tree), 0)], opts)
        assert len(res["paths"]) == len(folders)
        return [d for d in folders if abs(os.stat(d).st_atime - old) > 2]

    age()
    changed = scan()
    assert not changed, f"{len(changed)} folders got a new access time, e.g. {changed[0]}"
    age()
    if not scan(keep_access_time=False):
        pytest.skip("listing does not update access times here (noatime or similar)")


@pytest.mark.skipif(not IS_WINDOWS, reason="ReadDirectoryChangesW")
def test_windows_listing_raises_no_change_notifications(tree: Path) -> None:
    """Listing raises nothing the live monitor listens for (monitor.win.NOTIFY_FILTER, which has no
    access times): opening folders for FILE_WRITE_ATTRIBUTES must not look like a change."""
    from ctypes import wintypes

    from spaceknife.monitor.win import NOTIFY_FILTER

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.CreateFileW.restype = wintypes.HANDLE
    handle = k32.CreateFileW(
        str(tree), 0x1, 0x7, None, 3, 0x02000000, None
    )  # FILE_LIST_DIRECTORY, backup semantics
    assert handle not in (None, wintypes.HANDLE(-1).value)
    seen: list[str] = []

    def watch() -> None:
        buf = ctypes.create_string_buffer(65536)
        got = wintypes.DWORD()
        while k32.ReadDirectoryChangesW(
            handle, buf, len(buf), True, NOTIFY_FILTER, ctypes.byref(got), None, None
        ):
            off = 0
            while got.value:
                nxt = int.from_bytes(buf.raw[off : off + 4], "little")
                size = int.from_bytes(buf.raw[off + 8 : off + 12], "little")
                seen.append(buf.raw[off + 12 : off + 12 + size].decode("utf-16-le"))
                if not nxt:
                    break
                off += nxt
            if "sentinel.txt" in seen:
                return

    import threading

    watcher = threading.Thread(target=watch, daemon=True)
    watcher.start()
    try:
        native.scan_slice(
            [((0, 1, 0), str(tree), 0)], nopts(SliceOptions(roots=(root_opts(tree),), **EVERYTHING))
        )
        (tree / "sentinel.txt").write_bytes(b"s")  # something the watcher must see: the end marker
        watcher.join(10)
    finally:
        k32.CloseHandle(handle)
    assert seen and seen[-1] == "sentinel.txt"
    assert seen == ["sentinel.txt"] * len(seen), (
        f"the scan raised {[s for s in seen if s != 'sentinel.txt'][:5]}"
    )


@pytest.mark.skipif(sys.platform != "darwin", reason="getattrlistbulk is macOS")
def test_macos_lists_with_getattrlistbulk(tree: Path) -> None:
    """The bulk path is used, not silently turned off: its first batch is checked against fstatat
    and the whole tree must equal the Python walker's (scan_one compares them)."""
    scan_one(tree)
    state = native.bulk_listing()
    assert state == "on", state  # files; folders through fstatat


@pytest.mark.skipif(sys.platform != "darwin", reason="getattrlistbulk is macOS")
def test_macos_folders_from_getattrlistbulk_on_request(tree: Path) -> None:
    """SPACEKNIFE_BULK_DIRS=1: folders come from getattrlistbulk too, and the tree is still the
    Python walker's (the state is per process: a fresh one)."""
    code = "; ".join(
        [
            "import dataclasses, os, sys",
            "import numpy as np",
            "import spaceknife_native as n",
            "from spaceknife.scan import worker",
            "root = sys.argv[1]",
            "ro = worker.RootOptions(dev=os.stat(root).st_dev)",
            "opts = worker.SliceOptions(roots=(ro,), budget_s=1e9, max_entries=10**12)",
            "tasks = [((0, 1, 0), root, 0)]",
            "worker.scan_slice(tasks, opts)",  # a first read can still bump access times
            "w = worker.scan_slice(tasks, opts)",
            "d = {'roots': [dataclasses.asdict(ro)], 'budget_s': 1e9, 'max_entries': 10**12}",
            "r = n.scan_slice(tasks, d)",
            "bad = [k for k in w.cols if not np.array_equal(w.cols[k], r['cols'][k])]",
            "print(n.bulk_listing(), bad, r['name_blob'] == w.name_blob)",
        ]
    )
    env = {**os.environ, "SPACEKNIFE_BULK_DIRS": "1"}
    out = subprocess.run([sys.executable, "-c", code, str(tree)], capture_output=True, text=True, env=env)
    assert out.returncode == 0, out.stderr
    assert out.stdout.strip() == "on (folders too) [] True"


def test_bulk_listing_reports_a_state() -> None:
    state = native.bulk_listing()
    if sys.platform == "darwin":
        assert state == "untested" or state.startswith(("on", "off")), state
    else:
        assert state == "unsupported"


# ---------------------------------------------------------------------------- LinkSet


def _python_mark(seen: set[tuple[int, int]], cols: dict[str, np.ndarray]) -> None:
    """Scanner._fold's loop without the extension (the reference)."""
    flags = cols["flags"]
    for i in np.nonzero((cols["nlink"] > 1) & (cols["kind"] == 0))[0].tolist():
        key = (int(cols["dev"][i]), int(cols["ino"][i]))
        if key in seen:
            flags[i] |= np.uint32(Flag.HARDLINK_DUP)
        else:
            seen.add(key)


def test_link_set_marks_what_the_python_loop_marks() -> None:
    """Over several slices (repeats within one and across them, several devices, folders and
    single-link files with the same inode): the same names get HARDLINK_DUP, in place."""
    rng = np.random.default_rng(7)
    native_set, python_set = native.LinkSet(), set()
    for _ in range(5):
        n = 2000
        cols = {
            "dev": rng.integers(1, 3, n, dtype=np.uint64),
            "ino": rng.integers(1, 400, n, dtype=np.uint64),
            "nlink": rng.integers(1, 4, n, dtype=np.uint16),
            "kind": rng.choice(np.array([0, 0, 0, 1], dtype=np.uint8), n),
            "flags": rng.integers(0, 2, n, dtype=np.uint32) << np.uint32(5),  # other bits stay
        }
        ref = {k: v.copy() for k, v in cols.items()}
        marked = native_set.mark(cols["dev"], cols["ino"], cols["nlink"], cols["kind"], cols["flags"])
        _python_mark(python_set, ref)
        assert np.array_equal(cols["flags"], ref["flags"])
        assert marked == int(np.count_nonzero(ref["flags"] & np.uint32(Flag.HARDLINK_DUP)))
        assert len(native_set) == len(python_set)
    assert all(key in native_set for key in python_set)


def test_link_set_refuses_mismatched_columns() -> None:
    ok = {
        "dev": np.zeros(3, np.uint64),
        "ino": np.arange(3, dtype=np.uint64),
        "nlink": np.full(3, 2, np.uint16),
        "kind": np.zeros(3, np.uint8),
    }
    links = native.LinkSet()
    with pytest.raises(ValueError, match="length"):
        links.mark(ok["dev"], ok["ino"], ok["nlink"], ok["kind"], np.zeros(2, np.uint32))
    with pytest.raises(ValueError, match="ino"):
        links.mark(
            ok["dev"], np.arange(6, dtype=np.uint64)[::2], ok["nlink"], ok["kind"], np.zeros(3, np.uint32)
        )
    with pytest.raises(TypeError):
        links.mark(ok["dev"], ok["ino"], ok["nlink"], ok["kind"], np.zeros(3, np.int64))
