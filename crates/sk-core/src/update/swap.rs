//! Put a portable copy's new version in place: the app binary's `--apply-update <plan>` mode.
//!
//! 「无缝更新」 on a portable copy (the Windows zip, the Linux tar.gz): the app unpacks the new
//! version into its updates folder, writes a plan ([`Plan`], JSON) and starts *the new version's*
//! executable with `--apply-update <plan>`, detached, just before it quits. That process then
//! ([`main`]):
//!
//! 1. says `{"ok": true}` on stdout once the plan is usable (the app waits for this line: when the
//!    helper can't start, the app goes on as it was), and lets go of its stdio;
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
            Some(Value::Array(a)) => {
                a.iter().map(|k| k.as_str().map_or_else(|| k.to_string(), str::to_owned)).collect()
            }
            Some(_) => return Err(bad("keep is not a list")),
        };
        let plan = Plan { pid, app: p("app")?, new: p("new")?, old: p("old")?, exe: p("exe")?, log: p("log")?, keep };
        plan.check()?;
        Ok(plan)
    }

    /// The new version must be complete before anything is moved.
    pub fn check(&self) -> Result<(), PlanError> {
        for (name, p) in [("app", &self.app), ("new", &self.new), ("old", &self.old), ("exe", &self.exe), ("log", &self.log)] {
            if !p.is_absolute() {
                return Err(PlanError(format!("{name} is not an absolute path: {}", p.display())));
            }
        }
        if !self.app.is_dir() {
            return Err(PlanError(format!("no folder {}", self.app.display())));
        }
        if !self.exe.parent().is_some_and(|p| same(p, &self.app)) {
            return Err(PlanError(format!("{} is not in {}", self.exe.display(), self.app.display())));
        }
        let exe_name = self.exe.file_name().unwrap_or_default();
        if !self.new.join(exe_name).is_file() {
            return Err(PlanError(format!(
                "the new version is incomplete: no {} in {}",
                exe_name.to_string_lossy(),
                self.new.display()
            )));
        }
        if same(&self.new, &self.app) || same(&self.old, &self.app) {
            return Err(PlanError("the new and the old folder must not be the app's folder".into()));
        }
        if self.old.exists() && std::fs::read_dir(&self.old).is_ok_and(|mut d| d.next().is_some()) {
            return Err(PlanError(format!("{} is not empty", self.old.display())));
        }
        Ok(())
    }
}

fn normcase(p: &Path) -> String {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_owned());
    let s = abs.to_string_lossy().into_owned();
    let s = s.trim_end_matches(['/', '\\']).to_owned();
    if cfg!(windows) { s.replace('/', "\\").to_lowercase() } else { s }
}

fn same(a: &Path, b: &Path) -> bool {
    normcase(a) == normcase(b)
}

fn name_case(name: &str) -> String {
    if cfg!(windows) { name.to_lowercase() } else { name.to_owned() }
}

/// One timestamped line per step in the plan's log file (never fails).
#[derive(Clone, Debug)]
pub struct Log {
    pub path: PathBuf,
}

impl Log {
    pub fn new(path: &Path) -> Log {
        Log { path: path.to_owned() }
    }

    pub fn say(&self, message: &str) {
        let line = format!("{} {message}\n", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"));
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
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
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
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
pub fn move_out(src: &Path, dst: &Path, keep: &[String], log: &Log, timeout: Duration) -> io::Result<()> {
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
            return Err(io::Error::other(format!("cannot copy the link {}", target.display())));
        }
        std::fs::copy(src, dst)?;
        if let Ok(m) = meta.modified() {
            let _ = std::fs::File::options().write(true).open(dst).and_then(|f| f.set_modified(m));
        }
        return Ok(());
    }
    std::fs::create_dir(dst)?;
    let _ = std::fs::set_permissions(dst, meta.permissions());
    let mut children: Vec<PathBuf> = std::fs::read_dir(src)?.filter_map(Result::ok).map(|e| e.path()).collect();
    children.sort();
    for child in children {
        place_with(&child, &dst.join(child.file_name().unwrap_or_default()), rename)?;
    }
    let _ = std::fs::remove_dir(src); // stays when something in it had to be copied
    Ok(())
}

/// Remove `folder`'s entries except `keep` (taking out new files that only partly arrived).
pub fn clear(folder: &Path, keep: &[String], log: &Log) {
    let Ok(list) = entries(folder, keep) else { return };
    for entry in list {
        let is_dir = std::fs::symlink_metadata(&entry).is_ok_and(|m| m.is_dir());
        let r = if is_dir { std::fs::remove_dir_all(&entry) } else { std::fs::remove_file(&entry) };
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

/// Start the app on its own: it outlives this process, without its stdio.
pub fn launch(exe: &Path, args: &[&str]) -> io::Result<()> {
    use std::process::{Command, Stdio};
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let make = || {
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env_clear()
            .envs(launch_env(&env, None));
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
        if make().creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB).spawn().is_ok() {
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

// ---------------------------------------------------------------- the swap

/// How [`run_with`] waits, starts the app and places files (replaced in tests).
pub struct Ops<'a> {
    pub wait: &'a dyn Fn(u32, Duration) -> bool,
    pub start: &'a dyn Fn(&Path, &[&str]) -> io::Result<()>,
    pub place: &'a dyn Fn(&Path, &Path) -> io::Result<()>,
}

/// Steps 2-5; 0 when the new version is in place.
pub fn run(plan: &Plan, log: &Log) -> i32 {
    run_with(plan, log, &Ops { wait: &wait_gone, start: &launch, place: &place })
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
            (ops.place)(&entry, &plan.app.join(entry.file_name().unwrap_or_default()))?;
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
    {
        // SAFETY: plain fd juggling on the process's own standard descriptors.
        unsafe {
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            if null >= 0 {
                for fd in 0..3 {
                    libc::dup2(null, fd);
                }
            }
        }
    }
    // Windows: nothing writes to the standard handles after this point (the log is a file).
}

/// `--apply-update <plan.json>`: the arguments after the flag; returns the exit code.
pub fn main(argv: &[String]) -> i32 {
    let mut stdout = io::stdout();
    main_with(argv, &mut stdout, true)
}

/// [`main`] writing its one line to `out` (`release`: let go of the stdio afterwards).
pub fn main_with(argv: &[String], out: &mut dyn Write, release: bool) -> i32 {
    let [plan_path] = argv else {
        say(out, &serde_json::json!({"ok": false, "error": "usage: --apply-update <plan.json>"}));
        return 2;
    };
    let plan_path = Path::new(plan_path);
    let plan = match Plan::read(plan_path) {
        Ok(p) => p,
        Err(e) => {
            Log::new(&plan_path.with_extension("log")).say(&format!("not started: {e}"));
            say(out, &serde_json::json!({"ok": false, "error": e.to_string()}));
            return 2;
        }
    };
    say(out, &serde_json::json!({"ok": true}));
    if release {
        let_go_of_stdio();
    }
    run(&plan, &Log::new(&plan.log))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const EXE: &str = if cfg!(windows) { "SpaceKnife.exe" } else { "spaceknife" };

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
        let new = updates.join("staging-0.3.4").join("SpaceKnife-0.3.4-portable");
        for (folder, version) in [(&app, "0.3.3"), (&new, "0.3.4")] {
            write(&folder.join(EXE), version);
            write(&folder.join("resources/app.asar"), version);
            write(&folder.join("resources/backend/_internal/python312.dll"), version);
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
        read(&plan.log).lines().map(|l| l.splitn(3, ' ').nth(2).unwrap_or("").to_owned()).collect()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
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
            (("exe", Value::from(tmp.path().join(EXE).to_string_lossy().into_owned())), "is not in"),
            (("new", Value::from(plan.app.to_string_lossy().into_owned())), "must not be"),
        ] {
            let err = Plan::read(&write_plan(&plan, &file, &[change])).unwrap_err();
            assert!(err.0.contains(says), "{err} / {says}");
        }
        std::fs::remove_file(plan.new.join(EXE)).unwrap();
        assert!(Plan::read(&write_plan(&plan, &file, &[])).unwrap_err().0.contains("incomplete"));
        assert!(Plan::read(&tmp.path().join("missing.json")).unwrap_err().0.contains("can't read"));
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
        let code = run_with(&plan, &Log::new(&plan.log), &Ops { wait: &wait, start: &start, place: &place });
        assert_eq!(code, 0);
        assert_eq!(*waited.borrow(), [4242]);
        assert_eq!(*started.borrow(), [vec![plan.exe.to_string_lossy().into_owned(), "--updated".into()]]);
        assert_eq!(read(&plan.app.join(EXE)), "0.3.4");
        assert_eq!(read(&plan.app.join("resources/backend/_internal/python312.dll")), "0.3.4");
        assert!(plan.app.join("E2E_NEW_BUILD.txt").is_file());
        assert!(!plan.app.join(".hidden").exists());
        assert_eq!(names(&plan.app.join("data")), ["data", "keep.txt"]);
        let mut want = vec![".hidden".to_owned(), EXE.to_owned(), "resources".to_owned()];
        want.sort();
        assert_eq!(names(&plan.old), want);
        assert_eq!(read(&plan.old.join(EXE)), "0.3.3");
        assert_eq!(log_lines(&plan), ["waiting for SpaceKnife (4242) to quit", "updated"]);
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
        let code = run_with(&plan, &Log::new(&plan.log), &Ops { wait: &|_, _| false, start: &start, place: &place });
        assert_eq!(code, 1);
        assert_eq!(*started.borrow(), 0);
        assert_eq!(read(&plan.app.join(EXE)), "0.3.3");
        assert_eq!(log_lines(&plan).last().unwrap(), "SpaceKnife did not quit: kept the old version");
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
        let code = run_with(&plan, &Log::new(&plan.log), &Ops { wait: &|_, _| true, start: &start, place: &failing });
        assert_eq!(code, 1);
        assert_eq!(*started.borrow(), [0]); // the old version, without --updated
        assert_eq!(read(&plan.app.join(EXE)), "0.3.3");
        assert_eq!(read(&plan.app.join("resources/app.asar")), "0.3.3");
        assert!(plan.app.join(".hidden").is_file());
        assert!(!plan.app.join("E2E_NEW_BUILD.txt").exists());
        assert!(plan.app.join("data/keep.txt").is_file());
        let lines = log_lines(&plan);
        assert_eq!(lines[lines.len() - 2..], ["could not move the new files in: disk full", "kept the old version"]);
    }

    #[test]
    fn moving_out_tries_again_and_puts_back_what_moved() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        let held = RefCell::new(0);
        let rename = |src: &Path, dst: &Path| {
            if src.file_name().is_some_and(|n| n == "resources") && src.parent() == Some(plan.app.as_path()) {
                *held.borrow_mut() += 1;
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "in use"));
            }
            std::fs::rename(src, dst)
        };
        let log = Log::new(&plan.log);
        let err = move_out_with(&plan.app, &plan.old, &plan.keep, &log, Duration::from_millis(600), &rename);
        assert!(err.is_err());
        assert!(*held.borrow() >= 2); // tried again
        let mut want = vec![".hidden".to_owned(), EXE.to_owned(), "data".to_owned(), "resources".to_owned()];
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
        assert_eq!(read(&target.join("backend/_internal/python312.dll")), "0.3.4");
        assert!(held.is_file()); // copied: the original stays
        assert!(!plan.new.join("resources/app.asar").exists()); // moved
    }

    #[test]
    fn waiting_for_a_process() {
        #[cfg(windows)]
        let (quick, slow) = (("cmd", vec!["/C", "exit 0"]), ("cmd", vec!["/C", "ping -n 30 127.0.0.1 >NUL"]));
        #[cfg(not(windows))]
        let (quick, slow) = (("true", vec![]), ("sleep", vec!["30"]));
        let mut done = std::process::Command::new(quick.0).args(&quick.1).spawn().unwrap();
        done.wait().unwrap();
        assert!(wait_gone(done.id(), Duration::from_secs(1)));
        let mut running = std::process::Command::new(slow.0).args(&slow.1).spawn().unwrap();
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
            ("LD_LIBRARY_PATH", "/opt/sk/resources/backend/_internal:/usr/lib/x"),
            ("LD_LIBRARY_PATH_ORIG", "/usr/lib/x"),
            ("DYLD_LIBRARY_PATH", "/opt/sk/resources/backend/_internal"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let got = launch_env(&env, Some("/opt/sk/resources/backend/_internal"));
        let want: BTreeMap<String, String> =
            [("PATH", "/usr/bin"), ("LD_LIBRARY_PATH", "/usr/lib/x")].into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        assert_eq!(got, want);
        let mine: BTreeMap<String, String> = [("LD_LIBRARY_PATH".to_owned(), "/usr/lib/mine".to_owned())].into();
        assert_eq!(launch_env(&mine, Some("/opt/sk")), mine);
    }

    #[test]
    fn a_bad_plan_says_why_and_touches_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = tree(tmp.path());
        std::fs::remove_file(plan.new.join(EXE)).unwrap();
        let file = write_plan(&plan, &tmp.path().join("update-0.3.4.json"), &[]);
        let mut out = Vec::new();
        assert_eq!(main_with(&[file.to_string_lossy().into_owned()], &mut out, false), 2);
        let said: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(said["ok"], false);
        assert!(said["error"].as_str().unwrap().contains("incomplete"));
        assert!(read(&tmp.path().join("update-0.3.4.log")).contains("not started: the new version is incomplete"));
        assert_eq!(read(&plan.app.join(EXE)), "0.3.3");
        assert_eq!(main_with(&[], &mut Vec::new(), false), 2);
    }
}
