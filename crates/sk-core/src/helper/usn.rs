//! NTFS change journal (USN) for the helper's `query_usn` / `read_usn` (Python `helper/usn.py`,
//! docs/monitor.md 3).
//!
//! The backend keeps a checkpoint `(journal_id, next_usn)` per scanned volume. When a watcher
//! loses events it asks the helper for the records written since, and lists again the folders
//! they name (`parent_frn`). Everything here is read-only: the volume is opened with
//! `GENERIC_READ` and only `FSCTL_QUERY_USN_JOURNAL` / `FSCTL_READ_USN_JOURNAL` are issued.
//!
//! [`parse_read_buffer`] parses what `FSCTL_READ_USN_JOURNAL` returns (`USN_RECORD_V2` and `V3`)
//! and runs on every platform. [`read_journal`] drives any [`UsnDevice`] (the Win32 one from
//! [`open_volume`], or a test double) and returns the rows as columns. File references stay
//! 64-bit: V3 records carry 128-bit ids, of which the low 64 bits are the NTFS file reference
//! (record number in the low 48 bits, sequence above); the helper only serves NTFS volumes, where
//! the high half is zero.
//!
//! Every failure is a [`UsnError`] whose `code` is the helper's error code.

use std::time::{Duration, Instant};

use super::messages::{Scalars, code};
use crate::i18n::t;
use crate::tr;

// Win32 errors the journal FSCTLs report (winerror.h)
pub const ERROR_INVALID_FUNCTION: u32 = 1; // not a filesystem with a change journal (FAT, exFAT …)
pub const ERROR_ACCESS_DENIED: u32 = 5;
pub const ERROR_NOT_SUPPORTED: u32 = 50;
pub const ERROR_INVALID_PARAMETER: u32 = 87;
pub const ERROR_JOURNAL_DELETE_IN_PROGRESS: u32 = 1178;
pub const ERROR_JOURNAL_NOT_ACTIVE: u32 = 1179;
pub const ERROR_JOURNAL_ENTRY_DELETED: u32 = 1181; // start USN below the first record still kept
pub const ERROR_PRIVILEGE_NOT_HELD: u32 = 1314;

pub const FSCTL_READ_USN_JOURNAL: u32 = 0x0009_00BB;
pub const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00F4;
pub const ALL_REASONS: u32 = 0xFFFF_FFFF;

pub const MIN_BUFFER: usize = 4 << 10; // holds the largest record (76 + 255 UTF-16 units) several times over
pub const MAX_BUFFER: usize = 1 << 20;
const BYTES_PER_ROW: usize = 128; // buffer sizing for small reads: a typical record is 80–120 bytes
const V2_SIZE: usize = 60;
const V3_SIZE: usize = 76;

/// Output columns of `read_usn`, layout `usn.v1` (plus the `names` blob), in wire order.
pub const COLUMNS: [&str; 7] = ["frn", "parent_frn", "usn", "reason", "attrs", "name_off", "name_len"];

/// A journal operation failed; `code` is the helper's error code (`usn_gone` …).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsnError {
    pub code: &'static str,
    pub message: String,
}

impl UsnError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for UsnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for UsnError {}

/// The helper's error code for a Win32 error of the journal FSCTLs.
pub fn error_code(winerror: u32) -> &'static str {
    match winerror {
        ERROR_JOURNAL_NOT_ACTIVE | ERROR_JOURNAL_DELETE_IN_PROGRESS => code::USN_INACTIVE,
        ERROR_JOURNAL_ENTRY_DELETED => code::USN_GONE,
        ERROR_ACCESS_DENIED | ERROR_PRIVILEGE_NOT_HELD => code::NEEDS_ADMIN,
        ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED => code::NOT_SUPPORTED,
        _ => code::IO_ERROR,
    }
}

/// `USN_JOURNAL_DATA` of a volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Journal {
    /// Changes when the journal is deleted and created again.
    pub journal_id: u64,
    /// Oldest record still kept.
    pub first_usn: i64,
    /// Where the next record will be written.
    pub next_usn: i64,
    /// Records below were written by an earlier journal instance.
    pub lowest_valid_usn: i64,
    pub max_usn: i64,
    /// Bytes the journal may grow to before old records are dropped.
    pub max_size: u64,
    pub allocation_delta: u64,
    /// Newest record version the volume writes (V1 data; 2 when not reported).
    pub max_major: u16,
}

impl Journal {
    pub fn scalars(&self) -> Scalars {
        let mut m = Scalars::new();
        m.insert("journal_id".into(), self.journal_id.into());
        m.insert("first_usn".into(), self.first_usn.into());
        m.insert("next_usn".into(), self.next_usn.into());
        m.insert("lowest_valid_usn".into(), self.lowest_valid_usn.into());
        m.insert("max_usn".into(), self.max_usn.into());
        m.insert("max_size".into(), self.max_size.into());
        m.insert("allocation_delta".into(), self.allocation_delta.into());
        m
    }
}

/// An open volume's change journal (see [`open_volume`]).
pub trait UsnDevice: Send {
    fn query(&mut self) -> Result<Journal, UsnError>;

    /// One `FSCTL_READ_USN_JOURNAL` of at most `size` bytes from `start_usn`: the next USN
    /// (8 bytes) followed by whole records; returns at once when there are none.
    fn read(&mut self, start_usn: i64, journal_id: u64, size: usize) -> Result<Vec<u8>, UsnError>;
}

// --------------------------------------------------------------------------- parsing

/// The rows of a journal read, as columns and a UTF-8 (WTF-8: lone surrogates kept) names blob.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsnRows {
    pub frn: Vec<u64>,
    pub parent_frn: Vec<u64>,
    pub usn: Vec<i64>,
    pub reason: Vec<u32>,
    pub attrs: Vec<u32>,
    pub name_off: Vec<u32>,
    pub name_len: Vec<u16>,
    pub names: Vec<u8>,
}

impl UsnRows {
    pub fn len(&self) -> usize {
        self.usn.len()
    }

    pub fn is_empty(&self) -> bool {
        self.usn.is_empty()
    }

    /// The name of row `i`, as bytes.
    pub fn name(&self, i: usize) -> &[u8] {
        let off = self.name_off[i] as usize;
        &self.names[off..off + self.name_len[i] as usize]
    }

    fn with_capacity(n: usize) -> Self {
        Self {
            frn: Vec::with_capacity(n),
            parent_frn: Vec::with_capacity(n),
            usn: Vec::with_capacity(n),
            reason: Vec::with_capacity(n),
            attrs: Vec::with_capacity(n),
            name_off: Vec::with_capacity(n),
            name_len: Vec::with_capacity(n),
            names: Vec::new(),
        }
    }

    /// Append `other`, rebasing its name offsets.
    pub fn append(&mut self, other: &UsnRows) -> Result<(), String> {
        let base = self.names.len();
        if base + other.names.len() > u32::MAX as usize {
            return Err(t("USN 记录的文件名总长超过 4 GiB").to_owned());
        }
        self.frn.extend_from_slice(&other.frn);
        self.parent_frn.extend_from_slice(&other.parent_frn);
        self.usn.extend_from_slice(&other.usn);
        self.reason.extend_from_slice(&other.reason);
        self.attrs.extend_from_slice(&other.attrs);
        self.name_off.extend(other.name_off.iter().map(|o| o + base as u32));
        self.name_len.extend_from_slice(&other.name_len);
        self.names.extend_from_slice(&other.names);
        Ok(())
    }
}

/// The records of one read buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsnChunk {
    /// Where to continue: the buffer's next USN, or the first record not returned.
    pub next_usn: i64,
    pub rows: UsnRows,
    /// `limit` records returned and more were in the buffer.
    pub truncated: bool,
    /// Records of other versions (V4 range records …), not returned.
    pub skipped: u64,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"))
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"))
}

/// UTF-16LE `units` → WTF-8 appended to `out` (Python's `surrogatepass`: pairs become one
/// character, lone surrogates their 3-byte form).
pub fn utf16_to_wtf8(units: &[u8], out: &mut Vec<u8>) {
    let n = units.len() / 2;
    let unit = |i: usize| u16::from_le_bytes([units[2 * i], units[2 * i + 1]]);
    let mut i = 0;
    while i < n {
        let u = unit(i) as u32;
        i += 1;
        let cp = if (0xD800..0xDC00).contains(&u) && i < n && (0xDC00..0xE000).contains(&(unit(i) as u32)) {
            let lo = unit(i) as u32;
            i += 1;
            0x10000 + ((u - 0xD800) << 10) + (lo - 0xDC00)
        } else {
            u
        };
        match cp {
            0..=0x7F => out.push(cp as u8),
            0x80..=0x7FF => out.extend_from_slice(&[0xC0 | (cp >> 6) as u8, 0x80 | (cp & 0x3F) as u8]),
            0x800..=0xFFFF => out.extend_from_slice(&[
                0xE0 | (cp >> 12) as u8,
                0x80 | ((cp >> 6) & 0x3F) as u8,
                0x80 | (cp & 0x3F) as u8,
            ]),
            _ => out.extend_from_slice(&[
                0xF0 | (cp >> 18) as u8,
                0x80 | ((cp >> 12) & 0x3F) as u8,
                0x80 | ((cp >> 6) & 0x3F) as u8,
                0x80 | (cp & 0x3F) as u8,
            ]),
        }
    }
}

/// Parse the output of `FSCTL_READ_USN_JOURNAL`: a next USN, then V2/V3 records.
///
/// At most `limit` records are returned; `next_usn` is then the USN of the first record left out,
/// so reading again from it continues without a gap. Names are UTF-16 in the records and WTF-8 in
/// the blob. A malformed buffer is an error (the text names what is wrong).
pub fn parse_read_buffer(buf: &[u8], limit: Option<usize>) -> Result<UsnChunk, String> {
    if buf.len() < 8 {
        return Err(tr!("USN 读取结果只有 {size} 字节", size = buf.len()));
    }
    let mut next_usn = u64_at(buf, 0) as i64;
    let end = buf.len();
    let mut pos = 8;
    let mut skipped = 0u64;
    let mut stop = None;
    let mut rows = UsnRows::with_capacity(((end - 8) / 96).min(limit.unwrap_or(usize::MAX)));
    while pos < end {
        if end - pos < 8 {
            return Err(tr!("USN 读取结果末尾有 {size} 字节不完整的记录", size = end - pos));
        }
        let length = u32_at(buf, pos) as usize;
        let major = u16_at(buf, pos + 4);
        let size = match major {
            2 => Some(V2_SIZE),
            3 => Some(V3_SIZE),
            _ => None,
        };
        if length < size.unwrap_or(8) || length > end - pos {
            return Err(tr!(
                "USN 记录长度 {length} 无效（偏移 {offset}，版本 {version}）",
                length = length,
                offset = pos,
                version = major
            ));
        }
        let Some(head) = size else {
            skipped += 1;
            pos += length;
            continue;
        };
        if limit.is_some_and(|l| rows.len() >= l) {
            stop = Some((pos, major));
            break;
        }
        let rec = &buf[pos..pos + length];
        let (frn, parent, usn, reason, attrs, name_len, name_off) = if major == 2 {
            (u64_at(rec, 8), u64_at(rec, 16), u64_at(rec, 24) as i64, u32_at(rec, 40), u32_at(rec, 52), u16_at(rec, 56), u16_at(rec, 58))
        } else {
            (u64_at(rec, 8), u64_at(rec, 24), u64_at(rec, 40) as i64, u32_at(rec, 56), u32_at(rec, 68), u16_at(rec, 72), u16_at(rec, 74))
        };
        let (start, nbytes) = (name_off as usize, name_len as usize);
        if start < head || start + nbytes > length || nbytes % 2 != 0 {
            return Err(tr!("第 {index} 条 USN 记录的文件名越界或长度为奇数", index = rows.len() + 1));
        }
        let before = rows.names.len();
        utf16_to_wtf8(&rec[start..start + nbytes], &mut rows.names);
        let width = rows.names.len() - before;
        if width > 0xFFFF {
            return Err(t("USN 记录的文件名过长").to_owned());
        }
        if rows.names.len() > u32::MAX as usize {
            return Err(t("USN 记录的文件名总长超过 4 GiB").to_owned());
        }
        rows.frn.push(frn);
        rows.parent_frn.push(parent);
        rows.usn.push(usn);
        rows.reason.push(reason);
        rows.attrs.push(attrs);
        rows.name_off.push(before as u32);
        rows.name_len.push(width as u16);
        pos += length;
    }
    if let Some((at, major)) = stop {
        // limit reached: continue at the first record not returned
        next_usn = u64_at(buf, at + if major == 2 { 24 } else { 40 }) as i64;
    }
    Ok(UsnChunk {
        next_usn,
        rows,
        truncated: stop.is_some(),
        skipped,
    })
}

// --------------------------------------------------------------------------- reading

/// What [`read_journal`] returns: the rows and where to read from next time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsnTable {
    pub rows: UsnRows,
    pub journal_id: u64,
    /// The checkpoint to read from next time.
    pub next_usn: i64,
    /// `max_rows` was reached: read again from `next_usn` for the rest.
    pub truncated: bool,
}

impl UsnTable {
    /// The scalar entries sent as the response's extras.
    pub fn extras(&self) -> Scalars {
        let mut m = Scalars::new();
        m.insert("journal_id".into(), self.journal_id.into());
        m.insert("next_usn".into(), self.next_usn.into());
        m.insert("rows".into(), (self.rows.len() as u64).into());
        m.insert("truncated".into(), self.truncated.into());
        m
    }
}

/// Statistics of one [`read_journal`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsnStats {
    pub first_usn: i64,
    pub start_usn: i64,
    pub target_usn: i64,
    pub reads: u64,
    pub bytes_read: u64,
    pub skipped: u64,
}

impl UsnStats {
    pub fn scalars(&self) -> Scalars {
        let mut m = Scalars::new();
        m.insert("first_usn".into(), self.first_usn.into());
        m.insert("start_usn".into(), self.start_usn.into());
        m.insert("target_usn".into(), self.target_usn.into());
        m.insert("reads".into(), self.reads.into());
        m.insert("bytes_read".into(), self.bytes_read.into());
        m.insert("skipped".into(), self.skipped.into());
        m
    }
}

/// Read buffer for at most `max_rows` records: small reads stay small.
pub fn buffer_size(max_rows: usize) -> usize {
    max_rows.saturating_mul(BYTES_PER_ROW).clamp(MIN_BUFFER, MAX_BUFFER)
}

/// `usn_gone` unless the records from `start_usn` on are all still there.
pub fn check_checkpoint(journal: &Journal, start_usn: i64, journal_id: u64) -> Result<(), UsnError> {
    if journal.journal_id != journal_id {
        return Err(UsnError::new(
            code::USN_GONE,
            tr!(
                "USN 日志已重建（当前编号 {current:#x}，检查点属于 {checkpoint:#x}），检查点失效",
                current = journal.journal_id,
                checkpoint = journal_id
            ),
        ));
    }
    if start_usn < journal.first_usn.max(journal.lowest_valid_usn) {
        return Err(UsnError::new(
            code::USN_GONE,
            tr!(
                "检查点 {start} 之后的记录已被覆盖（日志现从 {first} 开始）",
                start = start_usn,
                first = journal.first_usn
            ),
        ));
    }
    if start_usn > journal.next_usn {
        return Err(UsnError::new(
            code::USN_GONE,
            tr!("检查点 {start} 超出日志末尾 {end}", start = start_usn, end = journal.next_usn),
        ));
    }
    Ok(())
}

/// Options of [`read_journal`].
pub struct ReadOptions<'a> {
    pub max_rows: usize,
    /// In (0, 1]: the reader is busy that share of the time (I/O limit).
    pub duty: f64,
    /// `(journal bytes read, bytes to read)` after each read.
    pub progress: Option<&'a mut dyn FnMut(u64, u64)>,
}

impl ReadOptions<'_> {
    pub fn rows(max_rows: usize) -> Self {
        Self {
            max_rows,
            duty: 1.0,
            progress: None,
        }
    }
}

/// Every record from `start_usn` up to the journal's end as it was when the read began
/// (records appended meanwhile come along with the last buffer), at most `max_rows`.
pub fn read_journal(
    device: &mut dyn UsnDevice,
    start_usn: i64,
    journal_id: u64,
    opts: ReadOptions<'_>,
) -> Result<(UsnTable, UsnStats), UsnError> {
    let t0 = Instant::now();
    read_journal_paced(device, start_usn, journal_id, opts, &mut || t0.elapsed(), &mut std::thread::sleep)
}

/// [`read_journal`] with the clock and the sleep passed in (tests).
#[doc(hidden)]
pub fn read_journal_paced(
    device: &mut dyn UsnDevice,
    start_usn: i64,
    journal_id: u64,
    mut opts: ReadOptions<'_>,
    clock: &mut dyn FnMut() -> Duration,
    sleep: &mut dyn FnMut(Duration),
) -> Result<(UsnTable, UsnStats), UsnError> {
    let journal = device.query()?;
    check_checkpoint(&journal, start_usn, journal_id)?;
    let target = journal.next_usn;
    let max_rows = opts.max_rows;
    let size = buffer_size(max_rows);
    let mut out = UsnRows::default();
    let mut stats = UsnStats {
        first_usn: journal.first_usn,
        start_usn,
        target_usn: target,
        ..Default::default()
    };
    let mut pos = start_usn;
    let mut truncated = false;
    while pos < target {
        if out.len() >= max_rows {
            truncated = true; // records between pos and target are left for the next read
            break;
        }
        let started = clock();
        let buf = match device.read(pos, journal_id, size) {
            Ok(b) => b,
            Err(e) => return Err(recheck(device, journal_id, e)),
        };
        let busy = clock().saturating_sub(started);
        stats.reads += 1;
        stats.bytes_read += buf.len() as u64;
        let chunk = parse_read_buffer(&buf, Some(max_rows - out.len()))
            .map_err(|why| UsnError::new(code::IO_ERROR, tr!("USN 日志记录无法解析：{reason}", reason = why)))?;
        if out.is_empty() {
            out = chunk.rows;
        } else {
            out.append(&chunk.rows).map_err(|why| UsnError::new(code::IO_ERROR, why))?;
        }
        stats.skipped += chunk.skipped;
        if chunk.truncated {
            truncated = true;
            pos = chunk.next_usn;
            break;
        }
        if chunk.next_usn <= pos {
            break; // no progress: nothing more to read
        }
        pos = chunk.next_usn;
        if let Some(p) = opts.progress.as_mut() {
            p((pos.min(target) - start_usn) as u64, (target - start_usn) as u64);
        }
        if opts.duty < 1.0 && pos < target {
            sleep(busy.mul_f64((1.0 - opts.duty) / opts.duty));
        }
    }
    let table = UsnTable {
        rows: out,
        journal_id,
        next_usn: pos,
        truncated,
    };
    Ok((table, stats))
}

/// A read failed: if the journal was recreated meanwhile, say so (`usn_gone`).
fn recheck(device: &mut dyn UsnDevice, journal_id: u64, err: UsnError) -> UsnError {
    if err.code != code::IO_ERROR {
        return err;
    }
    match device.query() {
        Ok(j) if j.journal_id != journal_id => {
            UsnError::new(code::USN_GONE, tr!("读取期间 USN 日志被重建（{reason}）", reason = err.message))
        }
        _ => err,
    }
}

/// The change journal of the local volume `X:` (read-only; needs administrator rights).
pub fn open_volume(volume: &str) -> Result<Box<dyn UsnDevice>, UsnError> {
    #[cfg(windows)]
    {
        Ok(Box::new(win::VolumeJournal::open(volume)?))
    }
    #[cfg(not(windows))]
    {
        let _ = volume;
        Err(UsnError::new(code::NOT_SUPPORTED, t("USN 日志只支持 Windows NTFS 卷")))
    }
}

// --------------------------------------------------------------------------- Win32

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::GENERIC_READ;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    use super::*;
    use crate::helper::win32::Handle;

    const JOURNAL_BUFFER: usize = 80;

    fn fail(what: &str, err: std::io::Error) -> UsnError {
        let code = err.raw_os_error().unwrap_or(0) as u32;
        UsnError::new(
            error_code(code),
            tr!("{what}失败：[WinError {code}] {reason}", what = what, code = code, reason = err.to_string()),
        )
    }

    /// `\\.\X:` opened for reading; the journal FSCTLs on it.
    pub struct VolumeJournal {
        volume: String,
        handle: Handle,
        max_major: u16,
    }

    impl VolumeJournal {
        pub fn open(volume: &str) -> Result<Self, UsnError> {
            let volume = volume.to_uppercase();
            let path = format!("\\\\.\\{volume}");
            let Some(handle) = Handle::open(&path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, 0) else {
                return Err(fail(&tr!("打开卷 {volume}", volume = &volume), std::io::Error::last_os_error()));
            };
            Ok(Self {
                volume,
                handle,
                max_major: 2,
            })
        }
    }

    impl UsnDevice for VolumeJournal {
        fn query(&mut self) -> Result<Journal, UsnError> {
            let what = tr!("查询 {volume} 的 USN 日志", volume = &self.volume);
            let out = self.handle.ioctl(FSCTL_QUERY_USN_JOURNAL, &[], JOURNAL_BUFFER).map_err(|e| fail(&what, e))?;
            if out.len() < 56 {
                return Err(UsnError::new(code::IO_ERROR, tr!("USN 日志信息只有 {size} 字节", size = out.len())));
            }
            if out.len() >= 60 {
                let max_major = u16_at(&out, 58);
                self.max_major = max_major.clamp(2, 3);
            }
            Ok(Journal {
                journal_id: u64_at(&out, 0),
                first_usn: u64_at(&out, 8) as i64,
                next_usn: u64_at(&out, 16) as i64,
                lowest_valid_usn: u64_at(&out, 24) as i64,
                max_usn: u64_at(&out, 32) as i64,
                max_size: u64_at(&out, 40),
                allocation_delta: u64_at(&out, 48),
                max_major: self.max_major,
            })
        }

        fn read(&mut self, start_usn: i64, journal_id: u64, size: usize) -> Result<Vec<u8>, UsnError> {
            // READ_USN_JOURNAL_DATA_V1: StartUsn, ReasonMask, ReturnOnlyOnClose, Timeout,
            // BytesToWaitFor, UsnJournalID, MinMajorVersion, MaxMajorVersion (sizeof 48)
            let mut data = Vec::with_capacity(48);
            data.extend_from_slice(&start_usn.to_le_bytes());
            data.extend_from_slice(&ALL_REASONS.to_le_bytes());
            data.extend_from_slice(&0u32.to_le_bytes());
            data.extend_from_slice(&0u64.to_le_bytes());
            data.extend_from_slice(&0u64.to_le_bytes());
            data.extend_from_slice(&journal_id.to_le_bytes());
            data.extend_from_slice(&2u16.to_le_bytes());
            data.extend_from_slice(&self.max_major.to_le_bytes());
            data.extend_from_slice(&[0; 4]);
            let what = tr!("读取 {volume} 的 USN 日志", volume = &self.volume);
            self.handle.ioctl(FSCTL_READ_USN_JOURNAL, &data, size).map_err(|e| fail(&what, e))
        }
    }
}
