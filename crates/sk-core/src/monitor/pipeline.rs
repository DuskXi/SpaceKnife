//! Live change monitoring of an open scan: the change pipeline (port of the core of
//! `server/monitors.py`'s `ScanMonitor`; docs/monitor.md §5).
//!
//! A platform watcher per scan root reports where something changed; batches are coalesced
//! (`coalesce` of quiet, at most [`MAX_WAIT`]), then
//!
//! - a file whose size or times changed is updated in place ([`mutate::update_stats`]);
//! - a folder whose entries changed is listed again — one level, with the scan's own lister — and
//!   folded in by [`mutate::relist`] (deepest folders first); new folders are scanned;
//! - a lost batch (`overflow`) is replayed from the NTFS change journal when the elevated helper
//!   can read it ([`UsnSource`]), else it (or an event storm) marks a subtree dirty, listed again
//!   folder by folder once [`QUIET_RESCAN`] passed without events.
//!
//! Changes run under the store's [`sk_store::Writer`]; [`MonitorHooks`] lets the views remap
//! their node ids (while the writer is held) and flash what changed (after). While a hard scan
//! still runs, folders with children waiting in its queue are deferred; while an MFT scan reads,
//! everything waits for its tree.
//!
//! [`ScanMonitor`] is synchronous: the server runs [`ScanMonitor::run`] on a thread of its own
//! (or calls [`ScanMonitor::process`] itself).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use sk_native::hardscan::{self, Ref, RootOptions, SliceOptions, SliceResult, Task};
use sk_protocol::scan::{MonitorState, MonitorStatus};
use sk_store::lookup::PathIndex;
use sk_store::{Flags, Kind, NodeId, Nodes, Store, mutate};

use super::poll::PollWatcher;
use super::{Change, ChangeKind, Sink, WatchState, Watcher, create_watcher};
use crate::scan::{IoGate, RootInfo, entry_batch, fold_slice, path_inside};
use crate::volume;

/// A steady stream of events is still applied this often.
pub const MAX_WAIT: Duration = Duration::from_secs(1);
/// More folders to list again in one batch: rescan their common subtree instead.
pub const STORM_FOLDERS: usize = 2000;
pub const STORM_EVENTS: usize = 20_000;
/// Dirty subtrees wait for this long without events.
pub const QUIET_RESCAN: Duration = Duration::from_secs(2);
/// Folders listed per round of a subtree rescan.
pub const LIST_BATCH: usize = 64;
pub const FREE_REFRESH: Duration = Duration::from_secs(1);
/// Checkpoints of the USN journal move on this often.
pub const USN_PERIOD: Duration = Duration::from_secs(30);
/// More records to replay than this: list the subtree again instead.
pub const USN_MAX_ROWS: usize = 1_000_000;
/// An NTFS file reference's record number.
pub const REF_MASK: u64 = (1 << 48) - 1;
/// While a scan runs: hand new folders to the watchers this often.
pub const SCAN_TICK: Duration = Duration::from_millis(250);

/// What the journal says since a checkpoint.
#[derive(Clone, Debug, Default)]
pub struct UsnRead {
    /// parent file references of every record (NTFS references: record number in the low 48 bits)
    pub parent_frn: Vec<u64>,
    /// more records than asked for: not usable
    pub truncated: bool,
}

/// The NTFS change journal, read through the elevated helper ([`crate::helper::SharedHelper`]
/// implements it: only a helper that already runs elevated is asked, never one started for it).
pub trait UsnSource: Send + Sync {
    /// `(journal_id, next_usn)` of a local NTFS volume `X:`.
    fn query(&self, volume: &str) -> Result<(u64, u64), String>;
    /// Records from the checkpoint `(journal_id, start_usn)` to the journal's end.
    fn read(&self, volume: &str, start_usn: u64, journal_id: u64, max_rows: usize) -> Result<UsnRead, String>;
}

impl UsnSource for crate::helper::SharedHelper {
    fn query(&self, volume: &str) -> Result<(u64, u64), String> {
        let c = self.current().filter(|c| c.elevated()).ok_or("no elevated helper")?;
        let j = c.query_usn(volume).map_err(|e| e.to_string())?;
        let get = |k: &str| j.get(k).and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|i| i.max(0) as u64)));
        Ok((get("journal_id").ok_or("no journal_id")?, get("next_usn").ok_or("no next_usn")?))
    }

    fn read(&self, volume: &str, start_usn: u64, journal_id: u64, max_rows: usize) -> Result<UsnRead, String> {
        let c = self.current().filter(|c| c.elevated()).ok_or("no elevated helper")?;
        let t = c
            .read_usn(volume, start_usn as i64, journal_id, max_rows as u64, 1.0, None)
            .map_err(|e| e.to_string())?;
        Ok(UsnRead { parent_frn: t.rows.parent_frn, truncated: t.truncated })
    }
}

/// What the pipeline tells the rest of the backend (views, the scan's status, the log).
pub trait MonitorHooks {
    /// A hard scan still appends to the store (folders waiting in its queue are not relisted).
    fn scan_running(&self) -> bool {
        false
    }
    /// An MFT read is under way: its tree replaces the store at the end, so changes wait.
    fn mft_running(&self) -> bool {
        false
    }
    /// Rows moved: called with the writer held (`nodes` is its view); views remap their ids.
    /// Never take the store's read or write guard in here.
    fn remapped(&mut self, _nodes: &Nodes, _remap: &IdRemap) {}
    /// The store was compacted and adopted: every id is new (`remap[old]` = new id or -1).
    /// Called with the writer held.
    fn compacted(&mut self, _nodes: &Nodes, _remap: &[i32]) {}
    /// Changes were applied: node → change of allocation (new ids), for the views to flash.
    fn applied(&mut self, _flashes: &HashMap<NodeId, i64>) {}
    /// A root's volume was read again and its free or used space changed.
    fn volumes_changed(&mut self, _roots: &[RootInfo]) {}
    /// A line for the log panel (events topic "monitor").
    fn message(&mut self, _text: String) {}
}

/// No hooks (tests, a monitor without views).
pub struct NoHooks;
impl MonitorHooks for NoHooks {}

/// Old ids to new ones over the relists of one batch (each relist moves rows at most once).
#[derive(Clone, Debug, Default)]
pub struct IdRemap {
    steps: Vec<(Vec<NodeId>, Vec<NodeId>)>,
}

impl IdRemap {
    fn add(&mut self, old: &[NodeId], new: &[NodeId]) {
        if old.is_empty() {
            return;
        }
        let mut pairs: Vec<(NodeId, NodeId)> = old.iter().copied().zip(new.iter().copied()).collect();
        pairs.sort_unstable();
        self.steps.push(pairs.into_iter().unzip());
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// The id `id` has now; None when it left the tree.
    pub fn map(&self, nodes: &Nodes, id: NodeId) -> Option<NodeId> {
        let mut out = id;
        for (old, new) in &self.steps {
            if let Ok(at) = old.binary_search(&out) {
                out = new[at];
            }
        }
        ((out as usize) < nodes.len() && !nodes.flags(out).intersects(Flags::GONE)).then_some(out)
    }
}

/// How a [`ScanMonitor`] works.
#[derive(Clone)]
pub struct MonitorOptions {
    /// Start platform watchers (false: only rescans asked for; live monitoring is off).
    pub watch: bool,
    /// Quiet time before a batch is applied (setting `scan.coalesce_ms`).
    pub coalesce: Duration,
    /// Polling watchers: seconds between rounds and folders per second.
    pub poll_interval: f64,
    pub poll_rate: u32,
    /// The tree came from a hard scan: file sizes from events follow its allocation rule.
    pub hard_rule: bool,
    /// The disk-load limit of the scan (asked before each listing batch while it runs).
    pub gate: Option<Arc<dyn IoGate>>,
    /// The change journal (Windows, elevated helper running).
    pub usn: Option<Arc<dyn UsnSource>>,
}

impl Default for MonitorOptions {
    fn default() -> Self {
        MonitorOptions {
            watch: true,
            coalesce: Duration::from_millis(150),
            poll_interval: 30.0,
            poll_rate: 2000,
            hard_rule: true,
            gate: None,
            usn: None,
        }
    }
}

struct Inbox {
    queue: Mutex<Vec<Change>>,
    rescans: Mutex<Vec<PathBuf>>,
    cv: Condvar,
    woken: Mutex<bool>,
}

/// Controls a [`ScanMonitor`] whose loop runs on another thread.
#[derive(Clone)]
pub struct MonitorHandle {
    inbox: Arc<Inbox>,
    stop: Arc<AtomicBool>,
}

impl MonitorHandle {
    /// Ends [`ScanMonitor::run`] (the watchers stop when the monitor is dropped or stopped).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.inbox.wake();
    }

    /// Look at the tree now (a scan just finished: its last folders still need watches).
    pub fn kick(&self) {
        self.inbox.wake();
    }

    /// List `path` and everything below it again, right away (F5 「重新扫描当前视图」).
    pub fn rescan(&self, path: &Path) {
        self.inbox.rescans.lock().unwrap().push(path.to_path_buf());
        self.inbox.wake();
    }

    /// Hand changes in as a watcher would.
    pub fn push(&self, changes: Vec<Change>) {
        self.inbox.queue.lock().unwrap().extend(changes);
        self.inbox.wake();
    }
}

impl Inbox {
    fn wake(&self) {
        *self.woken.lock().unwrap() = true;
        self.cv.notify_all();
    }

    /// Waits until woken or `timeout` (None: until woken); true when woken.
    fn wait(&self, timeout: Option<Duration>) -> bool {
        let g = self.woken.lock().unwrap();
        let mut g = match timeout {
            Some(t) => self.cv.wait_timeout_while(g, t, |w| !*w).unwrap().0,
            None => self.cv.wait_while(g, |w| !*w).unwrap(),
        };
        std::mem::replace(&mut *g, false)
    }
}

/// Live change monitoring of one scan (see the module docs).
pub struct ScanMonitor {
    store: Store,
    roots: Vec<RootInfo>,
    root_opts: Arc<Vec<RootOptions>>,
    opts: MonitorOptions,
    index: PathIndex,
    watchers: Vec<(PathBuf, Box<dyn Watcher>)>,
    inbox: Arc<Inbox>,
    stop: Arc<AtomicBool>,
    /// folders whose children still wait in the scan's queue
    deferred: HashSet<PathBuf>,
    /// subtrees to list again once quiet
    trees: HashSet<PathBuf>,
    pub events: i64,
    pub changes: i64,
    pub overflows: i64,
    pub last_apply_ms: f64,
    last_event: Option<Instant>,
    fed: usize,
    free_at: Option<Instant>,
    free_due: bool,
    failed: String,
    /// per volume: (journal id, safe checkpoint, newer checkpoint)
    usn: HashMap<String, (u64, u64, u64)>,
    usn_at: Option<Instant>,
    ino: Option<(usize, Vec<u64>, Vec<NodeId>)>,
}

fn bytes(p: &Path) -> &[u8] {
    p.as_os_str().as_encoded_bytes()
}

fn parent_dir(p: &Path) -> PathBuf {
    p.parent().map_or_else(|| p.to_path_buf(), Path::to_path_buf)
}

/// The common folder of `paths` per drive / file system root.
pub fn common_roots(paths: &HashSet<PathBuf>) -> HashSet<PathBuf> {
    let mut groups: HashMap<std::ffi::OsString, Vec<&PathBuf>> = HashMap::new();
    for p in paths {
        let key = p.components().next().map_or_else(Default::default, |c| c.as_os_str().to_os_string());
        groups.entry(key).or_default().push(p);
    }
    let mut out = HashSet::new();
    for qs in groups.values() {
        let mut common: PathBuf = qs[0].clone();
        for q in &qs[1..] {
            while !q.starts_with(&common) {
                if !common.pop() {
                    break;
                }
            }
        }
        if common.as_os_str().is_empty() {
            out.extend(qs.iter().map(|q| (*q).clone()));
        } else {
            out.insert(common);
        }
    }
    out
}

/// The numbers a hard scan records for a file of `size` (Windows estimates the allocation: a
/// cloud placeholder occupies nothing, files up to `resident_max` live in the MFT record, the rest
/// round up to whole clusters).
pub fn hard_alloc(size: i64, cloud: bool, opts: &RootOptions) -> i64 {
    if cloud || size <= opts.resident_max {
        return 0;
    }
    let cluster = if opts.cluster_size > 0 { opts.cluster_size } else { 4096 };
    (size + cluster - 1) / cluster * cluster
}

/// Names of a one-folder listing no path stands for alone: held more than once, or with a
/// separator in them.
fn unnamed(res: &SliceResult) -> HashSet<Vec<u8>> {
    let mut seen = HashSet::new();
    let mut out = HashSet::new();
    for name in res.entries.names() {
        let sep = name.iter().any(|&b| b == b'/' || (cfg!(windows) && b == b'\\'));
        if !seen.insert(name.to_vec()) || sep {
            out.insert(name.to_vec());
        }
    }
    out
}

impl ScanMonitor {
    pub fn new(store: Store, roots: Vec<RootInfo>, root_opts: Arc<Vec<RootOptions>>, opts: MonitorOptions) -> ScanMonitor {
        ScanMonitor {
            store,
            roots,
            root_opts,
            opts,
            index: PathIndex::new(),
            watchers: Vec::new(),
            inbox: Arc::new(Inbox {
                queue: Mutex::new(Vec::new()),
                rescans: Mutex::new(Vec::new()),
                cv: Condvar::new(),
                woken: Mutex::new(false),
            }),
            stop: Arc::new(AtomicBool::new(false)),
            deferred: HashSet::new(),
            trees: HashSet::new(),
            events: 0,
            changes: 0,
            overflows: 0,
            last_apply_ms: 0.0,
            last_event: None,
            fed: 0,
            free_at: None,
            free_due: false,
            failed: String::new(),
            usn: HashMap::new(),
            usn_at: None,
            ino: None,
        }
    }

    pub fn roots(&self) -> &[RootInfo] {
        &self.roots
    }

    /// What watchers deliver into (called from their threads).
    pub fn sink(&self) -> Sink {
        let inbox = self.inbox.clone();
        Arc::new(move |batch: Vec<Change>| {
            inbox.queue.lock().unwrap().extend(batch);
            inbox.wake();
        })
    }

    /// Start a watcher per root (polling when the platform's cannot watch). Network volumes and
    /// WSL's 9P drives are polled on POSIX: the kernel never hears of other machines' changes.
    pub fn start(&mut self) {
        if !self.opts.watch {
            return;
        }
        for root in self.roots.clone() {
            let remote = root.volume.as_ref().is_some_and(|v| v.is_remote || !v.note.is_empty());
            let first: Box<dyn Watcher> = if !cfg!(windows) && remote {
                Box::new(PollWatcher::new(&root.path, self.opts.poll_interval, self.opts.poll_rate))
            } else {
                create_watcher(&root.path, self.opts.poll_interval, self.opts.poll_rate)
            };
            if let Some(w) = self.start_watcher(&root.path, first) {
                self.watchers.push((root.path.clone(), w));
            }
        }
        self.kick();
    }

    fn start_watcher(&mut self, path: &Path, watcher: Box<dyn Watcher>) -> Option<Box<dyn Watcher>> {
        let poll = || Box::new(PollWatcher::new(path, self.opts.poll_interval, self.opts.poll_rate)) as Box<dyn Watcher>;
        if let Err(e) = watcher.start(self.sink()) {
            tracing::warn!("cannot watch {} with {}: {e}", path.display(), watcher.name());
            if watcher.name() == "poll" {
                self.failed = crate::tr!("无法监视 {path}：{error}", path = path.display().to_string(), error = e.to_string());
                return None;
            }
            return self.start_watcher(path, poll());
        }
        if watcher.name() != "poll" {
            // the first read is issued on the watcher's thread: give it a moment
            for _ in 0..10 {
                match watcher.status().state {
                    WatchState::Starting => std::thread::sleep(Duration::from_millis(50)),
                    WatchState::Failed => {
                        watcher.stop();
                        return self.start_watcher(path, poll());
                    }
                    _ => break,
                }
            }
        }
        Some(watcher)
    }

    /// Look at the tree now (a scan just finished: its last folders still need watches).
    pub fn kick(&self) {
        self.inbox.wake();
    }

    /// List `path` and everything below it again, right away (F5 「重新扫描当前视图」).
    pub fn rescan(&mut self, path: &Path) {
        self.trees.insert(path.to_path_buf());
        self.last_event = None; // no quiet period to wait for: asked for by the user
        self.kick();
    }

    /// Every folder of the tree was handed to the watchers.
    pub fn fed(&self) -> bool {
        self.fed >= self.store.len()
    }

    /// The whole tree was replaced (an MFT scan finished): every id is new.
    pub fn rebuilt(&mut self) {
        self.index.forget();
        self.fed = 0;
        self.ino = None;
    }

    /// A handle for other threads (stop, kick, rescan, push changes).
    pub fn handle(&self) -> MonitorHandle {
        MonitorHandle { inbox: self.inbox.clone(), stop: self.stop.clone() }
    }

    /// Stops the watchers (and a running loop).
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.kick();
        for (_, w) in self.watchers.drain(..) {
            w.stop();
        }
    }

    // ------------------------------------------------------------- status

    pub fn status(&self) -> MonitorStatus {
        let states: Vec<_> = self.watchers.iter().map(|(_, w)| (w.name(), w.status())).collect();
        let (state, detail) = if !self.failed.is_empty() && states.is_empty() {
            (MonitorState::Failed, self.failed.clone())
        } else if let Some((_, s)) = states.iter().find(|(_, s)| s.state == WatchState::Failed) {
            (MonitorState::Failed, s.detail.clone())
        } else if !self.failed.is_empty() || states.iter().any(|(_, s)| s.state == WatchState::Degraded) {
            let d = if self.failed.is_empty() {
                states.iter().find(|(_, s)| s.state == WatchState::Degraded).map(|(_, s)| s.detail.clone()).unwrap_or_default()
            } else {
                self.failed.clone()
            };
            (MonitorState::Degraded, d)
        } else if !states.is_empty() && states.iter().all(|(_, s)| s.state == WatchState::Running) {
            (MonitorState::Running, String::new())
        } else {
            (MonitorState::Starting, String::new())
        };
        let mut names: Vec<&str> = states.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        let queued = self.inbox.queue.lock().unwrap().len() + self.inbox.rescans.lock().unwrap().len();
        MonitorStatus {
            state,
            detail,
            watcher: names.join(","),
            watched_dirs: if states.iter().all(|(_, s)| s.watched_dirs >= 0) {
                states.iter().map(|(_, s)| s.watched_dirs).sum()
            } else {
                -1
            },
            polled_dirs: states.iter().map(|(_, s)| s.polled_dirs).sum(),
            events: self.events,
            changes: self.changes,
            overflows: self.overflows + states.iter().map(|(_, s)| s.overflows).sum::<i64>(),
            pending: (queued + self.deferred.len() + self.trees.len()) as i64,
            last_apply_ms: (self.last_apply_ms * 10.0).round() / 10.0,
        }
    }

    // ------------------------------------------------------------- the loop

    /// How long the loop may sleep before it has something to do (None: until an event).
    pub fn timeout(&self, hooks: &dyn MonitorHooks) -> Option<Duration> {
        let mut wait = if !self.trees.is_empty() {
            let since = self.last_event.map_or(QUIET_RESCAN, |t| t.elapsed());
            Some(QUIET_RESCAN.saturating_sub(since).max(Duration::from_millis(50)))
        } else if hooks.scan_running() || !self.deferred.is_empty() || !self.fed() {
            Some(SCAN_TICK)
        } else {
            None
        };
        if self.free_due {
            let since = self.free_at.map_or(FREE_REFRESH, |t| t.elapsed());
            let due = FREE_REFRESH.saturating_sub(since).max(Duration::from_millis(50));
            wait = Some(wait.map_or(due, |w| w.min(due)));
        }
        wait
    }

    /// Runs until [`ScanMonitor::stop`] (or the stop flag): waits for events, coalesces, applies.
    pub fn run(&mut self, hooks: &mut dyn MonitorHooks) {
        while !self.stop.load(Ordering::SeqCst) {
            self.inbox.wait(self.timeout(hooks));
            let first = Instant::now();
            while first.elapsed() < MAX_WAIT && !self.stop.load(Ordering::SeqCst) {
                // coalesce: wait for a quiet moment
                if !self.inbox.wait(Some(self.opts.coalesce)) {
                    break;
                }
            }
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            self.step(hooks);
        }
    }

    /// One turn of the loop: hand new folders to the watchers, apply what came in.
    pub fn step(&mut self, hooks: &mut dyn MonitorHooks) {
        let asked: Vec<PathBuf> = std::mem::take(&mut *self.inbox.rescans.lock().unwrap());
        if !asked.is_empty() {
            self.trees.extend(asked);
            self.last_event = None; // no quiet period to wait for: asked for by the user
        }
        let batch = std::mem::take(&mut *self.inbox.queue.lock().unwrap());
        self.feed();
        if hooks.mft_running() {
            // the tree arrives at once at the end: keep everything for it
            let mut q = self.inbox.queue.lock().unwrap();
            let later = std::mem::take(&mut *q);
            *q = batch;
            q.extend(later);
            return;
        }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.process(batch, hooks)));
        if r.is_err() {
            tracing::error!("applying file changes failed");
        }
        if self.free_due && self.free_at.is_none_or(|t| t.elapsed() >= FREE_REFRESH) && self.refresh_free() {
            hooks.volumes_changed(&self.roots);
        }
    }

    /// Hand folders that entered the tree to per-folder watchers (inotify, polling).
    pub fn feed(&mut self) {
        let n = self.store.len();
        if self.fed >= n {
            return;
        }
        let wanted: Vec<usize> = (0..self.watchers.len())
            .filter(|&i| matches!(self.watchers[i].1.name(), "inotify" | "poll"))
            .collect();
        let start = std::mem::replace(&mut self.fed, n);
        if wanted.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = {
            let r = self.store.read();
            let n = n.min(r.len());
            let mut ids: Vec<NodeId> = (start.max(1) as NodeId..n as NodeId)
                .filter(|&i| r.kind(i) == Kind::Dir && !r.flags(i).intersects(Flags::GONE | Flags::MOUNT))
                .collect();
            ids.sort_by_key(|&i| r.depth(i)); // shallow first
            ids.into_iter().map(|i| r.path(i)).collect()
        };
        for i in wanted {
            let (root, w) = &self.watchers[i];
            let mine: Vec<PathBuf> = paths.iter().filter(|p| p.starts_with(root)).cloned().collect();
            if !mine.is_empty() {
                w.add_dirs(mine);
            }
        }
    }

    // ------------------------------------------------------------- one batch

    /// Applies one batch of changes (and dirty subtrees once quiet).
    pub fn process(&mut self, batch: Vec<Change>, hooks: &mut dyn MonitorHooks) {
        let now = Instant::now();
        let running = hooks.scan_running();
        if !batch.is_empty() {
            self.events += batch.len() as i64;
            self.last_event = Some(now);
        }
        let mut stats: HashMap<PathBuf, Change> = HashMap::new();
        let mut lost: HashSet<PathBuf> = HashSet::new();
        let mut folders: HashSet<PathBuf> = std::mem::take(&mut self.deferred);
        let n_batch = batch.len();
        for ch in batch {
            match ch.kind {
                ChangeKind::Overflow => {
                    self.overflows += 1;
                    lost.insert(ch.path);
                }
                ChangeKind::Modified if ch.is_dir != Some(true) => {
                    stats.insert(ch.path.clone(), ch);
                }
                ChangeKind::Dir => {
                    folders.insert(ch.path);
                }
                _ => {
                    folders.insert(parent_dir(&ch.path));
                    if let Some(old) = &ch.old_path {
                        folders.insert(parent_dir(old));
                    }
                }
            }
        }
        if !lost.is_empty() {
            match self.replay_usn(&lost, hooks) {
                Some(replayed) => folders.extend(replayed),
                None => self.trees.extend(lost), // no journal to replay from: list again
            }
        }
        self.usn_tick(now);
        if folders.len() > STORM_FOLDERS || n_batch > STORM_EVENTS {
            let mut all = folders.clone();
            all.extend(stats.keys().map(|p| parent_dir(p)));
            self.trees.extend(common_roots(&all));
            hooks.message(crate::tr!("文件变化过多（{count} 条），稍后重新扫描受影响的文件夹", count = n_batch));
            folders.clear();
            stats.clear();
        }

        let t0 = Instant::now();
        let mut changes = 0i64;
        let mut flashes: HashMap<NodeId, i64> = HashMap::new();
        if !stats.is_empty() {
            let got = self.apply_stats(stats, &mut folders);
            changes += got.len() as i64;
            flashes.extend(got);
        }
        let mut targets = self.targets(&folders);
        let quiet = self.last_event.is_none_or(|t| now.duration_since(t) >= QUIET_RESCAN) || (n_batch == 0 && !self.trees.is_empty());
        let rescan = !self.trees.is_empty() && quiet && !running;
        if rescan {
            let r = self.store.read();
            let mut trees: Vec<PathBuf> = self.trees.drain().collect();
            trees.sort();
            for path in trees {
                if let (Some(node), _) = self.index.locate(&r, bytes(&path))
                    && r.kind(node) == Kind::Dir
                    && !self.index.unnamed(&r, node)
                {
                    targets.insert(node, r.path(node));
                }
            }
        }
        while !targets.is_empty() {
            let applied = self.relist(&targets, hooks, running);
            flashes.extend(applied.flashes);
            changes += applied.changes;
            self.scan_new(&applied.pending, running);
            // a subtree rescan goes on below every folder that stayed
            targets = HashMap::new();
            if rescan {
                let r = self.store.read();
                for k in applied.kept_folders {
                    if mutate::can_relist(&r, k) {
                        targets.insert(k, r.path(k));
                    }
                }
            }
        }
        if changes > 0 || !flashes.is_empty() {
            self.changes += changes;
            self.last_apply_ms = t0.elapsed().as_secs_f64() * 1000.0;
            if self.refresh_free() {
                hooks.volumes_changed(&self.roots);
            }
            hooks.applied(&flashes);
            self.maybe_compact(hooks, running);
        }
    }

    /// Folders to list again, by node: missing ones resolve to their deepest known ancestor;
    /// folders not listed yet are left to the scan; folders with children in the scan's queue
    /// wait; folders whose path could open another entry of the very same name stay as scanned.
    fn targets(&mut self, folders: &HashSet<PathBuf>) -> HashMap<NodeId, PathBuf> {
        let r = self.store.read();
        let mut out = HashMap::new();
        for path in folders {
            let (node, deepest) = self.index.locate(&r, bytes(path));
            let folder = match node {
                Some(n) if r.kind(n) == Kind::Dir => Some(n),
                _ => deepest,
            };
            let Some(folder) = folder else { continue };
            if r.kind(folder) != Kind::Dir || self.index.unnamed(&r, folder) {
                continue;
            }
            if r.flags(folder).intersects(Flags::PENDING) {
                continue; // the scan has not listed it yet: it will read it as it is now
            }
            if !mutate::can_relist(&r, folder) {
                self.deferred.insert(r.path(folder));
                continue;
            }
            out.insert(folder, r.path(folder));
        }
        out
    }

    fn root_index(&self, path: &Path) -> Option<usize> {
        self.roots.iter().position(|r| path_inside(path, &r.path))
    }

    /// Size and time changes of known files, in place; unknown paths make their folder dirty.
    fn apply_stats(&mut self, stats: HashMap<PathBuf, Change>, folders: &mut HashSet<PathBuf>) -> HashMap<NodeId, i64> {
        let mut known: Vec<(NodeId, Change)> = Vec::new();
        {
            let r = self.store.read();
            for (path, ch) in stats {
                match self.index.locate(&r, bytes(&path)).0 {
                    Some(node) if r.kind(node) == Kind::File => known.push((node, ch)),
                    _ => {
                        folders.insert(parent_dir(&path));
                    }
                }
            }
            for (node, ch) in known.iter_mut() {
                if cfg!(windows) {
                    // no allocation in the event (compressed or sparse file): its folder's listing has it
                    if ch.size.is_some() && ch.alloc.is_none() {
                        folders.insert(parent_dir(&ch.path));
                        *ch = Change::new(ChangeKind::Modified, ch.path.clone(), Some(false));
                    } else if self.opts.hard_rule
                        && let Some(size) = ch.size
                    {
                        let cloud = r.flags(*node).intersects(Flags::CLOUD);
                        let ro = self.root_index(&ch.path).and_then(|i| self.root_opts.get(i)).cloned().unwrap_or_default();
                        ch.alloc = Some(hard_alloc(size, cloud, &ro));
                    }
                } else if ch.size.is_none() {
                    // the watcher did not say: ask the file system
                    if let Ok(m) = std::fs::metadata(&ch.path) {
                        let cluster = self.roots.first().and_then(|r| r.volume.as_ref()).map_or(4096, |v| v.cluster_size.max(1));
                        #[cfg(unix)]
                        let alloc = {
                            use std::os::unix::fs::MetadataExt;
                            m.blocks() as i64 * 512
                        };
                        #[cfg(not(unix))]
                        let alloc = (m.len() as i64 + cluster - 1) / cluster * cluster;
                        let _ = cluster;
                        *ch = Change::modified(ch.path.clone(), m.len() as i64, Some(alloc), super::secs(m.modified()));
                    }
                }
            }
        }
        known.retain(|(_, c)| c.size.is_some() && c.alloc.is_some());
        if known.is_empty() {
            return HashMap::new();
        }
        let mut w = self.store.write();
        known.retain(|(n, _)| w.kind(*n) == Kind::File && !w.flags(*n).intersects(Flags::GONE));
        let ids: Vec<NodeId> = known.iter().map(|k| k.0).collect();
        let size: Vec<i64> = known.iter().map(|k| k.1.size.unwrap()).collect();
        let alloc: Vec<i64> = known.iter().map(|k| k.1.alloc.unwrap()).collect();
        let mtime: Vec<i64> = known.iter().map(|k| k.1.mtime.unwrap_or(0)).collect();
        let delta = mutate::update_stats(&mut w, &ids, &size, &alloc, Some(&mtime));
        ids.into_iter().zip(delta).filter(|(_, d)| *d != 0).collect()
    }

    fn permit(&self, running: bool) {
        if let (Some(g), true) = (&self.opts.gate, running) {
            while g.permit(Duration::from_millis(100)).is_none() {
                if self.stop.load(Ordering::SeqCst) {
                    return;
                }
            }
        }
    }

    /// Lists one folder (only it); None: vanished, or a folder a path cannot reach.
    fn list_one(&self, node: NodeId, path: &Path) -> Option<SliceResult> {
        let root = self.root_index(path)?;
        let opts = SliceOptions { roots: self.root_opts.as_ref().clone(), budget: Duration::ZERO, threads: 1, ..SliceOptions::default() };
        let task = Ref::Node(node as u64);
        let res = hardscan::scan_slice(vec![Task { target: task, path: path.to_path_buf(), root: root as u32 }], &opts).ok()?;
        if res.refs.len() != 1 || res.refs[0] != task {
            return None; // vanished (its parent lists it as gone) or no longer readable
        }
        if res.errors.iter().any(|e| e.target == task) {
            return None; // a folder a path cannot reach (a case-only sibling): stays as scanned
        }
        Some(res)
    }

    /// Lists `targets` again (in threads), then folds every listing in, deepest folder first.
    fn relist(&mut self, targets: &HashMap<NodeId, PathBuf>, hooks: &mut dyn MonitorHooks, running: bool) -> Applied {
        let mut items: Vec<(NodeId, PathBuf, u16)> = {
            let r = self.store.read();
            targets.iter().map(|(&k, p)| (k, p.clone(), r.depth(k))).collect()
        };
        items.sort_by_key(|i| std::cmp::Reverse(i.2));
        let mut listed: HashMap<NodeId, SliceResult> = HashMap::new();
        for chunk in items.chunks(LIST_BATCH) {
            self.permit(running);
            let this: &ScanMonitor = self;
            let got: Vec<(NodeId, Option<SliceResult>)> = std::thread::scope(|s| {
                let hs: Vec<_> = chunk.iter().map(|(n, p, _)| (n, s.spawn(move || this.list_one(*n, p)))).collect();
                hs.into_iter().map(|(n, h)| (*n, h.join().ok().flatten())).collect()
            });
            listed.extend(got.into_iter().filter_map(|(n, r)| r.map(|r| (n, r))));
        }
        let mut out = Applied::default();
        let mut w = self.store.write();
        let mut remap = IdRemap::default();
        for (node, path, _) in items {
            let Some(res) = listed.remove(&node) else { continue };
            if !mutate::can_relist(&w, node) || w.path(node) != path {
                continue;
            }
            let unnamed_names = unnamed(&res);
            let unreadable: HashSet<usize> = res
                .errors
                .iter()
                .filter_map(|e| match e.target {
                    Ref::Entry { listing: 0, entry } => Some(entry as usize),
                    _ => None,
                })
                .collect();
            let batch = entry_batch(&w, res.entries, &res.ext_table);
            let Ok(rel) = mutate::relist(&mut w, node, &batch) else { continue };
            // new subfolders no path can open: not scanned by their path
            let unreadable: HashSet<NodeId> = unreadable.into_iter().map(|j| rel.start + j as NodeId).collect();
            for &row in &rel.pending {
                if unreadable.contains(&row) {
                    w.mark(row, Flags::ACCESS_DENIED);
                    w.clear(row, Flags::PENDING);
                }
            }
            remap.add(&rel.moved_from, &rel.moved_to);
            out.changes += (rel.added.len() + rel.removed.len() + rel.grown.len()) as i64;
            for &i in &rel.added {
                out.flashes.insert(i, w.alloc(i));
            }
            for (&i, &d) in rel.grown.iter().zip(&rel.grown_by) {
                out.flashes.insert(i, d);
            }
            if !rel.removed.is_empty() {
                *out.flashes.entry(node).or_default() -= rel.removed_alloc.iter().sum::<i64>();
            }
            out.pending.extend(rel.pending.iter().copied().filter(|r| !unreadable.contains(r)));
            // names a folder holds twice, or with a separator, stay as scanned, with what lies below
            out.kept_folders.extend(
                rel.moved_to.iter().copied().filter(|&k| w.kind(k) == Kind::Dir && !unnamed_names.contains(w.name_bytes(k))),
            );
        }
        if !remap.is_empty() {
            let flashes = std::mem::take(&mut out.flashes);
            for (id, d) in flashes {
                if let Some(m) = remap.map(&w, id) {
                    out.flashes.insert(m, d);
                }
            }
            out.kept_folders = out.kept_folders.iter().filter_map(|&k| remap.map(&w, k)).collect();
            out.pending = out.pending.iter().filter_map(|&k| remap.map(&w, k)).collect();
            hooks.remapped(&w, &remap);
        }
        out
    }

    /// Scans folders that appeared (like the scan itself: listings in threads, fold here).
    fn scan_new(&mut self, folders: &[NodeId], running: bool) {
        let mut queue: Vec<Task> = {
            let r = self.store.read();
            folders
                .iter()
                .filter_map(|&node| {
                    let path = r.path(node);
                    self.root_index(&path).map(|i| Task { target: Ref::Node(node as u64), path, root: i as u32 })
                })
                .collect()
        };
        let opts = SliceOptions {
            roots: self.root_opts.as_ref().clone(),
            budget: Duration::from_millis(50),
            threads: 0,
            ..SliceOptions::default()
        };
        while !queue.is_empty() && !self.stop.load(Ordering::SeqCst) {
            let rest = queue.split_off(queue.len().min(LIST_BATCH));
            let batch = std::mem::replace(&mut queue, rest);
            self.permit(running);
            let Ok(res) = hardscan::scan_slice(batch, &opts) else { return };
            let mut app = self.store.appender();
            match fold_slice(&mut app, res, None) {
                Ok(f) => queue.extend(f.frontier),
                Err(e) => {
                    tracing::warn!("monitor: folding new folders failed: {e}");
                    return;
                }
            }
        }
    }

    // ------------------------------------------------------------- USN journal (Windows)

    /// `X:` of every local NTFS volume under the roots.
    fn usn_volumes(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .roots
            .iter()
            .filter_map(|r| r.volume.as_ref())
            .filter(|v| v.is_local_ntfs())
            .map(|v| v.mount_point[..2].to_uppercase())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Takes a checkpoint of each volume's change journal (when an overflow needs replaying).
    pub fn start_usn(&mut self) {
        let Some(src) = self.opts.usn.clone() else { return };
        for vol in self.usn_volumes() {
            match src.query(&vol) {
                Ok((jid, next)) => {
                    self.usn.insert(vol, (jid, next, next));
                }
                Err(e) => tracing::info!("no USN journal on {vol}: {e}"),
            }
        }
        self.usn_at = Some(Instant::now());
    }

    /// Moves the checkpoints on (the safe one to the previous newer one), every [`USN_PERIOD`].
    fn usn_tick(&mut self, now: Instant) {
        if self.usn.is_empty() || self.usn_at.is_some_and(|t| now.duration_since(t) < USN_PERIOD) {
            return;
        }
        self.usn_at = Some(now);
        let Some(src) = self.opts.usn.clone() else { return };
        let vols: Vec<String> = self.usn.keys().cloned().collect();
        for vol in vols {
            let (jid, _, newer) = self.usn[&vol];
            match src.query(&vol) {
                Err(_) => {
                    self.usn.remove(&vol);
                }
                Ok((j, next)) if j != jid => {
                    self.usn.insert(vol, (j, next, next));
                }
                Ok((_, next)) => {
                    self.usn.insert(vol, (jid, newer, next));
                }
            }
        }
    }

    /// Folders touched since the safe checkpoint of the volumes holding `lost`, from the journal;
    /// None when that is not possible (no journal, no rights, too many records).
    fn replay_usn(&mut self, lost: &HashSet<PathBuf>, hooks: &mut dyn MonitorHooks) -> Option<HashSet<PathBuf>> {
        let src = self.opts.usn.clone()?;
        let vols: HashSet<String> = lost
            .iter()
            .map(|p| {
                let s = p.to_string_lossy();
                if s.as_bytes().get(1) == Some(&b':') { s[..2].to_uppercase() } else { String::new() }
            })
            .collect();
        if vols.is_empty() || !vols.iter().all(|v| self.usn.contains_key(v)) {
            return None;
        }
        let mut frn: Vec<u64> = Vec::new();
        let mut sorted: Vec<&String> = vols.iter().collect();
        sorted.sort();
        for vol in sorted {
            let (jid, safe, _) = self.usn[vol];
            match src.read(vol, safe, jid, USN_MAX_ROWS) {
                Ok(got) if !got.truncated => frn.extend(got.parent_frn),
                Ok(_) => return None,
                Err(e) => {
                    tracing::info!("USN replay on {vol} failed: {e}");
                    return None;
                }
            }
        }
        frn.sort_unstable();
        frn.dedup();
        let nodes = self.nodes_of_frn(&frn)?;
        hooks.message(crate::tr!("变化通知溢出：按 USN 日志补齐 {count} 个文件夹", count = nodes.len()));
        let r = self.store.read();
        Some(nodes.into_iter().map(|n| r.path(n)).collect())
    }

    /// Live folders whose file id (record number) is in `frn`; None when the tree carries no file
    /// ids at all.
    fn nodes_of_frn(&mut self, frn: &[u64]) -> Option<Vec<NodeId>> {
        let r = self.store.read();
        let n = r.len();
        if self.ino.as_ref().is_none_or(|c| c.0 != n) {
            let mut pairs: Vec<(u64, NodeId)> = (0..n as NodeId)
                .filter(|&i| r.kind(i) == Kind::Dir && !r.flags(i).intersects(Flags::GONE))
                .map(|i| (r.ino(i) & REF_MASK, i))
                .filter(|p| p.0 != 0)
                .collect();
            pairs.sort_unstable();
            let (keys, ids) = pairs.into_iter().unzip();
            self.ino = Some((n, keys, ids));
        }
        let (_, keys, ids) = self.ino.as_ref().unwrap();
        if keys.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for f in frn {
            let want = f & REF_MASK;
            let lo = keys.partition_point(|&k| k < want);
            let mut i = lo;
            while i < keys.len() && keys[i] == want {
                out.push(ids[i]);
                i += 1;
            }
        }
        Some(out)
    }

    // ------------------------------------------------------------- volumes, compaction

    /// Free space moves with the changes: read it again at most once every [`FREE_REFRESH`].
    /// True when a volume's free or used space changed.
    fn refresh_free(&mut self) -> bool {
        if self.free_at.is_some_and(|t| t.elapsed() < FREE_REFRESH) {
            self.free_due = true;
            return false;
        }
        self.free_at = Some(Instant::now());
        self.free_due = false;
        let mut changed = false;
        for root in self.roots.iter_mut() {
            if let (Some(old), true) = (&root.volume, root.whole_volume)
                && let Ok(fresh) = volume::volume_for(&root.path)
            {
                changed |= (fresh.free, fresh.used()) != (old.free, old.used());
                root.volume = Some(fresh);
            }
        }
        changed
    }

    fn maybe_compact(&mut self, hooks: &mut dyn MonitorHooks, running: bool) {
        if running {
            return;
        }
        let (version, fresh) = {
            let r = self.store.read();
            if !mutate::needs_compaction(&r) {
                return;
            }
            match mutate::compact(&r) {
                Ok(x) => (r.version(), x),
                Err(e) => {
                    tracing::warn!("compaction failed: {e}");
                    return;
                }
            }
        };
        let (store, remap) = fresh;
        let mut w = self.store.write();
        if w.version() != version {
            return; // changed meanwhile: try again after the next batch
        }
        w.adopt(&store);
        hooks.compacted(&w, &remap);
        self.index.forget();
        self.ino = None;
        self.fed = w.len();
        tracing::info!("compacted after changes, {} rows", w.len());
    }
}

impl Drop for ScanMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
struct Applied {
    flashes: HashMap<NodeId, i64>,
    pending: Vec<NodeId>,
    kept_folders: Vec<NodeId>,
    changes: i64,
}
