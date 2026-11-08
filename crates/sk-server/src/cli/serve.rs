//! `serve`: the WebSocket backend for a UI (port of `cli.py serve` and `server/runner.py`).
//!
//! Contract with the launching process (docs/protocol.md §1):
//!
//! 1. With `--token-stdin` the first stdin line is the session token (never argv/env, which other
//!    local users can read). Without it a random token is generated and included in the ready line.
//! 2. Once listening, exactly one JSON line is written to **stdout**:
//!    `{"event": "ready", "url": "ws://127.0.0.1:PORT/v1", "host": …, "port": PORT, "pid": …,
//!    "protocol": "1.1", "subprotocol": "spaceknife.v1", "updates_dir": …}`. All logging goes to stderr.
//! 3. The backend exits gracefully when stdin reaches EOF (the parent closed the pipe or died), on
//!    `app.shutdown`, on SIGTERM/SIGINT (Ctrl+C), or — on Linux — when the parent dies
//!    (PR_SET_PDEATHSIG); macOS polls `getppid()`.

use std::io::{BufRead as _, Write as _};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use serde_json::Value;

use crate::{ServeOptions, Tuning};

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Interface to bind; keep loopback unless you know why.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    /// TCP port; 0 picks a free port (reported in the ready line).
    #[arg(long, default_value_t = 0)]
    pub port: u16,
    /// Read the session token from the first stdin line; exit on stdin EOF.
    #[arg(long)]
    pub token_stdin: bool,
    /// Extra allowed WebSocket Origin (repeatable), e.g. a dev server URL.
    #[arg(long = "allow-origin")]
    pub allow_origin: Vec<String>,
    /// Seconds a client has to send session.hello.
    #[arg(long, default_value_t = 5.0)]
    pub hello_timeout: f64,
    /// TRACE, DEBUG, INFO, WARNING, ERROR.
    #[arg(long, default_value = "INFO")]
    pub log_level: String,
    /// Log format on stderr: plain or json.
    #[arg(long, default_value = "plain")]
    pub log_format: String,
    /// Also write the ready info, including the token, to this file (owner-only; removed on exit).
    #[arg(long)]
    pub ready_file: Option<PathBuf>,
    /// The running app's version (update checks).
    #[arg(long)]
    pub app_version: Option<String>,
    /// How the app was installed: nsis, portable, appimage, deb, rpm, dmg.
    #[arg(long)]
    pub install_kind: Option<String>,
    /// The system's locale as the launcher read it (the interface language «system»).
    #[arg(long)]
    pub system_locale: Option<String>,
}

/// Logging of a long-running backend: stderr at `level`, the log bus, the rotating log file.
pub fn init_logging(level: &str, format: &str) {
    use sk_core::log::{LogBus, LogConfig, LogFormat, parse_level};
    let level = parse_level(level).unwrap_or(tracing::level_filters::LevelFilter::INFO);
    let format = LogFormat::parse(format).unwrap_or(LogFormat::Plain);
    let file = Some((
        sk_core::paths::log_file(),
        tracing::level_filters::LevelFilter::INFO,
    ));
    if let Err(e) = sk_core::log::init(&LogConfig {
        level,
        format,
        bus: Some(LogBus::global()),
        file,
    }) {
        super::eprint_line(&format!("log file unavailable: {e}"));
    }
}

/// The token from the first line of stdin (at least 16 characters).
pub fn read_token_line(input: &mut dyn std::io::BufRead) -> Result<String, String> {
    let mut line = Vec::new();
    let _ = input.read_until(b'\n', &mut line);
    let token = String::from_utf8_lossy(&line).trim().to_owned();
    if token.chars().count() < 16 {
        return Err("expected a token of at least 16 characters on the first stdin line".into());
    }
    Ok(token)
}

/// A JSON object as Python's `json.dumps` writes it: keys in order, `", "` and `": "`.
pub fn python_json(fields: &[(&str, Value)]) -> String {
    let parts: Vec<String> = fields
        .iter()
        .map(|(k, v)| format!("{}: {}", Value::from(*k), python_value(v)))
        .collect();
    format!("{{{}}}", parts.join(", "))
}

fn python_value(v: &Value) -> String {
    match v {
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(python_value).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => {
            let parts: Vec<String> = o
                .iter()
                .map(|(k, v)| format!("{}: {}", Value::from(k.as_str()), python_value(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        // non-ASCII as \uXXXX (ensure_ascii)
        Value::String(s) => {
            let mut out = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                        let mut buf = [0u16; 2];
                        for u in c.encode_utf16(&mut buf) {
                            out.push_str(&format!("\\u{u:04x}"));
                        }
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        other => other.to_string(),
    }
}

/// Connection info, the token included, readable by the current user only; written before the
/// ready line, removed on exit.
pub fn write_ready_file(path: &Path, ready: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    ));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(ready.as_bytes())?;
    }
    std::fs::rename(&tmp, path)
}

/// `spaceknife-cli serve`: returns the exit code.
pub fn serve(args: ServeArgs) -> i32 {
    init_logging(&args.log_level, &args.log_format);
    let (token, generated) = if args.token_stdin {
        match read_token_line(&mut std::io::stdin().lock()) {
            Ok(t) => (t, false),
            Err(e) => {
                super::eprint_line(&e);
                return 1;
            }
        }
    } else {
        (crate::util::new_token(), true)
    };
    let host: IpAddr = match args.host.as_str() {
        "localhost" => IpAddr::from([127, 0, 0, 1]),
        h => match h.trim_matches(['[', ']']).parse() {
            Ok(ip) => ip,
            Err(_) => {
                super::eprint_line(&format!(
                    "Error: Invalid value for '--host': {h} is not an IP address"
                ));
                return 2;
            }
        },
    };
    let opts = ServeOptions {
        host,
        port: args.port,
        token: token.clone(),
        allow_origins: args.allow_origin.clone(),
        app_version: args.app_version.clone(),
        install_kind: args.install_kind.clone(),
        system_locale: args.system_locale.clone(),
        static_dir: None,
        tls: None,
    };
    let tuning = Tuning {
        hello_timeout: Duration::from_secs_f64(args.hello_timeout.max(0.1)),
        ..Default::default()
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            super::eprint_line(&format!("cannot start the runtime: {e}"));
            return 1;
        }
    };
    let watch_stdin = args.token_stdin;
    let ready_file = args.ready_file.clone();
    let code = rt.block_on(async move {
        let handle = match crate::start_with(opts, tuning).await {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("cannot start the server: {e:#}");
                return 1;
            }
        };
        let mut ready: Vec<(&str, Value)> = vec![
            ("event", "ready".into()),
            ("url", handle.url.clone().into()),
            ("host", host.to_string().into()),
            ("port", handle.port.into()),
            ("pid", handle.pid.into()),
            ("protocol", sk_protocol::PROTOCOL_VERSION.into()),
            ("subprotocol", sk_protocol::SUBPROTOCOL.into()),
            // where update files are downloaded: the launcher starts only files from there
            (
                "updates_dir",
                handle.updates_dir.to_string_lossy().into_owned().into(),
            ),
        ];
        if let Some(path) = &ready_file {
            let mut with_token = ready.clone();
            with_token.push(("token", token.clone().into()));
            if let Err(e) = write_ready_file(path, &python_json(&with_token)) {
                tracing::warn!("cannot write the ready file {}: {e}", path.display());
            }
        }
        if generated {
            ready.push(("token", token.clone().into()));
        }
        {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", python_json(&ready));
            let _ = out.flush();
        }
        let server = handle.server.clone();
        tokio::spawn(async move {
            stop_signal(watch_stdin).await;
            server.request_shutdown();
        });
        handle.wait().await;
        0
    });
    if let Some(path) = &args.ready_file {
        let _ = std::fs::remove_file(path);
    }
    rt.shutdown_timeout(Duration::from_secs(2));
    code
}

/// Resolves when the backend should stop: stdin EOF (when watched), Ctrl+C / SIGINT, SIGTERM
/// (also what Linux sends when the parent dies), or on macOS the parent gone.
pub async fn stop_signal(watch_stdin: bool) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
    if watch_stdin {
        let eof_tx = tx.clone();
        watch_stdin_eof(move || {
            let _ = eof_tx.send("stdin closed by the parent process");
        });
        arm_parent_death_signal();
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            let tx = tx.clone();
            tokio::spawn(async move {
                // SAFETY: getppid has no preconditions.
                let original = unsafe { libc::getppid() };
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    if unsafe { libc::getppid() } != original {
                        let _ = tx.send("parent process exited; shutting down");
                        return;
                    }
                }
            });
        }
    }
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ = tx.send("interrupted");
            }
        });
    }
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        for kind in [SignalKind::terminate(), SignalKind::hangup()] {
            if let Ok(mut s) = signal(kind) {
                let tx = tx.clone();
                tokio::spawn(async move {
                    s.recv().await;
                    let _ = tx.send("terminated");
                });
            }
        }
    }
    drop(tx);
    if let Some(why) = rx.recv().await {
        tracing::info!("{why}");
    }
}

/// Linux: ask the kernel to SIGTERM us when the parent dies.
fn arm_parent_death_signal() {
    #[cfg(target_os = "linux")]
    // SAFETY: prctl with PR_SET_PDEATHSIG and a signal number.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }
}

/// Calls `on_eof` (once, from a thread) when the parent closes our stdin.
///
/// Windows: a thread blocked in a synchronous `ReadFile` on stdin makes `CreateProcess` in other
/// threads wait for that read (the handle's file object is locked), which would stall starting the
/// scan helper; `PeekNamedPipe` never leaves a read pending, so a pipe is polled.
pub fn watch_stdin_eof(on_eof: impl FnOnce() + Send + 'static) {
    #[cfg(windows)]
    if let Some(mut poll) = pipe_poller() {
        std::thread::Builder::new()
            .name("stdin-eof-watch".into())
            .spawn(move || {
                while !poll() {
                    std::thread::sleep(Duration::from_millis(250));
                }
                on_eof();
            })
            .ok();
        return;
    }
    std::thread::Builder::new()
        .name("stdin-eof-watch".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match stdin.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            on_eof();
        })
        .ok();
}

/// Windows: "has the writer closed the stdin pipe?" without a pending read, or None if stdin is
/// not a pipe.
#[cfg(windows)]
fn pipe_poller() -> Option<impl FnMut() -> bool + Send> {
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_NO_DATA, GetLastError, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::ReadFile;
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    struct H(HANDLE);
    // SAFETY: a process-wide standard handle, used from one thread at a time.
    unsafe impl Send for H {}
    // SAFETY: plain Win32 calls on the process's stdin handle.
    let h = H(unsafe { GetStdHandle(STD_INPUT_HANDLE) });
    let peek = move |h: &H| -> Option<bool> {
        let mut avail: u32 = 0;
        // SAFETY: valid out-pointer; no buffer requested.
        let ok = unsafe {
            PeekNamedPipe(
                h.0,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut avail,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            // SAFETY: no preconditions.
            let err = unsafe { GetLastError() };
            return if err == ERROR_BROKEN_PIPE || err == ERROR_NO_DATA {
                Some(true)
            } else {
                None
            };
        }
        if avail > 0 {
            // data is there, so this read cannot block: discard it
            let mut buf = vec![0u8; avail as usize];
            let mut read = 0u32;
            // SAFETY: buffer of `avail` bytes.
            unsafe {
                ReadFile(
                    h.0,
                    buf.as_mut_ptr(),
                    avail,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
        }
        Some(false)
    };
    peek(&h)?;
    Some(move || peek(&h).unwrap_or(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_line_is_json_dumps() {
        let line = python_json(&[
            ("event", "ready".into()),
            ("port", 5u16.into()),
            ("dir", "C:\\Ü".into()),
        ]);
        assert_eq!(
            line,
            r#"{"event": "ready", "port": 5, "dir": "C:\\\u00dc"}"#
        );
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["dir"], "C:\\Ü");
    }

    #[test]
    fn token_line() {
        assert_eq!(
            read_token_line(&mut &b"0123456789abcdef\r\nrest"[..]).unwrap(),
            "0123456789abcdef"
        );
        assert!(read_token_line(&mut &b"short\n"[..]).is_err());
        assert!(read_token_line(&mut &b""[..]).is_err());
    }
}
