//! Starting the helper process (Python `helper/launch.py`): a plain subprocess, or elevated through
//! the UAC prompt (Windows) or polkit's `pkexec` (Linux). The helper is the console program
//! `spaceknife-cli` next to the app, run with the `helper` subcommand ([`helper_command`]); it
//! dispatches that to [`super::run_main`]. The window program is never the one elevated.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::errors::HelperError;
use crate::i18n::t;
use crate::tr;

pub const ERROR_CANCELLED: i32 = 1223;
pub const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
pub const SEE_MASK_NOASYNC: u32 = 0x0000_0100;
pub const SEE_MASK_FLAG_NO_UI: u32 = 0x0000_0400;
pub const SEE_MASK_NO_CONSOLE: u32 = 0x0000_8000;
pub const SW_HIDE: i32 = 0;
pub const PKEXEC_PATHS: [&str; 2] = ["/usr/bin/pkexec", "/bin/pkexec"];
pub const PKEXEC_DISMISSED: i32 = 126; // the user closed the password dialog
pub const PKEXEC_REFUSED: i32 = 127; // not authorized, no authentication agent, or the program can't run
/// The subcommand of the app binary that runs [`super::run_main`].
pub const HELPER_SUBCOMMAND: &str = "helper";

/// A started helper process.
pub trait HelperProcess: Send {
    fn pid(&self) -> u32;
    /// The exit code once it has exited.
    fn poll(&mut self) -> Option<i32>;
    /// Wait up to `timeout`; the exit code if it has exited.
    fn wait(&mut self, timeout: Duration) -> Option<i32>;
    /// Kill it (when it is still ours to kill).
    fn terminate(&mut self);
    /// Release the OS handle (after reaping it if it has exited).
    fn close(&mut self);
    /// What the exit code of a launcher (pkexec) means; None: the helper itself exited.
    fn exit_error(&mut self, _code: i32) -> Option<HelperError> {
        None
    }
}

/// A child started with [`std::process::Command`].
pub struct ChildProcess {
    child: Child,
    code: Option<i32>,
}

impl ChildProcess {
    pub fn new(child: Child) -> Self {
        Self { child, code: None }
    }
}

impl HelperProcess for ChildProcess {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn poll(&mut self) -> Option<i32> {
        if self.code.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            self.code = Some(status.code().unwrap_or(-1));
        }
        self.code
    }

    fn wait(&mut self, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(code) = self.poll() {
                return Some(code);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
    }

    fn terminate(&mut self) {
        if self.poll().is_none() {
            let _ = self.child.kill();
        }
    }

    fn close(&mut self) {
        self.poll(); // reap if it has exited
    }
}

/// `pkexec helper …`: pkexec asks polkit, then replaces itself with the helper (the same pid, now
/// root). Its stderr is read on a thread: the last lines say why pkexec gave up, and the helper's
/// log goes to the backend's log.
pub struct PkexecProcess {
    inner: ChildProcess,
    tail: Arc<Mutex<VecDeque<String>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl PkexecProcess {
    pub fn new(mut child: Child) -> Self {
        let tail = Arc::new(Mutex::new(VecDeque::with_capacity(20)));
        let reader = child.stderr.take().map(|stderr| {
            let tail = tail.clone();
            std::thread::Builder::new()
                .name("sk-helper-stderr".into())
                .spawn(move || read_tail(stderr, &tail))
                .expect("spawn the stderr reader")
        });
        Self {
            inner: ChildProcess::new(child),
            tail,
            reader,
        }
    }
}

fn read_tail(stream: impl io::Read, tail: &Mutex<VecDeque<String>>) {
    use std::io::BufRead as _;
    for line in io::BufReader::new(stream)
        .split(b'\n')
        .map_while(Result::ok)
    {
        let line = String::from_utf8_lossy(&line).trim_end().to_owned();
        if line.is_empty() {
            continue;
        }
        tracing::debug!("helper: {line}");
        let mut t = tail.lock();
        if t.len() == 20 {
            t.pop_front();
        }
        t.push_back(line);
    }
}

impl HelperProcess for PkexecProcess {
    fn pid(&self) -> u32 {
        self.inner.pid()
    }
    fn poll(&mut self) -> Option<i32> {
        self.inner.poll()
    }
    fn wait(&mut self, timeout: Duration) -> Option<i32> {
        self.inner.wait(timeout)
    }
    fn terminate(&mut self) {
        // root by now: not ours to kill (the error is ignored)
        self.inner.terminate()
    }
    fn close(&mut self) {
        self.inner.close()
    }
    fn exit_error(&mut self, code: i32) -> Option<HelperError> {
        if let Some(reader) = self.reader.take() {
            // it has exited: its last words are on their way
            let deadline = Instant::now() + Duration::from_secs(1);
            while !reader.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
        let lines: Vec<String> = self.tail.lock().iter().cloned().collect();
        pkexec_error(code, &lines)
    }
}

/// What pkexec's exit code means, or None (the helper itself exited).
pub fn pkexec_error(code: i32, stderr: &[String]) -> Option<HelperError> {
    if code == PKEXEC_DISMISSED {
        return Some(HelperError::denied());
    }
    if code != PKEXEC_REFUSED {
        return None;
    }
    let text = stderr.join(" ").trim().to_owned();
    if text.to_lowercase().contains("authentication agent") {
        return Some(HelperError::unavailable(
            t("没有可以询问管理员密码的身份验证代理（比如在 SSH 会话里，或没有图形桌面）"),
            "no_agent",
        ));
    }
    let reason = if text.is_empty() {
        code.to_string()
    } else {
        text
    };
    Some(HelperError::unavailable(
        tr!("系统没有给予管理员授权：{reason}", reason = reason),
        "not_authorized",
    ))
}

/// pkexec at a fixed absolute path (never looked up in PATH), from `candidates`.
pub fn pkexec_path(candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find(|p| is_executable(p))
        .map(|p| (*p).to_owned())
}

fn is_executable(path: &str) -> bool {
    #[cfg(unix)]
    {
        let c = std::ffi::CString::new(path).unwrap_or_default();
        // SAFETY: valid C string.
        unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// An AppImage runs from a FUSE mount only its user can read: root can't start the helper.
pub fn in_appimage(executable: &str) -> bool {
    std::env::var_os("APPIMAGE").is_some_and(|v| !v.is_empty()) || executable.contains("/.mount_")
}

/// `pkexec argv…` (Linux): the desktop's polkit agent shows the password dialog while the helper
/// is being started; the caller waits for it to connect. Its own session, so pkexec never falls
/// back to asking on the backend's terminal. `pkexec` is the path to use (from [`pkexec_path`]).
pub fn start_pkexec(
    argv: &[OsString],
    pkexec: Option<&str>,
) -> Result<Box<dyn HelperProcess>, HelperError> {
    let Some(pkexec) = pkexec else {
        return Err(HelperError::unavailable(
            t("找不到 pkexec：请求管理员授权需要 polkit"),
            "not_supported",
        ));
    };
    let exe = argv
        .first()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_default();
    if in_appimage(&exe) {
        return Err(HelperError::unavailable(
            t("AppImage 版本不能以管理员身份运行助手；请改用 deb 或 rpm 安装包"),
            "not_supported",
        ));
    }
    if !std::path::Path::new(&exe).is_absolute() || !exe.starts_with('/') {
        return Err(HelperError::unavailable(
            tr!("助手的路径不是绝对路径：{path}", path = exe),
            "not_supported",
        ));
    }
    let mut cmd = Command::new(pkexec);
    cmd.args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let child = cmd.spawn().map_err(|e| {
        HelperError::unavailable(
            tr!("无法启动 pkexec：{reason}", reason = e.to_string()),
            "exited",
        )
    })?;
    Ok(Box::new(PkexecProcess::new(child)))
}

/// The console program every package puts next to the app's own executable (Tauri's `externalBin`,
/// the portable archives, the Electron shell's backend): the helper runs as it.
pub const CLI_FILE: &str = if cfg!(windows) {
    "spaceknife-cli.exe"
} else {
    "spaceknife-cli"
};

/// Whether `name` is [`CLI_FILE`] (Windows and macOS compare without case, as their file systems
/// usually do).
fn is_cli_file(name: &std::ffi::OsStr) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        name.to_str()
            .is_some_and(|n| n.eq_ignore_ascii_case(CLI_FILE))
    } else {
        name == CLI_FILE
    }
}

/// The helper of the running program: [`helper_command_for`] `current_exe`.
pub fn helper_command() -> io::Result<Vec<OsString>> {
    helper_command_for(&std::env::current_exe()?)
}

/// `[<spaceknife-cli>, "helper"]` for a program at `exe`: `exe` itself when it is the console
/// program, else the console program next to it (absolute, never looked up in PATH). Never the
/// window program: the administrator process loads nothing of the interface (WebView2, WebKitGTK,
/// GTK and their libraries). An error when there is no console program to run.
pub fn helper_command_for(exe: &std::path::Path) -> io::Result<Vec<OsString>> {
    let cli = if exe.file_name().is_some_and(is_cli_file) {
        exe.to_path_buf()
    } else {
        exe.parent()
            .map(|dir| dir.join(CLI_FILE))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, CLI_FILE))?
    };
    if !cli.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            tr!(
                "找不到扫描助手程序：{path}",
                path = cli.display().to_string()
            ),
        ));
    }
    Ok(vec![cli.into_os_string(), HELPER_SUBCOMMAND.into()])
}

/// Unelevated helper: stdin / stdout null, stderr shared (the helper's log).
pub fn start_subprocess(argv: &[OsString]) -> Result<Box<dyn HelperProcess>, HelperError> {
    let Some((exe, args)) = argv.split_first() else {
        return Err(HelperError::unavailable("empty helper command", "exited"));
    };
    let mut cmd = Command::new(exe);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: the backend may have no console to share
    }
    let child = cmd.spawn().map_err(|e| {
        HelperError::unavailable(
            tr!("无法启动扫描助手：{reason}", reason = e.to_string()),
            "exited",
        )
    })?;
    Ok(Box::new(ChildProcess::new(child)))
}

/// Python's `subprocess.list2cmdline`: the MS C runtime's quoting rules.
pub fn list2cmdline<S: AsRef<str>>(args: &[S]) -> String {
    let mut out = String::new();
    for (i, arg) in args.iter().enumerate() {
        let arg = arg.as_ref();
        if i > 0 {
            out.push(' ');
        }
        let quote = arg.is_empty() || arg.contains([' ', '\t']);
        if quote {
            out.push('"');
        }
        let mut backslashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    out.push('"');
                    backslashes = 0;
                }
                _ => {
                    out.extend(std::iter::repeat_n('\\', backslashes));
                    backslashes = 0;
                    out.push(c);
                }
            }
        }
        out.extend(std::iter::repeat_n('\\', backslashes));
        if quote {
            out.extend(std::iter::repeat_n('\\', backslashes));
            out.push('"');
        }
    }
    out
}

#[cfg(windows)]
pub use super::win32::ShellExecuteParams;

/// ShellExecuteExW arguments for `argv` behind the UAC prompt, hidden and without UI.
#[cfg(windows)]
pub fn elevated_params(argv: &[OsString]) -> ShellExecuteParams {
    let args: Vec<String> = argv
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    ShellExecuteParams {
        verb: "runas".into(),
        file: args.first().cloned().unwrap_or_default(),
        parameters: list2cmdline(&args[1.min(args.len())..]),
        directory: None,
        show: SW_HIDE,
        mask: SEE_MASK_NOCLOSEPROCESS
            | SEE_MASK_NO_CONSOLE
            | SEE_MASK_FLAG_NO_UI
            | SEE_MASK_NOASYNC,
    }
}

/// How the UAC prompt is shown: the real `ShellExecuteExW`, or a stand-in (tests).
#[cfg(windows)]
pub type ShellExecuteFn =
    Arc<dyn Fn(&ShellExecuteParams) -> io::Result<Box<dyn HelperProcess>> + Send + Sync>;

#[cfg(windows)]
struct UacProcess(super::win32::ProcessHandle);

#[cfg(windows)]
impl HelperProcess for UacProcess {
    fn pid(&self) -> u32 {
        self.0.pid()
    }
    fn poll(&mut self) -> Option<i32> {
        self.0.poll()
    }
    fn wait(&mut self, timeout: Duration) -> Option<i32> {
        self.0.wait(timeout)
    }
    fn terminate(&mut self) {
        self.0.terminate()
    }
    fn close(&mut self) {
        self.0.close()
    }
}

#[cfg(windows)]
impl Drop for UacProcess {
    fn drop(&mut self) {
        self.0.close()
    }
}

/// The real ShellExecuteExW (shows the UAC prompt).
#[cfg(windows)]
pub fn real_shell_execute() -> ShellExecuteFn {
    Arc::new(|params| {
        Ok(Box::new(UacProcess(super::win32::shell_execute(params)?)) as Box<dyn HelperProcess>)
    })
}

/// How an elevated helper is started.
#[derive(Clone)]
pub struct Elevator {
    #[cfg(windows)]
    pub shell_execute: ShellExecuteFn,
    /// pkexec candidates (Linux).
    pub pkexec_paths: Vec<String>,
}

impl Default for Elevator {
    fn default() -> Self {
        Self {
            #[cfg(windows)]
            shell_execute: real_shell_execute(),
            pkexec_paths: PKEXEC_PATHS.iter().map(|s| (*s).to_owned()).collect(),
        }
    }
}

/// Start `argv` elevated: the UAC prompt (Windows) or pkexec (Linux), waiting at most `timeout`
/// for the user to answer the UAC prompt.
///
/// ShellExecuteExW blocks until the user answers, so it runs on its own (COM-initialised)
/// thread. If the caller gives up first, a late process handle is released; the helper then finds
/// the pipe gone and exits by itself. pkexec returns at once; its password dialog stays open
/// while the caller waits for the helper to connect.
pub fn start_elevated(
    argv: &[OsString],
    elevator: &Elevator,
    timeout: Duration,
) -> Result<Box<dyn HelperProcess>, HelperError> {
    #[cfg(target_os = "linux")]
    {
        let _ = timeout;
        let candidates: Vec<&str> = elevator.pkexec_paths.iter().map(String::as_str).collect();
        start_pkexec(argv, pkexec_path(&candidates).as_deref())
    }
    #[cfg(windows)]
    {
        let params = elevated_params(argv);
        let run = elevator.shell_execute.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("sk-helper-uac".into())
            .spawn(move || {
                let result = run(&params);
                if let Err(std::sync::mpsc::TrySendError::Disconnected(Ok(mut late))) =
                    tx.try_send(result)
                {
                    late.close(); // the caller gave up meanwhile
                }
            })
            .map_err(|e| HelperError::unavailable(e.to_string(), "exited"))?;
        match rx.recv_timeout(timeout) {
            Ok(Ok(proc)) => Ok(proc),
            Ok(Err(err)) if err.raw_os_error() == Some(ERROR_CANCELLED) => {
                Err(HelperError::denied())
            }
            Ok(Err(err)) if err.kind() == io::ErrorKind::PermissionDenied => {
                Err(HelperError::unavailable(
                    tr!("无法启动扫描助手：{reason}", reason = err.to_string()),
                    "unavailable",
                ))
            }
            Ok(Err(err)) => Err(HelperError::unavailable(
                tr!("无法启动扫描助手：{reason}", reason = err.to_string()),
                "exited",
            )),
            Err(_) => Err(HelperError::unavailable(t("等待管理员授权超时"), "timeout")),
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (argv, elevator, timeout);
        Err(HelperError::unavailable(
            t("当前平台暂不支持提权助手"),
            "not_supported",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmdline_quoting() {
        assert_eq!(
            list2cmdline(&["-I", "-m", "x", "--pipe", r"\\.\pipe\x y"]),
            r#"-I -m x --pipe "\\.\pipe\x y""#
        );
        assert_eq!(
            list2cmdline(&["a\"b", "", r"c\", r"d e\"]),
            r#"a\"b "" c\ "d e\\""#
        );
        assert_eq!(list2cmdline::<&str>(&[]), "");
    }

    #[cfg(windows)]
    #[test]
    fn shell_execute_params() {
        let argv: Vec<OsString> = [
            r"C:\Program Files\SK\spaceknife.exe",
            "helper",
            "--pipe",
            r"\\.\pipe\x y",
        ]
        .map(OsString::from)
        .to_vec();
        let p = elevated_params(&argv);
        assert_eq!(p.verb, "runas");
        assert_eq!(p.file, r"C:\Program Files\SK\spaceknife.exe");
        assert_eq!(p.parameters, r#"helper --pipe "\\.\pipe\x y""#);
        assert_eq!(p.show, 0);
        assert_eq!(p.mask, 0x40 | 0x8000 | 0x400 | 0x100);
        assert_eq!(p.directory, None);
    }

    #[test]
    fn never_the_window_program() {
        // this test binary is not the console program: the helper is never it
        let exe = std::env::current_exe().unwrap();
        if let Ok(cmd) = helper_command() {
            assert_ne!(cmd[0], exe.as_os_str());
            assert!(is_cli_file(
                std::path::Path::new(&cmd[0]).file_name().unwrap()
            ));
            assert_eq!(cmd[1], "helper");
        }
    }

    #[test]
    fn the_helper_is_the_console_program_next_to_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join(if cfg!(windows) {
            "SpaceKnife.exe"
        } else {
            "spaceknife"
        });
        std::fs::write(&app, b"").unwrap();
        // no console program: no helper at all (never the window program instead)
        let err = helper_command_for(&app).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains(CLI_FILE), "{err}");
        let cli = dir.path().join(CLI_FILE);
        std::fs::write(&cli, b"").unwrap();
        assert_eq!(
            helper_command_for(&app).unwrap(),
            vec![cli.clone().into_os_string(), OsString::from("helper")]
        );
        // the console program itself (the Electron shell runs the backend as `spaceknife-cli serve`)
        assert_eq!(helper_command_for(&cli).unwrap()[0], cli.as_os_str());
        // a folder of that name is not a program
        let odd = tempfile::tempdir().unwrap();
        std::fs::create_dir(odd.path().join(CLI_FILE)).unwrap();
        assert!(helper_command_for(&odd.path().join("spaceknife")).is_err());
        if cfg!(windows) {
            let upper = dir.path().join("SPACEKNIFE-CLI.EXE");
            assert_eq!(helper_command_for(&upper).unwrap()[0], upper.as_os_str());
        } else if cfg!(target_os = "linux") {
            assert!(!is_cli_file(std::ffi::OsStr::new("SpaceKnife-cli")));
        }
    }

    const AGENT_MISSING: &str =
        "Error executing command as another user: No authentication agent found.";

    #[test]
    fn pkexec_exit_codes() {
        assert!(pkexec_error(126, &[]).unwrap().is_denied());
        let agent = pkexec_error(127, &[AGENT_MISSING.into()]).unwrap();
        assert!(agent.is_unavailable() && agent.code == "no_agent");
        let refused = pkexec_error(
            127,
            &[
                "Error executing command as another user: Not authorized".into(),
                "".into(),
                "This incident has been reported.".into(),
            ],
        )
        .unwrap();
        assert_eq!(refused.code, "not_authorized");
        assert!(refused.message.contains("Not authorized"));
        assert!(pkexec_error(0, &[]).is_none() && pkexec_error(3, &["x".into()]).is_none());
    }

    #[test]
    fn appimage_cannot_elevate() {
        // (APPIMAGE is not set in the test environment)
        if std::env::var_os("APPIMAGE").is_none() {
            assert!(!in_appimage("/opt/SpaceKnife/spaceknife"));
        }
        assert!(in_appimage("/tmp/.mount_SpaceKAbC123/spaceknife"));
    }

    #[test]
    fn start_pkexec_refuses_before_asking() {
        let argv: Vec<OsString> = vec!["/opt/sk/spaceknife".into(), "helper".into()];
        let err = start_pkexec(&argv, None).err().unwrap();
        assert_eq!(err.code, "not_supported");
        assert!(err.message.contains("pkexec"));
        let err = start_pkexec(&["/tmp/.mount_X/sk".into()], Some("/nonexistent/pkexec"))
            .err()
            .unwrap();
        assert!(err.code == "not_supported" && err.message.contains("AppImage"));
        let err = start_pkexec(&["spaceknife".into()], Some("/nonexistent/pkexec"))
            .err()
            .unwrap();
        assert_eq!(err.code, "not_supported"); // pkexec needs an absolute path
    }

    #[test]
    fn stderr_tail_keeps_the_last_lines() {
        let tail = Mutex::new(VecDeque::new());
        let text: String = (0..30).map(|i| format!("line {i}\n\n")).collect();
        read_tail(text.as_bytes(), &tail);
        let t = tail.lock();
        assert_eq!(t.len(), 20);
        assert_eq!(t.front().unwrap(), "line 10");
        assert_eq!(t.back().unwrap(), "line 29");
    }
}
