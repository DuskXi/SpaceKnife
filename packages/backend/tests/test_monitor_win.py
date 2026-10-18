"""Windows watcher (docs/monitor.md §2.1): ReadDirectoryChangesExW on small temporary trees.

Every wait polls with a timeout; nothing here touches anything outside pytest's tmp_path.
"""

from __future__ import annotations

import os
import shutil
import struct
import sys
import threading
import time
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest

from spaceknife.monitor import Change

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="ReadDirectoryChangesExW is Windows-only")

if sys.platform == "win32":
    from spaceknife.monitor import create_watcher
    from spaceknife.monitor.win import (
        NO_EXTENDED,
        ROOT_DELETED,
        ROOT_MOVED,
        NotifyDecoder,
        RdcwWatcher,
        is_network_path,
    )

TIMEOUT = 5.0


class Collector:
    """A sink that records its batches; tests wait on it (with a timeout, never a fixed sleep)."""

    def __init__(self) -> None:
        self.batches: list[list[Change]] = []
        self.threads: set[str] = set()
        self.cond = threading.Condition()

    def __call__(self, batch: list[Change]) -> None:
        with self.cond:
            self.batches.append(list(batch))
            self.threads.add(threading.current_thread().name)
            self.cond.notify_all()

    @property
    def changes(self) -> list[Change]:
        with self.cond:
            return [c for b in self.batches for c in b]

    def wait_for(self, pred: Callable[[Change], bool], timeout: float = TIMEOUT) -> Change:
        deadline = time.monotonic() + timeout
        with self.cond:
            while True:
                for change in self.changes:
                    if pred(change):
                        return change
                left = deadline - time.monotonic()
                if left <= 0:
                    raise AssertionError(f"no matching change within {timeout} s; got {self.changes}")
                self.cond.wait(left)


def wait_until(cond: Callable[[], bool], timeout: float = TIMEOUT) -> None:
    deadline = time.monotonic() + timeout
    while not cond():
        if time.monotonic() > deadline:
            raise AssertionError(f"condition not met within {timeout} s")
        time.sleep(0.01)


def start(root: Path, sink: Collector | None = None, **kw: int) -> tuple[RdcwWatcher, Collector]:
    watcher = RdcwWatcher(str(root), **kw)
    sink = sink or Collector()
    watcher.start(sink)
    wait_until(lambda: watcher.status().state == "running")
    return watcher, sink


@pytest.fixture
def watched(tmp_path: Path) -> Iterator[tuple[Path, RdcwWatcher, Collector]]:
    root = tmp_path / "root"
    root.mkdir()
    watcher, sink = start(root)
    try:
        yield root, watcher, sink
    finally:
        watcher.stop()


def is_kind(kind: str, path: Path | str) -> Callable[[Change], bool]:
    return lambda c: c.kind == kind and c.path == str(path)


# --------------------------------------------------------------------------- live watching


def test_file_lifecycle(watched: tuple[Path, RdcwWatcher, Collector]) -> None:
    root, watcher, sink = watched
    p = root / "a.txt"
    p.write_bytes(b"x")
    assert sink.wait_for(is_kind("created", p)).is_dir is False

    with p.open("ab") as f:
        f.write(b"y" * 5000)
    mod = sink.wait_for(lambda c: is_kind("modified", p)(c) and c.size == 5001)
    cluster = watcher._decoder.cluster
    assert mod.is_dir is False
    assert mod.alloc is not None and mod.alloc >= 5001 and (cluster == 0 or mod.alloc % cluster == 0)
    assert mod.mtime is not None and abs(mod.mtime - int(os.stat(p).st_mtime)) <= 2

    q = root / "b.txt"
    p.rename(q)
    ren = sink.wait_for(is_kind("renamed", q))
    assert (ren.old_path, ren.is_dir) == (str(p), False)

    q.unlink()
    assert sink.wait_for(is_kind("deleted", q)).is_dir is False

    status = watcher.status()
    assert (status.state, status.detail, status.watched_dirs, status.overflows) == ("running", "", -1, 0)
    assert status.events == len(sink.changes) >= 4
    assert threading.current_thread().name not in sink.threads  # the sink runs on the watcher's thread
    assert all(c.path.startswith(str(root) + "\\") for c in sink.changes)


def test_folders(watched: tuple[Path, RdcwWatcher, Collector]) -> None:
    root, _, sink = watched
    d = root / "d"
    d.mkdir()
    assert sink.wait_for(is_kind("created", d)).is_dir is True
    (d / "c.bin").write_bytes(b"")
    assert sink.wait_for(is_kind("created", d / "c.bin")).is_dir is False

    e = root / "e"
    d.rename(e)
    ren = sink.wait_for(is_kind("renamed", e))
    assert (ren.old_path, ren.is_dir) == (str(d), True)

    (e / "c.bin").write_bytes(b"z" * 10)  # paths below the renamed folder use its new name
    sink.wait_for(lambda c: is_kind("modified", e / "c.bin")(c) and c.size == 10)
    # a folder's own MODIFIED (its times changed because an entry did) is not reported
    assert all(c.is_dir is False for c in sink.changes if c.kind == "modified")


def test_unusual_names(watched: tuple[Path, RdcwWatcher, Collector]) -> None:
    root, _, sink = watched
    names = ["报告 😀.txt", "lone\ud800surrogate.txt"]
    for name in names:
        try:
            (root / name).write_bytes(b"1")
        except (OSError, UnicodeError) as exc:  # pragma: no cover - depends on the filesystem
            pytest.skip(f"cannot create {name!r}: {exc}")
    for name in names:
        sink.wait_for(is_kind("created", str(root) + "\\" + name))  # lone surrogates kept, as in the scan


def test_overflow_reported_and_watching_goes_on(tmp_path: Path) -> None:
    root = tmp_path / "root"
    root.mkdir()
    entered, release = threading.Event(), threading.Event()

    class Gated(Collector):
        """Holds the watcher's thread on the first batch so the kernel's tiny buffer overflows."""

        def __call__(self, batch: list[Change]) -> None:
            super().__call__(batch)
            if not entered.is_set():
                entered.set()
                release.wait(TIMEOUT)

    watcher, sink = start(root, Gated(), buffer_size=512)
    try:
        assert watcher.buffer_size == 512
        (root / "first.txt").write_bytes(b"")
        assert entered.wait(TIMEOUT)
        for i in range(300):
            (root / f"f{i:03d}.txt").write_bytes(b"")
        release.set()

        assert sink.wait_for(lambda c: c.kind == "overflow").path == str(root)
        status = watcher.status()
        assert status.state == "running" and status.overflows >= 1

        (root / "after.txt").write_bytes(b"")  # still watching
        sink.wait_for(is_kind("created", root / "after.txt"))
    finally:
        release.set()
        watcher.stop()


def test_stop_is_quick_and_final(watched: tuple[Path, RdcwWatcher, Collector]) -> None:
    root, watcher, sink = watched
    (root / "x.txt").write_bytes(b"")
    sink.wait_for(is_kind("created", root / "x.txt"))

    t0 = time.monotonic()
    watcher.stop()
    assert time.monotonic() - t0 < 1.0
    assert watcher._thread is not None and not watcher._thread.is_alive()
    calls = len(sink.batches)
    for i in range(20):
        (root / f"y{i}.txt").write_bytes(b"")
    time.sleep(0.2)  # room for a stray call, which must not come
    assert len(sink.batches) == calls

    status = watcher.status()
    assert (status.state, status.watched_dirs) == ("stopped", 0)
    t0 = time.monotonic()
    watcher.stop()  # idempotent
    assert time.monotonic() - t0 < 0.1
    with pytest.raises(RuntimeError):
        watcher.start(sink)


def test_stop_from_the_sink(tmp_path: Path) -> None:
    root = tmp_path / "root"
    root.mkdir()
    holder: list[RdcwWatcher] = []

    class Stopper(Collector):
        def __call__(self, batch: list[Change]) -> None:
            super().__call__(batch)
            holder[0].stop()  # on the watcher's own thread: must neither deadlock nor raise

    sink = Stopper()
    watcher = RdcwWatcher(str(root))
    holder.append(watcher)
    watcher.start(sink)
    wait_until(lambda: watcher.status().state in ("running", "stopped"))
    (root / "a.txt").write_bytes(b"")
    (root / "b.txt").write_bytes(b"")
    wait_until(lambda: watcher._thread is not None and not watcher._thread.is_alive())
    assert len(sink.batches) == 1
    assert watcher.status().state == "stopped"


def test_deleted_root_fails(watched: tuple[Path, RdcwWatcher, Collector]) -> None:
    root, watcher, sink = watched
    (root / "z.txt").write_bytes(b"")
    sink.wait_for(is_kind("created", root / "z.txt"))

    shutil.rmtree(root)
    sink.wait_for(is_kind("overflow", root))
    status = watcher.status()
    assert (status.state, status.detail, status.watched_dirs) == ("failed", ROOT_DELETED, 0)
    assert status.overflows == 1
    wait_until(lambda: watcher._thread is not None and not watcher._thread.is_alive())
    watcher.stop()
    assert watcher.status().state == "failed"  # the reason stays visible


def test_moved_root_fails(tmp_path: Path) -> None:
    root = tmp_path / "root"
    root.mkdir()
    watcher, sink = start(root)
    try:
        root.rename(tmp_path / "moved")  # events would still come, but under the old path
        sink.wait_for(is_kind("overflow", root))
        status = watcher.status()
        assert (status.state, status.detail) == ("failed", ROOT_MOVED)
    finally:
        watcher.stop()


def test_missing_root_fails_without_overflow(tmp_path: Path) -> None:
    watcher = RdcwWatcher(str(tmp_path / "nope"))
    sink = Collector()
    watcher.start(sink)
    wait_until(lambda: watcher.status().state == "failed")
    assert watcher.status().detail.startswith("无法监视此文件夹：")
    wait_until(lambda: watcher._thread is not None and not watcher._thread.is_alive())
    assert sink.batches == []  # nothing was ever watched, so nothing was lost
    watcher.stop()


def test_plain_call_fallback(tmp_path: Path) -> None:
    root = tmp_path / "root"
    root.mkdir()
    watcher = RdcwWatcher(str(root))
    watcher.extended = False  # what an old Windows or a foreign filesystem gets
    sink = Collector()
    watcher.start(sink)
    try:
        wait_until(lambda: watcher.status().state == "running")
        assert watcher.status().detail == NO_EXTENDED
        p = root / "a.txt"
        p.write_bytes(b"x")
        assert sink.wait_for(is_kind("created", p)).is_dir is None
        with p.open("ab") as f:
            f.write(b"y" * 100)
        mod = sink.wait_for(is_kind("modified", p))
        assert (mod.size, mod.alloc, mod.mtime, mod.is_dir) == (None, None, None, None)
        q = root / "b.txt"
        p.rename(q)
        assert sink.wait_for(is_kind("renamed", q)).old_path == str(p)
    finally:
        watcher.stop()


def test_buffer_sizes_and_factory(tmp_path: Path) -> None:
    assert not is_network_path(str(tmp_path))
    assert is_network_path(r"\\server\share\folder")  # decided from the string: no network lookup
    assert is_network_path(r"\\?\UNC\server\share")
    assert RdcwWatcher(str(tmp_path), buffer_size=1001).buffer_size == 1008  # DWORD-aligned
    watcher, _ = start(tmp_path)
    try:
        assert watcher.buffer_size == 1 << 20
        assert watcher.extended
    finally:
        watcher.stop()
    made = create_watcher(str(tmp_path))
    assert isinstance(made, RdcwWatcher) and made.name == "rdcw"
    made.add_dirs([str(tmp_path)])  # recursive: ignored
    made.stop()  # never started: fine


# --------------------------------------------------------------------------- decoding

_EXT = struct.Struct("<II8xq16xqqI20xI")
_STD = struct.Struct("<III")
_FT_1970 = 11_644_473_600 * 10_000_000


def ext(action: int, name: str, *, size: int = 0, alloc: int = 0, ft: int = 0, attrs: int = 0x20) -> bytes:
    raw = name.encode("utf-16-le", "surrogatepass")
    return _EXT.pack(0, action, ft, alloc, size, attrs, len(raw)) + raw


def std(action: int, name: str) -> bytes:
    raw = name.encode("utf-16-le", "surrogatepass")
    return _STD.pack(0, action, len(raw)) + raw


def chain(*records: bytes) -> bytes:
    """Link records like the kernel does: DWORD-aligned, NextEntryOffset 0 on the last."""
    out = b""
    for i, rec in enumerate(records):
        rec += b"\0" * (-len(rec) % 4)
        nxt = len(rec) if i < len(records) - 1 else 0
        out += struct.pack("<I", nxt) + rec[4:]
    return out


def test_decode_actions_and_numbers() -> None:
    dec = NotifyDecoder("C:\\r\\", cluster=4096)
    t = 1_700_000_000
    got = dec.decode(
        chain(
            ext(1, "new", attrs=0x10),
            ext(3, "a.bin", size=5001, alloc=8192, ft=_FT_1970 + t * 10_000_000 + 1234),
            ext(3, "small.txt", size=3, alloc=8),  # resident: no clusters
            ext(3, "packed.dat", size=9000, alloc=12288, attrs=0x820),  # compressed: unknown
            ext(3, "cloud.dat", size=9000, alloc=12288, attrs=0x400020),  # placeholder: 0, like the scan
            ext(3, "new", attrs=0x10),  # a folder's own MODIFIED: dropped
            ext(3, "old.txt", size=1, alloc=8, ft=5),  # FILETIME before 1970: 0
            ext(2, "gone\\x", attrs=0x20),
        )
    )
    assert got == [
        Change("created", "C:\\r\\new", is_dir=True),
        Change("modified", "C:\\r\\a.bin", is_dir=False, size=5001, alloc=8192, mtime=t),
        Change("modified", "C:\\r\\small.txt", is_dir=False, size=3, alloc=0, mtime=0),
        Change("modified", "C:\\r\\packed.dat", is_dir=False, size=9000, alloc=None, mtime=0),
        Change("modified", "C:\\r\\cloud.dat", is_dir=False, size=9000, alloc=0, mtime=0),
        Change("modified", "C:\\r\\old.txt", is_dir=False, size=1, alloc=0, mtime=0),
        Change("deleted", "C:\\r\\gone\\x", is_dir=False),
    ]


def test_decode_keeps_the_last_modified_per_path() -> None:
    dec = NotifyDecoder("C:\\", cluster=0)
    got = dec.decode(
        chain(ext(3, "a", size=1, alloc=8), ext(1, "b"), ext(3, "a", size=2, alloc=8), ext(3, "c", size=7))
    )
    assert [(c.kind, c.path, c.size) for c in got] == [
        ("created", "C:\\b", None),
        ("modified", "C:\\a", 2),
        ("modified", "C:\\c", 7),
    ]
    assert got[1].alloc == 8  # cluster size unknown: AllocatedLength as is


def test_decode_renames_across_reads() -> None:
    dec = NotifyDecoder("C:\\r\\")
    assert dec.decode(chain(ext(1, "a"), ext(4, "x", attrs=0x10))) == [
        Change("created", "C:\\r\\a", is_dir=False)
    ]
    assert dec.held == ("C:\\r\\x", True)
    assert dec.decode(chain(ext(5, "y", attrs=0x10))) == [
        Change("renamed", "C:\\r\\y", old_path="C:\\r\\x", is_dir=True)
    ]
    # an old name without its new one: that name is gone
    dec.decode(chain(ext(4, "p")))
    assert dec.decode(chain(ext(1, "q"))) == [
        Change("deleted", "C:\\r\\p", is_dir=False),
        Change("created", "C:\\r\\q", is_dir=False),
    ]
    dec.decode(chain(ext(4, "s")))
    assert dec.flush() == [Change("deleted", "C:\\r\\s", is_dir=False)] and dec.held is None
    # a new name alone (its old name was in a lost buffer)
    assert dec.decode(chain(ext(5, "t"))) == [Change("renamed", "C:\\r\\t", is_dir=False)]


def test_decode_plain_records_and_names() -> None:
    dec = NotifyDecoder("\\\\srv\\share\\", extended=False)
    got = dec.decode(chain(std(1, "报告\ud800.txt"), std(3, "a"), std(4, "a"), std(5, "b"), std(2, "c")))
    assert got == [
        Change("created", "\\\\srv\\share\\报告\ud800.txt"),
        Change("modified", "\\\\srv\\share\\a"),
        Change("renamed", "\\\\srv\\share\\b", old_path="\\\\srv\\share\\a"),
        Change("deleted", "\\\\srv\\share\\c"),
    ]
    assert dec.decode(b"") == []
