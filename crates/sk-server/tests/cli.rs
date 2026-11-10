//! `spaceknife-cli` run as a user runs it (the built program, `CARGO_BIN_EXE_spaceknife-cli`):
//! headless mode's command line in both languages and in 0.3.4's forms, what it prints while it
//! runs, what it reports and where it keeps its data, and output that goes nowhere (a closed pipe,
//! NUL / /dev/null).
//!
//! Every run is isolated: SPACEKNIFE_HOME (or, where the test needs it unset, every platform folder
//! SpaceKnife could fall back to) points into a temporary folder, updates are looked for on a dead
//! loopback address, and servers listen on 127.0.0.1 only.

mod common;

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;

const CLI: &str = env!("CARGO_BIN_EXE_spaceknife-cli");

/// A temporary folder next to the built program (hard links to it work there).
fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("sk-cli-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

/// The program at `exe` with nothing of this machine's SpaceKnife in reach: the platform folders
/// lead into `root`, `SPACEKNIFE_HOME` is unset, the language is `lang`.
fn isolated(exe: &Path, root: &Path, lang: &str) -> Command {
    let user = root.join("user");
    let mut c = Command::new(exe);
    c.env("SPACEKNIFE_LANGUAGE", lang)
        .env("SPACEKNIFE_UPDATE_FEED", "http://127.0.0.1:9/")
        .env("NO_COLOR", "1")
        .env("USERPROFILE", &user)
        .env("HOME", &user)
        .env("LOCALAPPDATA", user.join("local"))
        .env("APPDATA", user.join("roaming"))
        .env("XDG_DATA_HOME", user.join("share"))
        .env("XDG_CONFIG_HOME", user.join("config"))
        .env("XDG_CACHE_HOME", user.join("cache"))
        .env_remove("SPACEKNIFE_HOME")
        .env_remove("SPACEKNIFE_APP_VERSION")
        .env_remove("SPACEKNIFE_INSTALL_KIND")
        .env_remove("SPACEKNIFE_SYSTEM_LOCALE")
        .env_remove("APPIMAGE")
        .env_remove("APPDIR")
        .stdin(Stdio::null());
    c
}

/// [`isolated`] with its own SPACEKNIFE_HOME (`root/home`).
fn cli(root: &Path, lang: &str) -> Command {
    let mut c = isolated(Path::new(CLI), root, lang);
    c.env("SPACEKNIFE_HOME", root.join("home"));
    c
}

/// A built interface for headless mode: a page with an inline script.
fn ui_in(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("index.html"),
        "<!doctype html><title>SpaceKnife</title><script>window.x=1</script>",
    )
    .unwrap();
    dir.to_path_buf()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

/// Run to the end with stdout and stderr captured: (exit code, stdout, stderr).
fn run(mut c: Command) -> (Option<i32>, String, String) {
    let out = c
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    (out.status.code(), text(&out.stdout), text(&out.stderr))
}

/// Lines a running program printed, stdout and stderr together, as they come.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<String>>>);

impl Lines {
    fn follow(&self, stream: impl std::io::Read + Send + 'static) {
        let lines = self.0.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                lines.lock().unwrap().push(line);
            }
        });
    }

    fn all(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    fn find<T>(&self, f: impl Fn(&str) -> Option<T>) -> Option<T> {
        self.0.lock().unwrap().iter().find_map(|l| f(l))
    }
}

/// Headless mode running: the address and token it printed.
struct Headless {
    child: Child,
    lines: Lines,
    port: u16,
    token: String,
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

impl Headless {
    /// Start it and wait for the address it prints (`http(s)://localhost:PORT/#token=…`).
    fn start(mut c: Command) -> Headless {
        let mut child = c
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let lines = Lines::default();
        lines.follow(child.stdout.take().unwrap());
        lines.follow(child.stderr.take().unwrap());
        let re =
            regex::Regex::new(r"https?://localhost:(\d+)/#token=([A-Za-z0-9_.!~*'()%-]+)").unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some((port, token)) = lines.find(|l| {
                re.captures(l)
                    .map(|c| (c[1].parse::<u16>().unwrap(), percent_decode(&c[2])))
            }) {
                return Headless {
                    child,
                    lines,
                    port,
                    token,
                };
            }
            if let Some(status) = child.try_wait().unwrap() {
                std::thread::sleep(Duration::from_millis(200));
                panic!(
                    "headless mode ended ({status}) before printing its address:\n{}",
                    lines.all().join("\n")
                );
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("no address printed:\n{}", lines.all().join("\n"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn ws(&self) -> String {
        format!("ws://127.0.0.1:{}/v1", self.port)
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Wait (without blocking the runtime) until a printed line matches.
    async fn wait_line(&self, what: &str, f: impl Fn(&str) -> bool) -> String {
        for _ in 0..200 {
            if let Some(l) = self.lines.find(|l| f(l).then(|| l.to_owned())) {
                return l;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no line {what}:\n{}", self.lines.all().join("\n"));
    }
}

impl Drop for Headless {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ------------------------------------------------------------------ output that goes nowhere

/// `spaceknife-cli version | head -0` (a reader that went away): printing fails, the program
/// doesn't (Rust's `println!` panics there, exit 101).
#[test]
fn version_survives_a_closed_stdout() {
    let root = scratch();
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let child = cli(root.path(), "en")
        .arg("version")
        .stdout(writer)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}

/// Headless mode with its stdout closed keeps serving (it used to panic on its first line).
#[test]
fn headless_survives_a_closed_stdout() {
    let root = scratch();
    let ui = ui_in(&root.path().join("ui"));
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut c = cli(root.path(), "en");
    c.args(["headless", "--listen", &format!("127.0.0.1:{port}"), "--ui"])
        .arg(&ui)
        .stdout(writer)
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    let lines = Lines::default();
    lines.follow(child.stderr.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(60);
    let served = loop {
        if let Some(status) = child.try_wait().unwrap() {
            std::thread::sleep(Duration::from_millis(200));
            panic!("ended with {status}:\n{}", lines.all().join("\n"));
        }
        if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            use std::io::{Read as _, Write as _};
            let _ = s.write_all(
                format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            );
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            if buf.starts_with(b"HTTP/1.1 200") {
                break true;
            }
        }
        if Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = child.kill();
    let _ = child.wait();
    assert!(served, "{}", lines.all().join("\n"));
}

/// `spaceknife-cli scan <folder> > NUL` (`>/dev/null`): 0.3.4's Python command line took NUL
/// for a terminal and failed without its snapshot (fix/cli-nul-console). The scan ends with 0 and
/// the snapshot is saved.
#[test]
fn scan_with_stdout_to_the_null_device_saves_its_snapshot() {
    let root = scratch();
    let facts = build_tree(&root.path().join("tree"));
    let out = cli(root.path(), "en")
        .arg("scan")
        .arg(&facts.root)
        .args(["--top", "3"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(!err.contains("panicked"), "{err}");
    let snapshots: Vec<PathBuf> = walk(&root.path().join("home"))
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "sks"))
        .collect();
    assert_eq!(snapshots.len(), 1, "{snapshots:?}");
    assert!(std::fs::metadata(&snapshots[0]).unwrap().len() > 0);
    // and it lists, as a saved snapshot does
    let mut list = cli(root.path(), "en");
    list.args(["snapshot", "list"]);
    let (code, stdout, _) = run(list);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("1 snapshot in"), "{stdout}");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

// ------------------------------------------------------------------ the command line

/// `--help` and wrong options in the user's language, as 0.3.4 had them (not clap's English).
#[test]
fn headless_help_and_errors_speak_the_users_language() {
    let root = scratch();
    let case = |lang: &str, args: &[&str]| {
        let mut c = cli(root.path(), lang);
        c.args(args);
        run(c)
    };
    let (code, out, _) = case("zh-CN", &["headless", "--help"]);
    assert_eq!(code, Some(0));
    assert!(out.starts_with("用法：spaceknife-cli headless "), "{out}");
    assert!(out.contains("--tls-key <文件>"), "{out}");
    let (code, out, _) = case("en", &["headless", "-h"]);
    assert_eq!(code, Some(0));
    assert!(out.starts_with("Usage: spaceknife-cli headless "), "{out}");
    // 0.3.4's own form: the app's options
    let (code, out, _) = case("zh-CN", &["--headless", "--help"]);
    assert_eq!(code, Some(0));
    assert!(out.contains("[--headless] [--listen <地址>]"), "{out}");
    assert!(out.contains("  --headless  "), "{out}");

    let (code, _, err) = case("zh-CN", &["headless", "--listen"]);
    assert_eq!(code, Some(2));
    assert!(err.contains("--listen 后面要写地址"), "{err}");
    assert!(
        err.contains("运行 spaceknife-cli headless --help 查看所有选项。"),
        "{err}"
    );
    let (code, _, err) = case("en", &["headless", "--listen", "bad_host:1"]);
    assert_eq!(code, Some(2));
    assert!(err.contains("Not an address: bad_host:1"), "{err}");
    let (code, _, err) = case("en", &["headless", "--tls-cert"]);
    assert_eq!(code, Some(2));
    assert!(err.contains("--tls-cert needs a file path"), "{err}");
    let (code, _, err) = case("en", &["headless", "--tls-key", "k.pem"]);
    assert_eq!(code, Some(2));
    assert!(
        err.contains("--tls-cert and --tls-key go together"),
        "{err}"
    );
    let (code, _, err) = case("zh-CN", &["headless", "--bogus"]);
    assert_eq!(code, Some(2));
    assert!(err.contains("无法识别的选项：--bogus"), "{err}");
    // the options of headless mode without it, as 0.3.4 said
    let (code, _, err) = case("zh-CN", &["--yes"]);
    assert_eq!(code, Some(2));
    assert!(
        err.contains("--yes 只用于无头模式：请同时加上 --headless 或 --listen"),
        "{err}"
    );
    // the rest of the command line is unchanged
    let (code, out, _) = case("en", &["--help"]);
    assert_eq!(code, Some(0));
    assert!(out.contains("headless"), "{out}");
}

/// `--yes` anywhere, and 0.3.4's `--headless` / `--listen` anywhere among the arguments, as
/// scripts and service units written for 0.3.4 have them (Chromium's own switches pass by).
#[test]
fn the_old_forms_and_yes_work_anywhere() {
    let root = scratch();
    let ui = ui_in(&root.path().join("ui"));
    let ui = ui.to_str().unwrap();
    for args in [
        vec!["--yes", "--listen", "127.0.0.1:0", "--ui", ui],
        vec!["--ui", ui, "--listen=127.0.0.1:0", "--headless"],
        vec![
            "--no-sandbox",
            "--headless",
            "--ui",
            ui,
            "--listen",
            "127.0.0.1:0",
        ],
        vec!["headless", "--listen", "127.0.0.1:0", "--yes", "--ui", ui],
    ] {
        let mut c = cli(root.path(), "en");
        c.args(&args);
        let h = Headless::start(c);
        assert!(h.port > 0, "{args:?}");
    }
}

// ------------------------------------------------------------------ while it runs

#[tokio::test]
async fn headless_prints_browsers_coming_and_going() {
    let root = scratch();
    let ui = ui_in(&root.path().join("ui"));
    let mut c = cli(root.path(), "en");
    c.args(["headless", "--listen", "127.0.0.1:0", "--ui"])
        .arg(&ui);
    let h = Headless::start(c);
    let time = regex::Regex::new(r"^\d\d:\d\d:\d\d ").unwrap();
    let ws = raw(&h.ws(), Some(&h.origin()), true).await.unwrap();
    let line = h
        .wait_line("saying a browser connected", |l| {
            l.ends_with("127.0.0.1 connected")
        })
        .await;
    assert!(time.is_match(&line), "{line}");
    drop(ws);
    let line = h
        .wait_line("saying it went away", |l| {
            l.ends_with("127.0.0.1 disconnected")
        })
        .await;
    assert!(time.is_match(&line), "{line}");

    // in Chinese as 0.3.4 had it
    let root = scratch();
    let mut c = cli(root.path(), "zh-CN");
    c.args(["headless", "--listen", "127.0.0.1:0", "--ui"])
        .arg(&ui);
    let h = Headless::start(c);
    let ws = raw(&h.ws(), Some(&h.origin()), true).await.unwrap();
    h.wait_line("已连接", |l| l.ends_with("127.0.0.1 已连接"))
        .await;
    drop(ws);
    h.wait_line("已断开", |l| l.ends_with("127.0.0.1 已断开"))
        .await;
}

/// The version the launcher names (SPACEKNIFE_APP_VERSION, as the app's own shell passes it), in
/// the banner and in /sk/info, and the kind of install it names, for the update checks.
#[tokio::test]
async fn headless_reports_the_apps_version_and_install() {
    let root = scratch();
    let ui = ui_in(&root.path().join("ui"));
    let mut c = cli(root.path(), "en");
    c.env("SPACEKNIFE_APP_VERSION", "7.7.7-test")
        .env("SPACEKNIFE_INSTALL_KIND", "deb")
        .args(["headless", "--listen", "127.0.0.1:0", "--ui"])
        .arg(&ui);
    let h = Headless::start(c);
    h.wait_line("with the version", |l| {
        l == "SpaceKnife headless mode · 7.7.7-test"
    })
    .await;
    let bearer = format!("Bearer {}", h.token);
    let (status, _, body) = http(h.port, "/sk/info", &[("Authorization", &bearer)]).await;
    assert_eq!(status, 200);
    let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(info["version"], "7.7.7-test");
    let client = Client::open_with(&h.ws(), &h.token, all_caps(), Some(&h.origin()))
        .await
        .unwrap();
    let st = client.ok("update.status", json!({})).await;
    assert_eq!(st["current"], "7.7.7-test");
    assert_eq!(st["install_kind"], "deb");

    // without them: this build's version
    let root = scratch();
    let mut c = cli(root.path(), "en");
    c.args(["headless", "--listen", "127.0.0.1:0", "--ui"])
        .arg(&ui);
    let h = Headless::start(c);
    let banner = format!("SpaceKnife headless mode · {}", env!("CARGO_PKG_VERSION"));
    h.wait_line("with the version", |l| l == banner).await;
}

/// A portable copy (a `data` folder next to the program): headless mode keeps everything there, as
/// 0.3.4's `SpaceKnife --headless` did, and reports the install as portable.
#[tokio::test]
async fn a_portable_copy_keeps_its_headless_data_next_to_it() {
    let root = scratch();
    let exe_name = if cfg!(windows) {
        "spaceknife-cli.exe"
    } else {
        "spaceknife-cli"
    };
    // the app folder: macOS has the program in the app bundle, the data folder next to it
    let (app, bin, ui_dir) = if cfg!(target_os = "macos") {
        let app = root.path().join("SpaceKnife-portable");
        let contents = app.join("SpaceKnife.app").join("Contents");
        (
            app.clone(),
            contents.join("MacOS"),
            contents.join("Resources").join("ui"),
        )
    } else {
        let app = root.path().join("SpaceKnife-portable");
        (app.clone(), app.clone(), app.join("ui"))
    };
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(app.join("data")).unwrap();
    ui_in(&ui_dir);
    let exe = bin.join(exe_name);
    if std::fs::hard_link(CLI, &exe).is_err() {
        std::fs::copy(CLI, &exe).unwrap();
    }
    let mut c = isolated(&exe, root.path(), "en");
    c.args(["--headless", "--listen", "127.0.0.1:0"]);
    let h = Headless::start(c);
    let client = Client::open_with(&h.ws(), &h.token, all_caps(), Some(&h.origin()))
        .await
        .unwrap();
    let st = client.ok("update.status", json!({})).await;
    assert_eq!(st["install_kind"], "portable");
    // its log is in the copy's data folder, nothing went to the user's folders
    let log = app
        .join("data")
        .join("data")
        .join("logs")
        .join("spaceknife.log");
    for _ in 0..100 {
        if log.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(log.is_file(), "{:?}", walk(&app));
    let elsewhere = walk(&root.path().join("user"));
    assert!(elsewhere.is_empty(), "{elsewhere:?}");
}

/// HTTPS with PEM files (tests/tls); what rustls can't read is said plainly before anything
/// listens, with the way out (an encrypted key, a certificate given as the key).
#[test]
fn headless_takes_pem_files_and_says_what_it_cannot_read() {
    let tls = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("tls");
    let root = scratch();
    let ui = ui_in(&root.path().join("ui"));
    let start = |lang: &str, cert: &str, key: &str| {
        let mut c = cli(root.path(), lang);
        c.args(["headless", "--listen", "127.0.0.1:0", "--tls-cert"])
            .arg(tls.join(cert))
            .arg("--tls-key")
            .arg(tls.join(key))
            .arg("--ui")
            .arg(&ui);
        c
    };
    let h = Headless::start(start("en", "cert.pem", "key.pem"));
    assert!(
        h.lines
            .find(|l| l.contains("https://localhost:").then_some(()))
            .is_some(),
        "{:?}",
        h.lines.all()
    );
    drop(h);
    let (code, _, err) = run(start("zh-CN", "cert.pem", "key-encrypted.pem"));
    assert_eq!(code, Some(2), "{err}");
    assert!(
        err.contains("是加密的") && err.contains("openssl pkey -in"),
        "{err}"
    );
    let (code, _, err) = run(start("en", "cert.pem", "cert.pem"));
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("There is no PEM private key in"), "{err}");
    let (code, _, err) = run(start("en", "key.pem", "key.pem"));
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("-----BEGIN CERTIFICATE-----"), "{err}");
    let (code, _, err) = run(start("en", "cert.pem", "missing.pem"));
    assert_eq!(code, Some(2), "{err}");
    assert!(
        err.contains("Can't read the certificate or the key"),
        "{err}"
    );
}
