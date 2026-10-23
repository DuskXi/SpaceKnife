//! Guarded opens and metadata for the duplicate-file stages (docs/dupes.md §5).
//!
//! - `meta_of_path`: S1, metadata only. Windows opens an attribute-only handle (no data access:
//!   no recall, no access-time update, no antivirus scan of the contents) with the reparse point
//!   itself; POSIX `lstat`.
//! - `open_data`: S2/S3. Windows: FILE_READ_DATA | FILE_WRITE_ATTRIBUTES, FILE_FLAG_OPEN_NO_RECALL,
//!   then LastAccessTime = -1 on the handle (winio::keep_access_time); unbuffered unless the
//!   file is compressed, encrypted, sparse or a reparse point (WOF, dedup), which are read
//!   through their filters. Linux: O_NOATIME | O_NOFOLLOW | O_DIRECT. macOS (not verified on a
//!   real Mac): O_NOFOLLOW, F_NOCACHE, and the thread's access-time updates turned off with
//!   setiopolicy_np.
//! - `Meta::of_file`: the same metadata on an open handle, for the stamps before and after reads.

use std::fs::File;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::codes;

/// What identifies one look at a file: identity, size and the two modification times.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    pub dev: u64,
    pub fid_lo: u64,
    pub fid_hi: u64,
    pub size: u64,
    pub mtime: i64,
    pub change: i64,
}

impl Stamp {
    pub fn same_id(&self, dev: u64, fid_lo: u64, fid_hi: u64) -> bool {
        self.dev == dev && self.fid_lo == fid_lo && self.fid_hi == fid_hi
    }
}

#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub stamp: Stamp,
    pub nlink: u32,
    pub alloc: i64,
    pub atime: i64,
    pub btime: i64,
    pub attrs: u32,
    pub tag: u32,
    pub usn: i64,
    /// 0 a regular file, else a reason (NOT_FILE, SURROGATE, CLOUD)
    pub kind_reason: u8,
    pub case_ok: bool,
}

/// An open data handle; counts itself in `open` while it lives.
pub struct Handle {
    pub file: File,
    pub unbuffered: bool,
    pub atime_unkept: bool,
    open: Arc<AtomicUsize>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.open.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Handle {
    fn new(file: File, unbuffered: bool, atime_unkept: bool, open: &Arc<AtomicUsize>) -> Self {
        open.fetch_add(1, Ordering::Relaxed);
        Handle {
            file,
            unbuffered,
            atime_unkept,
            open: open.clone(),
        }
    }
}

pub fn io_reason(e: &io::Error) -> u8 {
    #[cfg(unix)]
    {
        match e.raw_os_error() {
            Some(libc::ENOENT) | Some(libc::ENOTDIR) => return codes::GONE,
            Some(libc::EACCES) | Some(libc::EPERM) => return codes::DENIED,
            Some(libc::ELOOP) => return codes::SURROGATE,
            Some(libc::EISDIR) => return codes::NOT_FILE,
            Some(libc::ETXTBSY) => return codes::IN_USE,
            _ => {}
        }
    }
    #[cfg(windows)]
    {
        match e.raw_os_error() {
            Some(2) | Some(3) | Some(161) => return codes::GONE, // file / path not found, bad path
            Some(5) => return codes::DENIED,
            Some(32) | Some(33) => return codes::IN_USE, // sharing / lock violation
            Some(1920) => return codes::CLOUD,           // ERROR_CANT_ACCESS_FILE
            Some(c) if (362..=404).contains(&c) => return codes::CLOUD, // ERROR_CLOUD_FILE_*
            _ => {}
        }
    }
    match e.kind() {
        io::ErrorKind::NotFound => codes::GONE,
        io::ErrorKind::PermissionDenied => codes::DENIED,
        _ => codes::READ_ERROR,
    }
}

// --------------------------------------------------------------------------- POSIX

#[cfg(unix)]
fn ns(secs: i64, nsec: i64) -> i64 {
    secs.saturating_mul(1_000_000_000).saturating_add(nsec)
}

#[cfg(unix)]
impl Meta {
    fn from_std(m: &std::fs::Metadata) -> Meta {
        use std::os::unix::fs::MetadataExt;
        let ft = m.file_type();
        let btime = m
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        #[allow(unused_mut)]
        let mut attrs = 0u32;
        #[cfg(target_os = "macos")]
        {
            use std::os::macos::fs::MetadataExt as MacExt;
            attrs = m.st_flags();
        }
        let kind_reason = if ft.is_symlink() {
            codes::SURROGATE
        } else if !ft.is_file() {
            codes::NOT_FILE
        } else {
            #[cfg(target_os = "macos")]
            {
                if attrs & codes::SF_DATALESS != 0 {
                    codes::CLOUD
                } else {
                    codes::OK
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                codes::OK
            }
        };
        Meta {
            stamp: Stamp {
                dev: m.dev(),
                fid_lo: m.ino(),
                fid_hi: 0,
                size: m.size(),
                mtime: ns(m.mtime(), m.mtime_nsec()),
                change: ns(m.ctime(), m.ctime_nsec()),
            },
            nlink: m.nlink().min(u32::MAX as u64) as u32,
            alloc: (m.blocks() as i64).saturating_mul(512),
            atime: ns(m.atime(), m.atime_nsec()),
            btime,
            attrs,
            tag: 0,
            usn: 0,
            kind_reason,
            case_ok: true,
        }
    }

    pub fn of_file(f: &File) -> io::Result<Meta> {
        f.metadata().map(|m| Meta::from_std(&m))
    }
}

/// S1 on POSIX: `lstat` (no data access).
#[cfg(unix)]
pub fn meta_of_path(path: &str, _want_usn: bool) -> Result<Meta, u8> {
    std::fs::symlink_metadata(path)
        .map(|m| Meta::from_std(&m))
        .map_err(|e| io_reason(&e))
}

#[cfg(target_os = "macos")]
mod mac {
    unsafe extern "C" {
        fn setiopolicy_np(iotype: libc::c_int, scope: libc::c_int, policy: libc::c_int) -> libc::c_int;
    }
    const IOPOL_TYPE_VFS_ATIME_UPDATES: libc::c_int = 2;
    const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: libc::c_int = 3;
    const IOPOL_SCOPE_THREAD: libc::c_int = 1;
    const IOPOL_ATIME_UPDATES_OFF: libc::c_int = 1;
    const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: libc::c_int = 1;

    thread_local! {
        static ATIME_OFF: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
        static DATALESS_OFF: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    }

    /// Whether reads on this thread never materialize dataless files (iCloud Drive, File
    /// Provider): with the policy off, reading one fails instead of downloading it. Set once per
    /// thread; macOS 10.15+ (not verified on a real Mac here).
    pub fn dataless_off() -> bool {
        DATALESS_OFF.with(|c| {
            if let Some(v) = c.get() {
                return v;
            }
            // SAFETY: plain libc call with constant arguments.
            let ok = unsafe {
                setiopolicy_np(
                    IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                    IOPOL_SCOPE_THREAD,
                    IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
                )
            } == 0;
            c.set(Some(ok));
            ok
        })
    }

    /// Whether reads on this thread leave access times alone (asked once per thread).
    pub fn atime_off() -> bool {
        ATIME_OFF.with(|c| {
            if let Some(v) = c.get() {
                return v;
            }
            // SAFETY: plain libc call with constant arguments.
            let ok = unsafe {
                setiopolicy_np(
                    IOPOL_TYPE_VFS_ATIME_UPDATES,
                    IOPOL_SCOPE_THREAD,
                    IOPOL_ATIME_UPDATES_OFF,
                )
            } == 0;
            c.set(Some(ok));
            ok
        })
    }
}

/// Whether the file system of `path` is mounted noatime (reads never update access times).
#[cfg(target_os = "linux")]
fn mounted_noatime(path: &str) -> bool {
    let Ok(c) = std::ffi::CString::new(path) else {
        return false;
    };
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is NUL-terminated and `st` writable.
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    rc == 0 && st.f_flag & libc::ST_NOATIME != 0
}

/// S2/S3 data open on POSIX. `keep_read`: open anyway when the access time cannot be kept.
#[cfg(unix)]
pub fn open_data(
    path: &str,
    unbuffered: bool,
    keep_read: bool,
    open: &Arc<AtomicUsize>,
) -> Result<Handle, u8> {
    use std::os::unix::fs::OpenOptionsExt;
    #[cfg(target_os = "linux")]
    {
        let try_open = |flags: i32| {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(flags | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)
        };
        let direct = if unbuffered { libc::O_DIRECT } else { 0 };
        let mut atime_unkept = false;
        let mut res = try_open(libc::O_NOATIME | direct);
        if let Err(e) = &res
            && e.raw_os_error() == Some(libc::EPERM)
        {
            // not the owner and no CAP_FOWNER: O_NOATIME is refused
            if mounted_noatime(path) {
                res = try_open(direct);
            } else if keep_read {
                atime_unkept = true;
                res = try_open(direct);
            } else {
                return Err(codes::ATIME);
            }
        }
        let mut is_direct = unbuffered;
        if let Err(e) = &res
            && e.raw_os_error() == Some(libc::EINVAL)
            && unbuffered
        {
            is_direct = false; // the file system has no O_DIRECT (tmpfs before 6.6, FUSE)
            let keep = if atime_unkept { 0 } else { libc::O_NOATIME };
            res = try_open(keep);
        }
        let f = res.map_err(|e| io_reason(&e))?;
        Ok(Handle::new(f, is_direct, atime_unkept, open))
    }
    #[cfg(not(target_os = "linux"))]
    {
        #[cfg(target_os = "macos")]
        let atime_unkept = if mac::atime_off() {
            false
        } else if keep_read {
            true
        } else {
            return Err(codes::ATIME);
        };
        #[cfg(not(target_os = "macos"))]
        let atime_unkept = {
            if !keep_read {
                return Err(codes::ATIME);
            }
            true
        };
        // no download: a file that turns dataless between S1's check and the read fails to read
        // instead of being fetched. Where the policy cannot be set, nothing is read (like CLOUD).
        #[cfg(target_os = "macos")]
        if !mac::dataless_off() {
            return Err(codes::CLOUD);
        }
        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|e| io_reason(&e))?;
        #[cfg(target_os = "macos")]
        if unbuffered {
            use std::os::fd::AsRawFd;
            // SAFETY: valid descriptor; F_NOCACHE takes an int.
            unsafe { libc::fcntl(f.as_raw_fd(), libc::F_NOCACHE, 1) };
        }
        Ok(Handle::new(f, unbuffered, atime_unkept, open))
    }
}

/// Positioned read. With O_DIRECT, an EINVAL (alignment the device refuses) turns the handle
/// buffered and the read is done again.
#[cfg(unix)]
pub fn read_at(h: &mut Handle, buf: &mut [u8], off: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    loop {
        match h.file.read_at(buf, off) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) && h.unbuffered => {
                #[cfg(target_os = "linux")]
                {
                    use std::os::fd::AsRawFd;
                    let fd = h.file.as_raw_fd();
                    // SAFETY: valid descriptor.
                    unsafe {
                        let fl = libc::fcntl(fd, libc::F_GETFL);
                        if fl >= 0 {
                            libc::fcntl(fd, libc::F_SETFL, fl & !libc::O_DIRECT);
                        }
                    }
                }
                h.unbuffered = false;
            }
            Err(e) => return Err(e),
        }
    }
}

// --------------------------------------------------------------------------- Windows

#[cfg(windows)]
mod win {
    use super::*;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, WIN32_FIND_DATAW,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_NO_BUFFERING,
        FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_NAME_NORMALIZED,
        FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FILE_STANDARD_INFO, FILE_WRITE_ATTRIBUTES, FileAttributeTagInfo, FileBasicInfo, FileIdInfo,
        FileStandardInfo, GetFileInformationByHandleEx, GetFinalPathNameByHandleW, SYNCHRONIZE,
        VOLUME_NAME_NONE,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{FSCTL_READ_FILE_USN_DATA, READ_FILE_USN_DATA};

    const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    const ATTR_DIRECTORY: u32 = 0x10;
    const ATTR_REPARSE: u32 = 0x400;
    /// compressed, encrypted, sparse, reparse point: read buffered, through the file system's filters
    pub const BUFFERED_ATTRS: u32 = 0x800 | 0x4000 | 0x200 | ATTR_REPARSE;
    const UNIX_EPOCH_FT: i64 = 116_444_736_000_000_000;

    fn ft_ns(t: i64) -> i64 {
        if t <= 0 {
            return 0;
        }
        (t - UNIX_EPOCH_FT).saturating_mul(100)
    }

    /// `\\?\` form of an absolute path, so that names with a trailing dot or space open as listed
    /// (Win32 would trim them) and long paths need no special handling.
    pub fn verbatim(path: &str) -> std::ffi::OsString {
        let p = path.replace('/', "\\");
        if p.starts_with("\\\\?\\") || p.starts_with("\\\\.\\") {
            return p.into();
        }
        if let Some(rest) = p.strip_prefix("\\\\") {
            return format!("\\\\?\\UNC\\{rest}").into();
        }
        format!("\\\\?\\{p}").into()
    }

    fn info<T: Default>(h: HANDLE, class: i32) -> Option<T> {
        let mut v = T::default();
        // SAFETY: `v` is writable for its size; the class matches T at every call site.
        let ok = unsafe {
            GetFileInformationByHandleEx(h, class, (&raw mut v).cast(), size_of::<T>() as u32)
        };
        (ok != 0).then_some(v)
    }

    fn usn_of(h: HANDLE) -> i64 {
        let input = READ_FILE_USN_DATA {
            MinMajorVersion: 2,
            MaxMajorVersion: 3,
        };
        let mut out = [0u64; 128];
        let mut got = 0u32;
        // SAFETY: buffers valid for the sizes given.
        let ok = unsafe {
            DeviceIoControl(
                h,
                FSCTL_READ_FILE_USN_DATA,
                (&raw const input).cast(),
                size_of::<READ_FILE_USN_DATA>() as u32,
                out.as_mut_ptr().cast(),
                size_of_val(&out) as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || got < 8 {
            return 0;
        }
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(out.as_ptr().cast::<u8>(), got as usize) };
        let major = u16::from_le_bytes([bytes[4], bytes[5]]);
        let at = match major {
            2 => 24,
            3 => 40,
            _ => return 0,
        };
        if bytes.len() < at + 8 {
            return 0;
        }
        i64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
    }

    /// The handle's own path below its volume (`\Users\x\a.bin`), normalized.
    fn final_path(h: HANDLE) -> Option<Vec<u16>> {
        let mut buf = vec![0u16; 1024];
        loop {
            // SAFETY: buf has buf.len() units.
            let n = unsafe {
                GetFinalPathNameByHandleW(
                    h,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    FILE_NAME_NORMALIZED | VOLUME_NAME_NONE,
                )
            } as usize;
            if n == 0 {
                return None;
            }
            if n < buf.len() {
                buf.truncate(n);
                return Some(buf);
            }
            buf.resize(n + 1, 0);
        }
    }

    /// Whether the handle reached the name we asked for. A path that differs from the handle's
    /// own only in case is fine unless a folder on the way holds another entry of that name in
    /// another case (WinBtrfs trees Linux made, case-sensitive NTFS folders): only then may the open
    /// have reached a twin. The scan root as the user typed it (`c:\users\x`) is not a twin.
    fn case_ok(h: HANDLE, path: &str) -> bool {
        let p = path.replace('/', "\\");
        let (drive, below): (&str, &str) = if p.len() >= 2 && p.as_bytes()[1] == b':' {
            (&p[..2], &p[2..])
        } else {
            return true; // UNC and other forms: not checked
        };
        let Some(fin) = final_path(h) else {
            return true;
        };
        let want: Vec<u16> = std::ffi::OsStr::new(below).encode_wide().collect();
        if fin == want {
            return true;
        }
        let fin_s = std::ffi::OsString::from_wide(&fin).to_string_lossy().into_owned();
        if fin_s.to_lowercase() != below.to_lowercase() {
            return true; // another name altogether (8.3, a junction on the way): not a case twin
        }
        let got: Vec<&str> = fin_s.split('\\').collect();
        let asked: Vec<&str> = below.split('\\').collect();
        if got.len() != asked.len() {
            return true;
        }
        let mut parent = String::from(drive);
        for (g, a) in got.iter().zip(asked.iter()) {
            if g.is_empty() {
                continue;
            }
            if g != a && has_case_twin(&parent, a) {
                return false;
            }
            parent.push('\\');
            parent.push_str(g);
        }
        true
    }

    thread_local! {
        static TWINS: std::cell::RefCell<(std::time::Instant, std::collections::HashMap<String, bool>)> =
            std::cell::RefCell::new((std::time::Instant::now(), std::collections::HashMap::new()));
    }

    /// Whether `parent`, whose entry the open reached under another case than `asked`, holds
    /// another entry for that name: two or more entries match `asked` as a pattern (matching
    /// ignores case), or one is exactly `asked` (a file system matching case-sensitively). Asked
    /// of the file system once per folder and name, remembered for 2 s per thread.
    fn has_case_twin(parent: &str, asked: &str) -> bool {
        let key = format!("{}\\{}", parent, asked);
        let hit = TWINS.with(|t| {
            let mut t = t.borrow_mut();
            if t.0.elapsed().as_secs_f64() > 2.0 || t.1.len() > 4096 {
                t.0 = std::time::Instant::now();
                t.1.clear();
            }
            t.1.get(&key).copied()
        });
        if let Some(v) = hit {
            return v;
        }
        let pattern = verbatim(&format!("{}\\{}", parent, asked));
        let exact: Vec<u16> = std::ffi::OsStr::new(asked).encode_wide().collect();
        let wide: Vec<u16> = pattern.encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: zeroed is a valid WIN32_FIND_DATAW; the pattern is NUL-terminated.
        let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
        // SAFETY: valid pointers for the call.
        let find = unsafe {
            FindFirstFileExW(
                wide.as_ptr(),
                FindExInfoBasic,
                &mut data as *mut _ as *mut std::ffi::c_void,
                FindExSearchNameMatch,
                std::ptr::null(),
                0,
            )
        };
        let twin = if find == INVALID_HANDLE_VALUE {
            false // nothing matches the name as asked: the entry reached is the only one
        } else {
            let name_of = |d: &WIN32_FIND_DATAW| -> Vec<u16> {
                let end = d.cFileName.iter().position(|&u| u == 0).unwrap_or(d.cFileName.len());
                d.cFileName[..end].to_vec()
            };
            let mut n = 1;
            let mut same = name_of(&data) == exact;
            // SAFETY: `find` is a valid search handle until FindClose.
            while n < 2 && !same && unsafe { FindNextFileW(find, &mut data) } != 0 {
                n += 1;
                same |= name_of(&data) == exact;
            }
            // SAFETY: as above.
            unsafe { FindClose(find) };
            n >= 2 || same
        };
        TWINS.with(|t| t.borrow_mut().1.insert(key, twin));
        twin
    }

    impl Meta {
        pub fn of_handle(h: HANDLE) -> Option<Meta> {
            let id: FILE_ID_INFO = info(h, FileIdInfo)?;
            let std_: FILE_STANDARD_INFO = info(h, FileStandardInfo)?;
            let basic: FILE_BASIC_INFO = info(h, FileBasicInfo)?;
            let tag: FILE_ATTRIBUTE_TAG_INFO = info(h, FileAttributeTagInfo).unwrap_or_default();
            let fid = u128::from_le_bytes(id.FileId.Identifier);
            let attrs = basic.FileAttributes;
            let rtag = if attrs & ATTR_REPARSE != 0 { tag.ReparseTag } else { 0 };
            let kind_reason = if attrs & codes::RECALL_ATTRS != 0 || codes::cloud_tag(rtag) {
                codes::CLOUD
            } else if attrs & ATTR_DIRECTORY != 0 || std_.Directory as u8 != 0 {
                codes::NOT_FILE
            } else if codes::surrogate_tag(rtag) {
                codes::SURROGATE
            } else {
                codes::OK
            };
            Some(Meta {
                stamp: Stamp {
                    dev: id.VolumeSerialNumber,
                    fid_lo: fid as u64,
                    fid_hi: (fid >> 64) as u64,
                    size: std_.EndOfFile.max(0) as u64,
                    mtime: ft_ns(basic.LastWriteTime),
                    change: ft_ns(basic.ChangeTime),
                },
                nlink: std_.NumberOfLinks,
                alloc: std_.AllocationSize,
                atime: ft_ns(basic.LastAccessTime),
                btime: ft_ns(basic.CreationTime),
                attrs,
                tag: rtag,
                usn: 0,
                kind_reason,
                case_ok: true,
            })
        }

        pub fn of_file(f: &File) -> io::Result<Meta> {
            Meta::of_handle(f.as_raw_handle() as HANDLE).ok_or_else(io::Error::last_os_error)
        }
    }

    /// S1 on Windows: an attribute-only handle on the name itself (reparse points not followed).
    pub fn meta_of_path(path: &str, want_usn: bool) -> Result<Meta, u8> {
        let f = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | SYNCHRONIZE)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL | FILE_FLAG_BACKUP_SEMANTICS)
            .open(verbatim(path))
            .map_err(|e| io_reason(&e))?;
        let h = f.as_raw_handle() as HANDLE;
        let mut m = Meta::of_handle(h).ok_or(codes::READ_ERROR)?;
        if m.kind_reason == codes::OK {
            m.case_ok = case_ok(h, path);
            if want_usn {
                m.usn = usn_of(h);
            }
        }
        Ok(m)
    }

    pub fn open_data(
        path: &str,
        unbuffered: bool,
        keep_read: bool,
        open: &Arc<AtomicUsize>,
    ) -> Result<Handle, u8> {
        let try_open = |access: u32, unbuf: bool| {
            let mut flags = FILE_FLAG_OPEN_NO_RECALL;
            if unbuf {
                flags |= FILE_FLAG_NO_BUFFERING;
            }
            std::fs::OpenOptions::new()
                .access_mode(access)
                .share_mode(SHARE_ALL)
                .custom_flags(flags)
                .open(verbatim(path))
        };
        let full = FILE_READ_DATA | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE;
        let plain = FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
        let mut atime_unkept = false;
        let f = match try_open(full, unbuffered) {
            Ok(f) => {
                if !crate::winio::keep_access_time(f.as_raw_handle() as HANDLE) {
                    if !keep_read {
                        return Err(codes::ATIME);
                    }
                    atime_unkept = true;
                }
                f
            }
            Err(e) if e.raw_os_error() == Some(5) => {
                // FILE_WRITE_ATTRIBUTES refused (TrustedInstaller folders): the access time
                // cannot be kept on this handle
                if !keep_read {
                    return Err(codes::ATIME);
                }
                atime_unkept = true;
                try_open(plain, unbuffered).map_err(|e| io_reason(&e))?
            }
            Err(e) if e.raw_os_error() == Some(87) && unbuffered => {
                // ERROR_INVALID_PARAMETER: no unbuffered access here
                let f = try_open(full, false).map_err(|e| io_reason(&e))?;
                if !crate::winio::keep_access_time(f.as_raw_handle() as HANDLE) {
                    if !keep_read {
                        return Err(codes::ATIME);
                    }
                    atime_unkept = true;
                }
                return Ok(Handle::new(f, false, atime_unkept, open));
            }
            Err(e) => return Err(io_reason(&e)),
        };
        Ok(Handle::new(f, unbuffered, atime_unkept, open))
    }

    pub fn read_at(h: &mut Handle, buf: &mut [u8], off: u64) -> io::Result<usize> {
        use std::os::windows::fs::FileExt;
        match h.file.seek_read(buf, off) {
            Ok(n) => Ok(n),
            Err(e) if e.raw_os_error() == Some(38) => Ok(0), // ERROR_HANDLE_EOF
            Err(e) => Err(e),
        }
    }
}

#[cfg(windows)]
pub use win::{BUFFERED_ATTRS, meta_of_path, open_data, read_at, verbatim};

/// Attributes (from S1) for which reads go through the file system's buffered path.
#[cfg(not(windows))]
pub const BUFFERED_ATTRS: u32 = 0;

/// Short read helper: fills as much of `buf` as the file has from `off` (stops at EOF).
/// Unbuffered handles stop at the first short read (only EOF gives one on a regular file, and a
/// second read would start at an unaligned offset).
pub fn read_full_at(h: &mut Handle, buf: &mut [u8], off: u64) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        let n = read_at(h, &mut buf[got..], off + got as u64)?;
        if n == 0 {
            break;
        }
        got += n;
        if h.unbuffered && got < buf.len() {
            break;
        }
    }
    Ok(got)
}
