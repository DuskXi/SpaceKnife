//! The delete basket (docs/manage.md §2.2–2.3): what the user wants gone, held by the backend.
//!
//! Items are kept by **path**, not node id: a file change gives a relisted folder's entries new
//! ids, so [`Basket::refresh`] finds every item again by its path and recomputes it.
//!
//! Containment: an item covers its path and everything below it (a small-item group covers
//! exactly its members). Adding a path that an item already covers merges it into that item;
//! adding an ancestor of items absorbs them. Covered paths are kept sorted, so "what lies below
//! this folder" is one range of keys with the folder's prefix.
//!
//! Freeable space (§2.3) is allocation, minus cloud placeholders (nothing stored locally), minus
//! hard links whose other names are not all in the basket (deleting one name frees nothing); a
//! group of links that is complete counts once, for the first item holding one of its names.
//! Links are found by `(volume, ino)` with the scan's `ino` / `nlink` columns.
//!
//! Every method takes the basket's lock. Methods given a store view ([`Nodes`]) read it: the
//! caller holds a reader of that store; [`Basket::refresh`] takes readers itself (call it where
//! no reader of those stores is held on the same thread).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use sk_protocol::compare::ChangeKind;
use sk_protocol::ops::{BasketItemStatus, OpCode};
use sk_store::lookup::PathIndex;
use sk_store::names::encode_name;
use sk_store::subtree::subtree_ids;
use sk_store::{Flags, Kind, NodeId, Nodes, Store};

use super::names::unnameable;
use super::paths::{nt_dirname, nt_normcase, nt_normpath, posix_dirname, posix_normpath, protected_reason};
use super::text::{basket_text as text, fill};
use super::trash::{OpResult, win32_trims};

pub type ItemKind = ChangeKind;
pub type ItemStatus = BasketItemStatus;

/// One item of the basket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BasketItem {
    /// stable: a hash of the normalised path (groups: of their folder)
    pub id: String,
    /// absolute path (a small-item group: its folder; members non-empty), as shown: names that
    /// are not valid Unicode have U+FFFD in them ([`BasketItem::os_path`] is the real one)
    pub path: String,
    /// `path` as the file system names it (lone surrogates, non-UTF-8 bytes kept): what is
    /// found again in the tree and executed
    pub os_path: PathBuf,
    pub name: String,
    pub kind: ItemKind,
    pub scan_id: String,
    /// group: member paths captured when added (as shown)
    pub members: Vec<String>,
    /// group: the members as the file system names them, parallel to `members`
    pub os_members: Vec<PathBuf>,
    /// freeable allocation (§2.3); 0 while missing or protected
    pub alloc: i64,
    pub size: i64,
    pub files: i64,
    pub dirs: i64,
    /// mount point of the volume holding it
    pub volume: String,
    /// the scan root it was found under
    pub root: String,
    pub status: ItemStatus,
    /// who holds it open, hard links, what failed …
    pub note: String,
}

impl BasketItem {
    /// The protocol model.
    pub fn to_protocol(&self) -> sk_protocol::ops::BasketItem {
        sk_protocol::ops::BasketItem {
            id: self.id.clone(),
            scan_id: self.scan_id.clone(),
            path: self.path.clone(),
            name: self.name.clone(),
            kind: self.kind,
            members: self.members.len() as i64,
            alloc: self.alloc,
            size: self.size,
            files: self.files,
            dirs: self.dirs,
            volume: self.volume.clone(),
            status: self.status,
            note: self.note.clone(),
            tag: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merged {
    pub path: String,
    /// id of the item that already covers `path`
    pub into: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub path: String,
    pub reason: String,
}

impl Refused {
    pub fn to_protocol(&self) -> sk_protocol::ops::Refusal {
        sk_protocol::ops::Refusal {
            path: self.path.clone(),
            reason: self.reason.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddResult {
    /// ids of new items (or of a group that grew)
    pub added: Vec<String>,
    /// already covered by an item
    pub merged: Vec<Merged>,
    /// ids of items now inside a new one (gone)
    pub absorbed: Vec<String>,
    pub refused: Vec<Refused>,
}

/// One path to execute: an item's path, or one member of a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// item id
    pub item: String,
    /// as shown (keys of the operation's maps, its results)
    pub path: String,
    /// as the file system names it: what the operation is given
    pub os_path: PathBuf,
    /// bytes it holds as last scanned (max of logical and allocated): trash capacity checks
    pub size: i64,
    /// the scan root it was found under
    pub root: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BasketError {
    #[error("a small-item group takes exactly one folder")]
    GroupFolder,
}

#[derive(Clone, Debug, Default)]
struct Measure {
    /// freeable bytes apart from hard links
    base: i64,
    size: i64,
    files: i64,
    dirs: i64,
    ino: Vec<u64>,
    link_alloc: Vec<i64>,
    nlink: Vec<i64>,
    member_sizes: Vec<i64>,
}

impl Measure {
    fn combine(a: &Measure, b: &Measure) -> Measure {
        let cat = |x: &[i64], y: &[i64]| [x, y].concat();
        Measure {
            base: a.base + b.base,
            size: a.size + b.size,
            files: a.files + b.files,
            dirs: a.dirs + b.dirs,
            ino: [a.ino.as_slice(), b.ino.as_slice()].concat(),
            link_alloc: cat(&a.link_alloc, &b.link_alloc),
            nlink: cat(&a.nlink, &b.nlink),
            member_sizes: cat(&a.member_sizes, &b.member_sizes),
        }
    }
}

type Fingerprint = Option<[u8; 16]>;

#[derive(Clone, Debug)]
struct Entry {
    item: BasketItem,
    /// normalised path (a group: its folder)
    key: String,
    member_keys: Vec<String>,
    measure: Measure,
    /// parallel to item.members
    member_sizes: Vec<i64>,
    /// node state the measure was computed from (None: recompute)
    fingerprint: Fingerprint,
    missing_members: i64,
    lock_note: String,
    error_note: String,
    link_note: String,
}

impl Entry {
    fn new(item: BasketItem, key: String) -> Entry {
        Entry {
            item,
            key,
            member_keys: Vec::new(),
            measure: Measure::default(),
            member_sizes: Vec::new(),
            fingerprint: None,
            missing_members: 0,
            lock_note: String::new(),
            error_note: String::new(),
            link_note: String::new(),
        }
    }

    fn group(&self) -> bool {
        self.item.kind == ChangeKind::Group
    }

    fn covered(&self) -> Vec<String> {
        if self.group() {
            self.member_keys.clone()
        } else {
            vec![self.key.clone()]
        }
    }
}

// --------------------------------------------------------------------------- keys

/// A path as the basket compares it (`normcase(normpath(path))` of this system).
pub fn path_key(path: &str) -> String {
    if cfg!(windows) {
        nt_normcase(&nt_normpath(path))
    } else {
        posix_normpath(path)
    }
}

const SEP: char = std::path::MAIN_SEPARATOR;

/// A path as shown: the lossy form of the path the file system knows (the one file operations
/// report back, so results find their items).
fn shown(os: &Path) -> String {
    os.to_string_lossy().into_owned()
}

/// A group's members as the file system names them (items made without them: from the shown).
fn os_members_of(item: &BasketItem) -> Vec<PathBuf> {
    if item.os_members.len() == item.members.len() {
        item.os_members.clone()
    } else {
        item.members.iter().map(PathBuf::from).collect()
    }
}

fn prefix(key: &str) -> String {
    if key.ends_with(SEP) {
        key.to_string()
    } else {
        format!("{key}{SEP}")
    }
}

fn dirname(key: &str) -> String {
    if cfg!(windows) { nt_dirname(key) } else { posix_dirname(key) }
}

fn ancestors_or_self(key: &str) -> Vec<String> {
    let mut out = vec![key.to_string()];
    let mut cur = key.to_string();
    loop {
        let up = dirname(&cur);
        if up == cur || up.is_empty() {
            return out;
        }
        out.push(up.clone());
        cur = up;
    }
}

fn item_id(key: &str, group: bool) -> String {
    let mut h = Sha256::new();
    h.update(if group { b"group:".as_slice() } else { b"path:".as_slice() });
    h.update(key.as_bytes());
    h.finalize()[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn anchor(path: &str) -> String {
    let abs = super::paths::absolute(Path::new(path));
    let s = abs.to_string_lossy();
    if cfg!(windows) && s.as_bytes().get(1) == Some(&b':') {
        return format!("{}{SEP}", &s[..2]);
    }
    SEP.to_string()
}

/// Mount point of the volume holding a scan root (falls back to the path's drive or `/`).
pub fn default_volume_of(root_path: &str) -> String {
    match crate::volume::volume_for(Path::new(root_path)) {
        Ok(v) if !v.mount_point.is_empty() => v.mount_point,
        _ => anchor(root_path),
    }
}

fn kind_name(kind: Kind) -> ItemKind {
    match kind {
        Kind::File => ChangeKind::File,
        Kind::Dir => ChangeKind::Dir,
        Kind::Link => ChangeKind::Link,
        Kind::Other => ChangeKind::Other,
    }
}

fn group_name(folder_path: &str, count: usize) -> String {
    let norm = if cfg!(windows) { nt_normpath(folder_path) } else { posix_normpath(folder_path) };
    let base = norm.rsplit(['\\', '/']).next().unwrap_or("");
    let folder = if base.is_empty() { folder_path } else { base };
    fill(text("group_name"), &[("folder", folder.into()), ("count", count.into())])
}

// --------------------------------------------------------------------------- measuring

/// Digest of the node state a measure depends on (store, ids, ranges, totals, own size, flags).
fn fingerprint(nodes: &Nodes, ids: &[NodeId]) -> [u8; 16] {
    let mut h = Sha256::new();
    h.update((nodes.parents().as_ptr() as usize).to_le_bytes());
    h.update((nodes.len() as u64).to_le_bytes());
    for &i in ids {
        h.update(i.to_le_bytes());
    }
    for &i in ids {
        h.update(nodes.child_start(i).to_le_bytes());
        h.update(nodes.child_count(i).to_le_bytes());
        h.update(nodes.tot_size(i).to_le_bytes());
        h.update(nodes.tot_alloc(i).to_le_bytes());
        h.update(nodes.tot_files(i).to_le_bytes());
        h.update(nodes.tot_dirs(i).to_le_bytes());
        h.update(nodes.size(i).to_le_bytes());
        h.update(nodes.alloc(i).to_le_bytes());
        h.update(nodes.flags_raw(i).to_le_bytes());
        h.update(nodes.nlink(i).to_le_bytes());
    }
    h.finalize()[..16].try_into().unwrap()
}

/// Size, counts and freeable bytes of `ids` (each with its subtree), §2.3. `members`: the
/// nodes are a group's members, so a member folder counts as a folder itself.
fn measure(nodes: &Nodes, ids: &[NodeId], members: bool) -> Measure {
    let mut out = Measure::default();
    let mut covered: Vec<NodeId> = Vec::new();
    for &node in ids {
        let dup = nodes.flags(node).intersects(Flags::HARDLINK_DUP);
        if nodes.is_dir(node) {
            covered.extend(subtree_ids(nodes, node).unwrap_or_default());
            let (size, alloc) = (nodes.tot_size(node), nodes.tot_alloc(node));
            out.files += nodes.tot_files(node) as i64;
            out.dirs += nodes.tot_dirs(node) as i64 + members as i64;
            out.member_sizes.push(size.max(alloc));
            out.size += size;
            out.base += alloc;
            continue;
        }
        covered.push(node);
        let size = nodes.size(node);
        out.size += size;
        out.base += if dup { 0 } else { nodes.alloc(node) };
        out.files += (nodes.kind(node) == Kind::File) as i64;
        out.member_sizes.push(size.max(nodes.alloc(node)));
    }
    for &i in &covered {
        let flags = nodes.flags(i);
        let kind = nodes.kind(i);
        let counted = !flags.intersects(Flags::HARDLINK_DUP);
        let cloud = flags.intersects(Flags::CLOUD) && kind != Kind::Dir;
        let nlink = nodes.nlink(i) as i64;
        let ino = nodes.ino(i);
        let linked = kind == Kind::File && nlink > 1 && ino != 0 && !cloud;
        let alloc = nodes.alloc(i);
        if counted && (cloud || linked) {
            // a placeholder frees nothing locally; links are counted by the basket-wide pass
            out.base -= alloc;
        }
        if linked {
            out.ino.push(ino);
            out.link_alloc.push(alloc);
            out.nlink.push(nlink);
        }
    }
    out.base = out.base.max(0);
    out
}

// --------------------------------------------------------------------------- the basket

/// Mount point of the volume holding a scan root.
pub type VolumeOf = Box<dyn Fn(&str) -> String + Send + Sync>;

struct Inner {
    entries: Vec<Entry>,
    /// covered key → item id, sorted
    cover: BTreeMap<String, String>,
    /// folder key → id of its group
    groups: HashMap<String, String>,
    volume_of: VolumeOf,
    /// scan root path → mount point
    volumes: HashMap<String, String>,
    /// during one add(): absorbed id → its path
    absorbed_paths: HashMap<String, String>,
    /// during one add(): folder → names it holds twice
    shared: HashMap<NodeId, HashSet<Vec<u8>>>,
    version: u64,
}

/// The delete basket.
pub struct Basket {
    inner: Mutex<Inner>,
}

impl Default for Basket {
    fn default() -> Self {
        Basket::new(None)
    }
}

impl Basket {
    /// `volume_of`: scan root path → mount point (default: [`default_volume_of`]).
    pub fn new(volume_of: Option<VolumeOf>) -> Basket {
        Basket {
            inner: Mutex::new(Inner {
                entries: Vec::new(),
                cover: BTreeMap::new(),
                groups: HashMap::new(),
                volume_of: volume_of.unwrap_or_else(|| Box::new(default_volume_of)),
                volumes: HashMap::new(),
                absorbed_paths: HashMap::new(),
                shared: HashMap::new(),
                version: 0,
            }),
        }
    }

    // ------------------------------------------------------------------ queries

    pub fn len(&self) -> usize {
        self.inner.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bumped by every change (pushes compare it).
    pub fn version(&self) -> u64 {
        self.inner.lock().version
    }

    /// Copies of the items, in the order they were added.
    pub fn items(&self) -> Vec<BasketItem> {
        self.inner.lock().entries.iter().map(|e| e.item.clone()).collect()
    }

    pub fn get(&self, id: &str) -> Option<BasketItem> {
        let g = self.inner.lock();
        g.entries.iter().find(|e| e.item.id == id).map(|e| e.item.clone())
    }

    /// Id of the item that covers `path` (it or an ancestor is in the basket), else None.
    pub fn contains(&self, path: &str) -> Option<String> {
        self.inner.lock().covering(&path_key(path))
    }

    /// Volume mount point → bytes the basket would free (ready and locked items).
    pub fn freeable(&self) -> HashMap<String, i64> {
        let g = self.inner.lock();
        let mut out = HashMap::new();
        for e in &g.entries {
            if matches!(e.item.status, ItemStatus::Ready | ItemStatus::Locked) {
                *out.entry(e.item.volume.clone()).or_insert(0) += e.item.alloc;
            }
        }
        out
    }

    /// What executing `ids` (default: everything) touches: item paths and group members.
    /// Missing and protected items are left out.
    pub fn targets(&self, ids: Option<&[String]>) -> Vec<Target> {
        let g = self.inner.lock();
        let chosen: Vec<&Entry> = match ids {
            None => g.entries.iter().collect(),
            Some(ids) => ids
                .iter()
                .filter_map(|id| g.entries.iter().find(|e| e.item.id == *id))
                .collect(),
        };
        let mut out = Vec::new();
        for e in chosen {
            if matches!(e.item.status, ItemStatus::Missing | ItemStatus::Protected) {
                continue;
            }
            if e.group() {
                let n = e.item.members.len();
                let sizes = if e.member_sizes.len() == n { e.member_sizes.clone() } else { vec![0; n] };
                for ((p, os), s) in e.item.members.iter().zip(os_members_of(&e.item)).zip(sizes) {
                    out.push(Target {
                        item: e.item.id.clone(),
                        path: p.clone(),
                        os_path: os,
                        size: s,
                        root: e.item.root.clone(),
                    });
                }
            } else {
                out.push(Target {
                    item: e.item.id.clone(),
                    path: e.item.path.clone(),
                    os_path: e.item.os_path.clone(),
                    size: e.item.size.max(e.measure.base).max(e.item.alloc),
                    root: e.item.root.clone(),
                });
            }
        }
        out
    }

    /// Paths to hand to [`super::locks::find_lockers`] (item paths; members of groups).
    pub fn lock_targets(&self) -> Vec<String> {
        self.targets(None).into_iter().map(|t| t.path).collect()
    }

    // ------------------------------------------------------------------ changes

    /// Add nodes of one scan (`nodes`: a reader's view of its store). With `members`, `nodes`
    /// is one folder and the members (its direct children) become one small-item group.
    pub fn add(
        &self,
        nodes: &Nodes,
        scan_id: &str,
        ids: &[i64],
        members: Option<&[i64]>,
    ) -> Result<AddResult, BasketError> {
        let mut g = self.inner.lock();
        let mut result = AddResult::default();
        match members {
            Some(members) => {
                if ids.len() != 1 {
                    return Err(BasketError::GroupFolder);
                }
                g.add_group(nodes, scan_id, ids[0], members, &mut result);
            }
            None => {
                for &node in ids {
                    g.add_node(nodes, scan_id, node, &mut result);
                }
            }
        }
        // an item added and absorbed within this call is simply merged into its absorber
        let gone: HashSet<String> = result
            .absorbed
            .iter()
            .filter(|a| result.added.contains(a))
            .cloned()
            .collect();
        if !gone.is_empty() {
            result.added.retain(|i| !gone.contains(i));
            result.absorbed.retain(|i| !gone.contains(i));
            let mut gone: Vec<&String> = gone.iter().collect();
            gone.sort();
            for id in gone {
                let path = g.absorbed_paths.get(id).cloned().unwrap_or_default();
                if !path.is_empty()
                    && let Some(into) = g.covering(&path_key(&path))
                {
                    result.merged.push(Merged { path, into });
                }
            }
        }
        g.absorbed_paths.clear();
        g.shared.clear();
        g.settle();
        Ok(result)
    }

    pub fn remove(&self, ids: &[String]) {
        let mut g = self.inner.lock();
        for id in ids {
            if let Some(pos) = g.pos(id) {
                g.drop_at(pos);
            }
        }
        g.settle();
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock();
        g.entries.clear();
        g.cover.clear();
        g.groups.clear();
        g.version += 1;
    }

    /// Find every item again in its scan's tree; recompute what changed (after tree changes).
    ///
    /// Items whose scan is closed (`store_of` returns None) keep what is known. Returns the ids
    /// of items that are newly missing (the service removes them and tells the user, UI-W5-48).
    pub fn refresh(&self, store_of: &mut dyn FnMut(&str) -> Option<Store>) -> Vec<String> {
        let mut g = self.inner.lock();
        // the store of every entry, grouped
        let mut stores: Vec<Store> = Vec::new();
        let mut which: Vec<Option<usize>> = Vec::with_capacity(g.entries.len());
        for e in &g.entries {
            which.push(store_of(&e.item.scan_id).map(|s| {
                match stores.iter().position(|k| k.ptr_eq(&s)) {
                    Some(k) => k,
                    None => {
                        stores.push(s);
                        stores.len() - 1
                    }
                }
            }));
        }
        let mut newly_missing: Vec<usize> = Vec::new();
        for (k, store) in stores.iter().enumerate() {
            let nodes = store.read();
            let mut index = PathIndex::new();
            for (i, w) in which.iter().enumerate() {
                if *w != Some(k) {
                    continue;
                }
                let e = &mut g.entries[i];
                // found by the names the file system has (the shown ones may have lost some)
                let paths: Vec<PathBuf> = if e.group() {
                    os_members_of(&e.item)
                } else {
                    vec![e.item.os_path.clone()]
                };
                let found_at: Vec<Option<NodeId>> = paths
                    .iter()
                    .map(|p| index.locate(&nodes, encode_name(p.as_os_str())).0)
                    .collect();
                let found: Vec<NodeId> = found_at.iter().flatten().copied().collect();
                let was = e.item.status;
                if found.is_empty() {
                    e.missing_members = if e.group() { paths.len() as i64 } else { 0 };
                    e.measure = Measure::default();
                    e.fingerprint = None;
                    e.item.status = ItemStatus::Missing;
                    if was != ItemStatus::Missing {
                        newly_missing.push(i);
                    }
                    continue;
                }
                e.missing_members = if e.group() { (paths.len() - found.len()) as i64 } else { 0 };
                if e.group() {
                    e.item.name = group_name(&e.item.path, found.len());
                }
                let mark = Some(fingerprint(&nodes, &found));
                if mark != e.fingerprint {
                    e.measure = measure(&nodes, &found, e.group());
                    e.fingerprint = mark;
                    if e.group() {
                        let sizes: HashMap<NodeId, i64> = found
                            .iter()
                            .copied()
                            .zip(e.measure.member_sizes.iter().copied())
                            .collect();
                        e.member_sizes = found_at
                            .iter()
                            .map(|n| n.and_then(|n| sizes.get(&n).copied()).unwrap_or(0))
                            .collect();
                    }
                }
                e.item.status = Inner::status(e, true);
            }
        }
        g.settle();
        newly_missing.sort();
        newly_missing
            .into_iter()
            .map(|i| g.entries[i].item.id.clone())
            .collect()
    }

    /// Apply [`super::locks::find_lockers`] results: items (or members) held open become `locked`.
    pub fn set_lockers(&self, lockers: &HashMap<String, Vec<String>>) {
        let mut g = self.inner.lock();
        let held: HashMap<String, &Vec<String>> = lockers
            .iter()
            .filter(|(_, n)| !n.is_empty())
            .map(|(p, n)| (path_key(p), n))
            .collect();
        for e in g.entries.iter_mut() {
            let mut names: Vec<String> = Vec::new();
            for k in e.covered() {
                for n in held.get(&k).map(|v| v.as_slice()).unwrap_or(&[]) {
                    if !names.contains(n) {
                        names.push(n.clone());
                    }
                }
            }
            e.lock_note = if names.is_empty() {
                String::new()
            } else {
                fill(text("locked"), &[("names", names.join(text("list_sep")).into())])
            };
            if matches!(e.item.status, ItemStatus::Ready | ItemStatus::Locked) {
                e.item.status = Inner::status(e, false);
            }
        }
        g.settle();
    }

    /// Take executed paths out of the basket: done items leave it, groups lose done members,
    /// failures stay with their reason (UI-W5-43). Returns what left (for undo: [`Basket::reinstate`]).
    pub fn apply_results<'r>(&self, results: impl IntoIterator<Item = &'r OpResult>) -> Vec<BasketItem> {
        let mut g = self.inner.lock();
        let by_key: HashMap<String, &OpResult> = results.into_iter().map(|r| (path_key(&r.path), r)).collect();
        let mut left = Vec::new();
        let ids: Vec<String> = g.entries.iter().map(|e| e.item.id.clone()).collect();
        for id in ids {
            let Some(pos) = g.pos(&id) else { continue };
            {
                let e = &mut g.entries[pos];
                if e.covered().iter().any(|k| by_key.contains_key(k)) {
                    e.error_note.clear();
                }
            }
            if g.entries[pos].group() {
                g.apply_group(pos, &by_key, &mut left);
                continue;
            }
            let Some(res) = by_key.get(&g.entries[pos].key) else { continue };
            if res.ok {
                let mut item = g.entries[pos].item.clone();
                item.members.clear();
                item.os_members.clear();
                left.push(item);
                g.drop_at(pos);
                continue;
            }
            let e = &mut g.entries[pos];
            e.error_note = fill(text("failed"), &[("message", res.message.clone().into())]);
            if res.code == OpCode::Missing {
                e.item.status = ItemStatus::Missing;
            } else if res.code == OpCode::Locked {
                e.lock_note = res.message.clone();
                e.item.status = ItemStatus::Locked;
            }
        }
        g.settle();
        left
    }

    /// Put items back (undo of a move to the trash); numbers are recomputed by [`Basket::refresh`].
    pub fn reinstate(&self, items: &[BasketItem]) -> AddResult {
        let mut g = self.inner.lock();
        let mut result = AddResult::default();
        for item in items {
            let mut known = Measure {
                base: item.alloc,
                size: item.size,
                files: item.files,
                dirs: item.dirs,
                ..Measure::default()
            };
            if item.kind == ChangeKind::Group {
                let keys: Vec<String> = item.members.iter().map(|m| path_key(m)).collect();
                known.member_sizes = vec![0; keys.len()];
                g.insert_group(
                    (&item.path, &item.os_path),
                    &item.scan_id,
                    &item.volume,
                    &item.root,
                    item.members.clone(),
                    os_members_of(item),
                    keys,
                    known,
                    None,
                    &mut result,
                );
                continue;
            }
            let mut copy = item.clone();
            copy.status = ItemStatus::Ready;
            copy.note.clear();
            let mut entry = Entry::new(copy, path_key(&item.path));
            entry.measure = known;
            g.insert(entry, &mut result, true);
        }
        g.settle();
        result
    }
}

impl Inner {
    fn pos(&self, id: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.item.id == id)
    }

    fn covering(&self, key: &str) -> Option<String> {
        ancestors_or_self(key)
            .into_iter()
            .find_map(|k| self.cover.get(&k).cloned())
    }

    /// Ids of items with a covered key strictly below `key`.
    fn below(&self, key: &str) -> Vec<String> {
        let p = prefix(key);
        let mut ids: Vec<String> = Vec::new();
        for (k, id) in self.cover.range(p.clone()..) {
            if !k.starts_with(&p) {
                break;
            }
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids
    }

    fn index_add(&mut self, id: &str, keys: &[String]) {
        for k in keys {
            self.cover.insert(k.clone(), id.to_string());
        }
    }

    fn index_remove(&mut self, keys: &[String]) {
        for k in keys {
            self.cover.remove(k);
        }
    }

    fn drop_at(&mut self, pos: usize) {
        let e = self.entries.remove(pos);
        self.index_remove(&e.covered());
        if e.group() && self.groups.get(&e.key) == Some(&e.item.id) {
            self.groups.remove(&e.key);
        }
        self.version += 1;
    }

    fn drop_id(&mut self, id: &str) {
        if let Some(pos) = self.pos(id) {
            self.drop_at(pos);
        }
    }

    fn volume(&mut self, nodes: &Nodes, node: NodeId) -> String {
        let root_path = root_path(nodes, node);
        if let Some(v) = self.volumes.get(&root_path) {
            return v.clone();
        }
        let v = (self.volume_of)(&root_path);
        self.volumes.insert(root_path, v.clone());
        v
    }

    /// Names more than one entry of `folder` has (cached for one add).
    fn shared_names(&mut self, nodes: &Nodes, folder: NodeId) -> &HashSet<Vec<u8>> {
        self.shared.entry(folder).or_insert_with(|| {
            let mut seen: HashSet<&[u8]> = HashSet::new();
            let mut twice: HashSet<Vec<u8>> = HashSet::new();
            for c in nodes.children(folder) {
                let name = nodes.name_bytes(c);
                if !seen.insert(name) {
                    twice.insert(name.to_vec());
                }
            }
            twice
        })
    }

    fn check_node(&mut self, nodes: &Nodes, node: i64) -> Option<&'static str> {
        if node <= 0 || node as usize >= nodes.len() || nodes.flags(node as NodeId).intersects(Flags::GONE) {
            return Some(text("bad_node"));
        }
        let node = node as NodeId;
        let line: Vec<NodeId> = std::iter::once(node).chain(nodes.ancestors(node)).collect();
        if line.iter().any(|&n| nodes.flags(n).intersects(Flags::VIRTUAL)) {
            return Some(text("virtual"));
        }
        // the scan root's own name is a path
        let below: Vec<NodeId> = line.iter().copied().filter(|&n| nodes.depth(n) > 1).collect();
        // a name no path can hold: its path names something else, or nothing
        if below.iter().any(|&n| unnameable(&nodes.name(n))) {
            return Some(text("bad_name"));
        }
        // the very same name twice in one folder: the path opens one of them
        for &n in &below {
            let parent = nodes.parent(n);
            if parent >= 0 && self.shared_names(nodes, parent as NodeId).contains(nodes.name_bytes(n)) {
                return Some(text("dup_name"));
            }
        }
        if win32_trims(&nodes.path_lossy(node)) {
            return Some(text("trailing_name"));
        }
        None
    }

    /// (shown, as the file system names it) of a node; a bad node: its number.
    fn node_path(nodes: &Nodes, node: i64) -> (String, PathBuf) {
        if node > 0 && (node as usize) < nodes.len() {
            let os = nodes.path(node as NodeId);
            (shown(&os), os)
        } else {
            (node.to_string(), PathBuf::from(node.to_string()))
        }
    }

    fn add_node(&mut self, nodes: &Nodes, scan_id: &str, node: i64, result: &mut AddResult) {
        let bad = self.check_node(nodes, node);
        let (path, os_path) = Self::node_path(nodes, node);
        if let Some(bad) = bad {
            result.refused.push(Refused { path, reason: bad.to_string() });
            return;
        }
        if let Some(reason) = protected_reason(&path) {
            result.refused.push(Refused { path, reason });
            return;
        }
        let key = path_key(&path);
        if let Some(into) = self.covering(&key) {
            result.merged.push(Merged { path, into });
            return;
        }
        let id = node as NodeId;
        let item = BasketItem {
            id: item_id(&key, false),
            path: path.clone(),
            os_path,
            name: if nodes.depth(id) > 1 { nodes.name(id).into_owned() } else { path.clone() },
            kind: kind_name(nodes.kind(id)),
            scan_id: scan_id.to_string(),
            members: Vec::new(),
            os_members: Vec::new(),
            alloc: 0,
            size: 0,
            files: 0,
            dirs: 0,
            volume: self.volume(nodes, id),
            root: root_path(nodes, id),
            status: ItemStatus::Ready,
            note: String::new(),
        };
        let mut entry = Entry::new(item, key);
        entry.measure = measure(nodes, &[id], false);
        entry.fingerprint = Some(fingerprint(nodes, &[id]));
        self.insert(entry, result, false);
    }

    fn add_group(&mut self, nodes: &Nodes, scan_id: &str, folder: i64, members: &[i64], result: &mut AddResult) {
        let mut bad = self.check_node(nodes, folder);
        let (folder_path, folder_os) = Self::node_path(nodes, folder);
        if bad.is_none() && !nodes.is_dir(folder as NodeId) {
            bad = Some(text("not_folder"));
        }
        if let Some(bad) = bad {
            result.refused.push(Refused { path: folder_path, reason: bad.to_string() });
            return;
        }
        let folder_key = path_key(&folder_path);
        if let Some(into) = self.covering(&folder_key) {
            result.merged.push(Merged { path: folder_path, into });
            return;
        }
        let mut paths = Vec::new();
        let mut os_paths = Vec::new();
        let mut keys = Vec::new();
        let mut ids: Vec<NodeId> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for &m in members {
            let mut bad = self.check_node(nodes, m).map(str::to_string);
            let (path, os) = Self::node_path(nodes, m);
            if bad.is_none() && nodes.parent(m as NodeId) as i64 != folder {
                bad = Some(text("not_member").to_string());
            }
            if let Some(reason) = bad.or_else(|| protected_reason(&path)) {
                result.refused.push(Refused { path, reason });
                continue;
            }
            let key = path_key(&path);
            if !seen.insert(key.clone()) {
                continue;
            }
            if let Some(into) = self.covering(&key) {
                result.merged.push(Merged { path, into });
                continue;
            }
            paths.push(path);
            os_paths.push(os);
            keys.push(key);
            ids.push(m as NodeId);
        }
        if paths.is_empty() {
            return;
        }
        let f = folder as NodeId;
        let volume = self.volume(nodes, f);
        let m = measure(nodes, &ids, true);
        let fp = fingerprint(nodes, &ids);
        self.insert_group(
            (&folder_path, &folder_os),
            scan_id,
            &volume,
            &root_path(nodes, f),
            paths,
            os_paths,
            keys,
            m,
            Some(fp),
            result,
        );
    }

    /// Add members to the group of `folder` (shown, as the file system names it; made if needed).
    /// `os_paths` parallel to `paths`. `measure` covers the given members; `fp` is None when they
    /// are not known in a store right now.
    #[allow(clippy::too_many_arguments)]
    fn insert_group(
        &mut self,
        folder: (&str, &Path),
        scan_id: &str,
        volume: &str,
        root: &str,
        paths: Vec<String>,
        os_paths: Vec<PathBuf>,
        keys: Vec<String>,
        measure: Measure,
        fp: Fingerprint,
        result: &mut AddResult,
    ) {
        let sizes = if measure.member_sizes.len() == paths.len() {
            measure.member_sizes.clone()
        } else {
            vec![0; paths.len()]
        };
        let (folder_path, folder_os) = folder;
        let folder_key = path_key(folder_path);
        if let Some(into) = self.covering(&folder_key) {
            result
                .merged
                .extend(paths.iter().map(|p| Merged { path: p.clone(), into: into.clone() }));
            return;
        }
        let mut fresh: Vec<(String, String, i64, PathBuf)> = Vec::new();
        for (((p, k), s), os) in paths.iter().zip(&keys).zip(&sizes).zip(os_paths) {
            match self.covering(k) {
                Some(hit) => result.merged.push(Merged { path: p.clone(), into: hit }),
                None => fresh.push((p.clone(), k.clone(), *s, os)),
            }
        }
        if fresh.is_empty() {
            return;
        }
        for (_, k, _, _) in &fresh {
            // whatever lies below a new member is now inside the group
            for below in self.below(k) {
                if let Some(pos) = self.pos(&below)
                    && self.entries[pos].key != folder_key
                {
                    result.absorbed.push(below.clone());
                    let path = self.entries[pos].item.path.clone();
                    self.absorbed_paths.insert(below.clone(), path);
                    self.drop_at(pos);
                }
            }
        }
        let existing = self.groups.get(&folder_key).cloned();
        let pos = match existing.as_ref().and_then(|id| self.pos(id)) {
            Some(pos) => pos,
            None => {
                let item = BasketItem {
                    id: item_id(&folder_key, true),
                    path: folder_path.to_string(),
                    os_path: folder_os.to_path_buf(),
                    name: String::new(),
                    kind: ChangeKind::Group,
                    scan_id: scan_id.to_string(),
                    members: Vec::new(),
                    os_members: Vec::new(),
                    alloc: 0,
                    size: 0,
                    files: 0,
                    dirs: 0,
                    volume: volume.to_string(),
                    root: root.to_string(),
                    status: ItemStatus::Ready,
                    note: String::new(),
                };
                self.groups.insert(folder_key.clone(), item.id.clone());
                self.entries.push(Entry::new(item, folder_key.clone()));
                self.entries.len() - 1
            }
        };
        let was_existing = existing.is_some();
        let fresh_keys: Vec<String> = fresh.iter().map(|f| f.1.clone()).collect();
        let id = {
            let e = &mut self.entries[pos];
            let before = e.item.members.len();
            let mut os_members = os_members_of(&e.item);
            os_members.extend(fresh.iter().map(|f| f.3.clone()));
            e.item.os_members = os_members;
            e.item.members.extend(fresh.iter().map(|f| f.0.clone()));
            e.member_keys.extend(fresh_keys.iter().cloned());
            if e.member_sizes.len() != before {
                e.member_sizes = vec![0; before];
            }
            e.member_sizes.extend(fresh.iter().map(|f| f.2));
            e.item.name = group_name(folder_path, e.item.members.len());
            if !was_existing && fp.is_some() && fresh.len() == paths.len() {
                e.measure = measure;
                e.fingerprint = fp;
            } else {
                // grown, or partly known: the next refresh measures the whole group again
                e.measure = if was_existing { Measure::combine(&e.measure, &measure) } else { measure };
                e.fingerprint = None;
            }
            e.item.status = Inner::status(e, false);
            e.item.id.clone()
        };
        self.index_add(&id, &fresh_keys);
        result.added.push(id);
        self.version += 1;
    }

    fn insert(&mut self, mut entry: Entry, result: &mut AddResult, check: bool) {
        if let Some(into) = self.covering(&entry.key) {
            result.merged.push(Merged { path: entry.item.path.clone(), into });
            return;
        }
        for below in self.below(&entry.key) {
            if let Some(pos) = self.pos(&below) {
                result.absorbed.push(below.clone());
                let path = self.entries[pos].item.path.clone();
                self.absorbed_paths.insert(below, path);
                self.drop_at(pos);
            }
        }
        entry.item.status = Inner::status(&entry, check);
        let id = entry.item.id.clone();
        let key = entry.key.clone();
        self.entries.push(entry);
        self.index_add(&id, &[key]);
        result.added.push(id);
        self.version += 1;
    }

    fn apply_group(&mut self, pos: usize, by_key: &HashMap<String, &OpResult>, left: &mut Vec<BasketItem>) {
        let mut done: Vec<usize> = Vec::new();
        let mut failed: Vec<&OpResult> = Vec::new();
        for (i, k) in self.entries[pos].member_keys.iter().enumerate() {
            if let Some(res) = by_key.get(k) {
                if res.ok {
                    done.push(i);
                } else {
                    failed.push(res);
                }
            }
        }
        if !done.is_empty() {
            let e = &self.entries[pos];
            let os_members = os_members_of(&e.item);
            let moved: Vec<String> = done.iter().map(|&i| e.item.members[i].clone()).collect();
            let mut gone_item = e.item.clone();
            gone_item.name = group_name(&e.item.path, moved.len());
            gone_item.members = moved;
            gone_item.os_members = done.iter().map(|&i| os_members[i].clone()).collect();
            left.push(gone_item);
            let n = e.member_keys.len();
            let sizes = if e.member_sizes.len() == n { e.member_sizes.clone() } else { vec![0; n] };
            let done_set: HashSet<usize> = done.iter().copied().collect();
            let gone_keys: Vec<String> = done.iter().map(|&i| e.member_keys[i].clone()).collect();
            let freed: i64 = done.iter().map(|&i| sizes[i]).sum();
            let keep: Vec<usize> = (0..n).filter(|i| !done_set.contains(i)).collect();
            let members: Vec<String> = keep.iter().map(|&i| e.item.members[i].clone()).collect();
            let kept_os: Vec<PathBuf> = keep.iter().map(|&i| os_members[i].clone()).collect();
            let keys: Vec<String> = keep.iter().map(|&i| e.member_keys[i].clone()).collect();
            let kept_sizes: Vec<i64> = keep.iter().map(|&i| sizes[i]).collect();
            self.index_remove(&gone_keys);
            let e = &mut self.entries[pos];
            e.item.members = members;
            e.item.os_members = kept_os;
            e.member_keys = keys;
            e.member_sizes = kept_sizes;
            if e.item.members.is_empty() {
                let id = e.item.id.clone();
                self.drop_id(&id);
                return;
            }
            e.measure.base = (e.measure.base - freed).max(0);
            e.measure.size = (e.measure.size - freed).max(0);
            e.fingerprint = None;
            e.item.name = group_name(&e.item.path, e.item.members.len());
            self.version += 1;
        }
        if let Some(first) = failed.first() {
            self.entries[pos].error_note = fill(
                text("members_failed"),
                &[("count", failed.len().into()), ("message", first.message.clone().into())],
            );
            self.version += 1;
        }
    }

    /// Status of an item that exists; `check`: ask [`protected_reason`] again (disk).
    fn status(e: &Entry, check: bool) -> ItemStatus {
        if check && !e.group() && protected_reason(&e.item.path).is_some() {
            return ItemStatus::Protected;
        }
        if !check && e.item.status == ItemStatus::Protected {
            return ItemStatus::Protected;
        }
        if e.lock_note.is_empty() { ItemStatus::Ready } else { ItemStatus::Locked }
    }

    /// Hard links across the basket, freeable bytes and notes of every item.
    fn settle(&mut self) {
        let n = self.entries.len();
        let mut gain = vec![0i64; n];
        let mut short = vec![0i64; n]; // names of incomplete link groups per item
        let mut lacking = vec![0i64; n]; // file items: other names not in the basket
        let mut by_volume: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, e) in self.entries.iter().enumerate() {
            if matches!(e.item.status, ItemStatus::Ready | ItemStatus::Locked) && !e.measure.ino.is_empty() {
                by_volume.entry(e.item.volume.as_str()).or_default().push(i);
            }
        }
        for idx in by_volume.values() {
            // (ino, owner, alloc, nlink), within a link group: items in the order added
            let mut links: Vec<(u64, usize, i64, i64)> = Vec::new();
            for &i in idx {
                let m = &self.entries[i].measure;
                for k in 0..m.ino.len() {
                    links.push((m.ino[k], i, m.link_alloc[k], m.nlink[k]));
                }
            }
            links.sort_by_key(|l| l.0); // stable
            let mut start = 0;
            while start < links.len() {
                let mut stop = start;
                while stop < links.len() && links[stop].0 == links[start].0 {
                    stop += 1;
                }
                let group = &links[start..stop];
                let count = group.len() as i64;
                let alloc = group.iter().map(|l| l.2).max().unwrap_or(0);
                let nlink = group.iter().map(|l| l.3).max().unwrap_or(0);
                if count >= nlink {
                    gain[group[0].1] += alloc;
                } else {
                    for l in group {
                        short[l.1] += 1;
                        lacking[l.1] = lacking[l.1].max(nlink - count);
                    }
                }
                start = stop;
            }
        }
        for (i, e) in self.entries.iter_mut().enumerate() {
            let live = matches!(e.item.status, ItemStatus::Ready | ItemStatus::Locked);
            e.item.alloc = if live { e.measure.base + gain[i] } else { 0 };
            e.item.size = e.measure.size;
            e.item.files = e.measure.files;
            e.item.dirs = e.measure.dirs;
            e.link_note = if short[i] > 0 && live {
                if e.item.kind == ChangeKind::File {
                    fill(text("links_file"), &[("count", lacking[i].into())])
                } else {
                    fill(text("links_dir"), &[("count", short[i].into())])
                }
            } else {
                String::new()
            };
            let missing = e.item.status == ItemStatus::Missing;
            let notes = [
                if missing { text("missing").to_string() } else { String::new() },
                if e.missing_members > 0 && !missing {
                    fill(text("members_missing"), &[("count", e.missing_members.into())])
                } else {
                    String::new()
                },
                e.lock_note.clone(),
                e.error_note.clone(),
                e.link_note.clone(),
            ];
            e.item.note = notes
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(text("note_sep"));
        }
        self.version += 1;
    }
}

/// The path of the scan root above `node` (its name).
fn root_path(nodes: &Nodes, node: NodeId) -> String {
    let mut n = node;
    while nodes.parent(n) > 0 {
        n = nodes.parent(n) as NodeId;
    }
    nodes.name(n).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_ids() {
        let k = path_key(if cfg!(windows) { "C:\\A\\b\\..\\C" } else { "/A/b/../C" });
        assert_eq!(k, if cfg!(windows) { "c:\\a\\c" } else { "/A/C" });
        assert_eq!(item_id(&k, false).len(), 16);
        assert_ne!(item_id(&k, false), item_id(&k, true));
        let up = ancestors_or_self(&k);
        assert_eq!(up.len(), 3);
    }
}
