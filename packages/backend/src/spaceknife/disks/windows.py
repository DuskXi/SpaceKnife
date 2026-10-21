"""Windows: physical disks, their links and the volumes on them, without administrator rights.

Each disk is opened with no access rights at all (the storage property queries need none; no
sector is read):

- ``IOCTL_STORAGE_QUERY_PROPERTY``: the device descriptor (model, serial, bus, removable), seek
  penalty (rotational), TRIM, and the protocol-specific data: ATA IDENTIFY (SATA speeds, words 76
  and 77) or the NVMe Identify Controller (serial, model, firmware).
- The protocol-specific data also carries what the disk records about itself: the NVMe SMART /
  Health log (page 02h) and the ATA Device Statistics log (04h, pages 01h and 05h).
- ``IOCTL_DISK_GET_DRIVE_GEOMETRY_EX``: the size; ``IOCTL_STORAGE_GET_DEVICE_NUMBER``: the disk
  number of Disk Management; ``IOCTL_DISK_PERFORMANCE``: bytes read and written since the boot.
- The device tree (SetupAPI / CfgMgr) above the disk: the PCIe link of an NVMe drive's controller,
  or of the add-in controller a SATA disk hangs on (DEVPKEY_PciDevice_*Link*), and for a USB disk
  the hub's connection query (``IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX(_V2)``).
- ``IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`` on each volume: the disks under it and how much of each.
"""

from __future__ import annotations

import ctypes
import os
import uuid
from collections.abc import Sequence
from ctypes import wintypes
from datetime import datetime
from functools import cache
from typing import Any

from loguru import logger

from spaceknife_protocol.machine import DiskHealth, DiskInterface, DiskIo, DiskLink, HealthNote, PhysicalDisk

from ..scan.media import classify, system_mount
from ..scan.volume import VolumeInfo
from . import Probe
from .health import ata_sector_size, ata_statistics, boot_time, nvme_health
from .links import pcie, pcie_gen_code, sata_identify, usb_windows

# STORAGE_BUS_TYPE (winioctl.h) -> interface
_BUS: dict[int, DiskInterface] = {
    1: "scsi",
    2: "ata",
    3: "ata",
    7: "usb",
    8: "raid",
    10: "sas",
    11: "sata",
    12: "sd",
    13: "sd",
    14: "virtual",
    15: "virtual",
    16: "raid",
    17: "nvme",
}
# the bus names scan.media.classify knows
_MEDIA_BUS = {
    "nvme": "nvme",
    "usb": "usb",
    "sata": "sata",
    "sd": "sd",
    "virtual": "virtual",
    "raid": "virtual",
}

IOCTL_STORAGE_QUERY_PROPERTY = 0x2D1400
IOCTL_STORAGE_GET_DEVICE_NUMBER = 0x2D1080
IOCTL_DISK_GET_DRIVE_GEOMETRY_EX = 0x700A0
IOCTL_DISK_PERFORMANCE = 0x70020
IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS = 0x560000
IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX = 0x220448
IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX_V2 = 0x22045C

GUID_DEVINTERFACE_DISK = "53f56307-b6bf-11d0-94f2-00a0c91efb8b"
GUID_DEVINTERFACE_USB_HUB = "f18a0e88-c30c-11d0-8815-00a0c906bed8"
DEVPKEY_PCI = "3ab22e31-8264-4b4e-9af5-a8d2d8e33e62"  # CurrentLinkSpeed 9, Width 10, MaxSpeed 11, MaxWidth 12
DEVPKEY_DEVICE = "a45c254e-df1c-4efd-8020-67d146a850e0"  # Address 30 (a USB device's port)


class _GUID(ctypes.Structure):
    _fields_ = [
        ("d1", ctypes.c_uint32),
        ("d2", ctypes.c_uint16),
        ("d3", ctypes.c_uint16),
        ("d4", ctypes.c_ubyte * 8),
    ]

    @classmethod
    def of(cls, text: str) -> _GUID:
        u = uuid.UUID(text)
        g = cls()
        g.d1, g.d2, g.d3 = u.fields[0], u.fields[1], u.fields[2]
        g.d4[:] = list(u.bytes[8:])
        return g


class _PROPKEY(ctypes.Structure):
    _fields_ = [("fmtid", _GUID), ("pid", ctypes.c_uint32)]


class _DEVINFO_DATA(ctypes.Structure):
    _fields_ = [
        ("cbSize", wintypes.DWORD),
        ("ClassGuid", _GUID),
        ("DevInst", wintypes.DWORD),
        ("Reserved", ctypes.c_void_p),
    ]


class _INTERFACE_DATA(ctypes.Structure):
    _fields_ = [
        ("cbSize", wintypes.DWORD),
        ("InterfaceClassGuid", _GUID),
        ("Flags", wintypes.DWORD),
        ("Reserved", ctypes.c_void_p),
    ]


@cache
def _api() -> Any:
    """kernel32 / setupapi / cfgmgr32 with their signatures (loaded on first use)."""

    class Api:
        pass

    api = Api()
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    k32.CreateFileW.restype = wintypes.HANDLE
    k32.CreateFileW.argtypes = [
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.HANDLE,
    ]
    k32.DeviceIoControl.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
        ctypes.c_void_p,
    ]
    k32.CloseHandle.argtypes = [wintypes.HANDLE]
    k32.GetVolumeNameForVolumeMountPointW.argtypes = [wintypes.LPCWSTR, wintypes.LPWSTR, wintypes.DWORD]
    setup = ctypes.WinDLL("setupapi", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    setup.SetupDiGetClassDevsW.restype = ctypes.c_void_p
    setup.SetupDiGetClassDevsW.argtypes = [
        ctypes.POINTER(_GUID),
        wintypes.LPCWSTR,
        wintypes.HWND,
        wintypes.DWORD,
    ]
    setup.SetupDiEnumDeviceInterfaces.argtypes = [
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.POINTER(_GUID),
        wintypes.DWORD,
        ctypes.POINTER(_INTERFACE_DATA),
    ]
    setup.SetupDiGetDeviceInterfaceDetailW.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(_INTERFACE_DATA),
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
        ctypes.POINTER(_DEVINFO_DATA),
    ]
    setup.SetupDiDestroyDeviceInfoList.argtypes = [ctypes.c_void_p]
    cm = ctypes.WinDLL("cfgmgr32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    cm.CM_Get_Parent.argtypes = [ctypes.POINTER(wintypes.DWORD), wintypes.DWORD, wintypes.ULONG]
    cm.CM_Get_Device_IDW.argtypes = [wintypes.DWORD, ctypes.c_wchar_p, wintypes.ULONG, wintypes.ULONG]
    cm.CM_Get_DevNode_PropertyW.argtypes = [
        wintypes.DWORD,
        ctypes.POINTER(_PROPKEY),
        ctypes.POINTER(wintypes.ULONG),
        ctypes.c_void_p,
        ctypes.POINTER(wintypes.ULONG),
        wintypes.ULONG,
    ]
    cm.CM_Get_Device_Interface_List_SizeW.argtypes = [
        ctypes.POINTER(wintypes.ULONG),
        ctypes.POINTER(_GUID),
        wintypes.LPCWSTR,
        wintypes.ULONG,
    ]
    cm.CM_Get_Device_Interface_ListW.argtypes = [
        ctypes.POINTER(_GUID),
        wintypes.LPCWSTR,
        ctypes.c_wchar_p,
        wintypes.ULONG,
        wintypes.ULONG,
    ]
    api.k32, api.setup, api.cm = k32, setup, cm  # type: ignore[attr-defined]
    return api


_INVALID = wintypes.HANDLE(-1).value


def _open(path: str, access: int = 0, share: int = 3) -> int | None:
    h = _api().k32.CreateFileW(path, access, share, None, 3, 0, None)  # OPEN_EXISTING
    return None if not h or h == _INVALID else int(h)


def _close(h: int) -> None:
    _api().k32.CloseHandle(h)


def _ioctl(h: int, code: int, data: bytes, size: int) -> bytes | None:
    inbuf = ctypes.create_string_buffer(data, len(data)) if data else None
    out = ctypes.create_string_buffer(size)
    got = wintypes.DWORD(0)
    ok = _api().k32.DeviceIoControl(h, code, inbuf, len(data), out, size, ctypes.byref(got), None)
    return out.raw[: got.value] if ok else None


def _u32(b: bytes, at: int) -> int:
    return int.from_bytes(b[at : at + 4], "little")


def _query(h: int, prop: int, size: int, extra: bytes = b"\0\0\0\0") -> bytes | None:
    """STORAGE_PROPERTY_QUERY {PropertyId, PropertyStandardQuery, AdditionalParameters}."""
    return _ioctl(h, IOCTL_STORAGE_QUERY_PROPERTY, prop.to_bytes(4, "little") + bytes(4) + extra, size)


def _text(b: bytes, at: int) -> str:
    if not at or at >= len(b):
        return ""
    end = b.find(b"\0", at)
    return b[at : end if end >= 0 else len(b)].decode("ascii", "replace").strip()


def _protocol_data(
    h: int, protocol: int, data_type: int, value: int, length: int, sub: int = 0
) -> bytes | None:
    """StorageDeviceProtocolSpecificProperty: STORAGE_PROTOCOL_SPECIFIC_DATA (40 bytes) follows the
    query's two DWORDs; the answer is a STORAGE_PROTOCOL_DATA_DESCRIPTOR (8 bytes) with the same
    40-byte block, then the data."""
    fields = (protocol, data_type, value, sub, 40, length, 0, 0, 0, 0)
    spec = b"".join(x.to_bytes(4, "little") for x in fields)
    raw = _query(h, 50, 8 + 40 + length, spec)
    return raw[48 : 48 + length] if raw and len(raw) >= 48 + length else None


def _ascii(b: bytes) -> str:
    return b.decode("ascii", "replace").replace("\0", " ").strip()


@cache
def _key(fmtid: str, pid: int) -> _PROPKEY:
    k = _PROPKEY()
    k.fmtid, k.pid = _GUID.of(fmtid), pid
    return k


def _prop_u32(inst: int, fmtid: str, pid: int) -> int | None:
    kind, value, size = wintypes.ULONG(0), ctypes.c_uint32(0), wintypes.ULONG(4)
    r = _api().cm.CM_Get_DevNode_PropertyW(
        inst, ctypes.byref(_key(fmtid, pid)), ctypes.byref(kind), ctypes.byref(value), ctypes.byref(size), 0
    )
    return int(value.value) if r == 0 else None


def _device_id(inst: int) -> str:
    buf = ctypes.create_unicode_buffer(512)
    return buf.value if _api().cm.CM_Get_Device_IDW(inst, buf, 512, 0) == 0 else ""


def _parent(inst: int) -> int | None:
    p = wintypes.DWORD(0)
    return int(p.value) if _api().cm.CM_Get_Parent(ctypes.byref(p), inst, 0) == 0 else None


def _chain(inst: int) -> list[tuple[int, str]]:
    """The device node and its ancestors, nearest first (at most 8)."""
    out: list[tuple[int, str]] = []
    cur: int | None = inst
    while cur is not None and len(out) < 8:
        out.append((cur, _device_id(cur).upper()))
        cur = _parent(cur)
    return out


def _disk_nodes() -> dict[int, tuple[str, int]]:
    """Disk number -> (interface path, device node) of every present disk."""
    api = _api()
    guid = _GUID.of(GUID_DEVINTERFACE_DISK)
    found: dict[int, tuple[str, int]] = {}
    info = api.setup.SetupDiGetClassDevsW(ctypes.byref(guid), None, None, 0x12)  # PRESENT | INTERFACE
    if not info or info == _INVALID:
        return found
    try:
        index = 0
        while True:
            iface = _INTERFACE_DATA()
            iface.cbSize = ctypes.sizeof(iface)
            if not api.setup.SetupDiEnumDeviceInterfaces(
                info, None, ctypes.byref(guid), index, ctypes.byref(iface)
            ):
                break
            index += 1
            need = wintypes.DWORD(0)
            api.setup.SetupDiGetDeviceInterfaceDetailW(
                info, ctypes.byref(iface), None, 0, ctypes.byref(need), None
            )
            if need.value < 8:
                continue
            detail = ctypes.create_string_buffer(need.value)
            # SP_DEVICE_INTERFACE_DETAIL_DATA_W.cbSize: 8 on 64-bit (a DWORD and a WCHAR, aligned)
            ctypes.c_uint32.from_buffer(detail).value = 8 if ctypes.sizeof(ctypes.c_void_p) == 8 else 6
            node = _DEVINFO_DATA()
            node.cbSize = ctypes.sizeof(node)
            if not api.setup.SetupDiGetDeviceInterfaceDetailW(
                info, ctypes.byref(iface), detail, need, None, ctypes.byref(node)
            ):
                continue
            path = ctypes.wstring_at(ctypes.addressof(detail) + 4)
            h = _open(path)
            if h is None:
                continue
            try:
                number = _ioctl(h, IOCTL_STORAGE_GET_DEVICE_NUMBER, b"", 12)
            finally:
                _close(h)
            if number and len(number) >= 8 and _u32(number, 0) == 7:  # FILE_DEVICE_DISK
                found[_u32(number, 4)] = (path, int(node.DevInst))
    finally:
        api.setup.SetupDiDestroyDeviceInfoList(info)
    return found


def _pci_link(inst: int) -> DiskLink | None:
    gen, width, max_gen, max_width = (_prop_u32(inst, DEVPKEY_PCI, pid) for pid in (9, 10, 11, 12))
    return pcie(pcie_gen_code(gen), width or None, pcie_gen_code(max_gen), max_width or None)


def _hub_path(hub: int) -> str | None:
    api = _api()
    guid = _GUID.of(GUID_DEVINTERFACE_USB_HUB)
    instance = _device_id(hub)
    size = wintypes.ULONG(0)
    if api.cm.CM_Get_Device_Interface_List_SizeW(ctypes.byref(size), ctypes.byref(guid), instance, 0) != 0:
        return None
    if size.value <= 1:
        return None
    buf = ctypes.create_unicode_buffer(size.value)
    if api.cm.CM_Get_Device_Interface_ListW(ctypes.byref(guid), instance, buf, size.value, 0) != 0:
        return None
    return buf.value or None


def _usb_link(device: int) -> DiskLink | None:
    """The link of a USB device, from its hub's connection query for its port."""
    port, hub = _prop_u32(device, DEVPKEY_DEVICE, 30), _parent(device)
    path = _hub_path(hub) if hub is not None else None
    if not port or path is None:
        return None
    h = _open(path, access=0x40000000, share=2)  # as USBView: GENERIC_WRITE, FILE_SHARE_WRITE
    if h is None:
        h = _open(path)
    if h is None:
        return None
    try:
        # USB_NODE_CONNECTION_INFORMATION_EX (packed): ConnectionIndex(4) DeviceDescriptor(18)
        # CurrentConfigurationValue(1) Speed(1) …
        ex = _ioctl(
            h, IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX, port.to_bytes(4, "little") + bytes(508), 512
        )
        speed = ex[4 + 18 + 1] if ex and len(ex) > 23 else None
        # USB_NODE_CONNECTION_INFORMATION_EX_V2: ConnectionIndex Length SupportedUsbProtocols Flags
        query = b"".join(x.to_bytes(4, "little") for x in (port, 16, 0x7, 0))
        v2 = _ioctl(h, IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX_V2, query, 16)
        flags = _u32(v2, 12) if v2 and len(v2) >= 16 else None
    finally:
        _close(h)
    return usb_windows(speed, flags)


def _links(inst: int, interface: DiskInterface) -> tuple[DiskLink | None, DiskLink | None]:
    """(the disk's own link, its controller's PCIe link) from the device tree above the disk."""
    chain = _chain(inst)[1:]
    usb = next((node for node, iid in chain if iid.startswith("USB\\VID_")), None)
    if usb is not None:
        return _usb_link(usb), None
    pci = next((node for node, iid in chain if iid.startswith("PCI\\")), None)
    if pci is None:
        return None, None
    if interface == "nvme":
        return _pci_link(pci), None
    return None, _pci_link(pci)  # a SATA / SAS controller: chipset ones report no link


def _io(h: int, since: datetime | None) -> DiskIo | None:
    """DISK_PERFORMANCE: BytesRead and BytesWritten since the boot (LARGE_INTEGERs at 0 and 8)."""
    perf = _ioctl(h, IOCTL_DISK_PERFORMANCE, b"", 88)
    if not perf or len(perf) < 16:
        return None
    read, written = int.from_bytes(perf[0:8], "little"), int.from_bytes(perf[8:16], "little")
    return DiskIo(read_bytes=read, written_bytes=written, since=since)


def _disk(number: int, path: str, inst: int, since: datetime | None = None) -> PhysicalDisk | None:
    h = _open(path)
    if h is None:
        return None
    try:
        dev = _query(h, 0, 1024)  # StorageDeviceProperty -> STORAGE_DEVICE_DESCRIPTOR
        if not dev or len(dev) < 32:
            return None
        removable, bus = bool(dev[10]), _u32(dev, 28)
        vendor, model, firmware, serial = (_text(dev, _u32(dev, at)) for at in (12, 16, 20, 24))
        seek = _query(h, 7, 12)  # DEVICE_SEEK_PENALTY_DESCRIPTOR
        rotational = bool(seek[8]) if seek and len(seek) > 8 else None
        trim_d = _query(h, 8, 12)  # DEVICE_TRIM_DESCRIPTOR
        trim = bool(trim_d[8]) if trim_d and len(trim_d) > 8 else None
        geometry = _ioctl(h, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, b"", 256)
        size = int.from_bytes(geometry[24:32], "little") if geometry and len(geometry) >= 32 else None
        interface = _BUS.get(bus, "other")
        link: DiskLink | None = None
        health: DiskHealth | None = None
        note: HealthNote = ""
        if interface == "sata":
            words = _protocol_data(h, 2, 1, 0, 512)  # ProtocolTypeAta, AtaDataTypeIdentify
            if words:
                link = sata_identify(
                    int.from_bytes(words[152:154], "little"), int.from_bytes(words[154:156], "little")
                )
            general = _protocol_data(h, 2, 2, 0x04, 512, sub=1)  # AtaDataTypeLogPage: Device Statistics
            if general:
                temps = _protocol_data(h, 2, 2, 0x04, 512, sub=5)
                health = ata_statistics(general, temps, ata_sector_size(words or b""))
            note = "" if health else "unsupported"
        elif interface == "nvme":
            ident = _protocol_data(h, 3, 1, 1, 4096)  # ProtocolTypeNvme, Identify, CNS = controller
            if ident:
                serial = _ascii(ident[4:24]) or serial
                model = _ascii(ident[24:64]) or model
                firmware = _ascii(ident[64:72]) or firmware
            log = _protocol_data(h, 3, 2, 2, 512)  # NVMeDataTypeLogPage, SMART / Health (02h)
            health = nvme_health(log) if log else None
            note = "" if health else "unsupported"
        elif interface == "usb":
            note = "usb"
        elif interface not in ("virtual", "raid"):
            note = "unavailable"
        io = _io(h, since)
    finally:
        _close(h)
    own, controller = _links(inst, interface)
    if own is not None and (link is None or interface != "sata"):
        link = own
    media_bus = _MEDIA_BUS.get(interface, "other")
    return PhysicalDisk(
        id=f"PhysicalDrive{number}",
        number=number,
        model=" ".join(model.split()),
        vendor=vendor if vendor.lower() not in ("", "ata", "nvme") else "",
        serial=serial.rstrip("."),
        firmware=firmware,
        size=size or None,
        media=classify(media_bus, rotational, removable),
        interface=interface,
        removable=removable,
        trim=trim,
        link=link,
        controller_link=controller,
        health=health,
        health_note=note if size else "",
        io=io,
    )


def _volume_name(mount_point: str) -> str | None:
    buf = ctypes.create_unicode_buffer(64)
    root = mount_point if mount_point.endswith("\\") else mount_point + "\\"
    if not _api().k32.GetVolumeNameForVolumeMountPointW(root, buf, 64):
        return None
    return buf.value.rstrip("\\")


def _extents(mount_point: str) -> list[tuple[int, int, int]]:
    """(disk number, starting offset, length) of the volume's extents."""
    name = _volume_name(mount_point)
    h = _open(name) if name else None
    if h is None:
        return []
    try:
        raw = _ioctl(h, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, b"", 8 + 24 * 32)
    finally:
        _close(h)
    if not raw or len(raw) < 8:
        return []
    out = []
    for i in range(min(_u32(raw, 0), (len(raw) - 8) // 24)):
        at = 8 + i * 24  # DISK_EXTENT: DiskNumber(4) pad(4) StartingOffset(8) ExtentLength(8)
        out.append(
            (
                _u32(raw, at),
                int.from_bytes(raw[at + 8 : at + 16], "little"),
                int.from_bytes(raw[at + 16 : at + 24], "little"),
            )
        )
    return out


def probe(volumes: Sequence[VolumeInfo]) -> Probe:
    result = Probe()
    since = boot_time()
    for number, (path, inst) in sorted(_disk_nodes().items()):
        try:
            disk = _disk(number, path, inst, since)
        except Exception as exc:  # one odd device must not hide the others
            logger.opt(exception=exc).debug("disk {} probe failed", number)
            continue
        if disk is not None:
            result.disks.append(disk)
    system = os.path.normcase(system_mount())
    for v in volumes:
        if v.is_remote:  # a slot without a medium still names its disk
            continue
        try:
            extents = _extents(v.mount_point)
        except Exception as exc:
            logger.opt(exception=exc).debug("extents of {} failed", v.mount_point)
            continue
        if not extents:
            continue
        result.parts[v.mount_point] = [(f"PhysicalDrive{n}", length or None) for n, _start, length in extents]
        result.offsets[v.mount_point] = extents[0][1]
        if os.path.normcase(v.mount_point) == system:
            on = {f"PhysicalDrive{n}" for n, _s, _l in extents}
            result.disks = [d.model_copy(update={"system": True}) if d.id in on else d for d in result.disks]
    return result


__all__ = ["probe"]
