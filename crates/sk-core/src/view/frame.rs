//! A rendered [`Frame`] on the wire (docs/protocol.md §4; Python `server/views.py`
//! `encode_draw`, `frame_info`, `filter_info`, `view_state`): the binary frame with the common
//! columns, the layout's geometry columns, the frame's own columns and the `names` blob, and the
//! JSON `FrameInfo` / `FilterInfo` / `ViewState` that go with it.

use sk_protocol::ProtocolError;
use sk_protocol::RequestId;
use sk_protocol::binary::FrameBuilder;
use sk_protocol::view::{
    Crumb, DRAWLIST_LAYOUT, FilterInfo, FlashItem, FrameInfo, FrameRoot, ViewState, Viewport,
    columns as col, view_stream,
};

use super::state::{Frame, View};
use crate::layout::rows::geometry_columns;

/// Write the binary frame of `frame` into `out` (cleared first, its allocation reused).
pub fn encode_frame_into(
    frame: &Frame,
    view_id: &str,
    seq: u64,
    request_id: Option<&RequestId>,
    out: &mut Vec<u8>,
) -> Result<(), ProtocolError> {
    let d = &frame.draw;
    let stream = view_stream(view_id);
    let geo = geometry_columns(d.layout).unwrap_or(["x", "y", "w", "h"]);
    let mut b = FrameBuilder::new(&stream, d.layout)
        .seq(seq)
        .request_id(request_id)
        .column(col::KEY.0, &d.key)
        .column(col::NODE.0, &d.node)
        .column(col::PARENT.0, &d.parent)
        .column(col::KIND.0, &d.kind)
        .column(col::DEPTH.0, &d.depth)
        .column(col::CLS.0, &d.cls)
        .column(col::LABEL.0, &d.label)
        .column(col::SIZE.0, &d.size)
        .column(col::COUNT.0, &d.count);
    for (name, values) in geo.iter().zip(&d.geo) {
        b = b.column(name, values);
    }
    b = b
        .column(col::FLAGS.0, &frame.flags)
        .column(col::TAG.0, &frame.tag)
        .column(col::TONE.0, &frame.tone)
        .column(col::BRANCH.0, &frame.branch)
        .column(col::NAME_LEN.0, &d.name_len);
    if let (Some(delta), Some(dstat)) = (&frame.delta, &frame.dstat) {
        b = b.column(col::DELTA.0, delta).column(col::DSTAT.0, dstat);
    }
    b.blob(col::NAMES_BLOB, &d.names).finish_into(out)
}

/// The binary frame of `frame`.
pub fn encode_frame(
    frame: &Frame,
    view_id: &str,
    seq: u64,
    request_id: Option<&RequestId>,
) -> Result<Vec<u8>, ProtocolError> {
    let mut out = Vec::new();
    encode_frame_into(frame, view_id, seq, request_id, &mut out)?;
    Ok(out)
}

/// The view's filter for `FrameInfo.filter` / `ViewUpdate.filter` (None: no filter text).
pub fn filter_info(view: &View, frame: Option<&Frame>) -> Option<FilterInfo> {
    let f = &view.filter;
    if f.text.is_empty() && !f.active() {
        return None;
    }
    let (files, alloc, size) = match frame.and_then(|fr| fr.filter_totals) {
        Some(t) => t,
        None => match &f.result {
            Some(r) if view.zoom.members.is_none() && (view.zoom.node as usize) < r.n => {
                r.totals(view.zoom.node)
            }
            _ => (0, 0, 0),
        },
    };
    Some(FilterInfo {
        text: f.text.clone(),
        active: f.active(),
        stale: f.stale,
        mode: view.filter_mode,
        files,
        alloc,
        size,
        diagnostics: f.diagnostics.iter().take(8).cloned().collect(),
    })
}

/// `FrameInfo` of `frame` (sent as `seq`, rendered because of `reason`).
pub fn frame_info(view: &View, frame: &Frame, seq: u64, reason: &str) -> FrameInfo {
    let d = &frame.draw;
    let drawn = d.layout != DRAWLIST_LAYOUT; // the zoom root is drawn too: root bar, centre disc
    let r = &frame.root;
    FrameInfo {
        view_id: view.id.clone(),
        seq,
        width: d.width as i64,
        height: d.height as i64,
        dpr: view.viewport.2,
        rows: d.len() as i64,
        selected_row: frame.selected_row.map(|r| r as i64),
        breadcrumb: frame
            .breadcrumb
            .iter()
            .map(|(k, l)| Crumb {
                key: *k,
                label: l.clone(),
            })
            .collect(),
        scanned: frame.scanned_total,
        filter: filter_info(view, Some(frame)),
        layout_ms: (d.elapsed_s * 1000.0 * 100.0).round() / 100.0,
        reason: reason.to_owned(),
        flash: frame
            .flash
            .iter()
            .map(|&(row, delta)| FlashItem { row, delta })
            .collect(),
        visual: d.visual.clone(),
        layout: d.layout.to_owned(),
        geometry: d.meta.clone(),
        root: drawn.then_some(FrameRoot {
            key: r.key,
            node: r.node as i64,
            kind: r.kind,
            size: r.size,
            count: r.count,
            flags: r.flags,
            tag: r.tag,
            tone: r.tone,
            branch: r.branch,
            delta: r.delta,
            dstat: r.dstat,
        }),
        generation: frame.generation,
    }
}

/// `ViewState` of a view of scan `scan_id`.
pub fn view_state(view: &View, scan_id: &str) -> ViewState {
    let (w, h, dpr) = view.viewport;
    ViewState {
        view_id: view.id.clone(),
        scan_id: scan_id.to_owned(),
        zoom_node: view.zoom.node,
        zoom_key: view.zoom.key,
        detail: view.detail,
        min_element: view.min_element,
        sort_by_size: view.sort_by_size,
        show_free: view.show_free,
        show_unknown: view.show_unknown,
        show_trash: view.show_trash,
        has_trash: view.has_trash(),
        hide_folders: view.hide_folders,
        hidden_folders: view.hidden_folder_count(),
        visual: view.visual.clone(),
        color_mode: view.color_mode(),
        colors: view.colors(),
        filter_mode: view.filter_mode,
        viewport: Viewport {
            width: w,
            height: h,
            dpr,
        },
        selected: view.selected,
        results_sort: view.results_sort,
        compare: view.compare.is_some(),
        direction: view.direction,
        can_back: !view.back.is_empty(),
        can_forward: !view.forward.is_empty(),
        can_up: view.can_go_up(),
    }
}
