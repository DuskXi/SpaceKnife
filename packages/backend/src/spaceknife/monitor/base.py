"""What every platform file watcher implements (docs/monitor.md §2).

A watcher reports *where* something changed; the backend works out *what* by listing the folder
again. Only a file whose size or times changed may be updated from the event's own numbers.
Watchers never coalesce or drop silently: losing events is reported as an ``overflow`` change.
"""

from __future__ import annotations

import sys
from collections.abc import Callable, Iterable
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

    def add_dirs(self, dirs: Iterable[str]) -> None:
        """Folders that are now part of the scanned tree, shallow first (per-folder watchers and
        polling need them; recursive watchers ignore the call). Safe from any thread."""

    def stop(self) -> None:
        """Stop within a second; ``sink`` is not called afterwards. Idempotent."""

    def status(self) -> WatchStatus: ...


def create_watcher(root: str, *, poll_interval: float = 30.0, poll_rate: int = 2000) -> Watcher:
    """The best watcher this platform offers for ``root`` (a scan root folder).

    ``poll_interval``: seconds between polling rounds; ``poll_rate``: folders stat'ed per second at
    most while polling (polling stays a light background load).
    """
    if sys.platform == "win32":
        from .win import RdcwWatcher

        return RdcwWatcher(root)
    if sys.platform.startswith("linux"):
        from .linux import InotifyWatcher

        return InotifyWatcher(root, poll_interval=poll_interval, poll_rate=poll_rate)
    if sys.platform == "darwin":
        try:
            from .mac import FsEventsWatcher

            return FsEventsWatcher(root)
        except OSError:
            pass  # CoreServices not usable: poll
    from .poll import PollWatcher

    return PollWatcher(root, poll_interval=poll_interval, poll_rate=poll_rate)


__all__ = ["Change", "ChangeKind", "Sink", "WatchState", "WatchStatus", "Watcher", "create_watcher"]
