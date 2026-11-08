//! The app's program asked `--startup-check`, as the swap helper of a portable copy's new version
//! asks it before it puts it in place (sk_core::update::swap::startup_check): it answers and ends,
//! without a window and without touching the app's files; without a web view runtime it says so.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use sk_core::update::swap::{STARTUP_CHECK, STARTUP_CHECK_TIMEOUT, startup_check};

const APP: &str = env!("CARGO_BIN_EXE_spaceknife");

/// The program asked the check with these variables; how it ended, and how long it took. Still
/// running after 30 s, it is a window: it is ended and the test fails.
fn ask(vars: &[(&str, &Path)]) -> (Output, Duration) {
    let mut cmd = Command::new(APP);
    cmd.arg(STARTUP_CHECK)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(vars.iter().map(|(k, v)| (*k, *v)));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: a development build is a console program
    }
    let started = Instant::now();
    let mut child = cmd.spawn().unwrap();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{APP} {STARTUP_CHECK} still ran after 30 s: it opened its window");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let took = started.elapsed();
    (child.wait_with_output().unwrap(), took)
}

fn said(out: &Output) -> Value {
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or_default();
    serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("{e}: {text:?} / {}", String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn answers_and_ends_without_a_window_or_a_file() {
    let home = tempfile::tempdir().unwrap();
    let (out, took) = ask(&[("SPACEKNIFE_HOME", home.path())]);
    // a window would keep it running
    assert!(took < Duration::from_secs(20), "{took:?}");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said = said(&out);
    assert_eq!(said["ok"], true, "{said}");
    assert!(
        !said["webview"].as_str().unwrap_or_default().is_empty(),
        "{said}"
    );
    // no settings, no log, no web view profile
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn the_swap_helper_takes_its_answer() {
    let env = std::env::vars().collect();
    let what = startup_check(Path::new(APP), &env, STARTUP_CHECK_TIMEOUT).unwrap();
    assert!(what.starts_with("webview "), "{what}");
}

/// WebView2's loader looks for the runtime where WEBVIEW2_BROWSER_EXECUTABLE_FOLDER says, when it
/// is set: an empty folder there is a Windows without the runtime (a Windows 10 LTSC, say).
#[cfg(windows)]
#[test]
fn a_windows_without_webview2_is_told_so() {
    let empty = tempfile::tempdir().unwrap();
    let (out, _) = ask(&[("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER", empty.path())]);
    assert_eq!(out.status.code(), Some(1));
    let said = said(&out);
    assert_eq!(said["ok"], false);
    let error = said["error"].as_str().unwrap();
    assert!(
        error.contains("needs the Microsoft Edge WebView2 Runtime"),
        "{error}"
    );
    // and the helper refuses the swap with these words
    let mut env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    env.insert(
        "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER".into(),
        empty.path().to_string_lossy().into_owned(),
    );
    let refused = startup_check(Path::new(APP), &env, STARTUP_CHECK_TIMEOUT).unwrap_err();
    assert_eq!(refused.0, error);
}
