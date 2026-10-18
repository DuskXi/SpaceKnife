"""Snapshot compare (docs/manage.md §3): two trees merged by path, with both ends and the change."""

from .engine import (
    DOMINANT,
    MINOR,
    STATUS_LABELS,
    Change,
    CompareCancelled,
    DiffTree,
    Direction,
    Status,
    compare,
)

__all__ = [
    "DOMINANT",
    "MINOR",
    "STATUS_LABELS",
    "Change",
    "CompareCancelled",
    "DiffTree",
    "Direction",
    "Status",
    "compare",
]
