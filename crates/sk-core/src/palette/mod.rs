//! Command palette (W4; port of `spaceknife.palette`): fuzzy scoring, the command registry, path
//! search and path input, recents.
//!
//! * [`fuzzy`]: [`fuzzy::score`] (character runs), [`fuzzy::utf16_spans`] for the frontends.
//! * [`commands`]: [`commands::registry`], [`commands::evaluate`], [`commands::match_command`]
//!   over a [`commands::Context`] of plain facts the server gathers.
//! * [`paths`]: name search in tiers over the scan's [`crate::filter::NameIndex`], a folder's
//!   children, the input read as a path and its Tab completion.
//! * [`recents`]: jumps per scan target and filters, persisted as JSON.

pub mod commands;
pub mod fuzzy;
pub mod paths;
pub mod recents;

pub use commands::{
    Command, Context, ShownVisual, ViewFacts, evaluate, label, match_command, registry,
};
pub use fuzzy::{score, utf16_spans};
pub use paths::{PathHit, PathInput, SearchOptions, Sizes};
pub use recents::Recents;
