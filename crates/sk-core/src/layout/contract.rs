//! The row contract every layout engine keeps (docs/protocol.md §4.1), checked on one draw list
//! (Python `layout/contract.py`).
//!
//! [`check_draw`] returns the broken rules, worded for a test failure; empty means the draw list is
//! fine. The rules:
//!
//! 1. parents come first: `parent[i] < i`; a parent row is an expanded folder one level up;
//! 2. keys are unique;
//! 3. rows of kind ≤ PENDING are real nodes (key = node id, file / folder / pending as in the
//!    store); synthetic rows carry their tagged keys;
//! 4. a node belongs to one aggregate block at most and is then not a row itself; a block's count
//!    is its number of members;
//! 5. (`full`) every node below the zoom root is a row, a member of an aggregate block or below a
//!    drawn row, except blocks too small to draw; every row's size is its nodes' bytes;
//! 6. only real rows with label bits have a name, and it is the node's name;
//! 7. partition layouts have at most `MAX_ROWS` rows;
//! 8. the geometry matches the layout; blocks lie on the canvas and inside their parent; in a
//!    partition the rows of one level share their band or ring and follow each other along it.

use std::collections::{HashMap, HashSet};

use sk_protocol::view::{DRAWLIST_LAYOUT, FLAME_LAYOUT, FrameGeometry, POLAR_LAYOUT};
use sk_store::{Flags, Kind, NodeId, Nodes};

use super::api::{MAX_ROWS, RootSpec, SizeSource, StoreSizes};
use super::rows::{
    DrawList, KEY_FREE, KEY_MORE, KEY_NODE_MASK, KEY_SMALL, KEY_TAG_BITS, KEY_UNKNOWN, kind,
};

/// Rule 5, treemap: what a block too small to draw spans at most along its container.
pub const SLACK_PX: f64 = 20.0;
/// Offending rows quoted per problem.
const SHOWN: usize = 5;

struct Report<'a>(&'a mut Vec<String>);

impl Report<'_> {
    fn rows(&mut self, rule: u32, what: &str, bad: impl IntoIterator<Item = usize>) {
        let bad: Vec<usize> = bad.into_iter().collect();
        if !bad.is_empty() {
            let more = if bad.len() > SHOWN {
                format!(" (+{} more)", bad.len() - SHOWN)
            } else {
                String::new()
            };
            self.0.push(format!(
                "rule {rule}: {what}: rows {:?}{more}",
                &bad[..bad.len().min(SHOWN)]
            ));
        }
    }
}

/// Problems of `draw`, laid out for `root` with `sizes` (None: the store's own sizes).
/// `full` adds rule 5, which walks the children of every expanded row.
pub fn check_draw(
    draw: &DrawList,
    nodes: &Nodes,
    root: &RootSpec,
    full: bool,
    sizes: Option<&dyn SizeSource>,
) -> Vec<String> {
    let mut problems = Vec::new();
    if !well_formed(draw, &mut problems) {
        return problems;
    }
    let n = draw.len();
    let n_nodes = nodes.len();
    let rows = 0..n;
    let parent = &draw.parent;
    let k = &draw.kind;
    let depth = &draw.depth;
    let top = |i: usize| parent[i] == -1;
    let inner = |i: usize| parent[i] >= 0 && (parent[i] as usize) < i;
    let real = |i: usize| k[i] <= kind::PENDING;
    let mut r = Report(&mut problems);

    // 1. parents first
    r.rows(
        1,
        "parent is not an earlier row",
        rows.clone().filter(|&i| !top(i) && !inner(i)),
    );
    r.rows(
        1,
        "parent row is not an expanded folder",
        rows.clone()
            .filter(|&i| inner(i) && k[parent[i] as usize] != kind::FOLDER),
    );
    r.rows(
        1,
        "depth is not one below the parent's",
        rows.clone().filter(|&i| {
            let want = if inner(i) {
                (depth[parent[i] as usize] as u32 + 1).min(255) as u8
            } else {
                1
            };
            (top(i) || inner(i)) && depth[i] != want
        }),
    );

    // 2. unique keys
    let mut seen = HashSet::with_capacity(n);
    let dups: Vec<usize> = rows
        .clone()
        .filter(|&i| !seen.insert(draw.key[i]))
        .collect();
    if !dups.is_empty() {
        let repeated: HashSet<u32> = dups.iter().map(|&i| draw.key[i]).collect();
        r.rows(2, &format!("{} keys repeat", repeated.len()), dups);
    }

    // 3. real nodes, tagged synthetic keys
    let node = &draw.node;
    let in_store = |i: usize| real(i) && node[i] >= 1 && (node[i] as usize) < n_nodes;
    r.rows(
        3,
        "real row without a node of the store",
        rows.clone().filter(|&i| real(i) && !in_store(i)),
    );
    r.rows(
        3,
        "real row keyed other than by its node",
        rows.clone()
            .filter(|&i| in_store(i) && draw.key[i] as i64 != node[i] as i64),
    );
    let is_dir = |i: usize| nodes.kind(node[i] as NodeId) == Kind::Dir;
    let pending = |i: usize| nodes.flags(node[i] as NodeId).intersects(Flags::PENDING);
    r.rows(
        3,
        "file row of a folder",
        rows.clone()
            .filter(|&i| in_store(i) && k[i] == kind::FILE && is_dir(i)),
    );
    r.rows(
        3,
        "folder row of a file",
        rows.clone()
            .filter(|&i| in_store(i) && k[i] != kind::FILE && !is_dir(i)),
    );
    r.rows(
        3,
        "pending row of a listed folder",
        rows.clone()
            .filter(|&i| in_store(i) && k[i] == kind::PENDING && !pending(i)),
    );
    r.rows(
        3,
        "folder row of a folder not listed yet",
        rows.clone()
            .filter(|&i| in_store(i) && k[i] != kind::PENDING && is_dir(i) && pending(i)),
    );
    r.rows(
        3,
        "unknown kind",
        rows.clone().filter(|&i| k[i] > kind::UNKNOWN),
    );
    r.rows(
        3,
        "SMALL row keyed other than KEY_SMALL | container",
        rows.clone().filter(|&i| {
            k[i] == kind::SMALL && draw.key[i] != KEY_SMALL | (node[i] as u32 & KEY_NODE_MASK)
        }),
    );
    r.rows(
        3,
        "MORE row without a KEY_MORE key",
        rows.clone()
            .filter(|&i| k[i] == kind::MORE && draw.key[i] & KEY_TAG_BITS != KEY_MORE),
    );
    r.rows(
        3,
        "FREE row keyed other than KEY_FREE on node -1",
        rows.clone()
            .filter(|&i| k[i] == kind::FREE && (draw.key[i] != KEY_FREE || node[i] != -1)),
    );
    r.rows(
        3,
        "UNKNOWN row keyed other than KEY_UNKNOWN on node -1",
        rows.clone()
            .filter(|&i| k[i] == kind::UNKNOWN && (draw.key[i] != KEY_UNKNOWN || node[i] != -1)),
    );

    // 4. aggregate blocks
    let odd: Vec<u32> = draw
        .members
        .keys()
        .copied()
        .filter(|key| ![KEY_SMALL, KEY_MORE].contains(&(key & KEY_TAG_BITS)))
        .collect();
    if !odd.is_empty() {
        r.0.push(format!(
            "rule 4: member blocks with keys that are not SMALL / MORE: {:?}",
            &odd[..odd.len().min(SHOWN)]
        ));
    }
    let mut every: HashSet<NodeId> = HashSet::new();
    let mut twice = false;
    let mut outside = false;
    for m in draw.members.values() {
        for &id in m {
            twice |= !every.insert(id);
            outside |= id < 1 || id as usize >= n_nodes;
        }
    }
    if twice {
        r.0.push("rule 4: a node is a member of more than one block (or twice of one)".into());
    }
    if outside {
        r.0.push("rule 4: a member is not a node of the store".into());
    }
    r.rows(
        4,
        "row of a node that is also a member",
        rows.clone()
            .filter(|&i| real(i) && node[i] >= 0 && every.contains(&(node[i] as NodeId))),
    );
    let agg = |i: usize| k[i] == kind::SMALL || k[i] == kind::MORE;
    r.rows(
        4,
        "aggregate row without its members",
        rows.clone()
            .filter(|&i| agg(i) && !draw.members.contains_key(&draw.key[i])),
    );
    r.rows(
        4,
        "aggregate row whose count is not its number of members",
        rows.clone().filter(|&i| {
            agg(i)
                && draw
                    .members
                    .get(&draw.key[i])
                    .is_some_and(|m| m.len() != draw.count[i] as usize)
        }),
    );

    // 5. coverage and sizes
    if full && !problems_block_coverage(r.0) {
        coverage(draw, nodes, root, sizes, &every, &mut r);
    }

    // 6. names
    let names = draw.name_list();
    r.rows(
        6,
        "name is not the node's (labelled real rows) or empty (the rest)",
        rows.clone().filter(|&i| {
            let want = if draw.label[i] != 0 && in_store(i) {
                nodes.name(node[i] as NodeId).into_owned()
            } else {
                String::new()
            };
            names[i] != want
        }),
    );

    // 7. row budget
    if draw.layout != DRAWLIST_LAYOUT && n > MAX_ROWS {
        r.0.push(format!("rule 7: {n} rows, more than {MAX_ROWS}"));
    }

    // 8. geometry
    geometry(draw, &mut r);
    problems
}

fn problems_block_coverage(p: &[String]) -> bool {
    p.iter()
        .any(|s| s.starts_with("rule 1") || s.starts_with("rule 3"))
}

fn well_formed(draw: &DrawList, problems: &mut Vec<String>) -> bool {
    let n = draw.len();
    let lens = [
        ("node", draw.node.len()),
        ("parent", draw.parent.len()),
        ("kind", draw.kind.len()),
        ("depth", draw.depth.len()),
        ("cls", draw.cls.len()),
        ("label", draw.label.len()),
        ("size", draw.size.len()),
        ("count", draw.count.len()),
        ("name_len", draw.name_len.len()),
    ];
    for (name, l) in lens {
        if l != n {
            problems.push(format!("rule 8: column {name} has {l} rows, not {n}"));
        }
    }
    for (i, g) in draw.geo.iter().enumerate() {
        if g.len() != n {
            problems.push(format!(
                "rule 8: geometry column {i} has {} rows, not {n}",
                g.len()
            ));
        }
    }
    let ok_meta = match (draw.layout, &draw.meta) {
        (DRAWLIST_LAYOUT, None) => true,
        (FLAME_LAYOUT, Some(FrameGeometry::Flame(_))) => true,
        (POLAR_LAYOUT, Some(FrameGeometry::Polar(_))) => true,
        (DRAWLIST_LAYOUT | FLAME_LAYOUT | POLAR_LAYOUT, _) => false,
        (other, _) => {
            problems.push(format!("rule 8: unknown layout {other:?}"));
            return false;
        }
    };
    if !ok_meta {
        problems.push(format!(
            "rule 8: meta does not match the layout {}",
            draw.layout
        ));
    }
    problems.is_empty()
}

fn coverage(
    draw: &DrawList,
    nodes: &Nodes,
    root: &RootSpec,
    sizes: Option<&dyn SizeSource>,
    every: &HashSet<NodeId>,
    r: &mut Report<'_>,
) {
    let own = StoreSizes::of(nodes);
    let sizes: &dyn SizeSource = sizes.unwrap_or(&own);
    let n_nodes = nodes.len().min(sizes.n());
    let size_of = |id: NodeId| -> i64 {
        if (id as usize) < n_nodes {
            sizes.size(nodes, id)
        } else {
            0
        }
    };
    let children = |of: NodeId| -> Vec<NodeId> {
        let (a, b) = nodes.child_range(of);
        (a..b.min(n_nodes as NodeId)).collect()
    };
    let n = draw.len();
    let k = &draw.kind;
    let real = |i: usize| k[i] <= kind::PENDING;
    let agg = |i: usize| k[i] == kind::SMALL || k[i] == kind::MORE;
    r.rows(
        5,
        "size is not the node's",
        (0..n).filter(|&i| real(i) && draw.size[i] != size_of(draw.node[i].max(0) as NodeId)),
    );
    r.rows(
        5,
        "aggregate size is not its members' bytes",
        (0..n).filter(|&i| {
            agg(i)
                && draw
                    .members
                    .get(&draw.key[i])
                    .is_some_and(|m| draw.size[i] != m.iter().map(|&id| size_of(id)).sum::<i64>())
        }),
    );
    r.rows(
        5,
        "free space other than the root's",
        (0..n).filter(|&i| k[i] == kind::FREE && draw.size[i] != root.free.unwrap_or(0)),
    );
    r.rows(
        5,
        "unknown space other than the root's",
        (0..n).filter(|&i| k[i] == kind::UNKNOWN && draw.size[i] != root.unknown.unwrap_or(0)),
    );
    let mut extra = root.free.unwrap_or(0) + root.unknown.unwrap_or(0).max(0);
    let is_dir = root.members.is_some()
        || ((root.node as usize) < nodes.len() && nodes.kind(root.node) == Kind::Dir);
    let mut containers: Vec<(i64, Vec<NodeId>, i64)> = Vec::new();
    if draw.meta.is_some() && (!is_dir || draw.levels == 0) {
        // partitions draw a file root, and a root with no band or ring, from FrameRoot alone
    } else if let Some(m) = &root.members {
        containers.push((-1, m.to_vec(), 0));
    } else if is_dir {
        containers.push((-1, children(root.node), size_of(root.node)));
    } else {
        containers.push((-1, vec![root.node], 0));
        extra = 0;
    }
    for (i, &rk) in k.iter().enumerate() {
        if rk == kind::FOLDER {
            containers.push((i as i64, children(draw.node[i] as NodeId), draw.size[i]));
        }
    }
    let slack = match &draw.meta {
        None => SLACK_PX,
        Some(FrameGeometry::Flame(f)) => (2 * f.gap + 2) as f64,
        Some(FrameGeometry::Polar(p)) => (2 * p.gap + 2) as f64,
    };
    let mut under_rows: HashMap<i64, Vec<usize>> = HashMap::new();
    for i in 0..n {
        under_rows.entry(draw.parent[i] as i64).or_default().push(i);
    }
    let empty = Vec::new();
    for (row, ids, own) in containers {
        let under = under_rows.get(&row).unwrap_or(&empty);
        let idset: HashSet<NodeId> = ids.iter().copied().collect();
        let drawn: HashSet<NodeId> = under
            .iter()
            .filter(|&&i| real(i))
            .map(|&i| draw.node[i] as NodeId)
            .collect();
        r.rows(
            5,
            &format!("row outside its container (row {row})"),
            under
                .iter()
                .copied()
                .filter(|&i| real(i) && !idset.contains(&(draw.node[i] as NodeId))),
        );
        r.rows(
            5,
            &format!("aggregate members outside its container (row {row})"),
            under.iter().copied().filter(|&i| {
                agg(i)
                    && draw
                        .members
                        .get(&draw.key[i])
                        .is_some_and(|m| !m.iter().all(|id| idset.contains(id)))
            }),
        );
        let s: Vec<i64> = ids.iter().map(|&id| size_of(id)).collect();
        let missing: Vec<usize> = (0..ids.len())
            .filter(|&j| s[j] > 0 && !drawn.contains(&ids[j]) && !every.contains(&ids[j]))
            .collect();
        if missing.is_empty() {
            continue;
        }
        let positive: i64 = s.iter().filter(|&&v| v > 0).sum();
        let basis = (positive + if row < 0 { extra } else { 0 }).max(own);
        let ext = extent(draw, row, under);
        let spans: Vec<(NodeId, f64)> = missing
            .iter()
            .map(|&j| (ids[j], s[j] as f64 / basis as f64 * ext))
            .filter(|&(_, span)| span > slack)
            .collect();
        if !spans.is_empty() {
            let max = spans.iter().map(|s| s.1).fold(0.0, f64::max);
            let where_ = if row < 0 {
                "the zoom root".to_owned()
            } else {
                format!("row {row}")
            };
            let shown: Vec<NodeId> = spans.iter().take(SHOWN).map(|s| s.0).collect();
            r.0.push(format!(
                "rule 5: nodes of {where_} are neither rows nor members though they span up to {max:.0} px: {shown:?}"
            ));
        }
    }
}

/// Pixels along which `row` (-1: the zoom root) shares out its children.
fn extent(draw: &DrawList, row: i64, under: &[usize]) -> f64 {
    let g = &draw.geo;
    match &draw.meta {
        Some(FrameGeometry::Polar(m)) => {
            let level = if row < 0 {
                0.0
            } else {
                draw.depth[row as usize] as f64
            };
            let turn = if row < 0 {
                1.0
            } else {
                (g[1][row as usize] - g[0][row as usize]) as f64 / m.unit as f64
            };
            turn * 2.0 * std::f64::consts::PI * (m.hole as f64 + (level + 0.5) * m.ring_w as f64)
        }
        Some(FrameGeometry::Flame(_)) => {
            if row < 0 {
                draw.width as f64
            } else {
                g[2][row as usize] as f64
            }
        }
        None => {
            if row < 0 {
                return draw.width.min(draw.height) as f64;
            }
            if under.is_empty() {
                return 0.0;
            }
            let x0 = under.iter().map(|&i| g[0][i]).min().unwrap_or(0) as i64;
            let x1 = under
                .iter()
                .map(|&i| g[0][i] as i64 + g[2][i] as i64)
                .max()
                .unwrap_or(0);
            let y0 = under.iter().map(|&i| g[1][i]).min().unwrap_or(0) as i64;
            let y1 = under
                .iter()
                .map(|&i| g[1][i] as i64 + g[3][i] as i64)
                .max()
                .unwrap_or(0);
            (x1 - x0).min(y1 - y0) as f64
        }
    }
}

fn geometry(draw: &DrawList, r: &mut Report<'_>) {
    let n = draw.len();
    let parent = &draw.parent;
    let inner = |i: usize| parent[i] >= 0 && (parent[i] as usize) < i;
    let g: [Vec<i64>; 4] = draw
        .geo
        .clone()
        .map(|c| c.into_iter().map(i64::from).collect());
    let (w_canvas, h_canvas) = (draw.width as i64, draw.height as i64);
    if draw.layout == DRAWLIST_LAYOUT || draw.layout == FLAME_LAYOUT {
        let [x, y, w, h] = &g;
        r.rows(8, "empty block", (0..n).filter(|&i| w[i] <= 0 || h[i] <= 0));
        r.rows(
            8,
            "block off the canvas",
            (0..n).filter(|&i| {
                x[i] < 0 || y[i] < 0 || x[i] + w[i] > w_canvas || y[i] + h[i] > h_canvas
            }),
        );
        r.rows(
            8,
            "block sticks out of its parent",
            (0..n).filter(|&i| {
                inner(i) && {
                    let p = parent[i] as usize;
                    x[i] < x[p] || x[i] + w[i] > x[p] + w[p]
                }
            }),
        );
        if draw.layout == DRAWLIST_LAYOUT {
            r.rows(
                8,
                "block sticks out of its parent",
                (0..n).filter(|&i| {
                    inner(i) && {
                        let p = parent[i] as usize;
                        y[i] < y[p] || y[i] + h[i] > y[p] + h[p]
                    }
                }),
            );
        } else {
            let ends: Vec<i64> = (0..n).map(|i| x[i] + w[i]).collect();
            levels(draw, x, &ends, [y, h], r);
        }
    } else {
        let [a0, a1, r0, r1] = &g;
        let unit = match &draw.meta {
            Some(FrameGeometry::Polar(m)) => m.unit,
            _ => sk_protocol::view::ANGLE_UNIT,
        };
        r.rows(
            8,
            "empty or out-of-turn sector",
            (0..n).filter(|&i| a0[i] < 0 || a1[i] <= a0[i] || a1[i] > unit),
        );
        r.rows(
            8,
            "empty ring",
            (0..n).filter(|&i| r0[i] < 0 || r1[i] <= r0[i]),
        );
        r.rows(
            8,
            "sector sticks out of its parent",
            (0..n).filter(|&i| {
                inner(i) && {
                    let p = parent[i] as usize;
                    a0[i] < a0[p] || a1[i] > a1[p] || r0[i] < r1[p]
                }
            }),
        );
        levels(draw, a0, a1, [r0, r1], r);
    }
}

/// Partition layouts: one band / ring per level, its rows in order along it without overlaps.
fn levels(draw: &DrawList, start: &[i64], end: &[i64], band: [&Vec<i64>; 2], r: &mut Report<'_>) {
    let mut by_depth: std::collections::BTreeMap<u8, Vec<usize>> = Default::default();
    for (i, &d) in draw.depth.iter().enumerate() {
        by_depth.entry(d).or_default().push(i);
    }
    for (d, rows) in by_depth {
        for col in band {
            r.rows(
                8,
                &format!("level {d} is not one band"),
                rows.iter().copied().filter(|&i| col[i] != col[rows[0]]),
            );
        }
        r.rows(
            8,
            &format!("level {d} is out of order or overlaps"),
            rows.windows(2)
                .filter(|w| start[w[1]] < end[w[0]])
                .map(|w| w[1]),
        );
    }
}
