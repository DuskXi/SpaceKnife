"""ops.locks: who holds a file open 鈥?a real handle held by this test, plus the parsers."""

from __future__ import annotations

import os
import subprocess
import sys
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

import pytest

from spaceknife.ops import locks
from spaceknife.ops.locks import find_lockers


@contextmanager
def held_open(path: Path) -> Iterator[None]:
    """Keep ``path`` open the way an editor would (Windows: without FILE_SHARE_DELETE)."""
    if sys.platform == "win32":
        import ctypes
        from ctypes import wintypes

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        k32.CreateFileW.restype = wintypes.HANDLE
        k32.CreateFileW.argtypes = [
            wintypes.LPCWSTR,
            wintypes.DWORD,
            wintypes.DWORD,
            ctypes.c_void_p,
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.HANDLE,
        ]
        k32.CloseHandle.argtypes = [wintypes.HANDLE]
        # GENERIC_READ, FILE_SHARE_READ only, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL
        handle = k32.CreateFileW(str(path), 0x80000000, 0x1, None, 3, 0x80, None)
        assert handle and handle != wintypes.HANDLE(-1).value
        try:
            yield
        finally:
            k32.CloseHandle(handle)
    else:
        fh = open(path, "rb")  # noqa: SIM115 - held across the yield on purpose
        try:
            yield
        finally:
            fh.close()


def own_names() -> set[str]:
    """Names this process may be reported under."""
    names = {"Python", "python.exe", "pythonw.exe", os.path.basename(sys.executable)}
    if sys.platform != "win32":
        names.add(os.path.basename(os.path.realpath(sys.executable)))
        try:
            with open("/proc/self/comm", encoding="utf-8") as fh:
                names.add(fh.read().strip())
        except OSError:
            pass
    return names


@pytest.mark.skipif(sys.platform == "darwin", reason="lsof based on macOS: not tested here")
def test_a_file_held_by_this_test(tmp_path: Path) -> None:
    folder = tmp_path / "work dir"
    folder.mkdir()
    held = folder / "鎶ュ憡.docx"
    held.write_bytes(b"x")
    free = tmp_path / "free.txt"
    free.write_bytes(b"y")
    with held_open(held):
        got = find_lockers([str(held), str(free), str(folder), str(tmp_path / "missing")])
        assert set(got) == {str(held), str(folder)}  # the folder: anything inside it
        assert got[str(held)] and set(got[str(held)]) & own_names(), got
    assert find_lockers([str(held), str(folder)]) == {}


def test_nothing_asked_nothing_found(tmp_path: Path) -> None:
    assert find_lockers([]) == {}
    assert find_lockers([str(tmp_path / "nope")]) == {}


def test_detection_failure_is_not_fatal(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    def broken(paths: list[str]) -> dict[str, list[str]]:
        raise RuntimeError("boom")

    name = "_windows" if sys.platform == "win32" else "_darwin" if sys.platform == "darwin" else "_linux"
    monkeypatch.setattr(locks, name, broken)
    assert find_lockers([str(tmp_path)]) == {}


@pytest.mark.skipif(sys.platform == "win32", reason="symlinks in a fake /proc need POSIX")
def test_proc_scan_matches_files_and_folders(tmp_path: Path) -> None:
    proc = tmp_path / "proc"
    target_dir = tmp_path / "data"
    (target_dir / "sub").mkdir(parents=True)
    file_a = target_dir / "sub" / "a.bin"
    file_a.write_bytes(b"")
    for pid, comm, targets in [
        ("101", "editor", [str(file_a), "socket:[1234]", "pipe:[9]"]),
        ("202", "indexer", [str(target_dir / "other.txt") + " (deleted)", "/dev/null"]),
        ("303", "viewer", [str(file_a)]),
    ]:
        fd = proc / pid / "fd"
        fd.mkdir(parents=True)
        (proc / pid / "comm").write_text(comm + "\n")
        for i, t in enumerate(targets):
            os.symlink(t, fd / str(i + 3))
    (proc / "404").mkdir()  # a process whose fds we may not read: skipped
    (proc / "self").mkdir()  # not a pid
    got = locks._linux([str(file_a), str(target_dir), str(tmp_path / "unrelated")], proc=str(proc))
    assert got == {str(file_a): ["editor", "viewer"], str(target_dir): ["editor", "viewer"]}


@pytest.mark.skipif(sys.platform == "win32", reason="lsof reports POSIX paths")
def test_lsof_output_is_parsed(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    folder = tmp_path / "proj"
    folder.mkdir()
    inside = folder / "x.psd"
    out = f"p12\ncPhotoshop\nn{inside}\np34\ncFinder\nn/somewhere/else\n"

    def fake_run(args: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
        assert "+D" in args and "-Fpcn" in args
        return subprocess.CompletedProcess(args, 0, stdout=out, stderr="")

    monkeypatch.setattr(locks.shutil, "which", lambda name: "/usr/sbin/lsof")
    monkeypatch.setattr(locks.subprocess, "run", fake_run)
    monkeypatch.setattr(locks, "_resolved", lambda p: p)
    assert locks._darwin([str(folder)]) == {str(folder): ["Photoshop"]}
