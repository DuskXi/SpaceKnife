//! How many folders the walker lists at once — its width — for calls on the process's shared
//! carriers (`threads` 0, the scanner's calls; docs/native.md 2.2 「列目录线程」).
//!
//! The width is a limit on listings in flight (`Slots`), not a thread count: every call's folders
//! take a slot before they are listed (walk.rs), so a new width holds within one listing, and the
//! carrier pool only needs at least as many threads (`capacity`). It starts at the start width,
//! the CPUs this process may use (`crate::cpus`), 2–16; on macOS twice that, 4–16 (APFS listings
//! wait in the kernel often enough that more threads keep the CPUs busy; docs/packaging.md). The
//! scanner calls `scan_slice` from as many threads.
//!
//! On a cold cache a listing waits for the disk most of its time (a folder's record, its index
//! blocks), and a handful of listings leave a disk with a handful of reads in flight; on a warm
//! cache more threads than CPUs only add switching, and on some file systems the listings contend
//! for the same locks so that fewer list faster (ReFS, warm, 32 CPUs: 8 listing threads 2.8 s,
//! 16 4.6 s, 64 6.1 s). The width follows what the listings do, over windows of at least 0.12 s
//! and 32 folders (or 1 s), measured as:
//!
//! - `u`, the CPU time of the whole process per second and per CPU it may use;
//! - `s`, the share of the width's slots spent listing: low when the scanner hands out too little
//!   work, or the disk's gate (the backend's disk-load limit) lets fewer through;
//! - `io`, disk reads per folder listed: Windows, the reads of the scan roots' volumes
//!   (IOCTL_DISK_PERFORMANCE; other programs' reads of them included); POSIX, this process's
//!   (getrusage ru_inblock: on Linux in 4 KiB, macOS counts each read);
//! - the rate: folders, plus entries / 64, listed per second.
//!
//! The rules (`decide`):
//!
//! - The disk is read (`io` >= 0.2) and the slots are busy (`s` > 0.5): straight to the cold
//!   width, twice the start width and at least 32 (on 4 CPUs a fixed 32 listed a WinBtrfs volume
//!   about twice as fast as 4, as fast as gdu). Beyond it, with CPU to spare (`u` < 0.6), a probe:
//!   the width doubles, and goes back unless the next window lists at least 15 % faster (the next
//!   probe then waits, 0.25 s doubling up to 4 s).
//! - Above the start width, nothing read any more (`io` < 0.05), or the CPUs busy (`u` > 0.9) with
//!   few reads: back to the start width.
//! - At or below the start width, nothing read and the slots busy: a warm probe tries half the
//!   width (at least a quarter of the start width, at least 2) — or, below the start width, twice
//!   it again, the two in turn — for one window, then the width it came from for one more; the
//!   new width stays if its window listed at least 15 % faster than the mean of the windows
//!   around it (that mean cancels a tree that gets slower or faster to list along the way).
//!   Throughput decides, not the CPU share: the threads that wait on a file system's locks may
//!   or may not burn CPU doing it.
//! - Without a read count (a volume without counters, a network share) it never goes above the
//!   start width. A window of 2 s with almost nothing listed (the scan ended, or was paused)
//!   starts over at the start width.
//!
//! The pool keeps the threads of the widest width until the width has been at or below the start
//! width for 2 s (or the scan ended); then it is rebuilt with the start width's.
//!
//! Environment (measurements): SPACEKNIFE_SCAN_THREADS=N (1–64) fixes the width and the
//! scanner's calling threads at N; SPACEKNIFE_WALK_ADAPT=0 keeps the start width;
//! SPACEKNIFE_WALK_MAX (default 64, 1–256; POSIX: also at most half the descriptors left below
//! RLIMIT_NOFILE) caps the width; SPACEKNIFE_WALK_TRACE=<file> appends every decision to that
//! file.

use std::io::Write;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::common::Unit;
use super::slots::Slots;

/// Shortest window a decision is made on, and the most folders it needs (fewer are enough once it
/// lasted MAX_WINDOW).
const MIN_WINDOW: Duration = Duration::from_millis(120);
const MIN_DIRS: u64 = 32;
const MAX_WINDOW: Duration = Duration::from_secs(1);
/// A window this long with almost nothing listed: the scan ended; the next one starts over.
const IDLE: Duration = Duration::from_secs(2);
/// The width at or below the start width this long: the pool gives up its extra threads.
const SHRINK_AFTER: Duration = Duration::from_secs(2);
const GROW_BELOW_U: f64 = 0.6;
const BUSY_ABOVE_S: f64 = 0.5;
const SHRINK_ABOVE_U: f64 = 0.9;
/// A warm probe tries a narrower width only below this: with the CPUs nearly all busy the
/// listings are not waiting for each other, and fewer list slower (4 CPUs, warm: u 0.85-0.96, half
/// the width listed at 0.55-0.65 of the rate, every time).
const NARROW_BELOW_U: f64 = 0.8;
/// Disk reads per folder listed: at least this to grow, below that for warm.
const IO_COLD: f64 = 0.2;
const IO_WARM: f64 = 0.05;
const PROBE_GAIN: f64 = 1.15;
const WARM_GAIN: f64 = 1.15;
const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(4);
/// No probe in a scan's first half second: a failed one costs a window at a worse width, too much
/// for a scan that lasts a second (a warm probe on C:\Program Files, 0.4 s, took 0.06 s of it).
const FIRST_PROBE: Duration = Duration::from_millis(500);
/// Entries that weigh as much as one folder in the rate (a warm folder's open and close against
/// its entries: big folders are not slow windows).
const ENTRIES_PER_DIR: f64 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The rules above.
    Adaptive,
    /// SPACEKNIFE_SCAN_THREADS: this many, always.
    Fixed,
    /// SPACEKNIFE_WALK_ADAPT=0: the start width, always.
    Start,
}

/// The widths a width moves between: the start width, the narrowest a warm probe tries, the cold
/// width and the most.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Widths {
    pub start: usize,
    pub least: usize,
    pub cold: usize,
    pub most: usize,
}

pub struct Pace {
    pub mode: Mode,
    pub n_cpu: usize,
    pub widths: Widths,
    /// The width: a limit on listings in flight.
    slots: Slots,
    /// Threads the carrier pool is to have (at least the width).
    capacity: AtomicUsize,
    /// This window: nanoseconds spent listing, folders and entries listed.
    busy_ns: AtomicU64,
    dirs: AtomicU64,
    entries: AtomicU64,
    /// The narrowest and widest width since `seen(true)`.
    low: AtomicUsize,
    high: AtomicUsize,
    ctl: Mutex<Ctl>,
    reads: Reads,
    trace: Option<Mutex<std::fs::File>>,
    born: Instant,
}

struct Ctl {
    start: Instant,
    clock: Option<Clock>,
    /// Disk reads counted up to `start` (None: no count to go by).
    reads: Option<u64>,
    probe: Option<Probe>,
    hold_until: Instant,
    backoff: Duration,
    /// Below the start width the warm probes go up and down in turn: up next.
    up_next: bool,
    /// Since when the width is at or below the start width while the pool has more threads.
    low_since: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Probe {
    /// Beyond the cold width: kept if its window lists PROBE_GAIN faster than the one before.
    Grow { from: usize, before: f64 },
    /// Warm: one window at `to`, one back at `from` (`trial` holds the first one's rate).
    Warm {
        from: usize,
        to: usize,
        before: f64,
        trial: Option<f64>,
    },
}

/// The process's CPU time and the wall clock in the same unit (Windows x86-64: TSC cycles, so the
/// process's cycle count needs no conversion; elsewhere nanoseconds or 100 ns).
#[derive(Clone, Copy)]
struct Clock {
    cpu: u64,
    wall: u64,
}

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
}

/// The start width for `n_cpu` usable CPUs (module docs).
pub fn start_width(n_cpu: usize) -> usize {
    if cfg!(target_os = "macos") {
        (2 * n_cpu).clamp(4, 16)
    } else {
        n_cpu.clamp(2, 16)
    }
}

/// The widths for a start width and a most (module docs).
pub fn widths(start: usize, most: usize) -> Widths {
    let most = most.max(start);
    Widths {
        start,
        least: (start / 4).max(2).min(start),
        cold: (2 * start).max(32).min(most),
        most,
    }
}

/// The process's pace (made on first use, from the environment).
pub fn get() -> &'static Pace {
    static PACE: OnceLock<Pace> = OnceLock::new();
    PACE.get_or_init(|| {
        let n_cpu = crate::cpus::usable();
        let fixed = env_usize("SPACEKNIFE_SCAN_THREADS")
            .filter(|&n| n > 0)
            .map(|n| n.min(64));
        let (mode, w) = match fixed {
            Some(n) => (Mode::Fixed, widths(n, n)),
            None => {
                let most = env_usize("SPACEKNIFE_WALK_MAX")
                    .unwrap_or(64)
                    .clamp(1, 256)
                    .min(fd_room());
                let mode = if std::env::var("SPACEKNIFE_WALK_ADAPT").is_ok_and(|v| v.trim() == "0")
                {
                    Mode::Start
                } else {
                    Mode::Adaptive
                };
                (mode, widths(start_width(n_cpu), most))
            }
        };
        let trace = std::env::var_os("SPACEKNIFE_WALK_TRACE").and_then(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
                .map(Mutex::new)
        });
        let now = Instant::now();
        let reads = Reads::default();
        let pace = Pace {
            mode,
            n_cpu,
            widths: w,
            slots: Slots::new(w.start),
            capacity: AtomicUsize::new(w.start),
            busy_ns: AtomicU64::new(0),
            dirs: AtomicU64::new(0),
            entries: AtomicU64::new(0),
            low: AtomicUsize::new(w.start),
            high: AtomicUsize::new(w.start),
            ctl: Mutex::new(Ctl {
                start: now,
                clock: clock(),
                reads: reads.total(),
                probe: None,
                hold_until: now + FIRST_PROBE,
                backoff: BACKOFF_MIN,
                up_next: false,
                low_since: None,
            }),
            reads,
            trace,
            born: now,
        };
        pace.log(&format!("start mode={mode:?} n_cpu={n_cpu} widths={w:?}"));
        pace
    })
}

impl Pace {
    /// The width: a limit on the listings in flight of calls on the carriers.
    pub fn slots(&'static self) -> &'static Slots {
        &self.slots
    }

    pub fn width(&self) -> usize {
        self.slots.limit()
    }

    /// Threads the carrier pool is to have now.
    pub fn capacity(&self) -> usize {
        self.capacity.load(Ordering::Relaxed)
    }

    /// The narrowest and widest width since the last reset (`reset`: start over from the width now).
    pub fn seen(&self, reset: bool) -> (usize, usize) {
        let w = self.width();
        if reset {
            (
                self.low.swap(w, Ordering::Relaxed),
                self.high.swap(w, Ordering::Relaxed),
            )
        } else {
            (
                self.low.load(Ordering::Relaxed),
                self.high.load(Ordering::Relaxed),
            )
        }
    }

    /// A call lists below this scan root (Windows: its volume's reads are counted from now on).
    pub fn note_root(&self, root: &[Unit]) {
        if self.mode == Mode::Adaptive {
            self.reads.note(root);
        }
    }

    /// One folder of `entries` entries was listed in `took`.
    pub fn listed(&self, took: Duration, entries: usize) {
        if self.mode == Mode::Adaptive {
            self.busy_ns
                .fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
            self.dirs.fetch_add(1, Ordering::Relaxed);
            self.entries.fetch_add(entries as u64, Ordering::Relaxed);
        }
    }

    fn set_width(&self, w: usize, c: &mut Ctl, now: Instant) {
        self.slots.set_limit(w);
        self.low.fetch_min(w, Ordering::Relaxed);
        self.high.fetch_max(w, Ordering::Relaxed);
        let cap = self.capacity();
        if w > cap {
            self.capacity.store(w, Ordering::Relaxed);
        }
        self.mind_capacity(c, now);
    }

    /// Back to the start width's threads once the width stayed at or below it for SHRINK_AFTER.
    fn mind_capacity(&self, c: &mut Ctl, now: Instant) {
        let (cap, start) = (self.capacity(), self.widths.start);
        if cap <= start || self.width() > start {
            c.low_since = None;
            return;
        }
        let since = *c.low_since.get_or_insert(now);
        if now - since >= SHRINK_AFTER {
            self.capacity.store(start, Ordering::Relaxed);
            c.low_since = None;
            self.log(&format!("pool {cap} -> {start} threads"));
        }
    }

    /// After every call: decides on the window behind it once it is long enough (one caller at a
    /// time; the others go on).
    pub fn after_call(&self) {
        if self.mode != Mode::Adaptive {
            return;
        }
        let Ok(mut c) = self.ctl.try_lock() else {
            return;
        };
        let now = Instant::now();
        let span = now - c.start;
        let dirs = self.dirs.load(Ordering::Relaxed);
        if span < MIN_WINDOW || (dirs < MIN_DIRS && span < MAX_WINDOW) {
            return;
        }
        let clk = clock();
        let reads = self.reads.total();
        let busy = self.busy_ns.swap(0, Ordering::Relaxed);
        let dirs = self.dirs.swap(0, Ordering::Relaxed);
        let entries = self.entries.swap(0, Ordering::Relaxed);
        let (prev_clk, prev_reads) = (c.clock, c.reads);
        c.clock = clk;
        c.start = now;
        c.reads = reads;
        let width = self.width();
        if span > IDLE && dirs < MIN_DIRS {
            // a new scan (or a pause): start over
            c.probe = None;
            c.backoff = BACKOFF_MIN;
            c.hold_until = now + FIRST_PROBE;
            c.up_next = false;
            c.low_since = None;
            let cap = self.capacity();
            if width != self.widths.start || cap != self.widths.start {
                self.slots.set_limit(self.widths.start);
                self.low.fetch_min(self.widths.start, Ordering::Relaxed);
                self.high.fetch_max(self.widths.start, Ordering::Relaxed);
                self.capacity.store(self.widths.start, Ordering::Relaxed);
                self.log(&format!(
                    "idle {:.1}s: width {width} -> {}, pool {cap} -> {} threads",
                    span.as_secs_f64(),
                    self.widths.start,
                    self.widths.start
                ));
            }
            return;
        }
        let secs = span.as_secs_f64().max(1e-6);
        let u = match (prev_clk, clk) {
            (Some(a), Some(b)) if b.wall > a.wall => Some(
                b.cpu.saturating_sub(a.cpu) as f64 / (b.wall - a.wall) as f64 / self.n_cpu as f64,
            ),
            _ => None,
        };
        let w = Window {
            u,
            s: (busy as f64 / 1e9 / secs / width as f64).min(1.0),
            rate: (dirs as f64 + entries as f64 / ENTRIES_PER_DIR) / secs,
            io: match (prev_reads, reads) {
                (Some(a), Some(b)) => Some(b.saturating_sub(a) as f64 / dirs.max(1) as f64),
                _ => None,
            },
        };
        let (action, next) = decide(&mut c, &w, width, self.widths, now);
        if next != width {
            self.set_width(next, &mut c, now);
        } else {
            self.mind_capacity(&mut c, now);
        }
        if self.trace.is_some() {
            self.log(&format!(
                "{action} u={} s={:.2} io={} rate={:.0}/s dirs={dirs} entries={entries} win={:.0}ms width {width} -> {next} pool {}",
                w.u.map_or("-".to_owned(), |x| format!("{x:.2}")),
                w.s,
                w.io.map_or("-".to_owned(), |x| format!("{x:.2}")),
                w.rate,
                secs * 1e3,
                self.capacity(),
            ));
        }
    }

    fn log(&self, line: &str) {
        if let Some(f) = &self.trace
            && let Ok(mut f) = f.lock()
        {
            let _ = writeln!(f, "{:.3} {line}", self.born.elapsed().as_secs_f64());
        }
    }
}

/// What one window measured (module docs; `u` None: no CPU clock).
struct Window {
    u: Option<f64>,
    s: f64,
    rate: f64,
    io: Option<f64>,
}

impl Ctl {
    /// A probe that did not pay: the next one waits longer.
    fn failed(&mut self, now: Instant) {
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
        self.hold_until = now + self.backoff;
    }

    fn kept(&mut self, now: Instant) {
        self.backoff = BACKOFF_MIN;
        self.hold_until = now + BACKOFF_MIN;
    }
}

/// The rules of the module docs: the next width after window `w` at `width`, and what was done.
fn decide(c: &mut Ctl, w: &Window, width: usize, l: Widths, now: Instant) -> (&'static str, usize) {
    let io = w.io.unwrap_or(0.0); // not counted: as if nothing were read
    let cold = io >= IO_COLD;
    match c.probe.take() {
        Some(Probe::Grow { from, before }) => {
            if w.rate < before * PROBE_GAIN || !cold {
                c.failed(now);
                return ("revert", from);
            }
            c.kept(now);
            return ("keep", width);
        }
        Some(Probe::Warm {
            from,
            to,
            before,
            trial,
        }) if !cold => {
            let Some(r) = trial else {
                c.probe = Some(Probe::Warm {
                    from,
                    to,
                    before,
                    trial: Some(w.rate),
                });
                return ("back", from);
            };
            c.up_next = to < from; // after a step down the next one goes up, and the reverse
            if r >= WARM_GAIN * (before + w.rate) / 2.0 {
                c.kept(now);
                return (if to < from { "narrow" } else { "widen" }, to);
            }
            c.failed(now);
            return ("stay", from);
        }
        _ => {} // no probe, or the disk began to be read during a warm one: the rules below
    }
    let busy_cpus = w.u.is_some_and(|u| u > SHRINK_ABOVE_U);
    if width > l.start && (io < IO_WARM || (busy_cpus && !cold)) {
        return ("warm", l.start);
    }
    if cold && w.s > BUSY_ABOVE_S {
        if width < l.cold {
            return ("cold", l.cold);
        }
        if w.u.is_some_and(|u| u < GROW_BELOW_U) && width < l.most && now >= c.hold_until {
            c.probe = Some(Probe::Grow {
                from: width,
                before: w.rate,
            });
            return ("grow", (width * 2).min(l.most));
        }
        return ("hold", width);
    }
    if io < IO_WARM && w.s > BUSY_ABOVE_S && width <= l.start && now >= c.hold_until {
        let may_narrow = width > l.least && w.u.is_none_or(|u| u < NARROW_BELOW_U);
        let down = may_narrow && !(c.up_next && width < l.start);
        let to = if down {
            (width / 2).max(l.least)
        } else {
            (width * 2).min(l.start)
        };
        if to != width {
            c.probe = Some(Probe::Warm {
                from: width,
                to,
                before: w.rate,
                trial: None,
            });
            return ("try", to);
        }
    }
    ("hold", width)
}

/// Disk reads to go by: Windows, the volumes of the scan roots calls named (IOCTL_DISK_PERFORMANCE
/// on a handle opened without access, as the backend's sampler does); POSIX, this process's own
/// block reads (getrusage ru_inblock, `inblock_reads`).
#[derive(Default)]
struct Reads {
    /// The roots noted, each with its volume's counters (None: none to be had).
    #[cfg(windows)]
    volumes: Mutex<Vec<Noted>>,
}

#[cfg(windows)]
type Noted = (Box<[Unit]>, Option<win::Volume>);

impl Reads {
    #[cfg(windows)]
    fn note(&self, root: &[Unit]) {
        let mut v = self.volumes.lock().unwrap_or_else(|e| e.into_inner());
        if root.is_empty() || v.iter().any(|(r, _)| &**r == root) || v.len() >= 8 {
            return;
        }
        v.push((root.into(), win::Volume::of(root)));
    }

    #[cfg(not(windows))]
    fn note(&self, _root: &[Unit]) {}

    #[cfg(windows)]
    fn total(&self) -> Option<u64> {
        let v = self.volumes.lock().unwrap_or_else(|e| e.into_inner());
        let mut sum = None;
        for vol in v.iter().filter_map(|(_, h)| h.as_ref()) {
            if let Some(n) = vol.reads() {
                *sum.get_or_insert(0) += n;
            }
        }
        sum
    }

    #[cfg(unix)]
    fn total(&self) -> Option<u64> {
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: ru is writable.
        (unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } == 0)
            .then(|| inblock_reads(ru.ru_inblock as u64))
    }
}

/// `ru_inblock` as reads. Linux counts the bytes the process had read from the disk in 512-byte
/// units (its I/O accounting), so eight make one 4 KiB read; macOS and the BSDs count input
/// operations, one per read, as the name says.
#[cfg(unix)]
fn inblock_reads(inblock: u64) -> u64 {
    if cfg!(target_os = "linux") {
        inblock / 8
    } else {
        inblock
    }
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, GetVolumeNameForVolumeMountPointW,
        GetVolumePathNameW, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{DISK_PERFORMANCE, IOCTL_DISK_PERFORMANCE};

    use super::Unit;

    /// A volume's handle for its performance counters, and the reads counted so far (the counter
    /// is 32 bits wide: widened here across wraps).
    pub struct Volume {
        h: HANDLE,
        last: std::sync::Mutex<(u32, u64)>,
    }

    // SAFETY: a kernel handle used only for DeviceIoControl, closed once on drop.
    unsafe impl Send for Volume {}
    unsafe impl Sync for Volume {}

    impl Drop for Volume {
        fn drop(&mut self) {
            // SAFETY: we own the handle.
            unsafe { CloseHandle(self.h) };
        }
    }

    impl Volume {
        /// The volume `root` lies on (its GUID name: also a folder a volume is mounted on).
        pub fn of(root: &[Unit]) -> Option<Self> {
            let path: Vec<u16> = root.iter().copied().chain([0]).collect();
            let mut mount = [0u16; 1024];
            let mut name = [0u16; 128];
            // SAFETY: NUL-terminated input, buffers of the given lengths.
            unsafe {
                if GetVolumePathNameW(path.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) == 0
                    || GetVolumeNameForVolumeMountPointW(
                        mount.as_ptr(),
                        name.as_mut_ptr(),
                        name.len() as u32,
                    ) == 0
                {
                    return None;
                }
            }
            let mut len = name.iter().position(|&u| u == 0)?;
            if len > 0 && name[len - 1] == b'\\' as u16 {
                len -= 1; // \\?\Volume{...}\ is the root folder; without the backslash, the device
            }
            name[len] = 0;
            // SAFETY: NUL-terminated name; no access asked (enough for FILE_ANY_ACCESS controls).
            let h = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if h == INVALID_HANDLE_VALUE || h.is_null() {
                return None;
            }
            let v = Self {
                h,
                last: std::sync::Mutex::new((0, 0)),
            };
            let first = v.count()?;
            *v.last.lock().unwrap_or_else(|e| e.into_inner()) = (first, 0);
            Some(v)
        }

        fn count(&self) -> Option<u32> {
            let mut perf = DISK_PERFORMANCE::default();
            let mut got = 0u32;
            // SAFETY: perf is writable for its size.
            let ok = unsafe {
                DeviceIoControl(
                    self.h,
                    IOCTL_DISK_PERFORMANCE,
                    std::ptr::null(),
                    0,
                    (&raw mut perf).cast(),
                    size_of::<DISK_PERFORMANCE>() as u32,
                    &mut got,
                    std::ptr::null_mut(),
                )
            };
            (ok != 0).then_some(perf.ReadCount)
        }

        /// Reads since the handle was opened.
        pub fn reads(&self) -> Option<u64> {
            let now = self.count()?;
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            let total = last.1 + now.wrapping_sub(last.0) as u64;
            *last = (now, total);
            Some(total)
        }
    }
}

/// POSIX: listings the descriptors allow (each holds a folder open; half of what is left below
/// the soft limit after 128 for everything else). Unlimited elsewhere.
fn fd_room() -> usize {
    #[cfg(unix)]
    {
        let mut r = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: r is writable.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) } == 0
            && r.rlim_cur != libc::RLIM_INFINITY
        {
            return ((r.rlim_cur as usize).saturating_sub(128) / 2).max(1);
        }
    }
    usize::MAX
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn clock() -> Option<Clock> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    use windows_sys::Win32::System::WindowsProgramming::QueryProcessCycleTime;
    let mut cycles = 0u64;
    // SAFETY: plain calls; `cycles` is writable. The process's cycle count is kept in TSC ticks,
    // the unit of _rdtsc.
    unsafe {
        if QueryProcessCycleTime(GetCurrentProcess(), &mut cycles) == 0 {
            return None;
        }
        Some(Clock {
            cpu: cycles,
            wall: core::arch::x86_64::_rdtsc(),
        })
    }
}

#[cfg(all(windows, not(target_arch = "x86_64")))]
fn clock() -> Option<Clock> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = *ORIGIN.get_or_init(Instant::now);
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
    // SAFETY: four writable FILETIMEs; the pseudo handle needs no closing. Process times are in
    // 100 ns units, charged per clock tick: coarse, but averaged over many threads and a window.
    if unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) } == 0 {
        return None;
    }
    let ft = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    Some(Clock {
        cpu: ft(k) + ft(u),
        wall: (origin.elapsed().as_nanos() / 100) as u64,
    })
}

#[cfg(unix)]
fn clock() -> Option<Clock> {
    let read = |id: libc::clockid_t| -> Option<u64> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: ts is writable.
        (unsafe { libc::clock_gettime(id, &mut ts) } == 0)
            .then(|| ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
    };
    Some(Clock {
        cpu: read(libc::CLOCK_PROCESS_CPUTIME_ID)?,
        wall: read(libc::CLOCK_MONOTONIC)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: Widths = Widths {
        start: 16,
        least: 4,
        cold: 32,
        most: 64,
    };

    fn ctl(now: Instant) -> Ctl {
        Ctl {
            start: now,
            clock: None,
            reads: None,
            probe: None,
            hold_until: now,
            backoff: BACKOFF_MIN,
            up_next: false,
            low_since: None,
        }
    }

    fn window(u: f64, s: f64, rate: f64, io: Option<f64>) -> Window {
        Window {
            u: Some(u),
            s,
            rate,
            io,
        }
    }

    /// Windows that read the disk (0.5 reads per folder), nothing, or a little.
    fn cold(u: f64, s: f64, rate: f64) -> Window {
        window(u, s, rate, Some(0.5))
    }

    fn warm(u: f64, s: f64, rate: f64) -> Window {
        window(u, s, rate, Some(0.0))
    }

    fn some(u: f64, s: f64, rate: f64) -> Window {
        window(u, s, rate, Some(0.1))
    }

    #[test]
    fn widths_follow_the_cpus() {
        if cfg!(target_os = "macos") {
            assert_eq!(start_width(1), 4);
            assert_eq!(start_width(3), 6);
            assert_eq!(start_width(32), 16);
        } else {
            assert_eq!(start_width(1), 2);
            assert_eq!(start_width(4), 4);
            assert_eq!(start_width(32), 16);
        }
        assert_eq!(widths(16, 64), L);
        assert_eq!(
            widths(4, 64),
            Widths {
                start: 4,
                least: 2,
                cold: 32,
                most: 64
            }
        );
        assert_eq!(
            widths(2, 64),
            Widths {
                start: 2,
                least: 2,
                cold: 32,
                most: 64
            }
        );
        assert_eq!(
            widths(16, 24),
            Widths {
                start: 16,
                least: 4,
                cold: 24,
                most: 24
            }
        );
        // a most below the start width (few descriptors): the start width
        assert_eq!(
            widths(16, 8),
            Widths {
                start: 16,
                least: 4,
                cold: 16,
                most: 16
            }
        );
        assert_eq!(
            widths(5, 5),
            Widths {
                start: 5,
                least: 2,
                cold: 5,
                most: 5
            }
        ); // fixed
    }

    #[test]
    fn cold_reads_take_it_to_the_cold_width_whatever_the_cpu() {
        let t = Instant::now();
        assert_eq!(
            decide(&mut ctl(t), &cold(0.3, 0.9, 1e4), 16, L, t),
            ("cold", 32)
        );
        assert_eq!(
            decide(&mut ctl(t), &cold(0.95, 0.9, 1e4), 16, L, t),
            ("cold", 32)
        );
        // from a narrowed width too
        assert_eq!(
            decide(&mut ctl(t), &cold(0.3, 0.9, 1e4), 4, L, t),
            ("cold", 32)
        );
        // the slots not busy: the scanner, or the disk's gate, lets too few through
        assert_eq!(
            decide(&mut ctl(t), &cold(0.2, 0.3, 1e4), 16, L, t),
            ("hold", 16)
        );
        // a few reads: neither cold nor warm
        assert_eq!(
            decide(&mut ctl(t), &some(0.2, 0.95, 1e4), 16, L, t),
            ("hold", 16)
        );
    }

    #[test]
    fn without_a_read_count_it_never_widens() {
        let t = Instant::now();
        let none = window(0.2, 0.95, 1e4, None);
        for width in [4, 8, 16] {
            let (_, next) = decide(&mut ctl(t), &none, width, L, t);
            assert!(next <= L.start, "{width} -> {next}");
        }
        // no CPU clock either: still never above the start width
        let blind = Window {
            u: None,
            s: 0.95,
            rate: 1e4,
            io: None,
        };
        assert!(decide(&mut ctl(t), &blind, 16, L, t).1 <= 16);
        // reads but no CPU clock: the cold width, no probe beyond it
        let no_clock = Window {
            u: None,
            ..cold(0.0, 0.95, 1e4)
        };
        assert_eq!(decide(&mut ctl(t), &no_clock, 16, L, t), ("cold", 32));
        assert_eq!(decide(&mut ctl(t), &no_clock, 32, L, t), ("hold", 32));
    }

    #[test]
    fn beyond_the_cold_width_a_probe_stays_only_if_it_lists_faster() {
        let t = Instant::now();
        let mut c = ctl(t);
        // CPU busy at the cold width: no probe
        assert_eq!(
            decide(&mut c, &cold(0.8, 0.9, 1000.0), 32, L, t),
            ("hold", 32)
        );
        assert_eq!(
            decide(&mut c, &cold(0.3, 0.9, 1000.0), 32, L, t),
            ("grow", 64)
        );
        assert_eq!(
            c.probe,
            Some(Probe::Grow {
                from: 32,
                before: 1000.0
            })
        );
        // not 15 % faster: back, and the next probe waits, longer each time
        assert_eq!(
            decide(&mut c, &cold(0.5, 0.9, 1100.0), 64, L, t),
            ("revert", 32)
        );
        assert_eq!(c.backoff, BACKOFF_MIN * 2);
        assert_eq!(
            decide(&mut c, &cold(0.3, 0.9, 1000.0), 32, L, t),
            ("hold", 32)
        );
        let later = t + BACKOFF_MIN * 2;
        assert_eq!(
            decide(&mut c, &cold(0.3, 0.9, 1000.0), 32, L, later),
            ("grow", 64)
        );
        // faster, but the reads stopped (the cache took over): back too
        assert_eq!(
            decide(&mut c, &warm(0.3, 0.9, 5000.0), 64, L, later),
            ("revert", 32)
        );
        assert_eq!(c.backoff, BACKOFF_MIN * 4);
        // a kept probe resets the wait; at the most, no more probes
        let much_later = later + BACKOFF_MAX;
        decide(&mut c, &cold(0.3, 0.9, 1000.0), 32, L, much_later);
        assert_eq!(
            decide(&mut c, &cold(0.3, 0.9, 2000.0), 64, L, much_later),
            ("keep", 64)
        );
        assert_eq!(c.backoff, BACKOFF_MIN);
        let after = much_later + BACKOFF_MIN;
        assert_eq!(
            decide(&mut c, &cold(0.3, 0.9, 2000.0), 64, L, after),
            ("hold", 64)
        );
    }

    #[test]
    fn above_the_start_width_no_reads_or_busy_cpus_go_back() {
        let t = Instant::now();
        assert_eq!(
            decide(&mut ctl(t), &warm(0.3, 0.9, 1e5), 64, L, t),
            ("warm", 16)
        );
        assert_eq!(
            decide(&mut ctl(t), &some(0.95, 0.9, 1e5), 32, L, t),
            ("warm", 16)
        );
        let none = window(0.3, 0.9, 1e5, None);
        assert_eq!(decide(&mut ctl(t), &none, 32, L, t), ("warm", 16));
        // CPUs busy while the disk is still read: stays (WinBtrfs cold on 4 CPUs)
        assert_eq!(
            decide(&mut ctl(t), &cold(0.95, 0.9, 1e5), 32, L, t),
            ("hold", 32)
        );
        // a few reads with CPU to spare: stays
        assert_eq!(
            decide(&mut ctl(t), &some(0.5, 0.9, 1e5), 32, L, t),
            ("hold", 32)
        );
    }

    #[test]
    fn warm_and_contended_a_narrower_width_stays_if_it_lists_faster() {
        let t = Instant::now();
        let mut c = ctl(t);
        // warm, the slots busy: try half
        assert_eq!(
            decide(&mut c, &warm(0.4, 0.95, 1000.0), 16, L, t),
            ("try", 8)
        );
        // the trial window lists 1600/s; back to 16 for one window, 1000/s again: 8 stays
        assert_eq!(
            decide(&mut c, &warm(0.3, 0.95, 1600.0), 8, L, t),
            ("back", 16)
        );
        assert_eq!(
            decide(&mut c, &warm(0.4, 0.95, 1000.0), 16, L, t),
            ("narrow", 8)
        );
        assert_eq!(c.backoff, BACKOFF_MIN);
        // the next probe goes up again (and waits a little first)
        assert_eq!(
            decide(&mut c, &warm(0.3, 0.95, 1600.0), 8, L, t),
            ("hold", 8)
        );
        let t1 = t + BACKOFF_MIN;
        assert_eq!(
            decide(&mut c, &warm(0.3, 0.95, 1600.0), 8, L, t1),
            ("try", 16)
        );
        assert_eq!(
            decide(&mut c, &warm(0.4, 0.95, 1000.0), 16, L, t1),
            ("back", 8)
        );
        assert_eq!(
            decide(&mut c, &warm(0.3, 0.95, 1600.0), 8, L, t1),
            ("stay", 8)
        );
        assert_eq!(c.backoff, BACKOFF_MIN * 2);
        // then down: 4 is the least for a start width of 16
        let t2 = t1 + BACKOFF_MIN * 2;
        assert_eq!(
            decide(&mut c, &warm(0.3, 0.95, 1600.0), 8, L, t2),
            ("try", 4)
        );
        assert_eq!(
            decide(&mut c, &warm(0.2, 0.95, 1700.0), 4, L, t2),
            ("back", 8)
        );
        assert_eq!(
            decide(&mut c, &warm(0.3, 0.95, 1600.0), 8, L, t2),
            ("stay", 8)
        );
        // the CPUs nearly all busy: the listings do not wait for each other, no narrower width
        assert_eq!(decide(&mut ctl(t), &warm(0.9, 0.95, 1000.0), 16, L, t), ("hold", 16));
        let mut c = ctl(t);
        c.up_next = false;
        assert_eq!(decide(&mut c, &warm(0.9, 0.95, 1000.0), 8, L, t), ("try", 16)); // up still
        // at the least only up; at the start width only down
        let mut c = ctl(t);
        c.up_next = false;
        assert_eq!(decide(&mut c, &warm(0.3, 0.95, 1.0), 4, L, t).1, 8);
        let mut c = ctl(t);
        c.up_next = true;
        assert_eq!(decide(&mut c, &warm(0.3, 0.95, 1.0), 16, L, t).1, 8);
    }

    #[test]
    fn a_warm_probe_needs_a_clear_gain_over_the_windows_around_it() {
        let t = Instant::now();
        // a tree that lists faster and faster along the way: the narrow window looks better than
        // the one before it, not than the mean around it
        let mut c = ctl(t);
        decide(&mut c, &warm(0.4, 0.95, 1000.0), 16, L, t);
        decide(&mut c, &warm(0.4, 0.95, 1300.0), 8, L, t);
        assert_eq!(
            decide(&mut c, &warm(0.4, 0.95, 1600.0), 16, L, t),
            ("stay", 16)
        );
        // not busy (the scanner or the gate is the limit): no warm probe
        assert_eq!(
            decide(&mut ctl(t), &warm(0.4, 0.4, 1000.0), 16, L, t),
            ("hold", 16)
        );
        // a few reads: no warm probe either
        assert_eq!(
            decide(&mut ctl(t), &some(0.4, 0.95, 1000.0), 16, L, t),
            ("hold", 16)
        );
        // the disk began to be read during the probe: the cold rules
        let mut c = ctl(t);
        decide(&mut c, &warm(0.4, 0.95, 1000.0), 16, L, t);
        assert_eq!(
            decide(&mut c, &cold(0.4, 0.95, 900.0), 8, L, t),
            ("cold", 32)
        );
        assert_eq!(c.probe, None);
        // a start width of 2 has nothing narrower to try
        let two = widths(2, 64);
        assert_eq!(
            decide(&mut ctl(t), &warm(0.4, 0.95, 1000.0), 2, two, t),
            ("hold", 2)
        );
    }

    #[test]
    fn the_clock_runs() {
        let a = clock().expect("a process CPU clock");
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed() < Duration::from_millis(60) {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        std::hint::black_box(x);
        let b = clock().unwrap();
        let share = (b.cpu - a.cpu) as f64 / (b.wall - a.wall) as f64;
        // this thread busy the whole time: about one CPU-second per second at least (other tests
        // run in this process at the same time; coarse process times on Windows ARM)
        assert!(share > 0.5, "{share}");
    }

    #[test]
    fn reads_can_be_counted() {
        let r = Reads::default();
        #[cfg(windows)]
        {
            let tmp: Vec<u16> = std::env::temp_dir()
                .to_string_lossy()
                .encode_utf16()
                .collect();
            r.note(&tmp);
        }
        assert!(r.total().is_some());
    }
}
