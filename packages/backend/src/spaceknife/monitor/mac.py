"""macOS watcher: FSEvents through ctypes (docs/monitor.md §2.3).

**Untested on macOS.** Written from the CoreServices headers without a Mac to run it on; the other
platforms only check that construction fails with ``OSError`` (so
:func:`~spaceknife.monitor.base.create_watcher` falls back to polling) and the event mapping
(:func:`map_events`, free of system calls apart from the injected existence check and stat).

One stream per scan root — file-level events, no deferral, ``WatchRoot``, 50 ms latency — is
scheduled on the CFRunLoop of a dedicated thread; FSEvents calls back on that thread and each
callback becomes one batch for the sink. The stream is recursive: :meth:`FsEventsWatcher.add_dirs`
is ignored.

FSEvents merges the flags of one path within the latency window (``Created | Removed`` can mean
either order), so structural events are decided by whether the path exists now; the backend lists
the folder again either way. A rename arrives as two events with consecutive ids (old path, then
new path) and is paired when the first one is gone and the second one exists. Event paths are
canonical (``/private/var/…``), so they are mapped back onto the root as the scan spelled it. A
path inside the boot volume group's Data volume (``/System/Volumes/Data/Users/…``: the scan does
not enter that mount, its files are in the tree through the firmlinks) first goes back through the
firmlinks (``/Users/…``); one no firmlink leads to is not in the tree and is dropped.
"""

from __future__ import annotations

import ctypes
import os
import threading
from collections.abc import Callable, Iterable, Sequence
from typing import Any

from loguru import logger

from ..i18n import Texts
from ..scan.volume import through_firmlinks, volume_group_twins
from .base import Change, Sink, WatchState, WatchStatus
from .poll import stat_file

# User-facing text (M7.5): the watch state and why FSEvents could not be used.
TEXT = Texts(
    {
        "missing": "找不到 {name}",
        "constants": "找不到 CoreFoundation 常量",
        "started": "监视器已经启动",
        "start_failed": "FSEventStreamStart 失败",
        "no_source": "CFRunLoop 没有可运行的源",
        "no_string": "无法创建路径字符串",
        "no_array": "无法创建路径数组",
        "create_failed": "FSEventStreamCreate 失败",
        "unavailable": "FSEvents 不可用（{error}），实时监控已停止",
        "root_lost": "扫描根目录已被删除或移走，实时监控已停止",
        "log_failed": "FSEvents 监视失败：{path}：{error}",
        "log_callback": "FSEvents 回调出错",
        "log_sink": "监视器回调出错",
    }
)

_CORE_FOUNDATION = "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
_CORE_SERVICES = "/System/Library/Frameworks/CoreServices.framework/CoreServices"

# FSEventStreamCreateFlags
_CREATE_NO_DEFER = 0x02
_CREATE_WATCH_ROOT = 0x04
_CREATE_FILE_EVENTS = 0x10
_CREATE_FLAGS = _CREATE_FILE_EVENTS | _CREATE_NO_DEFER | _CREATE_WATCH_ROOT
_SINCE_NOW = 0xFFFFFFFFFFFFFFFF  # kFSEventStreamEventIdSinceNow
_LATENCY = 0.05

# FSEventStreamEventFlags
F_MUST_SCAN_SUBDIRS = 0x00000001
F_USER_DROPPED = 0x00000002
F_KERNEL_DROPPED = 0x00000004
F_EVENT_IDS_WRAPPED = 0x00000008
F_HISTORY_DONE = 0x00000010
F_ROOT_CHANGED = 0x00000020
F_MOUNT = 0x00000040
F_UNMOUNT = 0x00000080
F_ITEM_CREATED = 0x00000100
F_ITEM_REMOVED = 0x00000200
F_ITEM_INODE_META_MOD = 0x00000400
F_ITEM_RENAMED = 0x00000800
F_ITEM_MODIFIED = 0x00001000
F_ITEM_IS_FILE = 0x00010000
F_ITEM_IS_DIR = 0x00020000
F_ITEM_IS_SYMLINK = 0x00040000
_ITEM_ANY = 0x007FFF00  # every kFSEventStreamEventFlagItem* bit
_DROPPED = F_USER_DROPPED | F_KERNEL_DROPPED

_RUN_FINISHED = 1  # kCFRunLoopRunFinished: the run loop has no sources left

_CALLBACK = ctypes.CFUNCTYPE(
    None,
    ctypes.c_void_p,  # ConstFSEventStreamRef
    ctypes.c_void_p,  # clientCallBackInfo
    ctypes.c_size_t,  # numEvents
    ctypes.POINTER(ctypes.c_char_p),  # eventPaths (char ** without kFSEventStreamCreateFlagUseCFTypes)
    ctypes.POINTER(ctypes.c_uint32),  # eventFlags
    ctypes.POINTER(ctypes.c_uint64),  # eventIds
)

Event = tuple[str, int, int]  # (path, flags, event id)


def map_events(
    events: Sequence[Event],
    root: str,
    *,
    exists: Callable[[str], bool] = os.path.lexists,
    numbers: Callable[[str], tuple[int, int, int] | None] = stat_file,
) -> tuple[list[Change], bool]:
    """Changes for one callback's events, and whether the root itself was moved or deleted."""
    out: list[Change] = []
    i, n = 0, len(events)
    while i < n:
        path, flags, eid = events[i]
        i += 1
        if flags & (_DROPPED | F_MUST_SCAN_SUBDIRS):
            where = root if flags & _DROPPED else path
            out.append(Change("overflow", where, is_dir=True))
            continue
        if flags & F_ROOT_CHANGED:
            out.append(Change("overflow", root, is_dir=True))
            return out, True
        if flags & (F_MOUNT | F_UNMOUNT):
            out.append(Change("overflow", path, is_dir=True))
            continue
        if not flags & _ITEM_ANY:
            if not flags & (F_HISTORY_DONE | F_EVENT_IDS_WRAPPED):
                out.append(Change("dir", path, is_dir=True))  # a folder-level event: list it again
            continue
        kinds = F_ITEM_IS_DIR | F_ITEM_IS_FILE | F_ITEM_IS_SYMLINK
        is_dir = bool(flags & F_ITEM_IS_DIR) if flags & kinds else None
        if flags & F_ITEM_RENAMED:
            here = exists(path)
            if not here and i < n:
                new, new_flags, new_id = events[i]
                if new_flags & F_ITEM_RENAMED and new_id == eid + 1 and exists(new):
                    out.append(Change("renamed", new, path, is_dir=is_dir))
                    i += 1
                    continue
            out.append(Change("created" if here else "deleted", path, is_dir=is_dir))
        elif flags & (F_ITEM_CREATED | F_ITEM_REMOVED):
            out.append(Change("created" if exists(path) else "deleted", path, is_dir=is_dir))
        elif flags & (F_ITEM_MODIFIED | F_ITEM_INODE_META_MOD) and not is_dir:
            got = numbers(path)
            if got is not None:
                out.append(Change("modified", path, is_dir=False, size=got[0], alloc=got[1], mtime=got[2]))
    return out, False


class _Api:
    """The CoreFoundation / CoreServices functions the watcher calls (OSError when unavailable)."""

    def __init__(self) -> None:
        cf = ctypes.CDLL(_CORE_FOUNDATION)
        cs = ctypes.CDLL(_CORE_SERVICES)
        vp = ctypes.c_void_p

        def fn(lib: ctypes.CDLL, name: str, restype: Any, *argtypes: Any) -> Any:
            try:
                f = getattr(lib, name)
            except AttributeError as exc:
                raise OSError(TEXT["missing"].format(name=name)) from exc
            f.restype = restype
            f.argtypes = list(argtypes)
            return f

        self.release = fn(cf, "CFRelease", None, vp)
        self.string = fn(cf, "CFStringCreateWithFileSystemRepresentation", vp, vp, ctypes.c_char_p)
        self.array = fn(cf, "CFArrayCreate", vp, vp, ctypes.POINTER(vp), ctypes.c_long, vp)
        self.current_loop = fn(cf, "CFRunLoopGetCurrent", vp)
        self.run_in_mode = fn(cf, "CFRunLoopRunInMode", ctypes.c_int32, vp, ctypes.c_double, ctypes.c_ubyte)
        self.stop_loop = fn(cf, "CFRunLoopStop", None, vp)
        self.wake_loop = fn(cf, "CFRunLoopWakeUp", None, vp)
        self.create = fn(
            cs,
            "FSEventStreamCreate",
            vp,
            vp,
            _CALLBACK,
            vp,
            vp,
            ctypes.c_uint64,
            ctypes.c_double,
            ctypes.c_uint32,
        )
        self.schedule = fn(cs, "FSEventStreamScheduleWithRunLoop", None, vp, vp, vp)
        self.start = fn(cs, "FSEventStreamStart", ctypes.c_ubyte, vp)
        self.stop = fn(cs, "FSEventStreamStop", None, vp)
        self.invalidate = fn(cs, "FSEventStreamInvalidate", None, vp)
        self.release_stream = fn(cs, "FSEventStreamRelease", None, vp)
        try:
            self.default_mode = vp.in_dll(cf, "kCFRunLoopDefaultMode").value
            self.array_callbacks = ctypes.addressof(ctypes.c_char.in_dll(cf, "kCFTypeArrayCallBacks"))
        except ValueError as exc:
            raise OSError(TEXT["constants"]) from exc


class FsEventsWatcher:
    """One recursive FSEvents stream for a scan root (constructor raises OSError off macOS)."""

    name = "fsevents"

    def __init__(self, root: str) -> None:
        self.root = os.path.abspath(root)
        self._real = os.path.realpath(self.root)
        self._api = _Api()
        # the boot volume group's Data volume, unless the root lies inside it (then paths match)
        self._twins = tuple(
            t for t in volume_group_twins("/") if self._real != t and not self._real.startswith(t + "/")
        )
        self._callback = _CALLBACK(self._on_events)  # kept alive as long as the stream
        self._lock = threading.RLock()  # sink calls and the run loop reference vs. stop()
        self._loop: int | None = None  # the thread's CFRunLoop while it runs
        self._sink: Sink | None = None
        self._thread: threading.Thread | None = None
        self._started = threading.Event()
        self._stopping = False
        self._state: WatchState = "starting"
        self._detail = ""
        self._events = 0
        self._overflows = 0

    def start(self, sink: Sink) -> None:
        if self._thread is not None:
            raise RuntimeError(TEXT["started"])
        self._sink = sink
        self._thread = threading.Thread(target=self._run, name="sk-fsevents", daemon=True)
        self._thread.start()
        self._started.wait(1.0)

    def add_dirs(self, dirs: Iterable[str], mtimes: Sequence[int] | None = None, since_ns: int = 0) -> None:
        pass  # recursive stream, from the scan's start

    def stop(self) -> None:
        with self._lock:
            if self._stopping:
                return
            self._stopping = True
            if self._loop is not None:
                self._api.stop_loop(self._loop)
                self._api.wake_loop(self._loop)
        thread = self._thread
        if thread is not None and thread is not threading.current_thread():
            thread.join(0.9)
        if self._state != "failed":
            self._state = "stopped"

    def status(self) -> WatchStatus:
        return WatchStatus(
            self._state,
            self._detail,
            watched_dirs=-1 if self._state == "running" else 0,
            overflows=self._overflows,
            events=self._events,
        )

    def _run(self) -> None:
        api = self._api
        stream: int | None = None
        started = False
        try:
            stream = self._create_stream()
            loop = api.current_loop()
            api.schedule(stream, loop, api.default_mode)
            if not api.start(stream):
                raise OSError(TEXT["start_failed"])
            started = True
            with self._lock:
                self._loop = loop
            self._state = "running"
            self._started.set()
            while not self._stopping and self._state == "running":
                if api.run_in_mode(api.default_mode, 0.25, 0) == _RUN_FINISHED:
                    raise OSError(TEXT["no_source"])
        except Exception as exc:
            logger.warning(TEXT["log_failed"].format(path=self.root, error=exc))
            self._state, self._detail = "failed", TEXT["unavailable"].format(error=exc)
            if started:  # events may have been lost since; a stream that never ran lost nothing
                self._emit([Change("overflow", self.root, is_dir=True)])
        finally:
            with self._lock:
                self._loop = None
            if stream is not None:
                if started:
                    api.stop(stream)
                api.invalidate(stream)
                api.release_stream(stream)
            self._started.set()

    def _create_stream(self) -> int:
        api = self._api
        path = api.string(None, os.fsencode(self.root))
        if not path:
            raise OSError(TEXT["no_string"])
        try:
            values = (ctypes.c_void_p * 1)(path)
            paths = api.array(None, values, 1, api.array_callbacks)
            if not paths:
                raise OSError(TEXT["no_array"])
            try:
                stream = api.create(None, self._callback, None, paths, _SINCE_NOW, _LATENCY, _CREATE_FLAGS)
            finally:
                api.release(paths)  # the stream keeps its own copy
        finally:
            api.release(path)
        if not stream:
            raise OSError(TEXT["create_failed"])
        return int(stream)

    def _local(self, raw: bytes | None) -> str | None:
        """An event path as the scan spells it (FSEvents reports the canonical one); None when it
        is not in the tree (a part of the Data volume no firmlink leads to)."""
        path = os.fsdecode(raw or b"")
        if len(path) > 1:
            path = path.rstrip("/")
        for twin in self._twins:
            if path == twin or path.startswith(twin + "/"):
                mapped = through_firmlinks(path, twin)
                if mapped is None:
                    return None
                path = mapped
                break
        real = self._real
        if real != self.root and (path == real or path.startswith(real.rstrip("/") + "/")):
            path = self.root + path[len(real) :]
        return path

    def _on_events(self, _stream: Any, _info: Any, count: int, paths: Any, flags: Any, ids: Any) -> None:
        try:
            events = [
                (path, int(flags[k]), int(ids[k]))
                for k in range(count)
                if (path := self._local(paths[k])) is not None
            ]
            changes, root_lost = map_events(events, self.root)
            if changes:
                self._emit(changes)
            if root_lost:
                self._state, self._detail = "failed", TEXT["root_lost"]
                self._api.stop_loop(self._api.current_loop())
        except Exception:
            logger.exception(TEXT["log_callback"])

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


__all__ = ["FsEventsWatcher", "map_events"]
