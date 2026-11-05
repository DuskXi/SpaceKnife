//! Portable copies (the Windows zip and the Linux tar.gz; docs/packaging.md): a "data" folder next
//! to the executable holds everything the app writes, as if SPACEKNIFE_HOME pointed at it.
//! (Was apps/electron/src/main/portable.ts.)

use std::path::{Path, PathBuf};

/// The portable data folder, or None. Packaged Windows and Linux apps only (a macOS app bundle is
/// portable already; an AppImage runs from a read-only mount), and an explicit SPACEKNIFE_HOME wins.
/// `platform` is `std::env::consts::OS`.
pub fn portable_home(exe: &Path, platform: &str, packaged: bool, explicit_home: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    if !packaged || platform == "macos" || explicit_home.is_some_and(|h| !h.is_empty()) {
        return None;
    }
    let dir = exe.parent()?.join("data");
    dir.is_dir().then_some(dir)
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

    #[test]
    fn uses_a_data_folder_next_to_the_executable() {
        let dir = app_dir("folder");
        let data = dir.path().join("data");
        assert_eq!(portable_home(&dir.path().join("SpaceKnife.exe"), "windows", true, None), Some(data.clone()));
        assert_eq!(portable_home(&dir.path().join("spaceknife"), "linux", true, None), Some(data));
    }

    #[test]
    fn is_off_without_the_folder_in_development_on_macos_and_with_an_explicit_home() {
        assert_eq!(portable_home(&app_dir("none").path().join("SpaceKnife.exe"), "windows", true, None), None);
        assert_eq!(portable_home(&app_dir("file").path().join("SpaceKnife.exe"), "windows", true, None), None);
        let dir = app_dir("folder");
        assert_eq!(portable_home(&dir.path().join("spaceknife.exe"), "windows", false, None), None);
        assert_eq!(portable_home(&dir.path().join("SpaceKnife"), "macos", true, None), None);
        assert_eq!(portable_home(&dir.path().join("SpaceKnife.exe"), "windows", true, Some(OsStr::new("D:\\elsewhere"))), None);
    }
}
