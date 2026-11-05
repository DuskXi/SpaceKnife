//! The scans every session shares: running scans, opened snapshots, tree queries (Python
//! `server/scans.py`).
//!
//! Each scan runs on a thread of its own: the hard scan ([`Scanner::run`]) or the MFT fast scan
//! through the elevated helper, then the finish (compaction, events, the auto snapshot, the
//! volume summary, the name index). Status goes to the handle's listeners (the `scan` topic, the
//! views) from a ticker at most 4 times a second, and once more at the end.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use sk_core::events::{self, EventLevel, Source};
use sk_core::helper::{HelperClient, LaunchOptions, MftOptions, Phase, SharedHelper};
use sk_core::io::{IoGovernor, IoRegistry, Level, RegistryOptions};
use sk_core::scan::mft::{MftBuild, table_to_store};
use sk_core::scan::{RootInfo, ScanOptions, ScanProgress, ScanState, Scanner, native_threads, normalise_root};
use sk_core::settings::SettingsStore;
use sk_core::view::{TagStore, ViewContext};
use sk_core::volume::VolumeInfo;
use sk_core::{paths, tr};
use sk_protocol::ErrorCode;
use sk_protocol::methods::ScanProgress as ScanProgressEvent_;
use sk_protocol::scan::{
    self as p, NodeInfo, NodeKindName, ScanMethod, ScanMethodUsed, ScanSource, ScanStartParams, ScanStatus,
};
use sk_store::snapshot::{self, SaveOptions, SnapshotHeader};
use sk_core::summary::Walk;
use sk_store::subtree::Summary;
use sk_store::{Flags, Kind, NodeId, Nodes, Store};

use crate::app::{Config, Server};
use crate::rpc::{RpcError, RpcResult};

pub const MAX_SCANS: usize = 8;
/// A scan started with the backend waits this long for disk sampling.
pub const IO_START_WAIT: Duration = Duration::from_secs(3);
/// Status pushes while a scan runs, also when nothing lands (MFT read, paused).
pub const TICK: Duration = Duration::from_millis(250);
/// How long a closed scan's size counts (the heap keeps it for the next scan).
pub const CLOSED_KEEP: Duration = Duration::from_secs(3600);
/// Of an MFT scan's progress: reading and parsing; the rest is building.
pub const MFT_READ_SHARE: f64 = 0.85;
/// The received table plus building the tree from it, per row (docs/memory.md §6).
pub const MFT_BUILD_BYTES_PER_ROW: usize = 280;
/// Largest files in a cached summary: the details panel asks for 5 or 50.
pub const SUMMARY_TOP: usize = 50;
/// Cached summaries per scan.
pub const SUMMARY_KEEP: usize = 8;

/// A listener of a scan's status.
pub type StatusListener = Arc<dyn Fn(&ScanStatus) + Send + Sync>;

/// What changes while a scan lives.
pub struct HandleState {
    pub method: ScanMethodUsed,
    pub phase: String,
    pub fallback: String,
    pub snapshot_path: Option<String>,
    pub running: bool,
    /// share of the $MFT read so far (helper heartbeats)
    pub mft_read: Option<f64>,
    /// seconds since the helper took the request, at the last heartbeat
    pub mft_elapsed: f64,
    pub mft_at: Instant,
    /// bytes held outside the store while an MFT tree is built (memory watchdog)
    pub transient: usize,
    /// the helper reading this scan's MFT (cancel aborts it)
    pub mft_client: Option<Arc<HelperClient>>,
    /// scan.use_mft: the helper is being started
    pub upgrading: bool,
    /// scan.use_mft: a helper ready to take over the running hard scan
    pub upgrade: Option<Arc<HelperClient>>,
}

/// One scan (or opened snapshot).
pub struct ScanHandle {
    pub id: String,
    pub store: Store,
    pub ctx: Arc<ViewContext>,
    pub folder_classes: Arc<sk_core::pathclass::FolderClasses>,
    pub roots: RwLock<Vec<RootInfo>>,
    pub source: ScanSource,
    pub scanner: Option<Arc<Scanner>>,
    pub saved: Option<ScanProgress>,
    pub governor: Option<Arc<IoGovernor>>,
    pub auto_snapshot: bool,
    pub wanted: ScanMethod,
    pub ads: bool,
    /// a whole local NTFS volume (Windows): the MFT fast scan can read it
    pub mft_ok: bool,
    pub st: Mutex<HandleState>,
    listeners: Mutex<Vec<(String, StatusListener)>>,
    done: tokio::sync::watch::Sender<bool>,
    summaries: Mutex<Vec<(SummaryKey, usize, Arc<Summary>)>>,
}

type SummaryKey = (NodeId, u64, usize, usize, u64);

impl std::fmt::Debug for ScanHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanHandle").field("id", &self.id).finish_non_exhaustive()
    }
}

/// The tags file of a scan's roots (setting 「记住标签」).
pub fn tags_file(settings: &SettingsStore, roots: &[String]) -> Option<PathBuf> {
    settings
        .get_bool("tags.remember")
        .then(|| paths::data_dir().join("tags").join(format!("{}.json", snapshot::root_key(roots))))
}

fn view_roots(roots: &[RootInfo]) -> Vec<sk_core::view::RootInfo> {
    roots
        .iter()
        .map(|r| sk_core::view::RootInfo {
            node: r.node,
            whole_volume: r.whole_volume,
            free: r.volume.as_ref().map(|v| v.free),
            used: r.volume.as_ref().map(VolumeInfo::used),
        })
        .collect()
}

impl ScanHandle {
    #[allow(clippy::too_many_arguments)]
    fn new(
        settings: &SettingsStore,
        store: Store,
        roots: Vec<RootInfo>,
        source: ScanSource,
        scanner: Option<Arc<Scanner>>,
        saved: Option<ScanProgress>,
        governor: Option<Arc<IoGovernor>>,
        method: ScanMethodUsed,
    ) -> ScanHandle {
        let paths_: Vec<String> = roots.iter().map(|r| r.path.to_string_lossy().into_owned()).collect();
        let tags = TagStore::new(tags_file(settings, &paths_));
        let folder_classes = Arc::new(sk_core::pathclass::FolderClasses::new());
        let basis = sk_core::layout::Basis::default().with_folder_classes(folder_classes.clone());
        let ctx = ViewContext::with_basis(store.clone(), view_roots(&roots), tags, Arc::new(basis));
        let (done, _) = tokio::sync::watch::channel(scanner.is_none());
        ScanHandle {
            id: format!("scan-{}", crate::util::random_hex(4)),
            store,
            ctx,
            folder_classes,
            roots: RwLock::new(roots),
            source,
            scanner,
            saved,
            governor,
            auto_snapshot: true,
            wanted: ScanMethod::Hard,
            ads: false,
            mft_ok: false,
            st: Mutex::new(HandleState {
                method,
                phase: String::new(),
                fallback: String::new(),
                snapshot_path: None,
                running: false,
                mft_read: None,
                mft_elapsed: 0.0,
                mft_at: Instant::now(),
                transient: 0,
                mft_client: None,
                upgrading: false,
                upgrade: None,
            }),
            listeners: Mutex::new(Vec::new()),
            done,
            summaries: Mutex::new(Vec::new()),
        }
    }

    pub fn running(&self) -> bool {
        self.st.lock().running
    }

    pub fn root_paths(&self) -> Vec<String> {
        self.roots.read().iter().map(|r| r.path.to_string_lossy().into_owned()).collect()
    }

    pub fn progress(&self) -> ScanProgress {
        match (&self.scanner, &self.saved) {
            (Some(s), _) => s.progress(),
            (None, Some(p)) => p.clone(),
            (None, None) => unreachable!("a scan handle has a scanner or a saved progress"),
        }
    }

    /// (share done, seconds left) of a running MFT scan, from how much of the $MFT was read.
    fn mft_progress(st: &HandleState) -> Option<(f64, Option<f64>)> {
        if st.method != ScanMethodUsed::Mft || !st.running || !matches!(st.phase.as_str(), "mft" | "receive" | "build") {
            return None;
        }
        let done = if st.phase == "mft" {
            MFT_READ_SHARE * st.mft_read?
        } else {
            MFT_READ_SHARE + (1.0 - MFT_READ_SHARE) * if st.phase == "receive" { 0.3 } else { 0.5 }
        };
        let elapsed = st.mft_elapsed + st.mft_at.elapsed().as_secs_f64();
        let left = (done >= 0.02 && elapsed > 0.5).then(|| elapsed * (1.0 - done) / done);
        Some((done, left))
    }

    pub fn status(&self, server: &Server) -> ScanStatus {
        let pr = self.progress();
        let roots = self.roots.read().clone();
        let mut status = ScanStatus {
            scan_id: self.id.clone(),
            state: pr.state,
            roots: roots.iter().map(RootInfo::to_protocol).collect(),
            items: 0,
            files: 0,
            dirs: 0,
            bytes_alloc: 0,
            bytes_size: 0,
            errors: 0,
            pending_dirs: 0,
            current_path: String::new(),
            elapsed_s: 0.0,
            rate: 0.0,
            byte_rate: 0.0,
            progress: None,
            remaining_s: None,
            expected_items: None,
            started_at: None,
            finished_at: None,
            denied: Vec::new(),
            message: String::new(),
            free: None,
            unknown: None,
            snapshot: None,
            source: self.source,
            method: ScanMethodUsed::Hard,
            phase: String::new(),
            fallback: String::new(),
            mft_available: self.mft_ok,
            paused: false,
            io: None,
            monitor: None,
        };
        pr.apply_to(&mut status);
        {
            let st = self.st.lock();
            if let Some((done, left)) = Self::mft_progress(&st) {
                status.progress = Some(done);
                status.remaining_s = left;
            }
            status.snapshot = st.snapshot_path.clone();
            status.method = if self.source == ScanSource::Snapshot { ScanMethodUsed::Snapshot } else { st.method };
            status.phase = st.phase.clone();
            status.fallback = st.fallback.clone();
            if let Some(g) = &self.governor
                && st.running
            {
                status.io = Some(g.status(true));
            }
        }
        let whole: Vec<&RootInfo> = roots.iter().filter(|r| r.whole_volume && r.volume.is_some()).collect();
        if !whole.is_empty() && whole.len() == roots.len() {
            let free: i64 = whole.iter().filter_map(|r| r.volume.as_ref()).map(|v| v.free).sum();
            let used: i64 = whole.iter().filter_map(|r| r.volume.as_ref()).map(VolumeInfo::used).sum();
            let scanned = self.store.read().tot_alloc(0);
            status.free = Some(free);
            status.unknown = Some((used - scanned).max(0));
        }
        status.monitor = server.monitors.status(&self.id);
        status
    }

    /// Tell the listeners (the `scan` topic, the views) the current status.
    pub fn publish(&self, server: &Server) {
        let listeners: Vec<StatusListener> = self.listeners.lock().iter().map(|(_, l)| l.clone()).collect();
        if listeners.is_empty() {
            return;
        }
        let status = self.status(server);
        for l in listeners {
            // one subscriber must never fail the scan or starve the others
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| l(&status))).is_err() {
                tracing::error!("a status listener of scan {} failed", self.id);
            }
        }
    }

    pub fn add_listener(&self, key: &str, l: StatusListener) {
        let mut g = self.listeners.lock();
        g.retain(|(k, _)| k != key);
        g.push((key.to_owned(), l));
    }

    pub fn remove_listener(&self, key: &str) {
        self.listeners.lock().retain(|(k, _)| k != key);
    }

    /// Resolves once the scan's thread finished (at once for snapshots).
    pub async fn finished(&self) {
        let mut rx = self.done.subscribe();
        let _ = rx.wait_for(|d| *d).await;
    }

    fn set_done(&self, done: bool) {
        self.done.send_replace(done);
    }

    /// The node exists.
    pub fn check_node(&self, node: NodeId) -> RpcResult<()> {
        if (node as usize) >= self.store.len() {
            return Err(RpcError::new(ErrorCode::UnknownNode, format!("node {node} does not exist in {}", self.id)));
        }
        Ok(())
    }

    /// The scan root holding `node`.
    pub fn root_of(&self, nodes: &Nodes, node: NodeId) -> Option<RootInfo> {
        let roots = self.roots.read();
        let chain: Vec<NodeId> = std::iter::once(node).chain(nodes.ancestors(node)).collect();
        roots.iter().find(|r| chain.contains(&r.node)).cloned()
    }

    /// `subtree::summarize` below `node` with the scan's classes, at most once per store version
    /// (the details panel, the Disks page and the end of a scan ask for the same summary).
    /// Calls for one scan run one at a time; `cache` false while the scan still writes.
    /// A walk that needed the fallback by extension, or failed, is not cached
    /// ([`sk_core::summary::summarize_node`]).
    pub fn summary(&self, node: NodeId, top: usize, cache: bool) -> Walk {
        let mut g = self.summaries.lock();
        let nodes = self.store.read();
        let map = self.ctx.basis.class_map();
        let key: SummaryKey =
            (node, nodes.version(), nodes.len(), Arc::as_ptr(&map) as usize, sk_core::pathclass::folders_version());
        if cache
            && let Some(i) = g.iter().position(|(k, t, _)| *k == key && *t >= top)
        {
            let hit = g.remove(i);
            let s = hit.2.clone();
            g.push(hit);
            return Walk::Full(s);
        }
        let want = top.max(SUMMARY_TOP);
        let walk = sk_core::summary::summarize_node(&self.ctx.basis, &nodes, node, want);
        g.retain(|(k, _, _)| k.1 == key.1 && k.2 == key.2 && k.3 == key.3 && k.4 == key.4);
        if cache && let Walk::Full(s) = &walk {
            g.push((key, want, s.clone()));
            while g.len() > SUMMARY_KEEP {
                g.remove(0);
            }
        }
        walk
    }
}

// ------------------------------------------------------------------------------- node info

fn flag_names(flags: Flags) -> Vec<String> {
    const NAMES: [(Flags, &str); 20] = [
        (Flags::ACCESS_DENIED, "access_denied"),
        (Flags::MOUNT, "mount"),
        (Flags::HARDLINK_DUP, "hardlink_dup"),
        (Flags::COMPRESSED, "compressed"),
        (Flags::SPARSE, "sparse"),
        (Flags::HIDDEN, "hidden"),
        (Flags::SYSTEM, "system"),
        (Flags::READONLY, "readonly"),
        (Flags::REPARSE, "reparse"),
        (Flags::CLOUD, "cloud"),
        (Flags::ERROR, "error"),
        (Flags::PENDING, "pending"),
        (Flags::ARCHIVE, "archive"),
        (Flags::ENCRYPTED, "encrypted"),
        (Flags::TEMPORARY, "temporary"),
        (Flags::NOT_INDEXED, "not_indexed"),
        (Flags::OFFLINE, "offline"),
        (Flags::VIRTUAL, "virtual"),
        (Flags::ADS, "ads"),
        (Flags::GONE, "gone"),
    ];
    NAMES.iter().filter(|(f, _)| flags.contains(*f)).map(|(_, n)| (*n).to_owned()).collect()
}

/// `NodeInfo` of a node.
pub fn node_info(nodes: &Nodes, node: NodeId) -> NodeInfo {
    let (size, alloc) = nodes.effective_size(node);
    let kind = nodes.kind(node);
    let is_dir = kind == Kind::Dir;
    NodeInfo {
        id: node,
        parent: i64::from(nodes.parent(node)),
        name: nodes.name(node).into_owned(),
        kind: match kind {
            Kind::File => NodeKindName::File,
            Kind::Dir => NodeKindName::Dir,
            Kind::Link => NodeKindName::Link,
            Kind::Other => NodeKindName::Other,
        },
        depth: i64::from(nodes.depth(node)),
        size,
        alloc,
        files: if is_dir { nodes.tot_files(node) as i64 } else { 0 },
        dirs: if is_dir { nodes.tot_dirs(node) as i64 } else { 0 },
        child_count: i64::from(nodes.child_count(node)),
        mtime: i64::from(nodes.mtime(node)),
        ctime: i64::from(nodes.ctime(node)),
        atime: i64::from(nodes.atime(node)),
        ext: nodes.ext_name(nodes.ext(node)).to_string(),
        flags: flag_names(nodes.flags(node)),
    }
}

/// Whether the node's path exists: not under a pseudo folder of an MFT scan.
pub fn on_disk(nodes: &Nodes, node: NodeId) -> bool {
    std::iter::once(node)
        .chain(nodes.ancestors(node))
        .filter(|&n| n > 0)
        .all(|n| !nodes.flags(n).intersects(Flags::VIRTUAL))
}

// ------------------------------------------------------------------------------- the manager

/// Scans held by the backend.
pub struct ScanManager {
    scans: Mutex<HashMap<String, Arc<ScanHandle>>>,
    order: Mutex<Vec<String>>,
    pub settings: Arc<SettingsStore>,
    pub io: Arc<IoRegistry>,
    /// one read-only scan helper per backend (MFT; asks for administrator rights once)
    pub helper: Arc<SharedHelper>,
    io_ready: tokio::sync::watch::Sender<bool>,
    io_starting: AtomicBool,
    low_io: AtomicUsize,
    /// (when, bytes) of the scans closed lately (the memory watchdog allows for them)
    pub closed: Mutex<Vec<(Instant, usize)>>,
}

impl ScanManager {
    pub fn new(_config: &Config, settings: &Arc<SettingsStore>) -> ScanManager {
        // the governor counts in folders listed at once (the walker's gate), up to its widest width
        let opts = RegistryOptions { max_workers: sk_core::scan::listing_max(), ..RegistryOptions::default() };
        let (io_ready, _) = tokio::sync::watch::channel(false);
        ScanManager {
            scans: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            settings: settings.clone(),
            io: Arc::new(IoRegistry::new(opts)),
            helper: Arc::new(SharedHelper::new(LaunchOptions::default())),
            io_ready,
            io_starting: AtomicBool::new(false),
            low_io: AtomicUsize::new(0),
            closed: Mutex::new(Vec::new()),
        }
    }

    /// Start disk-load sampling (at backend start: the W2 baseline is measured before a scan).
    pub fn start_io(&self, server: &Arc<Server>) {
        self.io_starting.store(true, Ordering::SeqCst);
        let io = self.io.clone();
        let ready = self.io_ready.clone();
        server.rt.spawn_blocking(move || {
            io.start();
            ready.send_replace(true);
        });
    }

    pub fn list(&self) -> Vec<Arc<ScanHandle>> {
        let scans = self.scans.lock();
        self.order.lock().iter().filter_map(|id| scans.get(id).cloned()).collect()
    }

    pub fn lookup(&self, id: &str) -> Option<Arc<ScanHandle>> {
        self.scans.lock().get(id).cloned()
    }

    pub fn get(&self, id: &str) -> RpcResult<Arc<ScanHandle>> {
        self.lookup(id).ok_or_else(|| RpcError::new(ErrorCode::UnknownScan, format!("unknown scan: {id}")))
    }

    fn insert(&self, h: Arc<ScanHandle>) {
        self.order.lock().push(h.id.clone());
        self.scans.lock().insert(h.id.clone(), h);
    }

    fn remove(&self, id: &str) -> Option<Arc<ScanHandle>> {
        self.order.lock().retain(|x| x != id);
        self.scans.lock().remove(id)
    }

    /// Evict idle scans (finished longest ago first) until there is room for one more.
    fn make_room(&self, server: &Arc<Server>) -> RpcResult<()> {
        loop {
            let all = self.list();
            if all.len() < MAX_SCANS {
                return Ok(());
            }
            let oldest = all
                .iter()
                .filter(|h| !h.running() && !server.views.has_views(&h.id))
                .min_by_key(|h| h.progress().finished_at.map_or(i64::MAX, |t| t.timestamp_millis()))
                .cloned();
            let Some(oldest) = oldest else {
                return Err(RpcError::new(
                    ErrorCode::ScanBusy,
                    format!("too many scans in memory (max {MAX_SCANS}); close one first"),
                ));
            };
            server.views.release(&oldest.id);
            self.remove(&oldest.id);
            server.monitors.stop(&oldest.id);
            tracing::info!("evicted scan {} to free memory", oldest.id);
        }
    }

    // ------------------------------------------------------------------ scans

    pub async fn start(&self, server: &Arc<Server>, params: ScanStartParams) -> RpcResult<Arc<ScanHandle>> {
        self.make_room(server)?;
        let st = self.settings.clone();
        let mut workers = params.workers;
        if workers.is_none() && st.get_str("scan.concurrency") == "manual" {
            workers = Some(st.get_int("scan.workers").max(0) as u32);
        }
        let cross = params.cross_mounts.unwrap_or_else(|| st.get_bool("scan.cross_mounts"));
        let auto = params.auto_snapshot.unwrap_or_else(|| st.get_bool("advanced.auto_snapshot"));
        let manual = workers.is_some_and(|w| w > 0);
        let threads = native_threads();
        let (gated, concurrency) = match workers {
            Some(0) => (false, 1), // one listing at a time, no governor (tests, tiny scans)
            Some(w) => (true, (w as usize).min(threads)),
            None => (true, threads),
        };
        let links_each = st.get_str("scan.hardlinks") == "each";
        let skip_protected = params.skip_protected.unwrap_or_else(|| {
            st.get_str("scan.protected_places") == "skip" && sk_core::privacy::full_disk_access(None) == Some(false)
        });
        // a scan right after the backend started waits a moment for disk sampling
        if gated && self.io_starting.load(Ordering::SeqCst) && !*self.io_ready.borrow() {
            let mut rx = self.io_ready.subscribe();
            let _ = tokio::time::timeout(IO_START_WAIT, rx.wait_for(|r| *r)).await;
        }
        let governor = if gated && self.io.running() && !params.roots.is_empty() {
            let io = self.io.clone();
            let first = normalise_root(&params.roots[0]);
            let gov = crate::compute::blocking(move || io.governor_for_path(&first)).await?;
            let level = params
                .load_level
                .or_else(|| Level::from_str(&st.get_str("scan.load_manual")))
                .unwrap_or(Level::Balanced);
            let custom = params.load_custom.map(f64::from).or_else(|| Some(st.get_int("scan.custom_limit") as f64));
            // in folders listed at once (the walker's gate, docs/native.md 4.2), up to its widest width
            let most = sk_core::scan::listing_max();
            gov.set_max_workers(most);
            gov.set_level(level, custom);
            gov.set_yield_when_busy(st.get_bool("scan.yield_when_busy"));
            gov.set_manual_concurrency(workers.filter(|_| manual).map(|w| (w as usize).clamp(1, most)));
            Some(gov)
        } else {
            None
        };
        let opts = ScanOptions {
            concurrency,
            cross_mounts: cross,
            count_links_each: links_each,
            protect: if skip_protected { sk_core::privacy::protected_folders(None) } else { Vec::new() },
            gate: governor.clone().map(|g| Arc::new(g) as Arc<dyn sk_core::scan::IoGate>),
            ..ScanOptions::default()
        };
        let roots_in = params.roots.clone();
        let scanner = crate::compute::blocking(move || Scanner::new(&roots_in, opts))
            .await?
            .map_err(|e| RpcError::new(ErrorCode::ScanFailed, e.to_string()))?;
        let scanner = Arc::new(scanner);
        let roots = scanner.roots();
        {
            // the expected total (progress by items, room reserved up front) and the media probes
            // (they may block: warm their cache before status() needs them)
            let sc = scanner.clone();
            let paths_: Vec<String> = roots.iter().map(|r| r.path.to_string_lossy().into_owned()).collect();
            let vols: Vec<VolumeInfo> = roots.iter().filter_map(|r| r.volume.clone()).collect();
            crate::compute::blocking(move || {
                sc.set_expected_items(last_item_count(&paths_));
                for v in vols {
                    let _ = v.to_protocol();
                }
            })
            .await?;
        }
        let wanted = params
            .method
            .or_else(|| ScanMethod::from_str(&st.get_str("scan.method")))
            .unwrap_or(ScanMethod::Auto);
        let mut handle = ScanHandle::new(
            &st,
            scanner.store().clone(),
            roots,
            ScanSource::Scan,
            Some(scanner),
            None,
            governor,
            ScanMethodUsed::Hard,
        );
        handle.auto_snapshot = auto;
        handle.wanted = wanted;
        handle.ads = params.ads.unwrap_or_else(|| st.get_bool("scan.ads"));
        handle.mft_ok = mft_eligible(&handle.roots.read());
        handle.st.lock().running = true;
        handle.set_done(false);
        let handle = Arc::new(handle);
        self.insert(handle.clone());
        // changes are watched from the start (UI-W2-09)
        server.monitors.start(server, &handle);
        spawn_ticker(server, &handle);
        let (srv, h) = (server.clone(), handle.clone());
        std::thread::Builder::new()
            .name(format!("scan-{}", handle.id))
            .spawn(move || run_scan(&srv, &h))
            .map_err(|e| RpcError::new(ErrorCode::ScanFailed, e.to_string()))?;
        tracing::info!("scan {} started: {}", handle.id, handle.root_paths().join(", "));
        Ok(handle)
    }

    /// An MFT scan needs no administrator prompt: rights already, or a live elevated helper.
    pub fn mft_ready(&self) -> bool {
        sk_core::helper::is_elevated() || self.helper.current().is_some_and(|c| c.alive() && c.elevated())
    }

    fn mft_wanted(&self, h: &ScanHandle) -> bool {
        if h.wanted == ScanMethod::Hard || !h.mft_ok {
            return false;
        }
        self.settings.get_bool("scan.elevate") || self.mft_ready()
    }

    /// Go on with a stopped hard scan (W2 待定 16): its queue still holds the folders it had not
    /// read; the tree grows on where it stopped, then it is saved like any finished scan.
    pub fn continue_scan(&self, server: &Arc<Server>, id: &str) -> RpcResult<Arc<ScanHandle>> {
        let h = self.get(id)?;
        if h.running() {
            return Ok(h);
        }
        let ok = h.source == ScanSource::Scan
            && h.st.lock().method == ScanMethodUsed::Hard
            && h.scanner.as_ref().is_some_and(|s| s.has_queue());
        if !ok {
            return Err(RpcError::invalid_request(tr!("这次扫描没有可以继续的部分")));
        }
        let scanner = h.scanner.clone().expect("checked");
        scanner.uncancel();
        h.st.lock().running = true;
        h.set_done(false);
        spawn_ticker(server, &h);
        let (srv, hh) = (server.clone(), h.clone());
        std::thread::Builder::new()
            .name(format!("scan-{}", h.id))
            .spawn(move || {
                run_hard(&srv, &hh, true);
                end_run(&srv, &hh);
            })
            .map_err(|e| RpcError::internal(e.to_string()))?;
        Ok(h)
    }

    /// Switch a running hard scan to the MFT fast scan (W2-24): the hard scan goes on while the
    /// helper starts (the administrator prompt), and is replaced once it is ready (W2-25).
    pub fn use_mft(&self, server: &Arc<Server>, id: &str) -> RpcResult<Arc<ScanHandle>> {
        let h = self.get(id)?;
        if !h.mft_ok {
            return Err(RpcError::invalid_request(tr!("只有整个本地 NTFS 卷能用 MFT 快速扫描")));
        }
        {
            let mut st = h.st.lock();
            if !st.running || st.method == ScanMethodUsed::Mft || st.upgrading {
                return Ok(h.clone());
            }
            st.upgrading = true;
            st.phase = "helper".into();
            st.fallback.clear();
        }
        h.publish(server);
        let (srv, hh) = (server.clone(), h.clone());
        let helper = self.helper.clone();
        std::thread::spawn(move || {
            match helper.get(true) {
                Err(e) => {
                    hh.st.lock().fallback = tr!("{reason}，继续硬扫描", reason = e.message.clone());
                    events::emit(
                        Source::Scan,
                        &tr!("未能启用 MFT 快速扫描：{reason}；继续硬扫描", reason = e.message),
                        EventLevel::Warning,
                        Some(&hh.id),
                        None,
                    );
                }
                Ok(client) => {
                    let mut st = hh.st.lock();
                    let scanner = hh.scanner.as_ref();
                    if st.running && st.method == ScanMethodUsed::Hard && scanner.is_some_and(|s| !s.cancelled()) {
                        st.upgrade = Some(client);
                        drop(st);
                        if let Some(s) = scanner {
                            s.cancel(); // the hard scan stops; run_hard switches over
                        }
                    }
                }
            }
            {
                let mut st = hh.st.lock();
                st.upgrading = false;
                if st.phase == "helper" {
                    st.phase.clear();
                }
            }
            hh.publish(&srv);
        });
        Ok(h)
    }

    pub fn cancel(&self, id: &str) -> RpcResult<Arc<ScanHandle>> {
        let h = self.get(id)?;
        let client = {
            let mut st = h.st.lock();
            st.upgrade = None;
            st.mft_client.clone()
        };
        if let Some(s) = &h.scanner {
            s.cancel();
        }
        if let Some(c) = client {
            c.abort(); // a running $MFT read fails at once
        }
        Ok(h)
    }

    pub fn pause(&self, server: &Server, id: &str) -> RpcResult<Arc<ScanHandle>> {
        let h = self.get(id)?;
        if let Some(s) = &h.scanner
            && h.st.lock().method == ScanMethodUsed::Hard
        {
            s.pause();
            h.publish(server);
        }
        Ok(h)
    }

    pub fn resume(&self, server: &Server, id: &str) -> RpcResult<Arc<ScanHandle>> {
        let h = self.get(id)?;
        if let Some(s) = &h.scanner {
            s.resume();
            h.publish(server);
        }
        Ok(h)
    }

    pub fn set_load(&self, server: &Server, params: &p::ScanSetLoadParams) -> RpcResult<Arc<ScanHandle>> {
        let h = self.get(&params.scan_id)?;
        let Some(gov) = &h.governor else {
            return Err(RpcError::invalid_request(tr!("这次扫描没有磁盘负载控制")));
        };
        gov.set_level(params.level, params.custom.map(f64::from));
        gov.set_manual_concurrency(params.concurrency.map(|c| c as usize));
        h.publish(server);
        Ok(h)
    }

    /// Forget a scan: stop it, wait for its thread, free its memory.
    pub async fn close_scan(&self, server: &Arc<Server>, id: &str) -> RpcResult<()> {
        let h = self.get(id)?;
        server.monitors.stop(id);
        let _ = self.cancel(id);
        h.finished().await;
        server.summaries.keep(server, &h); // what changed while it was watched
        self.remove(id);
        let bytes = h.store.ram_bytes();
        {
            let mut closed = self.closed.lock();
            closed.retain(|(t, _)| t.elapsed() < CLOSED_KEEP);
            closed.push((Instant::now(), bytes));
        }
        drop(h);
        // once the request that closed it let go of the handle too, the heap can give back
        server.rt.spawn(async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let _ = tokio::task::spawn_blocking(sk_core::memory::release).await;
        });
        Ok(())
    }

    /// Stop everything (server shutdown).
    pub async fn close(&self, server: &Arc<Server>) {
        server.monitors.close();
        let all = self.list();
        for h in &all {
            let _ = self.cancel(&h.id);
        }
        for h in &all {
            let _ = tokio::time::timeout(Duration::from_secs(5), h.finished()).await;
        }
        let helper = self.helper.clone();
        let io = self.io.clone();
        let _ = tokio::task::spawn_blocking(move || {
            helper.close();
            io.stop();
        })
        .await;
    }

    // ------------------------------------------------------------------ snapshots

    pub async fn save_snapshot(&self, id: &str, target: Option<String>) -> RpcResult<p::SnapshotInfo> {
        let h = self.get(id)?;
        if h.running() {
            return Err(RpcError::new(ErrorCode::ScanBusy, tr!("扫描完成（或停止）后才能保存快照")));
        }
        let path = target.map(PathBuf::from).unwrap_or_else(|| auto_path(&h.root_paths()));
        let (hh, pp) = (h.clone(), path.clone());
        let header = crate::compute::blocking(move || save(&hh, &pp)).await?.map_err(|e| {
            RpcError::new(ErrorCode::SnapshotError, tr!("写不了 {path}：{reason}", path = path.display().to_string(), reason = e))
        })?;
        h.st.lock().snapshot_path = Some(path.display().to_string());
        events::emit(
            Source::Snapshot,
            &tr!("快照已保存：{path}", path = path.display().to_string()),
            EventLevel::Info,
            Some(&h.id),
            None,
        );
        Ok(snapshot_info(&path, &header))
    }

    pub async fn load_snapshot(&self, server: &Arc<Server>, target: &str) -> RpcResult<Arc<ScanHandle>> {
        self.make_room(server)?;
        let path = PathBuf::from(target);
        let pp = path.clone();
        let (store, header) = crate::compute::blocking(move || snapshot::load(&pp)).await?.map_err(|e| {
            RpcError::new(ErrorCode::SnapshotError, tr!("打不开快照 {path}：{error}", path = target, error = e.to_string()))
        })?;
        let stats = &header.stats;
        let root_meta: HashMap<String, bool> = stats
            .get("roots")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        Some((m.get("path")?.as_str()?.to_owned(), m.get("whole_volume").and_then(|w| w.as_bool()).unwrap_or(false)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (roots, saved) = {
            let nodes = store.read();
            let mut roots = Vec::new();
            for (idx, node) in nodes.roots().enumerate() {
                let path = nodes.name(node).into_owned();
                let volume = header
                    .volumes
                    .get(idx)
                    .filter(|v| v.as_object().is_some_and(|o| !o.is_empty()))
                    .map(crate::util::volume_from_json);
                roots.push(RootInfo {
                    whole_volume: root_meta.get(&path).copied().unwrap_or(false),
                    path: PathBuf::from(path),
                    node,
                    volume,
                });
            }
            let state = saved_state(stats);
            let parse = |k: &str| {
                stats
                    .get(k)
                    .and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&chrono::Utc))
            };
            let created = chrono::DateTime::from_timestamp(header.created_at as i64, 0);
            let saved = ScanProgress {
                state,
                items: nodes.len() as i64 - 1,
                files: nodes.tot_files(0) as i64,
                dirs: nodes.tot_dirs(0) as i64,
                bytes_alloc: nodes.tot_alloc(0),
                bytes_size: nodes.tot_size(0),
                errors: stats.get("errors").and_then(|v| v.as_i64()).unwrap_or(0),
                pending_dirs: 0,
                current_path: String::new(),
                elapsed_s: stats.get("elapsed_s").and_then(|v| v.as_f64()).unwrap_or(0.0),
                rate: 0.0,
                progress: (state == ScanState::Done).then_some(1.0),
                started_at: parse("started_at"),
                finished_at: parse("finished_at").or(created),
                denied: stats
                    .get("denied")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|d| d.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default(),
                message: String::new(),
                byte_rate: 0.0,
                remaining_s: None,
                expected_items: None,
                paused: false,
            };
            (roots, saved)
        };
        let method = match saved_method(stats) {
            Some(p::TreeMethod::Mft) => ScanMethodUsed::Mft,
            _ => ScanMethodUsed::Hard,
        };
        let h = ScanHandle::new(&self.settings, store, roots, ScanSource::Snapshot, None, Some(saved), None, method);
        h.st.lock().snapshot_path = Some(target.to_owned());
        let h = Arc::new(h);
        self.insert(h.clone());
        tracing::info!("opened snapshot {target} as {}", h.id);
        events::emit(
            Source::Snapshot,
            &tr!("已打开快照 {path}（{count} 项）", path = target, count = sk_core::fmt::count(header.node_count as i64 - 1)),
            EventLevel::Info,
            Some(&h.id),
            None,
        );
        Ok(h)
    }
}

/// One whole local NTFS volume on Windows: the $MFT holds exactly this tree.
fn mft_eligible(roots: &[RootInfo]) -> bool {
    if !cfg!(windows) || roots.len() != 1 {
        return false;
    }
    let r = &roots[0];
    r.whole_volume && r.volume.as_ref().is_some_and(|v| !v.is_remote && v.fs_type.eq_ignore_ascii_case("NTFS"))
}

/// The auto-snapshot file of `roots` now.
pub fn auto_path(roots: &[String]) -> PathBuf {
    snapshot::auto_path(roots, None, &paths::snapshots_dir())
}

fn stats_of(h: &ScanHandle) -> serde_json::Map<String, serde_json::Value> {
    let pr = h.progress();
    let method = if h.st.lock().method == ScanMethodUsed::Mft { "mft" } else { "hard" };
    let v = serde_json::json!({
        "items": pr.items,
        "files": pr.files,
        "dirs": pr.dirs,
        "bytes_alloc": pr.bytes_alloc,
        "bytes_size": pr.bytes_size,
        "errors": pr.errors,
        "elapsed_s": pr.elapsed_s,
        "started_at": pr.started_at.map(|t| t.to_rfc3339()),
        "finished_at": pr.finished_at.map(|t| t.to_rfc3339()),
        "denied": pr.denied,
        "state": pr.state.as_str(),
        "roots": h.roots.read().iter().map(|r| serde_json::json!({
            "path": r.path.to_string_lossy(), "whole_volume": r.whole_volume
        })).collect::<Vec<_>>(),
        "method": method,
    });
    match v {
        serde_json::Value::Object(m) => m,
        _ => unreachable!(),
    }
}

/// Write a scan to a snapshot file.
pub fn save(h: &ScanHandle, path: &Path) -> Result<SnapshotHeader, String> {
    let roots = h.roots.read().clone();
    let opts = SaveOptions {
        roots: roots.iter().map(|r| r.path.to_string_lossy().into_owned()).collect(),
        volumes: roots
            .iter()
            .map(|r| r.volume.as_ref().map_or(serde_json::json!({}), crate::util::volume_to_json))
            .collect(),
        stats: stats_of(h),
        // the CPUs this process may use (affinity, cgroup quota), not the machine's
        threads: sk_native::cpus::usable().min(snapshot::SAVE_THREADS),
        ..SaveOptions::default()
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| crate::util::os_message(&e))?;
    }
    let nodes = h.store.read();
    snapshot::save(&nodes, path, &opts).map_err(|e| match e {
        snapshot::SnapshotError::Io(io) => crate::util::os_message(&io),
        other => other.to_string(),
    })
}

/// State a snapshot was saved in; snapshots that lack it were finished scans.
pub fn saved_state(stats: &serde_json::Map<String, serde_json::Value>) -> ScanState {
    let state = stats.get("state").and_then(|v| v.as_str()).and_then(ScanState::from_str).unwrap_or(ScanState::Done);
    match state {
        ScanState::Queued | ScanState::Scanning => ScanState::Cancelled,
        s => s,
    }
}

pub fn saved_method(stats: &serde_json::Map<String, serde_json::Value>) -> Option<p::TreeMethod> {
    stats.get("method").and_then(|v| v.as_str()).and_then(p::TreeMethod::from_str)
}

/// Items of the newest complete snapshot of these roots: the expected total of a new scan.
pub fn last_item_count(roots: &[String]) -> Option<i64> {
    let folder = paths::snapshots_dir().join(snapshot::root_key(roots));
    let mut found: Vec<PathBuf> = std::fs::read_dir(&folder)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "sks"))
        .collect();
    found.sort();
    found.reverse();
    for file in found {
        let Ok(header) = snapshot::read_header(&file) else { continue };
        if saved_state(&header.stats) == ScanState::Done {
            return Some((header.node_count as i64 - 1).max(0));
        }
    }
    None
}

pub fn snapshot_info(path: &Path, header: &SnapshotHeader) -> p::SnapshotInfo {
    let size = std::fs::metadata(path).map_or(0, |m| m.len() as i64);
    p::SnapshotInfo {
        path: path.display().to_string(),
        created_at: chrono::DateTime::from_timestamp(header.created_at as i64, 0).unwrap_or_default(),
        roots: header.roots.clone(),
        node_count: header.node_count as i64,
        bytes_alloc: header.stats.get("bytes_alloc").and_then(|v| v.as_i64()).unwrap_or(0),
        files: header.stats.get("files").and_then(|v| v.as_i64()).unwrap_or(0),
        file_bytes: size,
        method: saved_method(&header.stats),
    }
}

/// The $MFT reader's busy share for the scan's load level (a short sequential burst).
pub fn mft_duty(gov: Option<&IoGovernor>) -> f64 {
    match gov {
        None => 1.0,
        Some(g) => match g.level() {
            Level::Max => 1.0,
            Level::Idle => 0.2,
            _ => (g.limit() / 100.0).clamp(0.05, 1.0),
        },
    }
}

// ------------------------------------------------------------------------------- the scan thread

/// Publishes the status 4 times a second while the scan runs (also while nothing lands: MFT read,
/// paused), and once when it ends.
fn spawn_ticker(server: &Arc<Server>, h: &Arc<ScanHandle>) {
    let (weak_srv, weak_h) = (Arc::downgrade(server), Arc::downgrade(h));
    server.rt.spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let (Some(srv), Some(h)) = (weak_srv.upgrade(), weak_h.upgrade()) else { return };
            if !h.running() {
                return;
            }
            let _ = tokio::task::spawn_blocking(move || h.publish(&srv)).await;
        }
    });
}

fn run_scan(server: &Arc<Server>, h: &Arc<ScanHandle>) {
    let low = lower_io(server, h);
    let mft_done = server.scans.mft_wanted(h) && run_mft(server, h, None);
    if mft_done {
        finish(server, h, Some(0));
    } else {
        run_hard(server, h, false);
    }
    if low {
        restore_io(server);
    }
    end_run(server, h);
}

fn end_run(_server: &Arc<Server>, h: &Arc<ScanHandle>) {
    h.st.lock().running = false;
    h.set_done(true);
    // glibc: the scan's freed temporaries go back to the system (docs/memory.md §8); this is the
    // scan's own thread, nothing waits for it
    sk_core::memory::trim();
}

/// The idle level reads at the OS's low I/O priority (setting 「空闲时使用系统低 I/O 优先级」).
fn lower_io(server: &Server, h: &ScanHandle) -> bool {
    let Some(g) = &h.governor else { return false };
    if !g.low_io_priority() || !server.settings.get_bool("scan.idle_low_io") {
        return false;
    }
    if server.scans.low_io.fetch_add(1, Ordering::SeqCst) == 0 {
        sk_core::io::set_low_io_priority(None, true, false);
    }
    true
}

fn restore_io(server: &Server) {
    if server.scans.low_io.fetch_sub(1, Ordering::SeqCst) == 1 {
        sk_core::io::set_low_io_priority(None, false, false);
    }
}

/// Read the volume's $MFT through the read-only helper. False: fall back to a hard scan.
fn run_mft(server: &Arc<Server>, h: &Arc<ScanHandle>, client: Option<Arc<HelperClient>>) -> bool {
    let scanner = h.scanner.clone().expect("a scan");
    let root = h.roots.read()[0].clone();
    let root_path = root.path.to_string_lossy().into_owned();
    {
        let mut st = h.st.lock();
        st.method = ScanMethodUsed::Mft;
        st.phase = if client.is_none() { "helper" } else { "mft" }.into();
    }
    scanner.begin_external();
    events::emit(
        Source::Scan,
        &tr!("开始扫描 {path}（MFT 快速扫描）", path = root_path.clone()),
        EventLevel::Info,
        Some(&h.id),
        Some(&root_path),
    );
    h.publish(server);
    let fail = |reason: String| {
        {
            let mut st = h.st.lock();
            st.method = ScanMethodUsed::Hard;
            st.phase.clear();
            st.fallback = tr!("{reason}，已改用硬扫描", reason = reason.clone());
        }
        scanner.reset_external();
        events::emit(
            Source::Scan,
            &tr!("MFT 快速扫描不可用：{reason}；改用硬扫描", reason = reason),
            EventLevel::Warning,
            Some(&h.id),
            None,
        );
    };
    let clear = || {
        let mut st = h.st.lock();
        st.mft_client = None;
        st.phase.clear();
        st.transient = 0;
        st.mft_read = None;
    };
    let client = match client {
        Some(c) => c,
        None => match server.scans.helper.get(server.settings.get_bool("scan.elevate")) {
            Ok(c) => c,
            Err(e) => {
                clear();
                fail(e.message);
                return false;
            }
        },
    };
    if scanner.cancelled() {
        // stopped while the administrator prompt was open
        scanner.finish_external(ScanState::Cancelled);
        clear();
        return true;
    }
    {
        let mut st = h.st.lock();
        st.phase = "mft".into();
        st.mft_client = Some(client.clone());
    }
    h.publish(server);
    let volume: String = root_path.chars().take(2).collect();
    let duty = mft_duty(h.governor.as_deref());
    let hh = h.clone();
    let mut progress = move |phase: Phase, elapsed: f64, read: Option<f64>| {
        let mut st = hh.st.lock();
        st.phase = if phase == Phase::Send { "receive" } else { "mft" }.into();
        st.mft_elapsed = elapsed;
        st.mft_at = Instant::now();
        if read.is_some() {
            st.mft_read = read;
        }
    };
    let table = client.scan_mft(&volume, MftOptions { duty, ..MftOptions::default() }, Some(&mut progress));
    let table = match table {
        Ok(t) => t,
        Err(e) => {
            clear();
            if scanner.cancelled() {
                scanner.finish_external(ScanState::Cancelled);
                return true;
            }
            fail(e.message);
            return false;
        }
    };
    {
        let mut st = h.st.lock();
        st.transient = table.record.len() * MFT_BUILD_BYTES_PER_ROW;
        st.phase = "build".into();
    }
    h.publish(server);
    let build = MftBuild {
        include_ads: h.ads,
        count_links_each: server.settings.get_str("scan.hardlinks") == "each",
        folder_names: None,
    };
    let store = match table_to_store(table, &root_path, &build) {
        Ok(s) => s,
        Err(e) => {
            clear();
            fail(e.to_string());
            return false;
        }
    };
    {
        let mut w = h.store.write();
        w.adopt(&store);
    }
    drop(store);
    h.ctx.names.forget();
    server.monitors.rebuilt(&h.id); // every id changed
    scanner.finish_external(ScanState::Done);
    clear();
    true
}

fn warnings_on(server: &Server) -> bool {
    server.settings.get_bool("general.log_scan_warnings")
}

fn run_hard(server: &Arc<Server>, h: &Arc<ScanHandle>, again: bool) {
    let scanner = h.scanner.clone().expect("a scan");
    let roots = h.roots.read().clone();
    let listed = h.root_paths().join(", ");
    let started = if again {
        tr!("继续扫描 {paths}（硬扫描）", paths = listed)
    } else {
        tr!("开始扫描 {paths}（硬扫描）", paths = listed)
    };
    let first = roots.first().map(|r| r.path.to_string_lossy().into_owned());
    events::emit(Source::Scan, &started, EventLevel::Info, Some(&h.id), first.as_deref());
    for r in &roots {
        if let Some(v) = &r.volume
            && !v.note.is_empty()
        {
            events::emit(
                Source::Scan,
                &tr!("{path}：{note}", path = r.path.display().to_string(), note = v.note.clone()),
                EventLevel::Warning,
                Some(&h.id),
                None,
            );
        }
    }
    let mut reported = 0usize;
    let mut progressed = |pr: &ScanProgress| {
        let denied = &pr.denied;
        if !warnings_on(server) {
            reported = denied.len(); // 「记录扫描警告」 off: counted, not logged
        }
        if denied.len() > reported {
            let reasons = scanner.denied_reasons();
            for (i, path) in denied.iter().enumerate().skip(reported) {
                let reason = reasons.get(i).map_or_else(|| tr!("拒绝访问"), |r| events::denied_reason(r));
                events::emit(
                    Source::Perm,
                    &tr!("无法访问 {path}：{reason}，已计入未知空间", path = path.clone(), reason = reason),
                    EventLevel::Warning,
                    Some(&h.id),
                    Some(path),
                );
            }
        }
        reported = denied.len();
    };
    scanner.run(Some(&mut progressed), TICK);
    let upgrade = {
        let mut st = h.st.lock();
        if scanner.state() == ScanState::Cancelled { st.upgrade.take() } else { None }
    };
    if let Some(client) = upgrade {
        scanner.uncancel(); // that cancel was ours: a user's stop from now on counts again
        if run_mft(server, h, Some(client)) {
            finish(server, h, Some(0));
            return;
        }
        scanner.reset_external(); // the read failed: scan the rest of the volume the hard way
        h.st.lock().method = ScanMethodUsed::Hard;
        scanner.run(Some(&mut progressed), TICK);
    }
    let on = warnings_on(server);
    finish(server, h, on.then_some(reported));
}

/// After the tree is complete (hard or MFT): compact, report, save, index the names.
fn finish(server: &Arc<Server>, h: &Arc<ScanHandle>, reported: Option<usize>) {
    let scanner = h.scanner.clone().expect("a scan");
    let first = h.root_paths().into_iter().next();
    server.monitors.kick(&h.id); // the last folders of the scan get their watches now
    // nothing is added any more: give back the growth headroom
    let freed = h.store.appender().compact();
    tracing::debug!("scan {}: compacted the node store, {:.0} MB freed", h.id, freed as f64 / 1048576.0);
    let done = scanner.progress();
    if let Some(rep) = reported
        && done.errors > rep as i64
    {
        events::emit(
            Source::Perm,
            &tr!("另有 {count} 个文件夹无法访问，已计入未知空间", count = sk_core::fmt::count(done.errors - rep as i64)),
            EventLevel::Warning,
            Some(&h.id),
            None,
        );
    }
    let took = tr!(
        "{items} 项，{seconds} 秒",
        items = sk_core::fmt::count(done.items),
        seconds = format!("{:.1}", done.elapsed_s)
    );
    match done.state {
        ScanState::Done => {
            let text = if h.st.lock().method == ScanMethodUsed::Mft {
                tr!("MFT 快速扫描完成：{took}", took = took)
            } else {
                tr!("硬扫描完成：{took}", took = took)
            };
            events::emit(Source::Scan, &text, EventLevel::Info, Some(&h.id), first.as_deref());
        }
        ScanState::Cancelled => events::emit(
            Source::Scan,
            &tr!("已停止扫描：{took}，已扫描的结果保留", took = took),
            EventLevel::Warning,
            Some(&h.id),
            first.as_deref(),
        ),
        _ => {
            let msg = if done.message.is_empty() { tr!("未知错误") } else { done.message.clone() };
            events::emit(Source::Scan, &tr!("扫描失败：{error}", error = msg), EventLevel::Error, Some(&h.id), first.as_deref());
        }
    }
    if done.state == ScanState::Done {
        if h.auto_snapshot {
            auto_save(server, h);
        }
        server.summaries.keep(server, h);
    }
    h.publish(server);
    if done.state == ScanState::Done {
        // build the name index now, so the first wildcard filter does not pay for it
        let nodes = h.store.read();
        h.ctx.names.update(&nodes, nodes.len());
    }
}

/// Save a finished scan to the auto-snapshot folder; a failure only costs a log warning.
fn auto_save(server: &Server, h: &ScanHandle) {
    let path = auto_path(&h.root_paths());
    if let Err(reason) = save(h, &path) {
        tracing::warn!("could not save snapshot for {}: {reason}", h.id);
        events::emit(
            Source::Snapshot,
            &tr!("自动保存快照失败：{reason}", reason = reason),
            EventLevel::Warning,
            Some(&h.id),
            None,
        );
        return;
    }
    h.st.lock().snapshot_path = Some(path.display().to_string());
    let keep = server.settings.get_int("advanced.snapshot_keep").max(1) as usize;
    let pruned = path.parent().map(|dir| snapshot::prune(dir, keep).map(|v| v.len()).unwrap_or(0)).unwrap_or(0);
    tracing::info!("scan {} saved to {} ({pruned} old snapshots pruned)", h.id, path.display());
}

/// The scan's progress event name (re-exported for the handlers).
pub type ScanProgressEvent = ScanProgressEvent_;
