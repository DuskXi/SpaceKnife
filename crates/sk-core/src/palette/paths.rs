//! Path search and path input for the palette (UI-W4-26…29, UI-W4-33, UI-W4-37).
//!
//! Names are searched in tiers — exact name, name prefix, inside the name, scattered letters —
//! and each tier is ordered by size, largest first (UI-W4-28). Everything runs over the store's
//! name heap through the scan's [`NameIndex`], so a search over millions of names is a parallel
//! byte scan with no object per node.

use std::path::Path;

use sk_protocol::filter_syntax::normalize_glob;
use sk_protocol::palette::PathState;
use sk_store::{Flags, NodeId, Nodes};

use super::fuzzy::score;
use crate::filter::NameIndex;
/// The area basis per node, as the view sizes its blocks ([`crate::layout::StoreSizes`], or an
/// array: [`crate::layout::ArraySizes`]). `n()` is the node count it covers: nodes past it were
/// listed after it was taken (a running scan) and count as not scanned yet.
pub use crate::layout::SizeSource as Sizes;

#[cfg(windows)]
const SEPARATORS: &[char] = &['\\', '/'];
#[cfg(not(windows))]
const SEPARATORS: &[char] = &['/'];
const MAIN_SEP: char = std::path::MAIN_SEPARATOR;

/// One path hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathHit {
    pub node: NodeId,
    pub name: String,
    /// absolute path of the containing folder ("" for a scan root)
    pub parent: String,
    pub is_dir: bool,
    pub alloc: i64,
    /// highlighted character runs of `name`
    pub hits: Vec<(usize, usize)>,
    /// 0 exact name · 1 prefix · 2 inside the name · 3 scattered letters · 4 child listing
    pub tier: u8,
    /// absolute path, folders with a trailing separator
    pub complete: String,
}

impl PathHit {
    pub fn to_protocol(&self) -> sk_protocol::palette::PalettePath {
        sk_protocol::palette::PalettePath {
            node: self.node,
            name: self.name.clone(),
            parent: self.parent.clone(),
            is_dir: self.is_dir,
            alloc: self.alloc,
            hits: super::fuzzy::protocol_spans(&self.name, &self.hits),
            tier: self.tier,
            complete: self.complete.clone(),
        }
    }
}

/// Drive letter, UNC, absolute POSIX path or home-relative (UI-W4-33).
pub fn looks_like_path(text: &str) -> bool {
    let t = text.trim();
    let b = t.as_bytes();
    let drive = b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/');
    drive || t.starts_with("\\\\") || t.starts_with('/') || t.starts_with('~')
}

/// The text as a path: `~` expanded, `/` as `\` on Windows.
pub fn expand(text: &str) -> String {
    let t = crate::paths::expand_user(text.trim())
        .to_string_lossy()
        .into_owned();
    if cfg!(windows) {
        t.replace('/', "\\")
    } else {
        t
    }
}

/// Without trailing separators, except for a root (a drive root or `/`).
pub fn trimmed(path: &str) -> &str {
    let out = path.trim_end_matches(SEPARATORS);
    let drive = out.len() == 2 && out.as_bytes()[1] == b':';
    if out.is_empty() || drive {
        if path.len() > out.len() {
            let next = path[out.len()..].chars().next().map_or(0, char::len_utf8);
            return &path[..out.len() + next];
        }
        return path;
    }
    out
}

/// `("C:\Users\lenny\", "Vi")` for `C:\Users\lenny\Vi`.
pub fn split_last(path: &str) -> (&str, &str) {
    match path.rfind(SEPARATORS) {
        Some(cut) => path.split_at(cut + 1),
        None => ("", path),
    }
}

fn with_sep(path: String) -> String {
    if path.ends_with(SEPARATORS) {
        path
    } else {
        path + &MAIN_SEP.to_string()
    }
}

/// The display path of a node.
pub fn node_path(nodes: &Nodes, node: NodeId) -> String {
    nodes.path_lossy(node)
}

/// What a selection seeds the path search with: its name. A scan root is stored under its full
/// path and seeds with its last segment (`D:\Photos` -> `Photos`; a volume root with nothing).
pub fn seed_text(nodes: &Nodes, node: NodeId) -> String {
    let name = nodes.name(node).into_owned();
    if nodes.parent(node) != 0 {
        return name;
    }
    let t = name.trim_end_matches(SEPARATORS);
    if t.len() == 2 && t.as_bytes()[1] == b':' && cfg!(windows) {
        return String::new();
    }
    match t.rfind(SEPARATORS) {
        Some(i) => t[i + 1..].to_owned(),
        None => t.to_owned(),
    }
}

// ------------------------------------------------------------------ name search

fn escape(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    for ch in q.chars() {
        if matches!(ch, '\\' | '*' | '?') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// The single-character case variants of a character (as the name index compares them).
fn variants(ch: char) -> Vec<char> {
    let mut out = vec![ch];
    for v in [
        ch.to_lowercase().collect::<Vec<_>>(),
        ch.to_uppercase().collect::<Vec<_>>(),
    ] {
        if v.len() == 1 && !out.contains(&v[0]) {
            out.push(v[0]);
        }
    }
    out
}

/// The query's characters in order, anywhere in the name (non-ASCII queries).
fn subsequence(q: &str) -> impl Fn(&[u8]) -> bool + Sync + use<> {
    let wanted: Vec<Vec<char>> = q.chars().map(variants).collect();
    move |name: &[u8]| {
        let text = String::from_utf8_lossy(name);
        let mut k = 0;
        for ch in text.chars() {
            if k < wanted.len() && wanted[k].contains(&ch) {
                k += 1;
            }
        }
        k == wanted.len()
    }
}

/// The `k` largest of `ids`, largest first (ties by id).
fn top(nodes: &Nodes, ids: &[NodeId], sizes: &dyn Sizes, k: usize) -> Vec<NodeId> {
    if k == 0 || ids.is_empty() {
        return Vec::new();
    }
    let mut keyed: Vec<(i64, NodeId)> = ids.iter().map(|&i| (-sizes.size(nodes, i), i)).collect();
    if keyed.len() > k {
        keyed.select_nth_unstable(k - 1);
        keyed.truncate(k);
    }
    keyed.sort_unstable();
    keyed.into_iter().map(|(_, i)| i).collect()
}

fn ascii_lower(text: &str) -> Vec<char> {
    text.chars().map(|c| c.to_ascii_lowercase()).collect()
}

/// Highlighted runs of `name` (character runs) for a hit of `tier`.
fn hits_of(name: &str, query: &str, tier: u8) -> Vec<(usize, usize)> {
    let len = name.chars().count();
    match tier {
        0 => return vec![(0, len)],
        1 => return vec![(0, query.chars().count().min(len))],
        2 => {
            // ASCII-only lower-casing, as the index matches: offsets stay those of the name
            let (n, q) = (ascii_lower(name), ascii_lower(query));
            if q.is_empty() {
                return vec![(0, 0)];
            }
            if let Some(at) = n.windows(q.len()).position(|w| w == q.as_slice()) {
                return vec![(at, (at + q.len()).min(len))];
            }
        }
        _ => {}
    }
    score(query, name).map(|s| s.1).unwrap_or_default()
}

fn hit(
    nodes: &Nodes,
    sizes: &dyn Sizes,
    node: NodeId,
    query: &str,
    tier: u8,
    glob: bool,
) -> PathHit {
    let name = nodes.name(node).into_owned();
    let is_dir = nodes.is_dir(node);
    let parent = nodes.parent(node);
    let full = nodes.path_lossy(node);
    let hits = if tier == 4 || glob {
        Vec::new()
    } else {
        hits_of(&name, query, tier)
    };
    PathHit {
        node,
        parent: if parent > 0 {
            nodes.path_lossy(parent as NodeId)
        } else {
            String::new()
        },
        is_dir,
        alloc: sizes.size(nodes, node),
        hits,
        tier,
        complete: if is_dir { with_sep(full) } else { full },
        name,
    }
}

/// How [`search`] treats the query.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// leave out this node (the seed itself)
    pub exclude: Option<NodeId>,
    /// no scattered-letters tier
    pub contains_only: bool,
    /// `*` and `?` are plain characters (a selection's name as the seed)
    pub literal: bool,
}

/// The best `limit` nodes whose name matches `query`, and the number of all hits.
pub fn search(
    nodes: &Nodes,
    index: &NameIndex,
    sizes: &dyn Sizes,
    query: &str,
    limit: usize,
    opts: SearchOptions,
) -> (Vec<PathHit>, usize) {
    let q = query.trim();
    let n = nodes.len().min(sizes.n()); // sizes may be a moment older than a growing store
    if q.is_empty() || n <= 1 {
        return (Vec::new(), 0);
    }
    let wild = !opts.literal && q.contains(['*', '?']);
    let mut disjoint = true;
    let tiers: Vec<Vec<NodeId>> = if wild {
        // the user typed a glob: one tier, matched like a filter mask
        vec![index.match_glob(nodes, &normalize_glob(q), n)]
    } else if q.is_ascii() {
        // scattered letters only from three characters on: two scattered letters match almost anything
        let scattered = !opts.contains_only && q.len() >= 3;
        index.ascii_tiers(nodes, q, n, scattered).into()
    } else {
        disjoint = false;
        let lit = escape(q);
        let mut t = vec![
            index.match_glob(nodes, &normalize_glob(&lit), n),
            index.match_glob(nodes, &normalize_glob(&format!("{lit}*")), n),
            index.match_glob(nodes, &normalize_glob(&format!("*{lit}*")), n),
        ];
        if !opts.contains_only && q.chars().count() > 1 {
            let pred = subsequence(q);
            t.push(index.search(nodes, &format!("seq:{q}"), &pred, n));
        }
        t
    };
    let mut seen: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    let mut picked: Vec<(NodeId, u8)> = Vec::new();
    let mut total = 0usize;
    for (tier, ids) in tiers.into_iter().enumerate() {
        let ids: Vec<NodeId> = ids
            .into_iter()
            .filter(|&i| {
                i > 0
                    && (i as usize) < n
                    && !nodes.flags(i).intersects(Flags::GONE) // deleted, or the old copy of a moved row
                    && Some(i) != opts.exclude
                    && (disjoint || seen.insert(i))
            })
            .collect();
        total += ids.len();
        for node in top(nodes, &ids, sizes, limit.saturating_sub(picked.len())) {
            picked.push((node, if wild { 1 } else { tier as u8 }));
        }
    }
    let hits = picked
        .into_iter()
        .map(|(node, tier)| hit(nodes, sizes, node, q, tier, wild))
        .collect();
    (hits, total)
}

/// A folder's children for a path typed up to `partial` (UI-W4-37): prefix, inside, scattered.
pub fn children(
    nodes: &Nodes,
    sizes: &dyn Sizes,
    folder: NodeId,
    partial: &str,
    limit: usize,
) -> (Vec<PathHit>, usize) {
    // listed after `sizes` was taken (scan running): no size yet
    let kids: Vec<NodeId> = nodes
        .children(folder)
        .filter(|&k| (k as usize) < sizes.n())
        .collect();
    if partial.is_empty() {
        let hits = top(nodes, &kids, sizes, limit)
            .into_iter()
            .map(|k| hit(nodes, sizes, k, "", 4, false))
            .collect();
        return (hits, kids.len());
    }
    let p = partial.to_lowercase();
    let mut ranked: Vec<(u8, i64, NodeId)> = Vec::new();
    for k in kids {
        let name = nodes.name(k).to_lowercase();
        let tier = if name == p {
            0
        } else if name.starts_with(&p) {
            1
        } else if name.contains(&p) {
            2
        } else if score(partial, &name).is_some() {
            3
        } else {
            continue;
        };
        ranked.push((tier, -sizes.size(nodes, k), k));
    }
    ranked.sort_unstable();
    let total = ranked.len();
    let hits = ranked
        .into_iter()
        .take(limit)
        .map(|(t, _, k)| hit(nodes, sizes, k, partial, t, false))
        .collect();
    (hits, total)
}

// ------------------------------------------------------------------ the input as a path

/// The palette input read as a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathInput {
    pub state: PathState,
    pub path: String,
    pub node: Option<NodeId>,
    pub is_dir: bool,
    pub alloc: Option<i64>,
    pub complete: Option<String>,
}

/// The node at `path` once it has a size: one added after `sizes` was taken counts as not
/// scanned yet.
fn sized(nodes: Option<&Nodes>, sizes: Option<&dyn Sizes>, path: &str) -> Option<NodeId> {
    let (nodes, sizes) = (nodes?, sizes?);
    let node = nodes.find_path(Path::new(path))?;
    ((node as usize) < sizes.n()).then_some(node)
}

/// The input as a path: in the scan, elsewhere on disk, or missing; with its Tab completion.
pub fn resolve(nodes: Option<&Nodes>, sizes: Option<&dyn Sizes>, text: &str) -> PathInput {
    let path = expand(text);
    let complete = completion(nodes, sizes, &path);
    if let (Some(node), Some(n), Some(s)) = (sized(nodes, sizes, &path), nodes, sizes) {
        return PathInput {
            state: PathState::InScan,
            node: Some(node),
            is_dir: n.is_dir(node),
            alloc: Some(s.size(n, node)),
            complete,
            path,
        };
    }
    let meta = std::fs::metadata(&path);
    let (exists, is_dir) = match &meta {
        Ok(m) => (true, m.is_dir()),
        Err(_) => (false, false),
    };
    PathInput {
        state: if exists {
            PathState::Outside
        } else {
            PathState::Missing
        },
        path,
        node: None,
        is_dir,
        alloc: None,
        complete,
    }
}

/// The path completed by one segment (Tab), folders with a trailing separator.
pub fn completion(nodes: Option<&Nodes>, sizes: Option<&dyn Sizes>, path: &str) -> Option<String> {
    let (folder, partial) = split_last(path);
    if folder.is_empty() || partial.is_empty() {
        return None;
    }
    let p = partial.to_lowercase();
    if let (Some(node), Some(n), Some(s)) = (sized(nodes, sizes, folder), nodes, sizes)
        && n.is_dir(node)
    {
        let best = n
            .children(node)
            .filter(|&k| (k as usize) < s.n())
            .map(|k| (!n.is_dir(k), -s.size(n, k), n.name(k).to_lowercase(), k))
            .filter(|c| c.2.starts_with(&p))
            .min()?;
        let k = best.3;
        return Some(format!(
            "{folder}{}{}",
            n.name(k),
            if n.is_dir(k) {
                MAIN_SEP.to_string()
            } else {
                String::new()
            }
        ));
    }
    let mut found: Vec<(bool, String, String, bool)> = Vec::new();
    for e in std::fs::read_dir(folder).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        if !lower.starts_with(&p) {
            continue;
        }
        let no_follow_dir = e.file_type().is_ok_and(|t| t.is_dir());
        let is_dir = std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir());
        found.push((!no_follow_dir, lower, name, is_dir));
    }
    let (_, _, name, is_dir) = found.into_iter().min()?;
    Some(format!(
        "{folder}{name}{}",
        if is_dir {
            MAIN_SEP.to_string()
        } else {
            String::new()
        }
    ))
}
