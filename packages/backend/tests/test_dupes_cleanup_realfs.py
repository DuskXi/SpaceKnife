"""The cleanup executor on the file system the tests run on (docs/dupes.md §16): the primitives of
this system (Windows: winfs, by handle; elsewhere: POSIX), steps stamped with the search's own
native reads. These came out of the verification on real volumes (NTFS, ReFS, WinBtrfs, ext4,
btrfs, xfs) and run on every system, the Windows-only ones on Windows.

Everything happens in the test's temporary folder with the sandbox set to it.
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import textwrap
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from spaceknife.dupes.cleanup import execute as X
from spaceknife.dupes.cleanup.fsops import Faults, Sandbox, St, make_fs
from spaceknife.dupes.cleanup.journal import Journal, fold, read_records
from spaceknife.dupes.guard import Stamped, _digests
from spaceknife.dupes.reads import default_ops

WIN = sys.platform == "win32"
windows_only = pytest.mark.skipif(not WIN, reason="Windows primitives")
linux_only = pytest.mark.skipif(not sys.platform.startswith("linux"), reason="Linux")


def _rows(paths: list[Path]) -> list[dict[str, int]]:
    got = default_ops().identity([str(p) for p in paths], True)
    return [{k: int(v[i]) for k, v in got.items()} for i in range(len(paths))]


def _stamp(p: Path) -> dict[str, Any]:
    r = _rows([p])[0]
    assert r["reason"] == 0, r
    return {
        "path": str(p),
        "vol": 0,
        **{k: r[k] for k in ("dev", "fid_lo", "fid_hi", "size", "mtime", "change")},
        "usn": r.get("usn", 0),
    }


def _digest(p: Path) -> str:
    s = _stamp(p)
    got = _digests(
        "native",
        [Stamped(str(p), s["dev"], s["fid_lo"], s["fid_hi"], s["size"], s["mtime"], s["change"], s["usn"])],
    )[0]
    assert got is not None
    return got.hex()


def _step(seq: int, action: str, copy: Path, keeper: Path, *, verify: bool = False) -> dict[str, Any]:
    c, k = _stamp(copy), _stamp(keeper)
    return {
        "seq": seq,
        "group": 0,
        "digest": _digest(keeper),
        "action": action,
        "path": str(copy),
        "vol": 0,
        **{f: c[f] for f in ("dev", "fid_lo", "fid_hi", "size", "mtime", "change", "usn")},
        "parent": str(copy.parent),
        "parent_id": None,
        "keepers": [k],
        "target": k if action != "trash" else None,
        "text": os.path.relpath(keeper, copy.parent) if action == "symlink" else "",
        "risks": [],
        "alloc": c["size"],
        "scan": 0,
        "node": 0,
        "verify_content": verify,
        "row": -1,
    }


def _ctx(
    tmp_path: Path,
    steps: list[dict[str, Any]],
    *,
    run_id: str = "r-20261024-000000-real",
    sandbox: Path | None = None,
    faults: str = "",
) -> X.RunCtx:
    ctx = X.RunCtx(
        run_id=run_id,
        steps=steps,
        fs=make_fs(Sandbox([str(sandbox or tmp_path)]), Faults.parse(faults) if faults else None),
        options={"on_failure": "continue", "verify": "auto"},
        data_dir=tmp_path / "home" / "data",
        ops_kind="native",
        check_locks=False,
    )
    ctx.journal = Journal.create(
        run_id, {"steps": steps, "options": ctx.options, "ops": "native"}, root=tmp_path / "runs"
    )
    ctx.journal.write({"t": "run", "run": run_id}, sync=True)
    return ctx


def _ident(p: Path) -> tuple[int, int, int]:
    r = _rows([p])[0]
    return (r["dev"], r["fid_lo"], r["fid_hi"])


def _write(p: Path, data: bytes) -> Path:
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_bytes(data)
    return p


def _read_only(p: Path) -> None:
    os.chmod(p, stat.S_IREAD)


def _is_read_only(p: Path) -> bool:
    return not os.stat(p).st_mode & stat.S_IWRITE


def _leftovers(folder: Path) -> list[str]:
    return sorted(n for n in os.listdir(folder) if n.startswith(".sk-"))


# ------------------------------------------------------------------ several steps on one keeper
def test_three_links_to_one_read_only_keeper_with_emoji_names_and_their_undo(tmp_path: Path) -> None:
    """Every later step on a keeper sees it as the earlier ones left it (NTFS writes a USN record
    when the linking handle closes); names outside the BMP open; links to a read-only keeper are
    removed again on undo."""
    data = os.urandom(70000)
    keeper = _write(tmp_path / "k" / "🎵 歌曲.bin", data)
    copies = [_write(tmp_path / f"c{i}" / "🎵 歌曲 copy.bin", data) for i in range(3)]
    before = {c: _ident(c) for c in copies}
    _read_only(keeper)
    steps = [_step(i + 1, "hardlink", c, keeper) for i, c in enumerate(copies)]
    ctx = _ctx(tmp_path, steps)
    report = X.run(ctx)
    assert report["state"] == "done", {s: (o.code, o.why) for s, o in ctx.outcomes.items()}
    assert all(_ident(c) == _ident(keeper) for c in copies)
    assert all(_leftovers(c.parent) == [] for c in copies)
    ctx.journal.close()  # type: ignore[union-attr]
    out = X.undo(ctx, tmp_path / "runs" / ctx.run_id)
    assert {code for code, _ in out.values()} == {"undone"}, out
    assert all(_ident(c) == before[c] and c.read_bytes() == data for c in copies)
    assert os.stat(keeper).st_nlink == 1 and _is_read_only(keeper)
    assert all(_leftovers(c.parent) == [] for c in copies)


def test_purge_reads_the_keeper_as_it_is_now(tmp_path: Path) -> None:
    """A later link to the same keeper moves its change time: purging the earlier step's original
    (reading both again) must still go."""
    data = os.urandom(50000)
    keeper = _write(tmp_path / "k" / "a.bin", data)
    copies = [_write(tmp_path / f"c{i}" / "a.bin", data) for i in range(2)]
    ctx = _ctx(tmp_path, [_step(i + 1, "hardlink", c, keeper) for i, c in enumerate(copies)])
    X.run(ctx)
    ctx.journal.close()  # type: ignore[union-attr]
    run_dir = tmp_path / "runs" / ctx.run_id
    recs = [Path(ctx.outcomes[s].rec) for s in (1, 2)]
    out = X.purge(ctx, run_dir, verify="content")
    assert {code for code, _ in out.values()} == {"purged"}, out
    assert not any(r.exists() for r in recs)
    assert all(c.read_bytes() == data for c in copies)


# ------------------------------------------------------------------ the recycle bin
def test_a_recycle_bin_step_is_refused_outside_the_sandbox_not_raised(tmp_path: Path) -> None:
    data = os.urandom(20000)
    keeper = _write(tmp_path / "k" / "t.bin", data)
    copy = _write(tmp_path / "c" / "t.bin", data)
    ident = _ident(copy)
    ctx = _ctx(tmp_path, [_step(1, "trash", copy, keeper)], sandbox=tmp_path / "elsewhere")
    report = X.run(ctx)
    assert ctx.outcomes[1].code == "skipped_denied" and report["state"] == "partial"
    assert _ident(copy) == ident
    # a failure before anything moved says so for the step (and a disk error stops the run)
    ctx = _ctx(
        tmp_path, [_step(1, "trash", copy, keeper)], run_id="r-20261024-000001-real", faults="trash@1:eio"
    )
    X.run(ctx)
    if ctx.outcomes[1].code != "done":  # a volume without a recycle bin goes to the recovery area
        assert _ident(copy) == ident and ctx.stop_reason == "stop_disk"


CRASH = textwrap.dedent(
    """
    import json, sys
    from pathlib import Path
    from spaceknife.dupes.cleanup import execute as X
    from spaceknife.dupes.cleanup.fsops import Faults, Sandbox, make_fs
    from spaceknife.dupes.cleanup.journal import Journal
    tmp = Path(sys.argv[1]); steps = json.loads(Path(sys.argv[2]).read_text()); run_id = sys.argv[3]
    fs = make_fs(Sandbox([str(tmp)]), Faults.parse(sys.argv[4]))
    ctx = X.RunCtx(run_id=run_id, steps=steps, fs=fs, options={"verify": "metadata"}, data_dir=tmp / "home" / "data", ops_kind="native", check_locks=False)
    ctx.journal = Journal.create(run_id, {"steps": steps, "options": ctx.options, "ops": "native"}, root=tmp / "runs")
    ctx.journal.write({"t": "run", "run": run_id}, sync=True)
    X.run(ctx)
    """
)


def test_a_crash_after_the_recycle_bin_is_finished_from_the_bin(tmp_path: Path) -> None:
    """The file went to the recycle bin, the journal never heard: recovery finds it there (Windows:
    its $I record; freedesktop: its .trashinfo) by name and identity, and undo can put it back."""
    data = os.urandom(30000)
    keeper = _write(tmp_path / "k" / "b.bin", data)
    copy = _write(tmp_path / "c" / "b.bin", data)
    ident = _ident(copy)
    steps = [_step(1, "trash", copy, keeper)]
    sf = tmp_path / "steps.json"
    sf.write_text(json.dumps(steps))
    run_id = "r-20261024-000002-bin1"
    proc = subprocess.run(
        [sys.executable, "-c", CRASH, str(tmp_path), str(sf), run_id, "before_done@1:exit"],
        capture_output=True,
        timeout=120,
        check=False,
    )
    assert proc.returncode == 137, proc.stderr.decode(errors="replace")[-2000:]
    run_dir = tmp_path / "runs" / run_id
    ctx = X.RunCtx(
        run_id=run_id,
        steps=steps,
        fs=make_fs(Sandbox([str(tmp_path)])),
        options={},
        data_dir=tmp_path / "home" / "data",
        ops_kind="native",
    )
    X.recover(ctx, run_dir)
    st, _, end = fold(read_records(run_dir))
    assert end is not None
    if st[1].result.startswith("done"):
        assert not copy.exists()
        out = X.undo(ctx, run_dir)
        assert out[1][0] == "undone", out
    else:  # nothing moved before the crash
        assert st[1].result == "cancelled"
    assert copy.read_bytes() == data and _ident(copy) == ident


# ------------------------------------------------------------------ Windows: times put back
def _put_times_back(p: Path, mtime_ns: int, change_100ns: int) -> None:  # pragma: no cover - Windows
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
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
    k32.SetFileInformationByHandle.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    k32.CloseHandle.argtypes = [wintypes.HANDLE]
    h = k32.CreateFileW(str(p), 0x100 | 0x80, 7, None, 3, 0x02000000, None)
    raw = bytearray(40)
    raw[16:24] = (mtime_ns // 100 + 116444736000000000).to_bytes(8, "little", signed=True)
    raw[24:32] = change_100ns.to_bytes(8, "little", signed=True)
    buf = ctypes.create_string_buffer(bytes(raw), 40)
    assert k32.SetFileInformationByHandle(h, 0, buf, 40)
    k32.CloseHandle(h)


def _change_100ns(p: Path) -> int:  # pragma: no cover - Windows
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
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
    k32.GetFileInformationByHandleEx.argtypes = [
        wintypes.HANDLE,
        ctypes.c_int,
        ctypes.c_void_p,
        wintypes.DWORD,
    ]
    k32.CloseHandle.argtypes = [wintypes.HANDLE]
    h = k32.CreateFileW(str(p), 0x80, 7, None, 3, 0x02000000, None)
    buf = ctypes.create_string_buffer(40)
    assert k32.GetFileInformationByHandleEx(h, 0, buf, 40)
    k32.CloseHandle(h)
    return int.from_bytes(buf.raw[24:32], "little", signed=True)


@windows_only
@pytest.mark.parametrize("action", ["hardlink", "trash"])
def test_contents_changed_with_both_times_put_back_are_found(tmp_path: Path, action: str) -> None:
    """On a volume without a USN (WinBtrfs) a program that writes and then puts the modification and
    change times back leaves no stamp moved: the step reads the contents again (a replacement while
    it keeps others from writing) and leaves the file alone."""
    data = os.urandom(90000)
    keeper = _write(tmp_path / "k" / "s.bin", data)
    copy = _write(tmp_path / "c" / "s.bin", data)
    step = _step(1, action, copy, keeper, verify=True)
    step["usn"] = 0  # as on a volume without a USN journal
    for k in step["keepers"]:
        k["usn"] = 0
    ctx = _ctx(tmp_path, [step])
    skip = X.preflight(ctx, [step])
    assert not skip
    mtime, change = os.stat(copy).st_mtime_ns, _change_100ns(copy)
    with open(copy, "r+b") as f:
        f.seek(40000)
        f.write(b"\xa5" * 4096)
    _put_times_back(copy, mtime, change)
    X.run(ctx, skip=skip)
    assert ctx.outcomes[1].code == "skipped_changed", (ctx.outcomes[1].code, ctx.outcomes[1].why)
    assert copy.exists() and copy.read_bytes() != data and keeper.read_bytes() == data


# ------------------------------------------------------------------ Windows block clones (faked volume)
class _FakeCloneFs:
    swaps = False
    kind = "windows"

    def __init__(self, a: bytes, b: bytes) -> None:
        self.data = {1: a, 2: b}
        self.cloned: list[tuple[int, int]] = []

    def fault(self, phase: str) -> None:
        return None

    def read_at(self, h: int, n: int, off: int) -> bytes:
        return self.data[h][off : off + n]

    def clone_range(self, ch: int, kh: int, off: int, n: int) -> None:
        self.cloned.append((off, n))

    def fstat(self, h: int) -> St:
        return St(1, h, 0, len(self.data[h]), 5, 5, 1, "file")


def test_a_windows_block_clone_that_finds_other_contents_says_the_copy_changed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The segments are compared under handles that keep others from writing; one that differs
    (the copy changed after the review without its times moving) ends the step as 「变过」, the
    segments before it shared (they held the same bytes)."""
    from spaceknife.dupes.cleanup import winfs

    seg = 1 << 16
    monkeypatch.setattr(X, "CLONE_SEG", seg)
    keeper = b"k" * (2 * seg + 8192)
    copy = bytearray(keeper)
    copy[seg + 100] ^= 0xFF
    fs = _FakeCloneFs(bytes(copy), keeper)
    monkeypatch.setattr(winfs, "suspend_times", lambda h: True)
    monkeypatch.setattr(winfs, "cluster_size", lambda folder: 4096)
    monkeypatch.setattr(winfs, "integrity", lambda h: None)
    ctx = SimpleNamespace(fs=fs, permit=None, bad_volumes=set())
    s = {"seq": 1, "vol": 0, "path": str(tmp_path / "c"), "target": {"path": str(tmp_path / "k")}}
    o = X._clone_windows(ctx, s, 1, fs.fstat(1), 2, fs.fstat(2), len(keeper), str(tmp_path))  # type: ignore[arg-type]
    assert (o.code, o.why, o.shared) == ("skipped_changed", "contents differ", seg)
    assert fs.cloned == [(0, seg)]


# ------------------------------------------------------------------ Linux volumes
@linux_only
def test_a_nested_btrfs_subvolume_is_on_the_btrfs_above(monkeypatch: pytest.MonkeyPatch) -> None:
    """A subvolume has a device number of its own that changes at the subvolume, which is no mount
    point: its files are on the btrfs mounted above (else nothing can be done there)."""
    from spaceknife.scan import volume as vol

    mounts = [vol._Mount("/", "ext4", "/dev/sda1", 8, 1), vol._Mount("/data/b", "btrfs", "/dev/loop2", 7, 2)]
    fake_os = SimpleNamespace(
        stat=lambda p: SimpleNamespace(st_dev=os.makedev(0, 77)),
        major=os.major,
        minor=os.minor,
        path=SimpleNamespace(realpath=lambda p: p),
    )
    monkeypatch.setattr(vol, "os", fake_os)  # this module only
    monkeypatch.setattr(vol, "_linux_mounts", lambda: mounts)
    monkeypatch.setattr(vol, "in_wsl", lambda: False)
    monkeypatch.setattr(vol, "_mount_point_by_dev", lambda p: "/data/b/sub")

    def statvfs(path: str, point: str, fs: str, source: str, remote: bool) -> vol.VolumeInfo:
        return vol.VolumeInfo(point, source, fs, point, 100, 50, 4096, remote, False)

    monkeypatch.setattr(vol, "_posix_from_statvfs", statvfs)
    v = vol._linux_volume("/data/b/sub/x/a.bin")
    assert (v.fs_type, v.mount_point, v.device) == ("btrfs", "/data/b", "/dev/loop2")
    v = vol._linux_volume("/other/sub/a.bin")  # nothing above: unknown, as before
    assert v.fs_type == ""


@linux_only
@pytest.mark.parametrize("reflink", [True, False])
def test_xfs_made_without_reflink_offers_no_block_clone(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, reflink: bool
) -> None:
    import fcntl
    import struct

    from spaceknife.dupes.cleanup import caps

    def ioctl(fd: int, req: int, buf: bytearray, mutate: bool = True) -> int:
        assert req == caps.XFS_IOC_FSGEOMETRY
        struct.pack_into(
            "<I", buf, caps.XFS_GEOM_FLAGS_AT, caps.XFS_FSOP_GEOM_FLAGS_REFLINK if reflink else 0x7CDCA
        )
        return 0

    monkeypatch.setattr(fcntl, "ioctl", ioctl)
    monkeypatch.setattr(caps, "_XFS_REFLINK", {})
    v = SimpleNamespace(
        fs_type="xfs",
        sample_path=str(tmp_path / "a.bin"),
        mount=str(tmp_path),
        network=False,
        index=0,
        name="x",
        dev=1,
        fs_group=b"g",
        removable=False,
    )
    got = caps.volume_caps(v, sys_facts=SimpleNamespace(), probe_exchange=False)
    assert got.clone is reflink
