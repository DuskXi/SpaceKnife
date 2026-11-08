//! SpaceKnife.com as a terminal runs it, in a folder laid out as the app's: the launcher as
//! SpaceKnife.com, the stand-in (examples/stub.rs) as spaceknife-cli.exe and SpaceKnife.exe.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const LAUNCHER: &str = env!("CARGO_BIN_EXE_spaceknife-console");

/// The stand-in, built by `cargo test` with the examples.
fn stub() -> PathBuf {
    let dir = Path::new(LAUNCHER).parent().unwrap();
    let stub = dir.join("examples").join("stub.exe");
    assert!(
        stub.is_file(),
        "{} (cargo test builds the examples)",
        stub.display()
    );
    stub
}

/// A fresh app folder: SpaceKnife.com, and the stand-ins named in `with`.
fn app_folder(name: &str, with: &[&str]) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("launcher-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(LAUNCHER, dir.join("SpaceKnife.com")).unwrap();
    for exe in with {
        std::fs::copy(stub(), dir.join(exe)).unwrap();
    }
    dir
}

/// (exit code, the stand-in's lines: its file name, argv[0], the arguments).
fn launch(dir: &Path, args: &[&str]) -> (Option<i32>, Vec<String>) {
    let out = Command::new(dir.join("SpaceKnife.com"))
        .args(args)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    (out.status.code(), text.lines().map(str::to_owned).collect())
}

#[test]
fn headless_mode_runs_spaceknife_cli_with_the_arguments_as_typed() {
    let dir = app_folder("cli", &["spaceknife-cli.exe", "SpaceKnife.exe"]);
    let (code, lines) = launch(
        &dir,
        &["--listen", "0.0.0.0:8375", "--yes", "a b", "--ui=C:\\x y"],
    );
    // it waited, and ends as spaceknife-cli did
    assert_eq!(code, Some(7), "{lines:?}");
    assert_eq!(lines[0], "exe=spaceknife-cli.exe");
    // argv[0] is the name it was started by (spaceknife-cli's texts say SpaceKnife)
    assert!(lines[1].ends_with("SpaceKnife.com"), "{lines:?}");
    assert_eq!(
        lines[2..],
        ["--listen", "0.0.0.0:8375", "--yes", "a b", "--ui=C:\\x y"]
    );
    // anywhere among other arguments
    let (code, lines) = launch(&dir, &["--no-sandbox", "--headless"]);
    assert_eq!(code, Some(7));
    assert_eq!(lines[0], "exe=spaceknife-cli.exe");
    assert_eq!(lines[2..], ["--no-sandbox", "--headless"]);
}

#[test]
fn help_alone_asks_for_headless_modes_help() {
    let dir = app_folder("help", &["spaceknife-cli.exe", "SpaceKnife.exe"]);
    for help in ["--help", "-h"] {
        let (code, lines) = launch(&dir, &[help]);
        assert_eq!(code, Some(7));
        assert_eq!(lines[0], "exe=spaceknife-cli.exe");
        assert_eq!(lines[2..], ["--headless", help]);
    }
}

#[test]
fn anything_else_starts_the_window_and_waits_for_it_as_0_3_4_did() {
    let dir = app_folder("app", &["spaceknife-cli.exe", "SpaceKnife.exe"]);
    let record = dir.join("record.txt");
    let started = Instant::now();
    let out = Command::new(dir.join("SpaceKnife.com"))
        .args(["--updated", "x y"])
        .env("STUB_RECORD", &record)
        .env("STUB_SLEEP_MS", "1500")
        .env("STUB_ENV", "SPACEKNIFE_CONSOLE")
        .output()
        .unwrap();
    // the terminal waits for the window's program and ends as it did
    assert!(started.elapsed() >= Duration::from_millis(1500));
    assert_eq!(out.status.code(), Some(7));
    let text = std::fs::read_to_string(&record).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "exe=SpaceKnife.exe");
    // as 0.3.4 started it: its full path, then the arguments as typed
    assert_eq!(Path::new(lines[1]), dir.join("SpaceKnife.exe"));
    assert_eq!(lines[2..4], ["--updated", "x y"]);
    // redirected (here: pipes): it got the handles, so its output is the launcher's, and no
    // SPACEKNIFE_CONSOLE, which is for a terminal's console
    assert_eq!(lines[4], "env SPACEKNIFE_CONSOLE=(unset)");
    let shown = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    assert_eq!(shown.trim_end(), text);
}

#[test]
fn the_window_s_exit_code_is_the_launcher_s() {
    let dir = app_folder("app-code", &["SpaceKnife.exe"]);
    let (code, lines) = launch(&dir, &[]);
    assert_eq!(code, Some(7));
    assert_eq!(lines[0], "exe=SpaceKnife.exe");
    assert_eq!(lines.len(), 2, "{lines:?}"); // no arguments
}

#[test]
fn a_missing_program_is_said_plainly() {
    let dir = app_folder("missing", &["SpaceKnife.exe"]);
    let out = Command::new(dir.join("SpaceKnife.com"))
        .arg("--headless")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("spaceknife-cli.exe is missing"), "{err}");
}
