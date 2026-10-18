//! Windows lister: `NtOpenFile` + `NtQueryDirectoryFile(FileIdExtdDirectoryInformation)`, one
//! system call per buffer of entries with names, sizes, times, attributes and reparse tags.
//!
//! Field rules are worker.py's `_list_windows` (which reads the same data through
//! FindFirstFileW/FindNextFileW). Paths are opened in NT form (`\??\C:\...`), so long paths need
//! no prefix and names Win32 would rewrite (trailing dots or spaces) are listed as they are.

use std::cell::RefCell;
use std::mem::offset_of;
use std::ptr;
use std::sync::atomic::{AtomicU8, Ordering};

use windows_sys::Wdk::Foundation::{NtClose, OBJECT_ATTRIBUTES};
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_BOTH_DIR_INFORMATION, FILE_DIRECTORY_FILE, FILE_ID_BOTH_DIR_INFORMATION,
    FILE_ID_EXTD_DIR_INFORMATION, FILE_INFORMATION_CLASS, FILE_OPEN_FOR_BACKUP_INTENT,
    FILE_SYNCHRONOUS_IO_NONALERT, FileBothDirectoryInformation, FileIdBothDirectoryInformation,
    FileIdExtdDirectoryInformation, NtOpenFile, NtQueryDirectoryFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_FILENAME_EXCED_RANGE, ERROR_PATH_NOT_FOUND, GetLastError, HANDLE, NTSTATUS,
    OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, STATUS_INVALID_DEVICE_REQUEST,
    STATUS_INVALID_INFO_CLASS, STATUS_INVALID_LEVEL, STATUS_INVALID_PARAMETER,
    STATUS_NO_MORE_FILES, STATUS_NO_SUCH_FILE, STATUS_NOT_IMPLEMENTED, STATUS_NOT_SUPPORTED,
    STATUS_OBJECT_NAME_NOT_FOUND, SetLastError, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetCompressedFileSizeW, GetFullPathNameW, SYNCHRONIZE,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::common::{Arena, NativePath, RootOpts, Row, ext_len_with, flag, kind, secs};

const FA_READONLY: u32 = 0x1;
const FA_HIDDEN: u32 = 0x2;
const FA_SYSTEM: u32 = 0x4;
const FA_DIRECTORY: u32 = 0x10;
const FA_ARCHIVE: u32 = 0x20;
const FA_TEMPORARY: u32 = 0x100;
const FA_SPARSE: u32 = 0x200;
const FA_REPARSE: u32 = 0x400;
const FA_COMPRESSED: u32 = 0x800;
const FA_OFFLINE: u32 = 0x1000;
const FA_NOT_INDEXED: u32 = 0x2000;
const FA_ENCRYPTED: u32 = 0x4000;
const FA_RECALL_ON_OPEN: u32 = 0x40000;
const FA_RECALL_ON_DATA_ACCESS: u32 = 0x400000;
const REPARSE_NAME_SURROGATE: u32 = 0x2000_0000; // symlinks, junctions, mount points: never descend

const ATTR_FLAGS: [(u32, u32); 11] = [
    (FA_READONLY, flag::READONLY),
    (FA_HIDDEN, flag::HIDDEN),
    (FA_SYSTEM, flag::SYSTEM),
    (FA_ARCHIVE, flag::ARCHIVE),
    (FA_TEMPORARY, flag::TEMPORARY),
    (FA_SPARSE, flag::SPARSE),
    (FA_REPARSE, flag::REPARSE),
    (FA_COMPRESSED, flag::COMPRESSED),
    (FA_OFFLINE, flag::OFFLINE),
    (FA_NOT_INDEXED, flag::NOT_INDEXED),
    (FA_ENCRYPTED, flag::ENCRYPTED),
];

/// 64 KiB: what FindFirstFileEx's large fetch uses, and safe for SMB redirectors.
const BUF_BYTES: usize = 64 * 1024;
const SECS_1601_TO_1970: i64 = 11_644_473_600;

/// Directory information classes, best first. Not every file system has the first two.
const CLASSES: [FILE_INFORMATION_CLASS; 3] = [
    FileIdExtdDirectoryInformation,
    FileIdBothDirectoryInformation,
    FileBothDirectoryInformation,
];

/// Per root: the information class its file system accepts (learnt on the first refusal).
pub struct RootState {
    level: AtomicU8,
}

impl RootState {
    pub fn new() -> Self {
        Self {
            level: AtomicU8::new(0),
        }
    }
}

thread_local! {
    static BUF: RefCell<Vec<u64>> = RefCell::new(vec![0u64; BUF_BYTES / 8]);
    static SCRATCH: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
}

const BS: u16 = b'\\' as u16;
const SLASH: u16 = b'/' as u16;

fn starts(path: &[u16], prefix: &str) -> bool {
    path.len() >= prefix.len() && prefix.bytes().zip(path).all(|(b, &u)| b as u16 == u)
}

fn push_str(out: &mut Vec<u16>, s: &str) {
    out.extend(s.bytes().map(u16::from));
}

/// `-(-x // c) * c`: round up to whole clusters (saturating instead of overflowing).
fn round_up(x: i64, cluster: i64) -> i64 {
    let c = cluster.max(1) as i128;
    let r = (x as i128 + c - 1).div_euclid(c) * c;
    r.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// Code points of a WTF-8 run (lone surrogates are 3-byte sequences, one code point each).
fn wtf8_code_points(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b & 0xC0 != 0x80).count()
}

/// `str.encode("utf-8", "surrogatepass")` of a UTF-16 name (i.e. WTF-8), appended to `out`.
fn push_wtf8(units: &[u16], out: &mut Vec<u8>) {
    if units.iter().all(|&u| u < 0x80) {
        out.extend(units.iter().map(|&u| u as u8));
        return;
    }
    for r in char::decode_utf16(units.iter().copied()) {
        match r {
            Ok(c) => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            Err(e) => {
                let u = e.unpaired_surrogate() as u32;
                out.extend_from_slice(&[
                    0xE0 | (u >> 12) as u8,
                    0x80 | ((u >> 6) & 0x3F) as u8,
                    0x80 | (u & 0x3F) as u8,
                ]);
            }
        }
    }
}

/// `ntpath.join(parent, name)` for a plain entry name.
fn join_windows(parent: &[u16], name: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(parent.len() + 1 + name.len());
    out.extend_from_slice(parent);
    let bare_drive = parent.len() == 2 && parent[1] == b':' as u16;
    let ends_sep = matches!(parent.last(), Some(&c) if c == BS || c == SLASH);
    if !parent.is_empty() && !ends_sep && !bare_drive {
        out.push(BS);
    }
    out.extend_from_slice(name);
    out
}

fn is_drive_abs(path: &[u16]) -> bool {
    path.len() >= 3
        && path[0] < 0x80
        && (path[0] as u8).is_ascii_alphabetic()
        && path[1] == b':' as u16
        && path[2] == BS
}

/// `\??\` form of a Win32 path, or None for forms that need Win32 normalisation first.
fn nt_form(path: &[u16], out: &mut Vec<u16>) -> bool {
    out.clear();
    if starts(path, "\\\\?\\") || starts(path, "\\\\.\\") {
        push_str(out, "\\??\\");
        out.extend_from_slice(&path[4..]);
        return true;
    }
    if path.contains(&SLASH) {
        return false;
    }
    if starts(path, "\\\\") {
        push_str(out, "\\??\\UNC\\");
        out.extend_from_slice(&path[2..]);
        return true;
    }
    if is_drive_abs(path) {
        push_str(out, "\\??\\");
        let mut p = path;
        while p.len() > 3 && p.last() == Some(&BS) {
            p = &p[..p.len() - 1];
        }
        out.extend_from_slice(p);
        return true;
    }
    false
}

fn full_path(path: &[u16]) -> Option<Vec<u16>> {
    let mut z: Vec<u16> = path.to_vec();
    z.push(0);
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: z is NUL-terminated; buf has buf.len() units.
        let n = unsafe {
            GetFullPathNameW(
                z.as_ptr(),
                buf.len() as u32,
                buf.as_mut_ptr(),
                ptr::null_mut(),
            )
        };
        if n == 0 {
            return None;
        }
        if (n as usize) < buf.len() {
            buf.truncate(n as usize);
            return Some(buf);
        }
        buf.resize(n as usize + 1, 0);
    }
}

/// `\\?\` form for Win32 file APIs (GetCompressedFileSizeW), NUL-terminated, into `out`.
fn long_form(dir: &[u16], name: &[u16], out: &mut Vec<u16>) {
    out.clear();
    if starts(dir, "\\\\?\\") || dir.contains(&SLASH) {
        // already long, or a form Win32 must normalise: pass through like worker._long_path
    } else if starts(dir, "\\\\") && !starts(dir, "\\\\.\\") {
        push_str(out, "\\\\?\\UNC\\");
        out.extend_from_slice(&join_windows(&dir[2..], name));
        out.push(0);
        return;
    } else if is_drive_abs(dir) {
        push_str(out, "\\\\?\\");
    }
    out.extend_from_slice(&join_windows(dir, name));
    out.push(0);
}

/// Python's OSError.strerror for a Win32 error code (FormatMessageW, trailing dots/space cut).
pub fn win_message(code: u32) -> String {
    let mut buf = [0u16; 1024];
    // SAFETY: buf is writable for its full length; no inserts are used.
    let n = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            ptr::null(),
            code,
            0x0400, // MAKELANGID(LANG_NEUTRAL, SUBLANG_DEFAULT), like CPython
            buf.as_mut_ptr(),
            buf.len() as u32,
            ptr::null(),
        )
    };
    if n == 0 {
        return format!("Windows Error 0x{code:x}");
    }
    let mut n = n as usize;
    while n > 0 && (buf[n - 1] <= b' ' as u16 || buf[n - 1] == b'.' as u16) {
        n -= 1;
    }
    if n == 0 {
        return "OSError".into();
    }
    String::from_utf16_lossy(&buf[..n])
}

fn status_message(status: NTSTATUS) -> String {
    // SAFETY: pure conversion.
    win_message(unsafe { RtlNtStatusToDosError(status) })
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { NtClose(self.0) };
    }
}

fn open_dir(path: &[u16]) -> Result<Handle, String> {
    SCRATCH.with_borrow_mut(|nt| {
        if !nt_form(path, nt) {
            let full = full_path(path).ok_or_else(|| win_message(unsafe { GetLastError() }))?;
            if !nt_form(&full, nt) {
                return Err(win_message(ERROR_PATH_NOT_FOUND));
            }
        }
        if nt.len() * 2 > u16::MAX as usize - 2 {
            return Err(win_message(ERROR_FILENAME_EXCED_RANGE));
        }
        let name = UNICODE_STRING {
            Length: (nt.len() * 2) as u16,
            MaximumLength: (nt.len() * 2) as u16,
            Buffer: nt.as_mut_ptr(),
        };
        let oa = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: ptr::null_mut(),
            ObjectName: &name,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: ptr::null(),
            SecurityQualityOfService: ptr::null(),
        };
        let mut h: HANDLE = ptr::null_mut();
        let mut iosb = IO_STATUS_BLOCK::default();
        // SAFETY: all pointers are valid for the call; FindFirstFileW opens with the same flags.
        let status = unsafe {
            NtOpenFile(
                &mut h,
                FILE_LIST_DIRECTORY | SYNCHRONIZE,
                &oa,
                &mut iosb,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_FOR_BACKUP_INTENT,
            )
        };
        if status < 0 {
            // FindFirstFileW reports a missing directory as "path not found"
            if status == STATUS_OBJECT_NAME_NOT_FOUND {
                return Err(win_message(ERROR_PATH_NOT_FOUND));
            }
            return Err(status_message(status));
        }
        Ok(Handle(h))
    })
}

fn compressed_size(dir: &[u16], name: &[u16]) -> Option<i64> {
    SCRATCH.with_borrow_mut(|p| {
        long_form(dir, name, p);
        let mut high = 0u32;
        // SAFETY: p is NUL-terminated.
        let low = unsafe {
            SetLastError(0);
            GetCompressedFileSizeW(p.as_ptr(), &mut high)
        };
        if low == u32::MAX && unsafe { GetLastError() } != 0 {
            return None;
        }
        Some((((high as u64) << 32) | low as u64) as i64)
    })
}

/// FILETIME (100 ns since 1601) -> worker._secs of CPython's float seconds.
fn ft_secs(ft: i64) -> u32 {
    let sec = ft / 10_000_000 - SECS_1601_TO_1970;
    let nsec = ((ft % 10_000_000) * 100) as i32 as u32; // C int -> unsigned long, as in CPython
    secs(sec, nsec as u64)
}

struct Raw<'a> {
    attrs: u32,
    tag: u32,
    creation: i64,
    access: i64,
    write: i64,
    eof: i64,
    name: &'a [u16],
}

/// Reads one directory entry of class `CLASSES[level]` at `p`, or None if it would run past
/// `avail` bytes (a misbehaving filter driver; the kernel itself never does that).
///
/// SAFETY: `p` points at `avail` readable bytes.
unsafe fn read_entry<'a>(p: *const u8, avail: usize, level: u8) -> Option<(u32, Raw<'a>)> {
    type E = FILE_ID_EXTD_DIR_INFORMATION;
    // every field read below lies before the class's FileName
    let name_off = match level {
        0 => offset_of!(E, FileName),
        1 => offset_of!(FILE_ID_BOTH_DIR_INFORMATION, FileName),
        _ => offset_of!(FILE_BOTH_DIR_INFORMATION, FileName),
    };
    if avail < name_off {
        return None;
    }
    unsafe {
        macro_rules! field {
            ($t:ty, $s:ty, $f:ident) => {
                ptr::read_unaligned(p.add(offset_of!($s, $f)) as *const $t)
            };
        }
        let next: u32 = field!(u32, E, NextEntryOffset);
        let attrs: u32 = field!(u32, E, FileAttributes);
        let name_len = field!(u32, E, FileNameLength) as usize / 2;
        let tag = match level {
            0 => field!(u32, E, ReparsePointTag),
            1 => field!(u32, FILE_ID_BOTH_DIR_INFORMATION, EaSize),
            _ => field!(u32, FILE_BOTH_DIR_INFORMATION, EaSize),
        };
        if name_off + name_len * 2 > avail {
            return None;
        }
        let name = std::slice::from_raw_parts(p.add(name_off) as *const u16, name_len);
        let raw = Raw {
            attrs,
            tag: if attrs & FA_REPARSE != 0 { tag } else { 0 },
            creation: field!(i64, E, CreationTime),
            access: field!(i64, E, LastAccessTime),
            write: field!(i64, E, LastWriteTime),
            eof: field!(i64, E, EndOfFile),
            name,
        };
        Some((next, raw))
    }
}

fn unsupported(status: NTSTATUS) -> bool {
    matches!(
        status,
        STATUS_INVALID_INFO_CLASS
            | STATUS_NOT_IMPLEMENTED
            | STATUS_NOT_SUPPORTED
            | STATUS_INVALID_PARAMETER
            | STATUS_INVALID_LEVEL
            | STATUS_INVALID_DEVICE_REQUEST
    )
}

fn add_entry(
    raw: &Raw<'_>,
    dir: &[u16],
    root: &RootOpts,
    base: usize,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath)>,
) {
    let attrs = raw.attrs;
    let mut flags = 0u32;
    for (bit, flag) in ATTR_FLAGS {
        if attrs & bit != 0 {
            flags |= flag;
        }
    }
    let kind = if attrs & FA_REPARSE != 0 && raw.tag & REPARSE_NAME_SURROGATE != 0 {
        kind::LINK
    } else if attrs & FA_DIRECTORY != 0 {
        kind::DIR
    } else {
        kind::FILE
    };
    let size = if kind == kind::DIR { 0 } else { raw.eof };
    let cluster = root.cluster_size.max(1);
    let alloc = if kind != kind::FILE {
        0
    } else if attrs & (FA_RECALL_ON_DATA_ACCESS | FA_RECALL_ON_OPEN) != 0 {
        flags |= flag::CLOUD;
        0
    } else if attrs & (FA_COMPRESSED | FA_SPARSE) != 0 {
        round_up(compressed_size(dir, raw.name).unwrap_or(size), cluster)
    } else if size <= root.resident_max {
        0
    } else {
        round_up(size, cluster)
    };
    let name_start = arena.names.len();
    push_wtf8(raw.name, &mut arena.names);
    let ext_len = if kind == kind::FILE {
        ext_len_with(&arena.names[name_start..], wtf8_code_points)
    } else {
        0
    };
    let row = Row {
        kind,
        flags,
        size,
        alloc,
        mtime: ft_secs(raw.write),
        ctime: ft_secs(raw.creation),
        atime: ft_secs(raw.access),
        ino: 0,
        nlink: 1,
        dev: 0,
        ext_len,
    };
    let j = (arena.len() - base) as u32;
    arena.push_row(name_start, row);
    if kind == kind::DIR {
        subdirs.push((j, join_windows(dir, raw.name).into_boxed_slice()));
    }
}

/// Lists `path` into `arena` (worker._list_windows); subdirectories to descend go to `subdirs`.
pub fn list_dir(
    path: &[u16],
    _discovered: bool,
    root: &RootOpts,
    state: &RootState,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath)>,
) -> Result<(), String> {
    let h = open_dir(path)?;
    let base = arena.len();
    BUF.with_borrow_mut(|buf| {
        let mut first = true;
        loop {
            let level = state.level.load(Ordering::Relaxed);
            let mut iosb = IO_STATUS_BLOCK::default();
            // SAFETY: synchronous handle, buffer is 8-byte aligned and BUF_BYTES long.
            let status = unsafe {
                NtQueryDirectoryFile(
                    h.0,
                    ptr::null_mut(),
                    None,
                    ptr::null(),
                    &mut iosb,
                    buf.as_mut_ptr().cast(),
                    BUF_BYTES as u32,
                    CLASSES[level as usize],
                    false,
                    ptr::null(),
                    first,
                )
            };
            if status == STATUS_NO_MORE_FILES || (first && status == STATUS_NO_SUCH_FILE) {
                return Ok(());
            }
            if status < 0 {
                if first && unsupported(status) && (level as usize) < CLASSES.len() - 1 {
                    let _ = state.level.compare_exchange(
                        level,
                        level + 1,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                    continue;
                }
                return Err(status_message(status));
            }
            first = false;
            let filled = iosb.Information.min(BUF_BYTES);
            if filled == 0 {
                return Ok(());
            }
            let bytes = buf.as_ptr().cast::<u8>();
            let mut off = 0usize;
            loop {
                // SAFETY: off < filled <= BUF_BYTES.
                let Some((next, raw)) =
                    (unsafe { read_entry(bytes.add(off), filled - off, level) })
                else {
                    return Err(status_message(STATUS_INVALID_PARAMETER));
                };
                let n = raw.name;
                let dots = n == [b'.' as u16] || n == [b'.' as u16, b'.' as u16];
                if !dots {
                    add_entry(&raw, path, root, base, arena, subdirs);
                }
                if next == 0 || off + next as usize >= filled {
                    break;
                }
                off += next as usize;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn nt_forms() {
        let mut out = Vec::new();
        assert!(nt_form(&w("C:\\"), &mut out));
        assert_eq!(out, w("\\??\\C:\\"));
        assert!(nt_form(&w("C:\\a\\b\\"), &mut out));
        assert_eq!(out, w("\\??\\C:\\a\\b"));
        assert!(nt_form(&w("\\\\srv\\share\\x"), &mut out));
        assert_eq!(out, w("\\??\\UNC\\srv\\share\\x"));
        assert!(nt_form(&w("\\\\?\\C:\\x"), &mut out));
        assert_eq!(out, w("\\??\\C:\\x"));
        assert!(!nt_form(&w("C:/x"), &mut out));
        assert!(!nt_form(&w("rel\\x"), &mut out));
    }

    #[test]
    fn long_forms() {
        let mut out = Vec::new();
        long_form(&w("C:\\a"), &w("f.txt"), &mut out);
        assert_eq!(out, [w("\\\\?\\C:\\a\\f.txt"), vec![0]].concat());
        long_form(&w("\\\\srv\\share"), &w("f"), &mut out);
        assert_eq!(out, [w("\\\\?\\UNC\\srv\\share\\f"), vec![0]].concat());
    }

    #[test]
    fn joins_like_ntpath() {
        assert_eq!(join_windows(&w("C:\\"), &w("x")), w("C:\\x"));
        assert_eq!(join_windows(&w("C:\\a"), &w("x")), w("C:\\a\\x"));
        assert_eq!(join_windows(&w("C:"), &w("x")), w("C:x"));
        assert_eq!(
            join_windows(&w("\\\\srv\\share"), &w("x")),
            w("\\\\srv\\share\\x")
        );
    }

    #[test]
    fn wtf8_matches_surrogatepass() {
        let mut out = Vec::new();
        push_wtf8(&[0x61, 0xD800, 0x62, 0xD83D, 0xDE00, 0xDC00], &mut out);
        assert_eq!(out, b"a\xed\xa0\x80b\xf0\x9f\x98\x80\xed\xb0\x80");
        assert_eq!(wtf8_code_points(&out), 5);
    }

    #[test]
    fn round_up_clusters() {
        assert_eq!(round_up(0, 4096), 0);
        assert_eq!(round_up(1, 4096), 4096);
        assert_eq!(round_up(4096, 4096), 4096);
        assert_eq!(round_up(4097, 0), 4097);
        assert_eq!(round_up(i64::MAX, 4096), i64::MAX);
    }

    fn raw(attrs: u32, tag: u32, eof: i64) -> Raw<'static> {
        static NAME: [u16; 5] = [
            b'f' as u16,
            b'.' as u16,
            b'T' as u16,
            b'x' as u16,
            b'T' as u16,
        ];
        Raw {
            attrs,
            tag,
            creation: 0,
            access: 0,
            write: 0,
            eof,
            name: &NAME,
        }
    }

    fn entry(attrs: u32, tag: u32, eof: i64) -> (u8, u32, i64, i64, usize) {
        let root = RootOpts {
            cluster_size: 4096,
            resident_max: 512,
            ..RootOpts::default()
        };
        let mut arena = Arena::default();
        let mut subs = Vec::new();
        add_entry(
            &raw(attrs, tag, eof),
            &w("C:\\nowhere-spaceknife"),
            &root,
            0,
            &mut arena,
            &mut subs,
        );
        (
            arena.kind[0],
            arena.flags[0],
            arena.size[0],
            arena.alloc[0],
            subs.len(),
        )
    }

    #[test]
    fn entry_rules_match_worker() {
        // resident, cluster-rounded, directory, name-surrogate link, non-surrogate reparse dir
        assert_eq!(entry(0x20, 0, 512), (kind::FILE, flag::ARCHIVE, 512, 0, 0));
        assert_eq!(entry(0, 0, 513), (kind::FILE, 0, 513, 4096, 0));
        assert_eq!(
            entry(FA_DIRECTORY | FA_HIDDEN, 0, 99),
            (kind::DIR, flag::HIDDEN, 0, 0, 1)
        );
        assert_eq!(
            entry(FA_DIRECTORY | FA_REPARSE, 0xA000_0003, 0),
            (kind::LINK, flag::REPARSE, 0, 0, 0)
        );
        assert_eq!(
            entry(FA_DIRECTORY | FA_REPARSE, 0x9000_001A, 0),
            (kind::DIR, flag::REPARSE, 0, 0, 1)
        );
        // cloud placeholders take no space; compressed files fall back to the size when the
        // compressed size cannot be read
        assert_eq!(
            entry(FA_RECALL_ON_DATA_ACCESS | FA_OFFLINE, 0, 10_000),
            (kind::FILE, flag::OFFLINE | flag::CLOUD, 10_000, 0, 0)
        );
        assert_eq!(
            entry(FA_COMPRESSED, 0, 5000),
            (kind::FILE, flag::COMPRESSED, 5000, 8192, 0)
        );
    }

    #[test]
    fn short_last_entries_are_read_and_overruns_refused() {
        let name_offs = [
            offset_of!(FILE_ID_EXTD_DIR_INFORMATION, FileName),
            offset_of!(FILE_ID_BOTH_DIR_INFORMATION, FileName),
            offset_of!(FILE_BOTH_DIR_INFORMATION, FileName),
        ];
        for (level, &off) in name_offs.iter().enumerate() {
            let mut buf = vec![0u64; 32];
            let bytes = buf.as_mut_ptr().cast::<u8>();
            unsafe {
                ptr::write_unaligned(bytes.add(56) as *mut u32, FA_ARCHIVE); // FileAttributes
                ptr::write_unaligned(bytes.add(60) as *mut u32, 2); // FileNameLength
                ptr::write_unaligned(bytes.add(off) as *mut u16, b'a' as u16);
                let (next, raw) = read_entry(bytes, off + 2, level as u8).unwrap();
                assert_eq!(
                    (next, raw.attrs, raw.name),
                    (0, FA_ARCHIVE, &[b'a' as u16][..])
                );
                assert!(read_entry(bytes, off + 1, level as u8).is_none());
            }
        }
    }

    #[test]
    fn filetime_conversion() {
        assert_eq!(ft_secs(0), 0);
        assert_eq!(ft_secs(SECS_1601_TO_1970 * 10_000_000 + 15_000_000), 1);
    }

    #[test]
    fn missing_dir_is_path_not_found() {
        let err = open_dir(&w("C:\\definitely-not-here-spaceknife"))
            .err()
            .unwrap();
        assert_eq!(err, win_message(ERROR_PATH_NOT_FOUND));
    }
}
