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
pub fn write_token_file(token: &str, directory: Option<&Path>) -> io::Result<PathBuf> {
    let dir = directory.map_or_else(std::env::temp_dir, Path::to_path_buf);
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
    let data = (|| {
        use std::io::Read as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let mut buf = Vec::with_capacity(256);
        opts.open(path)?.take(256).read_to_end(&mut buf)?;
        Ok::<_, io::Error>(buf)
    })();
    let _ = std::fs::remove_file(path); // the backend removes it as well once connected
    let token = String::from_utf8_lossy(&data?).trim().to_owned();
    if !is_token(&token) {
        return Err(invalid(t("令牌格式无效")));
    }
    Ok(token)
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

    #[test]
    fn token_file_is_private_and_read_once() {
        let dir = tempfile::tempdir().unwrap();
        let token = new_token();
        assert!(is_token(&token));
        let path = write_token_file(&token, Some(dir.path())).unwrap();
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
        let err = read_token_file(&dir.path().join("other.txt")).unwrap_err();
        assert!(
            err.to_string().contains("路径") || err.to_string().contains("path"),
            "{err}"
        );
        let bad = dir.path().join("spaceknife-helper-0123456789abcdef.token");
        std::fs::write(&bad, "not-a-token").unwrap();
        let err = read_token_file(&bad).unwrap_err();
        assert!(
            err.to_string().contains("格式") || err.to_string().contains("format"),
            "{err}"
        );
        assert!(!bad.exists());
        assert!(read_token_file(Path::new("spaceknife-helper-0123456789abcdef.token")).is_err()); // relative
    }

    #[test]
    fn tokens_compare() {
        assert!(tokens_match("abc", "abc"));
        assert!(!tokens_match("abc", "abd"));
        assert!(!tokens_match("abc", "abcd"));
    }
}
