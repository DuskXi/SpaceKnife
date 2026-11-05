//! Memory of this process: private (committed) bytes and resident bytes, on every platform.
//!
//! `private` is what the process holds on its own — the Windows commit charge (Task Manager's
//! 「内存」 column counts the resident part of it), anonymous resident plus swapped pages on Linux,
//! the physical footprint on macOS. `None` where a number cannot be read.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessMemory {
    pub private: Option<u64>,
    pub resident: Option<u64>,
}

/// This process's memory now.
pub fn process_memory() -> ProcessMemory {
    imp::process_memory().unwrap_or_default()
}

/// Hand memory the allocator keeps but no longer uses back to the system (after a big scan was
/// closed). Best effort: Windows decommits free heap pages (HeapOptimizeResources), glibc trims its
/// arenas, macOS relieves its malloc zones; elsewhere nothing happens.
pub fn release() {
    imp::release();
}

/// Installed physical memory in bytes, or `None`.
pub fn physical_memory() -> Option<u64> {
    imp::physical_memory()
}

#[cfg(windows)]
mod imp {
    use super::ProcessMemory;
    use windows_sys::Win32::System::Memory::{HeapOptimizeResources, HeapSetInformation};
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub fn process_memory() -> Option<ProcessMemory> {
        // SAFETY: a zeroed plain-data struct whose size we pass; the pseudo handle needs no closing.
        unsafe {
            let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
            c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            let ok = GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&raw mut c).cast::<PROCESS_MEMORY_COUNTERS>(),
                c.cb,
            );
            (ok != 0).then_some(ProcessMemory {
                private: Some(c.PrivateUsage as u64),
                resident: Some(c.WorkingSetSize as u64),
            })
        }
    }

    #[repr(C)]
    struct OptimizeInfo {
        version: u32,
        flags: u32,
    }

    pub fn release() {
        let info = OptimizeInfo { version: 1, flags: 0 }; // HEAP_OPTIMIZE_RESOURCES_CURRENT_VERSION
        // SAFETY: a null heap handle with HeapOptimizeResources means every heap of the process.
        unsafe {
            HeapSetInformation(
                std::ptr::null_mut(),
                HeapOptimizeResources,
                (&raw const info).cast(),
                size_of::<OptimizeInfo>(),
            );
        }
    }

    pub fn physical_memory() -> Option<u64> {
        // SAFETY: zeroed plain-data struct with its length set.
        unsafe {
            let mut st: MEMORYSTATUSEX = std::mem::zeroed();
            st.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
            (GlobalMemoryStatusEx(&mut st) != 0).then_some(st.ullTotalPhys)
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::ProcessMemory;

    pub fn process_memory() -> Option<ProcessMemory> {
        let text = std::fs::read_to_string("/proc/self/status").ok()?;
        let (mut rss, mut anon, mut swap) = (None, None, None);
        for line in text.lines() {
            let Some((key, rest)) = line.split_once(':') else { continue };
            let kb = || rest.split_whitespace().next()?.parse::<u64>().ok().map(|v| v * 1024);
            match key {
                "VmRSS" => rss = kb(),
                "RssAnon" => anon = kb(),
                "VmSwap" => swap = kb(),
                _ => {}
            }
        }
        Some(ProcessMemory { private: anon.map(|a| a + swap.unwrap_or(0)), resident: rss })
    }

    pub fn release() {
        #[cfg(target_env = "gnu")]
        {
            // SAFETY: malloc_trim only gives free heap pages back.
            unsafe {
                libc::malloc_trim(0);
            }
        }
    }

    pub fn physical_memory() -> Option<u64> {
        // SAFETY: sysconf has no preconditions.
        let (pages, size) = unsafe { (libc::sysconf(libc::_SC_PHYS_PAGES), libc::sysconf(libc::_SC_PAGESIZE)) };
        (pages > 0 && size > 0).then(|| pages as u64 * size as u64)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::ProcessMemory;

    /// `task_vm_info_data_t` up to `phys_footprint` (TASK_VM_INFO flavor 22).
    #[repr(C)]
    #[derive(Default)]
    struct VmInfo {
        virtual_size: u64,
        region_count: i32,
        page_size: i32,
        resident_size: u64,
        resident_size_peak: u64,
        device: u64,
        device_peak: u64,
        internal: u64,
        internal_peak: u64,
        external: u64,
        external_peak: u64,
        reusable: u64,
        reusable_peak: u64,
        purgeable_volatile_pmap: u64,
        purgeable_volatile_resident: u64,
        purgeable_volatile_virtual: u64,
        compressed: u64,
        compressed_peak: u64,
        compressed_lifetime: u64,
        phys_footprint: u64,
    }

    unsafe extern "C" {
        static mach_task_self_: u32;
        fn task_info(task: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }

    pub fn process_memory() -> Option<ProcessMemory> {
        let mut info = VmInfo::default();
        let mut count = (size_of::<VmInfo>() / 4) as u32;
        // SAFETY: the struct is the documented prefix of task_vm_info_data_t, count in natural_t units.
        let rc = unsafe { task_info(mach_task_self_, 22, (&raw mut info).cast(), &mut count) };
        (rc == 0).then(|| ProcessMemory {
            private: Some(info.phys_footprint),
            resident: Some(info.resident_size),
        })
    }

    pub fn release() {
        // SAFETY: a null zone relieves every zone.
        unsafe {
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }

    pub fn physical_memory() -> Option<u64> {
        // SAFETY: sysconf has no preconditions.
        let (pages, size) = unsafe { (libc::sysconf(libc::_SC_PHYS_PAGES), libc::sysconf(libc::_SC_PAGESIZE)) };
        (pages > 0 && size > 0).then(|| pages as u64 * size as u64)
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod imp {
    use super::ProcessMemory;
    pub fn process_memory() -> Option<ProcessMemory> {
        None
    }
    pub fn release() {}
    pub fn physical_memory() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_memory_is_readable() {
        let mem = process_memory();
        if cfg!(any(windows, target_os = "linux")) {
            assert!(mem.private.is_some_and(|p| p > 0));
            assert!(mem.resident.is_some_and(|r| r > 0));
        }
        assert!(physical_memory().is_some_and(|p| p > 1 << 20));
    }

    #[test]
    fn release_runs_on_this_platform() {
        release(); // best effort: never fails
    }
}
