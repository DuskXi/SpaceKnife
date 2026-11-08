//! Synthetic NTFS `$MFT` images for tests: the Rust port of packages/backend/tests/mft_image.py.
//!
//! [`encode_record`] writes a valid FILE record — header, update sequence (fixups),
//! `$STANDARD_INFORMATION`, `$ATTRIBUTE_LIST`, `$FILE_NAME` in any namespace, resident /
//! non-resident / compressed / sparse `$DATA`, named streams, `$REPARSE_POINT` — or a deliberately
//! broken one. [`build_mft`] lays records out at their numbers; [`build_volume`] writes a small
//! volume image (boot sector + a fragmented `$MFT`, optionally continued through an extension
//! record) for the raw reader.

#![allow(dead_code)]

use std::path::Path;

pub const POSIX: u8 = 0;
pub const WIN32: u8 = 1;
pub const DOS: u8 = 2;
pub const WIN32_AND_DOS: u8 = 3;
pub const ROOT: u64 = 5;
pub const EXTEND: u64 = 11;
pub const RESIDENT_MAX: i64 = 256; // streams up to this size are resident by default
pub const DATA_LCN: u64 = 100_000; // where default non-resident runs point (never read)

pub const AT_SI: u32 = 0x10;
pub const AT_LIST: u32 = 0x20;
pub const AT_FN: u32 = 0x30;
pub const AT_DATA: u32 = 0x80;
pub const AT_REPARSE: u32 = 0xC0;
pub const AT_END: u32 = 0xFFFF_FFFF;
pub const FA_READONLY: u32 = 0x1;
pub const FA_HIDDEN: u32 = 0x2;
pub const FA_SYSTEM: u32 = 0x4;
pub const FA_ARCHIVE: u32 = 0x20;
pub const FA_SPARSE: u32 = 0x200;
pub const FA_REPARSE: u32 = 0x400;
pub const FA_COMPRESSED: u32 = 0x800;
pub const FA_ENCRYPTED: u32 = 0x4000;
pub const FA_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
pub const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
pub const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
pub const IO_REPARSE_TAG_DEDUP: u32 = 0x8000_0013; // not a name surrogate: a regular file
pub const EPOCH_DELTA: i64 = 11_644_473_600;

pub fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub fn filetime(unix: i64) -> u64 {
    if unix == 0 {
        0
    } else {
        ((unix + EPOCH_DELTA) * 10_000_000 + 1_234_567) as u64
    }
}

fn align8(v: usize) -> usize {
    (v + 7) & !7
}

/// One `$DATA` attribute (unnamed = the file's contents).
#[derive(Clone, Debug, Default)]
pub struct Stream {
    pub size: i64,
    pub name: Vec<u16>,
    /// default: size <= RESIDENT_MAX
    pub resident: Option<bool>,
    /// non-resident allocated size (default: size rounded up to clusters)
    pub alloc: Option<i64>,
    /// compressed/sparse: actual allocation (the 0x48-byte header)
    pub compressed: Option<i64>,
    pub sparse: bool,
    /// > 0: a later extent of a fragmented attribute
    pub lowest_vcn: u64,
    /// (lcn or None for sparse, clusters)
    pub runs: Option<Vec<(Option<u64>, u64)>>,
    /// initialised (valid data) length, default: size
    pub init: Option<i64>,
}

pub fn stream(size: i64) -> Stream {
    Stream {
        size,
        ..Stream::default()
    }
}

pub fn named(size: i64, name: &str) -> Stream {
    Stream {
        size,
        name: w(name),
        ..Stream::default()
    }
}

#[derive(Clone, Debug)]
pub struct Name {
    pub parent: u64,
    pub name: Vec<u16>,
    pub ns: u8,
    pub parent_seq: u64,
}

pub fn nm(parent: u64, name: &str, ns: u8) -> Name {
    nm16(parent, w(name), ns)
}

pub fn nm16(parent: u64, name: Vec<u16>, ns: u8) -> Name {
    Name {
        parent,
        name,
        ns,
        parent_seq: 1,
    }
}

pub fn nm_seq(parent: u64, name: &str, ns: u8, parent_seq: u64) -> Name {
    Name {
        parent_seq,
        ..nm(parent, name, ns)
    }
}

#[derive(Clone, Debug)]
pub struct Rec {
    pub number: u64,
    pub names: Vec<Name>,
    pub dir: bool,
    pub in_use: bool,
    pub seq: u16,
    pub attrs: u32,
    /// created, modified, accessed
    pub times: (i64, i64, i64),
    pub streams: Vec<Stream>,
    pub reparse_tag: Option<u32>,
    pub reparse_resident: bool,
    /// extension record of this base record
    pub base: Option<u64>,
    pub base_seq: u64,
    /// (type, holder record, lowest vcn)
    pub attr_list: Option<Vec<(u32, u64, u64)>>,
    pub no_si: bool,
    /// "fixup" | "usa" | "length" | "signature" | "first"
    pub corrupt: &'static str,
}

pub fn rec(number: u64, names: Vec<Name>) -> Rec {
    Rec {
        number,
        names,
        dir: false,
        in_use: true,
        seq: 1,
        attrs: FA_ARCHIVE,
        times: (1_600_000_000, 1_700_000_000, 1_700_000_500),
        streams: Vec::new(),
        reparse_tag: None,
        reparse_resident: true,
        base: None,
        base_seq: 1,
        attr_list: None,
        no_si: false,
        corrupt: "",
    }
}

fn utf16le(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

struct Out(Vec<u8>);

impl Out {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn i64(&mut self, v: i64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
}

fn put(buf: &mut [u8], at: usize, bytes: &[u8]) {
    buf[at..at + bytes.len()].copy_from_slice(bytes);
}

fn resident(typ: u32, value: &[u8], name: &[u16], flags: u16, indexed: u8) -> Vec<u8> {
    let name_b = utf16le(name);
    let voff = align8(0x18 + name_b.len());
    let total = align8(voff + value.len());
    let mut hdr = Out(Vec::new());
    hdr.u32(typ)
        .u32(total as u32)
        .u8(0)
        .u8(name.len() as u8)
        .u16(0x18)
        .u16(flags)
        .u16(0)
        .u32(value.len() as u32)
        .u16(voff as u16)
        .u8(indexed)
        .u8(0);
    let mut out = vec![0u8; total];
    put(&mut out, 0, &hdr.0);
    put(&mut out, 0x18, &name_b);
    put(&mut out, voff, value);
    out
}

fn uint_bytes(v: u64) -> Vec<u8> {
    let bits = 64 - v.leading_zeros() as usize;
    let k = bits.div_ceil(8).max(1);
    v.to_le_bytes()[..k].to_vec()
}

fn sint_bytes(v: i64) -> Vec<u8> {
    let mut k = 1;
    while !(-(1i128 << (8 * k - 1)) <= v as i128 && (v as i128) < (1i128 << (8 * k - 1))) {
        k += 1;
    }
    v.to_le_bytes()[..k].to_vec()
}

pub fn encode_runs(runs: &[(Option<u64>, u64)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut prev = 0i64;
    for &(lcn, count) in runs {
        let lb = uint_bytes(count);
        match lcn {
            None => {
                out.push(lb.len() as u8);
                out.extend_from_slice(&lb);
            }
            Some(lcn) => {
                let ob = sint_bytes(lcn as i64 - prev);
                out.push(((ob.len() as u8) << 4) | lb.len() as u8);
                out.extend_from_slice(&lb);
                out.extend_from_slice(&ob);
                prev = lcn as i64;
            }
        }
    }
    out.push(0);
    out
}

#[allow(clippy::too_many_arguments)]
fn nonresident(
    typ: u32,
    name: &[u16],
    runs: &[(Option<u64>, u64)],
    lowest_vcn: u64,
    alloc: i64,
    size: i64,
    init: i64,
    compressed: Option<i64>,
    flags: u16,
) -> Vec<u8> {
    let name_b = utf16le(name);
    let hdr_len = if compressed.is_some() { 0x48 } else { 0x40 };
    let mp_off = align8(hdr_len + name_b.len());
    let mp = encode_runs(runs);
    let total = align8(mp_off + mp.len());
    let clusters: u64 = runs.iter().map(|r| r.1).sum();
    let mut hdr = Out(Vec::new());
    hdr.u32(typ)
        .u32(total as u32)
        .u8(1)
        .u8(name.len() as u8)
        .u16(hdr_len as u16)
        .u16(flags)
        .u16(0)
        .u64(lowest_vcn)
        .u64((lowest_vcn + clusters).wrapping_sub(1))
        .u16(mp_off as u16)
        .u16(if flags & 1 != 0 { 4 } else { 0 })
        .u32(0)
        .i64(alloc)
        .i64(size)
        .i64(init);
    if let Some(c) = compressed {
        hdr.i64(c);
    }
    let mut out = vec![0u8; total];
    put(&mut out, 0, &hdr.0);
    put(&mut out, hdr_len, &name_b);
    put(&mut out, mp_off, &mp);
    out
}

fn data_attr(st: &Stream, cluster: i64) -> Vec<u8> {
    if st.resident.unwrap_or(st.size <= RESIDENT_MAX) {
        return resident(AT_DATA, &vec![0x5a; st.size as usize], &st.name, 0, 0);
    }
    let alloc = st
        .alloc
        .unwrap_or_else(|| (st.size + cluster - 1).div_euclid(cluster) * cluster);
    let runs = st
        .runs
        .clone()
        .unwrap_or_else(|| vec![(Some(DATA_LCN), (alloc / cluster).max(1) as u64)]);
    let flags = (if st.sparse { 0x8000 } else { 0 })
        | (if st.compressed.is_some() && !st.sparse {
            1
        } else {
            0
        });
    let first = st.lowest_vcn == 0;
    nonresident(
        AT_DATA,
        &st.name,
        &runs,
        st.lowest_vcn,
        if first { alloc } else { 0 },
        if first { st.size } else { 0 },
        if first { st.init.unwrap_or(st.size) } else { 0 },
        st.compressed,
        flags,
    )
}

fn si_value(r: &Rec) -> Vec<u8> {
    let (cr, mt, at) = (
        filetime(r.times.0),
        filetime(r.times.1),
        filetime(r.times.2),
    );
    let mut o = Out(Vec::new());
    o.u64(cr).u64(mt).u64(mt).u64(at).u32(r.attrs);
    o.u32(0).u32(0).u32(0).u32(0).u32(0x100).u64(0).u64(0);
    o.0
}

fn fn_value(n: &Name, r: &Rec) -> Vec<u8> {
    let (cr, mt, at) = (
        filetime(r.times.0),
        filetime(r.times.1),
        filetime(r.times.2),
    );
    let fa = r.attrs | if r.dir { 0x1000_0000 } else { 0 };
    let mut o = Out(Vec::new());
    o.u64((n.parent_seq << 48) | n.parent)
        .u64(cr)
        .u64(mt)
        .u64(mt)
        .u64(at)
        .i64(0)
        .i64(0)
        .u32(fa)
        .u32(0)
        .u8(n.name.len() as u8)
        .u8(n.ns);
    o.0.extend_from_slice(&utf16le(&n.name));
    o.0
}

fn list_value(entries: &[(u32, u64, u64)]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(typ, holder, vcn) in entries {
        let mut o = Out(Vec::new());
        o.u32(typ)
            .u16(0x20)
            .u8(0)
            .u8(0x1A)
            .u64(vcn)
            .u64((1 << 48) | holder)
            .u16(0);
        o.0.resize(0x20, 0);
        out.extend_from_slice(&o.0);
    }
    out
}

/// One FILE record with its update sequence applied (as it is on disk).
pub fn encode_record(r: &Rec, record_size: usize, cluster_size: usize) -> Vec<u8> {
    let usa_cnt = record_size / 512 + 1;
    let usa_ofs = 0x30usize;
    let first = align8(usa_ofs + 2 * usa_cnt);
    let mut attrs: Vec<Vec<u8>> = Vec::new();
    if r.base.is_none() && !r.no_si {
        attrs.push(resident(AT_SI, &si_value(r), &[], 0, 0));
    }
    if let Some(list) = &r.attr_list {
        attrs.push(resident(AT_LIST, &list_value(list), &[], 0, 0));
    }
    for n in &r.names {
        attrs.push(resident(AT_FN, &fn_value(n, r), &[], 0, 1));
    }
    for st in &r.streams {
        attrs.push(data_attr(st, cluster_size as i64));
    }
    if let Some(tag) = r.reparse_tag {
        if r.reparse_resident {
            let mut v = Out(Vec::new());
            v.u32(tag).u16(8).u16(0).u64(0);
            attrs.push(resident(AT_REPARSE, &v.0, &[], 0, 0));
        } else {
            attrs.push(nonresident(
                AT_REPARSE,
                &[],
                &[(Some(DATA_LCN), 1)],
                0,
                4096,
                16,
                16,
                None,
                0,
            ));
        }
    }
    let mut body: Vec<u8> = attrs.concat();
    body.extend_from_slice(&AT_END.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes());
    let used = first + body.len();
    assert!(
        used <= record_size,
        "record {} does not fit: {used} > {record_size}",
        r.number
    );
    let flags = u16::from(r.in_use) | if r.dir { 2 } else { 0 };
    let base_ref = r.base.map_or(0, |b| (r.base_seq << 48) | b);
    let links = r.names.iter().filter(|n| n.ns != DOS).count() as u16;
    let mut h = Out(b"FILE".to_vec());
    h.u16(usa_ofs as u16)
        .u16(usa_cnt as u16)
        .u64(0x1000 + r.number)
        .u16(r.seq)
        .u16(links)
        .u16(first as u16)
        .u16(flags)
        .u32(used as u32)
        .u32(record_size as u32)
        .u64(base_ref)
        .u16(attrs.len() as u16 + 1)
        .u16(0)
        .u32((r.number & 0xFFFF_FFFF) as u32);
    let mut buf = vec![0u8; record_size];
    put(&mut buf, 0, &h.0);
    put(&mut buf, first, &body);
    match r.corrupt {
        // first attribute claims to run past the used size
        "length" => put(&mut buf, first + 4, &(record_size as u32).to_le_bytes()),
        "first" => put(&mut buf, 0x14, &0x10u16.to_le_bytes()),
        _ => {}
    }
    let usn = ((r.number % 0xFFF0) as u16 + 1).to_le_bytes();
    put(&mut buf, usa_ofs, &usn);
    for i in 0..record_size / 512 {
        let pos = i * 512 + 510;
        let saved = [buf[pos], buf[pos + 1]];
        put(&mut buf, usa_ofs + 2 + 2 * i, &saved);
        put(&mut buf, pos, &usn);
    }
    match r.corrupt {
        // torn write: the last sector carries a stale update sequence number
        "fixup" => buf[record_size - 2] ^= 0xFF,
        "usa" => put(&mut buf, 6, &(usa_cnt as u16 + 1).to_le_bytes()),
        "signature" => put(&mut buf, 0, b"BAAD"),
        _ => {}
    }
    buf
}

/// Records at their numbers; numbers not given stay zero (never used).
pub fn build_mft(
    records: &[Rec],
    record_size: usize,
    cluster_size: usize,
    count: usize,
) -> Vec<u8> {
    let n = records
        .iter()
        .map(|r| r.number as usize + 1)
        .chain([count])
        .max()
        .unwrap_or(0);
    let mut out = vec![0u8; n * record_size];
    for r in records {
        let at = r.number as usize * record_size;
        put(&mut out, at, &encode_record(r, record_size, cluster_size));
    }
    out
}

const METAFILES: [(u64, &str); 10] = [
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
];

/// The standard metafiles, the root directory and a small $Extend tree.
pub fn metafile_records() -> Vec<Rec> {
    let mft_size = 1i64 << 20;
    let sys = FA_HIDDEN | FA_SYSTEM;
    let mut recs = vec![Rec {
        dir: true,
        seq: 5,
        attrs: sys,
        ..rec(ROOT, vec![nm_seq(ROOT, ".", WIN32_AND_DOS, 5)])
    }];
    for (number, name) in METAFILES {
        let streams = match number {
            0 => vec![stream(mft_size)],
            2 => vec![stream(64 << 20)],
            // as NTFS writes it: one hole over the whole volume, yet no sparse flag
            8 => vec![
                stream(0),
                Stream {
                    alloc: Some(1 << 40),
                    runs: Some(vec![(None, 1 << 28)]),
                    ..named(1 << 40, "$Bad")
                },
            ],
            9 => vec![named(300_000, "$SDS")],
            _ => Vec::new(),
        };
        recs.push(Rec {
            seq: if number == 0 { 1 } else { number as u16 },
            attrs: sys,
            streams,
            ..rec(number, vec![nm_seq(ROOT, name, WIN32_AND_DOS, 5)])
        });
    }
    recs.push(Rec {
        dir: true,
        seq: 11,
        attrs: sys,
        ..rec(EXTEND, vec![nm_seq(ROOT, "$Extend", WIN32_AND_DOS, 5)])
    });
    let ext = |n: u64, name: &str| Rec {
        attrs: sys,
        ..rec(n, vec![nm_seq(EXTEND, name, WIN32_AND_DOS, 11)])
    };
    recs.push(ext(24, "$Quota"));
    recs.push(ext(25, "$ObjId"));
    recs.push(Rec {
        streams: vec![
            Stream {
                sparse: true,
                compressed: Some(2 << 20),
                ..named(8 << 20, "$J")
            },
            named(32, "$Max"),
        ],
        ..ext(26, "$UsnJrnl")
    });
    recs.push(Rec {
        dir: true,
        ..ext(27, "$RmMetadata")
    });
    recs.push(Rec {
        dir: true,
        attrs: sys,
        ..rec(28, vec![nm(27, "$Txf", WIN32_AND_DOS)])
    });
    recs
}

pub const DOCS: u64 = 64;
pub const SUB: u64 = 98;
pub const CYCLE_A: u64 = 92;
pub const CYCLE_B: u64 = 93;
pub const LONG: &str = "a-rather-long-hard-link-name-that-pushes-attributes-across-sectors";

/// Metafiles plus one record for every case the parser and the tree builder handle.
pub fn sample_records() -> Vec<Rec> {
    let mut r = metafile_records();
    let (root, docs) = (ROOT, DOCS);
    let file = |n: u64, parent: u64, name: &str, ns: u8, size: i64| Rec {
        streams: vec![stream(size)],
        ..rec(n, vec![nm(parent, name, ns)])
    };
    let bare = |n: u64, name: &str| rec(n, vec![nm(root, name, WIN32)]);
    let lone = [w("lone"), vec![0xD800], w("surrogate.txt")].concat();
    r.extend([
        Rec {
            dir: true,
            attrs: 0x10,
            ..rec(docs, vec![nm(root, "docs", WIN32_AND_DOS)])
        },
        file(65, docs, "readme.txt", WIN32_AND_DOS, 100),
        file(66, docs, "big.iso", WIN32, 10_000_000),
        Rec {
            attrs: FA_ARCHIVE | FA_COMPRESSED,
            streams: vec![Stream {
                compressed: Some(256 << 10),
                ..stream(1 << 20)
            }],
            ..bare(67, "comp.log")
        },
        Rec {
            attrs: FA_SPARSE,
            streams: vec![Stream {
                sparse: true,
                alloc: Some(1 << 30),
                compressed: Some(64 << 10),
                ..stream(1 << 30)
            }],
            ..bare(68, "sparse.vhd")
        },
        Rec {
            streams: vec![stream(50_000)],
            ..rec(
                69,
                vec![nm(root, "hl-a.bin", POSIX), nm(docs, "hl-b.bin", WIN32)],
            )
        },
        Rec {
            streams: vec![stream(5000)],
            ..rec(
                70,
                vec![
                    nm(root, "Program Files Long.txt", WIN32),
                    nm(root, "PROGRA~1.TXT", DOS),
                ],
            )
        },
        Rec {
            streams: vec![stream(10)],
            ..rec(71, vec![nm(root, "x", WIN32), nm(docs, "Y~1", DOS)])
        },
        rec(
            72,
            vec![
                nm(root, "CaseSensitive", POSIX),
                nm(root, "casesensitive", POSIX),
            ],
        ),
        Rec {
            streams: vec![
                stream(3000),
                named(26, "Zone.Identifier"),
                named(100_000, "big"),
            ],
            ..bare(73, "with-ads.txt")
        },
        Rec {
            attrs: FA_REPARSE,
            reparse_tag: Some(IO_REPARSE_TAG_SYMLINK),
            ..bare(74, "link.txt")
        },
        Rec {
            dir: true,
            attrs: 0x10 | FA_REPARSE,
            reparse_tag: Some(IO_REPARSE_TAG_MOUNT_POINT),
            ..bare(75, "junction")
        },
        Rec {
            attrs: FA_REPARSE,
            reparse_tag: Some(IO_REPARSE_TAG_DEDUP),
            streams: vec![Stream {
                sparse: true,
                alloc: Some(1 << 20),
                compressed: Some(0),
                ..stream(1 << 20)
            }],
            ..bare(76, "dedup.vhdx")
        },
        Rec {
            attrs: FA_REPARSE,
            reparse_tag: Some(IO_REPARSE_TAG_SYMLINK),
            reparse_resident: false,
            ..bare(77, "long-target.lnk")
        },
        // attribute list: the data and a second name live in extension record 90
        Rec {
            attr_list: Some(vec![
                (AT_SI, 78, 0),
                (AT_LIST, 78, 0),
                (AT_FN, 78, 0),
                (AT_FN, 90, 0),
                (AT_DATA, 90, 0),
            ]),
            ..rec(78, vec![nm(docs, "fragmented.dat", WIN32)])
        },
        Rec {
            base: Some(78),
            streams: vec![stream(7_000_000)],
            ..bare(90, "frag-link.dat")
        },
        // extension records that must be ignored: stale sequence number, missing base, bad base
        Rec {
            seq: 3,
            ..bare(79, "stale-ext.bin")
        },
        Rec {
            base: Some(79),
            base_seq: 2,
            streams: vec![stream(1 << 20)],
            ..bare(91, "ghost")
        },
        Rec {
            base: Some(99_999),
            ..bare(104, "ghost2")
        },
        Rec {
            base: Some(81),
            ..bare(105, "ghost3")
        },
        Rec {
            in_use: false,
            streams: vec![stream(1 << 20)],
            ..bare(80, "deleted.txt")
        },
        Rec {
            corrupt: "fixup",
            ..bare(81, "torn")
        },
        Rec {
            corrupt: "usa",
            ..bare(82, "bad-usa")
        },
        Rec {
            corrupt: "length",
            ..bare(83, "bad-length")
        },
        Rec {
            corrupt: "signature",
            ..bare(84, "baad")
        },
        Rec {
            corrupt: "first",
            ..bare(85, "bad-first")
        },
        Rec {
            streams: vec![stream(1)],
            ..rec(86, vec![nm16(root, lone, POSIX)])
        },
        file(87, root, "\u{1F600}.PNG", WIN32, 2),
        file(88, docs, "文档.docx", WIN32, 3),
        file(89, 5000, "orphan.txt", WIN32, 4096),
        Rec {
            dir: true,
            ..rec(CYCLE_A, vec![nm(CYCLE_B, "cycle-a", WIN32)])
        },
        Rec {
            dir: true,
            ..rec(CYCLE_B, vec![nm(CYCLE_A, "cycle-b", WIN32)])
        },
        file(94, CYCLE_A, "in-cycle.txt", WIN32, 8192),
        Rec {
            attrs: FA_HIDDEN | FA_SYSTEM | FA_READONLY,
            ..bare(95, "hidden-sys-ro")
        },
        Rec {
            attrs: FA_RECALL_ON_DATA_ACCESS | FA_SPARSE,
            ..bare(96, "cloud.docx")
        },
        Rec {
            attrs: FA_ENCRYPTED,
            streams: vec![stream(1000)],
            ..bare(97, "secret.bin")
        },
        Rec {
            dir: true,
            ..rec(SUB, vec![nm(docs, "sub", WIN32)])
        },
        file(99, SUB, "deep.tar.gz", WIN32, 12_345),
        Rec {
            no_si: true,
            streams: vec![stream(10)],
            ..bare(100, "no-si")
        },
        file(101, root, "ads-in-ext", WIN32, 10),
        Rec {
            base: Some(101),
            streams: vec![named(5000, "spilled")],
            ..rec(102, Vec::new())
        },
        Rec {
            dir: true,
            streams: vec![named(9000, "meta")],
            ..bare(103, "adsdir")
        },
        Rec {
            streams: vec![stream(4097)],
            ..rec(
                106,
                (0..3)
                    .map(|i| nm(root, &format!("{LONG}-{i}"), WIN32))
                    .collect(),
            )
        },
        file(107, root, ".hidden-dotfile", WIN32, 1),
        file(108, root, "trailing.", WIN32, 1),
        file(109, root, "x.ThisExtensionIsTooLong", WIN32, 1),
        file(110, root, "y.SixteenCharsExtX", WIN32, 1),
        file(111, root, "z.扩展名", WIN32, 1),
    ]);
    r
}

pub fn boot_sector(
    cluster_size: usize,
    record_size: usize,
    mft_lcn: u64,
    total_sectors: u64,
) -> Vec<u8> {
    let mut boot = vec![0u8; 4096];
    put(&mut boot, 0, b"\xebR\x90");
    put(&mut boot, 3, b"NTFS    ");
    put(&mut boot, 0x0B, &512u16.to_le_bytes());
    boot[0x0D] = (cluster_size / 512) as u8;
    put(&mut boot, 0x28, &total_sectors.to_le_bytes());
    put(&mut boot, 0x30, &mft_lcn.to_le_bytes());
    put(&mut boot, 0x38, &(mft_lcn + 2).to_le_bytes());
    let per_record: i8 = if record_size >= cluster_size {
        (record_size / cluster_size) as i8
    } else {
        -((usize::BITS - record_size.leading_zeros() - 1) as i8)
    };
    boot[0x40] = per_record as u8;
    boot[0x44] = 1;
    put(&mut boot, 0x48, &0x1234_5678_9ABC_DEF0u64.to_le_bytes());
    put(&mut boot, 510, b"\x55\xaa");
    boot
}

/// Write a volume image whose `$MFT` lives in `runs` (lcn, clusters); return the expected `$MFT`.
///
/// `split_at` keeps only the first `split_at` runs in record 0 and moves the rest to extension
/// record 16 (reached through record 0's `$ATTRIBUTE_LIST`). `valid_records` makes the `$MFT`'s
/// valid data length shorter than its size: the reader must return zeros past it.
pub fn build_volume(
    path: &Path,
    records: &[Rec],
    runs: &[(u64, u64)],
    record_size: usize,
    cluster_size: usize,
    split_at: Option<usize>,
    valid_records: Option<usize>,
) -> Vec<u8> {
    let clusters: u64 = runs.iter().map(|r| r.1).sum();
    let size = clusters as usize * cluster_size;
    let count = size / record_size;
    let valid = valid_records.map_or(size, |v| v * record_size);
    let first_runs = &runs[..split_at.unwrap_or(runs.len())];
    let as_runs = |rs: &[(u64, u64)]| rs.iter().map(|&(l, c)| (Some(l), c)).collect::<Vec<_>>();
    let mut rec0 = Rec {
        attrs: FA_HIDDEN | FA_SYSTEM,
        streams: vec![Stream {
            alloc: Some(size as i64),
            runs: Some(as_runs(first_runs)),
            resident: Some(false),
            init: Some(valid as i64),
            ..stream(size as i64)
        }],
        ..rec(0, vec![nm_seq(ROOT, "$MFT", WIN32_AND_DOS, 5)])
    };
    let mut recs: Vec<Rec> = records.iter().filter(|r| r.number != 0).cloned().collect();
    if let Some(split) = split_at {
        let vcn: u64 = first_runs.iter().map(|r| r.1).sum();
        rec0.attr_list = Some(vec![
            (AT_SI, 0, 0),
            (AT_FN, 0, 0),
            (AT_DATA, 0, 0),
            (AT_DATA, 16, vcn),
        ]);
        recs.retain(|r| r.number != 16);
        let tail = Stream {
            lowest_vcn: vcn,
            runs: Some(as_runs(&runs[split..])),
            resident: Some(false),
            ..stream(0)
        };
        recs.push(Rec {
            base: Some(0),
            base_seq: 1,
            streams: vec![tail],
            ..rec(16, Vec::new())
        });
    }
    recs.insert(0, rec0);
    let mft = build_mft(&recs, record_size, cluster_size, count);
    assert_eq!(mft.len(), size, "records do not fit in the given runs");
    // the image: boot sector, garbage everywhere else, $MFT clusters where the runs say
    let end = runs.iter().map(|&(l, c)| l + c).max().unwrap() + 4;
    let mut img = vec![0xa5u8; end as usize * cluster_size];
    let total_sectors = (img.len() / 512) as u64;
    put(
        &mut img,
        0,
        &boot_sector(cluster_size, record_size, runs[0].0, total_sectors),
    );
    let mut on_disk = mft[..valid].to_vec();
    on_disk.resize(size, 0xcc); // stale bytes past the valid length
    let mut vcn = 0usize;
    for &(lcn, c) in runs {
        let (lcn, c) = (lcn as usize, c as usize);
        put(
            &mut img,
            lcn * cluster_size,
            &on_disk[vcn * cluster_size..(vcn + c) * cluster_size],
        );
        vcn += c;
    }
    std::fs::write(path, &img).unwrap();
    let mut expected = on_disk[..valid].to_vec();
    expected.resize(size, 0); // nothing past the valid data length is read
    expected
}

/// A large random tree below the root: `n` records from 32 on (folders, files with one or two
/// names, named streams, some non-resident), for the parallel and streaming paths.
pub fn synthetic_mft(n: usize, seed: u64, record_size: usize) -> Vec<u8> {
    build_mft(&synthetic_records(n, seed), record_size, 4096, 0)
}

/// The records of [`synthetic_mft`] (metafiles included).
pub fn synthetic_records(n: usize, seed: u64) -> Vec<Rec> {
    let mut rng = seed.max(1);
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut recs = metafile_records();
    let mut dirs = vec![ROOT];
    for i in 0..n as u64 {
        let number = 32 + i;
        let parent = dirs[(next() % dirs.len() as u64) as usize];
        let r = next();
        let name = format!(
            "item-{number}.{}",
            ["txt", "DAT", "jpg", "Log", "x"][(r % 5) as usize]
        );
        if r % 11 == 0 {
            recs.push(Rec {
                dir: true,
                attrs: 0x10,
                ..rec(number, vec![nm(parent, &name, WIN32)])
            });
            dirs.push(number);
            continue;
        }
        let mut names = vec![nm(parent, &name, WIN32)];
        if r % 7 == 0 {
            names.push(nm(
                dirs[(next() % dirs.len() as u64) as usize],
                &format!("link-{number}"),
                POSIX,
            ));
        }
        let mut streams = vec![stream((next() % 2_000_000) as i64)];
        if r % 13 == 0 {
            streams.push(named(26, "Zone.Identifier"));
        }
        recs.push(Rec {
            streams,
            ..rec(number, names)
        });
    }
    recs
}
