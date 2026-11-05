//! The delete basket and file operations on the server (W5, docs/manage.md §2; Python
//! `server/ops_service.py`).
//!
//! The basket lives here, one per backend, shared by every session: `basket.changed` goes to all
//! of them. Executing it runs the operation on a blocking thread ([`sk_core::ops::trash`]), streams
//! `ops.progress` to the session that asked, and keeps what undo needs: the trash references of
//! what went to the recycle bin, the items that left the basket. A permanent deletion waits
//! [`OpsService::delete_delay`] first (the undo window: nothing has been touched yet). Afterwards
//! the tree follows: with live monitoring the file change hooks see it; without, the parent
//! folders are read again.
//!
//! Moving to the trash never turns into a permanent deletion: an item the trash cannot take comes
//! back as `no_trash` and stays where it is (`ops::trash` guarantees it, this layer only reports).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sk_core::ops::{Basket, BasketItem, OpOptions, OpResult, Refused, TrashStatus};
use sk_core::settings::SettingsStore;
use sk_core::tr;
use sk_protocol::ErrorCode;
use sk_protocol::methods::{BasketChanged, OpsProgress};
use sk_protocol::ops::{
    BasketAddParams, BasketAddResult, BasketEvent, BasketExecuteParams, BasketState, BasketVolume,
    OpCode, OpMode, OpPhase, OpProgressEvent, OpTask, PathForms, Refusal,
};
use sk_store::{NodeId, Store};

use crate::app::{Config, Server};
use crate::rpc::{RpcError, RpcResult};
use crate::session::Session;

/// Permanent deletion: the undo window before anything is touched.
pub const DELETE_DELAY: Duration = Duration::from_secs(5);
pub const PROGRESS_GAP: Duration = Duration::from_millis(100);
/// Recycle bin state per volume, read again at most this often.
pub const TRASH_INFO_TTL: Duration = Duration::from_secs(30);
/// Finished operations remembered for undo.
pub const KEEP_TASKS: usize = 8;
/// After file changes, the basket is checked again once this quiet.
pub const REFRESH_QUIET: Duration = Duration::from_secs(1);

mod text {
    pub const NOT_ON_DISK: &str = "只能操作「现在」还在磁盘上的项";
    pub const NO_BLOCK: &str = "这个块已经不在视图里了，请再试一次";
    pub const BUSY: &str = "还有文件操作在进行，完成后再执行";
    pub const UNKNOWN_TASK: &str = "没有这个文件操作：{id}";
    pub const NOTHING: &str = "删除篮里没有可以执行的项";
    pub const TRASH_SAME: &str = "回收站在同一个卷上：清空回收站后才真正释放空间";
    pub const TRASH_USED: &str = "{note}，回收站现在有 {size}";
}

struct RunState {
    done: i64,
    current: String,
    phase: OpPhase,
    results: Vec<OpResult>,
    freed: i64,
    /// items that left the basket (undo)
    left: Vec<BasketItem>,
    /// trash references (undo)
    refs: Vec<String>,
    last_emit: Option<Instant>,
}

/// One file operation.
pub struct OpRun {
    pub id: String,
    pub mode: OpMode,
    session: Weak<Session>,
    pub total: i64,
    /// as the file system names them (results and the maps below: as shown)
    paths: Vec<std::path::PathBuf>,
    sizes: HashMap<String, i64>,
    /// item path -> freeable bytes (what a success frees)
    alloc: HashMap<String, i64>,
    /// path -> scan id (refresh the tree afterwards)
    scans: HashMap<String, String>,
    /// path -> its scan root (case twins, kept with the item so it holds once the scan is closed)
    roots: HashMap<String, String>,
    st: Mutex<RunState>,
    cancel: AtomicBool,
    called_off: AtomicBool,
    wake: tokio::sync::Notify,
}

impl OpRun {
    fn event(&self) -> OpProgressEvent {
        let st = self.st.lock();
        let is_final = matches!(
            st.phase,
            OpPhase::Done | OpPhase::Cancelled | OpPhase::Undone
        );
        OpProgressEvent {
            task_id: self.id.clone(),
            mode: self.mode,
            done: st.done,
            total: self.total,
            current: st.current.clone(),
            phase: st.phase,
            results: if is_final {
                st.results.iter().map(OpResult::to_protocol).collect()
            } else {
                Vec::new()
            },
            freed: st.freed,
            can_undo: st.phase == OpPhase::Waiting
                || (st.phase == OpPhase::Done && !st.refs.is_empty()),
        }
    }

    fn emit(&self, force: bool) {
        {
            let mut st = self.st.lock();
            if !force && st.last_emit.is_some_and(|t| t.elapsed() < PROGRESS_GAP) {
                return;
            }
            st.last_emit = Some(Instant::now());
        }
        if let Some(s) = self.session.upgrade() {
            s.emit::<OpsProgress>(&self.event());
        }
    }

    pub fn phase(&self) -> OpPhase {
        self.st.lock().phase
    }

    /// Trash references still to restore (tests clean up with them).
    pub fn refs(&self) -> Vec<String> {
        self.st.lock().refs.clone()
    }
}

/// The basket and the operations of this backend.
pub struct OpsService {
    pub basket: Arc<Basket>,
    runs: Mutex<Vec<Arc<OpRun>>>,
    ids: AtomicU64,
    trash_info: Mutex<HashMap<String, (Instant, TrashStatus)>>,
    dirty_at: Mutex<Option<Instant>>,
    refreshing: AtomicBool,
    locking: AtomicBool,
    delay: Mutex<Duration>,
}

fn rpc_internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(e.to_string())
}

impl OpsService {
    pub fn new(_config: &Config, _settings: &Arc<SettingsStore>) -> OpsService {
        OpsService {
            basket: Arc::new(Basket::new(None)),
            runs: Mutex::new(Vec::new()),
            ids: AtomicU64::new(1),
            trash_info: Mutex::new(HashMap::new()),
            dirty_at: Mutex::new(None),
            refreshing: AtomicBool::new(false),
            locking: AtomicBool::new(false),
            delay: Mutex::new(DELETE_DELAY),
        }
    }

    /// The undo window of a permanent deletion.
    pub fn delete_delay(&self) -> Duration {
        *self.delay.lock()
    }

    /// Change the undo window (tests shorten it).
    pub fn set_delete_delay(&self, d: Duration) {
        *self.delay.lock() = d;
    }

    /// A run by id (still remembered).
    pub fn run(&self, task_id: &str) -> Option<Arc<OpRun>> {
        self.runs.lock().iter().find(|r| r.id == task_id).cloned()
    }

    fn store_of(server: &Server) -> impl FnMut(&str) -> Option<Store> + '_ {
        |scan_id: &str| server.scans.lookup(scan_id).map(|h| h.store.clone())
    }

    fn trash(&self, volume: &str) -> TrashStatus {
        if let Some((t, s)) = self.trash_info.lock().get(volume)
            && t.elapsed() <= TRASH_INFO_TTL
        {
            return s.clone();
        }
        let s = sk_core::ops::trash_status(volume, true);
        self.trash_info
            .lock()
            .insert(volume.to_owned(), (Instant::now(), s.clone()));
        s
    }

    /// The basket for the front ends (reads volumes: call on a blocking thread).
    pub fn state_sync(&self) -> BasketState {
        let items = self.basket.items();
        let freeable = self.basket.freeable();
        let mounts: std::collections::BTreeSet<String> = items
            .iter()
            .filter(|i| !i.volume.is_empty())
            .map(|i| i.volume.clone())
            .collect();
        let mut volumes = Vec::new();
        for mount in mounts {
            let (total, free) = sk_core::volume::volume_for(Path::new(&mount))
                .map_or((0, 0), |v| (v.total, v.free));
            let st = self.trash(&mount);
            let note = if st.available {
                let mut note = if st.same_volume {
                    tr!(text::TRASH_SAME)
                } else {
                    st.location.clone()
                };
                if st.used >= 0 {
                    note = tr!(
                        text::TRASH_USED,
                        note = note,
                        size = sk_core::fmt::size(st.used as f64)
                    );
                }
                note
            } else {
                st.reason.clone()
            };
            volumes.push(BasketVolume {
                freeable: freeable.get(&mount).copied().unwrap_or(0),
                mount_point: mount,
                total,
                free,
                trash: st.available,
                trash_note: note,
            });
        }
        BasketState {
            items: items.iter().map(BasketItem::to_protocol).collect(),
            volumes,
            freeable: freeable.values().sum(),
        }
    }

    pub async fn state(&self, server: &Arc<Server>) -> RpcResult<BasketState> {
        let srv = server.clone();
        crate::compute::blocking(move || srv.ops.state_sync()).await
    }

    /// The basket to every session.
    pub async fn broadcast(&self, server: &Arc<Server>) -> RpcResult<BasketState> {
        let state = self.state(server).await?;
        server.broadcast::<BasketChanged>(
            &BasketEvent {
                state: state.clone(),
            },
            None,
        );
        Ok(state)
    }

    // ------------------------------------------------------------- path forms (W5-09…14)

    pub async fn path_forms(
        &self,
        server: &Arc<Server>,
        scan_id: &str,
        node: NodeId,
    ) -> RpcResult<PathForms> {
        if let Some(r) = server.compares.tree_node(scan_id, node) {
            let r = r?;
            let mut f = sk_core::ops::path_forms(&r.path, None);
            f.on_disk = r.on_disk;
            return Ok(f);
        }
        let h = server.scans.get(scan_id)?;
        h.check_node(node)?;
        crate::compute::blocking(move || {
            let nodes = h.store.read();
            let mut f = sk_core::ops::paths::path_forms_bytes(&nodes.path_bytes(node), None);
            f.on_disk = crate::scans::on_disk(&nodes, node);
            f
        })
        .await
    }

    // ------------------------------------------------------------- adding

    pub async fn add(
        &self,
        server: &Arc<Server>,
        session: &Arc<Session>,
        p: BasketAddParams,
    ) -> RpcResult<BasketAddResult> {
        let mut scan_id = p.scan_id.clone();
        let mut nodes: Vec<i64> = p.nodes.iter().map(|&n| i64::from(n)).collect();
        let mut members: Option<Vec<i64>> = None;
        if let (Some(key), Some(view_id)) = (p.key, &p.view_id) {
            let host = server.views.get(session, view_id)?;
            let h = host.clone();
            let found = crate::compute::blocking(move || {
                let v = h.view.lock();
                let frame = v.last.as_ref()?;
                let key = u32::try_from(key).ok()?;
                let row = frame.draw.row(key)?;
                match frame.draw.members.get(&key).filter(|g| !g.is_empty()) {
                    Some(group) => {
                        let parent = i64::from(v.store().read().parent(group[0]));
                        Some((
                            vec![parent],
                            Some(group.iter().map(|&m| i64::from(m)).collect::<Vec<_>>()),
                        ))
                    }
                    None => Some((vec![i64::from(frame.draw.node[row])], None)),
                }
            })
            .await?;
            let Some((n, m)) = found else {
                return Err(RpcError::invalid_params(tr!(text::NO_BLOCK)));
            };
            nodes = n;
            members = m;
            scan_id = host.scan_id.clone();
        }
        let mut refused: Vec<Refused> = Vec::new();
        let handle = match server.scans.lookup(&scan_id) {
            Some(h) => h,
            None if server.compares.view_target(&scan_id).is_some() => {
                // TODO(compares): map change-tree rows to the live tree (`live_node`); until then a
                // compare view's blocks are refused
                for n in &nodes {
                    let path = server
                        .compares
                        .tree_node(&scan_id, *n as NodeId)
                        .and_then(|r| r.ok())
                        .map_or_else(|| n.to_string(), |r| r.path);
                    refused.push(Refused {
                        path,
                        reason: tr!(text::NOT_ON_DISK),
                    });
                }
                return self
                    .added(
                        server,
                        sk_core::ops::AddResult {
                            refused,
                            ..Default::default()
                        },
                    )
                    .await;
            }
            None => {
                return Err(RpcError::new(
                    ErrorCode::UnknownScan,
                    format!("unknown scan: {scan_id}"),
                ));
            }
        };
        let mut result = sk_core::ops::AddResult::default();
        if !nodes.is_empty() && members.as_ref().is_none_or(|m| !m.is_empty()) {
            let basket = self.basket.clone();
            let id = handle.id.clone();
            result = crate::compute::compute(move || {
                let r = handle.store.read();
                basket.add(&r, &id, &nodes, members.as_deref())
            })
            .await?
            .map_err(|e| RpcError::invalid_params(e.to_string()))?;
        }
        result.refused.splice(0..0, refused);
        let out = self.added(server, result).await;
        self.check_locks(server);
        out
    }

    async fn added(
        &self,
        server: &Arc<Server>,
        r: sk_core::ops::AddResult,
    ) -> RpcResult<BasketAddResult> {
        let state = self.broadcast(server).await?;
        Ok(BasketAddResult {
            state,
            added: r.added.len() as i64,
            merged: r.merged.len() as i64,
            absorbed: r.absorbed.len() as i64,
            refused: r
                .refused
                .iter()
                .map(|x| Refusal {
                    path: x.path.clone(),
                    reason: x.reason.clone(),
                })
                .collect(),
        })
    }

    pub async fn remove(&self, server: &Arc<Server>, ids: &[String]) -> RpcResult<BasketState> {
        self.basket.remove(ids);
        self.broadcast(server).await
    }

    pub async fn clear(&self, server: &Arc<Server>) -> RpcResult<BasketState> {
        self.basket.clear();
        self.broadcast(server).await
    }

    /// Who holds basket items open (UI-W5-37), in the background; the drawer updates.
    fn check_locks(&self, server: &Arc<Server>) {
        if self.locking.swap(true, Ordering::SeqCst) {
            return;
        }
        let srv = server.clone();
        server.rt.spawn(async move {
            let ops = &srv.ops;
            let paths = ops.basket.lock_targets();
            if !paths.is_empty() {
                let before = ops.basket.version();
                let basket = ops.basket.clone();
                let ok = tokio::task::spawn_blocking(move || {
                    let lockers = sk_core::ops::locks::find_lockers(&paths);
                    basket.set_lockers(&lockers);
                })
                .await;
                if ok.is_err() {
                    tracing::error!("checking basket locks failed");
                } else if ops.basket.version() != before {
                    let _ = ops.broadcast(&srv).await;
                }
            }
            ops.locking.store(false, Ordering::SeqCst);
        });
    }

    // ------------------------------------------------------------- tree changes

    /// Files changed (monitor, or an operation of ours): sizes and missing items again.
    pub fn on_tree_changed(&self, server: &Arc<Server>, scan_id: &str) {
        if self.basket.is_empty() || !self.basket.items().iter().any(|i| i.scan_id == scan_id) {
            return;
        }
        *self.dirty_at.lock() = Some(Instant::now());
        if self.refreshing.swap(true, Ordering::SeqCst) {
            return;
        }
        let srv = server.clone();
        server.rt.spawn(async move {
            loop {
                let due = srv
                    .ops
                    .dirty_at
                    .lock()
                    .map_or(Instant::now(), |t| t + REFRESH_QUIET);
                let now = Instant::now();
                if due <= now {
                    break;
                }
                tokio::time::sleep(due - now).await;
            }
            srv.ops.refreshing.store(false, Ordering::SeqCst);
            if let Err(e) = srv.ops.refresh(&srv).await {
                tracing::error!("refreshing the basket failed: {e}");
            }
        });
    }

    /// Find every item again in its tree (no store reader held: `Basket::refresh` takes its own).
    async fn refresh(&self, server: &Arc<Server>) -> RpcResult<()> {
        let srv = server.clone();
        crate::compute::compute(move || {
            let mut store_of = Self::store_of(&srv);
            srv.ops.basket.refresh(&mut store_of);
        })
        .await?;
        self.broadcast(server).await?;
        Ok(())
    }

    // ------------------------------------------------------------- executing (W5-41…49)

    pub fn execute(
        &self,
        server: &Arc<Server>,
        session: &Arc<Session>,
        p: &BasketExecuteParams,
    ) -> RpcResult<OpTask> {
        if self
            .runs
            .lock()
            .iter()
            .any(|r| matches!(r.phase(), OpPhase::Waiting | OpPhase::Running))
        {
            return Err(RpcError::invalid_request(tr!(text::BUSY)));
        }
        let targets = self.basket.targets(p.ids.as_deref());
        if targets.is_empty() {
            return Err(RpcError::invalid_request(tr!(text::NOTHING)));
        }
        let items: HashMap<String, BasketItem> = self
            .basket
            .items()
            .into_iter()
            .map(|i| (i.id.clone(), i))
            .collect();
        let mut alloc = HashMap::new();
        let mut scans = HashMap::new();
        for t in &targets {
            let Some(it) = items.get(&t.item) else {
                continue;
            };
            scans.insert(t.path.clone(), it.scan_id.clone());
            // a group frees its members' share; a single item its own freeable bytes
            let a = if it.members.is_empty() {
                it.alloc
            } else {
                it.alloc / it.members.len().max(1) as i64
            };
            alloc.insert(t.path.clone(), a);
        }
        let delete = p.mode == OpMode::Delete;
        let run = Arc::new(OpRun {
            id: format!("op-{}", self.ids.fetch_add(1, Ordering::SeqCst)),
            mode: p.mode,
            session: Arc::downgrade(session),
            total: targets.len() as i64,
            paths: targets.iter().map(|t| t.os_path.clone()).collect(),
            sizes: targets.iter().map(|t| (t.path.clone(), t.size)).collect(),
            alloc,
            scans,
            roots: targets
                .iter()
                .filter(|t| !t.root.is_empty())
                .map(|t| (t.path.clone(), t.root.clone()))
                .collect(),
            st: Mutex::new(RunState {
                done: 0,
                current: String::new(),
                phase: if delete {
                    OpPhase::Waiting
                } else {
                    OpPhase::Running
                },
                results: Vec::new(),
                freed: 0,
                left: Vec::new(),
                refs: Vec::new(),
                last_emit: None,
            }),
            cancel: AtomicBool::new(false),
            called_off: AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
        });
        {
            let mut runs = self.runs.lock();
            runs.push(run.clone());
            while runs.len() > KEEP_TASKS {
                runs.remove(0);
            }
        }
        tracing::info!(
            "file operation {}: {} {} items",
            run.id,
            p.mode.as_str(),
            run.total
        );
        let delay = self.delete_delay();
        let (srv, r) = (server.clone(), run.clone());
        server
            .rt
            .spawn(async move { srv.ops.run_op(&srv, r, delay).await });
        Ok(OpTask {
            task_id: run.id.clone(),
            mode: run.mode,
            total: run.total,
            delay_s: if delete { delay.as_secs_f64() } else { 0.0 },
        })
    }

    async fn run_op(&self, server: &Arc<Server>, run: Arc<OpRun>, delay: Duration) {
        if run.mode == OpMode::Delete {
            run.emit(true);
            let woken = run.wake.notified();
            tokio::pin!(woken);
            if !run.called_off.load(Ordering::SeqCst) {
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = &mut woken => {}
                }
            }
            if run.called_off.load(Ordering::SeqCst) {
                run.st.lock().phase = OpPhase::Undone; // called off in the undo window: nothing touched
                run.emit(true);
                return;
            }
            run.st.lock().phase = OpPhase::Running;
        }
        run.emit(true);
        let r = run.clone();
        let results = tokio::task::spawn_blocking(move || {
            let mut progress = |done: usize, _total: usize, res: &OpResult| {
                {
                    let mut st = r.st.lock();
                    st.done = done as i64;
                    st.current = res.path.clone();
                }
                r.emit(false);
            };
            let cancel = || r.cancel.load(Ordering::SeqCst);
            let opts = OpOptions {
                progress: Some(&mut progress),
                cancel: Some(&cancel),
                sizes: (r.mode == OpMode::Trash).then_some(&r.sizes),
                roots: Some(&r.roots),
                ..OpOptions::default()
            };
            if r.mode == OpMode::Trash {
                sk_core::ops::move_to_trash(&r.paths, opts)
            } else {
                sk_core::ops::delete_permanently(&r.paths, opts)
            }
        })
        .await;
        match results {
            Ok(results) => {
                let left = self.basket.apply_results(&results);
                let ok = results.iter().filter(|r| r.ok).count();
                let failed: Vec<&OpResult> = results
                    .iter()
                    .filter(|r| !r.ok && r.code != OpCode::Cancelled)
                    .collect();
                for f in failed.iter().take(20) {
                    tracing::warn!("{}: {} ({})", f.path, f.message, f.code.as_str());
                }
                let mut st = run.st.lock();
                st.done = results
                    .iter()
                    .filter(|r| r.code != OpCode::Cancelled)
                    .count() as i64;
                st.freed = results
                    .iter()
                    .filter(|r| r.ok)
                    .map(|r| run.alloc.get(&r.path).copied().unwrap_or(0))
                    .sum();
                st.refs = results
                    .iter()
                    .filter(|r| r.ok && !r.trash_ref.is_empty())
                    .map(|r| r.trash_ref.clone())
                    .collect();
                st.left = left;
                st.phase = if run.cancel.load(Ordering::SeqCst) {
                    OpPhase::Cancelled
                } else {
                    OpPhase::Done
                };
                tracing::info!(
                    "file operation {}: {ok} ok, {} failed, {} freed",
                    run.id,
                    failed.len(),
                    sk_core::fmt::size(st.freed as f64)
                );
                st.results = results;
            }
            Err(e) => {
                tracing::error!("file operation {} failed: {e}", run.id);
                run.st.lock().phase = OpPhase::Done;
            }
        }
        run.emit(true);
        let done: Vec<String> = run
            .st
            .lock()
            .results
            .iter()
            .filter(|r| r.ok)
            .map(|r| r.path.clone())
            .collect();
        self.after(server, &run, &done);
        let _ = self.broadcast(server).await;
    }

    /// The tree follows what changed: monitored scans see it, the others read the parents again.
    fn after(&self, server: &Arc<Server>, run: &OpRun, paths: &[String]) {
        let mut by_scan: HashMap<&str, HashSet<String>> = HashMap::new();
        for p in paths {
            if let Some(scan) = run.scans.get(p) {
                let trimmed = p.trim_end_matches(['\\', '/']);
                let parent = Path::new(trimmed)
                    .parent()
                    .map(|x| x.to_string_lossy().into_owned())
                    .filter(|x| !x.is_empty())
                    .unwrap_or_else(|| p.clone());
                by_scan.entry(scan.as_str()).or_default().insert(parent);
            }
        }
        for (scan_id, parents) in by_scan {
            let Some(h) = server.scans.lookup(scan_id) else {
                continue;
            };
            if h.running() || server.monitors.status(scan_id).is_some() {
                continue; // the file change hooks see it
            }
            let mut parents: Vec<String> = parents.into_iter().collect();
            parents.sort();
            for parent in parents {
                let node = h.store.read().find_path(Path::new(&parent));
                if let Some(node) = node
                    && let Err(e) = server.monitors.rescan(server, &h, node)
                {
                    tracing::warn!("re-reading {parent} after a file operation: {}", e.message);
                }
            }
        }
    }

    fn get(&self, task_id: &str) -> RpcResult<Arc<OpRun>> {
        self.run(task_id)
            .ok_or_else(|| RpcError::invalid_params(tr!(text::UNKNOWN_TASK, id = task_id)))
    }

    pub fn cancel(&self, task_id: &str) -> RpcResult<()> {
        let run = self.get(task_id)?;
        match run.phase() {
            OpPhase::Waiting => {
                run.called_off.store(true, Ordering::SeqCst);
                run.wake.notify_waiters();
                run.wake.notify_one();
            }
            OpPhase::Running => run.cancel.store(true, Ordering::SeqCst),
            _ => {}
        }
        Ok(())
    }

    /// Call off a deletion still waiting, or put what went to the trash back (UI-W5-44).
    pub async fn undo(&self, server: &Arc<Server>, task_id: &str) -> RpcResult<()> {
        let run = self.get(task_id)?;
        let refs = {
            let mut st = run.st.lock();
            match st.phase {
                OpPhase::Waiting => {
                    drop(st);
                    return self.cancel(task_id);
                }
                OpPhase::Done if !st.refs.is_empty() => std::mem::take(&mut st.refs),
                _ => return Ok(()),
            }
        };
        let results = tokio::task::spawn_blocking(move || sk_core::ops::restore(&refs))
            .await
            .map_err(rpc_internal)?;
        for r in results.iter().filter(|r| !r.ok) {
            tracing::warn!("restoring {}: {} ({})", r.path, r.message, r.code.as_str());
        }
        let restored: HashSet<String> = results
            .iter()
            .filter(|r| r.ok)
            .map(|r| r.path.clone())
            .collect();
        let back: Vec<BasketItem> = run
            .st
            .lock()
            .left
            .iter()
            .filter(|it| {
                restored.contains(&it.path) || it.members.iter().any(|m| restored.contains(m))
            })
            .cloned()
            .collect();
        self.basket.reinstate(&back);
        {
            let mut st = run.st.lock();
            st.phase = OpPhase::Undone;
            st.results = results;
        }
        run.emit(true);
        let mut paths: Vec<String> = restored.into_iter().collect();
        paths.sort();
        self.after(server, &run, &paths);
        self.refresh(server).await
    }
}
