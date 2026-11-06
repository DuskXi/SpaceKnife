//! The parser vocabulary of this backend: file classes and what this platform does not record
//! (Python `filter/vocab.py`).

use std::sync::Arc;

use parking_lot::Mutex;
use sk_protocol::classes::localized;
use sk_protocol::filter::{AgeField, AttrName, FilterClassInfo, FilterVocab};
use sk_protocol::filter_syntax::{CompiledVocab, default_vocab};

/// Attributes the scanner does not collect on POSIX (hidden = dot files is collected everywhere).
pub const POSIX_MISSING_ATTRS: [AttrName; 10] = [
    AttrName::Archive,
    AttrName::System,
    AttrName::Readonly,
    AttrName::Compressed,
    AttrName::Encrypted,
    AttrName::Offline,
    AttrName::Temporary,
    AttrName::Notindexed,
    AttrName::Sparse,
    AttrName::Ads,
];

/// The built-in vocabulary with this platform's unsupported attributes and ages.
pub fn platform_base() -> FilterVocab {
    let mut v = default_vocab();
    if cfg!(windows) {
        v.unsupported_attrs = vec![AttrName::Ads]; // alternate data streams are not collected yet
    } else {
        v.unsupported_attrs = POSIX_MISSING_ATTRS.to_vec();
        if cfg!(target_os = "linux") {
            v.unsupported_ages = vec![AgeField::Create]; // no birth time from stat() on Linux
        }
    }
    v
}

/// The vocabulary with the class table `classes` as shown in `language` (built-in classes the user
/// has not renamed take their English names in English).
pub fn platform_vocab(classes: &[FilterClassInfo], language: &str) -> FilterVocab {
    let mut v = platform_base();
    v.classes = classes.iter().map(|c| localized(c, language)).collect();
    v
}

type Current = Option<((u64, String), Arc<CompiledVocab>)>;

static CURRENT: Mutex<Current> = Mutex::new(None);

/// [`platform_vocab`] compiled, the same object until `(version, language)` changes (`version`:
/// the class table's, bumped whenever it changes).
pub fn compiled_vocab(
    classes: &[FilterClassInfo],
    version: u64,
    language: &str,
) -> Arc<CompiledVocab> {
    let mut cur = CURRENT.lock();
    if let Some((key, v)) = &*cur
        && key.0 == version
        && key.1 == language
    {
        return v.clone();
    }
    let v = Arc::new(CompiledVocab::new(platform_vocab(classes, language)));
    *cur = Some(((version, language.to_owned()), v.clone()));
    v
}
