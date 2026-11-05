//! NTFS `$MFT` fast scan: an [`MftTable`] (sk-native's parser, read here or by the elevated
//! helper) → a whole [`Store`] in one go (port of `scan/mft.py`'s `table_to_store`).
//!
//! Record 5 becomes the root named `root_path`; metafiles (records 0–15 and the `$Extend` tree)
//! go under `$NTFS 元数据`; names whose parent is missing, not a folder or part of a parent
//! cycle go under `$孤立文件` (each pseudo folder only when non-empty). Sizes follow the hard
//! scan: folders and links occupy nothing, the second and later names of a file are
//! `HARDLINK_DUP`; alternate data streams count only with `include_ads`, except in the metafile
//! folder, where they always count toward allocation (`$UsnJrnl:$J`, `$Secure:$SDS` …).
//! `$BadClus:$Bad` never counts (it is as long as the volume).
//!
//! Memory: the table's columns are dropped as soon as they are gathered into node order; the
//! columns are written straight into a [`StoreBuilder`] (in parallel), no intermediate copy.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use rayon::prelude::*;
use sk_native::hardscan::lower_ext;
pub use sk_native::mft::{MftTable, VolumeInfo as MftVolumeInfo};
use sk_store::{Flags, Kind, Store, StoreBuilder, StoreError, StoreOptions};

/// Name of the metafile folder (message id).
pub const META_FOLDER: &str = "$NTFS 元数据";
/// Name of the folder of unreachable entries (message id).
pub const ORPHAN_FOLDER: &str = "$孤立文件";
pub const ROOT_RECORD: u64 = 5;
/// `$BadClus`: its `$Bad` stream spans the whole volume.
pub const BADCLUS_RECORD: u64 = 8;
/// Records 0–15 are metafiles / reserved.
pub const FIRST_USER_RECORD: u64 = 16;

/// How [`table_to_store`] builds.
#[derive(Clone, Debug, Default)]
pub struct MftBuild {
    /// Count alternate data streams.
    pub include_ads: bool,
    /// Every name of a hard-linked file counts.
    pub count_links_each: bool,
    /// Names of the pseudo folders (None: [`META_FOLDER`], [`ORPHAN_FOLDER`] in the interface
    /// language).
    pub folder_names: Option<(String, String)>,
}

/// Reads and parses the `$MFT` of a local NTFS volume (`"C:"`; administrator rights, Windows).
/// `duty` in (0, 1] limits the disk load; `progress(done, total)` follows the reading.
pub fn scan_volume(
    volume: &str,
    duty: f64,
    progress: Option<&mut dyn FnMut(u64, u64)>,
    cancel: Option<Arc<AtomicBool>>,
) -> io::Result<(MftTable, MftVolumeInfo)> {
    sk_native::mft::scan_mft(
        volume,
        sk_native::mft::ScanOptions { duty, progress, cancel, ..Default::default() },
    )
}

/// [`scan_volume`] on an image file holding an NTFS volume (tests).
pub fn scan_volume_image(
    path: &std::path::Path,
    window: usize,
) -> io::Result<(MftTable, MftVolumeInfo)> {
    sk_native::mft::scan_mft_image(path, sk_native::mft::ScanOptions { window, ..Default::default() })
}

/// The extension of a name by the scanner's rule: after the last dot, the dot not first, 1–16
/// characters (at most 64 bytes).
fn ext_of(name: &[u8]) -> Option<&[u8]> {
    let dot = name.iter().rposition(|&b| b == b'.')?;
    if dot == 0 {
        return None;
    }
    let ext = &name[dot + 1..];
    if ext.is_empty() || ext.len() > 64 {
        return None;
    }
    let chars = ext.iter().filter(|&&b| b & 0xC0 != 0x80).count();
    (chars <= 16).then_some(ext)
}

const NONE: i32 = -1;

/// Builds the node store of a whole volume from `$MFT` columns (see the module docs). The table
/// is consumed: its columns go as soon as they are in node order.
pub fn table_to_store(table: MftTable, root_path: &str, opts: &MftBuild) -> Result<Store, StoreError> {
    let MftTable {
        record: rec,
        parent: par,
        name_off,
        name_len,
        kind,
        flags,
        size,
        alloc,
        ads_size,
        ads_alloc,
        mtime,
        ctime,
        atime,
        nlink,
        names,
        ..
    } = table;
    let n = rec.len();
    // the second and later names of a record (rows of one record are adjacent when sorted)
    let mut dup = vec![false; n];
    if rec.windows(2).all(|w| w[0] <= w[1]) {
        for i in 1..n {
            dup[i] = rec[i] == rec[i - 1];
        }
    } else {
        let mut seen = std::collections::HashSet::with_capacity(n);
        for i in 0..n {
            dup[i] = !seen.insert(rec[i]);
        }
    }
    let (meta_row, orphan_row, root_row) = (n, n + 1, n + 2);
    let total = n + 3;
    let nrec = rec.iter().max().map_or(0, |&m| m as usize + 1).max(ROOT_RECORD as usize + 1);
    let container: Vec<bool> = (0..total)
        .map(|i| i >= n || (kind[i] == Kind::Dir as u8 && !dup[i]))
        .collect();
    let mut dir_row = vec![NONE; nrec];
    for i in 0..n {
        if container[i] {
            dir_row[rec[i] as usize] = i as i32;
        }
    }
    dir_row[ROOT_RECORD as usize] = root_row as i32;

    let mut pdir = vec![NONE; total];
    for i in 0..n {
        let p = par[i];
        let mut d = if (p as usize) < nrec { dir_row[p as usize] } else { NONE };
        if d < 0 || (container[i] && d == i as i32) {
            d = orphan_row as i32; // missing parent, or a folder that is its own parent
        }
        if rec[i] < FIRST_USER_RECORD && rec[i] != ROOT_RECORD {
            d = meta_row as i32;
        }
        if rec[i] == ROOT_RECORD {
            d = -2; // the root's own "." entry is the root node itself
        }
        pdir[i] = d;
    }
    drop(dir_row);
    drop(par);
    for row in [meta_row, orphan_row] {
        if pdir[..n].contains(&(row as i32)) {
            pdir[row] = root_row as i32;
        }
    }

    let (mut levels, mut child_start, mut counts) = layout(&pdir, &container, root_row);
    let placed: usize = levels.iter().map(Vec::len).sum();
    let children: usize = pdir.iter().filter(|&&d| d >= 0).count();
    if placed < 1 + children {
        // parent cycles: break each one under the orphans
        let mut seen = vec![false; total];
        for lv in &levels {
            for &r in lv {
                seen[r as usize] = true;
            }
        }
        let breakers = cycle_breakers(&pdir, &seen);
        tracing::warn!("MFT: {} parent cycles broken (corrupt directory entries)", breakers.len());
        for b in breakers {
            pdir[b] = orphan_row as i32;
        }
        pdir[orphan_row] = root_row as i32;
        (levels, child_start, counts) = layout(&pdir, &container, root_row);
    }
    let orphans = if pdir[orphan_row] >= 0 { counts[orphan_row] } else { 0 };

    let rows: Vec<u32> = levels.iter().flatten().copied().collect();
    let count = rows.len() + 1;
    let mut node_of_row = vec![0u32; total];
    for (k, &r) in rows.iter().enumerate() {
        node_of_row[r as usize] = k as u32 + 1;
    }
    let mut depth_of = vec![0u16; count];
    {
        let mut k = 1;
        for (d, lv) in levels.iter().enumerate() {
            for _ in lv {
                depth_of[k] = (d + 1).min(u16::MAX as usize) as u16;
                k += 1;
            }
        }
    }
    // metafiles' alternate streams always count (only rows inside the meta folder)
    let mut in_meta = vec![false; total];
    if !opts.include_ads && pdir[meta_row] >= 0 {
        in_meta[meta_row] = true;
        for lv in levels.iter().skip(1) {
            for &r in lv {
                let p = pdir[r as usize];
                if p >= 0 && in_meta[p as usize] {
                    in_meta[r as usize] = true;
                }
            }
        }
    }
    drop(levels);

    // extensions of files: distinct per chunk in parallel, then one table
    let name_of = |i: usize| -> &[u8] {
        let o = name_off[i] as usize;
        &names[o..o + name_len[i] as usize]
    };
    let chunk = 1 << 16;
    let parts: Vec<(Vec<Vec<u8>>, Vec<u16>)> = (0..n)
        .into_par_iter()
        .step_by(chunk)
        .map(|lo| {
            let hi = (lo + chunk).min(n);
            let mut local: HashMap<&[u8], u16> = HashMap::new();
            let mut table: Vec<Vec<u8>> = Vec::new();
            let mut ids = vec![0u16; hi - lo];
            for i in lo..hi {
                if kind[i] != Kind::File as u8 {
                    continue;
                }
                if let Some(e) = ext_of(name_of(i)) {
                    let next = table.len() as u16 + 1;
                    let id = *local.entry(e).or_insert_with(|| {
                        table.push(lower_ext(e));
                        next
                    });
                    ids[i - lo] = id;
                }
            }
            (table, ids)
        })
        .collect();
    let mut ext_table: Vec<String> = vec![String::new()];
    let mut ext_index: HashMap<Vec<u8>, u16> = HashMap::new();
    let mut ext_rows = vec![0u16; n];
    for (k, (table, ids)) in parts.into_iter().enumerate() {
        let remap: Vec<u16> = std::iter::once(0)
            .chain(table.into_iter().map(|e| {
                if let Some(&i) = ext_index.get(&e) {
                    return i;
                }
                if ext_table.len() >= 65535 {
                    return 0;
                }
                let i = ext_table.len() as u16;
                ext_table.push(String::from_utf8_lossy(&e).into_owned());
                ext_index.insert(e, i);
                i
            }))
            .collect();
        let lo = k * chunk;
        for (j, id) in ids.into_iter().enumerate() {
            ext_rows[lo + j] = remap[id as usize];
        }
    }

    // names: node 0 "", the table's names in node order, the root and the pseudo folders
    let (meta_name, orphan_name) = opts
        .folder_names
        .clone()
        .unwrap_or_else(|| {
            // in the interface language of the moment the tree is built
            (crate::i18n::t(META_FOLDER).to_owned(), crate::i18n::t(ORPHAN_FOLDER).to_owned())
        });
    let name_bytes = |row: usize| -> &[u8] {
        if row < n {
            name_of(row)
        } else if row == root_row {
            root_path.as_bytes()
        } else if row == meta_row {
            meta_name.as_bytes()
        } else {
            orphan_name.as_bytes()
        }
    };
    let image_len = 1 + rows.iter().map(|&r| name_bytes(r as usize).len() + 1).sum::<usize>();
    let mut b = StoreBuilder::new(count, image_len, StoreOptions::default());
    b.set_ext_table(ext_table);
    {
        let img = b.image();
        img[0] = 0;
        let mut at = 1;
        for &r in &rows {
            let nm = name_bytes(r as usize);
            img[at] = 0;
            img[at + 1..at + 1 + nm.len()].copy_from_slice(nm);
            at += nm.len() + 1;
        }
    }
    let root_first = (0..n).find(|&i| rec[i] == ROOT_RECORD);
    let clip = |t: i64| t.clamp(0, u32::MAX as i64) as u32;
    let virt = |r: usize| r >= n;
    let c = b.columns();
    // node 0
    c.parent[0] = -1;
    c.depth[0] = 0;
    c.kind[0] = Kind::Dir as u8;
    c.flags[0] = Flags::VIRTUAL.0;
    c.child_start[0] = 1;
    c.child_count[0] = 1;
    c.nlink[0] = 1;
    {
        let body = |k: usize| rows[k - 1] as usize;
        macro_rules! fill {
            ($col:expr, |$k:ident, $r:ident| $e:expr) => {
                $col[1..].par_iter_mut().enumerate().for_each(|(j, out)| {
                    let $k = j + 1;
                    let $r = body($k);
                    *out = $e;
                });
            };
        }
        fill!(c.parent, |k, r| {
            let p = pdir[r];
            if p < 0 { 0 } else { node_of_row[p as usize] as i32 }
        });
        fill!(c.depth, |k, r| { let _ = r; depth_of[k] });
        fill!(c.child_start, |k, r| { let _ = k; if container[r] && counts[r] > 0 { child_start[r] } else { 0 } });
        fill!(c.child_count, |k, r| { let _ = k; if container[r] { counts[r] as i32 } else { 0 } });
        fill!(c.kind, |k, r| { let _ = k; if virt(r) { Kind::Dir as u8 } else { kind[r] } });
        fill!(c.flags, |k, r| {
            let _ = k;
            if r == root_row {
                0
            } else if virt(r) {
                Flags::VIRTUAL.0
            } else if dup[r] && !opts.count_links_each {
                flags[r] | Flags::HARDLINK_DUP.0
            } else {
                flags[r]
            }
        });
        let ads_counts = |r: usize| rec[r] != BADCLUS_RECORD && (opts.include_ads || in_meta[r]);
        fill!(c.size, |k, r| {
            let _ = k;
            if virt(r) {
                0
            } else {
                let own = if kind[r] == Kind::Dir as u8 { 0 } else { size[r] };
                own + if opts.include_ads && rec[r] != BADCLUS_RECORD { ads_size[r] } else { 0 }
            }
        });
        fill!(c.alloc, |k, r| {
            let _ = k;
            if virt(r) {
                0
            } else {
                let own = if kind[r] == Kind::File as u8 { alloc[r] } else { 0 };
                own + if ads_counts(r) { ads_alloc[r] } else { 0 }
            }
        });
        let times = |col: &mut [u32], src_col: &[i64]| {
            col[1..].par_iter_mut().enumerate().for_each(|(j, out)| {
                let r = body(j + 1);
                *out = if virt(r) { 0 } else { clip(src_col[r]) };
            });
            if let Some(i) = root_first {
                col[1] = clip(src_col[i]);
            }
        };
        times(c.mtime, &mtime);
        times(c.ctime, &ctime);
        times(c.atime, &atime);
        fill!(c.ino, |k, r| { let _ = k; if virt(r) { 0 } else { rec[r] } });
        c.ino[1] = ROOT_RECORD;
        fill!(c.nlink, |k, r| { let _ = k; if virt(r) { 1 } else { nlink[r] } });
        fill!(c.name_len, |k, r| { let _ = k; name_bytes(r).len().min(u16::MAX as usize) as u16 });
        fill!(c.ext, |k, r| { let _ = k; if virt(r) { 0 } else { ext_rows[r] } });
    }
    let store = b.finish()?;
    store.appender().reaggregate();
    tracing::debug!("MFT: {} names -> {} nodes, {} orphans", n, store.len(), orphans);
    Ok(store)
}

/// Breadth-first layout from `root`: the rows level by level, each container's first child id
/// (ids count from 1: the root is node 1) and its child count.
fn layout(pdir: &[i32], container: &[bool], root: usize) -> (Vec<Vec<u32>>, Vec<i32>, Vec<u32>) {
    let total = pdir.len();
    // children grouped by parent, in row order (counting sort)
    let mut counts = vec![0u32; total];
    for &p in pdir {
        if p >= 0 {
            counts[p as usize] += 1;
        }
    }
    let mut starts = vec![0usize; total + 1];
    for i in 0..total {
        starts[i + 1] = starts[i] + counts[i] as usize;
    }
    let mut order = vec![0u32; starts[total]];
    let mut fill = starts.clone();
    for (i, &p) in pdir.iter().enumerate() {
        if p >= 0 {
            order[fill[p as usize]] = i as u32;
            fill[p as usize] += 1;
        }
    }
    let mut child_start = vec![0i32; total];
    let mut levels = vec![vec![root as u32]];
    let mut next_id: i64 = 2;
    let mut frontier = vec![root as u32];
    let mut visited = vec![false; total];
    visited[root] = true;
    while !frontier.is_empty() {
        let mut kids = Vec::new();
        for &f in &frontier {
            let f = f as usize;
            child_start[f] = next_id as i32;
            let c = &order[starts[f]..starts[f + 1]];
            next_id += c.len() as i64;
            kids.extend_from_slice(c);
        }
        if kids.is_empty() {
            break;
        }
        frontier = kids
            .iter()
            .copied()
            .filter(|&k| container[k as usize] && !std::mem::replace(&mut visited[k as usize], true))
            .collect();
        levels.push(kids);
    }
    (levels, child_start, counts)
}

/// One row per parent cycle among the rows not reached from the root (the smallest row of each).
fn cycle_breakers(pdir: &[i32], reached: &[bool]) -> Vec<usize> {
    let total = pdir.len();
    let mut mark = vec![0u32; total]; // 0 unvisited, else the walk that visited it
    let mut out = Vec::new();
    let mut walk = 0u32;
    for start in 0..total {
        if reached[start] || pdir[start] < 0 || mark[start] != 0 {
            continue;
        }
        walk += 1;
        let mut v = start;
        loop {
            if mark[v] == walk {
                // a new cycle: v is on it
                let mut best = v;
                let mut u = pdir[v] as usize;
                while u != v {
                    best = best.min(u);
                    u = pdir[u] as usize;
                }
                out.push(best);
                break;
            }
            if mark[v] != 0 || reached[v] || pdir[v] < 0 {
                break;
            }
            mark[v] = walk;
            v = pdir[v] as usize;
        }
    }
    out
}
