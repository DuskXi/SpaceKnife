//! POSIX lister: `open(O_DIRECTORY)` + `getdents64` (Linux) / `readdir` (elsewhere) +
//! `fstatat(dirfd, name, AT_SYMLINK_NOFOLLOW)` per entry: worker.py's `_list_posix`, syscall
//! for syscall, without the Python objects in between.

#[cfg(target_os = "linux")]
use std::cell::RefCell; // the getdents64 buffer
use std::ffi::{CStr, CString};

use super::common::{Arena, NativePath, RootOpts, Row, ext_len_with, flag, kind, secs};

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

/// Nothing to learn per root on POSIX.
pub struct RootState;

impl RootState {
    pub fn new() -> Self {
        Self
    }
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

fn add_entry(
    dirfd: libc::c_int,
    name: &CStr,
    dir: &[u8],
    root: &RootOpts,
    base: usize,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath)>,
) {
    let raw = name.to_bytes();
    let name_start = arena.names.len();
    arena.names.extend_from_slice(raw);
    let j = (arena.len() - base) as u32;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: name is NUL-terminated, st is writable.
    if unsafe { libc::fstatat(dirfd, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        arena.push_row(name_start, ERROR_ROW);
        return;
    }
    let mode = st.st_mode as u32 & libc::S_IFMT as u32;
    let mut flags = 0u32;
    let mut descend = false;
    let dev = st.st_dev as u64;
    let kind = if mode == libc::S_IFDIR as u32 {
        if (dev != root.dev && !root.cross_mounts) || is_skipped(&root.skip, dir, raw) {
            flags |= flag::MOUNT;
        } else {
            descend = true;
        }
        kind::DIR
    } else if mode == libc::S_IFLNK as u32 {
        kind::LINK
    } else if mode == libc::S_IFREG as u32 {
        kind::FILE
    } else {
        kind::OTHER
    };
    if raw.first() == Some(&b'.') {
        flags |= flag::HIDDEN;
    }
    let (bsec, bnsec) = st_birth(&st);
    let row = Row {
        kind,
        flags,
        size: if kind == kind::DIR {
            0
        } else {
            st.st_size as i64
        },
        alloc: (st.st_blocks as i64).saturating_mul(512),
        mtime: secs(st.st_mtime as i64, st.st_mtime_nsec as u64),
        ctime: secs(bsec, bnsec),
        atime: secs(st.st_atime as i64, st.st_atime_nsec as u64),
        ino: st.st_ino as u64,
        nlink: (st.st_nlink as u64).min(65535) as u16,
        dev,
        ext_len: if kind == kind::FILE {
            ext_len_with(raw, escaped_code_points)
        } else {
            0
        },
    };
    arena.push_row(name_start, row);
    if descend {
        subdirs.push((j, join_posix(dir, raw).into_boxed_slice()));
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

/// Lists `path` into `arena` (worker._list_posix); subdirectories to descend go to `subdirs`.
/// Directories found by this call are opened with O_NOFOLLOW (a swap for a symlink is refused);
/// given tasks are opened like the worker does, so a root may itself be a symlink.
pub fn list_dir(
    path: &[u8],
    discovered: bool,
    root: &RootOpts,
    _state: &RootState,
    arena: &mut Arena,
    subdirs: &mut Vec<(u32, NativePath)>,
) -> Result<(), String> {
    let c = CString::new(path).map_err(|_| "embedded null byte".to_owned())?;
    let mut oflags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    if discovered {
        oflags |= libc::O_NOFOLLOW;
    }
    // SAFETY: c is NUL-terminated.
    let fd = unsafe { libc::open(c.as_ptr(), oflags) };
    if fd < 0 {
        return Err(errno_message(errno()));
    }
    let fd = Fd(fd);
    let base = arena.len();
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
    fn joins_like_posixpath() {
        assert_eq!(join_posix(b"/", b"x"), b"/x");
        assert_eq!(join_posix(b"/a", b"x"), b"/a/x");
        assert_eq!(join_posix(b"", b"x"), b"x");
    }
}
