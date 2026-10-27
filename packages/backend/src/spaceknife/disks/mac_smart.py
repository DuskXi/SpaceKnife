"""macOS: each NVMe disk's SMART / Health log (log page 02h, 512 bytes), without root or other tools.

IOKit's NVMe SMART user client (the ``NVMeSMARTLib`` plug-in, public header
``IOKit/storage/nvme/NVMeSMARTLibExternal.h``), the path smartmontools and others use; it needs no
administrator rights and no entitlement, on Intel Macs with an NVMe SSD (all T2 Macs) and on Apple
silicon, and for NVMe drives in Thunderbolt / USB4 enclosures. USB bridges don't pass it on.

- find each ``IOBlockStorageDevice`` (the NVMe namespace) with ``"NVMe SMART Capable" = true`` and
  the BSD name of the whole disk below it (``IOBlockStorageDriver`` → ``IOMedia`` with ``Whole``);
- ``IOCreatePlugInInterfaceForService`` → ``QueryInterface`` → ``SMARTReadData`` (``GetLogPage``
  02h as a fallback) → release, at once: the user client is exclusive, so it is never held open.

ctypes only, in this process (CoreFoundation is not fork-safe), one read at a time. Any failure
gives nothing for that disk (another app such as a disk monitor may hold the client); a framework
that can't be loaded gives nothing at all.
"""

from __future__ import annotations

import ctypes as C
import re
import sys
import threading
import uuid
from typing import Any

from loguru import logger

_IOKIT = "/System/Library/Frameworks/IOKit.framework/IOKit"
_CF = "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
_UTF8 = 0x08000100  # kCFStringEncodingUTF8
_PLANE = b"IOService"  # kIOServicePlane
LOG_BYTES = 512  # the SMART / Health Information log page
_WHOLE = re.compile(r"disk\d+")

USER_CLIENT_TYPE = "AA0FA6F9-C2D6-457F-B10B-59A13253292F"  # kIONVMeSMARTUserClientTypeID
SMART_INTERFACE = "CCD1DB19-FD9A-4DAF-BF95-12454B230AB6"  # kIONVMeSMARTInterfaceID
PLUGIN_INTERFACE = "C244E858-109C-11D4-91D4-0050E4C6426F"  # kIOCFPlugInInterfaceID

_vp, _u16, _u32, _i32 = C.c_void_p, C.c_uint16, C.c_uint32, C.c_int32


class CFUUIDBytes(C.Structure):
    """16 separate bytes like the C struct: it is passed by value (REFIID), so no array member."""

    _fields_ = [(f"byte{i}", C.c_uint8) for i in range(16)]


def uuid_bytes(text: str) -> CFUUIDBytes:
    """CFUUIDBytes of a UUID string: byte0 is its first hex pair."""
    return CFUUIDBytes.from_buffer_copy(uuid.UUID(text).bytes)


# IUNKNOWN_C_GUTS (CFPlugInCOM.h); every `this` is the interface handle (Interface **)
_QueryInterface = C.CFUNCTYPE(_i32, _vp, CFUUIDBytes, C.POINTER(_vp))  # HRESULT (this, REFIID, LPVOID *)
_Ulong = C.CFUNCTYPE(_u32, _vp)  # AddRef / Release: ULONG (this)
_IUNKNOWN: list[tuple[str, Any]] = [
    ("_reserved", _vp),
    ("QueryInterface", _QueryInterface),
    ("AddRef", _Ulong),
    ("Release", _Ulong),
]


class IOCFPlugInInterface(C.Structure):
    """IOCFPlugIn.h"""

    _fields_ = [
        *_IUNKNOWN,
        ("version", _u16),
        ("revision", _u16),
        ("Probe", _vp),
        ("Start", _vp),
        ("Stop", _vp),
    ]


class IONVMeSMARTInterface(C.Structure):
    """NVMeSMARTLibExternal.h; only this prefix is ever read (reserved2 … follow)."""

    _fields_ = [
        *_IUNKNOWN,
        ("version", _u16),
        ("revision", _u16),
        ("SMARTReadData", C.CFUNCTYPE(_i32, _vp, _vp)),  # IOReturn (this, NVMeSMARTData *: 512 bytes)
        ("GetIdentifyData", C.CFUNCTYPE(_i32, _vp, _vp, _u32)),  # IOReturn (this, void *: 4096, nsid)
        ("reserved0", C.c_uint64),
        ("reserved1", C.c_uint64),
        ("GetLogPage", C.CFUNCTYPE(_i32, _vp, _vp, _u32, _u32)),  # IOReturn (this, buf, lid, dwords - 1)
    ]


class _Lib:
    """The IOKit and CoreFoundation calls; every io_object_t and CF object taken is released."""

    def __init__(self) -> None:
        iokit, cf = C.CDLL(_IOKIT), C.CDLL(_CF)

        def fn(lib: Any, name: str, restype: Any, *argtypes: Any) -> Any:
            f = getattr(lib, name)
            f.restype, f.argtypes = restype, list(argtypes)
            return f

        self.matching = fn(iokit, "IOServiceMatching", _vp, C.c_char_p)
        self.services = fn(iokit, "IOServiceGetMatchingServices", _i32, _u32, _vp, C.POINTER(_u32))
        self.next = fn(iokit, "IOIteratorNext", _u32, _u32)
        self.retain = fn(iokit, "IOObjectRetain", _i32, _u32)
        self.release = fn(iokit, "IOObjectRelease", _i32, _u32)
        self.conforms = fn(iokit, "IOObjectConformsTo", _i32, _u32, C.c_char_p)
        iterator = C.POINTER(_u32)
        self.child_iter = fn(iokit, "IORegistryEntryGetChildIterator", _i32, _u32, C.c_char_p, iterator)
        self.parent = fn(iokit, "IORegistryEntryGetParentEntry", _i32, _u32, C.c_char_p, C.POINTER(_u32))
        self.prop = fn(iokit, "IORegistryEntryCreateCFProperty", _vp, _u32, _vp, _vp, _u32)
        self.create_plugin = fn(
            iokit, "IOCreatePlugInInterfaceForService", _i32, _u32, _vp, _vp, C.POINTER(_vp), C.POINTER(_i32)
        )
        self.destroy_plugin = fn(iokit, "IODestroyPlugInInterface", _i32, _vp)
        self.cf_release = fn(cf, "CFRelease", None, _vp)
        self.type_of = fn(cf, "CFGetTypeID", C.c_ulong, _vp)
        self.bool_type = fn(cf, "CFBooleanGetTypeID", C.c_ulong)()
        self.string_type = fn(cf, "CFStringGetTypeID", C.c_ulong)()
        self.bool_value = fn(cf, "CFBooleanGetValue", C.c_bool, _vp)
        self.string_get = fn(cf, "CFStringGetCString", C.c_bool, _vp, C.c_char_p, C.c_long, _u32)
        cfstr = fn(cf, "CFStringCreateWithCString", _vp, _vp, C.c_char_p, _u32)
        uuid_from = fn(cf, "CFUUIDCreateFromString", _vp, _vp, _vp)  # pointers only: no by-value bytes
        # kept for the life of the process
        self.keys = {k: cfstr(None, k.encode(), _UTF8) for k in ("NVMe SMART Capable", "Whole", "BSD Name")}
        self.user_client = uuid_from(None, cfstr(None, USER_CLIENT_TYPE.encode(), _UTF8))
        self.plugin_if = uuid_from(None, cfstr(None, PLUGIN_INTERFACE.encode(), _UTF8))
        if not all(self.keys.values()) or not self.user_client or not self.plugin_if:
            raise OSError("CoreFoundation objects not created")

    # ------------------------------------------------------------------ registry

    def _bool(self, entry: int, key: str) -> bool:
        v = self.prop(entry, self.keys[key], None, 0)
        if not v:
            return False
        try:
            return bool(self.type_of(v) == self.bool_type and self.bool_value(v))
        finally:
            self.cf_release(v)

    def _str(self, entry: int, key: str) -> str | None:
        v = self.prop(entry, self.keys[key], None, 0)
        if not v:
            return None
        try:
            if self.type_of(v) != self.string_type:
                return None
            buf = C.create_string_buffer(128)
            return buf.value.decode(errors="replace") if self.string_get(v, buf, 128, _UTF8) else None
        finally:
            self.cf_release(v)

    def _children(self, entry: int) -> list[int]:
        """The entry's children in the service plane (each a reference to release)."""
        it = _u32(0)
        if self.child_iter(entry, _PLANE, C.byref(it)) != 0:
            return []
        out: list[int] = []
        try:
            while obj := self.next(it.value):
                out.append(obj)
        finally:
            self.release(it.value)
        return out

    def whole_bsd(self, device: int) -> str | None:
        """device → IOBlockStorageDriver → IOMedia with Whole = true → its BSD name ("disk0")."""
        drivers = self._children(device)
        try:
            for driver in drivers:
                if not self.conforms(driver, b"IOBlockStorageDriver"):
                    continue
                media = self._children(driver)
                try:
                    for m in media:
                        if self.conforms(m, b"IOMedia") and self._bool(m, "Whole"):
                            name = self._str(m, "BSD Name")
                            if name and _WHOLE.fullmatch(name):
                                return name
                finally:
                    for m in media:
                        self.release(m)
        finally:
            for d in drivers:
                self.release(d)
        return None

    def smart_node(self, device: int) -> int | None:
        """``device`` or its nearest ancestor (up to 3 up) with "NVMe SMART Capable" = true, as a
        new reference."""
        current, owned = device, False
        for _ in range(4):
            if self._bool(current, "NVMe SMART Capable"):
                if not owned:
                    self.retain(current)
                return current
            parent = _u32(0)
            ok = self.parent(current, _PLANE, C.byref(parent)) == 0 and parent.value
            if owned:
                self.release(current)
            if not ok:
                return None
            current, owned = parent.value, True
        if owned:
            self.release(current)
        return None

    # ------------------------------------------------------------------ the user client

    def read_log(self, node: int) -> bytes | None:
        """Open the user client, read the log, close it again."""
        plugin, smart, score = _vp(), _vp(), _i32(0)
        kr = self.create_plugin(node, self.user_client, self.plugin_if, C.byref(plugin), C.byref(score))
        if kr != 0 or not plugin.value:  # e.g. another app holds the (exclusive) client
            logger.debug("NVMe SMART: IOCreatePlugInInterfaceForService 0x{:08x}", kr & 0xFFFFFFFF)
            return None
        try:
            base = C.cast(plugin, C.POINTER(C.POINTER(IOCFPlugInInterface))).contents.contents
            hr = base.QueryInterface(plugin, uuid_bytes(SMART_INTERFACE), C.byref(smart))
            if hr != 0 or not smart.value:
                logger.debug("NVMe SMART: QueryInterface 0x{:08x}", hr & 0xFFFFFFFF)
                return None
            vt = C.cast(smart, C.POINTER(C.POINTER(IONVMeSMARTInterface))).contents.contents
            try:
                buf = C.create_string_buffer(4096)  # SMARTReadData writes 512 bytes; the rest is slack
                kr = vt.SMARTReadData(smart, buf) if vt.SMARTReadData else -1
                if kr != 0 and vt.GetLogPage:
                    C.memset(buf, 0, C.sizeof(buf))
                    kr = vt.GetLogPage(smart, buf, 0x02, LOG_BYTES // 4 - 1)  # dwords, zero-based
                if kr != 0:
                    logger.debug("NVMe SMART: read 0x{:08x}", kr & 0xFFFFFFFF)
                    return None
                raw = buf.raw[:LOG_BYTES]
                return raw if any(raw[:192]) else None
            finally:
                vt.Release(smart)
        finally:
            self.destroy_plugin(plugin)

    def logs(self) -> dict[str, bytes]:
        out: dict[str, bytes] = {}
        it = _u32(0)
        matching = self.matching(b"IOBlockStorageDevice")  # and its subclasses
        if not matching or self.services(0, matching, C.byref(it)) != 0:  # consumes ``matching``
            return out
        try:
            while device := self.next(it.value):
                try:
                    name = self.whole_bsd(device)  # no medium (an empty reader): nothing to ask
                    if not name or name in out:
                        continue
                    node = self.smart_node(device)
                    if node is None:
                        continue
                    try:
                        log = self.read_log(node)
                    finally:
                        self.release(node)
                    if log:
                        out[name] = log
                except (OSError, ValueError, TypeError) as exc:  # a NULL vtable entry …: this disk only
                    logger.debug("NVMe SMART: {}", exc)
                finally:
                    self.release(device)
        finally:
            self.release(it.value)
        return out


_lock = threading.Lock()  # one read at a time from this process (the user client is exclusive)
_lib: _Lib | None = None
_broken = False


def nvme_smart_logs() -> dict[str, bytes]:
    """Whole-disk BSD name → its 512-byte NVMe SMART / Health log; {} off macOS or on any failure.

    Blocking (one NVMe admin command per disk): call it in a thread, when the disks are read, never
    on a timer and never in a forked child."""
    global _lib, _broken
    if sys.platform != "darwin" or _broken:
        return {}
    with _lock:
        try:
            if _lib is None:
                _lib = _Lib()
            return _lib.logs()
        except Exception as exc:  # ctypes surprises must never break the disks probe
            logger.debug("NVMe SMART unavailable: {}", exc)
            if _lib is None:
                _broken = True  # the frameworks can't be loaded: don't try again
            return {}


__all__ = ["LOG_BYTES", "IOCFPlugInInterface", "IONVMeSMARTInterface", "nvme_smart_logs", "uuid_bytes"]
