//! `SpaceKnife --startup-check` (sk_core::update::swap::STARTUP_CHECK): the question the swap
//! helper of a portable copy's new version asks this program before it puts it in place. By the
//! time main runs, the system's loader has given the program every library it links (WebKitGTK 4.1
//! and GTK on Linux, the system's frameworks on macOS; the C runtime is linked in on Windows); what
//! is left to ask is the web view's runtime, which Windows may lack: WebView2's loader reports the
//! installed runtime's version (GetAvailableCoreWebView2BrowserVersionString), or that there is
//! none. One line of JSON on stdout; exit 0 when the window can open, 1 when it can't. Nothing else
//! happens: no window or web view, no setting, file or log of the app (main.rs answers it first).

use std::io::Write;

use serde_json::{Value, json};

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
    match webview {
        Ok(version) => (0, json!({"ok": true, "webview": version})),
        Err(detail) => (1, json!({"ok": false, "error": no_webview(&detail)})),
    }
}

/// Why the window can't open, as the app that asked shows it to the user (「无法启动更新：…」):
/// of the loader's error its HRESULT, when it has one (the rest is Rust's debug form).
fn no_webview(detail: &str) -> String {
    let detail = detail
        .split_once("HRESULT(")
        .and_then(|(_, rest)| rest.split_once(')'))
        .map_or_else(|| detail.to_owned(), |(code, _)| format!("error {code}"));
    if cfg!(windows) {
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
