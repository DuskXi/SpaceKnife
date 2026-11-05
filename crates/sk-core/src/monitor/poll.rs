//! Generic polling watcher (docs/monitor.md §2.4) and the mtime reconciliation inotify reuses.
//!
//! A folder's mtime changes whenever an entry is added, removed or renamed in it, so re-stat'ing
//! the scanned folders finds every structural change, at most `poll_interval` seconds (plus the
//! round's own duration) late. File content changes stay invisible.
//!
//! [`DirPoller`] is the clock-driven core (no thread, no locks): its owner calls
//! [`DirPoller::step`] whenever [`DirPoller::wake_time`] has come. A round stats at most
//! `poll_rate` folders per second, in small batches spaced out in time. The baseline of a newly
//! added folder is taken right away (in chunks, not rate limited).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::{Change, ChangeKind, Sink, WatchState, WatchStatus, Watcher, t};

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
        Some((m.size() as i64, m.blocks() as i64 * 512, super::secs(m.modified())))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
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
    fresh: VecDeque<PathBuf>,
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
        if self.mtimes.contains_key(&path) {
            return false;
        }
        self.mtimes.insert(path.clone(), PENDING);
        self.fresh.push_back(path);
        true
    }

    /// A folder moved: re-key it and every polled folder below it.
    pub fn rename_tree(&mut self, old: &Path, new: &Path) {
        let moved: Vec<PathBuf> = self.mtimes.keys().filter(|p| p.starts_with(old)).cloned().collect();
        for p in moved {
            let m = self.mtimes.remove(&p).unwrap();
            let rest = p.strip_prefix(old).unwrap();
            let to = if rest.as_os_str().is_empty() { new.to_path_buf() } else { new.join(rest) };
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
        let mut gone = Vec::new();
        let mut done = 0;
        while done < BASELINE_CHUNK {
            let Some(path) = self.fresh.pop_front() else { break };
            if self.mtimes.get(&path) != Some(&PENDING) {
                continue;
            }
            done += 1;
            match self.probe(&path) {
                None => gone.push(path), // already gone again: it was in the tree, so report it
                Some(m) => {
                    self.mtimes.insert(path, m);
                }
            }
        }
        if gone.is_empty() { Vec::new() } else { self.drop_gone(gone) }
    }

    /// The folder's mtime (ns); None: gone or no longer a folder; `UNKNOWN`: cannot tell.
    fn probe(&mut self, path: &Path) -> Option<i64> {
        self.stats += 1;
        let m = if path == self.root { std::fs::metadata(path) } else { std::fs::symlink_metadata(path) };
        match m {
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => None,
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
        self.mtimes.retain(|p, _| !gone.iter().any(|g| p.starts_with(g)));
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
        let sink = self.sink.lock().unwrap().clone();
        let Some(sink) = sink else { return };
        self.events.fetch_add(changes.len() as i64, Ordering::Relaxed);
        let lost = changes.iter().filter(|c| c.kind == ChangeKind::Overflow).count();
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
        Worker { done: Mutex::new(None) }
    }

    pub fn started(&self) -> bool {
        self.done.lock().unwrap().is_some()
    }

    pub fn spawn(&self, name: &str, f: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
        let (tx, rx) = mpsc::channel();
        let h = std::thread::Builder::new().name(name.into()).spawn(move || {
            f();
            let _ = tx.send(());
        })?;
        *self.done.lock().unwrap() = Some((rx, h.thread().id()));
        Ok(())
    }

    pub fn finish(&self, wait: Duration) {
        let done = self.done.lock().unwrap().take();
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
        Wake { flag: Mutex::new(false), cv: Condvar::new() }
    }
    pub fn set(&self) {
        *self.flag.lock().unwrap() = true;
        self.cv.notify_all();
    }
    /// Waits up to `d` for a wake-up; true when woken (the flag is cleared).
    pub fn wait(&self, d: Duration) -> bool {
        let g = self.flag.lock().unwrap();
        let (mut g, _) = self.cv.wait_timeout_while(g, d, |f| !*f).unwrap();
        std::mem::replace(&mut *g, false)
    }
}

// --------------------------------------------------------------------------- the watcher

struct PollInner {
    root: PathBuf,
    poller: Mutex<DirPoller>,
    polled: AtomicI64,
    incoming: Mutex<VecDeque<PathBuf>>,
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
        PollWatcher {
            inner: Arc::new(PollInner {
                poller: Mutex::new(DirPoller::new(&root, interval, poll_rate)),
                polled: AtomicI64::new(0),
                incoming: Mutex::new(VecDeque::from([root.clone()])),
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
                    let mut poller = inner.poller.lock().unwrap();
                    {
                        let mut inc = inner.incoming.lock().unwrap();
                        for _ in 0..inc.len().min(4096) {
                            let p = inc.pop_front().unwrap();
                            poller.add(p);
                        }
                    }
                    let ch = poller.step(Instant::now());
                    inner.polled.store(poller.len() as i64, Ordering::Relaxed);
                    (ch, poller.root_lost, poller.wake_time())
                };
                inner.emitter.emit(changes);
                if lost {
                    *inner.state.lock().unwrap() = (WatchState::Failed, t("扫描根目录已不存在，实时监控已停止"));
                    return;
                }
                if !inner.incoming.lock().unwrap().is_empty() {
                    continue;
                }
                let delay = wake.map_or(Duration::from_secs(1), |w| w.saturating_duration_since(Instant::now()));
                if delay > Duration::ZERO {
                    inner.wake.wait(delay.min(Duration::from_secs(1)));
                }
            }
        };
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).is_err() {
            tracing::error!("{}", crate::tr!("轮询监视线程出错：{path}", path = inner.root.display().to_string()));
            *inner.state.lock().unwrap() = (WatchState::Failed, t("实时监控出错，已停止"));
            inner.emitter.emit(vec![Change::overflow(inner.root.clone())]);
        }
    }

    /// Stat calls so far (tests).
    pub fn stats(&self) -> u64 {
        self.inner.poller.lock().unwrap().stats
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
        *self.inner.emitter.sink.lock().unwrap() = Some(sink);
        // degraded: polling instead of notifications (the detail is written when read)
        self.inner.state.lock().unwrap().0 = WatchState::Degraded;
        let inner = self.inner.clone();
        self.worker.spawn("sk-poll", move || PollWatcher::run(inner))
    }

    fn add_dirs(&self, dirs: Vec<PathBuf>) {
        self.inner.incoming.lock().unwrap().extend(dirs);
        self.inner.wake.set();
    }

    fn stop(&self) {
        if self.inner.emitter.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        self.inner.wake.set();
        self.worker.finish(Duration::from_millis(900));
        let mut st = self.inner.state.lock().unwrap();
        if st.0 != WatchState::Failed {
            st.0 = WatchState::Stopped;
        }
    }

    fn status(&self) -> WatchStatus {
        let (state, mut detail) = self.inner.state.lock().unwrap().clone();
        if detail.is_empty() && state != WatchState::Starting {
            let secs = self.inner.poller.lock().unwrap().interval.as_secs_f64();
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
            std::fs::OpenOptions::new().write(true).custom_flags(0x0200_0000).open(path).unwrap()
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
        assert!(got.contains(&Change::new(ChangeKind::Dir, a.clone(), Some(true))), "{got:?}");
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
        let deleted: Vec<_> = got.iter().filter(|c| c.kind == ChangeKind::Deleted).collect();
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
        assert_eq!(got, vec![Change::new(ChangeKind::Deleted, root.join("never"), Some(true))]);
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
        for x in [root.clone(), root.join("old"), root.join("old/x"), root.join("older")] {
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
        w.start(Arc::new(move |b| g.lock().unwrap().extend(b))).unwrap();
        w.add_dirs(vec![root.join("a"), root.join("b")]);
        std::thread::sleep(Duration::from_millis(150));
        std::fs::write(root.join("a/f"), b"x").unwrap();
        set_mtime(&root.join("a"), 5);
        std::fs::remove_dir(root.join("b")).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let g = got.lock().unwrap().clone();
            if g.iter().any(|c| c.kind == ChangeKind::Dir && c.path == root.join("a"))
                && g.iter().any(|c| c.kind == ChangeKind::Deleted && c.path == root.join("b"))
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
        w.start(Arc::new(move |b| g.lock().unwrap().extend(b))).unwrap();
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
}
