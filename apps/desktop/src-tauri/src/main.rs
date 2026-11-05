// No console window behind the app in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::OsString;

fn main() {
    // The other jobs of this one program, before anything else (no window, no logging, no settings):
    // the elevated scan helper the backend starts (`<exe> helper …`, sk_core::helper), and putting
    // a portable copy's new version in place (`<exe> --apply-update <plan>`, sk_core::update::swap;
    // install.rs starts the *new* version's program with it).
    let args: Vec<OsString> = std::env::args_os().collect();
    match args.get(1).and_then(|a| a.to_str()) {
        Some(sk_core::helper::HELPER_SUBCOMMAND) => std::process::exit(sk_core::helper::run_main(&args[2..])),
        Some("--apply-update") => {
            let rest: Vec<String> = args[2..].iter().map(|a| a.to_string_lossy().into_owned()).collect();
            std::process::exit(sk_core::update::swap::main(&rest));
        }
        _ => {}
    }
    spaceknife_desktop::prepare_environment();
    spaceknife_desktop::run();
}
