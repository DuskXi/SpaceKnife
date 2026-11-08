//! Evaluation of a filter's normal form over the store's columns (Python `filter/engine.py`).
//!
//! Semantics (docs/filter-syntax.md, section 5): candidates are the non-directory entries. A file
//! matches when every facet holds — name/class (any), folder (any ancestor below the scan root),
//! tag (any of the colours, own or inherited), every size and age range, the attribute set — and
//! no negated name/class/folder/tag/attribute condition holds. Directories never match
//! themselves; their per-subtree match counts and bytes are aggregated bottom-up.
//!
//! [`evaluate`] reads the rows of one [`Nodes`] view, so it runs while a scan keeps appending.
//! Per-node facets are evaluated in parallel (rayon); the propagation up and down the tree is one
//! sequential pass in id order (ids grow away from the roots while a scan builds the tree), or in
//! depth order after a relist moved folders.

use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;
use sk_protocol::classes::OTHER;
use sk_protocol::filter::{AttrName, AttrSet, FilterNormal, TagColor};
use sk_protocol::filter_syntax::{GlobPart, glob_parts};
use sk_store::{Flags, Kind, NodeId, Nodes};

use super::classes::{ClassMap, FALLBACK_BASE, FolderClasses, looks_like_stream};
use super::names::{Glob, NameIndex};

/// Store flag of each attribute.
pub fn attr_flag(a: AttrName) -> Flags {
    match a {
        AttrName::Archive => Flags::ARCHIVE,
        AttrName::System => Flags::SYSTEM,
        AttrName::Readonly => Flags::READONLY,
        AttrName::Hidden => Flags::HIDDEN,
        AttrName::Compressed => Flags::COMPRESSED,
        AttrName::Encrypted => Flags::ENCRYPTED,
        AttrName::Offline => Flags::OFFLINE,
        AttrName::Temporary => Flags::TEMPORARY,
        AttrName::Notindexed => Flags::NOT_INDEXED,
        AttrName::Sparse => Flags::SPARSE,
        AttrName::Ads => Flags::ADS,
    }
}

/// Own tag bits of a colour (the tags column of [`FilterContext::tags`]).
pub const fn tag_bit(c: TagColor) -> u8 {
    match c {
        TagColor::Red => 1,
        TagColor::Yellow => 2,
        TagColor::Green => 4,
        TagColor::Blue => 8,
    }
}

/// What an evaluation needs besides the store and the expression.
#[derive(Clone)]
pub struct FilterContext {
    /// unix seconds the ages are measured from
    pub now: i64,
    /// the class table (`:class:`)
    pub classes: Arc<ClassMap>,
    /// folder classes (a game install's files are 「游戏」 whatever their extension)
    pub folder_classes: Option<Arc<dyn FolderClasses>>,
    /// own tag bits per node ([`tag_bit`]); inherited downwards
    pub tags: Option<Arc<[u8]>>,
    /// the store's name index (None: a temporary one)
    pub names: Option<Arc<NameIndex>>,
}

impl FilterContext {
    /// A context at `now` with the built-in classes and nothing else.
    pub fn at(now: i64) -> FilterContext {
        FilterContext {
            now,
            classes: ClassMap::active(),
            folder_classes: None,
            tags: None,
            names: None,
        }
    }

    /// [`FilterContext::at`] the current time.
    pub fn current() -> FilterContext {
        FilterContext::at(unix_now())
    }
}

/// Seconds since the epoch.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The outcome of one evaluation: per node of the view it ran on.
#[derive(Clone, Debug, Default)]
pub struct FilterResult {
    /// nodes evaluated (ids < n)
    pub n: usize,
    /// this file matches
    pub matched: Vec<bool>,
    /// matching files in the subtree (1/0 for a file)
    pub count: Vec<u32>,
    /// allocated bytes of the matching files in the subtree (hard-link duplicates add none)
    pub alloc: Vec<i64>,
    /// logical bytes of the matching files in the subtree
    pub size: Vec<i64>,
    /// a folder that matches a positive folder mask (bold)
    pub folder_hit: Vec<bool>,
    pub elapsed_s: f64,
}

impl FilterResult {
    /// (matching files, allocated bytes, logical bytes) below `node`.
    pub fn totals(&self, node: NodeId) -> (i64, i64, i64) {
        let i = node as usize;
        if i >= self.n {
            return (0, 0, 0);
        }
        (self.count[i] as i64, self.alloc[i], self.size[i])
    }
}

/// Tree order for propagation: ids in an order where every parent comes before its children.
pub(crate) enum Order {
    /// id order works (every parent id is below its children's)
    Ids,
    /// by depth, ids ascending within a level
    Depth(Vec<NodeId>),
}

impl Order {
    pub(crate) fn of(nodes: &Nodes, n: usize) -> Order {
        let parent = &nodes.parents()[..n];
        let ordered = parent
            .par_iter()
            .enumerate()
            .skip(1)
            .all(|(i, &p)| (p as i64) < i as i64);
        if ordered {
            return Order::Ids;
        }
        let depth = &nodes.depths()[..n];
        let max = depth.iter().copied().max().unwrap_or(0) as usize;
        let mut start = vec![0usize; max + 2];
        for &d in depth {
            start[d as usize + 1] += 1;
        }
        for d in 1..start.len() {
            start[d] += start[d - 1];
        }
        let mut out = vec![0 as NodeId; n];
        for (i, &d) in depth.iter().enumerate() {
            out[start[d as usize]] = i as NodeId;
            start[d as usize] += 1;
        }
        Order::Depth(out)
    }

    /// Visit nodes parents first (node 0 and roots excluded where their parent is node 0 is not:
    /// every node with a parent ≥ 0).
    #[inline]
    pub(crate) fn down(&self, n: usize, mut f: impl FnMut(usize)) {
        match self {
            Order::Ids => (1..n).for_each(&mut f),
            Order::Depth(o) => o.iter().for_each(|&i| f(i as usize)),
        }
    }

    /// Visit nodes children first.
    #[inline]
    pub(crate) fn up(&self, n: usize, mut f: impl FnMut(usize)) {
        match self {
            Order::Ids => (1..n).rev().for_each(&mut f),
            Order::Depth(o) => o.iter().rev().for_each(|&i| f(i as usize)),
        }
    }
}

/// OR every node's value into its descendants (folder hits, inherited tags); roots do not inherit
/// from node 0.
fn down<T: Copy + std::ops::BitOrAssign>(order: &Order, parent: &[i32], values: &mut [T]) {
    let n = values.len();
    order.down(n, |i| {
        let p = parent[i];
        if p > 0 {
            let v = values[p as usize];
            values[i] |= v;
        }
    });
}

struct Eval<'a> {
    nodes: &'a Nodes,
    ctx: &'a FilterContext,
    n: usize,
    kind: &'a [u8],
    parent: &'a [i32],
    depth: &'a [u16],
    ext: &'a [u16],
    order: Order,
    index: Arc<NameIndex>,
}

#[inline]
fn is_leaf(kind: u8) -> bool {
    kind != Kind::Dir as u8
}

/// The last path component of a scan root's name (`D:\Photos` → `Photos`; `C:\`, `/` → "").
pub fn root_base_name(raw: &[u8]) -> &[u8] {
    let sep = |b: &u8| *b == b'/' || (cfg!(windows) && *b == b'\\');
    let mut end = raw.len();
    while end > 0 && sep(&raw[end - 1]) {
        end -= 1;
    }
    let trimmed = &raw[..end];
    let start = trimmed.iter().rposition(sep).map_or(0, |i| i + 1);
    let base = &trimmed[start..];
    // a drive ("C:") is no name
    if cfg!(windows) && start == 0 && base.len() == 2 && base[1] == b':' {
        return &[];
    }
    base
}

impl<'a> Eval<'a> {
    fn new(nodes: &'a Nodes, ctx: &'a FilterContext) -> Eval<'a> {
        let n = nodes.len();
        Eval {
            nodes,
            ctx,
            n,
            kind: nodes.kinds_raw(),
            parent: nodes.parents(),
            depth: nodes.depths(),
            ext: nodes.exts(),
            order: Order::of(nodes, n),
            index: ctx
                .names
                .clone()
                .unwrap_or_else(|| Arc::new(NameIndex::new())),
        }
    }

    /// Per node: the name matches (files and directories alike), OR-ed into `out`.
    fn name_mask_into(&self, pattern: &str, out: &mut [bool]) {
        let parts = glob_parts(pattern);
        if parts == [GlobPart::Any] {
            out.fill(true);
            return;
        }
        if let [GlobPart::Any, GlobPart::Lit(lit)] = parts.as_slice()
            && let Some(ext) = lit.strip_prefix('.')
            && !ext.is_empty()
            && !ext.contains('.')
            && ext.chars().count() <= 16
        {
            self.ext_fast(pattern, ext, out);
            return;
        }
        for id in self.index.match_glob(self.nodes, pattern, self.n) {
            out[id as usize] = true;
        }
    }

    /// `*.ext`: files by extension id; other leaves and the dotfile `.ext` checked by name.
    fn ext_fast(&self, pattern: &str, ext: &str, out: &mut [bool]) {
        let table = self.nodes.ext_table();
        let mut wanted = vec![false; table.len().max(1)];
        for (i, e) in table.iter().enumerate() {
            if i > 0 && &**e == ext {
                wanted[i] = true;
            }
        }
        let dotfile_len = ext.len() + 1;
        let glob = Glob::new(pattern);
        let name_len = self.nodes.name_lens();
        let (kind, exts, nodes) = (self.kind, self.ext, self.nodes);
        out.par_iter_mut()
            .enumerate()
            .with_min_len(1 << 14)
            .for_each(|(i, o)| {
                let k = kind[i];
                if k == Kind::File as u8 {
                    let e = exts[i] as usize;
                    if wanted.get(e).copied().unwrap_or(false)
                        || (e == 0
                            && name_len[i] as usize == dotfile_len
                            && glob.matches(nodes.name_bytes(i as NodeId)))
                    {
                        *o = true;
                    }
                } else if k != Kind::Dir as u8 && glob.matches(nodes.name_bytes(i as NodeId)) {
                    *o = true;
                }
            });
    }

    /// Folders matching any mask: every folder below a scan root, and the root itself under its
    /// own name. Folders above the root never take part.
    fn folders(&self, patterns: &[String]) -> Vec<bool> {
        let mut hit = vec![false; self.n];
        for p in patterns {
            self.name_mask_into(p, &mut hit);
        }
        let (kind, depth) = (self.kind, self.depth);
        hit.par_iter_mut()
            .enumerate()
            .with_min_len(1 << 14)
            .for_each(|(i, h)| {
                *h = *h && kind[i] == Kind::Dir as u8 && depth[i] >= 2;
            });
        let globs: Vec<Glob> = patterns.iter().map(|p| Glob::new(p)).collect();
        for root in self.nodes.roots() {
            let r = root as usize;
            if r < self.n && self.kind[r] == Kind::Dir as u8 {
                let base = root_base_name(self.nodes.name_bytes(root));
                hit[r] = globs.iter().any(|g| g.matches(base));
            }
        }
        hit
    }

    /// Files whose class is `class_id`: by extension (and size, for `ts`), under the folder
    /// classes.
    fn classes(&self, class_id: &str, folder: Option<&[u8]>) -> Vec<bool> {
        let map = &*self.ctx.classes;
        let table = self.nodes.ext_table();
        let idx = map.index_of(class_id);
        let other = map.other_index();
        let by_ext = match idx {
            Some(i) => map.ext_mask(i, &table),
            None => vec![false; table.len()],
        };
        let other_ext = map.ext_mask(other, &table);
        // size-split extensions: ext id -> (packet, is the class a recording's, is it a plain file's,
        // is "other" a plain file's)
        let splits: Vec<(u16, i64, bool, bool, bool)> = map
            .splits(&table)
            .into_iter()
            .map(|s| {
                let plain = idx.is_some_and(|i| s.rest.contains(&i))
                    || (class_id == OTHER && s.rest.is_empty());
                (
                    s.ext_id,
                    s.packet,
                    idx == Some(s.stream),
                    plain,
                    s.rest.is_empty(),
                )
            })
            .collect();
        let size = self.nodes.sizes();
        let (kind, ext) = (self.kind, self.ext);
        let member = |i: usize, want_other: bool| -> bool {
            let e = ext[i];
            for &(id, packet, stream_cls, plain, other_plain) in &splits {
                if id == e {
                    let stream = looks_like_stream(size[i], packet);
                    return if want_other {
                        !stream && other_plain
                    } else if stream {
                        stream_cls
                    } else {
                        plain
                    };
                }
            }
            let m = if want_other { &other_ext } else { &by_ext };
            m.get(e as usize).copied().unwrap_or(false)
        };
        let mut out = vec![false; self.n];
        out.par_iter_mut()
            .enumerate()
            .with_min_len(1 << 14)
            .for_each(|(i, o)| {
                if !is_leaf(kind[i]) {
                    return;
                }
                let mut hit = member(i, false);
                if let (Some(col), Some(ci)) = (folder, idx) {
                    let v = col.get(i).copied().unwrap_or(0);
                    if v > 0 && v < FALLBACK_BASE {
                        hit = (v - 1) as usize == ci;
                    } else if v >= FALLBACK_BASE && member(i, true) {
                        hit = (v - FALLBACK_BASE) as usize == ci;
                    }
                }
                *o = hit;
            });
        out
    }

    fn run(&self, nf: &FilterNormal) -> (Vec<bool>, Vec<bool>) {
        let n = self.n;
        let folder_col = match (
            &self.ctx.folder_classes,
            nf.classes.is_empty() && nf.not_classes.is_empty(),
        ) {
            (Some(fc), false) => fc.column(self.nodes, &self.ctx.classes),
            _ => None,
        };
        let folder_col = folder_col
            .as_deref()
            .map(Vec::as_slice)
            .filter(|c| c.iter().any(|&v| v != 0));
        // name / class: any
        let what = if !nf.masks.is_empty() || !nf.classes.is_empty() {
            let mut what = vec![false; n];
            for m in &nf.masks {
                self.name_mask_into(m, &mut what);
            }
            for k in &nf.classes {
                let c = self.classes(k, folder_col);
                or_into(&mut what, &c);
            }
            Some(what)
        } else {
            None
        };
        let mut not_what: Option<Vec<bool>> = None;
        if !nf.not_masks.is_empty() || !nf.not_classes.is_empty() {
            let mut v = vec![false; n];
            for m in &nf.not_masks {
                self.name_mask_into(m, &mut v);
            }
            for k in &nf.not_classes {
                let c = self.classes(k, folder_col);
                or_into(&mut v, &c);
            }
            not_what = Some(v);
        }
        let folder_hit = if nf.folders.is_empty() {
            vec![false; n]
        } else {
            self.folders(&nf.folders)
        };
        let in_folder = (!nf.folders.is_empty()).then(|| {
            let mut v = folder_hit.clone();
            down(&self.order, self.parent, &mut v);
            v
        });
        let not_folder = (!nf.not_folders.is_empty()).then(|| {
            let mut v = self.folders(&nf.not_folders);
            down(&self.order, self.parent, &mut v);
            v
        });
        let tags = if nf.tags.is_some() || !nf.not_tags.is_empty() {
            let mut bits = vec![0u8; n];
            if let Some(own) = &self.ctx.tags {
                let m = n.min(own.len());
                bits[..m].copy_from_slice(&own[..m]);
            }
            down(&self.order, self.parent, &mut bits);
            Some(bits)
        } else {
            None
        };
        let want_tags: Option<u8> = nf
            .tags
            .as_ref()
            .map(|t| t.iter().fold(0, |a, &c| a | tag_bit(c)));
        let drop_tags: u8 = nf.not_tags.iter().fold(0, |a, &c| a | tag_bit(c));
        let attrs = nf.attrs.as_ref().map(attr_masks);
        let not_attrs: Vec<(u32, u32)> = nf.not_attrs.iter().map(attr_masks).collect();
        struct R {
            disk: bool,
            lo: Option<i64>,
            hi: Option<i64>,
        }
        let sizes: Vec<R> = nf
            .sizes
            .iter()
            .map(|r| R {
                disk: r.field == "disk",
                lo: r.lo,
                hi: r.hi,
            })
            .collect();
        let ages: Vec<(u8, Option<i64>, Option<i64>)> = nf
            .ages
            .iter()
            .map(|r| {
                let col = match r.field.as_str() {
                    "create" => 1,
                    "access" => 2,
                    _ => 0,
                };
                (col, r.lo, r.hi)
            })
            .collect();
        let nodes = self.nodes;
        let (kind, flags) = (self.kind, nodes.flags_col());
        let (size, alloc) = (nodes.sizes(), nodes.allocs());
        let times = [nodes.mtimes(), nodes.ctimes(), nodes.atimes()];
        let now = self.ctx.now;
        let gone = Flags::GONE.bits();
        let mut ok = vec![false; n];
        ok.par_iter_mut()
            .enumerate()
            .with_min_len(1 << 14)
            .for_each(|(i, o)| {
                if !is_leaf(kind[i]) {
                    return;
                }
                let f = flags[i].load(std::sync::atomic::Ordering::Relaxed);
                if f & gone != 0 {
                    return;
                }
                if let Some(w) = &what
                    && !w[i]
                {
                    return;
                }
                if let Some(w) = &not_what
                    && w[i]
                {
                    return;
                }
                if let Some(v) = &in_folder
                    && !v[i]
                {
                    return;
                }
                if let Some(v) = &not_folder
                    && v[i]
                {
                    return;
                }
                if let Some(t) = &tags {
                    if let Some(want) = want_tags
                        && t[i] & want == 0
                    {
                        return;
                    }
                    if t[i] & drop_tags != 0 {
                        return;
                    }
                }
                for r in &sizes {
                    let v = if r.disk { alloc[i] } else { size[i] };
                    if r.lo.is_some_and(|lo| v < lo) || r.hi.is_some_and(|hi| v > hi) {
                        return;
                    }
                }
                for &(col, lo, hi) in &ages {
                    let t = times[col as usize][i] as i64;
                    if t == 0 {
                        return; // unknown timestamp: no age condition holds
                    }
                    let age = now - t;
                    if lo.is_some_and(|lo| age < lo) || hi.is_some_and(|hi| age > hi) {
                        return;
                    }
                }
                if let Some((req, forb)) = attrs
                    && (f & req != req || f & forb != 0)
                {
                    return;
                }
                for &(req, forb) in &not_attrs {
                    if f & req == req && f & forb == 0 {
                        return;
                    }
                }
                *o = true;
            });
        (ok, folder_hit)
    }
}

fn attr_masks(s: &AttrSet) -> (u32, u32) {
    let req = s.required.iter().fold(0, |a, &x| a | attr_flag(x).bits());
    let forb = s.forbidden.iter().fold(0, |a, &x| a | attr_flag(x).bits());
    (req, forb)
}

fn or_into(a: &mut [bool], b: &[bool]) {
    a.par_iter_mut()
        .zip(b.par_iter())
        .with_min_len(1 << 14)
        .for_each(|(x, &y)| *x |= y);
}

/// Evaluate a normal form over `nodes`; an empty one matches every file.
pub fn evaluate(nodes: &Nodes, nf: &FilterNormal, ctx: &FilterContext) -> FilterResult {
    let t0 = Instant::now();
    let ev = Eval::new(nodes, ctx);
    let (matched, folder_hit) = ev.run(nf);
    let n = ev.n;
    let mut count = vec![0u32; n];
    let mut alloc = vec![0i64; n];
    let mut size = vec![0i64; n];
    let flags = nodes.flags_col();
    let (a_col, s_col) = (nodes.allocs(), nodes.sizes());
    let dup = Flags::HARDLINK_DUP.bits();
    count
        .par_iter_mut()
        .zip(alloc.par_iter_mut())
        .zip(size.par_iter_mut())
        .enumerate()
        .with_min_len(1 << 14)
        .for_each(|(i, ((c, a), s))| {
            if matched[i] {
                *c = 1;
                if flags[i].load(std::sync::atomic::Ordering::Relaxed) & dup == 0 {
                    *a = a_col[i];
                    *s = s_col[i];
                }
            }
        });
    // add every node's values into its ancestors, children first
    let parent = ev.parent;
    ev.order.up(n, |i| {
        if count[i] == 0 {
            return;
        }
        let p = parent[i];
        if p >= 0 {
            let p = p as usize;
            count[p] += count[i];
            alloc[p] += alloc[i];
            size[p] += size[i];
        }
    });
    FilterResult {
        n,
        matched,
        count,
        alloc,
        size,
        folder_hit,
        elapsed_s: t0.elapsed().as_secs_f64(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_names() {
        assert_eq!(root_base_name(b"/data/Scan Root"), b"Scan Root");
        assert_eq!(root_base_name(b"/"), b"");
        #[cfg(windows)]
        {
            assert_eq!(root_base_name(b"C:\\"), b"");
            assert_eq!(root_base_name(b"D:\\Photos\\"), b"Photos");
        }
    }
}
