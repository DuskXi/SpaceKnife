//! Filter result rows of a view (W3 sidebar, UI-W3-31…33; Python `view/results.py`).
//!
//! Rows follow the drawn frame: a matching file that is drawn on its own is one row; matches hidden
//! in a collapsed folder, a "N small items" / "+N items" block, or under a sliver too small to draw
//! are grouped into the visible block that holds them (the last kind: one "rest" row for the zoom
//! root). Every match below the zoom root lands in exactly one row, so the rows add up to the
//! frame totals.

use std::sync::Arc;

use sk_protocol::filter::TagColor;
use sk_protocol::filter_syntax::casefold;
use sk_protocol::view::{ResultKind, ResultRow, ResultsSortName, ViewResults};
use sk_store::{NodeId, Nodes};

use super::state::View;
use crate::filter::engine::Order;
use crate::layout::rows::kind;

/// One result row before it is paged out (the protocol row has the path and tag instead of
/// `parent`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultEntry {
    /// draw-list key of the block to locate; the zoom key for "rest"
    pub key: u32,
    /// the file or folder; the container for "group" and "rest"
    pub node: NodeId,
    pub kind: ResultKind,
    /// file / folder name; for a one-file group the file's name, else ""
    pub name: String,
    /// matching files in this row
    pub count: i64,
    pub alloc: i64,
    pub size: i64,
    /// folder holding the block (path column)
    pub parent: NodeId,
}

/// All result rows for the view's current filter and last frame, unsorted.
pub fn result_rows(view: &View) -> Vec<ResultEntry> {
    let (Some(res), Some(frame)) = (
        view.filter.result.as_ref().filter(|_| view.filter.active()),
        view.last.as_ref(),
    ) else {
        return Vec::new();
    };
    let nodes = view.ctx.store.read();
    let draw = &frame.draw;
    let n = res.n.min(nodes.len());
    let rest = draw.len() as i64; // pseudo row index
    let mut owner = vec![-1i64; n];
    for i in 0..draw.len() {
        let nd = draw.node[i];
        let k = draw.kind[i];
        if k <= kind::PENDING && nd >= 0 && (nd as usize) < n {
            owner[nd as usize] = i as i64;
        }
    }
    for i in 0..draw.len() {
        if (draw.kind[i] == kind::SMALL || draw.kind[i] == kind::MORE)
            && let Some(m) = draw.members.get(&draw.key[i])
        {
            for &x in m {
                if (x as usize) < n {
                    owner[x as usize] = i as i64;
                }
            }
        }
    }
    let z = &view.zoom;
    let tops: Vec<NodeId> = match &z.members {
        None => vec![z.node],
        Some(m) => m.to_vec(),
    };
    for t in tops {
        if (t as usize) < n && owner[t as usize] < 0 {
            owner[t as usize] = rest;
        }
    }
    // undrawn nodes belong to the nearest drawn ancestor; nodes outside the zoom root stay -1
    let parent = nodes.parents();
    Order::of(&nodes, n).down(n, |i| {
        if owner[i] < 0 {
            let p = parent[i];
            if p >= 0 {
                owner[i] = owner[p as usize];
            }
        }
    });
    // group the matches by their row: (row, count, alloc, size, first match)
    let mut groups: std::collections::BTreeMap<i64, (i64, i64, i64, NodeId)> = Default::default();
    for (i, &o) in owner.iter().enumerate() {
        if res.matched[i] && o >= 0 {
            let g = groups.entry(o).or_insert((0, 0, 0, i as NodeId));
            g.0 += 1;
            g.1 += res.alloc[i];
            g.2 += res.size[i];
        }
    }
    let mut out = Vec::with_capacity(groups.len());
    for (row, (count, alloc, size, first)) in groups {
        if row == rest {
            out.push(ResultEntry {
                key: z.key,
                node: z.node,
                kind: ResultKind::Rest,
                name: String::new(),
                count,
                alloc,
                size,
                parent: z.node,
            });
            continue;
        }
        let r = row as usize;
        let rk = draw.kind[r];
        let nd = draw.node[r] as NodeId;
        let key = draw.key[r];
        let parent_of = |x: NodeId| nodes.parent(x).max(0) as NodeId;
        let (kind_, name, parent) = if rk == kind::SMALL || rk == kind::MORE {
            let name = if count == 1 {
                nodes.name_printable(first).into_owned()
            } else {
                String::new()
            };
            (ResultKind::Group, name, nd)
        } else if rk == kind::FILE && !nodes.is_dir(nd) {
            (
                ResultKind::File,
                nodes.name_printable(nd).into_owned(),
                parent_of(nd),
            )
        } else {
            (
                ResultKind::Folder,
                nodes.name_printable(nd).into_owned(),
                parent_of(nd),
            )
        };
        out.push(ResultEntry {
            key,
            node: nd,
            kind: kind_,
            name,
            count,
            alloc,
            size,
            parent,
        });
    }
    out
}

/// Rows in the order `by`.
pub fn sort_rows(mut rows: Vec<ResultEntry>, by: ResultsSortName) -> Vec<ResultEntry> {
    match by {
        ResultsSortName::Name => {
            let mut keyed: Vec<(String, ResultEntry)> = rows
                .into_iter()
                .map(|r| {
                    let k = if r.name.is_empty() {
                        "\u{ffff}".to_owned()
                    } else {
                        casefold(&r.name)
                    };
                    (k, r)
                })
                .collect();
            keyed.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then(b.1.alloc.cmp(&a.1.alloc))
                    .then(a.1.key.cmp(&b.1.key))
            });
            keyed.into_iter().map(|(_, r)| r).collect()
        }
        ResultsSortName::Count => {
            rows.sort_by(|a, b| {
                b.count
                    .cmp(&a.count)
                    .then(b.alloc.cmp(&a.alloc))
                    .then(a.key.cmp(&b.key))
            });
            rows
        }
        ResultsSortName::Size => {
            rows.sort_by(|a, b| {
                b.alloc
                    .cmp(&a.alloc)
                    .then(b.count.cmp(&a.count))
                    .then(a.key.cmp(&b.key))
            });
            rows
        }
    }
}

/// Cache of [`sorted_rows`]: (frame serial, filter result, order) -> rows.
#[derive(Default)]
pub struct ResultsCache {
    hit: Option<(u64, usize, ResultsSortName, Arc<Vec<ResultEntry>>)>,
}

/// The view's result rows in its current order, cached per (frame, filter result, order).
pub fn sorted_rows(view: &View, cache: &mut ResultsCache) -> Arc<Vec<ResultEntry>> {
    let serial = view.last.as_ref().map_or(0, |f| f.serial);
    let res = view
        .filter
        .result
        .as_ref()
        .filter(|_| view.filter.active())
        .map_or(0, |r| Arc::as_ptr(r) as usize);
    if let Some((s, r, by, rows)) = &cache.hit
        && (*s, *r, *by) == (serial, res, view.results_sort)
    {
        return rows.clone();
    }
    let rows = Arc::new(sort_rows(result_rows(view), view.results_sort));
    cache.hit = Some((serial, res, view.results_sort, rows.clone()));
    rows
}

/// Path of a folder below its scan root (the root's own name first when a view shows several).
pub fn relative_path(view: &View, node: NodeId) -> String {
    let nodes = view.ctx.store.read();
    relative_path_in(&nodes, view.home, node)
}

fn relative_path_in(nodes: &Nodes, home: NodeId, node: NodeId) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = node as i64;
    while cur > 0 && (cur as usize) < nodes.len() && nodes.parent(cur as NodeId) > 0 {
        parts.push(nodes.name_printable(cur as NodeId).into_owned());
        cur = nodes.parent(cur as NodeId) as i64;
    }
    if home == 0 && cur > 0 {
        // roots are named by their full path: its last name, cut from the stored bytes (an
        // escaped name holds a backslash itself)
        let root = nodes.name_bytes(cur as NodeId);
        let sep = |b: &u8| *b == b'/' || (cfg!(windows) && *b == b'\\');
        let end = root.len() - root.iter().rev().take_while(|b| sep(b)).count();
        let trimmed = &root[..end];
        let base = trimmed.rsplit(sep).next().unwrap_or(&[]);
        parts.push(
            sk_store::names::printable(if base.is_empty() { root } else { base }).into_owned(),
        );
    }
    parts.reverse();
    parts.join(std::path::MAIN_SEPARATOR_STR)
}

/// (tag colour, inherited) of a node: its own tag, else the nearest tagged ancestor's.
pub fn effective_tag(view: &View, node: NodeId) -> (Option<TagColor>, bool) {
    let nodes = view.ctx.store.read();
    let resolved = view.ctx.tags.lock().resolve(&nodes);
    if let Some(&c) = resolved.get(&node) {
        return (Some(c), false);
    }
    if (node as usize) < nodes.len() {
        for a in nodes.ancestors(node) {
            if let Some(&c) = resolved.get(&a) {
                return (Some(c), true);
            }
        }
    }
    (None, false)
}

/// One page of the result list (`view.results`), grouped by the frame `seq` was sent with.
pub fn results_page(
    view: &View,
    cache: &mut ResultsCache,
    seq: u64,
    offset: usize,
    limit: usize,
) -> ViewResults {
    let rows = sorted_rows(view, cache);
    let (files, alloc, size) = view
        .last
        .as_ref()
        .and_then(|f| f.filter_totals)
        .unwrap_or((0, 0, 0));
    let mut out = Vec::new();
    for r in rows.iter().skip(offset).take(limit) {
        let (tag, inherited) = if matches!(r.kind, ResultKind::File | ResultKind::Folder) {
            effective_tag(view, r.node)
        } else {
            (None, false)
        };
        out.push(ResultRow {
            key: r.key,
            node: r.node,
            kind: r.kind,
            name: r.name.clone(),
            count: r.count,
            alloc: r.alloc,
            size: r.size,
            path: relative_path(view, r.parent),
            tag,
            tag_inherited: inherited,
        });
    }
    ViewResults {
        view_id: view.id.clone(),
        seq,
        active: view.filter.active(),
        sort: view.results_sort,
        total: rows.len() as i64,
        files,
        alloc,
        size,
        max_alloc: rows.iter().map(|r| r.alloc).max().unwrap_or(0),
        offset: offset as i64,
        rows: out,
        // the rows are the last frame's
        generation: view.last.as_ref().map_or(0, |f| f.generation),
    }
}
