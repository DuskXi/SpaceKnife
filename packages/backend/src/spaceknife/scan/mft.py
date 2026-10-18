"""NTFS $MFT fast scan: raw $MFT -> ``MftTable`` columns -> :class:`NodeStore` (docs/native.md 2.1).

* :func:`parse_mft_table` uses the Rust parser when it is installed, else :func:`parse_mft_numpy`,
  a vectorised re-implementation with exactly the same rules and output (tests compare both
  column by column). Rules, shared with ``packages/native/src/mft/record.rs``:

  - signature 0 or a FILE record not in use: skipped; any other signature ("BAAD"): bad;
  - in-use FILE records: the update sequence (fixups, 512-byte stride) must match, then the
    attribute walk must stay inside the used size, else the record is bad (``bad_records``);
  - an in-use record with a base reference is an extension record: its attributes join the base
    record (in record-number order after the base's own) when the base is a good in-use base
    record with the same sequence number; this also covers non-resident ``$ATTRIBUTE_LIST``;
  - first ``$STANDARD_INFORMATION`` / unnamed ``$DATA`` first extent / ``$REPARSE_POINT`` wins,
    named ``$DATA`` first extents add up to ``ads_size`` / ``ads_alloc``;
  - one row per ``$FILE_NAME``, except a DOS 8.3 alias whose directory also holds a long name.

* :func:`read_raw_mft` reads a live volume's $MFT (administrator rights; Windows only).
* :func:`table_to_store` builds the node store in one go: the root is record 5, NTFS metafiles
  go to a ``$NTFS 元数据`` folder, unreachable entries to ``$孤立文件``.
"""

from __future__ import annotations

import io
import itertools
import os
import sys
from collections import defaultdict
from collections.abc import Callable, Mapping
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import Any, NamedTuple, Protocol

import numpy as np
from loguru import logger

from .. import native
from ..store.nodes import Flag, Kind, NodeStore, encode_name
from ..store.subtree import ranges_to_ids

MftTable = dict[str, Any]

META_FOLDER = "$NTFS 元数据"
ORPHAN_FOLDER = "$孤立文件"
ROOT_RECORD = 5
BADCLUS_RECORD = 8  # $BadClus: its $Bad stream spans the whole volume (see table_to_store)
FIRST_USER_RECORD = 16  # records 0–15 are metafiles / reserved

REF_MASK = (1 << 48) - 1
_FILE = int.from_bytes(b"FILE", "little")
_AT_SI, _AT_LIST, _AT_FN, _AT_DATA, _AT_REPARSE, _AT_END = 0x10, 0x20, 0x30, 0x80, 0xC0, 0xFFFFFFFF
_NS_DOS = 2
_COMPRESSED_OR_SPARSE = 0x0001 | 0x8000
_NAME_SURROGATE = 0x20000000
_EPOCH_TICKS = 116_444_736_000_000_000
_CHUNK = 16384  # records per vectorised step of the fallback
_PAD = 128  # bytes after a chunk so header-sized gathers never run off the end
_U32_MAX = 2**32 - 1

# Windows attributes -> Flag (same table as scan/worker.py and the Rust parser)
_ATTR_FLAGS = (
    (0x1, Flag.READONLY),
    (0x2, Flag.HIDDEN),
    (0x4, Flag.SYSTEM),
    (0x20, Flag.ARCHIVE),
    (0x100, Flag.TEMPORARY),
    (0x200, Flag.SPARSE),
    (0x400, Flag.REPARSE),
    (0x800, Flag.COMPRESSED),
    (0x1000, Flag.OFFLINE),
    (0x2000, Flag.NOT_INDEXED),
    (0x4000, Flag.ENCRYPTED),
    (0x40000, Flag.CLOUD),  # RECALL_ON_OPEN
    (0x400000, Flag.CLOUD),  # RECALL_ON_DATA_ACCESS
)

COLUMN_DTYPES: dict[str, np.dtype[Any]] = {
    "record": np.dtype("<u8"),
    "parent": np.dtype("<u8"),
    "name_off": np.dtype("<u4"),
    "name_len": np.dtype("<u2"),
    "kind": np.dtype("u1"),
    "flags": np.dtype("<u4"),
    "size": np.dtype("<i8"),
    "alloc": np.dtype("<i8"),
    "ads_size": np.dtype("<i8"),
    "ads_alloc": np.dtype("<i8"),
    "mtime": np.dtype("<i8"),
    "ctime": np.dtype("<i8"),
    "atime": np.dtype("<i8"),
    "nlink": np.dtype("<u2"),
}


def _check_sizes(record_size: int, cluster_size: int) -> None:
    if not 512 <= record_size <= 65536 or record_size % 512:
        raise ValueError(f"invalid record_size {record_size}")
    if cluster_size <= 0 or cluster_size & (cluster_size - 1):
        raise ValueError(f"invalid cluster_size {cluster_size}")


def parse_mft_table(data: bytes | bytearray | memoryview, record_size: int, cluster_size: int) -> MftTable:
    """``MftTable`` columns of a raw $MFT (native parser when available)."""
    mod = native.module()
    if mod is not None and native.has("parse_mft"):
        table: MftTable = mod.parse_mft(data, record_size, cluster_size)
        return table
    logger.info("native MFT parser not available: using the numpy fallback")
    return parse_mft_numpy(data, record_size, cluster_size)


# --------------------------------------------------------------------------- numpy fallback


class _Bytes:
    """Little-endian loads at arbitrary byte positions of one flat uint8 array."""

    __slots__ = ("flat",)

    def __init__(self, flat: np.ndarray) -> None:
        self.flat = flat

    def _take(self, pos: np.ndarray, width: int, dtype: str) -> np.ndarray:
        idx = pos[:, None] + np.arange(width, dtype=np.int64)
        return np.ascontiguousarray(self.flat[idx]).view(dtype).reshape(-1)

    def u8(self, pos: np.ndarray) -> np.ndarray:
        return np.asarray(self.flat[pos]).astype(np.int64)

    def u16(self, pos: np.ndarray) -> np.ndarray:
        return self._take(pos, 2, "<u2").astype(np.int64)

    def u32(self, pos: np.ndarray) -> np.ndarray:
        return self._take(pos, 4, "<u4").astype(np.int64)

    def u64(self, pos: np.ndarray) -> np.ndarray:
        return self._take(pos, 8, "<u8")

    def i64(self, pos: np.ndarray) -> np.ndarray:
        return self._take(pos, 8, "<i8")


def _header_col(recs: np.ndarray, off: int, dtype: str) -> np.ndarray:
    width = np.dtype(dtype).itemsize
    return np.ascontiguousarray(recs[:, off : off + width]).view(dtype).reshape(-1)


def _parse_chunk(
    block: np.ndarray, numbers: np.ndarray, record_size: int, parts: defaultdict[str, list[np.ndarray]]
) -> None:
    """Fixups and the attribute walk for one chunk of in-use FILE records (vectorised)."""
    m = len(block)
    rs = record_size
    flat = np.zeros(m * rs + _PAD, dtype=np.uint8)
    flat[: m * rs] = block.reshape(-1)
    b = _Bytes(flat)
    base = np.arange(m, dtype=np.int64) * rs

    usa_ofs = b.u16(base + 4)
    usa_cnt = b.u16(base + 6)
    sectors = rs // 512
    ok = (usa_cnt == sectors + 1) & (usa_ofs + 2 * usa_cnt <= 510)
    idx = np.flatnonzero(ok)
    rb = base[idx]
    uo = usa_ofs[idx]
    usn = b.u16(rb + uo)
    for i in range(sectors):
        p = rb + i * 512 + 510
        ok[idx[b.u16(p) != usn]] = False
        src = rb + uo + 2 + 2 * i
        flat[p] = flat[src]
        flat[p + 1] = flat[src + 1]

    used = b.u32(base + 0x18)
    first = b.u16(base + 0x14)
    ok &= (used <= rs) & (first >= usa_ofs + 2 * usa_cnt) & (first + 4 <= used)
    act = np.flatnonzero(ok)
    off = first[act]
    steps: list[tuple[np.ndarray, ...]] = []
    while act.size:
        u = used[act]
        p = base[act] + off
        f1 = off + 4 > u
        typ = b.u32(p)
        cont = ~f1 & (typ != _AT_END)
        alen = b.u32(p + 4)
        nonres = flat[p + 8] != 0
        nlen = b.u8(p + 9)
        vlen = b.u32(p + 0x10)
        voff = b.u16(p + 0x14)
        broken = (
            (off + 16 > u)
            | (alen < np.where(nonres, 0x40, 0x18))
            | (off + alen > u)
            | ((nlen > 0) & (b.u16(p + 10) + 2 * nlen > alen))
            | (~nonres & (voff + vlen > alen))
        )
        fail = f1 | (cont & broken)
        ok[act[fail]] = False
        sel = np.flatnonzero(cont & ~fail)
        steps.append(
            (act[sel], off[sel], p[sel], typ[sel], alen[sel], nonres[sel], nlen[sel], vlen[sel], voff[sel])
        )
        act = act[sel]
        off = off[sel] + alen[sel]
    parts["ok"].append(ok)
    if not steps:
        return
    r, aoff, p, typ, alen, nonres, nlen, vlen, voff = (
        np.concatenate(col) for col in zip(*steps, strict=True)
    )
    keep = ok[r]
    r, aoff, p, typ, alen, nonres, nlen, vlen, voff = (
        a[keep] for a in (r, aoff, p, typ, alen, nonres, nlen, vlen, voff)
    )
    rec = numbers[r]
    v = p + voff
    res = ~nonres

    sel = np.flatnonzero((typ == _AT_SI) & res & (vlen >= 0x24))
    parts["si_rec"].append(rec[sel])
    parts["si_off"].append(aoff[sel])
    parts["si_cr"].append(b.u64(v[sel]))
    parts["si_m"].append(b.u64(v[sel] + 0x08))
    parts["si_a"].append(b.u64(v[sel] + 0x18))
    parts["si_attrs"].append(b.u32(v[sel] + 0x20))

    sel = np.flatnonzero((typ == _AT_FN) & res & (vlen >= 0x42))
    chars = b.u8(v[sel] + 0x40)
    fit = 0x42 + 2 * chars <= vlen[sel]
    sel, chars = sel[fit], chars[fit]
    parts["fn_rec"].append(rec[sel])
    parts["fn_off"].append(aoff[sel])
    parts["fn_parent"].append(b.u64(v[sel]))
    parts["fn_ns"].append(b.u8(v[sel] + 0x41))
    parts["fn_units"].append(chars)
    parts["fn_utf16"].append(flat[ranges_to_ids(v[sel] + 0x42, 2 * chars)])

    is_data = typ == _AT_DATA
    lowest = np.zeros(len(p), dtype=np.uint64)
    nr = np.flatnonzero(is_data & nonres)
    lowest[nr] = b.u64(p[nr] + 0x10)
    sel = np.flatnonzero(is_data & (res | (lowest == 0)))
    size = np.where(res[sel], vlen[sel], 0).astype(np.int64)
    alloc = np.zeros(len(sel), dtype=np.int64)
    nrs = np.flatnonzero(nonres[sel])
    if nrs.size:
        q = p[sel][nrs]
        size[nrs] = b.i64(q + 0x30)
        packed = ((b.u16(q + 0x0C) & _COMPRESSED_OR_SPARSE) != 0) & (alen[sel][nrs] >= 0x48)
        alloc[nrs] = np.where(packed, b.i64(q + 0x40), b.i64(q + 0x28))
    parts["data_rec"].append(rec[sel])
    parts["data_off"].append(aoff[sel])
    parts["data_named"].append(nlen[sel] > 0)
    parts["data_size"].append(size)
    parts["data_alloc"].append(alloc)

    sel = np.flatnonzero(typ == _AT_REPARSE)
    tag = np.full(len(sel), -1, dtype=np.int64)  # -1: tag not in the MFT
    known = np.flatnonzero(res[sel] & (vlen[sel] >= 4))
    tag[known] = b.u32(v[sel][known])
    parts["rep_rec"].append(rec[sel])
    parts["rep_off"].append(aoff[sel])
    parts["rep_tag"].append(tag)


def _cat(parts: Mapping[str, list[np.ndarray]], key: str, dtype: Any) -> np.ndarray:
    chunks = parts.get(key, [])
    return np.concatenate(chunks).astype(dtype, copy=False) if chunks else np.empty(0, dtype=dtype)


def _first_per_owner(owner: np.ndarray, order: np.ndarray) -> np.ndarray:
    """Indices (into the unsorted rows) of the first row of each owner in ``order``."""
    so = owner[order]
    first = np.ones(len(so), dtype=bool)
    first[1:] = so[1:] != so[:-1]
    return order[first]


def _canonical(owner: np.ndarray, rec: np.ndarray, off: np.ndarray) -> np.ndarray:
    """Base record's attributes first, then extension records by number, each by offset."""
    return np.lexsort((off, rec, rec != owner, owner))


def _unix(ft: np.ndarray) -> np.ndarray:
    ft = ft.astype(np.uint64, copy=False)
    secs = (ft // np.uint64(10_000_000)).astype(np.int64) - _EPOCH_TICKS // 10_000_000
    return np.where(ft == 0, 0, secs)


def _attr_flags(attrs: np.ndarray) -> np.ndarray:
    out = np.zeros(len(attrs), dtype=np.uint32)
    for bit, flag in _ATTR_FLAGS:
        out[(attrs & bit) != 0] |= np.uint32(flag)
    return out


def _utf16_names(units16: np.ndarray, src_unit: np.ndarray, units: np.ndarray) -> tuple[bytes, np.ndarray]:
    """Decode names (UTF-16 code units) to one UTF-8 blob (surrogatepass) and per-name lengths."""
    if not len(units):
        return b"", np.empty(0, dtype=np.int64)
    stride = units + 1  # each name is followed by a NUL unit so surrogates never pair across names
    dest = np.cumsum(stride) - stride
    stream = np.zeros(int(stride.sum()), dtype=np.uint16)
    stream[ranges_to_ids(dest, units)] = units16[ranges_to_ids(src_unit, units)]
    text = stream.tobytes().decode("utf-16-le", "surrogatepass")
    enc = np.frombuffer(text.encode("utf-8", "surrogatepass"), dtype=np.uint8)
    hi = (stream >= 0xD800) & (stream < 0xDC00)
    lo = (stream >= 0xDC00) & (stream < 0xE000)
    pair = np.zeros(len(stream), dtype=bool)
    pair[:-1] = hi[:-1] & lo[1:]
    width = np.where(stream < 0x80, 1, np.where(stream < 0x800, 2, 3)).astype(np.int64)
    width[pair] = 4
    width[1:][pair[:-1]] = 0
    cs = np.concatenate(([0], np.cumsum(width)))
    assert int(cs[-1]) == len(enc)
    start = cs[dest]
    lens = cs[dest + units] - start
    return enc[ranges_to_ids(start, lens)].tobytes(), lens


def parse_mft_numpy(data: bytes | bytearray | memoryview, record_size: int, cluster_size: int) -> MftTable:
    """Pure numpy parser with the Rust parser's exact rules and output."""
    _check_sizes(record_size, cluster_size)
    buf = np.frombuffer(data, dtype=np.uint8)
    rs = record_size
    n = len(buf) // rs
    recs = buf[: n * rs].reshape(n, rs)
    sig = _header_col(recs, 0, "<u4")
    hflags = _header_col(recs, 0x16, "<u2")
    base_ref = _header_col(recs, 0x20, "<u8")
    seq = _header_col(recs, 0x10, "<u2")
    is_file = sig == _FILE
    live = is_file & ((hflags & 1) != 0)
    is_ext = live & (base_ref != 0)
    bad_sig = int(np.count_nonzero((sig != 0) & ~is_file))

    parts: defaultdict[str, list[np.ndarray]] = defaultdict(list)
    cand = np.flatnonzero(live)
    for lo in range(0, len(cand), _CHUNK):
        numbers = cand[lo : lo + _CHUNK]
        _parse_chunk(recs[numbers], numbers, rs, parts)
    good = np.zeros(n, dtype=bool)
    good[cand] = _cat(parts, "ok", bool)
    bad_records = bad_sig + int(np.count_nonzero(live & ~good))

    # owner record of every good in-use record (-1: dropped extension record)
    base_rec = (base_ref & np.uint64(REF_MASK)).astype(np.int64)
    owner = np.full(n, -1, dtype=np.int64)
    is_base = good & ~is_ext
    owner[is_base] = np.flatnonzero(is_base)
    ext = np.flatnonzero(good & is_ext)
    tgt = base_rec[ext]
    inside = tgt < n
    ext, tgt = ext[inside], tgt[inside]
    linked = is_base[tgt] & (seq[tgt] == (base_ref[ext] >> np.uint64(48)).astype(np.uint16))
    owner[ext[linked]] = tgt[linked]

    def owned(prefix: str) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        rec = _cat(parts, f"{prefix}_rec", np.int64)
        off = _cat(parts, f"{prefix}_off", np.int64)
        return rec, off, owner[rec] if len(rec) else np.empty(0, dtype=np.int64)

    # $STANDARD_INFORMATION
    crt = np.zeros(n, dtype=np.int64)
    mtm = np.zeros(n, dtype=np.int64)
    atm = np.zeros(n, dtype=np.int64)
    attrs = np.zeros(n, dtype=np.int64)
    rec, off, own = owned("si")
    sel = np.flatnonzero(own >= 0)
    first = _first_per_owner(own, sel[_canonical(own[sel], rec[sel], off[sel])])
    o = own[first]
    crt[o] = _unix(_cat(parts, "si_cr", np.uint64)[first])
    mtm[o] = _unix(_cat(parts, "si_m", np.uint64)[first])
    atm[o] = _unix(_cat(parts, "si_a", np.uint64)[first])
    attrs[o] = _cat(parts, "si_attrs", np.int64)[first]

    # $DATA
    size = np.zeros(n, dtype=np.int64)
    alloc = np.zeros(n, dtype=np.int64)
    ads_size = np.zeros(n, dtype=np.int64)
    ads_alloc = np.zeros(n, dtype=np.int64)
    has_ads = np.zeros(n, dtype=bool)
    rec, off, own = owned("data")
    named = _cat(parts, "data_named", bool)
    dsize = _cat(parts, "data_size", np.int64)
    dalloc = _cat(parts, "data_alloc", np.int64)
    sel = np.flatnonzero((own >= 0) & ~named)
    first = _first_per_owner(own, sel[_canonical(own[sel], rec[sel], off[sel])])
    size[own[first]] = dsize[first]
    alloc[own[first]] = dalloc[first]
    sel = np.flatnonzero((own >= 0) & named)
    np.add.at(ads_size, own[sel], dsize[sel])
    np.add.at(ads_alloc, own[sel], dalloc[sel])
    has_ads[own[sel]] = True

    # $REPARSE_POINT: 0 none, 1 name surrogate (or tag unknown), 2 other tag
    reparse = np.zeros(n, dtype=np.uint8)
    rec, off, own = owned("rep")
    tags = _cat(parts, "rep_tag", np.int64)
    sel = np.flatnonzero(own >= 0)
    first = _first_per_owner(own, sel[_canonical(own[sel], rec[sel], off[sel])])
    tag = tags[first]
    reparse[own[first]] = np.where((tag < 0) | ((tag & _NAME_SURROGATE) != 0), 1, 2)

    is_dir = (hflags & 2) != 0
    kind = np.where(reparse == 1, Kind.LINK, np.where(is_dir, Kind.DIR, Kind.FILE)).astype(np.uint8)
    flags = _attr_flags(attrs)
    flags[reparse != 0] |= np.uint32(Flag.REPARSE)
    flags[has_ads] |= np.uint32(Flag.ADS)
    size[is_dir] = 0
    alloc[is_dir] = 0

    # $FILE_NAME rows
    rec, off, own = owned("fn")
    parent = _cat(parts, "fn_parent", np.uint64)
    ns = _cat(parts, "fn_ns", np.int64)
    units = _cat(parts, "fn_units", np.int64)
    utf16 = _cat(parts, "fn_utf16", np.uint8)
    src_unit = np.cumsum(units) - units
    sel = np.flatnonzero(own >= 0)
    rows = sel[_canonical(own[sel], rec[sel], off[sel])]
    o = own[rows]
    p48 = (parent[rows] & np.uint64(REF_MASK)).astype(np.int64)
    dos = ns[rows] == _NS_DOS
    by_dir = np.lexsort((p48, o))
    go, gp = o[by_dir], p48[by_dir]
    starts = np.ones(len(go), dtype=bool)
    starts[1:] = (go[1:] != go[:-1]) | (gp[1:] != gp[:-1])
    group = np.cumsum(starts) - 1
    long_name = np.zeros(int(starts.sum()), dtype=bool)
    np.logical_or.at(long_name, group, ~dos[by_dir])
    drop = np.zeros(len(rows), dtype=bool)
    drop[by_dir] = dos[by_dir] & long_name[group]
    rows, o, p48 = rows[~drop], o[~drop], p48[~drop]
    nlink = np.minimum(np.bincount(o, minlength=n), 65535).astype(np.uint16)
    names, name_len = _utf16_names(utf16.view("<u2"), src_unit[rows], units[rows])
    name_off = np.cumsum(name_len) - name_len
    return {
        "record": o.astype(np.uint64),
        "parent": p48.astype(np.uint64),
        "name_off": name_off.astype(np.uint32),
        "name_len": name_len.astype(np.uint16),
        "kind": kind[o],
        "flags": flags[o],
        "size": size[o],
        "alloc": alloc[o],
        "ads_size": ads_size[o],
        "ads_alloc": ads_alloc[o],
        "mtime": mtm[o],
        "ctime": crt[o],
        "atime": atm[o],
        "nlink": nlink[o],
        "names": names,
        "bad_records": bad_records,
    }


# --------------------------------------------------------------------------- reading a volume

Run = tuple[int, int | None, int]  # (vcn, lcn or None if sparse, clusters)
_PIECE = 16 << 20  # bytes per read request
_MAX_CLUSTER = 2 << 20  # NTFS allows up to 2 MiB clusters
_MAX_LIST = 64 << 20  # an $ATTRIBUTE_LIST larger than this is corrupt


class _ReadAt(Protocol):
    def readinto_at(self, buf: memoryview, offset: int) -> None: ...


class _FileReader:
    """Positioned reads from an image file."""

    def __init__(self, fh: io.BufferedReader) -> None:
        self._fh = fh

    def readinto_at(self, buf: memoryview, offset: int) -> None:
        self._fh.seek(offset)
        if self._fh.readinto(buf) != len(buf):
            raise OSError(f"short read at {offset}")


@dataclass(frozen=True, slots=True)
class _Geometry:
    sector: int
    cluster: int
    record: int
    mft_lcn: int
    volume_size: int  # bytes, including the backup boot sector


class _Extent(NamedTuple):
    lowest_vcn: int
    sizes: tuple[int, int, int] | None  # (allocated, data, initialised) on the first extent
    runs: list[Run]


def _le(b: bytes, start: int, width: int) -> int:
    return int.from_bytes(b[start : start + width], "little")


def _fixup_one(raw: bytes) -> bytes | None:
    """One record with its update sequence undone, or None if it is torn."""
    rs = len(raw)
    usa_ofs, usa_cnt = _le(raw, 4, 2), _le(raw, 6, 2)
    if usa_cnt != rs // 512 + 1 or usa_ofs + 2 * usa_cnt > 510:
        return None
    out = bytearray(raw)
    usn = raw[usa_ofs : usa_ofs + 2]
    for i in range(rs // 512):
        p = i * 512 + 510
        if out[p : p + 2] != usn:
            return None
        out[p : p + 2] = raw[usa_ofs + 2 + 2 * i : usa_ofs + 4 + 2 * i]
    return bytes(out)


def decode_runs(mp: bytes, vcn: int = 0) -> list[Run] | None:
    """NTFS mapping pairs -> ``[(vcn, lcn or None if sparse, clusters)]`` (None if malformed)."""
    runs: list[Run] = []
    lcn = 0
    p = 0
    while p < len(mp) and mp[p] != 0:
        ls, os_ = mp[p] & 0x0F, mp[p] >> 4
        p += 1
        if ls == 0 or ls > 8 or os_ > 8 or p + ls + os_ > len(mp):
            return None
        length = _le(mp, p, ls)
        p += ls
        at: int | None = None
        if os_:
            lcn += int.from_bytes(mp[p : p + os_], "little", signed=True)
            p += os_
            if lcn < 0:
                return None
            at = lcn
        runs.append((vcn, at, length))
        vcn += length
    return runs


def _boot_geometry(boot: bytes) -> _Geometry | None:
    if len(boot) < 512 or boot[3:11] != b"NTFS    ":
        return None
    sector = _le(boot, 0x0B, 2)
    spc = boot[0x0D]
    # more than 128 sectors per cluster is stored as a negative power of two
    cluster = sector * ((1 << (256 - spc)) if spc > 0x80 else spc)
    raw = int.from_bytes(boot[0x40:0x41], "little", signed=True)
    record = (1 << -raw) if raw < 0 else raw * cluster
    volume_size = (_le(boot, 0x28, 8) + 1) * sector
    ok = (
        sector >= 512
        and sector & (sector - 1) == 0
        and sector <= cluster <= _MAX_CLUSTER
        and cluster & (cluster - 1) == 0
        and 512 <= record <= 65536
        and record % 512 == 0
        and volume_size < 1 << 64
    )
    return _Geometry(sector, cluster, record, _le(boot, 0x30, 8), volume_size) if ok else None


def _check_runs(runs: list[Run], geo: _Geometry) -> None:
    """Runs must stay inside the volume (corrupt mapping pairs could point anywhere)."""
    for vcn, lcn, count in runs:
        if (vcn + count) * geo.cluster >= 1 << 64 or (
            lcn is not None and (lcn + count) * geo.cluster > geo.volume_size
        ):
            raise OSError("$MFT: data runs lie outside the volume")


def _read_span(dev: _ReadAt, geo: _Geometry, pos: int, length: int) -> bytes:
    """``length`` bytes at any volume position, through whole-sector reads."""
    start = pos // geo.sector * geo.sector
    total = -(-(pos - start + length) // geo.sector) * geo.sector
    buf = bytearray(total)
    dev.readinto_at(memoryview(buf), start)
    return bytes(buf[pos - start : pos - start + length])


def _read_stream(dev: _ReadAt, geo: _Geometry, runs: list[Run], pos: int, length: int) -> bytes:
    """``length`` bytes at offset ``pos`` of a stream described by ``runs``."""
    out = bytearray()
    cl = geo.cluster
    while len(out) < length:
        vcn = pos // cl
        run = next((r for r in runs if r[0] <= vcn < r[0] + r[2]), None)
        if run is None:
            raise OSError("$MFT: stream offset not covered by the known runs")
        take = min((run[0] + run[2]) * cl - pos, length - len(out))
        if run[1] is None:
            out += bytes(take)
        else:
            out += _read_span(dev, geo, (run[1] + vcn - run[0]) * cl + pos % cl, take)
        pos += take
    return bytes(out)


def _record_extents(rec: bytes) -> tuple[list[_Extent], bytes | tuple[int, list[Run]] | None]:
    """Unnamed non-resident $DATA extents of one record, and its $ATTRIBUTE_LIST
    (the value, or (size, runs) when non-resident)."""
    used = min(_le(rec, 0x18, 4), len(rec))
    off = _le(rec, 0x14, 2)
    extents: list[_Extent] = []
    alist: bytes | tuple[int, list[Run]] | None = None
    while off + 16 <= used:
        typ, alen = _le(rec, off, 4), _le(rec, off + 4, 4)
        if typ == _AT_END:
            break
        if alen < 0x18 or off + alen > used:
            raise OSError("$MFT: malformed attribute")
        a = rec[off : off + alen]
        nonres = a[8] != 0
        if nonres and alen >= 0x40 and (typ == _AT_LIST or (typ == _AT_DATA and a[9] == 0)):
            lowest = _le(a, 0x10, 8) if typ == _AT_DATA else 0
            runs = decode_runs(a[_le(a, 0x20, 2) :], lowest)
            if runs is None:
                raise OSError("$MFT: bad mapping pairs")
            if typ == _AT_LIST:
                alist = (_le(a, 0x30, 8), runs)
            else:
                sizes = (_le(a, 0x28, 8), _le(a, 0x30, 8), _le(a, 0x38, 8)) if lowest == 0 else None
                extents.append(_Extent(lowest, sizes, runs))
        elif typ == _AT_LIST and not nonres:
            vlen, voff = _le(a, 0x10, 4), _le(a, 0x14, 2)
            alist = bytes(a[voff : voff + vlen]) if voff + vlen <= alen else None
        off += alen
    return extents, alist


def _layout(dev: _ReadAt) -> tuple[_Geometry, dict[str, int], list[Run]]:
    """Geometry (boot sector), sizes and complete run list of the $MFT behind ``dev``."""
    boot = bytearray(4096)
    dev.readinto_at(memoryview(boot), 0)
    geo = _boot_geometry(bytes(boot))
    if geo is None:
        raise OSError("not an NTFS volume")
    if geo.mft_lcn * geo.cluster + geo.record > geo.volume_size:
        raise OSError("$MFT: record 0 lies outside the volume")
    raw = _read_span(dev, geo, geo.mft_lcn * geo.cluster, geo.record)
    rec0 = _fixup_one(raw) if raw[:4] == b"FILE" else None
    if rec0 is None:
        raise OSError("$MFT: record 0 is unreadable")
    extents, alist = _record_extents(rec0)
    first = next((e for e in extents if e.lowest_vcn == 0), None)
    if first is None or first.sizes is None:
        raise OSError("$MFT: no $DATA in record 0")
    alloc, data_size, valid_size = first.sizes
    if alloc > geo.volume_size:
        raise OSError("$MFT: inconsistent sizes")
    clusters = -(-alloc // geo.cluster)
    runs = [r for e in extents for r in e.runs]
    _check_runs(runs, geo)
    if sum(r[2] for r in runs) < clusters:
        # fragmented enough for an attribute list: the other extents live in records that the
        # runs found so far already cover
        if alist is None:
            raise OSError("$MFT: runs incomplete and no attribute list")
        if isinstance(alist, bytes):
            entries = alist
        else:
            _check_runs(alist[1], geo)
            if alist[0] > _MAX_LIST:
                raise OSError("$MFT: attribute list too large")
            entries = _read_stream(dev, geo, alist[1], 0, alist[0])
        p = 0
        seen: set[int] = set()
        while p + 0x1A <= len(entries):
            typ, length = _le(entries, p, 4), _le(entries, p + 4, 2)
            if length < 0x1A:
                break
            holder = _le(entries, p + 0x10, 8) & REF_MASK
            if typ == _AT_DATA and entries[p + 6] == 0 and holder != 0 and holder not in seen:
                seen.add(holder)
                rec = _fixup_one(_read_stream(dev, geo, runs, holder * geo.record, geo.record))
                if rec is None:
                    raise OSError(f"$MFT: record {holder} fails its update sequence check")
                more = [r for e in _record_extents(rec)[0] for r in e.runs]
                _check_runs(more, geo)
                runs += more
            p += length
    runs = sorted(set(runs))
    nxt = 0
    for vcn, _, count in runs:
        if vcn != nxt:
            raise OSError("$MFT: data runs have a gap or overlap")
        nxt = vcn + count
    if nxt < clusters:
        raise OSError("$MFT: data runs do not cover the allocation")
    if data_size > alloc or valid_size > data_size or data_size % geo.record:
        raise OSError("$MFT: inconsistent sizes")
    info = {
        "bytes_per_sector": geo.sector,
        "cluster_size": geo.cluster,
        "record_size": geo.record,
        "mft_lcn": geo.mft_lcn,
        "mft_size": data_size,
        "mft_valid_size": valid_size,
        "mft_fragments": len(runs),
        "volume_size": geo.volume_size,
    }
    return geo, info, runs


def _read_all(dev: _ReadAt, geo: _Geometry, info: Mapping[str, int], runs: list[Run]) -> bytearray:
    """The $MFT $DATA; zeros after the valid data length."""
    size = info["mft_size"]
    out = bytearray(size)
    view = memoryview(out)
    cl = geo.cluster
    valid = min(-(-info["mft_valid_size"] // cl) * cl, size)
    pos = 0
    for vcn, lcn, count in runs:
        end = min((vcn + count) * cl, valid)
        while pos < end:
            take = min(_PIECE, end - pos)
            if lcn is not None:
                at = (lcn + pos // cl - vcn) * cl
                if take % geo.sector == 0:
                    dev.readinto_at(view[pos : pos + take], at)
                else:
                    view[pos : pos + take] = _read_span(dev, geo, at, take)
            pos += take
    view[info["mft_valid_size"] :] = bytes(size - info["mft_valid_size"])
    return out


def read_mft_image(path: str | Path) -> tuple[bytearray, dict[str, int]]:
    """$MFT and geometry of a volume image file (the pure-Python reader; tests)."""
    with open(path, "rb") as fh:
        dev = _FileReader(fh)
        geo, info, runs = _layout(dev)
        return _read_all(dev, geo, info, runs), info


def _device_path(volume: str) -> str:
    v = volume.rstrip("\\/")
    if not (len(v) == 1 or (len(v) == 2 and v[1] == ":")) or not (v[0].isascii() and v[0].isalpha()):
        raise ValueError(f"not a local volume like 'C:': {volume!r}")
    return f"\\\\.\\{v[0].upper()}:"


class _VolumeReader:
    """Read-only ctypes handle on ``\\\\.\\X:`` (Windows; needs administrator rights)."""

    def __init__(self, path: str) -> None:
        import ctypes
        from ctypes import wintypes

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
        create = k32.CreateFileW
        create.argtypes = [
            wintypes.LPCWSTR,
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.LPVOID,
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.HANDLE,
        ]
        create.restype = wintypes.HANDLE
        read = k32.ReadFile
        read.argtypes = [wintypes.HANDLE, wintypes.LPVOID, wintypes.DWORD, wintypes.LPDWORD, wintypes.LPVOID]
        read.restype = wintypes.BOOL

        class Overlapped(ctypes.Structure):
            _fields_ = (
                ("Internal", ctypes.c_size_t),
                ("InternalHigh", ctypes.c_size_t),
                ("Offset", wintypes.DWORD),
                ("OffsetHigh", wintypes.DWORD),
                ("hEvent", wintypes.HANDLE),
            )

        generic_read, share_all, open_existing = 0x80000000, 0x7, 3
        handle = create(path, generic_read, share_all, None, open_existing, 0, None)
        if handle is None or handle == ctypes.c_void_p(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined,unused-ignore]
        self._ct, self._wt, self._k32, self._read, self._ov = ctypes, wintypes, k32, read, Overlapped
        self._h = handle

    def readinto_at(self, buf: memoryview, offset: int) -> None:
        ct = self._ct
        target = (ct.c_char * len(buf)).from_buffer(buf)
        ov = self._ov(0, 0, offset & 0xFFFFFFFF, offset >> 32, None)
        got = self._wt.DWORD(0)
        if not self._read(self._h, ct.addressof(target), len(buf), ct.byref(got), ct.byref(ov)):
            raise ct.WinError(ct.get_last_error())  # type: ignore[attr-defined,unused-ignore]
        if got.value != len(buf):
            raise OSError(f"short read at {offset}")

    def close(self) -> None:
        self._k32.CloseHandle(self._h)


def read_raw_mft(volume: str) -> tuple[bytes | bytearray, dict[str, int]]:
    """The whole $MFT of a local NTFS volume and its geometry (``mft_volume_info`` keys).

    Needs administrator rights (PermissionError otherwise); NotImplementedError off Windows.
    """
    mod = native.module()
    if mod is not None and native.has("read_mft") and native.has("mft_volume_info"):
        info = dict(mod.mft_volume_info(volume))
        data: bytes = mod.read_mft(volume)
        info["mft_size"] = len(data)  # the $MFT may have grown in between
        return data, info
    if sys.platform != "win32":
        raise NotImplementedError("MFT access is only available on Windows")
    dev = _VolumeReader(_device_path(volume))
    try:
        geo, info, runs = _layout(dev)
        return _read_all(dev, geo, info, runs), info
    finally:
        dev.close()


MftProgress = Callable[[int, int], object]  # (bytes of $MFT parsed, bytes to parse)


def scan_volume(
    volume: str, duty: float = 1.0, progress: MftProgress | None = None
) -> tuple[MftTable, dict[str, int]]:
    """Read and parse the $MFT of a local NTFS volume: ``(table, geometry)``.

    With the native module this streams (32 MiB windows, read and parse overlapping): memory is
    the table plus a few windows. Without it the whole $MFT is read first (gigabytes on a large
    system volume). ``duty`` in (0, 1]: the reader is busy that share of the time (I/O limit;
    honoured by the native reader). ``progress(done, total)`` follows the reading (per window
    natively, once at the end otherwise). Needs administrator rights; NotImplementedError off Windows.
    """
    mod = native.module()
    if mod is not None and native.has("scan_mft"):
        table, info = mod.scan_mft(volume, duty, progress)
        return dict(table), dict(info)
    data, info = read_raw_mft(volume)
    if progress is not None:
        progress(len(data), len(data))
    return parse_mft_table(data, info["record_size"], info["cluster_size"]), info


def scan_volume_image(
    path: str | Path, duty: float = 1.0, progress: MftProgress | None = None, window: int = 0
) -> tuple[MftTable, dict[str, int]]:
    """:func:`scan_volume` on an NTFS volume image file (tests; ``window``: bytes per read window)."""
    mod = native.module()
    if mod is not None and native.has("_scan_mft_image"):
        table, info = mod._scan_mft_image(str(path), duty, window, progress)
        return dict(table), dict(info)
    data, info = read_mft_image(path)
    if progress is not None:
        progress(len(data), len(data))
    return parse_mft_table(data, info["record_size"], info["cluster_size"]), info


# --------------------------------------------------------------------------- MftTable -> NodeStore


def _stable_argsort(keys: np.ndarray, top: int) -> np.ndarray:
    """``argsort(keys, kind="stable")`` for ints in ``[0, top]`` through one uint64 sort (fast)."""
    n = len(keys)
    bits = max(1, (n - 1).bit_length())
    if top.bit_length() + bits > 63:
        return np.argsort(keys, kind="stable")
    packed = keys.astype(np.uint64)
    packed <<= np.uint64(bits)
    packed |= np.arange(n, dtype=np.uint64)
    packed.sort()
    packed &= np.uint64((1 << bits) - 1)
    return packed.view(np.int64)


def _load_u64(padded: np.ndarray, pos: np.ndarray) -> np.ndarray:
    """Little-endian uint64 at arbitrary byte positions (``padded`` has 16 spare bytes at the end)."""
    words = padded[: len(padded) // 8 * 8].view("<u8")
    shift = ((pos & 7) * 8).astype(np.uint64)
    lo = words[pos >> 3] >> shift
    hi = words[(pos >> 3) + 1] << ((np.uint64(64) - shift) & np.uint64(63))
    return np.asarray(np.where(shift == 0, lo, lo | hi))


class _ExtPart(NamedTuple):
    short: np.ndarray  # indices of names with a 1–7 byte extension
    keys: np.ndarray  # their uint64 keys: the bytes after the dot, the length in the top byte
    uniq: np.ndarray  # distinct keys of this part
    long: np.ndarray  # indices of names with an 8–64 byte extension
    long_dot: np.ndarray
    long_len: np.ndarray


def _ext_ids(
    blob: bytes,
    off: np.ndarray,
    length: np.ndarray,
    ext_table: list[str],
    pool: ThreadPoolExecutor | None = None,
    workers: int = 1,
) -> np.ndarray:
    """Extension ids with scan.worker's rule: the text after the last dot, lower-cased, when the
    dot is not the first character and at most 16 characters follow it. Appends to ``ext_table``.
    Fastest with names in blob order; splits the work over ``pool``."""
    n = len(off)
    out = np.zeros(n, dtype=np.uint16)
    if not n:
        return out
    raw = np.zeros(len(blob) + 16, dtype=np.uint8)
    raw[: len(blob)] = np.frombuffer(blob, dtype=np.uint8)
    last = len(raw) - 1
    dots = np.flatnonzero(raw[: len(blob)] == 0x2E)

    def scan(lo: int, hi: int) -> _ExtPart:
        o = off[lo:hi]
        end = o + length[lo:hi]
        j = np.searchsorted(dots, end) - 1
        dot = dots[np.maximum(j, 0)] if dots.size else np.zeros(hi - lo, dtype=np.int64)
        elen = end - dot - 1
        ok = (j >= 0) & (dot > o) & (elen > 0) & (elen <= 64)
        mid = np.flatnonzero(ok & (elen > 16))  # 17–64 bytes: count the characters
        if mid.size:
            lead = (raw[np.minimum((dot[mid] + 1)[:, None] + np.arange(64), last)] & 0xC0) != 0x80
            ok[mid] = np.count_nonzero(lead & (np.arange(64) < elen[mid][:, None]), axis=1) <= 16
        short = np.flatnonzero(ok & (elen <= 7))
        el = elen[short].astype(np.uint64)
        mask = (np.uint64(1) << (el * np.uint64(8))) - np.uint64(1)
        keys = ((_load_u64(raw, dot[short]) >> np.uint64(8)) & mask) | (el << np.uint64(56))
        long = np.flatnonzero(ok & (elen > 7))
        return _ExtPart(short + lo, keys, np.unique(keys), long + lo, dot[long], elen[long])

    bounds = np.linspace(0, n, max(1, min(workers, n // 100_000)) + 1).astype(np.int64).tolist()
    spans = list(itertools.pairwise(bounds))
    parts = list(pool.map(lambda s: scan(*s), spans)) if pool is not None else [scan(*s) for s in spans]

    index = {e: i for i, e in enumerate(ext_table)}

    def ext_id(text: bytes) -> int:
        ext = text.decode("utf-8", "surrogatepass").lower()
        idx = index.get(ext)
        if idx is None:
            if len(ext_table) >= 65535:
                return 0
            idx = index[ext] = len(ext_table)
            ext_table.append(ext)
        return idx

    uniq = np.unique(np.concatenate([p.uniq for p in parts]))
    ids = np.array(
        [ext_id(int(k).to_bytes(8, "little")[: int(k) >> 56]) for k in uniq.tolist()], dtype=np.uint16
    )

    def fill(p: _ExtPart) -> None:
        out[p.short] = ids[np.searchsorted(uniq, p.keys)]

    if pool is not None:
        list(pool.map(fill, parts))
    else:
        for p in parts:
            fill(p)
    sel = np.concatenate([p.long for p in parts])
    if sel.size:  # longer ones: length byte + bytes as fixed-width strings
        dot = np.concatenate([p.long_dot for p in parts])
        el = np.concatenate([p.long_len for p in parts])
        width = int(el.max())
        mat = np.zeros((len(sel), width + 1), dtype=np.uint8)
        mat[:, 0] = el
        mat[:, 1:] = np.where(
            np.arange(width) < el[:, None], raw[np.minimum((dot + 1)[:, None] + np.arange(width), last)], 0
        )
        uniq_s, inv = np.unique(mat.view(f"S{width + 1}").reshape(-1), return_inverse=True)
        padded = [bytes(k).ljust(width + 1, b"\0") for k in uniq_s.tolist()]
        out[sel] = np.array([ext_id(p[1 : 1 + p[0]]) for p in padded], dtype=np.uint16)[inv.reshape(-1)]
    return out


def _csr(pdir: np.ndarray, total: int) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Children of every row, in row order: (rows grouped by parent, group start, group size)."""
    keys = np.where(pdir >= 0, pdir, total)  # rows without a parent sort last and are cut off
    counts = np.bincount(keys, minlength=total + 1)[:total].astype(np.int64)
    order = _stable_argsort(keys, total)[: int(counts.sum())]
    return order, np.cumsum(counts) - counts, counts


def _bfs(
    order: np.ndarray, starts: np.ndarray, counts: np.ndarray, container: np.ndarray, root: int
) -> tuple[list[np.ndarray], np.ndarray]:
    """Rows level by level from ``root`` and each folder's first child id (ids count from 1)."""
    levels = [np.array([root], dtype=np.int64)]
    child_start = np.zeros(len(counts), dtype=np.int64)
    next_id = 2
    frontier = levels[0]
    while frontier.size:
        cnt = counts[frontier]
        child_start[frontier] = next_id + np.cumsum(cnt) - cnt
        kids = order[ranges_to_ids(starts[frontier], cnt)]
        if not kids.size:
            break
        next_id += len(kids)
        levels.append(kids)
        frontier = kids[container[kids]]
    return levels, child_start


def _cycle_breakers(pdir: np.ndarray, stuck: np.ndarray) -> np.ndarray:
    """One row per parent cycle among ``stuck`` (rows not reachable from the root)."""
    total = len(pdir)
    alive = np.zeros(total, dtype=bool)
    alive[stuck] = True
    indeg = np.bincount(pdir[stuck], minlength=total)
    leaves = stuck[indeg[stuck] == 0]
    while leaves.size:  # peel the trees hanging off the cycles
        alive[leaves] = False
        up = pdir[leaves]
        np.subtract.at(indeg, up, 1)
        up = np.unique(up)
        leaves = up[alive[up] & (indeg[up] == 0)]
    cyc = np.flatnonzero(alive)
    label = np.arange(total, dtype=np.int64)
    for _ in range(4096):  # smallest row of each cycle
        nxt = np.minimum(label[cyc], label[pdir[cyc]])
        if np.array_equal(nxt, label[cyc]):
            break
        label[cyc] = nxt
    else:
        return cyc
    return np.asarray(cyc[label[cyc] == cyc])


def table_to_store(
    table: Mapping[str, Any],
    root_path: str,
    volume_info: Mapping[str, int] | None = None,
    *,
    include_ads: bool = False,
    count_links_each: bool = False,
    consume: bool = False,
) -> NodeStore:
    """Build the node store of a whole volume from ``MftTable`` columns in one go.

    Record 5 becomes the root named ``root_path``; metafiles (records 0–15 and the $Extend tree)
    go under ``$NTFS 元数据``; names whose parent is missing, not a folder or part of a parent
    cycle go under ``$孤立文件`` (each pseudo folder only when non-empty). Sizes follow the hard
    scan: folders and links occupy nothing, the second and later names of a file are
    ``HARDLINK_DUP``; alternate data streams count only with ``include_ads``, except in the
    metafile folder, where they always count toward allocation ($UsnJrnl:$J, $Secure:$SDS …).
    ``$BadClus:$Bad`` never counts: it is as long as the volume and maps only the bad clusters,
    but carries no sparse flag, so its allocated size claims the whole volume.
    ``volume_info`` (``mft_volume_info``) is accepted for symmetry; nothing in it is needed.
    ``consume``: the caller hands the table over — its columns are dropped as soon as they are
    copied into node order (a C:-sized table is about a gigabyte: the build peak shrinks by it).
    """
    del volume_info
    rec = np.asarray(table["record"]).astype(np.int64)
    par = np.asarray(table["parent"]).astype(np.int64)
    kind = np.asarray(table["kind"]).astype(np.uint8, copy=False)
    blob: bytes = table["names"]
    n = len(rec)
    if n and bool(np.all(rec[1:] >= rec[:-1])):
        dup = np.zeros(n, dtype=bool)
        dup[1:] = rec[1:] == rec[:-1]
    else:
        dup = np.ones(n, dtype=bool)
        dup[np.unique(rec, return_index=True)[1]] = False
    meta_row, orphan_row, root_row = n, n + 1, n + 2  # pseudo rows after the table's
    total = n + 3

    nrec = max(int(rec.max()) + 1 if n else 0, ROOT_RECORD + 1)
    container = np.ones(total, dtype=bool)
    container[:n] = (kind == Kind.DIR) & ~dup
    dir_row = np.full(nrec, -1, dtype=np.int64)
    first_dirs = np.flatnonzero(container[:n])
    dir_row[rec[first_dirs]] = first_dirs
    dir_row[ROOT_RECORD] = root_row

    pdir = np.full(total, -1, dtype=np.int64)
    pd = pdir[:n]
    known = (par >= 0) & (par < nrec)
    pd[known] = dir_row[par[known]]
    pd[pd < 0] = orphan_row
    loops = first_dirs[pd[first_dirs] == first_dirs]  # a folder that is its own parent
    pd[loops] = orphan_row
    pd[(rec < FIRST_USER_RECORD) & (rec != ROOT_RECORD)] = meta_row
    pd[rec == ROOT_RECORD] = -2  # the root's own "." entry is the root node itself
    for row in (meta_row, orphan_row):  # pseudo folders only when they hold something
        pdir[row] = root_row if bool(np.any(pd == row)) else -1

    # breadth-first ids: every folder's children get one contiguous id range
    order, starts, counts = _csr(pdir, total)
    levels, child_start = _bfs(order, starts, counts, container, root_row)
    placed = sum(len(lv) for lv in levels)
    if placed < 1 + len(order):  # parent cycles: break each one under the orphans
        seen = np.zeros(total, dtype=bool)
        seen[np.concatenate(levels)] = True
        breakers = _cycle_breakers(pdir, np.flatnonzero(~seen & (pdir >= 0)))
        logger.warning("MFT: {} parent cycles broken (corrupt directory entries)", len(breakers))
        pdir[breakers] = orphan_row
        pdir[orphan_row] = root_row
        order, starts, counts = _csr(pdir, total)
        levels, child_start = _bfs(order, starts, counts, container, root_row)
    rows = np.concatenate(levels)
    count = len(rows) + 1
    vpos = np.flatnonzero(rows >= n) + 1  # node ids of the root and the pseudo folders
    virt = rows[vpos - 1]
    src = np.minimum(rows, max(n - 1, 0))  # pseudo nodes read row 0 for now, patched below
    node_of_row = np.zeros(total, dtype=np.int64)
    node_of_row[rows] = np.arange(1, count)
    parent_node = node_of_row[np.maximum(pdir, 0)]
    parent_node[pdir < 0] = 0

    def gather(values: Any, index: np.ndarray) -> np.ndarray:
        arr = np.asarray(values)
        return np.take(arr, index) if len(arr) else np.zeros(len(index), dtype=arr.dtype)

    # node-order copies of the columns: independent gathers that release the GIL, in parallel
    by_row: dict[str, Any] = {
        key: table[key]
        for key in (
            "kind",
            "flags",
            "size",
            "alloc",
            "mtime",
            "ctime",
            "atime",
            "nlink",
            "name_off",
            "name_len",
        )
    }
    by_row |= {"ino": rec, "dup": dup}
    if include_ads:
        by_row |= {"ads_size": table["ads_size"], "ads_alloc": table["ads_alloc"]}
    by_tree = {"parent": parent_node, "container": container, "child_start": child_start, "counts": counts}
    ext_table = [""]
    files = np.flatnonzero(kind == Kind.FILE)
    workers = min(8, os.cpu_count() or 1)
    with ThreadPoolExecutor(max_workers=workers, thread_name_prefix="mft-store") as pool:
        jobs = {key: pool.submit(gather, values, src) for key, values in by_row.items()}
        jobs |= {key: pool.submit(gather, values, rows) for key, values in by_tree.items()}
        noff = np.asarray(table["name_off"]).astype(np.int64)[files]
        nlen = np.asarray(table["name_len"]).astype(np.int64)[files]
        ext_rows = np.zeros(n, dtype=np.uint16)
        ext_rows[files] = _ext_ids(blob, noff, nlen, ext_table, pool, workers)
        got = {key: job.result() for key, job in jobs.items()}
        got["ext"] = gather(ext_rows, src)
    del ext_rows, noff, nlen, files, jobs
    bad_nodes = node_of_row[np.flatnonzero(rec == BADCLUS_RECORD)]
    if include_ads:
        got["ads_size"][bad_nodes - 1] = 0
        got["ads_alloc"][bad_nodes - 1] = 0
    root_rows = np.flatnonzero(rec == ROOT_RECORD)
    root_times = (
        {t: int(np.asarray(table[t])[root_rows[0]]) for t in ("mtime", "ctime", "atime")}
        if root_rows.size
        else {}
    )
    meta_ads = None
    if not include_ads and bool(np.any(virt == meta_row)):
        in_meta = np.zeros(total, dtype=bool)
        in_meta[meta_row] = True
        for lv in levels[2:]:
            in_meta[lv] = in_meta[pdir[lv]]
        meta_rows = np.flatnonzero(in_meta[:n])
        meta_ads = (node_of_row[meta_rows], np.asarray(table["ads_alloc"])[meta_rows])
        meta_ads[1][rec[meta_rows] == BADCLUS_RECORD] = 0
        del in_meta, meta_rows
    if consume:  # everything needed is in node order now: let the table go
        for key in [k for k in table if k != "names"]:
            del table[key]  # type: ignore[attr-defined]
    del by_row, by_tree, src, dup, par

    def node_col(key: str, dtype: Any, fill: int = 0) -> np.ndarray:
        out = np.empty(count, dtype=dtype)
        out[0] = fill
        out[1:] = got.pop(key)  # the node-order copy is not needed again: free it now
        out[vpos] = fill
        return out

    cols: dict[str, np.ndarray] = {}
    k = node_col("kind", np.uint8, Kind.DIR)
    cols["kind"] = k
    cols["parent"] = np.concatenate(([-1], got.pop("parent")))
    cols["depth"] = np.concatenate([[0], *(np.full(len(lv), i + 1) for i, lv in enumerate(levels))])
    flags = node_col("flags", np.uint32)
    if not count_links_each:  # the 「每个链接都计」 setting: every name of a file counts
        flags[node_col("dup", bool)] |= np.uint32(Flag.HARDLINK_DUP)
    flags[vpos] = np.where(virt == root_row, 0, np.uint32(Flag.VIRTUAL))
    flags[0] = Flag.VIRTUAL
    cols["flags"] = flags

    size = node_col("size", np.int64)
    size[k == Kind.DIR] = 0
    alloc = node_col("alloc", np.int64)
    alloc[k != Kind.FILE] = 0
    if include_ads:
        size += node_col("ads_size", np.int64)
        alloc += node_col("ads_alloc", np.int64)
    elif meta_ads is not None:
        alloc[meta_ads[0]] += meta_ads[1]
    cols["size"], cols["alloc"] = size, alloc

    for t in ("mtime", "ctime", "atime"):
        v = node_col(t, np.int64)
        if t in root_times:
            v[1] = root_times[t]
        cols[t] = np.clip(v, 0, _U32_MAX).astype(np.uint32)
        del v
    cols["ino"] = node_col("ino", np.uint64)
    cols["ino"][1] = ROOT_RECORD
    cols["nlink"] = node_col("nlink", np.uint16, 1)
    cols["ext"] = node_col("ext", np.uint16)

    # names: the table's blob, then the root and the pseudo folders
    extra = {
        root_row: encode_name(root_path),
        meta_row: META_FOLDER.encode(),
        orphan_row: ORPHAN_FOLDER.encode(),
    }
    name_off = node_col("name_off", np.int64)
    name_len = node_col("name_len", np.int64)
    tail = bytearray()
    for pos, row in zip(vpos.tolist(), virt.tolist(), strict=True):
        name_off[pos] = len(blob) + len(tail)
        name_len[pos] = len(extra[row])
        tail += extra[row]
    cols["name_off"], cols["name_len"] = name_off, name_len.astype(np.uint16)

    is_cont = got.pop("container")
    cols["child_start"] = np.concatenate(([1], np.where(is_cont, got.pop("child_start"), 0)))
    cols["child_count"] = np.concatenate(([1], np.where(is_cont, got.pop("counts"), 0)))
    orphans = int(counts[orphan_row]) if pdir[orphan_row] >= 0 else 0
    got.clear()
    del is_cont, rec, kind, container, dir_row, pdir, order, starts, counts, levels, child_start
    del rows, node_of_row, parent_node

    names = bytes(blob) + bytes(tail)
    if consume:
        del table["names"]  # type: ignore[attr-defined]
    del blob
    store = NodeStore.from_columns(cols, names, ext_table)
    del cols, names
    store.reaggregate()
    logger.debug("MFT: {:,} names -> {:,} nodes, {:,} orphans", n, len(store), orphans)
    return store


__all__ = [
    "META_FOLDER",
    "ORPHAN_FOLDER",
    "MftTable",
    "decode_runs",
    "parse_mft_numpy",
    "parse_mft_table",
    "read_mft_image",
    "read_raw_mft",
    "table_to_store",
]
