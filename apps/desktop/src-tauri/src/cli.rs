//! 0.3.4's headless launch forms (`SpaceKnife --headless`, `SpaceKnife --listen <address>`, with
//! the options anywhere among the arguments, and `--help`; plan §4 gap 1): scripts and service units
//! start the app that way. main.rs runs them in this process instead of opening a window, read by
//! spaceknife-cli's own reading of 0.3.4's form (`sk_server::cli::headless`: [`wants_headless`]
//! decides, `cli` with `Form::App` runs it), so the app, `spaceknife-cli --headless …`,
//! SpaceKnife.com (packaging/console-launcher) and the Electron shell
//! (apps/electron-shell/src/main/cli.ts) agree on every case: `--yes` or `--tls-*` alone is a
//! mistake (exit 2, no window), as in 0.3.4; arguments that are not headless mode's (macOS's
//! -psn_…, switches meant for another program) are skipped.

use std::ffi::OsString;

use sk_server::cli::headless;

/// Whether the program's arguments (after its own path) are a headless launch, not the window.
pub fn wants_headless(args: &[OsString]) -> bool {
    headless::wants_headless(args)
}

/// Run headless mode in this process from the whole command line (`argv[0]` names the program in
/// its texts) and return its exit code. Call from main after prepare_environment, before anything
/// else starts. The version and the kind of install the server reports are read as spaceknife-cli
/// reads them (`SPACEKNIFE_APP_VERSION` / `SPACEKNIFE_INSTALL_KIND` when a launcher sets them, else
/// this build's version and where this program is); the environment is never changed.
pub fn run_headless(argv: &[OsString]) -> i32 {
    console::attach_to_parent();
    sk_core::io::no_cloud_downloads(); // macOS: never fetch iCloud / File Provider contents
    headless::cli(argv, headless::Form::App)
}

#[cfg(windows)]
mod console {
    use std::ffi::c_void;

    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn AttachConsole(process: u32) -> i32;
    }

    /// The release program is a windowed one: started straight from a terminal it has no output.
    /// Then it writes to that terminal's console (as 0.3.4's SpaceKnife.exe did); a program given
    /// its handles (SpaceKnife.com, a redirect) keeps them.
    pub fn attach_to_parent() {
        // SAFETY: plain kernel32 calls without pointers of ours
        unsafe {
            let out = GetStdHandle(STD_OUTPUT_HANDLE);
            if out.is_null() || out as isize == -1 {
                AttachConsole(ATTACH_PARENT_PROCESS);
            }
        }
    }
}

#[cfg(not(windows))]
mod console {
    /// A Unix program has its terminal already.
    pub fn attach_to_parent() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wants(args: &[&str]) -> bool {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        wants_headless(&args)
    }

    #[test]
    fn opens_the_window_without_a_headless_option() {
        assert!(!wants(&[]));
        assert!(!wants(&["--updated"]));
        assert!(!wants(&["-psn_0_12345", "--no-sandbox"]));
        // the app's own modes (main.rs) are not headless
        assert!(!wants(&["--apply-update", "plan.json"]));
    }

    #[test]
    fn takes_headless_mode_anywhere_among_the_arguments() {
        assert!(wants(&["--headless"]));
        assert!(wants(&["--no-sandbox", "--headless"]));
        assert!(wants(&["--listen", "0.0.0.0:8375"]));
        assert!(wants(&["--no-sandbox", "--listen=[::]:8375"]));
        assert!(wants(&["--help"]));
        assert!(wants(&["-psn_0_1", "-h"]));
    }

    /// 0.3.4 answered `--yes` (or a TLS file) without `--headless` / `--listen` with a mistake, not a
    /// window (v0.3.4 apps/electron/src/main/headless.ts parseCommandLine, "headlessOnly").
    #[test]
    fn headless_options_alone_go_to_headless_mode_to_be_refused() {
        assert!(wants(&["--yes"]));
        assert!(wants(&["--tls-cert", "c.pem"]));
        assert!(wants(&["--tls-key=k.pem"]));
        let argv: Vec<OsString> = ["SpaceKnife", "--yes"].iter().map(OsString::from).collect();
        assert_eq!(
            headless::parse(&argv[1..], headless::Form::App),
            headless::Parsed::Error("headlessOnly", "--yes".into())
        );
    }
}
