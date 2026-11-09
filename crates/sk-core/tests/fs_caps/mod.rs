//! What the file system holding a test's temporary folder can do, found out at run time.
//!
//! CI's file-system matrix (`.github/workflows/ci.yml`, job `fs-matrix`; docs/manage.md §2.5)
//! runs the ops tests with the temporary folder on FAT32, exFAT, NTFS, ReFS, ext4, XFS, Btrfs,
//! NTFS (ntfs3), APFS, HFS+ … A test that needs what the file system there lacks (links,
//! case-sensitive names, names that are not Unicode, folder permissions, a trash …) skips that
//! part and prints why (`skipped: …`); where the file system has it, the part runs. Each probe
//! tries the thing itself in a folder of its own, so a feature that is there is never taken for
//! missing.

#![allow(dead_code)] // each test binary uses some of these

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The file system holding `dir` (`NTFS`, `exFAT`, `ext4`, `apfs` …), for the notes.
pub fn fs_name(dir: &Path) -> String {
    sk_core::volume::volume_for(dir)
        .map(|v| v.fs_type)
        .ok()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "?".into())
}

/// Say that `what` is skipped because the file system lacks something (`why`).
pub fn skip(what: &str, dir: &Path, why: &str) {
    eprintln!(
        "skipped: {what}: {why} ({} at {})",
        fs_name(dir),
        dir.display()
    );
}

/// A fresh folder for one probe, below `dir`.
fn probe_dir(dir: &Path) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = dir.join(format!(
        ".cap-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn remove(p: &Path) {
    let _ = std::fs::remove_dir_all(verbatim(p));
}

/// `p` with the `\\?\` prefix (`\\?\UNC\` for a network path) on Windows: no Win32 rewriting of
/// trailing dots and spaces, no MAX_PATH. As it is elsewhere.
pub fn verbatim(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if !cfg!(windows) || s.starts_with(r"\\?\") {
        s
    } else if let Some(unc) = s.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{unc}")
    } else {
        format!(r"\\?\{s}")
    }
}

/// A link at `link` to the folder `target` (a junction on Windows: no privilege needed; a symlink
/// elsewhere); false where the file system cannot hold one.
pub fn dir_link(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success())
            && std::fs::symlink_metadata(link).is_ok()
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).is_ok()
}

/// Whether folders in `dir` can be linked to ([`dir_link`]).
pub fn links(dir: &Path) -> bool {
    let p = probe_dir(dir);
    std::fs::create_dir(p.join("target")).unwrap();
    let ok = dir_link(&p.join("target"), &p.join("link"));
    if ok {
        if cfg!(windows) {
            let _ = std::fs::remove_dir(p.join("link"));
        } else {
            let _ = std::fs::remove_file(p.join("link"));
        }
    }
    remove(&p);
    ok
}

/// Whether a folder's permissions keep this process from deleting in it (POSIX, not root): a
/// folder made read-only refuses a new file. False on Windows (it has no such thing) and where
/// the file system keeps no permissions (FAT, exFAT, NTFS without a user mapping).
pub fn folder_permissions(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: a plain query.
        if unsafe { libc::geteuid() } == 0 {
            return false; // root deletes in a read-only folder
        }
        let p = probe_dir(dir);
        let f = p.join("f");
        std::fs::create_dir(&f).unwrap();
        let set = std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o555)).is_ok();
        let kept = set && std::fs::write(f.join("new"), b"n").is_err();
        let _ = std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755));
        remove(&p);
        kept
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        false
    }
}

/// Whether a file in `dir` can be marked read-only (Windows' attribute; POSIX: no write bits)
/// and is then refused a write.
pub fn read_only_files(dir: &Path) -> bool {
    let p = probe_dir(dir);
    let f = p.join("ro.txt");
    std::fs::write(&f, b"r").unwrap();
    let mut perm = std::fs::metadata(&f).unwrap().permissions();
    perm.set_readonly(true);
    let set = std::fs::set_permissions(&f, perm).is_ok();
    // SAFETY (unix): a plain query.
    #[cfg(unix)]
    let root = unsafe { libc::geteuid() } == 0;
    #[cfg(not(unix))]
    let root = false;
    let held = set
        && std::fs::metadata(&f).is_ok_and(|m| m.permissions().readonly())
        && (root || std::fs::OpenOptions::new().write(true).open(&f).is_err());
    let mut perm = std::fs::metadata(&f).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    let _ = std::fs::set_permissions(&f, perm);
    remove(&p);
    held
}

/// Makes `folder` (made if needed) hold names that differ only in case, or says it cannot. On
/// Windows it becomes a case-sensitive NTFS folder (fsutil's exit code says nothing, so the folder
/// is tried); elsewhere the file system decides (APFS, HFS+, FAT, exFAT fold case).
pub fn case_twins(folder: &Path) -> bool {
    std::fs::create_dir_all(folder).unwrap();
    if cfg!(windows) {
        let _ = std::process::Command::new("fsutil")
            .args(["file", "setCaseSensitiveInfo"])
            .arg(folder)
            .arg("enable")
            .output();
    }
    std::fs::create_dir(folder.join("probe")).unwrap();
    let twin = std::fs::create_dir(folder.join("PROBE")).is_ok();
    std::fs::remove_dir(folder.join("probe")).unwrap();
    if twin {
        std::fs::remove_dir(folder.join("PROBE")).unwrap();
    }
    twin
}

/// A name no `String` can hold, with `stem` and `ext` around it: a lone UTF-16 surrogate on
/// Windows, a byte that is not UTF-8 elsewhere.
pub fn odd_name(stem: &str, ext: &str) -> std::ffi::OsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let mut w: Vec<u16> = stem.encode_utf16().collect();
        w.push(0xDC00);
        w.extend(ext.encode_utf16());
        std::ffi::OsString::from_wide(&w)
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut b = stem.as_bytes().to_vec();
        b.push(0xFF);
        b.extend_from_slice(ext.as_bytes());
        std::ffi::OsString::from_vec(b)
    }
}

/// Whether `dir` takes names that are not valid Unicode ([`odd_name`]; NTFS and ext4 do, FAT,
/// exFAT, Btrfs through WinBtrfs, APFS and anything mounted with `utf8` do not).
pub fn odd_names(dir: &Path) -> bool {
    let p = probe_dir(dir);
    let ok = std::fs::write(p.join(odd_name("x", ".bin")), b"o").is_ok()
        && std::fs::read_dir(&p).is_ok_and(|d| d.count() == 1);
    remove(&p);
    ok
}

/// Whether `dir` takes names Win32 would rewrite, ending in a dot or a space (through `\\?\`);
/// always on POSIX.
pub fn trailing_names(dir: &Path) -> bool {
    if !cfg!(windows) {
        return true;
    }
    let p = probe_dir(dir);
    let long = verbatim(&p);
    let ok = ["odd.", "space "].iter().all(|n| {
        std::fs::write(format!("{long}\\{n}"), b"t").is_ok()
            && std::fs::read_dir(&p)
                .is_ok_and(|d| d.flatten().any(|e| e.file_name().to_string_lossy() == *n))
    });
    remove(&p);
    ok
}

/// Whether `dir` takes names with a backslash in them (POSIX file systems do; FAT, exFAT and
/// NTFS do not, whatever the system).
pub fn backslash_names(dir: &Path) -> bool {
    if cfg!(windows) {
        return false;
    }
    let p = probe_dir(dir);
    let ok = std::fs::write(p.join("a\\b"), b"b").is_ok();
    remove(&p);
    ok
}

/// Why moving something in `dir` to the trash is not possible there (the reason the backend
/// gives the user), None when it is.
pub fn no_trash(dir: &Path) -> Option<String> {
    let status = sk_core::ops::trash_status(&dir.to_string_lossy(), false);
    (!status.available).then_some(status.reason)
}

/// The 8.3 short name of an existing path (cmd's `%~sI`); None where the volume has none
/// (exFAT, ReFS, NTFS with them turned off) or off Windows.
pub fn short_name(p: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // raw: cmd parses its own command line, and Rust's quoting would escape the quotes
        let out = std::process::Command::new("cmd")
            .raw_arg(format!(
                "/d /c for %I in (\"{}\") do @echo %~sI",
                p.display()
            ))
            .output()
            .ok()?;
        let short = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (short.contains('~') && Path::new(&short).exists()).then_some(short)
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        None
    }
}
