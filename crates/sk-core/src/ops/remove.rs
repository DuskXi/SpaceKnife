//! Deleting a folder with everything inside, for good (docs/manage.md §2.5, permanent deletion).
//!
//! What a folder holds is never reached by joining a listed name to the folder's path and asking
//! by that path again without checking the answer, because it can be another item:
//!
//! * a listed name may hold a path separator (WinBtrfs lists a Linux file `..\x` as it is; joined
//!   to `I:\sel` that is `I:\x`), a `:` (a stream of another file, Windows) or be `.` / `..`:
//!   such names are refused (`bad_name_inside`), never joined;
//! * a folder can be replaced by a link between the moment it is listed and the moment it is
//!   entered or its entries are deleted (what CVE-2022-21658 fixed in `std::fs::remove_dir_all`):
//!   the deletion never follows a link, and checks that each entry it opens is the one listed
//!   (`moved_inside` otherwise);
//! * another file system mounted inside (on Linux also a bind mount of the same one) is not
//!   entered, and a folder mount point on Windows is not removed (`mount_inside`).
//!
//! POSIX: every entry is reached relative to its folder's open descriptor (`fstatat`, `openat`
//! with `O_NOFOLLOW`, `unlinkat`), so a folder above that is renamed or replaced changes nothing;
//! a folder opened must be the one `fstatat` saw (device and inode) and on the same mount (Linux:
//! the `statx` mount id, else `/proc/self/mountinfo`).
//!
//! Windows: every entry is opened by its full `\\?\` path (no Win32 rewriting of trailing dots or
//! spaces, no MAX_PATH) without following a reparse point, and checked on that handle: on the same
//! volume, and opened under the name its folder's and the listed name give (a link swapped in
//! on the way resolves to another name). It is then deleted through that very handle (POSIX
//! semantics where the file system has them; a read-only item loses the flag first, as Explorer
//! does after asking). A folder is listed through a handle to it, checked to be the same folder.
//!
//! `cancel` is polled before each folder is entered.

use std::ffi::OsString;
use std::io;
use std::path::Path;

use super::text::trash_text as text;
use super::trash::{CancelFn, seams};

/// What a tree deletion did.
#[derive(Debug, Default)]
pub(super) struct Removed {
    /// the first thing that failed
    pub first: Option<io::Error>,
    pub cancelled: bool,
    /// anything at all was deleted
    pub deleted: bool,
}

impl Removed {
    fn fail(&mut self, e: io::Error) {
        if self.first.is_none() {
            self.first = Some(e);
        }
    }
}

pub(super) fn mount_inside() -> io::Error {
    io::Error::other(text("mount_inside"))
}

fn bad_name_inside() -> io::Error {
    io::Error::other(text("bad_name_inside"))
}

fn moved_inside() -> io::Error {
    io::Error::other(text("moved_inside"))
}

fn cancelled(cancel: Option<CancelFn<'_>>) -> bool {
    cancel.is_some_and(|c| c())
}

/// Whether a listed name (its UTF-16 units or bytes) can be joined to its folder's path and still
/// name it: not empty, `.` or `..`, no NUL, no separator (Windows: `\`, `/` and the stream
/// separator `:`; POSIX: `/`).
fn joinable<T: Copy + Into<u32>>(name: &[T]) -> bool {
    let units: Vec<u32> = name.iter().map(|&c| c.into()).collect();
    let dot = u32::from(b'.');
    if units.is_empty() || units == [dot] || units == [dot, dot] || units.contains(&0) {
        return false;
    }
    let seps: &[u8] = if cfg!(windows) { b"\\/:" } else { b"/" };
    !units
        .iter()
        .any(|c| seps.iter().any(|&s| u32::from(s) == *c))
}

/// The names of a listing, with the test seam applied.
fn seam_names(folder: &Path, mut names: Vec<OsString>) -> Vec<OsString> {
    seams::listed(folder, &mut names);
    names
}

/// Delete the folder `top` (a real folder, not a link: the caller looked) and everything in it.
pub(super) fn remove_tree(top: &Path, cancel: Option<CancelFn<'_>>) -> Removed {
    imp::remove_tree(top, cancel)
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_that_can_be_joined() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        for ok in ["a", "a b", "...", "odd.", "a\u{FFFD}"] {
            assert!(super::joinable(&w(ok)), "{ok}");
            assert!(super::joinable(ok.as_bytes()), "{ok}");
        }
        for bad in ["", ".", "..", "a/b", "../x", "a\0b"] {
            assert!(!super::joinable(&w(bad)), "{bad:?}");
            assert!(!super::joinable(bad.as_bytes()), "{bad:?}");
        }
        for name in ["..\\x", "a:b"] {
            assert_eq!(super::joinable(&w(name)), !cfg!(windows), "{name}");
        }
    }
}

// --------------------------------------------------------------------------- POSIX

#[cfg(unix)]
mod imp {
    use std::ffi::{CString, OsString};
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    use super::*;

    struct Frame {
        fd: OwnedFd,
        /// its name in its parent's folder (the top: empty)
        name: CString,
        /// as given (messages, the seams)
        path: PathBuf,
        /// below the top resolved: what the mount table lists (only without mount ids)
        real: Option<PathBuf>,
        names: std::vec::IntoIter<OsString>,
    }

    fn last() -> io::Error {
        io::Error::last_os_error()
    }

    fn open_dir_at(dir: RawFd, name: &CString) -> io::Result<OwnedFd> {
        // SAFETY: a NUL-terminated name; the descriptor is owned from here on.
        let fd = unsafe {
            libc::openat(
                dir,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(last());
        }
        // SAFETY: a descriptor just opened.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn stat_at(dir: RawFd, name: &CString) -> io::Result<libc::stat> {
        // SAFETY: zeroed is a valid stat; the name is NUL-terminated.
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) != 0 {
                return Err(last());
            }
            Ok(st)
        }
    }

    fn stat_fd(fd: RawFd) -> io::Result<libc::stat> {
        // SAFETY: zeroed is a valid stat.
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstat(fd, &mut st) != 0 {
                return Err(last());
            }
            Ok(st)
        }
    }

    fn is_dir(st: &libc::stat) -> bool {
        st.st_mode & libc::S_IFMT == libc::S_IFDIR
    }

    fn unlink_at(dir: RawFd, name: &CString, folder: bool) -> io::Result<()> {
        let flags = if folder { libc::AT_REMOVEDIR } else { 0 };
        // SAFETY: a NUL-terminated name.
        if unsafe { libc::unlinkat(dir, name.as_ptr(), flags) } != 0 {
            return Err(last());
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn clear_errno() {
        // SAFETY: this thread's errno.
        unsafe { *libc::__errno_location() = 0 }
    }

    #[cfg(target_os = "macos")]
    fn clear_errno() {
        // SAFETY: this thread's errno.
        unsafe { *libc::__error() = 0 }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn clear_errno() {}

    /// The names in the folder open as `fd` (`.` and `..` left out).
    fn list(fd: RawFd) -> io::Result<Vec<OsString>> {
        // SAFETY: the duplicate belongs to the DIR stream, closed below; each entry is copied
        // before the next readdir.
        unsafe {
            let dup = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0);
            if dup < 0 {
                return Err(last());
            }
            let dir = libc::fdopendir(dup);
            if dir.is_null() {
                let e = last();
                libc::close(dup);
                return Err(e);
            }
            let mut names = Vec::new();
            let result = loop {
                clear_errno();
                let ent = libc::readdir(dir);
                if ent.is_null() {
                    let e = last();
                    break match e.raw_os_error() {
                        Some(0) | None => Ok(()),
                        _ => Err(e),
                    };
                }
                let name = std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()).to_bytes();
                if name != b"." && name != b".." {
                    names.push(OsString::from_vec(name.to_vec()));
                }
            };
            libc::closedir(dir);
            result.map(|()| names)
        }
    }

    fn frame(
        fd: OwnedFd,
        name: CString,
        path: PathBuf,
        real: Option<PathBuf>,
    ) -> io::Result<Frame> {
        let names = super::seam_names(&path, list(fd.as_raw_fd())?);
        Ok(Frame {
            fd,
            name,
            path,
            real,
            names: names.into_iter(),
        })
    }

    struct Tree {
        dev: libc::dev_t,
        /// the top's mount id (Linux 5.8+)
        mount: Option<u64>,
        /// the mount table, where there are no mount ids
        mounts: Option<Mounts>,
    }

    pub(super) fn remove_tree(top: &Path, cancel: Option<CancelFn<'_>>) -> Removed {
        let mut out = Removed::default();
        if super::cancelled(cancel) {
            out.cancelled = true;
            return out;
        }
        seams::entering(top);
        let start = (|| -> io::Result<(Frame, Tree)> {
            let c = CString::new(top.as_os_str().as_bytes())
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            let fd = open_dir_at(libc::AT_FDCWD, &c).map_err(|e| match e.raw_os_error() {
                Some(libc::ELOOP) | Some(libc::ENOTDIR) => super::moved_inside(),
                _ => e,
            })?;
            let st = stat_fd(fd.as_raw_fd())?;
            let mount = mount_id(fd.as_raw_fd());
            let mounts = if mount.is_none() {
                Mounts::read()
            } else {
                None
            };
            let real = mounts
                .as_ref()
                .map(|_| super::super::paths::resolve_lenient(top));
            let f = frame(fd, CString::default(), top.to_path_buf(), real)?;
            Ok((
                f,
                Tree {
                    dev: st.st_dev,
                    mount,
                    mounts,
                },
            ))
        })();
        let (first, tree) = match start {
            Ok(s) => s,
            Err(e) => {
                out.fail(e);
                return out;
            }
        };
        let mut stack = vec![first];
        while let Some(f) = stack.last_mut() {
            let Some(name) = f.names.next() else {
                let done = stack.pop().unwrap();
                let gone = match stack.last() {
                    Some(parent) => unlink_at(parent.fd.as_raw_fd(), &done.name, true),
                    None => {
                        drop(done.fd);
                        std::fs::remove_dir(top)
                    }
                };
                match gone {
                    Ok(()) => out.deleted = true,
                    Err(e) => out.fail(e),
                }
                continue;
            };
            if !super::joinable(name.as_bytes()) {
                out.fail(super::bad_name_inside());
                continue;
            }
            let Ok(c) = CString::new(name.as_bytes()) else {
                out.fail(super::bad_name_inside());
                continue;
            };
            let path = f.path.join(&name);
            let real = f.real.as_ref().map(|r| r.join(&name));
            seams::entering(&path);
            let dirfd = f.fd.as_raw_fd();
            let st = match stat_at(dirfd, &c) {
                Ok(st) => st,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue, // gone meanwhile
                Err(e) => {
                    out.fail(e);
                    continue;
                }
            };
            if !is_dir(&st) {
                // a file or a link: the entry itself, never what it points to
                match unlink_at(dirfd, &c, false) {
                    Ok(()) => out.deleted = true,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => out.fail(e),
                }
                continue;
            }
            if st.st_dev != tree.dev {
                out.fail(super::mount_inside());
                continue;
            }
            if super::cancelled(cancel) {
                out.cancelled = true;
                return out;
            }
            let opened = (|| -> io::Result<Frame> {
                let cfd = open_dir_at(dirfd, &c).map_err(|e| match e.raw_os_error() {
                    Some(libc::ELOOP) | Some(libc::ENOTDIR) => super::moved_inside(),
                    _ => e,
                })?;
                let now = stat_fd(cfd.as_raw_fd())?;
                if now.st_dev != st.st_dev || now.st_ino != st.st_ino {
                    return Err(super::moved_inside()); // replaced since it was looked at
                }
                let elsewhere = match (tree.mount, mount_id(cfd.as_raw_fd())) {
                    (Some(a), Some(b)) => a != b,
                    _ => match (&tree.mounts, &real) {
                        (Some(m), Some(r)) => m.contains(r),
                        _ => false,
                    },
                };
                if elsewhere {
                    return Err(super::mount_inside());
                }
                frame(cfd, c.clone(), path.clone(), real.clone())
            })();
            match opened {
                Ok(child) => stack.push(child),
                Err(e) => out.fail(e),
            }
        }
        out
    }

    /// The id of the mount the open `fd` is on (Linux 5.8+), else None.
    #[cfg(target_os = "linux")]
    fn mount_id(fd: RawFd) -> Option<u64> {
        // SAFETY: zeroed is a valid statx; an empty path with AT_EMPTY_PATH is `fd` itself.
        unsafe {
            let mut stx: libc::statx = std::mem::zeroed();
            let rc = libc::statx(
                fd,
                c"".as_ptr(),
                libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
                libc::STATX_MNT_ID,
                &mut stx,
            );
            (rc == 0 && stx.stx_mask & libc::STATX_MNT_ID != 0).then_some(stx.stx_mnt_id)
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn mount_id(_fd: RawFd) -> Option<u64> {
        None
    }

    /// The mount points `/proc/self/mountinfo` lists (Linux; None elsewhere or unreadable).
    struct Mounts(std::collections::HashSet<Vec<u8>>);

    impl Mounts {
        fn read() -> Option<Mounts> {
            if !cfg!(target_os = "linux") {
                return None;
            }
            let text = std::fs::read("/proc/self/mountinfo").ok()?;
            Some(Mounts(
                text.split(|&b| b == b'\n')
                    .filter_map(|line| line.split(|&b| b == b' ').nth(4))
                    .map(unescape)
                    .collect(),
            ))
        }

        fn contains(&self, path: &Path) -> bool {
            self.0.contains(path.as_os_str().as_bytes())
        }
    }

    /// mountinfo's octal escapes (`\040` for a space).
    fn unescape(field: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(field.len());
        let mut i = 0;
        while i < field.len() {
            if field[i] == b'\\'
                && i + 3 < field.len()
                && field[i + 1..i + 4]
                    .iter()
                    .all(|c| (b'0'..=b'7').contains(c))
            {
                let v = field[i + 1..i + 4]
                    .iter()
                    .fold(0u32, |a, c| a * 8 + u32::from(c - b'0'));
                out.push(v as u8);
                i += 4;
            } else {
                out.push(field[i]);
                i += 1;
            }
        }
        out
    }

    /// Whether the folder `path` is on a mount of its own (bind mounts of the same file system
    /// too): its mount is not its parent's.
    pub(crate) fn other_mount_than_parent(path: &Path) -> bool {
        let open = |p: &Path| -> Option<OwnedFd> {
            let c = CString::new(p.as_os_str().as_bytes()).ok()?;
            open_dir_at(libc::AT_FDCWD, &c).ok()
        };
        let (Some(me), Some(up)) = (open(path), open(&path.join(".."))) else {
            return false;
        };
        match (mount_id(me.as_raw_fd()), mount_id(up.as_raw_fd())) {
            (Some(a), Some(b)) => a != b,
            _ => Mounts::read()
                .is_some_and(|m| m.contains(&super::super::paths::resolve_lenient(path))),
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn mountinfo_escapes() {
            assert_eq!(super::unescape(b"/mnt/a\\040b"), b"/mnt/a b");
            assert_eq!(super::unescape(b"/x\\134y"), b"/x\\y");
            assert_eq!(super::unescape(b"/plain"), b"/plain");
            assert_eq!(super::unescape(b"/end\\04"), b"/end\\04");
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn mount_ids_are_known() {
            use std::os::fd::AsRawFd;
            let dir = tempfile::tempdir().unwrap();
            let f = std::fs::File::open(dir.path()).unwrap();
            // every supported kernel (RHEL 9: 5.14, Ubuntu 22.04: 5.15) has them
            assert!(super::mount_id(f.as_raw_fd()).is_some());
            assert!(!super::other_mount_than_parent(dir.path()));
        }
    }
}

#[cfg(unix)]
pub(super) use imp::other_mount_than_parent;

// --------------------------------------------------------------------------- Windows

#[cfg(windows)]
mod imp {
    use std::ffi::{OsString, c_void};
    use std::io;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FULL_DIR_INFO, GetFileInformationByHandle,
        GetFileInformationByHandleEx, SetFileInformationByHandle,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    use super::*;

    const DELETE: u32 = 0x0001_0000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_LIST_DIRECTORY: u32 = 0x0001;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const SHARE_ALL: u32 = 7;
    const OPEN_EXISTING: u32 = 3;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    const ATTR_READONLY: u32 = 0x1;
    const ATTR_DIRECTORY: u32 = 0x10;
    const ATTR_NORMAL: u32 = 0x80;
    const ATTR_REPARSE_POINT: u32 = 0x400;
    const TAG_NAME_SURROGATE: u32 = 0x2000_0000;
    const TAG_MOUNT_POINT: u32 = 0xA000_0003;

    const FILE_BASIC_INFO: i32 = 0;
    const FILE_NAME_INFO: i32 = 2;
    const FILE_DISPOSITION_INFO: i32 = 4;
    const FILE_ATTRIBUTE_TAG_INFO: i32 = 9;
    const FILE_FULL_DIRECTORY_INFO: i32 = 14;
    const FILE_FULL_DIRECTORY_RESTART_INFO: i32 = 15;
    const FILE_DISPOSITION_INFO_EX: i32 = 21;
    const DISPOSITION_DELETE: u32 = 0x1;
    const DISPOSITION_POSIX: u32 = 0x2;
    const DISPOSITION_IGNORE_READONLY: u32 = 0x10;
    const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_NO_MORE_FILES: i32 = 18;
    const ERROR_SHARING_VIOLATION: i32 = 32;

    const BACKSLASH: u16 = b'\\' as u16;

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: a handle this owns.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn last() -> io::Error {
        io::Error::last_os_error()
    }

    /// Open by a `\\?\` path; `follow`: through a reparse point at the end (else the entry
    /// itself).
    fn open(path: &[u16], access: u32, follow: bool) -> io::Result<Handle> {
        let mut w: Vec<u16> = path.to_vec();
        w.push(0);
        let flags = BACKUP_SEMANTICS | if follow { 0 } else { OPEN_REPARSE_POINT };
        // SAFETY: a NUL-terminated path; the handle is owned from here on.
        let h = unsafe {
            CreateFileW(
                w.as_ptr(),
                access,
                SHARE_ALL,
                null(),
                OPEN_EXISTING,
                flags,
                null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            return Err(last());
        }
        Ok(Handle(h))
    }

    /// What an open handle is.
    struct Facts {
        attrs: u32,
        tag: u32,
        volume: u32,
        index: u64,
        /// the name it was opened under, after any reparse (None: the file system cannot say)
        name: Option<Vec<u16>>,
        /// it could not be opened for deletion: why (a folder is still emptied)
        undeletable: Option<i32>,
    }

    impl Facts {
        fn dir(&self) -> bool {
            self.attrs & ATTR_DIRECTORY != 0
        }
        fn reparse(&self) -> bool {
            self.attrs & ATTR_REPARSE_POINT != 0
        }
        /// a link of some kind: symlink, junction, mount point
        fn link(&self) -> bool {
            self.reparse() && self.tag & TAG_NAME_SURROGATE != 0
        }
    }

    fn identity(h: &Handle) -> io::Result<(u32, u64)> {
        // SAFETY: zeroed is a valid BY_HANDLE_FILE_INFORMATION; the handle is open.
        unsafe {
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            if GetFileInformationByHandle(h.0, &mut info) == 0 {
                return Err(last());
            }
            Ok((
                info.dwVolumeSerialNumber,
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            ))
        }
    }

    /// The name `h` was opened under, below its volume, after reparse points on the way.
    fn opened_name(h: &Handle, buf: &mut [u64]) -> Option<Vec<u16>> {
        let bytes = (buf.len() * 8) as u32;
        // SAFETY: the buffer holds `bytes` bytes, 8-aligned (FILE_NAME_INFO: a u32 length in
        // bytes, then the name).
        unsafe {
            if GetFileInformationByHandleEx(
                h.0,
                FILE_NAME_INFO,
                buf.as_mut_ptr() as *mut c_void,
                bytes,
            ) == 0
            {
                return None;
            }
            let len = (*(buf.as_ptr() as *const u32) as usize / 2).min((buf.len() * 8 - 4) / 2);
            let name = std::slice::from_raw_parts((buf.as_ptr() as *const u16).add(2), len);
            Some(name.to_vec())
        }
    }

    /// Open an entry for deletion and tell what it is; None: it is gone. Where deleting it is
    /// refused (no right to, another program holds it open without sharing that), it is opened
    /// to look only: a folder can still be emptied.
    fn open_entry(path: &[u16], buf: &mut [u64]) -> io::Result<Option<(Handle, Facts)>> {
        let (h, undeletable) = match open(path, DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE, false)
        {
            Ok(h) => (h, None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(ERROR_ACCESS_DENIED) | Some(ERROR_SHARING_VIOLATION)
                ) =>
            {
                match open(path, FILE_READ_ATTRIBUTES | SYNCHRONIZE, false) {
                    Ok(h) => (h, e.raw_os_error()),
                    Err(_) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        };
        let mut tag = [0u32; 2];
        // SAFETY: FILE_ATTRIBUTE_TAG_INFO is two u32s.
        if unsafe {
            GetFileInformationByHandleEx(
                h.0,
                FILE_ATTRIBUTE_TAG_INFO,
                tag.as_mut_ptr() as *mut c_void,
                8,
            )
        } == 0
        {
            return Err(last());
        }
        let (volume, index) = identity(&h)?;
        let name = opened_name(&h, buf);
        Ok(Some((
            h,
            Facts {
                attrs: tag[0],
                tag: tag[1],
                volume,
                index,
                name,
                undeletable,
            },
        )))
    }

    /// Whether a mount-point reparse point mounts a volume (`\??\Volume{…}\`, what `mountvol`
    /// writes) rather than being a junction to a folder. Unreadable: the volume lookup decides.
    fn mounts_a_volume(h: &Handle, path: &[u16]) -> bool {
        let mut buf = vec![0u64; 16 * 1024 / 8];
        let mut got = 0u32;
        // SAFETY: an output buffer of the size given; no input, no overlapped I/O.
        let ok = unsafe {
            DeviceIoControl(
                h.0,
                FSCTL_GET_REPARSE_POINT,
                null(),
                0,
                buf.as_mut_ptr() as *mut c_void,
                (buf.len() * 8) as u32,
                &mut got,
                null_mut(),
            )
        } != 0;
        if !ok || (got as usize) < 16 {
            return super::super::paths::is_mount(&as_path(path));
        }
        // SAFETY: `got` bytes were written, the 16-byte header at least.
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, got as usize) };
        let u16_at = |i: usize| usize::from(u16::from_le_bytes([bytes[i], bytes[i + 1]]));
        if u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != TAG_MOUNT_POINT {
            return false;
        }
        // the substitute name, after the 16-byte header of a mount-point buffer
        let (off, len) = (16 + u16_at(8), u16_at(10));
        if off + len > bytes.len() {
            return true;
        }
        let units: Vec<u16> = bytes[off..off + len]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let name = String::from_utf16_lossy(&units).to_ascii_lowercase();
        name.starts_with("\\??\\volume{") || name.starts_with("\\\\?\\volume{")
    }

    fn set_disposition(h: &Handle, ex_flags: Option<u32>) -> io::Result<()> {
        // SAFETY: FILE_DISPOSITION_INFO_EX is one u32, FILE_DISPOSITION_INFO one BOOLEAN.
        let ok = unsafe {
            match ex_flags {
                Some(flags) => {
                    SetFileInformationByHandle(
                        h.0,
                        FILE_DISPOSITION_INFO_EX,
                        &flags as *const u32 as *const c_void,
                        4,
                    ) != 0
                }
                None => {
                    let delete: u8 = 1;
                    SetFileInformationByHandle(
                        h.0,
                        FILE_DISPOSITION_INFO,
                        &delete as *const u8 as *const c_void,
                        1,
                    ) != 0
                }
            }
        };
        if ok { Ok(()) } else { Err(last()) }
    }

    /// Delete what `h` is open on: POSIX semantics where the file system has them, else the
    /// classic flag; a read-only item loses the flag first (through another handle checked to
    /// be the same item), as Explorer does after asking.
    fn delete(h: &Handle, path: &[u16], f: &Facts) -> io::Result<()> {
        if let Some(code) = f.undeletable {
            return Err(io::Error::from_raw_os_error(code));
        }
        let all = DISPOSITION_DELETE | DISPOSITION_POSIX | DISPOSITION_IGNORE_READONLY;
        if set_disposition(h, Some(all)).is_ok() {
            return Ok(());
        }
        match set_disposition(h, None) {
            Ok(()) => Ok(()),
            Err(e)
                if f.attrs & ATTR_READONLY != 0
                    && e.raw_os_error() == Some(ERROR_ACCESS_DENIED) =>
            {
                let w = open(path, FILE_WRITE_ATTRIBUTES | SYNCHRONIZE, false)?;
                if identity(&w)? != (f.volume, f.index) {
                    return Err(super::moved_inside());
                }
                let attrs = match f.attrs & !ATTR_READONLY {
                    0 => ATTR_NORMAL,
                    a => a,
                };
                // FILE_BASIC_INFO: four times (0: unchanged), then the attributes
                let basic = [0u64, 0, 0, 0, u64::from(attrs)];
                // SAFETY: a FILE_BASIC_INFO-sized, 8-aligned buffer.
                if unsafe {
                    SetFileInformationByHandle(
                        w.0,
                        FILE_BASIC_INFO,
                        basic.as_ptr() as *const c_void,
                        40,
                    )
                } == 0
                {
                    return Err(last());
                }
                drop(w);
                set_disposition(h, Some(DISPOSITION_DELETE | DISPOSITION_POSIX))
                    .or_else(|_| set_disposition(h, None))
            }
            Err(e) => Err(e),
        }
    }

    /// The names in the folder `h` is open on (with listing access).
    fn list(h: &Handle, buf: &mut [u64]) -> io::Result<Vec<OsString>> {
        let bytes = (buf.len() * 8) as u32;
        let name_at = std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName);
        let len_at = std::mem::offset_of!(FILE_FULL_DIR_INFO, FileNameLength);
        let malformed = || io::Error::other("malformed directory listing");
        let mut names = Vec::new();
        let mut class = FILE_FULL_DIRECTORY_RESTART_INFO;
        loop {
            // SAFETY: the buffer holds `bytes` bytes, 8-aligned.
            let ok = unsafe {
                GetFileInformationByHandleEx(h.0, class, buf.as_mut_ptr() as *mut c_void, bytes)
            } != 0;
            if !ok {
                let e = last();
                if e.raw_os_error() == Some(ERROR_NO_MORE_FILES) {
                    return Ok(names);
                }
                return Err(e);
            }
            class = FILE_FULL_DIRECTORY_INFO;
            // SAFETY: the buffer is initialised memory of `bytes` bytes.
            let raw =
                unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, bytes as usize) };
            let u32_at = |i: usize| -> io::Result<usize> {
                let b = raw.get(i..i + 4).ok_or_else(malformed)?;
                Ok(u32::from_le_bytes(b.try_into().unwrap()) as usize)
            };
            let mut at = 0usize;
            loop {
                let next = u32_at(at)?;
                let len = u32_at(at + len_at)?;
                let name = raw
                    .get(at + name_at..at + name_at + len)
                    .ok_or_else(malformed)?;
                let units: Vec<u16> = name
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                if units != [b'.' as u16] && units != [b'.' as u16, b'.' as u16] {
                    names.push(OsString::from_wide(&units));
                }
                if next == 0 {
                    break;
                }
                at += next;
            }
        }
    }

    /// `\\?\` and the absolute path (no Win32 rewriting, no MAX_PATH).
    fn verbatim(p: &Path) -> Vec<u16> {
        let abs = super::super::paths::absolute(p);
        let w: Vec<u16> = abs.as_os_str().encode_wide().collect();
        let starts = |s: &str| w.starts_with(&s.encode_utf16().collect::<Vec<u16>>());
        if starts("\\\\?\\") || starts("\\??\\") {
            w
        } else if starts("\\\\") {
            "\\\\?\\UNC\\"
                .encode_utf16()
                .chain(w[2..].iter().copied())
                .collect()
        } else {
            "\\\\?\\".encode_utf16().chain(w.iter().copied()).collect()
        }
    }

    fn join(parent: &[u16], name: &[u16]) -> Vec<u16> {
        let mut out = parent.to_vec();
        if out.last() != Some(&BACKSLASH) {
            out.push(BACKSLASH);
        }
        out.extend_from_slice(name);
        out
    }

    fn as_path(w: &[u16]) -> PathBuf {
        PathBuf::from(OsString::from_wide(w))
    }

    struct Frame {
        /// what is deleted once its entries are gone
        h: Handle,
        facts: Facts,
        path: Vec<u16>,
        names: std::vec::IntoIter<OsString>,
    }

    /// List the folder `h` is open on: through a listing handle checked to be the same folder
    /// (for a folder that is a reparse point other than a link, cloud files, one its filter may
    /// fill in).
    fn frame(h: Handle, facts: Facts, path: Vec<u16>, buf: &mut [u64]) -> io::Result<Frame> {
        let lister = open(
            &path,
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            facts.reparse(),
        )?;
        if identity(&lister)? != (facts.volume, facts.index) {
            return Err(super::moved_inside());
        }
        let names = super::seam_names(&as_path(&path), list(&lister, buf)?);
        Ok(Frame {
            h,
            facts,
            path,
            names: names.into_iter(),
        })
    }

    pub(super) fn remove_tree(top: &Path, cancel: Option<CancelFn<'_>>) -> Removed {
        let mut out = Removed::default();
        if super::cancelled(cancel) {
            out.cancelled = true;
            return out;
        }
        // 64 KiB exactly: WinBtrfs refuses a name buffer of a few bytes more
        let mut buf = vec![0u64; 65536 / 8];
        seams::entering(top);
        let top_w = verbatim(top);
        let start = (|| -> io::Result<Frame> {
            let Some((h, f)) = open_entry(&top_w, &mut buf)? else {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            };
            if !f.dir() || f.link() {
                return Err(super::moved_inside()); // replaced since the caller looked
            }
            frame(h, f, top_w.clone(), &mut buf)
        })();
        let first = match start {
            Ok(f) => f,
            Err(e) => {
                out.fail(e);
                return out;
            }
        };
        let (volume, names_known) = (first.facts.volume, first.facts.name.is_some());
        let mut stack = vec![first];
        while let Some(f) = stack.last_mut() {
            let Some(name) = f.names.next() else {
                let done = stack.pop().unwrap();
                match delete(&done.h, &done.path, &done.facts) {
                    Ok(()) => out.deleted = true,
                    Err(e) => out.fail(e),
                }
                continue;
            };
            let units: Vec<u16> = name.encode_wide().collect();
            if !super::joinable(&units) {
                out.fail(super::bad_name_inside());
                continue;
            }
            let path = join(&f.path, &units);
            let expected = f.facts.name.as_ref().map(|n| join(n, &units));
            seams::entering(&as_path(&path));
            let (h, facts) = match open_entry(&path, &mut buf) {
                Ok(Some(found)) => found,
                Ok(None) => continue, // gone meanwhile
                Err(e) => {
                    out.fail(e);
                    continue;
                }
            };
            // the very entry listed: on this volume, under this folder, by this name
            if facts.volume != volume || (names_known && facts.name != expected) {
                out.fail(super::moved_inside());
                continue;
            }
            if facts.dir() && !facts.link() {
                if super::cancelled(cancel) {
                    out.cancelled = true;
                    return out;
                }
                match frame(h, facts, path, &mut buf) {
                    Ok(child) => stack.push(child),
                    Err(e) => out.fail(e),
                }
                continue;
            }
            if facts.dir() && facts.tag == TAG_MOUNT_POINT && mounts_a_volume(&h, &path) {
                out.fail(super::mount_inside());
                continue;
            }
            // a file, or a link of any kind: the entry itself, never what it points to
            match delete(&h, &path, &facts) {
                Ok(()) => out.deleted = true,
                Err(e) => out.fail(e),
            }
        }
        out
    }
}
