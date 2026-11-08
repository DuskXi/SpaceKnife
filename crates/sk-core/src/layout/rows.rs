//! Rows of a draw list: what every layout emits, whatever it draws (Python `layout/rows.py`).
//!
//! One row per drawn block, parents before their children. The common columns (key, node, parent,
//! kind, depth, class, label bits, size, count) mean the same in every layout; each wire layout
//! adds its own four geometry columns. See docs/protocol.md §4 for the row contract. Columns are
//! plain vectors kept across frames ([`DrawList::clear`]): a frame of a view reuses the buffers
//! of the frame before the last one.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use sk_protocol::view::{DRAWLIST_LAYOUT, FLAME_LAYOUT, FrameGeometry, POLAR_LAYOUT};
use sk_store::NodeId;

/// `| container id`: "N small items" of that container.
pub const KEY_SMALL: u32 = 1 << 30;
/// `| container id`: "+N items" of that container (in an aggregate zoom: `| its largest member`).
pub const KEY_MORE: u32 = 2 << 30;
pub const KEY_FREE: u32 = (3 << 30) | 1;
pub const KEY_UNKNOWN: u32 = (3 << 30) | 2;
pub const KEY_NODE_MASK: u32 = (1 << 30) - 1;
/// The tag part of a key.
pub const KEY_TAG_BITS: u32 = 3 << 30;

/// Aggregate zoom: the share of the view a shrunk "+N items" block keeps.
pub const PAGE_REST: f64 = 0.125;

/// `cls` of a row in no one class: a folder no folder class swallows, an aggregate of mixed
/// classes, "+N items". It paints like the Other class but never counts as it.
pub const NO_CLASS: u8 = 255;

/// Row kinds (`kind` column).
pub mod kind {
    pub const FILE: u8 = 0;
    /// expanded: frame + header (treemap) or solid bar / sector, children inside or beyond
    pub const FOLDER: u8 = 1;
    /// folder drawn as a solid block (detail level reached or too small)
    pub const COLLAPSED: u8 = 2;
    /// folder not scanned yet
    pub const PENDING: u8 = 3;
    /// "N small items"
    pub const SMALL: u8 = 4;
    /// "+N items"
    pub const MORE: u8 = 5;
    pub const FREE: u8 = 6;
    pub const UNKNOWN: u8 = 7;
}

/// Label bits (`label` column).
pub mod label {
    pub const NAME: u8 = 1;
    /// second line with the size
    pub const SIZE: u8 = 2;
    /// area > 52,000 px²: larger type
    pub const BIG: u8 = 4;
    /// height < 22 px: name only, smaller type, centred vertically
    pub const SHORT: u8 = 8;
    /// polar.v1: the name runs along the radius
    pub const RADIAL: u8 = 16;
}

/// Label bits of a `cw` × `ch` CSS px text box (UI-TM-14..16). `big`: the larger type is allowed
/// (treemap blocks; bars and sectors keep the normal size).
pub fn label_box(cw: f64, ch: f64, big: bool) -> u8 {
    if cw < 30.0 || ch < 15.0 {
        return 0;
    }
    if ch < 22.0 {
        return label::NAME | label::SHORT;
    }
    label::NAME
        | if cw >= 42.0 && ch >= 31.0 {
            label::SIZE
        } else {
            0
        }
        | if big && cw * ch > 52_000.0 {
            label::BIG
        } else {
            0
        }
}

/// Lookups per frame answered by scanning the keys, then they are indexed.
const ROW_SCANS: u32 = 8;

/// Geometry column names per wire layout.
pub fn geometry_columns(layout: &str) -> Option<[&'static str; 4]> {
    match layout {
        DRAWLIST_LAYOUT | FLAME_LAYOUT => Some(["x", "y", "w", "h"]),
        POLAR_LAYOUT => Some(["a0", "a1", "r0", "r1"]),
        _ => None,
    }
}

/// Columns of a draw list: one row per drawn block, parents before their children.
#[derive(Debug, Default)]
pub struct DrawList {
    /// stable id: node id, or a tagged key for synthetic blocks
    pub key: Vec<u32>,
    /// node id (container id for SMALL/MORE, -1 for FREE/UNKNOWN)
    pub node: Vec<i32>,
    /// row of the enclosing FOLDER, -1 at the top
    pub parent: Vec<i32>,
    pub kind: Vec<u8>,
    /// nesting level, 1 = child of the zoom root
    pub depth: Vec<u8>,
    /// index into the class table; [`NO_CLASS`]: in no one class
    pub cls: Vec<u8>,
    pub label: Vec<u8>,
    /// bytes (area basis, except a shrunk "+N items" in an aggregate zoom)
    pub size: Vec<i64>,
    /// files in SMALL, entries in MORE
    pub count: Vec<u32>,
    /// the layout's geometry columns ([`geometry_columns`]): x y w h, or a0 a1 r0 r1
    pub geo: [Vec<i32>; 4],
    /// UTF-8 names of the rows, back to back (the `names` blob)
    pub names: Vec<u8>,
    /// bytes of each row's name in [`DrawList::names`] (0: no label drawn)
    pub name_len: Vec<u32>,
    /// SMALL/MORE key -> member node ids
    pub members: HashMap<u32, Vec<NodeId>>,
    /// canvas in physical px
    pub width: i32,
    pub height: i32,
    pub elapsed_s: f64,
    /// wire layout of the rows
    pub layout: &'static str,
    /// the visualization that laid them out
    pub visual: String,
    /// `FrameInfo.geometry` (None: drawlist.v1)
    pub meta: Option<FrameGeometry>,
    /// levels below the zoom root the layout could draw
    pub levels: u32,
    index: OnceLock<HashMap<u32, u32>>,
    lookups: AtomicU32,
}

impl Clone for DrawList {
    fn clone(&self) -> Self {
        DrawList {
            key: self.key.clone(),
            node: self.node.clone(),
            parent: self.parent.clone(),
            kind: self.kind.clone(),
            depth: self.depth.clone(),
            cls: self.cls.clone(),
            label: self.label.clone(),
            size: self.size.clone(),
            count: self.count.clone(),
            geo: self.geo.clone(),
            names: self.names.clone(),
            name_len: self.name_len.clone(),
            members: self.members.clone(),
            width: self.width,
            height: self.height,
            elapsed_s: self.elapsed_s,
            layout: self.layout,
            visual: self.visual.clone(),
            meta: self.meta.clone(),
            levels: self.levels,
            index: OnceLock::new(),
            lookups: AtomicU32::new(0),
        }
    }
}

/// One row as an engine emits it.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub key: u32,
    pub node: i32,
    pub parent: i32,
    pub kind: u8,
    pub depth: u8,
    pub cls: u8,
    pub label: u8,
    pub size: i64,
    pub count: u32,
    pub geo: [i32; 4],
}

impl DrawList {
    pub fn new() -> DrawList {
        DrawList {
            layout: DRAWLIST_LAYOUT,
            visual: "treemap".to_owned(),
            ..DrawList::default()
        }
    }

    /// Empty every column, keeping the buffers.
    pub fn clear(&mut self) {
        self.key.clear();
        self.node.clear();
        self.parent.clear();
        self.kind.clear();
        self.depth.clear();
        self.cls.clear();
        self.label.clear();
        self.size.clear();
        self.count.clear();
        for g in &mut self.geo {
            g.clear();
        }
        self.names.clear();
        self.name_len.clear();
        self.members.clear();
        self.width = 0;
        self.height = 0;
        self.elapsed_s = 0.0;
        self.meta = None;
        self.levels = 0;
        self.index = OnceLock::new();
        self.lookups.store(0, Ordering::Relaxed);
    }

    pub fn len(&self) -> usize {
        self.key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.key.is_empty()
    }

    /// Append a row with its name bytes (empty: no label).
    #[inline]
    pub fn push(&mut self, r: Row, name: &[u8]) {
        self.key.push(r.key);
        self.node.push(r.node);
        self.parent.push(r.parent);
        self.kind.push(r.kind);
        self.depth.push(r.depth);
        self.cls.push(r.cls);
        self.label.push(r.label);
        self.size.push(r.size);
        self.count.push(r.count);
        for (g, v) in self.geo.iter_mut().zip(r.geo) {
            g.push(v);
        }
        let before = self.names.len();
        push_name(&mut self.names, name);
        self.name_len.push((self.names.len() - before) as u32);
    }

    pub fn x(&self) -> &[i32] {
        &self.geo[0]
    }
    pub fn y(&self) -> &[i32] {
        &self.geo[1]
    }
    pub fn w(&self) -> &[i32] {
        &self.geo[2]
    }
    pub fn h(&self) -> &[i32] {
        &self.geo[3]
    }

    /// The name of every row ("" where no label is drawn).
    pub fn name_list(&self) -> Vec<&str> {
        let mut out = Vec::with_capacity(self.len());
        let mut at = 0usize;
        for &l in &self.name_len {
            let b = &self.names[at..at + l as usize];
            out.push(std::str::from_utf8(b).unwrap_or(""));
            at += l as usize;
        }
        out
    }

    /// The first row with `key`. The first few lookups of a frame scan the key column (a render
    /// looks up one or two); after that the keys are indexed (change flashes look up hundreds).
    pub fn row(&self, key: u32) -> Option<usize> {
        if let Some(index) = self.index.get() {
            return index.get(&key).map(|&r| r as usize);
        }
        if self.lookups.fetch_add(1, Ordering::Relaxed) < ROW_SCANS {
            return self.key.iter().position(|&k| k == key);
        }
        let index = self.index.get_or_init(|| {
            let mut m = HashMap::with_capacity(self.key.len());
            for (i, &k) in self.key.iter().enumerate() {
                m.entry(k).or_insert(i as u32);
            }
            m
        });
        index.get(&key).map(|&r| r as usize)
    }
}

/// Append a name as UTF-8, as 0.3.4's frames carried it: what is not Unicode as `?`
/// ([`sk_store::names::frame_label`]).
#[inline]
pub fn push_name(out: &mut Vec<u8>, raw: &[u8]) {
    match std::str::from_utf8(raw) {
        Ok(_) => out.extend_from_slice(raw),
        Err(_) => out.extend_from_slice(sk_store::names::frame_label(raw).as_bytes()),
    }
}
