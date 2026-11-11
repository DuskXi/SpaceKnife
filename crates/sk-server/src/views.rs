//! Views on the server: ownership, rendering off the async threads, frame encoding, scan-driven
//! pushes (Python `server/views.py`).
//!
//! A view belongs to the session that opened it and dies with it. Its state ([`View`]) sits behind
//! a mutex that only blocking threads take; an async mutex per view serialises the requests on it
//! (one render at a time per view). While a scan runs its views are re-rendered at most 4 times a
//! second and their filters re-evaluated at most once a second.
//!
//! A session gets only the visualizations whose wire layout it negotiated
//! ([`sk_core::view::showable`]): new views and `view.open` fall back to the treemap, `view.set`
//! refuses with INVALID_PARAMS; the same goes for the `branch` colour mode without `visuals.v1`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sk_core::filter::classes::ClassMap;
use sk_core::layout::registry;
use sk_core::settings::{SettingsStore, folder_list};
use sk_core::tr;
use sk_core::view::state::BASELINE;
use sk_core::view::{
    CompareSource, ResultsCache, View, ViewContext, ViewSettings, encode_frame_into, filter_info,
    frame_info, showable, view_state,
};
use sk_protocol::filter_syntax::CompiledVocab;
use sk_protocol::methods::ViewFrame;
use sk_protocol::view::{
    ColorModeName, DRAWLIST_LAYOUT, FLAME_LAYOUT, FrameInfo, POLAR_LAYOUT, ViewFrameEvent,
    ViewOpenParams, ViewSettingsParams, ViewUpdate,
};
use sk_protocol::{CAP_VISUALS, ErrorCode, RequestId};
use sk_store::NodeId;

use crate::app::Server;
use crate::compute::compute;
use crate::rpc::{RpcError, RpcResult};
use crate::session::Session;

/// Between frames pushed during a scan.
pub const PUSH_INTERVAL: Duration = Duration::from_millis(250);
/// Between filter re-evaluations during a scan.
pub const REFILTER_INTERVAL: Duration = Duration::from_secs(1);
pub const MAX_VIEWS: usize = 64;
const LISTENER: &str = "views";

#[derive(Default)]
struct PushState {
    running: bool,
    /// the running push renders the scan's final frame
    is_final: bool,
    /// the scan ended while a progress push was rendering
    final_pending: bool,
    /// file changes arrived while a push was rendering (M5)
    change_pending: bool,
    last_push: Option<Instant>,
    last_refilter: Option<Instant>,
}

/// Node ids moved while the view was busy: applied before its next use.
type IdMap = Arc<dyn Fn(NodeId) -> Option<NodeId> + Send + Sync>;

/// Node ids moved while the view was busy: the mapping and the zoom chain before the move.
struct PendingRemap(IdMap, Vec<NodeId>);

/// One open view.
pub struct ViewHost {
    pub id: String,
    pub scan_id: String,
    pub session_id: String,
    session: Weak<Session>,
    /// the view itself; locked on blocking threads only
    pub view: Arc<Mutex<View>>,
    /// serialises requests and pushes on this view
    pub lock: tokio::sync::Mutex<()>,
    pub seq: AtomicU64,
    pub filter_gen: AtomicU64,
    push: Mutex<PushState>,
    pub results: Arc<Mutex<ResultsCache>>,
    pending: Arc<Mutex<Vec<PendingRemap>>>,
    /// size of the last binary frame: the next one's buffer is allocated that large once
    frame_hint: Arc<AtomicUsize>,
    /// the zoom node and its ancestors at the last render: where a view goes when its zoom
    /// node leaves the tree (a node that goes loses its parent)
    chain: Mutex<Vec<NodeId>>,
}

impl ViewHost {
    pub fn session(&self) -> Option<Arc<Session>> {
        self.session.upgrade()
    }
}

/// Apply remaps queued while the view was busy (call with the view locked).
fn apply_pending(pending: &Mutex<Vec<PendingRemap>>, v: &mut View) {
    let list = std::mem::take(&mut *pending.lock());
    for PendingRemap(f, chain) in list {
        let fallback = fallback_of(&chain, &*f, v.home);
        v.remap(&*f, fallback);
    }
}

/// The nearest node of `chain` still there (else the view's home, else the top).
fn fallback_of(chain: &[NodeId], f: &dyn Fn(NodeId) -> Option<NodeId>, home: NodeId) -> NodeId {
    chain
        .iter()
        .find_map(|&n| f(n))
        .or_else(|| f(home))
        .unwrap_or(0)
}

/// What a view opens on: a scan or a ready comparison.
pub struct Target {
    pub id: String,
    pub ctx: Arc<ViewContext>,
    pub compare: Option<Arc<dyn CompareSource>>,
    pub running: bool,
}

/// The views of every session.
#[derive(Default)]
pub struct ViewManager {
    views: Mutex<HashMap<String, Arc<ViewHost>>>,
    ids: AtomicU64,
}

/// The filter vocabulary of the active class table in the interface language.
pub fn current_vocab() -> Arc<CompiledVocab> {
    let map = ClassMap::active();
    sk_core::filter::vocab::compiled_vocab(
        map.classes(),
        sk_core::classes::version(),
        sk_core::i18n::language().as_str(),
    )
}

/// 「元素比例」 −60…60 → the width / height cells aim for: 0.5 (taller) … 1 (square) … 2 (wider).
pub fn aspect_of(setting: i64) -> f64 {
    2f64.powf(setting as f64 / 60.0)
}

impl ViewManager {
    pub fn new() -> ViewManager {
        ViewManager::default()
    }

    pub fn all(&self) -> Vec<Arc<ViewHost>> {
        self.views.lock().values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.views.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn hosts(&self, scan_id: &str) -> Vec<Arc<ViewHost>> {
        self.views
            .lock()
            .values()
            .filter(|h| h.scan_id == scan_id)
            .cloned()
            .collect()
    }

    pub fn has_views(&self, scan_id: &str) -> bool {
        self.views.lock().values().any(|h| h.scan_id == scan_id)
    }

    /// A view of `session` (UNKNOWN_VIEW for another session's).
    pub fn get(&self, session: &Session, view_id: &str) -> RpcResult<Arc<ViewHost>> {
        self.views
            .lock()
            .get(view_id)
            .filter(|h| h.session_id == session.id)
            .cloned()
            .ok_or_else(|| {
                RpcError::new(ErrorCode::UnknownView, format!("unknown view: {view_id}"))
            })
    }

    /// Any session's view.
    pub fn lookup(&self, view_id: &str) -> Option<Arc<ViewHost>> {
        self.views.lock().get(view_id).cloned()
    }

    /// `host` is still open (not closed, nor its session or its scan).
    fn is_open(&self, host: &Arc<ViewHost>) -> bool {
        self.views
            .lock()
            .get(&host.id)
            .is_some_and(|h| Arc::ptr_eq(h, host))
    }

    pub fn target(&self, server: &Server, id: &str) -> RpcResult<Target> {
        if let Some(h) = server.scans.lookup(id) {
            return Ok(Target {
                id: h.id.clone(),
                ctx: h.ctx.clone(),
                compare: None,
                running: h.running(),
            });
        }
        if let Some((ctx, source)) = server.compares.view_target(id) {
            return Ok(Target {
                id: id.to_owned(),
                ctx,
                compare: Some(source),
                running: false,
            });
        }
        Err(RpcError::new(
            ErrorCode::UnknownScan,
            format!("unknown scan: {id}"),
        ))
    }

    /// Make sure the scan tells the views its status (once per scan).
    fn listen(&self, server: &Arc<Server>, scan_id: &str) {
        let Some(h) = server.scans.lookup(scan_id) else {
            return;
        };
        let weak = Arc::downgrade(server);
        let id = scan_id.to_owned();
        h.add_listener(
            LISTENER,
            Arc::new(move |status| {
                if let Some(server) = weak.upgrade() {
                    server.views.on_scan(&server, &id, status.state);
                }
            }),
        );
    }

    pub fn open(
        &self,
        server: &Arc<Server>,
        session: &Arc<Session>,
        params: &ViewOpenParams,
    ) -> RpcResult<Arc<ViewHost>> {
        if self.len() >= MAX_VIEWS {
            return Err(RpcError::invalid_request(tr!(
                "打开的视图太多了，先关掉一些标签或窗格"
            )));
        }
        let t = self.target(server, &params.scan_id)?;
        let caps = session.capabilities();
        let compare = t.compare.is_some();
        let settings = new_view_settings(&server.settings, caps, compare);
        let mut view = View::new(
            &format!("view-{}", self.ids.fetch_add(1, Ordering::SeqCst) + 1),
            t.ctx.clone(),
            settings,
        )
        .ok_or_else(|| RpcError::internal("no such visualization"))?;
        view.set_viewport(
            params.viewport.width,
            params.viewport.height,
            params.viewport.dpr,
        );
        view.scanning = t.running && marks_scanning(&server.settings);
        view.compare = t.compare.clone(); // before the settings: the direction only applies to compare views
        if let Some(s) = &params.settings {
            apply_settings(&mut view, &fall_back(s, caps, compare), caps)?;
        }
        let host = Arc::new(ViewHost {
            id: view.id.clone(),
            scan_id: t.id.clone(),
            session_id: session.id.clone(),
            session: Arc::downgrade(session),
            view: Arc::new(Mutex::new(view)),
            lock: tokio::sync::Mutex::new(()),
            seq: AtomicU64::new(0),
            filter_gen: AtomicU64::new(0),
            push: Mutex::new(PushState::default()),
            results: Arc::new(Mutex::new(ResultsCache::default())),
            pending: Arc::new(Mutex::new(Vec::new())),
            frame_hint: Arc::new(AtomicUsize::new(0)),
            chain: Mutex::new(Vec::new()),
        });
        self.views.lock().insert(host.id.clone(), host.clone());
        if !compare {
            self.listen(server, &t.id);
        }
        Ok(host)
    }

    pub fn close(&self, view_id: &str) {
        let host = self.views.lock().remove(view_id);
        if let Some(h) = host
            && !self.has_views(&h.scan_id)
        {
            self.release_listener(&h.scan_id);
        }
    }

    fn release_listener(&self, _scan_id: &str) {
        // the listener stays harmless without views (it finds none); it goes with the scan
    }

    /// The scan goes (eviction): forget its listener.
    pub fn release(&self, scan_id: &str) {
        self.release_listener(scan_id);
    }

    pub fn close_session(&self, session_id: &str) {
        self.views.lock().retain(|_, h| h.session_id != session_id);
    }

    pub fn close_scan(&self, scan_id: &str) {
        self.views.lock().retain(|_, h| h.scan_id != scan_id);
    }

    // ------------------------------------------------------------- rendering

    /// Run `action` on the view (on a blocking thread), render when it changed something (or
    /// `force`), queue the binary frame, and answer with the view's update.
    pub async fn act<F>(
        &self,
        host: &Arc<ViewHost>,
        request: Option<RequestId>,
        action: F,
    ) -> RpcResult<ViewUpdate>
    where
        F: FnOnce(&mut View) -> RpcResult<(bool, &'static str)> + Send + 'static,
    {
        let _guard = host.lock.lock().await;
        let h = host.clone();
        let (update, frame) = compute(move || {
            let mut v = h.view.lock();
            apply_pending(&h.pending, &mut v);
            let (changed, reason) = action(&mut v)?;
            let (info, frame) = if changed {
                let (info, buf) = render_locked(&h, &mut v, reason, request.as_ref());
                (Some(info), Some(buf))
            } else {
                (None, None)
            };
            Ok::<_, RpcError>((
                ViewUpdate {
                    state: view_state(&v, &h.scan_id),
                    changed,
                    frame: info,
                    filter: filter_info(&v, None),
                },
                frame,
            ))
        })
        .await??;
        if let (Some(buf), Some(s)) = (frame, host.session()) {
            s.send_binary(buf);
        }
        Ok(update)
    }

    /// Render a frame for a push (the caller holds `host.lock`); the binary frame is queued.
    /// `None`: the view was closed before or while it rendered, nothing of it is sent (0.3.4
    /// cancelled its push).
    async fn render_push(
        &self,
        host: &Arc<ViewHost>,
        reason: &'static str,
        refilter: bool,
    ) -> RpcResult<Option<FrameInfo>> {
        if !self.is_open(host) {
            return Ok(None);
        }
        let h = host.clone();
        let (info, buf) = compute(move || {
            let mut v = h.view.lock();
            apply_pending(&h.pending, &mut v);
            if refilter && v.filter.active() {
                v.refilter(None);
            }
            render_locked(&h, &mut v, reason, None)
        })
        .await?;
        if !self.is_open(host) {
            return Ok(None);
        }
        if let Some(s) = host.session() {
            s.send_binary(buf);
        }
        Ok(Some(info))
    }

    /// Render and push a frame without a request (settings, tags, classes, changes).
    pub async fn repaint(&self, host: &Arc<ViewHost>, reason: &'static str, refilter: bool) {
        let _guard = host.lock.lock().await;
        match self.render_push(host, reason, refilter).await {
            Ok(Some(info)) => {
                if let Some(s) = host.session() {
                    s.emit::<ViewFrame>(&ViewFrameEvent { frame: info });
                }
            }
            Ok(None) => {}
            Err(e) => tracing::error!("repainting {} failed: {e}", host.id),
        }
    }

    fn spawn_repaint(
        &self,
        server: &Arc<Server>,
        host: &Arc<ViewHost>,
        reason: &'static str,
        refilter: bool,
    ) {
        let (srv, h) = (server.clone(), host.clone());
        server
            .rt
            .spawn(async move { srv.views.repaint(&h, reason, refilter).await });
    }

    // ------------------------------------------------------------- settings

    /// Geometry settings re-lay out every open view at once (UI-W7-32); the flame graph's
    /// direction the views showing a flame graph.
    pub fn apply_settings_live(&self, server: &Arc<Server>, st: &SettingsStore) {
        let min_element = setting_f64(st, "treemap.min_element");
        let sort_by_size = st.get_bool("treemap.sort_by_size");
        let font_scale = st.get_int("treemap.font_scale") as f64 / 100.0;
        let aspect = aspect_of(st.get_int("treemap.aspect"));
        let origin = st.get_str("treemap.flame_origin");
        let hidden = folder_list(&st.get_str("treemap.hidden_folders"));
        for host in self.all() {
            let changed = {
                let Some(mut v) = host.view.try_lock_for(Duration::from_millis(200)) else {
                    continue;
                };
                // 「隐藏的文件夹」: the picture changes when any is shown (or would be hidden)
                let hidden_changed = v.set_hidden_folders(hidden.clone());
                let mut changed = (v.min_element, v.sort_by_size, v.font_scale, v.aspect)
                    != (min_element, sort_by_size, font_scale, aspect)
                    || hidden_changed;
                v.min_element = min_element;
                v.sort_by_size = sort_by_size;
                v.font_scale = font_scale;
                v.aspect = aspect;
                let flame = vec![(
                    "origin".to_owned(),
                    sk_core::layout::api::OptValue::Str(origin.clone()),
                )];
                if v.visual_options.get("flame") != Some(&flame) {
                    v.visual_options.insert("flame".to_owned(), flame);
                    changed = changed || v.visual == "flame";
                }
                changed
            };
            if changed {
                self.spawn_repaint(server, &host, "settings", false);
            }
        }
    }

    /// The filter vocabulary of every scan follows the class table and the language.
    pub fn refresh_vocab(&self, server: &Server) {
        let vocab = current_vocab();
        for h in server.scans.list() {
            *h.ctx.vocab.write() = vocab.clone();
        }
        for ctx in server.compares.contexts() {
            *ctx.vocab.write() = vocab.clone();
        }
    }

    /// 「清除所有已保存的标签」: every scan's tags go; the views redraw without them.
    pub async fn forget_tags(&self, server: &Arc<Server>) {
        for h in server.scans.list() {
            h.ctx.tags.lock().forget();
        }
        for host in self.all() {
            self.repaint(&host, "tags", true).await;
        }
    }

    /// The file class table changed: every view re-runs its filter (`:class:`) and recolours.
    pub async fn reclassify(&self, server: &Arc<Server>) {
        self.refresh_vocab(server);
        for host in self.all() {
            self.repaint(&host, "classes", true).await;
        }
    }

    // ------------------------------------------------------------- file changes (M5)

    /// Node ids changed (call with the store's writer held: never lock the store here). Views
    /// not busy follow at once; busy ones before their next use.
    pub fn remap(
        &self,
        scan_id: &str,
        now: &dyn Fn(NodeId) -> Option<NodeId>,
        later: &dyn Fn() -> IdMap,
    ) {
        for host in self.hosts(scan_id) {
            let chain = host.chain.lock().clone();
            match host.view.try_lock() {
                Some(mut v) if host.pending.lock().is_empty() => {
                    let fallback = fallback_of(&chain, now, v.home);
                    v.remap(now, fallback);
                    *host.chain.lock() = chain.iter().filter_map(|&n| now(n)).collect();
                }
                _ => host.pending.lock().push(PendingRemap(later(), chain)),
            }
        }
    }

    /// The tree changed: views flash what changed (setting 「元素变化闪烁」) and get a new frame.
    pub fn flash(&self, server: &Arc<Server>, scan_id: &str, changes: &HashMap<NodeId, i64>) {
        server.compares.on_tree_changed(server, scan_id); // live comparisons follow (UI-W6-38)
        server.ops.on_tree_changed(server, scan_id); // basket sizes and missing items (UI-W5-48)
        let flash_on = server.settings.get_bool("treemap.flash_changes");
        for host in self.hosts(scan_id) {
            if flash_on && !changes.is_empty() {
                // add_flash only takes the view's flash list lock
                if let Some(v) = host.view.try_lock_for(Duration::from_millis(500)) {
                    v.add_flash(changes);
                }
            }
            self.push_change(server, &host);
        }
    }

    /// The free space of the scan's volumes moved: free and unknown space are drawn again.
    pub fn volumes_changed(&self, server: &Arc<Server>, scan_id: &str) {
        for host in self.hosts(scan_id) {
            self.push_change(server, &host);
        }
    }

    fn push_change(&self, server: &Arc<Server>, host: &Arc<ViewHost>) {
        {
            let mut p = host.push.lock();
            if p.running {
                p.change_pending = true; // the running push renders once more
                return;
            }
            p.running = true;
            p.is_final = false;
        }
        self.spawn_push(server, host, false, "change");
    }

    /// A live comparison was computed again: its views move to the new change tree.
    pub async fn replace_store(
        &self,
        server: &Arc<Server>,
        compare_id: &str,
        ctx: Arc<ViewContext>,
        source: Arc<dyn CompareSource>,
    ) {
        for host in self.hosts(compare_id) {
            {
                let _g = host.lock.lock().await;
                let (h, c, s) = (host.clone(), ctx.clone(), source.clone());
                let _ = compute(move || {
                    let mut v = h.view.lock();
                    h.pending.lock().clear();
                    v.rebase(c, Some(s));
                })
                .await;
            }
            let _ = server;
            self.repaint(&host, "change", false).await;
        }
    }

    // ------------------------------------------------------------- scan-driven pushes

    /// A scan published its status: views mark the folders still being listed and get frames
    /// (at most every [`PUSH_INTERVAL`] while it runs, once at the end).
    pub fn on_scan(
        &self,
        server: &Arc<Server>,
        scan_id: &str,
        state: sk_protocol::scan::ScanStateName,
    ) {
        use sk_protocol::scan::ScanStateName as S;
        let is_final = matches!(state, S::Done | S::Cancelled | S::Failed);
        let marks = state == S::Scanning && marks_scanning(&server.settings);
        for host in self.hosts(scan_id) {
            if let Some(mut v) = host.view.try_lock() {
                v.scanning = marks;
            }
            {
                let mut p = host.push.lock();
                if p.running {
                    if is_final && !p.is_final {
                        p.final_pending = true; // the running push renders the final frame next
                    }
                    continue;
                }
                if !is_final && p.last_push.is_some_and(|t| t.elapsed() < PUSH_INTERVAL) {
                    continue;
                }
                p.running = true;
                p.is_final = is_final;
            }
            self.spawn_push(server, &host, is_final, "");
        }
    }

    fn spawn_push(
        &self,
        server: &Arc<Server>,
        host: &Arc<ViewHost>,
        is_final: bool,
        reason: &'static str,
    ) {
        let (srv, h) = (server.clone(), host.clone());
        server
            .rt
            .spawn(async move { srv.views.push(&h, is_final, reason).await });
    }

    async fn push(&self, host: &Arc<ViewHost>, mut is_final: bool, mut reason: &'static str) {
        loop {
            {
                let _guard = host.lock.lock().await;
                let refilter = {
                    let p = host.push.lock();
                    is_final
                        || p.last_refilter
                            .is_none_or(|t| t.elapsed() >= REFILTER_INTERVAL)
                };
                let why = if !reason.is_empty() {
                    reason
                } else if is_final {
                    "scan-final"
                } else {
                    "scan"
                };
                if refilter {
                    host.push.lock().last_refilter = Some(Instant::now());
                }
                if is_final && let Some(mut v) = host.view.try_lock() {
                    v.scanning = false;
                }
                match self.render_push(host, why, refilter).await {
                    Ok(Some(info)) => {
                        host.push.lock().last_push = Some(Instant::now());
                        if let Some(s) = host.session() {
                            s.emit::<ViewFrame>(&ViewFrameEvent { frame: info });
                        }
                    }
                    Ok(None) => {
                        // closed: what was still pending for it goes too
                        let mut p = host.push.lock();
                        p.final_pending = false;
                        p.change_pending = false;
                        p.running = false;
                        return;
                    }
                    Err(e) => tracing::error!("pushing a frame of {} failed: {e}", host.id),
                }
            }
            let mut p = host.push.lock();
            if p.final_pending {
                p.final_pending = false;
                p.is_final = true;
                is_final = true;
                reason = "";
            } else if p.change_pending {
                p.change_pending = false;
                reason = "change";
            } else {
                p.running = false;
                return;
            }
        }
    }
}

/// Render the view (locked) and encode its binary frame: (FrameInfo, frame bytes).
fn render_locked(
    h: &ViewHost,
    v: &mut View,
    reason: &str,
    request: Option<&RequestId>,
) -> (FrameInfo, Vec<u8>) {
    let seq = h.seq.fetch_add(1, Ordering::SeqCst) + 1;
    let frame = v.render();
    let mut buf = Vec::with_capacity(h.frame_hint.load(Ordering::Relaxed));
    if let Err(e) = encode_frame_into(frame, &h.id, seq, request, &mut buf) {
        tracing::error!("encoding a frame of {} failed: {e}", h.id);
    }
    h.frame_hint.store(buf.len(), Ordering::Relaxed);
    {
        let nodes = v.ctx.store.read();
        let z = v.zoom.node;
        *h.chain.lock() = if (z as usize) < nodes.len() {
            std::iter::once(z).chain(nodes.ancestors(z)).collect()
        } else {
            Vec::new()
        };
    }
    let frame = v.last.as_ref().expect("just rendered");
    (frame_info(v, frame, seq, reason), buf)
}

/// Folders still being scanned breathe (UI-TM-42): the 「元素变化闪烁」 setting covers it.
fn marks_scanning(st: &SettingsStore) -> bool {
    st.get_bool("treemap.flash_changes")
}

/// Defaults of a new view from the settings (W7 树图 · 外观 · 可视化) for a session that announced
/// `caps`: a visualization it can't show falls back to the treemap, the `branch` colour mode
/// without `visuals.v1` to class colouring.
pub fn new_view_settings(st: &SettingsStore, caps: &[String], compare: bool) -> ViewSettings {
    let full = caps.iter().any(|c| c == CAP_VISUALS);
    let mode = |key: &str| {
        let v = ColorModeName::from_str(&st.get_str(key)).unwrap_or(ColorModeName::Class);
        if v == ColorModeName::Branch && !full {
            ColorModeName::Class
        } else {
            v
        }
    };
    let tree = mode("appearance.default_color_mode");
    let part = mode("appearance.partition_color_mode");
    let mut colors = BTreeMap::new();
    for v in registry::all_visuals() {
        if v.layout == DRAWLIST_LAYOUT {
            colors.insert(v.id.clone(), tree);
        } else if v.layout == FLAME_LAYOUT || v.layout == POLAR_LAYOUT {
            colors.insert(v.id.clone(), part);
        }
    }
    let visual = match showable(&st.get_str("appearance.default_visual"), caps) {
        Some(vis) if vis.compare || !compare => vis.id,
        _ => BASELINE.to_owned(),
    };
    let color_mode = colors.get(&visual).copied().unwrap_or_else(|| {
        registry::get(&visual).map_or(ColorModeName::Class, |v| v.default_color)
    });
    ViewSettings {
        detail: st.get_int("treemap.initial_detail").clamp(1, 15) as u32,
        min_element: setting_f64(st, "treemap.min_element"),
        sort_by_size: st.get_bool("treemap.sort_by_size"),
        show_free: st.get_bool("treemap.show_free"),
        show_unknown: st.get_bool("treemap.show_unknown"),
        show_trash: st.get_bool("treemap.show_trash"),
        hidden_folders: folder_list(&st.get_str("treemap.hidden_folders")),
        visual,
        color_mode,
        colors,
        font_scale: st.get_int("treemap.font_scale") as f64 / 100.0,
        aspect: aspect_of(st.get_int("treemap.aspect")),
        flame_origin: st.get_str("treemap.flame_origin"),
        ..ViewSettings::default()
    }
}

/// `view.open`: a visualization the session can't show becomes the treemap; the `branch` colour
/// mode without `visuals.v1` is left out (the view keeps its default).
pub fn fall_back(s: &ViewSettingsParams, caps: &[String], compare: bool) -> ViewSettingsParams {
    let mut out = s.clone();
    let full = caps.iter().any(|c| c == CAP_VISUALS);
    if let Some(v) = &s.visual {
        match showable(v, caps) {
            Some(vis) if !compare || vis.compare => {}
            _ => out.visual = Some(BASELINE.to_owned()),
        }
    }
    if s.color_mode == Some(ColorModeName::Branch) && !full {
        out.color_mode = None;
    }
    if let Some(colors) = &s.colors
        && !full
    {
        out.colors = Some(
            colors
                .iter()
                .filter(|(_, m)| **m != ColorModeName::Branch)
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
        );
    }
    out
}

/// Apply what `s` sets. A visualization or colour mode the session can't show is INVALID_PARAMS,
/// before anything changes. The visualization changes before the colour mode, which belongs to it.
pub fn apply_settings(view: &mut View, s: &ViewSettingsParams, caps: &[String]) -> RpcResult<()> {
    if let Some(v) = &s.visual {
        match showable(v, caps) {
            Some(vis) if view.compare.is_none() || vis.compare => {}
            _ => return Err(RpcError::invalid_params(tr!("这种可视化在当前界面不可用"))),
        }
    }
    let full = caps.iter().any(|c| c == CAP_VISUALS);
    if !full
        && (s.color_mode == Some(ColorModeName::Branch)
            || s.colors
                .as_ref()
                .is_some_and(|c| c.values().any(|m| *m == ColorModeName::Branch)))
    {
        return Err(RpcError::invalid_params(tr!("这种配色在当前界面不可用")));
    }
    if let Some(d) = s.detail {
        view.set_detail(d);
    }
    if let Some(m) = s.min_element {
        view.min_element = m;
    }
    if let Some(b) = s.sort_by_size {
        view.sort_by_size = b;
    }
    if let Some(b) = s.show_free {
        view.show_free = b;
    }
    if let Some(b) = s.show_unknown {
        view.show_unknown = b;
    }
    if let Some(b) = s.show_trash {
        view.show_trash = b;
    }
    if let Some(b) = s.hide_folders {
        view.hide_folders = b;
    }
    if let Some(c) = &s.colors {
        view.set_colors(c);
    }
    if let Some(v) = &s.visual {
        view.set_visual(v);
    }
    if let Some(m) = s.color_mode {
        view.set_color_mode(m);
    }
    if let Some(m) = s.filter_mode {
        view.filter_mode = m;
    }
    if let Some(vp) = &s.viewport {
        view.set_viewport(vp.width, vp.height, vp.dpr);
    }
    if let Some(d) = s.direction
        && view.compare.is_some()
    {
        view.direction = d;
    }
    Ok(())
}

/// What `view.set` can change: a frame is sent when any of it did.
pub fn settings_key(v: &View) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{:?}|{}",
        v.detail,
        v.min_element,
        v.sort_by_size,
        v.show_free,
        v.show_unknown,
        v.show_trash,
        v.hide_folders,
        v.visual,
        v.color_mode().as_str(),
        v.filter_mode.as_str(),
        v.viewport,
        v.direction.as_str()
    )
}

/// A number setting as f64 (int or float).
pub fn setting_f64(st: &SettingsStore, key: &str) -> f64 {
    match st.get(key) {
        Some(sk_protocol::settings::SettingValue::Float(f)) => f,
        Some(sk_protocol::settings::SettingValue::Int(i)) => i as f64,
        _ => 0.0,
    }
}
