//! `sk_native`: the platform-specific, performance-critical parts of the SpaceKnife scanner.
//!
//! * [`hardscan`] — a parallel directory walker ("hard scan") that lists a *slice* of directories
//!   within a time and entry budget and returns everything it saw as columns (struct-of-arrays),
//!   ready to append to a columnar node store. Windows (NT API, file ids, case-only siblings) and
//!   POSIX (`getdents64` / `getattrlistbulk` + `fstatat`).
//! * [`mft`] — an NTFS `$MFT` reader and parser (Windows volumes; images everywhere).
//!
//! See `README.md` for the API overview. Everything here is plain Rust: no Python, no GIL.

pub mod cpus;
#[cfg(feature = "hardscan")]
pub mod hardscan;
#[cfg(feature = "mft")]
pub mod mft;
mod util;

/// The crate version (as `spaceknife_native.__version__` was).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
