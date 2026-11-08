//! Folders a test binary keeps for all of its tests (a process-wide `SPACEKNIFE_HOME`), removed
//! when the binary exits. They are set once in a static, which is never dropped, and no single test
//! may remove them, so the C library's `atexit` does: libtest returns from `main` once every test
//! has passed, and the handler runs then. When a test failed, libtest ends the process with
//! `exit(101)`: on Unix the handler runs then too, on Windows (`ExitProcess`) it does not, and the
//! folders stay for a look. Also used by `crates/sk-server/tests/common/mod.rs`.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Mutex, Once};

static FOLDERS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

unsafe extern "C" {
    fn atexit(callback: extern "C" fn()) -> std::ffi::c_int;
}

/// Every folder [`remove_at_exit`] was given, as the process ends (nothing here can panic).
extern "C" fn remove_folders() {
    let folders = match FOLDERS.lock() {
        Ok(mut f) => std::mem::take(&mut *f),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    };
    for dir in folders {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Remove `dir` and what is in it when this process exits.
pub fn remove_at_exit(dir: PathBuf) {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // SAFETY: a plain function the C library calls once, at exit; it neither unwinds nor
        // touches anything but FOLDERS and the file system
        unsafe { atexit(remove_folders) };
    });
    FOLDERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(dir);
}
