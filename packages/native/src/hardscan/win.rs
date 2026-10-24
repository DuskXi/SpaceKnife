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
//!
//! Subfolders whose name another entry of the listing shares in another case (or exactly: WinBtrfs
//! shows invalid UTF-8 as U+FFFD) follow the stricter rules of case.rs: listed only through a
//! handle proven to be that entry, else opened by their file id relative to the parent, else a
//! listing error. A folder opened by file id is *pinned*: no path leads there, so everything below
//! it is opened relative to handles of this call only, never by path, and never handed back as a
//! task.

use std::cell::RefCell;
use std::mem::offset_of;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use windows_sys::Wdk::Foundation::{NtClose, NtQueryObject, OBJECT_ATTRIBUTES, OBJECT_NAME_INFORMATION};
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_BASIC_INFORMATION, FILE_BOTH_DIR_INFORMATION, FILE_DIRECTORY_FILE,
    FILE_FS_ATTRIBUTE_INFORMATION, FILE_ID_BOTH_DIR_INFORMATION, FILE_ID_EXTD_DIR_INFORMATION, FILE_ID_INFORMATION,
    FILE_INFORMATION_CLASS, FILE_INTERNAL_INFORMATION, FILE_NAME_INFORMATION, FILE_OPEN_BY_FILE_ID,
    FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    FileBasicInformation, FileBothDirectoryInformation, FileFsDeviceInformation,
    FileIdBothDirectoryInformation, FileIdExtdDirectoryInformation, FileIdInformation,
    FileFsAttributeInformation, FileInternalInformation, FileNameInformation,
    FileNormalizedNameInformation, NtOpenFile, NtQueryDirectoryFile,
    NtQueryInformationFile, NtQueryVolumeInformationFile, NtSetInformationFile,
};
use windows_sys::Wdk::System::SystemServices::{
    FILE_FS_DEVICE_INFORMATION, FILE_REMOTE_DEVICE, RtlUpcaseUnicodeChar,
};
use windows_sys::Win32::Foundation::{
    ERROR_FILENAME_EXCED_RANGE, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, GetLastError, HANDLE, NTSTATUS,
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
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::case::{self, CaseKey, Class, First, Outcome, Then, fs, mark, msg};
use super::common::{Arena, NativePath, RootOpts, Row, StateOpts, ext_len_with, flag, kind, secs};

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
/// The buffer of FileNameInformation queries (`with_stored_name`).
const NAME_BYTES: usize = 64 * 1024;
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
    /// once the longer one passes the limit. UNKNOWN until the first open by path measures it, and
    /// if that fails (then neither relative opens nor folders below one opened by file id).
    expansion: AtomicU32,
    /// The root's file system (`case::fs`), learnt on its first open by path.
    fs: AtomicU8,
    /// Tasks must be the folders their paths name, by the stored names below the root (Btrfs,
    /// where names fold onto case-sensitive storage, or the root's `verify_names`).
    verify_names: AtomicBool,
    /// The scan root's own stored name, read once per call for `check_task` (None: unreadable).
    root_name: OnceLock<Option<Box<[u16]>>>,
    /// Leave out the last query of a listing whose buffer had room for another entry, on local
    /// NTFS, ReFS and Btrfs (`skips_final`); with `verify_final` send it anyway and count it.
    skip_final: bool,
    verify_final: bool,
    /// `skip_final` checks itself: the first FINAL_CHECKED listings of the root in this call that
    /// could leave the query out, then one in FINAL_CHECKED, send it anyway; one that returns
    /// entries turns it off for the root (`final_ok`).
    final_ok: AtomicBool,
    final_seen: AtomicU32,
    /// Check a relative open by the file id of the "." entry its listing starts with, before
    /// querying the handle for it (`list_dir`).
    dot_check: bool,
    /// Ask for FILE_WRITE_ATTRIBUTES only where the file system updates access times
    /// (`atime_updated`; learnt with the file system on the root's first open by path).
    keep_where_updated: bool,
    /// Test seams (private options; docs/native.md 2.2): strict subfolders are opened by their
    /// upcased name (in a case-sensitive NTFS folder that reaches the upper twin, as WinBtrfs's
    /// lookup does), and FileIdExtd listings give every entry the listed folder's own id (as
    /// WinBtrfs's do).
    fold_strict: bool,
    extd_self_ids: bool,
}

const UNKNOWN: u32 = u32::MAX;

impl RootState {
    pub fn new(o: &StateOpts) -> Self {
        Self {
            level: AtomicU8::new(0),
            keep: AtomicBool::new(o.keep_access_time),
            kept: AtomicU32::new(0),
            refused: AtomicU32::new(0),
            relative: AtomicBool::new(o.relative_open),
            matched: AtomicU32::new(0),
            mismatched: AtomicU32::new(0),
            failed: AtomicU32::new(0),
            expansion: AtomicU32::new(UNKNOWN),
            fs: AtomicU8::new(fs::UNKNOWN),
            verify_names: AtomicBool::new(o.verify_names),
            root_name: OnceLock::new(),
            skip_final: o.skip_final,
            verify_final: o.verify_final,
            final_ok: AtomicBool::new(true),
            final_seen: AtomicU32::new(0),
            dot_check: o.dot_check,
            keep_where_updated: o.keep_where_updated,
            fold_strict: o.fold_strict,
            extd_self_ids: o.extd_self_ids,
        }
    }

    fn btrfs(&self) -> bool {
        self.fs.load(Ordering::Acquire) == fs::BTRFS
    }
}

/// How directories were opened, process-wide (`_open_stats`: tests and measurements).
pub struct OpenStats {
    /// Opened by name relative to the parent, and the handle had the listed file id (subfolders no
    /// other entry shares its name with, not below a folder opened by file id: the others count
    /// under `strict` and `pinned`).
    pub relative: AtomicU64,
    /// Opened by full path (tasks, and everything a relative open did not settle).
    pub by_path: AtomicU64,
    /// A relative open failed (the open by full path then gave the error, or succeeded).
    pub relative_failed: AtomicU64,
    /// A relative open reached a folder with another file id than the listing gave.
    pub id_mismatch: AtomicU64,
    /// The handle of a relative open (as for `relative`) had no file id to read: the root went back
    /// to opens by path.
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
    /// Found subfolders whose name another entry shares (TWIN, DUP, BADNAME): opened under the
    /// strict rule; of them, those opened by file id and those that became a listing error.
    pub strict: AtomicU64,
    pub strict_by_id: AtomicU64,
    pub strict_failed: AtomicU64,
    /// A plain relative open reached a folder with another file id and another stored name (a
    /// twin the upcase did not catch, on a volume with its own upcase table; a name that is another
    /// folder's 8.3 short name), opened by file id then.
    pub mismatch_renamed: AtomicU64,
    /// Folders listed through a handle opened by file id or below one; and folders left unread
    /// because the call's cap on what is listed past the budget was reached (`_pinned_cap`: such
    /// folders, and folders whose name another entry shares).
    pub pinned: AtomicU64,
    pub pinned_cap_hit: AtomicU64,
    /// Tasks whose folder is stored under other names than their path's (a listing error), and
    /// tasks whose stored name could not be read (listed unchecked).
    pub task_name_mismatch: AtomicU64,
    pub task_name_unknown: AtomicU64,
    /// Btrfs: a listing whose names do not add up to the size its parent's listing gave the folder
    /// (its i_size, 2 bytes per byte of every name): opened by name (counted only), or by file id
    /// (a listing error).
    pub btrfs_size_mismatch: AtomicU64,
    pub by_id_incomplete: AtomicU64,
    /// Listings whose last query was left out (`skip_final`); with `_verify_final`, those sent
    /// anyway that found nothing more, and those that still returned entries (expected: 0).
    pub final_skipped: AtomicU64,
    pub final_verified: AtomicU64,
    pub final_wrong: AtomicU64,
    /// Relative opens checked by their listing's "." entry, and those it could not settle ("."
    /// missing, or with another id or width): checked by querying the handle then.
    pub dot_matched: AtomicU64,
    pub dot_unmatched: AtomicU64,
    /// Roots (per call) whose file system does not update access times by the system's setting:
    /// no FILE_WRITE_ATTRIBUTES there (`keep_where_updated`).
    pub keep_off: AtomicU64,
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
    strict: AtomicU64::new(0),
    strict_by_id: AtomicU64::new(0),
    strict_failed: AtomicU64::new(0),
    mismatch_renamed: AtomicU64::new(0),
    pinned: AtomicU64::new(0),
    pinned_cap_hit: AtomicU64::new(0),
    task_name_mismatch: AtomicU64::new(0),
    task_name_unknown: AtomicU64::new(0),
    btrfs_size_mismatch: AtomicU64::new(0),
    by_id_incomplete: AtomicU64::new(0),
    final_skipped: AtomicU64::new(0),
    final_verified: AtomicU64::new(0),
    final_wrong: AtomicU64::new(0),
    dot_matched: AtomicU64::new(0),
    dot_unmatched: AtomicU64::new(0),
    keep_off: AtomicU64::new(0),
};

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

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
        ("strict", get(&s.strict)),
        ("strict_by_id", get(&s.strict_by_id)),
        ("strict_failed", get(&s.strict_failed)),
        ("mismatch_renamed", get(&s.mismatch_renamed)),
        ("pinned", get(&s.pinned)),
        ("pinned_cap_hit", get(&s.pinned_cap_hit)),
        ("task_name_mismatch", get(&s.task_name_mismatch)),
        ("task_name_unknown", get(&s.task_name_unknown)),
        ("btrfs_size_mismatch", get(&s.btrfs_size_mismatch)),
        ("by_id_incomplete", get(&s.by_id_incomplete)),
        ("final_skipped", get(&s.final_skipped)),
        ("final_verified", get(&s.final_verified)),
        ("final_wrong", get(&s.final_wrong)),
        ("dot_matched", get(&s.dot_matched)),
        ("dot_unmatched", get(&s.dot_unmatched)),
        ("keep_off", get(&s.keep_off)),
    ]
}

/// walk.rs: a folder no task's path could stand for was left unread (the call's cap).
pub fn cap_hit() {
    bump(&STATS.pinned_cap_hit);
}

/// What a found subdirectory keeps from its parent's listing to be opened from it: the file id
/// the listing gave (`bits`: 128 from FileIdExtd, 64 from FileIdBoth, 0 = none or not one to
/// trust), the length of its name (the last `name_len` units of its path; 0 when the name is not a
/// single component), its `case::mark` bits and, on Btrfs, the size its parent's listing gave it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hint {
    id: u128,
    bits: u8,
    name_len: u16,
    mark: u8,
    size: i64,
}

impl Hint {
    /// The listing gave a file id to check an open against (not 0, not all ones, not shared, not a
    /// reparse point's: those are cleared).
    fn trusted(&self) -> bool {
        self.bits != 0
    }
}

/// walk.rs: how strictly a found subfolder must be opened (`case::mark`).
pub fn marks(hint: &Hint) -> u8 {
    hint.mark
}

/// A listed directory's open handle, kept for its subdirectories (walk.rs drops it once the last
/// of them has started).
pub type Parent = Handle;

/// How a found subdirectory is opened: by name relative to its parent's handle, or by its file id
/// relative to it. `pinned`: the parent was opened by file id or lies below such a folder.
pub struct Via {
    parent: Arc<Parent>,
    hint: Hint,
    pinned: bool,
}

pub fn via(parent: &Arc<Parent>, hint: Hint, pinned: bool) -> Option<Via> {
    (pinned || hint.mark & mark::STRICT != 0 || hint.bits != 0).then(|| Via {
        parent: parent.clone(),
        hint,
        pinned,
    })
}

thread_local! {
    static BUF: RefCell<Vec<u64>> = RefCell::new(vec![0u64; BUF_BYTES / 8]);
    static SCRATCH: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
    /// One listing's entries for `case::mark_twins` (not BUF, borrowed for the whole listing, nor
    /// SCRATCH, which compressed_size and the opens use).
    static CASE: RefCell<Vec<CaseKey>> = const { RefCell::new(Vec::new()) };
    static MARKS: RefCell<Vec<(u32, u8)>> = const { RefCell::new(Vec::new()) };
    /// FileNameInformation of a handle (stored names).
    static NAME: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

/// RtlUpcaseUnicodeChar of every UTF-16 unit: how Windows (and WinBtrfs, which calls
/// RtlUpcaseUnicodeString) compares names. Built once per process (65,536 calls, well under 1 ms).
fn upcase() -> &'static [u16] {
    static UPCASE: OnceLock<Box<[u16]>> = OnceLock::new();
    UPCASE.get_or_init(|| {
        // SAFETY: a pure function of its argument.
        (0..=u16::MAX).map(|u| unsafe { RtlUpcaseUnicodeChar(u) }).collect()
    })
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

/// Opens directory `path` by its full path (the caller counts the handle it lists through:
/// `count_keep`, `STATS.by_path`). The first open of a root learns its file system (`detect`).
fn open_dir(path: &[u16], state: &RootState) -> Result<(Handle, Keep), String> {
    open_path(path, 0, state)
}

/// `open_dir` with more create options (FILE_OPEN_REPARSE_POINT).
fn open_path(path: &[u16], options: u32, state: &RootState) -> Result<(Handle, Keep), String> {
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
        let (h, keep) = open_with(&oa, options, state).map_err(open_error)?;
        if state.fs.load(Ordering::Acquire) == fs::UNKNOWN {
            detect(&h, nt.len(), state);
        }
        Ok((h, keep))
    })
}

/// The message of a failed open, as FindFirstFileW gives it (a missing directory is "path not found").
fn open_error(status: NTSTATUS) -> String {
    if status == STATUS_OBJECT_NAME_NOT_FOUND {
        win_message(ERROR_PATH_NOT_FOUND)
    } else {
        status_message(status)
    }
}

/// Learns, once per root and call, how its folders are opened (docs/native.md 2.2):
/// - relative opens only where they were measured to list the same and to pay or cost nothing:
///   NTFS, ReFS and Btrfs. FAT, exFAT and network drives keep the open by path they always had
///   (an SMB share reports the server's file system name, so the device decides "remote");
/// - Btrfs lists with FileIdBothDirectoryInformation from the first listing on (WinBtrfs's
///   FileIdExtd ids are the listed folder's own) and checks tasks by their stored names;
/// - how much longer the kernel's name is than the NT path (`expansion`), whether or not relative
///   opens are on: folders below one opened by file id need it. Without it, no relative opens;
/// - whether its file system updates access times at all (`atime_updated`): where it does not,
///   keeping them is left out (`keep_where_updated`).
fn detect(h: &Handle, nt_len: usize, state: &RootState) {
    let kind = fs_kind(h);
    if state.keep_where_updated
        && !atime_updated(kind, atime_settings())
        && state.keep.swap(false, Ordering::Relaxed)
    {
        bump(&STATS.keep_off);
    }
    if kind == fs::BTRFS {
        let _ = state
            .level
            .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed);
        state.verify_names.store(true, Ordering::Relaxed);
    }
    if !matches!(kind, fs::NTFS | fs::REFS | fs::BTRFS) {
        state.relative.store(false, Ordering::Relaxed);
    }
    match kernel_name_len(h) {
        Some(k) => state
            .expansion
            .store(k.saturating_sub(nt_len) as u32, Ordering::Relaxed),
        None => state.relative.store(false, Ordering::Relaxed),
    }
    state.fs.store(kind, Ordering::Release);
}

/// The system's access-time settings, read once per process (they take effect at the next boot):
/// `NtfsDisableLastAccessUpdate` and `RefsDisableLastAccessUpdate` under
/// HKLM\SYSTEM\CurrentControlSet\Control\FileSystem, None where a value is missing or unreadable.
fn atime_settings() -> (Option<u32>, Option<u32>) {
    static SETTINGS: OnceLock<(Option<u32>, Option<u32>)> = OnceLock::new();
    *SETTINGS.get_or_init(|| {
        let key: Vec<u16> = "SYSTEM\\CurrentControlSet\\Control\\FileSystem\0".encode_utf16().collect();
        let read = |name: &str| -> Option<u32> {
            let value: Vec<u16> = name.encode_utf16().chain([0]).collect();
            let mut data = 0u32;
            let mut size = size_of::<u32>() as u32;
            // SAFETY: NUL-terminated strings; `data` is writable for `size` bytes.
            let status = unsafe {
                RegGetValueW(
                    HKEY_LOCAL_MACHINE,
                    key.as_ptr(),
                    value.as_ptr(),
                    RRF_RT_REG_DWORD,
                    ptr::null_mut(),
                    (&raw mut data).cast(),
                    &mut size,
                )
            };
            (status == ERROR_SUCCESS).then_some(data)
        };
        (read("NtfsDisableLastAccessUpdate"), read("RefsDisableLastAccessUpdate"))
    })
}

/// Whether a file system updates the access times of folders, by the system's settings `(ntfs,
/// refs)` (`atime_settings`): yes unless a setting plainly says no.
/// - NTFS: no for the `fsutil behavior set disablelastaccess` values 1 (user managed, disabled)
///   and 3 (system managed, disabled), stored as they are or with the high bit Windows sets on
///   values it manages; 0 and 2 (enabled), anything else and no value: yes.
/// - ReFS: no for `RefsDisableLastAccessUpdate` = 1 (ReFS's default, but only a value that is
///   there counts).
/// - Any other file system: yes.
///
/// A setting changed since the last boot is not in effect yet: until the next boot a volume set to
/// "disabled" still updates access times, and a scan then lists its folders without keeping them
/// (as before 0.3.4); one set to "enabled" is kept as usual.
fn atime_updated(kind: u8, (ntfs, refs): (Option<u32>, Option<u32>)) -> bool {
    match kind {
        fs::NTFS => !matches!(ntfs.map(|v| v & 0x7FFF_FFFF), Some(1 | 3)),
        fs::REFS => refs != Some(1),
        _ => true,
    }
}

/// The file system of the volume `h` is on (`case::fs`).
fn fs_kind(h: &Handle) -> u8 {
    let mut dev = FILE_FS_DEVICE_INFORMATION::default();
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: `dev` is writable for its size.
    let status = unsafe {
        NtQueryVolumeInformationFile(
            h.0,
            &mut iosb,
            (&raw mut dev).cast(),
            size_of::<FILE_FS_DEVICE_INFORMATION>() as u32,
            FileFsDeviceInformation,
        )
    };
    let remote = status >= 0 && dev.Characteristics & FILE_REMOTE_DEVICE != 0;
    let mut buf = [0u64; 64]; // FILE_FS_ATTRIBUTE_INFORMATION and a name of up to 240 units
    // SAFETY: buf is writable for its whole length.
    let status = unsafe {
        NtQueryVolumeInformationFile(
            h.0,
            &mut iosb,
            buf.as_mut_ptr().cast(),
            size_of_val(&buf) as u32,
            FileFsAttributeInformation,
        )
    };
    if status < 0 {
        return fs::OTHER;
    }
    // SAFETY: on success the buffer starts with a FILE_FS_ATTRIBUTE_INFORMATION whose name follows
    let info = unsafe { &*buf.as_ptr().cast::<FILE_FS_ATTRIBUTE_INFORMATION>() };
    let units = (info.FileSystemNameLength as usize / 2)
        .min((size_of_val(&buf) - offset_of!(FILE_FS_ATTRIBUTE_INFORMATION, FileSystemName)) / 2);
    // SAFETY: `units` UTF-16 units of the name lie inside buf
    let name = unsafe {
        std::slice::from_raw_parts(
            buf.as_ptr()
                .cast::<u8>()
                .add(offset_of!(FILE_FS_ATTRIBUTE_INFORMATION, FileSystemName))
                .cast::<u16>(),
            units,
        )
    };
    case::fs_of(name, remote)
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
/// check) does not count. `options`: FILE_OPEN_BY_FILE_ID, FILE_OPEN_REPARSE_POINT, or 0.
fn open_with(oa: &OBJECT_ATTRIBUTES, options: u32, state: &RootState) -> Result<(Handle, Keep), NTSTATUS> {
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
                FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_FOR_BACKUP_INTENT | options,
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

/// A found subfolder's open handle, and how it was opened.
struct Opened {
    h: Handle,
    keep: Keep,
    by: By,
    /// A plain relative open: a listing that fails through it is done again by full path.
    relist: bool,
    /// A plain relative open not checked yet: the "." entry of its listing checks it, or the file
    /// id of the handle when that does not settle it (`list_dir`).
    unchecked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum By {
    Path,
    /// By name relative to the parent, checked by the listed file id (`open_relative`).
    Relative,
    /// By name relative to the parent under the rules of case.rs (`open_checked`).
    Named,
    Id,
}

impl Opened {
    fn new(h: Handle, keep: Keep, by: By) -> Self {
        Self {
            h,
            keep,
            by,
            relist: false,
            unchecked: false,
        }
    }

    /// A plain relative open whose handle is the listed folder.
    fn relative(h: Handle, keep: Keep) -> Self {
        Self {
            relist: true,
            ..Self::new(h, keep, By::Relative)
        }
    }
}

/// Counts the handle a listing goes through (only that one: an open given up does not count).
fn count_open(state: &RootState, o: &Opened) {
    count_keep(state, o.keep);
    match o.by {
        By::Path => bump(&STATS.by_path),
        By::Relative => bump(&STATS.relative),
        By::Named | By::Id => {} // `strict`, `pinned`
    }
}

/// Opens subdirectory `path`, found in the listing of the directory `via` holds open. A plain one
/// relative to it when that can be checked (`open_plain`), else by full path like a task; one
/// whose name another entry shares, or that lies below a folder opened by file id, only through a
/// handle proven to be the listed entry (`open_checked`, rules in case.rs). `dot`: a plain
/// relative open is left to be checked by its listing ("." entry, `list_dir`).
fn open_child(path: &[u16], via: &Via, state: &RootState, dot: bool) -> Result<Opened, String> {
    let class = case::class_of(via.hint.mark, via.pinned);
    if class == Class::Plain {
        return open_plain(path, via, state, dot);
    }
    let res = open_checked(path, via, class, state);
    if class == Class::Strict {
        bump(&STATS.strict);
        match &res {
            Ok(o) if o.by == By::Id => bump(&STATS.strict_by_id),
            Ok(_) => {}
            Err(_) => bump(&STATS.strict_failed),
        }
    }
    res
}

/// A plain subfolder: relative to its parent when relative opens are on and its path is one the
/// open by path would accept, then as `case::after_open` says for `Class::Plain` (`settle_plain`);
/// otherwise, and for whatever that leaves, by full path (which also reports any error). With
/// `dot` a relative open that works is returned unchecked: its listing checks it (`list_dir`).
fn open_plain(path: &[u16], via: &Via, state: &RootState, dot: bool) -> Result<Opened, String> {
    if state.relative.load(Ordering::Relaxed) && within_limit(path, via, state) {
        let (outcome, handle) = match open_plain_named(path, via, state) {
            Some((h, keep)) if dot => {
                return Ok(Opened {
                    unchecked: true,
                    ..Opened::relative(h, keep)
                });
            }
            Some((h, keep)) => check_relative(h, keep, path, via, state),
            None => (Outcome::Failed, None),
        };
        return settle_plain(path, via, state, outcome, handle);
    }
    let (h, keep) = open_dir(path, state)?;
    Ok(Opened::new(h, keep, By::Path))
}

/// How a plain subfolder is opened once its relative open found `outcome` (`case::after_open`,
/// `Class::Plain`): listed through that handle, by the listed file id (the name reached another
/// entry), or by full path.
fn settle_plain(
    path: &[u16],
    via: &Via,
    state: &RootState,
    outcome: Outcome,
    handle: Option<(Handle, Keep)>,
) -> Result<Opened, String> {
    match (case::after_open(Class::Plain, outcome, via.hint.mark, via.hint.trusted()), handle) {
        (Then::List, Some((h, keep))) => return Ok(Opened::relative(h, keep)),
        (Then::ById, _) => {
            bump(&STATS.mismatch_renamed);
            return open_by_id(via, state, None);
        }
        (then, _) => {
            if matches!(outcome, Outcome::Other { .. }) && then == Then::ByPath {
                // now and then a folder replaced meanwhile; again and again: ids that do not belong
                let n = state.mismatched.fetch_add(1, Ordering::Relaxed) + 1;
                if n > 64 && n > state.matched.load(Ordering::Relaxed) {
                    distrust_ids(state, if via.hint.bits == 128 { 0 } else { 1 });
                }
            }
        }
    }
    let (h, keep) = open_dir(path, state)?;
    if outcome == Outcome::Failed {
        bump(&STATS.path_after_failed);
    }
    Ok(Opened::new(h, keep, By::Path))
}

/// The open of a plain subdirectory by its name relative to its parent's handle (the NT form of
/// openat), and what it found; the handle only when it is the folder the parent's listing named
/// (`check_relative`).
#[cfg(test)]
fn open_relative(path: &[u16], via: &Via, state: &RootState) -> (Outcome, Option<(Handle, Keep)>) {
    match open_plain_named(path, via, state) {
        Some((h, keep)) => check_relative(h, keep, path, via, state),
        None => (Outcome::Failed, None),
    }
}

/// The relative open of a plain subdirectory by its name. When it fails (gone, denied) it is
/// counted on the root: failures outnumbering matches end relative opens here.
fn open_plain_named(path: &[u16], via: &Via, state: &RootState) -> Option<(Handle, Keep)> {
    let Ok(opened) = open_named(path, via, false, 0, state) else {
        bump(&STATS.relative_failed);
        // now and then a folder deleted meanwhile; again and again: relative opens do not work here
        let n = state.failed.fetch_add(1, Ordering::Relaxed) + 1;
        if n > 64 && n > state.matched.load(Ordering::Relaxed) {
            state.relative.store(false, Ordering::Relaxed);
        }
        return None;
    };
    Some(opened)
}

/// Whether the handle of a plain relative open is the folder the parent's listing named, i.e.
/// carries the file id that listing gave the name (queried from the handle). Counted on the root:
/// - another file id (`id_mismatch`): with it, whether its stored name is the listed one. Under
///   the listed name the folder was replaced meanwhile, or the listing's ids do not belong to its
///   entries (counted by `settle_plain`, which may distrust that class's ids); under another name
///   the name reached another entry (a case twin the upcase did not catch, on a volume with its own
///   upcase table; an 8.3 short name), which goes by file id instead;
/// - the file id cannot be read: such opens cannot be checked, so this root stops making them.
fn check_relative(
    h: Handle,
    keep: Keep,
    path: &[u16],
    via: &Via,
    state: &RootState,
) -> (Outcome, Option<(Handle, Keep)>) {
    match file_id(&h, via.hint.bits) {
        Some(id) if id == via.hint.id => {
            state.matched.fetch_add(1, Ordering::Relaxed);
            (Outcome::Same, Some((h, keep)))
        }
        Some(_) => {
            bump(&STATS.id_mismatch);
            let named = stored_name_is(&h, listed_name(path, via));
            (Outcome::Other { named }, None)
        }
        None => {
            bump(&STATS.id_unknown);
            state.relative.store(false, Ordering::Relaxed);
            (Outcome::Unreadable, None)
        }
    }
}

/// The open of a strict subfolder (TWIN, DUP, BADNAME), or of one below a folder opened by file id
/// (pinned), by the rules of case.rs. A strict one is opened by name as a plain one is (relative
/// where relative opens are on, else by full path), a pinned one only relative to its parent: no
/// path leads there. Unless the listed entry is itself a reparse point that is descended, the name
/// is opened as what it reaches, never followed: a twin that is a symlink or a junction is then
/// another entry, not the folder it points to (which can have the listed name). The handle is
/// listed when it is proven to be the listed entry (its file id is the listed one; without an id
/// to check, its stored name is the listed name); otherwise the folder is opened by its file id,
/// or it is a listing error. None of this counts toward the root's matches and mismatches: twins
/// are expected not to match.
fn open_checked(path: &[u16], via: &Via, class: Class, state: &RootState) -> Result<Opened, String> {
    let hint = &via.hint;
    let fold = state.fold_strict && class == Class::Strict;
    let own = if hint.mark & mark::REPARSE != 0 {
        0
    } else {
        FILE_OPEN_REPARSE_POINT
    };
    let opened = match case::first_open(hint.mark, hint.trusted(), via.pinned) {
        First::Fail(why) => return Err(why.to_owned()),
        First::ById => return open_by_id(via, state, None),
        // past what an open by path accepts: its error, so walks below pinned folders stay bounded
        First::Relative if !within_limit(path, via, state) => {
            return Err(win_message(ERROR_FILENAME_EXCED_RANGE));
        }
        First::Relative => open_named(path, via, fold, own, state)
            .map(|(h, keep)| Opened::new(h, keep, By::Named))
            .map_err(open_error),
        First::Name if state.relative.load(Ordering::Relaxed) && within_limit(path, via, state) => {
            open_named(path, via, fold, own, state)
                .map(|(h, keep)| Opened::new(h, keep, By::Named))
                .map_err(open_error)
        }
        First::Name | First::Path => {
            let target = if fold { folded(path, via) } else { path.to_vec() };
            open_path(&target, own, state).map(|(h, keep)| Opened::new(h, keep, By::Path))
        }
    };
    let outcome = match &opened {
        Ok(o) => outcome_of(&o.h, path, via),
        Err(_) => Outcome::Failed,
    };
    match case::after_open(class, outcome, hint.mark, hint.trusted()) {
        Then::List | Then::Fail | Then::ByPath => opened,
        Then::StoredName => {
            let o = opened?;
            let named = stored_name_is(&o.h, listed_name(path, via)) == Some(true);
            case::by_stored_name(named, hint.mark).map_err(str::to_owned)?;
            Ok(o)
        }
        Then::ById => open_by_id(via, state, opened.err()),
    }
}

/// What the open by name of a strict or pinned subfolder found.
fn outcome_of(h: &Handle, path: &[u16], via: &Via) -> Outcome {
    let hint = &via.hint;
    if !hint.trusted() {
        return Outcome::NoId;
    }
    match file_id(h, hint.bits) {
        Some(id) if id == hint.id => Outcome::Same,
        Some(_) => Outcome::Other {
            named: stored_name_is(h, listed_name(path, via)),
        },
        None => Outcome::Unreadable,
    }
}

/// The subfolder's name as its parent listed it: the last `name_len` units of its path.
fn listed_name<'a>(path: &'a [u16], via: &Via) -> &'a [u16] {
    &path[path.len() - (via.hint.name_len as usize).min(path.len())..]
}

/// `path` with its last name upcased (the `_fold_strict` test seam).
fn folded(path: &[u16], via: &Via) -> Vec<u16> {
    let up = upcase();
    let at = path.len() - listed_name(path, via).len();
    let mut out = path.to_vec();
    for u in &mut out[at..] {
        *u = case::upcase_unit(*u, up);
    }
    out
}

/// Whether `path` may be opened relative to its parent: no deeper than the open by full path
/// reaches. Its NT path and the kernel's expanded name keep a few units clear of the limit (near
/// it the open by path decides itself); a path Win32 must normalise first, or a root whose
/// expansion could not be measured, is left to the open by path.
fn within_limit(path: &[u16], via: &Via, state: &RootState) -> bool {
    let n = via.hint.name_len as usize;
    if n == 0 || n >= path.len() {
        return false;
    }
    let expansion = state.expansion.load(Ordering::Relaxed);
    if expansion == UNKNOWN {
        return false;
    }
    SCRATCH.with_borrow_mut(|nt| {
        nt_form(path, nt)
            && nt.len() * 2 <= u16::MAX as usize - 2
            && nt.len() + expansion as usize + LIMIT_MARGIN <= u16::MAX as usize / 2
    })
}

/// NtOpenFile of a found subfolder by its one name relative to its parent's handle (the NT form of
/// openat); with `fold`, by its upcased name (test seam). `options` as for `open_with`.
fn open_named(
    path: &[u16],
    via: &Via,
    fold: bool,
    options: u32,
    state: &RootState,
) -> Result<(Handle, Keep), NTSTATUS> {
    let listed = listed_name(path, via);
    let upcased: Vec<u16>;
    let name = if fold {
        let up = upcase();
        upcased = listed.iter().map(|&u| case::upcase_unit(u, up)).collect();
        &upcased[..]
    } else {
        listed
    };
    let us = UNICODE_STRING {
        Length: (name.len() * 2) as u16,
        MaximumLength: (name.len() * 2) as u16,
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
    open_with(&oa, options, state)
}

/// Opens a found subfolder by the file id its parent's listing gave it, relative to the parent's
/// handle (FILE_OPEN_BY_FILE_ID, with the access, sharing, options and access-time handling of
/// every open). The id goes as listed (`case::id_name`; on Btrfs as {inode, subvolume}). Accepted
/// only when the handle has that id and it is not the parent's own: a listing whose ids are its
/// own folder's (WinBtrfs's FileIdExtd) would otherwise list the parent again, and again. The
/// folder is then pinned. `name_error`: the error of the open by name, if that failed: the error
/// when this fails too.
fn open_by_id(via: &Via, state: &RootState, name_error: Option<String>) -> Result<Opened, String> {
    let hint = &via.hint;
    if !hint.trusted() {
        return Err(case::by_id_failed(hint.mark, name_error));
    }
    let (bytes, n) = case::id_name(hint.id, hint.bits, state.btrfs());
    let mut buf = [0u64; 2]; // aligned for the UNICODE_STRING that carries it
    // SAFETY: 16 bytes into a 16-byte buffer.
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), buf.as_mut_ptr().cast::<u8>(), bytes.len()) };
    let us = UNICODE_STRING {
        Length: n as u16,
        MaximumLength: n as u16,
        Buffer: buf.as_mut_ptr().cast(),
    };
    let oa = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: via.parent.0,
        ObjectName: &us,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let Ok((h, keep)) = open_with(&oa, FILE_OPEN_BY_FILE_ID, state) else {
        return Err(case::by_id_failed(hint.mark, name_error));
    };
    if file_id(&h, hint.bits) != Some(hint.id) || file_id(&via.parent, hint.bits) == Some(hint.id) {
        return Err(case::by_id_failed(hint.mark, name_error));
    }
    Ok(Opened::new(h, keep, By::Id))
}

/// Calls `f` with the stored name of what `h` refers to (its path below the volume's root as the
/// file system stores it, e.g. `\usr\share\terminfo\a`), or None when it cannot be read. It is the
/// normalised name (FileNormalizedNameInformation: NTFS gives long names for 8.3 short names the
/// open went through; GetFinalPathNameByHandleW's default) where the file system has it, else
/// FileNameInformation.
fn with_stored_name<R>(h: &Handle, f: impl FnOnce(Option<&[u16]>) -> R) -> R {
    NAME.with_borrow_mut(|buf| {
        if buf.is_empty() {
            // exactly 64 KiB, room for any name of an NT path: WinBtrfs refuses a buffer of a few
            // bytes more (STATUS_BUFFER_OVERFLOW for 65,540 bytes, both classes, measured 2026-10-22)
            buf.resize(NAME_BYTES / 8, 0);
        }
        let mut status = STATUS_INVALID_INFO_CLASS;
        for class in [FileNormalizedNameInformation, FileNameInformation] {
            let mut iosb = IO_STATUS_BLOCK::default();
            // SAFETY: buf is writable for its whole length.
            status = unsafe {
                NtQueryInformationFile(h.0, &mut iosb, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, class)
            };
            if !unsupported(status) {
                break;
            }
        }
        if status < 0 {
            return f(None);
        }
        // SAFETY: on success the buffer starts with a FILE_NAME_INFORMATION whose name follows
        let info = unsafe { &*buf.as_ptr().cast::<FILE_NAME_INFORMATION>() };
        let at = offset_of!(FILE_NAME_INFORMATION, FileName);
        let units = (info.FileNameLength as usize / 2).min((buf.len() * 8 - at) / 2);
        // SAFETY: `units` UTF-16 units of the name lie inside buf
        let name = unsafe {
            std::slice::from_raw_parts(buf.as_ptr().cast::<u8>().add(at).cast::<u16>(), units)
        };
        f(Some(name))
    })
}

/// Whether the last stored name of what `h` refers to is `name`, exactly (None: unreadable).
fn stored_name_is(h: &Handle, name: &[u16]) -> Option<bool> {
    with_stored_name(h, |stored| stored.map(|s| case::last_name(s) == name))
}

/// A task (a path from Python: the frontier, monitor relists, rescans) on a root whose names fold
/// onto case-sensitive storage must be the folder its path names: its stored name must be the scan
/// root's own followed by the task's names below the root, exactly (`case::task_is`). A case twin
/// that lost the open, anything below one, or a folder a twin that is a symlink led to, is a
/// listing error then, never another folder's listing under its name. Roots typed in another
/// case, SUBST drives and mount folders work: the root part is the root's own stored name.
fn check_task(h: &Handle, path: &[u16], root: &RootOpts, state: &RootState) -> Result<(), String> {
    if !state.btrfs() && !state.verify_names.load(Ordering::Relaxed) {
        return Ok(());
    }
    let Some(below) = case::below_root(path, &root.path) else {
        return Ok(()); // the root itself, or no root path to compare below
    };
    let root_name = state.root_name.get_or_init(|| {
        let (top, _) = open_dir(&root.path, state).ok()?;
        with_stored_name(&top, |s| s.map(Box::from))
    });
    with_stored_name(h, |stored| match stored {
        Some(s) if case::task_is(s, root_name.as_deref(), &below) => Ok(()),
        Some(_) => {
            bump(&STATS.task_name_mismatch);
            Err(msg::CASE.to_owned())
        }
        None => {
            bump(&STATS.task_name_unknown);
            Ok(())
        }
    })
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
    /// AllocationSize of the listing entry (the same offset in all three classes).
    alloc: i64,
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
            alloc: field!(i64, E, AllocationSize),
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
/// against (none for 0 or all ones), by a name that is a single path component (a name no path
/// can hold, `case::unnameable`, such as the `\` or `:` WinBtrfs shows in some Linux names, is
/// BADNAME, opened by its id only), and how a descended reparse point is opened. On Btrfs it also keeps the size
/// the listing gave the folder (`check_size`).
fn hint_of(raw: &Raw<'_>, btrfs: bool) -> Hint {
    let single = !case::unnameable(raw.name);
    // A reparse point that is descended (a cloud folder, ProjFS, a DFS link...) is opened by full
    // path: its listed id is the reparse point's own, so a layer that redirected only opens by
    // path would leave a relative open on the reparse point with a matching id. Its subdirectories
    // can be opened relative to it, as its handle is wherever the open by path led.
    let reparse = raw.attrs & FA_REPARSE != 0;
    let mut marks = 0;
    if !single {
        marks |= mark::BADNAME;
    }
    if reparse {
        marks |= mark::REPARSE;
    }
    let id_ok = raw.id_bits != 0 && raw.id != 0 && !case::all_ones(raw.id, raw.id_bits) && !reparse;
    Hint {
        id: if id_ok { raw.id } else { 0 },
        bits: if id_ok { raw.id_bits } else { 0 },
        name_len: if single && raw.name.len() <= u16::MAX as usize {
            raw.name.len() as u16
        } else {
            0
        },
        mark: marks,
        size: if btrfs { raw.eof } else { 0 },
    }
}

/// What the entries of one listing are read with.
#[derive(Clone, Copy)]
struct Listing {
    /// The folder was opened by file id or lies below such a folder: no path reaches its files
    /// (compressed and sparse files keep their rounded size: GetCompressedFileSizeW takes a path).
    pinned: bool,
    btrfs: bool,
    up: &'static [u16],
    /// When the listing was read (FILETIME, 100 ns since 1601; 0 = unknown).
    now: i64,
}

/// Measurement prototype (bench/method): SPACEKNIFE_WIN_ALLOC_FROM_LISTING=1 takes the size on disk
/// from the listing entry's AllocationSize (on NTFS a value that is not whole clusters is resident
/// data: 0), with no GetCompressedFileSizeW per sparse or compressed file.
fn alloc_from_listing() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPACEKNIFE_WIN_ALLOC_FROM_LISTING").is_some_and(|v| v == "1"))
}

/// The parent's copy of a file's allocation is written lazily: for a sparse file about 10–15 s after
/// the last write (measured on NTFS and ReFS, 2026-10-24). Sparse and compressed files written less
/// than this long ago are asked instead.
const LISTING_LAG_100NS: i64 = 60 * 10_000_000;

#[derive(Debug, PartialEq, Eq)]
enum ListedAlloc {
    Use(i64),
    /// Ask the file (GetCompressedFileSizeW).
    Ask,
}

/// Size on disk of a file from its listing entry (bench/method prototype, v2). `ntfs`: data that is
/// not whole clusters is resident (0). Asks the file only where the listing is known to be wrong:
/// a sparse/compressed file written in the last minute (lazy parent entry), and an NTFS file that
/// lists no clusters although it is too big to be resident and not sparse (WOF-compressed: its
/// data lives in an alternate stream, and the WOF filter hides the reparse attribute in listings).
/// WinBtrfs is never asked: its GetCompressedFileSizeW returns the logical size of sparse files.
fn listed_alloc(attrs: u32, eof: i64, alloc: i64, written: i64, how: Listing, ntfs: bool, cluster: i64) -> ListedAlloc {
    let a = alloc.max(0);
    let sparse_or_compressed = attrs & (FA_COMPRESSED | FA_SPARSE) != 0;
    let recent = how.now != 0 && how.now.saturating_sub(written) < LISTING_LAG_100NS;
    let no_clusters = ntfs && !sparse_or_compressed && a == 0 && eof > 0;
    if !how.pinned && !how.btrfs && ((sparse_or_compressed && recent) || no_clusters) {
        return ListedAlloc::Ask;
    }
    ListedAlloc::Use(if ntfs && a % cluster != 0 { 0 } else { a })
}

fn filetime_now() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64 + SECS_1601_TO_1970) * 10_000_000 + i64::from(d.subsec_nanos() / 100),
        Err(_) => 0,
    }
}

fn add_entry(
    raw: &Raw<'_>,
    dir: &[u16],
    root: &RootOpts,
    how: Listing,
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
    } else if alloc_from_listing() {
        let ntfs = root.resident_max > 0;
        match listed_alloc(attrs, raw.eof, raw.alloc, raw.write, how, ntfs, cluster) {
            ListedAlloc::Use(a) => a,
            ListedAlloc::Ask => match compressed_size(dir, raw.name) {
                // NTFS answers a resident file with its length: it has no clusters
                Some(real) if ntfs && real % cluster != 0 => 0,
                Some(real) => real.max(0),
                None => round_up(size, cluster),
            },
        }
    } else if attrs & (FA_COMPRESSED | FA_SPARSE) != 0 {
        let real = if how.pinned { None } else { compressed_size(dir, raw.name) };
        round_up(real.unwrap_or(size), cluster)
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
    CASE.with_borrow_mut(|keys| {
        keys.push(CaseKey {
            hash: case::upcase_hash(raw.name, how.up),
            start: name_start,
            len: arena.names.len() - name_start,
            j,
            dir: kind == kind::DIR,
        })
    });
    if kind == kind::DIR {
        subdirs.push((j, join_windows(dir, raw.name).into_boxed_slice(), hint_of(raw, how.btrfs)));
    }
}

/// Lists `path` into `arena` (worker._list_windows); subdirectories to descend go to `subdirs`.
/// `via`: how to open it from its listed parent (a found subdirectory). Returns the directory's
/// handle when its subdirectories can be opened from it, and whether it is pinned (opened by file
/// id, or below such a folder: its subdirectories are opened from it only).
pub fn list_dir(
    path: &[u16],
    _discovered: bool,
    via: Option<Via>,
    root: &RootOpts,
    state: &RootState,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath, Hint)>,
) -> Result<(Option<Arc<Parent>>, bool), String> {
    let mut opened = match &via {
        Some(v) => open_child(path, v, state, state.dot_check)?,
        None => {
            let (h, keep) = open_dir(path, state)?;
            check_task(&h, path, root, state)?;
            Opened::new(h, keep, By::Path)
        }
    };
    let mut pinned = opened.by == By::Id || via.as_ref().is_some_and(|v| v.pinned);
    let hint = via.as_ref().map(|v| v.hint);
    // the parent may close as soon as its last subdirectory is open (one opened unchecked keeps it
    // until its check, for what may follow)
    let held = if opened.unchecked {
        via
    } else {
        drop(via);
        None
    };
    if !opened.unchecked {
        count_open(state, &opened);
    }
    let base = arena.len();
    let mark = arena.mark();
    let first_sub = subdirs.len();
    let mut dot: DotId = None;
    let mut listed = list_into(&opened.h, path, root, state, pinned, base, arena, subdirs, &mut dot);
    if let Some(v) = held.as_ref().filter(|_| opened.unchecked) {
        // the "." entry is the listed folder itself: its file id, in the width of the parent's
        // listing, checks the open as the handle's own would (docs/native.md 2.2)
        if dot == Some((v.hint.id, v.hint.bits)) {
            bump(&STATS.dot_matched);
            state.matched.fetch_add(1, Ordering::Relaxed);
            opened.unchecked = false;
        } else {
            bump(&STATS.dot_unmatched);
            let Opened { h, keep, .. } = opened;
            let (outcome, handle) = check_relative(h, keep, path, v, state);
            opened = match handle {
                Some((h, keep)) if outcome == Outcome::Same => Opened::relative(h, keep),
                handle => {
                    // not the listed folder: what was listed through it goes
                    arena.truncate(mark);
                    subdirs.truncate(first_sub);
                    let again = settle_plain(path, v, state, outcome, handle)?;
                    pinned = again.by == By::Id;
                    listed = list_into(&again.h, path, root, state, pinned, base, arena, subdirs, &mut dot);
                    again
                }
            };
        }
        count_open(state, &opened);
    }
    drop(held);
    if pinned {
        bump(&STATS.pinned);
    }
    let Opened { mut h, by, relist, .. } = opened;
    if let Err(e) = listed {
        if !relist {
            return Err(e); // a folder no path reaches, or one a name may reach only as another
        }
        // whatever made it fail, the open by path has the last word, as for a task; it must reach
        // the folder the listing named, or the first error stands
        bump(&STATS.relisted_by_path);
        arena.truncate(mark);
        subdirs.truncate(first_sub);
        let (again, keep) = open_dir(path, state)?;
        if let Some(hint) = hint.filter(Hint::trusted) {
            if file_id(&again, hint.bits) != Some(hint.id) {
                return Err(e);
            }
        }
        count_keep(state, keep);
        bump(&STATS.by_path);
        h = again;
        list_into(&h, path, root, state, pinned, base, arena, subdirs, &mut dot)?;
    }
    if let Some(hint) = hint.filter(|h| h.size > 0) {
        check_size(&arena.names[mark.names..], hint.size, by == By::Id)?;
    }
    let subs = &subdirs[first_sub..];
    let marked = subs.iter().any(|s| s.2.mark & mark::STRICT != 0);
    let keep = !subs.is_empty() && (state.relative.load(Ordering::Relaxed) || pinned || marked);
    Ok((keep.then(|| Arc::new(h)), pinned))
}

/// Btrfs: a folder's size in its parent's listing (its i_size) is 2 bytes per byte of each of its
/// entries' names (a DIR_ITEM and a DIR_INDEX each). Checked on every listing of a found subfolder
/// whose names are all valid UTF-8 (WinBtrfs shows invalid ones as U+FFFD, which has its own
/// length). A folder opened by file id has no name filters could go by, and filters have listed
/// such folders in part without an error (docs/native.md 2.2): a mismatch there is a listing error.
/// Opened by name, it is only counted.
fn check_size(names: &[u8], size: i64, by_id: bool) -> Result<(), String> {
    if names.windows(3).any(|w| w == [0xEF, 0xBF, 0xBD]) || 2 * names.len() as i64 == size {
        return Ok(());
    }
    if by_id {
        bump(&STATS.by_id_incomplete);
        return Err(msg::INCOMPLETE.to_owned());
    }
    bump(&STATS.btrfs_size_mismatch);
    Ok(())
}

/// The longest name an entry can have on the file systems `skip_final` applies to, in UTF-16
/// units: NTFS and ReFS allow 255 per name, Linux (Btrfs) 255 bytes of UTF-8, which are at most 255
/// units (WinBtrfs shows each byte that is not UTF-8 as one U+FFFD).
const MAX_NAME_UNITS: usize = 255;

/// Whether a directory query that filled `filled` bytes of the buffer left room for one more
/// entry of class `CLASSES[level]` with the longest name there is. Each call returns as many
/// entries as fit whole in the buffer (NtQueryDirectoryFile's documented rule; WinBtrfs's
/// dirctrl.c fills it the same way), so then no entry is left and the next call would only return
/// STATUS_NO_MORE_FILES. Entries start 8-byte aligned.
fn room_for_another(filled: usize, level: u8) -> bool {
    let name_off = match level {
        0 => offset_of!(FILE_ID_EXTD_DIR_INFORMATION, FileName),
        1 => offset_of!(FILE_ID_BOTH_DIR_INFORMATION, FileName),
        _ => offset_of!(FILE_BOTH_DIR_INFORMATION, FileName),
    };
    let next = filled.next_multiple_of(8);
    BUF_BYTES.saturating_sub(next) >= name_off + 2 * MAX_NAME_UNITS
}

/// Whether listings of this root may leave out their last query (`skip_final`): only on local
/// NTFS, ReFS and Btrfs, where the rule of `room_for_another` was checked on whole volumes
/// (docs/native.md 2.2), and until a checked listing broke it. Elsewhere (FAT, exFAT, network
/// redirectors, which may return a short buffer early) every listing goes on until
/// STATUS_NO_MORE_FILES; so do listings of reparse points (`list_into`).
fn skips_final(state: &RootState) -> bool {
    state.skip_final
        && state.final_ok.load(Ordering::Relaxed)
        && matches!(state.fs.load(Ordering::Acquire), fs::NTFS | fs::REFS | fs::BTRFS)
}

/// Listings of a root `skip_final` checks before it trusts the rule, and one in how many after.
const FINAL_CHECKED: u32 = 64;

/// Whether this listing, which could leave its last query out, sends it to check the rule.
fn checks_final(state: &RootState) -> bool {
    let k = state.final_seen.fetch_add(1, Ordering::Relaxed);
    state.verify_final || k < FINAL_CHECKED || k % FINAL_CHECKED == 0
}

/// The file id of a listing's "." entry and its width in bits (`Raw::id`), if it had one.
type DotId = Option<(u128, u8)>;

#[allow(clippy::too_many_arguments)]
fn list_into(
    h: &Handle,
    path: &[u16],
    root: &RootOpts,
    state: &RootState,
    pinned: bool,
    base: usize,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath, Hint)>,
    dot: &mut DotId,
) -> Result<(), String> {
    let first_sub = subdirs.len();
    // one information class for the whole listing, even if another thread changes the root's
    let mut level = state.level.load(Ordering::Relaxed);
    let how = Listing {
        pinned,
        btrfs: state.btrfs(),
        up: upcase(),
        now: if alloc_from_listing() { filetime_now() } else { 0 },
    };
    // test seam: FileIdExtd ids as WinBtrfs gives them, the listed folder's own
    let own_id = if state.extd_self_ids { file_id(h, 128) } else { None };
    let mut skip = skips_final(state);
    let mut checked: Option<bool> = None; // checks_final, asked once per listing
    *dot = None;
    CASE.with_borrow_mut(Vec::clear);
    let listed = BUF.with_borrow_mut(|buf| {
        let mut first = true;
        // the last buffer had room for another entry: the query now running would be left out
        // (verify_final sends it to see)
        let mut skippable = false;
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
                if skippable {
                    bump(&STATS.final_verified);
                }
                break;
            }
            if skippable {
                bump(&STATS.final_wrong); // listed all the same: the query was sent
                if !state.verify_final {
                    state.final_ok.store(false, Ordering::Relaxed); // the rule does not hold here
                }
                skippable = false;
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
                let Some((next, mut raw)) =
                    (unsafe { read_entry(bytes.add(off), filled - off, level) })
                else {
                    return Err(status_message(STATUS_INVALID_PARAMETER));
                };
                if let (0, Some(id)) = (level, own_id) {
                    raw.id = id;
                }
                let n = raw.name;
                if n == [b'.' as u16] {
                    if dot.is_none() && raw.id_bits != 0 {
                        *dot = Some((raw.id, raw.id_bits));
                    }
                    // a reparse point's listing may come from a filter (cloud files, ProjFS)
                    // that fills its buffers by other rules
                    if raw.attrs & FA_REPARSE != 0 {
                        skip = false;
                    }
                } else if n != [b'.' as u16, b'.' as u16] {
                    add_entry(&raw, path, root, how, base, arena, subdirs);
                }
                if next == 0 || off + next as usize >= filled {
                    break;
                }
                off += next as usize;
            }
            if skip && room_for_another(filled, level) {
                if !*checked.get_or_insert_with(|| checks_final(state)) {
                    bump(&STATS.final_skipped);
                    break;
                }
                skippable = true;
            }
        }
        Ok(())
    });
    let subs = &mut subdirs[first_sub..];
    // every name is hashed as it is read; the sort and the marks only with a subfolder among two
    // entries or more (docs/native.md 2.2)
    if listed.is_ok() && !subs.is_empty() && arena.len() - base >= 2 {
        CASE.with_borrow_mut(|keys| {
            MARKS.with_borrow_mut(|marks| {
                case::mark_twins(keys, &arena.names, marks);
                for &(j, m) in marks.iter() {
                    if let Ok(i) = subs.binary_search_by_key(&j, |s| s.0) {
                        subs[i].2.mark |= m;
                    }
                }
            })
        });
    }
    // pool threads live on: a folder of a million entries must not leave them 32 MB each
    CASE.with_borrow_mut(trim);
    MARKS.with_borrow_mut(trim);
    listed?;
    let marked = subs.iter().any(|s| s.2.mark & mark::STRICT != 0);
    if (state.relative.load(Ordering::Relaxed) || pinned || marked) && shares_ids(subs) {
        distrust_ids(state, level);
        for sub in subs {
            // the ids go, what the names say stays: a twin without an id is decided by its name
            sub.2.id = 0;
            sub.2.bits = 0;
        }
    }
    Ok(())
}

/// Empties a per-thread buffer, and gives its memory back once it held a big listing's worth.
fn trim<T>(v: &mut Vec<T>) {
    v.clear();
    if v.capacity() > TRIM_ABOVE {
        *v = Vec::new();
    }
}

/// Entries a per-thread buffer of `list_into` keeps room for between listings.
const TRIM_ABOVE: usize = 1 << 16;

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

    #[test]
    fn listed_alloc_rules() {
        // values as NTFS, ReFS and WinBtrfs listed small probe files (2026-10-24)
        const S: i64 = 10_000_000;
        let how = |now: i64, pinned: bool, btrfs: bool| Listing { pinned, btrfs, up: &[], now };
        let old = how(1_000 * S, false, false);
        let c = 4096;
        let use_ = ListedAlloc::Use;
        // NTFS: resident data lists its length rounded to 8 bytes; non-resident whole clusters
        assert_eq!(listed_alloc(0x20, 700, 704, 0, old, true, c), use_(0));
        assert_eq!(listed_alloc(0x20, 425, 4096, 0, old, true, c), use_(4096)); // truncated: not resident again
        assert_eq!(listed_alloc(0x820, 1 << 20, 131072, 0, old, true, c), use_(131072)); // compressed text
        assert_eq!(listed_alloc(0x220, 16 << 20, 131072, 0, old, true, c), use_(131072)); // sparse, settled
        // NTFS: no clusters listed for a file too big to be resident (WOF) -> ask; empty file -> 0
        assert_eq!(listed_alloc(0x20, 1 << 20, 0, 0, old, true, c), ListedAlloc::Ask);
        assert_eq!(listed_alloc(0x20, 0, 0, 0, old, true, c), use_(0));
        // sparse or compressed written in the last minute: the parent entry may still be behind
        assert_eq!(listed_alloc(0x220, 16 << 20, 0, 990 * S, old, true, c), ListedAlloc::Ask);
        assert_eq!(listed_alloc(0x220, 16 << 20, 0, 900 * S, old, true, c), use_(0));
        assert_eq!(listed_alloc(0x20, 3000, 4096, 990 * S, old, true, c), use_(4096)); // plain: up to date at close
        // ReFS keeps small files inline and lists their length rounded to 8 bytes: kept as listed
        assert_eq!(listed_alloc(0x20, 100, 104, 0, old, false, c), use_(104));
        assert_eq!(listed_alloc(0x20, 1 << 20, 0, 0, old, false, c), use_(0));
        // WinBtrfs and folders opened by id are never asked
        assert_eq!(listed_alloc(0x220, 64 << 20, 4096, 999 * S, how(1_000 * S, false, true), false, c), use_(4096));
        assert_eq!(listed_alloc(0x20, 1 << 20, 0, 0, how(1_000 * S, true, false), true, c), use_(0));
        // unknown clock: never "recent"
        assert_eq!(listed_alloc(0x220, 16 << 20, 0, 0, how(0, false, false), true, c), use_(0));
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
            alloc: 0,
            id: 0,
            id_bits: 0,
            name: &NAME,
        }
    }

    fn opts(relative_open: bool) -> StateOpts {
        StateOpts {
            relative_open,
            ..StateOpts::default()
        }
    }

    fn hint(id: u128, bits: u8, name: &str, marks: u8) -> Hint {
        Hint {
            id,
            bits,
            name_len: name.encode_utf16().count() as u16,
            mark: marks,
            size: 0,
        }
    }

    /// A temporary folder with these subfolders and files, gone with it.
    struct Temp(std::path::PathBuf);

    impl Temp {
        fn new(tag: &str, dirs: &[&str], files: &[&str]) -> Self {
            let base = std::env::temp_dir().join(format!("spaceknife-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            for d in dirs {
                std::fs::create_dir_all(base.join(d)).unwrap();
            }
            for f in files {
                std::fs::write(base.join(f), b"f").unwrap();
            }
            Self(base)
        }

        fn path(&self) -> Vec<u16> {
            w(self.0.to_str().unwrap())
        }

        fn child(&self, name: &str) -> Vec<u16> {
            join_windows(&self.path(), &w(name))
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The file id of any file or folder, in `bits`.
    fn id_of_file(path: &std::path::Path, bits: u8) -> u128 {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000; // folders too
        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap();
        let h = Handle(f.as_raw_handle());
        let id = file_id(&h, bits).unwrap();
        std::mem::forget(h); // `f` closes it
        id
    }

    #[test]
    fn a_relative_open_reaching_another_folder_than_listed_is_refused() {
        let t = Temp::new("relative", &["a", "b"], &[]);
        let state = RootState::new(&opts(true));
        let parent = Arc::new(open_dir(&t.path(), &state).unwrap().0);
        for bits in [128u8, 64] {
            // as FileIdExtd (128 bits, FileIdInformation) and FileIdBoth (64, FileInternalInformation) list them
            let id_of = |name: &str| file_id(&open_dir(&t.child(name), &state).unwrap().0, bits).unwrap();
            let (a, b) = (id_of("a"), id_of("b"));
            assert_ne!(a, b);
            let via = |id: u128| Via {
                parent: parent.clone(),
                hint: hint(id, bits, "a", 0),
                pinned: false,
            };
            let (outcome, h) = open_relative(&t.child("a"), &via(a), &state);
            assert!(outcome == Outcome::Same && h.is_some());
            let before = STATS.id_mismatch.load(Ordering::Relaxed);
            // the name "a" leads to a, but the listing said b: not that folder, so not opened
            let (outcome, h) = open_relative(&t.child("a"), &via(b), &state);
            assert!(outcome == Outcome::Other { named: Some(true) } && h.is_none());
            assert!(STATS.id_mismatch.load(Ordering::Relaxed) > before);
            assert!(state.relative.load(Ordering::Relaxed)); // one mismatch does not end them
            // under the listed name: a folder replaced meanwhile, as before by its full path
            assert_eq!(open_child(&t.child("a"), &via(b), &state, false).unwrap().by, By::Path);
        }
    }

    #[test]
    fn a_name_that_reaches_another_entry_is_opened_by_the_listed_id() {
        let t = Temp::new("byid", &["x", "y", "gone"], &["f"]);
        let state = RootState::new(&opts(true));
        let parent = Arc::new(open_dir(&t.path(), &state).unwrap().0);
        for bits in [128u8, 64] {
            let id_of = |name: &str| file_id(&open_dir(&t.child(name), &state).unwrap().0, bits).unwrap();
            let y = id_of("y");
            let via = |id: u128, marks: u8| Via {
                parent: parent.clone(),
                hint: hint(id, bits, "X", marks),
                pinned: false,
            };
            // the listing says `X` is y; the open of `X` reaches x, stored as `x`: y goes by its id
            for marks in [mark::TWIN, 0] {
                let o = open_child(&t.child("X"), &via(y, marks), &state, false).unwrap();
                assert_eq!((o.by, file_id(&o.h, bits)), (By::Id, Some(y)), "{marks}");
            }
            // the parent's own id (WinBtrfs's FileIdExtd ids) is refused, a file's id is no folder
            let own = file_id(&parent, bits).unwrap();
            let file = id_of_file(&t.0.join("f"), bits);
            for id in [own, file] {
                let err = open_child(&t.child("X"), &via(id, mark::TWIN), &state, false).err();
                assert_eq!(err.as_deref(), Some(msg::CASE));
            }
            // a duplicate name proves nothing: by id even where the stored name is the listed one
            let dup = Via {
                parent: parent.clone(),
                hint: hint(y, bits, "x", mark::TWIN | mark::DUP),
                pinned: false,
            };
            assert_eq!(open_child(&t.child("x"), &dup, &state, false).unwrap().by, By::Id);
        }
        // a folder deleted since it was listed
        let gone = id_of_file(&t.0.join("gone"), 128);
        std::fs::remove_dir(t.0.join("gone")).unwrap();
        let via = Via {
            parent: parent.clone(),
            hint: hint(gone, 128, "X", mark::TWIN),
            pinned: false,
        };
        assert_eq!(open_child(&t.child("X"), &via, &state, false).err().as_deref(), Some(msg::CASE));
    }

    #[test]
    fn a_listing_through_a_name_that_reached_another_entry_is_dropped_for_the_listed_one() {
        // with dot_check the relative open is listed before it is checked: what x holds goes, and
        // y is listed by its id; without it the open is checked first, with the same result
        let t = Temp::new("dotid", &["x\\in-x", "y\\in-y"], &[]);
        let root = RootOpts::default();
        for dot_check in [true, false] {
            let state = RootState::new(&StateOpts { dot_check, ..opts(true) });
            let parent = Arc::new(open_dir(&t.path(), &state).unwrap().0);
            let y = file_id(&open_dir(&t.child("y"), &state).unwrap().0, 128).unwrap();
            for marks in [mark::TWIN, 0] {
                let via = Via {
                    parent: parent.clone(),
                    hint: hint(y, 128, "X", marks),
                    pinned: false,
                };
                let (mut arena, mut subs) = (Arena::default(), Vec::new());
                let (_, pinned) =
                    list_dir(&t.child("X"), true, Some(via), &root, &state, &mut arena, &mut subs).unwrap();
                assert_eq!(names(&arena), ["in-y"], "{dot_check} {marks}");
                assert!(pinned && subs.len() == 1, "{dot_check} {marks}");
            }
        }
    }

    #[test]
    fn folders_below_one_opened_by_id_open_relative_to_it_with_relative_opens_off() {
        let t = Temp::new("pinned", &["p\\c\\d"], &[]);
        let state = RootState::new(&opts(false));
        let root = RootOpts::default();
        let top = Arc::new(open_dir(&t.path(), &state).unwrap().0);
        // the expansion is measured although relative opens are off: pinned folders need it
        assert!(!state.relative.load(Ordering::Relaxed));
        assert_ne!(state.expansion.load(Ordering::Relaxed), UNKNOWN);
        // a strict `q` that no name reaches, listed with p's id: opened by that id, pinned
        let p = id_of_file(&t.0.join("p"), 128);
        let via = Via {
            parent: top.clone(),
            hint: hint(p, 128, "q", mark::TWIN),
            pinned: false,
        };
        let (mut arena, mut subs) = (Arena::default(), Vec::new());
        let (parent, pinned) =
            list_dir(&t.child("q"), true, Some(via), &root, &state, &mut arena, &mut subs).unwrap();
        assert!(pinned && subs.len() == 1);
        let (_, c_path, c_hint) = subs.pop().unwrap();
        assert_eq!(*c_path, *t.child("q\\c"));
        // c relative to p's handle, by its name: its path leads nowhere
        let via = sys_via(&parent.unwrap(), c_hint, true);
        let (parent, pinned) =
            list_dir(&c_path, true, Some(via), &root, &state, &mut arena, &mut subs).unwrap();
        assert!(pinned && parent.is_some() && subs.len() == 1);
        assert_eq!(*subs[0].1, *t.child("q\\c\\d"));
    }

    fn sys_via(parent: &Arc<Parent>, hint: Hint, pinned: bool) -> Via {
        via(parent, hint, pinned).unwrap()
    }

    #[test]
    fn stored_names_are_the_stored_case() {
        let t = Temp::new("stored", &["Mixed Case Folder Long"], &[]);
        let state = RootState::new(&opts(true));
        let parent = Arc::new(open_dir(&t.path(), &state).unwrap().0);
        let via = Via {
            parent,
            hint: hint(0, 0, "MIXED CASE FOLDER LONG", 0),
            pinned: true,
        };
        let (h, _) = open_named(&t.child("MIXED CASE FOLDER LONG"), &via, false, 0, &state).unwrap();
        assert_eq!(stored_name_is(&h, &w("Mixed Case Folder Long")), Some(true));
        assert_eq!(stored_name_is(&h, &w("MIXED CASE FOLDER LONG")), Some(false));
        let (h, _) = open_dir(&t.child("mixed case folder long"), &state).unwrap();
        assert_eq!(stored_name_is(&h, &w("Mixed Case Folder Long")), Some(true));
        // a task in another case than the stored name, on a root that checks names
        let checked = RootState::new(&StateOpts {
            verify_names: true,
            ..opts(true)
        });
        let root = RootOpts {
            path: t.path().into_boxed_slice(),
            ..RootOpts::default()
        };
        let upper = t.child("MIXED CASE FOLDER LONG");
        let (h, _) = open_dir(&upper, &checked).unwrap();
        assert_eq!(check_task(&h, &upper, &root, &checked), Err(msg::CASE.to_owned()));
        let exact = t.child("Mixed Case Folder Long");
        assert_eq!(check_task(&h, &exact, &root, &checked), Ok(()));
        assert_eq!(check_task(&h, &upper, &root, &state), Ok(())); // not checked there
    }

    /// The 8.3 short name of `path`, if the volume made one.
    fn short_name(path: &std::path::Path) -> Option<String> {
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
        let long: Vec<u16> = w(path.to_str().unwrap()).into_iter().chain([0]).collect();
        let mut buf = vec![0u16; 1024];
        // SAFETY: `long` is NUL-terminated, `buf` writable for its length.
        let n = unsafe { GetShortPathNameW(long.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        let full = String::from_utf16(&buf[..n.min(buf.len())]).ok()?;
        let short = full.rsplit('\\').next()?.to_owned();
        (n > 0 && short != path.file_name()?.to_str()?).then_some(short)
    }

    #[test]
    fn a_short_name_that_reaches_another_folder_is_opened_by_the_listed_id() {
        // An entry named like another folder's 8.3 short name (ntfs-3g writes such names): the
        // open of that name can reach the other folder through its short name, and its
        // FileNameInformation is then the short name, i.e. the listed name. Its normalised name is
        // the long one: another entry, so the listed folder goes by its file id.
        let t = Temp::new("short", &["Mixed Case Folder Long", "y"], &[]);
        let Some(short) = short_name(&t.0.join("Mixed Case Folder Long")) else {
            eprintln!("no 8.3 names on this volume");
            return;
        };
        let state = RootState::new(&opts(true));
        let parent = Arc::new(open_dir(&t.path(), &state).unwrap().0);
        let (h, _) = open_dir(&t.child(&short), &state).unwrap();
        assert_eq!(stored_name_is(&h, &w("Mixed Case Folder Long")), Some(true));
        for bits in [128u8, 64] {
            let y = id_of_file(&t.0.join("y"), bits);
            let via = Via {
                parent: parent.clone(),
                hint: hint(y, bits, &short, 0),
                pinned: false,
            };
            let o = open_child(&t.child(&short), &via, &state, false).unwrap();
            assert_eq!((o.by, file_id(&o.h, bits)), (By::Id, Some(y)), "{bits}");
        }
    }

    #[test]
    fn per_thread_buffers_give_big_listings_back() {
        let mut v: Vec<u64> = Vec::with_capacity(TRIM_ABOVE + 1);
        v.push(1);
        trim(&mut v);
        assert_eq!((v.len(), v.capacity()), (0, 0));
        let mut v: Vec<u64> = Vec::with_capacity(100);
        v.push(1);
        trim(&mut v);
        assert!(v.is_empty() && v.capacity() >= 100);
    }

    #[test]
    fn the_upcase_is_windows_own() {
        let up = upcase();
        for u in 0..=u16::MAX {
            // SAFETY: a pure function of its argument.
            assert_eq!(case::upcase_unit(u, up), unsafe { RtlUpcaseUnicodeChar(u) }, "{u:#x}");
        }
    }

    #[test]
    fn btrfs_sizes_tell_incomplete_listings() {
        assert_eq!(check_size(b"abc", 6, true), Ok(()));
        assert_eq!(check_size(b"abc", 8, true), Err(msg::INCOMPLETE.to_owned()));
        assert_eq!(check_size(b"abc", 8, false), Ok(())); // opened by name: counted only
        assert_eq!(check_size("a\u{FFFD}".as_bytes(), 99, true), Ok(())); // no way to tell
    }

    #[test]
    fn one_id_for_several_subdirectories_is_not_trusted() {
        let sub = |id: u128, bits: u8| (0u32, NativePath::from(vec![b'x' as u16]), hint(id, bits, "x", 0));
        assert!(!shares_ids(&[sub(5, 128), sub(6, 128)]));
        assert!(!shares_ids(&[sub(5, 128)]));
        assert!(!shares_ids(&[sub(5, 0), sub(5, 0)])); // no ids at all: nothing to trust
        assert!(shares_ids(&[sub(7, 128), sub(5, 128), sub(7, 128)])); // WinBtrfs FileIdExtd
        let state = RootState::new(&StateOpts {
            keep_access_time: true,
            ..opts(true)
        });
        distrust_ids(&state, 0);
        assert_eq!(state.level.load(Ordering::Relaxed), 1);
        assert!(state.relative.load(Ordering::Relaxed));
        distrust_ids(&state, 1);
        assert!(!state.relative.load(Ordering::Relaxed));
    }

    #[test]
    fn subdirectories_open_from_their_parent_only_by_a_checked_single_name() {
        let named = |name: &'static [u16], id: u128, id_bits: u8| {
            hint_of(
                &Raw {
                    id,
                    id_bits,
                    name,
                    ..raw(FA_DIRECTORY, 0, 0)
                },
                false,
            )
        };
        static PLAIN: [u16; 3] = [b'b' as u16, b'i' as u16, b'n' as u16];
        static BACKSLASH: [u16; 3] = [b'a' as u16, BS, b'b' as u16];
        static SLASHED: [u16; 3] = [b'a' as u16, SLASH, b'b' as u16];
        static COLON: [u16; 3] = [b'a' as u16, b':' as u16, b'b' as u16];
        assert_eq!(named(&PLAIN, 7 << 64 | 5, 128), hint(7 << 64 | 5, 128, "bin", 0));
        assert_eq!(named(&PLAIN, 5, 64).bits, 64);
        // no id to check the open against: a plain folder by full path
        let no_id = hint(0, 0, "bin", 0);
        assert_eq!(named(&PLAIN, 0, 128), no_id);
        assert_eq!(named(&PLAIN, 5, 0), no_id);
        assert_eq!(named(&PLAIN, u64::MAX as u128, 64), no_id);
        assert_eq!(named(&PLAIN, u128::MAX, 128), no_id);
        assert_eq!(named(&PLAIN, u64::MAX as u128, 128).bits, 128); // all ones in its own width only
        // a name that is a path of its own (WinBtrfs shows some Linux names with a backslash):
        // no name to open it by, only its id
        let bad = Hint {
            name_len: 0,
            ..hint(5, 128, "", mark::BADNAME)
        };
        assert_eq!(named(&BACKSLASH, 5, 128), bad);
        assert_eq!(named(&SLASHED, 5, 128), bad);
        assert_eq!(named(&COLON, 5, 128), bad); // a stream name to a path
        // a descended reparse point: its listed id is its own
        let reparse = hint_of(
            &Raw {
                id: 5,
                id_bits: 128,
                name: &PLAIN,
                ..raw(FA_DIRECTORY | FA_REPARSE, 0x9000_001A, 0)
            },
            false,
        );
        assert_eq!(reparse, hint(0, 0, "bin", mark::REPARSE));
        // Btrfs keeps the size its parent lists
        let sized = hint_of(
            &Raw {
                name: &PLAIN,
                ..raw(FA_DIRECTORY, 0, 42)
            },
            true,
        );
        assert_eq!(sized.size, 42);
        let parent = Arc::new(Handle(ptr::null_mut()));
        assert!(via(&parent, Hint::default(), false).is_none());
        assert!(via(&parent, reparse, false).is_none()); // by full path, as before
        assert!(via(&parent, named(&PLAIN, 5, 64), false).is_some());
        assert!(via(&parent, hint(0, 0, "bin", mark::TWIN), false).is_some());
        assert!(via(&parent, Hint::default(), true).is_some());
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
        let how = Listing {
            pinned: false,
            btrfs: false,
            up: upcase(),
            now: 0,
        };
        add_entry(
            &raw(attrs, tag, eof),
            &w("C:\\nowhere-spaceknife"),
            &root,
            how,
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
    fn the_last_query_is_left_out_only_where_another_entry_would_have_fit() {
        let name_offs = [
            offset_of!(FILE_ID_EXTD_DIR_INFORMATION, FileName),
            offset_of!(FILE_ID_BOTH_DIR_INFORMATION, FileName),
            offset_of!(FILE_BOTH_DIR_INFORMATION, FileName),
        ];
        for (level, &off) in name_offs.iter().enumerate() {
            let level = level as u8;
            let biggest = off + 2 * MAX_NAME_UNITS; // the longest name's entry
            let fit = (BUF_BYTES - biggest) / 8 * 8; // the last 8-aligned start it fits at
            assert!(room_for_another(0, level));
            assert!(room_for_another(fit, level));
            assert!(room_for_another(fit - 7, level)); // the next entry starts 8-aligned
            assert!(!room_for_another(fit + 1, level));
            assert!(!room_for_another(fit + 8, level));
            assert!(!room_for_another(BUF_BYTES, level));
        }
        // only local NTFS, ReFS and Btrfs
        let state = RootState::new(&StateOpts {
            skip_final: true,
            ..opts(true)
        });
        for (kind, skips) in [
            (fs::NTFS, true),
            (fs::REFS, true),
            (fs::BTRFS, true),
            (fs::OTHER, false),
            (fs::UNKNOWN, false),
        ] {
            state.fs.store(kind, Ordering::Release);
            assert_eq!(skips_final(&state), skips, "{kind}");
        }
        let off = RootState::new(&opts(true));
        off.fs.store(fs::NTFS, Ordering::Release);
        assert!(!skips_final(&off));
        // a checked listing that found more turned it off for the root
        state.fs.store(fs::NTFS, Ordering::Release);
        state.final_ok.store(false, Ordering::Relaxed);
        assert!(!skips_final(&state));
    }

    /// Lists `path` as a task the way a scan does, with these options.
    fn list_task(path: &[u16], o: &StateOpts) -> (Arena, Vec<(u32, NativePath, Hint)>, RootState) {
        let state = RootState::new(o);
        let (arena, subs) = list_with(path, &state);
        (arena, subs, state)
    }

    fn list_with(path: &[u16], state: &RootState) -> (Arena, Vec<(u32, NativePath, Hint)>) {
        let (mut arena, mut subs) = (Arena::default(), Vec::new());
        list_dir(path, false, None, &RootOpts::default(), state, &mut arena, &mut subs).unwrap();
        (arena, subs)
    }

    fn names(arena: &Arena) -> Vec<String> {
        let mut out = Vec::new();
        let mut at = 0;
        for &n in &arena.name_lens {
            out.push(String::from_utf8(arena.names[at..at + n as usize].to_vec()).unwrap());
            at += n as usize;
        }
        out.sort();
        out
    }

    #[test]
    fn listings_without_their_last_query_are_complete() {
        // a few entries (one buffer with room left), and long names over several full buffers
        let few: Vec<String> = (0..5).map(|i| format!("f{i}")).collect();
        let many: Vec<String> = (0..700).map(|i| format!("{i:04}{}", "x".repeat(200))).collect();
        for (tag, files) in [("final-few", &few), ("final-many", &many)] {
            let refs: Vec<&str> = files.iter().map(String::as_str).collect();
            let t = Temp::new(tag, &["d"], &refs);
            let mut want = files.clone();
            want.push("d".into());
            want.sort();
            let base = StateOpts {
                keep_access_time: true,
                ..opts(true)
            };
            // a root's first listings send the query to check the rule, later ones leave it out
            let state = RootState::new(&StateOpts { skip_final: true, ..base });
            let (skipped, verified) = (
                STATS.final_skipped.load(Ordering::Relaxed),
                STATS.final_verified.load(Ordering::Relaxed),
            );
            let (arena, _) = list_with(&t.path(), &state);
            assert_eq!(names(&arena), want, "{tag}");
            assert_eq!(STATS.final_skipped.load(Ordering::Relaxed), skipped, "{tag}");
            assert!(STATS.final_verified.load(Ordering::Relaxed) > verified, "{tag}");
            state.final_seen.store(FINAL_CHECKED + 1, Ordering::Relaxed);
            let (arena, _) = list_with(&t.path(), &state);
            assert_eq!(names(&arena), want, "{tag}");
            assert!(STATS.final_skipped.load(Ordering::Relaxed) > skipped, "{tag}");
            assert!(state.final_ok.load(Ordering::Relaxed), "{tag}");
            let (arena, _, _) = list_task(&t.path(), &base);
            assert_eq!(names(&arena), want, "{tag}");
            // the verification mode sends the query and finds nothing more
            let (verified, wrong) = (
                STATS.final_verified.load(Ordering::Relaxed),
                STATS.final_wrong.load(Ordering::Relaxed),
            );
            let o = StateOpts {
                skip_final: true,
                verify_final: true,
                ..base
            };
            let (arena, _, _) = list_task(&t.path(), &o);
            assert_eq!(names(&arena), want, "{tag}");
            assert!(STATS.final_verified.load(Ordering::Relaxed) > verified, "{tag}");
            assert_eq!(STATS.final_wrong.load(Ordering::Relaxed), wrong, "{tag}");
        }
    }

    #[test]
    fn relative_opens_are_checked_by_the_dot_entry_else_by_the_handle() {
        let t = Temp::new("dot", &["a\\in-a", "b\\in-b"], &["a\\file-a"]);
        let o = StateOpts {
            keep_access_time: true,
            dot_check: true,
            ..opts(true)
        };
        let state = RootState::new(&o);
        let parent = Arc::new(open_dir(&t.path(), &state).unwrap().0);
        let root = RootOpts::default();
        for bits in [128u8, 64] {
            // the class whose ids the parent's listing gave: FileIdExtd (128), FileIdBoth (64)
            state.level.store(if bits == 128 { 0 } else { 1 }, Ordering::Relaxed);
            let id_of = |name: &str| file_id(&open_dir(&t.child(name), &state).unwrap().0, bits).unwrap();
            let (a, b) = (id_of("a"), id_of("b"));
            let list_a = |id: u128| {
                let via = Via {
                    parent: parent.clone(),
                    hint: hint(id, bits, "a", 0),
                    pinned: false,
                };
                let (mut arena, mut subs) = (Arena::default(), Vec::new());
                let (handle, pinned) =
                    list_dir(&t.child("a"), true, Some(via), &root, &state, &mut arena, &mut subs).unwrap();
                assert!(handle.is_some() && !pinned);
                names(&arena)
            };
            let want = vec!["file-a".to_owned(), "in-a".to_owned()];
            // the listed id: checked by the "." entry, no query of the handle
            let (matched, ids, relative) = (
                STATS.dot_matched.load(Ordering::Relaxed),
                STATS.id_mismatch.load(Ordering::Relaxed),
                STATS.relative.load(Ordering::Relaxed),
            );
            assert_eq!(list_a(a), want);
            assert!(STATS.dot_matched.load(Ordering::Relaxed) > matched);
            assert!(STATS.relative.load(Ordering::Relaxed) > relative);
            // another id (a folder replaced meanwhile): "." does not settle it, the handle's id
            // does not either, so the folder is listed again by its full path, as without the dot
            let (unmatched, by_path) = (
                STATS.dot_unmatched.load(Ordering::Relaxed),
                STATS.by_path.load(Ordering::Relaxed),
            );
            assert_eq!(list_a(b), want);
            assert!(STATS.dot_unmatched.load(Ordering::Relaxed) > unmatched);
            assert!(STATS.id_mismatch.load(Ordering::Relaxed) > ids);
            assert!(STATS.by_path.load(Ordering::Relaxed) > by_path);
        }
        // without "." in the width asked for (the class changed since the parent's listing): the
        // handle's own id decides, and the listing through it stands
        state.level.store(2, Ordering::Relaxed); // FileBothDirectoryInformation: no ids at all
        let a = id_of_file(&t.0.join("a"), 128);
        let via = Via {
            parent: parent.clone(),
            hint: hint(a, 128, "a", 0),
            pinned: false,
        };
        let (mut arena, mut subs) = (Arena::default(), Vec::new());
        let (unmatched, by_path) = (
            STATS.dot_unmatched.load(Ordering::Relaxed),
            STATS.by_path.load(Ordering::Relaxed),
        );
        list_dir(&t.child("a"), true, Some(via), &root, &state, &mut arena, &mut subs).unwrap();
        assert_eq!(names(&arena), ["file-a", "in-a"]);
        assert!(STATS.dot_unmatched.load(Ordering::Relaxed) > unmatched);
        let _ = by_path; // (other tests may open by path meanwhile: not asserted)
    }

    #[test]
    fn access_times_are_kept_only_where_the_file_system_updates_them() {
        for v in [0u32, 2, 0x8000_0000, 0x8000_0002, 7] {
            assert!(atime_updated(fs::NTFS, (Some(v), None)), "{v:#x}");
        }
        for v in [1u32, 3, 0x8000_0001, 0x8000_0003] {
            assert!(!atime_updated(fs::NTFS, (Some(v), None)), "{v:#x}");
        }
        assert!(atime_updated(fs::NTFS, (None, Some(1))));
        assert!(!atime_updated(fs::REFS, (None, Some(1))));
        for refs in [None, Some(0), Some(2)] {
            assert!(atime_updated(fs::REFS, (Some(1), refs)), "{refs:?}");
        }
        for kind in [fs::BTRFS, fs::OTHER, fs::UNKNOWN] {
            assert!(atime_updated(kind, (Some(1), Some(1))));
        }
        // on this machine's temp folder (NTFS): as its setting says
        let t = Temp::new("atime", &[], &["f"]);
        let o = StateOpts {
            keep_access_time: true,
            keep_where_updated: true,
            ..opts(true)
        };
        let (_, _, state) = list_task(&t.path(), &o);
        let updated = atime_updated(state.fs.load(Ordering::Acquire), atime_settings());
        assert_eq!(state.keep.load(Ordering::Relaxed), updated);
        let (_, _, state) = list_task(&t.path(), &StateOpts { keep_where_updated: false, ..o });
        assert!(state.keep.load(Ordering::Relaxed));
    }

    #[test]
    fn missing_dir_is_path_not_found() {
        let state = RootState::new(&StateOpts {
            keep_access_time: true,
            ..opts(true)
        });
        let err = open_dir(&w("C:\\definitely-not-here-spaceknife"), &state)
            .err()
            .unwrap();
        assert_eq!(err, win_message(ERROR_PATH_NOT_FOUND));
    }
}
