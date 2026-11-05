//! The exported tree: the elements below one export root, as SpaceSniffer exports them
//! (port of `ExportTree` / `Element` in export/template.py).

use std::borrow::Cow;
use std::cell::OnceCell;

use sk_store::{Flags, Kind, NodeId, Nodes};

use super::ExportError;

/// Order of a folder's elements (SpaceSniffer's block sorting, fine sorting and direction).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ordering {
    pub blocks: Blocks,
    pub sort: SortKey,
    pub descending: bool,
}

impl Default for Ordering {
    fn default() -> Self {
        Ordering {
            blocks: Blocks::Mixed,
            sort: SortKey::Size,
            descending: true,
        }
    }
}

/// Files first, folders first, or interleaved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocks {
    Files,
    Folders,
    Mixed,
}

/// By allocated size, by name (case-insensitive) or listing order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Size,
    Name,
    None,
}

/// A filter mask: one bool per node (the files that pass the filter). Shorter than the store
/// when it was computed before the store grew: newer rows do not match.
pub type Mask<'a> = Option<&'a [bool]>;

#[cfg(windows)]
pub(crate) fn is_sep(b: u8) -> bool {
    b == b'\\' || b == b'/'
}
#[cfg(not(windows))]
pub(crate) fn is_sep(b: u8) -> bool {
    b == b'/'
}

pub(crate) const SEP: u8 = std::path::MAIN_SEPARATOR as u8;

/// Raw name bytes (WTF-8 on Windows, raw bytes on POSIX) as text: valid UTF-8 is borrowed; a lone
/// surrogate or a byte that is not UTF-8 is written as a `\udcXX`-style escape (what Python's
/// `backslashreplace` wrote for the same names).
pub fn text_of(raw: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(raw) {
        Ok(s) => Cow::Borrowed(s),
        Err(_) => {
            let mut out = String::with_capacity(raw.len() + 8);
            decode_units(raw, |p| match p {
                Piece::Text(s) => out.push_str(s),
                Piece::Unit(u) => out.push_str(&format!("\\u{u:04x}")),
            });
            Cow::Owned(out)
        }
    }
}

/// A part of a decoded name: valid text, or one UTF-16 unit that is not Unicode (a WTF-8
/// surrogate, or `0xDC00 + byte` for a stray byte).
pub(crate) enum Piece<'a> {
    Text(&'a str),
    Unit(u16),
}

/// Walks name bytes in order, handing out [`Piece`]s.
pub(crate) fn decode_units<'a>(raw: &'a [u8], mut f: impl FnMut(Piece<'a>)) {
    let mut rest = raw;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                f(Piece::Text(s));
                return;
            }
            Err(e) => {
                let (valid, at) = rest.split_at(e.valid_up_to());
                if !valid.is_empty() {
                    f(Piece::Text(std::str::from_utf8(valid).unwrap_or_default()));
                }
                if at.len() >= 3
                    && at[0] == 0xED
                    && (0xA0..=0xBF).contains(&at[1])
                    && (0x80..=0xBF).contains(&at[2])
                {
                    f(Piece::Unit(0xD000 | ((at[1] as u16 & 0x3F) << 6) | (at[2] as u16 & 0x3F)));
                    rest = &at[3..];
                } else {
                    f(Piece::Unit(0xDC00 | at[0] as u16));
                    rest = &at[1..];
                }
            }
        }
    }
}

/// `os.path.dirname` of a path ("" for a root: its dirname is itself).
pub(crate) fn dirname(path: &[u8]) -> &[u8] {
    let (drive, rest) = split_drive(path);
    let Some(i) = rest.iter().rposition(|&b| is_sep(b)) else {
        return &path[..drive.len()];
    };
    let mut head = &rest[..=i];
    let stripped = head.iter().rposition(|&b| !is_sep(b)).map(|k| &head[..=k]);
    if let Some(s) = stripped {
        head = s;
    }
    &path[..drive.len() + head.len()]
}

#[cfg(windows)]
fn split_drive(path: &[u8]) -> (&[u8], &[u8]) {
    if path.len() >= 2 && path[1] == b':' && path[0].is_ascii_alphabetic() {
        return path.split_at(2);
    }
    if path.len() >= 2 && is_sep(path[0]) && is_sep(path[1]) {
        // \\server\share
        let after = &path[2..];
        if let Some(a) = after.iter().position(|&b| is_sep(b)) {
            let share = &after[a + 1..];
            let b = share.iter().position(|&b| is_sep(b)).unwrap_or(share.len());
            return path.split_at(2 + a + 1 + b);
        }
    }
    (&path[..0], path)
}
#[cfg(not(windows))]
fn split_drive(path: &[u8]) -> (&[u8], &[u8]) {
    (&path[..0], path)
}

/// `os.path.basename(path.rstrip(seps))`, or the path itself when that is empty.
pub(crate) fn root_basename(path: &[u8]) -> &[u8] {
    let end = path.iter().rposition(|&b| !is_sep(b)).map_or(0, |k| k + 1);
    let trimmed = &path[..end];
    let (drive, rest) = split_drive(trimmed);
    let rest_start = drive.len();
    let start = rest.iter().rposition(|&b| is_sep(b)).map_or(rest_start, |k| rest_start + k + 1);
    let base = &trimmed[start..];
    if base.is_empty() { path } else { base }
}

/// Subtree counts of the visible elements (the element itself excluded).
struct Counts {
    files: Vec<u32>,
    dirs: Vec<u32>,
}

/// The elements below one export root.
///
/// Everything below the root is collected level by level, so each folder's children are one
/// contiguous run of *positions*; per-position vectors hold the ids and child runs, and — when a
/// filter mask is given or counts are asked for — the subtree counts and sizes of the visible
/// elements, folded bottom-up.
///
/// With a mask only matching files are visible, and a folder is visible when its subtree holds a
/// visible file; folder sizes and counts are those of the visible elements. The root is always
/// visible.
///
/// Memory: 13 bytes per element (ids, child runs, folder flag), 8 more with counts and 17 more with
/// a mask; nothing per node of the rest of the store.
pub struct ExportTree<'n> {
    pub nodes: &'n Nodes,
    pub root: NodeId,
    pub ordering: Ordering,
    /// unix seconds: "now" for the age tags
    pub now: f64,
    pub(crate) ids: Vec<NodeId>,
    kstart: Vec<u32>,
    kcount: Vec<u32>,
    pub(crate) is_dir: Vec<bool>,
    pub(crate) vis: Option<Vec<bool>>,
    sub_size: Option<Vec<i64>>,
    sub_alloc: Option<Vec<i64>>,
    counts: OnceCell<Counts>,
    /// whether a mask was applied
    pub masked: bool,
    pub root_path: Vec<u8>,
    pub root_name: Vec<u8>,
    pub root_parent: Vec<u8>,
}

/// One exported element: what tags and the formats read. Its byte strings live in the walk's
/// path buffer (valid for the callback only).
#[derive(Clone, Copy, Debug)]
pub struct Element<'a> {
    pub pos: u32,
    /// nesting level: 0 for the export root
    pub level: u32,
    pub node: NodeId,
    pub is_dir: bool,
    pub path: &'a [u8],
    pub parent_path: &'a [u8],
    pub name: &'a [u8],
}

impl Element<'_> {
    /// (name without extension, extension without the dot); folders have no extension.
    pub fn stem_ext(&self) -> (&[u8], &[u8]) {
        let name = self.name;
        match name.iter().rposition(|&b| b == b'.') {
            Some(dot) if !self.is_dir && dot > 0 && dot != name.len() - 1 => {
                (&name[..dot], &name[dot + 1..])
            }
            _ => (name, &name[..0]),
        }
    }
}

impl<'n> ExportTree<'n> {
    /// The tree below `root` (an element of the store: `0 < root < len`).
    pub fn new(
        nodes: &'n Nodes,
        root: NodeId,
        mask: Mask<'_>,
        ordering: Ordering,
        counts: bool,
        now: Option<f64>,
    ) -> Result<ExportTree<'n>, ExportError> {
        let n = nodes.len();
        if !(root > 0 && (root as usize) < n) {
            return Err(ExportError::NotAnElement(root));
        }
        let now = now.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64())
        });
        let root_path = nodes.path_bytes(root);
        let root_name = if nodes.parent(root) == 0 {
            root_basename(&root_path).to_vec()
        } else {
            nodes.name_bytes(root).to_vec()
        };
        let parent = dirname(&root_path);
        let root_parent = if parent == root_path.as_slice() { Vec::new() } else { parent.to_vec() };
        let mut tree = ExportTree {
            nodes,
            root,
            ordering,
            now,
            ids: Vec::new(),
            kstart: Vec::new(),
            kcount: Vec::new(),
            is_dir: Vec::new(),
            vis: None,
            sub_size: None,
            sub_alloc: None,
            counts: OnceCell::new(),
            masked: mask.is_some(),
            root_path,
            root_name,
            root_parent,
        };
        tree.collect();
        if mask.is_some() || counts {
            tree.aggregate(mask);
        }
        Ok(tree)
    }

    /// Number of elements (visible or not).
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Positions level by level: `ids`, each folder's child run `kstart`/`kcount`.
    fn collect(&mut self) {
        let nodes = self.nodes;
        let n = nodes.len();
        let gone = Flags::GONE.bits();
        let flags = nodes.flags_col();
        let mut ids: Vec<NodeId> = vec![self.root];
        let mut kstart: Vec<u32> = Vec::new();
        let mut kcount: Vec<u32> = Vec::new();
        let (mut a, mut b) = (0usize, 1usize);
        let mut levels = 0usize;
        loop {
            for p in a..b {
                let node = ids[p];
                kstart.push(ids.len() as u32);
                let mut count = 0u32;
                if nodes.is_dir(node) {
                    for c in nodes.children(node) {
                        if flags[c as usize].load(std::sync::atomic::Ordering::Relaxed) & gone == 0 {
                            ids.push(c);
                            count += 1;
                        }
                    }
                }
                kcount.push(count);
            }
            if ids.len() == b {
                break;
            }
            levels += 1;
            if ids.len() > n || levels > 65536 {
                // a torn read of a store being changed: never loop, this level reads as unlisted
                for c in &mut kcount[a..b] {
                    *c = 0;
                }
                ids.truncate(b);
                break;
            }
            a = b;
            b = ids.len();
        }
        self.is_dir = ids.iter().map(|&i| nodes.is_dir(i)).collect();
        self.ids = ids;
        self.kstart = kstart;
        self.kcount = kcount;
    }

    /// Subtree counts (and, under a mask, sizes and visibility) of the visible elements.
    /// Children always sit at higher positions than their folder: one backward pass folds them.
    fn aggregate(&mut self, mask: Mask<'_>) {
        let nodes = self.nodes;
        let m = self.ids.len();
        let mut files = vec![0u32; m];
        let mut dirs = vec![0u32; m];
        let dup = Flags::HARDLINK_DUP;
        let own_leaf: Vec<bool> = match mask {
            Some(mask) => (0..m)
                .map(|p| {
                    let id = self.ids[p] as usize;
                    !self.is_dir[p] && id < mask.len() && mask[id]
                })
                .collect(),
            None => self.is_dir.iter().map(|&d| !d).collect(),
        };
        let (mut sub_size, mut sub_alloc) = if mask.is_some() {
            (vec![0i64; m], vec![0i64; m])
        } else {
            (Vec::new(), Vec::new())
        };
        for p in (0..m).rev() {
            let (ks, kc) = (self.kstart[p] as usize, self.kcount[p] as usize);
            if kc == 0 {
                continue;
            }
            let (mut f, mut d, mut s, mut al) = (0u32, 0u32, 0i64, 0i64);
            for c in ks..ks + kc {
                f += own_leaf[c] as u32 + files[c];
                let vis_dir = self.is_dir[c] && (mask.is_none() || files[c] > 0);
                d += vis_dir as u32 + dirs[c];
                if mask.is_some() {
                    let id = self.ids[c];
                    if own_leaf[c] && !nodes.flags(id).intersects(dup) {
                        s += nodes.size(id);
                        al += nodes.alloc(id);
                    }
                    s += sub_size[c];
                    al += sub_alloc[c];
                }
            }
            files[p] = f;
            dirs[p] = d;
            if mask.is_some() {
                sub_size[p] = s;
                sub_alloc[p] = al;
            }
        }
        if mask.is_some() {
            let mut vis: Vec<bool> = (0..m).map(|p| own_leaf[p] || (self.is_dir[p] && files[p] > 0)).collect();
            vis[0] = true;
            self.vis = Some(vis);
            self.sub_size = Some(sub_size);
            self.sub_alloc = Some(sub_alloc);
        }
        let _ = self.counts.set(Counts { files, dirs });
    }

    fn counts_cell(&self) -> &Counts {
        self.counts.get_or_init(|| {
            // unmasked counts, asked for after construction
            let m = self.ids.len();
            let mut files = vec![0u32; m];
            let mut dirs = vec![0u32; m];
            for p in (0..m).rev() {
                let (ks, kc) = (self.kstart[p] as usize, self.kcount[p] as usize);
                let (mut f, mut d) = (0, 0);
                for c in ks..ks + kc {
                    f += !self.is_dir[c] as u32 + files[c];
                    d += self.is_dir[c] as u32 + dirs[c];
                }
                files[p] = f;
                dirs[p] = d;
            }
            Counts { files, dirs }
        })
    }

    /// (logical, allocated) bytes: subtree totals of a folder (visible files only under a mask).
    pub fn sizes(&self, pos: u32) -> (i64, i64) {
        let p = pos as usize;
        let node = self.ids[p];
        if self.is_dir[p] {
            if let (Some(s), Some(a)) = (&self.sub_size, &self.sub_alloc) {
                return (s[p], a[p]);
            }
            return (self.nodes.tot_size(node), self.nodes.tot_alloc(node));
        }
        if self.nodes.flags(node).intersects(Flags::HARDLINK_DUP) {
            return (0, 0); // another link of a file counted elsewhere
        }
        (self.nodes.size(node), self.nodes.alloc(node))
    }

    /// Allocated bytes of a position, as sorting and the report rank elements.
    pub fn alloc_key(&self, pos: u32) -> i64 {
        self.sizes(pos).1
    }

    /// (files, folders) in the subtree, the element itself excluded.
    pub fn counts(&self, pos: u32) -> (u32, u32) {
        let c = self.counts_cell();
        (c.files[pos as usize], c.dirs[pos as usize])
    }

    /// (files, folders) directly inside a folder.
    pub fn local_counts(&self, pos: u32) -> (u32, u32) {
        let p = pos as usize;
        let (ks, kc) = (self.kstart[p] as usize, self.kcount[p] as usize);
        let (mut f, mut d) = (0, 0);
        for c in ks..ks + kc {
            if self.vis.as_ref().is_some_and(|v| !v[c]) {
                continue;
            }
            if self.is_dir[c] {
                d += 1;
            } else {
                f += 1;
            }
        }
        (f, d)
    }

    pub fn kind(&self, pos: u32) -> Kind {
        self.nodes.kind(self.ids[pos as usize])
    }

    pub fn flags(&self, pos: u32) -> Flags {
        self.nodes.flags(self.ids[pos as usize])
    }

    /// Unix seconds of modification (0), creation (1) or access (2); 0 = unknown.
    pub fn time(&self, pos: u32, which: usize) -> i64 {
        let id = self.ids[pos as usize];
        (match which {
            0 => self.nodes.mtime(id),
            1 => self.nodes.ctime(id),
            _ => self.nodes.atime(id),
        }) as i64
    }

    /// Whether a position is visible (always without a mask).
    pub fn visible(&self, pos: u32) -> bool {
        self.vis.as_ref().is_none_or(|v| v[pos as usize])
    }

    /// Whether a folder has (unfiltered) children at all.
    pub fn has_children(&self, pos: u32) -> bool {
        self.kcount[pos as usize] > 0
    }

    /// Positions of a folder's visible children, in export order.
    pub fn children(&self, pos: u32) -> Vec<u32> {
        let p = pos as usize;
        let (ks, kc) = (self.kstart[p], self.kcount[p]);
        let mut r: Vec<u32> = (ks..ks + kc).filter(|&c| self.visible(c)).collect();
        if r.len() < 2 {
            return r;
        }
        let o = self.ordering;
        let block = |c: u32| -> bool {
            let d = self.is_dir[c as usize];
            match o.blocks {
                Blocks::Mixed => false,
                Blocks::Files => d,
                Blocks::Folders => !d,
            }
        };
        match o.sort {
            SortKey::Name => {
                let mut keyed: Vec<(bool, String, u32)> = r
                    .iter()
                    .map(|&c| {
                        let name = self.nodes.name(self.ids[c as usize]);
                        (block(c), sk_protocol::filter_syntax::casefold(&name), c)
                    })
                    .collect();
                if o.descending {
                    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
                } else {
                    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
                }
                r = keyed.into_iter().map(|k| k.2).collect();
            }
            SortKey::Size => {
                if o.descending {
                    r.sort_by_key(|&c| (block(c), std::cmp::Reverse(self.alloc_key(c))));
                } else {
                    r.sort_by_key(|&c| (block(c), self.alloc_key(c)));
                }
            }
            SortKey::None => r.sort_by_key(|&c| block(c)),
        }
        r
    }

    pub fn root_element(&self) -> Element<'_> {
        Element {
            pos: 0,
            level: 0,
            node: self.root,
            is_dir: self.is_dir[0],
            path: &self.root_path,
            parent_path: &self.root_parent,
            name: &self.root_name,
        }
    }

    /// Every visible element, depth first (the root first), each folder's children in order.
    /// Stops at the first error `f` returns.
    pub fn walk<E>(&self, mut f: impl FnMut(&Element<'_>) -> Result<(), E>) -> Result<(), E> {
        let root = self.root_element();
        f(&root)?;
        if !root.is_dir {
            return Ok(());
        }
        let mut buf: Vec<u8> = self.root_path.clone();
        let mut ends: Vec<usize> = vec![buf.len()]; // ends[k]: path length of the open folder at level k
        let mut frames: Vec<(Vec<u32>, usize)> = vec![(self.children(0), 0)];
        while let Some(frame) = frames.last_mut() {
            if frame.1 >= frame.0.len() {
                frames.pop();
                continue;
            }
            let pos = frame.0[frame.1];
            frame.1 += 1;
            let level = frames.len();
            let node = self.ids[pos as usize];
            let is_dir = self.is_dir[pos as usize];
            let parent_end = ends[level - 1];
            buf.truncate(parent_end);
            if !buf.last().is_some_and(|&b| is_sep(b)) {
                buf.push(SEP);
            }
            let name_start = buf.len();
            buf.extend_from_slice(self.nodes.name_bytes(node));
            let el = Element {
                pos,
                level: level as u32,
                node,
                is_dir,
                path: &buf,
                parent_path: &buf[..parent_end],
                name: &buf[name_start..],
            };
            f(&el)?;
            if is_dir {
                let kids = self.children(pos);
                if !kids.is_empty() {
                    ends.truncate(level);
                    ends.push(buf.len());
                    frames.push((kids, 0));
                }
            }
        }
        Ok(())
    }
}

/// The roots an export covers: node 0 (the virtual top) stands for every scan root.
pub fn export_roots(nodes: &Nodes, root: NodeId) -> Vec<NodeId> {
    if root == 0 { nodes.roots().collect() } else { vec![root] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_helpers() {
        assert_eq!(dirname(b"/data"), b"/");
        assert_eq!(dirname(b"/"), b"/");
        assert_eq!(dirname(b"/a/b"), b"/a");
        assert_eq!(root_basename(b"/data/"), b"data");
        assert_eq!(root_basename(b"/"), b"/");
        #[cfg(windows)]
        {
            assert_eq!(dirname(b"C:\\"), b"C:\\");
            assert_eq!(dirname(b"C:\\Users"), b"C:\\");
            assert_eq!(dirname(b"C:\\Users\\x"), b"C:\\Users");
            assert_eq!(root_basename(b"C:\\"), b"C:\\");
            assert_eq!(root_basename(b"D:\\Photos"), b"Photos");
        }
    }

    #[test]
    fn non_unicode_names_escape() {
        assert_eq!(text_of(b"bad\xffx"), "bad\\udcffx");
        assert_eq!(text_of(b"a\xed\xa0\x80b"), "a\\ud800b");
        assert_eq!(text_of("ok é".as_bytes()), "ok é");
    }
}
