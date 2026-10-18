"""Generic polling watcher (docs/monitor.md §2.4) and the mtime reconciliation inotify reuses.

A folder's mtime changes whenever an entry is added, removed or renamed in it, so re-stat'ing the
scanned folders finds every structural change, at most ``poll_interval`` seconds (plus the round's
own duration) late. File content changes stay invisible: a file's mtime does not reach its folder.

:class:`DirPoller` is the clock-driven core (no thread, no locks): its owner calls
:meth:`DirPoller.step` whenever :meth:`DirPoller.wake_time` has come. A round stats at most
``poll_rate`` folders per second, in small batches spaced out in time, so polling stays a light
background load. The baseline of a newly added folder is taken right away (in chunks, not rate
limited): the scan has just listed that folder, so the stat is served from the inode cache, and a
late baseline would silently swallow a change made in between.

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
from collections.abc import Iterable

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
        self._fresh: deque[str] = deque()
        self._round: list[str] = []
        self._pos = 0
        self._next_round = time.monotonic() + self.interval
        self._next_batch = 0.0

    def __len__(self) -> int:
        return len(self._mtimes)

    def __contains__(self, path: object) -> bool:
        return path in self._mtimes

    def add(self, path: str) -> bool:
        """Start polling ``path`` (False: already polled). The baseline is taken at the next step."""
        if path in self._mtimes:
            return False
        self._mtimes[path] = _PENDING
        self._fresh.append(path)
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
        gone: list[str] = []
        done = 0
        while self._fresh and done < _BASELINE_CHUNK:
            path = self._fresh.popleft()
            if self._mtimes.get(path) != _PENDING:
                continue
            done += 1
            mtime = self._probe(path)
            if mtime is None:
                gone.append(path)  # already gone again: it was in the tree, so report it
            else:
                self._mtimes[path] = mtime
        return self._drop(gone) if gone else []

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
        self._incoming: deque[str] = deque([self.root])
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

    def add_dirs(self, dirs: Iterable[str]) -> None:
        self._incoming.extend(dirs)
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
                for _ in range(min(len(self._incoming), 4096)):
                    poller.add(self._incoming.popleft())
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


__all__ = ["DirPoller", "PollWatcher", "stat_file"]
