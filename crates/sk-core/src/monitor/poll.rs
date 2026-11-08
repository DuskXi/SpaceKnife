//! Generic polling watcher (docs/monitor.md §2.4) and the mtime reconciliation inotify reuses.
//!
//! A folder's mtime changes whenever an entry is added, removed or renamed in it, so re-stat'ing
//! the scanned folders finds every structural change, at most `poll_interval` seconds (plus the
//! round's own duration) late. File content changes stay invisible.
//!
//! [`DirPoller`] is the clock-driven core (no thread, no locks): its owner calls
//! [`DirPoller::step`] whenever [`DirPoller::wake_time`] has come. A round stats at most
//! `poll_rate` folders per second, in small batches spaced out in time. The baseline of a newly
//! added folder is taken right away (in chunks, not rate limited): the scan has just listed that
//! folder, so the stat is served from the inode cache. Still, the folder may have changed between
//! the scan's listing and that baseline: when the scan's record of its mtime comes along
//! (`add_dirs`), the baseline is checked against it ([`listed_changed`]) and a difference is
//! reported as `dir`, instead of being taken as the starting point.
//!
//! Folders whose path reaches PATH_MAX (POSIX) cannot be stat'ed by their path (ENAMETOOLONG):
//! they are opened a piece of the path at a time, as the scan lists them, and `fstat`ed.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::{Change, ChangeKind, LockExt, Sink, WatchState, WatchStatus, Watcher, t, unpoison};

const PENDING: i64 = i64::MIN; // baseline not taken yet
const UNKNOWN: i64 = i64::MIN + 1; // the stat failed for another reason than "gone": retried
const BASELINE_CHUNK: usize = 512;
const MAX_BATCH: usize = 256;

/// `(size, alloc, mtime)` of a non-folder entry as the POSIX scanner records them, or None when
/// it vanished or is a folder. Windows: None (its watcher reads the numbers itself).
pub fn stat_file(path: &Path) -> Option<(i64, i64, i64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::symlink_metadata(path).ok()?;
        if m.is_dir() {
            return None;
        }
        Some((
            m.size() as i64,
            m.blocks() as i64 * 512,
            super::secs(m.modified()),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Seconds as the scanners record them: the double `sec + nsec * 1e-9` truncated, clamped to u32.
fn whole_secs(sec: i64, nsec: i64) -> u32 {
    let t = sec as f64 + nsec as f64 * 1e-9;
    if t.is_nan() || t <= 0.0 {
        0
    } else if t >= u32::MAX as f64 {
        u32::MAX
    } else {
        t as u32
    }
}

// A file time is stamped from a clock that may lag the wall clock by a tick (and kept to 10 ms on
// exFAT); file systems that keep whole seconds (ext3, HFS+) or even two (FAT) truncate more.
const FINE_SLACK_NS: i64 = 50_000_000;
const COARSE_SLACK_NS: i64 = 2_100_000_000;

/// Whether a folder whose mtime is now `mtime_ns` may have changed after the scan listed it.
///
/// `listed`: its mtime as the scan recorded it, in whole seconds (the tree's `mtime` column);
/// `since_ns`: a wall-clock time no later than that listing (the scan's start). A folder's mtime
/// moves whenever an entry is added, removed or renamed in it. Whole seconds that differ tell.
/// Within the same second "before the listing" and "after it" look alike, so a folder modified at
/// or after `since_ns` is taken as changed (one more listing of it): less the slack its file
/// system's time stamps need, a whole second's worth when the mtime is a whole second.
pub fn listed_changed(mtime_ns: i64, listed: u32, since_ns: i64) -> bool {
    let (sec, nsec) = (
        mtime_ns.div_euclid(1_000_000_000),
        mtime_ns.rem_euclid(1_000_000_000),
    );
    if whole_secs(sec, nsec) != listed {
        return true;
    }
    mtime_ns
        >= since_ns.saturating_sub(if nsec != 0 {
            FINE_SLACK_NS
        } else {
            COARSE_SLACK_NS
        })
}

/// Folders handed to a watcher (`add_dirs`), batch by batch as they came: each batch is the
/// caller's list itself with the mtimes the scan recorded for it (4 B each), nothing per folder
/// on top. A batch leaves only once all of it was placed, so while the backlog is not empty some
/// folder handed in is neither watched nor polled yet.
#[derive(Default)]
pub struct Backlog {
    batches: VecDeque<(Vec<PathBuf>, Option<Vec<u32>>, i64)>,
    /// folders of the first batch already placed
    at: usize,
}

impl Backlog {
    pub fn new() -> Backlog {
        Backlog::default()
    }

    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// Queue a batch; `mtimes` of another length than `dirs` are dropped (nothing is checked).
    pub fn add(&mut self, dirs: Vec<PathBuf>, mtimes: Option<Vec<u32>>, since_ns: i64) {
        let mtimes = match mtimes {
            Some(m) if m.len() != dirs.len() => {
                tracing::warn!("add_dirs: {} folders, {} mtimes", dirs.len(), m.len());
                None
            }
            m => m,
        };
        if !dirs.is_empty() {
            self.batches.push_back((dirs, mtimes, since_ns));
        }
    }

    /// Up to `n` of the next folders, all of one batch: (path, recorded mtime, since).
    pub fn peek(&self, n: usize) -> Vec<(PathBuf, Option<u32>, i64)> {
        let Some((paths, mtimes, since)) = self.batches.front() else {
            return Vec::new();
        };
        let end = paths.len().min(self.at + n);
        (self.at..end)
            .map(|i| (paths[i].clone(), mtimes.as_ref().map(|m| m[i]), *since))
            .collect()
    }

    /// The `n` folders [`Backlog::peek`] gave were placed.
    pub fn advance(&mut self, n: usize) {
        self.at += n;
        if self.batches.front().is_some_and(|b| self.at >= b.0.len()) {
            self.batches.pop_front();
            self.at = 0;
        }
    }
}

/// `(is_dir, mtime ns)` of a folder whose path is PATH_MAX or longer, which stat refuses
/// (ENAMETOOLONG): opened a piece of whole names at a time, each piece relative to the folder the
/// one before reached, as the scan lists such folders (sk-native's `open_long`); the last name is
/// not followed (as `symlink_metadata`). Linux opens with `O_PATH` (search permission is enough).
#[cfg(unix)]
pub(crate) fn stat_past_path_max(path: &Path) -> std::io::Result<(bool, i64)> {
    use std::os::unix::ffi::OsStrExt;
    const PIECE: usize = 1000; // bytes per open: well under PATH_MAX everywhere (macOS: 1,024)
    struct Fd(libc::c_int);
    impl Drop for Fd {
        fn drop(&mut self) {
            // SAFETY: our own descriptor.
            unsafe { libc::close(self.0) };
        }
    }
    #[cfg(target_os = "linux")]
    let (middle, last_flags) = (
        libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    );
    #[cfg(not(target_os = "linux"))]
    let (middle, last_flags) = (
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    );
    let mut rest = path.as_os_str().as_bytes();
    let mut dir: Option<Fd> = None;
    let mut one = false; // a piece too long once its symlinks are followed (macOS): name by name
    loop {
        let (piece, tail) = if one {
            match rest
                .get(1..)
                .and_then(|r| r.iter().position(|&b| b == b'/'))
            {
                Some(j) => (&rest[..j + 1], &rest[j + 2..]),
                None => (rest, &rest[rest.len()..]),
            }
        } else if rest.len() <= PIECE {
            (rest, &rest[rest.len()..])
        } else {
            match rest[1..=PIECE].iter().rposition(|&b| b == b'/') {
                Some(j) => (&rest[..j + 1], &rest[j + 2..]),
                None => return Err(std::io::Error::from_raw_os_error(libc::ENAMETOOLONG)),
            }
        };
        let tail = &tail[tail.iter().take_while(|&&b| b == b'/').count()..];
        let last = tail.is_empty();
        let c = std::ffi::CString::new(piece)
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        let at = dir.as_ref().map_or(libc::AT_FDCWD, |d| d.0);
        // SAFETY: NUL-terminated name; `at` is open (or AT_FDCWD) until `dir` is replaced below.
        let fd = unsafe { libc::openat(at, c.as_ptr(), if last { last_flags } else { middle }) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ENAMETOOLONG) && !one && piece[1..].contains(&b'/') {
                one = true;
                continue;
            }
            return Err(e);
        }
        let fd = Fd(fd);
        if last {
            // SAFETY: an open descriptor and a zeroed stat buffer of the right type.
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(fd.0, &mut st) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let is_dir = st.st_mode & libc::S_IFMT == libc::S_IFDIR;
            #[allow(clippy::unnecessary_cast)]
            let ns = (st.st_mtime as i64)
                .saturating_mul(1_000_000_000)
                .saturating_add(st.st_mtime_nsec as i64);
            return Ok((is_dir, ns.max(0)));
        }
        dir = Some(fd);
        rest = tail;
    }
}

/// mtime table of folders and the rate-limited rounds that compare it (single-threaded).
pub struct DirPoller {
    /// stat'ed through symlinks (a scan root may be a link); others are not
    pub root: PathBuf,
    pub interval: Duration,
    pub rate: u32,
    batch: usize,
    /// stat calls so far (diagnostics, tests)
    pub stats: u64,
    pub root_lost: bool,
    mtimes: HashMap<PathBuf, i64>,
    /// baselines to take: (path, mtime the scan recorded, since): see [`listed_changed`]
    fresh: VecDeque<(PathBuf, Option<u32>, i64)>,
    /// the scan's record of the root was checked
    root_listed: bool,
    round: Vec<PathBuf>,
    pos: usize,
    next_round: Instant,
    next_batch: Instant,
}

impl DirPoller {
    pub fn new(root: &Path, interval: Duration, rate: u32) -> DirPoller {
        let interval = interval.max(Duration::from_millis(10));
        let rate = rate.max(1);
        let now = Instant::now();
        DirPoller {
            root: root.to_path_buf(),
            interval,
            rate,
            batch: ((rate / 20) as usize).clamp(1, MAX_BATCH),
            stats: 0,
            root_lost: false,
            mtimes: HashMap::new(),
            fresh: VecDeque::new(),
            root_listed: false,
            round: Vec::new(),
            pos: 0,
            next_round: now + interval,
            next_batch: now,
        }
    }

    pub fn len(&self) -> usize {
        self.mtimes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mtimes.is_empty()
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.mtimes.contains_key(path)
    }

    /// Start polling `path` (false: already polled). The baseline is taken at the next step.
    pub fn add(&mut self, path: PathBuf) -> bool {
        self.add_listed(path, None, 0)
    }

    /// [`DirPoller::add`] with the scan's record of the folder: `listed` (its mtime as the scan
    /// recorded it, whole seconds) and `since_ns`; the baseline is checked against it first
    /// ([`listed_changed`]): a change since the listing is reported.
    ///
    /// A folder already polled is left as it is: it was handed in again because its row moved
    /// (its parent was listed again), and its own baseline stands. Only the root, polled from the
    /// start (before the scan read it), takes the scan's record once, when it comes.
    pub fn add_listed(&mut self, path: PathBuf, listed: Option<u32>, since_ns: i64) -> bool {
        if self.mtimes.contains_key(&path) {
            if listed.is_some() && path == self.root && !self.root_listed {
                self.root_listed = true;
                self.fresh.push_back((path, listed, since_ns));
            }
            return false;
        }
        if listed.is_some() && path == self.root {
            self.root_listed = true;
        }
        self.mtimes.insert(path.clone(), PENDING);
        self.fresh.push_back((path, listed, since_ns));
        true
    }

    /// A folder moved: re-key it and every polled folder below it.
    pub fn rename_tree(&mut self, old: &Path, new: &Path) {
        let moved: Vec<PathBuf> = self
            .mtimes
            .keys()
            .filter(|p| p.starts_with(old))
            .cloned()
            .collect();
        for p in moved {
            let m = self.mtimes.remove(&p).unwrap();
            let rest = p.strip_prefix(old).unwrap();
            let to = if rest.as_os_str().is_empty() {
                new.to_path_buf()
            } else {
                new.join(rest)
            };
            self.mtimes.insert(to, m);
        }
    }

    /// When [`DirPoller::step`] has work (None: nothing to poll).
    pub fn wake_time(&self) -> Option<Instant> {
        if !self.fresh.is_empty() {
            return Some(Instant::now() - Duration::from_millis(1));
        }
        if !self.round.is_empty() {
            return Some(self.next_batch);
        }
        (!self.mtimes.is_empty()).then_some(self.next_round)
    }

    /// Does the work that is due at `now`: one chunk of baselines, or one batch of a round.
    pub fn step(&mut self, now: Instant) -> Vec<Change> {
        if !self.fresh.is_empty() {
            return self.baselines();
        }
        if self.round.is_empty() {
            if now < self.next_round || self.mtimes.is_empty() {
                return Vec::new();
            }
            self.round = self.mtimes.keys().cloned().collect();
            self.round.sort();
            self.pos = 0;
            self.next_round = now + self.interval;
            self.next_batch = now;
        }
        if now < self.next_batch {
            return Vec::new();
        }
        let end = (self.pos + self.batch).min(self.round.len());
        let batch: Vec<PathBuf> = self.round[self.pos..end].to_vec();
        self.pos = end;
        if end >= self.round.len() {
            self.round.clear();
        }
        let mut out = Vec::new();
        let mut gone = Vec::new();
        let mut done = 0u32;
        for path in batch {
            let Some(&old) = self.mtimes.get(&path) else {
                continue; // removed meanwhile (inside a folder found gone)
            };
            done += 1;
            match self.probe(&path) {
                None => gone.push(path),
                Some(UNKNOWN) => {}
                Some(m) if old == PENDING || old == UNKNOWN => {
                    self.mtimes.insert(path, m);
                }
                Some(m) if m != old => {
                    self.mtimes.insert(path.clone(), m);
                    out.push(Change::new(ChangeKind::Dir, path, Some(true)));
                }
                Some(_) => {}
            }
        }
        self.next_batch = now + Duration::from_secs_f64(done as f64 / self.rate as f64);
        if !gone.is_empty() {
            out.extend(self.drop_gone(gone));
        }
        out
    }

    fn baselines(&mut self) -> Vec<Change> {
        let mut out = Vec::new();
        let mut gone = Vec::new();
        let mut done = 0;
        while done < BASELINE_CHUNK {
            let Some((path, listed, since)) = self.fresh.pop_front() else {
                break;
            };
            let Some(&old) = self.mtimes.get(&path) else {
                continue; // dropped meanwhile
            };
            if old != PENDING && listed.is_none() {
                continue; // no baseline to take
            }
            done += 1;
            let m = match self.probe(&path) {
                None => {
                    gone.push(path); // already gone again: it was in the tree, so report it
                    continue;
                }
                Some(m) => m,
            };
            if m == UNKNOWN && old != PENDING {
                continue; // the root's check, cannot tell now: its baseline stands
            }
            self.mtimes.insert(path.clone(), m);
            if let Some(listed) = listed
                && m != UNKNOWN
                && listed_changed(m, listed, since)
            {
                out.push(Change::new(ChangeKind::Dir, path, Some(true)));
            }
        }
        if !gone.is_empty() {
            out.extend(self.drop_gone(gone));
        }
        out
    }

    /// The folder's mtime (ns); None: gone or no longer a folder; `UNKNOWN`: cannot tell.
    fn probe(&mut self, path: &Path) -> Option<i64> {
        self.stats += 1;
        let m = if path == self.root {
            std::fs::metadata(path)
        } else {
            std::fs::symlink_metadata(path)
        };
        match m {
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                None
            }
            #[cfg(unix)]
            Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                match stat_past_path_max(path) {
                    Ok((true, ns)) => Some(ns),
                    Ok((false, _)) => None, // no longer a folder
                    // ELOOP: a symbolic link now (its last name is opened without following it)
                    Err(e)
                        if matches!(
                            e.raw_os_error(),
                            Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP)
                        ) =>
                    {
                        None
                    }
                    Err(_) => Some(UNKNOWN),
                }
            }
            Err(_) => Some(UNKNOWN),
            Ok(m) if !m.is_dir() => None,
            Ok(m) => Some(
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos().min(i64::MAX as u128 - 2) as i64),
            ),
        }
    }

    /// Forget folders found gone and every polled folder below them; report the topmost ones.
    fn drop_gone(&mut self, gone: Vec<PathBuf>) -> Vec<Change> {
        let mut out = Vec::new();
        self.mtimes
            .retain(|p, _| !gone.iter().any(|g| p.starts_with(g)));
        for path in &gone {
            if gone.iter().any(|g| g != path && path.starts_with(g)) {
                continue; // below another gone folder of this batch
            }
            if *path == self.root {
                self.root_lost = true;
                out.push(Change::overflow(path.clone()));
            } else {
                out.push(Change::new(ChangeKind::Deleted, path.clone(), Some(true)));
            }
        }
        out
    }
}

// --------------------------------------------------------------------------- a watcher thread

/// The common machinery of the per-folder watchers: the sink, counters, stop.
pub(crate) struct Emitter {
    pub sink: Mutex<Option<Sink>>,
    pub stopping: AtomicBool,
    pub events: AtomicI64,
    pub overflows: AtomicI64,
}

impl Emitter {
    pub fn new() -> Emitter {
        Emitter {
            sink: Mutex::new(None),
            stopping: AtomicBool::new(false),
            events: AtomicI64::new(0),
            overflows: AtomicI64::new(0),
        }
    }

    pub fn emit(&self, changes: Vec<Change>) {
        if changes.is_empty() || self.stopping.load(Ordering::SeqCst) {
            return;
        }
        let sink = self.sink.locked().clone();
        let Some(sink) = sink else { return };
        self.events
            .fetch_add(changes.len() as i64, Ordering::Relaxed);
        let lost = changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Overflow)
            .count();
        self.overflows.fetch_add(lost as i64, Ordering::Relaxed);
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(changes))).is_err() {
            tracing::error!("{}", t("监视器回调出错"));
        }
    }
}

/// A background thread with a bounded stop: `finish` waits up to 0.9 s for it to end.
pub(crate) struct Worker {
    done: Mutex<Option<(mpsc::Receiver<()>, std::thread::ThreadId)>>,
}

impl Worker {
    pub fn new() -> Worker {
        Worker {
            done: Mutex::new(None),
        }
    }

    pub fn started(&self) -> bool {
        self.done.locked().is_some()
    }

    pub fn spawn(&self, name: &str, f: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
        let (tx, rx) = mpsc::channel();
        let h = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                f();
                let _ = tx.send(());
            })?;
        *self.done.locked() = Some((rx, h.thread().id()));
        Ok(())
    }

    pub fn finish(&self, wait: Duration) {
        let done = self.done.locked().take();
        if let Some((rx, id)) = done
            && id != std::thread::current().id()
        {
            let _ = rx.recv_timeout(wait);
        }
    }
}

/// A wake-up signal for a watcher thread.
pub(crate) struct Wake {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl Wake {
    pub fn new() -> Wake {
        Wake {
            flag: Mutex::new(false),
            cv: Condvar::new(),
        }
    }
    pub fn set(&self) {
        *self.flag.locked() = true;
        self.cv.notify_all();
    }
    /// Waits up to `d` for a wake-up; true when woken (the flag is cleared).
    pub fn wait(&self, d: Duration) -> bool {
        let g = self.flag.locked();
        let (mut g, _) = unpoison(self.cv.wait_timeout_while(g, d, |f| !*f));
        std::mem::replace(&mut *g, false)
    }
}

// --------------------------------------------------------------------------- the watcher

struct PollInner {
    root: PathBuf,
    poller: Mutex<DirPoller>,
    polled: AtomicI64,
    incoming: Mutex<Backlog>,
    wake: Wake,
    emitter: Emitter,
    state: Mutex<(WatchState, String)>,
}

/// Fallback watcher: polls the mtime of every folder handed in through [`Watcher::add_dirs`].
pub struct PollWatcher {
    inner: Arc<PollInner>,
    worker: Worker,
}

impl PollWatcher {
    pub fn new(root: &Path, poll_interval: f64, poll_rate: u32) -> PollWatcher {
        let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
        let interval = Duration::from_secs_f64(poll_interval.max(0.01));
        let mut incoming = Backlog::new();
        incoming.add(vec![root.clone()], None, 0);
        PollWatcher {
            inner: Arc::new(PollInner {
                poller: Mutex::new(DirPoller::new(&root, interval, poll_rate)),
                polled: AtomicI64::new(0),
                incoming: Mutex::new(incoming),
                root,
                wake: Wake::new(),
                emitter: Emitter::new(),
                state: Mutex::new((WatchState::Starting, String::new())),
            }),
            worker: Worker::new(),
        }
    }

    fn run(inner: Arc<PollInner>) {
        let body = || {
            while !inner.emitter.stopping.load(Ordering::SeqCst) {
                let (changes, lost, wake) = {
                    let mut poller = inner.poller.locked();
                    {
                        let mut inc = inner.incoming.locked();
                        let fed = inc.peek(4096);
                        let n = fed.len();
                        for (path, listed, since) in fed {
                            poller.add_listed(path, listed, since);
                        }
                        inc.advance(n);
                    }
                    let ch = poller.step(Instant::now());
                    inner.polled.store(poller.len() as i64, Ordering::Relaxed);
                    (ch, poller.root_lost, poller.wake_time())
                };
                inner.emitter.emit(changes);
                if lost {
                    *inner.state.locked() =
                        (WatchState::Failed, t("扫描根目录已不存在，实时监控已停止"));
                    return;
                }
                if !inner.incoming.locked().is_empty() {
                    continue;
                }
                let delay = wake.map_or(Duration::from_secs(1), |w| {
                    w.saturating_duration_since(Instant::now())
                });
                if delay > Duration::ZERO {
                    inner.wake.wait(delay.min(Duration::from_secs(1)));
                }
            }
        };
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).is_err() {
            tracing::error!(
                "{}",
                crate::tr!(
                    "轮询监视线程出错：{path}",
                    path = inner.root.display().to_string()
                )
            );
            *inner.state.locked() = (WatchState::Failed, t("实时监控出错，已停止"));
            inner
                .emitter
                .emit(vec![Change::overflow(inner.root.clone())]);
        }
    }

    /// Stat calls so far (tests).
    pub fn stats(&self) -> u64 {
        self.inner.poller.locked().stats
    }
}

impl Watcher for PollWatcher {
    fn name(&self) -> &'static str {
        "poll"
    }

    fn start(&self, sink: Sink) -> std::io::Result<()> {
        if self.worker.started() {
            return Err(std::io::Error::other(t("监视器已经启动")));
        }
        *self.inner.emitter.sink.locked() = Some(sink);
        // degraded: polling instead of notifications (the detail is written when read)
        self.inner.state.locked().0 = WatchState::Degraded;
        let inner = self.inner.clone();
        self.worker
            .spawn("sk-poll", move || PollWatcher::run(inner))
    }

    fn add_dirs(&self, dirs: Vec<PathBuf>, mtimes: Option<Vec<u32>>, since_ns: i64) {
        self.inner.incoming.locked().add(dirs, mtimes, since_ns);
        self.inner.wake.set();
    }

    fn stop(&self) {
        if self.inner.emitter.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        self.inner.wake.set();
        self.worker.finish(Duration::from_millis(900));
        let mut st = self.inner.state.locked();
        if st.0 != WatchState::Failed {
            st.0 = WatchState::Stopped;
        }
    }

    fn status(&self) -> WatchStatus {
        let (state, mut detail) = self.inner.state.locked().clone();
        if detail.is_empty() && state != WatchState::Starting {
            let secs = self.inner.poller.locked().interval.as_secs_f64();
            detail = crate::tr!(
                "未使用系统的文件变化通知，改为每 {interval} 秒轮询一次文件夹；文件大小的变化发现不了",
                interval = format!("{secs}")
            );
        }
        WatchStatus {
            state,
            detail,
            watched_dirs: 0,
            polled_dirs: self.inner.polled.load(Ordering::Relaxed),
            overflows: self.inner.emitter.overflows.load(Ordering::Relaxed),
            events: self.inner.emitter.events.load(Ordering::Relaxed),
        }
    }
}

impl Drop for PollWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn later(s: f64) -> Instant {
        Instant::now() + Duration::from_secs_f64(s)
    }

    fn drain(p: &mut DirPoller, at: Instant) -> Vec<Change> {
        let mut out = Vec::new();
        for _ in 0..1000 {
            let ch = p.step(at);
            out.extend(ch);
            if p.wake_time().is_none_or(|w| w > at) {
                break;
            }
        }
        out
    }

    fn set_mtime(path: &Path, secs_ahead: u64) {
        #[cfg(windows)]
        let f = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(0x0200_0000)
                .open(path)
                .unwrap()
        };
        #[cfg(not(windows))]
        let f = std::fs::File::open(path).unwrap();
        let t = std::time::SystemTime::now() + Duration::from_secs(secs_ahead);
        f.set_modified(t).unwrap();
    }

    #[test]
    fn poller_dir_change_and_deleted_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let a = root.join("a");
        let b = root.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let mut p = DirPoller::new(&root, Duration::from_secs(1), 10_000);
        for x in [&root, &a, &b] {
            assert!(p.add(x.clone()));
        }
        assert!(!p.add(a.clone()));
        assert!(drain(&mut p, Instant::now()).is_empty()); // baselines
        assert_eq!(p.len(), 3);
        std::fs::write(a.join("new.txt"), b"x").unwrap();
        set_mtime(&a, 5);
        std::fs::remove_dir(&b).unwrap();
        let got = drain(&mut p, later(1.5));
        assert!(
            got.contains(&Change::new(ChangeKind::Dir, a.clone(), Some(true))),
            "{got:?}"
        );
        assert!(got.contains(&Change::new(ChangeKind::Deleted, b.clone(), Some(true))));
        assert!(!p.contains(&b));
    }

    #[test]
    fn poller_drops_polled_folders_below_a_gone_one() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let deep = root.join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        let mut p = DirPoller::new(&root, Duration::from_secs(1), 10_000);
        for x in [root.clone(), root.join("a"), root.join("a/b"), deep.clone()] {
            p.add(x);
        }
        drain(&mut p, Instant::now());
        std::fs::remove_dir_all(root.join("a")).unwrap();
        let got = drain(&mut p, later(1.5));
        let deleted: Vec<_> = got
            .iter()
            .filter(|c| c.kind == ChangeKind::Deleted)
            .collect();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].path, root.join("a"));
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn poller_vanished_before_baseline_and_root_lost() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        std::fs::create_dir(&root).unwrap();
        let mut p = DirPoller::new(&root, Duration::from_secs(1), 10_000);
        p.add(root.join("never"));
        let got = p.step(Instant::now());
        assert_eq!(
            got,
            vec![Change::new(
                ChangeKind::Deleted,
                root.join("never"),
                Some(true)
            )]
        );
        p.add(root.clone());
        drain(&mut p, Instant::now());
        std::fs::remove_dir(&root).unwrap();
        let got = drain(&mut p, later(1.5));
        assert_eq!(got, vec![Change::overflow(root.clone())]);
        assert!(p.root_lost);
    }

    #[test]
    fn poller_rename_tree_rekeys_below() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("old/x")).unwrap();
        let mut p = DirPoller::new(&root, Duration::from_secs(1), 10_000);
        for x in [
            root.clone(),
            root.join("old"),
            root.join("old/x"),
            root.join("older"),
        ] {
            p.add(x);
        }
        p.rename_tree(&root.join("old"), &root.join("new"));
        assert!(p.contains(&root.join("new")) && p.contains(&root.join("new/x")));
        assert!(p.contains(&root.join("older"))); // a sibling sharing the prefix stays
        assert!(!p.contains(&root.join("old")));
    }

    #[test]
    fn poller_rate_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for i in 0..100 {
            std::fs::create_dir(root.join(format!("d{i}"))).unwrap();
        }
        let mut p = DirPoller::new(&root, Duration::from_secs(1), 200);
        for i in 0..100 {
            p.add(root.join(format!("d{i}")));
        }
        drain(&mut p, Instant::now());
        let base = p.stats;
        let at = later(1.1);
        p.step(at); // first batch of the round: 10 (rate / 20)
        assert_eq!(p.stats - base, 10);
        p.step(at); // the next batch waits for its time
        assert_eq!(p.stats - base, 10);
        let next = p.wake_time().unwrap();
        assert!(next > at && next <= at + Duration::from_millis(60));
    }

    #[test]
    fn poll_watcher_dir_and_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        let w = PollWatcher::new(&root, 0.05, 100_000);
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        w.start(Arc::new(move |b| g.lock().unwrap().extend(b)))
            .unwrap();
        w.add_dirs(vec![root.join("a"), root.join("b")], None, 0);
        std::thread::sleep(Duration::from_millis(150));
        std::fs::write(root.join("a/f"), b"x").unwrap();
        set_mtime(&root.join("a"), 5);
        std::fs::remove_dir(root.join("b")).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let g = got.lock().unwrap().clone();
            if g.iter()
                .any(|c| c.kind == ChangeKind::Dir && c.path == root.join("a"))
                && g.iter()
                    .any(|c| c.kind == ChangeKind::Deleted && c.path == root.join("b"))
            {
                break;
            }
            assert!(Instant::now() < end, "{g:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
        let st = w.status();
        assert_eq!(st.state, WatchState::Degraded);
        assert!(st.detail.contains("0.05"));
        assert_eq!(st.polled_dirs, 2);
        w.stop();
        assert_eq!(w.status().state, WatchState::Stopped);
    }

    #[test]
    fn poll_watcher_root_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        std::fs::create_dir(&root).unwrap();
        let w = PollWatcher::new(&root, 0.05, 100_000);
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        w.start(Arc::new(move |b| g.lock().unwrap().extend(b)))
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        std::fs::remove_dir(&root).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while w.status().state != WatchState::Failed {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(*got.lock().unwrap(), vec![Change::overflow(root.clone())]);
        w.stop();
        assert_eq!(w.status().state, WatchState::Failed);
    }

    fn mtime_ns(path: &Path) -> i64 {
        let t = std::fs::metadata(path).unwrap().modified().unwrap();
        t.duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as i64
    }

    fn now_ns() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64
    }

    #[test]
    fn listed_changed_rule() {
        let s: u32 = 1_700_000_000;
        let ns = s as i64 * 1_000_000_000;
        let since = ns + 500_000_000; // the scan began half a second into that second
        assert!(listed_changed(ns + 600_000_000, s, since)); // modified after the scan began
        assert!(listed_changed(ns + 460_000_000, s, since)); // within a tick of it: cannot tell
        assert!(!listed_changed(ns + 400_000_000, s, since)); // well before: the listing saw it
        assert!(listed_changed(ns + 400_000_000, s - 1, since)); // whole seconds differ
        assert!(listed_changed(ns, s, since)); // a whole second: maybe a file system that keeps no more
        assert!(!listed_changed(ns, s, since + 3_000_000_000));
        // the scanners' seconds truncate the double sec + nsec * 1e-9, which can round up
        assert!(!listed_changed(
            ns + 999_999_999,
            s + 1,
            since + 10_000_000_000
        ));
        assert!(listed_changed(ns + 999_999_999, s, since + 10_000_000_000));
    }

    #[test]
    fn backlog_keeps_batches_until_placed() {
        let mut b = Backlog::new();
        assert!(b.is_empty() && b.peek(10).is_empty());
        b.add(
            vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")],
            Some(vec![1, 2, 3]),
            7,
        );
        b.add(vec![PathBuf::from("d")], None, 0);
        b.add(Vec::new(), None, 0); // nothing: no batch
        b.add(vec![PathBuf::from("e")], Some(vec![1, 2]), 0); // lengths differ: no mtimes
        assert_eq!(
            b.peek(2),
            vec![
                (PathBuf::from("a"), Some(1), 7),
                (PathBuf::from("b"), Some(2), 7)
            ]
        );
        b.advance(2);
        assert_eq!(b.peek(5), vec![(PathBuf::from("c"), Some(3), 7)]); // one batch at a time
        b.advance(1);
        assert_eq!(b.peek(5), vec![(PathBuf::from("d"), None, 0)]);
        b.advance(1);
        assert_eq!(b.peek(5), vec![(PathBuf::from("e"), None, 0)]);
        b.advance(1);
        assert!(b.is_empty());
    }

    #[test]
    fn poller_baseline_checks_what_the_scan_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let [same, moved, recent, plain] =
            ["same", "moved", "recent", "plain"].map(|n| tmp.path().join(n));
        for d in [&same, &moved, &recent, &plain] {
            std::fs::create_dir(d).unwrap();
        }
        let secs = |d: &Path| (mtime_ns(d) / 1_000_000_000) as u32;
        let later = now_ns() + 3600 * 1_000_000_000; // the scan started after every one of these times
        let mut p = DirPoller::new(tmp.path(), Duration::from_secs(60), 1000);
        assert!(p.add_listed(same.clone(), Some(secs(&same)), later));
        assert!(p.add_listed(moved.clone(), Some(secs(&moved) - 5), later)); // whole seconds differ
        assert!(p.add_listed(recent.clone(), Some(secs(&recent)), mtime_ns(&recent))); // after the scan began
        assert!(p.add(plain.clone())); // nothing recorded: the baseline as before
        assert_eq!(
            p.step(Instant::now()),
            vec![
                Change::new(ChangeKind::Dir, moved.clone(), Some(true)),
                Change::new(ChangeKind::Dir, recent.clone(), Some(true)),
            ]
        );
        assert!(p.step(Instant::now()).is_empty());
    }

    #[test]
    fn poller_root_checks_what_the_scan_recorded_once() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let mut p = DirPoller::new(&root, Duration::from_secs(60), 1000);
        assert!(p.add(root.clone()));
        assert!(p.step(Instant::now()).is_empty()); // the baseline
        let secs = (mtime_ns(&root) / 1_000_000_000) as u32;
        let later = now_ns() + 3600 * 1_000_000_000;
        assert!(!p.add_listed(root.clone(), Some(secs - 5), later));
        assert_eq!(
            p.step(Instant::now()),
            vec![Change::new(ChangeKind::Dir, root.clone(), Some(true))]
        );
        assert!(!p.add_listed(root.clone(), Some(secs - 5), later)); // once only
        assert!(p.step(Instant::now()).is_empty());
    }

    #[test]
    fn poll_watcher_checks_handed_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let listed = |d: &Path| (mtime_ns(d) / 1_000_000_000) as u32;
        let (la, lb) = (listed(&a), listed(&b) - 5); // b changed since its listing
        let w = PollWatcher::new(&root, 60.0, 100_000);
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        w.start(Arc::new(move |x| g.lock().unwrap().extend(x)))
            .unwrap();
        w.add_dirs(
            vec![a.clone(), b.clone()],
            Some(vec![la, lb]),
            now_ns() + 3600 * 1_000_000_000,
        );
        let end = Instant::now() + Duration::from_secs(5);
        while !got
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.kind == ChangeKind::Dir && c.path == b)
        {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!got.lock().unwrap().iter().any(|c| c.path == a));
        w.stop();
    }

    /// A folder whose path is longer than PATH_MAX (4,096 bytes on Linux, 1,024 on macOS), made
    /// one level at a time relative to the one above: no single call can name it.
    #[cfg(unix)]
    fn deep_folder(top: &Path, longer_than: usize) -> (PathBuf, std::fs::File) {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::{AsRawFd, FromRawFd};
        let name = std::ffi::CString::new("d".repeat(200)).unwrap();
        let mut dir = std::fs::File::open(top).unwrap();
        let mut path = top.as_os_str().as_bytes().to_vec();
        loop {
            // SAFETY: plain syscalls on descriptors we own.
            let below = unsafe {
                assert_eq!(libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o755), 0);
                libc::openat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            assert!(below >= 0);
            path.extend_from_slice(b"/");
            path.extend_from_slice(name.as_bytes());
            if path.len() > longer_than {
                // `dir` stays open: the parent of the deep folder
                return (PathBuf::from(std::ffi::OsStr::from_bytes(&path)), {
                    // SAFETY: closing the deepest folder, keeping its parent
                    unsafe { libc::close(below) };
                    dir
                });
            }
            // SAFETY: a descriptor we just opened
            dir = unsafe { std::fs::File::from_raw_fd(below) };
        }
    }

    #[cfg(unix)]
    #[test]
    fn poller_reads_folders_beyond_path_max() {
        use std::os::unix::io::AsRawFd;
        let tmp = tempfile::tempdir().unwrap();
        let (deep, parent) = deep_folder(tmp.path(), 5000);
        let err = std::fs::metadata(&deep).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENAMETOOLONG));
        let mut p = DirPoller::new(tmp.path(), Duration::from_secs(10), 1000);
        assert!(p.add(deep.clone()));
        assert!(p.step(Instant::now()).is_empty());
        let name = std::ffi::CString::new("d".repeat(200)).unwrap();
        // SAFETY: plain syscalls relative to the open parent; a zeroed stat buffer.
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            assert_eq!(
                libc::fstatat(parent.as_raw_fd(), name.as_ptr(), &mut st, 0),
                0
            );
            let times = [
                libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_OMIT,
                },
                libc::timespec {
                    tv_sec: st.st_mtime + 5,
                    tv_nsec: 0,
                },
            ];
            assert_eq!(
                libc::utimensat(parent.as_raw_fd(), name.as_ptr(), times.as_ptr(), 0),
                0
            );
        }
        let at = p.wake_time().unwrap() + Duration::from_millis(1);
        assert_eq!(
            p.step(at),
            vec![Change::new(ChangeKind::Dir, deep.clone(), Some(true))]
        );
        // SAFETY: as above
        unsafe {
            assert_eq!(
                libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR),
                0
            )
        };
        let at = p.wake_time().unwrap() + Duration::from_millis(1);
        assert_eq!(
            p.step(at),
            vec![Change::new(ChangeKind::Deleted, deep.clone(), Some(true))]
        );
        assert!(p.is_empty());
        drop(parent); // tempdir's remove_dir_all walks with openat: deep paths are fine
    }

    #[test]
    fn stat_file_numbers() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f");
        std::fs::write(&f, vec![b'x'; 5000]).unwrap();
        if cfg!(unix) {
            let (size, alloc, mtime) = stat_file(&f).unwrap();
            assert_eq!(size, 5000);
            assert!(alloc >= 4096 && mtime > 0);
            assert!(stat_file(tmp.path()).is_none());
        } else {
            assert!(stat_file(&f).is_none());
        }
        assert!(stat_file(&tmp.path().join("missing")).is_none());
    }

    #[test]
    fn a_poisoned_lock_does_not_turn_every_later_call_into_a_panic() {
        // a panic while one of the watcher's locks was held (a bug; the thread that had it caught
        // it and reported the watch as failed): status, add_dirs, the sink and stop go on
        let tmp = tempfile::tempdir().unwrap();
        let w = PollWatcher::new(tmp.path(), 0.05, 2000);
        let got = Arc::new(Mutex::new(0usize));
        let seen = got.clone();
        w.start(Arc::new(move |b| *seen.lock().unwrap() += b.len()))
            .unwrap();
        let inner = w.inner.clone();
        let poisoned = std::thread::spawn(move || {
            let _state = inner.state.lock().unwrap();
            let _incoming = inner.incoming.lock().unwrap();
            let _sink = inner.emitter.sink.lock().unwrap();
            let _flag = inner.wake.flag.lock().unwrap();
            panic!("injected: a bug while holding the watcher's locks");
        })
        .join();
        assert!(poisoned.is_err());
        assert!(w.inner.state.is_poisoned() && w.inner.emitter.sink.is_poisoned());
        let _ = w.status();
        w.add_dirs(vec![tmp.path().to_path_buf()], None, 0);
        // a folder's mtime moves on in clock ticks (a few ms on Linux): a file made in the tick the
        // folder was made in leaves the mtime the baseline saw, so add files until one is reported
        let end = Instant::now() + Duration::from_secs(10);
        let mut n = 0;
        while *got.lock().unwrap() == 0 && Instant::now() < end {
            std::fs::write(tmp.path().join(format!("new-{n}.txt")), b"x").unwrap();
            n += 1;
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(*got.lock().unwrap() > 0, "the watcher stopped reporting");
        w.stop();
        assert_eq!(w.status().state, WatchState::Stopped);
    }
}
