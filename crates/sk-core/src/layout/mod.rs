//! Layout engines (the treemap, the flame graphs; the interface every visualization's engine
//! implements) and their draw list (Python `spaceknife.layout`).
//!
//! * [`api`]: [`LayoutParams`], [`RootSpec`], [`SizeSource`], the [`Engine`] trait and
//!   [`Visualization`].
//! * [`rows`]: the [`DrawList`] (struct of column vectors, reused across frames), keys, kinds, labels.
//! * [`basis`]: per-store inputs ([`Basis`]: colour classes) and safe child reads.
//! * [`treemap`] (`drawlist.v1`), [`partition`] with [`flame`] (`flame.v1`) and [`flame_polar`]
//!   (`polar.v1`); [`merge`]: the small-item merges and aggregate pages they share.
//! * [`registry`]: the visualizations on offer; [`contract`]: the row contract as a checker.

pub mod api;
pub mod basis;
pub mod contract;
pub mod flame;
pub mod flame_polar;
pub mod merge;
pub mod partition;
pub mod registry;
pub mod rows;
pub mod squarify;
pub mod treemap;

pub use api::{
    ArraySizes, ENGINE_API, Engine, LayoutParams, MAX_ROWS, OptValue, RootSpec, SizeSource,
    StoreSizes, Visualization, effective_alloc,
};
pub use basis::{Basis, Classes, children};
pub use rows::{
    DrawList, KEY_FREE, KEY_MORE, KEY_NODE_MASK, KEY_SMALL, KEY_TAG_BITS, KEY_UNKNOWN, NO_CLASS,
    PAGE_REST, Row, kind, label, label_box,
};
pub use treemap::{Geom, TreemapEngine, treemap};
