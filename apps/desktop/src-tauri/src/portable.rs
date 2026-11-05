//! Portable copies (the Windows zip, the Linux tar.gz and the macOS zip; docs/packaging.md): a
//! "data" folder next to the program holds everything the app writes, as if SPACEKNIFE_HOME pointed
//! at it. On macOS the program is the app bundle: the folder is next to SpaceKnife.app.
//! (Was apps/electron/src/main/portable.ts.)

use std::path::{Path, PathBuf};

/// The app bundle an executable belongs to (…/SpaceKnife.app/Contents/MacOS/SpaceKnife), or None.
pub fn app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let named = |p: &Path, name: &str| p.file_name().is_some_and(|n| n == name);
    let is_app = bundle.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".app"));
    (named(macos, "MacOS") && named(contents, "Contents") && is_app).then(|| bundle.to_path_buf())
}

/// Whether macOS runs this copy from somewhere else (App Translocation, "Gatekeeper Path
/// Randomization"): an app still quarantined, opened where it was unpacked, runs from a read-only
/// copy under /private/var/folders/…/AppTranslocation/, where the data folder next to it is not.
pub fn is_translocated(path: &str) -> bool {
    path.contains("/AppTranslocation/")
}

/// The original of a translocated path, from the mount table (`/sbin/mount`'s output): the copy is
/// a read-only nullfs mount of the original, a line `<original> on <mount point> (nullfs, …)`.
pub fn untranslocate(path: &str, mount_table: &str) -> Option<String> {
    let mut best: Option<(&str, &str)> = None; // (mount point, source)
    for line in mount_table.lines() {
        let Some((source, point)) = nullfs_translocation(line.trim()) else { continue };
        let inside = path == point || path.strip_prefix(point).is_some_and(|rest| rest.starts_with('/'));
        if inside && best.is_none_or(|(p, _)| point.len() > p.len()) {
            best = Some((point, source));
        }
    }
    best.map(|(point, source)| format!("{source}{}", &path[point.len()..]))
}

/// One mount table line `<source> on <point> (nullfs…)` whose point is under AppTranslocation:
/// (source, point). The source is the shortest prefix before " on /…/AppTranslocation/…" (a name
/// with " on " in it stays whole as far as the mount point allows).
fn nullfs_translocation(line: &str) -> Option<(&str, &str)> {
    let mut from = 0;
    while let Some(i) = line[from..].find(" on /") {
        let at = from + i;
        let source = &line[..at];
        let rest = &line[at + 4..];
        // the mount point runs to the first " (nullfs," or " (nullfs)" after an AppTranslocation part
        if let Some(t) = rest.find("/AppTranslocation/") {
            for (j, _) in rest.match_indices(" (nullfs") {
                if j <= t {
                    continue;
                }
                let after = &rest[j + " (nullfs".len()..];
                if (after.starts_with(',') || after.starts_with(')')) && !source.is_empty() {
                    return Some((source, &rest[..j]));
                }
            }
        }
        from = at + 1;
    }
    None
}

/// The portable data folder, or None. Packaged apps only (an AppImage runs from a read-only mount
/// and has none next to it), and an explicit SPACEKNIFE_HOME wins. A translocated macOS copy looks
/// next to its original (`original`: where the mount table says it is), which the user can write
/// to. `platform` is `std::env::consts::OS`.
pub fn portable_home(
    exe: &Path,
    platform: &str,
    packaged: bool,
    explicit_home: Option<&std::ffi::OsStr>,
    original: impl Fn(&Path) -> Option<PathBuf>,
) -> Option<PathBuf> {
    if !packaged || explicit_home.is_some_and(|h| !h.is_empty()) {
        return None;
    }
    let program = if platform == "macos" {
        let bundle = app_bundle(exe)?;
        if is_translocated(&bundle.to_string_lossy()) {
            original(&bundle)?
        } else {
            bundle
        }
    } else {
        exe.to_path_buf()
    };
    let dir = program.parent()?.join("data");
    dir.is_dir().then_some(dir)
}

/// macOS: where a translocated copy really is, from `/sbin/mount`'s table; None when unknown.
pub fn original_path(path: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("/sbin/mount").output().ok()?;
    if !out.status.success() {
        return None;
    }
    untranslocate(&path.to_string_lossy(), &String::from_utf8_lossy(&out.stdout)).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn app_dir(with_data: &str) -> tempfile::TempDir {
        let dir = tempfile::Builder::new().prefix("sk-portable-").tempdir().unwrap();
        match with_data {
            "folder" => std::fs::create_dir(dir.path().join("data")).unwrap(),
            "file" => std::fs::write(dir.path().join("data"), "").unwrap(),
            _ => {}
        }
        dir
    }

    fn nowhere(_: &Path) -> Option<PathBuf> {
        None
    }

    #[test]
    fn uses_a_data_folder_next_to_the_executable() {
        let dir = app_dir("folder");
        let data = dir.path().join("data");
        assert_eq!(portable_home(&dir.path().join("SpaceKnife.exe"), "windows", true, None, nowhere), Some(data.clone()));
        assert_eq!(portable_home(&dir.path().join("spaceknife"), "linux", true, None, nowhere), Some(data));
    }

    #[test]
    fn is_off_without_the_folder_in_development_and_with_an_explicit_home() {
        assert_eq!(portable_home(&app_dir("none").path().join("SpaceKnife.exe"), "windows", true, None, nowhere), None);
        assert_eq!(portable_home(&app_dir("file").path().join("SpaceKnife.exe"), "windows", true, None, nowhere), None);
        let dir = app_dir("folder");
        assert_eq!(portable_home(&dir.path().join("spaceknife.exe"), "windows", false, None, nowhere), None);
        assert_eq!(portable_home(&dir.path().join("SpaceKnife.exe"), "windows", true, Some(OsStr::new("D:\\elsewhere")), nowhere), None);
    }

    #[test]
    fn on_macos_uses_a_data_folder_next_to_the_app_bundle() {
        let dir = app_dir("folder");
        let exe = dir.path().join("SpaceKnife.app").join("Contents").join("MacOS").join("SpaceKnife");
        assert_eq!(portable_home(&exe, "macos", true, None, nowhere), Some(dir.path().join("data")));
        // an executable that is not in a bundle, and a bundle with no data folder beside it (a dmg install)
        assert_eq!(portable_home(&dir.path().join("SpaceKnife"), "macos", true, None, nowhere), None);
        let none = app_dir("none");
        let exe = none.path().join("SpaceKnife.app").join("Contents").join("MacOS").join("SpaceKnife");
        assert_eq!(portable_home(&exe, "macos", true, None, nowhere), None);
    }

    #[test]
    fn on_macos_finds_the_folder_next_to_the_original_of_a_translocated_copy() {
        let dir = app_dir("folder");
        let moved = "/private/var/folders/x1/abc/T/AppTranslocation/6E1F2B3C/d/SpaceKnife.app";
        let bundle = dir.path().join("SpaceKnife.app");
        let original = |p: &Path| (p == Path::new(moved)).then(|| bundle.clone());
        let exe = PathBuf::from(format!("{moved}/Contents/MacOS/SpaceKnife"));
        assert_eq!(portable_home(&exe, "macos", true, None, original), Some(dir.path().join("data")));
        // where the original can't be found it runs as an installed app does
        assert_eq!(portable_home(&exe, "macos", true, None, nowhere), None);
    }

    #[test]
    fn finds_the_bundle_of_the_executable_only_inside_contents_macos() {
        assert_eq!(
            app_bundle(Path::new("/Users/lenny/Apps/SpaceKnife.app/Contents/MacOS/SpaceKnife")),
            Some(PathBuf::from("/Users/lenny/Apps/SpaceKnife.app"))
        );
        assert_eq!(app_bundle(Path::new("/Users/lenny/Apps/SpaceKnife/MacOS/SpaceKnife")), None);
        assert_eq!(app_bundle(Path::new("/Users/lenny/Apps/spaceknife")), None);
    }

    #[test]
    fn maps_a_translocated_path_back_through_the_mount_table() {
        let table = [
            "/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)",
            "/dev/disk3s5 on /System/Volumes/Data (apfs, local, journaled, nobrowse, protect)",
            "/Users/lenny/Downloads/SpaceKnife-0.4.0-mac-arm64-portable/SpaceKnife.app on /private/var/folders/x1/abc/T/AppTranslocation/6E1F2B3C/d/SpaceKnife.app (nullfs, local, nodev, nosuid, read-only, nobrowse, mounted by lenny)",
            "/Users/lenny/My Apps/Other.app on /private/var/folders/x1/abc/T/AppTranslocation/99AA/d/Other.app (nullfs, local, read-only)",
        ]
        .join("\n");
        let moved = "/private/var/folders/x1/abc/T/AppTranslocation/6E1F2B3C/d/SpaceKnife.app";
        let original = "/Users/lenny/Downloads/SpaceKnife-0.4.0-mac-arm64-portable/SpaceKnife.app";
        assert!(is_translocated(moved));
        assert!(!is_translocated("/Applications/SpaceKnife.app"));
        assert_eq!(untranslocate(moved, &table).as_deref(), Some(original));
        assert_eq!(untranslocate(&format!("{moved}/Contents/MacOS/SpaceKnife"), &table), Some(format!("{original}/Contents/MacOS/SpaceKnife")));
        assert_eq!(
            untranslocate("/private/var/folders/x1/abc/T/AppTranslocation/99AA/d/Other.app", &table).as_deref(),
            Some("/Users/lenny/My Apps/Other.app")
        );
        assert_eq!(untranslocate("/private/var/folders/x1/abc/T/AppTranslocation/77/d/Gone.app", &table), None);
        // a path that only starts like a mount point is not inside it
        assert_eq!(untranslocate(&format!("{moved}x"), &table), None);
    }
}
