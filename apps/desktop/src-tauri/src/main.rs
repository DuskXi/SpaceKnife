// No console window behind the app in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::OsString;

fn main() {
    // Before the first allocation that matters: glibc arena limits / trim threshold (no-op elsewhere).
    sk_core::memory::tune_allocator();
    // The other jobs of this one program, before anything else (no window, no logging, no settings):
    // the elevated scan helper the backend starts (`<exe> helper …`, sk_core::helper), putting a
    // portable copy's new version in place (`<exe> --apply-update <plan>`, sk_core::update::swap;
    // install.rs starts the *new* version's program with it), that helper's question whether
    // this program can start here (`<exe> --startup-check`, startup_check.rs), and what it starts
    // when the answer was no: the user told why (`<exe> --startup-problem`, startup_check.rs
    // explain, dialog.rs).
    let args: Vec<OsString> = std::env::args_os().collect();
    match args.get(1).and_then(|a| a.to_str()) {
        Some(sk_core::update::swap::STARTUP_CHECK) => {
            std::process::exit(spaceknife_desktop::startup_check::run())
        }
        Some(spaceknife_desktop::startup_check::STARTUP_PROBLEM) => {
            std::process::exit(spaceknife_desktop::startup_check::explain())
        }
        Some(sk_core::helper::HELPER_SUBCOMMAND) => {
            std::process::exit(sk_core::helper::run_main(&args[2..]))
        }
        Some("--apply-update") => {
            let rest: Vec<String> = args[2..]
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            std::process::exit(sk_core::update::swap::main(&rest));
        }
        _ => {}
    }
    // SPACEKNIFE_READY_FILE (end-to-end tests, demos, the swap watching this new version): out of
    // this process's environment, so that nothing the app starts inherits it (an update's helper
    // or installer, a terminal, an opened file); the backend writes that file once
    // (backend.rs keep_ready_file). The swap helper above reads it as it is, and starts nothing
    // but SpaceKnife, which it names its own file.
    let ready = std::env::var_os(sk_core::update::swap::READY_FILE_VAR);
    // SAFETY: one thread so far — the window, GTK/WebKitGTK and the backend start in run() below,
    // and nothing before this line starts a thread — so nothing reads the environment meanwhile.
    unsafe { std::env::remove_var(sk_core::update::swap::READY_FILE_VAR) };
    spaceknife_desktop::backend::keep_ready_file(ready);
    spaceknife_desktop::prepare_environment();
    // 0.3.4's `SpaceKnife --headless` / `--listen <address>` (anywhere among the arguments): headless
    // mode in this terminal, no window (cli.rs)
    if spaceknife_desktop::cli::wants_headless(args.get(1..).unwrap_or(&[])) {
        std::process::exit(spaceknife_desktop::cli::run_headless(&args));
    }
    // Windows, started by SpaceKnife.com from a terminal, which waits for the window as 0.3.4's
    // did: that terminal's output, and its Ctrl+C (console.rs)
    spaceknife_desktop::console::join_launchers_terminal();
    // the log from here on, so that what keeps the window from opening is in it (backend.rs)
    spaceknife_desktop::backend::init_logging();
    // Windows: no window without a WebView2 Runtime that can make it. The user is told why, in
    // their language, and the program ends (startup_check.rs window_can_open, dialog.rs), where
    // Tauri showed an English box and panicked, or ended without a word.
    if !spaceknife_desktop::startup_check::window_can_open() {
        std::process::exit(1);
    }
    spaceknife_desktop::run();
}
