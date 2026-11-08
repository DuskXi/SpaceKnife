//! Live change monitoring of open scans (M5 file change hooks; the server half of Python
//! `server/monitors.py`, the pipeline itself is [`sk_core::monitor::pipeline::ScanMonitor`]).
//!
//! One monitor per scan (not for opened snapshots), driven on a thread of its own. The monitor
//! sits behind a mutex so that its status can be read meanwhile; [`ServerHooks`] lets the views
//! follow moved node ids (with the store's writer held: views not busy follow at once, busy ones
//! before their next use) and flash what changed.
//!
//! As in 0.3.4, the scan's status is pushed (`scan.progress`) after every batch of changes
//! applied, after a moved free space and once the watchers of a finished scan started: the status
//! bar reads the monitor's state and the counts only from these pushes. A new scan waits for its
//! watchers ([`MonitorManager::wait_started`]): changes count from the scan's start, and its
//! final status names the running monitor.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use sk_core::events::{self, EventLevel, Source};
use sk_core::monitor::pipeline::{
    IdRemap, MAX_WAIT, MonitorHandle, MonitorHooks, MonitorOptions, ScanMonitor,
};
use sk_core::scan::RootInfo;
use sk_core::tr;
use sk_protocol::ErrorCode;
use sk_protocol::scan::{MonitorStatus, ScanMethodUsed, ScanSource};
use sk_store::{NodeId, Nodes};

use crate::app::Server;
use crate::rpc::{RpcError, RpcResult};
use crate::scans::ScanHandle;

struct Mon {
    monitor: Mutex<ScanMonitor>,
    handle: MonitorHandle,
    /// the last status read (while the monitor is busy); «starting» until its watchers started
    status: Mutex<MonitorStatus>,
    /// the watchers started (or failed to)
    started: Mutex<bool>,
    started_cv: Condvar,
}

/// How long a new scan waits for its watchers at most (a watch of a network share may hang).
const START_WAIT: Duration = Duration::from_secs(10);

/// Starts and stops the monitors of open scans.
#[derive(Default)]
pub struct MonitorManager {
    monitors: Mutex<HashMap<String, Arc<Mon>>>,
    /// rescans of scans that are not watched (live monitoring off)
    passive: Mutex<HashMap<String, Arc<Mon>>>,
}

/// What the pipeline tells the rest of the server.
struct ServerHooks {
    server: Weak<Server>,
    scan: Weak<ScanHandle>,
    scan_id: String,
    /// changes were applied or the free space moved since the last push of the scan's status
    publish: bool,
}

impl ServerHooks {
    fn method(&self) -> Option<ScanMethodUsed> {
        let h = self.scan.upgrade()?;
        let st = h.st.lock();
        st.running.then_some(st.method)
    }

    /// Views follow moved ids: `now` with the writer's view, `later` (a closure taking its own
    /// reader) for views busy right now.
    fn remap_views(
        &self,
        now: &dyn Fn(NodeId) -> Option<NodeId>,
        later: &dyn Fn() -> Arc<dyn Fn(NodeId) -> Option<NodeId> + Send + Sync>,
    ) {
        if let Some(server) = self.server.upgrade() {
            server.views.remap(&self.scan_id, now, later);
        }
    }
}

impl MonitorHooks for ServerHooks {
    fn scan_running(&self) -> bool {
        self.method() == Some(ScanMethodUsed::Hard)
    }

    fn mft_running(&self) -> bool {
        self.method() == Some(ScanMethodUsed::Mft)
    }

    fn remapped(&mut self, nodes: &Nodes, remap: &IdRemap) {
        let Some(h) = self.scan.upgrade() else { return };
        let store = h.store.clone();
        let shared = Arc::new(remap.clone());
        self.remap_views(&|id| remap.map(nodes, id), &|| {
            let (store, remap) = (store.clone(), shared.clone());
            Arc::new(move |id| remap.map(&store.read(), id))
        });
    }

    fn compacted(&mut self, _nodes: &Nodes, remap: &[i32]) {
        let map = |id: NodeId| {
            remap
                .get(id as usize)
                .copied()
                .filter(|&n| n >= 0)
                .map(|n| n as NodeId)
        };
        let owned: std::cell::OnceCell<Arc<Vec<i32>>> = std::cell::OnceCell::new();
        self.remap_views(&map, &|| {
            let v = owned.get_or_init(|| Arc::new(remap.to_vec())).clone();
            Arc::new(move |id| {
                v.get(id as usize)
                    .copied()
                    .filter(|&n| n >= 0)
                    .map(|n| n as NodeId)
            })
        });
        if let Some(h) = self.scan.upgrade() {
            h.ctx.names.forget();
        }
    }

    fn applied(&mut self, flashes: &HashMap<NodeId, i64>) {
        if let Some(server) = self.server.upgrade() {
            server.views.flash(&server, &self.scan_id, flashes);
        }
        // pushed once the step is through and the monitor's lock is free (its counts are new)
        self.publish = true;
    }

    fn volumes_changed(&mut self, roots: &[RootInfo]) {
        let (Some(server), Some(h)) = (self.server.upgrade(), self.scan.upgrade()) else {
            return;
        };
        {
            let mut mine = h.roots.write();
            for r in roots {
                if let Some(m) = mine
                    .iter_mut()
                    .find(|m| m.node == r.node || m.path == r.path)
                {
                    m.volume = r.volume.clone();
                }
            }
            let mut ctx_roots = h.ctx.roots.write();
            for (cr, r) in ctx_roots.iter_mut().zip(mine.iter()) {
                cr.free = r.volume.as_ref().map(|v| v.free);
                cr.used = r.volume.as_ref().map(sk_core::volume::VolumeInfo::used);
            }
        }
        server.views.volumes_changed(&server, &self.scan_id);
        self.publish = true; // the status bar's free space
    }

    fn message(&mut self, text: String) {
        events::emit(
            Source::Monitor,
            &text,
            EventLevel::Info,
            Some(&self.scan_id),
            None,
        );
    }
}

fn coalesce(server: &Server) -> Duration {
    let ms = server.settings.get_int("scan.coalesce_ms");
    Duration::from_secs_f64((ms as f64 / 1000.0).max(0.02))
}

impl MonitorManager {
    pub fn new() -> MonitorManager {
        MonitorManager::default()
    }

    fn enabled(server: &Server) -> bool {
        server.settings.get_bool("scan.monitor")
    }

    /// A monitor for `h`, not started yet ([`MonitorManager::launch`]).
    fn prepare(server: &Arc<Server>, h: &Arc<ScanHandle>, watch: bool) -> Option<Arc<Mon>> {
        let scanner = h.scanner.as_ref()?;
        let opts = MonitorOptions {
            watch,
            coalesce: coalesce(server),
            hard_rule: h.st.lock().method == ScanMethodUsed::Hard,
            gate: h
                .governor
                .clone()
                .map(|g| Arc::new(g) as Arc<dyn sk_core::scan::IoGate>),
            usn: if cfg!(windows) {
                Some(server.scans.helper.clone() as Arc<dyn sk_core::monitor::pipeline::UsnSource>)
            } else {
                None
            },
            // changes the scan's listings may have missed count from its start (None: not started
            // yet, the monitor's own start is earlier than any listing)
            listed_after: scanner
                .progress()
                .started_at
                .map(std::time::SystemTime::from),
            ..MonitorOptions::default()
        };
        // relists list like the scan did (root path, name check, Windows allocation option)
        let monitor = ScanMonitor::new(
            h.store.clone(),
            h.roots.read().clone(),
            scanner.root_options(),
            opts,
        );
        let handle = monitor.handle();
        let status = monitor.status(); // «starting»: no watchers yet
        Some(Arc::new(Mon {
            monitor: Mutex::new(monitor),
            handle,
            status: Mutex::new(status),
            started: Mutex::new(false),
            started_cv: Condvar::new(),
        }))
    }

    /// Run `mon` (in its map already: what it publishes names it) on a thread of its own: start
    /// its watchers, then wait for changes and apply them until it is stopped.
    fn launch(server: &Arc<Server>, h: &Arc<ScanHandle>, mon: &Arc<Mon>, watch: bool) -> bool {
        let mut hooks = ServerHooks {
            server: Arc::downgrade(server),
            scan: Arc::downgrade(h),
            scan_id: h.id.clone(),
            publish: false,
        };
        let m = mon.clone();
        let wait = coalesce(server);
        let publish = move |hooks: &ServerHooks| {
            if let (Some(server), Some(h)) = (hooks.server.upgrade(), hooks.scan.upgrade()) {
                h.publish(&server);
            }
        };
        std::thread::Builder::new()
            .name(format!("monitor-{}", h.id))
            .spawn(move || {
                {
                    let mut g = m.monitor.lock();
                    g.start();
                    *m.status.lock() = g.status();
                }
                *m.started.lock() = true;
                m.started_cv.notify_all();
                // live monitoring switched on for a finished scan: the status bar learns it now (a
                // running scan's next tick or final status says it)
                if watch && hooks.scan.upgrade().is_some_and(|h| !h.running()) {
                    publish(&hooks);
                }
                if cfg!(windows) {
                    // the checkpoint may wait for the helper (reading this very scan's MFT)
                    m.monitor.lock().start_usn();
                }
                let h = m.handle.clone();
                while !h.stopped() {
                    let timeout = m.monitor.lock().timeout(&hooks);
                    h.wait(timeout);
                    let first = Instant::now();
                    while first.elapsed() < MAX_WAIT && !h.stopped() {
                        if !h.wait(Some(wait)) {
                            break; // quiet: apply
                        }
                    }
                    if h.stopped() {
                        break;
                    }
                    {
                        let mut g = m.monitor.lock();
                        g.step(&mut hooks);
                        *m.status.lock() = g.status();
                    }
                    if std::mem::take(&mut hooks.publish) && !h.stopped() {
                        publish(&hooks);
                    }
                }
                m.monitor.lock().stop();
            })
            .is_ok()
    }

    /// Watch a scan from its start (UI-W2-09); the scan's thread waits for the watchers
    /// ([`MonitorManager::wait_started`]).
    pub fn start(&self, server: &Arc<Server>, h: &Arc<ScanHandle>) {
        if h.source != ScanSource::Scan
            || self.monitors.lock().contains_key(&h.id)
            || !Self::enabled(server)
        {
            return;
        }
        let Some(mon) = Self::prepare(server, h, true) else {
            return;
        };
        {
            let mut map = self.monitors.lock();
            if map.contains_key(&h.id) {
                return; // started meanwhile (the scan's start and the switch at once)
            }
            map.insert(h.id.clone(), mon.clone());
        }
        if !Self::launch(server, h, &mon, true) {
            self.monitors.lock().remove(&h.id);
        }
    }

    /// Wait (at most [`START_WAIT`]) until the watchers of the scan's monitor started, as 0.3.4's
    /// scan did before listing anything; at once without a monitor.
    pub fn wait_started(&self, scan_id: &str) {
        let Some(m) = self.find(scan_id) else { return };
        let deadline = Instant::now() + START_WAIT;
        let mut started = m.started.lock();
        while !*started {
            if m.started_cv.wait_until(&mut started, deadline).timed_out() {
                tracing::warn!("the watchers of scan {scan_id} are slow to start");
                return;
            }
        }
    }

    pub fn stop(&self, scan_id: &str) {
        for map in [&self.passive, &self.monitors] {
            if let Some(m) = map.lock().remove(scan_id) {
                m.handle.stop();
            }
        }
    }

    pub fn close(&self) {
        let ids: Vec<String> = self
            .monitors
            .lock()
            .keys()
            .chain(self.passive.lock().keys())
            .cloned()
            .collect();
        for id in ids {
            self.stop(&id);
        }
    }

    fn find(&self, scan_id: &str) -> Option<Arc<Mon>> {
        self.monitors.lock().get(scan_id).cloned()
    }

    /// The monitor's status (its last known one while it is busy).
    pub fn status(&self, scan_id: &str) -> Option<MonitorStatus> {
        let m = self.find(scan_id)?;
        if let Some(g) = m.monitor.try_lock() {
            let s = g.status();
            *m.status.lock() = s.clone();
            return Some(s);
        }
        Some(m.status.lock().clone())
    }

    /// The scan's last folders get their watches now.
    pub fn kick(&self, scan_id: &str) {
        if let Some(m) = self.find(scan_id) {
            m.handle.kick();
        }
    }

    /// The whole tree was replaced (an MFT scan finished): every id is new.
    pub fn rebuilt(&self, scan_id: &str) {
        if let Some(m) = self.find(scan_id) {
            m.monitor.lock().rebuilt();
        }
    }

    /// Read a folder again with everything below it (F5); with live monitoring off a monitor
    /// without watchers does it. Node 0, the top of a scan of several roots, reads every root.
    pub fn rescan(&self, server: &Arc<Server>, h: &Arc<ScanHandle>, node: NodeId) -> RpcResult<()> {
        if h.running() {
            return Err(RpcError::new(
                ErrorCode::ScanBusy,
                tr!("扫描进行中，完成后再重新扫描"),
            ));
        }
        if h.source != ScanSource::Scan {
            return Err(RpcError::invalid_request(tr!("快照不能重新扫描")));
        }
        let mon = match self
            .find(&h.id)
            .or_else(|| self.passive.lock().get(&h.id).cloned())
        {
            Some(m) => m,
            None => {
                let m = Self::prepare(server, h, false)
                    .ok_or_else(|| RpcError::internal("cannot start a rescan"))?;
                self.passive.lock().insert(h.id.clone(), m.clone());
                if !Self::launch(server, h, &m, false) {
                    self.passive.lock().remove(&h.id);
                    return Err(RpcError::internal("cannot start a rescan"));
                }
                m
            }
        };
        let paths: Vec<std::path::PathBuf> = {
            let nodes = h.store.read();
            if node == 0 {
                h.roots.read().iter().map(|r| nodes.path(r.node)).collect()
            } else {
                vec![nodes.path(node)]
            }
        };
        for p in paths {
            mon.handle.rescan(&p);
        }
        Ok(())
    }

    /// `scan.monitor` switched: start or stop monitoring every open scan.
    pub fn apply_settings(&self, server: &Arc<Server>) {
        let on = Self::enabled(server);
        let running = !self.monitors.lock().is_empty();
        if on {
            for h in server.scans.list() {
                self.start(server, &h);
            }
        } else if running {
            let ids: Vec<String> = self.monitors.lock().keys().cloned().collect();
            for id in ids {
                self.stop(&id);
            }
            for h in server.scans.list() {
                h.publish(server);
            }
        }
    }
}
