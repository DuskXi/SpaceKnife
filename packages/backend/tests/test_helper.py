"""Elevated scan helper (spaceknife.helper): framing, auth, lifecycle, bulk transfer.

Nothing here ever shows a UAC prompt: ``elevate=True`` is only exercised with ShellExecuteEx
mocked. Helpers run as plain subprocesses; when the test process itself has administrator
rights (Windows), they are started with a Safer "normal user" token so that they really are
unprivileged. MFT parsing is replaced by a synthetic ``spaceknife.scan.mft`` injected into the
helper process (``BOOT``), so no real volume is read.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import re
import signal
import stat
import subprocess
import sys
import time
from collections.abc import AsyncIterator, Sequence
from pathlib import Path
from typing import Any

import numpy as np
import pytest
from pydantic import ValidationError

import spaceknife.helper.client as client_mod
from spaceknife.helper import (
    HelperClient,
    HelperDenied,
    HelperError,
    HelperUnavailable,
    SharedHelper,
    is_elevated,
)
from spaceknife.helper.auth import new_token, read_token_file, write_token_file
from spaceknife.helper.client import concat_batches
from spaceknife.helper.launch import HelperProcess, elevated_params, helper_command
from spaceknife.helper.messages import (
    HEADER,
    KIND_BINARY,
    KIND_JSON,
    MAX_CONTROL_BYTES,
    REPLY,
    Error,
    FrameError,
    Ping,
    Request,
    Response,
    check_header,
    frame_header,
)
from spaceknife.helper.service import batch_bounds, frame_parts, rebase_names
from spaceknife.helper.transport import Listener, valid_address
from spaceknife_protocol.binary import decode_frame, encode_frame

IS_WINDOWS = sys.platform == "win32"

# --------------------------------------------------------------------------- synthetic MFT

FAKE_MFT = r"""
import time
import numpy as np


def synth(rows, shuffle=False):
    i = np.arange(rows, dtype=np.int64)
    name_len = (3 + i % 13).astype(np.uint16)
    name_off = np.zeros(rows, dtype=np.uint32)
    if rows:
        np.cumsum(name_len[:-1], out=name_off[1:], dtype=np.uint32)
    total = int(name_len.sum(dtype=np.int64))
    table = {
        "record": i.astype(np.uint64) + 16,
        "parent": (i // 5).astype(np.uint64) + 5,
        "name_off": name_off,
        "name_len": name_len,
        "kind": (i % 4 == 0).astype(np.uint8),
        "flags": ((i * 7) & 0xFFFF).astype(np.uint32),
        "size": i * 1000,
        "alloc": i * 1024,
        "ads_size": i % 3,
        "ads_alloc": (i % 3) * 8,
        "mtime": 1_700_000_000 + i,
        "ctime": 1_600_000_000 + i,
        "atime": 1_750_000_000 - i,
        "nlink": (1 + i % 2).astype(np.uint16),
    }
    if shuffle:  # rows out of name order: exercises the gather path of the batcher
        perm = np.random.default_rng(1).permutation(rows)
        table = {key: col[perm] for key, col in table.items()}
    table["names"] = ((np.arange(total) % 26) + 97).astype(np.uint8).tobytes()
    table["bad_records"] = 7
    return table


def _spec(data):
    return dict(part.split("=") for part in bytes(data).decode().split())


def read_mft_image(path):
    with open(path, "rb") as fh:
        data = fh.read()
    spec = _spec(data)
    if spec.get("fail") == "io":
        raise OSError(5, "simulated read error")
    if spec.get("fail") == "bad":
        raise ValueError("not an NTFS volume image")
    geometry = {"bytes_per_sector": 512, "mft_lcn": 4, "mft_size": len(data)}
    geometry["record_size"] = int(spec.get("record", 1024))
    geometry["cluster_size"] = int(spec.get("cluster", 4096))
    return data, geometry


def parse_mft_table(data, record_size, cluster_size):
    spec = _spec(data)
    time.sleep(float(spec.get("sleep", 0)))
    table = synth(int(spec["rows"]), spec.get("shuffle") == "1")
    table["record_size_seen"] = record_size
    table["cluster_size_seen"] = cluster_size
    return table


def read_raw_mft(volume):
    raise AssertionError("read_raw_mft must never run in tests")


def scan_volume(volume, duty=1.0, progress=None):
    raise AssertionError("scan_volume must never run in tests")


def scan_volume_image(path, duty=1.0, progress=None):
    data, info = read_mft_image(path)
    info["duty_seen"] = int(duty * 1000)
    if progress is not None:
        progress(len(data) // 2, len(data))  # half read while the (slow) parse runs
    return parse_mft_table(data, info["record_size"], info["cluster_size"]), info
"""

BOOT = (
    FAKE_MFT
    + r"""
import sys, types
_mod = types.ModuleType("spaceknife.scan.mft")
_mod.parse_mft_table = parse_mft_table
_mod.read_raw_mft = read_raw_mft
_mod.read_mft_image = read_mft_image
_mod.scan_volume = scan_volume
_mod.scan_volume_image = scan_volume_image
sys.modules["spaceknife.scan.mft"] = _mod
from spaceknife.helper.__main__ import main
sys.exit(main(sys.argv[1:]))
"""
)
FAKE_COMMAND = [sys.executable, "-c", BOOT]
_SYNTH: dict[str, Any] = {}
exec(FAKE_MFT, _SYNTH)


def synth(rows: int, shuffle: bool = False) -> dict[str, Any]:
    result: dict[str, Any] = _SYNTH["synth"](rows, shuffle)
    return result


def gather(names: bytes | np.ndarray, off: np.ndarray, length: np.ndarray) -> bytes:
    """All names in row order, concatenated (independent of where the blob put them)."""
    arr = np.frombuffer(names, dtype=np.uint8) if isinstance(names, bytes) else names
    starts = np.zeros(len(off), dtype=np.int64)
    np.cumsum(length[:-1], out=starts[1:])
    index = np.repeat(off.astype(np.int64) - starts, length) + np.arange(int(length.sum()), dtype=np.int64)
    return arr[index].tobytes()


def assert_tables_equal(got: dict[str, Any], want: dict[str, Any]) -> None:
    for key, col in want.items():
        if key in ("names", "name_off") or not isinstance(col, np.ndarray):
            continue
        assert got[key].dtype == col.dtype, key
        np.testing.assert_array_equal(got[key], col, err_msg=key)
    assert gather(got["names"], got["name_off"], got["name_len"]) == gather(
        want["names"], want["name_off"], want["name_len"]
    )


def image(tmp_path: Path, spec: str) -> str:
    path = tmp_path / "mft.img"
    path.write_text(spec)
    return str(path)


# --------------------------------------------------------------------------- fake helpers (auth / protocol)

FAKE_HELPER = r"""
import argparse, os, subprocess, sys, time
from spaceknife.helper.auth import read_token_file
from spaceknife.helper.messages import HEADER, KIND_BINARY, KIND_JSON, PROTOCOL_VERSION, Hello, Response, frame_header, json_frame
from spaceknife.helper.transport import connect

mode = sys.argv[1]
parser = argparse.ArgumentParser()
for flag in ("--pipe", "--token-file", "--server-pid", "--idle-timeout"):
    parser.add_argument(flag)
args = parser.parse_args(sys.argv[2:])
if mode == "child":  # a process the backend did not launch connects instead
    code = "CODE = " + repr(CODE) + chr(10) + CODE
    sys.exit(subprocess.run([sys.executable, "-c", code, "honest", *sys.argv[2:]]).returncode)
token = read_token_file(args.token_file)
chan = connect(args.pipe, 10)
pid = os.getpid() + (1 if mode == "wrong_pid" else 0)
hello = Hello(version=PROTOCOL_VERSION, token="0" * 64 if mode == "wrong_token" else token, pid=pid, elevated=False)
if mode != "silent":
    chan.send(json_frame(hello), 10)
if mode.startswith("garbage_"):
    kind, body = chan.read_frame(30, 30, 1 << 20)
    if mode == "garbage_kind":
        chan.send([HEADER.pack(5, 0x00), b"abcd"], 10)
    elif mode == "garbage_json":
        chan.send([frame_header(KIND_JSON, 8), b"not json"], 10)
    elif mode == "garbage_id":
        chan.send(json_frame(Response(id=999)), 10)
    elif mode == "garbage_binary":
        chan.send([frame_header(KIND_BINARY, 4), b"\xff\xff\xff\x7f"], 10)
    elif mode == "garbage_huge":
        chan.send([HEADER.pack(0xFFFFFFF0, KIND_BINARY)], 10)
try:
    while True:
        chan.read_frame(60, 60, 1 << 20)
except Exception:
    pass
"""


def fake_helper(mode: str) -> list[str]:
    return [sys.executable, "-c", f"CODE = {FAKE_HELPER!r}\n{FAKE_HELPER}", mode]


# --------------------------------------------------------------------------- unprivileged spawning

if IS_WINDOWS:
    import ctypes
    from ctypes import wintypes

    from spaceknife.helper import _win32

    REAL_SHELL_EXECUTE = _win32.shell_execute  # captured before any test patches it

    class _STARTUPINFOW(ctypes.Structure):
        _fields_ = (
            ("cb", wintypes.DWORD),
            ("lpReserved", wintypes.LPWSTR),
            ("lpDesktop", wintypes.LPWSTR),
            ("lpTitle", wintypes.LPWSTR),
            ("dwX", wintypes.DWORD),
            ("dwY", wintypes.DWORD),
            ("dwXSize", wintypes.DWORD),
            ("dwYSize", wintypes.DWORD),
            ("dwXCountChars", wintypes.DWORD),
            ("dwYCountChars", wintypes.DWORD),
            ("dwFillAttribute", wintypes.DWORD),
            ("dwFlags", wintypes.DWORD),
            ("wShowWindow", wintypes.WORD),
            ("cbReserved2", wintypes.WORD),
            ("lpReserved2", ctypes.c_void_p),
            ("hStdInput", wintypes.HANDLE),
            ("hStdOutput", wintypes.HANDLE),
            ("hStdError", wintypes.HANDLE),
        )

    class _PROCESS_INFORMATION(ctypes.Structure):
        _fields_ = (
            ("hProcess", wintypes.HANDLE),
            ("hThread", wintypes.HANDLE),
            ("dwProcessId", wintypes.DWORD),
            ("dwThreadId", wintypes.DWORD),
        )

    _adv = ctypes.WinDLL("advapi32", use_last_error=True)
    _k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _H = wintypes.HANDLE
    _adv.SaferCreateLevel.argtypes = (
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.DWORD,
        ctypes.POINTER(_H),
        ctypes.c_void_p,
    )
    _adv.SaferComputeTokenFromLevel.argtypes = (_H, _H, ctypes.POINTER(_H), wintypes.DWORD, ctypes.c_void_p)
    _adv.SaferCloseLevel.argtypes = (_H,)
    _adv.CreateProcessAsUserW.argtypes = (
        _H,
        wintypes.LPCWSTR,
        wintypes.LPWSTR,
        ctypes.c_void_p,
        ctypes.c_void_p,
        wintypes.BOOL,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.LPCWSTR,
        ctypes.POINTER(_STARTUPINFOW),
        ctypes.POINTER(_PROCESS_INFORMATION),
    )
    _k32.CloseHandle.argtypes = (_H,)

    def spawn_unprivileged(argv: Sequence[str]) -> HelperProcess:
        """CreateProcessAsUser with a Safer NORMALUSER token (Administrators deny-only)."""
        level, token = _H(), _H()
        assert _adv.SaferCreateLevel(2, 0x20000, 1, ctypes.byref(level), None), ctypes.get_last_error()
        try:
            assert _adv.SaferComputeTokenFromLevel(level, None, ctypes.byref(token), 0, None), (
                ctypes.get_last_error()
            )
        finally:
            _adv.SaferCloseLevel(level)
        try:
            si = _STARTUPINFOW()
            si.cb = ctypes.sizeof(si)
            pi = _PROCESS_INFORMATION()
            cmd = ctypes.create_unicode_buffer(subprocess.list2cmdline(list(argv)))
            ok = _adv.CreateProcessAsUserW(
                token,
                None,
                cmd,
                None,
                None,
                False,
                0x08000000,
                None,
                None,
                ctypes.byref(si),
                ctypes.byref(pi),
            )
            assert ok, ctypes.WinError(ctypes.get_last_error())
        finally:
            _k32.CloseHandle(token)
        _k32.CloseHandle(pi.hThread)
        return _win32.ProcessHandle(int(pi.hProcess), int(pi.dwProcessId))


@pytest.fixture
def unprivileged(monkeypatch: pytest.MonkeyPatch) -> None:
    """Helpers started by HelperClient.launch(elevate=False) run without administrator rights."""
    if not is_elevated():
        return
    if not IS_WINDOWS:
        pytest.skip("running as root: cannot start an unprivileged helper")
    monkeypatch.setattr(client_mod, "start_subprocess", spawn_unprivileged)


@pytest.fixture(autouse=True)
def no_uac(monkeypatch: pytest.MonkeyPatch) -> None:
    """A test that reaches the real ShellExecuteEx would show a UAC prompt: fail instead."""
    if IS_WINDOWS:

        def forbidden(params: object) -> object:
            raise AssertionError("tests must never call the real ShellExecuteExW")

        monkeypatch.setattr(_win32, "shell_execute", forbidden)


@pytest.fixture
async def helper(unprivileged: None) -> AsyncIterator[HelperClient]:
    client = await HelperClient.launch(elevate=False, command=FAKE_COMMAND)
    try:
        yield client
    finally:
        await client.close()


async def wait_exit(process: HelperProcess, seconds: float = 10.0) -> int | None:
    return await asyncio.to_thread(process.wait, seconds)


# --------------------------------------------------------------------------- pure units


def test_requests_are_strictly_validated() -> None:
    ok = Request.model_validate_json(b'{"type":"request","id":3,"call":{"op":"scan_mft","volume":"d:"}}')
    assert ok.call.op == "scan_mft"
    bad = [
        b'{"type":"request","id":1,"call":{"op":"scan_mft","volume":"C:\\\\"}}',
        b'{"type":"request","id":1,"call":{"op":"scan_mft","volume":"CC:"}}',
        b'{"type":"request","id":1,"call":{"op":"scan_mft","volume":"1:"}}',
        b'{"type":"request","id":1,"call":{"op":"scan_mft","volume":"C:\\n"}}',
        b'{"type":"request","id":1,"call":{"op":"volume_info","volume":"\\\\\\\\.\\\\C:"}}',
        b'{"type":"request","id":1,"call":{"op":"delete","path":"C:\\\\x"}}',
        b'{"type":"request","id":1,"call":{"op":"ping","extra":1}}',
        b'{"type":"request","id":"1","call":{"op":"ping"}}',
        b'{"type":"request","id":0,"call":{"op":"ping"}}',
        b'{"type":"request","id":1,"call":{"op":"scan_mft","volume":"C:","batch_rows":0}}',
        b'{"type":"request","id":1,"call":{"op":"scan_mft","volume":"C:","batch_rows":"5"}}',
    ]
    for raw in bad:
        with pytest.raises(ValidationError):
            Request.model_validate_json(raw)
    with pytest.raises(ValidationError):
        REPLY.validate_json(b'{"type":"hello","version":1,"token":"XYZ","pid":1,"elevated":false}')


def test_frame_header_checks() -> None:
    assert check_header(frame_header(KIND_JSON, 10), max_binary=0) == (KIND_JSON, 10)
    assert check_header(frame_header(KIND_BINARY, 100), max_binary=100) == (KIND_BINARY, 100)
    with pytest.raises(FrameError):
        check_header(frame_header(KIND_BINARY, 101), max_binary=100)
    with pytest.raises(FrameError):
        check_header(frame_header(KIND_JSON, MAX_CONTROL_BYTES + 1), max_binary=0)
    with pytest.raises(FrameError):
        check_header(HEADER.pack(1, 0x00), max_binary=0)
    with pytest.raises(FrameError):
        check_header(HEADER.pack(0, KIND_JSON), max_binary=0)


def test_batches_split_by_rows_and_name_bytes() -> None:
    lengths = np.full(10, 100, dtype=np.uint16)
    assert batch_bounds(lengths, 4, 10_000) == [(0, 4), (4, 8), (8, 10)]
    assert batch_bounds(lengths, 100, 250) == [(0, 2), (2, 4), (4, 6), (6, 8), (8, 10)]
    assert batch_bounds(lengths, 100, 50) == [(i, i + 1) for i in range(10)]  # one oversized name each
    assert batch_bounds(np.zeros(0, dtype=np.uint16), 5, 10) == [(0, 0)]


def test_rebase_names_slices_or_gathers() -> None:
    table = synth(5000, shuffle=True)
    names = np.frombuffer(table["names"], dtype=np.uint8)
    for a, b in [(0, 5000), (100, 1100), (4999, 5000), (7, 7)]:
        off, length = table["name_off"][a:b], table["name_len"][a:b]
        blob, new_off = rebase_names(names, off, length)
        assert new_off.dtype == np.uint32
        assert gather(blob, new_off, length) == gather(names, off, length)
        if b - a > 10:
            assert len(blob) == int(length.sum())  # scattered rows were gathered, not over-sliced
    ordered = synth(1000)
    blob, new_off = rebase_names(
        np.frombuffer(ordered["names"], np.uint8), ordered["name_off"][10:20], ordered["name_len"][10:20]
    )
    assert new_off[0] == 0 and len(blob) == int(ordered["name_len"][10:20].sum())


def test_frame_parts_match_encode_frame() -> None:
    table = synth(1001)
    cases: list[dict[str, Any]] = [
        {k: v for k, v in table.items() if isinstance(v, np.ndarray)},
        {
            "a": np.arange(10, dtype=">u4"),
            "b": np.arange(20, dtype=np.int64)[::2],
            "c": np.zeros(10, np.uint8),
        },
        {"empty": np.zeros(0, dtype=np.uint16)},
    ]
    for columns in cases:
        for blob in (table["names"], b"", b"xyz"):
            kwargs: dict[str, Any] = dict(stream="mft", layout="mft.v1", seq=3, final=False, request_id=9)
            parts, size = frame_parts(columns=columns, blobs={"names": blob}, **kwargs)
            joined = b"".join(bytes(p) for p in parts)
            assert joined == encode_frame(columns=columns, blobs={"names": blob}, **kwargs)
            assert size == len(joined)
            header, decoded = decode_frame(joined)
            assert header.seq == 3 and bytes(decoded["names"]) == blob


def test_concat_batches_rebases_offsets() -> None:
    table = synth(3000, shuffle=True)
    names = np.frombuffer(table["names"], np.uint8)
    batches = []
    for a, b in [(0, 1000), (1000, 2500), (2500, 3000)]:
        batch = {k: v[a:b] for k, v in table.items() if isinstance(v, np.ndarray)}
        blob, batch["name_off"] = rebase_names(names, table["name_off"][a:b], table["name_len"][a:b])
        batch["names"] = np.frombuffer(blob, np.uint8)
        batches.append(batch)
    joined = concat_batches(batches)
    assert isinstance(joined["names"], bytes)
    assert_tables_equal(joined, table)


def test_shell_execute_params() -> None:
    argv = [r"C:\Program Files\Py\python.exe", "-I", "-m", "spaceknife.helper", "--pipe", r"\\.\pipe\x y"]
    params = elevated_params(argv)
    assert params.verb == "runas"
    assert params.file == argv[0]
    assert params.parameters == r'-I -m spaceknife.helper --pipe "\\.\pipe\x y"'
    assert params.show == 0  # SW_HIDE
    assert params.mask == 0x40 | 0x8000 | 0x400 | 0x100  # NOCLOSEPROCESS | NO_CONSOLE | FLAG_NO_UI | NOASYNC
    assert params.directory is None
    assert helper_command(True)[1:] == ["-I", "-m", "spaceknife.helper"]
    assert helper_command(False)[1:] == ["-m", "spaceknife.helper"]


def test_token_file_is_private_and_read_once(tmp_path: Path) -> None:
    token = new_token()
    assert re.fullmatch(r"[0-9a-f]{64}", token)
    path = write_token_file(token, str(tmp_path))
    if IS_WINDOWS:
        sddl = _file_sddl(str(path))
        assert sddl.startswith("D:P")  # protected: nothing inherited from the directory
        trustees = set(re.findall(r"\(A;[^;]*;[^;]*;;;([^)]*)\)", sddl))
        assert trustees == {_win32.current_user_sid(), "BA"}, sddl
    else:
        assert stat.S_IMODE(path.stat().st_mode) == 0o600
    with pytest.raises(FileExistsError):
        _write_same(path, token)
    assert read_token_file(str(path)) == token
    assert not path.exists()
    with pytest.raises(ValueError, match="路径"):
        read_token_file(str(tmp_path / "other.txt"))
    bad = tmp_path / "spaceknife-helper-0123456789abcdef.token"
    bad.write_text("not-a-token")
    with pytest.raises(ValueError, match="格式"):
        read_token_file(str(bad))
    assert not bad.exists()


def _write_same(path: Path, token: str) -> None:
    if IS_WINDOWS:
        _win32.write_private_file(str(path), token.encode())
    else:
        os.close(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600))


def _file_sddl(path: str) -> str:
    adv = ctypes.WinDLL("advapi32", use_last_error=True)
    adv.GetFileSecurityW.argtypes = (
        wintypes.LPCWSTR,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
    )
    adv.ConvertSecurityDescriptorToStringSecurityDescriptorW.argtypes = (
        ctypes.c_void_p,
        wintypes.DWORD,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.LPWSTR),
        ctypes.c_void_p,
    )
    need = wintypes.DWORD()
    adv.GetFileSecurityW(path, 4, None, 0, ctypes.byref(need))  # DACL_SECURITY_INFORMATION
    buf = ctypes.create_string_buffer(need.value)
    assert adv.GetFileSecurityW(path, 4, buf, need, ctypes.byref(need))
    out = wintypes.LPWSTR()
    assert adv.ConvertSecurityDescriptorToStringSecurityDescriptorW(buf, 1, 4, ctypes.byref(out), None)
    return str(out.value)


def test_addresses_are_validated() -> None:
    listener = Listener()
    try:
        assert valid_address(listener.address)
        if IS_WINDOWS:
            assert re.fullmatch(r"\\\\\.\\pipe\\spaceknife-helper-[0-9a-f]{32}", listener.address)
        else:
            assert stat.S_IMODE(os.stat(os.path.dirname(listener.address)).st_mode) == 0o700
    finally:
        listener.close()
    for bad in [
        r"C:\Windows\System32\config\SAM",
        r"\\.\pipe\other",
        r"\\.\C:",
        "/etc/passwd",
        "relative.sock",
        "",
    ]:
        assert not valid_address(bad), bad


def test_helper_refuses_bad_arguments(tmp_path: Path) -> None:
    token_path = write_token_file(new_token(), str(tmp_path))
    base = [*helper_command(False), "--server-pid", str(os.getpid())]
    bad_pipe = subprocess.run(
        [*base, "--pipe", r"C:\Windows\win.ini", "--token-file", str(token_path)],
        capture_output=True,
        timeout=60,
    )
    assert bad_pipe.returncode == 3, bad_pipe.stderr
    assert not token_path.exists()  # read and deleted before anything else
    bad_token = subprocess.run(
        [*base, "--pipe", "x", "--token-file", str(tmp_path / "nope.token")], capture_output=True, timeout=60
    )
    assert bad_token.returncode == 2, bad_token.stderr


# --------------------------------------------------------------------------- live helper


async def test_hello_ping_and_close(unprivileged: None) -> None:
    client = await HelperClient.launch(elevate=False)  # the real `python -m spaceknife.helper`
    process = client._process
    assert client.alive
    assert client.elevated is False
    info = await client.ping()
    assert info == {"pid": client.pid, "elevated": False, "version": 1, "platform": sys.platform}
    await client.close()
    assert not client.alive
    assert await wait_exit(process) == 0  # shut down cleanly, not killed
    with pytest.raises(HelperUnavailable) as err:
        await client.ping()
    assert err.value.code == "closed"
    await client.close()  # idempotent


async def test_scan_mft_needs_admin(unprivileged: None) -> None:
    async with await HelperClient.launch(elevate=False) as client:
        with pytest.raises(HelperError) as err:
            await client.scan_mft("C:")
        assert err.value.code == "needs_admin"
        assert "需要管理员权限" in err.value.message
        assert not isinstance(err.value, HelperUnavailable)
        assert (await client.ping())["pid"] == client.pid  # still usable


async def test_volume_info(helper: HelperClient) -> None:
    if IS_WINDOWS:
        system = os.environ.get("SYSTEMDRIVE", "C:")
        info = await helper.volume_info(system.lower())
        assert info["volume"] == system.upper()
        assert info["fs_type"] == "NTFS"
        assert info["cluster_size"] and info["bytes_per_sector"]
        assert "record_size" not in info  # FSCTL data needs administrator rights
    else:
        with pytest.raises(HelperError) as err:
            await helper.volume_info("C:")
        assert err.value.code == "not_supported"
    with pytest.raises(HelperError) as bad:
        await helper.volume_info("C:\\Windows")
    assert bad.value.code == "invalid_request"


async def test_scan_mft_image_roundtrip(helper: HelperClient, tmp_path: Path) -> None:
    rows = 25_000
    phases: list[str] = []
    read: list[float | None] = []

    def on_progress(phase: str, _elapsed: float, fraction: float | None) -> None:
        phases.append(phase)
        read.append(fraction)

    path = image(tmp_path, f"rows={rows} shuffle=1 sleep=1.3 record=4096 cluster=8192")
    table = await helper.scan_mft_image(path, batch_rows=6000, on_progress=on_progress)
    assert_tables_equal(table, synth(rows, shuffle=True))
    # the parser got the geometry the reader found; scalars of the table come back with it
    assert (
        table["bad_records"] == 7 and table["record_size_seen"] == 4096 and table["cluster_size_seen"] == 8192
    )
    assert (
        not {"frames", "rows", "record_size", "mft_size"} & table.keys()
    )  # statistics stay out of the table
    stats = helper.last_scan
    assert stats["frames"] == 5 and stats["rows"] == rows
    assert (
        stats["record_size"] == 4096
        and stats["cluster_size"] == 8192
        and stats["mft_size"] == os.path.getsize(path)
    )
    assert "scan" in phases  # heartbeats while the (slow) scan ran
    assert any(f is not None and 0.45 < f <= 0.5 for f in read)  # ... carrying how much was read
    for col in table.values():
        if isinstance(col, np.ndarray):
            assert col.flags.writeable


async def test_stream_batches_and_abandon(helper: HelperClient, tmp_path: Path) -> None:
    path = image(tmp_path, "rows=10000")
    sizes = []
    async with contextlib.aclosing(helper.stream_mft_image(path, batch_rows=3000)) as batches:
        async for batch in batches:
            sizes.append(len(batch["record"]))
            assert batch["names"].dtype == np.uint8
            assert int(batch["name_off"][0]) == 0  # offsets are relative to the batch's own blob
    assert sizes == [3000, 3000, 3000, 1000]
    async with contextlib.aclosing(helper.stream_mft_image(path, batch_rows=1000)) as batches:
        async for _ in batches:
            break  # abandon: the rest of the stream is skipped, the helper stays usable
    assert helper.alive
    assert (await helper.ping())["pid"] == helper.pid
    empty = await helper.scan_mft_image(image(tmp_path, "rows=0"))
    assert len(empty["record"]) == 0 and empty["record"].dtype == np.uint64 and empty["names"] == b""


async def test_scan_errors_keep_the_helper(helper: HelperClient, tmp_path: Path) -> None:
    for spec, code in [("rows=1 fail=io", "io_error"), ("rows=1 fail=bad", "io_error")]:
        with pytest.raises(HelperError) as err:
            await helper.scan_mft_image(image(tmp_path, spec))
        assert err.value.code == code and not isinstance(err.value, HelperUnavailable)
    with pytest.raises(HelperError) as missing:
        await helper.scan_mft_image(str(tmp_path / "missing.img"))
    assert missing.value.code == "io_error"
    assert (await helper.ping())["pid"] == helper.pid


async def test_real_mft_module_end_to_end(unprivileged: None, tmp_path: Path) -> None:
    """The real ``spaceknife.scan.mft`` (reader + parser) on a synthetic NTFS volume image."""
    try:
        from spaceknife.scan import mft

        from . import mft_image as img

        records = img.metafile_records() + img.tree_records(
            {"a": {"x.txt": 5000, "y.TXT": 10}, "b.bin": 100_000}
        )
        img.build_volume(tmp_path / "vol.img", records, [(16, 64)])
    except (ImportError, AttributeError, TypeError, ValueError) as exc:
        pytest.skip(f"MFT module or image builder not available: {exc!r}")
    data, info = mft.read_mft_image(tmp_path / "vol.img")
    want = mft.parse_mft_table(data, info["record_size"], info["cluster_size"])
    async with await HelperClient.launch(elevate=False) as client:
        got = await client.scan_mft_image(str(tmp_path / "vol.img"), batch_rows=7)
    assert got.keys() == want.keys()
    assert_tables_equal(got, want)
    assert got["bad_records"] == want["bad_records"]
    assert client.last_scan["record_size"] == info["record_size"]


async def test_scan_mft_image_throughput(helper: HelperClient, tmp_path: Path) -> None:
    rows = 3_000_000
    path = image(tmp_path, f"rows={rows}")
    t0 = time.perf_counter()
    table = await helper.scan_mft_image(path)
    elapsed = time.perf_counter() - t0
    stats = helper.last_scan
    assert stats["frames"] == 3 and stats["rows"] == rows
    want = synth(rows)
    assert_tables_equal(table, want)
    wire = int(stats["wire_bytes"] or 0)
    transfer = elapsed - float(stats["scan_seconds"] or 0)
    print(
        f"\nhelper bulk transfer: {rows:,} rows, {wire / 1e6:.0f} MB in {stats['frames']} frames; "
        f"end-to-end {elapsed:.2f} s; helper send {stats['send_seconds']:.2f} s; "
        f"transfer+assembly {transfer:.2f} s = {wire / 1e6 / transfer:.0f} MB/s"
    )
    assert wire > rows * 85


async def test_idle_timeout(unprivileged: None) -> None:
    client = await HelperClient.launch(elevate=False, idle_timeout=0.5)
    assert await wait_exit(client._process) == 0
    assert not client.alive
    with pytest.raises(HelperUnavailable) as err:
        await client.ping()
    assert err.value.code == "exited"
    await client.close()


async def test_helper_exits_when_backend_disconnects(helper: HelperClient) -> None:
    helper._channel.close()  # what the OS does when the backend process dies
    assert await wait_exit(helper._process) == 0
    assert not helper.alive


async def test_cancelled_scan_tears_down_the_connection(helper: HelperClient, tmp_path: Path) -> None:
    started = asyncio.Event()

    async def consume() -> None:
        async with contextlib.aclosing(
            helper.stream_mft_image(image(tmp_path, "rows=200000"), batch_rows=1000)
        ) as s:
            async for _ in s:
                started.set()
                await asyncio.sleep(0.05)

    task = asyncio.create_task(consume())
    await started.wait()
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert not helper.alive  # no draining of 199 frames inside a cancelled task
    assert await wait_exit(helper._process) is not None


async def test_helper_death_is_reported(helper: HelperClient) -> None:
    os.kill(helper.pid, signal.SIGTERM if IS_WINDOWS else signal.SIGKILL)
    await wait_exit(helper._process)
    with pytest.raises(HelperUnavailable) as err:
        await helper.ping()
    assert err.value.code == "exited"
    assert not helper.alive


async def test_shared_helper_is_reused(unprivileged: None) -> None:
    shared = SharedHelper(command=FAKE_COMMAND)
    try:
        first = await shared.get(elevate=False)
        assert await shared.get(elevate=False) is first
        os.kill(first.pid, signal.SIGTERM if IS_WINDOWS else signal.SIGKILL)
        await wait_exit(first._process)
        second = await shared.get(elevate=False)
        assert second is not first and second.pid != first.pid
        assert shared.current is second
    finally:
        await shared.close()
    assert shared.current is None


# --------------------------------------------------------------------------- authentication


@pytest.mark.parametrize(
    ("mode", "why"),
    [
        ("wrong_token", "令牌不符"),
        ("wrong_pid", "不符"),
        ("child", "不是启动的进程"),
    ],
)
async def test_handshake_rejects_impostors(mode: str, why: str) -> None:
    with pytest.raises(HelperUnavailable) as err:
        await HelperClient.launch(elevate=False, command=fake_helper(mode))
    assert err.value.code == "auth"
    assert why in err.value.message


async def test_silent_helper_times_out() -> None:
    t0 = time.monotonic()
    with pytest.raises(HelperUnavailable) as err:
        await HelperClient.launch(elevate=False, command=fake_helper("silent"), connect_timeout=2)
    assert err.value.code == "timeout"
    assert time.monotonic() - t0 < 15


async def test_helper_that_exits_or_never_connects() -> None:
    t0 = time.monotonic()
    with pytest.raises(HelperUnavailable) as err:
        await HelperClient.launch(elevate=False, command=[sys.executable, "-c", "import sys; sys.exit(5)"])
    assert err.value.code == "exited" and "5" in err.value.message
    assert time.monotonic() - t0 < 15  # noticed the exit, did not wait for the timeout
    with pytest.raises(HelperUnavailable) as late:
        await HelperClient.launch(
            elevate=False, command=[sys.executable, "-c", "import time; time.sleep(60)"], connect_timeout=1
        )
    assert late.value.code == "timeout"


# --------------------------------------------------------------------------- malformed traffic


@pytest.mark.parametrize(
    "mode", ["garbage_kind", "garbage_json", "garbage_id", "garbage_binary", "garbage_huge"]
)
async def test_client_survives_malformed_helper(mode: str) -> None:
    client = await HelperClient.launch(elevate=False, command=fake_helper(mode))
    try:
        with pytest.raises(HelperUnavailable) as err:
            await client.ping()
        assert err.value.code == "protocol"
        assert not client.alive
    finally:
        await client.close()
    assert await wait_exit(client._process) is not None


async def test_helper_survives_malformed_requests(unprivileged: None, tmp_path: Path) -> None:
    listener = Listener()
    token = new_token()
    token_path = write_token_file(token, str(tmp_path))
    argv = [
        *helper_command(False),
        "--pipe",
        listener.address,
        "--token-file",
        str(token_path),
        "--server-pid",
        str(os.getpid()),
    ]
    process = client_mod.start_subprocess(argv)
    loop = asyncio.get_running_loop()
    try:
        chan = await client_mod._accept(listener, process, loop.time() + 30)
        await client_mod._handshake(chan, process, token, loop.time() + 10)
    finally:
        listener.close()

    async def send_raw(kind: int, body: bytes) -> Any:
        await chan.send([frame_header(kind, len(body)), body], 10)
        _kind, reply = await chan.read_frame(10)
        return REPLY.validate_json(reply)

    try:
        reply = await send_raw(KIND_JSON, b"not json at all")
        assert isinstance(reply, Error) and reply.code == "invalid_request" and reply.id is None
        reply = await send_raw(KIND_JSON, b'{"type":"request","id":7,"call":{"op":"format_disk"}}')
        assert isinstance(reply, Error) and reply.code == "invalid_request" and reply.id == 7
        reply = await send_raw(
            KIND_JSON, b'{"type":"request","id":8,"call":{"op":"scan_mft","volume":"C:\\\\Windows"}}'
        )
        assert isinstance(reply, Error) and reply.id == 8 and "volume" in reply.message
        reply = await send_raw(
            KIND_JSON, b'{"type":"request","id":9,"call":{"op":"scan_mft_image","path":""}}'
        )
        assert isinstance(reply, Error) and reply.id == 9
        reply = await send_raw(KIND_BINARY, b"\x00" * 16)
        assert isinstance(reply, Error) and reply.code == "invalid_request"
        await chan.send_message(Request(id=10, call=Ping()), 10)
        _kind, body = await chan.read_frame(10)
        assert isinstance(REPLY.validate_json(body), Response)
        await chan.send([HEADER.pack(MAX_CONTROL_BYTES + 100, KIND_JSON)], 10)  # oversized: stream is lost
        _kind, body = await chan.read_frame(10)
        final = REPLY.validate_json(body)
        assert isinstance(final, Error) and final.code == "bad_frame"
        with pytest.raises((EOFError, ConnectionError)):
            await chan.read_frame(10)
        assert await wait_exit(process) == 0
    finally:
        chan.close()
        process.terminate()
        process.close()


# --------------------------------------------------------------------------- elevation path (mocked)


@pytest.mark.skipif(not IS_WINDOWS, reason="UAC is Windows-only")
def test_shell_execute_builds_runas_request(monkeypatch: pytest.MonkeyPatch) -> None:
    """The real _win32.shell_execute, with the ShellExecuteExW entry point itself faked."""
    params = elevated_params([r"C:\py\python.exe", "-I", "-m", "spaceknife.helper", "--pipe", "p"])
    seen: dict[str, Any] = {}

    def fake_ok(ref: Any) -> int:
        info = ref._obj
        seen.update(
            verb=info.lpVerb,
            file=info.lpFile,
            params=info.lpParameters,
            show=info.nShow,
            mask=info.fMask,
            size=info.cbSize,
        )
        info.hProcess = _win32._k32.OpenProcess(0x00101000, False, os.getpid())  # SYNCHRONIZE | QUERY_LIMITED
        return 1

    monkeypatch.setattr(_win32._shell32, "ShellExecuteExW", fake_ok)
    assert _win32._shell32.ShellExecuteExW is fake_ok
    proc = REAL_SHELL_EXECUTE(params)
    try:
        assert proc.pid == os.getpid() and proc.poll() is None
    finally:
        proc.close()
    assert seen == {
        "verb": "runas",
        "file": r"C:\py\python.exe",
        "params": "-I -m spaceknife.helper --pipe p",
        "show": 0,
        "mask": params.mask,
        "size": ctypes.sizeof(_win32.SHELLEXECUTEINFOW),
    }

    def fake_cancel(ref: Any) -> int:
        ctypes.set_last_error(1223)
        return 0

    monkeypatch.setattr(_win32._shell32, "ShellExecuteExW", fake_cancel)
    assert _win32._shell32.ShellExecuteExW is fake_cancel
    with pytest.raises(PermissionError) as err:
        REAL_SHELL_EXECUTE(params)
    assert err.value.errno == 1223


@pytest.mark.skipif(not IS_WINDOWS, reason="UAC is Windows-only")
async def test_launch_elevated_denied(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[Any] = []
    monkeypatch.setattr(client_mod, "is_elevated", lambda: False)

    def deny(params: Any) -> Any:
        calls.append(params)
        raise PermissionError(1223, "cancelled")

    monkeypatch.setattr(_win32, "shell_execute", deny)
    with pytest.raises(HelperDenied) as err:
        await HelperClient.launch(elevate=True)
    assert err.value.code == "denied" and err.value.message == "用户拒绝了管理员授权"
    (params,) = calls
    assert params.verb == "runas" and params.file == sys.executable
    assert params.parameters.startswith("-I -m spaceknife.helper --pipe \\\\.\\pipe\\spaceknife-helper-")
    token_file = re.search(r"--token-file (\S+)", params.parameters)
    assert token_file and not Path(token_file.group(1)).exists()  # cleaned up after the refusal


@pytest.mark.skipif(not IS_WINDOWS, reason="UAC is Windows-only")
async def test_launch_elevated_path_with_mocked_uac(
    monkeypatch: pytest.MonkeyPatch, unprivileged: None
) -> None:
    """Everything after the UAC prompt is real: command line quoting, connect, hello, requests."""
    monkeypatch.setattr(client_mod, "is_elevated", lambda: False)
    started: list[HelperProcess] = []

    def fake_uac(params: Any) -> HelperProcess:
        assert params.verb == "runas"
        argv = [params.file, *_split_cmdline(params.parameters)]
        assert argv[1:4] == ["-I", "-m", "spaceknife.helper"]
        proc = spawn_unprivileged(argv) if is_elevated() else _popen(argv)
        started.append(proc)
        return proc

    monkeypatch.setattr(_win32, "shell_execute", fake_uac)
    client = await HelperClient.launch(elevate=True)
    try:
        assert started and (await client.ping())["pid"] == client.pid
    finally:
        await client.close()

    def slow_uac(params: Any) -> HelperProcess:
        time.sleep(2.0)  # the user leaves the prompt open
        raise PermissionError(1223, "cancelled")

    monkeypatch.setattr(_win32, "shell_execute", slow_uac)
    with pytest.raises(HelperUnavailable) as err:
        await HelperClient.launch(elevate=True, connect_timeout=0.5)
    assert err.value.code == "timeout" and not isinstance(err.value, HelperDenied)


def _split_cmdline(cmdline: str) -> list[str]:
    shell32 = ctypes.WinDLL("shell32")
    shell32.CommandLineToArgvW.restype = ctypes.POINTER(wintypes.LPWSTR)
    shell32.CommandLineToArgvW.argtypes = (wintypes.LPCWSTR, ctypes.POINTER(ctypes.c_int))
    argc = ctypes.c_int()
    argv = shell32.CommandLineToArgvW("x " + cmdline, ctypes.byref(argc))
    try:
        return [argv[i] for i in range(1, argc.value)]
    finally:
        ctypes.windll.kernel32.LocalFree(argv)


def _popen(argv: list[str]) -> HelperProcess:
    from spaceknife.helper.launch import start_subprocess

    return start_subprocess(argv)


@pytest.mark.skipif(IS_WINDOWS, reason="POSIX elevation is not implemented yet")
async def test_posix_elevation_not_supported() -> None:
    if is_elevated():
        pytest.skip("running as root")
    with pytest.raises(HelperUnavailable) as err:
        await HelperClient.launch(elevate=True)
    assert err.value.code == "not_supported"
