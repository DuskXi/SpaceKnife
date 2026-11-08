//! A stand-in for the app's program in the swap's tests (src/update/swap.rs).
//!
//! Asked `--startup-check`, it answers as the file `fake-app-mode.txt` next to it says (`ok` when
//! there is none):
//!
//! - `ok`: `{"ok": true, "webview": "fake 1.0"}`, exit 0
//! - `no-webview`: `{"ok": false, "error": "…WebView2…"}`, exit 1 (the Tauri app without WebView2)
//! - `silent`: nothing, exit 0 (a program that doesn't know the flag)
//! - `loader`: what Linux's loader says without WebKitGTK 4.1, exit 127
//! - `hang`: no answer for a minute
//! - `noisy`: the `ok` answer, then 1 MiB more on stdout and on stderr
//! - `bind-now`: the `ok` answer with the LD_BIND_NOW it was given (`unset` when none)
//! - `held`: the `ok` answer, exit 0 at once, leaving a process of its own that holds the output
//!   it inherited for 5 s (as Chromium's zygote and crash handler do after the Electron shell's
//!   program has ended)
//!
//! `--hold <seconds>`: that process (sleeps, holding its standard output and error).
//!
//! Started as the swap starts the new version (`--updated`), it runs as the file
//! `fake-app-run.txt` next to it says (`ready` when there is none), and notes its process id in
//! `fake-app-pid.txt` there first:
//!
//! - `ready`: writes its connection info to `SPACEKNIFE_READY_FILE` as the app does once its
//!   backend listens (a `.tmp` file renamed into place), exits 0 shortly after
//! - `crash`: exit 1 at once, nothing written
//! - `hang`: runs for a minute without a word
//! - `quit`: exit 0 at once, nothing written (it chose to)
//!
//! Anything else: it says so and ends with 3.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

fn beside(name: &str) -> Option<std::path::PathBuf> {
    std::env::current_exe().ok().map(|p| p.with_file_name(name))
}

fn mode(file: &str, default: &str) -> String {
    beside(file)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|m| m.trim().to_owned())
        .unwrap_or_else(|| default.to_owned())
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--startup-check") => startup_check(&mode("fake-app-mode.txt", "ok")),
        Some("--updated") => run(&mode("fake-app-run.txt", "ready")),
        Some("--hold") => {
            let secs = std::env::args()
                .nth(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(5);
            std::thread::sleep(Duration::from_secs(secs));
        }
        _ => {
            eprintln!(
                "fake_app: not a startup check: {:?}",
                std::env::args().collect::<Vec<_>>()
            );
            std::process::exit(3);
        }
    }
}

fn run(mode: &str) {
    if let Some(p) = beside("fake-app-pid.txt") {
        let _ = std::fs::write(p, std::process::id().to_string());
    }
    match mode {
        "ready" => {
            if let Some(file) = std::env::var_os("SPACEKNIFE_READY_FILE") {
                let file = Path::new(&file);
                let tmp = file.with_extension("json.tmp");
                let info = format!(
                    r#"{{"event": "ready", "url": "ws://127.0.0.1:1/", "pid": {}, "token": "fake"}}"#,
                    std::process::id()
                );
                std::fs::write(&tmp, info).unwrap();
                std::fs::rename(&tmp, file).unwrap();
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        "crash" => std::process::exit(1),
        "hang" => std::thread::sleep(Duration::from_secs(60)),
        "quit" => {}
        other => {
            eprintln!("fake_app: no run mode {other}");
            std::process::exit(4);
        }
    }
}

fn startup_check(mode: &str) {
    match mode {
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
        "hang" => std::thread::sleep(Duration::from_secs(60)),
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
        "held" => {
            println!(r#"{{"ok": true, "webview": "fake 1.0"}}"#);
            let _ = std::io::stdout().flush();
            // stdio inherited: the pipes of whoever asked stay open while it runs
            if let Ok(me) = std::env::current_exe() {
                let _ = std::process::Command::new(me).args(["--hold", "5"]).spawn();
            }
        }
        other => {
            eprintln!("fake_app: no mode {other}");
            std::process::exit(4);
        }
    }
}
