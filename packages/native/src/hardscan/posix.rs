//! POSIX lister: `open(O_DIRECTORY)` + `getdents64` (Linux) / `readdir` (elsewhere) +
//! `fstatat(dirfd, name, AT_SYMLINK_NOFOLLOW)` per entry: worker.py's `_list_posix`, syscall
//! for syscall, without the Python objects in between. On macOS `getattrlistbulk` gives the
//! attributes of whole batches of entries instead (`bulk`, checked against fstatat).

#[cfg(target_os = "linux")]
use std::cell::RefCell; // the getdents64 buffer
use std::ffi::{CStr, CString};
use std::sync::Arc;

use super::common::{Arena, NativePath, RootOpts, Row, StateOpts, ext_len_with, flag, kind, secs};

/// The row worker.py writes when an entry cannot be stat'ed.
const ERROR_ROW: Row = Row {
    kind: kind::OTHER,
    flags: flag::ERROR,
    size: 0,
    alloc: 0,
    mtime: 0,
    ctime: 0,
    atime: 0,
    ino: 0,
    nlink: 1,
    dev: 0,
    ext_len: 0,
};

/// Code points of `bytes.decode("utf-8", "surrogateescape")`: one per valid char or bad byte.
fn escaped_code_points(bytes: &[u8]) -> usize {
    bytes
        .utf8_chunks()
        .map(|c| c.valid().chars().count() + c.invalid().len())
        .sum()
}

/// `posixpath.join(parent, name)` for a plain entry name.
fn join_posix(parent: &[u8], name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(parent.len() + 1 + name.len());
    out.extend_from_slice(parent);
    if !parent.is_empty() && !parent.ends_with(b"/") {
        out.push(b'/');
    }
    out.extend_from_slice(name);
    out
}

/// Is `dir`/`name` one of the folders not to enter? (Compared without building the path.)
fn is_skipped(skip: &[Vec<u8>], dir: &[u8], name: &[u8]) -> bool {
    skip.iter().any(|s| {
        let Some(rest) = s.strip_prefix(dir) else {
            return false;
        };
        let rest = if dir.ends_with(b"/") || dir.is_empty() {
            rest
        } else {
            match rest.strip_prefix(b"/") {
                Some(r) => r,
                None => return false,
            }
        };
        rest == name
    })
}

/// Per root and call: whether listings keep folders' access times (Linux).
pub struct RootState {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    keep: bool,
}

impl RootState {
    /// `keep_access_time` works on Linux (O_NOATIME; macOS has nothing like it). The rest are
    /// Windows options: opening a subdirectory relative to its parent measured no faster here, and
    /// names are bytes compared exactly.
    pub fn new(o: &StateOpts) -> Self {
        Self {
            keep: o.keep_access_time,
        }
    }
}

/// Listing through `fd` leaves the folder's access time as it was: O_NOATIME, set once it is open
/// (fcntl F_SETFL), which the kernel allows to the folder's owner and to CAP_FOWNER; for anyone
/// else it is refused and the folder is listed as before. Under relatime the first listing of a
/// day otherwise rewrites the inode of every folder whose access time is a day old (on Btrfs a
/// copy-on-write of metadata that snapshots shared). Best effort.
#[cfg(target_os = "linux")]
fn keep_access_time(fd: libc::c_int) {
    // SAFETY: a plain call on our descriptor. F_SETFL replaces the status flags: O_RDONLY is 0,
    // and O_DIRECTORY, O_NOFOLLOW and O_CLOEXEC are not status flags, so only O_NOATIME changes.
    unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NOATIME) };
}

/// Windows hands a listed directory's handle to its subdirectories (win.rs); here nothing is, and
/// no folder is opened by file id (none is pinned, none strict: names are compared as bytes).
pub type Hint = ();
pub type Parent = ();
pub enum Via {}

pub fn via(_parent: &Arc<Parent>, _hint: Hint, _pinned: bool) -> Option<Via> {
    None
}

pub fn marks(_hint: &Hint) -> u8 {
    0
}

pub fn cap_hit() {}

pub fn open_stats(_reset: bool) -> Vec<(&'static str, u64)> {
    Vec::new()
}

/// Python's OSError.strerror for an errno (the C library's message).
pub fn errno_message(code: i32) -> String {
    let s = std::io::Error::from_raw_os_error(code).to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

struct Fd(libc::c_int);

impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: we own the descriptor.
        unsafe { libc::close(self.0) };
    }
}

/// macOS st_flags: the contents are in the cloud (iCloud Drive, a File Provider), worker.SF_DATALESS.
#[cfg(target_os = "macos")]
const SF_DATALESS: u32 = 0x4000_0000;

fn dataless(flags: u32) -> bool {
    #[cfg(target_os = "macos")]
    {
        flags & SF_DATALESS != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = flags;
        false // the bit means something else elsewhere (FreeBSD)
    }
}

fn st_flags(st: &libc::stat) -> u32 {
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    {
        st.st_flags as u32
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
    {
        let _ = st;
        0
    }
}

fn st_birth(st: &libc::stat) -> (i64, u64) {
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    {
        (st.st_birthtime as i64, st.st_birthtime_nsec as u64)
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
    {
        let _ = st;
        (0, 0) // Linux: Python's stat has no st_birthtime
    }
}

/// What a row needs of an entry: lstat's fields (fstatat, or getattrlistbulk on macOS).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Meta {
    mode: u32, // S_IFMT bits
    dev: u64,
    size: i64,
    alloc: i64, // st_blocks * 512
    mtime: (i64, u64),
    btime: (i64, u64),
    atime: (i64, u64),
    ino: u64,
    nlink: u64,
    flags: u32, // st_flags (BSDs)
}

impl Meta {
    fn from_stat(st: &libc::stat) -> Self {
        Self {
            mode: st.st_mode as u32 & libc::S_IFMT as u32,
            dev: st.st_dev as u64,
            size: st.st_size as i64,
            alloc: (st.st_blocks as i64).saturating_mul(512),
            mtime: (st.st_mtime as i64, st.st_mtime_nsec as u64),
            btime: st_birth(st),
            atime: (st.st_atime as i64, st.st_atime_nsec as u64),
            ino: st.st_ino as u64,
            nlink: st.st_nlink as u64,
            flags: st_flags(st),
        }
    }
}

/// fstatat(dirfd, name, AT_SYMLINK_NOFOLLOW); None when it fails.
fn stat_entry(dirfd: libc::c_int, name: &CStr) -> Option<Meta> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: name is NUL-terminated, st is writable.
    if unsafe { libc::fstatat(dirfd, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return None;
    }
    Some(Meta::from_stat(&st))
}

fn add_entry(
    dirfd: libc::c_int,
    name: &CStr,
    dir: &[u8],
    root: &RootOpts,
    base: usize,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath, Hint)>,
) {
    push_entry(
        stat_entry(dirfd, name),
        name.to_bytes(),
        dir,
        root,
        base,
        arena,
        subdirs,
    );
}

/// One entry's row (ERROR_ROW without `meta`); a folder to descend goes to `subdirs`.
fn push_entry(
    meta: Option<Meta>,
    raw: &[u8],
    dir: &[u8],
    root: &RootOpts,
    base: usize,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath, Hint)>,
) {
    let name_start = arena.names.len();
    arena.names.extend_from_slice(raw);
    let j = (arena.len() - base) as u32;
    let Some(m) = meta else {
        arena.push_row(name_start, ERROR_ROW);
        return;
    };
    let mut flags = 0u32;
    let mut descend = false;
    let cloud = dataless(m.flags);
    if cloud {
        flags |= flag::CLOUD; // takes no local space; a folder is not entered: listing would download it
    }
    let kind = if m.mode == libc::S_IFDIR as u32 {
        if (m.dev != root.dev && !root.cross_mounts) || is_skipped(&root.skip, dir, raw) {
            flags |= flag::MOUNT;
        } else if is_skipped(&root.protect, dir, raw) {
            flags |= flag::ACCESS_DENIED;
        } else {
            descend = !cloud;
        }
        kind::DIR
    } else if m.mode == libc::S_IFLNK as u32 {
        kind::LINK
    } else if m.mode == libc::S_IFREG as u32 {
        kind::FILE
    } else {
        kind::OTHER
    };
    if raw.first() == Some(&b'.') {
        flags |= flag::HIDDEN;
    }
    let row = Row {
        kind,
        flags,
        size: if kind == kind::DIR { 0 } else { m.size },
        alloc: m.alloc,
        mtime: secs(m.mtime.0, m.mtime.1),
        ctime: secs(m.btime.0, m.btime.1),
        atime: secs(m.atime.0, m.atime.1),
        ino: m.ino,
        nlink: m.nlink.min(65535) as u16,
        dev: m.dev,
        ext_len: if kind == kind::FILE {
            ext_len_with(raw, escaped_code_points)
        } else {
            0
        },
    };
    arena.push_row(name_start, row);
    if descend {
        subdirs.push((j, join_posix(dir, raw).into_boxed_slice(), ()));
    }
}

fn is_dots(name: &[u8]) -> bool {
    name == b"." || name == b".."
}

#[cfg(target_os = "linux")]
fn read_entries(fd: &Fd, mut each: impl FnMut(&CStr)) -> Result<(), i32> {
    const BUF_BYTES: usize = 64 * 1024;
    thread_local! {
        static BUF: RefCell<Vec<u64>> = RefCell::new(vec![0u64; BUF_BYTES / 8]);
    }
    BUF.with_borrow_mut(|buf| {
        loop {
            // SAFETY: buf is writable for BUF_BYTES bytes.
            let n =
                unsafe { libc::syscall(libc::SYS_getdents64, fd.0, buf.as_mut_ptr(), BUF_BYTES) };
            if n < 0 {
                return Err(errno());
            }
            if n == 0 {
                return Ok(());
            }
            let n = n as usize;
            let bytes = buf.as_ptr().cast::<u8>();
            let mut off = 0usize;
            while off + 20 <= n {
                // struct linux_dirent64 { u64 d_ino; i64 d_off; u16 d_reclen; u8 d_type; char d_name[]; }
                // SAFETY: off + 20 <= n, and the kernel NUL-terminates d_name inside the record.
                let reclen =
                    unsafe { std::ptr::read_unaligned(bytes.add(off + 16) as *const u16) } as usize;
                if reclen < 20 || off + reclen > n {
                    return Err(libc::EIO);
                }
                let name = unsafe { CStr::from_ptr(bytes.add(off + 19).cast()) };
                if !is_dots(name.to_bytes()) {
                    each(name);
                }
                off += reclen;
            }
        }
    })
}

#[cfg(not(target_os = "linux"))]
fn read_entries(fd: &Fd, mut each: impl FnMut(&CStr)) -> Result<(), i32> {
    // fdopendir takes ownership of its descriptor: give it a duplicate (like os.scandir(fd)).
    // SAFETY: plain descriptor calls; the DIR stream is closed on every path.
    unsafe {
        let dup = libc::fcntl(fd.0, libc::F_DUPFD_CLOEXEC, 0);
        if dup < 0 {
            return Err(errno());
        }
        let dir = libc::fdopendir(dup);
        if dir.is_null() {
            let e = errno();
            libc::close(dup);
            return Err(e);
        }
        libc::rewinddir(dir); // from the first entry, whatever read the descriptor before
        let result = loop {
            *errno_location() = 0;
            let ent = libc::readdir(dir);
            if ent.is_null() {
                let e = *errno_location();
                break if e == 0 { Ok(()) } else { Err(e) };
            }
            let name = CStr::from_ptr((*ent).d_name.as_ptr());
            if !is_dots(name.to_bytes()) {
                each(name);
            }
        };
        libc::closedir(dir);
        result
    }
}

/// macOS: getattrlistbulk(2) returns the names and attributes of a batch of entries per call,
/// where readdir + fstatat costs a system call per entry. Folders, entries reported with an error
/// and anything not as expected still go through fstatat. The first files a process reads are
/// checked against fstatat field by field; a difference turns the bulk path off for good, and
/// listings fall back to readdir + fstatat (so does SPACEKNIFE_NO_BULK=1).
///
/// Folders can come from it too (SPACEKNIFE_BULK_DIRS=1, checked the same way on their own; mount
/// points and firmlinks, whose fstatat answer is the other side, still fstatat'ed), but it is not
/// faster: asking for the folder attributes slows every call down about as much as the fstatat
/// calls it saves (GitHub's macOS runners, 2026-10-20; docs/packaging.md).
#[cfg(target_os = "macos")]
mod bulk {
    use std::cell::RefCell;
    use std::ffi::CStr;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU8, Ordering};

    use super::{Meta, errno, stat_entry};

    #[repr(C)]
    #[allow(dead_code)] // filled for the kernel, never read back
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }

    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_CMN_NAME: u32 = 0x0000_0001;
    const ATTR_CMN_DEVID: u32 = 0x0000_0002;
    const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    const ATTR_CMN_CRTIME: u32 = 0x0000_0200;
    const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
    const ATTR_CMN_ACCTIME: u32 = 0x0000_1000;
    const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
    const ATTR_CMN_FILEID: u32 = 0x0200_0000;
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    const ATTR_DIR_ENTRYCOUNT: u32 = 0x0000_0002;
    const ATTR_DIR_MOUNTSTATUS: u32 = 0x0000_0004;
    const ATTR_DIR_ALLOCSIZE: u32 = 0x0000_0008;
    const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
    const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
    const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;
    const FSOPT_PACK_INVAL_ATTRS: u64 = 0x0000_0008;
    const SF_FIRMLINK: u32 = 0x0080_0000; // st_flags: a firmlink (/Applications on the system volume)

    const COMMON: u32 = ATTR_CMN_RETURNED_ATTRS
        | ATTR_CMN_NAME
        | ATTR_CMN_ERROR
        | ATTR_CMN_DEVID
        | ATTR_CMN_OBJTYPE
        | ATTR_CMN_CRTIME
        | ATTR_CMN_MODTIME
        | ATTR_CMN_ACCTIME
        | ATTR_CMN_FLAGS
        | ATTR_CMN_FILEID;
    const DIR: u32 = ATTR_DIR_ENTRYCOUNT | ATTR_DIR_MOUNTSTATUS | ATTR_DIR_ALLOCSIZE;
    const FILE: u32 = ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE | ATTR_FILE_DATALENGTH;
    const NEEDED: u32 = COMMON & !(ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_ERROR);

    // One record with FSOPT_PACK_INVAL_ATTRS (requested attributes present, 4-byte aligned):
    // u32 length, attribute_set_t returned (5 x u32), u32 error (right after the returned set),
    // then by bit: attrreference_t name, dev_t devid, fsobj_type_t objtype, timespec crtime,
    // modtime, acctime, u32 flags, u64 fileid; then, for a folder, the folder attributes: u32
    // entrycount, u32 mountstatus, off_t allocsize; for anything else the file attributes: u32
    // linkcount, off_t allocsize, off_t datalength. The name's bytes follow the fixed part, where
    // its reference points: the folder attributes are read right after the common ones, the file
    // attributes right before the name (whether or not the other section is packed too).
    const AT_RETURNED: usize = 4;
    const AT_ERROR: usize = 24;
    const AT_NAME: usize = 28;
    const AT_DEVID: usize = 36;
    const AT_OBJTYPE: usize = 40;
    const AT_CRTIME: usize = 44;
    const AT_MODTIME: usize = 60;
    const AT_ACCTIME: usize = 76;
    const AT_FLAGS: usize = 92;
    const AT_FILEID: usize = 96;
    const COMMON_END: usize = 104; // every record: the common attributes
    const DIR_LEN: usize = 16; // entrycount, mountstatus, allocsize
    const FILE_LEN: usize = 20; // linkcount, allocsize, datalength
    const DIR_MNTSTATUS_MNTPOINT: u32 = 0x0000_0001;
    const DIR_MNTSTATUS_TRIGGER: u32 = 0x0000_0002;

    const VREG: u32 = 1;
    const VDIR: u32 = 2;

    const UNTESTED: u8 = 0;
    const ON: u8 = 1;
    const OFF: u8 = 2;
    const NOT_ASKED: u8 = 3; // folders: through fstatat, not asked of getattrlistbulk at all

    /// Trust in one kind of record (files, folders): untested, on, or off with the reason.
    struct Switch {
        state: AtomicU8,
        reason: Mutex<String>,
    }

    impl Switch {
        const fn new() -> Self {
            Self {
                state: AtomicU8::new(UNTESTED),
                reason: Mutex::new(String::new()),
            }
        }

        fn get(&self) -> u8 {
            self.state.load(Ordering::Relaxed)
        }

        fn turn_off(&self, why: String) {
            if self.state.swap(OFF, Ordering::Relaxed) != OFF {
                if let Ok(mut r) = self.reason.lock() {
                    *r = why;
                }
            }
        }

        fn trust(&self) {
            let _ = self
                .state
                .compare_exchange(UNTESTED, ON, Ordering::Relaxed, Ordering::Relaxed);
        }

        fn describe(&self) -> String {
            match self.get() {
                ON => "on".to_owned(),
                OFF => {
                    let why = self.reason.lock().map(|r| r.clone()).unwrap_or_default();
                    if why.is_empty() {
                        "off".to_owned()
                    } else {
                        format!("off: {why}")
                    }
                }
                NOT_ASKED => "not asked".to_owned(),
                _ => "untested".to_owned(),
            }
        }
    }

    /// Folders taken from getattrlistbulk (SPACEKNIFE_BULK_DIRS=1, not turned off by a check).
    fn dirs_used() -> bool {
        matches!(DIRS.get(), UNTESTED | ON)
    }

    static FILES: Switch = Switch::new(); // off: the whole bulk path is off
    static DIRS: Switch = Switch::new(); // off or not asked: folders go through fstatat
    const BUF_BYTES: usize = 256 * 1024;
    const CHECKS: usize = 8; // entries compared with fstatat before the bulk path is trusted

    unsafe extern "C" {
        fn getattrlistbulk(
            dirfd: libc::c_int,
            alist: *mut libc::c_void,
            buf: *mut libc::c_void,
            size: libc::size_t,
            options: u64,
        ) -> libc::c_int;
    }

    fn rd<T: Copy>(b: &[u8], at: usize) -> T {
        debug_assert!(at + std::mem::size_of::<T>() <= b.len());
        // SAFETY: callers check that the record holds `at + size_of::<T>()` bytes.
        unsafe { std::ptr::read_unaligned(b.as_ptr().add(at).cast::<T>()) }
    }

    fn time(b: &[u8], at: usize) -> (i64, u64) {
        (rd::<i64>(b, at), rd::<i64>(b, at + 8) as u64)
    }

    /// "on" (files and folders), "on (folders: …)", "untested", or "off: why".
    /// "on" (files), "on (folders too)", "on (folders: …)", "untested", or "off: why".
    pub fn state() -> String {
        match (FILES.get(), DIRS.get()) {
            (ON, NOT_ASKED) => "on".to_owned(),
            (ON, ON) => "on (folders too)".to_owned(),
            (ON, _) => format!("on (folders: {})", DIRS.describe()),
            _ => FILES.describe(),
        }
    }

    pub fn enabled() -> bool {
        match FILES.get() {
            ON => true,
            OFF => false,
            _ => {
                if std::env::var_os("SPACEKNIFE_NO_BULK").is_some_and(|v| v == "1") {
                    FILES.turn_off("SPACEKNIFE_NO_BULK=1".to_owned());
                    return false;
                }
                if !std::env::var_os("SPACEKNIFE_BULK_DIRS").is_some_and(|v| v == "1") {
                    let _ = DIRS.state.compare_exchange(
                        UNTESTED,
                        NOT_ASKED,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                }
                true
            }
        }
    }

    /// One parsed record: the name (with its NUL) and, when the record can be used, its
    /// attributes (None: fstatat it).
    fn parse(rec: &[u8]) -> Result<(&CStr, Option<Meta>), String> {
        if rec.len() < COMMON_END {
            return Err(format!(
                "a record of {} bytes, at least {COMMON_END} expected",
                rec.len()
            ));
        }
        let name_off = rd::<i32>(rec, AT_NAME) as isize;
        let name_len = rd::<u32>(rec, AT_NAME + 4) as usize;
        let start = AT_NAME as isize + name_off;
        if start < COMMON_END as isize || name_len == 0 || start as usize + name_len > rec.len() {
            return Err(format!(
                "name at {start}, {name_len} bytes, in a record of {}",
                rec.len()
            ));
        }
        let start = start as usize;
        let raw = &rec[start..start + name_len];
        let name =
            CStr::from_bytes_until_nul(raw).map_err(|_| format!("name without NUL: {raw:?}"))?;
        let returned = rd::<u32>(rec, AT_RETURNED);
        let returned_dir = rd::<u32>(rec, AT_RETURNED + 8);
        let returned_file = rd::<u32>(rec, AT_RETURNED + 12);
        let objtype = rd::<u32>(rec, AT_OBJTYPE);
        let flags = rd::<u32>(rec, AT_FLAGS);
        if rd::<u32>(rec, AT_ERROR) != 0 || returned & NEEDED != NEEDED {
            return Ok((name, None));
        }
        let times = (
            time(rec, AT_MODTIME),
            time(rec, AT_CRTIME),
            time(rec, AT_ACCTIME),
        );
        if objtype == VDIR {
            // a mount point's or firmlink's fstatat describes what is on the other side
            let usable = dirs_used()
                && returned_dir & DIR == DIR
                && start >= COMMON_END + DIR_LEN
                && rd::<u32>(rec, COMMON_END + 4)
                    & (DIR_MNTSTATUS_MNTPOINT | DIR_MNTSTATUS_TRIGGER)
                    == 0
                && flags & SF_FIRMLINK == 0;
            if !usable {
                return Ok((name, None));
            }
            let alloc = rd::<i64>(rec, COMMON_END + 8).max(0);
            return Ok((
                name,
                Some(Meta {
                    mode: libc::S_IFDIR as u32,
                    dev: rd::<i32>(rec, AT_DEVID) as u64,
                    size: 0, // a folder's row has no size of its own (fstatat's st_size is not read)
                    alloc: (alloc + 511) / 512 * 512,
                    mtime: times.0,
                    btime: times.1,
                    atime: times.2,
                    ino: rd::<u64>(rec, AT_FILEID),
                    // APFS gives a folder st_nlink = its entries + 2 (ATTR_DIR_LINKCOUNT counts only
                    // hard links to it); a file system that counts otherwise fails the check
                    nlink: rd::<u32>(rec, COMMON_END) as u64 + 2,
                    flags,
                }),
            ));
        }
        if returned_file & FILE != FILE || start < COMMON_END + FILE_LEN {
            return Ok((name, None));
        }
        let at = start - FILE_LEN; // the file attributes end where the name starts
        let mode = match objtype {
            VREG => libc::S_IFREG,
            3 => libc::S_IFBLK,
            4 => libc::S_IFCHR,
            5 => libc::S_IFLNK,
            6 => libc::S_IFSOCK,
            7 => libc::S_IFIFO,
            _ => return Ok((name, None)),
        } as u32;
        let alloc = rd::<i64>(rec, at + 4).max(0);
        let meta = Meta {
            mode,
            dev: rd::<i32>(rec, AT_DEVID) as u64, // like st_dev (int32) as u64
            size: rd::<i64>(rec, at + 12),
            alloc: (alloc + 511) / 512 * 512, // st_blocks: the total allocation in 512-byte blocks
            mtime: times.0,
            btime: times.1,
            atime: times.2,
            ino: rd::<u64>(rec, AT_FILEID),
            nlink: rd::<u32>(rec, at) as u64,
            flags,
        };
        Ok((name, Some(meta)))
    }

    fn is_dir(m: &Meta) -> bool {
        m.mode == libc::S_IFDIR as u32
    }

    /// The fields fstatat and getattrlistbulk must agree on (access times may move meanwhile; a
    /// folder's size is not used).
    fn same(a: &Meta, b: &Meta) -> bool {
        let key = |m: &Meta| {
            let size = if is_dir(m) { 0 } else { m.size };
            (
                m.mode, m.dev, size, m.alloc, m.mtime, m.btime, m.ino, m.nlink, m.flags,
            )
        };
        key(a) == key(b)
    }

    /// Lists `dirfd` with getattrlistbulk: `each(name, meta)` per entry, meta None meaning "fstatat
    /// it". Ok(false): nothing read, list it the other way (unsupported here, or turned off).
    pub fn read(
        dirfd: libc::c_int,
        mut each: impl FnMut(&CStr, Option<Meta>),
    ) -> Result<bool, i32> {
        thread_local! {
            static BUF: RefCell<Vec<u64>> = RefCell::new(vec![0u64; BUF_BYTES / 8]);
        }
        BUF.with_borrow_mut(|buf| {
            let mut alist = AttrList {
                bitmapcount: ATTR_BIT_MAP_COUNT,
                reserved: 0,
                commonattr: COMMON,
                volattr: 0,
                dirattr: if dirs_used() { DIR } else { 0 },
                fileattr: FILE,
                forkattr: 0,
            };
            let mut emitted = false;
            let mut batch: Vec<(&CStr, Option<Meta>)> = Vec::new();
            loop {
                // SAFETY: alist and buf are valid for the call; buf holds BUF_BYTES bytes.
                let n = unsafe {
                    getattrlistbulk(
                        dirfd,
                        (&mut alist as *mut AttrList).cast(),
                        buf.as_mut_ptr().cast(),
                        BUF_BYTES,
                        FSOPT_PACK_INVAL_ATTRS,
                    )
                };
                if n < 0 {
                    let e = errno();
                    return if emitted { Err(e) } else { Ok(false) };
                }
                if n == 0 {
                    return Ok(true);
                }
                // SAFETY: the kernel filled at most BUF_BYTES bytes of buf.
                let bytes: &[u8] =
                    unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), BUF_BYTES) };
                batch.clear();
                let mut off = 0usize;
                for _ in 0..n {
                    if off + 4 > BUF_BYTES {
                        return bail(emitted, "records past the buffer".to_owned());
                    }
                    let len = rd::<u32>(bytes, off) as usize;
                    if len < 4 || off + len > BUF_BYTES {
                        return bail(emitted, format!("a record length of {len}"));
                    }
                    match parse(&bytes[off..off + len]) {
                        Ok(item) => batch.push(item),
                        Err(why) => return bail(emitted, why),
                    }
                    off += len;
                }
                if FILES.get() == UNTESTED {
                    if let Err(why) = check(dirfd, &batch, false, &FILES) {
                        FILES.turn_off(why);
                        if !emitted {
                            return Ok(false);
                        }
                    }
                }
                if DIRS.get() == UNTESTED {
                    if let Err(why) = check(dirfd, &batch, true, &DIRS) {
                        DIRS.turn_off(why);
                    }
                }
                let (files, dirs) = (FILES.get() != OFF, dirs_used());
                if !files || !dirs {
                    // turned off (here or by another thread's check): fstatat the rest
                    for (_, meta) in batch.iter_mut() {
                        if meta.as_ref().is_some_and(|m| !files || is_dir(m)) {
                            *meta = None;
                        }
                    }
                }
                for (name, meta) in batch.drain(..) {
                    each(name, meta);
                }
                emitted = true;
            }
        })
    }

    /// A record that cannot be read: turn the bulk path off; before anything was emitted the
    /// folder is listed the other way, after it the listing fails (EIO).
    fn bail(emitted: bool, why: String) -> Result<bool, i32> {
        FILES.turn_off(why);
        if emitted { Err(libc::EIO) } else { Ok(false) }
    }

    /// Compare up to CHECKS records of one kind (folders or the rest) with fstatat; once some
    /// agree and none differs, `switch` trusts that kind (nothing to compare yet: the next batch
    /// is checked).
    fn check(
        dirfd: libc::c_int,
        batch: &[(&CStr, Option<Meta>)],
        dirs: bool,
        switch: &Switch,
    ) -> Result<(), String> {
        let mut checked = 0;
        for (name, meta) in batch {
            let Some(bulk) = meta else { continue };
            if is_dir(bulk) != dirs {
                continue;
            }
            let Some(stat) = stat_entry(dirfd, name) else {
                continue;
            };
            if !same(bulk, &stat) {
                return Err(format!("{name:?} differs: bulk {bulk:?}, fstatat {stat:?}"));
            }
            checked += 1;
            if checked == CHECKS {
                break;
            }
        }
        if checked > 0 {
            switch.trust();
        }
        Ok(())
    }
}

#[cfg(all(
    not(target_os = "linux"),
    any(target_os = "macos", target_os = "ios", target_os = "freebsd")
))]
unsafe fn errno_location() -> *mut libc::c_int {
    unsafe { libc::__error() }
}

#[cfg(all(
    not(target_os = "linux"),
    not(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))
))]
unsafe fn errno_location() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}

#[cfg(target_os = "macos")]
pub fn bulk_state() -> String {
    bulk::state()
}

/// Bytes of path given to one open by `open_long`: well under PATH_MAX everywhere (macOS: 1,024
/// with the NUL; a name is at most 255).
const PIECE: usize = 1000;

/// `open(path, oflags)` for a path of PATH_MAX bytes or more, which open() refuses (ENAMETOOLONG)
/// although the folder is there to list (worker._open_dir): opened a piece of whole names at a
/// time, each piece relative to the folder the one before reached. Resolution is that of one open
/// of the whole path: symlinks in the middle are followed, `oflags` (O_NOFOLLOW) apply to the last
/// name only, and the folders on the way need search permission only (O_PATH; elsewhere read
/// permission too, so a few such paths still fail, as they did before). A piece that is too long
/// only once its symlinks are followed (macOS) is opened again one name at a time.
fn open_long(path: &[u8], oflags: libc::c_int) -> Result<Fd, i32> {
    #[cfg(target_os = "linux")]
    let middle = libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let middle = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    let mut dir: Option<Fd> = None;
    let mut rest = path;
    // macOS counts a followed symlink's target into the 1,024 bytes of the piece being resolved:
    // once a piece is too long that way, go on one name at a time (a target alone always fits)
    let mut one = false;
    loop {
        let (piece, tail) = if one {
            // a leading "/" stays with the first name
            match rest.get(1..).and_then(|r| r.iter().position(|&b| b == b'/')) {
                Some(j) => (&rest[..j + 1], &rest[j + 2..]),
                None => (rest, &rest[rest.len()..]),
            }
        } else if rest.len() <= PIECE {
            (rest, &rest[rest.len()..])
        } else {
            // a leading "/" stays with the first piece
            match rest[1..=PIECE].iter().rposition(|&b| b == b'/') {
                Some(j) => (&rest[..j + 1], &rest[j + 2..]),
                None => return Err(libc::ENAMETOOLONG),
            }
        };
        let tail = &tail[tail.iter().take_while(|&&b| b == b'/').count()..]; // "a//b": no "/b"
        let last = tail.is_empty();
        let c = CString::new(piece).map_err(|_| libc::EINVAL)?;
        let at = dir.as_ref().map_or(libc::AT_FDCWD, |d| d.0);
        // SAFETY: c is NUL-terminated; `at` is open (or AT_FDCWD) until `dir` is replaced below.
        let fd = unsafe { libc::openat(at, c.as_ptr(), if last { oflags } else { middle }) };
        if fd < 0 {
            let code = errno();
            if code == libc::ENAMETOOLONG && !one && piece[1..].contains(&b'/') {
                one = true;
                continue;
            }
            return Err(code);
        }
        let fd = Fd(fd);
        if last {
            return Ok(fd);
        }
        dir = Some(fd);
        rest = tail;
    }
}

/// Lists `path` into `arena` (worker._list_posix); subdirectories to descend go to `subdirs`.
/// Directories found by this call are opened with O_NOFOLLOW (a swap for a symlink is refused);
/// given tasks are opened like the worker does, so a root may itself be a symlink.
pub fn list_dir(
    path: &[u8],
    discovered: bool,
    _via: Option<Via>,
    root: &RootOpts,
    state: &RootState,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath, Hint)>,
) -> Result<(Option<Arc<Parent>>, bool), String> {
    list_path(path, discovered, root, state, arena, subdirs).map(|()| (None, false))
}

fn list_path(
    path: &[u8],
    discovered: bool,
    root: &RootOpts,
    state: &RootState,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath, Hint)>,
) -> Result<(), String> {
    let c = CString::new(path).map_err(|_| "embedded null byte".to_owned())?;
    let mut oflags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    if discovered {
        oflags |= libc::O_NOFOLLOW;
    }
    // SAFETY: c is NUL-terminated.
    let fd = match unsafe { libc::open(c.as_ptr(), oflags) } {
        fd if fd >= 0 => Fd(fd),
        _ => match errno() {
            libc::ENAMETOOLONG => open_long(path, oflags).map_err(errno_message)?,
            code => return Err(errno_message(code)),
        },
    };
    #[cfg(target_os = "linux")]
    if state.keep {
        keep_access_time(fd.0);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = state;
    let base = arena.len();
    #[cfg(target_os = "macos")]
    {
        if bulk::enabled() {
            let listed = bulk::read(fd.0, |name, meta| match meta {
                Some(m) => push_entry(Some(m), name.to_bytes(), path, root, base, arena, subdirs),
                None => add_entry(fd.0, name, path, root, base, arena, subdirs),
            })
            .map_err(errno_message)?;
            if listed {
                return Ok(());
            }
            // getattrlistbulk moved the directory offset: read it again from the start
            // SAFETY: plain call on our descriptor.
            unsafe { libc::lseek(fd.0, 0, libc::SEEK_SET) };
        }
    }
    read_entries(&fd, |name| {
        add_entry(fd.0, name, path, root, base, arena, subdirs)
    })
    .map_err(errno_message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_plain_strerror() {
        assert_eq!(errno_message(libc::EACCES), "Permission denied");
        assert_eq!(errno_message(libc::ENOENT), "No such file or directory");
    }

    #[test]
    fn escaped_counts_bad_bytes_individually() {
        assert_eq!(escaped_code_points(b"ab"), 2);
        assert_eq!(escaped_code_points(b"\xff\xfe"), 2);
        assert_eq!(escaped_code_points(b"\xe4\xb8\xe4\xb8\xad"), 3);
        assert_eq!(escaped_code_points(b"\xed\xa0\x80"), 3);
    }

    #[test]
    fn skips_exact_children_only() {
        let skip = vec![b"/System/Volumes/Data".to_vec()];
        assert!(is_skipped(&skip, b"/System/Volumes", b"Data"));
        assert!(is_skipped(&skip, b"/System/Volumes/", b"Data"));
        assert!(!is_skipped(&skip, b"/System/Volumes", b"Dat"));
        assert!(!is_skipped(&skip, b"/System/Volumes", b"Data2"));
        assert!(!is_skipped(&skip, b"/System", b"Volumes"));
        assert!(!is_skipped(&[], b"/System/Volumes", b"Data"));
        assert!(is_skipped(&[b"/x".to_vec()], b"/", b"x"));
    }

    #[test]
    fn paths_past_path_max_open_piece_by_piece() {
        let base = std::env::temp_dir().join(format!("spaceknife-long-{}", std::process::id()));
        std::fs::create_dir(&base).unwrap();
        let mut path = base.as_os_str().as_encoded_bytes().to_vec();
        let name = CString::new(vec![b'd'; 200]).unwrap();
        // SAFETY: plain calls on NUL-terminated names and our own descriptors.
        let mut dir = Fd(unsafe { libc::open(CString::new(path.clone()).unwrap().as_ptr(), libc::O_RDONLY) });
        let mut depth = 0;
        while path.len() <= 5000 {
            unsafe {
                assert_eq!(libc::mkdirat(dir.0, name.as_ptr(), 0o755), 0);
                dir = Fd(libc::openat(dir.0, name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY));
            }
            assert!(dir.0 >= 0);
            path.extend_from_slice(b"/");
            path.extend_from_slice(name.as_bytes());
            depth += 1;
        }
        let ino = |fd: libc::c_int| {
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { libc::fstat(fd, &mut st) }, 0);
            st.st_ino
        };
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        let c = CString::new(path.clone()).unwrap();
        assert_eq!(unsafe { libc::open(c.as_ptr(), flags) }, -1);
        assert_eq!(errno(), libc::ENAMETOOLONG);
        assert_eq!(ino(open_long(&path, flags).unwrap().0), ino(dir.0));
        // doubled slashes, and a symlink in the middle (followed, as one open would)
        let doubled = String::from_utf8(path.clone()).unwrap().replacen("/d", "//d", 3);
        assert_eq!(ino(open_long(doubled.as_bytes(), flags).unwrap().0), ino(dir.0));
        let first = base.join("d".repeat(200));
        std::os::unix::fs::symlink(&first, base.join("via")).unwrap();
        let mut via = base.join("via").as_os_str().as_encoded_bytes().to_vec();
        via.extend_from_slice(&path[first.as_os_str().len()..]);
        assert_eq!(ino(open_long(&via, flags).unwrap().0), ino(dir.0));
        // the last name keeps O_NOFOLLOW; failures give the errno one open would
        let mut missing = path.clone();
        missing.extend_from_slice(b"/missing");
        assert_eq!(open_long(&missing, flags).err(), Some(libc::ENOENT));
        let dangling = CString::new("dangling").unwrap();
        unsafe { assert_eq!(libc::symlinkat(c"/nonexistent".as_ptr(), dir.0, dangling.as_ptr()), 0) };
        std::os::unix::fs::symlink("/nonexistent", base.join("dangling")).unwrap();
        let short = CString::new(base.join("dangling").as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::open(short.as_ptr(), flags) }, -1);
        let mut link = path.clone();
        link.extend_from_slice(b"/dangling");
        assert_eq!(open_long(&link, flags).err(), Some(errno()));
        let mut gap = path.clone();
        gap.extend_from_slice(b"/");
        gap.extend_from_slice(&[b'x'; PIECE + 1]);
        assert_eq!(open_long(&gap, flags).err(), Some(libc::ENAMETOOLONG));
        drop(dir);
        assert!(depth > 4096 / 201);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn joins_like_posixpath() {
        assert_eq!(join_posix(b"/", b"x"), b"/x");
        assert_eq!(join_posix(b"/a", b"x"), b"/a/x");
        assert_eq!(join_posix(b"", b"x"), b"x");
    }
}
