//! The backend the page talks to (docs/protocol.md: WebSocket JSON-RPC and binary frames). It will
//! run in this process: crate sk-server serves the protocol on a loopback port with a token, and
//! the page connects to it as it connected to the Python child process before
//! (state/actions.ts boot → window.sk.backend() → RpcClient.connect(url, token)).
//!
//! This module is the one place that starts it and tells the page: [`start`] runs once from setup
//! (lib.rs); [`Backend::set`] keeps the state for `backend_state` and emits it as `sk://backend`.
//! Until sk-server is ready the state stays "starting" (the page shows its connecting state).

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::events::BACKEND;

/// Where the page connects (shared/api.ts BackendInfo).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BackendInfo {
    /// ws://127.0.0.1:<port>/v1
    pub url: String,
    pub token: String,
    /// the process serving it: this one, once the server is in-process
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

/// The backend's state, managed by Tauri (app.manage).
pub struct Backend {
    state: Mutex<BackendState>,
}

impl Default for Backend {
    fn default() -> Self {
        Self { state: Mutex::new(BackendState::Starting) }
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
}

/// Start the backend (once, from setup).
pub fn start(app: &AppHandle) {
    let _backend = app.state::<Backend>();
    // TODO(sk-server): start the in-process server here, on tauri::async_runtime:
    //   - bind 127.0.0.1:0 with a fresh random token; allowed origins: the window's own origin
    //     (tauri://localhost, http://tauri.localhost) and, in development, the dev server
    //     (http://localhost:3000, tauri.conf.json build.devUrl);
    //   - its home: SPACEKNIFE_HOME (set by main.rs for a portable copy) or the platform default;
    //     the locale: sys_locale::get_locale(); the app version: app.package_info().version;
    //   - once it listens:
    //       _backend.set(app, BackendState::Ready { info: BackendInfo { url: format!("ws://127.0.0.1:{port}/v1"), token, pid: std::process::id() } });
    //   - when it fails to start:  _backend.set(app, BackendState::Failed { message });
    //   - when it stops on its own: _backend.set(app, BackendState::Exited { code: None });
    //   and stop it on RunEvent::ExitRequested (lib.rs run()).
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_the_page_expects() {
        let json = |s: &BackendState| serde_json::to_string(s).unwrap();
        assert_eq!(json(&BackendState::Starting), r#"{"state":"starting"}"#);
        let ready = BackendState::Ready { info: BackendInfo { url: "ws://127.0.0.1:1/v1".into(), token: "t".into(), pid: 7 } };
        assert_eq!(json(&ready), r#"{"state":"ready","info":{"url":"ws://127.0.0.1:1/v1","token":"t","pid":7}}"#);
        assert_eq!(json(&BackendState::Failed { message: "x".into() }), r#"{"state":"failed","message":"x"}"#);
        assert_eq!(json(&BackendState::Exited { code: None }), r#"{"state":"exited","code":null}"#);
    }
}
