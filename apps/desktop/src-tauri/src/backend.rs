//! The backend the page talks to (docs/protocol.md: WebSocket JSON-RPC and binary frames). It runs
//! in this process: crate sk-server serves the protocol on a loopback port with a fresh token, and
//! the page connects to it (state/actions.ts boot → window.sk.backend() → RpcClient.connect(url,
//! token)).
//!
//! This module is the one place that starts and stops it and tells the page: [`start`] runs from
//! setup (lib.rs) and again when an update could not start ([`crate::install`]); [`stop`] on exit
//! and before an update replaces the files. [`Backend::set`] keeps the state for `backend_state`
//! and emits it as `sk://backend`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, mpsc};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::events::BACKEND;

/// Where the page connects (shared/api.ts BackendInfo).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BackendInfo {
    /// ws://127.0.0.1:<port>/v1
    pub url: String,
    pub token: String,
    /// the process serving it: this one
    pub pid: u32,
}

/// shared/api.ts BackendState.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum BackendState {
    Starting,
    Ready { info: BackendInfo },
    Failed { message: String },
    Exited { code: Option<i32> },
}

/// The running server: enough to stop it and to know where it downloads updates.
struct Running {
    server: Arc<sk_server::app::Server>,
    updates_dir: PathBuf,
    /// set before a stop asked for here: its end is not news for the page
    stopping: Arc<AtomicBool>,
    /// one message once the server stopped
    done: mpsc::Receiver<()>,
}

/// The backend's state, managed by Tauri (app.manage).
pub struct Backend {
    state: Mutex<BackendState>,
    running: Mutex<Option<Running>>,
}

impl Default for Backend {
    fn default() -> Self {
        Self {
            state: Mutex::new(BackendState::Starting),
            running: Mutex::new(None),
        }
    }
}

impl Backend {
    pub fn state(&self) -> BackendState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Keep the new state and tell the page (sk://backend).
    pub fn set(&self, app: &AppHandle, state: BackendState) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state.clone();
        if let Err(err) = app.emit(BACKEND, state) {
            eprintln!("[backend] could not tell the window: {err}");
        }
    }

    /// Where the running server downloads update files (the only files install_update starts).
    pub fn updates_dir(&self) -> Option<PathBuf> {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|r| r.updates_dir.clone())
    }
}

/// How this copy runs, for the server (update checks, the About page).
#[derive(Clone, Debug)]
pub struct LaunchFacts {
    pub app_version: String,
    pub install_kind: String,
    pub system_locale: Option<String>,
    /// more WebSocket origins: the dev server in development
    pub allow_origins: Vec<String>,
}

impl LaunchFacts {
    pub fn of(app: &AppHandle) -> Self {
        let dev_origin = if tauri::is_dev() {
            app.config().build.dev_url.as_ref().map(origin_of)
        } else {
            None
        };
        LaunchFacts {
            app_version: app.package_info().version.to_string(),
            install_kind: crate::install::this_install_kind().to_owned(),
            system_locale: sys_locale::get_locale(),
            allow_origins: dev_origin.into_iter().collect(),
        }
    }
}

/// "http://localhost:3000/" → "http://localhost:3000" (an Origin header has no path).
pub fn origin_of(url: &url::Url) -> String {
    url.origin().ascii_serialization()
}

static LOGGING: Once = Once::new();

/// Start the backend: from setup, and again after an update could not start. The page hears
/// "starting", then "ready" (or "failed" with the reason).
pub fn start(app: &AppHandle) {
    LOGGING.call_once(|| {
        // stderr (a console in development), the log bus of the 日志 panel, the rotating log file
        sk_server::cli::serve::init_logging("INFO", "plain");
        sk_core::io::no_cloud_downloads(); // macOS: never fetch iCloud / File Provider contents
    });
    let facts = LaunchFacts::of(app);
    let backend = app.state::<Backend>();
    backend.set(app, BackendState::Starting);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let token = sk_server::util::new_token();
        let opts = sk_server::ServeOptions {
            token: token.clone(),
            allow_origins: facts.allow_origins,
            app_version: Some(facts.app_version),
            install_kind: Some(facts.install_kind),
            system_locale: facts.system_locale,
            ..Default::default()
        };
        let backend = app.state::<Backend>();
        let handle = match sk_server::start(opts).await {
            Ok(h) => h,
            Err(err) => {
                tracing::error!("the backend could not start: {err:#}");
                backend.set(
                    &app,
                    BackendState::Failed {
                        message: format!("{err:#}"),
                    },
                );
                return;
            }
        };
        tracing::info!("backend listening on {}", handle.url);
        let info = BackendInfo {
            url: handle.url.clone(),
            token,
            pid: handle.pid,
        };
        let stopping = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        *backend.running.lock().unwrap_or_else(|e| e.into_inner()) = Some(Running {
            server: handle.server.clone(),
            updates_dir: handle.updates_dir.clone(),
            stopping: stopping.clone(),
            done: rx,
        });
        write_ready_file(&info);
        backend.set(&app, BackendState::Ready { info });
        handle.wait().await;
        let _ = tx.send(());
        if !stopping.load(Ordering::SeqCst) {
            // stopped on its own (app.shutdown): the page shows that the backend is gone
            tracing::warn!("the backend stopped");
            backend
                .running
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            backend.set(&app, BackendState::Exited { code: None });
        }
    });
}

/// The ready file this process was started with (`SPACEKNIFE_READY_FILE`: end-to-end tests, demos,
/// the swap watching a new version, sk_core::update::swap::Ready). main takes the variable out of
/// the environment before anything else runs and hands it here ([`keep_ready_file`]), so that
/// nothing the app starts inherits it (an update's helper or installer, a terminal, an opened
/// file); the first backend writes it, and then it is forgotten: the swap removes the file's folder
/// once the new version has spoken, and that place in a shared temp folder may be anyone's.
static READY_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// What main read from `SPACEKNIFE_READY_FILE` before taking it out of the environment.
pub fn keep_ready_file(value: Option<std::ffi::OsString>) {
    *READY_FILE.lock().unwrap_or_else(|e| e.into_inner()) =
        value.filter(|v| !v.is_empty()).map(PathBuf::from);
}

/// The ready file (end-to-end tests, demos, the swap): the connection info, the token included,
/// written to it once (owner-only on Unix, never through a link), as `spaceknife-cli serve
/// --ready-file` does.
fn write_ready_file(info: &BackendInfo) {
    let Some(path) = READY_FILE.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let text = serde_json::json!({"event": "ready", "url": info.url, "pid": info.pid, "token": info.token}).to_string();
    if let Err(e) = sk_server::cli::serve::write_ready_file(&path, &text) {
        tracing::warn!("cannot write the ready file {}: {e}", path.display());
    }
}

/// Stop the backend and wait up to `timeout` for it (exit, an update replacing the files). True
/// when it stopped in time (or was not running).
pub fn stop(app: &AppHandle, timeout: Duration) -> bool {
    let backend = app.state::<Backend>();
    let Some(running) = backend
        .running
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    else {
        return true;
    };
    running.stopping.store(true, Ordering::SeqCst);
    running.server.request_shutdown();
    let stopped = running.done.recv_timeout(timeout).is_ok();
    if !stopped {
        eprintln!("[backend] did not stop within {timeout:?}");
    }
    stopped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_the_page_expects() {
        let json = |s: &BackendState| serde_json::to_string(s).unwrap();
        assert_eq!(json(&BackendState::Starting), r#"{"state":"starting"}"#);
        let ready = BackendState::Ready {
            info: BackendInfo {
                url: "ws://127.0.0.1:1/v1".into(),
                token: "t".into(),
                pid: 7,
            },
        };
        assert_eq!(
            json(&ready),
            r#"{"state":"ready","info":{"url":"ws://127.0.0.1:1/v1","token":"t","pid":7}}"#
        );
        assert_eq!(
            json(&BackendState::Failed {
                message: "x".into()
            }),
            r#"{"state":"failed","message":"x"}"#
        );
        assert_eq!(
            json(&BackendState::Exited { code: None }),
            r#"{"state":"exited","code":null}"#
        );
    }

    #[test]
    fn the_ready_file_is_written_once() {
        // main takes SPACEKNIFE_READY_FILE out of the environment and keeps it here: the first
        // backend writes it; one started again (an update that could not start) does not, as the
        // swap that named it may have removed its folder by then
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("ready.json");
        keep_ready_file(Some(file.clone().into_os_string()));
        let info = BackendInfo {
            url: "ws://127.0.0.1:1/v1".into(),
            token: "t0123456789abcdef".into(),
            pid: 7,
        };
        write_ready_file(&info);
        let said: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(said["token"], "t0123456789abcdef");
        assert_eq!(said["pid"], 7);
        std::fs::remove_file(&file).unwrap();
        write_ready_file(&info);
        assert!(!file.exists(), "written again");
        keep_ready_file(Some("".into()));
        write_ready_file(&info);
        assert!(!file.exists());
    }

    #[test]
    fn the_dev_servers_origin_has_no_path() {
        let url = url::Url::parse("http://localhost:3000/").unwrap();
        assert_eq!(origin_of(&url), "http://localhost:3000");
    }
}
