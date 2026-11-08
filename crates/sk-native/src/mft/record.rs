//! One FILE record: header classification, update-sequence fixups and the attribute walk.
//!
//! The rules here are mirrored exactly by the numpy fallback (`spaceknife/scan/mft.py`); the
//! tests compare both column by column, so any change must be made on both sides.

pub const SECTOR: usize = 512; // update-sequence stride (NTFS_BLOCK_SIZE), independent of the disk
pub const FILE_SIG: u32 = u32::from_le_bytes(*b"FILE");
pub const REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

pub const AT_STANDARD_INFORMATION: u32 = 0x10;
pub const AT_ATTRIBUTE_LIST: u32 = 0x20;
pub const AT_FILE_NAME: u32 = 0x30;
pub const AT_DATA: u32 = 0x80;
pub const AT_REPARSE_POINT: u32 = 0xC0;
pub const AT_END: u32 = 0xFFFF_FFFF;

const ATTR_COMPRESSED_OR_SPARSE: u16 = 0x0001 | 0x8000;
pub const NS_DOS: u8 = 2;

#[inline(always)]
pub fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

#[inline(always)]
pub fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

#[inline(always)]
pub fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

#[inline(always)]
pub fn i64_at(b: &[u8], o: usize) -> i64 {
    i64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// What a raw record is, judged from its header (sector 0 is never touched by fixups).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Unused,   // all-zero signature, or a FILE record not in use
    Bad,      // any other signature ("BAAD", garbage)
    Base,     // in-use base record
    Ext(u64), // in-use extension record: full reference (seq << 48 | record) of its base
}

#[inline]
pub fn classify(raw: &[u8]) -> Class {
    let sig = u32_at(raw, 0);
    if sig == 0 {
        return Class::Unused;
    }
    if sig != FILE_SIG {
        return Class::Bad;
    }
    if u16_at(raw, 0x16) & 1 == 0 {
        return Class::Unused;
    }
    match u64_at(raw, 0x20) {
        0 => Class::Base,
        base => Class::Ext(base),
    }
}

/// Check the update sequence of every sector, then copy the record's used part into `buf` with
/// the sequence undone; false if the record is torn or its update sequence array is malformed.
/// (`buf` is shorter than the record when the used size is: the attribute walk stops there.)
pub fn fixup(raw: &[u8], buf: &mut Vec<u8>) -> bool {
    let sectors = raw.len() / SECTOR;
    let usa_ofs = u16_at(raw, 4) as usize;
    let usa_cnt = u16_at(raw, 6) as usize;
    if usa_cnt != sectors + 1 || usa_ofs + 2 * usa_cnt > SECTOR - 2 {
        return false;
    }
    let (u0, u1) = (raw[usa_ofs], raw[usa_ofs + 1]);
    for i in 0..sectors {
        let p = i * SECTOR + SECTOR - 2;
        if raw[p] != u0 || raw[p + 1] != u1 {
            return false;
        }
    }
    let len = (u32_at(raw, 0x18) as usize).clamp(0x30, raw.len());
    buf.clear();
    buf.extend_from_slice(&raw[..len]);
    for i in 0..sectors {
        let p = i * SECTOR + SECTOR - 2;
        let s = usa_ofs + 2 + 2 * i;
        if p < len {
            buf[p] = raw[s];
        }
        if p + 1 < len {
            buf[p + 1] = raw[s + 1];
        }
    }
    true
}

#[derive(Clone, Copy, Default)]
pub struct Si {
    pub crtime: u64,
    pub mtime: u64,
    pub atime: u64,
    pub attrs: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Reparse {
    #[default]
    None,
    Tag(u32),
    Unknown, // non-resident or truncated: the tag is not in the MFT
}

#[derive(Clone, Copy)]
pub struct Name {
    pub parent: u64, // full reference (seq << 48 | record)
    pub ns: u8,
    pub start: u32, // in Acc::name_buf (UTF-8)
    pub len: u32,
}

/// Everything collected from one file's attributes (base record, then its extension records).
#[derive(Default)]
pub struct Acc {
    pub si: Option<Si>,
    pub data: Option<(i64, i64)>, // unnamed $DATA (size, alloc), first extent only
    pub ads_size: i64,
    pub ads_alloc: i64,
    pub has_ads: bool,
    pub reparse: Reparse,
    pub names: Vec<Name>,
    pub name_buf: Vec<u8>,
    pub has_list: bool, // the record has an $ATTRIBUTE_LIST: extension records may add to it
}

impl Acc {
    pub fn reset(&mut self) {
        self.si = None;
        self.data = None;
        self.ads_size = 0;
        self.ads_alloc = 0;
        self.has_ads = false;
        self.reparse = Reparse::None;
        self.names.clear();
        self.name_buf.clear();
        self.has_list = false;
    }

    /// Fold an extension record in (first occurrence wins, names append, streams add up).
    pub fn merge(&mut self, ext: &Acc) {
        if self.si.is_none() {
            self.si = ext.si;
        }
        if self.data.is_none() {
            self.data = ext.data;
        }
        self.ads_size = self.ads_size.wrapping_add(ext.ads_size);
        self.ads_alloc = self.ads_alloc.wrapping_add(ext.ads_alloc);
        self.has_ads |= ext.has_ads;
        if self.reparse == Reparse::None {
            self.reparse = ext.reparse;
        }
        let shift = self.name_buf.len() as u32;
        self.name_buf.extend_from_slice(&ext.name_buf);
        self.names.extend(ext.names.iter().map(|n| Name {
            start: n.start + shift,
            ..*n
        }));
    }

    fn data_extent(&mut self, named: bool, size: i64, alloc: i64) {
        if named {
            self.ads_size = self.ads_size.wrapping_add(size);
            self.ads_alloc = self.ads_alloc.wrapping_add(alloc);
            self.has_ads = true;
        } else if self.data.is_none() {
            self.data = Some((size, alloc));
        }
    }
}

/// Walk the attributes of a fixed-up record into `acc`; false if the record is malformed.
pub fn walk(rec: &[u8], acc: &mut Acc) -> bool {
    let usa_end = u16_at(rec, 4) as usize + 2 * u16_at(rec, 6) as usize;
    let used = u32_at(rec, 0x18) as usize;
    let mut off = u16_at(rec, 0x14) as usize;
    if used > rec.len() || off < usa_end {
        return false;
    }
    loop {
        if off + 4 > used {
            return false;
        }
        let typ = u32_at(rec, off);
        if typ == AT_END {
            return true;
        }
        if off + 16 > used {
            return false;
        }
        let alen = u32_at(rec, off + 4) as usize;
        let nonres = rec[off + 8] != 0;
        if alen < if nonres { 0x40 } else { 0x18 } || off + alen > used {
            return false;
        }
        let a = &rec[off..off + alen];
        if typ == AT_ATTRIBUTE_LIST {
            acc.has_list = true;
        }
        let nlen = a[9] as usize;
        if nlen > 0 && u16_at(a, 10) as usize + 2 * nlen > alen {
            return false;
        }
        if nonres {
            match typ {
                AT_DATA if u64_at(a, 0x10) == 0 => {
                    let alloc = if u16_at(a, 0x0C) & ATTR_COMPRESSED_OR_SPARSE != 0 && alen >= 0x48
                    {
                        i64_at(a, 0x40)
                    } else {
                        i64_at(a, 0x28)
                    };
                    acc.data_extent(nlen > 0, i64_at(a, 0x30), alloc);
                }
                AT_REPARSE_POINT if acc.reparse == Reparse::None => acc.reparse = Reparse::Unknown,
                _ => {}
            }
        } else {
            let vlen = u32_at(a, 0x10) as usize;
            let voff = u16_at(a, 0x14) as usize;
            if voff + vlen > alen {
                return false;
            }
            let v = &a[voff..voff + vlen];
            match typ {
                AT_STANDARD_INFORMATION if acc.si.is_none() && vlen >= 0x24 => {
                    acc.si = Some(Si {
                        crtime: u64_at(v, 0),
                        mtime: u64_at(v, 0x08),
                        atime: u64_at(v, 0x18),
                        attrs: u32_at(v, 0x20),
                    });
                }
                AT_FILE_NAME if vlen >= 0x42 => {
                    let chars = v[0x40] as usize;
                    if 0x42 + 2 * chars <= vlen {
                        let start = acc.name_buf.len();
                        push_utf16le(&v[0x42..0x42 + 2 * chars], &mut acc.name_buf);
                        acc.names.push(Name {
                            parent: u64_at(v, 0),
                            ns: v[0x41],
                            start: start as u32,
                            len: (acc.name_buf.len() - start) as u32,
                        });
                    }
                }
                AT_DATA => acc.data_extent(nlen > 0, vlen as i64, 0),
                AT_REPARSE_POINT if acc.reparse == Reparse::None => {
                    acc.reparse = if vlen >= 4 {
                        Reparse::Tag(u32_at(v, 0))
                    } else {
                        Reparse::Unknown
                    };
                }
                _ => {}
            }
        }
        off += alen;
    }
}

/// UTF-16LE -> UTF-8; unpaired surrogates become 3-byte sequences (Python's "surrogatepass").
pub fn push_utf16le(src: &[u8], out: &mut Vec<u8>) {
    let n = src.len() / 2;
    let unit = |i: usize| u16::from_le_bytes([src[2 * i], src[2 * i + 1]]) as u32;
    let mut i = 0;
    while i < n {
        let u = unit(i);
        i += 1;
        if u < 0x80 {
            out.push(u as u8);
        } else if u < 0x800 {
            out.extend_from_slice(&[0xC0 | (u >> 6) as u8, 0x80 | (u & 0x3F) as u8]);
        } else {
            if (0xD800..0xDC00).contains(&u) && i < n {
                let low = unit(i);
                if (0xDC00..0xE000).contains(&low) {
                    let c = 0x10000 + ((u - 0xD800) << 10) + (low - 0xDC00);
                    out.extend_from_slice(&[
                        0xF0 | (c >> 18) as u8,
                        0x80 | ((c >> 12) & 0x3F) as u8,
                        0x80 | ((c >> 6) & 0x3F) as u8,
                        0x80 | (c & 0x3F) as u8,
                    ]);
                    i += 1;
                    continue;
                }
            }
            out.extend_from_slice(&[
                0xE0 | (u >> 12) as u8,
                0x80 | ((u >> 6) & 0x3F) as u8,
                0x80 | (u & 0x3F) as u8,
            ]);
        }
    }
}

/// FILETIME (100 ns ticks since 1601) -> Unix seconds; 0 stays 0 ("unknown").
#[inline]
pub fn unix_secs(ft: u64) -> i64 {
    if ft == 0 {
        0
    } else {
        (ft / 10_000_000) as i64 - 11_644_473_600
    }
}

// spaceknife.store.nodes.Flag bits
pub const F_COMPRESSED: u32 = 1 << 3;
pub const F_SPARSE: u32 = 1 << 4;
pub const F_HIDDEN: u32 = 1 << 5;
pub const F_SYSTEM: u32 = 1 << 6;
pub const F_READONLY: u32 = 1 << 7;
pub const F_REPARSE: u32 = 1 << 8;
pub const F_CLOUD: u32 = 1 << 9;
pub const F_ARCHIVE: u32 = 1 << 12;
pub const F_ENCRYPTED: u32 = 1 << 13;
pub const F_TEMPORARY: u32 = 1 << 14;
pub const F_NOT_INDEXED: u32 = 1 << 15;
pub const F_OFFLINE: u32 = 1 << 16;
pub const F_ADS: u32 = 1 << 18;

const ATTR_FLAGS: [(u32, u32); 13] = [
    (0x1, F_READONLY),
    (0x2, F_HIDDEN),
    (0x4, F_SYSTEM),
    (0x20, F_ARCHIVE),
    (0x100, F_TEMPORARY),
    (0x200, F_SPARSE),
    (0x400, F_REPARSE),
    (0x800, F_COMPRESSED),
    (0x1000, F_OFFLINE),
    (0x2000, F_NOT_INDEXED),
    (0x4000, F_ENCRYPTED),
    (0x40000, F_CLOUD),  // RECALL_ON_OPEN
    (0x400000, F_CLOUD), // RECALL_ON_DATA_ACCESS
];

/// Windows file attributes -> store flag bits (same table as scan/worker.py).
#[inline]
pub fn attr_flags(attrs: u32) -> u32 {
    ATTR_FLAGS
        .iter()
        .filter(|(bit, _)| attrs & bit != 0)
        .fold(0, |acc, (_, f)| acc | f)
}

pub const REPARSE_NAME_SURROGATE: u32 = 0x2000_0000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_surrogatepass() {
        let units: [u16; 9] = [
            0xD800, 0x0000, 0xDC00, 0x41, 0xD83D, 0xDE00, 0xD800, 0xD800, 0xDC00,
        ];
        let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
        let mut out = Vec::new();
        push_utf16le(&bytes, &mut out);
        let expected: &[u8] =
            b"\xed\xa0\x80\x00\xed\xb0\x80A\xf0\x9f\x98\x80\xed\xa0\x80\xf0\x90\x80\x80";
        assert_eq!(out, expected);
    }

    #[test]
    fn filetime() {
        assert_eq!(unix_secs(0), 0);
        assert_eq!(unix_secs(116_444_736_000_000_000), 0);
        assert_eq!(unix_secs(116_444_736_000_000_000 + 15_000_000), 1);
    }

    #[test]
    fn attribute_flags() {
        assert_eq!(
            attr_flags(0x1 | 0x2 | 0x400000),
            F_READONLY | F_HIDDEN | F_CLOUD
        );
        assert_eq!(attr_flags(0x10), 0);
    }
}
