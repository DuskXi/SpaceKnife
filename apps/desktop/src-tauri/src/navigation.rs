//! What the window may show (review issue 14): only the interface's own pages. A link, a form or a
//! script that tries to take the window somewhere else is stopped (window.rs `on_navigation`), and
//! a new window is never opened (`on_new_window`): a page of the project goes to the browser
//! instead, as the Electron shell's will-navigate and setWindowOpenHandler do.

use url::Url;

/// Whether the window may navigate to `url`: the interface Tauri serves (tauri://localhost on macOS
/// and Linux, http(s)://tauri.localhost on Windows, never with another port), or in development the
/// dev server's origin (`dev`).
pub fn is_own_page(url: &Url, dev: Option<&Url>) -> bool {
    let served = match (url.scheme(), url.host_str()) {
        ("tauri", Some("localhost")) | ("http" | "https", Some("tauri.localhost")) => {
            url.port().is_none() && url.username().is_empty() && url.password().is_none()
        }
        _ => false,
    };
    served || dev.is_some_and(|d| d.origin() == url.origin() && url.origin().is_tuple())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn the_interface_tauri_serves_is_the_windows_own() {
        for u in [
            "tauri://localhost",
            "tauri://localhost/",
            "tauri://localhost/index.html",
            "http://tauri.localhost/",
            "http://tauri.localhost/_nuxt/entry.js#x",
            "https://tauri.localhost/index.html",
        ] {
            assert!(is_own_page(&url(u), None), "{u}");
        }
    }

    #[test]
    fn anything_else_is_stopped() {
        for u in [
            "https://github.com/DuskXi/SpaceKnife/releases",
            "https://spaceknife.kaltsit.dev/docs/",
            "http://localhost:3000/",
            "http://tauri.localhost:8080/",
            "http://tauri.localhost.evil.example/",
            "http://evil.example/?tauri.localhost",
            "tauri://evil/",
            "tauri://localhost:9/",
            "file:///C:/Windows/System32/calc.exe",
            "file:///etc/passwd",
            "data:text/html,<script>alert(1)</script>",
            "javascript:alert(1)",
            "ipc://localhost/",
            "about:blank",
        ] {
            assert!(!is_own_page(&url(u), None), "{u}");
        }
    }

    #[test]
    fn the_dev_server_in_development_only() {
        let dev = url("http://localhost:3000/");
        assert!(is_own_page(
            &url("http://localhost:3000/index.html"),
            Some(&dev)
        ));
        assert!(!is_own_page(&url("http://localhost:3001/"), Some(&dev)));
        assert!(!is_own_page(&url("https://localhost:3000/"), Some(&dev)));
        assert!(!is_own_page(&url("http://localhost:3000/"), None));
    }
}
