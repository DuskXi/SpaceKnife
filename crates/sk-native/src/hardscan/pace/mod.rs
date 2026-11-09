//! How many folders the walker lists at once for calls on the process's shared carriers
//! (`threads` 0, the scanner's calls; docs/native.md 2.2 「列目录线程」).
//!
//! Two lanes, each a limit on listings in flight (`Slots`), not a thread count: folders expected
//! to wait for the disk (cold) are listed in the IO lane, the others in the CPU lane. Every
//! listing is a sample: its wall time less the CPU time its thread used is its wait; the model
//! (model.rs) turns waits into the posterior that a folder was cold, which its subfolders take as
//! their prior (walk.rs `Node::prior`, `Task::prior`), and each lane follows its own
//! rate-against-listings-in-flight curve (lane.rs); ctl.rs puts them together. The controller is
//! clock-free; this module runs it with the real clock under a lock that carriers take with
//! `try_lock` only: a carrier that finds it busy leaves its sample in the inbox for the holder.
//! What a carrier needs at every listing (the model's parameters, the lanes' limits, the calls
//! wanted) is published in atomics.
//!
//! The carrier pool has at least as many threads as the two lanes' widths together, rounded up
//! to a multiple of 8 (widths change every few milliseconds; a pool is not rebuilt for each); it
//! goes back to fewer threads once the widths stayed 8 below it for 2 s (or the scan ended).
//!
//! A pace follows one medium at a time ([`super::DeviceHint`]): the first call after it was idle
//! for a second names it (its first root's); another medium then starts the lanes and the model
//! over.
//!
//! Environment (measurements): SPACEKNIFE_SCAN_THREADS=N (1–64) one lane of N, and the scanner's
//! calls N; SPACEKNIFE_WALK_ADAPT=0 one lane at the start width; SPACEKNIFE_WALK_LANES=0 one lane
//! following its curve; SPACEKNIFE_WALK_MAX (default 64, 1–256) the IO lane's widest (POSIX: the
//! lanes together also at most half the descriptors left below RLIMIT_NOFILE);
//! SPACEKNIFE_WALK_TRACE=<file> appends every round's decision to that file.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::common::Unit;
use super::slots::Slots;
use super::{DeviceHint, Medium};

mod ctl;
mod lane;
mod model;
#[cfg(test)]
mod sim;
#[cfg(test)]
mod v1;

pub use ctl::{CPU, IO, Mode, Point, Sample, units};
pub use model::{Params, feature};

/// The widths at or below the pool's threads less this for SHRINK_AFTER: a smaller pool.
const SHRINK_AFTER: Duration = Duration::from_secs(2);
const POOL_STEP: usize = 8;
/// Samples a busy controller keeps waiting for it (more are dropped: it is far behind).
const INBOX_MAX: usize = 4096;

/// The start width for `n_cpu` usable CPUs: the CPU lane's start (halved past 8, ctl.rs) and the
/// scanner's first calls.
pub fn start_width(n_cpu: usize) -> usize {
    if cfg!(target_os = "macos") {
        (2 * n_cpu).clamp(4, 16)
    } else {
        n_cpu.clamp(2, 16)
    }
}

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
}

fn env_off(key: &str) -> bool {
    std::env::var(key).is_ok_and(|v| v.trim() == "0")
}

/// The controller's configuration from the CPUs and the environment (module docs).
fn config() -> ctl::Config {
    let n_cpu = crate::cpus::usable();
    let start = start_width(n_cpu);
    let fixed = env_usize("SPACEKNIFE_SCAN_THREADS")
        .filter(|&n| n > 0)
        .map(|n| n.min(64));
    let mode = if fixed.is_some() {
        Mode::Fixed
    } else if env_off("SPACEKNIFE_WALK_ADAPT") {
        Mode::Start
    } else if env_off("SPACEKNIFE_WALK_LANES") {
        Mode::OneLane
    } else {
        Mode::Lanes
    };
    let cpu_most = if cfg!(target_os = "macos") {
        2 * n_cpu
    } else {
        n_cpu
    }
    .clamp(start, 64);
    let room = fd_room();
    let io_most = env_usize("SPACEKNIFE_WALK_MAX")
        .unwrap_or(64)
        .clamp(1, 256)
        .min(room.saturating_sub(cpu_most).max(1));
    ctl::Config {
        mode,
        n_cpu,
        start,
        least: (start / 4).max(2).min(start),
        cpu_most: cpu_most.min(room),
        io_most,
        fixed: fixed.unwrap_or(start),
        cpu_per_call: if cfg!(windows) { 2 } else { 1 },
    }
}

/// A thread's CPU clock at a moment: (CPU, wall) in one unit (Windows x86-64: the thread's cycle
/// count and the TSC, whose units agree; elsewhere the thread's CPU nanoseconds).
#[derive(Clone, Copy, Debug)]
pub struct CpuMark(u64, u64);

/// The calling thread's CPU clock now (None: none to be had: Windows on ARM).
#[cfg(all(windows, target_arch = "x86_64"))]
pub fn cpu_mark() -> Option<CpuMark> {
    use windows_sys::Win32::System::Threading::GetCurrentThread;
    use windows_sys::Win32::System::WindowsProgramming::QueryThreadCycleTime;
    let mut cycles = 0u64;
    // SAFETY: plain calls; `cycles` is writable; the pseudo handle needs no closing. The thread's
    // cycle count is kept in TSC ticks, the unit of _rdtsc (busy: 0.9965 of the TSC, asleep 0.0002;
    // about 115 ns a call, docs/native.md 2.2).
    unsafe {
        if QueryThreadCycleTime(GetCurrentThread(), &mut cycles) == 0 {
            return None;
        }
        Some(CpuMark(cycles, core::arch::x86_64::_rdtsc()))
    }
}

#[cfg(all(windows, not(target_arch = "x86_64")))]
pub fn cpu_mark() -> Option<CpuMark> {
    None // the thread times Windows keeps elsewhere are in clock ticks: too coarse for a listing
}

#[cfg(unix)]
pub fn cpu_mark() -> Option<CpuMark> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is writable.
    (unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } == 0)
        .then(|| CpuMark(ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64, 0))
}

/// The CPU time the thread used between two marks, in ns, for a stretch of `wall_ns` (at most
/// that).
pub fn cpu_ns(a: CpuMark, b: CpuMark, wall_ns: u64) -> u64 {
    let used = b.0.saturating_sub(a.0);
    let cpu = if cfg!(all(windows, target_arch = "x86_64")) {
        let wall = b.1.saturating_sub(a.1).max(1);
        (wall_ns as f64 * (used as f64 / wall as f64)) as u64
    } else {
        used
    };
    cpu.min(wall_ns)
}

/// The width of one lane: now, the narrowest and widest since the last reset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LaneSeen {
    pub width: usize,
    pub low: usize,
    pub high: usize,
}

struct Seen {
    low: AtomicUsize,
    high: AtomicUsize,
}

impl Seen {
    fn new(v: usize) -> Self {
        Seen {
            low: AtomicUsize::new(v),
            high: AtomicUsize::new(v),
        }
    }

    fn note(&self, v: usize) {
        self.low.fetch_min(v, Ordering::Relaxed);
        self.high.fetch_max(v, Ordering::Relaxed);
    }

    fn get(&self, now: usize, reset: bool) -> (usize, usize) {
        if reset {
            (
                self.low.swap(now, Ordering::Relaxed),
                self.high.swap(now, Ordering::Relaxed),
            )
        } else {
            (
                self.low.load(Ordering::Relaxed),
                self.high.load(Ordering::Relaxed),
            )
        }
    }
}

/// The model's parameters as carriers read them: f32 bits (a torn read between fields is
/// harmless: they move slowly).
struct Published {
    mu: [AtomicU32; 2],
    sd: [AtomicU32; 2],
    kappa: AtomicU32,
    cold: AtomicU32,
    quiet: AtomicBool,
}

impl Published {
    fn new(p: Params) -> Self {
        let f = |x: f32| AtomicU32::new(x.to_bits());
        Published {
            mu: [f(p.mu[0]), f(p.mu[1])],
            sd: [f(p.sd[0]), f(p.sd[1])],
            kappa: f(p.kappa),
            cold: f(p.cold),
            quiet: AtomicBool::new(p.quiet),
        }
    }

    fn set(&self, p: Params) {
        let s = |a: &AtomicU32, x: f32| a.store(x.to_bits(), Ordering::Relaxed);
        s(&self.mu[0], p.mu[0]);
        s(&self.mu[1], p.mu[1]);
        s(&self.sd[0], p.sd[0]);
        s(&self.sd[1], p.sd[1]);
        s(&self.kappa, p.kappa);
        s(&self.cold, p.cold);
        self.quiet.store(p.quiet, Ordering::Relaxed);
    }

    fn get(&self) -> Params {
        let g = |a: &AtomicU32| f32::from_bits(a.load(Ordering::Relaxed));
        Params {
            mu: [g(&self.mu[0]), g(&self.mu[1])],
            sd: [g(&self.sd[0]), g(&self.sd[1])],
            kappa: g(&self.kappa),
            cold: g(&self.cold),
            quiet: self.quiet.load(Ordering::Relaxed),
        }
    }
}

struct Held {
    ctl: ctl::Ctl,
    /// Since when the widths stay POOL_STEP below the pool's threads.
    low_since: Option<Instant>,
}

pub struct Pace {
    pub mode: Mode,
    pub n_cpu: usize,
    /// The lanes' limits on listings in flight.
    lanes: [Slots; 2],
    /// Threads the carrier pool is to have (at least the lanes' widths together).
    capacity: AtomicUsize,
    calls: AtomicUsize,
    params: Published,
    held: Mutex<Held>,
    inbox: Mutex<Vec<(u64, Sample)>>,
    seen: [Seen; 2],
    seen_calls: Seen,
    reads: Reads,
    trace: Option<Mutex<std::fs::File>>,
    born: Instant,
}

/// The process's pace (made on first use, from the environment).
pub fn get() -> &'static Pace {
    static PACE: OnceLock<Pace> = OnceLock::new();
    PACE.get_or_init(|| {
        let cfg = config();
        let trace = std::env::var_os("SPACEKNIFE_WALK_TRACE").and_then(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
                .map(Mutex::new)
        });
        let mut ctl = ctl::Ctl::new(cfg, DeviceHint::default());
        if trace.is_some() {
            ctl.trace = Some(Vec::new());
        }
        let widths = ctl.widths();
        let pace = Pace {
            mode: cfg.mode,
            n_cpu: cfg.n_cpu,
            lanes: [Slots::new(widths[IO]), Slots::new(widths[CPU])],
            capacity: AtomicUsize::new(pool_size(widths)),
            calls: AtomicUsize::new(ctl.calls()),
            params: Published::new(ctl.params()),
            seen: [Seen::new(widths[IO]), Seen::new(widths[CPU])],
            seen_calls: Seen::new(ctl.calls()),
            held: Mutex::new(Held {
                ctl,
                low_since: None,
            }),
            inbox: Mutex::new(Vec::new()),
            reads: Reads::default(),
            trace,
            born: Instant::now(),
        };
        // the wall clock of time 0, to line the trace up with others (SPACEKNIFE_IO_TRACE)
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64() - pace.born.elapsed().as_secs_f64());
        pace.log(&format!("start {cfg:?} wall={wall:.3}"));
        pace
    })
}

/// The lanes' configurations of a pace (their bounds: hi is the widest).
pub fn lane_cfgs_of(p: &Pace) -> [lane::LaneCfg; 2] {
    let h = p.held.lock().unwrap_or_else(|e| e.into_inner());
    [h.ctl.lanes[IO].cfg, h.ctl.lanes[CPU].cfg]
}

/// Carrier threads for these widths: their sum rounded up to POOL_STEP.
fn pool_size(widths: [usize; 2]) -> usize {
    (widths[IO] + widths[CPU]).div_ceil(POOL_STEP).max(1) * POOL_STEP
}

impl Pace {
    /// The limit of `lane`'s listings in flight.
    pub fn lane(&'static self, lane: usize) -> &'static Slots {
        &self.lanes[lane.min(CPU)]
    }

    /// Both lanes' limits together.
    pub fn width(&self) -> usize {
        self.lanes[IO].limit() + self.lanes[CPU].limit()
    }

    /// The limits of the lanes (one lane in the fixed and start modes: the CPU lane's).
    pub fn widths(&self) -> [usize; 2] {
        [self.lanes[IO].limit(), self.lanes[CPU].limit()]
    }

    /// Threads the carrier pool is to have now.
    pub fn capacity(&self) -> usize {
        self.capacity.load(Ordering::Relaxed)
    }

    /// Calls a scanner should keep in flight now (ctl.rs `calls`).
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    /// The model's parameters now: a carrier judges its listing with them.
    pub fn params(&self) -> Params {
        self.params.get()
    }

    /// The lane a folder of prior `prior` (NaN: none) goes to.
    pub fn lane_for(&self, prior: f32) -> usize {
        let p = if prior.is_nan() {
            self.params().unknown_prior()
        } else {
            prior
        };
        ctl::lane_for(self.mode, p)
    }

    /// The narrowest and widest of each lane and of the calls since the last reset (`reset`:
    /// start over from now).
    pub fn seen(&self, reset: bool) -> ([LaneSeen; 2], (usize, usize)) {
        let w = self.widths();
        let lane = |k: usize| {
            let (low, high) = self.seen[k].get(w[k], reset);
            LaneSeen {
                width: w[k],
                low,
                high,
            }
        };
        (
            [lane(IO), lane(CPU)],
            self.seen_calls.get(self.calls(), reset),
        )
    }

    /// A call lists below this scan root (Windows: its volume's reads are counted from now on).
    pub fn note_root(&self, root: &[Unit]) {
        if self.mode == Mode::Lanes || self.mode == Mode::OneLane {
            self.reads.note(root);
        }
    }

    /// A call lists on a device of this kind: a pace that has been idle for a second (a new scan)
    /// follows it from now on.
    pub fn note_hint(&self, hint: DeviceHint) {
        let now = self.now();
        let mut h = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if h.ctl.hint != hint && !h.ctl.active(now) {
            h.ctl.restart(hint);
            self.publish(&mut h, now);
        }
    }

    fn now(&self) -> u64 {
        self.born.elapsed().as_nanos() as u64
    }

    /// One listing finished (walk.rs); the controller learns from it now, or the next carrier
    /// that gets its lock does.
    pub fn listed(&self, s: Sample) {
        let now = self.now();
        let Ok(mut h) = self.held.try_lock() else {
            let mut inbox = self.inbox.lock().unwrap_or_else(|e| e.into_inner());
            if inbox.len() < INBOX_MAX {
                inbox.push((now, s));
            }
            return;
        };
        let waiting = std::mem::take(&mut *self.inbox.lock().unwrap_or_else(|e| e.into_inner()));
        for (t, w) in waiting {
            h.ctl.sample(&w, t);
        }
        h.ctl.sample(&s, now);
        if h.ctl.counter_due(now) {
            let total = self.reads.total();
            h.ctl.counter(total, now);
        }
        self.publish(&mut h, now);
    }

    /// Samples left in the inbox are learnt from (after a call: none stays behind when a scan
    /// ends).
    pub fn after_call(&self) {
        if self.inbox.lock().map(|i| i.is_empty()).unwrap_or(true) {
            return;
        }
        if let Ok(mut h) = self.held.try_lock() {
            let now = self.now();
            let waiting =
                std::mem::take(&mut *self.inbox.lock().unwrap_or_else(|e| e.into_inner()));
            for (t, w) in waiting {
                h.ctl.sample(&w, t);
            }
            self.publish(&mut h, now);
        }
    }

    /// The controller's state out to the carriers: the parameters, the lanes' limits, the calls,
    /// the pool's size; and the trace.
    fn publish(&self, h: &mut Held, now: u64) {
        self.params.set(h.ctl.params());
        let widths = h.ctl.widths();
        for k in [IO, CPU] {
            if self.lanes[k].limit() != widths[k] {
                self.lanes[k].set_limit(widths[k]);
            }
            self.seen[k].note(widths[k]);
        }
        let calls = h.ctl.calls();
        self.calls.store(calls, Ordering::Relaxed);
        self.seen_calls.note(calls);
        let want = pool_size(widths);
        let cap = self.capacity();
        let at = self.born + Duration::from_nanos(now);
        if want > cap {
            self.capacity.store(want, Ordering::Relaxed);
            h.low_since = None;
            self.log(&format!("pool {cap} -> {want} threads"));
        } else if want + POOL_STEP <= cap {
            let since = *h.low_since.get_or_insert(at);
            if at.saturating_duration_since(since) >= SHRINK_AFTER {
                self.capacity.store(want, Ordering::Relaxed);
                h.low_since = None;
                self.log(&format!("pool {cap} -> {want} threads"));
            }
        } else {
            h.low_since = None;
        }
        if let Some(lines) = h.ctl.trace.as_mut().filter(|l| !l.is_empty()) {
            let lines = std::mem::take(lines);
            if let Some(f) = &self.trace
                && let Ok(mut f) = f.lock()
            {
                for l in lines {
                    let _ = writeln!(f, "{l}");
                }
            }
        }
    }

    /// The report of the stretch since the last reset (`reset`: a new one begins).
    pub fn report(&self, reset: bool) -> Report {
        let mut h = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let timeline = h.ctl.timeline.snapshot();
        let r = Report {
            timeline,
            medium: h.ctl.hint.medium,
            vetoed: h.ctl.model.vetoed,
            restarts: h.ctl.restarts,
            cold: h.ctl.model.cold(),
            cpu_share: [h.ctl.cpu_share(IO), h.ctl.cpu_share(CPU)],
        };
        if reset {
            h.ctl.reset_timeline();
        }
        r
    }

    fn log(&self, line: &str) {
        if let Some(f) = &self.trace
            && let Ok(mut f) = f.lock()
        {
            let _ = writeln!(f, "{:.3} {line}", self.born.elapsed().as_secs_f64());
        }
    }
}

/// What a pace tells of a stretch of listing (`Pace::report`).
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub timeline: Vec<Point>,
    pub medium: Medium,
    pub vetoed: u64,
    pub restarts: u64,
    pub cold: f64,
    pub cpu_share: [Option<f64>; 2],
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(pool_size([16, 8]), 24);
        assert_eq!(pool_size([17, 8]), 32);
        assert_eq!(pool_size([1, 1]), 8);
    }

    #[test]
    fn a_threads_cpu_time_is_measured() {
        let (Some(a), t) = (cpu_mark(), Instant::now()) else {
            return; // no thread clock here (Windows on ARM)
        };
        // busy until the thread's clock says 20 ms: how much of the wall time that takes is up to
        // the scheduler and the machine (on a shared CI runner this thread once had 9.6 µs of
        // 35 ms: a virtual processor taken away goes on in wall time), so the wall time is only a
        // bound, for a clock that does not move
        const WANT: u64 = 20_000_000;
        const BOUND: Duration = Duration::from_secs(5);
        let mut x = 0u64;
        let (busy, wall) = loop {
            for _ in 0..10_000 {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            }
            std::hint::black_box(x);
            let wall = t.elapsed().as_nanos() as u64;
            let busy = cpu_ns(a, cpu_mark().unwrap(), wall);
            if busy >= WANT || wall > BOUND.as_nanos() as u64 {
                break (busy, wall);
            }
        };
        assert!(busy >= WANT, "{busy} ns of processor time in {wall} ns");
        // asleep the thread uses next to nothing: its clock is not the wall clock
        let (a, t) = (cpu_mark().unwrap(), Instant::now());
        std::thread::sleep(Duration::from_millis(30));
        let wall = t.elapsed().as_nanos() as u64;
        let asleep = cpu_ns(a, cpu_mark().unwrap(), wall);
        assert!((asleep as f64) < 0.2 * wall as f64, "{asleep} of {wall}");
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

    #[test]
    fn published_parameters_read_back() {
        let p = Params::prior(Medium::Hdd);
        let pub_ = Published::new(Params::prior(Medium::Nvme));
        pub_.set(p);
        assert_eq!(pub_.get(), p);
    }
}
