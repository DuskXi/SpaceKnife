//! Snapshot comparisons on the server (W6, docs/manage.md §3; Python `server/compares.py`).
//!
//! `compare.open` loads the snapshot end(s) and merges the two trees on a blocking thread
//! ([`sk_core::diff::compare_with`]), keeping only what changed: a [`ChangeTree`], a small store
//! whose sizes are the changes. Views open it like a scan (`view.open {scan_id: compare_id}`):
//! [`CompareManager::view_target`] hands them a [`ViewContext`] over the change store (sharing the
//! scan's tags) and an [`ArrayChanges`] with the signed change and status per row.
//!
//! A comparison whose target is the live tree ("now") follows file changes: once the tree was quiet
//! for [`LIVE_QUIET`] (at most every [`LIVE_MIN_GAP`]) it is computed again and its views move to
//! the new change tree by path ([`crate::views::ViewManager::replace_store`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use sk_core::diff::{
    Change, ChangeTree, CompareOptions, DOMINANT, DiffError, Direction, MINOR, Status, change_item,
    compare_with, rest_item,
};
use sk_core::filter::NameIndex;
use sk_core::layout::Basis;
use sk_core::settings::SettingsStore;
use sk_core::tr;
use sk_core::view::trash::TrashCache;
use sk_core::view::{ArrayChanges, CompareSource, View, ViewContext};
use sk_protocol::ErrorCode;
use sk_protocol::compare::{
    ChangeItem, ChangeKind, ChangeStatus, CompareDirection, CompareInfo, CompareNodeResult,
    CompareOpenParams, ComparePhase, CompareProgressEvent, CompareSide, CompareSideKind,
    CompareSourcesResult, CompareSummary, CompareTask, NOW,
};
use sk_protocol::methods::CompareProgress;
use sk_protocol::scan::{ScanSource, TreeNodeResult};
use sk_store::snapshot;
use sk_store::{NodeId, Store, StoreOptions};

use crate::app::{Config, Server};
use crate::rpc::{RpcError, RpcResult};
use crate::scans::{ScanHandle, node_info, snapshot_info};
use crate::session::Session;
use crate::views::ViewHost;

pub const PREFIX: &str = "cmp-";
/// Comparisons per backend; the oldest one without views goes first.
pub const MAX_COMPARES: usize = 6;
/// A live comparison is recomputed once the tree was quiet this long …
pub const LIVE_QUIET: Duration = Duration::from_secs(3);
/// … and at most this often.
pub const LIVE_MIN_GAP: Duration = Duration::from_secs(10);
/// Change units counted for 「前 N 项」 (the lists show the first ones).
pub const COUNT_UNITS: usize = 2000;
const PROGRESS_GAP: Duration = Duration::from_millis(200);

fn not_ready() -> RpcError {
    RpcError::invalid_request(tr!("对比还在计算"))
}

/// Nodes of a change tree found in the tab's live tree ([`CompareManager::live_nodes`]).
pub struct LiveNodes {
    pub scan_id: String,
    /// per node asked: its live node, None where it is not on disk now
    pub found: Vec<Option<NodeId>>,
    /// the live tree's numbering they are in
    pub generation: u64,
}

/// A computed comparison: the change tree, the views' context over it, the change source.
pub struct Ready {
    pub tree: Arc<ChangeTree>,
    pub ctx: Arc<ViewContext>,
    pub source: Arc<dyn CompareSource>,
}

struct CmpState {
    base: Option<CompareSide>,
    target: Option<CompareSide>,
    ready: Option<Arc<Ready>>,
    phase: ComparePhase,
    progress: Option<f64>,
    message: String,
    revision: i64,
    /// the live tree changed; a recompute is pending
    dirty_at: Option<Instant>,
    computed_at: Option<Instant>,
    last_emit: Option<Instant>,
    following: bool,
}

/// One comparison.
pub struct CompareHandle {
    pub id: String,
    pub scan_id: String,
    pub session_id: String,
    session: Weak<Session>,
    scan: Weak<ScanHandle>,
    pub base_spec: String,
    pub target_spec: String,
    st: Mutex<CmpState>,
    cancel: Arc<AtomicBool>,
    /// the tab's scan went away: no more live recomputes
    stopped: AtomicBool,
}

impl CompareHandle {
    pub fn live(&self) -> bool {
        self.target_spec == NOW || self.base_spec == NOW
    }

    pub fn ready(&self) -> Option<Arc<Ready>> {
        self.st.lock().ready.clone()
    }

    /// Its file is there to open or delete: the target is the live tree and it was not deleted.
    pub fn on_disk(&self, tree: &ChangeTree, node: NodeId) -> bool {
        self.target_spec == NOW
            && self
                .scan
                .upgrade()
                .is_some_and(|s| s.source == ScanSource::Scan)
            && tree
                .diff
                .status
                .get(node as usize)
                .is_some_and(|s| *s != Status::Removed)
    }

    fn info_of(&self, st: &CmpState) -> Option<CompareInfo> {
        let r = st.ready.as_ref()?;
        let d = &r.tree.diff;
        let roots: Vec<NodeId> = d.store.read().roots().collect();
        Some(CompareInfo {
            compare_id: self.id.clone(),
            scan_id: self.scan_id.clone(),
            base: st.base.clone()?,
            target: st.target.clone()?,
            grow: roots.iter().map(|&r| d.grow[r as usize]).sum(),
            shrink: roots.iter().map(|&r| d.shrink[r as usize]).sum(),
            changed: (1..d.len())
                .filter(|&i| d.grow[i] + d.shrink[i] > 0)
                .count() as i64,
            nodes: d.len() as i64,
            live: self.live(),
            revision: st.revision,
        })
    }

    pub fn info(&self) -> RpcResult<CompareInfo> {
        self.info_of(&self.st.lock()).ok_or_else(not_ready)
    }

    fn event(&self) -> CompareProgressEvent {
        let st = self.st.lock();
        CompareProgressEvent {
            compare_id: self.id.clone(),
            phase: st.phase,
            progress: st.progress,
            message: st.message.clone(),
            info: if st.phase == ComparePhase::Ready {
                self.info_of(&st)
            } else {
                None
            },
        }
    }

    fn set(&self, phase: ComparePhase, progress: Option<f64>, message: String) {
        let mut st = self.st.lock();
        st.phase = phase;
        st.progress = progress;
        st.message = message;
    }

    /// Send `compare.progress` to the session that opened it (`force`: else at most every 0.2 s).
    fn emit(&self, force: bool) {
        {
            let mut st = self.st.lock();
            if !force && st.last_emit.is_some_and(|t| t.elapsed() < PROGRESS_GAP) {
                return;
            }
            st.last_emit = Some(Instant::now());
        }
        if let Some(s) = self.session.upgrade() {
            s.emit::<CompareProgress>(&self.event());
        }
    }
}

/// Comparisons every session can open views on.
pub struct CompareManager {
    compares: Mutex<HashMap<String, Arc<CompareHandle>>>,
    order: Mutex<Vec<String>>,
    ids: AtomicU64,
}

/// The "now" end of a comparison: the scan as it is.
pub fn now_side(h: &ScanHandle) -> CompareSide {
    let pr = h.progress();
    let when = if h.source == ScanSource::Scan {
        chrono::Utc::now()
    } else {
        pr.finished_at.unwrap_or_else(chrono::Utc::now)
    };
    let nodes = h.store.read();
    CompareSide {
        kind: CompareSideKind::Now,
        path: if h.source == ScanSource::Snapshot {
            h.st.lock().snapshot_path.clone()
        } else {
            None
        },
        created_at: when,
        bytes_alloc: h.roots.read().iter().map(|r| nodes.tot_alloc(r.node)).sum(),
        node_count: nodes.len() as i64,
    }
}

/// One end of a comparison: the live store, or a snapshot of the same roots.
fn side(scan: &ScanHandle, spec: &str) -> RpcResult<(Store, CompareSide)> {
    if spec == NOW {
        return Ok((scan.store.clone(), now_side(scan)));
    }
    let path = PathBuf::from(spec);
    let (store, header) = snapshot::load(&path).map_err(|e| {
        RpcError::new(
            ErrorCode::SnapshotError,
            tr!(
                "打不开快照 {path}：{error}",
                path = path.display().to_string(),
                error = e.to_string()
            ),
        )
    })?;
    let mine = scan.root_paths();
    if snapshot::root_key(&header.roots) != snapshot::root_key(&mine) {
        let sep = sk_core::i18n::t("、");
        return Err(RpcError::new(
            ErrorCode::SnapshotError,
            tr!(
                "快照的扫描位置和当前标签不同：{a} ≠ {b}",
                a = header.roots.join(sep),
                b = mine.join(sep)
            ),
        ));
    }
    let info = snapshot_info(&path, &header);
    let side = CompareSide {
        kind: CompareSideKind::Snapshot,
        path: Some(path.display().to_string()),
        created_at: info.created_at,
        bytes_alloc: info.bytes_alloc,
        node_count: header.node_count as i64,
    };
    Ok((store, side))
}

/// Why a computation ended without a tree.
enum Failure {
    Cancelled,
    Failed(String),
}

/// Load both ends and merge them (blocking).
fn compute(h: &Arc<CompareHandle>, scan: &ScanHandle) -> Result<ChangeTree, Failure> {
    h.set(ComparePhase::Loading, None, tr!("正在读取快照…"));
    h.emit(true);
    let (base, bside) = side(scan, &h.base_spec).map_err(|e| Failure::Failed(e.message))?;
    let (target, tside) = side(scan, &h.target_spec).map_err(|e| Failure::Failed(e.message))?;
    {
        let mut st = h.st.lock();
        st.base = Some(bside);
        st.target = Some(tside);
    }
    h.set(ComparePhase::Comparing, Some(0.0), tr!("正在对比…"));
    h.emit(true);
    let hh = h.clone();
    let mut progress = move |f: f64| {
        hh.st.lock().progress = Some(f);
        hh.emit(false);
    };
    let cancel = h.cancel.clone();
    let cancelled = move || cancel.load(Ordering::SeqCst);
    let diff = {
        let (b, t) = (base.read(), target.read());
        compare_with(
            &b,
            &t,
            CompareOptions {
                progress: Some(&mut progress),
                cancel: Some(&cancelled),
                ..CompareOptions::default()
            },
        )
    };
    let diff = match diff {
        Ok(d) => d,
        Err(DiffError::Cancelled) => return Err(Failure::Cancelled),
        Err(e) => {
            return Err(Failure::Failed(tr!(
                "对比失败：{error}",
                error = e.to_string()
            )));
        }
    };
    ChangeTree::of(&diff, StoreOptions::default())
        .map_err(|e| Failure::Failed(tr!("对比失败：{error}", error = e.to_string())))
}

/// The views' context and change source over a change tree (the scan's tags).
fn ready_of(tree: ChangeTree, scan: &ScanHandle) -> Ready {
    let tree = Arc::new(tree);
    let store = tree.store().clone();
    let roots: Vec<sk_core::view::RootInfo> = store
        .read()
        .roots()
        .map(sk_core::view::RootInfo::folder)
        .collect();
    let ctx = Arc::new(ViewContext {
        store,
        roots: RwLock::new(roots),
        basis: Arc::new(Basis::for_scan()),
        names: Arc::new(NameIndex::new()),
        tags: scan.ctx.tags.clone(),
        trash: Arc::new(TrashCache::new()),
        vocab: RwLock::new(crate::views::current_vocab()),
    });
    let t = tree.clone();
    let source: Arc<dyn CompareSource> = Arc::new(ArrayChanges::new(
        tree.diff.delta.clone(),
        tree.diff.status.iter().map(|s| *s as u8).collect(),
        move |d, m| {
            t.amounts(
                d != CompareDirection::Shrink,
                d != CompareDirection::Grow,
                m,
            )
        },
    ));
    Ready { tree, ctx, source }
}

impl CompareManager {
    pub fn new(_config: &Config, _settings: &Arc<SettingsStore>) -> CompareManager {
        CompareManager {
            compares: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            ids: AtomicU64::new(1),
        }
    }

    pub fn lookup(&self, id: &str) -> Option<Arc<CompareHandle>> {
        self.compares.lock().get(id).cloned()
    }

    pub fn get(&self, id: &str) -> RpcResult<Arc<CompareHandle>> {
        self.lookup(id).ok_or_else(|| {
            RpcError::new(ErrorCode::UnknownScan, tr!("没有这个对比：{id}", id = id))
        })
    }

    fn all(&self) -> Vec<Arc<CompareHandle>> {
        let m = self.compares.lock();
        self.order
            .lock()
            .iter()
            .filter_map(|id| m.get(id).cloned())
            .collect()
    }

    /// A ready comparison opens like a scan: its context and change source.
    pub fn view_target(&self, id: &str) -> Option<(Arc<ViewContext>, Arc<dyn CompareSource>)> {
        let r = self.lookup(id)?.ready()?;
        Some((r.ctx.clone(), r.source.clone()))
    }

    /// The view contexts of every comparison (the filter vocabulary follows the language).
    pub fn contexts(&self) -> Vec<Arc<ViewContext>> {
        self.all()
            .iter()
            .filter_map(|h| h.ready().map(|r| r.ctx.clone()))
            .collect()
    }

    // ------------------------------------------------------------- sources

    pub fn sources(&self, server: &Server, scan_id: &str) -> RpcResult<CompareSourcesResult> {
        let h = server.scans.get(scan_id)?;
        let key = snapshot::root_key(&h.root_paths());
        let own = if h.source == ScanSource::Snapshot {
            h.st.lock().snapshot_path.clone()
        } else {
            None
        };
        let found = snapshot::list_snapshots(&sk_core::paths::snapshots_dir());
        let snapshots = found
            .iter()
            .filter(|(p, hd)| {
                snapshot::root_key(&hd.roots) == key
                    && own.as_deref() != Some(p.display().to_string().as_str())
            })
            .map(|(p, hd)| snapshot_info(p, hd))
            .collect();
        if h.running() {
            return Ok(CompareSourcesResult {
                snapshots,
                now: None,
                now_note: tr!("扫描还没有完成，完成后才能与「现在」对比"),
            });
        }
        Ok(CompareSourcesResult {
            snapshots,
            now: Some(now_side(&h)),
            now_note: String::new(),
        })
    }

    // ------------------------------------------------------------- lifecycle

    pub fn open(
        &self,
        server: &Arc<Server>,
        session: &Arc<Session>,
        params: &CompareOpenParams,
    ) -> RpcResult<CompareTask> {
        let scan = server.scans.get(&params.scan_id)?;
        if params.base == params.target {
            return Err(RpcError::invalid_params(tr!("基准和对比目标是同一份数据")));
        }
        if (params.base == NOW || params.target == NOW) && scan.running() {
            return Err(RpcError::new(
                ErrorCode::ScanBusy,
                tr!("扫描还没有完成，完成后才能与「现在」对比"),
            ));
        }
        self.make_room(server);
        let h = Arc::new(CompareHandle {
            id: format!("{PREFIX}{}", self.ids.fetch_add(1, Ordering::SeqCst)),
            scan_id: scan.id.clone(),
            session_id: session.id.clone(),
            session: Arc::downgrade(session),
            scan: Arc::downgrade(&scan),
            base_spec: params.base.clone(),
            target_spec: params.target.clone(),
            st: Mutex::new(CmpState {
                base: None,
                target: None,
                ready: None,
                phase: ComparePhase::Loading,
                progress: None,
                message: String::new(),
                revision: 0,
                dirty_at: None,
                computed_at: None,
                last_emit: None,
                following: false,
            }),
            cancel: Arc::new(AtomicBool::new(false)),
            stopped: AtomicBool::new(false),
        });
        self.order.lock().push(h.id.clone());
        self.compares.lock().insert(h.id.clone(), h.clone());
        let id = h.id.clone();
        server.rt.spawn_blocking(move || {
            let res = compute(&h, &scan);
            match res {
                Ok(tree) => {
                    let rows = tree.len();
                    let mb = tree.memory_bytes() as f64 / 1e6;
                    let ready = ready_of(tree, &scan);
                    {
                        let mut st = h.st.lock();
                        st.ready = Some(Arc::new(ready));
                        st.computed_at = Some(Instant::now());
                    }
                    h.set(ComparePhase::Ready, Some(1.0), String::new());
                    tracing::info!("compare {}: {rows} change rows ({mb:.1} MB)", h.id);
                }
                Err(Failure::Cancelled) => h.set(ComparePhase::Cancelled, None, String::new()),
                Err(Failure::Failed(m)) => {
                    let p = h.st.lock().progress;
                    h.set(ComparePhase::Failed, p, m)
                }
            }
            if !h.cancel.load(Ordering::SeqCst) {
                h.emit(true);
            }
        });
        Ok(CompareTask { compare_id: id })
    }

    fn make_room(&self, server: &Arc<Server>) {
        while self.compares.lock().len() >= MAX_COMPARES {
            let idle = self
                .all()
                .into_iter()
                .find(|h| !server.views.has_views(&h.id));
            match idle {
                Some(h) => self.close(server, &h.id),
                None => return,
            }
        }
    }

    /// Forget a comparison (or stop computing it); its views close.
    pub fn close(&self, server: &Arc<Server>, id: &str) {
        let h = self.compares.lock().remove(id);
        self.order.lock().retain(|x| x != id);
        if let Some(h) = h {
            h.cancel.store(true, Ordering::SeqCst);
            h.stopped.store(true, Ordering::SeqCst);
            server.views.close_scan(id);
            h.st.lock().ready = None;
        }
    }

    /// A session went: its comparisons go.
    pub fn close_session(&self, server: &Arc<Server>, session_id: &str) {
        for h in self.all() {
            if h.session_id == session_id {
                self.close(server, &h.id);
            }
        }
    }

    /// The tab's scan went away: comparisons with its live tree stop following it.
    pub fn close_scan(&self, _server: &Arc<Server>, scan_id: &str) {
        for h in self.all() {
            if h.scan_id == scan_id {
                h.stopped.store(true, Ordering::SeqCst);
            }
        }
    }

    // ------------------------------------------------------------- live target (UI-W6-38)

    /// Files of a scan changed (monitor): its live comparisons are computed again soon.
    pub fn on_tree_changed(&self, server: &Arc<Server>, scan_id: &str) {
        for h in self.all() {
            if h.scan_id != scan_id || !h.live() || h.stopped.load(Ordering::SeqCst) {
                continue;
            }
            {
                let mut st = h.st.lock();
                if st.phase != ComparePhase::Ready {
                    continue;
                }
                st.dirty_at = Some(Instant::now());
                if st.following {
                    continue;
                }
                st.following = true;
            }
            let (srv, hh) = (server.clone(), h.clone());
            server.rt.spawn(async move {
                srv.compares.follow(&srv, &hh).await;
                hh.st.lock().following = false;
            });
        }
    }

    async fn follow(&self, server: &Arc<Server>, h: &Arc<CompareHandle>) {
        loop {
            if h.stopped.load(Ordering::SeqCst) || self.lookup(&h.id).is_none() {
                return;
            }
            let wait = {
                let st = h.st.lock();
                let Some(dirty) = st.dirty_at else { return };
                let due =
                    (dirty + LIVE_QUIET).max(st.computed_at.map_or(dirty, |c| c + LIVE_MIN_GAP));
                due.saturating_duration_since(Instant::now())
            };
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
                continue;
            }
            h.st.lock().dirty_at = None;
            let Some(scan) = server.scans.lookup(&h.scan_id) else {
                return;
            };
            if scan.running() {
                return;
            }
            let (hh, sc) = (h.clone(), scan.clone());
            let res =
                tokio::task::spawn_blocking(move || compute(&hh, &sc).map(|t| ready_of(t, &sc)))
                    .await;
            let ready = match res {
                Ok(Ok(r)) => Arc::new(r),
                Ok(Err(f)) => {
                    let why = match f {
                        Failure::Cancelled => "cancelled".to_owned(),
                        Failure::Failed(m) => m,
                    };
                    tracing::warn!("live compare {} stopped: {why}", h.id);
                    h.set(ComparePhase::Ready, Some(1.0), String::new()); // keep the last result
                    h.emit(true);
                    return;
                }
                Err(_) => return,
            };
            if self.lookup(&h.id).is_none() {
                return;
            }
            {
                let mut st = h.st.lock();
                st.ready = Some(ready.clone());
                st.revision += 1;
                st.computed_at = Some(Instant::now());
            }
            h.set(ComparePhase::Ready, Some(1.0), String::new());
            server
                .views
                .replace_store(server, &h.id, ready.ctx.clone(), ready.source.clone())
                .await;
            h.emit(true);
        }
    }

    // ------------------------------------------------------------- tree.node, sidebar, hover card

    /// `tree.node` on a comparison's change tree (None: not a comparison).
    pub fn tree_node(&self, id: &str, node: NodeId) -> Option<RpcResult<TreeNodeResult>> {
        self.with_node(id, node, |nodes, on_disk| TreeNodeResult {
            node: node_info(nodes, node),
            path: nodes.path_printable(node),
            on_disk,
        })
    }

    /// (path as the store keeps it, on disk) of a change tree row: `path.forms` of a comparison
    /// (None: not a comparison).
    pub fn path_bytes(&self, id: &str, node: NodeId) -> Option<RpcResult<(Vec<u8>, bool)>> {
        self.with_node(id, node, |nodes, on_disk| (nodes.path_bytes(node), on_disk))
    }

    fn with_node<T>(
        &self,
        id: &str,
        node: NodeId,
        f: impl FnOnce(&sk_store::Nodes, bool) -> T,
    ) -> Option<RpcResult<T>> {
        let h = self.lookup(id)?;
        let Some(r) = h.ready() else {
            return Some(Err(not_ready()));
        };
        if node as usize >= r.tree.len() {
            return Some(Err(RpcError::new(
                ErrorCode::UnknownNode,
                format!("unknown node: {node}"),
            )));
        }
        let nodes = r.tree.store().read();
        Some(Ok(f(&nodes, h.on_disk(&r.tree, node))))
    }

    /// The same items in the tab's live tree (file operations from a compare view): for each
    /// node of the change tree the live node, None where it is not on disk now. Ids read in
    /// another numbering than `generation` (the comparison was computed again since) are refused
    /// as a whole. None: no such comparison, or its tab's scan is gone.
    pub fn live_nodes(
        &self,
        id: &str,
        nodes: &[i64],
        generation: Option<u64>,
    ) -> Option<RpcResult<LiveNodes>> {
        let h = self.lookup(id)?;
        let r = h.ready()?;
        let scan = h.scan.upgrade()?;
        // a change tree is never renumbered: a comparison computed again is another one
        if generation.is_some_and(|g| g != r.tree.store().generation()) {
            return Some(Err(crate::ops_service::stale_nodes()));
        }
        let paths: Vec<Option<Vec<u8>>> = {
            let tree = r.tree.store().read();
            nodes
                .iter()
                .map(|&n| {
                    let node = NodeId::try_from(n).ok()?;
                    (node != 0 && (node as usize) < r.tree.len() && h.on_disk(&r.tree, node))
                        .then(|| tree.path_bytes(node))
                })
                .collect()
        };
        let live = scan.store.read();
        let found = paths
            .iter()
            .map(|p| p.as_ref().and_then(|p| live.find(p)))
            .collect();
        Some(Ok(LiveNodes {
            scan_id: scan.id.clone(),
            found,
            // under the reader the ids were found with
            generation: scan.store.generation(),
        }))
    }

    fn host_ready(&self, host: &ViewHost) -> RpcResult<(Arc<CompareHandle>, Arc<Ready>)> {
        let h = self.get(&host.scan_id)?;
        let r = h.ready().ok_or_else(not_ready)?;
        Ok((h, r))
    }

    /// `compare.summary` (blocking: call on a compute thread).
    pub fn summary_of(
        &self,
        server: &Server,
        host: &ViewHost,
        limit: usize,
    ) -> RpcResult<CompareSummary> {
        let (h, r) = self.host_ready(host)?;
        let v = host.view.lock();
        Ok(summary(server, &h, &r.tree, &v, limit))
    }

    /// `compare.node`: one block of the last frame with its children that changed most.
    pub fn node(&self, host: &ViewHost, key: i64, children: usize) -> RpcResult<CompareNodeResult> {
        let (h, r) = self.host_ready(host)?;
        let tree = &r.tree;
        let v = host.view.lock();
        let no_block = || RpcError::invalid_params(format!("no block {key} in the last frame"));
        let frame = v.last.as_ref().ok_or_else(no_block)?;
        let draw = &frame.draw;
        let k = u32::try_from(key).map_err(|_| no_block())?;
        let row = draw.row(k).ok_or_else(no_block)?;
        let node = draw.node[row];
        let members = draw.members.get(&draw.key[row]);
        let scope = v.zoom.node;
        let (card, kids): (ChangeItem, Vec<NodeId>) = match members.filter(|m| !m.is_empty()) {
            Some(ids) => {
                let nodes = tree.store().read();
                let folder = nodes.parent(ids[0]).max(0) as NodeId;
                let start: usize = draw.name_len[..row].iter().map(|&l| l as usize).sum();
                let label = String::from_utf8_lossy(
                    &draw.names[start..start + draw.name_len[row] as usize],
                )
                .into_owned();
                let d = &tree.diff;
                let item = ChangeItem {
                    node: folder,
                    path: nodes.path_printable(folder),
                    label,
                    kind: ChangeKind::Group,
                    delta: ids.iter().map(|&i| d.delta[i as usize]).sum(),
                    base: ids.iter().map(|&i| d.base_alloc[i as usize]).sum(),
                    target: ids.iter().map(|&i| d.target_alloc[i as usize]).sum(),
                    status: ChangeStatus::Changed,
                    new_files: ids
                        .iter()
                        .map(|&i| i64::from(tree.new_files[i as usize]))
                        .sum(),
                    members: ids.len() as i64,
                    reason: String::new(),
                    on_disk: false,
                };
                (item, ids.clone())
            }
            None => {
                if node < 0 {
                    return Err(no_block());
                }
                let node = node as NodeId;
                let kids: Vec<NodeId> = {
                    let nodes = tree.store().read();
                    if nodes.is_dir(node) {
                        nodes.children(node).collect()
                    } else {
                        Vec::new()
                    }
                };
                (item(&h, tree, node, scope), kids)
            }
        };
        drop(v);
        let mut top = Vec::new();
        if children > 0 && !kids.is_empty() {
            let mut order = kids;
            order.sort_by_key(|&k| std::cmp::Reverse(tree.diff.delta[k as usize].abs()));
            top = order
                .into_iter()
                .take(children)
                .map(|k| item(&h, tree, k, scope))
                .collect();
        }
        Ok(CompareNodeResult {
            item: card,
            children: top,
        })
    }
}

/// The change item of a row, with its insight note.
fn item(h: &CompareHandle, tree: &ChangeTree, node: NodeId, scope: NodeId) -> ChangeItem {
    let reason = {
        let nodes = tree.store().read();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sk_core::insights::match_node(&nodes, node, None)
        }))
        .ok()
        .flatten()
        .map(|i| i.note)
        .unwrap_or_default()
    };
    change_item(tree, node, scope, reason, h.on_disk(tree, node))
}

fn unit_item(h: &CompareHandle, tree: &ChangeTree, unit: &Change, scope: NodeId) -> ChangeItem {
    rest_item(item(h, tree, unit.node, scope), unit)
}

/// Change units inside the scope, largest first (they partition the change); an aggregate zoom
/// lists its members.
fn units(
    h: &CompareHandle,
    tree: &ChangeTree,
    direction: Direction,
    amount: &[i64],
    members: Option<&[NodeId]>,
    scope: NodeId,
    limit: usize,
) -> (Vec<ChangeItem>, i64) {
    if let Some(m) = members {
        let mut ids: Vec<NodeId> = m
            .iter()
            .copied()
            .filter(|&n| amount.get(n as usize).copied().unwrap_or(0) > 0)
            .collect();
        ids.sort_by_key(|&n| std::cmp::Reverse(amount[n as usize]));
        let shown = ids
            .iter()
            .take(limit)
            .map(|&n| item(h, tree, n, scope))
            .collect();
        return (shown, ids.len() as i64);
    }
    let all = tree.diff.changes(
        direction,
        limit.max(COUNT_UNITS),
        scope,
        Some(amount),
        DOMINANT,
        MINOR,
    );
    let items = all
        .iter()
        .take(limit)
        .map(|u| unit_item(h, tree, u, scope))
        .collect();
    (items, all.len() as i64)
}

fn summary(
    server: &Server,
    h: &CompareHandle,
    tree: &ChangeTree,
    view: &View,
    limit: usize,
) -> CompareSummary {
    let z = &view.zoom;
    let matched = view
        .filter
        .result
        .as_ref()
        .filter(|_| view.filter.active())
        .map(|r| r.matched.clone());
    let (grow, shrink) = match &matched {
        Some(m) => (
            tree.amounts(true, false, Some(m)),
            tree.amounts(false, true, Some(m)),
        ),
        None => (tree.diff.grow.clone(), tree.diff.shrink.clone()),
    };
    let members: Option<Vec<NodeId>> = z.members.as_ref().map(|m| m.as_ref().clone());
    let at = |v: &[i64], n: NodeId| v.get(n as usize).copied().unwrap_or(0);
    let (g, s) = match &members {
        Some(m) => (
            m.iter().map(|&n| at(&grow, n)).sum(),
            m.iter().map(|&n| at(&shrink, n)).sum(),
        ),
        None => (at(&grow, z.node), at(&shrink, z.node)),
    };
    let whole = members.is_none() && z.node == view.home;
    let (grow_items, grow_count) = units(
        h,
        tree,
        Direction::Grow,
        &grow,
        members.as_deref(),
        z.node,
        limit,
    );
    let (shrink_items, shrink_count) = units(
        h,
        tree,
        Direction::Shrink,
        &shrink,
        members.as_deref(),
        z.node,
        limit,
    );
    let (base, target) = {
        let st = h.st.lock();
        (
            st.base.clone().expect("ready"),
            st.target.clone().expect("ready"),
        )
    };
    let span = (target.created_at - base.created_at).num_milliseconds() as f64 / 1000.0;
    let net = g - s;
    let mut free = None;
    if h.target_spec == NOW
        && let Some(scan) = server.scans.lookup(&h.scan_id)
    {
        let frees: Vec<i64> = scan
            .roots
            .read()
            .iter()
            .filter_map(|r| r.volume.as_ref().map(|v| v.free))
            .collect();
        if !frees.is_empty() {
            free = Some(frees.iter().sum());
        }
    }
    let (mut daily, mut days) = (None, None);
    if net > 0 && span > 0.0 {
        let d = net as f64 / (span / 86400.0);
        daily = Some(d);
        if let Some(f) = free
            && d > 0.0
        {
            days = Some(f as f64 / d);
        }
    }
    let scope = if z.node > 0 {
        tree.store().read().path_printable(z.node)
    } else {
        String::new()
    };
    CompareSummary {
        view_id: view.id.clone(),
        compare_id: h.id.clone(),
        scope,
        whole,
        base,
        target,
        span_s: span,
        grow: g,
        shrink: s,
        net,
        free,
        daily,
        days_to_full: days,
        grow_items,
        shrink_items,
        grow_count,
        shrink_count,
    }
}
