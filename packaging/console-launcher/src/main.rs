//! SpaceKnife.com, the console entry of the Windows app (as devenv.com is to devenv.exe).
//!
//! The command processor and PowerShell try `.com` before `.exe`, so `SpaceKnife …` typed in a
//! terminal, in the app's folder or with it on the PATH, lands here, as it did with 0.3.4.
//!
//! - Headless mode, as 0.3.4's app took it (`--headless`, `--listen <address>`, `--yes`,
//!   `--tls-cert`, `--tls-key`, `--help`, anywhere among the arguments): it runs
//!   `spaceknife-cli.exe` next to it with the arguments as typed (their quoting unchanged) and the
//!   same standard handles, and waits. The terminal waits too, the Y/N question is asked there,
//!   Ctrl+C reaches spaceknife-cli (this program ignores it) and the exit code is spaceknife-cli's.
//!   spaceknife-cli reads 0.3.4's form wherever the options are (crates/sk-server/src/cli/
//!   headless.rs); `--help` / `-h` alone, which 0.3.4 answered with headless mode's options, goes
//!   on as `--headless --help`. spaceknife-cli's first argument is the name this program was
//!   started by, so its texts say `SpaceKnife --help`.
//! - Anything else starts the windowed app (`SpaceKnife.exe`, this program's name with `.exe`)
//!   with the same arguments and returns at once, as any windowed program started from a terminal.

#![cfg_attr(not(windows), allow(dead_code))]

/// The options of 0.3.4's app that start (or belong to) headless mode, `--name` and `--name=…`
/// (spaceknife-cli's `APP_OPTIONS`).
const HEADLESS_OPTIONS: [&str; 5] = ["--headless", "--listen", "--yes", "--tls-cert", "--tls-key"];
const HELP_OPTIONS: [&str; 2] = ["--help", "-h"];

/// Where a command line goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// spaceknife-cli.exe; `help_only`: only `--help` / `-h` of headless mode's options, so
    /// `--headless` goes in front
    Cli { help_only: bool },
    /// the windowed app
    App,
}

/// `--name=value` → `--name`; anything else unchanged.
fn option_name(arg: &str) -> &str {
    match arg.strip_prefix("--").and_then(|rest| rest.split_once('=')) {
        Some((name, _)) => &arg[..2 + name.len()],
        None => arg,
    }
}

/// Where the arguments (after the program) go: what 0.3.4's app did with them.
pub fn route<S: AsRef<str>>(args: &[S]) -> Route {
    let names: Vec<&str> = args.iter().map(|a| option_name(a.as_ref())).collect();
    if names.iter().any(|n| HEADLESS_OPTIONS.contains(n)) {
        Route::Cli { help_only: false }
    } else if names.iter().any(|n| HELP_OPTIONS.contains(n)) {
        Route::Cli { help_only: true }
    } else {
        Route::App
    }
}

/// A command line split as Windows splits its program from its arguments (argv[0]: up to the
/// closing quote when it starts with one, else up to the first blank): (the program as typed, the
/// arguments as typed).
pub fn split_command_line(line: &[u16]) -> (&[u16], &[u16]) {
    const QUOTE: u16 = b'"' as u16;
    let blank = |c: u16| c == b' ' as u16 || c == b'\t' as u16;
    let mut i = 0;
    if line.first() == Some(&QUOTE) {
        i = 1;
        while i < line.len() && line[i] != QUOTE {
            i += 1;
        }
        i = (i + 1).min(line.len());
    } else {
        while i < line.len() && !blank(line[i]) {
            i += 1;
        }
    }
    let program = &line[..i];
    while i < line.len() && blank(line[i]) {
        i += 1;
    }
    (program, &line[i..])
}

/// The command line for the program it starts: `program` (as this one was typed), `--headless`
/// when only the help was asked for, then the arguments as typed.
pub fn command_line(program: &[u16], route: Route, tail: &[u16]) -> Vec<u16> {
    let mut out: Vec<u16> = program.to_vec();
    if out.is_empty() {
        out.extend("SpaceKnife".encode_utf16());
    }
    if route == (Route::Cli { help_only: true }) {
        out.extend(" --headless".encode_utf16());
    }
    if !tail.is_empty() {
        out.push(b' ' as u16);
        out.extend_from_slice(tail);
    }
    out
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::{null, null_mut};

    use super::{Route, command_line, route, split_command_line};

    type Handle = *mut c_void;

    #[repr(C)]
    struct StartupInfoW {
        cb: u32,
        reserved: *mut u16,
        desktop: *mut u16,
        title: *mut u16,
        x: u32,
        y: u32,
        x_size: u32,
        y_size: u32,
        x_count_chars: u32,
        y_count_chars: u32,
        fill_attribute: u32,
        flags: u32,
        show_window: u16,
        reserved2_size: u16,
        reserved2: *mut u8,
        std_input: Handle,
        std_output: Handle,
        std_error: Handle,
    }

    #[repr(C)]
    struct ProcessInformation {
        process: Handle,
        thread: Handle,
        process_id: u32,
        thread_id: u32,
    }

    const INFINITE: u32 = 0xFFFF_FFFF;
    const STD_HANDLES: [u32; 3] = [-10i32 as u32, -11i32 as u32, -12i32 as u32]; // input, output, error
    const STARTF_USESTDHANDLES: u32 = 0x100;
    const HANDLE_FLAG_INHERIT: u32 = 1;
    const INVALID_HANDLE: Handle = -1isize as Handle;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCommandLineW() -> *const u16;
        fn GetStdHandle(which: u32) -> Handle;
        fn SetHandleInformation(handle: Handle, mask: u32, flags: u32) -> i32;
        fn CreateProcessW(
            application: *const u16,
            command_line: *mut u16,
            process_attributes: *mut c_void,
            thread_attributes: *mut c_void,
            inherit_handles: i32,
            creation_flags: u32,
            environment: *mut c_void,
            current_directory: *const u16,
            startup_info: *mut StartupInfoW,
            process_information: *mut ProcessInformation,
        ) -> i32;
        fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
        fn GetExitCodeProcess(process: Handle, exit_code: *mut u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain([0]).collect()
    }

    fn missing(target: &Path) -> i32 {
        eprintln!(
            "SpaceKnife: {} is missing (SpaceKnife.com must sit next to it)",
            target.display()
        );
        eprintln!(
            "SpaceKnife：找不到 {}（SpaceKnife.com 要和它放在同一个文件夹里）",
            target.display()
        );
        1
    }

    pub fn run() -> i32 {
        let me = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("SpaceKnife: can't find this program's folder: {e}");
                return 1;
            }
        };
        let args: Vec<String> = std::env::args_os()
            .skip(1)
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let way = route(&args);
        let target = match way {
            Route::Cli { .. } => me.with_file_name("spaceknife-cli.exe"),
            Route::App => me.with_extension("exe"),
        };
        if target == me || !target.is_file() {
            return missing(&target);
        }
        // SAFETY: the process's command line, NUL-terminated, alive for the whole process.
        let line = unsafe {
            let p = GetCommandLineW();
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            std::slice::from_raw_parts(p, n)
        };
        let (program, tail) = split_command_line(line);
        let mut command = command_line(program, way, tail);
        command.push(0);
        let application = wide(&target);
        let cli = way != Route::App;
        // SAFETY: zeroed plain-data structures with their sizes set, buffers NUL-terminated.
        unsafe {
            let mut startup: StartupInfoW = std::mem::zeroed();
            startup.cb = size_of::<StartupInfoW>() as u32;
            if cli {
                // spaceknife-cli gets this program's standard handles (the console or what they
                // are redirected to); a console it shares, so Ctrl+C reaches it there
                let handles = STD_HANDLES.map(|which| GetStdHandle(which));
                for &h in &handles {
                    if !h.is_null() && h != INVALID_HANDLE {
                        SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
                    }
                }
                startup.flags = STARTF_USESTDHANDLES;
                [startup.std_input, startup.std_output, startup.std_error] = handles;
            }
            let mut info: ProcessInformation = std::mem::zeroed();
            let ok = CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                null_mut(),
                null_mut(),
                cli as i32,
                0,
                null_mut(),
                null(),
                &mut startup,
                &mut info,
            );
            if ok == 0 {
                eprintln!(
                    "SpaceKnife: can't start {}: {}",
                    target.display(),
                    std::io::Error::last_os_error()
                );
                return 1;
            }
            CloseHandle(info.thread);
            if !cli {
                // the window is on its own; the terminal is free again
                CloseHandle(info.process);
                return 0;
            }
            // Ctrl+C is spaceknife-cli's (it stops cleanly); ignored here only after the start,
            // so that spaceknife-cli doesn't inherit "ignore Ctrl+C"
            SetConsoleCtrlHandler(None, 1);
            WaitForSingleObject(info.process, INFINITE);
            let mut code: u32 = 1;
            GetExitCodeProcess(info.process, &mut code);
            CloseHandle(info.process);
            code as i32
        }
    }
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("SpaceKnife.com is for Windows only");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn split(s: &str) -> (String, String) {
        let line = w(s);
        let (program, tail) = split_command_line(&line);
        (
            String::from_utf16(program).unwrap(),
            String::from_utf16(tail).unwrap(),
        )
    }

    #[test]
    fn keeps_the_arguments_as_typed() {
        assert_eq!(
            split(r#""C:\Program Files\SpaceKnife\SpaceKnife.com" --listen "0.0.0.0:8375" --yes"#),
            (
                r#""C:\Program Files\SpaceKnife\SpaceKnife.com""#.into(),
                r#"--listen "0.0.0.0:8375" --yes"#.into()
            )
        );
        assert_eq!(
            split("SpaceKnife  --headless"),
            ("SpaceKnife".into(), "--headless".into())
        );
        assert_eq!(
            split("SpaceKnife.com"),
            ("SpaceKnife.com".into(), "".into())
        );
        assert_eq!(
            split(r#""SpaceKnife""#),
            (r#""SpaceKnife""#.into(), "".into())
        );
        assert_eq!(
            split("SpaceKnife\t--listen\t::"),
            ("SpaceKnife".into(), "--listen\t::".into())
        );
        assert_eq!(split(""), ("".into(), "".into()));
    }

    #[test]
    fn routes_as_0_3_4_did() {
        let cli = Route::Cli { help_only: false };
        assert_eq!(route(&["--headless"]), cli);
        assert_eq!(route(&["--listen", "0.0.0.0"]), cli);
        assert_eq!(route(&["--listen=[::]:9000"]), cli);
        assert_eq!(route(&["--no-sandbox", "--yes"]), cli);
        assert_eq!(route(&["--tls-cert=c.pem"]), cli);
        assert_eq!(route(&["--tls-key", "k.pem", "--help"]), cli);
        assert_eq!(route(&["--help"]), Route::Cli { help_only: true });
        assert_eq!(route(&["-h"]), Route::Cli { help_only: true });
        // the window
        assert_eq!(route::<&str>(&[]), Route::App);
        assert_eq!(route(&["--updated"]), Route::App);
        assert_eq!(route(&["--listening"]), Route::App);
        assert_eq!(route(&["C:\\--headless"]), Route::App);
    }

    #[test]
    fn command_lines() {
        let line = |program: &str, route: Route, tail: &str| {
            String::from_utf16(&command_line(&w(program), route, &w(tail))).unwrap()
        };
        assert_eq!(
            line(
                "SpaceKnife",
                Route::Cli { help_only: false },
                r#"--listen "a b""#
            ),
            r#"SpaceKnife --listen "a b""#
        );
        assert_eq!(
            line(
                r#""C:\SK\SpaceKnife.com""#,
                Route::Cli { help_only: true },
                "-h"
            ),
            r#""C:\SK\SpaceKnife.com" --headless -h"#
        );
        assert_eq!(line("SpaceKnife", Route::App, ""), "SpaceKnife");
        assert_eq!(line("", Route::App, "--x"), "SpaceKnife --x");
    }
}
