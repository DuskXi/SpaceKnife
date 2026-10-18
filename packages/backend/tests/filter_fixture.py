"""Fixture tree for filter compatibility with SpaceSniffer (shared by the oracle recorder and tests).

Ages are relative to the moment the tree is built; every probe stays far from boundaries so a
few seconds between building and scanning never change the outcome.
"""

from __future__ import annotations

import os
import sys
from dataclasses import dataclass
from pathlib import Path

VERSION = 2
DAY = 86400.0


@dataclass(frozen=True)
class F:
    path: str
    size: int
    created_days: float
    modified_days: float
    accessed_days: float
    hidden: bool = False
    readonly: bool = False
    system: bool = False


FILES: list[F] = [
    F("notes.txt", 100, 2, 2, 1),
    F("big.txt", 3_000_000, 40, 10, 2),
    F("photo.jpg", 20_000_000, 400, 40, 10),
    F("app.log", 5000, 400, 400, 400),
    F("exact4000.bin", 4000, 2, 2, 2),
    F("README", 10, 10, 10, 10),
    F("UPPER.TXT", 2048, 10, 10, 10),
    F("a1.txt", 1024, 10, 10, 10),
    F("music.mp3", 4_000_000, 10, 10, 10, hidden=True, readonly=True),
    F("sub/movie.mp4", 50_000_000, 10, 2, 1),
    F("sub/tiny.jpg", 200, 2, 10, 2),
    F("sub/deeper/temp/x.txt", 300, 10, 10, 10),
    F("temp/cache.txt", 1500, 2, 1, 1, hidden=True),
    F("temp/data.zip", 8_000_000, 2, 40, 40, readonly=True),
    F("Program Files/x/setup.exe", 2_000_000, 10, 10, 10, hidden=True, system=True),
    F("zero.dat", 0, 10, 10, 10),
]

PROBES: list[str] = [
    "",
    # names and wildcards
    "*.txt",
    "*.TXT",
    "?1.txt",
    "a*",
    "*o*",
    "README",
    "*.txt;*.jpg",
    "*.txt; *.jpg",
    "|*.txt",
    "|*.txt;|*.jpg",
    "*.txt;|a*",
    # sizes (default = disk size)
    ">1mb",
    "<1kb",
    ">1mb;<10mb",
    ">10mb;<1mb",
    ">1m",
    ">1048576b",
    ">3999b",
    ">4000b",
    "s>4000b",
    "d>4000b",
    "size>1mb",
    "disksize>1mb",
    "|>1mb",
    "*.txt;>1mb",
    "*.txt;*.jpg;>1mb",
    # ages (default = modify time)
    "<5d",
    ">30d",
    "<1week",
    ">1month",
    ">1year",
    "m<5d",
    "c<5d",
    "a<5d",
    "c>30d",
    "m<5d;m>1.5d",
    "c<5d;m>30d",
    "m<5d;a<5d",
    "|<5d",
    "<5days;>1mb",
    # attributes
    ":attr:h",
    ":attr:r",
    ":attr:s",
    ":attr:h+r",
    ":attr:h,r",
    ":attr:-h",
    ":attr:h-r",
    "|:attr:h",
    ":attr:h;*.txt",
    # classes
    ":class:Pictures/Images",
    ":class:Video/Movies",
    ":class:Pictures/Images;*.txt",
    "|:class:Pictures/Images",
    ":class:Pictures/Images;:class:Video/Movies",
    # folder masks
    "\\temp",
    "|\\temp",
    "\\sub",
    "\\sub;*.txt",
    "\\sub;\\temp",
    "\\*program*",
    "|\\*program*",
    "\\temp;|*.zip",
    # combinations across kinds
    "*.jpg;<5d",
    "*.jpg;>1mb;\\sub",
    "|*.txt;>1mb",
    # round 2: settle open questions
    "m<5d;m>36h",
    "m>36h",
    "m>1.5d",
    "<5d;>36h",
    ">1mb;>2mb",
    "s>3999b",
    "s<4000b",
    "s>4001b",
    "s<4001b",
    "d<1kb",
    "d<4097b",
    ">1k",
    ">1kb",
    ">=1mb",
    "> 1mb",
    "<2w",
    "<2weeks",
    ">35h",
    "<1y",
    "<30s",
    "create<5d",
    "modify<5d",
    "access<5d",
    "modified<5d",
    "filesize>1mb",
    "logicalsize>1mb",
    "clustersize>1mb",
    ":attr:a",
    ":attr:-a",
    ":attrs:h",
    ":attr:hidden",
    ":attr:readonly+hidden",
    ":attr:h;:attr:r",
    "*.txt;:class:Video/Movies",
    ":class:pictures/images",
    ":class:Pictures",
    ":red",
    "|:red",
    ":tag:red",
    ":tag:all",
    "\\notes*",
    "*.txt;\\temp;>1kb",
    "|README;|*.txt;|*.jpg",
    "*",
    "*.*",
]


# Round 3/4: a separate mini-tree with sizes between 10^n and 2^n (decimal vs binary units) and ages
# just past 30/31 and 365/366 days (length of a month and a year). No sub-folders.
UNIT_FILES: list[F] = [
    F("k1010.bin", 1010, 10, 10, 10),
    F("m1030k.bin", 1_030_000, 10, 10, 10),
    F("age30_5.bin", 10, 400, 30.5, 400),
    F("age31_5.bin", 10, 400, 31.5, 400),
    F("age365_5.bin", 10, 400, 365.5, 400),
    F("age366_5.bin", 10, 400, 366.5, 400),
]
UNIT_PROBES: list[str] = [
    ">1mb",
    "s>1mb",
    "s>1kb",
    "s<1kb",
    "s>1000b",
    "s<1023b",
    ">1month",
    ">1months",
    ">1y",
    ">1year",
    ">1years",
    ">30d",
    ">31d",
]

# Probes re-run on the main tree (results merged into "results", replacing earlier rounds).
RECHECK: list[str] = [
    ":attr:r",
    "m<5d;m>1.5d",
    "m>1.5d",
    ">1k",
    ">=1mb",
    r"\notes*",
    ":class:Pictures",
    r"\fixture",
    r"\ss-oracle*",
    "|>1mb",
    "|<5d",
    ">1m",
    # round 4: spellings, numbers, quoting, keywords, help examples
    "d<16k",
    ">500",
    "<5",
    ">0b",
    ">1gb",
    ">1tb",
    ">1kib",
    ">1MB",
    "M<5D",
    "<5 d",
    ">1 mb",
    ">1min",
    ">1mon",
    ">1minute",
    ">1minutes",
    ">1hour",
    ">1hours",
    ">1sec",
    ">1second",
    ">1seconds",
    ">1day",
    "created<5d",
    "accessed<5d",
    "| *.txt",
    r"\ temp",
    r"\temp\x",
    r"\sub*",
    r"\*",
    r"\sub;|\deeper",
    r"|\sub;|\temp",
    r"*.txt;*.jpg;|\sub",
    ":tag:r+y",
    ":tag:all-red",
    ":yellow",
    ":all",
    ":foo",
    ":tag:purple",
    ":attr:z",
    ":attr:compressed",
    ":attr:ni",
    ":attr:sp",
    ":attr:ads",
    ":attr:a+h-s",
    ":attr:a,h,-s",
    "|:attr:a",
    ":ATTR:H",
    ":attr: h",
    ":CLASS:video/movies",
    ":class: Video/Movies",
    ":class:Audio/Music",
    ":class:Executables/System files",
    ":class:Archives/Disk images",
    ">1mb;|>10mb",
    "*.t?t",
    "a*;b*",
    ";;*.txt;",
    '"notes.txt"',
    '*"*',
    "s>4000b;s<4000b",
    r"|*.exe;|*.dll;\*temp*;|\*windows*",
    "c>1month",
    "m<1day",
]


def _set_times_windows(path: Path, created: float, modified: float, accessed: float) -> None:
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
    k32.CreateFileW.restype = wintypes.HANDLE
    handle = k32.CreateFileW(str(path), 0x100, 0x7, None, 3, 0x02000000, None)  # FILE_WRITE_ATTRIBUTES
    if handle in (None, wintypes.HANDLE(-1).value):
        raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined,unused-ignore]

    def ft(ts: float) -> wintypes.FILETIME:
        v = int((ts + 11644473600) * 10_000_000)
        return wintypes.FILETIME(v & 0xFFFFFFFF, v >> 32)

    c, a, m = ft(created), ft(accessed), ft(modified)
    ok = k32.SetFileTime(handle, ctypes.byref(c), ctypes.byref(a), ctypes.byref(m))
    k32.CloseHandle(handle)
    if not ok:
        raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined,unused-ignore]


def _set_attrs_windows(path: Path, f: F) -> None:
    import ctypes

    attrs = 0x20  # ARCHIVE
    if f.hidden:
        attrs |= 0x2
    if f.readonly:
        attrs |= 0x1
    if f.system:
        attrs |= 0x4
    ctypes.WinDLL("kernel32").SetFileAttributesW(str(path), attrs)  # type: ignore[attr-defined,unused-ignore]


def build(root: Path, now: float, files: list[F] | None = None) -> None:
    """Create the fixture tree. Attributes and creation times are only applied on Windows."""
    for f in FILES if files is None else files:
        path = root / f.path
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, "wb") as fh:
            fh.truncate(f.size)
        created, modified, accessed = (
            now - f.created_days * DAY,
            now - f.modified_days * DAY,
            now - f.accessed_days * DAY,
        )
        if sys.platform == "win32":
            _set_times_windows(path, created, modified, accessed)
            _set_attrs_windows(path, f)
        else:
            os.utime(path, (accessed, modified))
