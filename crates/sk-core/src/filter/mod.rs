//! Filter engine: SKF 1 expressions (`sk_protocol::filter_syntax`) evaluated on node columns
//! (Python `spaceknife.filter`).
//!
//! * [`engine`]: [`evaluate`] a parsed normal form over a store view → [`FilterResult`].
//! * [`names`]: the per-store [`NameIndex`] (glob matches, palette searches and tiers).
//! * [`classes`]: [`ClassMap`] (class membership by extension and size) and the folder-class
//!   hook [`FolderClasses`] (stand-ins until `crate::classes` / `crate::pathclass` exist).
//! * [`vocab`]: the parser vocabulary of this platform.

pub mod classes;
pub mod engine;
pub mod names;
pub mod vocab;

pub use classes::{ClassMap, FolderClasses};
pub use engine::{FilterContext, FilterResult, attr_flag, evaluate, tag_bit, unix_now};
pub use names::{Glob, NameIndex};
