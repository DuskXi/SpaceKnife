//! macOS privacy protection (TCC) a disk scan runs into: Full Disk Access, and the folders that ask.
//!
//! Without Full Disk Access macOS guards several places with a consent of their own (Desktop,
//! Documents, Downloads, iCloud Drive, other cloud providers, the Photos library, other apps' data),
//! so a scan of the home folder or of the disk raises one prompt after another. Full Disk Access
//! grants them all at once.
//!
//! [`full_disk_access`] tells whether this process has it, by reading a file only Full Disk Access
//! may read (the user's TCC database: refused without a prompt). [`protected_folders`] lists the
//! places to leave out when the user prefers not to grant it. Both answer `None` / nothing off
//! macOS; the `*_in` forms do the work on any platform (tests).

use std::io;
use std::path::{Path, PathBuf};

/// System Settings › Privacy & Security › Full Disk Access (macOS 13+; older versions open the pane)
pub const FULL_DISK_ACCESS_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// Home-relative places behind a consent prompt of their own (macOS 14 and 15).
pub const PROMPTING: &[&str] = &[
    "Desktop",
    "Documents",
    "Downloads",
    "Library/Mobile Documents", // iCloud Drive
    "Library/CloudStorage",     // File Providers: OneDrive, Google Drive, Dropbox…
    "Pictures/Photos Library.photoslibrary",
    "Library/Containers", // other apps' data (macOS 14+)
    "Library/Group Containers",
];

/// Files only Full Disk Access may read; the first that exists decides.
const PROBES: &[&str] = &[
    "Library/Application Support/com.apple.TCC/TCC.db",
    "Library/Safari/Bookmarks.plist",
    "Library/Mail",
];

fn home(home: Option<&Path>) -> PathBuf {
    home.map_or_else(crate::paths::home_dir, Path::to_path_buf)
}

/// macOS: does this process have Full Disk Access? `None` elsewhere, or when no probe exists.
pub fn full_disk_access(home_dir: Option<&Path>) -> Option<bool> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    full_disk_access_in(&home(home_dir), &open_probe)
}

fn open_probe(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        std::fs::read_dir(path).map(drop)
    } else {
        std::fs::File::open(path).map(drop)
    }
}

/// The probe on any platform, with `open` reading a probe (a directory is listed).
pub fn full_disk_access_in(home: &Path, open: &dyn Fn(&Path) -> io::Result<()>) -> Option<bool> {
    for rel in PROBES {
        match open(&home.join(rel)) {
            Ok(()) => return Some(true),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Some(false),
            Err(_) => continue, // missing: the next probe
        }
    }
    None
}

/// macOS: this user's places that ask for consent one by one without Full Disk Access (the ones
/// that exist; looking at a folder itself asks nothing, listing it does). Empty elsewhere.
pub fn protected_folders(home_dir: Option<&Path>) -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    protected_folders_in(&home(home_dir))
}

pub fn protected_folders_in(home: &Path) -> Vec<PathBuf> {
    PROMPTING
        .iter()
        .map(|rel| home.join(rel))
        .filter(|p| std::fs::symlink_metadata(p).is_ok())
        .collect()
}

/// `path` lies strictly below `root` (POSIX spelling).
pub fn inside(path: &str, root: &str) -> bool {
    let trimmed = root.trim_end_matches('/');
    let base = if trimmed.is_empty() { "/" } else { trimmed };
    if path == base {
        return false;
    }
    if base == "/" {
        path.starts_with('/')
    } else {
        path.strip_prefix(base).is_some_and(|r| r.starts_with('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcc_db(home: &Path) {
        let db = home.join("Library/Application Support/com.apple.TCC/TCC.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(db, b"SQLite format 3\0").unwrap();
    }

    #[test]
    fn off_macos_there_is_nothing_to_tell() {
        if cfg!(target_os = "macos") {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        tcc_db(tmp.path());
        assert_eq!(full_disk_access(Some(tmp.path())), None);
        assert!(protected_folders(Some(tmp.path())).is_empty());
    }

    #[test]
    fn full_disk_access_reads_the_tcc_database() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(full_disk_access_in(tmp.path(), &open_probe), None); // no probe at all
        tcc_db(tmp.path());
        assert_eq!(full_disk_access_in(tmp.path(), &open_probe), Some(true));
        let refuse = |_: &Path| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(full_disk_access_in(tmp.path(), &refuse), Some(false));
    }

    #[test]
    fn protected_folders_are_the_existing_prompting_places() {
        let tmp = tempfile::tempdir().unwrap();
        for rel in ["Desktop", "Downloads", "Library/Mobile Documents"] {
            std::fs::create_dir_all(tmp.path().join(rel)).unwrap();
        }
        let found: Vec<_> = protected_folders_in(tmp.path())
            .iter()
            .map(|p| {
                p.strip_prefix(tmp.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(found, ["Desktop", "Downloads", "Library/Mobile Documents"]);
    }

    #[test]
    fn inside_root() {
        assert!(inside("/Users/lenny/Desktop", "/Users/lenny"));
        assert!(inside("/Users/lenny/Desktop", "/"));
        assert!(!inside("/Users/lenny", "/Users/lenny"));
        assert!(!inside("/Users/lennyx/Desktop", "/Users/lenny"));
    }
}
