"""This computer's physical disks and which volumes live on them (the 「本机磁盘」 page).

:func:`probe` asks the system, without administrator rights and without reading a sector:

- Windows (:mod:`.windows`): storage property queries on each disk, the PCIe link of its controller
  and the USB hub's connection query (CfgMgr), the disk extents of each volume.
- Linux (:mod:`.linux`): sysfs (``/sys/block``, the PCI device's link, ``ata_link``, the USB
  device) and ``/dev/disk/by-id``.
- macOS (:mod:`.darwin`): ``diskutil`` and ``system_profiler``.

Nothing here may fail the page: a probe that breaks gives fewer facts, never an error.
"""

from __future__ import annotations

import sys
from collections.abc import Sequence
from dataclasses import dataclass, field

from loguru import logger

from spaceknife_protocol.machine import PhysicalDisk

from ..scan.volume import VolumeInfo


@dataclass
class Probe:
    disks: list[PhysicalDisk] = field(default_factory=list)
    # mount point -> [(disk id, bytes of the volume on that disk or None)], in disk order
    parts: dict[str, list[tuple[str, int | None]]] = field(default_factory=dict)
    # mount point -> where the volume starts on its first disk (orders the volumes on a disk)
    offsets: dict[str, int] = field(default_factory=dict)


def probe(volumes: Sequence[VolumeInfo]) -> Probe:
    """The disks and the volumes' place on them (blocking: run it in a thread)."""
    try:
        if sys.platform == "win32":
            from .windows import probe as run
        elif sys.platform == "darwin":
            from .darwin import probe as run
        else:
            from .linux import probe as run
        return run(volumes)
    except Exception as exc:  # a broken probe must never break the page
        logger.opt(exception=exc).warning("disk probe failed")
        return Probe()


__all__ = ["Probe", "probe"]
