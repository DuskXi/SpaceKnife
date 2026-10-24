//! S1b: block maps on volumes that can share data blocks (docs/dupes.md §2.3). Metadata only.
//!
//! Each file's map is folded into a 16-byte BLAKE3 of its (logical, physical, length) runs. Two
//! files whose complete maps are equal share every data block: "the same data" (a block clone or
//! reflink), which says something about the storage, not a content check, so it never makes a
//! file deletable. Partly shared extents are only counted (`shared`, Linux FIEMAP_EXTENT_SHARED).
//!
//! - Linux: FS_IOC_FIEMAP without FIEMAP_FLAG_SYNC (btrfs, xfs, bcachefs, ocfs2).
//! - Windows: FSCTL_GET_RETRIEVAL_POINTERS on an attribute-only handle. A run at LCN −1 (a hole, or
//!   data ReFS has not placed yet: a file written a moment ago) and runs that stop before the end of
//!   the file (WinBtrfs answers that way) make the map incomplete: it proves nothing then.
//! - macOS: not implemented yet (APFS clone ids need getattrlist, untested): UNSUPPORTED.

use super::codes;

#[derive(Clone, Debug, Default)]
pub struct ExtMap {
    pub reason: u8,
    pub hash: [u8; 16],
    pub shared: i64,
    pub complete: bool,
    pub extents: u32,
}

/// Folds runs (logical, physical, length) into the map digest.
#[derive(Default)]
pub struct Folder {
    h: blake3::Hasher,
    pub n: u32,
}

impl Folder {
    pub fn push(&mut self, logical: u64, physical: i64, length: u64) {
        self.h.update(&logical.to_le_bytes());
        self.h.update(&physical.to_le_bytes());
        self.h.update(&length.to_le_bytes());
        self.n += 1;
    }

    pub fn digest(&self) -> [u8; 16] {
        let d = self.h.finalize();
        d.as_bytes()[..16].try_into().unwrap()
    }
}

/// Folds one FSCTL_GET_RETRIEVAL_POINTERS answer — the starting VCN, then (next VCN, LCN) pairs —
/// into ``folder``; returns the VCN after the last run. An LCN below zero (−1) is no place on the
/// disk: a hole of a sparse or compressed file, or data not written out yet (ReFS allocates late,
/// so a file written a moment ago maps as all −1). Two such maps being equal proves nothing about
/// the contents, so ``allocated`` turns false and the map is not complete.
pub fn fold_retrieval(start: i64, pairs: &[u64], folder: &mut Folder, allocated: &mut bool) -> i64 {
    let mut prev = start;
    for p in pairs.chunks_exact(2) {
        let next = p[0] as i64;
        let lcn = p[1] as i64;
        if lcn < 0 {
            *allocated = false;
        }
        folder.push(prev as u64, lcn, (next - prev) as u64);
        prev = next;
    }
    prev
}

/// Whether two maps prove that two files share all of their data.
pub fn same_data(a: &ExtMap, b: &ExtMap) -> bool {
    a.reason == codes::OK && b.reason == codes::OK && a.complete && b.complete && a.extents > 0 && a.hash == b.hash
}

#[cfg(target_os = "linux")]
pub fn extents(path: &str) -> ExtMap {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    const FS_IOC_FIEMAP: libc::c_ulong = 0xC020_660B;
    const FLAG_LAST: u32 = 0x1;
    const FLAG_UNKNOWN: u32 = 0x2;
    const FLAG_DELALLOC: u32 = 0x4;
    const FLAG_ENCODED: u32 = 0x8;
    const FLAG_ENCRYPTED: u32 = 0x80;
    const FLAG_NOT_ALIGNED: u32 = 0x100;
    const FLAG_INLINE: u32 = 0x200;
    const FLAG_TAIL: u32 = 0x400;
    const FLAG_UNWRITTEN: u32 = 0x800;
    const FLAG_SHARED: u32 = 0x2000;
    const BAD: u32 = FLAG_UNKNOWN
        | FLAG_DELALLOC
        | FLAG_ENCODED
        | FLAG_ENCRYPTED
        | FLAG_NOT_ALIGNED
        | FLAG_INLINE
        | FLAG_TAIL
        | FLAG_UNWRITTEN;
    const N: usize = 256;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Extent {
        logical: u64,
        physical: u64,
        length: u64,
        reserved64: [u64; 2],
        flags: u32,
        reserved: [u32; 3],
    }
    #[repr(C)]
    struct Fiemap {
        start: u64,
        length: u64,
        flags: u32,
        mapped: u32,
        count: u32,
        reserved: u32,
        extents: [Extent; N],
    }

    let mut out = ExtMap::default();
    let open = |flags: i32| {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(flags | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
    };
    // opening never updates the access time; O_NOATIME only when allowed
    let f = match open(libc::O_NOATIME).or_else(|_| open(0)) {
        Ok(f) => f,
        Err(e) => {
            out.reason = super::open::io_reason(&e);
            return out;
        }
    };
    let mut fm: Box<Fiemap> = Box::new(Fiemap {
        start: 0,
        length: u64::MAX,
        flags: 0,
        mapped: 0,
        count: N as u32,
        reserved: 0,
        extents: [Extent::default(); N],
    });
    let mut folder = Folder::default();
    let mut complete = true;
    let mut last = false;
    let mut start = 0u64;
    for _round in 0..4096 {
        fm.start = start;
        fm.length = u64::MAX - start;
        fm.flags = 0;
        fm.mapped = 0;
        fm.count = N as u32;
        // SAFETY: `fm` is a valid fiemap with room for `count` extents.
        let rc = unsafe { libc::ioctl(f.as_raw_fd(), FS_IOC_FIEMAP, &mut *fm as *mut Fiemap) };
        if rc != 0 {
            out.reason = codes::UNSUPPORTED;
            return out;
        }
        let mapped = fm.mapped as usize;
        if mapped == 0 {
            last = true;
            break;
        }
        for e in &fm.extents[..mapped.min(N)] {
            if e.flags & BAD != 0 || e.physical == 0 {
                complete = false;
            }
            if e.flags & FLAG_SHARED != 0 {
                out.shared = out.shared.saturating_add(e.length as i64);
            }
            folder.push(e.logical, e.physical as i64, e.length);
            start = e.logical.saturating_add(e.length);
            if e.flags & FLAG_LAST != 0 {
                last = true;
            }
        }
        if last {
            break;
        }
    }
    out.reason = codes::OK;
    out.complete = complete && last;
    out.extents = folder.n;
    out.hash = folder.digest();
    out
}

#[cfg(windows)]
pub fn extents(path: &str) -> ExtMap {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_NO_RECALL, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, SYNCHRONIZE,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{FSCTL_GET_RETRIEVAL_POINTERS, STARTING_VCN_INPUT_BUFFER};

    const ERROR_MORE_DATA: i32 = 234;
    const ERROR_HANDLE_EOF: i32 = 38;

    let mut out = ExtMap::default();
    let f = match std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | SYNCHRONIZE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_NO_RECALL)
        .open(super::open::verbatim(path))
    {
        Ok(f) => f,
        Err(e) => {
            out.reason = super::open::io_reason(&e);
            return out;
        }
    };
    let h = f.as_raw_handle() as HANDLE;
    let mut folder = Folder::default();
    let mut allocated = true;
    let mut end: i64 = 0;
    let mut vcn: i64 = 0;
    let mut buf = vec![0u64; 8192]; // 64 KiB: ExtentCount, StartingVcn, then (NextVcn, Lcn) pairs
    for _round in 0..4096 {
        let input = STARTING_VCN_INPUT_BUFFER { StartingVcn: vcn };
        let mut got = 0u32;
        // SAFETY: buffers valid for the sizes given.
        let ok = unsafe {
            DeviceIoControl(
                h,
                FSCTL_GET_RETRIEVAL_POINTERS,
                (&raw const input).cast(),
                size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        let err = if ok == 0 {
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
        } else {
            0
        };
        if ok == 0 && err != ERROR_MORE_DATA {
            out.reason = if err == ERROR_HANDLE_EOF {
                codes::OK // resident or empty: no blocks to share
            } else {
                codes::UNSUPPORTED
            };
            out.complete = false;
            return out;
        }
        let count = (buf[0] & 0xFFFF_FFFF) as usize;
        let pairs = &buf[2..2 + 2 * count.min((buf.len() - 2) / 2)];
        let prev = fold_retrieval(buf[1] as i64, pairs, &mut folder, &mut allocated);
        end = end.max(prev);
        if ok != 0 {
            break;
        }
        if prev <= vcn {
            break;
        }
        vcn = prev;
    }
    // the runs must reach the end of the file: WinBtrfs answers "that was all" well before it
    if allocated {
        let size = f.metadata().map(|m| m.len()).ok();
        match (size, cluster_size(path)) {
            (Some(n), Some(c)) if (end.max(0) as u64).saturating_mul(c) >= n => {}
            _ => allocated = false,
        }
    }
    out.reason = codes::OK;
    out.complete = allocated && folder.n > 0;
    out.extents = folder.n;
    out.hash = folder.digest();
    out
}

/// Bytes per cluster of the volume holding ``path`` (Windows).
#[cfg(windows)]
fn cluster_size(path: &str) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceW, GetVolumePathNameW};

    let wide: Vec<u16> = super::open::verbatim(path).encode_wide().chain(Some(0)).collect();
    let mut root = vec![0u16; 1024];
    // SAFETY: both buffers are valid, NUL-terminated / sized as given.
    if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return None;
    }
    let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: `root` is NUL-terminated; the out pointers are valid.
    if unsafe { GetDiskFreeSpaceW(root.as_ptr(), &mut spc, &mut bps, &mut free, &mut total) } == 0 {
        return None;
    }
    let c = u64::from(spc) * u64::from(bps);
    (c > 0).then_some(c)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn extents(_path: &str) -> ExtMap {
    ExtMap {
        reason: codes::UNSUPPORTED,
        ..ExtMap::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(runs: &[(u64, i64, u64)], complete: bool) -> ExtMap {
        let mut f = Folder::default();
        for &(l, p, n) in runs {
            f.push(l, p, n);
        }
        ExtMap {
            reason: codes::OK,
            hash: f.digest(),
            shared: 0,
            complete,
            extents: f.n,
        }
    }

    #[test]
    fn equal_maps_are_the_same_data() {
        let a = map(&[(0, 100, 4096), (4096, 900, 8192)], true);
        let b = map(&[(0, 100, 4096), (4096, 900, 8192)], true);
        assert!(same_data(&a, &b));
    }

    #[test]
    fn unallocated_runs_never_make_a_complete_map() {
        // two different files written a moment ago on ReFS: the same runs, every one at LCN −1
        let fresh: Vec<u64> = vec![1, u64::MAX, 2048, u64::MAX, 4353, u64::MAX];
        let fold = |pairs: &[u64]| {
            let mut f = Folder::default();
            let mut allocated = true;
            let end = fold_retrieval(0, pairs, &mut f, &mut allocated);
            let m = ExtMap {
                reason: codes::OK,
                hash: f.digest(),
                shared: 0,
                complete: allocated && f.n > 0,
                extents: f.n,
            };
            (m, end)
        };
        let (a, end) = fold(&fresh);
        let (b, _) = fold(&fresh);
        assert_eq!(end, 4353);
        assert!(!a.complete && a.hash == b.hash);
        assert!(!same_data(&a, &b), "data not on the disk yet proves nothing");
        // allocated runs map completely; a hole in the middle does not
        let (c, _) = fold(&[16, 1000, 32, 5000]);
        assert!(c.complete && c.extents == 2);
        let (d, _) = fold(&[16, 1000, 32, u64::MAX, 48, 2000]);
        assert!(!d.complete);
    }

    #[test]
    fn different_or_incomplete_maps_are_not() {
        let a = map(&[(0, 100, 4096)], true);
        assert!(!same_data(&a, &map(&[(0, 101, 4096)], true)));
        assert!(!same_data(&a, &map(&[(0, 100, 8192)], true)));
        assert!(!same_data(&a, &map(&[(0, 100, 4096)], false)));
        assert!(!same_data(&map(&[], true), &map(&[], true)));
        let mut gone = map(&[(0, 100, 4096)], true);
        gone.reason = codes::GONE;
        assert!(!same_data(&gone, &a));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_file_maps_like_itself_and_unlike_a_copy() {
        let dir = std::env::temp_dir().join(format!("sk-share-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.bin");
        let b = dir.join("b.bin");
        let data = vec![7u8; 256 * 1024];
        std::fs::write(&a, &data).unwrap();
        std::fs::write(&b, &data).unwrap();
        let ma = extents(a.to_str().unwrap());
        let ma2 = extents(a.to_str().unwrap());
        let mb = extents(b.to_str().unwrap());
        if ma.reason == codes::OK && ma.complete {
            assert_eq!(ma.hash, ma2.hash);
            assert!(!same_data(&ma, &mb), "a plain copy has its own blocks");
        }
        let gone = extents(dir.join("missing").to_str().unwrap());
        assert_eq!(gone.reason, codes::GONE);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
