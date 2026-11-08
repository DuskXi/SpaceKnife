//! Files that hold the user's own choices (`classes.json`, `class_folders.json`): one this build
//! cannot read is never written over or removed. It may be damaged, or valid to the version that
//! wrote it (0.3.x) and not to this one; either way it is the user's, so a save first moves it
//! aside, next to where it was.

use std::io;
use std::path::{Path, PathBuf};

/// Before `path` is replaced or removed: when it exists and `readable` says no to what it holds
/// (or it cannot be read at all), it is renamed to `<stem>.unreadable-<UTC time>.<ext>` beside
/// itself; returns that name. When it cannot be moved, the error: the caller must not go on
/// and overwrite it.
pub fn keep_if_unreadable(
    path: &Path,
    readable: impl Fn(&str) -> bool,
) -> io::Result<Option<PathBuf>> {
    match std::fs::read(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Ok(bytes) if std::str::from_utf8(&bytes).is_ok_and(&readable) => return Ok(None),
        _ => {}
    }
    let aside = aside_name(path);
    std::fs::rename(path, &aside)?;
    tracing::warn!(
        "{} could not be read: kept as {} (a new one replaces it)",
        path.display(),
        aside.display()
    );
    Ok(Some(aside))
}

/// `classes.json` → `classes.unreadable-20261108-031500.json` (`-2`, `-3` … if taken).
fn aside_name(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map_or_else(|| "file".into(), |s| s.to_string_lossy().into_owned());
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let stamp = sk_store::snapshot::stamp(secs);
    let dir = path.parent().unwrap_or(Path::new("."));
    let first = dir.join(format!("{stem}.unreadable-{stamp}{ext}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem}.unreadable-{stamp}-{n}{ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_files_move_aside_readable_ones_stay() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.json");
        assert_eq!(keep_if_unreadable(&f, |_| true).unwrap(), None);
        std::fs::write(&f, "good").unwrap();
        assert_eq!(keep_if_unreadable(&f, |t| t == "good").unwrap(), None);
        assert!(f.exists());
        let a = keep_if_unreadable(&f, |_| false).unwrap().unwrap();
        assert!(!f.exists());
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "good");
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("x.unreadable-") && name.ends_with(".json"),
            "{name}"
        );
        std::fs::write(&f, [0xff, 0xfe]).unwrap(); // not UTF-8: unreadable too
        let b = keep_if_unreadable(&f, |_| true).unwrap().unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&b).unwrap(), [0xff, 0xfe]);
    }
}
