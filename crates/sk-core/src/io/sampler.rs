//! Per-disk load samplers: busy fraction, average queue depth and read statistics over a window
//! (Python `io/sampler.py`).
//!
//! * Windows: PDH (`\PhysicalDisk(*)\% Idle Time`, `Avg. Disk Queue Length`, and for reads
//!   `Disk Reads/sec`, `Avg. Disk sec/Read`, `Disk Read Bytes/sec`), no elevation; falls back to
//!   the raw `IOCTL_DISK_PERFORMANCE` counters when the performance-counter subsystem is broken.
//! * Linux: `/sys/block/<disk>/stat` deltas (io_ticks → busy, time_in_queue → queue depth).
//! * macOS: the block-storage statistics `ioreg -r -c IOBlockStorageDriver -a` reports (every
//!   `IOBlockStorageDriver`'s `Statistics`, named by the whole disk below it). No idle time there:
//!   busy is approximated by the queue depth. Without it the governor runs open loop.
//!
//! [`DiskSampler::sample`] blocks (system calls, a subprocess for `ioreg`): call it from a worker
//! thread. The first call only primes the counters and returns nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::disks::{DiskId, DiskRef, linux_physical_disks, win_disk_id};
use crate::media::Media;

/// Load of one disk over the window `(t - dt, t]` (`t` on the monotonic clock, seconds).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DiskSample {
    pub t: f64,
    pub dt: f64,
    /// Fraction of the window with I/O outstanding (Task Manager "active time"), 0..1.
    pub busy: f64,
    /// Average number of outstanding I/Os (reads and writes).
    pub queue: f64,
    // reads only (scans only read; writes absorbed by an SSD's cache would distort the latency)
    pub read_iops: Option<f64>,
    /// Mean seconds per read.
    pub read_latency: Option<f64>,
    /// Bytes read per second.
    pub read_bps: Option<f64>,
}

impl DiskSample {
    pub fn new(t: f64, dt: f64, busy: f64, queue: f64) -> Self {
        Self {
            t,
            dt,
            busy,
            queue,
            ..Default::default()
        }
    }
}

fn clamp01(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        0.0
    } else if x > 1.0 {
        1.0
    } else {
        x
    }
}

/// Seconds on a process-wide monotonic clock (the samplers' and the governors' `t`).
pub fn monotonic() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// The disks a sampler knows (`watch` adds one).
#[derive(Clone, Debug, Default)]
pub struct RefSet {
    refs: Vec<DiskRef>,
}

impl RefSet {
    pub fn refs(&self) -> Vec<DiskRef> {
        self.refs.clone()
    }

    pub fn get(&self, disk: &str) -> Option<DiskRef> {
        self.refs.iter().find(|r| r.id == disk).cloned()
    }

    pub fn contains(&self, disk: &str) -> bool {
        self.refs.iter().any(|r| r.id == disk)
    }

    pub fn watch(&mut self, r: DiskRef) {
        if !r.sampled() {
            return;
        }
        match self.refs.iter_mut().find(|k| k.id == r.id) {
            None => self.refs.push(r),
            Some(known) if known.media == Media::Unknown && r.media != Media::Unknown => *known = r,
            Some(_) => {}
        }
    }
}

/// Samples every disk it knows about; `watch` adds one.
pub trait DiskSampler: Send {
    fn name(&self) -> &'static str;
    /// Whether it measures anything at all (else every governor runs open loop).
    fn supported(&self) -> bool {
        true
    }
    /// Seconds between samples this sampler can sustain cheaply.
    fn min_interval(&self) -> f64 {
        0.0
    }
    fn sample(&mut self) -> HashMap<DiskId, DiskSample>;
    fn refs(&self) -> Vec<DiskRef>;
    fn disk_ref(&self, disk: &str) -> Option<DiskRef>;
    fn watch(&mut self, r: DiskRef);
    fn close(&mut self) {}
}

/// No load data on this platform: every governor runs open loop.
#[derive(Default)]
pub struct NullSampler {
    refs: RefSet,
}

impl DiskSampler for NullSampler {
    fn name(&self) -> &'static str {
        "none"
    }
    fn supported(&self) -> bool {
        false
    }
    fn sample(&mut self) -> HashMap<DiskId, DiskSample> {
        HashMap::new()
    }
    fn refs(&self) -> Vec<DiskRef> {
        self.refs.refs()
    }
    fn disk_ref(&self, disk: &str) -> Option<DiskRef> {
        self.refs.get(disk)
    }
    fn watch(&mut self, r: DiskRef) {
        self.refs.watch(r)
    }
}

// --------------------------------------------------------------------------- Windows: PDH

/// Counter key → PDH path.
pub const PDH_COUNTERS: [(&str, &str); 5] = [
    ("idle", "\\PhysicalDisk(*)\\% Idle Time"),
    ("queue", "\\PhysicalDisk(*)\\Avg. Disk Queue Length"),
    ("latency", "\\PhysicalDisk(*)\\Avg. Disk sec/Read"),
    ("iops", "\\PhysicalDisk(*)\\Disk Reads/sec"),
    ("bps", "\\PhysicalDisk(*)\\Disk Read Bytes/sec"),
];

/// Disk number of a `PhysicalDisk` instance name (`"3 C: D:"` → 3; `"_Total"` → None).
pub fn pdh_disk_number(instance: &str) -> Option<u32> {
    let t = instance.trim_start();
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    let rest = &t[digits.len()..];
    if digits.is_empty() || rest.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    digits.parse().ok()
}

/// The drive letters of an instance name (`"1 D: e:"` → `["D:\\", "E:\\"]`).
pub fn pdh_instance_letters(instance: &str) -> Vec<String> {
    let b = instance.as_bytes();
    let mut out = Vec::new();
    for i in 0..b.len().saturating_sub(1) {
        let boundary = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if boundary && b[i].is_ascii_alphabetic() && b[i + 1] == b':' {
            out.push(format!("{}:\\", (b[i] as char).to_ascii_uppercase()));
        }
    }
    out
}

/// Combine formatted PDH arrays (counter key → instance → value) into per-disk samples.
pub fn pdh_samples(values: &HashMap<String, HashMap<String, f64>>, t: f64, dt: f64) -> HashMap<DiskId, DiskSample> {
    let mut out = HashMap::new();
    let get = |key: &str, inst: &str| values.get(key).and_then(|m| m.get(inst)).copied();
    let Some(idle) = values.get("idle") else { return out };
    for (inst, &idle) in idle {
        let Some(number) = pdh_disk_number(inst) else { continue };
        out.insert(
            win_disk_id(number),
            DiskSample {
                t,
                dt,
                busy: clamp01(1.0 - idle / 100.0),
                queue: get("queue", inst).unwrap_or(0.0).max(0.0),
                read_iops: get("iops", inst).map(|v| v.max(0.0)),
                read_latency: get("latency", inst).map(|v| v.max(0.0)),
                read_bps: get("bps", inst).map(|v| v.max(0.0)),
            },
        );
    }
    out
}

#[cfg(windows)]
pub use win::{PdhSampler, WinPerfSampler};

#[cfg(windows)]
mod win {
    use std::ptr::null;

    use windows_sys::Win32::System::Performance::{
        PDH_FMT_COUNTERVALUE_ITEM_W, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
        PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    };

    use super::*;
    use crate::disks::windows::IOCTL_DISK_PERFORMANCE;
    use crate::helper::win32::{Handle, from_wide_ptr, wide};
    use crate::media::media_of;

    const PDH_MORE_DATA: u32 = 0x8000_07D2;
    const FMT: u32 = 0x0000_0200 | 0x0000_8000; // PDH_FMT_DOUBLE | PDH_FMT_NOCAP100

    pub struct PdhSampler {
        refs: RefSet,
        query: isize,
        counters: Vec<(&'static str, isize)>,
        last: f64,
    }

    impl PdhSampler {
        pub fn new() -> std::io::Result<Self> {
            let mut query: *mut std::ffi::c_void = std::ptr::null_mut();
            // SAFETY: out-pointer.
            let status = unsafe { PdhOpenQueryW(null(), 0, &mut query) };
            if status != 0 {
                return Err(std::io::Error::other(format!("PdhOpenQueryW failed: 0x{status:08X}")));
            }
            let mut me = Self {
                refs: RefSet::default(),
                query: query as isize,
                counters: Vec::new(),
                last: monotonic(),
            };
            for (key, path) in PDH_COUNTERS {
                let w = wide(path);
                let mut counter: *mut std::ffi::c_void = std::ptr::null_mut();
                // SAFETY: valid query, path and out-pointer.
                let status = unsafe { PdhAddEnglishCounterW(query, w.as_ptr(), 0, &mut counter) };
                if status != 0 {
                    if key == "idle" || key == "queue" {
                        me.close();
                        return Err(std::io::Error::other(format!("PdhAddEnglishCounterW({path}) failed: 0x{status:08X}")));
                    }
                    continue; // latency / IOPS / bytes only refine the Q_eff estimate
                }
                me.counters.push((key, counter as isize));
            }
            // SAFETY: valid query.
            let status = unsafe { PdhCollectQueryData(query) };
            if status != 0 {
                me.close();
                return Err(std::io::Error::other(format!("PdhCollectQueryData failed: 0x{status:08X}")));
            }
            me.last = monotonic();
            Ok(me)
        }

        fn array(&self, counter: isize) -> HashMap<String, f64> {
            let mut out = HashMap::new();
            let (mut size, mut count) = (0u32, 0u32);
            // SAFETY: the size query (null buffer), then a buffer of the size asked for.
            unsafe {
                let status = PdhGetFormattedCounterArrayW(counter as _, FMT, &mut size, &mut count, std::ptr::null_mut());
                if status != PDH_MORE_DATA || size == 0 {
                    return out;
                }
                let mut buf = vec![0u64; (size as usize).div_ceil(8)];
                let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
                if PdhGetFormattedCounterArrayW(counter as _, FMT, &mut size, &mut count, items) != 0 {
                    return out;
                }
                for i in 0..count as usize {
                    let item = &*items.add(i);
                    // PDH_CSTATUS_VALID_DATA / NEW_DATA
                    if (item.FmtValue.CStatus == 0 || item.FmtValue.CStatus == 1) && !item.szName.is_null() {
                        out.insert(from_wide_ptr(item.szName), item.FmtValue.Anonymous.doubleValue);
                    }
                }
            }
            out
        }

        fn discover(&mut self, number: u32, instance: &str) {
            let letters = pdh_instance_letters(instance);
            let mount = letters.first().cloned().unwrap_or_default();
            let media = if mount.is_empty() { Media::Unknown } else { media_of(&mount, mount.trim_end_matches('\\'), false) };
            self.watch(DiskRef {
                number: Some(number),
                media,
                mount,
                ..DiskRef::physical(win_disk_id(number))
            });
        }
    }

    impl DiskSampler for PdhSampler {
        fn name(&self) -> &'static str {
            "pdh"
        }
        fn sample(&mut self) -> HashMap<DiskId, DiskSample> {
            if self.query == 0 {
                return HashMap::new();
            }
            // SAFETY: valid query.
            if unsafe { PdhCollectQueryData(self.query as _) } != 0 {
                return HashMap::new();
            }
            let now = monotonic();
            let dt = now - self.last;
            self.last = now;
            let values: HashMap<String, HashMap<String, f64>> =
                self.counters.iter().map(|&(k, h)| (k.to_owned(), self.array(h))).collect();
            if let Some(idle) = values.get("idle") {
                for inst in idle.keys() {
                    if let Some(n) = pdh_disk_number(inst)
                        && !self.refs.contains(&win_disk_id(n))
                    {
                        self.discover(n, inst);
                    }
                }
            }
            pdh_samples(&values, now, dt)
        }
        fn refs(&self) -> Vec<DiskRef> {
            self.refs.refs()
        }
        fn disk_ref(&self, disk: &str) -> Option<DiskRef> {
            self.refs.get(disk)
        }
        fn watch(&mut self, r: DiskRef) {
            self.refs.watch(r)
        }
        fn close(&mut self) {
            if self.query != 0 {
                // SAFETY: valid query, closed once.
                unsafe { PdhCloseQuery(self.query as _) };
                self.query = 0;
            }
        }
    }

    impl Drop for PdhSampler {
        fn drop(&mut self) {
            self.close();
        }
    }

    /// `IOCTL_DISK_PERFORMANCE` on `\\.\PhysicalDriveN` (access 0: no elevation).
    pub struct WinPerfSampler {
        refs: RefSet,
        prev: HashMap<u32, DiskPerf>,
        rescan: f64,
    }

    impl WinPerfSampler {
        const MAX_DISKS: u32 = 64;

        pub fn new() -> std::io::Result<Self> {
            let mut me = Self {
                refs: RefSet::default(),
                prev: HashMap::new(),
                rescan: 0.0,
            };
            me.prev = me.read_all();
            if me.prev.is_empty() {
                return Err(std::io::Error::other("IOCTL_DISK_PERFORMANCE is not available"));
            }
            Ok(me)
        }

        fn read(number: u32) -> Option<DiskPerf> {
            let h = Handle::open(&format!("\\\\.\\PhysicalDrive{number}"), 0, 3, 0)?;
            parse_disk_performance(&h.ioctl(IOCTL_DISK_PERFORMANCE, &[], 88).ok()?)
        }

        fn read_all(&mut self) -> HashMap<u32, DiskPerf> {
            let now = monotonic();
            let numbers: Vec<u32> = if now >= self.rescan {
                self.rescan = now + 30.0; // pick up hot-plugged disks now and then
                (0..Self::MAX_DISKS).collect()
            } else {
                self.prev.keys().copied().collect()
            };
            let mut out = HashMap::new();
            for n in numbers {
                if let Some(perf) = Self::read(n) {
                    out.insert(n, perf);
                    if !self.refs.contains(&win_disk_id(n)) {
                        self.watch(DiskRef {
                            number: Some(n),
                            ..DiskRef::physical(win_disk_id(n))
                        });
                    }
                }
            }
            out
        }
    }

    impl DiskSampler for WinPerfSampler {
        fn name(&self) -> &'static str {
            "ioctl"
        }
        fn sample(&mut self) -> HashMap<DiskId, DiskSample> {
            let cur = self.read_all();
            let t = monotonic();
            let mut out = HashMap::new();
            for (n, perf) in &cur {
                if let Some(prev) = self.prev.get(n)
                    && let Some(s) = perf_sample(prev, perf, t)
                {
                    out.insert(win_disk_id(*n), s);
                }
            }
            self.prev = cur;
            out
        }
        fn refs(&self) -> Vec<DiskRef> {
            self.refs.refs()
        }
        fn disk_ref(&self, disk: &str) -> Option<DiskRef> {
            self.refs.get(disk)
        }
        fn watch(&mut self, r: DiskRef) {
            self.refs.watch(r)
        }
    }
}

// --------------------------------------------------------------------------- Windows: raw counters

/// `DISK_PERFORMANCE` (times in 100 ns units).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskPerf {
    pub bytes_read: i64,
    pub bytes_written: i64,
    pub read_time: i64,
    pub write_time: i64,
    pub idle_time: i64,
    pub reads: u32,
    pub writes: u32,
    pub query_time: i64,
}

pub fn parse_disk_performance(raw: &[u8]) -> Option<DiskPerf> {
    if raw.len() < 64 {
        return None;
    }
    let q = |o: usize| i64::from_le_bytes(raw[o..o + 8].try_into().expect("8 bytes"));
    let d = |o: usize| u32::from_le_bytes(raw[o..o + 4].try_into().expect("4 bytes"));
    // BytesRead, BytesWritten, ReadTime, WriteTime, IdleTime, ReadCount, WriteCount, QueueDepth,
    // SplitCount, QueryTime
    Some(DiskPerf {
        bytes_read: q(0),
        bytes_written: q(8),
        read_time: q(16),
        write_time: q(24),
        idle_time: q(32),
        reads: d(40),
        writes: d(44),
        query_time: q(56),
    })
}

pub fn perf_sample(prev: &DiskPerf, cur: &DiskPerf, t: f64) -> Option<DiskSample> {
    let span = cur.query_time - prev.query_time;
    if span <= 0 {
        return None;
    }
    let io_time = (cur.read_time - prev.read_time) + (cur.write_time - prev.write_time);
    let reads = cur.reads.wrapping_sub(prev.reads) as f64;
    let read_time = cur.read_time - prev.read_time;
    if io_time < 0 {
        return None;
    }
    let dt = span as f64 * 1e-7;
    Some(DiskSample {
        t,
        dt,
        busy: clamp01(1.0 - (cur.idle_time - prev.idle_time) as f64 / span as f64),
        queue: io_time as f64 / span as f64,
        read_iops: Some(reads / dt),
        read_latency: (reads > 0.0 && read_time >= 0).then(|| read_time as f64 * 1e-7 / reads),
        read_bps: Some((cur.bytes_read - prev.bytes_read).max(0) as f64 / dt),
    })
}

// --------------------------------------------------------------------------- Linux

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinuxStat {
    pub reads: u64,
    pub read_ms: u64,
    pub writes: u64,
    pub write_ms: u64,
    /// 512-byte units.
    pub sectors_read: u64,
    pub in_flight: u64,
    pub io_ticks: u64,
    pub time_in_queue: u64,
}

/// `/sys/block/<disk>/stat` (11, 15 or 17 fields; see Documentation/block/stat.rst).
pub fn parse_linux_stat(text: &str) -> Option<LinuxStat> {
    let fields: Vec<&str> = text.split_whitespace().collect();
    if fields.len() < 11 {
        return None;
    }
    let v: Vec<u64> = fields[..11].iter().map(|f| f.parse().ok()).collect::<Option<_>>()?;
    Some(LinuxStat {
        reads: v[0],
        read_ms: v[3],
        writes: v[4],
        write_ms: v[7],
        sectors_read: v[2],
        in_flight: v[8],
        io_ticks: v[9],
        time_in_queue: v[10],
    })
}

pub fn linux_sample(prev: &LinuxStat, cur: &LinuxStat, dt: f64, t: f64) -> Option<DiskSample> {
    if dt <= 0.0 {
        return None;
    }
    let delta = |a: u64, b: u64| b.checked_sub(a);
    // counter wrapped / device replaced
    let d_ticks = delta(prev.io_ticks, cur.io_ticks)? as f64;
    let d_queue = delta(prev.time_in_queue, cur.time_in_queue)? as f64;
    let d_reads = delta(prev.reads, cur.reads)? as f64;
    let d_read_ms = delta(prev.read_ms, cur.read_ms)? as f64;
    let ms = dt * 1000.0;
    Some(DiskSample {
        t,
        dt,
        busy: clamp01(d_ticks / ms),
        queue: d_queue / ms,
        read_iops: Some(d_reads / dt),
        read_latency: (d_reads > 0.0).then(|| d_read_ms / 1000.0 / d_reads),
        read_bps: Some(cur.sectors_read.saturating_sub(prev.sectors_read) as f64 * 512.0 / dt),
    })
}

pub struct SysfsSampler {
    root: PathBuf,
    refs: RefSet,
    prev: HashMap<DiskId, (f64, LinuxStat)>,
}

impl SysfsSampler {
    pub fn new(root: &Path) -> Self {
        let mut me = Self {
            root: root.to_path_buf(),
            refs: RefSet::default(),
            prev: HashMap::new(),
        };
        for name in linux_physical_disks(root) {
            #[cfg(all(unix, not(target_os = "macos")))]
            let media = crate::media::media_of("", &format!("/dev/{name}"), false);
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            let media = Media::Unknown;
            me.watch(DiskRef::physical(name.clone()).with_name(name).with_media(media));
        }
        me.sample();
        me
    }

    fn read(&self, name: &str) -> Option<LinuxStat> {
        parse_linux_stat(&std::fs::read_to_string(self.root.join("block").join(name).join("stat")).ok()?)
    }
}

impl DiskSampler for SysfsSampler {
    fn name(&self) -> &'static str {
        "sysfs"
    }
    fn sample(&mut self) -> HashMap<DiskId, DiskSample> {
        let mut out = HashMap::new();
        for r in self.refs.refs() {
            let cur = self.read(if r.name.is_empty() { &r.id } else { &r.name });
            let now = monotonic();
            let Some(cur) = cur else { continue };
            let prev = self.prev.insert(r.id.clone(), (now, cur));
            if let Some((t0, p)) = prev
                && let Some(s) = linux_sample(&p, &cur, now - t0, now)
            {
                out.insert(r.id.clone(), s);
            }
        }
        out
    }
    fn refs(&self) -> Vec<DiskRef> {
        self.refs.refs()
    }
    fn disk_ref(&self, disk: &str) -> Option<DiskRef> {
        self.refs.get(disk)
    }
    fn watch(&mut self, r: DiskRef) {
        self.refs.watch(r)
    }
}

// --------------------------------------------------------------------------- macOS

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DarwinStat {
    /// Summed service time of all reads and writes.
    pub total_ns: i64,
    pub reads: i64,
    pub read_ns: i64,
    pub read_bytes: i64,
}

fn bsd_name(entry: &serde_json::Value, depth: usize) -> Option<String> {
    if depth > 4 || !entry.is_object() {
        return None;
    }
    if let Some(name) = entry.get("BSD Name").and_then(|v| v.as_str()) {
        let digits = name.strip_prefix("disk").unwrap_or("");
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            return Some(name.to_owned());
        }
    }
    entry
        .get("IORegistryEntryChildren")
        .and_then(|c| c.as_array())
        .and_then(|children| children.iter().find_map(|c| bsd_name(c, depth + 1)))
}

/// `ioreg -r -c IOBlockStorageDriver -a` → whole-disk BSD name → cumulative statistics.
pub fn parse_ioreg(raw: &[u8]) -> HashMap<String, DarwinStat> {
    let mut out = HashMap::new();
    let Some(entries) = crate::disks::darwin::parse_plist(raw) else { return out };
    let list = match entries {
        serde_json::Value::Array(a) => a,
        other => vec![other],
    };
    for entry in &list {
        let (Some(stats), Some(name)) = (entry.get("Statistics").filter(|s| s.is_object()), bsd_name(entry, 0)) else {
            continue;
        };
        let n = |k: &str| stats.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        let read_ns = n("Total Time (Read)");
        out.insert(
            name,
            DarwinStat {
                total_ns: read_ns + n("Total Time (Write)"),
                reads: n("Operations (Read)"),
                read_ns,
                read_bytes: n("Bytes (Read)"),
            },
        );
    }
    out
}

pub fn darwin_sample(prev: &DarwinStat, cur: &DarwinStat, dt: f64, t: f64) -> Option<DiskSample> {
    let (d_ns, d_reads, d_read_ns) = (cur.total_ns - prev.total_ns, cur.reads - prev.reads, cur.read_ns - prev.read_ns);
    if dt <= 0.0 || d_ns < 0 || d_reads < 0 || d_read_ns < 0 {
        return None;
    }
    let queue = d_ns as f64 / 1e9 / dt;
    Some(DiskSample {
        t,
        dt,
        busy: clamp01(queue), // IOKit has no idle time: overlapping I/Os make this an upper bound
        queue,
        read_iops: Some(d_reads as f64 / dt),
        read_latency: (d_reads > 0).then(|| d_read_ns as f64 / 1e9 / d_reads as f64),
        read_bps: Some((cur.read_bytes - prev.read_bytes).max(0) as f64 / dt),
    })
}

/// Deltas of the cumulative statistics `ioreg` reports, per whole disk (one `ioreg` process per
/// sample, so at most one sample a second).
#[cfg(target_os = "macos")]
pub struct IoregSampler {
    refs: RefSet,
    prev: HashMap<String, (f64, DarwinStat)>,
}

#[cfg(target_os = "macos")]
impl IoregSampler {
    pub fn new() -> std::io::Result<Self> {
        let mut me = Self {
            refs: RefSet::default(),
            prev: HashMap::new(),
        };
        if Self::read().is_empty() {
            return Err(std::io::Error::other("ioreg returned no block storage statistics"));
        }
        me.sample();
        Ok(me)
    }

    fn read() -> HashMap<String, DarwinStat> {
        std::process::Command::new("ioreg")
            .args(["-r", "-c", "IOBlockStorageDriver", "-a"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| parse_ioreg(&o.stdout))
            .unwrap_or_default()
    }
}

#[cfg(target_os = "macos")]
impl DiskSampler for IoregSampler {
    fn name(&self) -> &'static str {
        "ioreg"
    }
    fn min_interval(&self) -> f64 {
        1.0
    }
    fn sample(&mut self) -> HashMap<DiskId, DiskSample> {
        let cur = Self::read();
        let now = monotonic();
        let mut out = HashMap::new();
        for (name, stat) in cur {
            if !self.refs.contains(&name) {
                self.watch(DiskRef::physical(name.clone()).with_name(name.clone()));
            }
            if let Some((t0, p)) = self.prev.insert(name.clone(), (now, stat))
                && let Some(s) = darwin_sample(&p, &stat, now - t0, now)
            {
                out.insert(name, s);
            }
        }
        out
    }
    fn refs(&self) -> Vec<DiskRef> {
        self.refs.refs()
    }
    fn disk_ref(&self, disk: &str) -> Option<DiskRef> {
        self.refs.get(disk)
    }
    fn watch(&mut self, r: DiskRef) {
        self.refs.watch(r)
    }
}

// --------------------------------------------------------------------------- selection

/// The best sampler this platform offers (never fails: falls back to [`NullSampler`]).
pub fn default_sampler() -> Box<dyn DiskSampler> {
    #[cfg(windows)]
    {
        match PdhSampler::new() {
            Ok(s) => return Box::new(s),
            Err(e) => tracing::warn!("disk load sampler pdh unavailable: {e}"),
        }
        match WinPerfSampler::new() {
            Ok(s) => return Box::new(s),
            Err(e) => tracing::warn!("disk load sampler ioctl unavailable: {e}"),
        }
    }
    #[cfg(target_os = "macos")]
    {
        match IoregSampler::new() {
            Ok(s) => return Box::new(s),
            Err(e) => tracing::warn!("disk load sampler ioreg unavailable: {e}"),
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return Box::new(SysfsSampler::new(Path::new("/sys")));
    }
    #[allow(unreachable_code)]
    Box::new(NullSampler::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    // a real line from Ubuntu 22.04 / kernel 6.6 (17 fields)
    const STAT_17: &str = " 4136354  4515195 69216064   470692  2082300  6051928 65073816  7927831        0   675528  8398529        0        0        0        0        1        5\n";

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * b.abs().max(1.0)
    }

    #[test]
    fn parse_linux_stat_field_counts() {
        let st = parse_linux_stat(STAT_17).unwrap();
        assert_eq!(st, LinuxStat {
            reads: 4136354,
            read_ms: 470692,
            writes: 2082300,
            write_ms: 7927831,
            sectors_read: 69216064,
            in_flight: 0,
            io_ticks: 675528,
            time_in_queue: 8398529,
        });
        let eleven = STAT_17.split_whitespace().take(11).collect::<Vec<_>>().join(" ");
        assert_eq!(parse_linux_stat(&eleven), Some(st)); // kernels before 4.18
        assert_eq!(parse_linux_stat("1 2 3"), None);
        assert_eq!(parse_linux_stat("a b c d e f g h i j k"), None);
    }

    #[test]
    fn linux_sample_deltas() {
        let prev = LinuxStat {
            reads: 1000,
            read_ms: 500,
            writes: 10,
            write_ms: 40,
            sectors_read: 8000,
            in_flight: 0,
            io_ticks: 10_000,
            time_in_queue: 20_000,
        };
        let cur = LinuxStat {
            reads: 3000,
            read_ms: 900,
            writes: 20,
            write_ms: 140,
            sectors_read: 8000 + 2000 * 32,
            in_flight: 3,
            io_ticks: 10_250,
            time_in_queue: 20_750,
        };
        let s = linux_sample(&prev, &cur, 0.5, 100.0).unwrap();
        assert!(close(s.busy, 0.5)); // 250 ms of 500 ms with I/O in flight
        assert!(close(s.queue, 1.5)); // 750 ms weighted / 500 ms
        assert!(close(s.read_iops.unwrap(), 4000.0));
        assert!(close(s.read_latency.unwrap(), 0.0002)); // 400 ms over 2000 reads
        assert!(close(s.read_bps.unwrap(), 2000.0 * 32.0 * 512.0 / 0.5)); // 16 KiB reads
        let wrapped = LinuxStat { io_ticks: 5, ..cur };
        assert_eq!(linux_sample(&prev, &wrapped, 0.5, 100.0), None);
        assert_eq!(linux_sample(&prev, &cur, 0.0, 100.0), None);
    }

    #[test]
    fn pdh_instances_and_combination() {
        assert_eq!(pdh_disk_number("3 C: D:"), Some(3));
        assert_eq!(pdh_disk_number("12"), Some(12));
        assert_eq!(pdh_disk_number("_Total"), None);
        assert_eq!(pdh_instance_letters("1 D: e:"), ["D:\\", "E:\\"]);
        let mut values: HashMap<String, HashMap<String, f64>> = HashMap::new();
        let mut put = |k: &str, kv: &[(&str, f64)]| {
            values.insert(k.into(), kv.iter().map(|(i, v)| (i.to_string(), *v)).collect());
        };
        put("idle", &[("0 D:", 100.0028), ("3 C:", 40.0), ("_Total", 70.0)]);
        put("queue", &[("0 D:", 0.0), ("3 C:", 2.5), ("_Total", 2.5)]);
        put("latency", &[("3 C:", 0.0001)]);
        put("iops", &[("3 C:", 25000.0)]);
        put("bps", &[("3 C:", 25000.0 * 16384.0)]);
        let out = pdh_samples(&values, 5.0, 0.25);
        let mut keys: Vec<_> = out.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["PhysicalDrive0", "PhysicalDrive3"]);
        let c = out["PhysicalDrive3"];
        assert!(close(c.busy, 0.6) && c.queue == 2.5 && c.dt == 0.25);
        assert_eq!((c.read_iops, c.read_latency, c.read_bps), (Some(25000.0), Some(0.0001), Some(25000.0 * 16384.0)));
        let d = out["PhysicalDrive0"];
        assert_eq!(d.busy, 0.0); // "% Idle Time" above 100 is clamped
        assert_eq!(d.read_iops, None);
    }

    fn perf_raw(v: [i64; 5], counts: [u32; 4], query: i64) -> Vec<u8> {
        let mut b = Vec::new();
        for x in v {
            b.extend_from_slice(&x.to_le_bytes());
        }
        for c in counts {
            b.extend_from_slice(&c.to_le_bytes());
        }
        b.extend_from_slice(&query.to_le_bytes());
        b.extend_from_slice(&3u32.to_le_bytes());
        b.resize(88, 0);
        b
    }

    #[test]
    fn disk_performance_counters() {
        let p0 = parse_disk_performance(&perf_raw([0; 5], [0; 4], 1_000_000)).unwrap();
        let p1 = parse_disk_performance(&perf_raw([100 * 4096, 0, 2_000_000, 500_000, 2_500_000], [100, 10, 2, 0], 6_000_000)).unwrap();
        assert_eq!(p1, DiskPerf {
            bytes_read: 100 * 4096,
            bytes_written: 0,
            read_time: 2_000_000,
            write_time: 500_000,
            idle_time: 2_500_000,
            reads: 100,
            writes: 10,
            query_time: 6_000_000,
        });
        let s = perf_sample(&p0, &p1, 1.0).unwrap();
        assert!(close(s.dt, 0.5)); // 5e6 × 100 ns
        assert!(close(s.busy, 0.5));
        assert!(close(s.queue, 0.5)); // (2e6 + 0.5e6) / 5e6
        assert!(close(s.read_iops.unwrap(), 200.0));
        assert!(close(s.read_latency.unwrap(), 0.002));
        assert_eq!(perf_sample(&p1, &p0, 1.0), None);
        assert_eq!(parse_disk_performance(b"short"), None);
    }

    #[test]
    fn ioreg_statistics() {
        let entry = |name: &str, reads: i64, read_ns: i64, write_ns: i64| {
            let mut stats = plist::Dictionary::new();
            stats.insert("Operations (Read)".into(), reads.into());
            stats.insert("Operations (Write)".into(), 7.into());
            stats.insert("Total Time (Read)".into(), read_ns.into());
            stats.insert("Total Time (Write)".into(), write_ns.into());
            stats.insert("Bytes (Read)".into(), (reads * 4096).into());
            let mut child = plist::Dictionary::new();
            child.insert("IOObjectClass".into(), "IOMedia".into());
            child.insert("BSD Name".into(), name.into());
            let mut e = plist::Dictionary::new();
            e.insert("IOObjectClass".into(), "IOBlockStorageDriver".into());
            e.insert("Statistics".into(), plist::Value::Dictionary(stats));
            e.insert("IORegistryEntryChildren".into(), plist::Value::Array(vec![plist::Value::Dictionary(child)]));
            plist::Value::Dictionary(e)
        };
        let mut no_stats = plist::Dictionary::new();
        no_stats.insert("no".into(), "stats".into());
        let doc = plist::Value::Array(vec![entry("disk0", 1000, 2_000_000_000, 0), plist::Value::Dictionary(no_stats)]);
        let mut raw = Vec::new();
        doc.to_writer_xml(&mut raw).unwrap();
        let stats = parse_ioreg(&raw);
        assert_eq!(stats, HashMap::from([("disk0".to_owned(), DarwinStat {
            total_ns: 2_000_000_000,
            reads: 1000,
            read_ns: 2_000_000_000,
            read_bytes: 4_096_000
        })]));
        let later = DarwinStat {
            total_ns: 2_500_000_000,
            reads: 2000,
            read_ns: 2_400_000_000,
            read_bytes: 8_192_000,
        };
        let s = darwin_sample(&stats["disk0"], &later, 1.0, 2.0).unwrap();
        assert!(close(s.queue, 0.5) && close(s.busy, 0.5));
        assert_eq!(s.read_iops, Some(1000.0));
        assert!(close(s.read_latency.unwrap(), 0.0004));
        assert!(parse_ioreg(b"not a plist").is_empty());
    }

    #[test]
    fn sysfs_sampler_on_a_fake_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("block/sda");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("stat"), STAT_17).unwrap();
        assert_eq!(linux_physical_disks(tmp.path()), ["sda"]);
        let mut sampler = SysfsSampler::new(tmp.path());
        assert_eq!(sampler.refs().iter().map(|r| r.id.clone()).collect::<Vec<_>>(), ["sda"]);
        let mut fields: Vec<u64> = STAT_17.split_whitespace().map(|f| f.parse().unwrap()).collect();
        fields[0] += 500; // reads
        fields[3] += 50; // read ms
        fields[9] += 100; // io_ticks
        fields[10] += 300; // time_in_queue
        std::thread::sleep(std::time::Duration::from_millis(200));
        std::fs::write(d.join("stat"), fields.iter().map(u64::to_string).collect::<Vec<_>>().join(" ")).unwrap();
        let out = sampler.sample();
        let s = out["sda"];
        assert!(0.0 < s.busy && s.busy <= 1.0 && s.queue > s.busy, "{s:?}");
        assert!(close(s.read_latency.unwrap(), 0.0001));
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn live_sampler_sees_the_disk_of_this_checkout() {
        let mut sampler = default_sampler();
        assert!(sampler.supported(), "{}", sampler.name());
        let disk = super::super::disks::disk_ref_for_path(&std::env::current_dir().unwrap()).id;
        sampler.sample();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let out = sampler.sample();
        sampler.close();
        if disk.starts_with("vol:") {
            return; // e.g. an overlay root in a container
        }
        let s = out.get(&disk).unwrap_or_else(|| panic!("{disk} not in {:?}", out.keys().collect::<Vec<_>>()));
        assert!((0.0..=1.0).contains(&s.busy) && s.queue >= 0.0 && 0.2 < s.dt && s.dt < 2.0, "{s:?}");
    }

    #[cfg(windows)]
    #[test]
    fn disk_performance_ioctl_without_elevation() {
        let mut sampler = WinPerfSampler::new().unwrap(); // access 0 on \\.\PhysicalDriveN
        std::thread::sleep(std::time::Duration::from_millis(200));
        let out = sampler.sample();
        let disk = super::super::disks::disk_ref_for_path(&std::env::current_dir().unwrap()).id;
        assert!(out.contains_key(&disk), "{disk}: {:?}", out.keys().collect::<Vec<_>>());
    }
}
