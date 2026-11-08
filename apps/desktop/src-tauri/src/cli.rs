//! 0.3.4's headless launch forms (`SpaceKnife --headless`, `SpaceKnife --listen <address>`, with
//! the options anywhere among the arguments, and `--help`; plan §4 gap 1): scripts and service units
//! start the app that way. main.rs runs them as `spaceknife-cli headless …` in this process instead
//! of opening a window. The Electron shell's apps/electron-shell/src/main/cli.ts and the Windows
//! console launcher (packaging/console-launcher) read them the same way, with the same cases.
//!
//! Only the headless options are passed on; everything else among the arguments (macOS's -psn_…,
//! switches meant for another program) is left out, as 0.3.4 ignored it. A value is the next
//! argument unless that is missing or another option (`--…`), or follows "=".

use std::ffi::{OsStr, OsString};

/// Options with a value, as spaceknife-cli headless takes them.
const VALUE_OPTIONS: [&str; 4] = ["--listen", "--tls-cert", "--tls-key", "--ui"];

/// The arguments for spaceknife-cli (`headless …`) when these are a headless launch, else None
/// (the window). `args` are the program's, after its own path.
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

/// Run `spaceknife-cli headless …` in this process (sk_server's command line, as spaceknife-cli
/// runs it) and return its exit code. Call before anything else starts (main.rs, after
/// prepare_environment): it tells the server how this copy was installed through the environment.
pub fn run_headless(program: &OsStr, args: Vec<OsString>) -> i32 {
    // the backend's update checks: this copy's version and kind of install, unless told otherwise
    // SAFETY: called from main before the runtime or any other thread exists
    unsafe {
        if std::env::var_os("SPACEKNIFE_APP_VERSION").is_none() {
            std::env::set_var("SPACEKNIFE_APP_VERSION", env!("CARGO_PKG_VERSION"));
        }
        if std::env::var_os("SPACEKNIFE_INSTALL_KIND").is_none() {
            std::env::set_var(
                "SPACEKNIFE_INSTALL_KIND",
                crate::install::this_install_kind(),
            );
        }
    }
    console::attach_to_parent();
    sk_core::io::no_cloud_downloads(); // macOS: never fetch iCloud / File Provider contents
    use clap::Parser as _;
    let cli = sk_server::cli::Cli::parse_from(std::iter::once(program.to_owned()).chain(args));
    match cli.command {
        sk_server::cli::Command::Headless(a) => sk_server::cli::headless::headless(a),
        _ => 2,
    }
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

    fn run(args: &[&str]) -> Option<Vec<String>> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        headless_args(&args).map(|v| {
            v.into_iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        })
    }

    #[test]
    fn opens_the_window_without_a_headless_option() {
        assert_eq!(run(&[]), None);
        assert_eq!(run(&["--updated"]), None);
        assert_eq!(run(&["-psn_0_12345", "--no-sandbox"]), None);
        // only with --headless or --listen, as in 0.3.4
        assert_eq!(run(&["--yes"]), None);
        // the app's own modes (main.rs) are not headless
        assert_eq!(run(&["--apply-update", "plan.json"]), None);
    }

    #[test]
    fn takes_headless_and_listen_anywhere_among_the_arguments() {
        assert_eq!(run(&["--headless"]).unwrap(), ["headless"]);
        assert_eq!(run(&["--no-sandbox", "--headless"]).unwrap(), ["headless"]);
        assert_eq!(
            run(&["--listen", "0.0.0.0:8375", "--yes"]).unwrap(),
            ["headless", "--listen=0.0.0.0:8375", "--yes"]
        );
        assert_eq!(
            run(&["--yes", "--headless", "--listen=:9000"]).unwrap(),
            ["headless", "--yes", "--listen=:9000"]
        );
        assert_eq!(
            run(&["--no-sandbox", "--listen", "[::]:8375", "--headless"]).unwrap(),
            ["headless", "--listen=[::]:8375"]
        );
    }

    #[test]
    fn passes_the_tls_files_and_the_interface_folder_and_nothing_else() {
        assert_eq!(
            run(&[
                "--headless",
                "--tls-cert",
                "/etc/sk/cert.pem",
                "--tls-key=/etc/sk/key.pem",
                "--frobnicate",
                "x"
            ])
            .unwrap(),
            [
                "headless",
                "--tls-cert=/etc/sk/cert.pem",
                "--tls-key=/etc/sk/key.pem"
            ]
        );
        assert_eq!(
            run(&["--listen", "8375", "--ui", "C:\\sk ui\\public"]).unwrap(),
            ["headless", "--listen=8375", "--ui=C:\\sk ui\\public"]
        );
    }

    #[test]
    fn leaves_a_missing_value_missing_for_spaceknife_cli_to_say_so() {
        assert_eq!(run(&["--listen"]).unwrap(), ["headless", "--listen"]);
        assert_eq!(
            run(&["--listen", "--yes"]).unwrap(),
            ["headless", "--listen", "--yes"]
        );
        assert_eq!(
            run(&["--headless", "--tls-cert"]).unwrap(),
            ["headless", "--tls-cert"]
        );
    }

    #[test]
    fn shows_headless_modes_help_for_help_as_0_3_4_did() {
        assert_eq!(run(&["--help"]).unwrap(), ["headless", "--help"]);
        assert_eq!(run(&["-h"]).unwrap(), ["headless", "--help"]);
        assert_eq!(
            run(&["--headless", "--help"]).unwrap(),
            ["headless", "--help"]
        );
    }

    #[test]
    fn the_command_line_spaceknife_cli_reads_is_its_headless_command() {
        use clap::Parser as _;
        let args = headless_args(&[
            "--listen".into(),
            "0.0.0.0:9".into(),
            "--yes".into(),
            "--tls-cert".into(),
            "c.pem".into(),
            "--tls-key".into(),
            "k.pem".into(),
        ])
        .unwrap();
        let cli =
            sk_server::cli::Cli::try_parse_from(std::iter::once(OsString::from("x")).chain(args))
                .unwrap();
        let sk_server::cli::Command::Headless(a) = cli.command else {
            panic!("not the headless command");
        };
        assert_eq!(a.listen.as_deref(), Some("0.0.0.0:9"));
        assert!(a.yes);
        assert_eq!(a.tls_cert.as_deref(), Some(std::path::Path::new("c.pem")));
        assert_eq!(a.tls_key.as_deref(), Some(std::path::Path::new("k.pem")));
    }

    #[cfg(unix)]
    #[test]
    fn keeps_a_path_that_is_not_unicode() {
        use std::os::unix::ffi::OsStringExt as _;
        let odd = OsString::from_vec(b"/srv/\xff/ui".to_vec());
        let out = headless_args(&["--headless".into(), "--ui".into(), odd]).unwrap();
        let mut want = OsString::from("--ui=");
        want.push(OsString::from_vec(b"/srv/\xff/ui".to_vec()));
        assert_eq!(out, [OsString::from("headless"), want]);
    }
}
