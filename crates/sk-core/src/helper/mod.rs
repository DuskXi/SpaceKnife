//! Elevated read-only scan helper (Python `spaceknife.helper`, docs/native.md 3).
//!
//! The backend never runs elevated. For an MFT quick scan (and USN journal reads, and on Linux a
//! disk's own health record) it starts the console program `spaceknife-cli` next to the app as a
//! separate process with the `helper` subcommand ([`run_main`], [`helper_command`]; never the window
//! program), behind the UAC prompt on Windows or polkit on Linux, and
//! talks to it over a local named pipe / unix socket with one-time token authentication. The
//! helper only performs a whitelist of read-only operations and exits when the backend
//! disconnects, on shutdown, or after 10 idle minutes.
//!
//! Backend API: [`HelperClient`] (one connection) and [`SharedHelper`] (one helper per backend,
//! reused across scans); failures are [`HelperError`]s.
//!
//! `spaceknife-cli` dispatches `spaceknife-cli helper <args…>` to [`run_main`]
//! (`std::process::exit` with its result) before doing anything else.

pub mod auth;
pub mod client;
pub mod errors;
pub mod launch;
pub mod messages;
pub mod service;
pub mod transport;
pub mod usn;
#[cfg(windows)]
pub mod win32;

pub use auth::is_elevated;
pub use client::{HelperClient, LaunchOptions, MftOptions, ProgressFn, SharedHelper};
pub use errors::{HelperError, HelperErrorKind};
pub use launch::{CLI_FILE, HELPER_SUBCOMMAND, helper_command, helper_command_for};
pub use messages::{Phase, Scalars};
pub use service::run_main;
pub use usn::{UsnRows, UsnTable};
