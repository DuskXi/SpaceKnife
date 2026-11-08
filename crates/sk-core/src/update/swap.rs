//! Put a portable copy's new version in place: the app binary's `--apply-update <plan>` mode.
//!
//! 「无缝更新」 on a portable copy (the Windows zip, the Linux tar.gz): the app unpacks the new
//! version into its updates folder, writes a plan ([`Plan`], JSON) and starts *the new version's*
//! executable with `--apply-update <plan>`, detached, just before it quits. That process then
//! ([`main`]):
//!
//! 1. says `{"ok": true}` on stdout once the plan is usable and the new version's program starts
//!    on this machine ([`startup_check`]: its libraries load, the web view's runtime is there), and
//!    lets go of its stdio. Otherwise it says `{"ok": false, "error": …}` and nothing is touched:
//!    the app waits for this line, shows the error (「无法启动更新」) and goes on as it was;
//! 2. waits for the app's main process to exit;
//! 3. moves the app folder's entries, except `data`, to the plan's `old` folder, trying again while
//!    files are still held (the closing app's own helpers, a virus scanner);
//! 4. moves the new version's entries into the app folder. This process runs from among them: what
//!    can't be moved at once (Windows doesn't rename a folder with a running program in it) moves
//!    piece by piece, and is copied where even that fails;
//! 5. starts the app again, with `--updated` once the new version is in place. When anything
//!    fails, the old files go back and the old version starts.
//!
//! Every step is written to the plan's log. The plan is a contract with every older version: an
//! app hands its plan to the *newer* version, so later helpers keep reading `v: 1`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

pub const PLAN_VERSION: i64 = 1;
/// The app quits right after starting this.
pub const QUIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Files of the old version still held after it quit.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// The plan can't be used: nothing was touched.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PlanError(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The app's main process, to wait for
    pub pid: u32,
    /// The portable copy's folder
    pub app: PathBuf,
    /// The unpacked new version (the folder with its executable)
    pub new: PathBuf,
    /// Where the replaced files go (the new version clears it)
    pub old: PathBuf,
    /// The app's executable, in `app`
    pub exe: PathBuf,
    pub log: PathBuf,
    /// Entries of `app` that stay where they are
    pub keep: Vec<String>,
}

impl Plan {
    pub fn read(path: &Path) -> Result<Plan, PlanError> {
        let cant = |e: String| PlanError(format!("can't read the plan {}: {e}", path.display()));
        let text = std::fs::read_to_string(path).map_err(|e| cant(e.to_string()))?;
        let raw: Value = serde_json::from_str(&text).map_err(|e| cant(e.to_string()))?;
        if raw.get("v").and_then(Value::as_i64) != Some(PLAN_VERSION) {
            return Err(PlanError(format!(
                "{} is not an update plan of version {PLAN_VERSION}",
                path.display()
            )));
        }
        let bad = |what: &str| PlanError(format!("{}: {what}", path.display()));
        let p = |key: &str| -> Result<PathBuf, PlanError> {
            match raw.get(key) {
                Some(Value::String(s)) => Ok(PathBuf::from(s)),
                Some(Value::Null) | None => Err(bad(&format!("no {key}"))),
                Some(other) => Ok(PathBuf::from(other.to_string())),
            }
        };
        let pid = match raw.get("pid") {
            Some(Value::Number(n)) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
            Some(Value::String(s)) => s.trim().parse::<u32>().ok(),
            _ => None,
        }
        .ok_or_else(|| bad("pid is not a process id"))?;
        let keep = match raw.get("keep") {
            None | Some(Value::Null) => vec!["data".to_owned()],
            Some(Value::Array(a)) => a
                .iter()
                .map(|k| k.as_str().map_or_else(|| k.to_string(), str::to_owned))
                .collect(),
            Some(_) => return Err(bad("keep is not a list")),
        };
        let plan = Plan {
            pid,
            app: p("app")?,
            new: p("new")?,
            old: p("old")?,
            exe: p("exe")?,
            log: p("log")?,
            keep,
        };
        plan.check()?;
        Ok(plan)
    }

    /// The new version must be complete before anything is moved.
    pub fn check(&self) -> Result<(), PlanError> {
        for (name, p) in [
            ("app", &self.app),
            ("new", &self.new),
            ("old", &self.old),
            ("exe", &self.exe),
            ("log", &self.log),
        ] {
            if !p.is_absolute() {
                return Err(PlanError(format!(
                    "{name} is not an absolute path: {}",
                    p.display()
                )));
            }
        }
        if !self.app.is_dir() {
            return Err(PlanError(format!("no folder {}", self.app.display())));
        }
        // the executable is in the app folder: directly, or in the app bundle there (macOS)
        let Some(inside) = within(&self.app, &self.exe) else {
            return Err(PlanError(format!(
                "{} is not in {}",
                self.exe.display(),
                self.app.display()
            )));
        };
        if !self.new.join(&inside).is_file() {
            return Err(PlanError(format!(
                "the new version is incomplete: no {} in {}",
                inside.display(),
                self.new.display()
            )));
        }
        if same(&self.new, &self.app) || same(&self.old, &self.app) {
            return Err(PlanError(
                "the new and the old folder must not be the app's folder".into(),
            ));
        }
        if self.old.exists() && std::fs::read_dir(&self.old).is_ok_and(|mut d| d.next().is_some()) {
            return Err(PlanError(format!("{} is not empty", self.old.display())));
        }
        Ok(())
    }

    /// The new version's program: the executable's place in the app folder, in the new version
    /// ([`Plan::check`] made sure it is there).
    pub fn new_program(&self) -> PathBuf {
        within(&self.app, &self.exe)
            .map_or_else(|| self.exe.clone(), |inside| self.new.join(inside))
    }
}

fn normcase(p: &Path) -> String {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_owned());
    let s = abs.to_string_lossy().into_owned();
    let s = s.trim_end_matches(['/', '\\']).to_owned();
    if cfg!(windows) {
        s.replace('/', "\\").to_lowercase()
    } else {
        s
    }
}

fn same(a: &Path, b: &Path) -> bool {
    normcase(a) == normcase(b)
}

/// `file`'s path below `folder` (compared as [`same`] does), or None when it is not below it.
fn within(folder: &Path, file: &Path) -> Option<PathBuf> {
    let mut rel = Vec::new();
    let mut at = file;
    while let Some(parent) = at.parent() {
        rel.push(at.file_name()?.to_owned());
        if same(parent, folder) {
            return Some(rel.iter().rev().collect());
        }
        at = parent;
    }
    None
}

fn name_case(name: &str) -> String {
    if cfg!(windows) {
        name.to_lowercase()
    } else {
        name.to_owned()
    }
}

/// One timestamped line per step in the plan's log file (never fails).
#[derive(Clone, Debug)]
pub struct Log {
    pub path: PathBuf,
}

impl Log {
    pub fn new(path: &Path) -> Log {
        Log {
            path: path.to_owned(),
        }
    }

    pub fn say(&self, message: &str) {
        let line = format!(
            "{} {message}\n",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
        );
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

// ---------------------------------------------------------------- waiting

/// True once process `pid` has exited (or never was), false when it still runs after `timeout`.
pub fn wait_gone(pid: u32, timeout: Duration) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };
        // SAFETY: plain Win32 calls on a handle we own and close.
        unsafe {
            let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if handle.is_null() {
                return true; // gone
            }
            let ms = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
            let done = WaitForSingleObject(handle, ms) == WAIT_OBJECT_0;
            CloseHandle(handle);
            done
        }
    }
    #[cfg(not(windows))]
    {
        let deadline = Instant::now() + timeout;
        loop {
            if !alive(pid) {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for the process.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc != 0 {
        return io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    }
    // a process that exited but was not reaped by its parent yet counts as gone
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        && let Some((_, rest)) = stat.rsplit_once(')')
    {
        return rest.split_whitespace().next() != Some("Z");
    }
    true
}

#[cfg(not(any(unix, windows)))]
fn alive(_pid: u32) -> bool {
    false
}

// ---------------------------------------------------------------- moving

/// The renaming primitive (replaced in tests).
pub type Rename<'a> = &'a dyn Fn(&Path, &Path) -> io::Result<()>;

fn fs_rename(a: &Path, b: &Path) -> io::Result<()> {
    std::fs::rename(a, b)
}

/// The folder's entries, except the kept names (case-insensitive on Windows), sorted.
pub fn entries(folder: &Path, keep: &[String]) -> io::Result<Vec<PathBuf>> {
    let kept: Vec<String> = keep.iter().map(|k| name_case(k)).collect();
    let mut out: Vec<PathBuf> = std::fs::read_dir(folder)?
        .filter_map(Result::ok)
        .filter(|e| !kept.contains(&name_case(&e.file_name().to_string_lossy())))
        .map(|e| e.path())
        .collect();
    out.sort();
    Ok(out)
}

/// Move `src`'s entries (except `keep`) into `dst`, trying again while one is held. On failure
/// what was moved goes back, and the error is returned.
pub fn move_out(
    src: &Path,
    dst: &Path,
    keep: &[String],
    log: &Log,
    timeout: Duration,
) -> io::Result<()> {
    move_out_with(src, dst, keep, log, timeout, &fs_rename)
}

pub fn move_out_with(
    src: &Path,
    dst: &Path,
    keep: &[String],
    log: &Log,
    timeout: Duration,
    rename: Rename<'_>,
) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    let mut moved: Vec<OsString> = Vec::new();
    let deadline = Instant::now() + timeout;
    let result = (|| {
        for entry in entries(src, keep)? {
            let name = entry.file_name().unwrap_or_default().to_owned();
            let mut said = false;
            loop {
                match rename(&entry, &dst.join(&name)) {
                    Ok(()) => break,
                    Err(e) => {
                        if Instant::now() > deadline {
                            return Err(e);
                        }
                        if !said {
                            log.say(&format!("busy: {}: {e}", name.to_string_lossy()));
                            said = true;
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
            moved.push(name);
        }
        Ok(())
    })();
    if result.is_err() {
        for name in moved.iter().rev() {
            let _ = rename(&dst.join(name), &src.join(name));
        }
    }
    result
}

/// Move `src` to `dst` (which doesn't exist). What can't be renamed at once (an open file, a
/// folder holding one) moves piece by piece, and is copied where even that fails.
pub fn place(src: &Path, dst: &Path) -> io::Result<()> {
    place_with(src, dst, &fs_rename)
}

pub fn place_with(src: &Path, dst: &Path, rename: Rename<'_>) -> io::Result<()> {
    if rename(src, dst).is_ok() {
        return Ok(());
    }
    let meta = std::fs::symlink_metadata(src)?;
    if !meta.is_dir() {
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(src)?;
            #[cfg(unix)]
            return std::os::unix::fs::symlink(target, dst);
            #[cfg(windows)]
            return std::os::windows::fs::symlink_file(target, dst);
            #[cfg(not(any(unix, windows)))]
            return Err(io::Error::other(format!(
                "cannot copy the link {}",
                target.display()
            )));
        }
        std::fs::copy(src, dst)?;
        if let Ok(m) = meta.modified() {
            let _ = std::fs::File::options()
                .write(true)
                .open(dst)
                .and_then(|f| f.set_modified(m));
        }
        return Ok(());
    }
    std::fs::create_dir(dst)?;
    let _ = std::fs::set_permissions(dst, meta.permissions());
    let mut children: Vec<PathBuf> = std::fs::read_dir(src)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    children.sort();
    for child in children {
        place_with(
            &child,
            &dst.join(child.file_name().unwrap_or_default()),
            rename,
        )?;
    }
    let _ = std::fs::remove_dir(src); // stays when something in it had to be copied
    Ok(())
}

/// Remove `folder`'s entries except `keep` (taking out new files that only partly arrived).
pub fn clear(folder: &Path, keep: &[String], log: &Log) {
    let Ok(list) = entries(folder, keep) else {
        return;
    };
    for entry in list {
        let is_dir = std::fs::symlink_metadata(&entry).is_ok_and(|m| m.is_dir());
        let r = if is_dir {
            std::fs::remove_dir_all(&entry)
        } else {
            std::fs::remove_file(&entry)
        };
        if let Err(e) = r {
            log.say(&format!(
                "could not take out {}: {e}",
                entry.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
}

// ---------------------------------------------------------------- starting the app

/// The environment for the app: without what a bundling bootloader set for itself (`_PYI_*`,
/// `_MEIPASS*`; its own library folder `base` on `LD_LIBRARY_PATH`, restored from `…_ORIG`).
pub fn launch_env(env: &BTreeMap<String, String>, base: Option<&str>) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = env
        .iter()
        .filter(|(k, _)| !k.starts_with("_PYI_") && !k.starts_with("_MEIPASS"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for var in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"] {
        if let Some(orig) = out.remove(&format!("{var}_ORIG")) {
            out.insert(var.to_owned(), orig);
        } else if let Some(base) = base.filter(|b| !b.is_empty())
            && out.get(var).is_some_and(|v| v.contains(base))
        {
            out.remove(var);
        }
    }
    out
}

/// The environment for the app the swap starts (the new version, or the old one when the swap was
/// undone): [`launch_env`] without a `SPACEKNIFE_HOME` that names the copy's own data folder. The
/// app that wrote the plan set that for its own processes — it is how a portable copy keeps its
/// data next to it (0.3.4's Electron main, the Tauri app's `prepare_environment`); passed on, the
/// started app would take it for a home the user chose, not see that it is a portable copy, and
/// find no update file for itself. A home anywhere else is the user's and stays.
pub fn app_env(env: &BTreeMap<String, String>, plan: &Plan) -> BTreeMap<String, String> {
    let data = plan.app.join("data");
    launch_env(env, None)
        .into_iter()
        .filter(|(k, v)| {
            let home = if cfg!(windows) {
                k.eq_ignore_ascii_case("SPACEKNIFE_HOME")
            } else {
                k == "SPACEKNIFE_HOME"
            };
            !(home && same(Path::new(v), &data))
        })
        .collect()
}

/// Start the app on its own: it outlives this process, without its stdio.
pub fn launch(exe: &Path, args: &[&str]) -> io::Result<()> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    launch_with(exe, args, &launch_env(&env, None))
}

/// [`launch`] with exactly the environment `env`.
pub fn launch_with(exe: &Path, args: &[&str], env: &BTreeMap<String, String>) -> io::Result<()> {
    use std::process::{Command, Stdio};
    let make = || {
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env_clear()
            .envs(env);
        if let Some(dir) = exe.parent() {
            cmd.current_dir(dir);
        }
        cmd
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        // out of any job this process is in, so that closing it doesn't end the app
        if make()
            .creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB)
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
        make().creation_flags(flags).spawn().map(drop) // a job that doesn't allow it
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut cmd = make();
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn().map(drop)
    }
    #[cfg(not(any(unix, windows)))]
    {
        make().spawn().map(drop)
    }
}

// ---------------------------------------------------------------- can the new version start here?

/// The flag the app's program answers without opening anything (the Tauri app's main.rs, the
/// Electron shell's index.ts): once the system's loader has given it every library it links (the
/// web view's: WebKitGTK on Linux; the C runtime), it checks for the web view's runtime (WebView2 on
/// Windows), says `{"ok": true, …}` or `{"ok": false, "error": …}` on stdout and ends: before any
/// window, web view, setting or file of the app. The helper and the program it asks are the same
/// version, so the flag only has to be known to the version that has this helper.
pub const STARTUP_CHECK: &str = "--startup-check";
/// How long the new version's program has to answer: well within the 30 s the app waits for the
/// helper's first line (0.3.4's index.ts startSwap; install.rs SWAP_ANSWER).
pub const STARTUP_CHECK_TIMEOUT: Duration = Duration::from_secs(20);
/// What is kept of each of the program's outputs.
const STARTUP_OUTPUT_LIMIT: usize = 64 * 1024;

/// Why the new version's program can't start on this machine: the swap is not begun.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the new version can't start on this computer: {0}")]
pub struct StartError(pub String);

/// Run `program --startup-check` with the environment the app would get (`env`), its standard
/// input closed and its output read, without a window or a console, for at most `timeout`; Ok with
/// what it said (the web view's version) when it can start. Linux: with every symbol bound at
/// once (LD_BIND_NOW), so that a library too old for the program fails here, not later in the
/// window. Windows: without the system's error boxes (a missing DLL, a crash): the program inherits
/// SEM_FAILCRITICALERRORS and SEM_NOGPFAULTERRORBOX, and fails with its status instead.
pub fn startup_check(
    program: &Path,
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<String, StartError> {
    use std::process::{Command, Stdio};
    let name = program.file_name().map_or_else(
        || program.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let mut cmd = Command::new(program);
    cmd.arg(STARTUP_CHECK)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .envs(env);
    if let Some(dir) = program.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(target_os = "linux")]
    cmd.env("LD_BIND_NOW", "1");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let spawned = {
        let _quiet = ERROR_MODES.quiet();
        cmd.spawn()
    };
    let mut child = spawned.map_err(|e| StartError(format!("{name} can't be run: {e}")))?;
    let stdout = read_capped(child.stdout.take());
    let stderr = read_capped(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(StartError(format!(
                    "{name} did not answer {STARTUP_CHECK} within {} s",
                    timeout.as_secs()
                )));
            }
            Err(e) => return Err(StartError(format!("{name}: {e}"))),
        }
    };
    // the pipes close with the program (unless something it started holds them: not waited for)
    let grace = Duration::from_millis(500);
    let stdout = stdout.recv_timeout(grace).unwrap_or_default();
    let stderr = stderr.recv_timeout(grace).unwrap_or_default();
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
    #[cfg(not(unix))]
    let signal = None;
    judge_startup(
        &name,
        status.code(),
        signal,
        &String::from_utf8_lossy(&stdout),
        &String::from_utf8_lossy(&stderr),
    )
    .map_err(StartError)
}

/// What the program's answer means: Ok with what it said when it can start, else why not, in
/// words (the app shows it). `code`/`signal` as [`std::process::ExitStatus`] has them.
pub fn judge_startup(
    name: &str,
    code: Option<i32>,
    signal: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Result<String, String> {
    let said: Option<serde_json::Map<String, Value>> = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .and_then(|l| serde_json::from_str(l).ok());
    match (&said, code) {
        (Some(said), Some(0)) if said.get("ok") == Some(&Value::Bool(true)) => {
            let what: Vec<String> = said
                .iter()
                .filter(|(k, _)| k.as_str() != "ok")
                .map(|(k, v)| {
                    format!(
                        "{k} {}",
                        v.as_str().map_or_else(|| v.to_string(), str::to_owned)
                    )
                })
                .collect();
            return Ok(what.join(", "));
        }
        (Some(said), _) if said.get("ok") == Some(&Value::Bool(false)) => {
            let error = said.get("error").and_then(Value::as_str).unwrap_or("");
            return Err(if error.is_empty() {
                format!("{name} says it can't start")
            } else {
                error.to_owned()
            });
        }
        _ => {}
    }
    // the last thing it wrote to stderr: the loader's own words, a panic
    let last = stderr
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(|l| {
            // "/path/to/spaceknife: error while loading shared libraries: …" → from "error"
            let l = l.find("error while loading").map_or(l, |at| &l[at..]);
            l.chars().take(300).collect::<String>()
        });
    let why = match (code, signal) {
        (Some(0), _) => format!("{name} did not answer {STARTUP_CHECK}"),
        (_, Some(sig)) => format!("{name} was ended by signal {sig}"),
        (Some(code), _) if cfg!(windows) && (code as u32) >= 0xC000_0000 => {
            let status = code as u32;
            let what = match status {
                0xC000_0135 => "Windows can't find a DLL it needs (STATUS_DLL_NOT_FOUND)",
                0xC000_0139 => {
                    "a DLL it needs lacks a function: too old (STATUS_ENTRYPOINT_NOT_FOUND)"
                }
                0xC000_007B => {
                    "it or a DLL it needs is not for this Windows (STATUS_INVALID_IMAGE_FORMAT)"
                }
                0xC000_0142 => "a DLL it needs failed to start (STATUS_DLL_INIT_FAILED)",
                0xC000_0005 => "it crashed (STATUS_ACCESS_VIOLATION)",
                0xC000_0409 => "it crashed (STATUS_STACK_BUFFER_OVERRUN)",
                _ => "it ended at once",
            };
            format!("{name}: {what}, status 0x{status:08X}")
        }
        (Some(code), _) => format!("{name} ended with exit code {code}"),
        (None, None) => format!("{name} ended without an exit code"),
    };
    Err(match last {
        Some(last) if code != Some(0) => format!("{why}: {last}"),
        _ => why,
    })
}

/// A reader of one of the program's outputs, keeping its first [`STARTUP_OUTPUT_LIMIT`] bytes (it
/// reads on to the end, so that the program never blocks on a full pipe).
fn read_capped<R: io::Read + Send + 'static>(
    from: Option<R>,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(mut from) = from {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                match from.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let room = STARTUP_OUTPUT_LIMIT.saturating_sub(kept.len());
                        kept.extend_from_slice(&buf[..n.min(room)]);
                    }
                }
            }
            let _ = tx.send(kept);
        });
    } else {
        let _ = tx.send(Vec::new());
    }
    rx
}

/// SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX: no error box for a missing DLL, none for a crash.
const QUIET_MODE: u32 = 0x0001 | 0x0002;

/// The error mode the programs this process starts inherit (Windows: it is copied when a process
/// is created), shared by the starts that overlap (threads, tests): the first makes it quiet, the
/// last puts the process's own mode back, so no start in between gets the boxes.
struct ErrorModes {
    /// how many starts are on, and the mode from before the first
    state: std::sync::Mutex<(usize, u32)>,
    /// sets the process's mode, returns the one before (SetErrorMode)
    set: fn(u32) -> u32,
}

impl ErrorModes {
    /// Quiet until the guard is dropped (and every other one).
    fn quiet(&self) -> QuietErrors<'_> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.0 == 0 {
            let before = (self.set)(QUIET_MODE);
            (self.set)(before | QUIET_MODE);
            state.1 = before;
        }
        state.0 += 1;
        QuietErrors { modes: self }
    }
}

struct QuietErrors<'a> {
    modes: &'a ErrorModes,
}

impl Drop for QuietErrors<'_> {
    fn drop(&mut self) {
        let mut state = self.modes.state.lock().unwrap_or_else(|e| e.into_inner());
        state.0 -= 1;
        if state.0 == 0 {
            (self.modes.set)(state.1);
        }
    }
}

fn set_error_mode(mode: u32) -> u32 {
    #[cfg(windows)]
    {
        // SAFETY: the process's error mode, no pointers
        unsafe { windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(mode) }
    }
    #[cfg(not(windows))]
    {
        let _ = mode;
        0 // no such thing
    }
}

static ERROR_MODES: ErrorModes = ErrorModes {
    state: std::sync::Mutex::new((0, 0)),
    set: set_error_mode,
};

// ---------------------------------------------------------------- the swap

/// How [`run_with`] waits, starts the app and places files (replaced in tests).
pub struct Ops<'a> {
    pub wait: &'a dyn Fn(u32, Duration) -> bool,
    pub start: &'a dyn Fn(&Path, &[&str]) -> io::Result<()>,
    pub place: &'a dyn Fn(&Path, &Path) -> io::Result<()>,
}

/// Steps 2-5; 0 when the new version is in place.
pub fn run(plan: &Plan, log: &Log) -> i32 {
    let env = app_env(&std::env::vars().collect(), plan);
    let start = |exe: &Path, args: &[&str]| launch_with(exe, args, &env);
    run_with(
        plan,
        log,
        &Ops {
            wait: &wait_gone,
            start: &start,
            place: &place,
        },
    )
}

pub fn run_with(plan: &Plan, log: &Log, ops: &Ops<'_>) -> i32 {
    log.say(&format!("waiting for SpaceKnife ({}) to quit", plan.pid));
    if !(ops.wait)(plan.pid, QUIT_TIMEOUT) {
        log.say("SpaceKnife did not quit: kept the old version");
        return 1;
    }
    if let Err(e) = move_out(&plan.app, &plan.old, &plan.keep, log, BUSY_TIMEOUT) {
        log.say(&format!("could not move the old files out: {e}"));
        return keep_old(plan, log, ops);
    }
    let placed = (|| {
        for entry in entries(&plan.new, &plan.keep)? {
            (ops.place)(
                &entry,
                &plan.app.join(entry.file_name().unwrap_or_default()),
            )?;
        }
        Ok::<(), io::Error>(())
    })();
    if let Err(e) = placed {
        log.say(&format!("could not move the new files in: {e}"));
        clear(&plan.app, &plan.keep, log);
        if let Err(back) = move_out(&plan.old, &plan.app, &[], log, BUSY_TIMEOUT) {
            log.say(&format!("could not restore the old files: {back}"));
        }
        return keep_old(plan, log, ops);
    }
    log.say("updated");
    if let Err(e) = (ops.start)(&plan.exe, &["--updated"]) {
        log.say(&format!("could not start SpaceKnife: {e}"));
        return 1;
    }
    0
}

fn keep_old(plan: &Plan, log: &Log, ops: &Ops<'_>) -> i32 {
    log.say("kept the old version");
    if let Err(e) = (ops.start)(&plan.exe, &[]) {
        log.say(&format!("could not start SpaceKnife: {e}"));
    }
    1
}

fn say(out: &mut dyn Write, message: &Value) {
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

/// The app reads one line and quits: later writes must not fail on its closed pipe.
fn let_go_of_stdio() {
    #[cfg(unix)]
    point_at_null(&[0, 1, 2]);
    // Windows: nothing writes to the standard handles after this point (the log is a file).
}

/// Point `fds` at /dev/null. The descriptor opened for that is closed again (it would stay open
/// here, and in the app this process starts), unless it is one of `fds` itself (a standard
/// descriptor that was closed, which `open` reuses).
#[cfg(unix)]
#[doc(hidden)]
pub fn point_at_null(fds: &[std::os::fd::RawFd]) {
    use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
    // SAFETY: plain fd juggling on descriptors the caller hands over; `null` is ours.
    unsafe {
        let raw = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if raw < 0 {
            return;
        }
        let null = OwnedFd::from_raw_fd(raw);
        for &fd in fds {
            if fd != raw {
                libc::dup2(raw, fd);
            }
        }
        if fds.contains(&raw) {
            let _ = null.into_raw_fd(); // one of them now: it stays
        }
    }
}

/// `--apply-update <plan.json>`: the arguments after the flag; returns the exit code.
pub fn main(argv: &[String]) -> i32 {
    let mut stdout = io::stdout();
    main_with(argv, &mut stdout, true)
}

/// [`main`] writing its one line to `out` (`release`: let go of the stdio afterwards).
pub fn main_with(argv: &[String], out: &mut dyn Write, release: bool) -> i32 {
    let check = |plan: &Plan| {
        let env = app_env(&std::env::vars().collect(), plan);
        startup_check(&plan.new_program(), &env, STARTUP_CHECK_TIMEOUT)
    };
    main_checked(argv, out, release, &check)
}

/// [`main_with`] asking `check` whether the new version's program starts (replaced in tests).
pub fn main_checked(
    argv: &[String],
    out: &mut dyn Write,
    release: bool,
    check: &dyn Fn(&Plan) -> Result<String, StartError>,
) -> i32 {
    let [plan_path] = argv else {
        say(
            out,
            &serde_json::json!({"ok": false, "error": "usage: --apply-update <plan.json>"}),
        );
        return 2;
    };
    let plan_path = Path::new(plan_path);
    let plan = match Plan::read(plan_path) {
        Ok(p) => p,
        Err(e) => {
            Log::new(&plan_path.with_extension("log")).say(&format!("not started: {e}"));
            say(
                out,
                &serde_json::json!({"ok": false, "error": e.to_string()}),
            );
            return 2;
        }
    };
    let log = Log::new(&plan.log);
    // before anything is touched: a version that can't start here is not put in place (no
    // WebView2, no WebKitGTK 4.1, a missing DLL); the app goes on as it was and says why
    match check(&plan) {
        Ok(said) => log.say(&format!(
            "the new version starts here ({})",
            if said.is_empty() { "no details" } else { &said }
        )),
        Err(e) => {
            log.say(&format!("not started: {e}"));
            say(
                out,
                &serde_json::json!({"ok": false, "error": e.to_string()}),
            );
            return 2;
        }
    }
    say(out, &serde_json::json!({"ok": true}));
    if release {
        let_go_of_stdio();
    }
    run(&plan, &log)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const EXE: &str = if cfg!(windows) {
        "SpaceKnife.exe"
    } else {
        "spaceknife"
    };

    fn write(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    /// A portable copy with its data folder, and the new version unpacked in its updates folder.
    fn tree(root: &Path) -> Plan {
        let app = root.join("SpaceKnife-0.3.3-portable");
        let updates = app.join("data").join("data").join("updates");
        let new = updates
            .join("staging-0.3.4")
            .join("SpaceKnife-0.3.4-portable");
        for (folder, version) in [(&app, "0.3.3"), (&new, "0.3.4")] {
            write(&folder.join(EXE), version);
            write(&folder.join("resources/app.asar"), version);
            write(
                &folder.join("resources/backend/_internal/python312.dll"),
                version,
            );
            std::fs::create_dir_all(folder.join("data")).unwrap();
        }
        write(&app.join(".hidden"), "old");
        write(&app.join("data/keep.txt"), "mine");
        write(&new.join("data/README.txt"), "the archive's note");
        write(&new.join("E2E_NEW_BUILD.txt"), "0.3.4");
        Plan {
            pid: 4242,
            exe: app.join(EXE),
            app,
            new,
            old: updates.join("previous-0.3.3"),
            log: updates.join("update-0.3.4.log"),
            keep: vec!["data".into()],
        }
    }

    fn write_plan(plan: &Plan, file: &Path, change: &[(&str, Value)]) -> PathBuf {
        let mut raw = serde_json::json!({
            "v": 1, "pid": plan.pid,
            "app": plan.app, "new": plan.new, "old": plan.old, "exe": plan.exe, "log": plan.log,
            "keep": plan.keep,
        });
        for (k, v) in change {
            raw[*k] = v.clone();
        }
        std::fs::write(file, raw.to_string()).unwrap();
        file.to_owned()
    }

    fn log_lines(plan: &Plan) -> Vec<String> {
        read(&plan.log)
            .lines()
            .map(|l| l.splitn(3, ' ').nth(2).unwrap_or("").to_owned())
            .collect()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn the_plan_is_read_and_checked_before_anything_moves() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        let file = tmp.path().join("update-0.3.4.json");
        assert_eq!(Plan::read(&write_plan(&plan, &file, &[])).unwrap(), plan);
        for (change, says) in [
            (("v", Value::from(2)), "version 1"),
            (("pid", Value::from("x")), "pid"),
            (("new", Value::from("relative/new")), "absolute"),
            (
                (
                    "exe",
                    Value::from(tmp.path().join(EXE).to_string_lossy().into_owned()),
                ),
                "is not in",
            ),
            (
                ("new", Value::from(plan.app.to_string_lossy().into_owned())),
                "must not be",
            ),
        ] {
            let err = Plan::read(&write_plan(&plan, &file, &[change])).unwrap_err();
            assert!(err.0.contains(says), "{err} / {says}");
        }
        std::fs::remove_file(plan.new.join(EXE)).unwrap();
        assert!(
            Plan::read(&write_plan(&plan, &file, &[]))
                .unwrap_err()
                .0
                .contains("incomplete")
        );
        assert!(
            Plan::read(&tmp.path().join("missing.json"))
                .unwrap_err()
                .0
                .contains("can't read")
        );
    }

    #[test]
    fn the_new_version_takes_the_place_of_the_old_one() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        let started = RefCell::new(Vec::<Vec<String>>::new());
        let waited = RefCell::new(Vec::new());
        let wait = |pid: u32, _t: Duration| {
            waited.borrow_mut().push(pid);
            true
        };
        let start = |exe: &Path, args: &[&str]| {
            let mut v = vec![exe.to_string_lossy().into_owned()];
            v.extend(args.iter().map(|a| (*a).to_owned()));
            started.borrow_mut().push(v);
            Ok(())
        };
        let code = run_with(
            &plan,
            &Log::new(&plan.log),
            &Ops {
                wait: &wait,
                start: &start,
                place: &place,
            },
        );
        assert_eq!(code, 0);
        assert_eq!(*waited.borrow(), [4242]);
        assert_eq!(
            *started.borrow(),
            [vec![
                plan.exe.to_string_lossy().into_owned(),
                "--updated".into()
            ]]
        );
        assert_eq!(read(&plan.app.join(EXE)), "0.3.4");
        assert_eq!(
            read(&plan.app.join("resources/backend/_internal/python312.dll")),
            "0.3.4"
        );
        assert!(plan.app.join("E2E_NEW_BUILD.txt").is_file());
        assert!(!plan.app.join(".hidden").exists());
        assert_eq!(names(&plan.app.join("data")), ["data", "keep.txt"]);
        let mut want = vec![".hidden".to_owned(), EXE.to_owned(), "resources".to_owned()];
        want.sort();
        assert_eq!(names(&plan.old), want);
        assert_eq!(read(&plan.old.join(EXE)), "0.3.3");
        assert_eq!(
            log_lines(&plan),
            ["waiting for SpaceKnife (4242) to quit", "updated"]
        );
    }

    #[test]
    fn an_app_that_does_not_quit_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        let started = RefCell::new(0);
        let start = |_: &Path, _: &[&str]| {
            *started.borrow_mut() += 1;
            Ok(())
        };
        let code = run_with(
            &plan,
            &Log::new(&plan.log),
            &Ops {
                wait: &|_, _| false,
                start: &start,
                place: &place,
            },
        );
        assert_eq!(code, 1);
        assert_eq!(*started.borrow(), 0);
        assert_eq!(read(&plan.app.join(EXE)), "0.3.3");
        assert_eq!(
            log_lines(&plan).last().unwrap(),
            "SpaceKnife did not quit: kept the old version"
        );
    }

    #[test]
    fn the_old_version_comes_back_when_the_new_files_cannot_move_in() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        let failing = |src: &Path, dst: &Path| {
            if src.file_name().is_some_and(|n| n == "resources") {
                return Err(io::Error::other("disk full"));
            }
            place(src, dst)
        };
        let started = RefCell::new(Vec::<usize>::new());
        let start = |_: &Path, args: &[&str]| {
            started.borrow_mut().push(args.len());
            Ok(())
        };
        let code = run_with(
            &plan,
            &Log::new(&plan.log),
            &Ops {
                wait: &|_, _| true,
                start: &start,
                place: &failing,
            },
        );
        assert_eq!(code, 1);
        assert_eq!(*started.borrow(), [0]); // the old version, without --updated
        assert_eq!(read(&plan.app.join(EXE)), "0.3.3");
        assert_eq!(read(&plan.app.join("resources/app.asar")), "0.3.3");
        assert!(plan.app.join(".hidden").is_file());
        assert!(!plan.app.join("E2E_NEW_BUILD.txt").exists());
        assert!(plan.app.join("data/keep.txt").is_file());
        let lines = log_lines(&plan);
        assert_eq!(
            lines[lines.len() - 2..],
            [
                "could not move the new files in: disk full",
                "kept the old version"
            ]
        );
    }

    #[test]
    fn moving_out_tries_again_and_puts_back_what_moved() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        let held = RefCell::new(0);
        let rename = |src: &Path, dst: &Path| {
            if src.file_name().is_some_and(|n| n == "resources")
                && src.parent() == Some(plan.app.as_path())
            {
                *held.borrow_mut() += 1;
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "in use"));
            }
            std::fs::rename(src, dst)
        };
        let log = Log::new(&plan.log);
        let err = move_out_with(
            &plan.app,
            &plan.old,
            &plan.keep,
            &log,
            Duration::from_millis(600),
            &rename,
        );
        assert!(err.is_err());
        assert!(*held.borrow() >= 2); // tried again
        let mut want = vec![
            ".hidden".to_owned(),
            EXE.to_owned(),
            "data".to_owned(),
            "resources".to_owned(),
        ];
        want.sort();
        assert_eq!(names(&plan.app), want);
        assert!(names(&plan.old).is_empty());
        assert_eq!(log_lines(&plan), ["busy: resources: in use"]);
    }

    #[test]
    fn what_cannot_be_renamed_moves_piece_by_piece_or_is_copied() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        let held = plan.new.join("resources/backend/_internal/python312.dll");
        let rename = |src: &Path, dst: &Path| {
            if src == held || held.starts_with(src) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "in use"));
            }
            std::fs::rename(src, dst)
        };
        let target = tmp.path().join("placed");
        place_with(&plan.new.join("resources"), &target, &rename).unwrap();
        assert_eq!(read(&target.join("app.asar")), "0.3.4");
        assert_eq!(
            read(&target.join("backend/_internal/python312.dll")),
            "0.3.4"
        );
        assert!(held.is_file()); // copied: the original stays
        assert!(!plan.new.join("resources/app.asar").exists()); // moved
    }

    #[test]
    fn the_executable_may_be_inside_an_app_bundle_in_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let mut plan = tree(tmp.path());
        let inner = Path::new("SpaceKnife.app")
            .join("Contents")
            .join("MacOS")
            .join("spaceknife");
        write(&plan.app.join(&inner), "0.3.3");
        plan.exe = plan.app.join(&inner);
        // the new version must have it at the same place
        assert!(plan.check().unwrap_err().0.contains("incomplete"));
        write(&plan.new.join(&inner), "0.3.4");
        assert_eq!(plan.check(), Ok(()));
        // not in the folder at all
        plan.exe = tmp.path().join("elsewhere").join("spaceknife");
        assert!(plan.check().unwrap_err().0.contains("is not in"));
    }

    #[test]
    fn waiting_for_a_process() {
        #[cfg(windows)]
        let (quick, slow) = (
            ("cmd", vec!["/C", "exit 0"]),
            ("cmd", vec!["/C", "ping -n 30 127.0.0.1 >NUL"]),
        );
        #[cfg(not(windows))]
        let (quick, slow) = (("true", Vec::<&str>::new()), ("sleep", vec!["30"]));
        let mut done = std::process::Command::new(quick.0)
            .args(&quick.1)
            .spawn()
            .unwrap();
        done.wait().unwrap();
        assert!(wait_gone(done.id(), Duration::from_secs(1)));
        let mut running = std::process::Command::new(slow.0)
            .args(&slow.1)
            .spawn()
            .unwrap();
        assert!(!wait_gone(running.id(), Duration::from_millis(300)));
        running.kill().unwrap();
        running.wait().unwrap();
    }

    #[test]
    fn the_app_starts_without_the_bootloader_s_environment() {
        let env: BTreeMap<String, String> = [
            ("PATH", "/usr/bin"),
            ("_PYI_ARCHIVE_FILE", "x"),
            ("_PYI_PARENT_PROCESS_LEVEL", "1"),
            (
                "LD_LIBRARY_PATH",
                "/opt/sk/resources/backend/_internal:/usr/lib/x",
            ),
            ("LD_LIBRARY_PATH_ORIG", "/usr/lib/x"),
            ("DYLD_LIBRARY_PATH", "/opt/sk/resources/backend/_internal"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let got = launch_env(&env, Some("/opt/sk/resources/backend/_internal"));
        let want: BTreeMap<String, String> =
            [("PATH", "/usr/bin"), ("LD_LIBRARY_PATH", "/usr/lib/x")]
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect();
        assert_eq!(got, want);
        let mine: BTreeMap<String, String> =
            [("LD_LIBRARY_PATH".to_owned(), "/usr/lib/mine".to_owned())].into();
        assert_eq!(launch_env(&mine, Some("/opt/sk")), mine);
    }

    #[test]
    fn the_new_version_finds_its_data_folder_by_itself() {
        // the app that wrote the plan pointed SPACEKNIFE_HOME at the copy's data folder for its
        // own processes (0.3.4's Electron main, the Tauri app's prepare_environment); started
        // with it, the new version takes that for a home the user chose, is no portable copy
        // and gets no update file
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        let data = plan.app.join("data").to_string_lossy().into_owned();
        let shown = if cfg!(windows) {
            format!("{}\\", data.to_uppercase())
        } else {
            format!("{data}/")
        };
        for home in [data.clone(), shown] {
            let env: BTreeMap<String, String> = [
                ("PATH".to_owned(), "/usr/bin".to_owned()),
                ("SPACEKNIFE_HOME".to_owned(), home.clone()),
                (
                    "SPACEKNIFE_UPDATE_FEED".to_owned(),
                    "http://127.0.0.1:9/r".to_owned(),
                ),
            ]
            .into();
            let got = app_env(&env, &plan);
            assert!(!got.contains_key("SPACEKNIFE_HOME"), "{home}");
            assert_eq!(got["PATH"], "/usr/bin");
            assert_eq!(got["SPACEKNIFE_UPDATE_FEED"], "http://127.0.0.1:9/r");
        }
        // a home somewhere else is the user's: it stays
        let elsewhere = tmp.path().join("mine").to_string_lossy().into_owned();
        let env: BTreeMap<String, String> =
            [("SPACEKNIFE_HOME".to_owned(), elsewhere.clone())].into();
        assert_eq!(app_env(&env, &plan)["SPACEKNIFE_HOME"], elsewhere);
        if cfg!(windows) {
            // Windows names the variable in any case
            let env: BTreeMap<String, String> = [("SpaceKnife_Home".to_owned(), data)].into();
            assert!(app_env(&env, &plan).is_empty());
        }
    }

    #[test]
    fn a_bad_plan_says_why_and_touches_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        std::fs::remove_file(plan.new.join(EXE)).unwrap();
        let file = write_plan(&plan, &tmp.path().join("update-0.3.4.json"), &[]);
        let mut out = Vec::new();
        assert_eq!(
            main_with(&[file.to_string_lossy().into_owned()], &mut out, false),
            2
        );
        let said: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(said["ok"], false);
        assert!(said["error"].as_str().unwrap().contains("incomplete"));
        assert!(
            read(&tmp.path().join("update-0.3.4.log"))
                .contains("not started: the new version is incomplete")
        );
        assert_eq!(read(&plan.app.join(EXE)), "0.3.3");
        assert_eq!(main_with(&[], &mut Vec::new(), false), 2);
    }

    // ------------------------------------------------------------ the startup check

    /// A stand-in program of examples/ (`cargo test` builds them with the tests).
    fn example(name: &str) -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        let path = exe
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples")
            .join(file);
        assert!(
            path.is_file(),
            "{} (cargo test builds the examples)",
            path.display()
        );
        path
    }

    /// The new version's program of `plan` as examples/fake_app.rs, answering in `mode`.
    fn fake_new_program(plan: &Plan, mode: &str) -> PathBuf {
        let program = plan.new_program();
        std::fs::copy(example("fake_app"), &program).unwrap();
        std::fs::write(program.with_file_name("fake-app-mode.txt"), mode).unwrap();
        program
    }

    fn this_env() -> BTreeMap<String, String> {
        std::env::vars().collect()
    }

    fn check(program: &Path) -> Result<String, StartError> {
        startup_check(program, &this_env(), STARTUP_CHECK_TIMEOUT)
    }

    #[test]
    fn a_new_version_that_cannot_start_here_is_not_put_in_place() {
        // the Tauri app on a Windows without WebView2: the helper says so, and nothing moves (0.3.4's
        // app then shows 「无法启动更新：…」 and goes on, its copy untouched)
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        fake_new_program(&plan, "no-webview");
        let file = write_plan(&plan, &tmp.path().join("update-0.3.4.json"), &[]);
        let mut out = Vec::new();
        let code = main_with(&[file.to_string_lossy().into_owned()], &mut out, false);
        assert_eq!(code, 2);
        let said: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(said["ok"], false, "{said}");
        let error = said["error"].as_str().unwrap();
        assert!(
            error.starts_with("the new version can't start on this computer: ")
                && error.contains("WebView2 Runtime"),
            "{error}"
        );
        assert_eq!(read(&plan.app.join(EXE)), "0.3.3");
        assert!(plan.app.join(".hidden").is_file());
        assert!(plan.new.join("E2E_NEW_BUILD.txt").is_file());
        assert!(!plan.old.exists());
        assert_eq!(
            log_lines(&plan),
            [format!("not started: {error}")],
            "the reason is in the update's log"
        );
    }

    #[test]
    fn the_check_comes_before_the_ok_and_the_swap() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        let file = write_plan(&plan, &tmp.path().join("update-0.3.4.json"), &[]);
        let asked = RefCell::new(Vec::new());
        let refuse = |p: &Plan| {
            asked.borrow_mut().push(p.new_program());
            Err(StartError("no WebKitGTK 4.1".into()))
        };
        let mut out = Vec::new();
        let code = main_checked(
            &[file.to_string_lossy().into_owned()],
            &mut out,
            false,
            &refuse,
        );
        assert_eq!(code, 2);
        assert_eq!(*asked.borrow(), [plan.new.join(EXE)]);
        let said: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            said,
            serde_json::json!({"ok": false, "error": "the new version can't start on this computer: no WebKitGTK 4.1"})
        );
        assert_eq!(read(&plan.app.join(EXE)), "0.3.3");
    }

    #[test]
    fn the_new_program_is_the_executable_at_its_place_in_the_new_version() {
        let tmp = tempfile::tempdir().unwrap();
        let mut plan = tree(tmp.path());
        assert_eq!(plan.new_program(), plan.new.join(EXE));
        plan.exe = plan.app.join("SpaceKnife.app/Contents/MacOS/SpaceKnife");
        assert_eq!(
            plan.new_program(),
            plan.new.join("SpaceKnife.app/Contents/MacOS/SpaceKnife")
        );
    }

    #[test]
    fn the_programs_answer_decides() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        let answer = |mode: &str| check(&fake_new_program(&plan, mode));
        assert_eq!(answer("ok"), Ok("webview fake 1.0".into()));
        let no = answer("no-webview").unwrap_err();
        assert!(
            no.0.contains("needs the Microsoft Edge WebView2 Runtime"),
            "{no}"
        );
        // ended well but said nothing: it doesn't know the flag, it is not the program this helper
        // belongs with
        let silent = answer("silent").unwrap_err();
        assert!(
            silent.0.contains("did not answer --startup-check"),
            "{silent}"
        );
        // the loader's own words, as Linux says them without WebKitGTK 4.1
        let loader = answer("loader").unwrap_err();
        assert_eq!(
            loader.0,
            format!(
                "{EXE} ended with exit code 127: error while loading shared libraries: libwebkit2gtk-4.1.so.0: cannot open shared object file: No such file or directory"
            )
        );
        // a lot of output does not hold it up
        assert_eq!(answer("noisy"), Ok("webview fake 1.0".into()));
        let bind = answer("bind-now").unwrap();
        let want = if cfg!(target_os = "linux") {
            "bind_now 1"
        } else {
            "bind_now unset"
        };
        assert_eq!(bind, want);
    }

    #[test]
    fn a_program_that_does_not_answer_is_ended() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        let program = fake_new_program(&plan, "hang");
        let started = Instant::now();
        let err = startup_check(&program, &this_env(), Duration::from_secs(1)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(
            err.0,
            format!("{EXE} did not answer --startup-check within 1 s")
        );
    }

    #[test]
    fn a_program_that_cannot_be_run_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        // the tree's new program is a text file
        let err = check(&plan.new_program()).unwrap_err();
        assert!(err.0.starts_with(&format!("{EXE} can't be run: ")), "{err}");
        let err = check(&tmp.path().join("nothing").join(EXE)).unwrap_err();
        assert!(err.0.contains("can't be run"), "{err}");
    }

    /// A copy of the stand-in that links a library no machine has.
    fn program_with_a_missing_library(dir: &Path) -> PathBuf {
        let program = dir.join(EXE);
        std::fs::create_dir_all(dir).unwrap();
        if cfg!(windows) {
            // links spaceknife_missing_runtime.dll, as a program without +crt-static links
            // VCRUNTIME140.dll
            std::fs::copy(example("fake_app_missing_dll"), &program).unwrap();
        } else {
            // the C library's name changed, as long as before: the loader can't find it
            let (from, to): (&[u8], &[u8]) = if cfg!(target_os = "macos") {
                (b"libSystem.B.dylib", b"libSystem.Q.dylib")
            } else {
                (b"libc.so.6\0", b"libq.so.6\0")
            };
            let mut bytes = std::fs::read(example("fake_app")).unwrap();
            let mut changed = 0;
            for at in 0..bytes.len().saturating_sub(from.len()) {
                if &bytes[at..at + from.len()] == from {
                    bytes[at..at + from.len()].copy_from_slice(to);
                    changed += 1;
                }
            }
            assert!(changed > 0, "the stand-in names no {from:?}");
            std::fs::write(&program, bytes).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        program
    }

    #[test]
    fn a_program_whose_libraries_are_missing_cannot_start() {
        // Windows without the Visual C++ runtime, Linux without libwebkit2gtk-4.1: the loader stops
        // the program before main. On Windows without the box that says so (it would hold the
        // program until someone closes it): the check ends at once with the loader's status.
        let tmp = tempfile::tempdir().unwrap();
        let program = program_with_a_missing_library(&tmp.path().join("new"));
        let started = Instant::now();
        let err = check(&program).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "{:?}",
            started.elapsed()
        );
        if cfg!(windows) {
            assert_eq!(
                err.0,
                format!(
                    "{EXE}: Windows can't find a DLL it needs (STATUS_DLL_NOT_FOUND), status 0xC0000135"
                )
            );
        } else if cfg!(target_os = "linux") {
            assert!(
                err.0
                    .contains("exit code 127: error while loading shared libraries: libq.so.6"),
                "{err}"
            );
        } else {
            assert!(!err.0.is_empty());
        }
        assert!(
            err.to_string()
                .starts_with("the new version can't start on this computer: ")
        );
    }

    /// The process's error mode, as SetErrorMode keeps it (a test's own).
    static FAKE_MODE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x8000);

    fn fake_set(mode: u32) -> u32 {
        FAKE_MODE.swap(mode, std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn starts_at_the_same_time_stay_quiet_until_the_last_is_done() {
        // two checks at once (threads, tests): the first one done must not give the other's
        // program the error boxes back while it is being started
        let modes = ErrorModes {
            state: std::sync::Mutex::new((0, 0)),
            set: fake_set,
        };
        let mode = || FAKE_MODE.load(std::sync::atomic::Ordering::SeqCst);
        let first = modes.quiet();
        assert_eq!(mode(), 0x8000 | QUIET_MODE); // the process's own bits kept
        let second = modes.quiet();
        drop(first);
        assert_eq!(mode(), 0x8000 | QUIET_MODE, "the second start is still on");
        drop(second);
        assert_eq!(mode(), 0x8000, "the process's own mode is back");
        drop(modes.quiet());
        assert_eq!(mode(), 0x8000);
    }

    #[test]
    fn what_the_answer_means() {
        let judge = |code, signal, out: &str, err: &str| {
            judge_startup("SpaceKnife.exe", code, signal, out, err)
        };
        assert_eq!(
            judge(
                Some(0),
                None,
                "\n{\"ok\": true, \"webview\": \"131.0\", \"x\": 1}\n",
                ""
            ),
            Ok("webview 131.0, x 1".into())
        );
        // ok, but it failed after all
        assert_eq!(
            judge(Some(1), None, "{\"ok\": true}", "panicked at main.rs\n"),
            Err("SpaceKnife.exe ended with exit code 1: panicked at main.rs".into())
        );
        assert_eq!(
            judge(Some(1), None, "{\"ok\": false}", ""),
            Err("SpaceKnife.exe says it can't start".into())
        );
        assert_eq!(
            judge(Some(0), None, "something else\n", "a warning\n"),
            Err("SpaceKnife.exe did not answer --startup-check".into())
        );
        assert_eq!(
            judge(
                None,
                Some(6),
                "",
                "dyld: Library not loaded: @rpath/x.dylib\n"
            ),
            Err(
                "SpaceKnife.exe was ended by signal 6: dyld: Library not loaded: @rpath/x.dylib"
                    .into()
            )
        );
        if cfg!(windows) {
            for (status, says) in [
                (
                    0xC000_0135u32,
                    "Windows can't find a DLL it needs (STATUS_DLL_NOT_FOUND), status 0xC0000135",
                ),
                (
                    0xC000_0139,
                    "a DLL it needs lacks a function: too old (STATUS_ENTRYPOINT_NOT_FOUND), status 0xC0000139",
                ),
                (
                    0xC000_007B,
                    "it or a DLL it needs is not for this Windows (STATUS_INVALID_IMAGE_FORMAT), status 0xC000007B",
                ),
                (0xC000_0001, "it ended at once, status 0xC0000001"),
            ] {
                assert_eq!(
                    judge(Some(status as i32), None, "", ""),
                    Err(format!("SpaceKnife.exe: {says}"))
                );
            }
        }
    }
}
