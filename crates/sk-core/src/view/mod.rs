//! Views (Python `spaceknife.view`): per-tab / per-pane state over a shared scan — navigation,
//! detail, filter, tags, colours — rendered to frames.
//!
//! * [`state`]: [`View`] (zoom, history, selection, filter, render → [`Frame`]) and the
//!   [`ViewContext`] the views of one scan share.
//! * [`frame`]: the binary frame and the JSON `FrameInfo` / `FilterInfo` / `ViewState`.
//! * [`results`]: the W3 result list rows and pages.
//! * [`tags`]: [`TagStore`] (colour tags persisted by path); [`branch`]: branch colour slots;
//!   [`trash`]: recycle-bin detection and the size source without bins;
//!   [`compare`]: what a compare view needs of its change tree.

pub mod branch;
pub mod compare;
pub mod frame;
pub mod results;
pub mod state;
pub mod tags;
pub mod trash;

pub use compare::{ArrayChanges, CompareSource};
pub use frame::{encode_frame, encode_frame_into, filter_info, frame_info, view_state};
pub use results::{ResultEntry, ResultsCache, results_page, sorted_rows};
pub use state::{
    FilterState, Frame, RootInfo, RootRow, View, ViewContext, ViewSettings, Zoom, flag, showable,
};
pub use tags::TagStore;
