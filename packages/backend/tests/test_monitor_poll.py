"""Polling watcher (docs/monitor.md §2.4): the mtime table, its rate limit, and the live thread."""

from __future__ import annotations

import os
import stat
import sys
import threading
import time
from collections.abc import Callable
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from spaceknife.monitor.base import Change
from spaceknife.monitor.poll import DirPoller, PollWatcher, listed_changed, stat_file


class Collector:
    """A sink that keeps every batch and lets a test wait for a condition on them."""

    def __init__(self) -> None:
        self.cond = threading.Condition()
        self.batches: list[list[Change]] = []

    def __call__(self, batch: list[Change]) -> None:
        with self.cond:
            self.batches.append(list(batch))
            self.cond.notify_all()

    def changes(self) -> list[Change]:
        with self.cond:
            return [c for b in self.batches for c in b]

    def wait_for(self, pred: Callable[[list[Change]], bool], timeout: float = 5.0) -> list[Change]:
        deadline = time.monotonic() + timeout
        with self.cond:
            while True:
                flat = [c for b in self.batches for c in b]
                if pred(flat):
                    return flat
                left = deadline - time.monotonic()
                if left <= 0:
                    raise AssertionError(f"timed out waiting; got {flat}")
                self.cond.wait(left)


def has(kind: str, path: Path | str, **fields: Any) -> Callable[[list[Change]], bool]:
    def pred(changes: list[Change]) -> bool:
        return any(
            c.kind == kind and c.path == str(path) and all(getattr(c, k) == v for k, v in fields.items())
            for c in changes
        )

    return pred


def wait_until(cond: Callable[[], bool], timeout: float = 5.0) -> None:
    deadline = time.monotonic() + timeout
    while not cond():
        if time.monotonic() > deadline:
            raise AssertionError("timed out")
        time.sleep(0.01)


def bump_mtime(path: Path, seconds: int = 5) -> None:
    st = os.stat(path)
    os.utime(path, ns=(st.st_atime_ns, st.st_mtime_ns + seconds * 1_000_000_000))


# --------------------------------------------------------------------------- DirPoller (clock-driven)


def test_poller_dir_change_and_deleted_folder(tmp_path: Path) -> None:
    a = tmp_path / "a"
    b = a / "b"
    b.mkdir(parents=True)
    p = DirPoller(str(tmp_path), interval=10, rate=1000)
    for d in (tmp_path, a, b):
        assert p.add(str(d))
    assert not p.add(str(a))
    assert len(p) == 3
    assert p.wake_time() == 0.0  # baselines are due at once
    assert p.step(0.0) == []
    assert p.stats == 3
    first = p.wake_time()
    assert first > 0.0 and first != float("inf")

    bump_mtime(b)
    assert p.step(first - 0.001) == []  # not due yet
    assert p.step(first) == [Change("dir", str(b), is_dir=True)]
    assert p.step(first + 1) == []  # the round is over; the next one is interval later
    second = p.wake_time()
    assert second == pytest.approx(first + 10)
    assert p.step(second) == []  # nothing changed

    b.rmdir()
    changes = p.step(p.wake_time())
    assert Change("deleted", str(b), is_dir=True) in changes
    assert str(b) not in p
    assert len(p) == 2


def test_poller_drops_polled_folders_below_a_gone_one(tmp_path: Path) -> None:
    deep = tmp_path / "a" / "b" / "c"
    deep.mkdir(parents=True)
    p = DirPoller(str(tmp_path), interval=1, rate=1000)
    for d in (tmp_path / "a", tmp_path / "a" / "b", deep):
        p.add(str(d))
    p.step(0.0)
    deep.rmdir()
    (tmp_path / "a" / "b").rmdir()
    (tmp_path / "a").rmdir()
    changes = p.step(p.wake_time())
    gone = [c for c in changes if c.kind == "deleted"]
    assert gone == [Change("deleted", str(tmp_path / "a"), is_dir=True)]  # not once per folder below
    assert len(p) == 0


def test_poller_vanished_before_baseline_is_reported(tmp_path: Path) -> None:
    p = DirPoller(str(tmp_path), interval=1, rate=100)
    p.add(str(tmp_path / "never-there"))
    assert p.step(0.0) == [Change("deleted", str(tmp_path / "never-there"), is_dir=True)]


def test_poller_root_lost(tmp_path: Path) -> None:
    root = tmp_path / "root"
    root.mkdir()
    p = DirPoller(str(root), interval=1, rate=100)
    p.add(str(root))
    p.step(0.0)
    root.rmdir()
    assert p.step(p.wake_time()) == [Change("overflow", str(root), is_dir=True)]
    assert p.root_lost


def test_listed_changed() -> None:
    s = 1_700_000_000
    ns = s * 1_000_000_000
    since = ns + 500_000_000  # the scan began half a second into that second
    assert listed_changed(ns + 600_000_000, s, since)  # modified after the scan began
    assert listed_changed(ns + 460_000_000, s, since)  # within a tick of it: cannot tell
    assert not listed_changed(ns + 400_000_000, s, since)  # well before: the listing saw it
    assert listed_changed(ns + 400_000_000, s - 1, since)  # whole seconds differ
    assert listed_changed(ns, s, since)  # a whole second: maybe a file system that keeps no more
    assert not listed_changed(ns, s, since + 3_000_000_000)
    # the scanners' seconds truncate the double sec + nsec * 1e-9, which can round up
    assert not listed_changed(ns + 999_999_999, s + 1, since + 10_000_000_000)
    assert listed_changed(ns + 999_999_999, s, since + 10_000_000_000)


def test_poller_baseline_checks_what_the_scan_recorded(tmp_path: Path) -> None:
    """A folder handed in with the mtime the scan recorded is checked when its baseline is taken:
    a change made after the scan listed it, before polling began, is reported (``dir``)."""
    same, moved, recent, plain = (tmp_path / n for n in ("same", "moved", "recent", "plain"))
    for d in (same, moved, recent, plain):
        d.mkdir()
    secs = {d: int(os.stat(d).st_mtime) for d in (same, moved, recent)}
    later = time.time_ns() + 3600 * 1_000_000_000  # the scan started after every one of these times
    p = DirPoller(str(tmp_path), interval=60, rate=1000)
    assert p.add(str(same), secs[same], later)
    assert p.add(str(moved), secs[moved] - 5, later)  # whole seconds differ
    assert p.add(str(recent), secs[recent], os.stat(recent).st_mtime_ns)  # changed after the scan began
    assert p.add(str(plain))  # nothing recorded: the baseline as before
    assert p.step(0.0) == [Change("dir", str(moved), is_dir=True), Change("dir", str(recent), is_dir=True)]
    assert p.step(0.0) == []


def test_poller_root_checks_what_the_scan_recorded_once(tmp_path: Path) -> None:
    """The root is polled from the start, before the scan read it: the record the scan hands in
    later is checked against it once."""
    p = DirPoller(str(tmp_path), interval=60, rate=1000)
    assert p.add(str(tmp_path))
    assert p.step(0.0) == []  # the baseline
    secs = int(os.stat(tmp_path).st_mtime)
    later = time.time_ns() + 3600 * 1_000_000_000
    assert not p.add(str(tmp_path), secs - 5, later)
    assert p.step(0.0) == [Change("dir", str(tmp_path), is_dir=True)]
    assert not p.add(str(tmp_path), secs - 5, later)  # once only
    assert p.step(0.0) == []


def test_poller_rename_tree_rekeys_below(tmp_path: Path) -> None:
    sep = os.sep
    p = DirPoller(str(tmp_path), interval=1, rate=100)
    old = f"{tmp_path}{sep}a"
    for path in (old, f"{old}{sep}x", f"{old}{sep}x{sep}y", f"{old}b"):
        p.add(path)
    p.rename_tree(old, f"{tmp_path}{sep}z")
    assert f"{tmp_path}{sep}z" in p
    assert f"{tmp_path}{sep}z{sep}x{sep}y" in p
    assert old not in p
    assert f"{old}b" in p  # a sibling that merely shares the prefix stays


def _fake_stat(monkeypatch: pytest.MonkeyPatch, folders: dict[str, int]) -> list[str]:
    """Replace os.stat with a counter over fake folders (no disk access at all)."""
    calls: list[str] = []
    real = os.stat

    def fake(path: Any, *args: Any, **kwargs: Any) -> Any:
        key = os.fspath(path)
        if key in folders:
            calls.append(key)
            return SimpleNamespace(st_mode=stat.S_IFDIR | 0o755, st_mtime_ns=folders[key])
        return real(path, *args, **kwargs)

    monkeypatch.setattr(os, "stat", fake)
    return calls


@pytest.mark.parametrize(("rate", "count"), [(10, 40), (2000, 5000)])
def test_poller_rate_limit(monkeypatch: pytest.MonkeyPatch, rate: int, count: int) -> None:
    root = os.path.join(os.sep, "fake-root")
    folders = {os.path.join(root, f"d{i}"): 1 for i in range(count)}
    calls = _fake_stat(monkeypatch, folders)
    p = DirPoller(root, interval=100, rate=rate)
    for path in folders:
        p.add(path)
    now = 0.0
    while p.wake_time() <= now:  # baselines: chunked, not rate limited (cache-hot folders)
        p.step(now)
    assert len(calls) == count
    calls.clear()

    stamps: list[tuple[float, int]] = []  # (time, stats made by that step)
    now = p.wake_time()
    start = now
    while True:
        before = len(calls)
        p.step(now)
        stamps.append((now, len(calls) - before))
        if len(calls) >= count:
            break
        now = p.wake_time()
        assert now > stamps[-1][0]  # batches are spaced out in time
    assert sum(n for _, n in stamps) == count
    for t0, _ in stamps:  # at most `rate` stats in any one-second window (one batch of slack)
        in_window = sum(n for t, n in stamps if t0 <= t < t0 + 1.0)
        assert in_window <= rate + p.batch
    assert now - start >= (count - p.batch) / rate - 1e-6
    assert p.wake_time() == pytest.approx(start + 100)  # next round: one interval after this one began


# --------------------------------------------------------------------------- PollWatcher (thread)


def _start(root: Path, interval: float = 0.1) -> tuple[PollWatcher, Collector]:
    w = PollWatcher(str(root), poll_interval=interval, poll_rate=1000)
    got = Collector()
    w.start(got)
    return w, got


@pytest.fixture
def tree(tmp_path: Path) -> Path:
    root = tmp_path / "root"
    (root / "a" / "b" / "c").mkdir(parents=True)
    return root


def test_poll_watcher_dir_and_deleted(tree: Path) -> None:
    a, b, c = tree / "a", tree / "a" / "b", tree / "a" / "b" / "c"
    w, got = _start(tree)
    try:
        assert w.name == "poll"
        w.add_dirs([str(a), str(b), str(c)])
        wait_until(lambda: w.status().polled_dirs == 4)
        time.sleep(0.15)  # baselines are taken right away; let one round pass
        bump_mtime(b)
        got.wait_for(has("dir", b, is_dir=True))
        c.rmdir()
        got.wait_for(has("deleted", c, is_dir=True))
        st = w.status()
        assert st.state == "degraded" and "轮询" in st.detail
        assert st.watched_dirs == 0 and st.polled_dirs == 3
        assert st.events == len(got.changes()) and st.overflows == 0
    finally:
        t0 = time.monotonic()
        w.stop()
        assert time.monotonic() - t0 < 1.0
    assert w.status().state == "stopped"
    seen = len(got.changes())
    bump_mtime(a)
    time.sleep(0.3)
    assert len(got.changes()) == seen  # no sink calls after stop()
    w.stop()  # idempotent


def test_poll_watcher_root_gone(tree: Path) -> None:
    w, got = _start(tree / "a" / "b" / "c")
    try:
        wait_until(lambda: w.status().polled_dirs == 1)
        time.sleep(0.05)
        (tree / "a" / "b" / "c").rmdir()
        got.wait_for(has("overflow", tree / "a" / "b" / "c"))
        wait_until(lambda: w.status().state == "failed")
        assert w.status().overflows == 1
    finally:
        w.stop()
    assert w.status().state == "failed"


def test_poll_watcher_live_rate_limit(tree: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Real thread, real clock: stats per second stay under poll_rate (upper bound only)."""
    folders = [tree / f"d{i:02d}" for i in range(30)]
    for d in folders:
        d.mkdir()
    lock = threading.Lock()
    stamps: list[float] = []
    real = os.stat
    prefix = str(tree)

    def counting(path: Any, *args: Any, **kwargs: Any) -> Any:
        if os.fspath(path).startswith(prefix):
            with lock:
                stamps.append(time.monotonic())
        return real(path, *args, **kwargs)

    w = PollWatcher(str(tree), poll_interval=0.05, poll_rate=20)
    w.start(Collector())
    try:
        w.add_dirs(str(d) for d in folders)
        wait_until(lambda: w.status().polled_dirs == 31)
        time.sleep(0.05)
        monkeypatch.setattr(os, "stat", counting)
        time.sleep(1.2)
    finally:
        w.stop()
        monkeypatch.undo()
    with lock:
        seen = list(stamps)
    assert seen, "the watcher did not poll at all"
    for t0 in seen:
        assert sum(1 for t in seen if t0 <= t < t0 + 1.0) <= 20 + 1


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX numbers (st_blocks)")
def test_stat_file_numbers(tmp_path: Path) -> None:
    f = tmp_path / "f.bin"
    f.write_bytes(b"x" * 10)
    size, alloc, mtime = stat_file(str(f)) or (0, 0, 0)
    assert size == 10 and alloc >= 0 and mtime == int(os.stat(f).st_mtime)
    assert stat_file(str(tmp_path)) is None
    assert stat_file(str(tmp_path / "missing")) is None


def test_stat_file_is_posix_only() -> None:
    if sys.platform == "win32":
        assert stat_file(__file__) is None
