"""Generic polling watcher (docs/monitor.md §2.4) and the mtime reconciliation inotify reuses.

A folder's mtime changes whenever an entry is added, removed or renamed in it, so re-stat'ing the
scanned folders finds every structural change, at most ``poll_interval`` seconds (plus the round's
own duration) late. File content changes stay invisible: a file's mtime does not reach its folder.

:class:`DirPoller` is the clock-driven core (no thread, no locks): its owner calls
:meth:`DirPoller.step` whenever :meth:`DirPoller.wake_time` has come. A round stats at most
``poll_rate`` folders per second, in small batches spaced out in time, so polling stays a light
background load. The baseline of a newly added folder is taken right away (in chunks, not rate
limited): the scan has just listed that folder, so the stat is served from the inode cache. Still,
the folder may have changed between the scan's listing and that baseline: when the scan's record of
its mtime comes along (``add_dirs``), the baseline is checked against it (:func:`listed_changed`)
and a difference is reported as ``dir``, instead of being taken as the starting point.

Memory: about 120 B plus the path length per polled folder (measured: 163 B at 44-character paths).
"""

from __future__ import annotations

import math
import os
import stat
import sys
import threading
import time
from collections import deque
from collections.abc import Iterable, Sequence

from loguru import logger

from ..i18n import Texts
from .base import Change, Sink, WatchState, WatchStatus

# User-facing text (M7.5): the watch state of the polling watcher.
TEXT = Texts(
    {
        "started": "监视器已经启动",
        "polling": "未使用系统的文件变化通知，改为每 {interval} 秒轮询一次文件夹；文件大小的变化发现不了",
        "root_gone": "扫描根目录已不存在，实时监控已停止",
        "crashed": "实时监控出错，已停止",
        "log_thread": "轮询监视线程出错：{path}",
        "log_sink": "监视器回调出错",
    }
)

_PENDING = -1  # baseline not taken yet
_UNKNOWN = -2  # the stat failed for another reason than "gone" (permissions, I/O error): retried
_BASELINE_CHUNK = 512  # baselines per step (keeps the owner's loop responsive)
_MAX_BATCH = 256
_U32_MAX = 2**32 - 1


def _secs(t: float) -> int:
    if t <= 0:
        return 0
    return _U32_MAX if t >= _U32_MAX else int(t)


def stat_file(path: str) -> tuple[int, int, int] | None:
    """``(size, alloc, mtime)`` of a non-folder entry as the POSIX scanner records them, or None when
    it vanished or is a folder. Shared by the POSIX watchers for ``modified`` changes."""
    if sys.platform == "win32":  # no st_blocks (and the Windows watcher reads its numbers itself)
        return None
    try:
        st = os.stat(path, follow_symlinks=False)
    except OSError:
        return None
    if stat.S_ISDIR(st.st_mode):
        return None
    return st.st_size, st.st_blocks * 512, _secs(st.st_mtime)


# A file time is stamped from a clock that may lag the wall clock by a tick (and kept to 10 ms on
# exFAT); file systems that keep whole seconds (ext3, HFS+) or even two (FAT) truncate more.
_FINE_SLACK_NS = 50_000_000
_COARSE_SLACK_NS = 2_100_000_000


def listed_changed(mtime_ns: int, listed: int, since_ns: int) -> bool:
    """Whether a folder whose mtime is now ``mtime_ns`` may have changed after the scan listed it.

    ``listed``: its mtime as the scan recorded it, in whole seconds (the tree's ``mtime`` column:
    the scanners truncate the same double ``st_mtime`` is, ``sec + nsec * 1e-9``); ``since_ns``: a
    wall-clock time no later than that listing (the scan's start). A folder's mtime moves whenever
    an entry is added, removed or renamed in it. Whole seconds that differ tell. Within the same
    second "before the listing" and "after it" look alike, so a folder modified at or after
    ``since_ns`` is taken as changed (one more listing of it): less the slack its file system's
    time stamps need, a whole second's worth when the mtime is a whole second.
    """
    sec, nsec = divmod(mtime_ns, 1_000_000_000)
    if _secs(sec + nsec * 1e-9) != listed:
        return True
    return mtime_ns >= since_ns - (_FINE_SLACK_NS if nsec else _COARSE_SLACK_NS)


class Backlog:
    """Folders handed to a watcher (``add_dirs``), batch by batch as they came: each batch is the
    caller's list itself (a list is not copied: it must not change afterwards) with the mtimes the
    scan recorded for it (4 B each in an ``array``), nothing per folder on top.

    :meth:`add` is safe from any thread; :meth:`peek` and :meth:`advance` belong to the watcher's
    own thread. A batch leaves only once all of it was placed, so while ``bool(backlog)`` is true
    some folder handed in is neither watched nor polled yet.
    """

    def __init__(self) -> None:
        self._batches: deque[tuple[list[str], Sequence[int] | None, int]] = deque()
        self._at = 0  # folders of the first batch already placed

    def __bool__(self) -> bool:
        return bool(self._batches)

    def add(self, dirs: Iterable[str], mtimes: Sequence[int] | None = None, since_ns: int = 0) -> None:
        paths = dirs if isinstance(dirs, list) else list(dirs)
        if mtimes is not None and len(mtimes) != len(paths):
            raise ValueError(f"add_dirs: {len(paths)} folders, {len(mtimes)} mtimes")
        if paths:
            self._batches.append((paths, mtimes, since_ns))

    def peek(self, n: int) -> list[tuple[str, int | None, int]]:
        """Up to ``n`` of the next folders, all of one batch: (path, recorded mtime, since)."""
        if not self._batches:
            return []
        paths, mtimes, since = self._batches[0]
        end = min(len(paths), self._at + n)
        if mtimes is None:
            return [(paths[i], None, since) for i in range(self._at, end)]
        return [(paths[i], int(mtimes[i]), since) for i in range(self._at, end)]

    def advance(self, n: int) -> None:
        """The ``n`` folders :meth:`peek` gave were placed."""
        self._at += n
        if self._batches and self._at >= len(self._batches[0][0]):
            self._batches.popleft()
            self._at = 0


_SEPS = (os.sep, os.altsep) if os.altsep else (os.sep,)


def _tree_prefixes(path: str) -> tuple[str, ...]:
    """Prefixes of every path strictly below the folder ``path``."""
    if path.endswith(_SEPS):  # a file system root ("/", "C:\\")
        return (path,)
    return tuple(path + s for s in _SEPS)


class DirPoller:
    """mtime table of folders and the rate-limited rounds that compare it (single-threaded)."""

    def __init__(self, root: str, *, interval: float, rate: int) -> None:
        self.root = root  # stat'ed through symlinks (a scan root may be a link); others are not
        self.interval = max(0.01, float(interval))
        self.rate = max(1, int(rate))
        self.batch = max(1, min(_MAX_BATCH, self.rate // 20))  # ≥ 50 ms between batches at most rates
        self.stats = 0  # stat calls so far (diagnostics, tests)
        self.root_lost = False
        self._mtimes: dict[str, int] = {}
        # baselines to take: (path, mtime the scan recorded or None, since): see listed_changed
        self._fresh: deque[tuple[str, int | None, int]] = deque()
        self._root_listed = False  # the scan's record of the root was checked
        self._round: list[str] = []
        self._pos = 0
        self._next_round = time.monotonic() + self.interval
        self._next_batch = 0.0

    def __len__(self) -> int:
        return len(self._mtimes)

    def __contains__(self, path: object) -> bool:
        return path in self._mtimes

    def add(self, path: str, listed: int | None = None, since_ns: int = 0) -> bool:
        """Start polling ``path`` (False: already polled). The baseline is taken at the next step;
        with ``listed`` (the folder's mtime as the scan recorded it, whole seconds) and ``since_ns``
        it is checked first (:func:`listed_changed`): a change since the listing is reported.

        A folder already polled is left as it is: it was handed in again because its row moved (its
        parent was listed again), and its own baseline stands. Only the root, polled from the start
        (before the scan read it), takes the scan's record once, when it comes."""
        if path in self._mtimes:
            if listed is not None and path == self.root and not self._root_listed:
                self._root_listed = True
                self._fresh.append((path, listed, since_ns))
            return False
        self._mtimes[path] = _PENDING
        self._fresh.append((path, listed, since_ns))
        if path == self.root and listed is not None:
            self._root_listed = True
        return True

    def rename_tree(self, old: str, new: str) -> None:
        """A folder moved: re-key it and every polled folder below it (O(polled folders))."""
        if not self._mtimes:
            return
        prefixes = _tree_prefixes(old)
        moved = [(p, m) for p, m in self._mtimes.items() if p == old or p.startswith(prefixes)]
        for p, m in moved:
            del self._mtimes[p]
            self._mtimes[new + p[len(old) :]] = m

    def wake_time(self) -> float:
        """Monotonic time at which :meth:`step` has work (``inf``: nothing to poll)."""
        if self._fresh:
            return 0.0
        if self._round:
            return self._next_batch
        return self._next_round if self._mtimes else math.inf

    def step(self, now: float) -> list[Change]:
        """Do the work that is due at ``now``: one chunk of baselines, or one batch of a round."""
        if self._fresh:
            return self._baselines()
        if not self._round:
            if now < self._next_round or not self._mtimes:
                return []
            self._round = list(self._mtimes)
            self._pos = 0
            self._next_round = now + self.interval  # round starts are poll_interval apart
            self._next_batch = now
        if now < self._next_batch:
            return []
        end = min(len(self._round), self._pos + self.batch)
        batch = self._round[self._pos : end]
        self._pos = end
        if end >= len(self._round):
            self._round = []
        out: list[Change] = []
        gone: list[str] = []
        done = 0
        for path in batch:
            old = self._mtimes.get(path)
            if old is None:
                continue  # removed meanwhile (inside a folder found gone)
            done += 1
            mtime = self._probe(path)
            if mtime is None:
                gone.append(path)
            elif mtime == _UNKNOWN:
                continue
            elif old in (_PENDING, _UNKNOWN):
                self._mtimes[path] = mtime
            elif mtime != old:
                self._mtimes[path] = mtime
                out.append(Change("dir", path, is_dir=True))
        self._next_batch = now + done / self.rate
        if gone:
            out.extend(self._drop(gone))
        return out

    def _baselines(self) -> list[Change]:
        out: list[Change] = []
        gone: list[str] = []
        done = 0
        while self._fresh and done < _BASELINE_CHUNK:
            path, listed, since = self._fresh.popleft()
            old = self._mtimes.get(path)
            if old is None or (old != _PENDING and listed is None):
                continue  # dropped meanwhile, or no baseline to take
            done += 1
            mtime = self._probe(path)
            if mtime is None:
                gone.append(path)  # already gone again: it was in the tree, so report it
                continue
            if mtime == _UNKNOWN and old != _PENDING:
                continue  # the root's check, cannot tell now: its baseline stands
            self._mtimes[path] = mtime
            if listed is not None and mtime != _UNKNOWN and listed_changed(mtime, listed, since):
                out.append(Change("dir", path, is_dir=True))
        if gone:
            out.extend(self._drop(gone))
        return out

    def _probe(self, path: str) -> int | None:
        """The folder's mtime (ns); None: gone or no longer a folder; ``_UNKNOWN``: cannot tell."""
        self.stats += 1
        try:
            st = os.stat(path, follow_symlinks=path == self.root)
        except (FileNotFoundError, NotADirectoryError):
            return None
        except OSError:
            return _UNKNOWN
        return st.st_mtime_ns if stat.S_ISDIR(st.st_mode) else None

    def _drop(self, gone: list[str]) -> list[Change]:
        """Forget folders found gone and every polled folder below them; report the topmost ones."""
        out: list[Change] = []
        prefixes = tuple(p for g in gone for p in _tree_prefixes(g))
        for path in [p for p in self._mtimes if p.startswith(prefixes)]:
            del self._mtimes[path]
        for path in gone:
            self._mtimes.pop(path, None)
            if path.startswith(prefixes):
                continue  # below another gone folder of this batch
            if path == self.root:
                self.root_lost = True
                out.append(Change("overflow", path, is_dir=True))
            else:
                out.append(Change("deleted", path, is_dir=True))
        return out


class PollWatcher:
    """Fallback watcher: polls the mtime of every folder handed in through :meth:`add_dirs`."""

    name = "poll"

    def __init__(self, root: str, *, poll_interval: float = 30.0, poll_rate: int = 2000) -> None:
        self.root = os.path.abspath(root)
        self._poller = DirPoller(self.root, interval=poll_interval, rate=poll_rate)
        self._incoming = Backlog()
        self._incoming.add([self.root])
        self._wake = threading.Event()
        self._lock = threading.RLock()  # sink calls vs. stop()
        self._sink: Sink | None = None
        self._thread: threading.Thread | None = None
        self._stopping = False
        self._state: WatchState = "starting"
        self._detail = ""
        self._events = 0
        self._overflows = 0

    def start(self, sink: Sink) -> None:
        if self._thread is not None:
            raise RuntimeError(TEXT["started"])
        self._sink = sink
        self._state = "degraded"  # the detail (TEXT["polling"]) is written when the status is read
        self._thread = threading.Thread(target=self._run, name="sk-poll", daemon=True)
        self._thread.start()

    def add_dirs(self, dirs: Iterable[str], mtimes: Sequence[int] | None = None, since_ns: int = 0) -> None:
        self._incoming.add(dirs, mtimes, since_ns)
        self._wake.set()

    def stop(self) -> None:
        with self._lock:
            if self._stopping:
                return
            self._stopping = True
        self._wake.set()
        thread = self._thread
        if thread is not None and thread is not threading.current_thread():
            thread.join(0.9)
        if self._state != "failed":
            self._state = "stopped"

    def status(self) -> WatchStatus:
        detail = self._detail
        if not detail and self._thread is not None:  # started: polling instead of notifications
            detail = TEXT["polling"].format(interval=f"{self._poller.interval:g}")
        return WatchStatus(
            self._state,
            detail,
            watched_dirs=0,
            polled_dirs=len(self._poller),
            overflows=self._overflows,
            events=self._events,
        )

    def _run(self) -> None:
        poller = self._poller
        try:
            while not self._stopping:
                fed = self._incoming.peek(4096)
                for path, listed, since in fed:
                    poller.add(path, listed, since)
                self._incoming.advance(len(fed))
                changes = poller.step(time.monotonic())
                if changes:
                    self._emit(changes)
                if poller.root_lost:
                    self._state, self._detail = "failed", TEXT["root_gone"]
                    return
                delay = 0.0 if self._incoming else poller.wake_time() - time.monotonic()
                if delay > 0 and self._wake.wait(min(delay, 1.0)):
                    self._wake.clear()
        except Exception:
            logger.exception(TEXT["log_thread"].format(path=self.root))
            self._state, self._detail = "failed", TEXT["crashed"]
            self._emit([Change("overflow", self.root, is_dir=True)])

    def _emit(self, changes: list[Change]) -> None:
        with self._lock:
            if self._stopping or self._sink is None:
                return
            self._events += len(changes)
            self._overflows += sum(1 for c in changes if c.kind == "overflow")
            try:
                self._sink(changes)
            except Exception:
                logger.exception(TEXT["log_sink"])


__all__ = ["Backlog", "DirPoller", "PollWatcher", "listed_changed", "stat_file"]
