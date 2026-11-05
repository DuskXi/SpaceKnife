//! sk-server: the WebSocket JSON-RPC server of SpaceKnife (protocol v1, `docs/protocol.md`).
//!
//! [`start`] binds (loopback by default), serves `/v1` and, in headless mode, the built interface
//! with `/sk/info`; it returns a [`ServerHandle`] once listening. Sessions (one per connection)
//! share the managers of [`app::Server`]: scans ([`scans`]), views ([`views`]), file monitors
//! ([`monitors`]), comparisons ([`compares`]), the delete basket ([`ops_service`]), update checks
//! ([`updates`]). Handlers live in [`handlers`], one module per area.

pub mod app;
pub mod compares;
pub mod compute;
pub mod diag;
pub mod handlers;
pub mod http;
pub mod monitors;
pub mod ops_service;
pub mod rpc;
pub mod scans;
pub mod session;
pub mod topics;
pub mod updates;
pub mod util;
pub mod views;
pub mod volume_summaries;

/// The console binary `spaceknife-cli` (scan, run, snapshot, serve, headless; cli task).
pub mod cli;

pub use app::{ServeOptions, ServerHandle, Tuning, start, start_with};
