//! Types and pure rules shared by the platform listers (mirrors spaceknife/scan/worker.py).

/// worker.KIND_* (each platform produces a subset).
#[allow(dead_code)]
pub mod kind {
    pub const FILE: u8 = 0;
    pub const DIR: u8 = 1;
    pub const LINK: u8 = 2;
    pub const OTHER: u8 = 3;
}

/// spaceknife.store.nodes.Flag bits, the literals worker.py uses (each platform sets a subset).
#[allow(dead_code)]
pub mod flag {
    pub const ACCESS_DENIED: u32 = 1 << 0;
    pub const MOUNT: u32 = 1 << 1;
    pub const COMPRESSED: u32 = 1 << 3;
    pub const SPARSE: u32 = 1 << 4;
    pub const HIDDEN: u32 = 1 << 5;
    pub const SYSTEM: u32 = 1 << 6;
    pub const READONLY: u32 = 1 << 7;
    pub const REPARSE: u32 = 1 << 8;
    pub const CLOUD: u32 = 1 << 9;
    pub const ERROR: u32 = 1 << 10;
    pub const ARCHIVE: u32 = 1 << 12;
    pub const ENCRYPTED: u32 = 1 << 13;
    pub const TEMPORARY: u32 = 1 << 14;
    pub const NOT_INDEXED: u32 = 1 << 15;
    pub const OFFLINE: u32 = 1 << 16;
}

/// Per-root options (worker.RootOptions).
#[derive(Clone, Debug)]
pub struct RootOpts {
    pub dev: u64,
    pub cluster_size: i64,
    pub resident_max: i64,
    pub cross_mounts: bool,
    /// Folders not to enter, as mount points (POSIX paths as bytes; worker.RootOptions.skip).
    pub skip: Vec<Vec<u8>>,
    /// Folders not to enter, as not accessible (worker.RootOptions.protect).
    pub protect: Vec<Vec<u8>>,
}

impl Default for RootOpts {
    fn default() -> Self {
        Self {
            dev: 0,
            cluster_size: 4096,
            resident_max: 0,
            cross_mounts: false,
            skip: Vec::new(),
            protect: Vec::new(),
        }
    }
}

/// How directories are opened (Windows; POSIX ignores it). Both on by default, and both fall back
/// to the plain open by path wherever they cannot be used.
#[derive(Clone, Copy, Debug)]
pub struct OpenOpts {
    /// Subdirectories found during a call are opened by their file id, relative to a directory
    /// of the same volume, instead of by full path: the file system skips resolving every path
    /// component again (half the CPU time of a warm NTFS scan). Off when the root crosses mounts.
    pub by_id: bool,
    /// Directories are opened with FILE_WRITE_ATTRIBUTES and told not to update their last
    /// access time (SetFileTime's -1): listing them then writes nothing (on NTFS with access
    /// time updates on, 2–3 GB per scan of a big system volume) and raises no change
    /// notification. Where that access is denied, the folder is opened as before.
    pub keep_access_time: bool,
}

impl Default for OpenOpts {
    fn default() -> Self {
        Self {
            by_id: true,
            keep_access_time: true,
        }
    }
}

/// Directory path in the platform's native form: UTF-16 units on Windows, raw bytes on POSIX.
#[cfg(windows)]
pub type Unit = u16;
#[cfg(not(windows))]
pub type Unit = u8;
pub type NativePath = Box<[Unit]>;

/// One entry's columns (everything but the name).
#[derive(Clone, Copy, Debug, Default)]
pub struct Row {
    pub kind: u8,
    pub flags: u32,
    pub size: i64,
    pub alloc: i64,
    pub mtime: u32,
    pub ctime: u32,
    pub atime: u32,
    pub ino: u64,
    pub nlink: u16,
    pub dev: u64,
    /// Byte length of the raw (not yet lowercased) extension at the end of the name; 0 = id 0.
    pub ext_len: u8,
}

/// Column store one pool thread appends its listings to; a listing is a contiguous range.
#[derive(Default)]
pub struct Arena {
    pub names: Vec<u8>,
    pub name_lens: Vec<u16>,
    pub ext_lens: Vec<u8>,
    pub kind: Vec<u8>,
    pub flags: Vec<u32>,
    pub size: Vec<i64>,
    pub alloc: Vec<i64>,
    pub mtime: Vec<u32>,
    pub ctime: Vec<u32>,
    pub atime: Vec<u32>,
    pub ino: Vec<u64>,
    pub nlink: Vec<u16>,
    pub dev: Vec<u64>,
}

#[derive(Clone, Copy, Debug)]
pub struct Mark {
    pub entries: usize,
    pub names: usize,
}

impl Arena {
    pub fn len(&self) -> usize {
        self.kind.len()
    }

    pub fn mark(&self) -> Mark {
        Mark {
            entries: self.kind.len(),
            names: self.names.len(),
        }
    }

    /// Appends one entry; its name must already be in `names` from byte `name_start` on.
    pub fn push_row(&mut self, name_start: usize, row: Row) {
        self.name_lens.push((self.names.len() - name_start) as u16);
        self.ext_lens.push(row.ext_len);
        self.kind.push(row.kind);
        self.flags.push(row.flags);
        self.size.push(row.size);
        self.alloc.push(row.alloc);
        self.mtime.push(row.mtime);
        self.ctime.push(row.ctime);
        self.atime.push(row.atime);
        self.ino.push(row.ino);
        self.nlink.push(row.nlink);
        self.dev.push(row.dev);
    }

    /// Drops everything appended after `m` (a listing that failed half-way).
    pub fn truncate(&mut self, m: Mark) {
        self.names.truncate(m.names);
        let n = m.entries;
        self.name_lens.truncate(n);
        self.ext_lens.truncate(n);
        self.kind.truncate(n);
        self.flags.truncate(n);
        self.size.truncate(n);
        self.alloc.truncate(n);
        self.mtime.truncate(n);
        self.ctime.truncate(n);
        self.atime.truncate(n);
        self.ino.truncate(n);
        self.nlink.truncate(n);
        self.dev.truncate(n);
    }
}

/// worker._secs of a Python stat float `sec + nsec * 1e-9` (same double arithmetic as CPython).
pub fn secs(sec: i64, nsec: u64) -> u32 {
    let t = sec as f64 + nsec as f64 * 1e-9;
    if t.is_nan() || t <= 0.0 {
        0
    } else if t >= u32::MAX as f64 {
        u32::MAX
    } else {
        t as u32
    }
}

/// Length in bytes of the extension worker._Acc.ext_id takes from a name (`rfind(".")`, not at
/// 0, at most 16 code points after it), or 0 for "no extension". `count_cps` counts the Python
/// code points of a byte run in the platform's name encoding.
pub fn ext_len_with(name: &[u8], count_cps: impl Fn(&[u8]) -> usize) -> u8 {
    let Some(dot) = name.iter().rposition(|&b| b == b'.') else {
        return 0;
    };
    if dot == 0 {
        return 0;
    }
    let tail = &name[dot + 1..];
    if tail.len() > 64 || count_cps(tail) > 16 {
        return 0;
    }
    tail.len() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(name: &str) -> &str {
        let n = ext_len_with(name.as_bytes(), |b| {
            std::str::from_utf8(b).unwrap().chars().count()
        }) as usize;
        &name[name.len() - n..]
    }

    #[test]
    fn ext_rules_match_worker() {
        assert_eq!(ext("a.TXT"), "TXT");
        assert_eq!(ext(".bashrc"), "");
        assert_eq!(ext("noext"), "");
        assert_eq!(ext("trailing."), "");
        assert_eq!(ext("a.b.c"), "c");
        assert_eq!(ext("x.0123456789abcdef"), "0123456789abcdef");
        assert_eq!(ext("x.0123456789abcdefg"), "");
        assert_eq!(ext("照片.中文扩展名"), "中文扩展名");
        assert_eq!(
            ext("x.😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀"),
            "😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀"
        );
    }

    #[test]
    fn secs_matches_python_float_truncation() {
        assert_eq!(secs(1_700_000_000, 999_999_999), 1_700_000_001);
        assert_eq!(secs(1_700_000_000, 500_000_000), 1_700_000_000);
        assert_eq!(secs(0, 0), 0);
        assert_eq!(secs(-5, 0), 0);
        assert_eq!(secs(1 << 40, 0), u32::MAX);
    }
}
