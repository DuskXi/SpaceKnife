"""macOS: physical disks, their links and the volumes on them (no root needed).

- ``diskutil list -plist``: the whole disks; ``diskutil info -plist diskN``: model, protocol, solid
  state, removable, size, whether it is physical (APFS containers are synthesized disks).
- ``diskutil info -plist <mount point>``: the volume's disk (``APFSPhysicalStores`` for APFS, else
  ``ParentWholeDisk``).
- ``system_profiler -json`` (NVMe, SATA, USB): serial, firmware, TRIM and the links (NVMe "8.0 GT/s"
  and "x4", the SATA controller's negotiated speed, a USB device's speed), by BSD name.

The parsing is in pure functions over the decoded plist / JSON, tested with recorded samples.
"""

from __future__ import annotations

import json
import plistlib
import re
import subprocess
from collections.abc import Iterator, Sequence
from typing import Any

from loguru import logger

from spaceknife_protocol.machine import DiskInterface, DiskLink, PhysicalDisk

from ..scan.media import classify
from ..scan.volume import VolumeInfo
from . import Probe
from .links import gbps_text, pcie, pcie_gen_text, pcie_width_text, usb_mac

_PROFILES = ("SPNVMeDataType", "SPSerialATADataType", "SPUSBDataType", "SPUSBHostDataType")
_MEDIA_BUS = {"nvme": "nvme", "usb": "usb", "sata": "sata", "sd": "sd", "virtual": "virtual"}
_WHOLE = re.compile(r"^(disk\d+)")
_PROFILE_TEXT = (("device_serial", "serial"), ("device_revision", "firmware"), ("device_model", "model"))


def _plist(*args: str) -> dict[str, Any] | None:
    try:
        raw = subprocess.run(["diskutil", *args], capture_output=True, timeout=10, check=True).stdout
        data = plistlib.loads(raw)
    except (OSError, subprocess.SubprocessError, plistlib.InvalidFileException, ValueError):
        return None
    return data if isinstance(data, dict) else None


def _profile() -> dict[str, Any]:
    try:
        raw = subprocess.run(
            ["system_profiler", "-json", *_PROFILES], capture_output=True, timeout=30, check=True
        ).stdout
        data = json.loads(raw)
    except (OSError, subprocess.SubprocessError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def interface_of(protocol: str) -> DiskInterface:
    p = protocol.lower()
    if "pci" in p or "fabric" in p or "nvme" in p:
        return "nvme"
    if "usb" in p:
        return "usb"
    if "sata" in p:
        return "sata"
    if "thunderbolt" in p:
        return "thunderbolt"
    if "secure digital" in p or p == "sd":
        return "sd"
    if "image" in p or "virtual" in p:
        return "virtual"
    if "sas" in p:
        return "sas"
    return "other" if p else "unknown"


def disk_from_info(info: dict[str, Any]) -> PhysicalDisk | None:
    """A physical disk from ``diskutil info -plist diskN`` (None for a synthesized one)."""
    ident = str(info.get("DeviceIdentifier", ""))
    if not _WHOLE.fullmatch(ident) or info.get("VirtualOrPhysical") == "Virtual":
        return None
    interface = interface_of(str(info.get("BusProtocol", "")))
    solid = info.get("SolidState")
    rotational = None if solid is None else not bool(solid)
    removable = bool(info.get("Removable") or info.get("RemovableMedia") or info.get("Ejectable"))
    size = info.get("TotalSize") or info.get("Size") or info.get("IOKitSize")
    model = str(info.get("MediaName") or info.get("IORegistryEntryName") or "")
    media_bus = _MEDIA_BUS.get(interface, "other")
    return PhysicalDisk(
        id=ident,
        model=" ".join(model.replace(" Media", "").split()),
        size=int(size) if isinstance(size, int) and size > 0 else None,
        media=classify(media_bus, rotational, removable and interface == "usb"),
        interface=interface,
        removable=removable,
    )


Dict = dict[str, Any]


def _walk(node: Any, parents: tuple[Dict, ...] = ()) -> Iterator[tuple[Dict, tuple[Dict, ...]]]:
    """Every dict in the profile with the dicts above it (nearest last)."""
    if isinstance(node, dict):
        yield node, parents
        for value in node.values():
            yield from _walk(value, (*parents, node))
    elif isinstance(node, list):
        for item in node:
            yield from _walk(item, parents)


def _yes(value: Any) -> bool | None:
    if value is None:
        return None
    text = str(value).lower()
    return text in ("yes", "spsata_yes", "true", "1") or text.endswith("_yes")


def profile_facts(profile: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """BSD name -> {serial, firmware, model, trim, link} from ``system_profiler -json``."""
    facts: dict[str, dict[str, Any]] = {}
    for d, parents in _walk(profile):
        bsd = d.get("bsd_name")
        if not isinstance(bsd, str) or not _WHOLE.fullmatch(bsd):
            continue
        f = facts.setdefault(bsd, {})
        for key, field in _PROFILE_TEXT:
            if isinstance(d.get(key), str) and d[key].strip():
                f.setdefault(field, d[key].strip())
        trim = d.get("spnvme_trim_support", d.get("spsata_trim_support"))
        if trim is not None:
            f.setdefault("trim", _yes(trim))
        link: DiskLink | None = None
        if "spnvme_linkspeed" in d or "spnvme_linkwidth" in d:
            link = pcie(
                pcie_gen_text(str(d.get("spnvme_linkspeed", ""))),
                pcie_width_text(str(d.get("spnvme_linkwidth", ""))),
                None,
                None,
            )
        else:
            for above in reversed(parents):  # the SATA controller, or the USB device holding the medium
                if "spsata_negotiatedlinkspeed" in above:
                    rate = gbps_text(str(above["spsata_negotiatedlinkspeed"]))
                    top = gbps_text(str(above.get("spsata_linkspeed", "")))
                    link = DiskLink(kind="sata", rate_mbps=rate, max_rate_mbps=top) if rate or top else None
                    break
                speed = above.get("device_speed") or above.get("USBDeviceKeyLinkSpeed")
                if isinstance(speed, str):
                    link = usb_mac(speed)
                    break
        if link is not None:
            f.setdefault("link", link)
    return facts


def volume_disks(info: dict[str, Any]) -> list[tuple[str, int | None]]:
    """The disks of a volume from ``diskutil info -plist <mount point>``: APFS volumes share their
    container (no size of their own on the disk); others take their partition."""
    stores = info.get("APFSPhysicalStores")
    if isinstance(stores, list) and stores:
        out: list[tuple[str, int | None]] = []
        for s in stores:
            m = _WHOLE.match(str(s.get("APFSPhysicalStore", ""))) if isinstance(s, dict) else None
            if m:
                out.append((m.group(1), None))
        return out
    parent = str(info.get("ParentWholeDisk", ""))
    size = info.get("TotalSize") or info.get("Size")
    if not _WHOLE.fullmatch(parent):
        return []
    return [(parent, int(size) if isinstance(size, int) and size > 0 else None)]


def probe(volumes: Sequence[VolumeInfo]) -> Probe:
    result = Probe()
    listing = _plist("list", "-plist") or {}
    whole = [str(x) for x in listing.get("WholeDisks", []) if _WHOLE.fullmatch(str(x))]
    facts = profile_facts(_profile())
    for ident in whole:
        info = _plist("info", "-plist", ident)
        disk = disk_from_info(info) if info else None
        if disk is None:
            continue
        f = facts.get(ident, {})
        disk = disk.model_copy(
            update={
                "serial": f.get("serial", ""),
                "firmware": f.get("firmware", ""),
                "model": disk.model or f.get("model", ""),
                "trim": f.get("trim"),
                "link": f.get("link"),
            }
        )
        result.disks.append(disk)
    known = {d.id for d in result.disks}
    for v in volumes:
        if v.is_remote or v.reason:
            continue
        info = _plist("info", "-plist", v.mount_point)
        parts = [(disk, size) for disk, size in volume_disks(info or {}) if disk in known]
        if not parts:
            continue
        result.parts[v.mount_point] = parts
        if v.mount_point == "/":
            on = {disk for disk, _s in parts}
            result.disks = [d.model_copy(update={"system": True}) if d.id in on else d for d in result.disks]
    if not result.disks:
        logger.debug("no physical disks from diskutil")
    return result


__all__ = ["disk_from_info", "interface_of", "probe", "profile_facts", "volume_disks"]
