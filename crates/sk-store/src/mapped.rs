//! Raw column / name-chunk buffers in plain memory or in self-deleting mapped files, and the
//! [`Policy`] deciding when a store moves to mapped files (docs/memory.md §3).
//!
//! Pages of a shared file mapping are backed by their file, not by the page file: they are not
//! private commit (Windows) or anonymous memory (Linux, macOS). The OS keeps the pages in use in
//! RAM and drops cold ones without writing them to swap. A store maps when it grows past
//! [`Policy::large`] (`advanced.map_large_scans`, off by default), or when its columns would push
//! the open stores of its policy past the memory budget (`fraction` of physical RAM).
//!
//! The files clean up after themselves (`tempfile`: delete-on-close on Windows, unlinked right
//! after creation on POSIX). A growing mapped column is extended in place: the file is made
//! longer and mapped again; the old mapping stays valid and shares its pages with the new one, so
//! nothing is copied or written twice. A finished scan keeps its mapped columns as they are
//! (the unused tail was never touched).

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;

/// Alignment of every buffer (any column type, and cache lines).
const ALIGN: usize = 64;
/// Smallest mapped file.
const MIN_MAPPED: usize = 64 << 10;

/// A store whose columns and names reach this maps by itself when `large` is switched on
/// (about two million nodes).
pub const LARGE_BYTES: u64 = 256 << 20;

/// A zero-filled raw buffer that never moves. Shared between generations of a store's columns
/// (`Arc<Buf>`); see the crate docs for who may read and write which part when.
pub(crate) struct Buf {
    ptr: NonNull<u8>,
    bytes: usize,
    backing: Backing,
}

enum Backing {
    Heap(Layout),
    Vec(#[allow(dead_code)] Vec<u8>),
    Mapped {
        _map: memmap2::MmapRaw,
        file: Option<Arc<File>>,
    },
}

// SAFETY: a Buf is plain memory; synchronisation of its contents is the store's job (see the
// crate docs: rows below the published count are only written atomically or exclusively).
unsafe impl Send for Buf {}
unsafe impl Sync for Buf {}

impl Buf {
    /// `bytes` zeroed bytes of plain memory.
    pub(crate) fn heap(bytes: usize) -> Buf {
        let layout = Layout::from_size_align(bytes.max(ALIGN), ALIGN).expect("buffer too large");
        Buf::try_heap(bytes).unwrap_or_else(|| std::alloc::handle_alloc_error(layout))
    }

    /// [`Buf::heap`], or None when there is not that much memory (what a file says it holds is
    /// asked for this way: a refusal is an error, not an abort).
    pub(crate) fn try_heap(bytes: usize) -> Option<Buf> {
        let layout = Layout::from_size_align(bytes.max(ALIGN), ALIGN).ok()?;
        // SAFETY: the layout has a non-zero size.
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(Buf {
            ptr,
            bytes,
            backing: Backing::Heap(layout),
        })
    }

    /// Takes over a vector (a name image built aside). The vector is never touched again.
    pub(crate) fn from_vec(mut v: Vec<u8>) -> Buf {
        if v.is_empty() {
            return Buf::heap(0);
        }
        let ptr = NonNull::new(v.as_mut_ptr()).expect("vec pointer");
        Buf {
            ptr,
            bytes: v.len(),
            backing: Backing::Vec(v),
        }
    }

    /// `bytes` zeroed bytes in a new self-deleting file under `dir`. `growable`: keeps the file
    /// so [`Buf::grow_mapped`] can extend it in place. The file's blocks are reserved first
    /// ([`reserve`]): no room on the disk is an error here (the store then keeps the buffer in
    /// plain memory), never a SIGBUS later.
    pub(crate) fn mapped(bytes: usize, dir: &Path, growable: bool) -> io::Result<Buf> {
        let size = bytes.max(MIN_MAPPED);
        std::fs::create_dir_all(dir)?;
        let file = tempfile::tempfile_in(dir)?;
        reserve(&file, 0, size as u64)?;
        file.set_len(size as u64)?;
        let map = memmap2::MmapOptions::new().len(size).map_raw(&file)?;
        let ptr = NonNull::new(map.as_mut_ptr()).expect("mapping pointer");
        let file = growable.then(|| Arc::new(file));
        Ok(Buf {
            ptr,
            bytes,
            backing: Backing::Mapped { _map: map, file },
        })
    }

    /// The same file, `bytes` long, mapped again: the bytes it holds stay where they are, the
    /// old buffer stays valid and shares its pages. None when this is not a growable mapped
    /// buffer or the file cannot grow (no room on the disk).
    pub(crate) fn grow_mapped(&self, bytes: usize) -> Option<Buf> {
        let Backing::Mapped {
            file: Some(file), ..
        } = &self.backing
        else {
            return None;
        };
        let size = bytes.max(MIN_MAPPED);
        let current = file.metadata().ok()?.len();
        if (size as u64) > current {
            reserve(file, current, size as u64).ok()?;
            file.set_len(size as u64).ok()?;
        }
        let map = memmap2::MmapOptions::new()
            .len(size)
            .map_raw(&**file)
            .ok()?;
        let ptr = NonNull::new(map.as_mut_ptr())?;
        Some(Buf {
            ptr,
            bytes,
            backing: Backing::Mapped {
                _map: map,
                file: Some(file.clone()),
            },
        })
    }

    #[inline]
    pub(crate) fn ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    #[inline]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn is_mapped(&self) -> bool {
        matches!(self.backing, Backing::Mapped { .. })
    }

    /// Bytes of plain memory this buffer holds (0 when mapped).
    pub(crate) fn ram(&self) -> usize {
        if self.is_mapped() { 0 } else { self.bytes }
    }

    /// The identity of a growable mapping's file (tests: a grown column keeps its file).
    #[cfg(test)]
    pub(crate) fn file_id(&self) -> Option<usize> {
        match &self.backing {
            Backing::Mapped { file: Some(f), .. } => Some(Arc::as_ptr(f) as usize),
            _ => None,
        }
    }
}

/// Gives `file` real blocks for its bytes `from..to` before they are mapped. A mapping of a
/// sparse file (what `set_len` makes on ext4, XFS, Btrfs, APFS, tmpfs) has no room behind its
/// pages: the first write to one once the disk is full is SIGBUS, which kills the process (the
/// app, not just a backend). Reserved blocks make "no room" an error up front instead.
///
/// Linux `fallocate`, macOS `F_PREALLOCATE`, other systems `posix_fallocate`. A file system that
/// cannot reserve at all (no `fallocate`: some network and FUSE file systems, older ZFS) is
/// mapped as before. Windows needs nothing: `set_len` gives an NTFS file its clusters (a file is
/// not sparse unless asked to be), and fails when there is no room.
pub(crate) fn reserve(file: &File, from: u64, to: u64) -> io::Result<()> {
    if to <= from {
        return Ok(());
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::fd::AsRawFd;
        let (Ok(at), Ok(len)) = (
            libc::off_t::try_from(from),
            libc::off_t::try_from(to - from),
        ) else {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        };
        // SAFETY: a valid descriptor; fallocate has no other preconditions.
        if unsafe { libc::fallocate(file.as_raw_fd(), 0, at, len) } == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EOPNOTSUPP | libc::ENOSYS) => Ok(()), // cannot reserve: as before
            _ => Err(err),
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let Ok(len) = libc::off_t::try_from(to - from) else {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        };
        // past the blocks the file has: contiguous if it can be, else wherever
        let mut store = libc::fstore_t {
            fst_flags: libc::F_ALLOCATECONTIG | libc::F_ALLOCATEALL,
            fst_posmode: libc::F_PEOFPOSMODE,
            fst_offset: 0,
            fst_length: len,
            fst_bytesalloc: 0,
        };
        // SAFETY: a valid descriptor and a valid fstore_t for F_PREALLOCATE.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &raw mut store) } != -1 {
            return Ok(());
        }
        store.fst_flags = libc::F_ALLOCATEALL;
        // SAFETY: as above.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &raw mut store) } != -1 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::ENOTSUP | libc::EOPNOTSUPP | libc::EINVAL) => Ok(()), // cannot: as before
            _ => Err(err),
        }
    }
    #[cfg(all(
        unix,
        not(any(target_os = "linux", target_os = "android", target_os = "macos"))
    ))]
    {
        use std::os::fd::AsRawFd;
        let (Ok(at), Ok(len)) = (
            libc::off_t::try_from(from),
            libc::off_t::try_from(to - from),
        ) else {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        };
        // SAFETY: a valid descriptor; posix_fallocate returns the error itself.
        match unsafe { libc::posix_fallocate(file.as_raw_fd(), at, len) } {
            0 => Ok(()),
            libc::EOPNOTSUPP | libc::EINVAL => Ok(()), // cannot reserve: as before
            e => Err(io::Error::from_raw_os_error(e)),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(())
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        if let Backing::Heap(layout) = self.backing {
            // SAFETY: allocated in Buf::heap with this layout.
            unsafe { dealloc(self.ptr.as_ptr(), layout) }
        }
    }
}

// --------------------------------------------------------------------------- the policy

/// Something whose plain-memory bytes count against a policy's budget (a store).
pub(crate) trait RamUser: Send + Sync {
    fn ram_bytes(&self) -> usize;
}

struct PolicyCfg {
    fraction: f64,
    large: Option<u64>,
    force_bytes: Option<u64>,
    dir: PathBuf,
    physical: Option<Option<u64>>,
}

/// When a growing or loading store switches to file-backed columns.
///
/// A store of `large` bytes or more (columns with their headroom, and names) maps by itself;
/// `None` (the default) turns that off. `fraction` of physical memory is the budget for the
/// columns and names of all open stores of this policy in plain memory. `force_bytes`
/// (`SPACEKNIFE_MAP_ABOVE` for the global policy) replaces both, for tests and experiments.
pub struct Policy {
    cfg: Mutex<PolicyCfg>,
    stores: Mutex<Vec<Weak<dyn RamUser>>>,
}

impl Default for Policy {
    fn default() -> Self {
        Policy::new()
    }
}

impl Policy {
    /// A policy with the defaults: budget 25% of RAM, `large` off, no forced threshold, files in
    /// `<temp>/spaceknife-columns`.
    pub fn new() -> Policy {
        Policy {
            cfg: Mutex::new(PolicyCfg {
                fraction: 0.25,
                large: None,
                force_bytes: None,
                dir: std::env::temp_dir().join("spaceknife-columns"),
                physical: None,
            }),
            stores: Mutex::new(Vec::new()),
        }
    }

    /// The process-wide policy every store uses unless given another one. Reads
    /// `SPACEKNIFE_MAP_ABOVE` (bytes) once.
    pub fn global() -> &'static Arc<Policy> {
        static GLOBAL: OnceLock<Arc<Policy>> = OnceLock::new();
        GLOBAL.get_or_init(|| {
            let p = Policy::new();
            if let Some(v) = std::env::var("SPACEKNIFE_MAP_ABOVE")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                p.set_force_bytes(Some(v));
            }
            Arc::new(p)
        })
    }

    pub fn set_fraction(&self, fraction: f64) {
        self.cfg.lock().fraction = fraction;
    }
    pub fn fraction(&self) -> f64 {
        self.cfg.lock().fraction
    }
    /// `Some(LARGE_BYTES)` with `advanced.map_large_scans`.
    pub fn set_large(&self, large: Option<u64>) {
        self.cfg.lock().large = large;
    }
    pub fn large(&self) -> Option<u64> {
        self.cfg.lock().large
    }
    pub fn set_force_bytes(&self, bytes: Option<u64>) {
        self.cfg.lock().force_bytes = bytes;
    }
    pub fn force_bytes(&self) -> Option<u64> {
        self.cfg.lock().force_bytes
    }
    /// Where mapped files go (`<cache dir>/columns` in the app).
    pub fn set_columns_dir(&self, dir: PathBuf) {
        self.cfg.lock().dir = dir;
    }
    pub fn columns_dir(&self) -> PathBuf {
        self.cfg.lock().dir.clone()
    }
    /// Replace the detected physical memory (`Some(None)`: unknown); `None` detects again.
    pub fn set_physical_memory(&self, bytes: Option<Option<u64>>) {
        self.cfg.lock().physical = bytes;
    }

    pub(crate) fn register(&self, store: Weak<dyn RamUser>) {
        let mut stores = self.stores.lock();
        stores.retain(|w| w.strong_count() > 0);
        stores.push(store);
    }

    /// Bytes all stores of this policy may hold in plain memory (None: no limit known).
    pub fn budget(&self) -> Option<u64> {
        let cfg = self.cfg.lock();
        if let Some(force) = cfg.force_bytes {
            return Some(force);
        }
        let phys = match cfg.physical {
            Some(p) => p,
            None => physical_memory(),
        };
        phys.map(|p| (p as f64 * cfg.fraction) as u64)
    }

    /// Whether a store (`own`: its address, excluded from the sum) that is about to hold
    /// `new_bytes` in columns and names should map them.
    pub(crate) fn should_map(&self, own: *const (), new_bytes: u64) -> bool {
        {
            let cfg = self.cfg.lock();
            if cfg.force_bytes.is_none() && cfg.large.is_some_and(|l| new_bytes >= l) {
                return true;
            }
        }
        let Some(budget) = self.budget() else {
            return false;
        };
        let others: Vec<Arc<dyn RamUser>> = {
            let stores = self.stores.lock();
            stores
                .iter()
                .filter_map(|w| w.upgrade())
                .filter(|s| Arc::as_ptr(s) as *const () != own)
                .collect()
        };
        let held: u64 = others.iter().map(|s| s.ram_bytes() as u64).sum();
        held + new_bytes > budget
    }

    /// Public form of the decision for a store that does not exist yet.
    pub fn should_map_new(&self, new_bytes: u64) -> bool {
        self.should_map(std::ptr::null(), new_bytes)
    }

    /// Remove column files left by processes that are gone (files in use cannot be deleted).
    pub fn cleanup_stale(&self) -> usize {
        let dir = self.columns_dir();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|e| std::fs::remove_file(e.path()).is_ok())
            .count()
    }
}

/// Physical memory of this machine in bytes (None: unknown).
pub fn physical_memory() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        // SAFETY: a zeroed MEMORYSTATUSEX with its length set is what the call expects.
        unsafe {
            let mut st: MEMORYSTATUSEX = std::mem::zeroed();
            st.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut st) != 0 {
                return Some(st.ullTotalPhys);
            }
        }
        None
    }
    #[cfg(unix)]
    {
        // SAFETY: sysconf has no preconditions.
        let (pages, size) = unsafe {
            (
                libc::sysconf(libc::_SC_PHYS_PAGES),
                libc::sysconf(libc::_SC_PAGESIZE),
            )
        };
        if pages > 0 && size > 0 {
            return Some(pages as u64 * size as u64);
        }
        None
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapped_buffers_grow_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let a = Buf::mapped(8000, dir.path(), true).unwrap();
        // SAFETY: fresh buffer, 8000 bytes.
        unsafe {
            for i in 0..1000u64 {
                (a.ptr() as *mut u64).add(i as usize).write(i);
            }
        }
        let b = a.grow_mapped(400_000).unwrap();
        assert_eq!(a.file_id(), b.file_id());
        // SAFETY: both map the same file; reads within their lengths.
        unsafe {
            assert_eq!((b.ptr() as *const u64).add(999).read(), 999);
            assert_eq!((b.ptr() as *const u64).add(40_000).read(), 0);
            (b.ptr() as *mut u64).add(3).write(77);
            assert_eq!((a.ptr() as *const u64).add(3).read(), 77); // the same pages
        }
        assert!(Buf::heap(10).grow_mapped(20).is_none());
        assert!(
            Buf::mapped(10, dir.path(), false)
                .unwrap()
                .grow_mapped(20)
                .is_none()
        );
        drop((a, b));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0); // self-deleting
    }

    /// A mapped buffer's file has its blocks before it is mapped (and after it grows): a write
    /// through the mapping can never be the first to find the disk full (review issue 21).
    #[cfg(unix)]
    #[test]
    fn mapped_files_have_their_blocks() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let blocks_of = |b: &Buf| -> u64 {
            let Backing::Mapped { file: Some(f), .. } = &b.backing else {
                panic!("a growable mapping")
            };
            f.metadata().unwrap().blocks() * 512
        };
        let a = Buf::mapped(1 << 20, dir.path(), true).unwrap();
        assert!(blocks_of(&a) >= 1 << 20, "{} bytes reserved", blocks_of(&a));
        let b = a.grow_mapped(3 << 20).unwrap();
        assert!(blocks_of(&b) >= 3 << 20, "{} bytes reserved", blocks_of(&b));
    }

    /// With `SK_TEST_SMALL_FS` naming a folder on a small file system (a tmpfs of a few MiB: `mount
    /// -t tmpfs -o size=4m tmpfs <dir>`), a mapped buffer larger than its room is refused up front:
    /// it used to be a sparse file whose first page past the room killed the process with SIGBUS
    /// (review issue 21). A buffer that fits is still mapped; growing past the room is refused.
    #[test]
    fn no_room_for_a_mapped_file_is_an_error_not_a_crash() {
        let Some(dir) = std::env::var_os("SK_TEST_SMALL_FS") else {
            return;
        };
        let dir = Path::new(&dir);
        assert!(Buf::mapped(64 << 20, dir, false).is_err());
        let a = Buf::mapped(1 << 20, dir, true).unwrap();
        assert!(a.grow_mapped(64 << 20).is_none());
        drop(a);
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
    }

    #[test]
    fn policy_maps_beyond_the_budget() {
        let p = Policy::new();
        p.set_physical_memory(Some(Some(1000)));
        p.set_fraction(0.5);
        assert!(!p.should_map_new(400));
        assert!(p.should_map_new(600));
        p.set_physical_memory(Some(None));
        assert!(!p.should_map_new(1 << 40)); // unknown RAM: stay in plain memory
    }

    #[test]
    fn policy_maps_large_stores_by_themselves() {
        let p = Policy::new();
        p.set_physical_memory(Some(Some(1 << 40)));
        assert!(p.large().is_none());
        assert!(!p.should_map_new(4 * LARGE_BYTES));
        p.set_large(Some(LARGE_BYTES));
        assert!(!p.should_map_new(LARGE_BYTES - 1));
        assert!(p.should_map_new(LARGE_BYTES));
        p.set_physical_memory(Some(None));
        assert!(p.should_map_new(LARGE_BYTES));
        p.set_force_bytes(Some(1 << 50)); // replaces both rules
        assert!(!p.should_map_new(4 * LARGE_BYTES));
    }

    #[test]
    fn physical_memory_is_known_here() {
        assert!(physical_memory().unwrap_or(0) > 1 << 20);
    }
}
