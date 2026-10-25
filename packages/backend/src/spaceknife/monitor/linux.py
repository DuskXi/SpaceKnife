"""Linux watcher: inotify within a watch budget, mtime polling beyond it (docs/monitor.md §2.2).

inotify watches one folder at a time and the kernel caps the watches per user
(``/proc/sys/fs/inotify/max_user_watches``; 8192 on older systems), so a budget — 80 % of that cap,
shared by every watcher in this process — is handed out breadth-first: folders arrive through
:meth:`InotifyWatcher.add_dirs` shallow first, and a folder only gets a watch when its parent has
one. Everything else (beyond the budget, or a watch that could not be added) is kept in sync by
mtime polling (:class:`~spaceknife.monitor.poll.DirPoller`), driven from the same thread.

Watched folders form a tree rooted at the scan root (:class:`WatchTree`: wd → (parent wd, name)),
so renaming a watched folder re-keys one entry and every path below it follows. Folders that
appear later (created, or moved in from outside) are watched as soon as their event is read and
listed once, reporting ``created`` for whatever is already inside (``mkdir`` + write races).

The scan's folders arrive a moment after the scan found them (the server hands them over in ticks,
then they wait in a :class:`~spaceknife.monitor.poll.Backlog`), often after the scan listed them
too, and what changed inside a folder in between raises no event. So once a folder's watch is on,
one ``stat`` compares its mtime with the one the scan recorded
(:func:`~spaceknife.monitor.poll.listed_changed`): a folder that may have changed is reported as
``dir`` and listed again. Until every folder handed in is watched or polled the status says
``starting``.

Decoding (:func:`decode_events`) and interpretation (:func:`interpret`) make no system calls, so
tests feed them synthetic records — ``IN_Q_OVERFLOW`` is hard to provoke for real.

Memory: about 210 B plus the name length per watched folder (the tree's two dict entries, the key
tuple and the name; measured: 223 B at 12-character names), plus the kernel's own ~1 KiB per watch.
"""

from __future__ import annotations

import contextlib
import ctypes
import errno
import math
import os
import select
import struct
import sys
import threading
import time
from collections import deque
from collections.abc import Iterable, Sequence
from dataclasses import dataclass, field
from typing import Any

from loguru import logger

from .. import fmt
from ..i18n import Texts
from .base import Change, Sink, WatchState, WatchStatus
from .poll import Backlog, DirPoller, listed_changed, stat_file

# <sys/inotify.h>
IN_ACCESS = 0x00000001
IN_MODIFY = 0x00000002
IN_ATTRIB = 0x00000004
IN_CLOSE_WRITE = 0x00000008
IN_CLOSE_NOWRITE = 0x00000010
IN_OPEN = 0x00000020
IN_MOVED_FROM = 0x00000040
IN_MOVED_TO = 0x00000080
IN_CREATE = 0x00000100
IN_DELETE = 0x00000200
IN_DELETE_SELF = 0x00000400
IN_MOVE_SELF = 0x00000800
IN_UNMOUNT = 0x00002000
IN_Q_OVERFLOW = 0x00004000
IN_IGNORED = 0x00008000
IN_ONLYDIR = 0x01000000
IN_DONT_FOLLOW = 0x02000000
IN_ISDIR = 0x40000000

_EVENTS = IN_CREATE | IN_DELETE | IN_MOVED_FROM | IN_MOVED_TO | IN_CLOSE_WRITE | IN_ATTRIB
_EVENTS |= IN_DELETE_SELF | IN_MOVE_SELF
WATCH_MASK = _EVENTS | IN_ONLYDIR | IN_DONT_FOLLOW
ROOT_MASK = _EVENTS | IN_ONLYDIR  # the scan root itself may be a symbolic link to a folder
_SELF = IN_DELETE_SELF | IN_MOVE_SELF | IN_UNMOUNT | IN_IGNORED

_HEADER = struct.Struct("iIII")  # struct inotify_event: wd, mask, cookie, len (then the name)
_READ_LIMIT = 1 << 20  # bytes drained per wake-up before the batch is handled
_PAIR_WAIT = 0.002  # a batch ending in an unpaired IN_MOVED_FROM waits this long for its IN_MOVED_TO
_ADD_CHUNK = 2048  # add_dirs folders placed per loop turn (keeps event reading responsive)
_WALK_CHUNK = 32  # new folders listed per loop turn
_MAX_DEPTH = 4096
_BUDGET_SHARE = 0.8
DEFAULT_MAX_USER_WATCHES = 8192

# User-facing text (M7.5): watch states and the reasons inotify cannot be used.
TEXT = Texts(
    {
        "emfile": "inotify 实例数已达上限（fs.inotify.max_user_instances）",
        "enfile": "系统打开的文件过多",
        "enosys": "内核不支持",
        "denied": "没有权限",
        "enomem": "内存不足",
        "not_linux": "inotify 只在 Linux 上可用",
        "no_libc": "libc 没有 inotify",
        "no_poll": "需要 poll()",
        "started": "监视器已经启动",
        "fallback": "inotify 不可用（{reason}），全部文件夹改为轮询",
        "root_fallback": "无法监视扫描根目录（{reason}），全部文件夹改为轮询",
        "short": "inotify 监视额度不足，{count} 个文件夹改为轮询",
        "polled": "{count} 个文件夹无法用 inotify 监视，改为轮询",
        "root_lost": "扫描根目录已被删除或移走，实时监控已停止",
        "root_gone": "扫描根目录已不存在，实时监控已停止",
        "crashed": "实时监控出错，已停止",
        "log_fallback": "inotify 不可用（{error}）：{path} 改为轮询",
        "log_root": "inotify 无法监视 {path}：{error}",
        "log_thread": "inotify 监视线程出错：{path}",
        "log_sink": "监视器回调出错",
    }
)
_ERRNO_TEXT = {
    errno.EMFILE: "emfile",
    errno.ENFILE: "enfile",
    errno.ENOSYS: "enosys",
    errno.EACCES: "denied",
    errno.EPERM: "denied",
    errno.ENOMEM: "enomem",
}


def _why(exc: OSError) -> str:
    if exc.errno in _ERRNO_TEXT:
        return TEXT[_ERRNO_TEXT[exc.errno]]
    return exc.strerror or str(exc)


def max_user_watches() -> int:
    """The per-user inotify watch cap (``DEFAULT_MAX_USER_WATCHES`` when it cannot be read)."""
    try:
        with open("/proc/sys/fs/inotify/max_user_watches", "rb") as f:
            return max(1, int(f.read().strip()))
    except (OSError, ValueError):
        return DEFAULT_MAX_USER_WATCHES


def _join(folder: str, name: str) -> str:
    return folder + name if folder.endswith("/") else f"{folder}/{name}"


# --------------------------------------------------------------------------- decoding


@dataclass(frozen=True, slots=True)
class RawEvent:
    wd: int
    mask: int
    cookie: int = 0
    name: bytes = b""


def decode_events(buf: bytes) -> list[RawEvent]:
    """Split what ``read()`` returned from an inotify descriptor into events (names without padding)."""
    out: list[RawEvent] = []
    pos, end, size = 0, len(buf), _HEADER.size
    while pos + size <= end:
        wd, mask, cookie, length = _HEADER.unpack_from(buf, pos)
        pos += size
        if pos + length > end:
            break  # the kernel never splits an event; a truncated buffer ends here
        name = buf[pos : pos + length].split(b"\0", 1)[0]
        pos += length
        out.append(RawEvent(wd, mask, cookie, name))
    return out


class WatchTree:
    """Watched folders as a tree: wd → (parent wd, name). Every folder but the root needs its
    parent watched, so a path is always the root plus a chain of names."""

    def __init__(self, root: str) -> None:
        self.root = root
        self.root_wd = -1
        self._node: dict[int, tuple[int, str]] = {}
        self._child: dict[tuple[int, str], int] = {}

    def __len__(self) -> int:
        return len(self._node)

    def __contains__(self, wd: object) -> bool:
        return wd in self._node

    def set_root(self, wd: int) -> None:
        self.root_wd = wd
        self._node[wd] = (-1, "")

    def add(self, wd: int, parent: int, name: str) -> None:
        key = (parent, name)
        self._node[wd] = key
        self._child[key] = wd

    def lookup(self, parent: int, name: str) -> int | None:
        return self._child.get((parent, name))

    def remove(self, wd: int) -> None:
        key = self._node.pop(wd, None)
        if key is not None and self._child.get(key) == wd:
            del self._child[key]

    def move(self, wd: int, parent: int, name: str) -> None:
        self.remove(wd)
        self.add(wd, parent, name)

    def wds(self) -> list[int]:
        """Every watch, in the order the folders were added (shallow first)."""
        return list(self._node)

    def path(self, wd: int) -> str | None:
        """Absolute path of a watched folder (None: unknown, or cut off from the root)."""
        names: list[str] = []
        while wd != self.root_wd:
            node = self._node.get(wd)
            if node is None or len(names) > _MAX_DEPTH:
                return None
            wd, name = node
            names.append(name)
        if wd not in self._node:
            return None
        if not names:
            return self.root
        names.append(self.root.rstrip("/"))
        return "/".join(reversed(names))

    def find(self, path: str) -> int | None:
        """The watch of the folder at ``path`` (None: not watched)."""
        if self.root_wd not in self._node:
            return None
        if path == self.root:
            return self.root_wd
        prefix = self.root if self.root.endswith("/") else self.root + "/"
        if not path.startswith(prefix):
            return None
        wd = self.root_wd
        for name in path[len(prefix) :].split("/"):
            child = self._child.get((wd, name))
            if child is None:
                return None
            wd = child
        return wd

    def orphans(self) -> list[int]:
        """Watches whose chain of parents no longer reaches the root (a folder above left the tree)."""
        reach: dict[int, bool] = {self.root_wd: self.root_wd in self._node}
        out: list[int] = []
        for start in self._node:
            chain: list[int] = []
            wd = start
            while wd not in reach:
                node = self._node.get(wd)
                if node is None or len(chain) > _MAX_DEPTH:
                    reach[wd] = False
                    break
                chain.append(wd)
                wd = node[0]
            ok = reach[wd]
            for w in chain:
                reach[w] = ok
            if not ok:
                out.append(start)
        return out


@dataclass(slots=True)
class Interpreted:
    """What a batch of events means (:func:`interpret`); the watcher carries out the rest."""

    changes: list[Change] = field(default_factory=list)
    modified: list[str] = field(default_factory=list)  # files to stat, deduplicated (last event wins)
    new_dirs: list[str] = field(default_factory=list)  # folders that appeared: watch and list them
    detached: list[int] = field(default_factory=list)  # watches cut from the tree: remove (with orphans)
    moved_dirs: list[tuple[str, str]] = field(default_factory=list)  # (old, new): polled folders follow
    overflows: int = 0
    queue_overflow: bool = False  # the kernel dropped events: the tree may hold stale watches
    root_lost: bool = False


def interpret(events: Sequence[RawEvent], tree: WatchTree) -> Interpreted:
    """Turn decoded events into changes, updating ``tree`` for folders renamed or gone (no I/O).

    ``IN_MOVED_FROM`` / ``IN_MOVED_TO`` with the same cookie in the batch become one ``renamed``;
    an unpaired ``IN_MOVED_FROM`` (moved out of sight) is ``deleted``, an unpaired ``IN_MOVED_TO``
    (moved in) ``created``. ``modified`` changes are left to the watcher (they need a stat).
    """
    out = Interpreted()
    modified: dict[str, None] = {}
    moved_to: dict[int, int] = {}
    for i, ev in enumerate(events):
        if ev.mask & IN_MOVED_TO and ev.cookie:
            moved_to.setdefault(ev.cookie, i)
    paired: set[int] = set()
    for i, ev in enumerate(events):
        mask = ev.mask
        if mask & IN_Q_OVERFLOW:
            out.changes.append(Change("overflow", tree.root, is_dir=True))
            out.overflows += 1
            out.queue_overflow = True
            continue
        if i in paired or ev.wd not in tree:
            continue
        if mask & _SELF:
            if ev.wd == tree.root_wd:
                out.changes.append(Change("overflow", tree.root, is_dir=True))
                out.overflows += 1
                out.root_lost = True
                break
            if mask & IN_DELETE_SELF:
                tree.remove(ev.wd)  # the parent's IN_DELETE reports it; the kernel drops the watch
            elif mask & (IN_UNMOUNT | IN_IGNORED):  # the watch went away under a folder still in the tree
                lost = tree.path(ev.wd)
                tree.remove(ev.wd)
                out.detached.append(ev.wd)
                if lost is not None:
                    out.changes.append(Change("overflow", lost, is_dir=True))
                    out.overflows += 1
            continue  # IN_MOVE_SELF: the parent's IN_MOVED_FROM / IN_MOVED_TO carry the move
        folder = tree.path(ev.wd)
        if folder is None or not ev.name:
            continue  # cut off (swept after this batch), or about the folder itself (IN_ATTRIB)
        name = os.fsdecode(ev.name)
        path = _join(folder, name)
        is_dir = bool(mask & IN_ISDIR)
        if mask & IN_MOVED_FROM:
            modified.pop(path, None)
            j = moved_to.get(ev.cookie, -1) if ev.cookie else -1
            dst = events[j] if j > i else None
            dst_folder = tree.path(dst.wd) if dst is not None else None
            child = tree.lookup(ev.wd, name) if is_dir else None
            if dst is not None and dst_folder is not None:
                paired.add(j)
                dst_name = os.fsdecode(dst.name)
                new = _join(dst_folder, dst_name)
                out.changes.append(Change("renamed", new, path, is_dir=is_dir))
                if is_dir:
                    victim = tree.lookup(dst.wd, dst_name)  # an empty folder replaced by the move
                    if victim is not None and victim != child:
                        tree.remove(victim)
                        out.detached.append(victim)
                    if child is not None:
                        tree.move(child, dst.wd, dst_name)
                    out.moved_dirs.append((path, new))
            else:
                out.changes.append(Change("deleted", path, is_dir=is_dir))
                if child is not None:
                    tree.remove(child)
                    out.detached.append(child)
        elif mask & IN_MOVED_TO:
            out.changes.append(Change("created", path, is_dir=is_dir))
            if is_dir:
                victim = tree.lookup(ev.wd, name)
                if victim is not None:
                    tree.remove(victim)
                    out.detached.append(victim)
                out.new_dirs.append(path)
        elif mask & IN_CREATE:
            out.changes.append(Change("created", path, is_dir=is_dir))
            if is_dir:
                out.new_dirs.append(path)
        elif mask & IN_DELETE:
            modified.pop(path, None)
            out.changes.append(Change("deleted", path, is_dir=is_dir))
            if is_dir and (child := tree.lookup(ev.wd, name)) is not None:
                tree.remove(child)  # rmdir: the kernel drops that (childless) watch itself
        elif mask & (IN_CLOSE_WRITE | IN_ATTRIB) and not is_dir:
            modified.pop(path, None)
            modified[path] = None
    out.modified = list(modified)
    return out


def _dangling_moves(events: Sequence[RawEvent]) -> bool:
    froms = {e.cookie for e in events if e.mask & IN_MOVED_FROM and e.cookie}
    if not froms:
        return False
    return bool(froms - {e.cookie for e in events if e.mask & IN_MOVED_TO})


# --------------------------------------------------------------------------- system calls


def _open_inotify() -> tuple[int, Any, Any]:
    """``inotify_init1(IN_NONBLOCK | IN_CLOEXEC)`` plus the watch calls (libc through ctypes)."""
    if not sys.platform.startswith("linux"):
        raise OSError(errno.ENOSYS, TEXT["not_linux"])
    libc = ctypes.CDLL(None, use_errno=True)
    try:
        init1, add, rm = libc.inotify_init1, libc.inotify_add_watch, libc.inotify_rm_watch
    except AttributeError as exc:
        raise OSError(errno.ENOSYS, TEXT["no_libc"]) from exc
    init1.argtypes = [ctypes.c_int]
    init1.restype = ctypes.c_int
    add.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_uint32]
    add.restype = ctypes.c_int
    rm.argtypes = [ctypes.c_int, ctypes.c_int]
    rm.restype = ctypes.c_int
    fd = init1(os.O_NONBLOCK | os.O_CLOEXEC)  # IN_NONBLOCK / IN_CLOEXEC equal these on every arch
    if fd < 0:
        err = ctypes.get_errno()
        raise OSError(err, os.strerror(err))
    return int(fd), add, rm


class _Inotify:
    """An inotify instance: non-blocking, close-on-exec."""

    def __init__(self) -> None:
        self.fd, self._add, self._rm = _open_inotify()

    def add_watch(self, path: str, mask: int) -> int:
        wd = self._add(self.fd, os.fsencode(path), mask)
        if wd < 0:
            err = ctypes.get_errno()
            raise OSError(err, os.strerror(err), path)
        return int(wd)

    def rm_watch(self, wd: int) -> None:
        self._rm(self.fd, wd)  # EINVAL when the kernel already dropped it: nothing to do

    def read(self, limit: int) -> bytes:
        chunks: list[bytes] = []
        total = 0
        while total < limit:
            try:
                data = os.read(self.fd, 65536)
            except BlockingIOError:
                break
            except InterruptedError:
                continue
            if not data:
                break
            chunks.append(data)
            total += len(data)
        return b"".join(chunks)

    def close(self) -> None:
        os.close(self.fd)


def _wait_readable(fds: list[int], timeout: float) -> set[int]:
    if sys.platform == "win32":
        raise OSError(errno.ENOSYS, TEXT["no_poll"])
    poller = select.poll()
    for fd in fds:
        poller.register(fd, select.POLLIN)
    try:
        ready = poller.poll(math.ceil(max(0.0, timeout) * 1000))
    except InterruptedError:
        return set()
    return {fd for fd, _ in ready}


def _drain(fd: int) -> None:
    try:
        while os.read(fd, 4096):
            pass
    except OSError:
        pass


class _Budget:
    """Watches held by every :class:`InotifyWatcher` of this process (the kernel cap is per user)."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self.used = 0

    def take(self, limit: int) -> bool:
        with self._lock:
            if self.used >= limit:
                return False
            self.used += 1
            return True

    def give(self, n: int) -> None:
        with self._lock:
            self.used = max(0, self.used - n)


_BUDGET = _Budget()


# --------------------------------------------------------------------------- the watcher


class InotifyWatcher:
    """One scan root: inotify for the folders within the budget, mtime polling for the rest.

    ``max_watches`` caps this watcher's own watches (tests); the process-wide budget always applies.
    """

    name = "inotify"

    def __init__(
        self,
        root: str,
        *,
        poll_interval: float = 30.0,
        poll_rate: int = 2000,
        max_watches: int | None = None,
    ) -> None:
        self.root = os.path.abspath(root)
        self._prefix = self.root if self.root.endswith("/") else self.root + "/"
        self._max_watches = max_watches
        self._limit = int(max_user_watches() * _BUDGET_SHARE)
        self._tree = WatchTree(self.root)
        self._poller = DirPoller(self.root, interval=poll_interval, rate=poll_rate)
        self._incoming = Backlog()  # from add_dirs, waiting for their watch (or polling)
        self._root_listed = False  # the scan's record of the root was checked
        self._walk: deque[str] = deque()
        self._recheck: deque[int] = deque()  # watches to revalidate after a queue overflow
        self._ino: _Inotify | None = None
        self._wake_r = self._wake_w = -1
        self._lock = threading.RLock()  # sink calls vs. stop()
        self._fd_lock = threading.Lock()  # wake-ups from other threads vs. closing the pipe
        self._sink: Sink | None = None
        self._thread: threading.Thread | None = None
        self._stopping = False
        self._state: WatchState = "starting"
        self._detail = ""
        self._fallback = ""  # why inotify serves nothing at all (everything is polled)
        self._short = False  # the watch budget ran out at some point
        self._kernel_full = False  # ENOSPC: the per-user cap is used up (other processes too)
        self._held = 0  # budget units this watcher holds
        self._parent_hint: tuple[str, int] | None = None  # add_dirs comes grouped by parent
        self._events = 0
        self._overflows = 0

    # ------------------------------------------------------------------ Watcher

    def start(self, sink: Sink) -> None:
        if self._thread is not None:
            raise RuntimeError(TEXT["started"])
        self._sink = sink
        self._wake_r, self._wake_w = os.pipe()
        os.set_blocking(self._wake_r, False)
        os.set_blocking(self._wake_w, False)
        try:
            self._ino = _Inotify()
        except OSError as exc:
            self._fallback = TEXT["fallback"].format(reason=_why(exc))
            logger.warning(TEXT["log_fallback"].format(error=exc, path=self.root))
        if self._ino is not None:
            self._watch_root()
        if self._tree.root_wd not in self._tree:
            self._poller.add(self.root)
        self._state = "running"
        self._thread = threading.Thread(target=self._run, name="sk-inotify", daemon=True)
        self._thread.start()

    def add_dirs(self, dirs: Iterable[str], mtimes: Sequence[int] | None = None, since_ns: int = 0) -> None:
        self._incoming.add(dirs, mtimes, since_ns)
        self._wake()

    def stop(self) -> None:
        with self._lock:
            if self._stopping:
                return
            self._stopping = True
        self._wake()
        thread = self._thread
        if thread is not None and thread is not threading.current_thread():
            thread.join(0.9)
        if self._state != "failed":
            self._state = "stopped"

    def status(self) -> WatchStatus:
        state, detail = self._state, self._detail
        polled = len(self._poller)
        if state == "running":
            if self._fallback:
                state, detail = "degraded", self._fallback
            elif polled and self._short:
                state, detail = "degraded", TEXT["short"].format(count=fmt.count(polled))
            elif polled:
                state, detail = "degraded", TEXT["polled"].format(count=fmt.count(polled))
            elif self._incoming or self._poller.settling:
                state = "starting"  # folders handed in still wait for their watch (and check)
        return WatchStatus(
            state,
            detail,
            watched_dirs=len(self._tree),
            polled_dirs=polled,
            overflows=self._overflows,
            events=self._events,
        )

    # ------------------------------------------------------------------ placement

    def _reserve(self) -> bool:
        if self._kernel_full or (self._max_watches is not None and self._held >= self._max_watches):
            return False
        if not _BUDGET.take(self._limit):
            return False
        self._held += 1
        return True

    def _release(self, n: int = 1) -> None:
        _BUDGET.give(n)
        self._held -= n

    def _sync_budget(self) -> None:
        extra = self._held - len(self._tree)
        if extra > 0:
            self._release(extra)
            self._kernel_full = False  # a watch was freed: worth trying again

    def _watch_root(self) -> None:
        assert self._ino is not None
        if not self._reserve():
            self._short = True
            return
        try:
            wd = self._ino.add_watch(self.root, ROOT_MASK)
        except OSError as exc:
            self._release()
            if exc.errno == errno.ENOSPC:
                self._kernel_full = self._short = True
            else:
                self._fallback = TEXT["root_fallback"].format(reason=_why(exc))
            logger.warning(TEXT["log_root"].format(path=self.root, error=exc))
            return
        self._tree.set_root(wd)

    def _watch(
        self, path: str, parent: int, name: str, listed: int | None = None, since_ns: int = 0
    ) -> int | None:
        """Watch a folder whose parent is watched; poll it when that is not possible (None).
        ``listed`` / ``since_ns``: the scan's record of it, which a polling baseline is checked
        against (the caller checks a watched folder itself)."""
        ino = self._ino
        if ino is None or not self._reserve():
            self._short = self._short or ino is not None
            self._poller.add(path, listed, since_ns)
            return None
        try:
            wd = ino.add_watch(path, WATCH_MASK)
        except OSError as exc:
            self._release()
            if exc.errno in (errno.ENOENT, errno.ENOTDIR, errno.ELOOP):
                return None  # gone again, or not a folder (any more): the parent's events tell
            if exc.errno == errno.ENOSPC:
                self._kernel_full = self._short = True
            else:
                logger.debug("inotify_add_watch({}) failed: {}", path, exc)
            self._poller.add(path, listed, since_ns)
            return None
        if wd in self._tree:  # the same folder under another path (bind mount): one path per watch
            self._release()
            self._poller.add(path, listed, since_ns)
            return None
        self._tree.add(wd, parent, name)
        return wd

    def _place(self, path: str, listed: int | None, since_ns: int, out: list[Change]) -> None:
        """A folder from add_dirs: watch it (parent watched, budget left) or poll it. With the
        scan's record of it (``listed``), a change since its listing is added to ``out``."""
        if path == self.root:
            if listed is not None and not self._root_listed:  # watched (or polled) since start()
                self._root_listed = True
                if self._tree.root_wd in self._tree:
                    self._check(path, listed, since_ns, out)
                else:
                    self._poller.add(path, listed, since_ns)
            return
        if not path.startswith(self._prefix):
            return
        parent_path, _, name = path.rpartition("/")
        parent_path = parent_path or "/"
        if not name:
            return  # "…/" is not a folder path the scan produces
        hint = self._parent_hint
        if hint is not None and hint[0] == parent_path:
            parent: int | None = hint[1]
        else:
            parent = self._tree.find(parent_path)
            if parent is not None:
                self._parent_hint = (parent_path, parent)
        if parent is None:
            self._poller.add(path, listed, since_ns)  # the parent is polled (or unwatchable): so is this one
            return
        if self._tree.lookup(parent, name) is not None or path in self._poller:
            return  # handed in again (its row moved when its parent was listed again): as it is
        if self._watch(path, parent, name, listed, since_ns) is not None and listed is not None:
            self._check(path, listed, since_ns, out)

    def _check(self, path: str, listed: int, since_ns: int, out: list[Change]) -> None:
        """The folder is watched now: did it change after the scan listed it, unseen? (One stat.)"""
        try:
            st = os.stat(path, follow_symlinks=path == self.root)
        except OSError:
            return  # gone again: its parent's events (or the parent's own check) tell
        if listed_changed(st.st_mtime_ns, listed, since_ns):
            out.append(Change("dir", path, is_dir=True))

    def _take_incoming(self) -> None:
        fed = self._incoming.peek(_ADD_CHUNK)
        if not fed:
            return
        changes: list[Change] = []
        for path, listed, since in fed:
            self._place(path, listed, since, changes)
        if changes:
            self._emit(changes)
        self._incoming.advance(len(fed))  # after the changes went out: "starting" until then

    def _walk_some(self) -> None:
        """Watch folders that appeared and report what is already inside them."""
        changes: list[Change] = []
        done = 0
        while self._walk and done < _WALK_CHUNK:
            path = self._walk.popleft()
            parent_path, _, name = path.rpartition("/")
            parent = self._tree.find(parent_path or "/")
            if parent is None or self._tree.lookup(parent, name) is not None or path in self._poller:
                continue  # gone with its parent, or already watched / polled
            done += 1
            if self._watch(path, parent, name) is None:
                continue  # polled from now on (the scan of the new folder reads its contents)
            try:
                with os.scandir(path) as it:
                    for entry in it:
                        is_dir = entry.is_dir(follow_symlinks=False)
                        changes.append(Change("created", entry.path, is_dir=is_dir))
                        if is_dir:
                            self._walk.append(entry.path)
            except OSError:
                continue
        if changes:
            self._emit(changes)

    def _recheck_some(self, ino: _Inotify) -> bool:
        """After a queue overflow: drop watches whose path now names another folder (or none).

        Adding a watch for an inode already watched returns the existing wd, so a different wd (or
        an error) means the deletion or move of the old folder was among the lost events. The new
        folder at that path is walked again; folders below a stale one are swept. True when the
        root itself is no longer the folder being watched.
        """
        stale = False
        for _ in range(min(len(self._recheck), _ADD_CHUNK)):
            wd = self._recheck.popleft()
            path = self._tree.path(wd) if wd in self._tree else None
            if path is None:
                continue
            root = wd == self._tree.root_wd
            try:
                got = ino.add_watch(path, ROOT_MASK if root else WATCH_MASK)
            except OSError:
                got = -1
            if got == wd:
                continue
            if root:
                self._emit([Change("overflow", self.root, is_dir=True)])
                self._state, self._detail = "failed", TEXT["root_lost"]
                return True
            stale = True
            self._tree.remove(wd)
            ino.rm_watch(wd)
            if got >= 0:
                if got not in self._tree:
                    ino.rm_watch(got)  # the walk adds it again and reports what is inside
                self._walk.append(path)
        if stale:
            self._parent_hint = None
            for wd in self._tree.orphans():
                self._tree.remove(wd)
                ino.rm_watch(wd)
            self._sync_budget()
        return False

    # ------------------------------------------------------------------ the thread

    def _run(self) -> None:
        try:
            self._loop()
        except Exception:
            logger.exception(TEXT["log_thread"].format(path=self.root))
            self._state, self._detail = "failed", TEXT["crashed"]
            self._emit([Change("overflow", self.root, is_dir=True)])
        finally:
            self._close()

    def _loop(self) -> None:
        ino = self._ino
        fds = [self._wake_r] if ino is None else [self._wake_r, ino.fd]
        while not self._stopping:
            self._take_incoming()
            if ino is not None and self._recheck and self._recheck_some(ino):
                return
            self._walk_some()
            changes = self._poller.step(time.monotonic())
            if changes:
                self._emit(changes)
            if self._poller.root_lost:
                self._state, self._detail = "failed", TEXT["root_gone"]
                return
            if self._incoming or self._walk or self._recheck:
                timeout = 0.0
            else:
                timeout = min(1.0, self._poller.wake_time() - time.monotonic())
            ready = _wait_readable(fds, timeout)
            if self._wake_r in ready:
                _drain(self._wake_r)
            if ino is not None and ino.fd in ready and self._handle(ino):
                return

    def _handle(self, ino: _Inotify) -> bool:
        """Read and apply one batch of events; True when the root is gone (watching ends)."""
        events = decode_events(ino.read(_READ_LIMIT))
        if _dangling_moves(events) and ino.fd in _wait_readable([ino.fd], _PAIR_WAIT):
            events += decode_events(ino.read(_READ_LIMIT))
        res = interpret(events, self._tree)
        self._parent_hint = None
        if res.queue_overflow:
            self._recheck = deque(self._tree.wds())
        for wd in res.detached:
            ino.rm_watch(wd)
        if res.detached:
            for wd in self._tree.orphans():
                self._tree.remove(wd)
                ino.rm_watch(wd)
        self._sync_budget()
        for old, new in res.moved_dirs:
            self._poller.rename_tree(old, new)
        self._walk.extend(res.new_dirs)
        changes = res.changes
        for path in res.modified:
            numbers = stat_file(path)
            if numbers is not None:  # vanished meanwhile: its deletion is (or will be) reported
                size, alloc, mtime = numbers
                changes.append(Change("modified", path, is_dir=False, size=size, alloc=alloc, mtime=mtime))
        if changes:
            self._emit(changes)
        if res.root_lost:
            self._state, self._detail = "failed", TEXT["root_lost"]
        return res.root_lost

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

    def _wake(self) -> None:
        with self._fd_lock:
            if self._wake_w >= 0:
                with contextlib.suppress(OSError):  # a full pipe: the thread is awake anyway
                    os.write(self._wake_w, b"\0")

    def _close(self) -> None:
        with self._fd_lock:
            if self._ino is not None:
                self._ino.close()  # drops every watch of this instance
                self._ino = None
            for fd in (self._wake_r, self._wake_w):
                if fd >= 0:
                    os.close(fd)
            self._wake_r = self._wake_w = -1
            self._tree = WatchTree(self.root)
            self._release(self._held)


__all__ = [
    "IN_ATTRIB",
    "IN_CLOSE_WRITE",
    "IN_CREATE",
    "IN_DELETE",
    "IN_DELETE_SELF",
    "IN_IGNORED",
    "IN_ISDIR",
    "IN_MOVED_FROM",
    "IN_MOVED_TO",
    "IN_MOVE_SELF",
    "IN_Q_OVERFLOW",
    "IN_UNMOUNT",
    "InotifyWatcher",
    "Interpreted",
    "RawEvent",
    "WatchTree",
    "decode_events",
    "interpret",
    "max_user_watches",
]
