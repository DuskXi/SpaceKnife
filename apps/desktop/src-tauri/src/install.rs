//! Updates on the shell's side (was apps/electron/src/main/update.ts and index.ts installUpdate):
//! how this copy was installed — which decides the release file the backend downloads — and the
//! update card's two buttons (app/shared/install.ts says what each does per kind of install).
//!
//! - Windows installer (Tauri's NSIS bundle, `uninstall.exe` next to the program): 「无缝更新」 runs
//!   it silently and it starts the new version; the classic way opens its pages. SpaceKnife quits.
//! - Portable copy (a `data` folder next to the program): the archive is unpacked in the updates
//!   folder, a plan is written, and *the new version's* program swaps the files once this one has
//!   quit (`--apply-update <plan>`, sk_core::update::swap; main.rs). The classic way shows the file.
//! - AppImage: the new file takes the old one's place and starts.
//! - dmg / deb / rpm: the file opens in the system's installer and SpaceKnife quits.

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::backend::{self, Backend};
use crate::text::{TextKey, mt};
use crate::update::{MAC_APP, portable_version_for, swap_helper};

/// The uninstaller Tauri's NSIS installer puts next to the executable.
pub const NSIS_UNINSTALLER: &str = "uninstall.exe";
/// dpkg's file list of the installed deb (Tauri names the package after productName, in kebab case).
const DPKG_LISTS: [&str; 2] = [
    "/var/lib/dpkg/info/space-knife.list",
    "/var/lib/dpkg/info/spaceknife.list",
];
/// How long the backend has to stop before an update replaces its files.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the new version's program has to say the plan is usable.
const SWAP_ANSWER: Duration = Duration::from_secs(30);

/// The kinds sk_core::update::InstallKind knows.
const KINDS: [&str; 8] = [
    "nsis", "portable", "appimage", "deb", "rpm", "dmg", "dev", "unknown",
];

/// The kind of install. `portable`: a portable copy with its data folder (lib.rs
/// prepare_environment); `bundle`: what Tauri's bundler patched into the executable
/// (tauri::utils::platform::bundle_type: "nsis", "deb", "rpm", "appimage", "msi"; None for a plain
/// build); else from where the executable is and what lies around it. `platform` is
/// `std::env::consts::OS`.
pub fn install_kind(
    exe: &Path,
    platform: &str,
    packaged: bool,
    portable: bool,
    bundle: Option<&str>,
    env: &dyn Fn(&str) -> Option<OsString>,
    exists: &dyn Fn(&Path) -> bool,
) -> &'static str {
    if !packaged {
        return "dev";
    }
    if portable {
        return "portable";
    }
    match bundle {
        Some("nsis") => return "nsis",
        Some("deb") => return "deb",
        Some("rpm") => return "rpm",
        // the AppImage's own program (a program it starts inherits APPIMAGE, not the patch)
        Some("appimage") if env("APPIMAGE").is_some_and(|a| !a.is_empty()) => return "appimage",
        Some("msi") => return "unknown", // no MSI bundle, no kind for it in the protocol
        _ => {}
    }
    match platform {
        "windows" => {
            let uninstaller = exe.parent().map(|d| d.join(NSIS_UNINSTALLER));
            if uninstaller.is_some_and(|u| exists(&u)) {
                "nsis"
            } else {
                "unknown"
            }
        }
        "macos" => "dmg",
        "linux" => {
            // the packages first: programs started from an AppImage inherit its APPIMAGE and APPDIR
            if exe.starts_with("/usr") {
                return if DPKG_LISTS.iter().any(|l| exists(Path::new(l))) {
                    "deb"
                } else {
                    "rpm"
                };
            }
            let appdir = env("APPDIR").filter(|d| !d.is_empty());
            if env("APPIMAGE").is_some_and(|a| !a.is_empty())
                && appdir.is_none_or(|d| exe.starts_with(d))
            {
                return "appimage";
            }
            "unknown"
        }
        _ => "unknown",
    }
}

/// This copy's kind, once. `SPACEKNIFE_INSTALL_KIND` (tests, demos) wins when it names a kind.
pub fn this_install_kind() -> &'static str {
    static KIND: OnceLock<&'static str> = OnceLock::new();
    KIND.get_or_init(|| {
        let forced = std::env::var("SPACEKNIFE_INSTALL_KIND")
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        if let Some(k) = KINDS.iter().find(|k| **k == forced) {
            return k;
        }
        let Ok(exe) = std::env::current_exe() else {
            return "unknown";
        };
        use tauri::utils::config::BundleType;
        let bundle = tauri::utils::platform::bundle_type().and_then(|b| match b {
            BundleType::Nsis => Some("nsis"),
            BundleType::Deb => Some("deb"),
            BundleType::Rpm => Some("rpm"),
            BundleType::AppImage => Some("appimage"),
            BundleType::Msi => Some("msi"),
            _ => None,
        });
        install_kind(
            &exe,
            std::env::consts::OS,
            !tauri::is_dev(),
            crate::portable_dir().is_some(),
            bundle,
            &|k| std::env::var_os(k),
            &|p| p.exists(),
        )
    })
}

/// shared/install.ts InstallStep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Silent,
    Swap,
    Replace,
    Wizard,
    Open,
    Reveal,
}

/// shared/install.ts installStep: what a button does for a kind of install (`how`: "seamless" or
/// "classic"); None when the card doesn't offer it.
pub fn install_step(kind: &str, how: &str) -> Option<Step> {
    let seamless = how == "seamless";
    Some(match (kind, seamless) {
        ("nsis", true) => Step::Silent,
        ("nsis", false) => Step::Wizard,
        ("portable", true) => Step::Swap,
        ("appimage", true) => Step::Replace,
        ("portable" | "appimage", false) => Step::Reveal,
        ("dmg" | "deb" | "rpm", false) => Step::Open,
        _ => return None,
    })
}

/// Whether `name` is `template` (sk_core::update::asset_template) with a version for `{v}`.
fn fits_template(name: &str, template: &str) -> bool {
    let Some((head, tail)) = template.split_once("{v}") else {
        return false;
    };
    let Some(v) = name.strip_prefix(head).and_then(|r| r.strip_suffix(tail)) else {
        return false;
    };
    v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || ".+~_-".contains(c))
}

/// Whether `name` is a release file of this kind of install on `platform` (`std::env::consts::OS`).
pub fn is_release_file(name: &str, kind: &str, platform: &str, machine: &str) -> bool {
    if kind == "portable" {
        return portable_version_for(name, platform).is_some();
    }
    let kind = sk_core::update::install_kind_from(kind);
    let system = match platform {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    };
    sk_core::update::asset_template(kind, system, machine).is_some_and(|t| fits_template(name, &t))
}

/// Why `file` can't be used as the downloaded update, or None when it can: a file directly in the
/// backend's updates folder, links resolved, named like this install's release file.
pub fn install_refusal(
    file: &Path,
    kind: &str,
    updates: &Path,
    platform: &str,
    machine: &str,
) -> Option<TextKey> {
    if install_step(kind, "seamless").is_none() && install_step(kind, "classic").is_none() {
        return Some(TextKey::UpdateNoInstall);
    }
    let (Ok(real), Ok(folder)) = (file.canonicalize(), updates.canonicalize()) else {
        return Some(TextKey::UpdateMissing);
    };
    if !real.is_file() {
        return Some(TextKey::UpdateMissing);
    }
    let lower = |p: &Path| {
        let s = p.to_string_lossy().into_owned();
        if platform == "linux" {
            s
        } else {
            s.to_lowercase()
        }
    };
    if real.parent().map(lower) != Some(lower(&folder)) {
        return Some(TextKey::UpdateNotInUpdates);
    }
    let name = real
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (!is_release_file(&name, kind, platform, machine)).then_some(TextKey::UpdateNotAnUpdate)
}

/// The Windows installer's arguments for a silent update in place (Tauri's NSIS template): `/S` no
/// pages, `/UPDATE` an update of the installed copy (its shortcuts stay), `/R` start the app
/// afterwards, `/ARGS` its arguments.
pub fn nsis_update_args() -> Vec<&'static str> {
    vec!["/S", "/UPDATE", "/R", "/ARGS", "--updated"]
}

/// The plan file for the new version's `--apply-update <plan>` (sk_core::update::swap::Plan,
/// version 1, which every later version keeps reading).
pub struct SwapPlan<'a> {
    pub pid: u32,
    pub app_dir: &'a Path,
    pub staged: &'a Path,
    pub previous: &'a Path,
    pub exe: &'a Path,
    pub log: &'a Path,
}

pub fn swap_plan_json(p: &SwapPlan<'_>) -> String {
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let plan = serde_json::json!({
        "v": sk_core::update::swap::PLAN_VERSION, "pid": p.pid, "app": s(p.app_dir), "new": s(p.staged),
        "old": s(p.previous), "exe": s(p.exe), "log": s(p.log), "keep": ["data"],
    });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&plan).unwrap_or_default()
    )
}

// ------------------------------------------------------------------ the steps

/// One of the update card's two buttons with the file the backend downloaded: "" when under way,
/// else why not (and the app goes on as it was). Blocking: run off the async runtime.
pub fn install_update(app: &AppHandle, file: &Path, how: &str) -> String {
    let kind = this_install_kind();
    let updates = app.state::<Backend>().updates_dir();
    let refusal = match &updates {
        Some(u) => install_refusal(file, kind, u, std::env::consts::OS, std::env::consts::ARCH),
        None => Some(TextKey::UpdateMissing),
    };
    if let Some(key) = refusal {
        tracing::warn!("[update] not using {}: {key:?}", file.display());
        return mt(key, &[]);
    }
    let step = install_step(kind, how);
    tracing::info!("[update] {how} ({step:?}) with {}", file.display());
    let failed =
        |e: &dyn std::fmt::Display| mt(TextKey::UpdateStartFailed, &[("error", &e.to_string())]);
    match step {
        Some(Step::Silent) => restart_into(app, || launch(file, &nsis_update_args())),
        // the installer with its pages upgrades in place; SpaceKnife is gone before it gets far
        Some(Step::Wizard) => restart_into(app, || launch(file, &[])),
        Some(Step::Swap) => {
            replace_portable(app, file, updates.as_deref().unwrap_or(Path::new("")))
        }
        Some(Step::Replace) => replace_appimage(app, file),
        Some(Step::Open) => {
            // a disk image mounts, a package opens in the system's installer: either replaces this
            // app's files, so it quits once the file is open
            if let Err(e) = app.opener().open_path(file.to_string_lossy(), None::<&str>) {
                return failed(&e);
            }
            let app = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                app.exit(0);
            });
            String::new()
        }
        Some(Step::Reveal) => {
            // unpacked or put in place by hand: SpaceKnife keeps running
            if let Err(e) = tauri_plugin_opener::reveal_item_in_dir(file) {
                return failed(&e);
            }
            String::new()
        }
        None => mt(TextKey::UpdateNoInstall, &[]),
    }
}

/// Stop the backend (its files must be free before they are replaced), start what puts the new
/// version in place, and exit. When that can't start, the backend comes back and the app goes on.
fn restart_into(app: &AppHandle, start: impl FnOnce() -> std::io::Result<()>) -> String {
    backend::stop(app, STOP_TIMEOUT);
    if let Err(e) = start() {
        tracing::warn!("[update] could not start the update: {e}");
        backend::start(app);
        return mt(TextKey::UpdateStartFailed, &[("error", &e.to_string())]);
    }
    app.exit(0);
    String::new()
}

/// Start a program on its own: it outlives the app.
fn launch(file: &Path, args: &[&str]) -> std::io::Result<()> {
    sk_core::update::swap::launch(file, args)
}

/// An AppImage: the new file takes the old one's place (a rename: the running app keeps the old
/// file open), then the new one starts. Needs a folder the user can write to.
fn replace_appimage(app: &AppHandle, file: &Path) -> String {
    let Some(target) = std::env::var_os("APPIMAGE")
        .filter(|t| !t.is_empty())
        .map(PathBuf::from)
    else {
        return mt(TextKey::UpdateNoInstall, &[]);
    };
    let mut next = target.clone().into_os_string();
    next.push(".new");
    let next = PathBuf::from(next);
    let replaced = (|| {
        std::fs::copy(file, &next)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&next, &target)
    })();
    if let Err(e) = replaced {
        let _ = std::fs::remove_file(&next);
        return mt(TextKey::UpdateReplaceFailed, &[("error", &e.to_string())]);
    }
    restart_into(app, || launch(&target, &["--updated"]))
}

/// A portable copy: unpack the new version in the updates folder, then the new version's own
/// program swaps the files once the app has quit and starts it again. The "data" folder stays.
fn replace_portable(app: &AppHandle, file: &Path, updates: &Path) -> String {
    let platform = std::env::consts::OS;
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (Some(version), Some(data)) =
        (portable_version_for(&name, platform), crate::portable_dir())
    else {
        return mt(TextKey::UpdateNoInstall, &[]);
    };
    let Ok(exe) = std::env::current_exe() else {
        return mt(TextKey::UpdateNoInstall, &[]);
    };
    let exe_name = exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let unpack_failed = |e: &str| mt(TextKey::UpdateUnpackFailed, &[("error", e)]);
    let staging = updates.join(format!("staging-{version}"));
    let unpacked = (|| {
        if staging.exists() {
            std::fs::remove_dir_all(&staging)?;
        }
        std::fs::create_dir_all(&staging)?;
        unpack(file, &staging)
    })();
    if let Err(e) = unpacked {
        tracing::warn!("[update] could not unpack {}: {e}", file.display());
        return unpack_failed(&e.to_string());
    }
    // one folder at the top, holding the new version's program (on macOS the app bundle)
    let top: Vec<PathBuf> = std::fs::read_dir(&staging)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    let program = if platform == "macos" {
        MAC_APP.to_owned()
    } else {
        exe_name.clone()
    };
    let staged = match top.as_slice() {
        [one] if one.join(&program).exists() => one.clone(),
        _ => {
            tracing::warn!("[update] {name} is not one folder with {program}");
            return unpack_failed(&name);
        }
    };
    let helper = PathBuf::from(swap_helper(&staged.to_string_lossy(), platform, &exe_name));
    if !helper.is_file() {
        tracing::warn!("[update] {name} has no {}", helper.display());
        return unpack_failed(&name);
    }
    // the copy's folder holds its data folder; on macOS the executable is in the bundle there, which a
    // translocated copy runs from somewhere else: the swap starts the one in the folder
    let Some(app_dir) = data.parent() else {
        return mt(TextKey::UpdateNoInstall, &[]);
    };
    let exe_in_folder = match (platform, crate::portable::app_bundle(&exe)) {
        ("macos", Some(bundle)) => app_dir
            .join(bundle.file_name().unwrap_or_default())
            .join("Contents")
            .join("MacOS")
            .join(&exe_name),
        _ => exe.clone(),
    };
    let previous = updates.join(format!("previous-{}", app.package_info().version));
    let _ = std::fs::remove_dir_all(&previous); // left by an attempt that was undone
    let log = updates.join(format!("update-{version}.log"));
    let plan_file = updates.join(format!("update-{version}.json"));
    let plan = SwapPlan {
        pid: std::process::id(),
        app_dir,
        staged: &staged,
        previous: &previous,
        exe: &exe_in_folder,
        log: &log,
    };
    if let Err(e) = std::fs::write(&plan_file, swap_plan_json(&plan)) {
        return unpack_failed(&e.to_string());
    }
    tracing::info!(
        "[update] {version} unpacked: its program swaps the files once the app has quit ({})",
        plan_file.display()
    );
    restart_into(app, || start_swap(&helper, &plan_file, updates))
}

/// Start the new version's program in its `--apply-update` mode, detached. Ok once it says the plan
/// is usable; an error when it says otherwise, exits or doesn't answer in time.
fn start_swap(helper: &Path, plan: &Path, cwd: &Path) -> std::io::Result<()> {
    let mut cmd = Command::new(helper);
    cmd.arg("--apply-update")
        .arg(plan)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc_setsid();
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("no stdout"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let name = helper
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let line = match rx.recv_timeout(SWAP_ANSWER) {
        Ok(line) => line,
        Err(_) => {
            let _ = child.kill();
            return Err(std::io::Error::other(format!("{name} did not answer")));
        }
    };
    let said: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
    if said.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(()); // it goes on by itself; its stdout is let go of
    }
    let _ = child.kill();
    let why = said
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Err(std::io::Error::other(why.unwrap_or_else(|| {
        if line.trim().is_empty() {
            format!("{name} exited")
        } else {
            line.trim().to_owned()
        }
    })))
}

#[cfg(unix)]
fn libc_setsid() {
    unsafe extern "C" {
        fn setsid() -> i32;
    }
    // SAFETY: no preconditions.
    unsafe {
        setsid();
    }
}

/// Unpack a portable archive (zip on Windows and macOS, tar.gz on Linux) with the system's tools:
/// Windows 10 1803 and later have bsdtar, which reads zip and refuses entries leaving the folder
/// (PowerShell's Expand-Archive where it is missing); macOS's ditto keeps an app bundle's links and
/// attributes as the archive's own ditto made them.
fn unpack(file: &Path, into: &Path) -> std::io::Result<()> {
    let run = |cmd: &str, args: &[&std::ffi::OsStr]| -> std::io::Result<()> {
        let mut c = Command::new(cmd);
        c.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let status = c.status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!("{cmd} exited with {status}")))
        }
    };
    let (f, d) = (file.as_os_str(), into.as_os_str());
    if cfg!(target_os = "macos") {
        return run("/usr/bin/ditto", &["-x".as_ref(), "-k".as_ref(), f, d]);
    }
    if !cfg!(windows) {
        return run("tar", &["-xzf".as_ref(), f, "-C".as_ref(), d]);
    }
    // Windows' own bsdtar (System32), not another tar earlier on the PATH
    let root =
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let tar = root.join("System32").join("tar.exe");
    run(
        &tar.to_string_lossy(),
        &["-xf".as_ref(), f, "-C".as_ref(), d],
    )
    .or_else(|_| {
        let q = |p: &Path| p.to_string_lossy().replace('\'', "''");
        let script = format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            q(file),
            q(into)
        );
        run(
            "powershell.exe",
            &[
                "-NoProfile".as_ref(),
                "-NonInteractive".as_ref(),
                "-Command".as_ref(),
                script.as_ref(),
            ],
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(exe: &str, platform: &str, env: &[(&str, &str)], files: &[&str]) -> &'static str {
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
        install_kind(
            Path::new(exe),
            platform,
            true,
            false,
            None,
            &|k| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.into()),
            &|p| files.iter().any(|f| f == p),
        )
    }

    #[test]
    fn tells_the_kind_of_install() {
        let none = |_: &str| None;
        let no = |_: &Path| false;
        assert_eq!(
            install_kind(
                Path::new("C:\\x\\spaceknife.exe"),
                "windows",
                false,
                false,
                None,
                &none,
                &no
            ),
            "dev"
        );
        assert_eq!(
            install_kind(
                Path::new("C:\\x\\spaceknife.exe"),
                "windows",
                true,
                true,
                Some("nsis"),
                &none,
                &no
            ),
            "portable"
        );
        let nsis = Path::new("C:\\Users\\l\\AppData\\Local\\SpaceKnife").join(NSIS_UNINSTALLER);
        let exe = Path::new("C:\\Users\\l\\AppData\\Local\\SpaceKnife").join("spaceknife.exe");
        assert_eq!(
            install_kind(&exe, "windows", true, false, None, &none, &|p| p == nsis),
            "nsis"
        );
        assert_eq!(
            install_kind(&exe, "windows", true, false, None, &none, &no),
            "unknown"
        );
        // what the bundler patched into the program wins over the files around it
        assert_eq!(
            install_kind(&exe, "windows", true, false, Some("nsis"), &none, &no),
            "nsis"
        );
        assert_eq!(
            install_kind(&exe, "windows", true, false, Some("msi"), &none, &|p| p
                == nsis),
            "unknown"
        );
        let opt = Path::new("/usr/bin/spaceknife");
        assert_eq!(
            install_kind(opt, "linux", true, false, Some("rpm"), &none, &|_| true),
            "rpm"
        );
        assert_eq!(
            install_kind(
                Path::new("/tmp/.mount_x/usr/bin/spaceknife"),
                "linux",
                true,
                false,
                Some("appimage"),
                &|k| (k == "APPIMAGE").then(|| "/h/S.AppImage".into()),
                &no
            ),
            "appimage"
        );
        assert_eq!(
            kind(
                "/Applications/SpaceKnife.app/Contents/MacOS/spaceknife",
                "macos",
                &[],
                &[]
            ),
            "dmg"
        );
        assert_eq!(
            kind(
                "/usr/bin/spaceknife",
                "linux",
                &[],
                &["/var/lib/dpkg/info/space-knife.list"]
            ),
            "deb"
        );
        assert_eq!(
            kind(
                "/usr/bin/spaceknife",
                "linux",
                &[("APPIMAGE", "/x.AppImage")],
                &[]
            ),
            "rpm"
        );
        assert_eq!(
            kind(
                "/tmp/.mount_x/usr/bin/spaceknife",
                "linux",
                &[("APPIMAGE", "/h/S.AppImage"), ("APPDIR", "/tmp/.mount_x")],
                &[]
            ),
            "appimage"
        );
        // started from an AppImage, but not the AppImage's own program
        assert_eq!(
            kind(
                "/opt/other/spaceknife",
                "linux",
                &[("APPIMAGE", "/h/S.AppImage"), ("APPDIR", "/tmp/.mount_x")],
                &[]
            ),
            "unknown"
        );
    }

    #[test]
    fn says_what_each_button_does() {
        assert_eq!(install_step("nsis", "seamless"), Some(Step::Silent));
        assert_eq!(install_step("nsis", "classic"), Some(Step::Wizard));
        assert_eq!(install_step("portable", "seamless"), Some(Step::Swap));
        assert_eq!(install_step("portable", "classic"), Some(Step::Reveal));
        assert_eq!(install_step("appimage", "seamless"), Some(Step::Replace));
        assert_eq!(install_step("dmg", "classic"), Some(Step::Open));
        assert_eq!(install_step("deb", "seamless"), None);
        assert_eq!(install_step("dev", "classic"), None);
        assert_eq!(install_step("unknown", "seamless"), None);
    }

    #[test]
    fn knows_the_release_files_tauris_bundler_makes() {
        assert!(is_release_file(
            "SpaceKnife_0.4.1_x64-setup.exe",
            "nsis",
            "windows",
            "x86_64"
        ));
        assert!(is_release_file(
            "SpaceKnife_0.4.1-beta.1_x64-setup.exe",
            "nsis",
            "windows",
            "aarch64"
        ));
        assert!(!is_release_file(
            "SpaceKnife_0.4.1_x64-setup.exe.sig",
            "nsis",
            "windows",
            "x86_64"
        ));
        assert!(!is_release_file(
            "SpaceKnife_../x_x64-setup.exe",
            "nsis",
            "windows",
            "x86_64"
        ));
        assert!(is_release_file(
            "SpaceKnife-0.4.1-win-x64-portable.zip",
            "portable",
            "windows",
            "x86_64"
        ));
        assert!(!is_release_file(
            "SpaceKnife_0.4.1_x64-setup.exe",
            "portable",
            "windows",
            "x86_64"
        ));
        assert!(is_release_file(
            "SpaceKnife_0.4.1_amd64.AppImage",
            "appimage",
            "linux",
            "x86_64"
        ));
        assert!(is_release_file(
            "SpaceKnife_0.4.1_amd64.deb",
            "deb",
            "linux",
            "x86_64"
        ));
        assert!(is_release_file(
            "SpaceKnife-0.4.1-1.x86_64.rpm",
            "rpm",
            "linux",
            "x86_64"
        ));
        assert!(is_release_file(
            "SpaceKnife_0.4.1_aarch64.dmg",
            "dmg",
            "macos",
            "aarch64"
        ));
        assert!(!is_release_file(
            "SpaceKnife_0.4.1_x64.dmg",
            "dmg",
            "macos",
            "aarch64"
        ));
        assert!(!is_release_file(
            "SpaceKnife_0.4.1_x64-setup.exe",
            "dev",
            "windows",
            "x86_64"
        ));
    }

    #[test]
    fn uses_only_a_release_file_in_the_updates_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let updates = tmp.path().join("updates");
        std::fs::create_dir(&updates).unwrap();
        let good = updates.join("SpaceKnife_0.4.1_x64-setup.exe");
        std::fs::write(&good, "x").unwrap();
        let other = updates.join("evil.exe");
        std::fs::write(&other, "x").unwrap();
        let outside = tmp.path().join("SpaceKnife_0.4.1_x64-setup.exe");
        std::fs::write(&outside, "x").unwrap();
        let os = "windows";
        assert_eq!(install_refusal(&good, "nsis", &updates, os, "x86_64"), None);
        assert_eq!(
            install_refusal(&other, "nsis", &updates, os, "x86_64"),
            Some(TextKey::UpdateNotAnUpdate)
        );
        assert_eq!(
            install_refusal(&outside, "nsis", &updates, os, "x86_64"),
            Some(TextKey::UpdateNotInUpdates)
        );
        assert_eq!(
            install_refusal(&updates.join("gone.exe"), "nsis", &updates, os, "x86_64"),
            Some(TextKey::UpdateMissing)
        );
        assert_eq!(
            install_refusal(&good, "dev", &updates, os, "x86_64"),
            Some(TextKey::UpdateNoInstall)
        );
    }

    #[test]
    fn writes_a_plan_the_swap_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("SpaceKnife-0.4.0-win-x64-portable");
        let updates = app.join("data").join("data").join("updates");
        let staged = updates
            .join("staging-0.4.1")
            .join("SpaceKnife-0.4.1-win-x64-portable");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(app.join("spaceknife.exe"), "old").unwrap();
        std::fs::write(staged.join("spaceknife.exe"), "new").unwrap();
        let (exe, previous, log) = (
            app.join("spaceknife.exe"),
            updates.join("previous-0.4.0"),
            updates.join("update-0.4.1.log"),
        );
        let plan = SwapPlan {
            pid: 42,
            app_dir: &app,
            staged: &staged,
            previous: &previous,
            exe: &exe,
            log: &log,
        };
        let file = updates.join("update-0.4.1.json");
        std::fs::write(&file, swap_plan_json(&plan)).unwrap();
        let read = sk_core::update::swap::Plan::read(&file).unwrap();
        assert_eq!(
            (read.pid, read.app, read.new, read.exe, read.keep),
            (42, app.clone(), staged, exe, vec!["data".to_owned()])
        );
    }
}
