//! SpaceKnife.com, the console entry of the Windows app (as devenv.com is to devenv.exe).
//!
//! SpaceKnife.exe is a windowed program: a terminal doesn't wait for it and keeps the keyboard, so
//! headless mode (`--headless`, `--listen`) couldn't ask its Y/N question there. This console program
//! sits next to it, starts it with the same arguments and waits. The command processor and PowerShell
//! try `.com` before `.exe`, so `SpaceKnife --headless` in a terminal lands here. It ends with the
//! app's exit code.
//!
//! In a terminal (standard input, output and error all the console) the app gets no standard
//! handles: Electron then attaches it to this console, so its output appears here and Ctrl+C reaches
//! it, and SPACEKNIFE_CONSOLE=1 tells it that the console's keyboard is free for its question
//! (apps/electron/src/main/console.ts). This program ignores Ctrl+C itself.
//!
//! Redirected (`> headless.log`, a pipe): the app gets this program's handles as they are and is not
//! attached to the console, so Ctrl+C here ends it (its backend then stops by itself).

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
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
        fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
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
        !handle.is_null() && handle != INVALID_HANDLE && unsafe { GetFileType(handle) == FILE_TYPE_CHAR && GetConsoleMode(handle, &mut mode) != 0 }
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().collect()
    }

    pub fn run() -> i32 {
        let me = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("SpaceKnife: can't find this program's folder: {e}");
                return 1;
            }
        };
        let target = me.with_extension("exe");
        if target == me || !target.is_file() {
            eprintln!("SpaceKnife: {} is missing (this launcher must sit next to it, named like it with .com)", target.display());
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
        if terminal {
            // no standard handles: Electron attaches the app to this console, which it may read from
            unsafe { std::env::set_var("SPACEKNIFE_CONSOLE", "1") };
        } else {
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
            eprintln!("SpaceKnife: can't start {}: {}", target.display(), std::io::Error::last_os_error());
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

#[cfg(all(test, windows))]
mod tests {
    use super::win::arguments_tail;

    fn tail(s: &str) -> String {
        let w: Vec<u16> = s.encode_utf16().collect();
        String::from_utf16(arguments_tail(&w)).unwrap()
    }

    #[test]
    fn keeps_the_arguments_as_typed() {
        assert_eq!(tail(r#""C:\Program Files\SpaceKnife\SpaceKnife.com" --listen "0.0.0.0:8375" --yes"#), r#"--listen "0.0.0.0:8375" --yes"#);
        assert_eq!(tail("SpaceKnife  --headless"), "--headless");
        assert_eq!(tail("SpaceKnife.com"), "");
        assert_eq!(tail(r#""SpaceKnife""#), "");
        assert_eq!(tail("SpaceKnife\t--listen\t::"), "--listen\t::");
    }
}
