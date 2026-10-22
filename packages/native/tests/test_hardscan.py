"""spaceknife_native.scan_slice must equal spaceknife.scan.worker.scan_slice, column by column.

Untruncated slices (huge budget) are compared field by field on varied trees; truncated slices
(budget 0, tiny max_entries) are checked for valid references and for reassembling into the
same tree as the Python worker.
"""

from __future__ import annotations

import asyncio
import ctypes
import dataclasses
import errno
import os
import stat
import subprocess
import sys
from collections.abc import Iterator
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
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


def root_opts(path: Path, **kw: Any) -> RootOptions:
    kw.setdefault("dev", os.stat(path).st_dev)
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
    """Documented difference: the worker's Win32 paths drop a trailing dot; NT paths keep it."""
    root = tmp_path / "dots"
    root.mkdir()
    os.mkdir("\\\\?\\" + str(root / "odd."))
    with open("\\\\?\\" + str(root / "odd.") + "\\inside.txt", "wb") as f:
        f.write(b"i")
    res = native.scan_slice(
        [((0, 1, 0), str(root), 0)], nopts(SliceOptions(roots=(RootOptions(),), **EVERYTHING))
    )
    assert res["paths"] == [str(root), str(root / "odd.")]
    assert not res["errors"] and len(res["name_lens"]) == 2


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
    deepest folders past the limit, and next to it. (The worker cannot take part: os.scandir raises
    ValueError for such a path.)"""
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


def tree_snapshot(store: NodeStore) -> dict[str, tuple[object, ...]]:
    """Path -> node facts; store ids and extension ids depend on slice order, so they are left out."""
    c = store.cols
    paths: dict[int, str] = {}
    out: dict[str, tuple[object, ...]] = {}
    for i in range(1, len(store)):
        parent = int(c["parent"][i])
        path = (paths[parent] + "/" if parent in paths else "") + store.name(i)
        paths[i] = path
        facts = tuple(
            int(c[k][i])
            for k in ("kind", "flags", "size", "alloc", "mtime", "tot_size", "tot_files", "tot_dirs")
        )
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
def test_native_scan_of_a_real_tree_matches_the_python_walker() -> None:
    """On a real folder or drive (file system filters, ACLs, odd names, files changing meanwhile), the
    native walker as the app drives it finds what the Python walker finds. The Python walker scans
    before and after, and only a path both of its scans agree on counts: a difference there is the
    walker's, not a file that changed. This is how the partial listings of opens by file id showed
    up (a temporary test tree has none of the filters that caused them)."""
    from spaceknife.scan.scanner import NATIVE_BUDGET_S, default_workers, make_pool, native_threads

    root = os.environ["SPACEKNIFE_TEST_SCAN_ROOT"]

    def python_scan() -> dict[str, tuple[object, ...]]:
        pool = make_pool()
        try:
            scanner = Scanner([root], executor=pool, concurrency=default_workers())
            asyncio.run(scanner.run())
        finally:
            pool.shutdown()
        return tree_snapshot(scanner.store)

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
        return tree_snapshot(scanner.store)

    def win32_only(path: str) -> bool:  # names Win32 rewrites: the worker cannot open them (on purpose)
        return IS_WINDOWS and any(part.endswith((".", " ")) for part in path.replace("\\", "/").split("/"))

    before = python_scan()
    nat = native_scan()
    after = python_scan()
    stable = {p for p in before.keys() & after.keys() if before[p] == after[p]}
    differ = {p for p in stable if nat.get(p) != before[p]}
    # in the native scan only: a file that came and went meanwhile, unless its folder held still
    extra = {p for p in nat.keys() - before.keys() - after.keys() if p.rpartition("/")[0] in stable}
    excused = {p for p in differ | extra if win32_only(p)}
    bad = sorted(
        p for p in differ | extra if p not in excused and not any(q.startswith(p + "/") for q in excused)
    )
    assert not bad, f"{len(bad)} paths differ from the Python walker, e.g. {bad[:8]}"


@pytest.mark.skipif(not IS_WINDOWS, reason="kept access times are Windows")
def test_windows_keeping_access_times_changes_nothing(tree: Path) -> None:
    """Opening folders so that they keep their access times lists the same tree."""
    tasks: list[Task] = [((0, 1, 0), str(tree), 0)]
    roots = (root_opts(tree),)
    compare(tasks, roots, keep_access_time=False)
    compare(tasks, roots, keep_access_time=True)
    compare(tasks, roots, threads=1)


@pytest.mark.skipif(not IS_WINDOWS, reason="NTFS last access times")
def test_windows_listing_keeps_folder_access_times(tree: Path) -> None:
    """A scan does not update the last access time of the folders it lists (on NTFS with access
    time updates on, it rewrote every folder's record: ~2 GB of writes for a system volume)."""
    old = 978_307_200  # 2001-01-01
    folders = [Path(d) for d, _dirs, _files in os.walk(tree)]
    for d in reversed(folders):  # children first: setting a folder's times touches its parent
        os.utime(d, (old, os.stat(d).st_mtime))
    assert all(abs(os.stat(d).st_atime - old) <= 2 for d in folders)
    res = native.scan_slice(
        [((0, 1, 0), str(tree), 0)], nopts(SliceOptions(roots=(root_opts(tree),), **EVERYTHING))
    )
    assert len(res["paths"]) == len(folders)
    changed = [d for d in folders if abs(os.stat(d).st_atime - old) > 2]
    assert not changed, f"{len(changed)} folders got a new access time, e.g. {changed[0]}"


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
