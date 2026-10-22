//! Windows lister: `NtOpenFile` + `NtQueryDirectoryFile(FileIdExtdDirectoryInformation)`, one
//! system call per buffer of entries with names, sizes, times, attributes and reparse tags.
//!
//! Field rules are worker.py's `_list_windows` (which reads the same data through
//! FindFirstFileW/FindNextFileW). Paths are opened in NT form (`\??\C:\...`), so long paths need
//! no prefix and names Win32 would rewrite (trailing dots or spaces) are listed as they are.
//!
//! With `keep_access_time` (on by default, docs/native.md 2.2) directories are opened with
//! FILE_WRITE_ATTRIBUTES and told to keep their last access time (the -1 of SetFileTime): on NTFS
//! with access time updates on, listing then no longer rewrites every folder's record (1.5-3.3 GB
//! of writes for a scan of a big system volume). It falls back to the plain open wherever that
//! access is refused.
//!
//! With `relative_open` (on by default) a subdirectory found during the call is opened by its
//! name relative to its parent's open handle (the NT form of openat), so the file system looks up
//! one name instead of resolving the whole path from the volume root again. It is still an open by
//! name: filters get the parent's file object and the one name, from which the filter manager
//! builds the same full name an open by full path has (filters that follow its rules see no
//! difference). Opens by file id had no name at all, and filters showed some folders only in part
//! (docs/native.md 2.2). The handle is then checked against the file id the parent's listing gave
//! that name, which rules out opening any other folder; whatever does not match, fails or cannot
//! be checked is opened by full path, as a task is. That the listings are the same is checked on
//! whole volumes (docs/native.md 2.2), not by the file id.

use std::cell::RefCell;
use std::mem::offset_of;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use windows_sys::Wdk::Foundation::{NtClose, NtQueryObject, OBJECT_ATTRIBUTES, OBJECT_NAME_INFORMATION};
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_BASIC_INFORMATION, FILE_BOTH_DIR_INFORMATION, FILE_DIRECTORY_FILE,
    FILE_ID_BOTH_DIR_INFORMATION, FILE_ID_EXTD_DIR_INFORMATION, FILE_ID_INFORMATION,
    FILE_INFORMATION_CLASS, FILE_INTERNAL_INFORMATION, FILE_OPEN_FOR_BACKUP_INTENT,
    FILE_SYNCHRONOUS_IO_NONALERT, FileBasicInformation, FileBothDirectoryInformation,
    FileIdBothDirectoryInformation, FileIdExtdDirectoryInformation, FileIdInformation,
    FileInternalInformation, NtOpenFile, NtQueryDirectoryFile, NtQueryInformationFile,
    NtSetInformationFile,
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
    FILE_WRITE_ATTRIBUTES, GetCompressedFileSizeW, GetFullPathNameW, SYNCHRONIZE,
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

/// Per root and call: the information class its file system accepts (learnt on the first
/// refusal), whether directories are opened to keep their access times, and whether found
/// subdirectories are opened from their parent.
pub struct RootState {
    level: AtomicU8,
    /// Ask for FILE_WRITE_ATTRIBUTES to keep access times (cleared when that mostly fails).
    keep: AtomicBool,
    kept: AtomicU32,
    refused: AtomicU32,
    /// Open found subdirectories relative to their parent. Cleared when such opens cannot be
    /// checked or do not work here: the file id of a handle cannot be read, no information class
    /// left gives ids that belong to their entries (`distrust_ids`), or failed relative opens
    /// outnumber the ones that passed their check.
    relative: AtomicBool,
    /// Relative opens whose handle had the listed file id, those that had another, and those
    /// that failed (each backs relative opens off when it outnumbers the matches, past 64).
    matched: AtomicU32,
    mismatched: AtomicU32,
    failed: AtomicU32,
    /// How much longer the kernel's name of a folder of this root is than its NT path (`\??\C:`
    /// becomes `\Device\HarddiskVolumeN`, a SUBST drive its target): the open by path is refused
    /// once the longer one passes the limit. UNKNOWN until the first open by path measures it.
    expansion: AtomicU32,
}

const UNKNOWN: u32 = u32::MAX;

impl RootState {
    pub fn new(keep_access_time: bool, relative_open: bool) -> Self {
        Self {
            level: AtomicU8::new(0),
            keep: AtomicBool::new(keep_access_time),
            kept: AtomicU32::new(0),
            refused: AtomicU32::new(0),
            relative: AtomicBool::new(relative_open),
            matched: AtomicU32::new(0),
            mismatched: AtomicU32::new(0),
            failed: AtomicU32::new(0),
            expansion: AtomicU32::new(UNKNOWN),
        }
    }
}

/// How directories were opened, process-wide (`_open_stats`: tests and measurements).
pub struct OpenStats {
    /// Opened by name relative to the parent, and the handle had the listed file id.
    pub relative: AtomicU64,
    /// Opened by full path (tasks, and everything a relative open did not settle).
    pub by_path: AtomicU64,
    /// A relative open failed (the open by full path then gave the error, or succeeded).
    pub relative_failed: AtomicU64,
    /// A relative open reached a folder with another file id than the listing gave.
    pub id_mismatch: AtomicU64,
    /// The handle's file id could not be read: the root went back to opens by path.
    pub id_unknown: AtomicU64,
    /// FILE_WRITE_ATTRIBUTES was refused: that folder's access time was updated by its listing.
    pub keep_refused: AtomicU64,
    /// A root stopped trusting the file ids of an information class (`distrust_ids`).
    pub ids_distrusted: AtomicU64,
    /// A relative open failed and the open by path then worked: something on the volume treats
    /// the two differently (expected: 0; a folder created meanwhile under a replaced parent).
    pub path_after_failed: AtomicU64,
    /// Listing through a relatively opened handle failed and was done again by path.
    pub relisted_by_path: AtomicU64,
}

pub static STATS: OpenStats = OpenStats {
    relative: AtomicU64::new(0),
    by_path: AtomicU64::new(0),
    relative_failed: AtomicU64::new(0),
    id_mismatch: AtomicU64::new(0),
    id_unknown: AtomicU64::new(0),
    keep_refused: AtomicU64::new(0),
    ids_distrusted: AtomicU64::new(0),
    path_after_failed: AtomicU64::new(0),
    relisted_by_path: AtomicU64::new(0),
};

pub fn open_stats(reset: bool) -> Vec<(&'static str, u64)> {
    let s = &STATS;
    let get = |c: &AtomicU64| {
        if reset {
            c.swap(0, Ordering::Relaxed)
        } else {
            c.load(Ordering::Relaxed)
        }
    };
    vec![
        ("relative", get(&s.relative)),
        ("by_path", get(&s.by_path)),
        ("relative_failed", get(&s.relative_failed)),
        ("id_mismatch", get(&s.id_mismatch)),
        ("id_unknown", get(&s.id_unknown)),
        ("keep_refused", get(&s.keep_refused)),
        ("ids_distrusted", get(&s.ids_distrusted)),
        ("path_after_failed", get(&s.path_after_failed)),
        ("relisted_by_path", get(&s.relisted_by_path)),
    ]
}

/// What a found subdirectory keeps from its parent's listing to be opened from it: the file id
/// the listing gave (`bits`: 128 from FileIdExtd, 64 from FileIdBoth, 0 = none, or a name that
/// cannot be opened on its own) and the length of its name, the last `name_len` units of its path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hint {
    id: u128,
    bits: u8,
    name_len: u16,
}

/// A listed directory's open handle, kept for its subdirectories (walk.rs drops it once the last
/// of them has started).
pub type Parent = Handle;

/// How a found subdirectory is opened: by name relative to its parent's handle.
pub struct Via {
    parent: Arc<Parent>,
    hint: Hint,
}

pub fn via(parent: &Arc<Parent>, hint: Hint) -> Option<Via> {
    (hint.bits != 0).then(|| Via {
        parent: parent.clone(),
        hint,
    })
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

pub struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { NtClose(self.0) };
    }
}

// SAFETY: a kernel handle may be used from any thread; a parent's handle is only read (as the
// RootDirectory of its subdirectories' opens) and closed once, by the last owner.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

fn open_dir(path: &[u16], state: &RootState) -> Result<Handle, String> {
    STATS.by_path.fetch_add(1, Ordering::Relaxed);
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
        let (h, keep) = open_with(&oa, state).map_err(|status| {
            // FindFirstFileW reports a missing directory as "path not found"
            if status == STATUS_OBJECT_NAME_NOT_FOUND {
                win_message(ERROR_PATH_NOT_FOUND)
            } else {
                status_message(status)
            }
        })?;
        count_keep(state, keep);
        if state.relative.load(Ordering::Relaxed) && state.expansion.load(Ordering::Relaxed) == UNKNOWN {
            match kernel_name_len(&h) {
                Some(k) => state.expansion.store(k.saturating_sub(nt.len()) as u32, Ordering::Relaxed),
                None => state.relative.store(false, Ordering::Relaxed), // no way to keep to the limit
            }
        }
        Ok(h)
    })
}

/// Length in UTF-16 units of the kernel's name of what `h` refers to (`\Device\HarddiskVolume3\…`).
fn kernel_name_len(h: &Handle) -> Option<usize> {
    const OBJECT_NAME_INFORMATION_CLASS: i32 = 1;
    let mut buf = vec![0u64; (size_of::<OBJECT_NAME_INFORMATION>() + 65536) / 8 + 1];
    let mut got = 0u32;
    // SAFETY: buf is writable for its whole length.
    let status = unsafe {
        NtQueryObject(
            h.0,
            OBJECT_NAME_INFORMATION_CLASS,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut got,
        )
    };
    if status < 0 {
        return None;
    }
    // SAFETY: on success the buffer starts with an OBJECT_NAME_INFORMATION.
    let info = unsafe { &*buf.as_ptr().cast::<OBJECT_NAME_INFORMATION>() };
    Some(info.Name.Length as usize / 2)
}

/// Whether a handle keeps its directory's access time: it was opened with FILE_WRITE_ATTRIBUTES
/// and told so, the plain open followed a refusal, or keeping is off for this root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keep {
    Kept,
    Refused,
    Off,
}

/// NtOpenFile of a directory with FindFirstFileW's flags. With `keep` on, first with
/// FILE_WRITE_ATTRIBUTES, telling the handle to keep the access time; when that is refused
/// (denied, read-only media, a sharing mode) the plain open follows. The caller counts the handle
/// it uses (`count_keep`), so a handle opened and given up (a relative open that did not pass its
/// check) does not count.
fn open_with(oa: &OBJECT_ATTRIBUTES, state: &RootState) -> Result<(Handle, Keep), NTSTATUS> {
    let open = |access: u32| -> Result<Handle, NTSTATUS> {
        let mut h: HANDLE = ptr::null_mut();
        let mut iosb = IO_STATUS_BLOCK::default();
        // SAFETY: all pointers are valid for the call; FindFirstFileW opens with the same flags.
        let status = unsafe {
            NtOpenFile(
                &mut h,
                access,
                oa,
                &mut iosb,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_FOR_BACKUP_INTENT,
            )
        };
        if status < 0 {
            Err(status)
        } else {
            Ok(Handle(h))
        }
    };
    if !state.keep.load(Ordering::Relaxed) {
        return open(FILE_LIST_DIRECTORY | SYNCHRONIZE).map(|h| (h, Keep::Off));
    }
    if let Ok(h) = open(FILE_LIST_DIRECTORY | SYNCHRONIZE | FILE_WRITE_ATTRIBUTES) {
        keep_access_time(h.0);
        return Ok((h, Keep::Kept));
    }
    open(FILE_LIST_DIRECTORY | SYNCHRONIZE).map(|h| (h, Keep::Refused))
}

/// Counts a handle that is used for a listing. Once refusals outnumber the kept opens (a
/// standard user in system folders) this root stops asking for FILE_WRITE_ATTRIBUTES.
fn count_keep(state: &RootState, keep: Keep) {
    match keep {
        Keep::Kept => {
            state.kept.fetch_add(1, Ordering::Relaxed);
        }
        Keep::Refused => {
            STATS.keep_refused.fetch_add(1, Ordering::Relaxed);
            let refused = state.refused.fetch_add(1, Ordering::Relaxed) + 1;
            if refused > 64 && refused > state.kept.load(Ordering::Relaxed) {
                state.keep.store(false, Ordering::Relaxed);
            }
        }
        Keep::Off => {}
    }
}

/// Opens subdirectory `path`, found in the listing of the directory `via` holds open: relative to
/// it when that can be checked (`open_relative`), else by full path like a task. The flag says
/// whether the handle is a relative one.
fn open_child(path: &[u16], via: &Via, state: &RootState) -> Result<(Handle, bool), String> {
    if !state.relative.load(Ordering::Relaxed) {
        return open_dir(path, state).map(|h| (h, false));
    }
    match open_relative(path, via, state) {
        Ok(h) => {
            STATS.relative.fetch_add(1, Ordering::Relaxed);
            Ok((h, true))
        }
        Err(why) => {
            let h = open_dir(path, state)?;
            if why == Declined::Failed {
                STATS.path_after_failed.fetch_add(1, Ordering::Relaxed);
            }
            Ok((h, false))
        }
    }
}

/// Why `open_relative` left a folder to the open by path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Declined {
    /// Not a case for it (path forms and lengths the open by path decides on).
    Ineligible,
    Failed,
    Mismatch,
    Unknown,
}

/// The open of a subdirectory by its name relative to its parent's handle, only when it is the
/// folder the parent's listing named: the handle must carry the file id that listing gave the
/// name. None hands the open to the open by full path (which also reports any error):
/// - its NT path is one the open by path would not accept (too long, or a form Win32 must
///   normalise first): a relative open must not reach further than the open by path does;
/// - the open fails (gone, denied): counted; failures outnumbering matches end relative opens
///   on this root;
/// - the handle has another file id (the folder was replaced meanwhile, or the listing's ids do
///   not belong to its entries): counted; mismatches outnumbering matches make the root distrust
///   that information class's ids (`distrust_ids`);
/// - the file id cannot be read: such opens cannot be checked, so this root stops making them.
fn open_relative(path: &[u16], via: &Via, state: &RootState) -> Result<Handle, Declined> {
    let n = via.hint.name_len as usize;
    if n == 0 || n >= path.len() {
        return Err(Declined::Ineligible);
    }
    let expansion = state.expansion.load(Ordering::Relaxed);
    if expansion == UNKNOWN {
        return Err(Declined::Ineligible);
    }
    // what the open by path accepts, with a few units to spare: near the limit it decides itself
    let opens_by_path = SCRATCH.with_borrow_mut(|nt| {
        nt_form(path, nt)
            && nt.len() * 2 <= u16::MAX as usize - 2
            && nt.len() + expansion as usize + LIMIT_MARGIN <= u16::MAX as usize / 2
    });
    if !opens_by_path {
        return Err(Declined::Ineligible);
    }
    let name = &path[path.len() - n..];
    let us = UNICODE_STRING {
        Length: (n * 2) as u16,
        MaximumLength: (n * 2) as u16,
        Buffer: name.as_ptr().cast_mut(), // only read by the open
    };
    let oa = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: via.parent.0,
        ObjectName: &us,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let Ok((h, keep)) = open_with(&oa, state) else {
        STATS.relative_failed.fetch_add(1, Ordering::Relaxed);
        // now and then a folder deleted meanwhile; again and again: relative opens do not work here
        let n = state.failed.fetch_add(1, Ordering::Relaxed) + 1;
        if n > 64 && n > state.matched.load(Ordering::Relaxed) {
            state.relative.store(false, Ordering::Relaxed);
        }
        return Err(Declined::Failed);
    };
    match file_id(&h, via.hint.bits) {
        Some(id) if id == via.hint.id => {
            state.matched.fetch_add(1, Ordering::Relaxed);
            count_keep(state, keep);
            Ok(h)
        }
        Some(_) => {
            STATS.id_mismatch.fetch_add(1, Ordering::Relaxed);
            // now and then a folder replaced meanwhile; again and again: ids that do not belong
            let n = state.mismatched.fetch_add(1, Ordering::Relaxed) + 1;
            if n > 64 && n > state.matched.load(Ordering::Relaxed) {
                distrust_ids(state, if via.hint.bits == 128 { 0 } else { 1 });
            }
            Err(Declined::Mismatch)
        }
        None => {
            STATS.id_unknown.fetch_add(1, Ordering::Relaxed);
            state.relative.store(false, Ordering::Relaxed);
            Err(Declined::Unknown)
        }
    }
}

/// Units below the kernel's name limit that relative opens keep clear of (the exact rule for the
/// expanded name is the object manager's; within this margin the open by path decides).
const LIMIT_MARGIN: usize = 8;

/// The file id of the folder `h` refers to, in the width its parent's listing gave: 128 bits
/// (FileIdInformation, the id FileIdExtdDirectoryInformation lists) or 64 (FileInternalInformation,
/// the id FileIdBothDirectoryInformation lists).
fn file_id(h: &Handle, bits: u8) -> Option<u128> {
    let mut iosb = IO_STATUS_BLOCK::default();
    if bits == 128 {
        let mut info = FILE_ID_INFORMATION::default();
        // SAFETY: `info` is writable for its size.
        let status = unsafe {
            NtQueryInformationFile(
                h.0,
                &mut iosb,
                (&raw mut info).cast(),
                size_of::<FILE_ID_INFORMATION>() as u32,
                FileIdInformation,
            )
        };
        (status >= 0).then(|| u128::from_le_bytes(info.FileId.Identifier))
    } else {
        let mut info = FILE_INTERNAL_INFORMATION::default();
        // SAFETY: `info` is writable for its size.
        let status = unsafe {
            NtQueryInformationFile(
                h.0,
                &mut iosb,
                (&raw mut info).cast(),
                size_of::<FILE_INTERNAL_INFORMATION>() as u32,
                FileInternalInformation,
            )
        };
        (status >= 0).then(|| info.IndexNumber as u64 as u128)
    }
}

/// What is done through `h` no longer updates the directory's last access time (SetFileTime's
/// -1; FILE_BASIC_INFORMATION's zero fields change nothing). It writes no USN record and leaves
/// ChangeTime as it was. Best effort.
fn keep_access_time(h: HANDLE) {
    let info = FILE_BASIC_INFORMATION {
        CreationTime: 0,
        LastAccessTime: -1,
        LastWriteTime: 0,
        ChangeTime: 0,
        FileAttributes: 0,
    };
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: `info` is a valid FILE_BASIC_INFORMATION for the call.
    unsafe {
        NtSetInformationFile(
            h,
            &mut iosb,
            (&raw const info).cast(),
            size_of::<FILE_BASIC_INFORMATION>() as u32,
            FileBasicInformation,
        );
    }
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
    /// File id and its width in bits (FileIdExtd 128, FileIdBoth 64, FileBoth none: 0).
    id: u128,
    id_bits: u8,
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
        let (id, id_bits) = match level {
            0 => (
                u128::from_le_bytes(ptr::read_unaligned(
                    p.add(offset_of!(E, FileId)) as *const [u8; 16]
                )),
                128,
            ),
            1 => (
                field!(i64, FILE_ID_BOTH_DIR_INFORMATION, FileId) as u64 as u128,
                64,
            ),
            _ => (0, 0),
        };
        let raw = Raw {
            attrs,
            tag: if attrs & FA_REPARSE != 0 { tag } else { 0 },
            creation: field!(i64, E, CreationTime),
            access: field!(i64, E, LastAccessTime),
            write: field!(i64, E, LastWriteTime),
            eof: field!(i64, E, EndOfFile),
            id,
            id_bits,
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

/// How a subdirectory of this listing can be opened from it: with a file id to check the open
/// against (0 is no id), a name that is a single path component (a `\` or `/` in it, as WinBtrfs
/// shows some Linux names, would make the name a path of its own), and no reparse point.
fn hint_of(raw: &Raw<'_>) -> Hint {
    let single = !raw.name.iter().any(|&u| u == BS || u == SLASH);
    // A reparse point that is descended (a cloud folder, ProjFS, a DFS link...) is opened by full
    // path: its listed id is the reparse point's own, so a layer that redirected only opens by
    // path would leave a relative open on the reparse point with a matching id. Its subdirectories
    // can be opened relative to it, as its handle is wherever the open by path led.
    let reparse = raw.attrs & FA_REPARSE != 0;
    if raw.id_bits == 0 || raw.id == 0 || !single || reparse || raw.name.len() > u16::MAX as usize {
        return Hint::default();
    }
    Hint {
        id: raw.id,
        bits: raw.id_bits,
        name_len: raw.name.len() as u16,
    }
}

fn add_entry(
    raw: &Raw<'_>,
    dir: &[u16],
    root: &RootOpts,
    base: usize,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath, Hint)>,
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
        subdirs.push((j, join_windows(dir, raw.name).into_boxed_slice(), hint_of(raw)));
    }
}

/// Lists `path` into `arena` (worker._list_windows); subdirectories to descend go to `subdirs`.
/// `via`: how to open it from its listed parent (a found subdirectory). Returns the directory's
/// handle when its subdirectories can be opened from it.
pub fn list_dir(
    path: &[u16],
    _discovered: bool,
    via: Option<Via>,
    root: &RootOpts,
    state: &RootState,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath, Hint)>,
) -> Result<Option<Arc<Parent>>, String> {
    let (mut h, relative) = match &via {
        Some(v) => open_child(path, v, state)?,
        None => (open_dir(path, state)?, false),
    };
    drop(via); // the parent may close as soon as its last subdirectory is open
    let base = arena.len();
    let mark = arena.mark();
    let first_sub = subdirs.len();
    if let Err(e) = list_into(&h, path, root, state, base, arena, subdirs) {
        if !relative {
            return Err(e);
        }
        // whatever made it fail, the open by path has the last word, as for a task
        STATS.relisted_by_path.fetch_add(1, Ordering::Relaxed);
        arena.truncate(mark);
        subdirs.truncate(first_sub);
        h = open_dir(path, state)?;
        list_into(&h, path, root, state, base, arena, subdirs)?;
    }
    let keep = subdirs.len() > first_sub && state.relative.load(Ordering::Relaxed);
    Ok(keep.then(|| Arc::new(h)))
}

fn list_into(
    h: &Handle,
    path: &[u16],
    root: &RootOpts,
    state: &RootState,
    base: usize,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath, Hint)>,
) -> Result<(), String> {
    let first_sub = subdirs.len();
    // one information class for the whole listing, even if another thread changes the root's
    let mut level = state.level.load(Ordering::Relaxed);
    BUF.with_borrow_mut(|buf| {
        let mut first = true;
        loop {
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
                break;
            }
            if status < 0 {
                if first && unsupported(status) && (level as usize) < CLASSES.len() - 1 {
                    let _ = state.level.compare_exchange(
                        level,
                        level + 1,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                    level = state.level.load(Ordering::Relaxed).max(level + 1);
                    continue;
                }
                return Err(status_message(status));
            }
            first = false;
            let filled = iosb.Information.min(BUF_BYTES);
            if filled == 0 {
                break;
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
        Ok(())
    })?;
    if state.relative.load(Ordering::Relaxed) && shares_ids(&subdirs[first_sub..]) {
        distrust_ids(state, level);
        for sub in &mut subdirs[first_sub..] {
            sub.2 = Hint::default();
        }
    }
    Ok(())
}

/// Two subdirectories of one listing with the same file id: the information class reports ids
/// that do not belong to the entries (directories have no hard links). WinBtrfs gives every entry
/// of a FileIdExtdDirectoryInformation listing the listed directory's own id, while its
/// FileIdBothDirectoryInformation ids and its handles' ids are right.
fn shares_ids(subs: &[(u32, NativePath, Hint)]) -> bool {
    let mut ids: Vec<u128> = subs
        .iter()
        .filter(|s| s.2.bits != 0)
        .map(|s| s.2.id)
        .collect();
    if ids.len() < 2 {
        return false;
    }
    ids.sort_unstable();
    ids.windows(2).any(|w| w[0] == w[1])
}

/// The ids listings of this root give at `level` do not belong to their entries: list with the
/// next class (FileIdBoth after FileIdExtd); with none left that has ids, stop opening relatively.
fn distrust_ids(state: &RootState, level: u8) {
    let changed = if level == 0 {
        state
            .level
            .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    } else {
        state.relative.swap(false, Ordering::Relaxed)
    };
    if changed {
        STATS.ids_distrusted.fetch_add(1, Ordering::Relaxed);
    }
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
            id: 0,
            id_bits: 0,
            name: &NAME,
        }
    }

    #[test]
    fn a_relative_open_reaching_another_folder_than_listed_is_refused() {
        let base = std::env::temp_dir().join(format!("spaceknife-relative-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("a")).unwrap();
        std::fs::create_dir_all(base.join("b")).unwrap();
        let dir = w(base.to_str().unwrap());
        let child = |name: &str| join_windows(&dir, &w(name));
        let state = RootState::new(false, true);
        let parent = Arc::new(open_dir(&dir, &state).unwrap());
        for bits in [128u8, 64] {
            // as FileIdExtd (128 bits, FileIdInformation) and FileIdBoth (64, FileInternalInformation) list them
            let id_of = |name: &str| file_id(&open_dir(&child(name), &state).unwrap(), bits).unwrap();
            let (a, b) = (id_of("a"), id_of("b"));
            assert_ne!(a, b);
            let via = |id: u128| Via {
                parent: parent.clone(),
                hint: Hint {
                    id,
                    bits,
                    name_len: 1,
                },
            };
            assert!(open_relative(&child("a"), &via(a), &state).is_ok());
            let before = STATS.id_mismatch.load(Ordering::Relaxed);
            // the name "a" leads to a, but the listing said b: not that folder, so not opened
            assert_eq!(
                open_relative(&child("a"), &via(b), &state).err(),
                Some(Declined::Mismatch)
            );
            assert!(STATS.id_mismatch.load(Ordering::Relaxed) > before);
            assert!(state.relative.load(Ordering::Relaxed)); // one mismatch does not end them
        }
        drop(parent);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn one_id_for_several_subdirectories_is_not_trusted() {
        let sub = |id: u128, bits: u8| {
            (
                0u32,
                NativePath::from(vec![b'x' as u16]),
                Hint {
                    id,
                    bits,
                    name_len: 1,
                },
            )
        };
        assert!(!shares_ids(&[sub(5, 128), sub(6, 128)]));
        assert!(!shares_ids(&[sub(5, 128)]));
        assert!(!shares_ids(&[sub(5, 0), sub(5, 0)])); // no ids at all: nothing to trust
        assert!(shares_ids(&[sub(7, 128), sub(5, 128), sub(7, 128)])); // WinBtrfs FileIdExtd
        let state = RootState::new(true, true);
        distrust_ids(&state, 0);
        assert_eq!(state.level.load(Ordering::Relaxed), 1);
        assert!(state.relative.load(Ordering::Relaxed));
        distrust_ids(&state, 1);
        assert!(!state.relative.load(Ordering::Relaxed));
    }

    #[test]
    fn subdirectories_open_from_their_parent_only_by_a_checked_single_name() {
        let named = |name: &'static [u16], id: u128, id_bits: u8| {
            hint_of(&Raw {
                id,
                id_bits,
                name,
                ..raw(FA_DIRECTORY, 0, 0)
            })
        };
        static PLAIN: [u16; 3] = [b'b' as u16, b'i' as u16, b'n' as u16];
        static BACKSLASH: [u16; 3] = [b'a' as u16, BS, b'b' as u16];
        static SLASHED: [u16; 3] = [b'a' as u16, SLASH, b'b' as u16];
        assert_eq!(
            named(&PLAIN, 7 << 64 | 5, 128),
            Hint {
                id: 7 << 64 | 5,
                bits: 128,
                name_len: 3
            }
        );
        assert_eq!(named(&PLAIN, 5, 64).bits, 64);
        // no id to check the open against: by full path
        assert_eq!(named(&PLAIN, 0, 128), Hint::default());
        assert_eq!(named(&PLAIN, 5, 0), Hint::default());
        // a name that is a path of its own (WinBtrfs shows some Linux names with a backslash)
        assert_eq!(named(&BACKSLASH, 5, 128), Hint::default());
        assert_eq!(named(&SLASHED, 5, 128), Hint::default());
        let parent = Arc::new(Handle(ptr::null_mut()));
        assert!(via(&parent, Hint::default()).is_none());
        assert!(via(&parent, named(&PLAIN, 5, 64)).is_some());
        std::mem::forget(Arc::into_inner(parent)); // not a real handle: never closed
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
        let state = RootState::new(true, true);
        let err = open_dir(&w("C:\\definitely-not-here-spaceknife"), &state)
            .err()
            .unwrap();
        assert_eq!(err, win_message(ERROR_PATH_NOT_FOUND));
    }
}
