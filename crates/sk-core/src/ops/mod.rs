//! File operations (docs/manage.md §2): everything that changes the user's files lives here.
//!
//! * [`paths`] — the four copy formats of a path; the built-in protected paths per platform.
//! * [`basket`] — the delete basket (held by the backend only): containment, small-item groups,
//!   freeable space with cloud placeholders and hard links, refresh after tree changes.
//! * [`locks`] — which processes hold files open (Restart Manager, `/proc`, `lsof`).
//! * [`trash`] — move to the trash (never a permanent deletion in disguise), restore, delete
//!   permanently; one result per item with an error code and a reason for the user.
//! * `win` — the Win32 / COM plumbing behind them (Windows only).

pub mod basket;
pub mod locks;
pub mod names;
pub mod paths;
mod remove;
mod text;
pub mod trash;
#[cfg(windows)]
pub mod win;

pub use basket::{
    AddResult, Basket, BasketError, BasketItem, Merged, Refused, Target, printable_path,
};
pub use paths::{Rules, System, match_rules, path_forms, platform_rules, protected_reason};
pub use sk_protocol::ops::PathForms;
pub use text::PATH_KEYS;
pub use trash::{
    OpCode, OpOptions, OpResult, TrashStatus, delete_permanently, move_to_trash, restore,
    trash_available, trash_status,
};

/// Bytes for the user (`1.5 GB`).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn fmt_size(n: u64) -> String {
    crate::fmt::size(n as f64)
}
