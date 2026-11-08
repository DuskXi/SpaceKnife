//! `sk_native`: the platform-specific, performance-critical parts of the SpaceKnife scanner.
//!
//! * [`hardscan`] — a parallel directory walker ("hard scan") that lists a *slice* of directories
//!   within a time and entry budget and returns everything it saw as columns (struct-of-arrays),
//!   ready to append to a columnar node store. Windows (NT API, file ids, case-only siblings) and
//!   POSIX (`getdents64` / `getattrlistbulk` + `fstatat`).
//! * [`mft`] — an NTFS `$MFT` reader and parser (Windows volumes; images everywhere), and the
//!   parallel build of a whole volume's node columns from its table ([`mft::build_tree`]).
//! * [`fold`] — the fold of listings into a node store's new rows (the per-entry work of the
//!   store's `ingest_slice`, `ingest_listing` and relist).
//! * [`columns`] — the node store's columns as plain slices, its kind and flag values.
//!
//! The store itself (sk-store) owns the memory, the locks and what readers see; it lends this
//! crate the rows to fill through [`columns::NodeColumns`], [`fold::Tree`] and
//! [`mft::TreeSink`]. This crate depends on no other crate of the workspace.
//!
//! See `README.md` for the API overview. Everything here is plain Rust: no Python, no GIL.

pub mod columns;
pub mod cpus;
pub mod fold;
#[cfg(feature = "hardscan")]
pub mod hardscan;
#[cfg(feature = "mft")]
pub mod mft;
mod util;

/// The crate version (as `spaceknife_native.__version__` was).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
