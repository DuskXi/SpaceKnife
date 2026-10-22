"""File change hooks (docs/monitor.md): platform watchers and the change pipeline."""

from .base import (
    Change,
    ChangeKind,
    Sink,
    Watcher,
    WatcherKind,
    WatchState,
    WatchStatus,
    create_watcher,
    register_watcher,
    unregister_watcher,
    watcher_kinds,
)

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
