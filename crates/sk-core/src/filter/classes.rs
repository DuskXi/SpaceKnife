//! File classes as the filter engine and the layouts read them: per-extension membership, the
//! size-split extensions (`ts`, `mts`) and the folder-class column.
//!
//! [`ClassMap`] is the class table in the form the filter and the layouts read it: per-extension
//! membership as class indices and the size splits. It is made from the classes task's
//! [`crate::classes::ClassTable`] ([`ClassMap::from_table`], [`ClassMap::active`]: the user's table,
//! cached per table version) and keeps that table, which the folder-class column of
//! [`crate::pathclass::FolderClasses`] is keyed on (it implements [`FolderClasses`] here).

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use sk_protocol::classes::OTHER;
use sk_protocol::filter::FilterClassInfo;
use sk_store::Nodes;

use crate::classes::ClassTable;

/// Extensions a recording and a source file share, told apart by size alone:
/// extension -> (class of a recording, packet size).
pub const STREAM_SPLITS: &[(&str, &str, i64)] = &[("ts", "video", 188), ("mts", "video", 192)];
/// A whole number of packets this large is a recording.
pub const STREAM_ALIGNED_MIN: i64 = 1 << 20;
/// Anything this large is a recording: no source file is.
pub const STREAM_ANY_MIN: i64 = 16 << 20;

/// Folder-class column values (see `pathclass`): 0 nothing, 1–127 the swallowing class's index + 1,
/// 128–255 the fallback class's index + 128.
pub const FALLBACK_BASE: u8 = 128;

/// A file this large is a transport-stream recording, not source code.
#[inline]
pub fn looks_like_stream(size: i64, packet: i64) -> bool {
    size >= STREAM_ANY_MIN || (size >= STREAM_ALIGNED_MIN && size % packet == 0)
}

/// A size-split extension of one extension table, under one class map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    pub ext_id: u16,
    pub packet: i64,
    /// class index of a recording
    pub stream: usize,
    /// class indices of any other file with this extension (empty: "other")
    pub rest: Vec<usize>,
}

/// Classes in display order (`other` among them), with per-extension membership.
#[derive(Clone, Debug)]
pub struct ClassMap {
    classes: Vec<FilterClassInfo>,
    ids: Vec<String>,
    other: usize,
    /// lower-case extension -> owning class indices, in class order
    ext_classes: HashMap<String, Vec<usize>>,
    /// the classes task's table it was made from (keys the folder-class column)
    table: Option<Arc<ClassTable>>,
}

impl Default for ClassMap {
    fn default() -> Self {
        ClassMap::builtin().clone()
    }
}

static BUILTIN: std::sync::LazyLock<ClassMap> =
    std::sync::LazyLock::new(|| ClassMap::from_table(ClassTable::builtin()));

impl ClassMap {
    /// A map of `classes`; None without an `other` class.
    pub fn new(classes: &[FilterClassInfo]) -> Option<ClassMap> {
        let ids: Vec<String> = classes.iter().map(|c| c.id.clone()).collect();
        let other = ids.iter().position(|i| i == OTHER)?;
        let mut ext_classes: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, c) in classes.iter().enumerate() {
            for e in &c.extensions {
                let owners = ext_classes.entry(e.to_lowercase()).or_default();
                if !owners.contains(&i) {
                    owners.push(i);
                }
            }
        }
        Some(ClassMap {
            classes: classes.to_vec(),
            ids,
            other,
            ext_classes,
            table: None,
        })
    }

    /// The map of a class table of `crate::classes`.
    pub fn from_table(table: Arc<ClassTable>) -> ClassMap {
        let mut m = ClassMap::new(table.classes()).expect("a class table has an 'other' class");
        m.table = Some(table);
        m
    }

    /// The map of the active (user-edited) class table, the same object until the table changes.
    pub fn active() -> Arc<ClassMap> {
        static ACTIVE: Mutex<Option<(u64, Arc<ClassMap>)>> = Mutex::new(None);
        let version = crate::classes::version();
        let mut cur = ACTIVE.lock();
        if let Some((v, m)) = &*cur
            && *v == version
        {
            return m.clone();
        }
        let m = Arc::new(ClassMap::from_table(crate::classes::active()));
        *cur = Some((version, m.clone()));
        m
    }

    /// The class table this map was made from, if any.
    pub fn table(&self) -> Option<&Arc<ClassTable>> {
        self.table.as_ref()
    }

    /// The built-in table (shared).
    pub fn builtin() -> &'static ClassMap {
        &BUILTIN
    }

    pub fn classes(&self) -> &[FilterClassInfo] {
        &self.classes
    }

    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Index of `other`.
    pub fn other_index(&self) -> usize {
        self.other
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.ids.iter().position(|i| i == id)
    }

    /// Owning classes of an extension (empty: in no class, i.e. "other").
    pub fn classes_of(&self, ext: &str) -> &[usize] {
        let lower;
        let key = if ext.bytes().any(|b| b.is_ascii_uppercase()) || !ext.is_ascii() {
            lower = ext.to_lowercase();
            lower.as_str()
        } else {
            ext
        };
        self.ext_classes.get(key).map_or(&[], Vec::as_slice)
    }

    /// The colour class of an extension: its first owner, else `other`.
    pub fn color_class(&self, ext: &str) -> usize {
        self.classes_of(ext).first().copied().unwrap_or(self.other)
    }

    /// (class of a recording, packet, other owners) when `ext` is split by size here: only while the
    /// recording's class still lists the extension.
    fn split_of(&self, ext: &str) -> Option<(usize, i64, Vec<usize>)> {
        let lower = ext.to_lowercase();
        let (_, stream_id, packet) = STREAM_SPLITS.iter().find(|(e, _, _)| *e == lower)?;
        let stream = self.index_of(stream_id)?;
        let owners = self.classes_of(&lower);
        if !owners.contains(&stream) {
            return None;
        }
        Some((
            stream,
            *packet,
            owners.iter().copied().filter(|&o| o != stream).collect(),
        ))
    }

    /// The classes one file belongs to (empty: "other").
    pub fn file_classes(&self, ext: &str, size: i64) -> Vec<usize> {
        match self.split_of(ext) {
            None => self.classes_of(ext).to_vec(),
            Some((stream, packet, rest)) => {
                if looks_like_stream(size, packet) {
                    vec![stream]
                } else {
                    rest
                }
            }
        }
    }

    /// The extensions of `ext_table` whose files are split by size.
    pub fn splits(&self, ext_table: &[Arc<str>]) -> Vec<Split> {
        let mut out = Vec::new();
        for (i, e) in ext_table.iter().enumerate() {
            if e.is_empty() {
                continue;
            }
            if let Some((stream, packet, rest)) = self.split_of(e) {
                out.push(Split {
                    ext_id: i as u16,
                    packet,
                    stream,
                    rest,
                });
            }
        }
        out
    }

    /// Per extension-table id: the extension belongs to class `cls` (for `other`: to no class).
    pub fn ext_mask(&self, cls: usize, ext_table: &[Arc<str>]) -> Vec<bool> {
        ext_table
            .iter()
            .map(|e| {
                let owners = self.classes_of(e);
                if cls == self.other {
                    owners.is_empty()
                } else {
                    owners.contains(&cls)
                }
            })
            .collect()
    }

    /// Colour class per extension-table id (size-split extensions: see [`ClassMap::splits`]).
    pub fn ext_colors(&self, ext_table: &[Arc<str>]) -> Vec<u8> {
        ext_table
            .iter()
            .map(|e| {
                if e.is_empty() {
                    self.other as u8
                } else {
                    self.color_class(e) as u8
                }
            })
            .collect()
    }
}

/// The folder-class column of a store (Python `pathclass.column`): one byte per node, values as
/// described at [`FALLBACK_BASE`]. Implemented by the scan task's `pathclass`.
pub trait FolderClasses: Send + Sync {
    /// The column for `nodes` (at least `nodes.len()` long, up to date with the view), or None when
    /// no folder class applies anywhere.
    fn column(&self, nodes: &Nodes, classes: &ClassMap) -> Option<Arc<Vec<u8>>>;
}

/// The scan task's folder classes (`pathclass`), for the table of `classes` (the active table when
/// the map was not made from one).
impl FolderClasses for crate::pathclass::FolderClasses {
    fn column(&self, nodes: &Nodes, classes: &ClassMap) -> Option<Arc<Vec<u8>>> {
        let table = classes
            .table()
            .cloned()
            .unwrap_or_else(crate::classes::active);
        Some(crate::pathclass::FolderClasses::column(self, nodes, &table))
    }
}

/// Class index of a file under the folder-class value `col`: `base` is its class by extension.
#[inline]
pub fn resolve(col: u8, base: usize, other: usize) -> usize {
    if col > 0 && col < FALLBACK_BASE {
        (col - 1) as usize
    } else if col >= FALLBACK_BASE && base == other {
        (col - FALLBACK_BASE) as usize
    } else {
        base
    }
}
