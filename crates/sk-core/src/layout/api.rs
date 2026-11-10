//! The engine interface: what a visualization's layout engine gets and must return (Python
//! `layout/api.py`).
//!
//! Built-in visualizations and (later) plugins implement [`Engine`] and describe themselves with a
//! [`Visualization`], registered in [`super::registry`]. Hosts (views, the server) reach engines
//! only through the registry. [`ENGINE_API`] changes when this interface does.

use std::sync::Arc;

use sk_protocol::view::ColorModeName;
use sk_store::{NodeId, Nodes};

use super::basis::Basis;
use super::rows::DrawList;

pub const ENGINE_API: u32 = 1;
/// Rows per frame of a partition layout (flame.v1, polar.v1); the treemap has no limit.
pub const MAX_ROWS: usize = 50_000;

/// A visualization's own setting value ([`LayoutParams::options`]).
#[derive(Clone, Debug, PartialEq)]
pub enum OptValue {
    Str(String),
    Num(f64),
    Bool(bool),
}

/// The viewport and the view's settings for one run.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutParams {
    /// CSS px
    pub width: f64,
    pub height: f64,
    pub dpr: f64,
    /// levels drawn below the zoom root (1–15)
    pub detail: u32,
    /// CSS px: blocks below min_element² are merged
    pub min_element: f64,
    pub sort_by_size: bool,
    pub gap: f64,
    pub border: f64,
    pub padding: f64,
    pub header: f64,
    pub min_draw: f64,
    pub min_content: f64,
    pub expand_min_w: f64,
    pub expand_min_h: f64,
    /// 「元素字号」: header height and label thresholds scale with it
    pub font_scale: f64,
    /// 「元素比例」: the width / height cells aim for
    pub aspect: f64,
    /// the visualization's own settings, by name
    pub options: Vec<(String, OptValue)>,
}

impl Default for LayoutParams {
    fn default() -> Self {
        LayoutParams {
            width: 1280.0,
            height: 800.0,
            dpr: 1.0,
            detail: 5,
            min_element: 14.0,
            sort_by_size: true,
            gap: 2.0,
            border: 1.0,
            padding: 2.0,
            header: 18.0,
            min_draw: 2.0,
            min_content: 4.0,
            expand_min_w: 48.0,
            expand_min_h: 40.0,
            font_scale: 1.0,
            aspect: 1.0,
            options: Vec::new(),
        }
    }
}

impl LayoutParams {
    /// A viewport of `width` × `height` CSS px, the rest default.
    pub fn new(width: f64, height: f64) -> LayoutParams {
        LayoutParams {
            width,
            height,
            ..LayoutParams::default()
        }
    }

    /// String option `name`; `default` when it is not set or not a string.
    pub fn opt_str<'a>(&'a self, name: &str, default: &'a str) -> &'a str {
        for (k, v) in &self.options {
            if k == name {
                return match v {
                    OptValue::Str(s) => s,
                    _ => default,
                };
            }
        }
        default
    }

    /// Canvas size in physical px.
    pub fn canvas(&self) -> (i32, i32) {
        (
            (self.width * self.dpr).round_ties_even() as i32,
            (self.height * self.dpr).round_ties_even() as i32,
        )
    }
}

/// What fills the view: a node's children, or the members of an aggregate block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RootSpec {
    pub node: NodeId,
    /// zoomed into a SMALL/MORE block: just these nodes
    pub members: Option<Arc<Vec<NodeId>>>,
    /// whole-volume root: free and unknown space blocks
    pub free: Option<i64>,
    pub unknown: Option<i64>,
    /// the SMALL/MORE block zoomed into: no block inside reuses its key
    pub key: Option<u32>,
}

impl RootSpec {
    pub fn node(node: NodeId) -> RootSpec {
        RootSpec {
            node,
            ..RootSpec::default()
        }
    }
}

/// Area basis per node for one run (bytes).
pub trait SizeSource: Sync {
    /// Nodes covered: ids from 0 to `n` - 1.
    fn n(&self) -> usize;
    /// The size of one node (`id < n`).
    fn size(&self, nodes: &Nodes, id: NodeId) -> i64;
}

/// Area basis of a node as the store has it: subtree allocation for folders, own allocation for
/// files, 0 for hard-link duplicates.
#[inline]
pub fn effective_alloc(nodes: &Nodes, id: NodeId) -> i64 {
    nodes.effective_size(id).1
}

/// The store's own sizes ([`effective_alloc`]).
#[derive(Clone, Copy, Debug)]
pub struct StoreSizes {
    pub n: usize,
}

impl StoreSizes {
    pub fn of(nodes: &Nodes) -> StoreSizes {
        StoreSizes { n: nodes.len() }
    }
}

impl SizeSource for StoreSizes {
    fn n(&self) -> usize {
        self.n
    }
    #[inline]
    fn size(&self, nodes: &Nodes, id: NodeId) -> i64 {
        effective_alloc(nodes, id)
    }
}

/// Sizes from an array: hide mode's matching bytes, a compare view's amounts.
#[derive(Clone, Debug)]
pub struct ArraySizes(pub Arc<Vec<i64>>);

impl SizeSource for ArraySizes {
    fn n(&self) -> usize {
        self.0.len()
    }
    #[inline]
    fn size(&self, _nodes: &Nodes, id: NodeId) -> i64 {
        self.0[id as usize]
    }
}

/// Lays out one store for one visualization. Engines keep no per-store state (that is the
/// [`Basis`]), so one engine serves every view.
pub trait Engine: Send + Sync {
    /// The rows of `root` in the viewport, sized by `sizes`, written into `out` (cleared first);
    /// satisfies the row contract (`super::contract::check_draw`).
    fn run(
        &self,
        nodes: &Nodes,
        basis: &Basis,
        params: &LayoutParams,
        root: &RootSpec,
        sizes: &dyn SizeSource,
        out: &mut DrawList,
    );

    /// Levels below the zoom root this viewport can show (at most `params.detail`).
    fn levels(&self, params: &LayoutParams) -> u32;
}

/// A visualization the server offers: its engine and the wire layout frontends draw it with.
#[derive(Clone)]
pub struct Visualization {
    /// "treemap", "flame", "flame_polar"; plugins: dotted / dashed lowercase words
    pub id: String,
    /// [`ENGINE_API`] it was written against
    pub api: u32,
    /// wire layout of its frames ("drawlist.v1", "flame.v1", "polar.v1")
    pub layout: &'static str,
    /// what a session must have announced to get it ("" for the baseline drawlist.v1)
    pub capability: String,
    /// message id of its name (Chinese source text; translate with the i18n module)
    pub title_id: String,
    /// design icon key
    pub icon: String,
    /// place in pickers, then id
    pub order: i32,
    pub engine: Arc<dyn Engine>,
    /// colour mode a view starts with on it
    pub default_color: ColorModeName,
    /// can show compare views
    pub compare: bool,
    /// shows free and unknown space at a whole-volume root
    pub free_space: bool,
}

impl std::fmt::Debug for Visualization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Visualization")
            .field("id", &self.id)
            .field("layout", &self.layout)
            .field("capability", &self.capability)
            .field("order", &self.order)
            .finish()
    }
}
