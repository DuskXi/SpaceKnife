//! The hard-scan coordinator (scanner.py without the asyncio loop and the process pool).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use sk_native::hardscan::{
    self, LinkSet, Prior, Ref, RootOptions, SliceOptions, SliceResult, Task,
};
use sk_protocol::DateTime;
use sk_store::{Flags, Kind, NodeId, Store, Times};

use super::gate::{IoGate, Permit};
use super::{
    MAX_CALLERS, ScanState, calls_follow_the_walker, device_hint, fold_slice, native_threads,
    normalise_root, path_inside, verify_names,
};
use crate::volume::{self, VolumeInfo};

/// Denied folders recorded per scan (the count goes on).
pub const MAX_RECORDED_DENIED: usize = 200;
/// Sliding window of the item / byte rates, seconds.
pub const RATE_WINDOW_S: f64 = 4.0;
/// No remaining-time estimate before this much time.
pub const ESTIMATE_AFTER_S: f64 = 3.0;
/// Past this share of the used space a byte-based progress is a guess.
pub const BYTES_CEILING: f64 = 0.95;

/// A scan that cannot start.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("no scan roots given")]
    NoRoots,
    #[error("cannot access {path}: {message}")]
    Access { path: String, message: String },
    #[error("not a directory: {0}")]
    NotADirectory(String),
    #[error("{0}")]
    Store(#[from] sk_store::StoreError),
}

/// A scan root.
#[derive(Clone, Debug)]
pub struct RootInfo {
    pub path: PathBuf,
    pub node: NodeId,
    pub volume: Option<VolumeInfo>,
    pub whole_volume: bool,
}

impl RootInfo {
    pub fn to_protocol(&self) -> sk_protocol::scan::ScanRootInfo {
        sk_protocol::scan::ScanRootInfo {
            path: sk_store::names::printable(sk_store::names::encode_name(self.path.as_os_str()))
                .into_owned(),
            node: self.node,
            whole_volume: self.whole_volume,
            volume: self.volume.as_ref().map(VolumeInfo::to_protocol),
        }
    }
}

/// How a [`Scanner`] works.
#[derive(Clone)]
pub struct ScanOptions {
    /// Listing calls in flight; 0 = as many as the walker wants (its `calls`, up to
    /// [`MAX_CALLERS`]; [`native_threads`] off the shared carriers or with
    /// `SPACEKNIFE_SCAN_THREADS`).
    pub concurrency: usize,
    /// Descend into other filesystems mounted below a root (POSIX).
    pub cross_mounts: bool,
    /// Time a slice lists before it returns.
    pub budget: Duration,
    /// Folders per batch, at most.
    pub batch_tasks: usize,
    /// Every hard link counts (the 「每个链接都计」 setting).
    pub count_links_each: bool,
    /// Folders to leave out as not accessible (macOS privacy-protected places; the caller passes
    /// `privacy::protected_folders()` when the user chose so). Only those inside a root apply.
    pub protect: Vec<PathBuf>,
    /// The disk-load limit (io governor), if any.
    pub gate: Option<Arc<dyn IoGate>>,
    /// Test seam: called with `"list"` on a listing thread before each batch is listed and with
    /// `"fold"` on the scan's thread before each fold into the tree; a panic there stands for a
    /// bug in that step.
    #[doc(hidden)]
    pub fault: Option<ScanFault>,
}

/// [`ScanOptions::fault`].
#[doc(hidden)]
pub type ScanFault = Arc<dyn Fn(&str) + Send + Sync>;

/// One batch, listed on a listing thread. A panic while listing (a bug) is the batch's error:
/// without an answer the scan would wait for the batch forever, «scanning» for good.
fn list_batch(
    tasks: Vec<Task>,
    opts: &SliceOptions,
    fault: Option<&ScanFault>,
) -> Result<SliceResult, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(f) = fault {
            f("list");
        }
        hardscan::scan_slice(tasks, opts)
    }))
    .map_err(|p| format!("listing failed: {}", panic_message(p.as_ref())))?
    .map_err(|e| e.to_string())
}

/// The message of a panic's payload.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned())
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            concurrency: 0,
            cross_mounts: false,
            budget: super::NATIVE_BUDGET,
            batch_tasks: super::NATIVE_BATCH,
            count_links_each: false,
            protect: Vec::new(),
            gate: None,
            fault: None,
        }
    }
}

impl std::fmt::Debug for ScanOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanOptions")
            .field("concurrency", &self.concurrency)
            .field("cross_mounts", &self.cross_mounts)
            .field("budget", &self.budget)
            .field("batch_tasks", &self.batch_tasks)
            .field("count_links_each", &self.count_links_each)
            .field("gate", &self.gate.is_some())
            .finish()
    }
}

/// A scan's state and numbers (scanner.py's `ScanProgress`; the server turns it into
/// `sk_protocol::scan::ScanStatus`, see [`ScanProgress::apply_to`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ScanProgress {
    pub state: ScanState,
    pub items: i64,
    pub files: i64,
    pub dirs: i64,
    pub bytes_alloc: i64,
    pub bytes_size: i64,
    pub errors: i64,
    pub pending_dirs: i64,
    pub current_path: String,
    pub elapsed_s: f64,
    /// items per second over the last few seconds
    pub rate: f64,
    pub progress: Option<f64>,
    pub started_at: Option<DateTime>,
    pub finished_at: Option<DateTime>,
    pub denied: Vec<String>,
    pub message: String,
    /// allocated bytes per second (same window as `rate`)
    pub byte_rate: f64,
    pub remaining_s: Option<f64>,
    pub expected_items: Option<i64>,
    pub paused: bool,
}

impl ScanProgress {
    /// Fills the scanner's fields of a protocol status (the server sets the rest: ids, roots,
    /// snapshot, method, phase, io, monitor, free / unknown via [`Scanner::space_summary`]).
    pub fn apply_to(&self, s: &mut sk_protocol::scan::ScanStatus) {
        s.state = self.state;
        s.items = self.items;
        s.files = self.files;
        s.dirs = self.dirs;
        s.bytes_alloc = self.bytes_alloc;
        s.bytes_size = self.bytes_size;
        s.errors = self.errors;
        s.pending_dirs = self.pending_dirs;
        s.current_path = self.current_path.clone();
        s.elapsed_s = self.elapsed_s;
        s.rate = self.rate;
        s.byte_rate = self.byte_rate;
        s.progress = self.progress;
        s.remaining_s = self.remaining_s;
        s.expected_items = self.expected_items;
        s.started_at = self.started_at;
        s.finished_at = self.finished_at;
        s.denied = self.denied.clone();
        s.message = self.message.clone();
        s.paused = self.paused;
    }
}

/// Where a scan's time went (logged when it ends).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScanProfile {
    /// scanning, start to end, without the time it was paused
    pub wall_s: f64,
    /// listing calls in flight at most
    pub threads: usize,
    /// listing calls allowed in flight, time-mean (the walker's `calls`, under the disk-load
    /// limit's concurrency); `threads` when fixed
    pub calls_mean: f64,
    /// the disk-load limit's concurrency, time-mean (None: no limit)
    pub gate_mean: Option<f64>,
    /// summed time the slices listed
    pub busy_s: f64,
    /// coordinator: folding the slices into the tree
    pub ingest_s: f64,
    /// coordinator: waiting for the disk-load limit with nothing in flight
    pub limited_s: f64,
    pub slices: u64,
    /// entries this run added to the tree
    pub items: u64,
    /// The walker's shared listing threads: the narrowest and widest width (both lanes) it
    /// listed at during the run (process-wide: scans at the same time share it); (0, 0) without
    /// them.
    pub width: (usize, usize),
    /// Its lanes (docs/native.md 2.2 「列目录线程」): each one's narrowest and widest width and
    /// time-mean width, the share of the folders listed in the IO lane, the cold fraction over
    /// time (seconds, mean posterior), the device hint it followed.
    pub lanes: Option<LaneProfile>,
}

/// The walker's lanes during a scan ([`ScanProfile::lanes`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaneProfile {
    pub io: (usize, usize),
    pub cpu: (usize, usize),
    pub io_mean: f64,
    pub cpu_mean: f64,
    pub io_share: f64,
    pub cold: Vec<(f64, f64)>,
    pub medium: String,
    pub calls: (usize, usize),
}

impl LaneProfile {
    fn of(t: &hardscan::ListingThreads, r: &hardscan::PaceReport) -> LaneProfile {
        let (mut dt, mut io, mut cpu, mut n, mut n_io) = (0.0, 0.0, 0.0, 0u64, 0u64);
        for p in &r.timeline {
            dt += p.dt;
            io += p.io_width * p.dt;
            cpu += p.cpu_width * p.dt;
            n += p.listings;
            n_io += p.io_listings;
        }
        LaneProfile {
            io: (t.io.low, t.io.high),
            cpu: (t.cpu.low, t.cpu.high),
            io_mean: if dt > 0.0 { io / dt } else { t.io.width as f64 },
            cpu_mean: if dt > 0.0 {
                cpu / dt
            } else {
                t.cpu.width as f64
            },
            io_share: if n > 0 { n_io as f64 / n as f64 } else { 0.0 },
            cold: r
                .timeline
                .iter()
                .filter(|p| p.listings > 0)
                .map(|p| (p.t, p.cold))
                .collect(),
            medium: format!("{:?}", r.medium).to_lowercase(),
            calls: (t.calls_low, t.calls_high),
        }
    }

    /// The cold fraction at most `n` points, evenly spaced over the scan: "0.98 0.95 … 0.10 (per
    /// 2.0 s)".
    pub fn cold_curve(&self, n: usize) -> (String, f64) {
        let pts = &self.cold;
        if pts.is_empty() {
            return (String::new(), 0.0);
        }
        let step = pts.len().div_ceil(n.max(1));
        let per = if pts.len() > 1 {
            (pts[pts.len() - 1].0 - pts[0].0) / (pts.len() - 1) as f64 * step as f64
        } else {
            1.0
        };
        let curve = pts
            .chunks(step)
            .map(|c| format!("{:.2}", c.iter().map(|p| p.1).sum::<f64>() / c.len() as f64))
            .collect::<Vec<_>>()
            .join(" ");
        (curve, per.max(1.0))
    }
}

impl ScanProfile {
    /// The calls' busy share: listing time over the time-mean calls allowed.
    pub fn busy(&self) -> f64 {
        let calls = if self.calls_mean > 0.0 {
            self.calls_mean
        } else {
            self.threads.max(1) as f64
        };
        self.busy_s / (self.wall_s.max(1e-9) * calls)
    }
    pub fn ingest(&self) -> f64 {
        self.ingest_s / self.wall_s.max(1e-9)
    }
    pub fn limited(&self) -> f64 {
        self.limited_s / self.wall_s.max(1e-9)
    }
    pub fn text(&self) -> String {
        let bulk = hardscan::bulk_listing();
        let range = |(low, high): (usize, usize)| {
            if low == high {
                low.to_string()
            } else {
                format!("{low}–{high}")
            }
        };
        let mut s = format!(
            "{} items in {:.1}s ({:.0}/s); listing calls busy {:.0}% of {:.1} on average ({} at most)",
            self.items,
            self.wall_s,
            self.items as f64 / self.wall_s.max(1e-9),
            self.busy() * 100.0,
            if self.calls_mean > 0.0 {
                self.calls_mean
            } else {
                self.threads as f64
            },
            self.threads,
        );
        if self.width.1 > 0 {
            s.push_str(&format!(" (listing width {})", range(self.width)));
        }
        if let Some(l) = &self.lanes {
            let (curve, per) = l.cold_curve(12);
            s.push_str(&format!(
                "; io lane {} (mean {:.1}), cpu lane {} (mean {:.1}), {:.0}% of folders in the io lane; cold fraction {curve} (per {per:.1}s); device {}",
                range(l.io),
                l.io_mean,
                range(l.cpu),
                l.cpu_mean,
                l.io_share * 100.0,
                l.medium,
            ));
        }
        if let Some(g) = self.gate_mean {
            s.push_str(&format!(
                "; disk-load limit's concurrency {g:.1} on average"
            ));
        }
        s.push_str(&format!(
            "; coordinator folding {:.0}%; held by the disk-load limit {:.0}%; {} slices",
            self.ingest() * 100.0,
            self.limited() * 100.0,
            self.slices,
        ));
        if cfg!(target_os = "macos") {
            s.push_str(&format!("; getattrlistbulk {bulk}"));
        }
        s
    }
}

struct Shared {
    state: ScanState,
    queue: VecDeque<Task>,
    errors: i64,
    denied: Vec<String>,
    denied_reasons: Vec<String>,
    current: String,
    /// This run's start; None before the first.
    started: Option<Instant>,
    /// This run's end; None while it runs.
    finished: Option<Instant>,
    /// Earlier runs of this scan (stopped, then continued): `elapsed_s` adds them, not the waits.
    before: Duration,
    /// The hard runs' time while an MFT read stands in for them.
    held: Duration,
    /// The scan's first start, kept by later runs.
    started_at: Option<DateTime>,
    finished_at: Option<DateTime>,
    message: String,
    expected_items: Option<i64>,
    samples: VecDeque<(Instant, i64, i64)>,
    busy: Duration,
    ingest: Duration,
    limited: Duration,
    paused_total: Duration,
    paused_since: Option<Instant>,
    folded: u64,
    slices: u64,
    inflight: usize,
    /// ScanProfile::width
    width: (usize, usize),
    /// Calls allowed × seconds, the disk-load limit's concurrency × seconds, the most calls in
    /// flight (ScanProfile::calls_mean, gate_mean, threads).
    calls_area: f64,
    gate_area: f64,
    calls_peak: usize,
    lanes: Option<LaneProfile>,
}

impl Shared {
    /// This run so far, the time it was paused included (`elapsed_s`; the profile leaves it out).
    fn run_time_at(st: &Shared, now: Instant) -> Duration {
        st.started.map_or(Duration::ZERO, |s| {
            st.finished.unwrap_or(now).saturating_duration_since(s)
        })
    }

    fn run_time(st: &Shared) -> Duration {
        Shared::run_time_at(st, Instant::now())
    }

    /// A run starts at `now`: the scan keeps its first start (`started_at`), an earlier run (a
    /// stopped scan going on) counts in `before`.
    fn next_run(st: &mut Shared, now: Instant) {
        st.before += Shared::run_time_at(st, now);
        st.started = Some(now);
        st.finished = None;
        st.finished_at = None;
        if st.started_at.is_none() {
            st.started_at = Some(chrono::Utc::now());
        }
    }
}

enum Msg {
    /// a listed batch, or why it failed (an error of the listing, or the panic that stopped it)
    Done(Box<Result<SliceResult, String>>, Option<Permit>),
    Wake,
}

struct Job {
    tasks: Vec<Task>,
    opts: Arc<SliceOptions>,
    permit: Option<Permit>,
}

/// A hard scan of one or more roots into a [`Store`].
///
/// Shared by threads (`Arc<Scanner>`): one runs [`Scanner::run`]; others read
/// [`Scanner::progress`], [`Scanner::store`] (views) and call [`Scanner::pause`],
/// [`Scanner::resume`], [`Scanner::cancel`] at any time.
pub struct Scanner {
    store: Store,
    roots: RwLock<Vec<RootInfo>>,
    root_opts: Arc<Vec<RootOptions>>,
    opts: ScanOptions,
    /// Calls in flight at most; `dynamic`: as many as the walker wants up to that.
    concurrency: usize,
    dynamic: bool,
    cancel: Arc<AtomicBool>,
    paused: AtomicBool,
    st: Mutex<Shared>,
    links: Mutex<LinkSet>,
    wake: Mutex<Option<mpsc::Sender<Msg>>>,
}

impl std::fmt::Debug for Scanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scanner")
            .field("roots", &self.roots())
            .finish()
    }
}

fn secs(t: std::io::Result<std::time::SystemTime>) -> u32 {
    t.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs().min(u32::MAX as u64) as u32)
}

/// A root's creation time by the rule of its entries' (the native listing): Windows' creation
/// time, `st_birthtime` on macOS and the BSDs; none on Linux, whose stat has no birth time (std
/// reads statx's), as in 0.3.4.
fn created(meta: &std::fs::Metadata) -> u32 {
    if cfg!(any(
        windows,
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    )) {
        secs(meta.created())
    } else {
        0
    }
}

#[cfg(unix)]
fn dev_of(m: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.dev()
}
#[cfg(not(unix))]
fn dev_of(_: &std::fs::Metadata) -> u64 {
    0
}

impl Scanner {
    /// Sets up a scan of `roots` (nested roots are dropped: their ancestor covers them).
    pub fn new<S: AsRef<str>>(roots: &[S], opts: ScanOptions) -> Result<Scanner, ScanError> {
        if roots.is_empty() {
            return Err(ScanError::NoRoots);
        }
        let mut normalised: Vec<PathBuf> = Vec::new();
        for raw in roots {
            let path = normalise_root(raw.as_ref());
            if normalised.iter().any(|r| path_inside(&path, r)) {
                continue; // nested inside another root: already covered
            }
            normalised.push(path);
        }
        let store = Store::new();
        let mut infos = Vec::new();
        let mut root_opts = Vec::new();
        let mut queue = VecDeque::new();
        {
            let mut app = store.appender();
            for path in normalised {
                let shown = path.to_string_lossy().into_owned();
                let meta = std::fs::metadata(&path).map_err(|e| ScanError::Access {
                    path: shown.clone(),
                    message: volume::os_message(&e),
                })?;
                if !meta.is_dir() {
                    return Err(ScanError::NotADirectory(shown));
                }
                let vol = volume::volume_for(&path).ok();
                let times = Times {
                    mtime: secs(meta.modified()),
                    ctime: created(&meta),
                    atime: secs(meta.accessed()),
                };
                let node = app.add_root(
                    path.as_os_str().as_encoded_bytes(),
                    Kind::Dir,
                    Flags::PENDING,
                    times,
                )?;
                let whole = vol
                    .as_ref()
                    .is_some_and(|v| volume::is_volume_root(&path, v));
                let ntfs = cfg!(windows)
                    && vol
                        .as_ref()
                        .is_some_and(|v| v.fs_type.eq_ignore_ascii_case("NTFS"));
                let skip = if opts.cross_mounts {
                    Vec::new()
                } else {
                    volume::volume_group_twins(&shown)
                        .into_iter()
                        .map(PathBuf::from)
                        .collect()
                };
                root_opts.push(RootOptions {
                    dev: dev_of(&meta),
                    cluster_size: vol.as_ref().map_or(4096, |v| v.cluster_size),
                    resident_max: if ntfs { 512 } else { 0 },
                    cross_mounts: opts.cross_mounts,
                    skip,
                    protect: opts
                        .protect
                        .iter()
                        .filter(|p| path_inside(p, &path))
                        .cloned()
                        .collect(),
                    path: Some(path.clone()),
                    verify_names: verify_names(vol.as_ref()),
                    listed_alloc: super::winopts::listed_alloc(vol.as_ref()),
                    device: device_hint(vol.as_ref()),
                });
                queue.push_back(Task {
                    target: Ref::Node(node as u64),
                    path: path.clone(),
                    root: (root_opts.len() - 1) as u32,
                    prior: Prior::UNKNOWN,
                });
                infos.push(RootInfo {
                    path,
                    node,
                    volume: vol,
                    whole_volume: whole,
                });
            }
        }
        let dynamic = opts.concurrency == 0 && calls_follow_the_walker();
        let concurrency = match opts.concurrency {
            0 if dynamic => MAX_CALLERS,
            0 => native_threads(),
            n => n,
        };
        Ok(Scanner {
            store,
            roots: RwLock::new(infos),
            root_opts: Arc::new(root_opts),
            concurrency,
            dynamic,
            opts,
            cancel: Arc::new(AtomicBool::new(false)),
            paused: AtomicBool::new(false),
            st: Mutex::new(Shared {
                state: ScanState::Queued,
                queue,
                errors: 0,
                denied: Vec::new(),
                denied_reasons: Vec::new(),
                current: String::new(),
                started: None,
                finished: None,
                before: Duration::ZERO,
                held: Duration::ZERO,
                started_at: None,
                finished_at: None,
                message: String::new(),
                expected_items: None,
                samples: VecDeque::new(),
                busy: Duration::ZERO,
                ingest: Duration::ZERO,
                limited: Duration::ZERO,
                paused_total: Duration::ZERO,
                paused_since: None,
                folded: 0,
                slices: 0,
                inflight: 0,
                width: (0, 0),
                calls_area: 0.0,
                gate_area: 0.0,
                calls_peak: 0,
                lanes: None,
            }),
            links: Mutex::new(LinkSet::new()),
            wake: Mutex::new(None),
        })
    }

    fn st(&self) -> MutexGuard<'_, Shared> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The tree being built (cheap handle; views read it while the scan runs).
    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn roots(&self) -> Vec<RootInfo> {
        self.roots.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Changes a root's record: its volume read again (free space moves with changes).
    pub fn update_root(&self, root: usize, f: impl FnOnce(&mut RootInfo)) {
        if let Some(r) = self
            .roots
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(root)
        {
            f(r);
        }
    }

    /// Listing options per scan root (the monitor lists folders the same way).
    pub fn root_options(&self) -> Arc<Vec<RootOptions>> {
        self.root_opts.clone()
    }

    /// Listing calls in flight at most (following the walker: up to [`MAX_CALLERS`]).
    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// Whether the calls in flight follow the walker's `calls`.
    pub fn calls_follow_the_walker(&self) -> bool {
        self.dynamic
    }

    /// Calls allowed in flight now.
    fn allowed(&self) -> usize {
        if self.dynamic {
            hardscan::wanted_calls().clamp(1, self.concurrency)
        } else {
            self.concurrency
        }
    }

    pub fn state(&self) -> ScanState {
        self.st().state
    }

    /// Folders are left to read (a stopped scan can go on with them).
    pub fn has_queue(&self) -> bool {
        !self.st().queue.is_empty()
    }

    /// Items of the last scan of the same roots: progress by items, and the store reserves
    /// room for them up front (no column copies while growing).
    pub fn set_expected_items(&self, items: Option<i64>) {
        self.st().expected_items = items;
        if let Some(n) = items.filter(|&n| n > 0) {
            self.store
                .appender()
                .reserve((n as usize).saturating_mul(21) / 20 + 1024);
        }
    }

    /// OS message per recorded denied path (log panel).
    pub fn denied_reasons(&self) -> Vec<String> {
        self.st().denied_reasons.clone()
    }

    // ------------------------------------------------------------------ control

    fn wake(&self) {
        if let Some(tx) = self.wake.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.send(Msg::Wake);
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        {
            let mut st = self.st();
            Self::unpaused(&mut st);
        }
        self.paused.store(false, Ordering::SeqCst);
        self.wake();
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Forget a cancel that only stopped the hard scan to switch to another method.
    pub fn uncancel(&self) {
        self.cancel.store(false, Ordering::SeqCst);
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Stop dispatching batches; those in flight finish within their time budget.
    pub fn pause(&self) {
        let mut st = self.st();
        if !self.paused() && matches!(st.state, ScanState::Queued | ScanState::Scanning) {
            self.paused.store(true, Ordering::SeqCst);
            st.paused_since = Some(Instant::now());
            drop(st);
            if let Some(g) = &self.opts.gate {
                g.pause();
            }
        }
    }

    fn unpaused(st: &mut Shared) {
        if let Some(since) = st.paused_since.take() {
            st.paused_total += since.elapsed();
        }
    }

    pub fn resume(&self) {
        if self.paused() {
            Self::unpaused(&mut self.st());
            self.paused.store(false, Ordering::SeqCst);
            if let Some(g) = &self.opts.gate {
                g.resume();
            }
            self.wake();
        }
    }

    // the tree comes from elsewhere (an MFT scan adopted into the store): timing and state only

    /// The read starts; its tree replaces whatever a hard part listed, and so does its time (the
    /// next "about N s" estimate comes from it). The hard part's time is held for
    /// [`Scanner::reset_external`].
    pub fn begin_external(&self) {
        let mut st = self.st();
        st.state = ScanState::Scanning;
        st.held = st.before + Shared::run_time(&st);
        st.before = Duration::ZERO;
        st.started = None;
        st.paused_total = Duration::ZERO;
        Shared::next_run(&mut st, Instant::now());
    }

    /// The scan ended with a failure that [`Scanner::run`] could not record (a panic that
    /// unwound out of it): failed, `message` saying why, nothing in flight, not paused.
    pub fn fail(&self, message: &str) {
        if self.paused() {
            self.paused.store(false, Ordering::SeqCst);
            if let Some(g) = &self.opts.gate {
                g.resume();
            }
        }
        let mut st = self.st();
        Self::unpaused(&mut st);
        st.state = ScanState::Failed;
        st.message = message.to_owned();
        st.finished = Some(Instant::now());
        st.finished_at = Some(chrono::Utc::now());
        st.inflight = 0;
    }

    pub fn finish_external(&self, state: ScanState) {
        let mut st = self.st();
        st.state = state;
        st.finished = Some(Instant::now());
        st.finished_at = Some(chrono::Utc::now());
        st.queue.clear();
        st.current.clear();
    }

    /// The external read failed: [`Scanner::run`] scans as usual, going on with the hard part's
    /// time (not the attempt's: the tree keeps nothing of it).
    pub fn reset_external(&self) {
        let mut st = self.st();
        st.state = ScanState::Queued;
        st.before = std::mem::take(&mut st.held);
        st.started = None;
        st.finished = None;
        st.finished_at = None;
    }

    // ------------------------------------------------------------------ running

    /// Scans until the queue is empty or the scan is cancelled; returns the final progress.
    /// `on_progress` is called on this thread every `interval` and once at the end.
    pub fn run(
        &self,
        mut on_progress: Option<&mut dyn FnMut(&ScanProgress)>,
        interval: Duration,
    ) -> ScanProgress {
        {
            let mut st = self.st();
            st.state = ScanState::Scanning;
            let now = Instant::now();
            Shared::next_run(&mut st, now);
            st.samples.clear(); // the rates come from this run's listing, not across a stop
            st.busy = Duration::ZERO;
            st.ingest = Duration::ZERO;
            st.limited = Duration::ZERO;
            st.paused_total = Duration::ZERO;
            st.slices = 0;
            st.folded = 0;
            st.calls_area = 0.0;
            st.gate_area = 0.0;
            st.calls_peak = 0;
            st.lanes = None;
            if st.paused_since.is_some() {
                st.paused_since = Some(now); // paused while queued
            }
        }
        let (tx, rx) = mpsc::channel::<Msg>();
        *self.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx.clone());
        // the walker's shared carriers (threads 0): it decides how many folders are listed at
        // once; the disk's gate holds them to the governor's concurrency (counted in listings) and
        // stops new ones while paused. Else a pool owned by each calling thread.
        let shared = super::shared_listing();
        let listing_gate = if shared {
            self.opts.gate.as_ref().and_then(|g| g.listing_gate())
        } else {
            None
        };
        if shared {
            hardscan::listing_threads(true);
            hardscan::pace_report(true);
        }
        let base = |budget: Duration| {
            let mut o = SliceOptions {
                roots: self.root_opts.as_ref().clone(),
                budget,
                threads: if shared { 0 } else { 1 },
                gate: listing_gate.clone(),
                cancel: Some(self.cancel.clone()),
                ..SliceOptions::default()
            };
            super::winopts::apply_env(&mut o); // skip_final, dot_check, keep_where_updated
            Arc::new(o)
        };
        let full = base(self.opts.budget);
        let short = base(self.opts.budget.min(Duration::from_millis(10)));
        let failure: Option<String> = std::thread::scope(|scope| {
            let (job_tx, job_rx) = mpsc::channel::<Job>();
            let job_rx = Arc::new(Mutex::new(job_rx));
            // the calling threads start as they are needed (a call is dispatched with every one
            // busy), up to `concurrency`; idle ones wait on the job channel until the scan ends
            let start = |i: usize| {
                let rx = job_rx.clone();
                let tx = tx.clone();
                std::thread::Builder::new()
                    .name(format!("sk-scan-{i}"))
                    .spawn_scoped(scope, move || {
                        loop {
                            let job = match rx.lock() {
                                Ok(r) => r.recv(),
                                Err(_) => return,
                            };
                            let Ok(job) = job else { return };
                            let res = list_batch(job.tasks, &job.opts, self.opts.fault.as_ref());
                            if tx.send(Msg::Done(Box::new(res), job.permit)).is_err() {
                                return;
                            }
                        }
                    })
                    .is_ok()
            };
            let fail = self.coordinate(
                &rx,
                &job_tx,
                &start,
                &full,
                &short,
                &mut on_progress,
                interval,
            );
            drop(job_tx); // workers end
            fail
        });
        *self.wake.lock().unwrap_or_else(|e| e.into_inner()) = None;
        {
            let mut st = self.st();
            st.state = if let Some(msg) = failure {
                tracing::error!("scan failed: {msg}");
                st.message = msg;
                ScanState::Failed
            } else if self.cancelled() {
                ScanState::Cancelled
            } else {
                ScanState::Done
            };
            if self.paused() {
                self.paused.store(false, Ordering::SeqCst);
                if let Some(g) = &self.opts.gate {
                    g.resume();
                }
            }
            Self::unpaused(&mut st);
            st.finished = Some(Instant::now());
            st.finished_at = Some(chrono::Utc::now());
            st.inflight = 0;
            if shared {
                let w = hardscan::listing_threads(false);
                st.width = (w.low, w.high);
                st.lanes = Some(LaneProfile::of(&w, &hardscan::pace_report(false)));
            }
        }
        let result = self.progress();
        if let Some(cb) = on_progress.as_mut() {
            cb(&result);
        }
        tracing::info!(
            "scan {}: {} items, {} files, {:.1} GiB in {:.2}s ({:.0} items/s), {} errors",
            result.state.as_str(),
            result.items,
            result.files,
            result.bytes_alloc as f64 / (1u64 << 30) as f64,
            result.elapsed_s,
            result.rate,
            result.errors
        );
        tracing::info!("scan profile: {}", self.profile().text());
        result
    }

    /// The dispatch / fold loop (`start(i)` starts calling thread `i`); Some(message) when the
    /// scan failed.
    #[allow(clippy::too_many_arguments)]
    fn coordinate(
        &self,
        rx: &mpsc::Receiver<Msg>,
        jobs: &mpsc::Sender<Job>,
        start: &dyn Fn(usize) -> bool,
        full: &Arc<SliceOptions>,
        short: &Arc<SliceOptions>,
        on_progress: &mut Option<&mut dyn FnMut(&ScanProgress)>,
        interval: Duration,
    ) -> Option<String> {
        let gate = self.opts.gate.clone();
        let mut inflight = 0usize;
        let mut started = 0usize;
        let mut last_report = Instant::now();
        let mut fail: Option<String> = None;
        let tick = interval.clamp(Duration::from_millis(10), Duration::from_millis(250));
        let mut last_tick = Instant::now();
        loop {
            if self.cancelled() || fail.is_some() {
                break;
            }
            if inflight == 0 && self.st().queue.is_empty() {
                break;
            }
            // the calls allowed and the disk-load limit's concurrency, time-weighted (profile)
            let allowed = self.allowed();
            let limit = gate.as_ref().map(|g| g.concurrency());
            {
                let now = Instant::now();
                let dt = now.duration_since(last_tick).as_secs_f64();
                last_tick = now;
                let mut st = self.st();
                if !self.paused() {
                    st.calls_area += dt * limit.map_or(allowed, |l| l.clamp(1, allowed)) as f64;
                    st.gate_area += dt * limit.unwrap_or(0) as f64;
                }
            }
            // dispatch
            if !self.paused() {
                loop {
                    let allowed = self.allowed();
                    let parallel = gate
                        .as_ref()
                        .map_or(allowed, |g| g.concurrency().clamp(1, allowed));
                    if inflight >= parallel || self.st().queue.is_empty() {
                        break;
                    }
                    let permit = match &gate {
                        None => None,
                        Some(g) => {
                            let asked = Instant::now();
                            let wait = if inflight == 0 { tick } else { Duration::ZERO };
                            let p = g.permit(wait);
                            if inflight == 0 {
                                self.st().limited += asked.elapsed();
                            }
                            match p {
                                Some(p) => Some(p),
                                None => break,
                            }
                        }
                    };
                    if self.cancelled() || self.paused() {
                        break; // the permit drops: given back
                    }
                    let (tasks, opts) = {
                        let mut st = self.st();
                        let n = st.queue.len();
                        let share = (n / parallel.max(1)).clamp(1, self.opts.batch_tasks.max(1));
                        let tasks: Vec<Task> = st.queue.drain(..share.min(n)).collect();
                        let opts = if st.queue.len() < parallel {
                            short.clone()
                        } else {
                            full.clone()
                        };
                        st.inflight += 1;
                        (tasks, opts)
                    };
                    if tasks.is_empty() {
                        self.st().inflight -= 1;
                        break;
                    }
                    if inflight >= started && started < self.concurrency.max(1) {
                        if start(started) {
                            started += 1;
                        } else if started == 0 {
                            return Some("cannot start a listing thread".into());
                        }
                    }
                    inflight += 1;
                    {
                        let mut st = self.st();
                        st.calls_peak = st.calls_peak.max(inflight);
                    }
                    if jobs
                        .send(Job {
                            tasks,
                            opts,
                            permit,
                        })
                        .is_err()
                    {
                        return Some("the listing threads stopped".into());
                    }
                }
            }
            // collect what landed
            let wait = if inflight == 0 && (self.paused() || gate.is_none()) {
                tick
            } else if inflight == 0 {
                Duration::from_millis(1) // the gate held the batch (it waited a tick already)
            } else {
                interval
                    .saturating_sub(last_report.elapsed())
                    .max(Duration::from_millis(1))
            };
            let mut landed: Vec<SliceResult> = Vec::new();
            let take = |msg: Msg,
                        inflight: &mut usize,
                        landed: &mut Vec<SliceResult>,
                        fail: &mut Option<String>| {
                if let Msg::Done(res, permit) = msg {
                    drop(permit);
                    *inflight -= 1;
                    self.st().inflight -= 1;
                    match *res {
                        Ok(r) => landed.push(r),
                        Err(e) => *fail = Some(e),
                    }
                }
            };
            match rx.recv_timeout(wait) {
                Ok(msg) => {
                    take(msg, &mut inflight, &mut landed, &mut fail);
                    while let Ok(msg) = rx.try_recv() {
                        take(msg, &mut inflight, &mut landed, &mut fail);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Some("scan channel closed".into());
                }
            }
            if !landed.is_empty()
                && let Err(e) = self.apply(landed)
            {
                fail = Some(e);
            }
            if let Some(cb) = on_progress.as_mut()
                && last_report.elapsed() >= interval
            {
                last_report = Instant::now();
                cb(&self.progress());
            }
        }
        // cancelled (or failed): let the slices in flight land so the partial tree stays consistent
        while inflight > 0 {
            match rx.recv() {
                Ok(Msg::Done(res, permit)) => {
                    drop(permit);
                    inflight -= 1;
                    self.st().inflight -= 1;
                    if let Ok(r) = *res
                        && fail.is_none()
                        && let Err(e) = self.apply(vec![r])
                    {
                        fail = Some(e);
                    }
                }
                Ok(Msg::Wake) => {}
                Err(_) => break,
            }
        }
        fail
    }

    /// Folds results that came in together as one slice (fewer, larger calls into the store).
    fn apply(&self, mut results: Vec<SliceResult>) -> Result<(), String> {
        if let Some(f) = &self.opts.fault {
            f("fold");
        }
        let t0 = Instant::now();
        let n = results.len() as u64;
        let busy: Duration = results.iter().map(|r| r.elapsed).sum();
        let mut res = results.swap_remove(0);
        for r in results {
            res.append(r);
        }
        let folded = {
            let mut app = self.store.appender();
            let mut links = self.links.lock().unwrap_or_else(|e| e.into_inner());
            let links = (!self.opts.count_links_each).then_some(&mut *links);
            fold_slice(&mut app, res, links).map_err(|e| e.to_string())?
        };
        if folded.skipped > 0 {
            tracing::debug!(
                "scan: {} listing(s) of folders deleted while they were read, skipped",
                folded.skipped
            );
        }
        let mut st = self.st();
        st.queue.extend(folded.frontier);
        for (_, path, msg) in folded.denied {
            st.errors += 1;
            if st.denied.len() < MAX_RECORDED_DENIED {
                st.denied.push(
                    sk_store::names::printable(path.as_os_str().as_encoded_bytes()).into_owned(),
                );
                st.denied_reasons.push(msg);
            }
        }
        if let Some(p) = folded.last_path {
            st.current = sk_store::names::printable(p.as_os_str().as_encoded_bytes()).into_owned();
        }
        st.folded += folded.entries as u64;
        st.ingest += t0.elapsed();
        st.busy += busy;
        st.slices += n;
        Ok(())
    }

    // ------------------------------------------------------------------ reporting

    pub fn profile(&self) -> ScanProfile {
        let st = self.st();
        let end = st.finished.unwrap_or_else(Instant::now);
        let paused = st.paused_total
            + st.paused_since
                .map_or(Duration::ZERO, |s| end.saturating_duration_since(s));
        let wall_s = st.started.map_or(0.0, |s| {
            end.saturating_duration_since(s)
                .saturating_sub(paused)
                .as_secs_f64()
        });
        ScanProfile {
            wall_s,
            threads: if self.dynamic {
                st.calls_peak
            } else {
                self.concurrency
            },
            calls_mean: if wall_s > 0.0 && st.calls_area > 0.0 {
                st.calls_area / wall_s
            } else {
                0.0
            },
            gate_mean: (self.opts.gate.is_some() && wall_s > 0.0).then(|| st.gate_area / wall_s),
            busy_s: st.busy.as_secs_f64(),
            ingest_s: st.ingest.as_secs_f64(),
            limited_s: st.limited.as_secs_f64(),
            slices: st.slices,
            items: st.folded,
            width: st.width,
            lanes: st.lanes.clone(),
        }
    }

    pub fn progress(&self) -> ScanProgress {
        let (items, files, dirs, alloc, size) = {
            let r = self.store.read();
            (
                r.len() as i64 - 1,
                r.tot_files(0) as i64,
                r.tot_dirs(0) as i64,
                r.tot_alloc(0),
                r.tot_size(0),
            )
        };
        let roots = self.roots();
        let mut st = self.st();
        let now = Instant::now();
        // every run of the scan, without the waits between
        let elapsed = (st.before + Shared::run_time_at(&st, now)).as_secs_f64();
        let (rate, byte_rate) = Self::rates(&mut st, items, alloc, elapsed, now);
        let running = st.state == ScanState::Scanning;
        let used: i64 =
            if !roots.is_empty() && roots.iter().all(|r| r.whole_volume && r.volume.is_some()) {
                roots
                    .iter()
                    .filter_map(|r| r.volume.as_ref())
                    .map(VolumeInfo::used)
                    .sum()
            } else {
                0
            };
        // By items when the last scan of these roots says how many there are; else by bytes against
        // the volume's used space, up to BYTES_CEILING (used space is no exact total).
        let expected = st.expected_items.unwrap_or(0);
        let by_items = (expected > 0).then(|| items as f64 / expected as f64);
        let by_bytes = (used > 0).then(|| alloc as f64 / used as f64);
        let mut fraction = None;
        if st.state == ScanState::Done {
            fraction = (by_items.is_some() || by_bytes.is_some()).then_some(1.0);
        } else if let Some(b) = by_items.filter(|&b| b < 1.0) {
            fraction = Some(b.min(0.99));
        } else if let Some(b) = by_bytes.filter(|&b| b < BYTES_CEILING) {
            fraction = Some(b);
        }
        let mut remaining = None;
        if running && elapsed >= ESTIMATE_AFTER_S {
            if by_items.is_some_and(|b| b < 1.0) && rate > 0.0 {
                remaining = Some((expected - items) as f64 / rate);
            } else if by_bytes.is_some_and(|b| b < BYTES_CEILING) && byte_rate > 0.0 {
                remaining = Some((used - alloc) as f64 / byte_rate);
            }
        }
        ScanProgress {
            state: st.state,
            items,
            files,
            dirs,
            bytes_alloc: alloc,
            bytes_size: size,
            errors: st.errors,
            pending_dirs: st.queue.len() as i64,
            current_path: st.current.clone(),
            elapsed_s: elapsed,
            rate,
            progress: fraction,
            started_at: st.started_at,
            finished_at: st.finished_at,
            denied: st.denied.clone(),
            message: st.message.clone(),
            byte_rate,
            remaining_s: remaining,
            expected_items: st.expected_items,
            paused: self.paused(),
        }
    }

    /// Items and bytes per second over the last few seconds, not since the start.
    fn rates(st: &mut Shared, items: i64, alloc: i64, elapsed: f64, now: Instant) -> (f64, f64) {
        let overall = || {
            if elapsed > 0.0 {
                (items as f64 / elapsed, alloc as f64 / elapsed)
            } else {
                (0.0, 0.0)
            }
        };
        if st.state != ScanState::Scanning {
            return overall();
        }
        let samples = &mut st.samples;
        if samples
            .back()
            .is_none_or(|s| now.duration_since(s.0).as_secs_f64() >= 0.25)
        {
            samples.push_back((now, items, alloc));
        }
        while samples.len() > 2 && now.duration_since(samples[1].0).as_secs_f64() >= RATE_WINDOW_S {
            samples.pop_front();
        }
        let (t0, i0, b0) = samples[0];
        let span = now.duration_since(t0).as_secs_f64();
        if span < 1.0 {
            return overall();
        }
        ((items - i0) as f64 / span, (alloc - b0) as f64 / span)
    }

    /// (free, unknown) bytes for a whole-volume root, else None.
    pub fn space_summary(&self, root: &RootInfo) -> Option<(i64, i64)> {
        let vol = root.volume.as_ref().filter(|_| root.whole_volume)?;
        let scanned = self.store.read().tot_alloc(root.node);
        Some((vol.free, (vol.used() - scanned).max(0)))
    }
}

/// Scans `path` completely on the calling thread pool (tests, the CLI): a convenience over
/// [`Scanner::new`] + [`Scanner::run`].
pub fn scan_path(path: &Path, opts: ScanOptions) -> Result<(Scanner, ScanProgress), ScanError> {
    let s = Scanner::new(&[path.to_string_lossy()], opts)?;
    let p = s.run(None, Duration::from_millis(250));
    Ok((s, p))
}
