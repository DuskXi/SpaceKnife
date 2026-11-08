//! The app's program started the way 0.3.4 was for headless mode (plan §4 gap 1) runs spaceknife-cli's
//! headless command, with no window: `--help` prints its options, a bad `--listen` is refused with
//! exit code 2. (Before, the program opened its window whatever the arguments.) A program that is
//! still running after a while would be a window: it is ended and the test fails.

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Run the app's program with these arguments: exit code and everything it printed.
fn run(args: &[&str]) -> (Option<i32>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_spaceknife"))
        .args(args)
        .env(
            "SPACEKNIFE_HOME",
            std::env::temp_dir().join("sk-headless-forms"),
        )
        .env("SPACEKNIFE_LANGUAGE", "en")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("{args:?}: still running after 60 s (a window?)");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut text = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    (status.code(), text)
}

#[test]
fn help_shows_the_headless_options() {
    for args in [
        &["--help"][..],
        &["--headless", "--help"],
        &["--no-sandbox", "-h"],
    ] {
        let (code, text) = run(args);
        assert_eq!(code, Some(0), "{args:?}: {text}");
        assert!(
            text.contains("--listen") && text.contains("--tls-cert"),
            "{args:?}: {text}"
        );
    }
}

#[test]
fn a_bad_listen_address_is_refused_without_a_window() {
    let (code, text) = run(&["--yes", "--listen", "not an address:x"]);
    assert_eq!(code, Some(2), "{text}");
    let (code, text) = run(&["--headless", "--listen"]);
    assert_eq!(code, Some(2), "{text}");
}
