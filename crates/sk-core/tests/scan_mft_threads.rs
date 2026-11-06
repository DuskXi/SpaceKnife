//! The MFT build leaves no threads behind (port of test_mft_build.py's
//! `test_the_build_leaves_no_threads_behind`): it runs on a rayon pool of its own that goes with
//! it, so nothing stays in the process (rayon's global pool would keep a thread per CPU for
//! good). One test in its own binary: no other test's threads come and go meanwhile.

#[path = "../../sk-native/tests/mft_image/mod.rs"]
#[allow(dead_code, clippy::all)]
mod im;

use sk_core::scan::mft::{MftBuild, table_to_store};

#[cfg(windows)]
fn threads() -> Option<usize> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    // SAFETY: plain Win32 calls with a valid, sized entry; the snapshot is closed.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let me = std::process::id();
        let mut e: THREADENTRY32 = std::mem::zeroed();
        e.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut n = 0;
        let mut ok = Thread32First(snap, &mut e);
        while ok != 0 {
            n += usize::from(e.th32OwnerProcessID == me);
            ok = Thread32Next(snap, &mut e);
        }
        CloseHandle(snap);
        Some(n)
    }
}

#[cfg(target_os = "linux")]
fn threads() -> Option<usize> {
    Some(std::fs::read_dir("/proc/self/task").ok()?.count())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn threads() -> Option<usize> {
    None
}

#[test]
fn the_build_leaves_no_threads_behind() {
    let data = im::synthetic_mft(20_000, 3, 1024);
    let Some(before) = threads() else {
        eprintln!("skipped: threads are not counted on this system");
        return;
    };
    // the parse's own pool goes too
    let table = sk_native::mft::parse_mft(&data, 1024, 4096).unwrap();
    let store = table_to_store(table, "C:\\", &MftBuild::default()).unwrap();
    assert!(store.read().len() > 20_000);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut now = threads().unwrap();
    while now > before && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        now = threads().unwrap();
    }
    assert!(
        now <= before,
        "{before} threads before the build, {now} after"
    );
}
