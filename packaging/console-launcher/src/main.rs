//! SpaceKnife.com, the console entry of the Windows app (as devenv.com is to devenv.exe). The command
//! processor and PowerShell try `.com` before `.exe`, so `SpaceKnife …` typed in a terminal lands
//! here; it ends with the exit code of the program it starts.
//!
//! - 0.3.4's headless launch forms (`--headless`, `--listen <address>`, `--help`, anywhere among
//!   the arguments; plan §4 gap 1): headless mode runs right here as `spaceknife-cli headless …`
//!   (spaceknife-cli.exe sits next to this program), a console program sharing this terminal: it
//!   asks its Y/N question there, prints where to open the interface, and stops on Ctrl+C. The
//!   arguments are read as the shells read them (apps/desktop/src-tauri/src/cli.rs,
//!   apps/electron-shell/src/main/cli.ts; the same cases below).
//! - Anything else: SpaceKnife.exe, a windowed program a terminal doesn't wait for, starts with the
//!   same arguments as typed, and this program waits for it (as 0.3.4's did). In a terminal it gets
//!   no standard handles (the Tauri shell then writes to this console); redirected (`> log.txt`, a
//!   pipe) it gets this program's handles, and Ctrl+C here ends it.

#![cfg_attr(not(windows), allow(dead_code))]

use std::ffi::OsString;

/// Options with a value, as spaceknife-cli headless takes them.
const VALUE_OPTIONS: [&str; 4] = ["--listen", "--tls-cert", "--tls-key", "--ui"];

/// The arguments for spaceknife-cli (`headless …`) when these are a headless launch, else None
/// (the window). Only the headless options are passed on (0.3.4 ignored the rest); a value is the
/// next argument unless that is missing or another option (`--…`), or follows "=".
pub fn headless_args(args: &[OsString]) -> Option<Vec<OsString>> {
    let mut headless = false;
    let mut out: Vec<OsString> = vec!["headless".into()];
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        // option names are ASCII: an argument that isn't Unicode is no option
        let Some(text) = arg.to_str() else { continue };
        let (name, inline) = match text.strip_prefix("--").and_then(|_| text.split_once('=')) {
            Some((n, v)) => (n, Some(v)),
            None => (text, None),
        };
        match name {
            "--headless" if inline.is_none() => headless = true,
            "--help" | "-h" => {
                headless = true;
                out.push("--help".into());
            }
            "--yes" if inline.is_none() => out.push("--yes".into()),
            _ if VALUE_OPTIONS.contains(&name) => {
                if name == "--listen" {
                    headless = true;
                }
                let value: Option<OsString> = match inline {
                    Some(v) => Some(v.into()),
                    None => match args.get(i) {
                        Some(next) if !next.to_string_lossy().starts_with("--") => {
                            i += 1;
                            Some(next.clone())
                        }
                        _ => None,
                    },
                };
                // without a value spaceknife-cli says the value is missing
                let mut opt = OsString::from(name);
                if let Some(v) = value {
                    opt.push("=");
                    opt.push(v);
                }
                out.push(opt);
            }
            _ => {}
        }
    }
    headless.then_some(out)
}

#[cfg(windows)]
mod win {
    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicPtr, Ordering};

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
    const FILE_TYPE_CHAR: u32 = 2;
    const STARTF_USESTDHANDLES: u32 = 0x100;
    const HANDLE_FLAG_INHERIT: u32 = 1;
    const INVALID_HANDLE: Handle = -1isize as Handle;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCommandLineW() -> *const u16;
        fn GetStdHandle(which: u32) -> Handle;
        fn GetFileType(handle: Handle) -> u32;
        fn GetConsoleMode(handle: Handle, mode: *mut u32) -> i32;
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
        fn TerminateProcess(process: Handle, exit_code: u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    /// The app, for the Ctrl+C handler of the redirected case.
    static CHILD: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

    /// Ctrl+C, Ctrl+Break, the console closing: end the app (exit code 130, as after SIGINT).
    unsafe extern "system" fn stop_child(_event: u32) -> i32 {
        let child = CHILD.load(Ordering::SeqCst);
        if !child.is_null() {
            unsafe { TerminateProcess(child, 130) };
        }
        1
    }

    /// The arguments as typed, after the program name (Windows' rule for argv[0]: up to the closing
    /// quote when it starts with one, else up to the first blank), so their quoting reaches the app
    /// unchanged.
    pub fn arguments_tail(line: &[u16]) -> &[u16] {
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
        while i < line.len() && blank(line[i]) {
            i += 1;
        }
        &line[i..]
    }

    fn is_console(handle: Handle) -> bool {
        let mut mode = 0u32;
        !handle.is_null()
            && handle != INVALID_HANDLE
            && unsafe {
                GetFileType(handle) == FILE_TYPE_CHAR && GetConsoleMode(handle, &mut mode) != 0
            }
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().collect()
    }

    /// Headless mode in this terminal: spaceknife-cli, a console program, shares this console and
    /// these handles; Ctrl+C is its to handle (it stops the server and says so), this program waits.
    fn run_headless(me: &std::path::Path, args: Vec<OsString>) -> i32 {
        let cli = me.with_file_name("spaceknife-cli.exe");
        if !cli.is_file() {
            eprintln!(
                "SpaceKnife: {} is missing (headless mode runs it; it is installed next to this launcher)",
                cli.display()
            );
            return 1;
        }
        let mut child = match std::process::Command::new(&cli).args(&args).spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("SpaceKnife: can't start {}: {e}", cli.display());
                return 1;
            }
        };
        // after it started, so that it doesn't inherit "ignore Ctrl+C"
        unsafe { SetConsoleCtrlHandler(None, 1) };
        match child.wait() {
            Ok(status) => status.code().unwrap_or(1),
            Err(e) => {
                eprintln!("SpaceKnife: {e}");
                1
            }
        }
    }

    pub fn run() -> i32 {
        let me = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("SpaceKnife: can't find this program's folder: {e}");
                return 1;
            }
        };
        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        if let Some(headless) = super::headless_args(&args) {
            return run_headless(&me, headless);
        }
        let target = me.with_extension("exe");
        if target == me || !target.is_file() {
            eprintln!(
                "SpaceKnife: {} is missing (this launcher must sit next to it, named like it with .com)",
                target.display()
            );
            return 1;
        }
        let line = unsafe {
            let p = GetCommandLineW();
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            std::slice::from_raw_parts(p, n)
        };
        let mut command: Vec<u16> = Vec::new();
        command.push(b'"' as u16);
        command.extend(wide(target.as_os_str()));
        command.push(b'"' as u16);
        let tail = arguments_tail(line);
        if !tail.is_empty() {
            command.push(b' ' as u16);
            command.extend_from_slice(tail);
        }
        command.push(0);
        let mut application = wide(target.as_os_str());
        application.push(0);

        let handles = STD_HANDLES.map(|which| unsafe { GetStdHandle(which) });
        let terminal = handles.iter().all(|&h| is_console(h));
        let mut startup: StartupInfoW = unsafe { std::mem::zeroed() };
        startup.cb = size_of::<StartupInfoW>() as u32;
        if !terminal {
            for &h in &handles {
                if !h.is_null() && h != INVALID_HANDLE {
                    unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
                }
            }
            startup.flags = STARTF_USESTDHANDLES;
            [startup.std_input, startup.std_output, startup.std_error] = handles;
        }
        let mut info: ProcessInformation = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                null_mut(),
                null_mut(),
                (!terminal) as i32,
                0,
                null_mut(),
                null(),
                &mut startup,
                &mut info,
            )
        };
        if ok == 0 {
            eprintln!(
                "SpaceKnife: can't start {}: {}",
                target.display(),
                std::io::Error::last_os_error()
            );
            return 1;
        }
        unsafe {
            CloseHandle(info.thread);
            // after the app started, so that it doesn't inherit "ignore Ctrl+C"
            if terminal {
                SetConsoleCtrlHandler(None, 1); // Ctrl+C is the app's (attached to this console)
            } else {
                CHILD.store(info.process, Ordering::SeqCst);
                SetConsoleCtrlHandler(Some(stop_child), 1);
            }
            WaitForSingleObject(info.process, INFINITE);
            let mut code: u32 = 1;
            GetExitCodeProcess(info.process, &mut code);
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

    fn run(args: &[&str]) -> Option<Vec<String>> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        headless_args(&args).map(|v| {
            v.into_iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        })
    }

    #[test]
    fn starts_the_window_without_a_headless_option() {
        assert_eq!(run(&[]), None);
        assert_eq!(run(&["--updated"]), None);
        assert_eq!(run(&["--yes"]), None);
        assert_eq!(run(&["D:\\Games"]), None);
    }

    #[test]
    fn runs_headless_mode_for_0_3_4s_forms_anywhere_among_the_arguments() {
        assert_eq!(run(&["--headless"]).unwrap(), ["headless"]);
        assert_eq!(
            run(&["--yes", "--listen", "0.0.0.0:8375"]).unwrap(),
            ["headless", "--yes", "--listen=0.0.0.0:8375"]
        );
        assert_eq!(
            run(&[
                "--listen=:9000",
                "--tls-cert",
                "C:\\certs\\a b.pem",
                "--tls-key",
                "k.pem",
                "--x"
            ])
            .unwrap(),
            [
                "headless",
                "--listen=:9000",
                "--tls-cert=C:\\certs\\a b.pem",
                "--tls-key=k.pem"
            ]
        );
        assert_eq!(
            run(&["--listen", "--yes"]).unwrap(),
            ["headless", "--listen", "--yes"]
        );
        assert_eq!(run(&["-h"]).unwrap(), ["headless", "--help"]);
    }

    #[cfg(windows)]
    #[test]
    fn keeps_the_arguments_as_typed() {
        use super::win::arguments_tail;
        let tail = |s: &str| {
            let w: Vec<u16> = s.encode_utf16().collect();
            String::from_utf16(arguments_tail(&w)).unwrap()
        };
        assert_eq!(
            tail(r#""C:\Program Files\SpaceKnife\SpaceKnife.com" --updated "D:\a b""#),
            r#"--updated "D:\a b""#
        );
        assert_eq!(tail("SpaceKnife  --updated"), "--updated");
        assert_eq!(tail("SpaceKnife.com"), "");
        assert_eq!(tail(r#""SpaceKnife""#), "");
        assert_eq!(tail("SpaceKnife\t--x\t::"), "--x\t::");
    }
}
