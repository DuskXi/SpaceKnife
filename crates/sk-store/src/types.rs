//! Node kinds, flags, column ids and the batches a scanner hands to the store.

use std::fmt;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

/// A node's id: its row in every column. Node 0 is the virtual top node.
pub type NodeId = u32;

/// What a node is. Stored as one byte (`kind` column).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    File = 0,
    Dir = 1,
    /// symlink / junction / mount-point reparse: shown, never descended
    Link = 2,
    /// device, fifo, socket; also every `GONE` row
    Other = 3,
}

impl Kind {
    #[inline]
    pub fn from_u8(v: u8) -> Kind {
        match v {
            0 => Kind::File,
            1 => Kind::Dir,
            2 => Kind::Link,
            _ => Kind::Other,
        }
    }
}

/// Node flags (`flags` column, u32). Values are part of the snapshot format.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags(pub u32);

impl Flags {
    pub const NONE: Flags = Flags(0);
    /// directory could not be listed
    pub const ACCESS_DENIED: Flags = Flags(1 << 0);
    /// another filesystem mounted here; not crossed (never `PENDING`)
    pub const MOUNT: Flags = Flags(1 << 1);
    /// additional link of an already counted file: contributes 0
    pub const HARDLINK_DUP: Flags = Flags(1 << 2);
    pub const COMPRESSED: Flags = Flags(1 << 3);
    pub const SPARSE: Flags = Flags(1 << 4);
    pub const HIDDEN: Flags = Flags(1 << 5);
    pub const SYSTEM: Flags = Flags(1 << 6);
    pub const READONLY: Flags = Flags(1 << 7);
    pub const REPARSE: Flags = Flags(1 << 8);
    /// cloud placeholder (OneDrive/iCloud): not stored locally
    pub const CLOUD: Flags = Flags(1 << 9);
    /// could not stat this entry
    pub const ERROR: Flags = Flags(1 << 10);
    /// directory not listed yet (scan in progress)
    pub const PENDING: Flags = Flags(1 << 11);
    pub const ARCHIVE: Flags = Flags(1 << 12);
    pub const ENCRYPTED: Flags = Flags(1 << 13);
    pub const TEMPORARY: Flags = Flags(1 << 14);
    pub const NOT_INDEXED: Flags = Flags(1 << 15);
    pub const OFFLINE: Flags = Flags(1 << 16);
    /// node 0
    pub const VIRTUAL: Flags = Flags(1 << 17);
    /// has alternate data streams (reserved)
    pub const ADS: Flags = Flags(1 << 18);
    /// deleted, or the old copy of a moved row (see [`crate::mutate`]): outside every child range
    pub const GONE: Flags = Flags(1 << 19);

    #[inline]
    pub const fn bits(self) -> u32 {
        self.0
    }
    #[inline]
    pub const fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }
    #[inline]
    pub const fn intersects(self, other: Flags) -> bool {
        self.0 & other.0 != 0
    }
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for Flags {
    type Output = Flags;
    fn bitor(self, rhs: Flags) -> Flags {
        Flags(self.0 | rhs.0)
    }
}
impl BitOrAssign for Flags {
    fn bitor_assign(&mut self, rhs: Flags) {
        self.0 |= rhs.0;
    }
}
impl BitAnd for Flags {
    type Output = Flags;
    fn bitand(self, rhs: Flags) -> Flags {
        Flags(self.0 & rhs.0)
    }
}
impl BitAndAssign for Flags {
    fn bitand_assign(&mut self, rhs: Flags) {
        self.0 &= rhs.0;
    }
}
impl Not for Flags {
    type Output = Flags;
    fn not(self) -> Flags {
        Flags(!self.0)
    }
}
impl fmt::Debug for Flags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Flags({:#x})", self.0)
    }
}

/// The node columns. The order is the storage order; [`Col::item`] is the element size.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Col {
    Parent = 0,
    Depth,
    Kind,
    Flags,
    NameOff,
    NameLen,
    Ext,
    Size,
    Alloc,
    Mtime,
    Ctime,
    Atime,
    Ino,
    Nlink,
    ChildStart,
    ChildCount,
    TotSize,
    TotAlloc,
    TotFiles,
    TotDirs,
}

/// Number of columns.
pub const NCOL: usize = 20;

impl Col {
    pub const ALL: [Col; NCOL] = [
        Col::Parent,
        Col::Depth,
        Col::Kind,
        Col::Flags,
        Col::NameOff,
        Col::NameLen,
        Col::Ext,
        Col::Size,
        Col::Alloc,
        Col::Mtime,
        Col::Ctime,
        Col::Atime,
        Col::Ino,
        Col::Nlink,
        Col::ChildStart,
        Col::ChildCount,
        Col::TotSize,
        Col::TotAlloc,
        Col::TotFiles,
        Col::TotDirs,
    ];

    /// Element size in bytes (`NameOff`: the narrow size; it widens to 8 past 4 GiB of names).
    pub const fn item(self) -> usize {
        match self {
            Col::Kind => 1,
            Col::Depth | Col::NameLen | Col::Ext | Col::Nlink => 2,
            Col::Parent
            | Col::Flags
            | Col::NameOff
            | Col::Mtime
            | Col::Ctime
            | Col::Atime
            | Col::ChildStart
            | Col::ChildCount
            | Col::TotFiles
            | Col::TotDirs => 4,
            Col::Size | Col::Alloc | Col::Ino | Col::TotSize | Col::TotAlloc => 8,
        }
    }

    /// Element type in numpy notation (snapshot headers): `"i4"`, `"u2"`, ...
    pub const fn dtype(self) -> &'static str {
        match self {
            Col::Parent | Col::ChildStart | Col::ChildCount | Col::TotFiles | Col::TotDirs => "i4",
            Col::Depth | Col::NameLen | Col::Ext | Col::Nlink => "u2",
            Col::Kind => "u1",
            Col::Flags | Col::NameOff | Col::Mtime | Col::Ctime | Col::Atime => "u4",
            Col::Size | Col::Alloc | Col::TotSize | Col::TotAlloc => "i8",
            Col::Ino => "u8",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Col::Parent => "parent",
            Col::Depth => "depth",
            Col::Kind => "kind",
            Col::Flags => "flags",
            Col::NameOff => "name_off",
            Col::NameLen => "name_len",
            Col::Ext => "ext",
            Col::Size => "size",
            Col::Alloc => "alloc",
            Col::Mtime => "mtime",
            Col::Ctime => "ctime",
            Col::Atime => "atime",
            Col::Ino => "ino",
            Col::Nlink => "nlink",
            Col::ChildStart => "child_start",
            Col::ChildCount => "child_count",
            Col::TotSize => "tot_size",
            Col::TotAlloc => "tot_alloc",
            Col::TotFiles => "tot_files",
            Col::TotDirs => "tot_dirs",
        }
    }

    pub fn from_name(name: &str) -> Option<Col> {
        Col::ALL.into_iter().find(|c| c.name() == name)
    }

    /// Columns that may change after their row is published while readers run (scan appends,
    /// see the crate docs): they are read and written atomically.
    pub const fn is_atomic(self) -> bool {
        matches!(
            self,
            Col::Flags
                | Col::ChildStart
                | Col::ChildCount
                | Col::TotSize
                | Col::TotAlloc
                | Col::TotFiles
                | Col::TotDirs
        )
    }
}

mod sealed {
    pub trait Sealed {}
}

/// Plain integer types a column holds ([`crate::Writer::set_raw`]).
pub trait ColValue: Copy + sealed::Sealed {}
macro_rules! col_value {
    ($($t:ty),*) => { $(impl sealed::Sealed for $t {} impl ColValue for $t {})* };
}
col_value!(u8, u16, u32, i32, i64, u64);

/// Bytes of columns per row (narrow `name_off`): 89.
pub const ROW_BYTES: usize = {
    let mut sum = 0;
    let mut i = 0;
    while i < NCOL {
        sum += Col::ALL[i].item();
        i += 1;
    }
    sum
};

/// Times of a node (unix seconds, 0 = unknown).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Times {
    pub mtime: u32,
    pub ctime: u32,
    pub atime: u32,
}

/// One entry for [`EntryBatch::push`].
#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    pub name: &'a [u8],
    pub kind: Kind,
    pub flags: Flags,
    pub size: i64,
    pub alloc: i64,
    pub mtime: u32,
    pub ctime: u32,
    pub atime: u32,
    pub ino: u64,
    pub nlink: u16,
    /// index into the store's extension table ([`crate::Appender::ext_id`])
    pub ext: u16,
}

impl<'a> Entry<'a> {
    pub fn new(name: &'a [u8], kind: Kind) -> Self {
        Entry {
            name,
            kind,
            flags: Flags::NONE,
            size: 0,
            alloc: 0,
            mtime: 0,
            ctime: 0,
            atime: 0,
            ino: 0,
            nlink: 1,
            ext: 0,
        }
    }
    pub fn file(name: &'a [u8], size: i64, alloc: i64) -> Self {
        Entry {
            size,
            alloc,
            ..Entry::new(name, Kind::File)
        }
    }
    pub fn dir(name: &'a [u8]) -> Self {
        Entry::new(name, Kind::Dir)
    }
}

/// Entries of one or more directory listings, struct-of-arrays: what a scanner (or the file
/// monitor) gives to [`crate::Appender::ingest_listing`], [`crate::Appender::ingest_slice`] and
/// [`crate::mutate::relist`]. Names are UTF-8 (WTF-8 on Windows, raw bytes on POSIX: see
/// [`crate::names`]) concatenated in `names`, each `name_lens[i]` bytes long.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntryBatch {
    pub names: Vec<u8>,
    pub name_lens: Vec<u16>,
    pub ext: Vec<u16>,
    pub kind: Vec<u8>,
    pub flags: Vec<u32>,
    pub size: Vec<i64>,
    pub alloc: Vec<i64>,
    pub mtime: Vec<u32>,
    pub ctime: Vec<u32>,
    pub atime: Vec<u32>,
    pub ino: Vec<u64>,
    pub nlink: Vec<u16>,
}

impl EntryBatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(entries: usize, name_bytes: usize) -> Self {
        EntryBatch {
            names: Vec::with_capacity(name_bytes),
            name_lens: Vec::with_capacity(entries),
            ext: Vec::with_capacity(entries),
            kind: Vec::with_capacity(entries),
            flags: Vec::with_capacity(entries),
            size: Vec::with_capacity(entries),
            alloc: Vec::with_capacity(entries),
            mtime: Vec::with_capacity(entries),
            ctime: Vec::with_capacity(entries),
            atime: Vec::with_capacity(entries),
            ino: Vec::with_capacity(entries),
            nlink: Vec::with_capacity(entries),
        }
    }

    pub fn len(&self) -> usize {
        self.name_lens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name_lens.is_empty()
    }

    pub fn clear(&mut self) {
        self.names.clear();
        self.name_lens.clear();
        self.ext.clear();
        self.kind.clear();
        self.flags.clear();
        self.size.clear();
        self.alloc.clear();
        self.mtime.clear();
        self.ctime.clear();
        self.atime.clear();
        self.ino.clear();
        self.nlink.clear();
    }

    /// Appends one entry. Panics if the name is longer than 65535 bytes.
    pub fn push(&mut self, e: Entry<'_>) {
        let len = u16::try_from(e.name.len()).expect("a name longer than 65535 bytes");
        self.names.extend_from_slice(e.name);
        self.name_lens.push(len);
        self.ext.push(e.ext);
        self.kind.push(e.kind as u8);
        self.flags.push(e.flags.0);
        self.size.push(e.size);
        self.alloc.push(e.alloc);
        self.mtime.push(e.mtime);
        self.ctime.push(e.ctime);
        self.atime.push(e.atime);
        self.ino.push(e.ino);
        self.nlink.push(e.nlink);
    }

    /// Every column has the same length and the names add up.
    pub fn check(&self) -> Result<(), crate::StoreError> {
        let n = self.name_lens.len();
        let same = [
            self.ext.len(),
            self.kind.len(),
            self.flags.len(),
            self.size.len(),
            self.alloc.len(),
            self.mtime.len(),
            self.ctime.len(),
            self.atime.len(),
            self.ino.len(),
            self.nlink.len(),
        ]
        .iter()
        .all(|&l| l == n);
        let bytes: usize = self.name_lens.iter().map(|&l| l as usize).sum();
        if !same || bytes != self.names.len() {
            return Err(crate::StoreError::BadBatch);
        }
        Ok(())
    }

    /// The columns as sk-native's fold reads them.
    pub fn columns(&self) -> sk_native::fold::EntryColumns<'_> {
        sk_native::fold::EntryColumns {
            names: &self.names,
            name_lens: &self.name_lens,
            ext: &self.ext,
            kind: &self.kind,
            flags: &self.flags,
            size: &self.size,
            alloc: &self.alloc,
            mtime: &self.mtime,
            ctime: &self.ctime,
            atime: &self.atime,
            ino: &self.ino,
            nlink: &self.nlink,
        }
    }

    /// Byte offset of every name in `names` (plus the end).
    pub fn name_offsets(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.len() + 1);
        let mut pos = 0usize;
        out.push(0);
        for &l in &self.name_lens {
            pos += l as usize;
            out.push(pos);
        }
        out
    }

    /// The name of entry `i` (slow: sums lengths; use [`EntryBatch::name_offsets`] in loops).
    pub fn name(&self, i: usize) -> &[u8] {
        let start: usize = self.name_lens[..i].iter().map(|&l| l as usize).sum();
        &self.names[start..start + self.name_lens[i] as usize]
    }

    /// A batch of the rows `ranges` selects, in order.
    pub fn select(&self, ranges: &[std::ops::Range<usize>]) -> EntryBatch {
        let offs = self.name_offsets();
        let rows: usize = ranges.iter().map(|r| r.len()).sum();
        let mut out = EntryBatch::with_capacity(rows, 0);
        for r in ranges {
            out.names
                .extend_from_slice(&self.names[offs[r.start]..offs[r.end]]);
            out.name_lens.extend_from_slice(&self.name_lens[r.clone()]);
            out.ext.extend_from_slice(&self.ext[r.clone()]);
            out.kind.extend_from_slice(&self.kind[r.clone()]);
            out.flags.extend_from_slice(&self.flags[r.clone()]);
            out.size.extend_from_slice(&self.size[r.clone()]);
            out.alloc.extend_from_slice(&self.alloc[r.clone()]);
            out.mtime.extend_from_slice(&self.mtime[r.clone()]);
            out.ctime.extend_from_slice(&self.ctime[r.clone()]);
            out.atime.extend_from_slice(&self.atime[r.clone()]);
            out.ino.extend_from_slice(&self.ino[r.clone()]);
            out.nlink.extend_from_slice(&self.nlink[r.clone()]);
        }
        out
    }
}

/// Which directory a listing of a slice lists ([`crate::Appender::ingest_slice`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliceRef {
    /// a node already in the store
    Node(NodeId),
    /// entry `entry` of the earlier listing `listing` of the same slice
    Entry { listing: u32, entry: u32 },
}

/// A materialised node, for queries and tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeView {
    pub id: NodeId,
    pub name: String,
    pub kind: Kind,
    pub flags: Flags,
    pub parent: i32,
    pub depth: u16,
    pub size: i64,
    pub alloc: i64,
    /// effective logical bytes: subtree total for folders, own size otherwise (0: hard-link dup)
    pub tot_size: i64,
    /// effective allocated bytes, likewise
    pub tot_alloc: i64,
    pub tot_files: i32,
    pub tot_dirs: i32,
    pub mtime: u32,
    pub ctime: u32,
    pub atime: u32,
    pub child_count: i32,
    pub ext: String,
}

/// Errors of the store's building and changing calls.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("node {0} is not a directory")]
    NotADirectory(NodeId),
    #[error("directory {0} was already listed")]
    AlreadyListed(NodeId),
    #[error("roots must be added before scanning starts")]
    RootsAfterScan,
    #[error("a node of {0} bytes of name is too long")]
    NameTooLong(usize),
    #[error("an entry batch whose columns differ in length")]
    BadBatch,
    #[error("bad slice: {0}")]
    BadSlice(&'static str),
    #[error("node {0} is not in the store")]
    NotInStore(i64),
    #[error("node {0} is not a folder of the tree")]
    NotAFolder(NodeId),
    #[error("subtree walk below node {0} revisited nodes")]
    RunawayWalk(NodeId),
    #[error("the store reached 2^31 nodes")]
    Full,
    #[error("inconsistent columns: {0}")]
    Inconsistent(&'static str),
}
