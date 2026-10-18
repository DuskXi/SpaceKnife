"""Synthetic NTFS $MFT images for tests (and tools/bench_mft.py).

:func:`encode_record` writes a valid FILE record — header, update sequence (fixups),
$STANDARD_INFORMATION, $ATTRIBUTE_LIST, $FILE_NAME in any namespace, resident / non-resident /
compressed / sparse $DATA, named streams, $REPARSE_POINT — or a deliberately broken one.
:func:`build_mft` lays records out at their numbers; :func:`build_volume` writes a small volume
image (boot sector + a fragmented $MFT, optionally continued through an extension record) for the
raw reader; :func:`tree_records` turns a nested dict into a directory tree on top of the NTFS
metafiles.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import numpy as np

WIN32, POSIX, DOS, WIN32_AND_DOS = 1, 0, 2, 3
ROOT = 5
EXTEND = 11
FIRST_USER = 64  # tree_records numbers user files from here
RESIDENT_MAX = 256  # default: streams up to this size are resident
DATA_LCN = 100_000  # where default non-resident runs point (never read)

AT_SI, AT_LIST, AT_FN, AT_DATA, AT_REPARSE, AT_END = 0x10, 0x20, 0x30, 0x80, 0xC0, 0xFFFFFFFF
FA_READONLY, FA_HIDDEN, FA_SYSTEM, FA_ARCHIVE = 0x1, 0x2, 0x4, 0x20
FA_SPARSE, FA_REPARSE, FA_COMPRESSED, FA_ENCRYPTED = 0x200, 0x400, 0x800, 0x4000
IO_REPARSE_TAG_SYMLINK = 0xA000000C
IO_REPARSE_TAG_MOUNT_POINT = 0xA0000003
IO_REPARSE_TAG_DEDUP = 0x80000013  # not a name surrogate: a regular file
EPOCH_DELTA = 11_644_473_600


def filetime(unix: int) -> int:
    return 0 if unix == 0 else (unix + EPOCH_DELTA) * 10_000_000 + 1234567


def _align8(v: int) -> int:
    return (v + 7) & ~7


@dataclass
class Stream:
    """One $DATA attribute (unnamed = the file's contents)."""

    size: int
    name: str = ""
    resident: bool | None = None  # default: size <= RESIDENT_MAX
    alloc: int | None = None  # non-resident allocated size (default: size rounded up to clusters)
    compressed: int | None = None  # compressed/sparse: actual allocation (the 0x48-byte header)
    sparse: bool = False
    lowest_vcn: int = 0  # > 0: a later extent of a fragmented attribute
    runs: list[tuple[int | None, int]] | None = None  # (lcn or None for sparse, clusters)
    init: int | None = None  # initialised (valid data) length, default: size


@dataclass
class Name:
    parent: int
    name: str
    ns: int = WIN32_AND_DOS
    parent_seq: int = 1


@dataclass
class Rec:
    number: int
    names: list[Name] = field(default_factory=list)
    dir: bool = False
    in_use: bool = True
    seq: int = 1
    attrs: int = FA_ARCHIVE
    times: tuple[int, int, int] = (1_600_000_000, 1_700_000_000, 1_700_000_500)  # created, modified, accessed
    streams: list[Stream] = field(default_factory=list)
    reparse_tag: int | None = None
    reparse_resident: bool = True
    base: int | None = None  # extension record of this base record
    base_seq: int = 1
    attr_list: list[tuple[int, int, int]] | None = None  # (type, holder record, lowest vcn)
    no_si: bool = False
    corrupt: str = ""  # "fixup" | "usa" | "length" | "signature" | "first"


def _resident(typ: int, value: bytes, name: str = "", flags: int = 0, indexed: int = 0) -> bytes:
    name_b = name.encode("utf-16-le", "surrogatepass")
    voff = _align8(0x18 + len(name_b))
    total = _align8(voff + len(value))
    hdr = struct.pack(
        "<IIBBHHHIHBB", typ, total, 0, len(name_b) // 2, 0x18, flags, 0, len(value), voff, indexed, 0
    )
    out = bytearray(total)
    out[: len(hdr)] = hdr
    out[0x18 : 0x18 + len(name_b)] = name_b
    out[voff : voff + len(value)] = value
    return bytes(out)


def _uint_bytes(v: int) -> bytes:
    k = max(1, (v.bit_length() + 7) // 8)
    return v.to_bytes(k, "little")


def _sint_bytes(v: int) -> bytes:
    k = 1
    while not -(1 << (8 * k - 1)) <= v < (1 << (8 * k - 1)):
        k += 1
    return v.to_bytes(k, "little", signed=True)


def encode_runs(runs: list[tuple[int | None, int]]) -> bytes:
    out = bytearray()
    prev = 0
    for lcn, count in runs:
        lb = _uint_bytes(count)
        if lcn is None:
            out.append(len(lb))
            out += lb
        else:
            ob = _sint_bytes(lcn - prev)
            out.append(len(ob) << 4 | len(lb))
            out += lb + ob
            prev = lcn
    out.append(0)
    return bytes(out)


def _nonresident(
    typ: int,
    name: str,
    runs: list[tuple[int | None, int]],
    *,
    lowest_vcn: int,
    alloc: int,
    size: int,
    init: int,
    compressed: int | None = None,
    flags: int = 0,
) -> bytes:
    name_b = name.encode("utf-16-le", "surrogatepass")
    hdr_len = 0x48 if compressed is not None else 0x40
    mp_off = _align8(hdr_len + len(name_b))
    mp = encode_runs(runs)
    total = _align8(mp_off + len(mp))
    clusters = sum(c for _, c in runs)
    hdr = struct.pack(
        "<IIBBHHHQQHHIqqq",
        typ,
        total,
        1,
        len(name_b) // 2,
        hdr_len,
        flags,
        0,
        lowest_vcn,
        lowest_vcn + clusters - 1,
        mp_off,
        4 if flags & 0x0001 else 0,
        0,
        alloc,
        size,
        init,
    )
    if compressed is not None:
        hdr += struct.pack("<q", compressed)
    out = bytearray(total)
    out[: len(hdr)] = hdr
    out[hdr_len : hdr_len + len(name_b)] = name_b
    out[mp_off : mp_off + len(mp)] = mp
    return bytes(out)


def _data_attr(st: Stream, cluster: int) -> bytes:
    resident = st.resident if st.resident is not None else st.size <= RESIDENT_MAX
    if resident:
        return _resident(AT_DATA, b"\x5a" * st.size, st.name)
    alloc = st.alloc if st.alloc is not None else -(-st.size // cluster) * cluster
    runs = st.runs if st.runs is not None else [(DATA_LCN, max(1, alloc // cluster))]
    flags = (0x8000 if st.sparse else 0) | (0x0001 if st.compressed is not None and not st.sparse else 0)
    return _nonresident(
        AT_DATA,
        st.name,
        runs,
        lowest_vcn=st.lowest_vcn,
        alloc=alloc if st.lowest_vcn == 0 else 0,
        size=st.size if st.lowest_vcn == 0 else 0,
        init=(st.size if st.init is None else st.init) if st.lowest_vcn == 0 else 0,
        compressed=st.compressed,
        flags=flags,
    )


def _si_value(rec: Rec) -> bytes:
    cr, mt, at = (filetime(t) for t in rec.times)
    return struct.pack("<QQQQIIIIIIQQ", cr, mt, mt, at, rec.attrs, 0, 0, 0, 0, 0x100, 0, 0)


def _fn_value(nm: Name, rec: Rec) -> bytes:
    name_b = nm.name.encode("utf-16-le", "surrogatepass")
    cr, mt, at = (filetime(t) for t in rec.times)
    fa = rec.attrs | (0x10000000 if rec.dir else 0)
    return (
        struct.pack(
            "<QQQQQqqIIBB",
            (nm.parent_seq << 48) | nm.parent,
            cr,
            mt,
            mt,
            at,
            0,
            0,
            fa,
            0,
            len(name_b) // 2,
            nm.ns,
        )
        + name_b
    )


def _list_value(entries: list[tuple[int, int, int]]) -> bytes:
    out = b""
    for typ, holder, vcn in entries:
        out += struct.pack("<IHBBQQH", typ, 0x20, 0, 0x1A, vcn, (1 << 48) | holder, 0).ljust(0x20, b"\0")
    return out


def encode_record(rec: Rec, record_size: int = 1024, cluster_size: int = 4096) -> bytes:
    """One FILE record with its update sequence applied (as it is on disk)."""
    usa_cnt = record_size // 512 + 1
    usa_ofs = 0x30
    first = _align8(usa_ofs + 2 * usa_cnt)
    attrs: list[bytes] = []
    if rec.base is None and not rec.no_si:
        attrs.append(_resident(AT_SI, _si_value(rec)))
    if rec.attr_list:
        attrs.append(_resident(AT_LIST, _list_value(rec.attr_list)))
    attrs += [_resident(AT_FN, _fn_value(nm, rec), indexed=1) for nm in rec.names]
    attrs += [_data_attr(st, cluster_size) for st in rec.streams]
    if rec.reparse_tag is not None:
        if rec.reparse_resident:
            attrs.append(_resident(AT_REPARSE, struct.pack("<IHH", rec.reparse_tag, 8, 0) + b"\0" * 8))
        else:
            attrs.append(
                _nonresident(AT_REPARSE, "", [(DATA_LCN, 1)], lowest_vcn=0, alloc=4096, size=16, init=16)
            )
    body = b"".join(attrs) + struct.pack("<II", AT_END, 0)
    used = first + len(body)
    if used > record_size:
        raise ValueError(f"record {rec.number} does not fit: {used} > {record_size}")
    flags = (1 if rec.in_use else 0) | (2 if rec.dir else 0)
    base_ref = 0 if rec.base is None else (rec.base_seq << 48) | rec.base
    links = sum(1 for nm in rec.names if nm.ns != DOS)
    header = struct.pack(
        "<4sHHQHHHHIIQHHI",
        b"FILE",
        usa_ofs,
        usa_cnt,
        0x1000 + rec.number,
        rec.seq,
        links,
        first,
        flags,
        used,
        record_size,
        base_ref,
        len(attrs) + 1,
        0,
        rec.number & 0xFFFFFFFF,
    )
    buf = bytearray(record_size)
    buf[: len(header)] = header
    buf[first:used] = body
    if rec.corrupt == "length":  # first attribute claims to run past the used size
        struct.pack_into("<I", buf, first + 4, record_size)
    if rec.corrupt == "first":
        struct.pack_into("<H", buf, 0x14, 0x10)
    usn = struct.pack("<H", (rec.number % 0xFFF0) + 1)
    buf[usa_ofs : usa_ofs + 2] = usn
    for i in range(record_size // 512):
        pos = i * 512 + 510
        buf[usa_ofs + 2 + 2 * i : usa_ofs + 4 + 2 * i] = buf[pos : pos + 2]
        buf[pos : pos + 2] = usn
    if rec.corrupt == "fixup":  # torn write: the last sector carries a stale update sequence number
        buf[record_size - 2] ^= 0xFF
    elif rec.corrupt == "usa":
        struct.pack_into("<H", buf, 6, usa_cnt + 1)
    elif rec.corrupt == "signature":
        buf[:4] = b"BAAD"
    return bytes(buf)


def build_mft(records: list[Rec], record_size: int = 1024, cluster_size: int = 4096, count: int = 0) -> bytes:
    """Records at their numbers; numbers not given stay zero (never used)."""
    n = max([count, *(r.number + 1 for r in records)])
    out = bytearray(n * record_size)
    for r in records:
        out[r.number * record_size : (r.number + 1) * record_size] = encode_record(
            r, record_size, cluster_size
        )
    return bytes(out)


# --------------------------------------------------------------------------- metafiles and trees

METAFILES = [
    (0, "$MFT"),
    (1, "$MFTMirr"),
    (2, "$LogFile"),
    (3, "$Volume"),
    (4, "$AttrDef"),
    (6, "$Bitmap"),
    (7, "$Boot"),
    (8, "$BadClus"),
    (9, "$Secure"),
    (10, "$UpCase"),
]


def metafile_records(mft_size: int = 1 << 20) -> list[Rec]:
    """The standard metafiles, the root directory and a small $Extend tree."""
    sys_attrs = FA_HIDDEN | FA_SYSTEM
    recs = [Rec(ROOT, [Name(ROOT, ".", WIN32_AND_DOS, 5)], dir=True, seq=5, attrs=sys_attrs)]
    for number, name in METAFILES:
        streams: list[Stream] = []
        if number == 0:
            streams = [Stream(mft_size)]
        elif number == 2:
            streams = [Stream(64 << 20)]
        elif number == 8:
            # as NTFS writes it: one hole over the whole volume, yet no sparse flag
            streams = [Stream(0), Stream(1 << 40, "$Bad", alloc=1 << 40, runs=[(None, 1 << 28)])]
        elif number == 9:
            streams = [Stream(300_000, "$SDS")]
        recs.append(
            Rec(
                number,
                [Name(ROOT, name, WIN32_AND_DOS, 5)],
                seq=number or 1,
                attrs=sys_attrs,
                streams=streams,
            )
        )
    recs.append(Rec(EXTEND, [Name(ROOT, "$Extend", WIN32_AND_DOS, 5)], dir=True, seq=11, attrs=sys_attrs))
    recs.append(Rec(24, [Name(EXTEND, "$Quota", WIN32_AND_DOS, 11)], attrs=sys_attrs))
    recs.append(Rec(25, [Name(EXTEND, "$ObjId", WIN32_AND_DOS, 11)], attrs=sys_attrs))
    recs.append(
        Rec(
            26,
            [Name(EXTEND, "$UsnJrnl", WIN32_AND_DOS, 11)],
            attrs=sys_attrs,
            streams=[Stream(8 << 20, "$J", sparse=True, compressed=2 << 20), Stream(32, "$Max")],
        )
    )
    recs.append(Rec(27, [Name(EXTEND, "$RmMetadata", WIN32_AND_DOS, 11)], dir=True, attrs=sys_attrs))
    recs.append(Rec(28, [Name(27, "$Txf", WIN32_AND_DOS)], dir=True, attrs=sys_attrs))
    return recs


DOCS, SUB, CYCLE_A, CYCLE_B = 64, 98, 92, 93
FA_RECALL_ON_DATA_ACCESS = 0x400000
LONG = "a-rather-long-hard-link-name-that-pushes-attributes-across-sectors"


def sample_records() -> list[Rec]:
    """Metafiles plus one record for every case the parser and the tree builder handle."""
    r = metafile_records()
    root, docs = ROOT, DOCS
    r += [
        Rec(docs, [Name(root, "docs", WIN32_AND_DOS)], dir=True, attrs=0x10),
        Rec(65, [Name(docs, "readme.txt", WIN32_AND_DOS)], streams=[Stream(100)]),
        Rec(66, [Name(docs, "big.iso", WIN32)], streams=[Stream(10_000_000)]),
        Rec(
            67,
            [Name(root, "comp.log", WIN32)],
            attrs=FA_ARCHIVE | FA_COMPRESSED,
            streams=[Stream(1 << 20, compressed=256 << 10)],
        ),
        Rec(
            68,
            [Name(root, "sparse.vhd", WIN32)],
            attrs=FA_SPARSE,
            streams=[Stream(1 << 30, sparse=True, alloc=1 << 30, compressed=64 << 10)],
        ),
        Rec(69, [Name(root, "hl-a.bin", POSIX), Name(docs, "hl-b.bin", WIN32)], streams=[Stream(50_000)]),
        Rec(
            70,
            [Name(root, "Program Files Long.txt", WIN32), Name(root, "PROGRA~1.TXT", DOS)],
            streams=[Stream(5000)],
        ),
        Rec(71, [Name(root, "x", WIN32), Name(docs, "Y~1", DOS)], streams=[Stream(10)]),
        Rec(72, [Name(root, "CaseSensitive", POSIX), Name(root, "casesensitive", POSIX)]),
        Rec(
            73,
            [Name(root, "with-ads.txt", WIN32)],
            streams=[Stream(3000), Stream(26, "Zone.Identifier"), Stream(100_000, "big")],
        ),
        Rec(74, [Name(root, "link.txt", WIN32)], attrs=FA_REPARSE, reparse_tag=IO_REPARSE_TAG_SYMLINK),
        Rec(
            75,
            [Name(root, "junction", WIN32)],
            dir=True,
            attrs=0x10 | FA_REPARSE,
            reparse_tag=IO_REPARSE_TAG_MOUNT_POINT,
        ),
        Rec(
            76,
            [Name(root, "dedup.vhdx", WIN32)],
            attrs=FA_REPARSE,
            reparse_tag=IO_REPARSE_TAG_DEDUP,
            streams=[Stream(1 << 20, sparse=True, alloc=1 << 20, compressed=0)],
        ),
        Rec(
            77,
            [Name(root, "long-target.lnk", WIN32)],
            attrs=FA_REPARSE,
            reparse_tag=IO_REPARSE_TAG_SYMLINK,
            reparse_resident=False,
        ),
        # attribute list: the data and a second name live in extension record 90
        Rec(
            78,
            [Name(docs, "fragmented.dat", WIN32)],
            attr_list=[(AT_SI, 78, 0), (AT_LIST, 78, 0), (AT_FN, 78, 0), (AT_FN, 90, 0), (AT_DATA, 90, 0)],
        ),
        Rec(90, [Name(root, "frag-link.dat", WIN32)], base=78, streams=[Stream(7_000_000)]),
        # extension records that must be ignored: stale sequence number, missing base, bad base
        Rec(79, [Name(root, "stale-ext.bin", WIN32)], seq=3),
        Rec(91, [Name(root, "ghost", WIN32)], base=79, base_seq=2, streams=[Stream(1 << 20)]),
        Rec(104, [Name(root, "ghost2", WIN32)], base=99_999),
        Rec(105, [Name(root, "ghost3", WIN32)], base=81),
        Rec(80, [Name(root, "deleted.txt", WIN32)], in_use=False, streams=[Stream(1 << 20)]),
        Rec(81, [Name(root, "torn", WIN32)], corrupt="fixup"),
        Rec(82, [Name(root, "bad-usa", WIN32)], corrupt="usa"),
        Rec(83, [Name(root, "bad-length", WIN32)], corrupt="length"),
        Rec(84, [Name(root, "baad", WIN32)], corrupt="signature"),
        Rec(85, [Name(root, "bad-first", WIN32)], corrupt="first"),
        Rec(86, [Name(root, "lone\ud800surrogate.txt", POSIX)], streams=[Stream(1)]),
        Rec(87, [Name(root, "\U0001f600.PNG", WIN32)], streams=[Stream(2)]),
        Rec(88, [Name(docs, "文档.docx", WIN32)], streams=[Stream(3)]),
        Rec(89, [Name(5000, "orphan.txt", WIN32)], streams=[Stream(4096)]),
        Rec(CYCLE_A, [Name(CYCLE_B, "cycle-a", WIN32)], dir=True),
        Rec(CYCLE_B, [Name(CYCLE_A, "cycle-b", WIN32)], dir=True),
        Rec(94, [Name(CYCLE_A, "in-cycle.txt", WIN32)], streams=[Stream(8192)]),
        Rec(95, [Name(root, "hidden-sys-ro", WIN32)], attrs=FA_HIDDEN | FA_SYSTEM | FA_READONLY),
        Rec(96, [Name(root, "cloud.docx", WIN32)], attrs=FA_RECALL_ON_DATA_ACCESS | FA_SPARSE),
        Rec(97, [Name(root, "secret.bin", WIN32)], attrs=FA_ENCRYPTED, streams=[Stream(1000)]),
        Rec(SUB, [Name(docs, "sub", WIN32)], dir=True),
        Rec(99, [Name(SUB, "deep.tar.gz", WIN32)], streams=[Stream(12_345)]),
        Rec(100, [Name(root, "no-si", WIN32)], no_si=True, streams=[Stream(10)]),
        Rec(101, [Name(root, "ads-in-ext", WIN32)], streams=[Stream(10)]),
        Rec(102, base=101, streams=[Stream(5000, "spilled")]),
        Rec(103, [Name(root, "adsdir", WIN32)], dir=True, streams=[Stream(9000, "meta")]),
        Rec(106, [Name(root, f"{LONG}-{i}", WIN32) for i in range(3)], streams=[Stream(4097)]),
        Rec(107, [Name(root, ".hidden-dotfile", WIN32)], streams=[Stream(1)]),
        Rec(108, [Name(root, "trailing.", WIN32)], streams=[Stream(1)]),
        Rec(109, [Name(root, "x.ThisExtensionIsTooLong", WIN32)], streams=[Stream(1)]),
        Rec(110, [Name(root, "y.SixteenCharsExtX", WIN32)], streams=[Stream(1)]),
        Rec(111, [Name(root, "z.扩展名", WIN32)], streams=[Stream(1)]),
    ]
    return r


def tree_records(spec: dict[str, Any], *, first: int = FIRST_USER) -> list[Rec]:
    """Records for a nested dict below the root: ``{"name": size}`` is a file, a dict a folder.

    Files larger than :data:`RESIDENT_MAX` get a non-resident $DATA.
    """
    out: list[Rec] = []
    number = first
    queue: list[tuple[int, dict[str, Any]]] = [(ROOT, spec)]
    while queue:
        parent, children = queue.pop(0)
        for name, value in children.items():
            if isinstance(value, dict):
                out.append(Rec(number, [Name(parent, name, WIN32)], dir=True, attrs=0x10))
                queue.append((number, value))
            else:
                out.append(Rec(number, [Name(parent, name, WIN32)], streams=[Stream(int(value))]))
            number += 1
    return out


# --------------------------------------------------------------------------- volume images


def boot_sector(*, cluster_size: int, record_size: int, mft_lcn: int, total_sectors: int) -> bytes:
    boot = bytearray(4096)
    boot[0:3] = b"\xebR\x90"
    boot[3:11] = b"NTFS    "
    struct.pack_into("<HB", boot, 0x0B, 512, cluster_size // 512)
    struct.pack_into("<Q", boot, 0x28, total_sectors)
    struct.pack_into("<QQ", boot, 0x30, mft_lcn, mft_lcn + 2)
    per_record = (
        record_size // cluster_size if record_size >= cluster_size else -(record_size.bit_length() - 1)
    )
    struct.pack_into("<b", boot, 0x40, per_record)
    struct.pack_into("<b", boot, 0x44, 1)
    struct.pack_into("<Q", boot, 0x48, 0x1234_5678_9ABC_DEF0)
    boot[510:512] = b"\x55\xaa"
    return bytes(boot)


def build_volume(
    path: Path,
    records: list[Rec],
    runs: list[tuple[int, int]],
    *,
    record_size: int = 1024,
    cluster_size: int = 4096,
    split_at: int | None = None,
    valid_records: int | None = None,
) -> bytes:
    """Write a volume image whose $MFT lives in ``runs`` (lcn, clusters); return the expected $MFT.

    ``split_at`` keeps only the first ``split_at`` runs in record 0 and moves the rest to
    extension record 16 (reached through record 0's $ATTRIBUTE_LIST). ``valid_records`` makes the
    $MFT's valid data length shorter than its size: the reader must return zeros past it.
    """
    clusters = sum(c for _, c in runs)
    size = clusters * cluster_size
    count = size // record_size
    valid = size if valid_records is None else valid_records * record_size
    first_runs = runs if split_at is None else runs[:split_at]
    mft_streams = [Stream(size, alloc=size, runs=list(first_runs), resident=False, init=valid)]
    rec0 = Rec(0, [Name(ROOT, "$MFT", WIN32_AND_DOS, 5)], attrs=FA_HIDDEN | FA_SYSTEM, streams=mft_streams)
    recs = [r for r in records if r.number != 0]
    if split_at is not None:
        vcn = sum(c for _, c in first_runs)
        rec0.attr_list = [(AT_SI, 0, 0), (AT_FN, 0, 0), (AT_DATA, 0, 0), (AT_DATA, 16, vcn)]
        recs = [r for r in recs if r.number != 16]
        tail = Stream(0, lowest_vcn=vcn, runs=list(runs[split_at:]), resident=False)
        recs.append(Rec(16, base=0, base_seq=1, streams=[tail]))
    mft = bytearray(build_mft([rec0, *recs], record_size, cluster_size, count=count))
    if len(mft) != size:
        raise ValueError("records do not fit in the given runs")
    # the image: boot sector, garbage everywhere else, $MFT clusters where the runs say
    end = max(lcn + c for lcn, c in runs) + 4
    img = bytearray(b"\xa5" * (end * cluster_size))
    mft_lcn = runs[0][0]
    img[:4096] = boot_sector(
        cluster_size=cluster_size, record_size=record_size, mft_lcn=mft_lcn, total_sectors=len(img) // 512
    )
    on_disk = bytes(mft[:valid]) + b"\xcc" * (size - valid)  # stale bytes past the valid length
    vcn = 0
    for lcn, c in runs:
        img[lcn * cluster_size : (lcn + c) * cluster_size] = on_disk[
            vcn * cluster_size : (vcn + c) * cluster_size
        ]
        vcn += c
    path.write_bytes(bytes(img))
    return on_disk[:valid] + bytes(size - valid)  # nothing past the valid data length is read


# --------------------------------------------------------------------------- large synthetic images

_MAGIC_PARENT = 0x0000_7A5C_3E91_D4B7  # placeholder parent record, patched per row
_FIRST_SYNTH = 32


def _templates(record_size: int, cluster_size: int) -> tuple[np.ndarray, list[list[int]], np.ndarray]:
    """Encoded file/dir records with a placeholder parent; returns (records, parent offsets, is_dir)."""
    p = _MAGIC_PARENT
    specs: list[Rec] = []
    exts = ["txt", "dll", "jpg", "png", "cs", "h", "json", "xml", "mp4", "manifest", "日志", ""]
    for i in range(48):
        ext = exts[i % len(exts)]
        name = f"file{i:02d}-{'x' * (i % 9)}" + (f".{ext}" if ext else "")
        size = [0, 120, 700, 5000, 1 << 20, 123_456_789][i % 6]
        streams = [Stream(size)]
        if i % 11 == 3:
            streams.append(Stream(26, "Zone.Identifier"))
        names = [Name(p, name, WIN32 if i % 4 else WIN32_AND_DOS)]
        if i % 7 == 5:
            names.append(Name(p, f"FILE{i:02d}~1.TXT", DOS))
        if i % 13 == 6:
            names.append(Name(p, f"link{i:02d}.bin", WIN32))
        specs.append(Rec(0, names, streams=streams, attrs=FA_ARCHIVE | (FA_HIDDEN if i % 10 == 0 else 0)))
    for i in range(8):
        specs.append(Rec(0, [Name(p, f"folder{i}" + "-dir" * (i % 3), WIN32_AND_DOS)], dir=True, attrs=0x10))
    recs = np.zeros((len(specs), record_size), dtype=np.uint8)
    offsets: list[list[int]] = []
    pattern = struct.pack("<Q", (1 << 48) | p)
    for k, spec in enumerate(specs):
        raw = encode_record(spec, record_size, cluster_size)
        found = [i for i in range(len(raw) - 7) if raw[i : i + 8] == pattern]
        assert len(found) == len(spec.names) and all(f + 8 <= 500 for f in found)
        recs[k] = np.frombuffer(raw, dtype=np.uint8)
        offsets.append(found)
    return recs, offsets, np.array([s.dir for s in specs])


def synthetic_mft(
    count: int, *, record_size: int = 1024, cluster_size: int = 4096, seed: int = 0
) -> np.ndarray:
    """``count`` records as an ``(count, record_size)`` uint8 array: metafiles, then a random tree.

    Built from a few dozen encoded templates (hard links, DOS aliases, streams, folders) whose
    parent references are patched per record, so millions of records take seconds.
    """
    rng = np.random.default_rng(seed)
    out = np.zeros((count, record_size), dtype=np.uint8)
    head = build_mft(metafile_records(), record_size, cluster_size)
    nhead = min(count, len(head) // record_size)
    out[:nhead] = np.frombuffer(head, dtype=np.uint8).reshape(-1, record_size)[:nhead]
    if count <= _FIRST_SYNTH:
        return out
    tpl, offsets, tpl_dir = _templates(record_size, cluster_size)
    n = count - _FIRST_SYNTH
    files = np.flatnonzero(~tpl_dir)
    dirs = np.flatnonzero(tpl_dir)
    is_dir = rng.random(n) < 0.06
    is_dir[0] = True
    choice = np.where(is_dir, rng.choice(dirs, n), rng.choice(files, n))
    out[_FIRST_SYNTH:] = tpl[choice]
    numbers = np.arange(_FIRST_SYNTH, count, dtype=np.int64)
    dir_numbers = numbers[is_dir]
    # parent: a random directory created earlier (or the root); dirs nest, files spread out
    rank = np.searchsorted(dir_numbers, numbers) - 1  # directories before this record
    pick = (rng.random(n) * (rank + 2)).astype(np.int64) - 1
    parent = np.where(pick >= 0, dir_numbers[np.maximum(pick, 0)], ROOT)
    refs = ((1 << 48) | parent.astype(np.uint64)).astype("<u8").view(np.uint8).reshape(n, 8)
    body = out[_FIRST_SYNTH:]
    for t, offs in enumerate(offsets):
        rows = np.flatnonzero(choice == t)
        for off in offs:
            body[rows, off : off + 8] = refs[rows]
    body[:, 0x2C:0x30] = numbers.astype("<u4").view(np.uint8).reshape(n, 4)
    return out
