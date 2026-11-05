//! What 「无缝更新」 of a portable copy needs to know about the release files (was the portable part
//! of apps/electron/src/main/update.ts): which archive is one, the version in its name, and the new
//! version's program that swaps the files. The install steps are in install.rs.

use std::path::Path;

/// The app bundle in a macOS portable copy's folder (tools/build_package.py make_portable).
pub const MAC_APP: &str = "SpaceKnife.app";

/// The architectures a portable copy may move to: the backend takes the processor's build (an x64
/// copy on Windows on ARM becomes the native one; update asset_templates).
const ARCHES: [&str; 2] = ["x64", "arm64"];

/// `\d+\.\d+\.\d+(?:[-~_][0-9A-Za-z.~_-]+)?`, the versions in release file names.
fn is_version(v: &str) -> bool {
    let (core, tail) = match v.find(['-', '~', '_']) {
        Some(i) => (&v[..i], Some(&v[i + 1..])),
        None => (v, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    let core_ok = parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    let tail_ok = tail.is_none_or(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric() || b"._~-".contains(&b)));
    core_ok && tail_ok
}

/// The portable archive names of a platform (`std::env::consts::OS`): zip on Windows and macOS,
/// tar.gz on Linux; x64 or arm64.
fn suffixes(platform: &str) -> Vec<String> {
    let (os, ext) = match platform {
        "windows" => ("win", "zip"),
        "macos" => ("mac", "zip"),
        "linux" => ("linux", "tar.gz"),
        _ => return Vec::new(),
    };
    ARCHES.iter().map(|arch| format!("-{os}-{arch}-portable.{ext}")).collect()
}

/// The version in a portable archive's name for `platform`, or None.
pub fn portable_version_for(name: &str, platform: &str) -> Option<String> {
    let rest = name.strip_prefix("SpaceKnife-")?;
    suffixes(platform).iter().find_map(|s| rest.strip_suffix(s.as_str())).filter(|v| is_version(v)).map(str::to_owned)
}

/// The version in a portable archive's name (Windows, Linux or macOS; x64 or arm64), or None.
pub fn portable_version(name: &str) -> Option<String> {
    ["windows", "linux", "macos"].iter().find_map(|p| portable_version_for(name, p))
}

/// The new version's own executable in its unpacked files: started with `--apply-update <plan>`
/// it swaps the files (main.rs). Inside the app bundle on macOS. Joined with the platform's separator.
pub fn swap_helper(staged: &str, platform: &str, exe_name: &str) -> String {
    let (sep, parts): (&str, &[&str]) = match platform {
        "macos" => ("/", &[MAC_APP, "Contents", "MacOS", exe_name]),
        "windows" => ("\\", &[exe_name]),
        _ => ("/", &[exe_name]),
    };
    let mut out = staged.trim_end_matches(sep).to_owned();
    for p in parts {
        out.push_str(sep);
        out.push_str(p);
    }
    out
}

/// The program a portable copy's folder must hold in the new version: the app bundle on macOS, the
/// executable elsewhere.
pub fn portable_program(exe: &Path, platform: &str) -> Option<String> {
    if platform == "macos" {
        Some(MAC_APP.to_owned())
    } else {
        exe.file_name().map(|n| n.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_version_of_either_platforms_archive_x64_or_arm64_and_nothing_else() {
        assert_eq!(portable_version("SpaceKnife-0.4.0-win-x64-portable.zip").as_deref(), Some("0.4.0"));
        assert_eq!(portable_version("SpaceKnife-0.4.0-win-arm64-portable.zip").as_deref(), Some("0.4.0"));
        assert_eq!(portable_version("SpaceKnife-0.4.10-beta.1-linux-arm64-portable.tar.gz").as_deref(), Some("0.4.10-beta.1"));
        assert_eq!(portable_version("SpaceKnife-0.4.0-mac-arm64-portable.zip").as_deref(), Some("0.4.0"));
        assert_eq!(portable_version("SpaceKnife-0.4.0-mac-x64-portable.tar.gz"), None);
        assert_eq!(portable_version("SpaceKnife-0.4.0-linux-x64-portable.zip"), None);
        assert_eq!(portable_version("SpaceKnife-0.4.0-Setup-x64.exe"), None);
        assert_eq!(portable_version("SpaceKnife-../x-win-x64-portable.zip"), None);
    }

    #[test]
    fn takes_only_the_platforms_own_archives() {
        assert_eq!(portable_version_for("SpaceKnife-0.4.0-win-arm64-portable.zip", "windows").as_deref(), Some("0.4.0"));
        assert_eq!(portable_version_for("SpaceKnife-0.4.0-linux-arm64-portable.tar.gz", "linux").as_deref(), Some("0.4.0"));
        assert_eq!(portable_version_for("SpaceKnife-0.4.0-mac-x64-portable.zip", "macos").as_deref(), Some("0.4.0"));
        // the other platform's archive, and a build that does not exist
        assert_eq!(portable_version_for("SpaceKnife-0.4.0-win-x64-portable.zip", "linux"), None);
        assert_eq!(portable_version_for("SpaceKnife-0.4.0-linux-riscv64-portable.tar.gz", "linux"), None);
    }

    #[test]
    fn the_swap_is_done_by_the_new_versions_own_program() {
        assert_eq!(
            swap_helper(r"C:\Users\lenny\sk\staging-0.4.0\SpaceKnife-0.4.0-win-x64-portable", "windows", "spaceknife.exe"),
            r"C:\Users\lenny\sk\staging-0.4.0\SpaceKnife-0.4.0-win-x64-portable\spaceknife.exe"
        );
        assert_eq!(
            swap_helper("/Users/lenny/sk/staging-0.4.0/SpaceKnife-0.4.0-mac-arm64-portable", "macos", "spaceknife"),
            "/Users/lenny/sk/staging-0.4.0/SpaceKnife-0.4.0-mac-arm64-portable/SpaceKnife.app/Contents/MacOS/spaceknife"
        );
        assert_eq!(
            swap_helper("/home/lenny/sk/staging-0.4.0/SpaceKnife-0.4.0-linux-x64-portable/", "linux", "spaceknife"),
            "/home/lenny/sk/staging-0.4.0/SpaceKnife-0.4.0-linux-x64-portable/spaceknife"
        );
    }

    #[test]
    fn the_new_version_holds_the_bundle_on_macos_and_the_executable_elsewhere() {
        assert_eq!(portable_program(Path::new("/x/SpaceKnife.app/Contents/MacOS/spaceknife"), "macos").as_deref(), Some(MAC_APP));
        assert_eq!(portable_program(Path::new("spaceknife.exe"), "windows").as_deref(), Some("spaceknife.exe"));
    }
}
