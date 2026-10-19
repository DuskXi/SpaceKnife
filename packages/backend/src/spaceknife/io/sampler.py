"""Per-disk load samplers: busy fraction, average queue depth and read statistics over a window.

* Windows: PDH (``\\PhysicalDisk(*)\\% Idle Time``, ``Avg. Disk Queue Length``, and for reads
  ``Disk Reads/sec``, ``Avg. Disk sec/Read``, ``Disk Read Bytes/sec``), no elevation; falls back
  to the raw ``IOCTL_DISK_PERFORMANCE`` counters when the performance-counter subsystem is broken.
* Linux: ``/sys/block/<disk>/stat`` deltas (io_ticks → busy, time_in_queue → queue depth).
* macOS: IOKit block-storage statistics (what ``iostat`` reads: every ``IOBlockStorageDriver``'s
  ``Statistics``, named by the whole disk below it), read in process through ctypes; ``ioreg -a``
  as the fallback. No idle time there: busy is approximated by the queue depth. Without either
  the governor runs open loop.

:meth:`DiskSampler.sample` blocks (system calls, a subprocess for ``ioreg``): call it from a worker
thread. The first call only primes the counters and returns nothing.
"""

from __future__ import annotations

import os
import plistlib
import re
import struct
import subprocess
import sys
import time
from collections.abc import Iterable
from dataclasses import dataclass
from typing import Any, NamedTuple

from loguru import logger

from ..scan.media import media_of
from .disks import DiskId, DiskRef, linux_physical_disks, win_disk_id


@dataclass(frozen=True, slots=True)
class DiskSample:
    """Load of one disk over the window ``(t - dt, t]`` (``t`` on the ``time.monotonic`` clock)."""

    t: float
    dt: float
    busy: float  # fraction of the window with I/O outstanding (Task Manager "active time"), 0..1
    queue: float  # average number of outstanding I/Os (reads and writes)
    # reads only (scans only read; writes absorbed by an SSD's cache would distort the latency)
    read_iops: float | None = None
    read_latency: float | None = None  # mean seconds per read
    read_bps: float | None = None  # bytes read per second


def _clamp01(x: float) -> float:
    return 0.0 if x != x or x < 0.0 else 1.0 if x > 1.0 else x  # NaN → 0


class DiskSampler:
    """Base class: samples every disk it knows about; :meth:`watch` adds one."""

    name = "none"
    supported = False
    min_interval = 0.0  # seconds between samples this sampler can sustain cheaply

    def __init__(self) -> None:
        self._refs: dict[DiskId, DiskRef] = {}

    def sample(self) -> dict[DiskId, DiskSample]:
        return {}

    def refs(self) -> list[DiskRef]:
        return list(self._refs.values())

    def ref(self, disk: DiskId) -> DiskRef | None:
        return self._refs.get(disk)

    def watch(self, ref: DiskRef) -> None:
        if ref.sampled:
            known = self._refs.get(ref.id)
            if known is None or (known.media == "unknown" and ref.media != "unknown"):
                self._refs[ref.id] = ref

    def close(self) -> None:
        return None


class NullSampler(DiskSampler):
    """No load data on this platform: every governor runs open loop."""


# --------------------------------------------------------------------------- Windows: PDH

PDH_COUNTERS = {
    "idle": "\\PhysicalDisk(*)\\% Idle Time",
    "queue": "\\PhysicalDisk(*)\\Avg. Disk Queue Length",
    "latency": "\\PhysicalDisk(*)\\Avg. Disk sec/Read",
    "iops": "\\PhysicalDisk(*)\\Disk Reads/sec",
    "bps": "\\PhysicalDisk(*)\\Disk Read Bytes/sec",
}
_PDH_INSTANCE = re.compile(r"^\s*(\d+)\b")
_PDH_LETTER = re.compile(r"\b([A-Za-z]):")


def pdh_disk_number(instance: str) -> int | None:
    """Disk number of a ``PhysicalDisk`` instance name (``"3 C: D:"`` → 3; ``"_Total"`` → None)."""
    m = _PDH_INSTANCE.match(instance)
    return int(m.group(1)) if m else None


def pdh_instance_letters(instance: str) -> list[str]:
    return [f"{m.group(1).upper()}:\\" for m in _PDH_LETTER.finditer(instance)]


def pdh_samples(values: dict[str, dict[str, float]], t: float, dt: float) -> dict[DiskId, DiskSample]:
    """Combine formatted PDH arrays (counter key → instance → value) into per-disk samples."""
    out: dict[DiskId, DiskSample] = {}
    for inst, idle in values.get("idle", {}).items():
        number = pdh_disk_number(inst)
        if number is None:
            continue
        latency = values.get("latency", {}).get(inst)
        iops = values.get("iops", {}).get(inst)
        bps = values.get("bps", {}).get(inst)
        out[win_disk_id(number)] = DiskSample(
            t=t,
            dt=dt,
            busy=_clamp01(1.0 - idle / 100.0),
            queue=max(0.0, values.get("queue", {}).get(inst, 0.0)),
            read_iops=None if iops is None else max(0.0, iops),
            read_latency=None if latency is None else max(0.0, latency),
            read_bps=None if bps is None else max(0.0, bps),
        )
    return out


class PdhSampler(DiskSampler):
    name = "pdh"
    supported = True

    PDH_MORE_DATA = 0x800007D2
    FMT = 0x00000200 | 0x00008000  # PDH_FMT_DOUBLE | PDH_FMT_NOCAP100

    def __init__(self) -> None:
        super().__init__()
        import ctypes
        from ctypes import wintypes

        self._ct = ctypes
        pdh = ctypes.WinDLL("pdh")  # type: ignore[attr-defined,unused-ignore]
        for fn in ("PdhOpenQueryW", "PdhAddEnglishCounterW", "PdhCollectQueryData"):
            getattr(pdh, fn).restype = ctypes.c_uint32
        pdh.PdhGetFormattedCounterArrayW.restype = ctypes.c_uint32
        pdh.PdhGetFormattedCounterArrayW.argtypes = [
            wintypes.HANDLE,
            wintypes.DWORD,
            ctypes.POINTER(wintypes.DWORD),
            ctypes.POINTER(wintypes.DWORD),
            ctypes.c_void_p,
        ]
        pdh.PdhCloseQuery.argtypes = [wintypes.HANDLE]

        class Value(ctypes.Structure):
            _fields_ = [("CStatus", wintypes.DWORD), ("doubleValue", ctypes.c_double)]

        class Item(ctypes.Structure):
            _fields_ = [("szName", ctypes.c_wchar_p), ("FmtValue", Value)]

        self._item = Item
        self._pdh = pdh
        self._query: Any = wintypes.HANDLE()
        if status := pdh.PdhOpenQueryW(None, 0, ctypes.byref(self._query)):
            raise OSError(f"PdhOpenQueryW failed: 0x{status:08X}")
        self._counters: dict[str, Any] = {}
        try:
            for key, path in PDH_COUNTERS.items():
                handle = wintypes.HANDLE()
                status = pdh.PdhAddEnglishCounterW(self._query, path, 0, ctypes.byref(handle))
                if status:
                    if key in ("idle", "queue"):
                        raise OSError(f"PdhAddEnglishCounterW({path}) failed: 0x{status:08X}")
                    continue  # latency / IOPS / bytes only refine the Q_eff estimate
                self._counters[key] = handle
            if status := pdh.PdhCollectQueryData(self._query):
                raise OSError(f"PdhCollectQueryData failed: 0x{status:08X}")
        except BaseException:
            self.close()
            raise
        self._last = time.monotonic()

    def _array(self, handle: Any) -> dict[str, float]:
        ct = self._ct
        size = ct.c_ulong(0)
        count = ct.c_ulong(0)
        status = self._pdh.PdhGetFormattedCounterArrayW(
            handle, self.FMT, ct.byref(size), ct.byref(count), None
        )
        if status != self.PDH_MORE_DATA or not size.value:
            return {}
        buf = ct.create_string_buffer(size.value)
        status = self._pdh.PdhGetFormattedCounterArrayW(
            handle, self.FMT, ct.byref(size), ct.byref(count), buf
        )
        if status:
            return {}
        items = ct.cast(buf, ct.POINTER(self._item))
        out = {}
        for i in range(count.value):
            item = items[i]
            if item.FmtValue.CStatus in (0, 1) and item.szName:  # PDH_CSTATUS_VALID_DATA / NEW_DATA
                out[str(item.szName)] = float(item.FmtValue.doubleValue)
        return out

    def sample(self) -> dict[DiskId, DiskSample]:
        if self._query is None:
            return {}
        if self._pdh.PdhCollectQueryData(self._query):
            return {}
        now = time.monotonic()
        dt, self._last = now - self._last, now
        values = {key: self._array(handle) for key, handle in self._counters.items()}
        for inst in values.get("idle", {}):
            number = pdh_disk_number(inst)
            if number is not None and win_disk_id(number) not in self._refs:
                self._discover(number, inst)
        return pdh_samples(values, now, dt)

    def _discover(self, number: int, instance: str) -> None:
        letters = pdh_instance_letters(instance)
        mount = letters[0] if letters else ""
        media = media_of(mount, mount.rstrip("\\"), False) if mount else "unknown"
        self.watch(DiskRef(win_disk_id(number), number=number, media=media, mount=mount))

    def close(self) -> None:
        if self._query is not None:
            self._pdh.PdhCloseQuery(self._query)
            self._query = None


# --------------------------------------------------------------------------- Windows: raw counters


class DiskPerf(NamedTuple):
    """``DISK_PERFORMANCE`` (times in 100 ns units)."""

    bytes_read: int
    bytes_written: int
    read_time: int
    write_time: int
    idle_time: int
    reads: int
    writes: int
    query_time: int


def parse_disk_performance(raw: bytes) -> DiskPerf | None:
    if len(raw) < 64:
        return None
    b_read, b_written, read_t, write_t, idle_t, reads, writes, _qd, _split, query = struct.unpack_from(
        "<qqqqqIIIIq", raw
    )
    return DiskPerf(b_read, b_written, read_t, write_t, idle_t, reads, writes, query)


def perf_sample(prev: DiskPerf, cur: DiskPerf, t: float) -> DiskSample | None:
    span = cur.query_time - prev.query_time
    if span <= 0:
        return None
    io_time = (cur.read_time - prev.read_time) + (cur.write_time - prev.write_time)
    reads = (cur.reads - prev.reads) & 0xFFFFFFFF
    read_time = cur.read_time - prev.read_time
    if io_time < 0:
        return None
    dt = span * 1e-7
    return DiskSample(
        t=t,
        dt=dt,
        busy=_clamp01(1.0 - (cur.idle_time - prev.idle_time) / span),
        queue=io_time / span,
        read_iops=reads / dt,
        read_latency=read_time * 1e-7 / reads if reads and read_time >= 0 else None,
        read_bps=max(0, cur.bytes_read - prev.bytes_read) / dt,
    )


class WinPerfSampler(DiskSampler):
    """``IOCTL_DISK_PERFORMANCE`` on ``\\\\.\\PhysicalDriveN`` (access 0: no elevation)."""

    name = "ioctl"
    supported = True
    MAX_DISKS = 64

    def __init__(self) -> None:
        super().__init__()
        self._prev: dict[int, DiskPerf] = {}
        self._rescan = 0.0
        self._prev = self._read_all()
        if not self._prev:
            raise OSError("IOCTL_DISK_PERFORMANCE is not available")

    def _read(self, number: int) -> DiskPerf | None:
        from . import _win

        raw = _win.device_ioctl(f"\\\\.\\PhysicalDrive{number}", _win.IOCTL_DISK_PERFORMANCE, 88, grow=False)
        return parse_disk_performance(raw) if raw else None

    def _read_all(self) -> dict[int, DiskPerf]:
        now = time.monotonic()
        rescan = now >= self._rescan
        if rescan:
            self._rescan = now + 30.0  # pick up hot-plugged disks now and then
        numbers: Iterable[int] = range(self.MAX_DISKS) if rescan else list(self._prev)
        out = {}
        for n in numbers:
            perf = self._read(n)
            if perf is not None:
                out[n] = perf
                if win_disk_id(n) not in self._refs:
                    self.watch(DiskRef(win_disk_id(n), number=n))
        return out

    def sample(self) -> dict[DiskId, DiskSample]:
        cur = self._read_all()
        t = time.monotonic()
        out: dict[DiskId, DiskSample] = {}
        for n, perf in cur.items():
            prev = self._prev.get(n)
            if prev is not None and (s := perf_sample(prev, perf, t)) is not None:
                out[win_disk_id(n)] = s
        self._prev = cur
        return out


# --------------------------------------------------------------------------- Linux


class LinuxStat(NamedTuple):
    reads: int
    read_ms: int
    writes: int
    write_ms: int
    sectors_read: int  # 512-byte units
    in_flight: int
    io_ticks: int
    time_in_queue: int


def parse_linux_stat(text: str) -> LinuxStat | None:
    """``/sys/block/<disk>/stat`` (11, 15 or 17 fields; see Documentation/block/stat.rst)."""
    fields = text.split()
    if len(fields) < 11:
        return None
    try:
        v = [int(x) for x in fields[:11]]
    except ValueError:
        return None
    return LinuxStat(v[0], v[3], v[4], v[7], v[2], v[8], v[9], v[10])


def linux_sample(prev: LinuxStat, cur: LinuxStat, dt: float, t: float) -> DiskSample | None:
    if dt <= 0:
        return None
    d_ticks = cur.io_ticks - prev.io_ticks
    d_queue = cur.time_in_queue - prev.time_in_queue
    d_reads = cur.reads - prev.reads
    d_read_ms = cur.read_ms - prev.read_ms
    if d_ticks < 0 or d_queue < 0 or d_reads < 0 or d_read_ms < 0:  # counter wrapped / device replaced
        return None
    ms = dt * 1000.0
    return DiskSample(
        t=t,
        dt=dt,
        busy=_clamp01(d_ticks / ms),
        queue=d_queue / ms,
        read_iops=d_reads / dt,
        read_latency=d_read_ms / 1000.0 / d_reads if d_reads else None,
        read_bps=max(0, cur.sectors_read - prev.sectors_read) * 512 / dt,
    )


class SysfsSampler(DiskSampler):
    name = "sysfs"
    supported = True

    def __init__(self, root: str = "/sys") -> None:
        super().__init__()
        self._root = root
        self._prev: dict[DiskId, tuple[float, LinuxStat]] = {}
        for name in linux_physical_disks(root):
            self.watch(DiskRef(DiskId(name), name=name, media=media_of("", f"/dev/{name}", False)))
        self.sample()

    def _read(self, name: str) -> LinuxStat | None:
        try:
            with open(os.path.join(self._root, "block", name, "stat"), encoding="ascii") as fh:
                return parse_linux_stat(fh.read())
        except OSError:
            return None

    def sample(self) -> dict[DiskId, DiskSample]:
        out: dict[DiskId, DiskSample] = {}
        for ref in list(self._refs.values()):
            cur = self._read(ref.name or ref.id)
            now = time.monotonic()
            if cur is None:
                continue
            prev = self._prev.get(ref.id)
            self._prev[ref.id] = (now, cur)
            if prev is not None and (s := linux_sample(prev[1], cur, now - prev[0], now)) is not None:
                out[ref.id] = s
        return out


# --------------------------------------------------------------------------- macOS


class DarwinStat(NamedTuple):
    total_ns: int  # summed service time of all reads and writes
    reads: int = 0
    read_ns: int = 0
    read_bytes: int = 0


def _bsd_name(entry: Any, depth: int = 0) -> str | None:
    if not isinstance(entry, dict) or depth > 4:
        return None
    name = entry.get("BSD Name")
    if isinstance(name, str) and re.fullmatch(r"disk\d+", name):
        return name
    for child in entry.get("IORegistryEntryChildren", []) or []:
        if found := _bsd_name(child, depth + 1):
            return found
    return None


def parse_ioreg(raw: bytes) -> dict[str, DarwinStat]:
    """``ioreg -r -c IOBlockStorageDriver -a`` → whole-disk BSD name → cumulative statistics."""
    try:
        entries = plistlib.loads(raw)
    except (plistlib.InvalidFileException, ValueError, TypeError):
        return {}
    out: dict[str, DarwinStat] = {}
    for entry in entries if isinstance(entries, list) else [entries]:
        if not isinstance(entry, dict):
            continue
        stats = entry.get("Statistics")
        name = _bsd_name(entry)
        if not isinstance(stats, dict) or name is None:
            continue
        read_ns = int(stats.get("Total Time (Read)", 0))
        total = read_ns + int(stats.get("Total Time (Write)", 0))
        reads = int(stats.get("Operations (Read)", 0))
        out[name] = DarwinStat(total, reads, read_ns, int(stats.get("Bytes (Read)", 0)))
    return out


def darwin_sample(prev: DarwinStat, cur: DarwinStat, dt: float, t: float) -> DiskSample | None:
    d_ns, d_reads, d_read_ns = (
        cur.total_ns - prev.total_ns,
        cur.reads - prev.reads,
        cur.read_ns - prev.read_ns,
    )
    if dt <= 0 or d_ns < 0 or d_reads < 0 or d_read_ns < 0:
        return None
    queue = d_ns / 1e9 / dt
    return DiskSample(
        t=t,
        dt=dt,
        busy=_clamp01(queue),  # IOKit has no idle time: overlapping I/Os make this an upper bound
        queue=queue,
        read_iops=d_reads / dt,
        read_latency=d_read_ns / 1e9 / d_reads if d_reads else None,
        read_bps=max(0, cur.read_bytes - prev.read_bytes) / dt,
    )


_KCF_UTF8 = 0x08000100  # kCFStringEncodingUTF8
_KCF_SINT64 = 4  # kCFNumberSInt64Type
_STAT_KEYS = (
    "Statistics",
    "BSD Name",
    "Total Time (Read)",
    "Total Time (Write)",
    "Operations (Read)",
    "Bytes (Read)",
)


class _IOKit:
    """The IOKit and CoreFoundation calls :class:`IOKitSampler` needs (ctypes; macOS only)."""

    # ctypes functions, set in __init__ (declared for type checkers on other platforms)
    ctypes: Any
    matching: Any
    services: Any
    next: Any
    release_object: Any
    property: Any
    search: Any
    release: Any
    dict_get: Any
    number_get: Any
    string_get: Any
    type_of: Any
    number_type: int
    string_type: int
    keys: dict[str, Any]

    def __init__(self) -> None:
        import ctypes

        if sys.platform != "darwin":
            raise OSError("IOKit is macOS only")
        try:
            iokit = ctypes.cdll.LoadLibrary("/System/Library/Frameworks/IOKit.framework/IOKit")
            cf = ctypes.cdll.LoadLibrary("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
        except OSError as exc:
            raise OSError(f"IOKit not loadable: {exc}") from None
        vp, u32, i32, ulong = ctypes.c_void_p, ctypes.c_uint32, ctypes.c_int32, ctypes.c_ulong

        def fn(lib: Any, name: str, restype: Any, *argtypes: Any) -> Any:
            f = getattr(lib, name)
            f.restype, f.argtypes = restype, list(argtypes)
            return f

        self.ctypes = ctypes
        self.matching = fn(iokit, "IOServiceMatching", vp, ctypes.c_char_p)
        self.services = fn(iokit, "IOServiceGetMatchingServices", i32, u32, vp, ctypes.POINTER(u32))
        self.next = fn(iokit, "IOIteratorNext", u32, u32)
        self.release_object = fn(iokit, "IOObjectRelease", i32, u32)
        self.property = fn(iokit, "IORegistryEntryCreateCFProperty", vp, u32, vp, vp, u32)
        self.search = fn(iokit, "IORegistryEntrySearchCFProperty", vp, u32, ctypes.c_char_p, vp, vp, u32)
        self.release = fn(cf, "CFRelease", None, vp)
        self.dict_get = fn(cf, "CFDictionaryGetValue", vp, vp, vp)
        self.number_get = fn(cf, "CFNumberGetValue", ctypes.c_bool, vp, i32, vp)
        self.string_get = fn(cf, "CFStringGetCString", ctypes.c_bool, vp, ctypes.c_char_p, ctypes.c_long, u32)
        self.type_of = fn(cf, "CFGetTypeID", ulong, vp)
        self.number_type = fn(cf, "CFNumberGetTypeID", ulong)()
        self.string_type = fn(cf, "CFStringGetTypeID", ulong)()
        cfstr = fn(cf, "CFStringCreateWithCString", vp, vp, ctypes.c_char_p, u32)
        self.keys = {k: cfstr(None, k.encode(), _KCF_UTF8) for k in _STAT_KEYS}  # kept for the process

    def read(self) -> dict[str, DarwinStat]:
        """Cumulative statistics per whole disk (BSD name)."""
        ct = self.ctypes
        it = ct.c_uint32(0)
        matching = self.matching(b"IOBlockStorageDriver")
        if not matching or self.services(0, matching, ct.byref(it)) != 0:  # consumes ``matching``
            return {}
        out: dict[str, DarwinStat] = {}
        try:
            while entry := self.next(it.value):
                try:
                    name, stat = self._bsd_name(entry), self._stat(entry)
                    if name is not None and stat is not None:
                        out[name] = stat
                finally:
                    self.release_object(entry)
        finally:
            self.release_object(it.value)
        return out

    def _number(self, d: int, key: str) -> int:
        value = self.dict_get(d, self.keys[key])
        if not value or self.type_of(value) != self.number_type:
            return 0
        n = self.ctypes.c_int64(0)
        self.number_get(value, _KCF_SINT64, self.ctypes.byref(n))
        return int(n.value)

    def _stat(self, entry: int) -> DarwinStat | None:
        d = self.property(entry, self.keys["Statistics"], None, 0)
        if not d:
            return None
        try:
            read_ns = self._number(d, "Total Time (Read)")
            total = read_ns + self._number(d, "Total Time (Write)")
            reads = self._number(d, "Operations (Read)")
            return DarwinStat(total, reads, read_ns, self._number(d, "Bytes (Read)"))
        finally:
            self.release(d)

    def _bsd_name(self, entry: int) -> str | None:
        """The whole disk below a block storage driver: the first "BSD Name" in its subtree."""
        value = self.search(entry, b"IOService", self.keys["BSD Name"], None, 1)  # recursively
        if not value:
            return None
        try:
            if self.type_of(value) != self.string_type:
                return None
            buf = self.ctypes.create_string_buffer(64)
            if not self.string_get(value, buf, 64, _KCF_UTF8):
                return None
            m = re.match(r"disk\d+", buf.value.decode(errors="replace"))
            return m.group(0) if m else None
        finally:
            self.release(value)


class _DarwinSampler(DiskSampler):
    """Deltas of the cumulative IOKit statistics, per whole disk."""

    supported = True

    def __init__(self) -> None:
        super().__init__()
        self._prev: dict[str, tuple[float, DarwinStat]] = {}

    def _read(self) -> dict[str, DarwinStat]:
        raise NotImplementedError

    def sample(self) -> dict[DiskId, DiskSample]:
        cur = self._read()
        now = time.monotonic()
        out: dict[DiskId, DiskSample] = {}
        for name, stat in cur.items():
            disk = DiskId(name)
            if disk not in self._refs:
                self.watch(DiskRef(disk, name=name))
            prev = self._prev.get(name)
            self._prev[name] = (now, stat)
            if prev is not None and (s := darwin_sample(prev[1], stat, now - prev[0], now)) is not None:
                out[disk] = s
        return out


class IOKitSampler(_DarwinSampler):
    name = "iokit"
    min_interval = 0.0  # in process: microseconds per disk

    def __init__(self) -> None:
        super().__init__()
        self._api = _IOKit()
        found = self._read()
        if not found:
            raise OSError("IOKit reports no block storage statistics")
        logger.debug("disk load sampler iokit: {}", ", ".join(sorted(found)))
        self.sample()

    def _read(self) -> dict[str, DarwinStat]:
        return self._api.read()


class IoregSampler(_DarwinSampler):
    name = "ioreg"
    min_interval = 1.0  # one ioreg process per sample

    def __init__(self) -> None:
        super().__init__()
        if not self._read():
            raise OSError("ioreg returned no block storage statistics")
        self.sample()

    def _read(self) -> dict[str, DarwinStat]:
        try:
            raw = subprocess.run(
                ["ioreg", "-r", "-c", "IOBlockStorageDriver", "-a"],
                capture_output=True,
                timeout=3,
                check=True,
            ).stdout
        except (OSError, subprocess.SubprocessError):
            return {}
        return parse_ioreg(raw)


# --------------------------------------------------------------------------- selection


def default_sampler() -> DiskSampler:
    """The best sampler this platform offers (never raises: falls back to :class:`NullSampler`)."""
    candidates: list[type[DiskSampler]]
    if sys.platform == "win32":
        candidates = [PdhSampler, WinPerfSampler]
    elif sys.platform == "darwin":
        candidates = [IOKitSampler, IoregSampler]
    else:
        candidates = [SysfsSampler]
    for cls in candidates:
        try:
            sampler = cls()
        except Exception as exc:
            logger.warning("disk load sampler {} unavailable: {}", cls.name, exc)
            continue
        logger.debug("disk load sampler: {}", sampler.name)
        return sampler
    return NullSampler()


__all__ = [
    "DarwinStat",
    "DiskPerf",
    "DiskSample",
    "DiskSampler",
    "IOKitSampler",
    "IoregSampler",
    "LinuxStat",
    "NullSampler",
    "PdhSampler",
    "SysfsSampler",
    "WinPerfSampler",
    "darwin_sample",
    "default_sampler",
    "linux_sample",
    "parse_disk_performance",
    "parse_ioreg",
    "parse_linux_stat",
    "pdh_disk_number",
    "pdh_instance_letters",
    "pdh_samples",
    "perf_sample",
]
