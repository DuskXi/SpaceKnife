//! `SpaceKnife --startup-check` (sk_core::update::swap::STARTUP_CHECK): the question the swap
//! helper of a portable copy's new version asks this program before it puts it in place. By the
//! time main runs, the system's loader has given the program every library it links (WebKitGTK 4.1
//! and GTK on Linux, the system's frameworks on macOS; the C runtime is linked in on Windows); what
//! is left to ask is the web view's runtime, which Windows may lack: WebView2's loader reports the
//! installed runtime's version (GetAvailableCoreWebView2BrowserVersionString), or that there is
//! none; a runtime older than [`MIN_WEBVIEW2`] can't open the window either. One line of JSON on
//! stdout; exit 0 when the window can open, 1 when it can't. Nothing else
//! happens: no window or web view, no setting, file or log of the app (main.rs answers it first).

use std::io::Write;

use serde_json::{Value, json};

/// The oldest WebView2 Runtime the window opens with. wry gives the web view its background colour
/// through ICoreWebView2Controller2 and fails the window without it (window.rs always sets one);
/// that interface is "Stable in Release" since WebView2 SDK 1.0.774.44, which Microsoft's release
/// notes pair with Runtime 89.0.774.44. Every other interface wry asks for while making the window
/// is older, or optional (it carries on without).
pub const MIN_WEBVIEW2: [u32; 4] = [89, 0, 774, 44];

/// Answer the check on stdout; the exit code.
pub fn run() -> i32 {
    let (code, said) = answer(tauri::webview_version().map_err(|e| e.to_string()));
    // a reader that went away is no reason to fail differently
    let _ = writeln!(std::io::stdout(), "{said}");
    let _ = std::io::stdout().flush();
    code
}

/// The exit code and the line for what the web view's runtime said (its version, or why not).
pub fn answer(webview: Result<String, String>) -> (i32, Value) {
    answer_on(webview, cfg!(windows))
}

/// [`answer`] on Windows (`windows`: the version is WebView2's, held to [`MIN_WEBVIEW2`]) or
/// elsewhere (WebKit's: the loader has checked the library already).
pub fn answer_on(webview: Result<String, String>, windows: bool) -> (i32, Value) {
    match webview {
        Ok(version) if windows && older_than_needed(&version) => {
            (1, json!({"ok": false, "error": old_webview(&version)}))
        }
        Ok(version) => (0, json!({"ok": true, "webview": version})),
        Err(detail) => (
            1,
            json!({"ok": false, "error": no_webview(&detail, windows)}),
        ),
    }
}

/// Whether a WebView2 version ("131.0.2903.112", maybe with a channel after it) is older than
/// [`MIN_WEBVIEW2`]. A version that doesn't read as numbers is not held against the runtime.
fn older_than_needed(version: &str) -> bool {
    let first = version.split_whitespace().next().unwrap_or_default();
    let parts: Option<Vec<u32>> = first.split('.').map(|p| p.parse().ok()).collect();
    match parts {
        Some(mut v) if !v.is_empty() && v.len() <= 4 => {
            v.resize(4, 0);
            v.as_slice() < MIN_WEBVIEW2.as_slice()
        }
        _ => false,
    }
}

/// Why the window can't open with this WebView2 Runtime (the app shows it: 「无法启动更新：…」).
fn old_webview(version: &str) -> String {
    let min = MIN_WEBVIEW2.map(|n| n.to_string()).join(".");
    format!(
        "SpaceKnife needs the Microsoft Edge WebView2 Runtime {min} or later; this computer has \
         {version}. Microsoft offers the current one for free: \
         https://developer.microsoft.com/microsoft-edge/webview2/"
    )
}

/// Why the window can't open, as the app that asked shows it to the user (「无法启动更新：…」):
/// of the loader's error its HRESULT, when it has one (the rest is Rust's debug form).
fn no_webview(detail: &str, windows: bool) -> String {
    let detail = detail
        .split_once("HRESULT(")
        .and_then(|(_, rest)| rest.split_once(')'))
        .map_or_else(|| detail.to_owned(), |(code, _)| format!("error {code}"));
    if windows {
        format!(
            "SpaceKnife needs the Microsoft Edge WebView2 Runtime, which is not installed \
             ({detail}); Microsoft offers it for free: \
             https://developer.microsoft.com/microsoft-edge/webview2/"
        )
    } else {
        format!("the system's web view can't be found ({detail})")
    }
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
        assert!(ok("canary"), "not a version: not held against it");
        assert!(!ok("89.0.774.43") && !ok("88.0.705.81") && !ok("86.0.616.0"));
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
}
