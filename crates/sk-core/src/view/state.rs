//! View state machine (Python `view/state.py`): one tab or split pane looking at one scan
//! (UI-TM-33…38, UI-TM-55, W3, W8).
//!
//! Each view owns its zoom root, back/forward history (every entry remembers its selection),
//! detail level, viewport, free/unknown switches, visualization, colour mode (one per
//! visualization), filter and selection. Views of the same scan share a [`ViewContext`]: the
//! store, the layout basis, the name index, the tags and the trash cache. Rendering returns a
//! [`Frame`] — the draw list plus per-block flags, tag, time tone, branch colour, the zoom root,
//! the selected row and the breadcrumb — that [`super::frame`] turns into one binary frame.
//!
//! Locking: every call that reads the store takes one reader guard of its own; never call a view
//! method while the same thread holds a guard of the view's store.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use sk_protocol::CAP_VISUALS;
use sk_protocol::compare::CompareDirection;
use sk_protocol::filter::{FilterDiagnostic, FilterNormal, TagColor};
use sk_protocol::filter_syntax::CompiledVocab;
use sk_protocol::view::{ColorModeName, DRAWLIST_LAYOUT, FilterModeName, ResultsSortName};
use sk_store::{Flags, NodeId, Nodes, Store};

use super::branch::BranchHues;
use super::compare::CompareSource;
use super::tags::{TagStore, tag_index};
use super::trash::{TrashCache, WithoutBins};
use crate::filter::vocab::platform_vocab;
use crate::filter::{FilterContext, FilterResult, NameIndex, evaluate, unix_now};
use crate::layout::api::{ArraySizes, LayoutParams, OptValue, RootSpec, SizeSource, StoreSizes};
use crate::layout::registry;
use crate::layout::rows::{DrawList, KEY_MORE, KEY_NODE_MASK, KEY_SMALL, KEY_TAG_BITS, kind};
use crate::layout::{Basis, Visualization, effective_alloc};

pub const DETAIL_MIN: u32 = 1;
pub const DETAIL_MAX: u32 = 15;
/// Back steps kept per view.
pub const HISTORY: usize = 200;
/// Changes flashed per frame (the largest).
pub const MAX_FLASH: usize = 200;
/// Levels below the view root that breathe while their folder is being scanned.
pub const SCANNING_LEVELS: u8 = 3;

/// Per-block flags of a rendered frame (`flags` column).
pub mod flag {
    /// filter active: this file matches / this block contains matches
    pub const MATCH: u16 = 1;
    /// filter active and nothing here matches (dim mode)
    pub const DIM: u16 = 2;
    /// folder named by a positive folder mask (bold title)
    pub const FOLDER_HIT: u16 = 4;
    /// carries its own tag (see the tag column)
    pub const TAGGED: u16 = 8;
    /// the selection, or its nearest visible ancestor
    pub const SELECTED: u16 = 16;
    /// an ancestor of the selection (halo header)
    pub const ON_PATH: u16 = 32;
    /// scan running and this folder's subtree is not fully listed yet (top levels only)
    pub const SCANNING: u16 = 64;
    /// in a recycle bin / trash, or the bin itself
    pub const TRASH: u16 = 128;
    /// the bin folder itself
    pub const TRASH_BIN: u16 = 256;
}

pub const DAY: i64 = 86_400;
/// Time colouring: < 1 d, < 1 w, < 1 mo, < 1 y, older.
pub const TONE_LIMITS: [i64; 4] = [DAY, 7 * DAY, 30 * DAY, 365 * DAY];
/// The visualization every session can show (drawlist.v1, no capability).
pub const BASELINE: &str = "treemap";

/// The visualization `visual` if a session that announced `caps` can show it: registered, its
/// capability announced, and without `visuals.v1` only the baseline treemap.
pub fn showable<S: AsRef<str>>(visual: &str, caps: &[S]) -> Option<Visualization> {
    let vis = registry::get(visual)?;
    if !caps.iter().any(|c| c.as_ref() == CAP_VISUALS) {
        return (vis.id == BASELINE).then_some(vis);
    }
    registry::is_available(&vis, caps).then_some(vis)
}

/// What a view needs to know about a scan root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootInfo {
    pub node: NodeId,
    pub whole_volume: bool,
    pub free: Option<i64>,
    pub used: Option<i64>,
}

impl RootInfo {
    pub fn folder(node: NodeId) -> RootInfo {
        RootInfo {
            node,
            whole_volume: false,
            free: None,
            used: None,
        }
    }
}

/// What the views of one scan share.
pub struct ViewContext {
    pub store: Store,
    pub roots: RwLock<Vec<RootInfo>>,
    pub basis: Arc<Basis>,
    pub names: Arc<NameIndex>,
    pub tags: Arc<Mutex<TagStore>>,
    pub trash: Arc<TrashCache>,
    /// the filter parser's vocabulary (the server replaces it when classes or language change)
    pub vocab: RwLock<Arc<CompiledVocab>>,
}

impl ViewContext {
    /// A context with the active class table, folder classes, a fresh name index and trash cache.
    pub fn new(store: Store, roots: Vec<RootInfo>, tags: TagStore) -> Arc<ViewContext> {
        ViewContext::with_basis(store, roots, tags, Arc::new(Basis::for_scan()))
    }

    pub fn with_basis(
        store: Store,
        roots: Vec<RootInfo>,
        tags: TagStore,
        basis: Arc<Basis>,
    ) -> Arc<ViewContext> {
        let vocab = CompiledVocab::new(platform_vocab(basis.class_map().classes(), "zh"));
        Arc::new(ViewContext {
            store,
            roots: RwLock::new(roots),
            basis,
            names: Arc::new(NameIndex::new()),
            tags: Arc::new(Mutex::new(tags)),
            trash: Arc::new(TrashCache::new()),
            vocab: RwLock::new(Arc::new(vocab)),
        })
    }
}

/// What fills the view.
#[derive(Clone, Debug, PartialEq)]
pub struct Zoom {
    /// folder or file filling the view; for aggregates the container
    pub node: NodeId,
    /// the block that was zoomed into (node id or SMALL/MORE key)
    pub key: u32,
    pub members: Option<Arc<Vec<NodeId>>>,
    /// aggregate size for the breadcrumb ("18,406 small items")
    pub count: u32,
    /// the aggregate zoom of the same container this one was opened in
    pub parent: Option<Arc<Zoom>>,
}

impl Zoom {
    pub fn at(node: NodeId) -> Zoom {
        Zoom {
            node,
            key: node,
            members: None,
            count: 0,
            parent: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub zoom: Zoom,
    pub selected: Option<u32>,
}

/// A view's settings when it opens.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewSettings {
    pub detail: u32,
    pub min_element: f64,
    pub sort_by_size: bool,
    pub show_free: bool,
    pub show_unknown: bool,
    /// the recycle bins (「回收站」 chip)
    pub show_trash: bool,
    /// the folders of treemap.hidden_folders are taken out of the picture (「隐藏文件夹」 chip)
    pub hide_folders: bool,
    /// treemap.hidden_folders: absolute paths, as the user wrote them
    pub hidden_folders: Vec<String>,
    /// a registered visualization
    pub visual: String,
    /// of `visual`, the one the view starts with
    pub color_mode: ColorModeName,
    /// the colour mode each other visualization starts with
    pub colors: BTreeMap<String, ColorModeName>,
    pub filter_mode: FilterModeName,
    pub font_scale: f64,
    pub aspect: f64,
    /// flame graph: the root bar at the "top" or the "bottom"
    pub flame_origin: String,
}

impl Default for ViewSettings {
    fn default() -> Self {
        ViewSettings {
            detail: 5,
            min_element: 14.0,
            sort_by_size: true,
            show_free: true,
            show_unknown: true,
            show_trash: true,
            hide_folders: true,
            hidden_folders: Vec::new(),
            visual: BASELINE.to_owned(),
            color_mode: ColorModeName::Class,
            colors: BTreeMap::new(),
            filter_mode: FilterModeName::Dim,
            font_scale: 1.0,
            aspect: 1.0,
            flame_origin: "bottom".to_owned(),
        }
    }
}

/// The zoom root as the facts of a row (`FrameInfo.root`: the root bar, the centre disc).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootRow {
    pub key: u32,
    pub node: NodeId,
    /// FOLDER, PENDING, FILE, or SMALL / MORE for an aggregate zoom
    pub kind: u8,
    /// bytes on this frame's basis
    pub size: i64,
    pub count: u32,
    pub flags: u16,
    pub tag: u8,
    pub tone: u8,
    pub branch: u8,
    pub delta: Option<i64>,
    pub dstat: Option<u8>,
}

/// The filter of a view.
#[derive(Clone, Debug, Default)]
pub struct FilterState {
    pub text: String,
    /// last valid, non-empty normal form (None: no filtering)
    pub normal: Option<FilterNormal>,
    pub result: Option<Arc<FilterResult>>,
    pub diagnostics: Vec<FilterDiagnostic>,
    /// the text has errors: `normal` / `result` belong to an earlier text
    pub stale: bool,
}

impl FilterState {
    pub fn active(&self) -> bool {
        self.normal.is_some()
    }
}

/// One rendered frame of a view.
#[derive(Debug, Default)]
pub struct Frame {
    pub draw: DrawList,
    /// F_* bits per row
    pub flags: Vec<u16>,
    /// own tag per row: 0 none, 1–4 red, yellow, green, blue
    pub tag: Vec<u8>,
    /// time-colouring band per row (0 newest … 4 oldest)
    pub tone: Vec<u8>,
    /// top-level branch colour slot per row (0 neutral, 1–8)
    pub branch: Vec<u8>,
    pub root: RootRow,
    pub selected_row: Option<usize>,
    /// (key, label) from the home root to the zoom root
    pub breadcrumb: Vec<(u32, String)>,
    /// (files, allocated, logical) below the zoom root
    pub filter_totals: Option<(i64, i64, i64)>,
    /// allocated bytes below the zoom root
    pub scanned_total: i64,
    /// (row, allocation change): file changes; row -1 is the zoom root (partition layouts)
    pub flash: Vec<(i64, i64)>,
    /// compare views: signed net change per row
    pub delta: Option<Vec<i64>>,
    /// compare views: 0 unchanged, 1 new, 2 deleted, 3 changed
    pub dstat: Option<Vec<u8>>,
    /// render counter of the view (identifies the frame, e.g. for result caches)
    pub serial: u64,
    /// the numbering its node ids and keys belong to ([`sk_store::Store::generation`])
    pub generation: u64,
}

/// Python's `searchsorted(TONE_LIMITS, age, side="right")`; unknown times are the oldest.
#[inline]
pub fn band(now: i64, mtime: u32) -> u8 {
    if mtime == 0 {
        return 4;
    }
    let age = now - mtime as i64;
    TONE_LIMITS.iter().filter(|&&l| l <= age).count() as u8
}

/// Time band of an aggregate: the one holding most of its members' bytes.
pub fn tone_of(nodes: &Nodes, now: i64, members: &[NodeId]) -> u8 {
    let mut w = [0f64; 5];
    for &m in members {
        if (m as usize) < nodes.len() {
            w[band(now, nodes.mtime(m)) as usize] += effective_alloc(nodes, m) as f64;
        }
    }
    let mut top = 0;
    for i in 1..5 {
        if w[i] > w[top] {
            top = i;
        }
    }
    top as u8
}

/// The area basis of the zoom of a hide-mode frame: the matched bytes of a filter result.
struct MatchedSizes<'a> {
    res: &'a FilterResult,
    n: usize,
}

impl SizeSource for MatchedSizes<'_> {
    fn n(&self) -> usize {
        self.n
    }
    fn size(&self, _nodes: &Nodes, id: NodeId) -> i64 {
        self.res.alloc.get(id as usize).copied().unwrap_or(0)
    }
}

/// One view of a scan.
pub struct View {
    pub id: String,
    pub ctx: Arc<ViewContext>,
    pub visual: String,
    colors: BTreeMap<String, ColorModeName>,
    /// the visualizations' own settings (`LayoutParams::options`)
    pub visual_options: HashMap<String, Vec<(String, OptValue)>>,
    pub hues: BranchHues,
    pub detail: u32,
    pub min_element: f64,
    pub sort_by_size: bool,
    pub show_free: bool,
    pub show_unknown: bool,
    pub show_trash: bool,
    /// the folders of [`View::set_hidden_folders`] are taken out of the picture
    pub hide_folders: bool,
    hidden_paths: Vec<String>,
    /// the nodes of `hidden_paths` in the store of version .0 ([`View::hidden_nodes`])
    hidden_found: Mutex<Option<(u64, Vec<NodeId>)>>,
    pub font_scale: f64,
    pub aspect: f64,
    pub filter_mode: FilterModeName,
    /// (width, height, dpr), CSS px
    pub viewport: (f64, f64, f64),
    pub home: NodeId,
    pub zoom: Zoom,
    pub back: Vec<HistoryEntry>,
    pub forward: Vec<HistoryEntry>,
    /// a node id, or a SMALL/MORE key
    pub selected: Option<u32>,
    pub filter: FilterState,
    /// W3 result list order, remembered per view
    pub results_sort: ResultsSortName,
    pub last: Option<Frame>,
    /// the frame before the last: its buffers are reused by the next render
    spare: Option<Frame>,
    /// compare views: areas are changes (W6)
    pub compare: Option<Arc<dyn CompareSource>>,
    /// compare views: 「显示的变化」 all / grow / shrink
    pub direction: CompareDirection,
    /// its scan is running: frames mark the folders still being listed
    pub scanning: bool,
    /// node -> allocation change, shown with the next frame
    flashes: Mutex<HashMap<NodeId, i64>>,
    serial: u64,
    scratch: Vec<bool>,
}

impl View {
    /// A view of `ctx`'s scan; None when `settings.visual` is not a registered visualization.
    pub fn new(id: &str, ctx: Arc<ViewContext>, settings: ViewSettings) -> Option<View> {
        registry::get(&settings.visual)?;
        let s = settings;
        let mut colors = s.colors.clone();
        colors.insert(s.visual.clone(), s.color_mode);
        let mut visual_options = HashMap::new();
        visual_options.insert(
            "flame".to_owned(),
            vec![("origin".to_owned(), OptValue::Str(s.flame_origin.clone()))],
        );
        let home = {
            let roots = ctx.roots.read();
            if roots.len() == 1 { roots[0].node } else { 0 }
        };
        Some(View {
            id: id.to_owned(),
            ctx,
            visual: s.visual,
            colors,
            visual_options,
            hues: BranchHues::new(),
            detail: s.detail,
            min_element: s.min_element,
            sort_by_size: s.sort_by_size,
            show_free: s.show_free,
            show_unknown: s.show_unknown,
            show_trash: s.show_trash,
            hide_folders: s.hide_folders,
            hidden_paths: s.hidden_folders,
            hidden_found: Mutex::new(None),
            font_scale: s.font_scale,
            aspect: s.aspect,
            filter_mode: s.filter_mode,
            viewport: (1280.0, 800.0, 1.0),
            home,
            zoom: Zoom::at(home),
            back: Vec::new(),
            forward: Vec::new(),
            selected: None,
            filter: FilterState::default(),
            results_sort: ResultsSortName::Size,
            last: None,
            spare: None,
            compare: None,
            direction: CompareDirection::All,
            scanning: false,
            flashes: Mutex::new(HashMap::new()),
            serial: 0,
            scratch: Vec::new(),
        })
    }

    pub fn store(&self) -> &Store {
        &self.ctx.store
    }

    // ------------------------------------------------------------- visualization

    /// Colour mode of the current visualization (each one remembers its own).
    pub fn color_mode(&self) -> ColorModeName {
        self.colors.get(&self.visual).copied().unwrap_or_else(|| {
            registry::get(&self.visual).map_or(ColorModeName::Class, |v| v.default_color)
        })
    }

    pub fn set_color_mode(&mut self, mode: ColorModeName) {
        self.colors.insert(self.visual.clone(), mode);
    }

    /// The colour modes set so far, per visualization.
    pub fn colors(&self) -> BTreeMap<String, ColorModeName> {
        self.colors.clone()
    }

    /// Take over another view's colour modes; ids of unknown visualizations are left out.
    pub fn set_colors(&mut self, colors: &BTreeMap<String, ColorModeName>) {
        for (vis, &mode) in colors {
            if registry::get(vis).is_some() {
                self.colors.insert(vis.clone(), mode);
            }
        }
    }

    /// Show another visualization (None: not registered). Zoom root, history, selection and filter
    /// stay; switching is no history step. Returns whether it changed.
    pub fn set_visual(&mut self, visual: &str) -> Option<bool> {
        registry::get(visual)?;
        if visual == self.visual {
            return Some(false);
        }
        self.visual = visual.to_owned();
        Some(true)
    }

    fn vis(&self) -> Visualization {
        registry::get(&self.visual)
            .or_else(|| registry::get(BASELINE))
            .expect("the baseline visualization")
    }

    pub fn params(&self) -> LayoutParams {
        let (w, h, dpr) = self.viewport;
        LayoutParams {
            width: w,
            height: h,
            dpr,
            detail: self.detail,
            min_element: self.min_element,
            sort_by_size: self.sort_by_size,
            font_scale: self.font_scale,
            aspect: self.aspect,
            options: self
                .visual_options
                .get(&self.visual)
                .cloned()
                .unwrap_or_default(),
            ..LayoutParams::default()
        }
    }

    // ------------------------------------------------------------- navigation

    fn push(&mut self) {
        self.back.push(HistoryEntry {
            zoom: self.zoom.clone(),
            selected: self.selected,
        });
        if self.back.len() > HISTORY {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    fn valid(&self, zoom: &Zoom) -> bool {
        (zoom.node as usize) < self.ctx.store.len()
    }

    /// Node ids changed (a folder listed again, the store compacted): `new_id` maps old ids to new
    /// ones (None: gone). A zoom onto a gone node falls back to `fallback`; a gone selection is
    /// dropped; history keeps its order.
    pub fn remap(&mut self, new_id: &dyn Fn(NodeId) -> Option<NodeId>, fallback: NodeId) {
        let one = |key: u32| -> Option<u32> {
            let (tag, node) = if key >= KEY_SMALL {
                (key & !(KEY_SMALL - 1), key & (KEY_SMALL - 1))
            } else {
                (0, key)
            };
            new_id(node).map(|n| tag | n)
        };
        fn zoom_of(
            z: &Zoom,
            one: &dyn Fn(u32) -> Option<u32>,
            new_id: &dyn Fn(NodeId) -> Option<NodeId>,
            fallback: NodeId,
        ) -> Zoom {
            let Some(node) = one(z.node) else {
                return Zoom::at(fallback);
            };
            let members = z
                .members
                .as_ref()
                .map(|m| Arc::new(m.iter().filter_map(|&i| new_id(i)).collect::<Vec<_>>()));
            let key = one(z.key).filter(|&k| k != 0).unwrap_or(node);
            Zoom {
                node,
                key,
                members,
                count: z.count,
                parent: z
                    .parent
                    .as_ref()
                    .map(|p| Arc::new(zoom_of(p, one, new_id, fallback))),
            }
        }
        self.home = one(self.home).filter(|&h| h != 0).unwrap_or(fallback);
        self.zoom = zoom_of(&self.zoom, &one, new_id, fallback);
        self.selected = self.selected.and_then(one);
        for list in [&mut self.back, &mut self.forward] {
            for e in list.iter_mut() {
                e.zoom = zoom_of(&e.zoom, &one, new_id, fallback);
                e.selected = e.selected.and_then(one);
            }
        }
        {
            let mut f = self.flashes.lock();
            if !f.is_empty() {
                *f = f
                    .drain()
                    .filter_map(|(k, d)| new_id(k).map(|m| (m, d)))
                    .collect();
            }
        }
        self.hues.remap(new_id);
        self.last = None;
    }

    /// Blocks to flash in the next frame: node -> change of its allocation (UI-TM-43).
    pub fn add_flash(&self, changes: &HashMap<NodeId, i64>) {
        let mut f = self.flashes.lock();
        for (&node, &d) in changes {
            *f.entry(node).or_default() += d;
        }
    }

    /// Zoom into a block of the last frame (double-click / Enter). False if it cannot be zoomed.
    pub fn zoom_into(&mut self, key: u32) -> bool {
        let row = self.last.as_ref().and_then(|f| f.draw.row(key));
        let Some(row) = row else {
            if key > 0 && (key as usize) < self.ctx.store.len() {
                return self.zoom_to_node(key); // a node not drawn right now
            }
            return false;
        };
        let frame = self.last.as_ref().expect("a frame");
        let k = frame.draw.kind[row];
        if k == kind::FREE || k == kind::UNKNOWN {
            return false;
        }
        let node = frame.draw.node[row] as NodeId;
        let zoom = if k == kind::SMALL || k == kind::MORE {
            let outer = (self.zoom.members.is_some() && self.zoom.node == node)
                .then(|| Arc::new(self.zoom.clone()));
            Zoom {
                node,
                key,
                members: frame.draw.members.get(&key).map(|m| Arc::new(m.clone())),
                count: frame.draw.count[row],
                parent: outer,
            }
        } else {
            let is_dir = self.ctx.store.read().is_dir(node);
            if !is_dir {
                return self.zoom_to_file(node);
            }
            Zoom::at(node)
        };
        if zoom.key == self.zoom.key && zoom.node == self.zoom.node {
            return false;
        }
        self.push();
        self.zoom = zoom;
        self.selected = None;
        true
    }

    /// Double-clicking a file zooms to its folder and selects the file (UI-MAIN-31).
    fn zoom_to_file(&mut self, node: NodeId) -> bool {
        let parent = self.ctx.store.read().parent(node);
        if parent >= 0 && parent as NodeId == self.zoom.node && self.zoom.members.is_none() {
            if self.selected == Some(node) {
                return false;
            }
            self.selected = Some(node);
            return true;
        }
        if parent <= 0 && self.home != 0 {
            return false;
        }
        self.push();
        self.zoom = Zoom::at(parent.max(0) as NodeId);
        self.selected = Some(node);
        true
    }

    pub fn zoom_to_node(&mut self, node: NodeId) -> bool {
        if (node as usize) >= self.ctx.store.len()
            || (node == self.zoom.node && self.zoom.members.is_none())
        {
            return false;
        }
        if !self.ctx.store.read().is_dir(node) {
            return self.zoom_to_file(node);
        }
        self.push();
        self.zoom = Zoom::at(node);
        self.selected = None;
        true
    }

    pub fn go_back(&mut self) -> bool {
        while let Some(e) = self.back.pop() {
            if self.valid(&e.zoom) {
                self.forward.push(HistoryEntry {
                    zoom: self.zoom.clone(),
                    selected: self.selected,
                });
                self.zoom = e.zoom;
                self.selected = e.selected;
                return true;
            }
        }
        false
    }

    pub fn go_forward(&mut self) -> bool {
        while let Some(e) = self.forward.pop() {
            if self.valid(&e.zoom) {
                self.back.push(HistoryEntry {
                    zoom: self.zoom.clone(),
                    selected: self.selected,
                });
                self.zoom = e.zoom;
                self.selected = e.selected;
                return true;
            }
        }
        false
    }

    pub fn can_go_up(&self) -> bool {
        self.zoom.members.is_some() || self.zoom.node != self.home
    }

    /// Zoom to the parent; the block just left becomes the selection (UI-TM-37).
    pub fn go_up(&mut self) -> bool {
        if !self.can_go_up() {
            return false;
        }
        let z = self.zoom.clone();
        let up = if z.members.is_some() {
            z.parent
                .as_deref()
                .cloned()
                .unwrap_or_else(|| Zoom::at(z.node))
        } else {
            let parent = self.ctx.store.read().parent(z.node);
            if parent < 0 || (self.home != 0 && parent == 0) {
                return false;
            }
            Zoom::at(parent as NodeId)
        };
        self.push();
        self.zoom = up;
        self.selected = Some(z.key);
        true
    }

    pub fn go_home(&mut self) -> bool {
        if self.zoom.node == self.home && self.zoom.members.is_none() {
            return false;
        }
        self.push();
        self.zoom = Zoom::at(self.home);
        self.selected = None;
        true
    }

    pub fn set_detail(&mut self, detail: u32) -> u32 {
        self.detail = detail.clamp(DETAIL_MIN, DETAIL_MAX);
        self.detail
    }

    pub fn set_viewport(&mut self, width: f64, height: f64, dpr: f64) {
        self.viewport = (width.max(1.0), height.max(1.0), dpr.max(0.5));
    }

    pub fn select(&mut self, key: Option<u32>) {
        self.selected = key;
    }

    /// Select a block and make sure it is drawn (W3 result click, UI-W3-33).
    ///
    /// A block of the last frame is just selected, and so is a node merged into one of its
    /// aggregate blocks. A node that is not drawn makes the view zoom (with history) to the
    /// highest ancestor under which it is within the detail level (or the levels the
    /// visualization fits in the viewport); if it is still not drawn there (too small), to its
    /// parent folder.
    pub fn reveal(&mut self, key: u32) -> bool {
        if self
            .last
            .as_ref()
            .is_some_and(|f| f.draw.row(key).is_some())
        {
            if self.selected == Some(key) {
                return false;
            }
            self.selected = Some(key);
            return true;
        }
        let (chain, target) = {
            let nodes = self.ctx.store.read();
            if key >= KEY_SMALL || key == 0 || key as usize >= nodes.len() || key == self.zoom.node
            {
                return false;
            }
            let mut chain: Vec<NodeId> = nodes.ancestors(key).collect(); // parent first
            if self.home != 0 {
                let Some(at) = chain.iter().position(|&a| a == self.home) else {
                    return false;
                };
                chain.truncate(at + 1);
            } else if chain.last() != Some(&0) {
                chain.push(0);
            }
            let here = self.zoom.members.is_none();
            let inside = here && chain.contains(&self.zoom.node);
            let member = !here && self.zoom.members.as_ref().is_some_and(|m| m.contains(&key));
            if (inside || member)
                && let Some(frame) = &self.last
                && let Some(row) = hidden_row(&nodes, &frame.draw, key, None)
                && matches!(frame.draw.kind[row], kind::SMALL | kind::MORE)
            {
                let changed = self.selected != Some(key);
                self.selected = Some(key); // drawn inside an aggregate block: select that block
                return changed;
            }
            let base = if inside {
                self.zoom.node
            } else {
                *chain.last().expect("a chain")
            };
            let d = |a: NodeId| if a == 0 { 0 } else { nodes.depth(a) as i64 };
            let levels = self
                .detail
                .min(self.vis().engine.levels(&self.params()))
                .max(1) as i64;
            let want = d(base).max(nodes.depth(key) as i64 - levels);
            let mut target = chain
                .iter()
                .copied()
                .find(|&a| d(a) == want)
                .unwrap_or(chain[0]);
            if here && target == self.zoom.node {
                target = chain[0];
            }
            if here && target == self.zoom.node {
                // a direct child that is not drawn (zero size)
                let changed = self.selected != Some(key);
                self.selected = Some(key);
                return changed;
            }
            (chain.clone(), target)
        };
        self.push();
        self.zoom = Zoom::at(target);
        self.selected = Some(key);
        if target != chain[0] && self.render().draw.row(key).is_none() {
            self.zoom = Zoom::at(chain[0]); // too small under the higher root: its folder
        }
        true
    }

    // ------------------------------------------------------------- tags

    /// Ctrl+1..4 / Ctrl+0 on the selection (a real node). Returns whether anything changed.
    pub fn tag_selected(&mut self, color: Option<TagColor>) -> bool {
        let Some(node) = self.selected else {
            return false;
        };
        if node >= KEY_SMALL || node == 0 {
            return false;
        }
        let changed = {
            let nodes = self.ctx.store.read();
            let mut tags = self.ctx.tags.lock();
            let changed = match color {
                None => tags.clear_subtree(&nodes, node) > 0,
                Some(c) => tags.set(&nodes, node, Some(c)),
            };
            if changed && let Err(e) = tags.save() {
                tracing::warn!("could not save the tags: {e}");
            }
            changed
        };
        if changed
            && self
                .filter
                .normal
                .as_ref()
                .is_some_and(|n| n.tags.is_some() || !n.not_tags.is_empty())
        {
            self.refilter(None);
        }
        changed
    }

    // ------------------------------------------------------------- filter

    /// Apply a filter text. With errors the previous valid filter stays in effect (UI-TM-48).
    pub fn set_filter(&mut self, text: &str, cursor: Option<u32>) -> &FilterState {
        let vocab = self.ctx.vocab.read().clone();
        let parsed = vocab.parse(text, cursor);
        let f = &mut self.filter;
        f.text = text.to_owned();
        f.diagnostics = parsed.diagnostics;
        let Some(normal) = parsed.normal else {
            f.stale = true;
            return &self.filter;
        };
        f.stale = false;
        if normal.is_empty() {
            f.normal = None;
            f.result = None;
            return &self.filter;
        }
        f.normal = Some(normal);
        self.refilter(None);
        &self.filter
    }

    /// Re-evaluate the current filter (data changed, time passed, tags changed).
    pub fn refilter(&mut self, now: Option<i64>) {
        let Some(normal) = &self.filter.normal else {
            return;
        };
        let nodes = self.ctx.store.read();
        let tags = if normal.tags.is_some() || !normal.not_tags.is_empty() {
            Some(self.ctx.tags.lock().bits(&nodes))
        } else {
            None
        };
        let ctx = FilterContext {
            now: now.unwrap_or_else(unix_now),
            classes: self.ctx.basis.class_map(),
            folder_classes: self.ctx.basis.folder_classes(),
            tags,
            names: Some(self.ctx.names.clone()),
        };
        self.filter.result = Some(Arc::new(evaluate(&nodes, normal, &ctx)));
    }

    // ------------------------------------------------------------- rendering

    fn root_spec(&self, nodes: &Nodes, vis: &Visualization) -> RootSpec {
        let z = &self.zoom;
        if let Some(m) = &z.members {
            return RootSpec {
                node: z.node,
                members: Some(m.clone()),
                key: Some(z.key),
                ..RootSpec::default()
            };
        }
        let mut spec = RootSpec::node(z.node);
        let hide = self.filter.active() && self.filter_mode == FilterModeName::Hide;
        let roots = self.ctx.roots.read();
        let root = roots.iter().find(|r| r.node == z.node && r.whole_volume);
        if let Some(root) = root
            && !hide
            && vis.free_space
        {
            if self.show_free
                && let Some(free) = root.free
                && free != 0
            {
                spec.free = Some(free);
            }
            if self.show_unknown
                && let Some(used) = root.used
            {
                spec.unknown = Some(used - nodes.tot_alloc(z.node));
            }
        }
        spec
    }

    /// Lay out and annotate the next frame (it becomes [`View::last`]).
    pub fn render(&mut self) -> &Frame {
        let ctx = self.ctx.clone();
        let nodes = ctx.store.read();
        let vis = self.vis();
        let res = if self.filter.active() {
            self.filter.result.clone()
        } else {
            None
        };
        let hide = res.is_some() && self.filter_mode == FilterModeName::Hide;
        let compare_sizes;
        let matched;
        let store_sizes = StoreSizes::of(&nodes);
        let base: &dyn SizeSource = if let Some(t) = &self.compare
            && self.direction != CompareDirection::All
        {
            let m = if hide {
                res.as_ref().map(|r| r.matched.as_slice())
            } else {
                None
            };
            compare_sizes = ArraySizes(Arc::new(t.amounts(self.direction, m)));
            &compare_sizes
        } else if hide && let Some(r) = &res {
            matched = MatchedSizes {
                res: r,
                n: nodes.len(),
            };
            &matched
        } else {
            &store_sizes
        };
        let out = self.taken_out(&nodes);
        let without;
        let sizes: &dyn SizeSource = if out.is_empty() {
            base
        } else {
            without = WithoutBins::new(base, &nodes, &out);
            &without
        };
        let mut frame = self.spare.take().unwrap_or_default();
        let spec = self.root_spec(&nodes, &vis);
        vis.engine.run(
            &nodes,
            &ctx.basis,
            &self.params(),
            &spec,
            sizes,
            &mut frame.draw,
        );
        frame.draw.visual = vis.id.clone();
        self.annotate(&nodes, &mut frame, res.as_deref(), sizes);
        if self.scanning {
            let (rows, root) = self.scanning_rows(&nodes, &frame.draw);
            for r in rows {
                frame.flags[r] |= flag::SCANNING;
            }
            if root {
                frame.root.flags |= flag::SCANNING;
            }
        }
        frame.flash = self.flash_rows(&nodes, &frame.draw);
        self.serial += 1;
        frame.serial = self.serial;
        // read under the reader: no renumbering in between
        frame.generation = ctx.store.generation();
        drop(nodes);
        self.spare = self.last.replace(frame);
        self.last.as_ref().expect("just rendered")
    }

    /// The scan holds a recycle bin (the 「回收站」 chip is offered).
    pub fn has_trash(&self) -> bool {
        self.compare.is_none() && !self.ctx.trash.find_bins(&self.ctx.store.read()).is_empty()
    }

    /// The subtrees taken out of the picture, sorted, none inside another: the recycle bins
    /// (「回收站」 off) unless the view is in one, and the hidden folders (「隐藏文件夹」) but those
    /// the view is in. Nothing in a comparison.
    fn taken_out(&self, nodes: &Nodes) -> Vec<NodeId> {
        if self.compare.is_some() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if !self.show_trash {
            let bins = self.ctx.trash.find_bins(nodes);
            if !bins.is_empty() && !self.in_bin(nodes, &bins) {
                out.extend(bins);
            }
        }
        if self.hide_folders {
            let z = self.zoom.node;
            let around: Vec<NodeId> = if z == 0 || z as usize >= nodes.len() {
                Vec::new()
            } else {
                std::iter::once(z).chain(nodes.ancestors(z)).collect()
            };
            out.extend(
                self.hidden_nodes(nodes)
                    .into_iter()
                    .filter(|h| !around.contains(h)),
            );
        }
        outermost(nodes, out)
    }

    /// The nodes of the hidden folders ([`View::set_hidden_folders`]) the store holds, sorted,
    /// none inside another; not a scan's root (hiding it would leave nothing to see). Looked up
    /// again when the store changed; none in a comparison.
    pub fn hidden_nodes(&self, nodes: &Nodes) -> Vec<NodeId> {
        if self.compare.is_some() || self.hidden_paths.is_empty() {
            return Vec::new();
        }
        let version = nodes.version();
        let mut found = self.hidden_found.lock();
        if let Some((v, ids)) = &*found
            && *v == version
        {
            return ids.clone();
        }
        let roots = nodes.roots();
        let ids = outermost(
            nodes,
            self.hidden_paths
                .iter()
                .filter_map(|p| nodes.find_shown(std::path::Path::new(p)))
                .filter(|&id| id != 0 && !roots.contains(&id))
                .collect(),
        );
        *found = Some((version, ids.clone()));
        ids
    }

    /// How many hidden folders the scan holds (the 「隐藏文件夹」 chip is offered when any).
    pub fn hidden_folder_count(&self) -> u32 {
        self.hidden_nodes(&self.ctx.store.read()).len() as u32
    }

    /// treemap.hidden_folders (absolute paths); true when the list changed.
    pub fn set_hidden_folders(&mut self, paths: Vec<String>) -> bool {
        if paths == self.hidden_paths {
            return false;
        }
        self.hidden_paths = paths;
        *self.hidden_found.get_mut() = None;
        true
    }

    fn in_bin(&self, nodes: &Nodes, bins: &[NodeId]) -> bool {
        let node = self.zoom.node;
        if node == 0 || node as usize >= nodes.len() {
            return false;
        }
        std::iter::once(node)
            .chain(nodes.ancestors(node))
            .any(|a| bins.binary_search(&a).is_ok())
    }

    fn annotate(
        &mut self,
        nodes: &Nodes,
        frame: &mut Frame,
        res: Option<&FilterResult>,
        sizes: &dyn SizeSource,
    ) {
        let draw = &frame.draw;
        let n = draw.len();
        let real = |i: usize| draw.kind[i] <= kind::PENDING && draw.node[i] >= 0;
        let agg = |i: usize| draw.kind[i] == kind::SMALL || draw.kind[i] == kind::MORE;
        frame.flags.clear();
        frame.flags.resize(n, 0);
        frame.tag.clear();
        frame.tag.resize(n, 0);
        frame.tone.clear();
        frame.tone.resize(n, 4);
        // own tags
        let resolved = self.ctx.tags.lock().resolve(nodes);
        if !resolved.is_empty() {
            for i in 0..n {
                if real(i)
                    && let Some(&c) = resolved.get(&(draw.node[i] as NodeId))
                {
                    frame.tag[i] = tag_index(c);
                    frame.flags[i] |= flag::TAGGED;
                }
            }
        }
        // in a recycle bin: real blocks by their node, aggregates by their container
        let holders: Vec<i64> = (0..n)
            .map(|i| {
                if real(i) || agg(i) {
                    draw.node[i] as i64
                } else {
                    -1
                }
            })
            .collect();
        let (inside, is_bin) = self.ctx.trash.marks(nodes, &holders);
        for i in 0..n {
            if inside[i] {
                frame.flags[i] |= flag::TRASH;
            }
            if is_bin[i] && real(i) {
                frame.flags[i] |= flag::TRASH_BIN;
            }
        }
        // time bands (files: own mtime; aggregates: the band holding most of their bytes)
        let now = unix_now();
        for i in 0..n {
            if real(i) && draw.kind[i] == kind::FILE {
                frame.tone[i] = band(now, nodes.mtime(draw.node[i] as NodeId));
            } else if agg(i)
                && let Some(m) = draw.members.get(&draw.key[i])
                && !m.is_empty()
            {
                frame.tone[i] = tone_of(nodes, now, m);
            }
        }
        // filter state
        let mut totals = None;
        if let Some(res) = res {
            let rn = res.n;
            for i in 0..n {
                let matched = if real(i) {
                    let nd = draw.node[i] as usize;
                    nd < rn && res.count[nd] > 0
                } else if agg(i) {
                    draw.members.get(&draw.key[i]).is_some_and(|m| {
                        m.iter()
                            .any(|&x| (x as usize) < rn && res.count[x as usize] > 0)
                    })
                } else {
                    false
                };
                frame.flags[i] |= if matched { flag::MATCH } else { flag::DIM };
                if real(i) {
                    let nd = draw.node[i] as usize;
                    if nd < rn && res.folder_hit[nd] {
                        frame.flags[i] |= flag::FOLDER_HIT;
                    }
                }
            }
            match &self.zoom.members {
                None if (self.zoom.node as usize) < rn => totals = Some(res.totals(self.zoom.node)),
                None => {}
                Some(m) => {
                    let (mut c, mut a, mut s) = (0i64, 0i64, 0i64);
                    for &x in m.iter().filter(|&&x| (x as usize) < rn) {
                        c += res.count[x as usize] as i64;
                        a += res.alloc[x as usize];
                        s += res.size[x as usize];
                    }
                    totals = Some((c, a, s));
                }
            }
        }
        // selection: the selected block, or its nearest visible ancestor / aggregate
        let selected_row = self.selected_row(nodes, draw);
        if let Some(r) = selected_row {
            frame.flags[r] |= flag::SELECTED;
            let mut p = draw.parent[r];
            while p >= 0 {
                frame.flags[p as usize] |= flag::ON_PATH;
                p = draw.parent[p as usize];
            }
        }
        let scanned = match &self.zoom.members {
            Some(m) => m
                .iter()
                .filter(|&&x| (x as usize) < nodes.len())
                .map(|&x| effective_alloc(nodes, x))
                .sum(),
            None if (self.zoom.node as usize) < nodes.len() => {
                effective_alloc(nodes, self.zoom.node)
            }
            None => 0,
        };
        let (delta, dstat) = match &self.compare {
            Some(t) => {
                let (d, s) = self.changes(t.as_ref(), draw);
                (Some(d), Some(s))
            }
            None => (None, None),
        };
        let mut branch = std::mem::take(&mut frame.branch);
        self.hues.hues_into(self.zoom.key, draw, &mut branch);
        frame.branch = branch;
        frame.root = self.root_row(nodes, res, sizes, &resolved, now);
        frame.selected_row = selected_row;
        frame.breadcrumb = self.breadcrumb_in(nodes);
        frame.filter_totals = totals;
        frame.scanned_total = scanned;
        frame.delta = delta;
        frame.dstat = dstat;
    }

    fn root_row(
        &self,
        nodes: &Nodes,
        res: Option<&FilterResult>,
        sizes: &dyn SizeSource,
        tags: &HashMap<NodeId, TagColor>,
        now: i64,
    ) -> RootRow {
        let z = &self.zoom;
        let mut flags = 0u16;
        let mut tag = 0u8;
        let mut tone = 4u8;
        let (mut delta, mut dstat) = (None, None);
        let k;
        let size;
        let ids: Vec<NodeId>;
        let valid = (z.node as usize) < nodes.len();
        if let Some(members) = &z.members {
            k = if z.key & KEY_TAG_BITS == KEY_SMALL {
                kind::SMALL
            } else {
                kind::MORE
            };
            ids = members
                .iter()
                .copied()
                .filter(|&x| (x as usize) < sizes.n())
                .collect();
            size = ids.iter().map(|&x| sizes.size(nodes, x)).sum();
            if !members.is_empty() {
                tone = tone_of(nodes, now, members);
            }
            if let Some(t) = &self.compare {
                let (d, s) = aggregate_change(t.as_ref(), self.direction, members);
                delta = Some(d);
                dstat = Some(s);
            }
        } else {
            let node = z.node;
            ids = vec![node];
            if valid && !nodes.is_dir(node) {
                k = kind::FILE;
                tone = band(now, nodes.mtime(node));
            } else if valid && nodes.flags(node).intersects(Flags::PENDING) {
                k = kind::PENDING;
            } else {
                k = kind::FOLDER;
            }
            size = if (node as usize) < sizes.n() {
                sizes.size(nodes, node)
            } else {
                0
            };
            if node > 0
                && let Some(&c) = tags.get(&node)
            {
                tag = tag_index(c);
                flags |= flag::TAGGED;
            }
            if let Some(t) = &self.compare
                && (node as usize) < t.len()
            {
                delta = Some(t.delta(node));
                dstat = Some(t.status(node));
            }
        }
        if let Some(res) = res {
            let count: i64 = ids
                .iter()
                .filter(|&&x| (x as usize) < res.n)
                .map(|&x| res.count[x as usize] as i64)
                .sum();
            flags |= if count > 0 { flag::MATCH } else { flag::DIM };
            if z.members.is_none() && (z.node as usize) < res.n && res.folder_hit[z.node as usize] {
                flags |= flag::FOLDER_HIT;
            }
        }
        let (inside, is_bin) = self.ctx.trash.marks(nodes, &[z.node as i64]);
        if inside[0] {
            flags |= flag::TRASH;
        }
        if is_bin[0] && z.members.is_none() {
            flags |= flag::TRASH_BIN;
        }
        RootRow {
            key: z.key,
            node: z.node,
            kind: k,
            size,
            count: if z.members.is_some() { z.count } else { 0 },
            flags,
            tag,
            tone,
            branch: 0,
            delta,
            dstat,
        }
    }

    fn changes(&self, t: &dyn CompareSource, draw: &DrawList) -> (Vec<i64>, Vec<u8>) {
        let n = draw.len();
        let mut delta = vec![0i64; n];
        let mut dstat = vec![0u8; n];
        for i in 0..n {
            let k = draw.kind[i];
            let node = draw.node[i];
            if k <= kind::PENDING && node >= 0 && (node as usize) < t.len() {
                delta[i] = t.delta(node as NodeId);
                dstat[i] = t.status(node as NodeId);
            } else if (k == kind::SMALL || k == kind::MORE)
                && let Some(m) = draw.members.get(&draw.key[i])
                && !m.is_empty()
            {
                (delta[i], dstat[i]) = aggregate_change(t, self.direction, m);
            }
        }
        (delta, dstat)
    }

    /// Rows of the folders, down to [`SCANNING_LEVELS`] below the view root, whose subtree still
    /// holds folders not listed yet (UI-TM-42), and whether the zoom root (a folder) is one or is
    /// not listed itself. The not-listed folders climb to their ancestors one level per step; a
    /// folder reached before stops the climb there.
    fn scanning_rows(&mut self, nodes: &Nodes, draw: &DrawList) -> (Vec<usize>, bool) {
        let rows: Vec<usize> = (0..draw.len())
            .filter(|&i| {
                matches!(draw.kind[i], kind::FOLDER | kind::COLLAPSED)
                    && draw.depth[i] <= SCANNING_LEVELS
                    && draw.node[i] >= 0
            })
            .collect();
        let n = nodes.len();
        let z = &self.zoom;
        let root: i64 = if z.members.is_none() && (z.node as usize) < n && nodes.is_dir(z.node) {
            z.node as i64
        } else {
            -1
        };
        if rows.is_empty() && root < 0 {
            return (rows, false);
        }
        use rayon::prelude::*;
        let flags = nodes.flags_col();
        let wanted = Flags::PENDING.bits() | Flags::GONE.bits();
        let pend = Flags::PENDING.bits();
        let listing: Vec<NodeId> = (0..n)
            .into_par_iter()
            .with_min_len(1 << 15)
            .filter(|&i| flags[i].load(std::sync::atomic::Ordering::Relaxed) & wanted == pend)
            .map(|i| i as NodeId)
            .collect();
        if listing.is_empty() {
            return (Vec::new(), false);
        }
        let low = if root >= 0 {
            nodes.depth(root as NodeId)
        } else {
            rows.iter()
                .map(|&r| nodes.depth(draw.node[r] as NodeId))
                .min()
                .unwrap_or(0)
        };
        let reached = &mut self.scratch;
        reached.clear();
        reached.resize(n, false);
        let mut cur: Vec<NodeId> = Vec::with_capacity(listing.len());
        // the folders themselves are drawn as PENDING blocks, not marked
        cur.extend(
            listing
                .iter()
                .map(|&i| nodes.parent(i))
                .filter(|&p| p >= 0)
                .map(|p| p as NodeId),
        );
        let mut next = Vec::new();
        while !cur.is_empty() {
            next.clear();
            for &c in &cur {
                if reached[c as usize] {
                    continue;
                }
                reached[c as usize] = true;
                if nodes.depth(c) > low {
                    let p = nodes.parent(c);
                    if p >= 0 {
                        next.push(p as NodeId);
                    }
                }
            }
            std::mem::swap(&mut cur, &mut next);
        }
        let out: Vec<usize> = rows
            .into_iter()
            .filter(|&r| reached[draw.node[r] as usize])
            .collect();
        let root_hit = root >= 0 && {
            let r = root as usize;
            reached[r] || flags[r].load(std::sync::atomic::Ordering::Relaxed) & wanted == pend
        };
        (out, root_hit)
    }

    /// Rows for the pending flashes: the block itself, else its aggregate or nearest drawn
    /// ancestor (several changes in one row add up). The largest [`MAX_FLASH`] changes only.
    /// Partition layouts draw the zoom root too: a change below it that lands on no row is row -1.
    fn flash_rows(&self, nodes: &Nodes, draw: &DrawList) -> Vec<(i64, i64)> {
        let pending: HashMap<NodeId, i64> = std::mem::take(&mut *self.flashes.lock());
        if pending.is_empty() {
            return Vec::new();
        }
        let n = nodes.len();
        let rooted = draw.layout != DRAWLIST_LAYOUT;
        let mut items: Vec<(NodeId, i64)> = pending.into_iter().collect();
        items.sort_by(|a, b| b.1.abs().cmp(&a.1.abs()).then(a.0.cmp(&b.0)));
        items.truncate(MAX_FLASH);
        let member_of = member_index(draw);
        let mut rows: BTreeMap<i64, i64> = BTreeMap::new();
        for (node, delta) in items {
            if node as usize >= n {
                continue;
            }
            let mut row = draw.row(node).map(|r| r as i64);
            if row.is_none() {
                row = hidden_row(nodes, draw, node, Some(&member_of)).map(|r| r as i64);
            }
            if row.is_none() && rooted && self.in_zoom(nodes, node) {
                row = Some(-1);
            }
            if let Some(r) = row {
                *rows.entry(r).or_default() += delta;
            }
        }
        rows.into_iter().collect()
    }

    /// Whether `node` is the zoom root or below it (an aggregate zoom: one of its members).
    fn in_zoom(&self, nodes: &Nodes, node: NodeId) -> bool {
        let z = &self.zoom;
        let mut chain = std::iter::once(node).chain(nodes.ancestors(node));
        match &z.members {
            Some(m) => chain.any(|a| m.contains(&a)),
            None => z.node == 0 || chain.any(|a| a == z.node),
        }
    }

    /// Row of the selection: the selected block, or the aggregate / nearest ancestor holding it.
    fn selected_row(&self, nodes: &Nodes, draw: &DrawList) -> Option<usize> {
        let mut sel = self.selected?;
        if let Some(r) = draw.row(sel) {
            return Some(r);
        }
        if sel >= KEY_SMALL {
            let tag = sel & KEY_TAG_BITS;
            if tag != KEY_SMALL && tag != KEY_MORE {
                return None;
            }
            sel &= KEY_NODE_MASK;
            if sel == self.zoom.node || sel == 0 || sel as usize >= nodes.len() {
                return None;
            }
            if let Some(r) = draw.row(sel) {
                return Some(r);
            }
        }
        if sel as usize >= nodes.len() {
            return None;
        }
        hidden_row(nodes, draw, sel, None)
    }

    /// (key, label) from the home root to the zoom root.
    pub fn breadcrumb(&self) -> Vec<(u32, String)> {
        let nodes = self.ctx.store.read();
        self.breadcrumb_in(&nodes)
    }

    fn breadcrumb_in(&self, nodes: &Nodes) -> Vec<(u32, String)> {
        let z = &self.zoom;
        let mut chain: Vec<NodeId> = Vec::new();
        let mut cur = z.node as i64;
        while cur > 0 && (cur as usize) < nodes.len() {
            chain.push(cur as NodeId);
            if cur as NodeId == self.home {
                break;
            }
            cur = nodes.parent(cur as NodeId) as i64;
        }
        if self.home == 0 {
            chain.push(0);
        }
        let mut out: Vec<(u32, String)> = chain
            .iter()
            .rev()
            .map(|&k| {
                (
                    k,
                    if k == 0 {
                        String::new()
                    } else {
                        nodes.name_printable(k).into_owned()
                    },
                )
            })
            .collect();
        let mut aggregates: Vec<&Zoom> = Vec::new();
        let mut agg = Some(z);
        while let Some(a) = agg {
            if a.members.is_none() {
                break;
            }
            aggregates.push(a);
            agg = a.parent.as_deref();
        }
        for a in aggregates.iter().rev() {
            let what = if a.key & KEY_TAG_BITS == KEY_SMALL {
                "small"
            } else {
                "more"
            };
            out.push((a.key, format!("{what}:{}", a.count)));
        }
        out
    }

    /// A compare view moves to a recomputed change tree (live target): zoom root and selection
    /// follow by path, the history starts anew.
    pub fn rebase(&mut self, ctx: Arc<ViewContext>, compare: Option<Arc<dyn CompareSource>>) {
        let (zoom_path, sel_path) = {
            let old = self.ctx.store.read();
            let path_of =
                |node: u32| (node > 0 && (node as usize) < old.len()).then(|| old.path_bytes(node));
            (path_of(self.zoom.node), self.selected.and_then(path_of))
        };
        self.ctx = ctx;
        self.compare = compare;
        self.hues.clear();
        self.home = {
            let roots = self.ctx.roots.read();
            if roots.len() == 1 { roots[0].node } else { 0 }
        };
        {
            let nodes = self.ctx.store.read();
            self.zoom = zoom_path
                .and_then(|p| nodes.find(&p))
                .map_or(Zoom::at(self.home), Zoom::at);
            self.selected = sel_path.and_then(|p| nodes.find(&p));
        }
        self.back.clear();
        self.forward.clear();
        self.last = None;
        self.spare = None;
        self.refilter(None);
    }
}

/// `ids` without repeats and without the ones inside another of them, sorted: subtrees to take
/// out once each ([`WithoutBins`] takes each one's bytes off all its ancestors).
fn outermost(nodes: &Nodes, mut ids: Vec<NodeId>) -> Vec<NodeId> {
    ids.sort_unstable();
    ids.dedup();
    if ids.len() < 2 {
        return ids;
    }
    let all = ids.clone();
    ids.retain(|&id| !nodes.ancestors(id).any(|a| all.binary_search(&a).is_ok()));
    ids
}

/// An aggregate's net change (the shown direction's part) and status.
fn aggregate_change(
    t: &dyn CompareSource,
    direction: CompareDirection,
    members: &[NodeId],
) -> (i64, u8) {
    let mut d = 0i64;
    let mut first: Option<u8> = None;
    let mut same = true;
    for &m in members {
        if m as usize >= t.len() {
            continue;
        }
        let v = t.delta(m);
        d += match direction {
            CompareDirection::Grow if v <= 0 => 0,
            CompareDirection::Shrink if v >= 0 => 0,
            _ => v,
        };
        let s = t.status(m);
        match first {
            None => first = Some(s),
            Some(f) if f != s => same = false,
            _ => {}
        }
    }
    let st = match first {
        Some(f) if same && (f == 1 || f == 2) => f,
        _ => 3,
    };
    (d, st)
}

/// Member node -> its aggregate block's key, for many lookups in one frame.
fn member_index(draw: &DrawList) -> HashMap<NodeId, u32> {
    let mut m = HashMap::new();
    for (&key, members) in &draw.members {
        let tag = key & KEY_TAG_BITS;
        if tag == KEY_SMALL || tag == KEY_MORE {
            for &x in members {
                m.insert(x, key);
            }
        }
    }
    m
}

/// The drawn aggregate row holding an undrawn node, else its nearest drawn ancestor's row.
pub(crate) fn hidden_row(
    nodes: &Nodes,
    draw: &DrawList,
    node: NodeId,
    index: Option<&HashMap<NodeId, u32>>,
) -> Option<usize> {
    let key = match index {
        Some(ix) => ix.get(&node).copied(),
        None => draw.members.iter().find_map(|(&key, members)| {
            let tag = key & KEY_TAG_BITS;
            ((tag == KEY_SMALL || tag == KEY_MORE) && members.contains(&node)).then_some(key)
        }),
    };
    if let Some(key) = key
        && let Some(r) = draw.row(key)
    {
        return Some(r);
    }
    if node as usize >= nodes.len() {
        return None;
    }
    nodes.ancestors(node).find_map(|a| draw.row(a))
}
