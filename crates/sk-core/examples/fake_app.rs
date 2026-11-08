//! A stand-in for the app's program in the swap's tests (src/update/swap.rs): asked
//! `--startup-check`, it answers as the file `fake-app-mode.txt` next to it says (`ok` when there
//! is none). Anything else: it says so and ends with 3.
//!
//! - `ok`: `{"ok": true, "webview": "fake 1.0"}`, exit 0
//! - `no-webview`: `{"ok": false, "error": "…WebView2…"}`, exit 1 (the Tauri app without WebView2)
//! - `silent`: nothing, exit 0 (a program that doesn't know the flag)
//! - `loader`: what Linux's loader says without WebKitGTK 4.1, exit 127
//! - `hang`: no answer for a minute
//! - `noisy`: the `ok` answer, then 1 MiB more on stdout and on stderr
//! - `bind-now`: the `ok` answer with the LD_BIND_NOW it was given (`unset` when none)

use std::io::Write;

fn main() {
    let mode = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::read_to_string(p.with_file_name("fake-app-mode.txt")).ok())
        .map(|m| m.trim().to_owned())
        .unwrap_or_else(|| "ok".to_owned());
    if std::env::args().nth(1).as_deref() != Some("--startup-check") {
        eprintln!(
            "fake_app: not a startup check: {:?}",
            std::env::args().collect::<Vec<_>>()
        );
        std::process::exit(3);
    }
    match mode.as_str() {
        "ok" => println!(r#"{{"ok": true, "webview": "fake 1.0"}}"#),
        "no-webview" => {
            println!(
                r#"{{"ok": false, "error": "SpaceKnife needs the Microsoft Edge WebView2 Runtime, which is not installed"}}"#
            );
            std::process::exit(1);
        }
        "silent" => {}
        "loader" => {
            eprintln!(
                "/opt/SpaceKnife/spaceknife: error while loading shared libraries: libwebkit2gtk-4.1.so.0: cannot open shared object file: No such file or directory"
            );
            std::process::exit(127);
        }
        "hang" => std::thread::sleep(std::time::Duration::from_secs(60)),
        "noisy" => {
            let junk = vec![b'x'; 1 << 20];
            let _ = std::io::stderr().write_all(&junk);
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(br#"{"ok": true, "webview": "fake 1.0"}"#);
            let _ = out.write_all(b"\n");
            let _ = out.write_all(&junk);
        }
        "bind-now" => {
            let bind = std::env::var("LD_BIND_NOW").unwrap_or_else(|_| "unset".to_owned());
            println!(r#"{{"ok": true, "bind_now": "{bind}"}}"#);
        }
        other => {
            eprintln!("fake_app: no mode {other}");
            std::process::exit(4);
        }
    }
}
