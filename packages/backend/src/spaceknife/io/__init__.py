"""Disk-load sampling and the per-disk I/O governor (docs/native.md §4).

Typical wiring::

    registry = IoRegistry(max_workers=pool_size)
    await registry.start()                              # at backend start: pre-scan baseline
    gov = await registry.governor_for_path(root)        # shared by every scan on that disk
    gov.set_level("quiet")
    permit = await gov.permit(); fut = loop.run_in_executor(...); permit.bind(fut)
    unsubscribe = gov.subscribe(push_status)            # 2 Hz IoStatus pushes

The names below load with their submodule on first use: the command-line scan without a disk-load
limit only needs ``io.priority``, and the governor brings in the protocol's pydantic models (docs/
packaging.md, 命令行扫描的启动).
"""

from __future__ import annotations

import importlib
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
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

_SUBMODULE = {
    "DiskId": "disks",
    "DiskRef": "disks",
    "disk_for_path": "disks",
    "disk_ref_for_path": "disks",
    "local_disks": "disks",
    "GovernorCore": "governor",
    "IoGovernor": "governor",
    "IoStatus": "governor",
    "Level": "governor",
    "LoadPoint": "governor",
    "Permit": "governor",
    "level_limit": "governor",
    "io_priority_is_low": "priority",
    "set_low_io_priority": "priority",
    "set_thread_low_io_priority": "priority",
    "IoRegistry": "registry",
    "DiskSample": "sampler",
    "DiskSampler": "sampler",
    "default_sampler": "sampler",
}


def __getattr__(name: str) -> Any:
    sub = _SUBMODULE.get(name)
    if sub is None:
        raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
    value = getattr(importlib.import_module(f"{__name__}.{sub}"), name)
    globals()[name] = value  # later lookups find it directly
    return value


def __dir__() -> list[str]:
    return sorted({*globals(), *__all__})
