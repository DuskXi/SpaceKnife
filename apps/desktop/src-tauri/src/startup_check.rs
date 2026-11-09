//! `SpaceKnife --startup-check` (sk_core::update::swap::STARTUP_CHECK): the question the swap
//! helper of a portable copy's new version asks this program before it puts it in place. By the
//! time main runs, the system's loader has given the program every library it links (WebKitGTK 4.1
//! and GTK on Linux, the system's frameworks on macOS; the C runtime is linked in on Windows); what
//! is left to ask is the web view's runtime, which Windows may lack: WebView2's loader reports the
//! installed runtime's version (GetAvailableCoreWebView2BrowserVersionString), or that there is
//! none; a runtime older than [`MIN_WEBVIEW2`] can't open the window either, and one whose version
//! is empty or doesn't read as one counts as none. One line of JSON on stdout; exit 0 when the
//! window can open, 1 when it can't. Nothing else happens: no window or web view, no setting, file
//! or log of the app (main.rs answers it first).
//!
//! The same check, told to the user (dialog.rs):
//! - before the window (main.rs, Windows; [`window_can_open`]): without a runtime that can make it
//!   the program says why and ends with 1, where Tauri showed an English box and panicked (no
//!   runtime) or the program ended without a word or a log (one too old);
//! - when making the window failed all the same (lib.rs setup; [`window_failed`]);
//! - `SpaceKnife --startup-problem` ([`STARTUP_PROBLEM`]; main.rs answers it first, as the check):
//!   what the swap helper starts, detached, when it refuses to put this version in place. It runs
//!   the check again and explains what it finds, or that this version can't start on this
//!   computer, then ends ([`explain`]): no window or web view, no setting, file or log of the app.
//!   Elsewhere than on Windows the explanation goes to standard error.

use std::io::Write;

use serde_json::{Value, json};

use crate::dialog::{self, Context};

/// The oldest WebView2 Runtime the window opens with. wry gives the web view its background colour
/// through ICoreWebView2Controller2 and fails the window without it (window.rs always sets one);
/// that interface is "Stable in Release" since WebView2 SDK 1.0.774.44, which Microsoft's release
/// notes pair with Runtime 89.0.774.44. Every other interface wry asks for while making the window
/// is older, or optional (it carries on without).
pub const MIN_WEBVIEW2: [u32; 4] = [89, 0, 774, 44];

/// The flag the swap helper starts this program with when it refused to put it in place: explain
/// why to the user, then end ([`explain`]).
pub const STARTUP_PROBLEM: &str = "--startup-problem";

/// What keeps the window from opening.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// No web view runtime: the loader's words. On Windows also a WebView2 Runtime whose version is
    /// empty or doesn't read as one.
    Missing(String),
    /// A WebView2 Runtime older than [`MIN_WEBVIEW2`]: its version.
    TooOld(String),
    /// The runtime answered (with this version), but the window could not be made with it.
    WindowFailed {
        webview: Option<String>,
        error: String,
    },
    /// Nothing the check can name: this version can't start here for another reason (the swap
    /// helper's log says which).
    Unknown,
}

impl Problem {
    /// The problem in one English line: `--startup-check`'s "error" (the swap helper logs it, the
    /// app that asked shows it: 「无法启动更新：…」) and the app's log.
    pub fn error(&self, windows: bool) -> String {
        match self {
            Problem::Missing(detail) => no_webview(detail, windows),
            Problem::TooOld(version) => old_webview(version),
            Problem::WindowFailed {
                webview: Some(version),
                error,
            } => format!("SpaceKnife could not open its window (web view {version}): {error}"),
            Problem::WindowFailed {
                webview: None,
                error,
            } => format!("SpaceKnife could not open its window: {error}"),
            Problem::Unknown => "SpaceKnife can't start on this computer".to_owned(),
        }
    }
}

/// The web view runtime's version, or why there is none (Tauri's words).
fn webview_version() -> Result<String, String> {
    tauri::webview_version().map_err(|e| e.to_string())
}

/// Answer the check on stdout; the exit code.
pub fn run() -> i32 {
    let (code, said) = answer(webview_version());
    // a reader that went away is no reason to fail differently
    let _ = writeln!(std::io::stdout(), "{said}");
    let _ = std::io::stdout().flush();
    code
}

/// `--startup-problem`: run the check again and explain what it finds, or that this version can't
/// start on this computer (dialog.rs: on Windows a dialog, back when it is closed). The exit code
/// is the check's: 1 when it found what is missing, 0 when it found nothing.
pub fn explain() -> i32 {
    let found = check(webview_version());
    let code = i32::from(found.is_err());
    dialog::tell(&found.err().unwrap_or(Problem::Unknown), Context::Update);
    code
}

/// Before the window (main.rs, once logging runs): whether the web view's runtime can make it. On
/// Windows, when it can't, the reason goes to the log and to standard error, a dialog explains it
/// (back when it is closed) and the answer is false: the program ends with 1. Elsewhere the loader
/// has checked WebKit already: true.
pub fn window_can_open() -> bool {
    if !cfg!(windows) {
        return true;
    }
    match check(webview_version()) {
        Ok(version) => {
            tracing::info!("Microsoft Edge WebView2 Runtime {version}");
            true
        }
        Err(problem) => {
            dialog::tell(&problem, Context::Start);
            false
        }
    }
}

/// Making the window failed with `error` (lib.rs): what to tell the user. The check again first:
/// a runtime that is missing or too old says more than the error does.
pub fn window_failed(error: &str) -> Problem {
    match check(webview_version()) {
        Err(problem) => problem,
        Ok(version) => Problem::WindowFailed {
            webview: Some(version),
            error: error.to_owned(),
        },
    }
}

/// The runtime's version when the window can open with it, else what keeps it from opening.
pub fn check(webview: Result<String, String>) -> Result<String, Problem> {
    check_on(webview, cfg!(windows))
}

/// [`check`] on Windows (`windows`: the version is WebView2's, held to [`MIN_WEBVIEW2`]; one that
/// is empty or doesn't read as one is no runtime) or elsewhere (WebKit's: the loader has checked
/// the library already).
pub fn check_on(webview: Result<String, String>, windows: bool) -> Result<String, Problem> {
    match webview {
        Ok(version) if windows => match read_version(&version) {
            None if version.trim().is_empty() => {
                Err(Problem::Missing("the loader reports no version".into()))
            }
            None => Err(Problem::Missing(format!(
                "the loader reports version {:?}",
                version.trim()
            ))),
            Some(v) if v < MIN_WEBVIEW2 => Err(Problem::TooOld(version)),
            Some(_) => Ok(version),
        },
        Ok(version) => Ok(version),
        Err(detail) => Err(Problem::Missing(detail)),
    }
}

/// The exit code and the line for what the web view's runtime said (its version, or why not).
pub fn answer(webview: Result<String, String>) -> (i32, Value) {
    answer_on(webview, cfg!(windows))
}

/// [`answer`] on Windows or elsewhere, as [`check_on`] tells them apart.
pub fn answer_on(webview: Result<String, String>, windows: bool) -> (i32, Value) {
    match check_on(webview, windows) {
        Ok(version) => (0, json!({"ok": true, "webview": version})),
        Err(problem) => (1, json!({"ok": false, "error": problem.error(windows)})),
    }
}

/// A WebView2 version ("131.0.2903.112", maybe with a channel after it) as four numbers; None when
/// it doesn't read as one (empty, or not numbers).
fn read_version(version: &str) -> Option<[u32; 4]> {
    let first = version.split_whitespace().next()?;
    let parts: Vec<u32> = first
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut v = [0; 4];
    v[..parts.len()].copy_from_slice(&parts);
    Some(v)
}

/// The oldest runtime the window opens with, as Microsoft writes versions.
pub fn min_webview2() -> String {
    MIN_WEBVIEW2.map(|n| n.to_string()).join(".")
}

/// Why the window can't open with this WebView2 Runtime (the app shows it: 「无法启动更新：…」).
fn old_webview(version: &str) -> String {
    let min = min_webview2();
    format!(
        "SpaceKnife needs the Microsoft Edge WebView2 Runtime {min} or later; this computer has \
         {version}. Microsoft offers the current one for free: {}",
        dialog::WEBVIEW2_PAGE
    )
}

/// Why the window can't open, as the app that asked shows it to the user (「无法启动更新：…」):
/// of the loader's error its HRESULT, when it has one (the rest is Rust's debug form).
fn no_webview(detail: &str, windows: bool) -> String {
    let detail = short_detail(detail);
    if windows {
        format!(
            "SpaceKnife needs the Microsoft Edge WebView2 Runtime, which is not installed \
             ({detail}); Microsoft offers it for free: {}",
            dialog::WEBVIEW2_PAGE
        )
    } else {
        format!("the system's web view can't be found ({detail})")
    }
}

/// Of the loader's error its HRESULT ("error 0x80070002"), when it has one; else the words as
/// they are.
pub fn short_detail(detail: &str) -> String {
    detail
        .split_once("HRESULT(")
        .and_then(|(_, rest)| rest.split_once(')'))
        .map_or_else(|| detail.to_owned(), |(code, _)| format!("error {code}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_ok_with_the_web_views_version() {
        assert_eq!(
            answer(Ok("131.0.2903.112".into())),
            (0, json!({"ok": true, "webview": "131.0.2903.112"}))
        );
    }

    #[test]
    fn a_webview2_runtime_too_old_for_the_window_is_refused() {
        let ok = |v: &str| answer_on(Ok(v.into()), true).0 == 0;
        assert!(ok("89.0.774.44"), "the oldest that has what wry needs");
        assert!(ok("89.0.774.45") && ok("89.1") && ok("142.0.3595.80"));
        assert!(ok("131.0.2903.112 beta"), "a channel after the number");
        assert!(!ok("89.0.774.43") && !ok("88.0.705.81") && !ok("86.0.616.0"));
        assert_eq!(
            check_on(Ok("88.0.705.81".into()), true),
            Err(Problem::TooOld("88.0.705.81".into()))
        );
        let (code, said) = answer_on(Ok("88.0.705.81".into()), true);
        assert_eq!(code, 1);
        assert_eq!(
            said,
            json!({"ok": false, "error": "SpaceKnife needs the Microsoft Edge WebView2 Runtime \
                89.0.774.44 or later; this computer has 88.0.705.81. Microsoft offers the current \
                one for free: https://developer.microsoft.com/microsoft-edge/webview2/"})
        );
        // WebKit's version elsewhere is not WebView2's
        assert_eq!(answer_on(Ok("2.44.0".into()), false).0, 0);
        // what sk_core's helper makes of it: the program's own words
        assert_eq!(
            sk_core::update::swap::judge_startup(
                "SpaceKnife.exe",
                Some(1),
                None,
                &said.to_string(),
                ""
            ),
            Err(said["error"].as_str().unwrap().to_owned())
        );
    }

    /// Older WebView2 loaders say "no runtime" with an empty version and S_OK; a version that
    /// doesn't read as one is no runtime the window can count on either.
    #[test]
    fn a_webview2_version_that_is_empty_or_unreadable_is_no_runtime() {
        for (version, detail) in [
            ("", "the loader reports no version"),
            ("  ", "the loader reports no version"),
            ("canary", "the loader reports version \"canary\""),
            (
                "131.x.2903.112",
                "the loader reports version \"131.x.2903.112\"",
            ),
            ("1.2.3.4.5", "the loader reports version \"1.2.3.4.5\""),
        ] {
            assert_eq!(
                check_on(Ok(version.into()), true),
                Err(Problem::Missing(detail.into())),
                "{version:?}"
            );
            let (code, said) = answer_on(Ok(version.into()), true);
            assert_eq!(code, 1, "{version:?}");
            assert_eq!(
                said["error"],
                format!(
                    "SpaceKnife needs the Microsoft Edge WebView2 Runtime, which is not \
                     installed ({detail}); Microsoft offers it for free: \
                     https://developer.microsoft.com/microsoft-edge/webview2/"
                )
            );
        }
        // the JSON keeps its two shapes
        let (_, said) = answer_on(Ok(String::new()), true);
        assert_eq!(said.as_object().unwrap().len(), 2);
        assert_eq!(said["ok"], false);
        // WebKit's version is not read
        assert_eq!(check_on(Ok(String::new()), false), Ok(String::new()));
    }

    #[test]
    fn says_why_the_window_cannot_open() {
        // wry's error for a Windows without WebView2: of it the HRESULT
        let wry = r#"WebView2 error: WindowsError(Error { code: HRESULT(0x80070002), message: "The system cannot find the file specified." })"#;
        let (code, said) = answer(Err(wry.into()));
        assert_eq!(code, 1);
        assert_eq!(said["ok"], false);
        let error = said["error"].as_str().unwrap();
        if cfg!(windows) {
            assert_eq!(
                error,
                "SpaceKnife needs the Microsoft Edge WebView2 Runtime, which is not installed (error \
                 0x80070002); Microsoft offers it for free: \
                 https://developer.microsoft.com/microsoft-edge/webview2/"
            );
        } else {
            assert_eq!(
                error,
                "the system's web view can't be found (error 0x80070002)"
            );
        }
        assert_eq!(check(Err(wry.into())), Err(Problem::Missing(wry.into())));
        let (_, other) = answer(Err("no WebKit bundle".into()));
        assert!(
            other["error"]
                .as_str()
                .unwrap()
                .contains("(no WebKit bundle)")
        );
        // what sk_core's helper makes of it: the program's own words
        let line = said.to_string();
        assert_eq!(
            sk_core::update::swap::judge_startup("SpaceKnife.exe", Some(1), None, &line, ""),
            Err(error.to_owned())
        );
    }

    #[test]
    fn a_window_that_failed_and_the_unknown_are_said_in_a_line() {
        let failed = Problem::WindowFailed {
            webview: Some("120.0.2210.91".into()),
            error: "WebView2 error: E_NOINTERFACE".into(),
        };
        assert_eq!(
            failed.error(true),
            "SpaceKnife could not open its window (web view 120.0.2210.91): WebView2 error: \
             E_NOINTERFACE"
        );
        let failed = Problem::WindowFailed {
            webview: None,
            error: "no display".into(),
        };
        assert_eq!(
            failed.error(false),
            "SpaceKnife could not open its window: no display"
        );
        assert_eq!(
            Problem::Unknown.error(true),
            "SpaceKnife can't start on this computer"
        );
    }

    #[test]
    fn the_flags_differ() {
        assert_ne!(STARTUP_PROBLEM, sk_core::update::swap::STARTUP_CHECK);
        assert_eq!(min_webview2(), "89.0.774.44");
    }
}
