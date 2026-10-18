"""Disk-load sampling and the per-disk I/O governor (docs/native.md §4).

Typical wiring::

    registry = IoRegistry(max_workers=pool_size)
    await registry.start()                              # at backend start: pre-scan baseline
    gov = await registry.governor_for_path(root)        # shared by every scan on that disk
    gov.set_level("quiet")
    permit = await gov.permit(); fut = loop.run_in_executor(...); permit.bind(fut)
    unsubscribe = gov.subscribe(push_status)            # 2 Hz IoStatus pushes
"""

from __future__ import annotations

from .disks import DiskId, DiskRef, disk_for_path, disk_ref_for_path, local_disks
from .governor import GovernorCore, IoGovernor, IoStatus, Level, LoadPoint, Permit, level_limit
from .priority import io_priority_is_low, set_low_io_priority, set_thread_low_io_priority
from .registry import IoRegistry
from .sampler import DiskSample, DiskSampler, default_sampler

__all__ = [
    "DiskId",
    "DiskRef",
    "DiskSample",
    "DiskSampler",
    "GovernorCore",
    "IoGovernor",
    "IoRegistry",
    "IoStatus",
    "Level",
    "LoadPoint",
    "Permit",
    "default_sampler",
    "disk_for_path",
    "disk_ref_for_path",
    "io_priority_is_low",
    "level_limit",
    "local_disks",
    "set_low_io_priority",
    "set_thread_low_io_priority",
]
