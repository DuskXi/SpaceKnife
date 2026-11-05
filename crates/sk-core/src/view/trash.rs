//! Which blocks are in a recycle bin / trash (row flag `F_TRASH`: the views draw them apart, and
//! the bin's own block says how much emptying it frees). Python `view/trash.py`.
//!
//! The bins, by path (any platform's, so a snapshot from another system is read the same):
//!
//! - Windows: `<drive>:\$Recycle.Bin` (and `RECYCLER` / `RECYCLED`), at the top of a drive or share;
//! - macOS: `/Users/<name>/.Trash` and `.Trashes` at the top of a volume;
//! - Linux (freedesktop.org): `…/.local/share/Trash` of a home folder (or `$XDG_DATA_HOME/Trash`),
//!   `.Trash-<uid>` at the top of a mount, and the shared `.Trash` at the top of a scanned mount.
//!
//! Only the rows of a frame and their ancestors are looked at by [`TrashCache::marks`]: a folder
//! whose name has the length of a bin's name is a candidate, and only candidates are named and
//! matched. [`TrashCache::find_bins`] searches a whole store (the 「回收站」 chip).

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rayon::prelude::*;
use regex::Regex;
use sk_store::{Kind, NodeId, Nodes};

use crate::layout::api::SizeSource;

/// Byte lengths of the bins' folder names: Trash, .Trash, .Trashes / RECYCLER / RECYCLED,
/// $Recycle.Bin, .Trash-<uid> (up to 10 digits).
fn length_ok(len: u16) -> bool {
    matches!(len, 5 | 6 | 8 | 12) || (8..=17).contains(&len)
}

const NAMES: &[&str] = &[
    "trash",
    ".trash",
    ".trashes",
    "$recycle.bin",
    "recycler",
    "recycled",
    ".trash-",
];

static UID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\.Trash-\d{1,10}$").unwrap());
static WIN_BIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:[a-z]:|[\\/]{2}[^\\/]+[\\/][^\\/]+)[\\/]+(?:\$recycle\.bin|recycler|recycled)$",
    )
    .unwrap()
});
static MAC_HOME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/Users/[^/]+/\.Trash$").unwrap());
static MAC_VOLUME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:/Volumes/[^/]+)?/\.Trashes$").unwrap());
static XDG_HOME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^.*/\.local/share/Trash$").unwrap());
static UID_TRASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^.*/\.Trash-\d{1,10}$").unwrap());

/// This user's home trash when `XDG_DATA_HOME` moves it (Linux).
fn xdg_trash() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let data = std::env::var("XDG_DATA_HOME").ok()?;
    let data = data.trim();
    if data.is_empty() {
        return None;
    }
    Some(
        std::path::Path::new(data)
            .join("Trash")
            .to_string_lossy()
            .trim_end_matches('/')
            .to_owned(),
    )
}

/// Whether `path` is a recycle bin / trash folder. `top`: its parent is a scan root (the shared
/// `.Trash` of a mount and a volume's `.Trashes` count there too).
pub fn is_trash_path(path: &str, top: bool) -> bool {
    let p = if path.chars().count() > 3 {
        path.trim_end_matches(['\\', '/'])
    } else {
        path
    };
    if WIN_BIN.is_match(p) {
        return true;
    }
    let q = p.replace('\\', "/");
    if [&*MAC_HOME, &*MAC_VOLUME, &*XDG_HOME, &*UID_TRASH]
        .iter()
        .any(|r| r.is_match(&q))
    {
        return true;
    }
    let name = q.rsplit('/').next().unwrap_or("");
    if top && (name == ".Trash" || name == ".Trashes") {
        return true;
    }
    xdg_trash().is_some_and(|x| p.trim_end_matches('/') == x)
}

fn candidate(name: &str) -> bool {
    let low = sk_protocol::filter_syntax::casefold(name);
    NAMES.contains(&low.as_str()) || UID.is_match(name)
}

/// Whether node `id` (a folder) is a bin.
fn is_bin(nodes: &Nodes, id: NodeId) -> bool {
    let up = nodes.parent(id);
    if up == 0 {
        // a scan root that is itself a bin (the trash folder was scanned on its own)
        return is_trash_path(&nodes.name(id), false);
    }
    if !length_ok(nodes.name_len(id)) || !candidate(&nodes.name(id)) {
        return false;
    }
    let top = up > 0 && nodes.parent(up as NodeId) == 0;
    is_trash_path(&nodes.path_lossy(id), top)
}

/// The deepest a bin sits below its scan root (depth 1): /home/<user>/.local/share/Trash.
pub const BIN_DEPTH: u16 = 7;
/// A store that keeps changing is searched whole at most this often.
pub const FULL_SEARCH: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Known {
    version: u64,
    checked: HashSet<NodeId>,
    bins: HashSet<NodeId>,
}

struct Found {
    version: u64,
    at: Instant,
    ids: Vec<NodeId>,
    paths: Vec<Vec<u8>>,
}

/// What the nodes of one store were found to be (one per store, shared by its views).
#[derive(Default)]
pub struct TrashCache {
    known: Mutex<Known>,
    found: Mutex<Option<Found>>,
}

impl TrashCache {
    pub fn new() -> TrashCache {
        TrashCache::default()
    }

    /// Per node of `holders` (ids; negative: none): whether it is in a bin (the bin itself
    /// included), and whether it is a bin.
    pub fn marks(&self, nodes: &Nodes, holders: &[i64]) -> (Vec<bool>, Vec<bool>) {
        let n = nodes.len() as i64;
        let mut inside = vec![false; holders.len()];
        let mut bin = vec![false; holders.len()];
        let mut known = self.known.lock();
        let version = nodes.version();
        if known.version != version {
            *known = Known {
                version,
                ..Known::default()
            };
        }
        let known = &mut *known;
        let mut check = |id: NodeId| -> bool {
            if known.checked.insert(id) && nodes.kind(id) == Kind::Dir && is_bin(nodes, id) {
                known.bins.insert(id);
            }
            known.bins.contains(&id)
        };
        for (i, &h) in holders.iter().enumerate() {
            if h <= 0 || h >= n {
                continue;
            }
            let mut cur = h as NodeId;
            let mut first = true;
            let mut steps = 0usize;
            loop {
                if check(cur) {
                    inside[i] = true;
                    if first {
                        bin[i] = true;
                    }
                }
                first = false;
                let p = nodes.parent(cur);
                steps += 1;
                if p <= 0 || steps > nodes.len() {
                    break;
                }
                cur = p as NodeId;
            }
        }
        (inside, bin)
    }

    /// Per node of `holders`: it is a bin or inside one.
    pub fn in_trash(&self, nodes: &Nodes, holders: &[i64]) -> Vec<bool> {
        self.marks(nodes, holders).0
    }

    /// The ids of every recycle bin / trash folder of the store, sorted. A store that changes all
    /// the time (scans, monitoring) is searched whole at most every [`FULL_SEARCH`]; in between,
    /// the bins found last are looked up again by path.
    pub fn find_bins(&self, nodes: &Nodes) -> Vec<NodeId> {
        let now = Instant::now();
        let version = nodes.version();
        let mut found = self.found.lock();
        if let Some(f) = &*found {
            if f.version == version {
                return f.ids.clone();
            }
            if now.duration_since(f.at) < FULL_SEARCH {
                let mut ids: Vec<NodeId> = f.paths.iter().filter_map(|p| nodes.find(p)).collect();
                ids.sort_unstable();
                ids.dedup();
                let paths = ids.iter().map(|&i| nodes.path_bytes(i)).collect();
                let at = f.at;
                *found = Some(Found {
                    version,
                    at,
                    ids: ids.clone(),
                    paths,
                });
                return ids;
            }
        }
        let ids = search(nodes);
        let paths = ids.iter().map(|&i| nodes.path_bytes(i)).collect();
        *found = Some(Found {
            version,
            at: now,
            ids: ids.clone(),
            paths,
        });
        ids
    }
}

/// Every bin in the store: folders near the top of a root whose name and path say so.
fn search(nodes: &Nodes) -> Vec<NodeId> {
    let n = nodes.len();
    if n <= 1 {
        return Vec::new();
    }
    let (kind, depth, lens) = (nodes.kinds_raw(), nodes.depths(), nodes.name_lens());
    let mut bins: Vec<NodeId> = (1..n)
        .into_par_iter()
        .with_min_len(1 << 15)
        .filter(|&i| {
            kind[i] == Kind::Dir as u8 && (1..=BIN_DEPTH).contains(&depth[i]) && length_ok(lens[i])
        })
        .filter(|&i| {
            let id = i as NodeId;
            let first = nodes.name_bytes(id).first().copied().unwrap_or(0);
            b"$.TtRr".contains(&first) && candidate(&nodes.name(id)) && {
                let up = nodes.parent(id);
                is_trash_path(
                    &nodes.path_lossy(id),
                    up > 0 && nodes.parent(up as NodeId) == 0,
                )
            }
        })
        .map(|i| i as NodeId)
        .collect();
    for root in nodes.roots() {
        if is_trash_path(&nodes.name(root), false) {
            bins.push(root);
        }
    }
    bins.sort_unstable();
    bins.dedup();
    bins
}

/// `tops` and everything below them, sorted.
pub fn subtree(nodes: &Nodes, tops: &[NodeId]) -> Vec<NodeId> {
    let mut out: Vec<NodeId> = tops.to_vec();
    let mut todo: Vec<NodeId> = tops.to_vec();
    while let Some(id) = todo.pop() {
        if nodes.is_dir(id) {
            for c in nodes.children(id) {
                out.push(c);
                todo.push(c);
            }
        }
        if out.len() > nodes.len() * 2 {
            break; // inconsistent ranges: never run away
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// A size source with the recycle bins taken out (the 「回收站」 chip off): a bin and everything
/// in it weigh nothing, and each of its ancestors loses the bin's bytes.
pub struct WithoutBins<'a> {
    base: &'a dyn SizeSource,
    inside: Vec<NodeId>,
    cut: HashMap<NodeId, i64>,
}

impl<'a> WithoutBins<'a> {
    pub fn new(base: &'a dyn SizeSource, nodes: &Nodes, bins: &[NodeId]) -> WithoutBins<'a> {
        let n = base.n();
        let bins: Vec<NodeId> = bins.iter().copied().filter(|&b| (b as usize) < n).collect();
        let inside: Vec<NodeId> = subtree(nodes, &bins)
            .into_iter()
            .filter(|&i| (i as usize) < n)
            .collect();
        let mut cut: HashMap<NodeId, i64> = HashMap::new();
        for &b in &bins {
            let size = base.size(nodes, b);
            for a in nodes.ancestors(b) {
                *cut.entry(a).or_default() += size;
            }
        }
        WithoutBins { base, inside, cut }
    }
}

impl SizeSource for WithoutBins<'_> {
    fn n(&self) -> usize {
        self.base.n()
    }

    fn size(&self, nodes: &Nodes, id: NodeId) -> i64 {
        if self.inside.binary_search(&id).is_ok() {
            return 0;
        }
        let s = self.base.size(nodes, id) - self.cut.get(&id).copied().unwrap_or(0);
        s.max(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert!(is_trash_path("C:\\$Recycle.Bin", false));
        assert!(is_trash_path("c:/$RECYCLE.BIN/", false));
        assert!(is_trash_path("\\\\server\\share\\$Recycle.Bin", false));
        assert!(is_trash_path("D:\\RECYCLER", false));
        assert!(!is_trash_path("C:\\Users\\$Recycle.Bin", false));
        assert!(is_trash_path("/Users/ann/.Trash", false));
        assert!(is_trash_path("/.Trashes", false));
        assert!(is_trash_path("/Volumes/USB/.Trashes", false));
        assert!(is_trash_path("/home/ann/.local/share/Trash", false));
        assert!(is_trash_path("/mnt/data/.Trash-1000", false));
        assert!(!is_trash_path("/mnt/data/.Trash", false));
        assert!(is_trash_path("/mnt/data/.Trash", true));
        assert!(!is_trash_path("/home/ann/Trash", false));
    }
}
