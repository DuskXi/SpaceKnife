// No console window behind the app in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::OsString;

fn main() {
    // Before the first allocation that matters: glibc arena limits / trim threshold (no-op elsewhere).
    sk_core::memory::tune_allocator();
    // The other jobs of this one program, before anything else (no window, no logging, no settings):
    // the elevated scan helper the backend starts (`<exe> helper …`, sk_core::helper), putting a
    // portable copy's new version in place (`<exe> --apply-update <plan>`, sk_core::update::swap;
    // install.rs starts the *new* version's program with it), and that helper's question whether
    // this program can start here (`<exe> --startup-check`, startup_check.rs).
    let args: Vec<OsString> = std::env::args_os().collect();
    match args.get(1).and_then(|a| a.to_str()) {
        Some(sk_core::update::swap::STARTUP_CHECK) => {
            std::process::exit(spaceknife_desktop::startup_check::run())
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
    spaceknife_desktop::prepare_environment();
    // 0.3.4's `SpaceKnife --headless` / `--listen <address>` (anywhere among the arguments): headless
    // mode in this terminal, no window (cli.rs)
    if spaceknife_desktop::cli::wants_headless(args.get(1..).unwrap_or(&[])) {
        std::process::exit(spaceknife_desktop::cli::run_headless(&args));
    }
    // Windows, started by SpaceKnife.com from a terminal, which waits for the window as 0.3.4's
    // did: that terminal's output, and its Ctrl+C (console.rs)
    spaceknife_desktop::console::join_launchers_terminal();
    spaceknife_desktop::run();
}
