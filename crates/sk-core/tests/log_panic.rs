//! Panics reach the log file (`sk_core::log::init` installs the panic hook): the window program on
//! Windows has no stderr, so without it a panic left no trace. Its own test binary: the hook and
//! the log file are process-wide.

use std::time::Duration;

use sk_core::log::{LogConfig, init};
use tracing::level_filters::LevelFilter;

#[test]
fn a_panic_lands_in_the_log_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("logs").join("spaceknife.log");
    init(&LogConfig {
        file: Some((file.clone(), LevelFilter::INFO)),
        ..LogConfig::default()
    })
    .unwrap();
    let joined = std::thread::Builder::new()
        .name("sk-test-panicker".into())
        .spawn(|| panic!("injected panic for the log file"))
        .unwrap()
        .join();
    assert!(joined.is_err());
    // a String payload too (formatted panic messages)
    let n = std::hint::black_box(42);
    let caught = std::panic::catch_unwind(|| panic!("formatted panic {n}"));
    assert!(caught.is_err());
    let end = std::time::Instant::now() + Duration::from_secs(5);
    let text = loop {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        if text.contains("formatted panic 42") || std::time::Instant::now() > end {
            break text;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let line = text
        .lines()
        .find(|l| l.contains("injected panic for the log file"))
        .unwrap_or_else(|| panic!("no panic line in the log file:\n{text}"));
    assert!(line.contains("ERROR"), "{line}");
    assert!(line.contains("thread 'sk-test-panicker'"), "{line}");
    assert!(line.contains("log_panic.rs"), "{line}");
    assert!(text.contains("formatted panic 42"), "{text}");
    sk_core::log::set_file_logging(None, LevelFilter::OFF).unwrap();
}
