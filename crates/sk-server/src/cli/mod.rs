//! The console binary `spaceknife-cli` (port of `spaceknife.cli`, `cli_scan`, `cli_run`,
//! `__main__`, `packaging/backend_entry.py`, and Electron's headless mode):
//!
//! ```text
//! spaceknife-cli scan <paths>… [--top N] [--depth N] [--mft] [--no-snapshot] …   scan, print where the space goes
//! spaceknife-cli run <command sequence>                                          SpaceSniffer-style commands
//! spaceknife-cli snapshot list | show <file>                                     saved snapshots
//! spaceknife-cli serve [--token-stdin] [--port N] …                             the WebSocket backend for a UI
//! spaceknife-cli headless [--listen <address>] [--yes] [--tls-cert F --tls-key F] [--ui DIR]
//! spaceknife-cli … --headless … | … --listen <address> …                        0.3.4's app's form of it
//! spaceknife-cli helper …                                                        the elevated scan helper
//! spaceknife-cli --apply-update <plan>                                           put an update in place
//! ```
//!
//! `scan` is what scripts and the external benchmark run (Disk-Scan-Benchmark parses its output):
//! it starts nothing but the scanner, and its text is the Python CLI's, character for character
//! ([`rich`]).

pub mod headless;
pub mod rich;
pub mod run;
pub mod scan;
pub mod serve;

#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Command-line parse result.
#[derive(Debug, Parser)]
#[command(
    name = "spaceknife-cli",
    version,
    about = "SpaceKnife backend: disk space scanning, analysis and the WebSocket server for the UIs.",
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Scan folders or drives and show where the space goes.
    Scan(scan::ScanArgs),
    /// Run SpaceSniffer-style commands without a window: scan, load, filter, save, export, autoclose, help.
    Run {
        /// The command sequence (`spaceknife-cli run help` for the commands).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
    /// Inspect saved snapshots.
    #[command(subcommand, arg_required_else_help = true)]
    Snapshot(SnapshotCommand),
    /// Run the WebSocket backend. Prints one JSON 'ready' line on stdout once listening.
    Serve(serve::ServeArgs),
    /// Serve the interface over HTTP for a browser, on this computer or another one (no window).
    /// `spaceknife-cli headless --help` for its options.
    #[command(disable_help_flag = true)]
    Headless {
        /// --listen <address>, --yes, --tls-cert <file>, --tls-key <file>, --ui <folder>
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<OsString>,
    },
    /// Print the backend and protocol versions.
    Version,
}

#[derive(Debug, Subcommand)]
pub enum SnapshotCommand {
    /// List auto-saved snapshots, newest first.
    List,
    /// Show where the space went in a saved snapshot (no rescan).
    Show(ShowArgs),
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Snapshot file (.sks).
    pub file: PathBuf,
    /// Rows per level.
    #[arg(long, default_value_t = 15)]
    pub top: usize,
    /// Levels to expand.
    #[arg(long, default_value_t = 2)]
    pub depth: usize,
}

/// The whole program: returns the exit code.
pub fn main() -> i32 {
    let args: Vec<OsString> = std::env::args_os().collect();
    // the elevated helper and the update swap: before anything else (no logging, no settings)
    match args.get(1).and_then(|a| a.to_str()) {
        Some(sk_core::helper::HELPER_SUBCOMMAND) => return sk_core::helper::run_main(&args[2..]),
        Some("--apply-update") => {
            let rest: Vec<String> = args[2..]
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            return sk_core::update::swap::main(&rest);
        }
        _ => {}
    }
    sk_core::io::no_cloud_downloads(); // macOS: never fetch iCloud / File Provider contents
    match args.get(1).and_then(|a| a.to_str()) {
        // the arguments after `run` reach its own parser untouched (SpaceSniffer's rules)
        Some("run") => {
            let tokens: Vec<String> = args[2..]
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            return run::cli(&tokens);
        }
        // headless mode reads its own options: 0.3.4's texts, in the user's language
        Some("headless") => return headless::cli(&args, headless::Form::Subcommand),
        Some("help") if args.get(2).and_then(|a| a.to_str()) == Some("headless") => {
            let asked = [args[0].clone(), "headless".into(), "--help".into()];
            return headless::cli(&asked, headless::Form::Subcommand);
        }
        _ => {}
    }
    // `--headless` / `--listen …` as 0.3.4's app took them: anywhere among the arguments
    if headless::is_app_form(&args[1..]) {
        return headless::cli(&args, headless::Form::App);
    }
    dispatch(Cli::parse_from(args))
}

fn dispatch(cli: Cli) -> i32 {
    match cli.command {
        Command::Scan(a) => scan::scan(a),
        Command::Run { args } => run::cli(&args),
        Command::Snapshot(SnapshotCommand::List) => scan::snapshot_list(),
        Command::Snapshot(SnapshotCommand::Show(a)) => scan::snapshot_show(&a.file, a.top, a.depth),
        Command::Serve(a) => serve::serve(a),
        Command::Headless { args } => {
            let mut argv: Vec<OsString> = vec!["spaceknife-cli".into(), "headless".into()];
            argv.extend(args);
            headless::cli(&argv, headless::Form::Subcommand)
        }
        Command::Version => {
            print_line(&format!(
                "spaceknife {} · protocol {} · rust backend",
                env!("CARGO_PKG_VERSION"),
                sk_protocol::PROTOCOL_VERSION
            ));
            0
        }
    }
}

// ------------------------------------------------------------------------------------- output

/// A line on stdout. A stdout that is closed or gone (`| head`, a terminal closed) is no error:
/// the command goes on (`println!` would panic).
pub fn print_line(text: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

/// A line on stderr, as [`print_line`].
pub fn eprint_line(text: &str) {
    use std::io::Write as _;
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{text}");
    let _ = err.flush();
}

/// Text on stderr without a line end (a question), as [`print_line`].
pub fn eprint_part(text: &str) {
    use std::io::Write as _;
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "{text}");
    let _ = err.flush();
}

// ------------------------------------------------------------------------------------- shared

/// The interface language of a command-line program (M7.5): the user's setting
/// (`general.language`) when the settings file says another than "system", else the system's.
/// Only that one value is read (no settings store): the scan's start-up stays short.
pub fn apply_language() {
    let mut setting = None;
    if std::env::var_os("SPACEKNIFE_LANGUAGE").is_none_or(|v| v.is_empty()) {
        setting = stored_language(&sk_core::paths::config_dir().join("settings.json"));
    }
    sk_core::i18n::set_language(sk_core::i18n::resolve(setting.as_deref()));
}

/// `general.language` of a settings file, unless missing, unreadable or "system".
pub fn stored_language(file: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    let value = doc.get("values")?.get("general.language")?.as_str()?;
    (value != "system" && matches!(value, "zh-CN" | "en")).then(|| value.to_owned())
}

/// Warnings and errors of the core on stderr (as `setup_logging("WARNING")`); nothing else.
pub fn init_warnings() {
    let _ = sk_core::log::init(&sk_core::log::LogConfig {
        level: tracing::level_filters::LevelFilter::WARN,
        ..Default::default()
    });
}

/// Ctrl+C (and on POSIX SIGTERM): the first one sets [`interrupted`] for the running command to
/// stop at its next check; with `exit_now` (no cooperative stop possible) it prints `message` and
/// ends the process with 130.
pub mod interrupt {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    static FLAG: AtomicBool = AtomicBool::new(false);
    static HARD: AtomicBool = AtomicBool::new(false);
    static MESSAGE: OnceLock<Vec<u8>> = OnceLock::new();

    pub fn interrupted() -> bool {
        FLAG.load(Ordering::SeqCst)
    }

    /// Whether a Ctrl+C now stops cooperatively (true) or ends the process at once (false).
    pub fn cooperative(on: bool) {
        HARD.store(on, Ordering::SeqCst);
    }

    fn fire() {
        let again = FLAG.swap(true, Ordering::SeqCst);
        if again || !HARD.load(Ordering::SeqCst) {
            if let Some(m) = MESSAGE.get() {
                write_raw(m);
            }
            exit_now(130);
        }
    }

    /// Install the handler; `message` is printed when the process ends because of it.
    pub fn install(message: String) {
        let _ = MESSAGE.set(message.into_bytes());
        imp::install();
    }

    #[cfg(windows)]
    fn write_raw(m: &[u8]) {
        use std::io::Write as _;
        let _ = std::io::stdout().write_all(m);
        let _ = std::io::stdout().flush();
    }
    #[cfg(windows)]
    fn exit_now(code: i32) {
        std::process::exit(code);
    }

    #[cfg(unix)]
    fn write_raw(m: &[u8]) {
        // SAFETY: write(2) is async-signal-safe.
        unsafe {
            libc::write(libc::STDOUT_FILENO, m.as_ptr().cast(), m.len());
        }
    }
    #[cfg(unix)]
    fn exit_now(code: i32) {
        // SAFETY: _exit is async-signal-safe.
        unsafe { libc::_exit(code) }
    }

    #[cfg(windows)]
    mod imp {
        use windows_sys::Win32::Foundation::{FALSE, TRUE};
        use windows_sys::Win32::System::Console::{
            CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
        };

        unsafe extern "system" fn handler(kind: u32) -> i32 {
            if kind == CTRL_C_EVENT || kind == CTRL_BREAK_EVENT {
                super::fire();
                TRUE
            } else {
                FALSE
            }
        }

        pub fn install() {
            // SAFETY: registers a handler with the right signature.
            unsafe {
                SetConsoleCtrlHandler(Some(handler), TRUE);
            }
        }
    }

    #[cfg(unix)]
    mod imp {
        extern "C" fn handler(_: libc::c_int) {
            super::fire();
        }

        pub fn install() {
            // SAFETY: installs an async-signal-safe handler (atomics, write, _exit).
            unsafe {
                let handler = handler as extern "C" fn(libc::c_int) as libc::sighandler_t;
                libc::signal(libc::SIGINT, handler);
                libc::signal(libc::SIGTERM, handler);
            }
        }
    }
}
