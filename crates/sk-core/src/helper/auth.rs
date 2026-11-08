//! One-time token and peer checks (Python `helper/auth.py`).
//!
//! The backend writes a 32-byte random token (hex) to a temp file only the current user can read
//! (Windows: protected DACL, see [`super::win32::owner_only_sddl`]; POSIX: 0600, O_EXCL) and passes
//! the *path* on the helper's command line. The helper reads and deletes it and proves itself with
//! the token in its first frame; the backend also checks, from the OS, that the peer is the process
//! it launched. The helper in turn checks that the pipe/socket server is the backend pid it was
//! given.

use std::io;
use std::path::{Path, PathBuf};

use super::transport::hex;
use crate::i18n::t;

pub const TOKEN_BYTES: usize = 32;

fn random_bytes(n: usize) -> io::Result<Vec<u8>> {
    #[cfg(windows)]
    {
        Ok(super::win32::random_bytes(n))
    }
    #[cfg(unix)]
    {
        use std::io::Read as _;
        let mut buf = vec![0u8; n];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
        Ok(buf)
    }
}

pub fn new_token() -> String {
    hex(&random_bytes(TOKEN_BYTES).expect("the system's random source"))
}

fn is_token(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_token_file_name(name: &str) -> bool {
    name.strip_prefix("spaceknife-helper-")
        .and_then(|rest| rest.strip_suffix(".token"))
        .is_some_and(|hex| {
            hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// Create a fresh token file readable only by this user (in `directory`, default the temp
/// directory); returns its path.
///
/// POSIX: the path names the folder as it really is, without a symbolic link on the way: the
/// helper deletes the file only when it reaches it without one ([`read_token_file`]), and macOS's
/// temp folder is reached through one (`/var/folders/…` is `/private/var/folders/…`). Windows
/// keeps the folder as given: its helper takes the 8.3 and long forms of a name as the same place.
pub fn write_token_file(token: &str, directory: Option<&Path>) -> io::Result<PathBuf> {
    let dir = directory.map_or_else(std::env::temp_dir, Path::to_path_buf);
    #[cfg(unix)]
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let path = dir.join(format!(
        "spaceknife-helper-{}.token",
        hex(&random_bytes(8)?)
    ));
    write_new_private(&path, token.as_bytes())?;
    Ok(path)
}

/// Create `path` (it must not exist) readable only by this user, with `data`.
pub fn write_new_private(path: &Path, data: &[u8]) -> io::Result<()> {
    #[cfg(windows)]
    {
        super::win32::write_private_file(path, data)
    }
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        f.write_all(data)
    }
}

/// Helper side: read the token and delete the file. Refuses anything but a token file.
///
/// The helper may run as administrator while the file's folder belongs to the user: the file is
/// read, checked and deleted through one handle (Windows) or within the one folder opened (POSIX),
/// so a file or folder swapped meanwhile is never followed into a delete. When a junction, mount
/// point or symbolic link leads to the file, it is read but left in place; the backend removes it
/// as well once connected.
pub fn read_token_file(path: &Path) -> io::Result<String> {
    let invalid = |msg: &str| io::Error::new(io::ErrorKind::InvalidInput, msg.to_owned());
    let name_ok = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_token_file_name);
    if !path.is_absolute() || !name_ok {
        return Err(invalid(t("令牌文件路径无效")));
    }
    let meta = std::fs::symlink_metadata(path)?;
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    };
    #[cfg(unix)]
    let reparse = false;
    if !meta.file_type().is_file() || reparse {
        return Err(invalid(t("令牌文件不是普通文件")));
    }
    let (data, _deleted) = read_once(path, 256).map_err(|e| {
        if e.kind() == io::ErrorKind::InvalidInput {
            invalid(t("令牌文件不是普通文件"))
        } else {
            e
        }
    })?;
    let token = String::from_utf8_lossy(&data).trim().to_owned();
    if !is_token(&token) {
        return Err(invalid(t("令牌格式无效")));
    }
    Ok(token)
}

/// Read at most `max` bytes of the regular file `path` and delete it when nothing redirected the
/// way to it; `(bytes, deleted)`. An `InvalidInput` error: not a regular file.
#[cfg(windows)]
fn read_once(path: &Path, max: usize) -> io::Result<(Vec<u8>, bool)> {
    super::win32::read_file_once(path, max)
}

/// POSIX: the file is opened (`O_NOFOLLOW`) within its folder, and unlinked within that same
/// folder only if it is still the file read and the folder is the one at `path`'s parent when
/// walked from `/` without following any symbolic link.
#[cfg(unix)]
fn read_once(path: &Path, max: usize) -> io::Result<(Vec<u8>, bool)> {
    use std::io::Read as _;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let not_found = || io::Error::from(io::ErrorKind::NotFound);
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(not_found());
    };
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(parent)?;
    let cname = std::ffi::CString::new(name.as_bytes()).map_err(|_| not_found())?;
    // SAFETY: a directory fd we own and a NUL-terminated name.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            cname.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh fd we own.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut data = Vec::with_capacity(max);
    (&file).take(max as u64).read_to_end(&mut data)?;
    let in_place = walk_without_links(parent).is_ok_and(|walked| {
        let (a, b) = (walked.metadata(), dir.metadata());
        matches!((a, b), (Ok(a), Ok(b)) if a.dev() == b.dev() && a.ino() == b.ino())
    });
    #[allow(clippy::unnecessary_cast)] // st_dev / st_ino are not u64 everywhere
    let same = || {
        // SAFETY: zeroed POD out struct; our directory fd and a NUL-terminated name.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                cname.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0;
        ok && st.st_dev as u64 == meta.dev() && st.st_ino as u64 == meta.ino()
    };
    // SAFETY: our directory fd and a NUL-terminated name.
    let deleted =
        in_place && same() && unsafe { libc::unlinkat(dir.as_raw_fd(), cname.as_ptr(), 0) } == 0;
    Ok((data, deleted))
}

/// The folder `dir` (absolute), opened component by component from `/` without following a
/// symbolic link; an error when one is on the way.
#[cfg(unix)]
fn walk_without_links(dir: &Path) -> io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Component;

    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let mut cur = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open("/")?;
    for c in dir.components() {
        match c {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => {
                let cpart = std::ffi::CString::new(part.as_bytes())
                    .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
                // SAFETY: a directory fd we own and a NUL-terminated name.
                let fd = unsafe { libc::openat(cur.as_raw_fd(), cpart.as_ptr(), flags) };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: a fresh fd we own.
                cur = unsafe { std::fs::File::from_raw_fd(fd) };
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
        }
    }
    Ok(cur)
}

/// Constant-time comparison.
pub fn tokens_match(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Whether this process has administrator rights (Windows: BUILTIN\Administrators enabled; POSIX:
/// effective uid 0).
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        super::win32::is_admin()
    }
    #[cfg(unix)]
    {
        // SAFETY: no preconditions.
        unsafe { libc::geteuid() == 0 }
    }
}

/// Whether the connected peer is the process we started. Python also accepted a child of it
/// (a virtual environment's `python.exe` launcher); the single app binary has no launcher.
pub fn peer_is_launched(peer_pid: u32, launched_pid: u32) -> bool {
    peer_pid == launched_pid
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary folder reached without links (macOS: `/var` is a link to `/private/var`).
    fn plain_tempdir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(dir.path()).unwrap();
        #[cfg(windows)]
        let path = {
            let s = path.to_string_lossy().into_owned();
            PathBuf::from(s.strip_prefix(r"\\?\").map_or(s.clone(), str::to_owned))
        };
        (dir, path)
    }

    #[test]
    fn token_file_is_private_and_read_once() {
        let (_tmp, dir) = plain_tempdir();
        let token = new_token();
        assert!(is_token(&token));
        let path = write_token_file(&token, Some(&dir)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        #[cfg(windows)]
        {
            let sddl = super::super::win32::owner_only_sddl().unwrap();
            assert!(
                sddl.starts_with("D:P(A;;GA;;;S-1-") && sddl.ends_with("(A;;GA;;;BA)"),
                "{sddl}"
            );
        }
        let again = write_new_private(&path, token.as_bytes()).unwrap_err();
        assert_eq!(again.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(read_token_file(&path).unwrap(), token);
        assert!(!path.exists());
        let err = read_token_file(&dir.join("other.txt")).unwrap_err();
        assert!(
            err.to_string().contains("路径") || err.to_string().contains("path"),
            "{err}"
        );
        let bad = dir.join("spaceknife-helper-0123456789abcdef.token");
        std::fs::write(&bad, "not-a-token").unwrap();
        let err = read_token_file(&bad).unwrap_err();
        assert!(
            err.to_string().contains("格式") || err.to_string().contains("format"),
            "{err}"
        );
        assert!(!bad.exists());
        assert!(read_token_file(Path::new("spaceknife-helper-0123456789abcdef.token")).is_err()); // relative
    }

    /// A link `link` to the folder `target`: a junction (Windows; no rights needed) or a
    /// symbolic link. None when this system cannot make one.
    fn folder_link(target: &Path, link: &Path) -> Option<()> {
        #[cfg(windows)]
        {
            let ok = std::process::Command::new("cmd")
                .arg("/c")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            ok.then_some(())
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).ok()
        }
    }

    #[test]
    fn token_file_behind_a_link_is_read_but_never_deleted() {
        // the helper may run as administrator in a folder the user controls: when a junction /
        // symbolic link leads to the file (a folder swapped meanwhile), it reads the token but
        // never deletes through the link (the backend removes the file itself)
        let (_tmp, dir) = plain_tempdir();
        let real = dir.join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.join("link");
        if folder_link(&real, &link).is_none() {
            println!("skipped: cannot make a folder link here");
            return;
        }
        let token = new_token();
        let path = write_token_file(&token, Some(&real)).unwrap();
        let via = link.join(path.file_name().unwrap());
        assert_eq!(read_token_file(&via).unwrap(), token);
        assert!(path.exists(), "deleted through the link");
        // a link further up the way: the same
        let deeper = real.join("sub");
        std::fs::create_dir(&deeper).unwrap();
        let inner = write_token_file(&token, Some(&deeper)).unwrap();
        let via_up = link.join("sub").join(inner.file_name().unwrap());
        assert_eq!(read_token_file(&via_up).unwrap(), token);
        assert!(inner.exists(), "deleted through a link higher up");
        // the plain way: read once and gone
        assert_eq!(read_token_file(&path).unwrap(), token);
        assert!(!path.exists());
        assert_eq!(read_token_file(&inner).unwrap(), token);
        assert!(!inner.exists());
        // the token file itself a link: refused, the link and its target stay
        let target = write_token_file(&token, Some(&real)).unwrap();
        let file_link = dir.join("spaceknife-helper-fedcba9876543210.token");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&target, &file_link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&target, &file_link).is_ok(); // needs a privilege
        if made {
            let err = read_token_file(&file_link).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
            assert!(target.exists() && std::fs::symlink_metadata(&file_link).is_ok());
        }
    }

    #[cfg(unix)]
    #[test]
    fn token_file_in_a_temp_folder_named_through_a_link_is_deleted() {
        // macOS: the temp folder is /var/folders/…, and /var is a link to /private/var. The file is
        // named by where it really is, so the helper (which never deletes through a link) deletes
        // it once read
        let (_tmp, dir) = plain_tempdir();
        let real = dir.join("private-var");
        std::fs::create_dir(&real).unwrap();
        let var = dir.join("var");
        std::os::unix::fs::symlink(&real, &var).unwrap();
        let token = new_token();
        let path = write_token_file(&token, Some(&var)).unwrap();
        assert_eq!(path.parent(), Some(real.as_path()));
        assert_eq!(read_token_file(&path).unwrap(), token);
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn token_file_by_its_short_name_is_deleted() {
        // a temp folder given in 8.3 form (a CI runner's %TEMP% is C:\Users\RUNNER~1\…) is the
        // same place, not a redirection
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let (_tmp, dir) = plain_tempdir();
        let long = dir.join("a folder with a long name");
        std::fs::create_dir(&long).unwrap();
        let w: Vec<u16> = long.as_os_str().encode_wide().chain([0]).collect();
        let mut buf = vec![0u16; 1024];
        // SAFETY: NUL-terminated input, a buffer of the stated size.
        let n = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetShortPathNameW(
                w.as_ptr(),
                buf.as_mut_ptr(),
                buf.len() as u32,
            )
        } as usize;
        let short = PathBuf::from(std::ffi::OsString::from_wide(&buf[..n]));
        if n == 0 || short == long {
            println!("skipped: no 8.3 names on this volume");
            return;
        }
        let token = new_token();
        let path = write_token_file(&token, Some(&short)).unwrap();
        assert_eq!(read_token_file(&path).unwrap(), token);
        assert!(!path.exists() && !long.join(path.file_name().unwrap()).exists());
    }

    #[test]
    fn tokens_compare() {
        assert!(tokens_match("abc", "abc"));
        assert!(!tokens_match("abc", "abd"));
        assert!(!tokens_match("abc", "abcd"));
    }
}
