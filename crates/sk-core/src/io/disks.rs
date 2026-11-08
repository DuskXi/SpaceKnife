//! Volume → physical disk mapping (UI-W2-36: load is measured and limited per physical disk;
//! Python `io/disks.py`).
//!
//! Read-only probes that need no elevation:
//!
//! * Windows: `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` on the volume device opened with access 0
//!   (falls back to `IOCTL_STORAGE_GET_DEVICE_NUMBER`); the disk is `PhysicalDrive<N>`.
//! * Linux: `/sys/dev/block/<maj>:<min>` → whole disk (partitions resolved to their parent, device
//!   mapper / LVM / LUKS / md to their first backing device via `slaves/`).
//! * macOS: `diskutil info -plist` (APFS physical store → parent whole disk), best effort.
//!
//! A [`DiskId`] is a stable string: `PhysicalDrive3` · `nvme0n1` · `disk0` for physical disks,
//! `net:<remote>` for network volumes and `vol:<mount point>` when the disk cannot be resolved
//! (both are governed open loop: there is nothing to sample).

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::media::Media;

pub type DiskId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiskKind {
    Physical,
    Network,
    Volume,
}

/// A physical disk (or an unsampled stand-in) and what is known about it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DiskRef {
    pub id: DiskId,
    pub kind: DiskKind,
    /// Windows disk number (`\\.\PhysicalDriveN`, PDH instance "N C:").
    pub number: Option<u32>,
    /// Kernel / BSD name on Linux and macOS (sysfs: `/sys/block/<name>`).
    pub name: String,
    pub media: Media,
    /// One volume on the disk, for display.
    pub mount: String,
}

impl DiskRef {
    pub fn new(id: impl Into<String>, kind: DiskKind) -> Self {
        Self {
            id: id.into(),
            kind,
            number: None,
            name: String::new(),
            media: Media::Unknown,
            mount: String::new(),
        }
    }

    /// A physical disk.
    pub fn physical(id: impl Into<String>) -> Self {
        Self::new(id, DiskKind::Physical)
    }

    pub fn with_media(mut self, media: Media) -> Self {
        self.media = media;
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_mount(mut self, mount: impl Into<String>) -> Self {
        self.mount = mount.into();
        self
    }

    fn network(remote: &str, mount: &str) -> Self {
        Self::new(format!("net:{remote}"), DiskKind::Network)
            .with_media(Media::Network)
            .with_mount(mount)
    }

    fn volume(mount: &str) -> Self {
        Self::new(format!("vol:{mount}"), DiskKind::Volume).with_mount(mount)
    }

    pub fn sampled(&self) -> bool {
        self.kind == DiskKind::Physical
    }
}

pub fn win_disk_id(number: u32) -> DiskId {
    format!("PhysicalDrive{number}")
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

// --------------------------------------------------------------------------- Windows

#[cfg(windows)]
mod win {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetLogicalDrives, GetVolumePathNameW,
    };

    use super::*;
    use crate::disks::windows::{IOCTL_STORAGE_GET_DEVICE_NUMBER, device_extents, volume_name};
    use crate::helper::win32::{Handle, from_wide, wide};
    use crate::media::media_of;

    /// Disk numbers behind a volume device (`\\.\C:` or `\\?\Volume{…}`).
    fn disk_numbers(device: &str) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for (n, _, _) in device_extents(device) {
            if !out.contains(&n) {
                out.push(n);
            }
        }
        if !out.is_empty() {
            return out;
        }
        let Some(h) = Handle::open(device, 0, 3, 0) else {
            return out;
        };
        if let Ok(raw) = h.ioctl(IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], 12)
            && raw.len() >= 12
            && u32::from_le_bytes(raw[0..4].try_into().expect("4 bytes")) == 7
        {
            // FILE_DEVICE_DISK
            out.push(u32::from_le_bytes(raw[4..8].try_into().expect("4 bytes")));
        }
        out
    }

    pub(super) fn root(path: &Path) -> Option<String> {
        let w = wide(absolute(path));
        let mut buf = vec![0u16; 1024];
        // SAFETY: buffer of the given length.
        if unsafe { GetVolumePathNameW(w.as_ptr(), buf.as_mut_ptr(), 1024) } == 0 {
            return None;
        }
        let r = from_wide(&buf);
        Some(if r.ends_with('\\') {
            r
        } else {
            format!("{r}\\")
        })
    }

    fn remote(root: &str) -> String {
        let local = wide(root.trim_end_matches('\\'));
        let mut buf = vec![0u16; 1024];
        let mut size = 1024u32;
        // SAFETY: buffer of the given length.
        if unsafe { WNetGetConnectionW(local.as_ptr(), buf.as_mut_ptr(), &mut size) } == 0 {
            return from_wide(&buf);
        }
        root.trim_end_matches('\\').to_owned()
    }

    fn drive_type(root: &str) -> u32 {
        let w = wide(root);
        // SAFETY: a valid string.
        unsafe { GetDriveTypeW(w.as_ptr()) }
    }

    /// The disk under a volume root (`C:\`, `\\server\share\`, a mount folder); cached.
    pub fn ref_for_root(root: &str) -> DiskRef {
        static CACHE: LazyLock<Mutex<HashMap<String, DiskRef>>> = LazyLock::new(Default::default);
        if let Some(r) = CACHE.lock().expect("cache").get(root) {
            return r.clone();
        }
        let r = compute(root);
        let mut cache = CACHE.lock().expect("cache");
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(root.to_owned(), r.clone());
        r
    }

    fn compute(root: &str) -> DiskRef {
        if root.starts_with("\\\\") && !root.starts_with("\\\\?\\") {
            return DiskRef::network(root.trim_end_matches('\\'), root);
        }
        if drive_type(root) == 4 {
            // DRIVE_REMOTE
            return DiskRef::network(&remote(root), root);
        }
        let device =
            volume_name(root).unwrap_or_else(|| format!("\\\\.\\{}", &root[..2.min(root.len())]));
        let numbers = disk_numbers(&device);
        let Some(&number) = numbers.first() else {
            return DiskRef::volume(root);
        };
        let media = if root.len() == 3 {
            media_of(root, root.trim_end_matches('\\'), false)
        } else {
            Media::Unknown
        };
        DiskRef {
            number: Some(number),
            media,
            mount: root.to_owned(),
            ..DiskRef::physical(win_disk_id(number))
        }
    }

    pub fn local() -> Vec<DiskRef> {
        // SAFETY: no preconditions.
        let mask = unsafe { GetLogicalDrives() };
        let mut seen: Vec<DiskRef> = Vec::new();
        for i in 0..26 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let root = format!("{}:\\", (b'A' + i as u8) as char);
            if !matches!(drive_type(&root), 2 | 3) {
                continue; // removable, fixed
            }
            let r = ref_for_root(&root);
            if r.sampled() && !seen.iter().any(|s| s.id == r.id) {
                seen.push(r);
            }
        }
        seen
    }
}

#[cfg(windows)]
pub use win::ref_for_root as win_ref_for_root;

// --------------------------------------------------------------------------- Linux

/// Kernel name of the whole disk behind a sysfs block device directory (`root`: the sysfs mount).
pub fn linux_whole_disk(sys_path: &Path, root: &Path) -> Option<String> {
    let mut path = std::fs::canonicalize(sys_path).ok()?;
    for _ in 0..8 {
        // dm on dm on md …: bounded
        if !path.is_dir() {
            return None;
        }
        let slaves = path.join("slaves");
        let mut inner: Vec<String> = std::fs::read_dir(&slaves)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        inner.sort();
        if let Some(first) = inner.first() {
            path = std::fs::canonicalize(root.join("class").join("block").join(first)).ok()?;
            continue;
        }
        if path.join("partition").exists() {
            path = path.parent()?.to_path_buf();
        }
        return path.file_name().map(|n| n.to_string_lossy().into_owned());
    }
    None
}

/// Whole disks worth sampling: `/sys/block` minus virtual devices (loop, ram, zram, dm, md …).
pub fn linux_physical_disks(root: &Path) -> Vec<String> {
    let base = root.join("block");
    let mut names: Vec<String> = std::fs::read_dir(&base)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
        .into_iter()
        .filter(|n| {
            let real = std::fs::canonicalize(base.join(n)).unwrap_or_else(|_| base.join(n));
            !real
                .to_string_lossy()
                .replace('\\', "/")
                .contains("/devices/virtual/")
        })
        .collect()
}

/// mountinfo escapes the source like `C:\134` for `C:\`.
pub fn unescape_octal(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push(((b[i + 1] - b'0') << 6) | ((b[i + 2] - b'0') << 3) | (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(all(unix, not(target_os = "macos")))]
mod linux {
    use std::os::unix::fs::MetadataExt;

    use super::*;
    use crate::media::media_of;

    const SYS: &str = "/sys";

    pub fn disk_ref(path: &Path) -> std::io::Result<DiskRef> {
        let dev = std::fs::metadata(path)?.dev();
        let (major, minor) = (libc::major(dev), libc::minor(dev));
        let mut name = if major != 0 {
            linux_whole_disk(
                &Path::new(SYS)
                    .join("dev/block")
                    .join(format!("{major}:{minor}")),
                Path::new(SYS),
            )
        } else {
            None
        };
        let mut vol = None;
        if name.is_none() {
            // btrfs / overlay / network: anonymous st_dev, go through the mount source
            let v = crate::volume::volume_for(path)?;
            if v.is_remote {
                let remote = if v.remote_path.is_empty() {
                    &v.mount_point
                } else {
                    &v.remote_path
                };
                return Ok(DiskRef::network(&unescape_octal(remote), ""));
            }
            if v.device.starts_with("/dev/") {
                let real =
                    std::fs::canonicalize(&v.device).unwrap_or_else(|_| PathBuf::from(&v.device));
                let dev = real
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                name = linux_whole_disk(
                    &Path::new(SYS).join("class/block").join(dev),
                    Path::new(SYS),
                );
            }
            vol = Some(v);
        }
        let Some(name) = name else {
            let v = match vol {
                Some(v) => v,
                None => crate::volume::volume_for(path)?,
            };
            return Ok(DiskRef::volume(&v.mount_point));
        };
        Ok(DiskRef::physical(name.clone())
            .with_media(media_of("", &format!("/dev/{name}"), false))
            .with_name(name))
    }

    pub fn local() -> Vec<DiskRef> {
        linux_physical_disks(Path::new(SYS))
            .into_iter()
            .map(|n| {
                DiskRef::physical(n.clone())
                    .with_media(media_of("", &format!("/dev/{n}"), false))
                    .with_name(n)
            })
            .collect()
    }
}

// --------------------------------------------------------------------------- macOS

/// BSD name of the physical whole disk from `diskutil info -plist` (APFS: via the physical store).
pub fn darwin_whole_disk(info: &Value, lookup: &dyn Fn(&str) -> Value) -> Option<String> {
    let bsd = |s: &str| {
        let digits = s.strip_prefix("disk")?;
        let n = digits.bytes().take_while(u8::is_ascii_digit).count();
        (n > 0).then(|| format!("disk{}", &digits[..n]))
    };
    if let Some(store) = info
        .get("APFSPhysicalStores")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|s| s.get("APFSPhysicalStore"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        if let Some(parent) = lookup(store)
            .get("ParentWholeDisk")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return Some(parent.to_owned());
        }
        if let Some(d) = bsd(store) {
            return Some(d);
        }
    }
    if let Some(parent) = info
        .get("ParentWholeDisk")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Some(parent.to_owned());
    }
    bsd(info
        .get("DeviceIdentifier")
        .and_then(Value::as_str)
        .unwrap_or(""))
}

#[cfg(target_os = "macos")]
mod darwin {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    use super::*;
    use crate::disks::darwin::run_plist;
    use crate::media::media_of;

    fn diskutil(target: &str) -> Value {
        run_plist("diskutil", &["info", "-plist", target]).unwrap_or(Value::Null)
    }

    pub fn ref_for_mount(mount: &str, remote: bool, remote_path: &str) -> DiskRef {
        type Cache = HashMap<(String, bool, String), DiskRef>;
        static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);
        let key = (mount.to_owned(), remote, remote_path.to_owned());
        if let Some(r) = CACHE.lock().expect("cache").get(&key) {
            return r.clone();
        }
        let r = if remote {
            DiskRef::network(
                if remote_path.is_empty() {
                    mount
                } else {
                    remote_path
                },
                mount,
            )
        } else {
            match darwin_whole_disk(&diskutil(mount), &diskutil) {
                None => DiskRef::volume(mount),
                Some(name) => DiskRef::physical(name.clone())
                    .with_name(name)
                    .with_media(media_of(mount, "", false))
                    .with_mount(mount),
            }
        };
        let mut cache = CACHE.lock().expect("cache");
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(key, r.clone());
        r
    }

    pub fn disk_ref(path: &Path) -> std::io::Result<DiskRef> {
        let v = crate::volume::volume_for(path)?;
        Ok(ref_for_mount(&v.mount_point, v.is_remote, &v.remote_path))
    }

    pub fn local() -> Vec<DiskRef> {
        let mut seen: Vec<DiskRef> = Vec::new();
        for v in crate::volume::list_volumes() {
            if v.is_remote {
                continue;
            }
            let r = ref_for_mount(&v.mount_point, false, "");
            if r.sampled() && !seen.iter().any(|s| s.id == r.id) {
                seen.push(r);
            }
        }
        seen
    }
}

// --------------------------------------------------------------------------- public

/// The physical disk holding `path`; never fails (falls back to a `vol:` stand-in).
pub fn disk_ref_for_path(path: &Path) -> DiskRef {
    let fallback = || DiskRef::volume(&absolute(path).to_string_lossy());
    let run = || -> Option<DiskRef> {
        #[cfg(windows)]
        {
            // the deepest existing ancestor names the volume
            let mut p = absolute(path);
            loop {
                if p.exists() {
                    break;
                }
                if !p.pop() {
                    break;
                }
            }
            win::root(&p).map(|r| win::ref_for_root(&r))
        }
        #[cfg(target_os = "macos")]
        {
            darwin::disk_ref(&existing(path)).ok()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            linux::disk_ref(&existing(path)).ok()
        }
    };
    // a probe must never break a scan
    std::panic::catch_unwind(run)
        .ok()
        .flatten()
        .unwrap_or_else(fallback)
}

#[cfg(unix)]
fn existing(path: &Path) -> PathBuf {
    let mut p = absolute(path);
    while !p.exists() && p.pop() {}
    p
}

pub fn disk_for_path(path: &Path) -> DiskId {
    disk_ref_for_path(path).id
}

/// Local physical disks (sampled from backend start so W2 has a pre-scan baseline).
pub fn local_disks() -> Vec<DiskRef> {
    let run = || {
        #[cfg(windows)]
        {
            win::local()
        }
        #[cfg(target_os = "macos")]
        {
            darwin::local()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            linux::local()
        }
    };
    std::panic::catch_unwind(run).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn this_checkout_maps_to_a_physical_disk() {
        let here = std::env::current_dir().unwrap();
        let r = disk_ref_for_path(&here);
        if cfg!(windows) {
            assert!(
                r.id.starts_with("PhysicalDrive") && r.number.is_some(),
                "{r:?}"
            );
            assert_eq!(r.id, format!("PhysicalDrive{}", r.number.unwrap()));
            let system = std::env::var("SYSTEMDRIVE").unwrap_or_else(|_| "C:".into());
            assert!(disk_for_path(Path::new(&format!("{system}\\"))).starts_with("PhysicalDrive"));
        }
        if r.kind == DiskKind::Volume && !cfg!(windows) {
            return; // no block device behind the checkout (overlay / tmpfs)
        }
        assert!(r.sampled());
        assert!(
            cfg!(target_os = "macos") || local_disks().iter().any(|d| d.id == r.id),
            "{r:?}"
        );
    }

    #[test]
    fn mapping_never_fails() {
        let r = disk_ref_for_path(
            &std::env::current_dir()
                .unwrap()
                .join("no")
                .join("such")
                .join("dir"),
        );
        assert!(!r.id.is_empty()); // resolved through the existing part of the path, or a vol: stand-in
    }

    #[cfg(windows)]
    #[test]
    fn unc_roots_are_network_disks() {
        let r = win_ref_for_root("\\\\nas\\share\\"); // decided from the path alone, no network access
        assert!(
            r.kind == DiskKind::Network && r.id == "net:\\\\nas\\share" && !r.sampled(),
            "{r:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn linux_whole_disk_resolves_partitions_and_device_mapper() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let sysfs = tmp.path().join("sys");
        let nvme = sysfs.join("devices/pci0000:00/nvme/nvme0/nvme0n1");
        std::fs::create_dir_all(nvme.join("nvme0n1p2")).unwrap();
        std::fs::write(nvme.join("nvme0n1p2/partition"), "2").unwrap();
        let dm = sysfs.join("devices/virtual/block/dm-0");
        std::fs::create_dir_all(dm.join("slaves")).unwrap();
        let classes = sysfs.join("class/block");
        std::fs::create_dir_all(&classes).unwrap();
        symlink(nvme.join("nvme0n1p2"), classes.join("nvme0n1p2")).unwrap();
        symlink(&nvme, classes.join("nvme0n1")).unwrap();
        symlink(&dm, classes.join("dm-0")).unwrap();
        symlink(classes.join("nvme0n1p2"), dm.join("slaves/nvme0n1p2")).unwrap();
        assert_eq!(
            linux_whole_disk(&classes.join("nvme0n1p2"), &sysfs).as_deref(),
            Some("nvme0n1")
        ); // partition → disk
        assert_eq!(
            linux_whole_disk(&classes.join("nvme0n1"), &sysfs).as_deref(),
            Some("nvme0n1")
        );
        assert_eq!(
            linux_whole_disk(&classes.join("dm-0"), &sysfs).as_deref(),
            Some("nvme0n1")
        ); // LVM / LUKS
        assert_eq!(linux_whole_disk(&classes.join("missing"), &sysfs), None);
    }

    #[test]
    fn physical_disks_skip_virtual_devices() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("block/sda")).unwrap();
        std::fs::create_dir_all(tmp.path().join("block/nvme0n1")).unwrap();
        assert_eq!(linux_physical_disks(tmp.path()), ["nvme0n1", "sda"]);
        assert!(linux_physical_disks(&tmp.path().join("missing")).is_empty());
    }

    #[test]
    fn darwin_whole_disk_from_diskutil_info() {
        let infos = json!({"disk3s5": {"ParentWholeDisk": "disk0"}, "disk0s2": {"ParentWholeDisk": "disk0"}});
        let lookup = |name: &str| infos.get(name).cloned().unwrap_or(json!({}));
        let apfs = json!({"DeviceIdentifier": "disk3s1s1", "APFSPhysicalStores": [{"APFSPhysicalStore": "disk0s2"}]});
        assert_eq!(darwin_whole_disk(&apfs, &lookup).as_deref(), Some("disk0"));
        let none = |_: &str| json!({});
        assert_eq!(
            darwin_whole_disk(
                &json!({"DeviceIdentifier": "disk4s1", "ParentWholeDisk": "disk4"}),
                &none
            )
            .as_deref(),
            Some("disk4")
        );
        assert_eq!(
            darwin_whole_disk(&json!({"DeviceIdentifier": "disk5s2"}), &none).as_deref(),
            Some("disk5")
        );
        assert_eq!(darwin_whole_disk(&json!({}), &none), None);
    }

    #[test]
    fn octal_escapes() {
        assert_eq!(unescape_octal(r"C:\134"), r"C:\");
        assert_eq!(unescape_octal(r"a\040b"), "a b");
        assert_eq!(unescape_octal(r"plain\9"), r"plain\9");
    }
}
