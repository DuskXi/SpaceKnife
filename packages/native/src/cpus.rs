//! The CPUs this process may use (docs/native.md 2.2 「列目录线程」): what sizes CPU work in the
//! backend: the walker's starting width (hardscan/pace.rs), the compute and MFT pools, snapshot
//! compression. A process limited to a few CPUs (by affinity, a job object, a container's quota)
//! that sized its threads by the machine's CPUs only made them wait for each other.
//!
//! - Windows: the process's affinity mask; when its threads span several processor groups (the
//!   masks are 0 then), the active processors of each group it runs in.
//! - Linux: `sched_getaffinity`, and the cgroup v2 quota (`cpu.max`: quota / period, rounded up;
//!   the smallest along the cgroup's path up to the mount), never more than std's count (which
//!   also knows cgroup v1).
//! - Elsewhere (macOS): the logical CPUs.

use pyo3::prelude::*;

/// See the module docs; at least 1. Not cached: an affinity can change while the process runs.
pub fn usable() -> usize {
    let std_count = std::thread::available_parallelism().map_or(1, |n| n.get());
    own().map_or(std_count, |n| n.min(std_count).max(1))
}

#[cfg(windows)]
fn own() -> Option<usize> {
    use windows_sys::Win32::System::Threading::{
        GetActiveProcessorCount, GetCurrentProcess, GetProcessAffinityMask, GetProcessGroupAffinity,
    };
    // SAFETY: plain calls with valid pointers; the pseudo handle needs no closing.
    unsafe {
        let me = GetCurrentProcess();
        let (mut mine, mut system) = (0usize, 0usize);
        if GetProcessAffinityMask(me, &mut mine, &mut system) != 0 && mine != 0 {
            return Some(mine.count_ones() as usize);
        }
        let mut groups = [0u16; 64];
        let mut n = groups.len() as u16;
        if GetProcessGroupAffinity(me, &mut n, groups.as_mut_ptr()) == 0 || n == 0 {
            return None;
        }
        let total: u32 = groups[..n as usize]
            .iter()
            .map(|&g| GetActiveProcessorCount(g))
            .sum();
        (total > 0).then_some(total as usize)
    }
}

#[cfg(target_os = "linux")]
fn own() -> Option<usize> {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: `set` is writable for its size; CPU_COUNT reads it.
    let affinity = unsafe {
        (libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut set) == 0)
            .then(|| libc::CPU_COUNT(&set) as usize)
            .filter(|&n| n > 0)
    };
    let quota = cgroup::quota(std::path::Path::new("/"));
    match (affinity, quota) {
        (Some(a), Some(q)) => Some(a.min(q)),
        (a, q) => a.or(q),
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn own() -> Option<usize> {
    None
}

/// cgroup v2 CPU quotas, read below `root` ("/" but in tests).
#[cfg(any(target_os = "linux", test))]
pub(crate) mod cgroup {
    use std::path::{Path, PathBuf};

    /// The smallest quota, in CPUs rounded up, from this process's cgroup up to the cgroup2 mount;
    /// None without cgroup v2 or without a quota.
    pub fn quota(root: &Path) -> Option<usize> {
        let at = |p: &str| root.join(p.trim_start_matches('/'));
        let groups = std::fs::read_to_string(at("/proc/self/cgroup")).ok()?;
        let mounts = std::fs::read_to_string(at("/proc/self/mountinfo")).ok()?;
        let path = own_path(&groups)?;
        let (mount_root, mount_point) = mount(&mounts)?;
        // the path is relative to the cgroup namespace's root, which the mount shows as its root
        let below = match mount_root.trim_end_matches('/') {
            "" => path,
            r => match path.strip_prefix(r) {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => rest,
                _ => path,
            },
        };
        let top = at(&mount_point);
        let mut dir: PathBuf = top.join(below.trim_start_matches('/'));
        let mut best: Option<usize> = None;
        loop {
            if let Some(cpus) = std::fs::read_to_string(dir.join("cpu.max"))
                .ok()
                .and_then(|s| cpu_max(&s))
            {
                best = Some(best.map_or(cpus, |b| b.min(cpus)));
            }
            if dir == top || !dir.pop() || !dir.starts_with(&top) {
                break;
            }
        }
        best
    }

    /// The v2 entry of /proc/self/cgroup (`0::/user.slice/...`).
    pub fn own_path(groups: &str) -> Option<&str> {
        groups.lines().find_map(|l| l.strip_prefix("0::"))
    }

    /// (root, mount point) of the cgroup2 mount in /proc/self/mountinfo (octal escapes undone).
    pub fn mount(mounts: &str) -> Option<(String, String)> {
        mounts.lines().find_map(|l| {
            let (fields, after) = l.split_once(" - ")?;
            if after.split(' ').next()? != "cgroup2" {
                return None;
            }
            let f: Vec<&str> = fields.split(' ').collect();
            Some((unescape(f.get(3)?), unescape(f.get(4)?)))
        })
    }

    /// `cpu.max` ("max 100000" or "<quota> <period>") as CPUs, rounded up; None without a quota.
    pub fn cpu_max(text: &str) -> Option<usize> {
        let mut it = text.split_whitespace();
        let quota: u64 = it.next()?.parse().ok()?; // "max" does not parse: no quota
        let period: u64 = it.next().map_or(Some(100_000), |p| p.parse().ok())?;
        (period > 0 && quota > 0).then(|| quota.div_ceil(period).max(1) as usize)
    }

    fn unescape(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\'
                && i + 3 < b.len()
                && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
            {
                out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
                i += 4;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

/// `cpu_count()`: the CPUs this process may use (module docs).
#[pyfunction]
fn cpu_count() -> usize {
    usable()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(cpu_count, m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::cgroup::{cpu_max, mount, own_path, quota};
    use super::*;

    #[test]
    fn at_least_one_and_never_more_than_std() {
        let n = usable();
        assert!(n >= 1);
        assert!(n <= std::thread::available_parallelism().unwrap().get());
    }

    #[test]
    fn cpu_max_is_read_as_cpus_rounded_up() {
        assert_eq!(cpu_max("max 100000\n"), None);
        assert_eq!(cpu_max("max"), None);
        assert_eq!(cpu_max("200000 100000\n"), Some(2));
        assert_eq!(cpu_max("150000 100000"), Some(2));
        assert_eq!(cpu_max("50000 100000"), Some(1));
        assert_eq!(cpu_max("400000"), Some(4)); // period left out: the default 100 ms
        assert_eq!(cpu_max(""), None);
        assert_eq!(cpu_max("0 100000"), None);
        assert_eq!(cpu_max("x y"), None);
    }

    #[test]
    fn the_v2_cgroup_and_its_mount_are_found() {
        let groups = "12:cpu,cpuacct:/old\n0::/user.slice/user-1000.slice/session-3.scope\n";
        assert_eq!(
            own_path(groups),
            Some("/user.slice/user-1000.slice/session-3.scope")
        );
        assert_eq!(own_path("5:memory:/x\n"), None);
        let mounts = "25 30 0:23 / /sys rw - sysfs sysfs rw\n\
                      36 25 0:31 / /sys/fs/cgroup rw,nosuid shared:9 - cgroup2 cgroup2 rw,nsdelegate\n";
        assert_eq!(mount(mounts), Some(("/".into(), "/sys/fs/cgroup".into())));
        let escaped = "40 25 0:40 /ns /mnt/my\\040groups rw - cgroup2 none rw\n";
        assert_eq!(
            mount(escaped),
            Some(("/ns".into(), "/mnt/my groups".into()))
        );
        assert_eq!(mount("1 2 0:1 / / rw - ext4 /dev/sda rw\n"), None);
    }

    #[test]
    fn the_smallest_quota_along_the_path_counts() {
        let base = std::env::temp_dir().join(format!("sk-cgroup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let put = |p: &str, s: &str| {
            let f = base.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, s).unwrap();
        };
        put("proc/self/cgroup", "0::/a/b/c\n");
        put(
            "proc/self/mountinfo",
            "36 25 0:31 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
        );
        put("sys/fs/cgroup/cpu.max", "max 100000\n");
        put("sys/fs/cgroup/a/cpu.max", "300000 100000\n");
        put("sys/fs/cgroup/a/b/cpu.max", "max 100000\n");
        put("sys/fs/cgroup/a/b/c/cpu.max", "550000 100000\n");
        assert_eq!(quota(&base), Some(3)); // a's 3 CPUs, though c alone would allow 6
        put("sys/fs/cgroup/a/cpu.max", "max 100000\n");
        assert_eq!(quota(&base), Some(6));
        put("sys/fs/cgroup/a/b/c/cpu.max", "max 100000\n");
        assert_eq!(quota(&base), None);
        // a namespaced mount: the cgroup's path is below the mount's root
        put(
            "proc/self/mountinfo",
            "36 25 0:31 /a /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
        );
        put("proc/self/cgroup", "0::/a/b\n");
        put("sys/fs/cgroup/b/cpu.max", "120000 100000\n");
        assert_eq!(quota(&base), Some(2));
        // no cgroup v2 at all
        put("proc/self/cgroup", "4:cpu:/x\n");
        assert_eq!(quota(&base), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
