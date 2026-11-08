//! Tests of the console binary's commands (ports of test_cli_run.py and the CLI parts of the
//! Python tests, plus the benchmark's parser over `scan`'s output).

mod bench;
mod run;

use std::path::Path;

use sk_core::i18n::Language;

/// Serialises tests that switch the process-wide language; sets `lang` meanwhile.
pub(super) struct LanguageGuard(#[allow(dead_code)] parking_lot::MutexGuard<'static, ()>);

impl Drop for LanguageGuard {
    fn drop(&mut self) {
        sk_core::i18n::set_language(Language::ZhCn);
    }
}

pub(super) fn lock_language(lang: Language) -> LanguageGuard {
    let g = sk_core::i18n::TEST_LOCK.lock();
    sk_core::i18n::set_language(lang);
    LanguageGuard(g)
}

/// Files of the given sizes below `root` (folders made as needed).
pub(super) fn tree(root: &Path, files: &[(&str, usize)]) {
    for (rel, size) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![b'x'; *size]).unwrap();
    }
}
