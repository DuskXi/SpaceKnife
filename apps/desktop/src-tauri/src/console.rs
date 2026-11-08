//! The window's program started by SpaceKnife.com from a terminal (Windows;
//! packaging/console-launcher). The launcher waits for it, as 0.3.4's did, and when the terminal
//! is its console it passes no standard handles and sets SPACEKNIFE_CONSOLE=1. As 0.3.4's
//! SpaceKnife.exe (Electron, which takes its parent's console by itself) the program then writes
//! to that terminal, and Ctrl+C there ends it: here the way the window's close button does (the
//! scans stop, the backend writes what it keeps), where 0.3.4's was ended at once. Started any
//! other way (Explorer, a redirect, the update), and on other systems, nothing changes.

use std::ffi::OsStr;
use std::sync::OnceLock;

/// What SpaceKnife.com sets for the program when the terminal is the console.
pub const CONSOLE_VAR: &str = "SPACEKNIFE_CONSOLE";

/// The exit code after Ctrl+C, as after SIGINT (SpaceKnife.com passes it on).
pub const CTRL_C_EXIT: i32 = 130;

/// The app Ctrl+C ends (lib.rs setup).
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Whether the program takes the launcher's console: SPACEKNIFE_CONSOLE=1, and no output of its
/// own (the launcher passed none; a redirect keeps its handles).
pub fn wants_console(var: Option<&OsStr>, has_output: bool) -> bool {
    var.is_some_and(|v| v == "1") && !has_output
}

/// Take the launcher's console when it says so (main, before the window).
pub fn join_launchers_terminal() {
    #[cfg(windows)]
    win::join();
}

/// Ctrl+C in that console ends this app (lib.rs setup).
pub fn set_app(app: &tauri::AppHandle) {
    let _ = APP.set(app.clone());
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use super::{APP, CONSOLE_VAR, CTRL_C_EXIT, wants_console};

    type Handle = *mut c_void;

    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const FILE_TYPE_UNKNOWN: u32 = 0;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    const OPEN_EXISTING: u32 = 3;
    const CTRL_C_EVENT: u32 = 0;
    const CTRL_BREAK_EVENT: u32 = 1;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> Handle;
        fn SetStdHandle(which: u32, handle: Handle) -> i32;
        fn GetFileType(handle: Handle) -> u32;
        fn AttachConsole(process: u32) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut c_void,
            disposition: u32,
            flags: u32,
            template: Handle,
        ) -> Handle;
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    /// A standard handle that leads somewhere: not none, and a file, pipe or console (a value the
    /// process was never given is of no known type).
    fn leads_somewhere(which: u32) -> bool {
        // SAFETY: plain kernel32 queries of a handle value
        unsafe {
            let h = GetStdHandle(which);
            !h.is_null() && h as isize != -1 && GetFileType(h) != FILE_TYPE_UNKNOWN
        }
    }

    pub fn join() {
        let var = std::env::var_os(CONSOLE_VAR);
        let has_output = leads_somewhere(STD_OUTPUT_HANDLE) || leads_somewhere(STD_ERROR_HANDLE);
        if !wants_console(var.as_deref(), has_output) {
            return;
        }
        // SAFETY: plain kernel32 calls; the file name is NUL-terminated
        unsafe {
            if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
                return;
            }
            // the console's output as standard output and error, where attaching left none
            if !leads_somewhere(STD_OUTPUT_HANDLE) {
                let name: Vec<u16> = "CONOUT$".encode_utf16().chain([0]).collect();
                let out = CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                );
                if !out.is_null() && out as isize != -1 {
                    SetStdHandle(STD_OUTPUT_HANDLE, out);
                    SetStdHandle(STD_ERROR_HANDLE, out);
                }
            }
            SetConsoleCtrlHandler(Some(on_ctrl), 1);
        }
    }

    /// Ctrl+C and Ctrl+Break end the app as its close button does; before the app is there, and
    /// for the console closing, the system's way (the process ends, as 0.3.4's did).
    unsafe extern "system" fn on_ctrl(event: u32) -> i32 {
        match (event, APP.get()) {
            (CTRL_C_EVENT | CTRL_BREAK_EVENT, Some(app)) => {
                app.exit(CTRL_C_EXIT);
                1
            }
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_console_only_when_the_launcher_says_so_and_gave_no_output() {
        let one = Some(OsStr::new("1"));
        assert!(wants_console(one, false));
        // redirected: SpaceKnife.com passed its handles (and doesn't set the variable then)
        assert!(!wants_console(one, true));
        // not started by SpaceKnife.com in a terminal
        assert!(!wants_console(None, false));
        assert!(!wants_console(Some(OsStr::new("0")), false));
        assert!(!wants_console(Some(OsStr::new("")), false));
    }
}
