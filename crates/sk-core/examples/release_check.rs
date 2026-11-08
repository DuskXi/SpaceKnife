//! Read a published release the way an installed SpaceKnife reads it (the Release and Release
//! check workflows; was `tools/release.py check`):
//!
//!     cargo run -p sk-core --example release_check -- 0.3.5 [--sums DIR] [--download]
//!
//! The release must be the newest of its channel (preview; stable too unless it is a pre-release),
//! every kind of install must find its file with the app's own [`update::pick_asset`] under the
//! name [`update::asset_template`] gives, and the checksum the app would read for it
//! ([`update::fetch_checksums`]) must match `--sums` (the folder `tools/release.mjs assemble`
//! filled: its `SHA256SUMS*.txt`) or, with `--download`, the file itself (downloaded with the
//! app's [`update::download`], which hashes while it streams). No token: the public release list,
//! as the app reads it. Exit status 1 when anything is wrong.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use sk_core::update::{self, Shell};
use sk_protocol::update::{InstallKind, UpdateChannel};

const AGENT: &str = "SpaceKnife-release-check";

/// (install kind, shell, platform as the app names it, machine) for every kind of install a
/// release serves: the Windows installer, the rpm and the AppImage are the Electron shell's, the
/// rest Tauri's (update::asset_templates)
const INSTALLS: &[(InstallKind, Shell, &str, &str)] = &[
    (InstallKind::Nsis, Shell::Electron, "win32", "x86_64"),
    (InstallKind::Portable, Shell::Tauri, "win32", "x86_64"),
    (InstallKind::Portable, Shell::Tauri, "win32", "aarch64"),
    (InstallKind::Portable, Shell::Tauri, "linux", "x86_64"),
    (InstallKind::Portable, Shell::Tauri, "linux", "aarch64"),
    (InstallKind::Portable, Shell::Tauri, "darwin", "aarch64"),
    (InstallKind::Portable, Shell::Tauri, "darwin", "x86_64"),
    (InstallKind::Appimage, Shell::Electron, "linux", "x86_64"),
    (InstallKind::Deb, Shell::Tauri, "linux", "x86_64"),
    (InstallKind::Rpm, Shell::Electron, "linux", "x86_64"),
    (InstallKind::Dmg, Shell::Tauri, "darwin", "aarch64"),
    (InstallKind::Dmg, Shell::Tauri, "darwin", "x86_64"),
];

fn main() {
    let mut version = None;
    let mut sums: Option<PathBuf> = None;
    let mut download = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--sums" => sums = args.next().map(PathBuf::from),
            "--download" => download = true,
            v if !v.starts_with("--") && version.is_none() => {
                version = Some(v.trim_start_matches('v').to_owned())
            }
            _ => {
                eprintln!("usage: release_check VERSION [--sums DIR] [--download]");
                std::process::exit(2);
            }
        }
    }
    let Some(version) = version else {
        eprintln!("usage: release_check VERSION [--sums DIR] [--download]");
        std::process::exit(2);
    };
    let problems = check(&version, sums, download);
    for p in &problems {
        println!("PROBLEM {p}");
    }
    if problems.is_empty() {
        println!("{version}: every kind of install finds its file and checksum");
    }
    std::process::exit(i32::from(!problems.is_empty()));
}

fn check(version: &str, sums_dir: Option<PathBuf>, download: bool) -> Vec<String> {
    let timeout = Duration::from_secs(60);
    let releases = match update::fetch_releases(update::RELEASES_URL, None, AGENT, timeout) {
        Ok(f) => f.releases.unwrap_or_default(),
        Err(e) => return vec![format!("the release list: {e}")],
    };
    let Some(release) = releases.iter().find(|r| r.version == version) else {
        return vec![format!(
            "{version} is not in the published release list (a draft is not)"
        )];
    };
    let mut problems = Vec::new();
    let newest = update::choose(&releases, UpdateChannel::Preview).map(|r| r.version.clone());
    if newest.as_deref() != Some(version) {
        problems.push(format!(
            "the newest release (preview channel) is {newest:?}"
        ));
    }
    if !release.preview() {
        let stable = update::choose(&releases, UpdateChannel::Stable).map(|r| r.version.clone());
        if stable.as_deref() != Some(version) {
            problems.push(format!("the newest stable release is {stable:?}"));
        }
    }
    // the assembled checksums: every SHA256SUMS*.txt in the folder
    let mut local: HashMap<String, String> = HashMap::new();
    if let Some(dir) = &sums_dir {
        match std::fs::read_dir(dir) {
            Ok(rd) => {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if update::is_checksum_file(&name) {
                        let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                        local.extend(update::parse_checksums(&text));
                    }
                }
            }
            Err(e) => problems.push(format!("--sums {}: {e}", dir.display())),
        }
    }
    let scratch = std::env::temp_dir().join(format!("sk-release-check-{}", std::process::id()));
    println!("{:<9} {:<15} {:<45} checksum", "install", "system", "file");
    for &(kind, shell, system, machine) in INSTALLS {
        let row = |file: &str, state: &str| {
            println!(
                "{:<9} {:<15} {:<45} {state}",
                kind.as_str(),
                format!("{system}/{machine}"),
                file
            )
        };
        let want =
            update::asset_template(kind, shell, system, machine).map(|t| t.replace("{v}", version));
        let asset = update::pick_asset(release, kind, shell, Some(system), Some(machine));
        let Some(asset) = asset else {
            problems.push(format!(
                "{} on {system}/{machine}: no file, want {want:?}",
                kind.as_str()
            ));
            row("-", "-");
            continue;
        };
        // a pre-release's packages may spell the version otherwise (rpm: ~); the app takes either
        if want.as_deref() != Some(asset.name.as_str()) && !release.preview() {
            problems.push(format!(
                "{} on {system}/{machine}: found {}, want {want:?}",
                kind.as_str(),
                asset.name
            ));
        }
        let state = match update::fetch_checksums(release, asset, AGENT, timeout) {
            Err(e) => {
                problems.push(format!("{}: the checksums can't be read: {e}", asset.name));
                "unreadable".to_owned()
            }
            Ok(None) => {
                problems.push(format!("{}: no published checksum", asset.name));
                "none".to_owned()
            }
            Ok(Some(published)) => {
                if !local.is_empty() && local.get(&asset.name) != Some(&published) {
                    problems.push(format!(
                        "{}: published {published}, assembled {:?}",
                        asset.name,
                        local.get(&asset.name)
                    ));
                    "differs".to_owned()
                } else if download {
                    let dest = scratch.join(&asset.name);
                    let cancel = AtomicBool::new(false);
                    let got = update::download(
                        &asset.url,
                        &dest,
                        asset.size,
                        Some(&published),
                        AGENT,
                        &mut |_, _, _| {},
                        &cancel,
                        Duration::from_secs(600),
                    );
                    let _ = std::fs::remove_file(&dest);
                    match got {
                        Ok(Some(true)) => "ok (downloaded)".to_owned(),
                        Ok(other) => {
                            problems
                                .push(format!("{}: downloaded, verified {other:?}", asset.name));
                            "differs".to_owned()
                        }
                        Err(e) => {
                            problems.push(format!("{}: download: {e}", asset.name));
                            "failed".to_owned()
                        }
                    }
                } else {
                    "ok".to_owned()
                }
            }
        };
        row(&asset.name, &state);
    }
    let _ = std::fs::remove_dir_all(&scratch);
    problems
}
