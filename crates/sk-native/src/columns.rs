//! The node store's columns as the performance layer writes them: the kind and flag values, and
//! [`NodeColumns`] — every column but `name_off` as plain mutable slices — with [`NameOffsets`]
//! for `name_off`. [`crate::mft::build_tree`] and [`crate::fold`] write into these; the store
//! (sk-store) owns the memory and lends the slices. Nothing here knows the store's types: the
//! values are the store's `Kind` / `Flags` values (sk-store's tests check that they agree).

/// Entry kinds (the store's `Kind`; each lister produces a subset).
pub mod kind {
    pub const FILE: u8 = 0;
    pub const DIR: u8 = 1;
    /// Symlink, junction, mount-point reparse point: shown, never descended.
    pub const LINK: u8 = 2;
    /// Device, fifo, socket.
    pub const OTHER: u8 = 3;
}

/// The node store's `Flags` bits. Listers set a subset; [`HARDLINK_DUP`] is set by
/// `hardscan::LinkSet` and the `$MFT` build; [`PENDING`], [`VIRTUAL`] and [`GONE`] only by the
/// fold, the build and the store.
pub mod flag {
    /// A directory that could not be listed.
    pub const ACCESS_DENIED: u32 = 1 << 0;
    /// Another file system mounted here (or a skipped folder): not entered.
    pub const MOUNT: u32 = 1 << 1;
    /// An additional link of an already counted file.
    pub const HARDLINK_DUP: u32 = 1 << 2;
    pub const COMPRESSED: u32 = 1 << 3;
    pub const SPARSE: u32 = 1 << 4;
    pub const HIDDEN: u32 = 1 << 5;
    pub const SYSTEM: u32 = 1 << 6;
    pub const READONLY: u32 = 1 << 7;
    pub const REPARSE: u32 = 1 << 8;
    pub const CLOUD: u32 = 1 << 9;
    pub const ERROR: u32 = 1 << 10;
    /// A folder not listed yet (a scan is on its way to it).
    pub const PENDING: u32 = 1 << 11;
    pub const ARCHIVE: u32 = 1 << 12;
    pub const ENCRYPTED: u32 = 1 << 13;
    pub const TEMPORARY: u32 = 1 << 14;
    pub const NOT_INDEXED: u32 = 1 << 15;
    pub const OFFLINE: u32 = 1 << 16;
    /// A node no file stands for (node 0, the `$MFT` build's pseudo folders).
    pub const VIRTUAL: u32 = 1 << 17;
    /// Has alternate data streams (MFT scans).
    pub const ADS: u32 = 1 << 18;
    /// A row that left the tree (deleted, or the old copy of a moved row).
    pub const GONE: u32 = 1 << 19;
}

/// The node store's columns, all but `name_off`, as plain mutable slices of the same length (one
/// element per node; the rows a store lends, or a whole store being built). `name_off` follows
/// from the names: [`NameOffsets`] where it is written row by row. The default has no rows.
#[derive(Debug, Default)]
pub struct NodeColumns<'a> {
    pub parent: &'a mut [i32],
    pub depth: &'a mut [u16],
    pub kind: &'a mut [u8],
    pub flags: &'a mut [u32],
    pub name_len: &'a mut [u16],
    pub ext: &'a mut [u16],
    pub size: &'a mut [i64],
    pub alloc: &'a mut [i64],
    pub mtime: &'a mut [u32],
    pub ctime: &'a mut [u32],
    pub atime: &'a mut [u32],
    pub ino: &'a mut [u64],
    pub nlink: &'a mut [u16],
    pub child_start: &'a mut [i32],
    pub child_count: &'a mut [i32],
    pub tot_size: &'a mut [i64],
    pub tot_alloc: &'a mut [i64],
    pub tot_files: &'a mut [i32],
    pub tot_dirs: &'a mut [i32],
}

impl NodeColumns<'_> {
    /// Rows (the length of `parent`).
    pub fn len(&self) -> usize {
        self.parent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    /// Every column has [`NodeColumns::len`] rows.
    pub fn is_uniform(&self) -> bool {
        let n = self.len();
        [
            self.depth.len(),
            self.kind.len(),
            self.flags.len(),
            self.name_len.len(),
            self.ext.len(),
            self.size.len(),
            self.alloc.len(),
            self.mtime.len(),
            self.ctime.len(),
            self.atime.len(),
            self.ino.len(),
            self.nlink.len(),
            self.child_start.len(),
            self.child_count.len(),
            self.tot_size.len(),
            self.tot_alloc.len(),
            self.tot_files.len(),
            self.tot_dirs.len(),
        ]
        .iter()
        .all(|&l| l == n)
    }
}

/// Rows of the `name_off` column: 32-bit until the store's names pass 4 GiB, 64-bit after.
#[derive(Debug)]
pub enum NameOffsets<'a> {
    Narrow(&'a mut [u32]),
    Wide(&'a mut [u64]),
}

impl Default for NameOffsets<'_> {
    /// No rows.
    fn default() -> Self {
        NameOffsets::Narrow(&mut [])
    }
}

impl NameOffsets<'_> {
    pub fn len(&self) -> usize {
        match self {
            NameOffsets::Narrow(o) => o.len(),
            NameOffsets::Wide(o) => o.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
