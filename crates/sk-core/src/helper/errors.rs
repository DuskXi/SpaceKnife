//! Errors of [`HelperClient`](super::HelperClient) (Python `helper/errors.py`).
//!
//! One type, [`HelperError`], with a [`HelperErrorKind`]:
//!
//! - `Error` — an operation failed; `code` is the helper's error code (`needs_admin`, `not_ntfs`,
//!   `io_error`, `invalid_request`, `usn_inactive` (no change journal on the volume), `usn_gone`
//!   (the checkpoint is no longer in the journal) …). The helper is still usable.
//! - `Unavailable` — there is no working helper; `code` says why: `exited`, `timeout`, `auth`,
//!   `protocol` (it sent garbage and was disconnected), `closed`, `not_supported`, `no_agent`,
//!   `not_authorized`. Callers fall back to the hard scan.
//! - `Denied` — the user declined the UAC prompt / polkit dialog (code `denied`; also unavailable).

use std::fmt;

use crate::i18n::t;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelperErrorKind {
    /// The operation failed; the helper stays usable.
    Error,
    /// No working helper (Python `HelperUnavailable`).
    Unavailable,
    /// The user declined the elevation prompt (Python `HelperDenied`, a `HelperUnavailable`).
    Denied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperError {
    pub kind: HelperErrorKind,
    pub code: String,
    pub message: String,
}

impl HelperError {
    pub fn error(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            kind: HelperErrorKind::Error,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            kind: HelperErrorKind::Unavailable,
            code: code.into(),
            message: message.into(),
        }
    }

    /// The user declined the prompt (`code` "denied").
    pub fn denied() -> Self {
        Self {
            kind: HelperErrorKind::Denied,
            code: "denied".into(),
            message: t("用户拒绝了管理员授权").to_owned(),
        }
    }

    /// No working helper (`Unavailable` or `Denied`): fall back to the hard scan.
    pub fn is_unavailable(&self) -> bool {
        self.kind != HelperErrorKind::Error
    }

    pub fn is_denied(&self) -> bool {
        self.kind == HelperErrorKind::Denied
    }
}

impl fmt::Display for HelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HelperError {}
