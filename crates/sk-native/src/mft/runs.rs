//! Locating and reading the $MFT through its own data runs, over any positioned reader.
//!
//! Used with a raw volume handle on Windows, and with an image file in tests (any platform).

use std::io;
use std::sync::Mutex;

use super::record::{AT_DATA, AT_END, FILE_SIG, REF_MASK, fixup, u16_at, u32_at, u64_at};

const AT_ATTRIBUTE_LIST: u32 = 0x20;
pub const PIECE: usize = 16 << 20; // bytes per read request
const MAX_CLUSTER: usize = 2 << 20; // NTFS allows up to 2 MiB clusters
const MAX_LIST: u64 = 64 << 20; // an $ATTRIBUTE_LIST larger than this is corrupt
const ALIGN: usize = 4096; // buffer alignment for unbuffered (sector-aligned) I/O

pub trait ReadAt: Sync {
    /// Fill `buf` from byte offset `off`; offsets and lengths are multiples of the sector size.
    fn read_exact_at(&self, buf: &mut [u8], off: u64) -> io::Result<()>;
}

impl ReadAt for std::fs::File {
    fn read_exact_at(&self, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
        while !buf.is_empty() {
            #[cfg(windows)]
            let got = std::os::windows::fs::FileExt::seek_read(self, buf, off)?;
            #[cfg(unix)]
            let got = std::os::unix::fs::FileExt::read_at(self, buf, off)?;
            if got == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read"));
            }
            buf = &mut buf[got..];
            off += got as u64;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub vcn: u64,
    pub lcn: Option<u64>, // None: sparse
    pub len: u64,         // clusters
}

#[derive(Clone, Debug)]
pub struct Geometry {
    pub sector: usize,
    pub cluster: usize,
    pub record: usize,
    pub mft_lcn: u64,
    pub volume_size: u64, // bytes, including the backup boot sector
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub data_size: u64,
    pub valid_size: u64,
    pub runs: Vec<Run>,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("$MFT: {msg}"))
}

/// Decode NTFS mapping pairs starting at `vcn`.
pub fn decode_runs(mp: &[u8], mut vcn: u64) -> Option<Vec<Run>> {
    let mut runs = Vec::new();
    let mut lcn: i64 = 0;
    let mut p = 0;
    while p < mp.len() && mp[p] != 0 {
        let h = mp[p];
        let (ls, os) = ((h & 0x0F) as usize, (h >> 4) as usize);
        p += 1;
        if ls == 0 || ls > 8 || os > 8 || p + ls + os > mp.len() {
            return None;
        }
        let mut len = 0u64;
        for k in 0..ls {
            len |= (mp[p + k] as u64) << (8 * k);
        }
        p += ls;
        let at = if os == 0 {
            None
        } else {
            let mut delta = 0i64;
            for k in 0..os {
                delta |= (mp[p + k] as i64) << (8 * k);
            }
            if os < 8 {
                delta = (delta << (64 - 8 * os)) >> (64 - 8 * os); // sign-extend
            }
            p += os;
            lcn = lcn.checked_add(delta)?;
            Some(u64::try_from(lcn).ok()?)
        };
        runs.push(Run { vcn, lcn: at, len });
        vcn = vcn.checked_add(len)?;
    }
    Some(runs)
}

struct DataExtent {
    lowest_vcn: u64,
    sizes: Option<(u64, u64, u64)>, // (allocated, data, initialised) on the first extent
    runs: Vec<Run>,
}

/// Unnamed non-resident $DATA extents and the resident $ATTRIBUTE_LIST value of one record.
fn record_extents(rec: &[u8]) -> io::Result<(Vec<DataExtent>, Option<AttrList>)> {
    let used = (u32_at(rec, 0x18) as usize).min(rec.len());
    let mut off = u16_at(rec, 0x14) as usize;
    let mut out = Vec::new();
    let mut list = None;
    while off + 16 <= used {
        let typ = u32_at(rec, off);
        if typ == AT_END {
            break;
        }
        let alen = u32_at(rec, off + 4) as usize;
        if alen < 0x18 || off + alen > used {
            return Err(bad("malformed attribute"));
        }
        let a = &rec[off..off + alen];
        let nonres = a[8] != 0;
        if typ == AT_DATA && a[9] == 0 && nonres && alen >= 0x40 {
            let lowest = u64_at(a, 0x10);
            let mp = u16_at(a, 0x20) as usize;
            let runs = decode_runs(a.get(mp..).ok_or_else(|| bad("bad mapping pairs"))?, lowest)
                .ok_or_else(|| bad("bad mapping pairs"))?;
            let sizes = (lowest == 0).then(|| (u64_at(a, 0x28), u64_at(a, 0x30), u64_at(a, 0x38)));
            out.push(DataExtent {
                lowest_vcn: lowest,
                sizes,
                runs,
            });
        } else if typ == AT_ATTRIBUTE_LIST {
            if nonres && alen >= 0x40 {
                let mp = u16_at(a, 0x20) as usize;
                let runs = decode_runs(a.get(mp..).ok_or_else(|| bad("bad list runs"))?, 0)
                    .ok_or_else(|| bad("bad list runs"))?;
                list = Some(AttrList::NonResident {
                    size: u64_at(a, 0x30),
                    runs,
                });
            } else if !nonres {
                let vlen = u32_at(a, 0x10) as usize;
                let voff = u16_at(a, 0x14) as usize;
                list = a
                    .get(voff..voff + vlen)
                    .map(|v| AttrList::Resident(v.to_vec()));
            }
        }
        off += alen;
    }
    Ok((out, list))
}

enum AttrList {
    Resident(Vec<u8>),
    NonResident { size: u64, runs: Vec<Run> },
}

/// A buffer whose usable part starts on an `ALIGN` boundary.
struct Aligned {
    raw: Vec<u8>,
    start: usize,
    len: usize,
}

impl Aligned {
    fn new(len: usize) -> Self {
        let raw = vec![0u8; len + ALIGN];
        let start = raw.as_ptr().align_offset(ALIGN);
        Aligned { raw, start, len }
    }
    fn get(&mut self, len: usize) -> &mut [u8] {
        debug_assert!(len <= self.len);
        &mut self.raw[self.start..self.start + len]
    }
}

pub fn round_up(v: usize, to: usize) -> usize {
    v.div_ceil(to) * to
}

/// Runs must stay inside the volume (corrupt mapping pairs could point anywhere, or overflow).
fn check_runs(runs: &[Run], geo: &Geometry) -> io::Result<()> {
    let cluster = geo.cluster as u64;
    for r in runs {
        let vcn_end = r
            .vcn
            .checked_add(r.len)
            .and_then(|v| v.checked_mul(cluster));
        let lcn_ok = match r.lcn {
            Some(l) => l
                .checked_add(r.len)
                .and_then(|e| e.checked_mul(cluster))
                .is_some_and(|e| e <= geo.volume_size),
            None => true,
        };
        if vcn_end.is_none() || !lcn_ok {
            return Err(bad("data runs lie outside the volume"));
        }
    }
    Ok(())
}

/// Read `len` bytes at a non-aligned volume position through an aligned bounce buffer.
fn read_unaligned<R: ReadAt>(dev: &R, geo: &Geometry, pos: u64, len: usize) -> io::Result<Vec<u8>> {
    let sector = geo.sector as u64;
    let start = pos / sector * sector;
    let skip = (pos - start) as usize;
    let total = round_up(skip + len, geo.sector);
    let mut buf = Aligned::new(total);
    dev.read_exact_at(buf.get(total), start)?;
    Ok(buf.get(total)[skip..skip + len].to_vec())
}

/// Read `len` bytes at offset `pos` of a stream described by `runs` (may span several runs).
fn read_stream<R: ReadAt>(
    dev: &R,
    geo: &Geometry,
    runs: &[Run],
    mut pos: u64,
    len: usize,
) -> io::Result<Vec<u8>> {
    let cluster = geo.cluster as u64;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let vcn = pos / cluster;
        let run = runs
            .iter()
            .find(|r| vcn >= r.vcn && vcn < r.vcn + r.len)
            .ok_or_else(|| bad("stream offset not covered by the known runs"))?;
        let avail = ((run.vcn + run.len) * cluster - pos) as usize;
        let take = avail.min(len - out.len());
        match run.lcn {
            Some(lcn) => {
                let at = (lcn + vcn - run.vcn) * cluster + pos % cluster;
                out.extend(read_unaligned(dev, geo, at, take)?);
            }
            None => out.resize(out.len() + take, 0),
        }
        pos += take as u64;
    }
    Ok(out)
}

fn read_record<R: ReadAt>(
    dev: &R,
    geo: &Geometry,
    runs: &[Run],
    number: u64,
) -> io::Result<Vec<u8>> {
    let raw = read_stream(dev, geo, runs, number * geo.record as u64, geo.record)?;
    let mut rec = Vec::new();
    if !fixup(&raw, &mut rec) {
        return Err(bad(&format!(
            "record {number} fails its update sequence check"
        )));
    }
    Ok(rec)
}

/// Locate the $MFT: sizes and the complete run list (following its attribute list if any).
pub fn layout<R: ReadAt>(dev: &R, geo: &Geometry) -> io::Result<Layout> {
    let rec0_pos = geo
        .mft_lcn
        .checked_mul(geo.cluster as u64)
        .filter(|p| {
            p.checked_add(geo.record as u64)
                .is_some_and(|e| e <= geo.volume_size)
        })
        .ok_or_else(|| bad("record 0 lies outside the volume"))?;
    let raw = read_unaligned(dev, geo, rec0_pos, geo.record)?;
    if u32_at(&raw, 0) != FILE_SIG {
        return Err(bad("record 0 is not a FILE record"));
    }
    let mut rec0 = Vec::new();
    if !fixup(&raw, &mut rec0) {
        return Err(bad("record 0 fails its update sequence check"));
    }
    let (extents, list) = record_extents(&rec0)?;
    let first = extents
        .iter()
        .find(|e| e.lowest_vcn == 0)
        .ok_or_else(|| bad("no $DATA in record 0"))?;
    let (alloc, data_size, valid_size) = first.sizes.unwrap_or_default();
    let cluster = geo.cluster as u64;
    if alloc > geo.volume_size {
        return Err(bad("inconsistent sizes"));
    }
    let clusters = alloc.div_ceil(cluster);
    let mut runs: Vec<Run> = extents
        .iter()
        .flat_map(|e| e.runs.iter().copied())
        .collect();
    check_runs(&runs, geo)?;
    let covered = |runs: &[Run]| runs.iter().map(|r| r.len).sum::<u64>() >= clusters;
    if !covered(&runs) {
        // fragmented enough to need an attribute list: the other extents live in records
        // that the runs found so far already cover
        let list = match list.ok_or_else(|| bad("runs incomplete and no attribute list"))? {
            AttrList::Resident(v) => v,
            AttrList::NonResident { size, runs: lr } => {
                check_runs(&lr, geo)?;
                if size > MAX_LIST {
                    return Err(bad("attribute list too large"));
                }
                read_stream(dev, geo, &lr, 0, size as usize)?
            }
        };
        let mut p = 0;
        let mut seen = Vec::new();
        while p + 0x1A <= list.len() {
            let typ = u32_at(&list, p);
            let len = u16_at(&list, p + 4) as usize;
            if len < 0x1A {
                break;
            }
            let holder = u64_at(&list, p + 0x10) & REF_MASK;
            if typ == AT_DATA && list[p + 6] == 0 && holder != 0 && !seen.contains(&holder) {
                seen.push(holder);
                let rec = read_record(dev, geo, &runs, holder)?;
                let (more, _) = record_extents(&rec)?;
                let more: Vec<Run> = more.iter().flat_map(|e| e.runs.iter().copied()).collect();
                check_runs(&more, geo)?;
                runs.extend(more);
            }
            p += len;
        }
    }
    runs.sort_by_key(|r| r.vcn);
    runs.dedup();
    let mut next = 0u64;
    for r in &runs {
        if r.vcn != next {
            return Err(bad("data runs have a gap or overlap"));
        }
        next = r.vcn + r.len; // no overflow: check_runs
    }
    if next < clusters {
        return Err(bad("data runs do not cover the allocation"));
    }
    if data_size > alloc || valid_size > data_size || data_size % geo.record as u64 != 0 {
        return Err(bad("inconsistent sizes"));
    }
    Ok(Layout {
        data_size,
        valid_size,
        runs,
    })
}

/// Read the whole $MFT `$DATA` into `out` (length `data_size`); bytes past the valid data
/// length are zero. Requests of up to 16 MiB are spread over the given readers (one thread each).
pub fn read_into<R: ReadAt>(
    devs: &[R],
    geo: &Geometry,
    lay: &Layout,
    out: &mut [u8],
) -> io::Result<()> {
    assert_eq!(out.len() as u64, lay.data_size);
    let cluster = geo.cluster as u64;
    let valid = round_up(lay.valid_size as usize, geo.cluster).min(out.len());
    let (head, tail) = out.split_at_mut(valid);
    tail.fill(0);
    // split the valid part into (destination, volume offset) pieces
    let mut pieces: Vec<(&mut [u8], Option<u64>)> = Vec::new();
    let mut rest = head;
    for r in &lay.runs {
        let mut vcn = r.vcn;
        while vcn < r.vcn + r.len && !rest.is_empty() {
            let take = (((r.vcn + r.len - vcn) * cluster) as usize)
                .min(PIECE)
                .min(rest.len());
            let (dst, tail) = rest.split_at_mut(take);
            rest = tail;
            pieces.push((dst, r.lcn.map(|l| (l + vcn - r.vcn) * cluster)));
            vcn += take.div_ceil(geo.cluster) as u64;
        }
    }
    rest.fill(0); // layout() checked the runs cover the data: normally empty
    let queue = Mutex::new(pieces.into_iter());
    let failure: Mutex<Option<io::Error>> = Mutex::new(None);
    std::thread::scope(|s| {
        for dev in devs {
            s.spawn(|| {
                let mut bounce = Aligned::new(PIECE);
                loop {
                    let Some((dst, at)) = queue.lock().unwrap().next() else {
                        break;
                    };
                    if failure.lock().unwrap().is_some() {
                        break;
                    }
                    let Some(at) = at else {
                        dst.fill(0);
                        continue;
                    };
                    let want = round_up(dst.len(), geo.sector);
                    let buf = bounce.get(want);
                    if let Err(e) = dev.read_exact_at(buf, at) {
                        failure.lock().unwrap().get_or_insert(e);
                        break;
                    }
                    let n = dst.len();
                    dst.copy_from_slice(&buf[..n]);
                }
            });
        }
    });
    if let Some(e) = failure.into_inner().unwrap() {
        return Err(e);
    }
    out[lay.valid_size as usize..].fill(0); // whole clusters were read; the rest of the last is stale
    Ok(())
}

/// Read bytes `[start, start + out.len())` of the $MFT `$DATA` into `out` (one record fetched
/// again after the streaming scan); bytes past the valid data length read as zero. Same pieces
/// and readers as [`read_into`].
pub fn read_range<R: ReadAt>(
    devs: &[R],
    geo: &Geometry,
    lay: &Layout,
    start: u64,
    out: &mut [u8],
) -> io::Result<()> {
    let pieces = plan_window(geo, lay, start, out.len(), PIECE);
    let mut parts: Vec<(&mut [u8], Option<u64>, usize)> = Vec::with_capacity(pieces.len());
    let mut rest: &mut [u8] = out;
    for p in &pieces {
        let (dst, tail) = std::mem::take(&mut rest).split_at_mut(p.len); // pieces are back to back
        rest = tail;
        parts.push((dst, p.at, p.skip));
    }
    read_pieces(devs, geo, parts)?;
    zero_tail(lay, start, out, &pieces);
    Ok(())
}

/// One read of a window of the $MFT: `len` bytes for window offset `dst`, from volume offset
/// `at` (None: a sparse run, zeros); clusters are read whole, so the first `skip` bytes read are
/// not part of the window (a window that starts inside a cluster).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub dst: usize,
    pub len: usize,
    pub at: Option<u64>,
    pub skip: usize,
}

/// The reads that fill bytes `[start, start + len)` of the $MFT `$DATA` (the pieces of
/// [`read_range`]): at most `max_piece` bytes each (counting `skip`), in window order. Bytes the
/// pieces do not cover (past the valid data length or the runs) and the stale rest of the last
/// valid cluster are what [`zero_tail`] clears.
pub fn plan_window(
    geo: &Geometry,
    lay: &Layout,
    start: u64,
    len: usize,
    max_piece: usize,
) -> Vec<Piece> {
    let cluster = geo.cluster as u64;
    let end = start + len as u64;
    let valid_end = (round_up(lay.valid_size as usize, geo.cluster) as u64).min(end);
    let mut pieces = Vec::new();
    let mut pos = start;
    for r in &lay.runs {
        let hi = (r.vcn + r.len) * cluster;
        if hi <= pos || pos >= valid_end {
            continue;
        }
        while pos < hi && pos < valid_end {
            let first_cluster = pos / cluster;
            let skip = (pos - first_cluster * cluster) as usize;
            let take = ((hi.min(valid_end) - pos) as usize)
                .min(max_piece.saturating_sub(skip).max(geo.cluster))
                .min((end - pos) as usize);
            pieces.push(Piece {
                dst: (pos - start) as usize,
                len: take,
                at: r.lcn.map(|l| (l + first_cluster - r.vcn) * cluster),
                skip,
            });
            pos += take as u64;
        }
    }
    pieces
}

/// After a window's pieces are in: zeros where no piece wrote (past the valid data length or the
/// runs; reads overshoot to whole sectors there) and over the stale rest of the last valid cluster.
pub fn zero_tail(lay: &Layout, start: u64, window: &mut [u8], pieces: &[Piece]) {
    let covered = pieces.last().map_or(0, |p| p.dst + p.len);
    let len = window.len();
    window[covered.min(len)..].fill(0);
    let end = start + window.len() as u64;
    if end > lay.valid_size {
        let from = (lay.valid_size.saturating_sub(start) as usize).min(window.len());
        window[from..].fill(0);
    }
}

/// Read `(destination, volume offset, skip)` pieces with one thread per reader.
fn read_pieces<R: ReadAt>(
    devs: &[R],
    geo: &Geometry,
    pieces: Vec<(&mut [u8], Option<u64>, usize)>,
) -> io::Result<()> {
    let queue = Mutex::new(pieces.into_iter());
    let failure: Mutex<Option<io::Error>> = Mutex::new(None);
    std::thread::scope(|s| {
        for dev in devs {
            s.spawn(|| {
                let mut bounce: Option<Aligned> = None; // as large as the largest piece needs
                loop {
                    let Some((dst, at, skip)) = queue.lock().unwrap().next() else {
                        break;
                    };
                    if failure.lock().unwrap().is_some() {
                        break;
                    }
                    let Some(at) = at else {
                        dst.fill(0);
                        continue;
                    };
                    let want = round_up(skip + dst.len(), geo.sector);
                    if bounce.as_ref().is_none_or(|b| b.len < want) {
                        bounce = Some(Aligned::new(want));
                    }
                    let buf = bounce.as_mut().unwrap().get(want);
                    if let Err(e) = dev.read_exact_at(buf, at) {
                        failure.lock().unwrap().get_or_insert(e);
                        break;
                    }
                    let n = dst.len();
                    dst.copy_from_slice(&buf[skip..skip + n]);
                }
            });
        }
    });
    match failure.into_inner().unwrap() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Geometry from an NTFS boot sector (None if it is not one).
pub fn boot_geometry(boot: &[u8]) -> Option<Geometry> {
    if boot.len() < 512 || &boot[3..11] != b"NTFS    " {
        return None;
    }
    let sector = u16_at(boot, 0x0B) as usize;
    let spc = boot[0x0D];
    // more than 128 sectors per cluster is stored as a negative power of two
    let per_cluster = if spc > 0x80 {
        1usize.checked_shl(256 - spc as u32)?
    } else {
        spc as usize
    };
    let cluster = sector.checked_mul(per_cluster)?;
    let raw = boot[0x40] as i8;
    let record = if raw < 0 {
        1usize.checked_shl(-(raw as i32) as u32)?
    } else {
        (raw as usize).checked_mul(cluster)?
    };
    let volume_size = u64_at(boot, 0x28)
        .checked_add(1)?
        .checked_mul(sector as u64)?;
    let ok = sector >= 512
        && sector.is_power_of_two()
        && cluster >= sector
        && cluster <= MAX_CLUSTER
        && cluster.is_power_of_two()
        && (512..=65536).contains(&record)
        && record.is_multiple_of(512);
    ok.then_some(Geometry {
        sector,
        cluster,
        record,
        mft_lcn: u64_at(boot, 0x30),
        volume_size,
    })
}

/// Geometry and $MFT layout of the NTFS volume (or image) behind `dev`.
pub fn open_layout<R: ReadAt>(dev: &R) -> io::Result<(Geometry, Layout)> {
    let mut boot = Aligned::new(ALIGN);
    dev.read_exact_at(boot.get(ALIGN), 0)?;
    let geo = boot_geometry(boot.get(ALIGN))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not an NTFS volume"))?;
    let lay = layout(dev, &geo)?;
    Ok((geo, lay))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_pairs() {
        // 0x21: len 1 byte, off 2 bytes ; 0x01: sparse ; 0x31 with a negative delta
        let mp = [
            0x21, 0x10, 0x00, 0x01, 0x01, 0x05, 0x31, 0x02, 0xFF, 0xFF, 0xFF, 0x00,
        ];
        let runs = decode_runs(&mp, 0).unwrap();
        assert_eq!(
            runs,
            vec![
                Run {
                    vcn: 0,
                    lcn: Some(0x100),
                    len: 0x10
                },
                Run {
                    vcn: 0x10,
                    lcn: None,
                    len: 5
                },
                Run {
                    vcn: 0x15,
                    lcn: Some(0xFF),
                    len: 2
                },
            ]
        );
        assert!(decode_runs(&[0x21, 0x10], 0).is_none());
    }
}
