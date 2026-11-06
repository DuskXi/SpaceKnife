//! Operating-system I/O priority (UI-W7-14: the idle level and background rescans read at low
//! priority; Python `io/priority.py`).
//!
//! * Windows: `NtSetInformationProcess` / `NtSetInformationThread` with the I/O priority class
//!   (very low ↔ normal); lowering needs no privilege, also for our own child processes. The
//!   heavier background processing mode (`PROCESS_MODE_BACKGROUND_BEGIN` via `SetPriorityClass`,
//!   calling process only) is available with `background_mode`, but it also drops the CPU priority
//!   to idle: on a busy machine the workers then starve for seconds, so the I/O-only class is the
//!   default.
//! * Linux: `ioprio_set(IOPRIO_WHO_PROCESS, tid, IOPRIO_CLASS_IDLE)` for every thread of the
//!   process (the I/O priority is per thread; threads started later inherit it). Only I/O
//!   schedulers that honour priorities (BFQ, recent mq-deadline) act on it.
//! * macOS: `setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS, IOPOL_THROTTLE)`; it only applies
//!   to the calling process (or thread), so workers set it themselves.
//!
//! For a worker pool the simplest correct use is per batch inside the worker:
//! `set_thread_low_io_priority(governor.low_io_priority())` (cheap, and it follows level changes).
//! All functions return whether the change took effect and never fail.

pub const IO_PRIORITY_VERY_LOW: u32 = 0;
pub const IO_PRIORITY_LOW: u32 = 1;
pub const IO_PRIORITY_NORMAL: u32 = 2;

pub const IOPRIO_CLASS_NONE: i32 = 0;
pub const IOPRIO_CLASS_RT: i32 = 1;
pub const IOPRIO_CLASS_BE: i32 = 2;
pub const IOPRIO_CLASS_IDLE: i32 = 3;

// --------------------------------------------------------------------------- Windows

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, NtQueryInformationThread, NtSetInformationThread,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThread, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SET_INFORMATION, SetPriorityClass,
    };

    use super::*;

    const PROCESS_MODE_BACKGROUND_BEGIN: u32 = 0x0010_0000;
    const PROCESS_MODE_BACKGROUND_END: u32 = 0x0020_0000;
    const ERROR_PROCESS_MODE_ALREADY_BACKGROUND: u32 = 402;
    const ERROR_PROCESS_MODE_NOT_BACKGROUND: u32 = 403;
    const PROCESS_IO_PRIORITY: i32 = 33; // PROCESSINFOCLASS.ProcessIoPriority
    const THREAD_IO_PRIORITY: i32 = 22; // THREADINFOCLASS.ThreadIoPriority

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSetInformationProcess(
            process: HANDLE,
            class: i32,
            info: *const c_void,
            len: u32,
        ) -> i32;
    }

    fn set_io(handle: HANDLE, thread: bool, on: bool) -> bool {
        let value: u32 = if on {
            IO_PRIORITY_VERY_LOW
        } else {
            IO_PRIORITY_NORMAL
        };
        let p = (&value as *const u32).cast();
        // SAFETY: a valid handle and a 4-byte value.
        unsafe {
            if thread {
                NtSetInformationThread(handle, THREAD_IO_PRIORITY, p, 4) == 0
            } else {
                NtSetInformationProcess(handle, PROCESS_IO_PRIORITY, p, 4) == 0
            }
        }
    }

    fn get_io(handle: HANDLE, thread: bool) -> Option<u32> {
        let mut value = 0u32;
        let mut got = 0u32;
        let p = (&mut value as *mut u32).cast();
        // SAFETY: a valid handle and a 4-byte buffer.
        let status = unsafe {
            if thread {
                NtQueryInformationThread(handle, THREAD_IO_PRIORITY, p, 4, &mut got)
            } else {
                NtQueryInformationProcess(handle, PROCESS_IO_PRIORITY, p, 4, &mut got)
            }
        };
        (status == 0).then_some(value)
    }

    fn with_process<R>(pid: u32, access: u32, f: impl FnOnce(HANDLE) -> Option<R>) -> Option<R> {
        // SAFETY: the handle is closed below.
        let h = unsafe { OpenProcess(access, 0, pid) };
        if h.is_null() {
            return None;
        }
        let r = f(h);
        // SAFETY: we own it.
        unsafe { CloseHandle(h) };
        r
    }

    pub fn set_process(pid: Option<u32>, on: bool, background_mode: bool) -> bool {
        let own = pid.is_none_or(|p| p == std::process::id());
        if own && background_mode {
            let mode = if on {
                PROCESS_MODE_BACKGROUND_BEGIN
            } else {
                PROCESS_MODE_BACKGROUND_END
            };
            // SAFETY: the pseudo handle of this process.
            if unsafe { SetPriorityClass(GetCurrentProcess(), mode) } != 0 {
                return true;
            }
            let err = crate::helper::win32::last_error();
            return err == ERROR_PROCESS_MODE_ALREADY_BACKGROUND
                || err == ERROR_PROCESS_MODE_NOT_BACKGROUND;
        }
        if own {
            // SAFETY: the pseudo handle of this process.
            return set_io(unsafe { GetCurrentProcess() }, false, on);
        }
        with_process(pid.expect("not own"), PROCESS_SET_INFORMATION, |h| {
            Some(set_io(h, false, on))
        })
        .unwrap_or(false)
    }

    pub fn set_thread(on: bool) -> bool {
        // SAFETY: the pseudo handle of this thread.
        set_io(unsafe { GetCurrentThread() }, true, on)
    }

    pub fn is_low(pid: Option<u32>, thread: bool) -> Option<bool> {
        let value = if thread {
            // SAFETY: the pseudo handle of this thread.
            get_io(unsafe { GetCurrentThread() }, true)
        } else if pid.is_none_or(|p| p == std::process::id()) {
            // SAFETY: the pseudo handle of this process.
            get_io(unsafe { GetCurrentProcess() }, false)
        } else {
            with_process(
                pid.expect("not own"),
                PROCESS_QUERY_LIMITED_INFORMATION,
                |h| get_io(h, false),
            )
        };
        value.map(|v| v < IO_PRIORITY_NORMAL)
    }
}

// --------------------------------------------------------------------------- Linux

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use super::*;

    const IOPRIO_WHO_PROCESS: libc::c_long = 1;
    const IOPRIO_CLASS_SHIFT: i32 = 13;

    fn tids(pid: Option<u32>) -> Vec<libc::c_long> {
        let base = match pid {
            None => "/proc/self/task".to_owned(),
            Some(p) => format!("/proc/{p}/task"),
        };
        let mut out: Vec<libc::c_long> = std::fs::read_dir(base)
            .map(|rd| {
                rd.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        if out.is_empty() {
            out.push(pid.map_or(0, |p| p as libc::c_long));
        }
        out.sort();
        out
    }

    fn set(tids: &[libc::c_long], on: bool) -> bool {
        let prio = ((if on {
            IOPRIO_CLASS_IDLE
        } else {
            IOPRIO_CLASS_NONE
        }) << IOPRIO_CLASS_SHIFT) as libc::c_long;
        let mut ok = false;
        for &tid in tids {
            // SAFETY: a plain syscall with integer arguments.
            if unsafe { libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, tid, prio) } == 0 {
                ok = true;
            } else {
                tracing::debug!(
                    "ioprio_set({tid}) failed: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
        ok
    }

    fn get(tid: libc::c_long) -> Option<i32> {
        // SAFETY: a plain syscall with integer arguments.
        let v = unsafe { libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_PROCESS, tid) };
        (v >= 0).then_some((v as i32) >> IOPRIO_CLASS_SHIFT)
    }

    fn other(pid: Option<u32>) -> Option<u32> {
        pid.filter(|&p| p != std::process::id())
    }

    pub fn set_process(pid: Option<u32>, on: bool, _background_mode: bool) -> bool {
        set(&tids(other(pid)), on)
    }

    pub fn set_thread(on: bool) -> bool {
        set(&[0], on)
    }

    pub fn is_low(pid: Option<u32>, thread: bool) -> Option<bool> {
        let tid = if thread { 0 } else { tids(other(pid))[0] };
        get(tid).map(|c| c == IOPRIO_CLASS_IDLE)
    }
}

// --------------------------------------------------------------------------- macOS

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_int;

    const IOPOL_TYPE_DISK: c_int = 0;
    const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3; // macOS 10.15+
    const IOPOL_SCOPE_PROCESS: c_int = 0;
    const IOPOL_SCOPE_THREAD: c_int = 1;
    const IOPOL_DEFAULT: c_int = 0;
    const IOPOL_THROTTLE: c_int = 3;
    const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: c_int = 1;

    unsafe extern "C" {
        fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
        fn getiopolicy_np(iotype: c_int, scope: c_int) -> c_int;
    }

    fn set(scope: c_int, on: bool) -> bool {
        // SAFETY: plain libSystem call.
        unsafe {
            setiopolicy_np(
                IOPOL_TYPE_DISK,
                scope,
                if on { IOPOL_THROTTLE } else { IOPOL_DEFAULT },
            ) == 0
        }
    }

    pub fn set_process(pid: Option<u32>, on: bool, _background_mode: bool) -> bool {
        pid.is_none_or(|p| p == std::process::id()) && set(IOPOL_SCOPE_PROCESS, on)
    }

    pub fn set_thread(on: bool) -> bool {
        set(IOPOL_SCOPE_THREAD, on)
    }

    pub fn is_low(pid: Option<u32>, thread: bool) -> Option<bool> {
        if !thread && pid.is_some_and(|p| p != std::process::id()) {
            return None;
        }
        // SAFETY: plain libSystem call.
        let v = unsafe {
            getiopolicy_np(
                IOPOL_TYPE_DISK,
                if thread {
                    IOPOL_SCOPE_THREAD
                } else {
                    IOPOL_SCOPE_PROCESS
                },
            )
        };
        (v >= 0).then_some(v == IOPOL_THROTTLE)
    }

    pub fn no_cloud_downloads() -> bool {
        // SAFETY: plain libSystem call.
        let ok = unsafe {
            setiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_PROCESS,
                IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            )
        } == 0;
        if !ok {
            tracing::debug!(
                "no_cloud_downloads: setiopolicy_np failed ({})",
                std::io::Error::last_os_error()
            );
        }
        ok
    }
}

// --------------------------------------------------------------------------- public

/// Lower (`on`) or restore the I/O priority of a process (`None`: the calling process).
///
/// `background_mode` (Windows, calling process only): use `PROCESS_MODE_BACKGROUND_BEGIN`, which
/// also lowers CPU and memory priority. On macOS only the calling process can be changed.
pub fn set_low_io_priority(pid: Option<u32>, on: bool, background_mode: bool) -> bool {
    imp::set_process(pid, on, background_mode)
}

/// Lower or restore the I/O priority of the calling thread only (cheap: per batch in a worker).
pub fn set_thread_low_io_priority(on: bool) -> bool {
    imp::set_thread(on)
}

/// Whether a process (or the calling thread) reads at low I/O priority (None: cannot tell).
pub fn io_priority_is_low(pid: Option<u32>, thread: bool) -> Option<bool> {
    imp::is_low(pid, thread)
}

/// macOS: this process never downloads ("materializes") a dataless file or folder (iCloud Drive,
/// File Provider): listing such a folder fails instead of fetching it over the network. True when
/// set; elsewhere false and nothing to do.
pub fn no_cloud_downloads() -> bool {
    #[cfg(target_os = "macos")]
    {
        imp::no_cloud_downloads()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The priority tests start from normal I/O priority (some environments start every process
    /// at low, which children inherit) and run one at a time (they change the whole process).
    #[cfg(not(target_os = "macos"))]
    fn normal() -> parking_lot::MutexGuard<'static, ()> {
        static LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
        let guard = LOCK.lock();
        if io_priority_is_low(None, false) == Some(true) {
            assert!(set_low_io_priority(None, false, false));
        }
        guard
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn process_io_priority_round_trip() {
        let _g = normal();
        assert_eq!(io_priority_is_low(None, false), Some(false));
        assert!(set_low_io_priority(None, true, false));
        assert_eq!(io_priority_is_low(None, false), Some(true));
        assert!(set_low_io_priority(None, false, false));
        assert_eq!(io_priority_is_low(None, false), Some(false));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn thread_io_priority_is_per_thread() {
        let _g = normal();
        let seen = std::thread::spawn(|| {
            let set = set_thread_low_io_priority(true);
            let low = io_priority_is_low(None, true);
            let restored = set_thread_low_io_priority(false);
            let after = io_priority_is_low(None, true);
            (set, low, restored, after)
        })
        .join()
        .unwrap();
        assert_eq!(seen, (true, Some(true), true, Some(false)));
        assert_eq!(io_priority_is_low(None, false), Some(false)); // the rest of the process is untouched
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn child_process_io_priority() {
        let _g = normal();
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        #[cfg(unix)]
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let check = || {
            assert_eq!(io_priority_is_low(Some(pid), false), Some(false));
            assert!(set_low_io_priority(Some(pid), true, false)); // a pool worker, lowered from the parent
            assert_eq!(io_priority_is_low(Some(pid), false), Some(true));
            assert!(set_low_io_priority(Some(pid), false, false));
            assert_eq!(io_priority_is_low(Some(pid), false), Some(false));
        };
        let result = std::panic::catch_unwind(check);
        let _ = child.kill();
        let _ = child.wait();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_background_mode() {
        let _g = normal();
        assert!(set_low_io_priority(None, true, true));
        assert_eq!(io_priority_is_low(None, false), Some(true));
        assert!(set_low_io_priority(None, false, true));
        assert_eq!(io_priority_is_low(None, false), Some(false));
    }

    #[test]
    fn no_cloud_downloads_is_a_macos_process_policy() {
        assert_eq!(no_cloud_downloads(), cfg!(target_os = "macos"));
    }
}
