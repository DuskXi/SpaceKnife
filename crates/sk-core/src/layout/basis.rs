//! What every layout of one store shares: colour classes and safe child reads (Python
//! `layout/basis.py`). One [`Basis`] per store, shared by its views; the area basis is a
//! [`super::api::SizeSource`].

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use sk_store::{Kind, NodeId, Nodes};

use super::rows::NO_CLASS;
use crate::filter::classes::{ClassMap, FALLBACK_BASE, FolderClasses, looks_like_stream, resolve};

/// Per-store inputs of the layout engines; reusable across calls and engines.
pub struct Basis {
    /// None: follow the active (user-edited) table
    classes: RwLock<Option<Arc<ClassMap>>>,
    folder: RwLock<Option<Arc<dyn FolderClasses>>>,
    /// (class map, extension count) -> colour class per extension and the size splits
    ext: Mutex<Option<(usize, usize, Arc<ExtClasses>)>>,
}

/// A basis that follows the active class table, without folder classes.
impl Default for Basis {
    fn default() -> Self {
        Basis {
            classes: RwLock::new(None),
            folder: RwLock::new(None),
            ext: Mutex::new(None),
        }
    }
}

impl std::fmt::Debug for Basis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Basis").finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct ExtClasses {
    colors: Vec<u8>,
    /// (ext id, packet, class of a recording, class of a plain file)
    splits: Vec<(u16, i64, u8, u8)>,
}

/// The class lookups of one run (taken from a [`Basis`] for one store view).
pub struct Classes {
    pub map: Arc<ClassMap>,
    ext: Arc<ExtClasses>,
    /// folder-class column (None: no folder class anywhere)
    col: Option<Arc<Vec<u8>>>,
    other: u8,
}

impl Basis {
    /// A basis with a fixed class table.
    pub fn new(classes: Arc<ClassMap>) -> Basis {
        Basis {
            classes: RwLock::new(Some(classes)),
            folder: RwLock::new(None),
            ext: Mutex::new(None),
        }
    }

    /// What a scan's views use: the active class table and the folder classes of `pathclass`.
    pub fn for_scan() -> Basis {
        Basis::default().with_folder_classes(Arc::new(crate::pathclass::FolderClasses::new()))
    }

    /// With folder classes (`pathclass`).
    pub fn with_folder_classes(self, folder: Arc<dyn FolderClasses>) -> Basis {
        *self.folder.write() = Some(folder);
        self
    }

    /// Use a fixed class table (None: follow the active one).
    pub fn set_classes(&self, classes: Option<Arc<ClassMap>>) {
        *self.classes.write() = classes;
    }

    pub fn set_folder_classes(&self, folder: Option<Arc<dyn FolderClasses>>) {
        *self.folder.write() = folder;
    }

    /// The class table in use.
    pub fn class_map(&self) -> Arc<ClassMap> {
        self.classes.read().clone().unwrap_or_else(ClassMap::active)
    }

    pub fn folder_classes(&self) -> Option<Arc<dyn FolderClasses>> {
        self.folder.read().clone()
    }

    pub fn class_ids(&self) -> Vec<String> {
        self.class_map().ids().to_vec()
    }

    /// The class lookups for one run over `nodes`.
    pub fn classes(&self, nodes: &Nodes) -> Classes {
        let map = self.class_map();
        let table = nodes.ext_table();
        let key = (Arc::as_ptr(&map) as usize, table.len());
        let ext = {
            let mut cache = self.ext.lock();
            match &*cache {
                Some((p, l, e)) if (*p, *l) == key => e.clone(),
                _ => {
                    let other = map.other_index() as u8;
                    let e = Arc::new(ExtClasses {
                        colors: map.ext_colors(&table),
                        splits: map
                            .splits(&table)
                            .into_iter()
                            .map(|s| {
                                let plain = s.rest.first().map_or(other, |&r| r as u8);
                                (s.ext_id, s.packet, s.stream as u8, plain)
                            })
                            .collect(),
                    });
                    *cache = Some((key.0, key.1, e.clone()));
                    e
                }
            }
        };
        let col = self
            .folder_classes()
            .and_then(|f| f.column(nodes, &map))
            .filter(|c| c.iter().any(|&v| v != 0));
        Classes {
            other: map.other_index() as u8,
            map,
            ext,
            col,
        }
    }
}

impl Classes {
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn other(&self) -> u8 {
        self.other
    }

    /// Class index of a node by its extension alone (and size, for `ts`).
    #[inline]
    pub fn ext_cls_of(&self, nodes: &Nodes, id: NodeId) -> u8 {
        let e = nodes.ext(id);
        for &(ext_id, packet, stream, plain) in &self.ext.splits {
            if ext_id == e {
                return if looks_like_stream(nodes.size(id), packet) {
                    stream
                } else {
                    plain
                };
            }
        }
        self.ext
            .colors
            .get(e as usize)
            .copied()
            .unwrap_or(self.other)
    }

    /// Colour class of a node: by extension, under the folder classes. A folder has a class only
    /// when a folder class swallows it; any other folder gets [`NO_CLASS`].
    #[inline]
    pub fn cls_of(&self, nodes: &Nodes, id: NodeId) -> u8 {
        let mut out = self.ext_cls_of(nodes, id);
        let col = self
            .col
            .as_ref()
            .and_then(|c| c.get(id as usize).copied())
            .unwrap_or(0);
        if col != 0 {
            out = resolve(col, out as usize, self.other as usize) as u8;
        }
        if nodes.kind_raw(id) == Kind::Dir as u8 && !(col > 0 && col < FALLBACK_BASE) {
            return NO_CLASS;
        }
        out
    }
}

/// Child ids of `node` below node count `n`; empty while the folder is not listed.
///
/// A scan may list this folder while we read: count before start, a range never starts at node 0
/// (else: not listed yet), and never past the node count.
#[inline]
pub fn children(nodes: &Nodes, node: NodeId, n: usize) -> std::ops::Range<NodeId> {
    let count = nodes.child_count(node);
    if count <= 0 {
        return 0..0;
    }
    let start = nodes.child_start(node);
    if start < 1 {
        return 0..0;
    }
    let stop = (start as i64 + count as i64)
        .min(n as i64)
        .max(start as i64);
    start as NodeId..stop as NodeId
}
