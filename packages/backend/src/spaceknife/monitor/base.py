"""What every platform file watcher implements (docs/monitor.md §2).

A watcher reports *where* something changed; the backend works out *what* by listing the folder
again. Only a file whose size or times changed may be updated from the event's own numbers.
Watchers never coalesce or drop silently: losing events is reported as an ``overflow`` change.
"""

from __future__ import annotations

import sys
from collections.abc import Callable, Iterable, Sequence
from dataclasses import dataclass
from typing import Literal, Protocol

ChangeKind = Literal["modified", "created", "deleted", "renamed", "dir", "overflow"]
WatchState = Literal["starting", "running", "degraded", "failed", "stopped"]


@dataclass(frozen=True, slots=True)
class Change:
    kind: ChangeKind
    # absolute; renamed: the new path; dir: the folder whose entries changed; overflow: lost below here
    path: str
    old_path: str = ""  # renamed: the old path ("" when only the new name is known)
    is_dir: bool | None = None
    size: int | None = None  # modified, when the watcher knows: logical size, allocation, mtime (Unix s)
    alloc: int | None = None
    mtime: int | None = None


@dataclass(frozen=True, slots=True)
class WatchStatus:
    state: WatchState
    detail: str = ""  # one Chinese sentence the UI shows as is (degraded / failed reasons)
    watched_dirs: int = 0  # folders under live watch; -1: the whole tree (recursive watch)
    polled_dirs: int = 0  # folders kept in sync by polling
    overflows: int = 0
    events: int = 0


Sink = Callable[[list[Change]], None]


class Watcher(Protocol):
    name: str  # "rdcw" | "inotify" | "fsevents" | "poll"

    def start(self, sink: Sink) -> None:
        """Begin watching; ``sink`` is called from the watcher's own thread, a batch at least every
        50 ms while there are events."""

    def add_dirs(self, dirs: Iterable[str], mtimes: Sequence[int] | None = None, since_ns: int = 0) -> None:
        """Folders that are now part of the scanned tree, shallow first (per-folder watchers and
        polling need them; recursive watchers ignore the call). Safe from any thread.

        ``mtimes``: each folder's mtime as the scan recorded it (whole seconds); ``since_ns``: a
        wall-clock time no later than any of those listings (the scan's start). A per-folder
        watcher places a folder's watch a moment after the scan found it, maybe after the scan
        listed it too, and a change in between raises no event: once the folder is watched (or
        its polling baseline taken) it is checked against the record (``poll.listed_changed``)
        and reported as ``dir`` when it may have changed. Without ``mtimes`` nothing is checked."""

    def stop(self) -> None:
        """Stop within a second; ``sink`` is not called afterwards. Idempotent."""

    def status(self) -> WatchStatus: ...


@dataclass(frozen=True, slots=True)
class WatcherKind:
    """One way to watch a scan root (docs/monitor.md §2): the platform watchers register here.

    ``create_watcher`` takes the kind of highest ``priority`` whose ``platforms`` (``sys.platform``
    prefixes; empty: any) and ``filesystems`` (lower-case names as the volume reports them; empty:
    any) fit the root; a factory that raises ``OSError`` (a facility this machine lacks) passes the
    root on to the next kind. Polling, at the lowest priority, fits everywhere.
    """

    name: str
    factory: Callable[[str, float, int], Watcher]  # (root, poll_interval, poll_rate)
    platforms: tuple[str, ...] = ()
    filesystems: tuple[str, ...] = ()
    priority: int = 0
    owner: str = "builtin"

    def fits(self, platform: str, filesystem: str) -> bool:
        if self.platforms and not any(platform.startswith(p) for p in self.platforms):
            return False
        return not self.filesystems or filesystem.lower() in self.filesystems


_KINDS: dict[str, WatcherKind] = {}


def register_watcher(kind: WatcherKind) -> None:
    """Add (or replace) a way to watch; the built-in four are registered at import."""
    _KINDS[kind.name] = kind


def unregister_watcher(name: str) -> WatcherKind | None:
    return _KINDS.pop(name, None)


def watcher_kinds(platform: str | None = None, filesystem: str = "") -> list[WatcherKind]:
    """The kinds that fit, best first (all of them without ``platform``)."""
    kinds = list(_KINDS.values())
    if platform is not None:
        kinds = [k for k in kinds if k.fits(platform, filesystem)]
    return sorted(kinds, key=lambda k: (-k.priority, k.name))


def create_watcher(
    root: str, *, poll_interval: float = 30.0, poll_rate: int = 2000, filesystem: str = ""
) -> Watcher:
    """The best watcher this platform offers for ``root`` (a scan root folder).

    ``poll_interval``: seconds between polling rounds; ``poll_rate``: folders stat'ed per second at
    most while polling (polling stays a light background load). ``filesystem``: the root volume's
    file system, for kinds that only work on some.
    """
    for kind in watcher_kinds(sys.platform, filesystem):
        try:
            return kind.factory(root, poll_interval, poll_rate)
        except OSError:
            continue  # e.g. CoreServices not usable: the next kind (in the end, polling)
    from .poll import PollWatcher

    return PollWatcher(root, poll_interval=poll_interval, poll_rate=poll_rate)


def _rdcw(root: str, poll_interval: float, poll_rate: int) -> Watcher:
    if sys.platform == "win32":
        from .win import RdcwWatcher

        return RdcwWatcher(root)
    raise OSError("ReadDirectoryChangesW is Windows only")


def _inotify(root: str, poll_interval: float, poll_rate: int) -> Watcher:
    from .linux import InotifyWatcher

    return InotifyWatcher(root, poll_interval=poll_interval, poll_rate=poll_rate)


def _fsevents(root: str, poll_interval: float, poll_rate: int) -> Watcher:
    from .mac import FsEventsWatcher

    return FsEventsWatcher(root)


def _poll(root: str, poll_interval: float, poll_rate: int) -> Watcher:
    from .poll import PollWatcher

    return PollWatcher(root, poll_interval=poll_interval, poll_rate=poll_rate)


register_watcher(WatcherKind("rdcw", _rdcw, platforms=("win32",), priority=10))
register_watcher(WatcherKind("inotify", _inotify, platforms=("linux",), priority=10))
register_watcher(WatcherKind("fsevents", _fsevents, platforms=("darwin",), priority=10))
register_watcher(WatcherKind("poll", _poll, priority=-100))


__all__ = [
    "Change",
    "ChangeKind",
    "Sink",
    "WatchState",
    "WatchStatus",
    "Watcher",
    "WatcherKind",
    "create_watcher",
    "register_watcher",
    "unregister_watcher",
    "watcher_kinds",
]
