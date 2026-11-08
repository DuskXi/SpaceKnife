//! Volume information: filesystem type, capacity, free space, cluster size, remote/removable
//! (port of `spaceknife.scan.volume`).
//!
//! Free space is what the user can still write (`f_bavail` / `GetDiskFreeSpaceEx` total free),
//! and `used = total - free - reserved`, so for a whole-volume scan
//!
//! ```text
//! scanned allocation + unknown + free == total
//! ```
//!
//! where *unknown* is whatever the scan could not see (no permission, filesystem metadata,
//! snapshots, reserved blocks …).
//!
//! Public API (stable, used by the system / server tasks): [`VolumeInfo`], [`volume_for`],
//! [`list_volumes`], [`is_volume_root`], [`in_wsl`], [`volume_group_twins`],
//! [`parse_firmlinks`], [`darwin_firmlinks`], [`through_firmlinks`].

use std::io;
use std::path::Path;
use std::sync::OnceLock;

use sk_protocol::scan as p;

/// `msg` (a Chinese message id) in the interface language.
fn tr(msg: &str) -> String {
    crate::i18n::t(msg).to_owned()
}

pub const TEXT_OFFLINE: &str = "网络卷未连接";
pub const TEXT_NO_MEDIA: &str = "没有介质";
pub const TEXT_UNREADABLE: &str = "无法读取卷：{reason}";
pub const TEXT_WSL_LABEL: &str = "{drive}（Windows 盘）";
/// The WSL drive note (message id; shown translated).
pub const WSL_DRIVE_NOTE: &str =
    "WSL 经 9P 协议访问的 Windows 盘，扫描会非常慢；建议用 Windows 版的 SpaceKnife 扫描";

pub const NETWORK_FS: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "afpfs",
    "webdav",
    "davfs",
    "9p",
    "ceph",
    "glusterfs",
    "fuse.sshfs",
    "fuse.rclone",
    "fuse.gvfsd-fuse",
];
pub const PSEUDO_FS: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "tmpfs",
    "cgroup",
    "cgroup2",
    "debugfs",
    "tracefs",
    "securityfs",
    "pstore",
    "bpf",
    "configfs",
    "efivarfs",
    "autofs",
    "mqueue",
    "hugetlbfs",
    "fusectl",
    "binfmt_misc",
    "rpc_pipefs",
    "nsfs",
    "squashfs",
    "overlay",
    "ramfs",
];

/// A mounted volume (Python `VolumeInfo`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeInfo {
    pub mount_point: String,
    pub device: String,
    pub fs_type: String,
    pub label: String,
    pub total: i64,
    pub free: i64,
    pub cluster_size: i64,
    pub is_remote: bool,
    pub is_removable: bool,
    pub serial: String,
    /// `st_dev` on POSIX (0 on Windows)
    pub dev: u64,
    /// network volumes: the server path (UNC share, host:/export)
    pub remote_path: String,
    /// cannot be scanned now (no media, disconnected, locked)
    pub reason: String,
    /// can be scanned but is not recommended, and why (shown on the volume card)
    pub note: String,
    /// free only for the super-user (ext4 keeps 5 % by default): neither used nor free
    pub reserved: i64,
}

impl VolumeInfo {
    pub fn used(&self) -> i64 {
        (self.total - self.free - self.reserved).max(0)
    }

    /// Whether this is a local NTFS volume of a drive letter (`C:\`): MFT scans and the USN journal.
    pub fn is_local_ntfs(&self) -> bool {
        !self.is_remote
            && self.fs_type.eq_ignore_ascii_case("NTFS")
            && self.mount_point.as_bytes().get(1) == Some(&b':')
    }

    /// The protocol model (server `volume_model`): media probed (cached), the system volume marked.
    pub fn to_protocol(&self) -> p::VolumeInfo {
        let media = if self.reason.is_empty() {
            crate::media::media_of(&self.mount_point, &self.device, self.is_remote)
        } else {
            p::MediaName::Unknown
        };
        p::VolumeInfo {
            mount_point: self.mount_point.clone(),
            device: self.device.clone(),
            fs_type: self.fs_type.clone(),
            label: self.label.clone(),
            total: self.total,
            free: self.free,
            used: self.used(),
            cluster_size: self.cluster_size,
            is_remote: self.is_remote,
            is_removable: self.is_removable,
            serial: self.serial.clone(),
            media,
            is_system: normcase_str(&self.mount_point)
                == normcase_str(&crate::media::system_mount()),
            remote_path: self.remote_path.clone(),
            reason: self.reason.clone(),
            note: self.note.clone(),
            reserved: self.reserved,
        }
    }
}

fn normcase_str(s: &str) -> String {
    String::from_utf8_lossy(&sk_store::names::normcase(s.as_bytes())).into_owned()
}

fn norm_key(path: &str) -> String {
    let norm = sk_store::names::normpath(path.as_bytes());
    String::from_utf8_lossy(&sk_store::names::normcase(&norm)).into_owned()
}

/// `path` (made absolute) is the mount point of `volume`.
pub fn is_volume_root(path: &Path, volume: &VolumeInfo) -> bool {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    norm_key(&abs.to_string_lossy()) == norm_key(&volume.mount_point)
}

/// An `io::Error`'s text without Rust's " (os error N)" suffix (Python's `strerror`).
pub fn os_message(err: &io::Error) -> String {
    let s = err.to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].trim_end_matches(['.', '。']).to_owned(),
        None => s,
    }
}

// --------------------------------------------------------------------------- public

/// Volume that contains `path`.
pub fn volume_for(path: &Path) -> io::Result<VolumeInfo> {
    #[cfg(windows)]
    {
        win::volume(path)
    }
    #[cfg(target_os = "macos")]
    {
        darwin::volume(path)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::volume(path)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = path;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// Volumes worth offering on the start page (no pseudo filesystems).
pub fn list_volumes() -> Vec<VolumeInfo> {
    #[cfg(windows)]
    {
        win::list()
    }
    #[cfg(target_os = "macos")]
    {
        darwin::list()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::list()
    }
    #[cfg(not(any(windows, unix)))]
    {
        Vec::new()
    }
}

// --------------------------------------------------------------------------- Windows

#[cfg(windows)]
mod win {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDiskFreeSpaceW, GetDriveTypeW, GetLogicalDriveStringsW,
        GetVolumeInformationW, GetVolumePathNameW,
    };

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;
    const ERROR_NOT_READY: i32 = 21;

    pub(super) fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..n])
    }

    pub(super) fn drive_type(root: &str) -> u32 {
        let w = wide(root.as_ref());
        // SAFETY: NUL-terminated wide string.
        unsafe { GetDriveTypeW(w.as_ptr()) }
    }

    pub(super) fn volume(path: &Path) -> io::Result<VolumeInfo> {
        let abs = std::path::absolute(path)?;
        let w = wide(abs.as_os_str());
        let mut buf = vec![0u16; 1024];
        // SAFETY: valid buffers of the stated sizes.
        if unsafe { GetVolumePathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut root = from_wide(&buf);
        if !root.ends_with('\\') {
            root.push('\\');
        }
        Ok(info_of_root(&root))
    }

    fn info_of_root(root: &str) -> VolumeInfo {
        let wr = wide(root.as_ref());
        let mut label = [0u16; 261];
        let mut fsname = [0u16; 261];
        let (mut serial, mut maxlen, mut fsflags) = (0u32, 0u32, 0u32);
        // SAFETY: buffers of the stated sizes.
        let ok = unsafe {
            GetVolumeInformationW(
                wr.as_ptr(),
                label.as_mut_ptr(),
                261,
                &mut serial,
                &mut maxlen,
                &mut fsflags,
                fsname.as_mut_ptr(),
                261,
            )
        } != 0;
        let (mut free_caller, mut total, mut total_free) = (0u64, 0u64, 0u64);
        // SAFETY: out pointers to locals.
        unsafe { GetDiskFreeSpaceExW(wr.as_ptr(), &mut free_caller, &mut total, &mut total_free) };
        let (mut spc, mut bps, mut nfree, mut nclusters) = (0u32, 0u32, 0u32, 0u32);
        let mut cluster = 4096i64;
        // SAFETY: out pointers to locals.
        if unsafe { GetDiskFreeSpaceW(wr.as_ptr(), &mut spc, &mut bps, &mut nfree, &mut nclusters) }
            != 0
        {
            let c = spc as i64 * bps as i64;
            cluster = if c > 0 { c } else { 4096 };
        }
        let dt = drive_type(root);
        VolumeInfo {
            mount_point: root.to_owned(),
            device: root.trim_end_matches('\\').to_owned(),
            fs_type: if ok {
                from_wide(&fsname)
            } else {
                String::new()
            },
            label: if ok { from_wide(&label) } else { String::new() },
            total: total as i64,
            free: total_free as i64,
            cluster_size: cluster,
            is_remote: dt == DRIVE_REMOTE,
            is_removable: dt == DRIVE_REMOVABLE || dt == DRIVE_CDROM,
            serial: if ok {
                format!("{serial:08X}")
            } else {
                String::new()
            },
            ..Default::default()
        }
    }

    /// `\\server\share` behind a mapped drive letter, or "".
    fn remote_path(root: &str) -> String {
        let w = wide(root.trim_end_matches('\\').as_ref());
        let mut buf = vec![0u16; 1024];
        let mut size = buf.len() as u32;
        // SAFETY: buffer of `size` units.
        if unsafe { WNetGetConnectionW(w.as_ptr(), buf.as_mut_ptr(), &mut size) } == 0 {
            from_wide(&buf)
        } else {
            String::new()
        }
    }

    pub(super) fn list() -> Vec<VolumeInfo> {
        let mut buf = vec![0u16; 4096];
        // SAFETY: buffer of the stated size.
        let n = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
        let roots: Vec<String> = buf[..n.min(buf.len())]
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect();
        let mut out = Vec::new();
        for root in roots {
            let dt = drive_type(&root);
            if matches!(dt, 0 | 1 | DRIVE_CDROM) {
                continue; // unknown, no root, CD-ROM
            }
            let remote = if dt == DRIVE_REMOTE {
                remote_path(&root)
            } else {
                String::new()
            };
            let mut vol = match volume(Path::new(&root)) {
                Ok(v) => v,
                Err(e) => {
                    // still listed, but disabled with the reason (UI-W1-22)
                    let reason = if dt == DRIVE_REMOTE {
                        tr(TEXT_OFFLINE)
                    } else if e.raw_os_error() == Some(ERROR_NOT_READY) {
                        tr(TEXT_NO_MEDIA)
                    } else {
                        tr(TEXT_UNREADABLE).replace("{reason}", &os_message(&e))
                    };
                    VolumeInfo {
                        mount_point: root.clone(),
                        device: root.trim_end_matches('\\').to_owned(),
                        cluster_size: 4096,
                        is_remote: dt == DRIVE_REMOTE,
                        is_removable: dt == DRIVE_REMOVABLE,
                        reason,
                        ..Default::default()
                    }
                }
            };
            if vol.total == 0 && vol.reason.is_empty() {
                vol.reason = tr(TEXT_NO_MEDIA);
            }
            if !remote.is_empty() {
                vol.remote_path = remote;
            }
            out.push(vol);
        }
        out
    }
}

// --------------------------------------------------------------------------- Linux (pure parts everywhere)

/// One line of `/proc/self/mountinfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub mount_point: String,
    pub fs_type: String,
    pub source: String,
    pub major: u32,
    pub minor: u32,
}

/// Decodes the octal escapes of mountinfo (`\040` space, `\134` backslash).
fn unescape_octal(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 4 <= b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = (b[i + 1] - b'0') as u32 * 64
                + (b[i + 2] - b'0') as u32 * 8
                + (b[i + 3] - b'0') as u32;
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses `/proc/self/mountinfo` text.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    let mut out = Vec::new();
    for line in text.lines() {
        let (left, right) = match line.split_once(" - ") {
            Some(x) => x,
            None => (line, ""),
        };
        let fields: Vec<&str> = left.split_whitespace().collect();
        let rfields: Vec<&str> = right.split_whitespace().collect();
        if fields.len() < 5 || rfields.len() < 2 {
            continue;
        }
        let (major, minor) = fields[2].split_once(':').unwrap_or((fields[2], "0"));
        let (Ok(major), Ok(minor)) = (major.parse(), minor.parse()) else {
            continue;
        };
        out.push(Mount {
            mount_point: unescape_octal(fields[4]),
            fs_type: rfields[0].to_owned(),
            source: unescape_octal(rfields[1]),
            major,
            minor,
        });
    }
    out
}

/// Running inside the Windows Subsystem for Linux.
pub fn in_wsl() -> bool {
    static WSL: OnceLock<bool> = OnceLock::new();
    *WSL.get_or_init(|| {
        cfg!(target_os = "linux")
            && std::fs::read("/proc/sys/kernel/osrelease")
                .map(|b| {
                    String::from_utf8_lossy(&b)
                        .to_lowercase()
                        .contains("microsoft")
                })
                .unwrap_or(false)
    })
}

/// WSL plumbing mounted into every distribution: never a scan target.
const WSL_HIDDEN: &[&str] = &[
    "/mnt/wsl",
    "/mnt/wslg",
    "/usr/lib/wsl",
    "/usr/lib/modules",
    "/Docker/",
    "/init",
];

/// `C:` for a Windows drive mounted into WSL over 9P (DrvFs), else "".
pub fn wsl_drive(m: &Mount) -> String {
    let s = m.source.as_bytes();
    let drive_like = (s.len() == 2 || (s.len() == 3 && s[2] == b'\\'))
        && s[0].is_ascii_alphabetic()
        && s[1] == b':';
    if matches!(m.fs_type.as_str(), "9p" | "drvfs" | "v9fs") && drive_like {
        m.source[..2].to_uppercase()
    } else {
        String::new()
    }
}

/// A Windows drive seen from WSL: named after the drive and marked as not recommended.
pub fn wsl_marked(m: &Mount, mut vol: VolumeInfo, wsl: bool) -> VolumeInfo {
    let drive = if wsl { wsl_drive(m) } else { String::new() };
    if drive.is_empty() {
        return vol;
    }
    vol.label = tr(TEXT_WSL_LABEL).replace("{drive}", &drive);
    vol.note = tr(WSL_DRIVE_NOTE);
    vol.remote_path = String::new();
    vol
}

/// The numbers of `statvfs(2)` a volume needs.
#[derive(Clone, Copy, Debug, Default)]
pub struct StatVfs {
    pub frsize: u64,
    pub bsize: u64,
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
}

/// A POSIX volume from its `statvfs` numbers (Python `_posix_from_statvfs`).
pub fn volume_from_statvfs(
    st: StatVfs,
    dev: u64,
    mount_point: &str,
    fs_type: &str,
    source: &str,
    remote: bool,
) -> VolumeInfo {
    let frsize = if st.frsize > 0 { st.frsize } else { st.bsize } as i64;
    let base = mount_point.trim_end_matches('/');
    let label = base.rsplit('/').next().unwrap_or("");
    VolumeInfo {
        mount_point: mount_point.to_owned(),
        device: source.to_owned(),
        fs_type: fs_type.to_owned(),
        label: if label.is_empty() {
            mount_point.to_owned()
        } else {
            label.to_owned()
        },
        total: st.blocks as i64 * frsize,
        free: st.bavail as i64 * frsize,
        reserved: st.bfree.saturating_sub(st.bavail) as i64 * frsize,
        cluster_size: if st.bsize > 0 { st.bsize as i64 } else { 4096 },
        is_remote: remote,
        is_removable: ["/media/", "/run/media/", "/Volumes/"]
            .iter()
            .any(|p| mount_point.starts_with(p)),
        dev,
        remote_path: if remote {
            source.to_owned()
        } else {
            String::new()
        },
        ..Default::default()
    }
}

/// The Linux volume list from mounts (pure: `probe` makes a volume of a mount).
pub fn linux_list_from(
    mounts: &[Mount],
    wsl: bool,
    probe: impl Fn(&Mount) -> io::Result<VolumeInfo>,
) -> Vec<VolumeInfo> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for m in mounts {
        if PSEUDO_FS.contains(&m.fs_type.as_str())
            || ["/proc", "/sys", "/dev", "/run/user", "/snap/"]
                .iter()
                .any(|p| m.mount_point.starts_with(p))
        {
            continue;
        }
        if wsl
            && (WSL_HIDDEN.iter().any(|p| m.mount_point.starts_with(p))
                || (m.fs_type == "9p" && wsl_drive(m).is_empty()))
        {
            continue;
        }
        if !seen.insert((m.major, m.minor)) {
            continue;
        }
        let Ok(vol) = probe(m) else { continue };
        out.push(wsl_marked(m, vol, wsl));
    }
    out
}

#[cfg(unix)]
mod posix {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    pub(super) fn statvfs(path: &Path) -> io::Result<StatVfs> {
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: zeroed POD out struct, NUL-terminated path.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(StatVfs {
            frsize: st.f_frsize as u64,
            bsize: st.f_bsize as u64,
            blocks: st.f_blocks as u64,
            bfree: st.f_bfree as u64,
            bavail: st.f_bavail as u64,
        })
    }

    pub(super) fn from_statvfs(
        path: &Path,
        mount_point: &str,
        fs_type: &str,
        source: &str,
        remote: bool,
    ) -> io::Result<VolumeInfo> {
        let st = statvfs(path)?;
        let dev = std::fs::metadata(path)?.dev();
        Ok(volume_from_statvfs(
            st,
            dev,
            mount_point,
            fs_type,
            source,
            remote,
        ))
    }

    pub(super) fn mount_point_by_dev(path: &Path) -> io::Result<String> {
        let mut path = std::fs::canonicalize(path)?;
        let dev = std::fs::metadata(&path)?.dev();
        loop {
            let Some(parent) = path.parent().map(Path::to_path_buf) else {
                return Ok(path.to_string_lossy().into_owned());
            };
            if parent == path || std::fs::metadata(&parent)?.dev() != dev {
                return Ok(path.to_string_lossy().into_owned());
            }
            path = parent;
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod linux {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn mounts() -> Vec<Mount> {
        std::fs::read("/proc/self/mountinfo")
            .map(|b| parse_mountinfo(&String::from_utf8_lossy(&b)))
            .unwrap_or_default()
    }

    fn major(dev: u64) -> u32 {
        libc::major(dev as libc::dev_t) as u32
    }
    fn minor(dev: u64) -> u32 {
        libc::minor(dev as libc::dev_t) as u32
    }

    pub(super) fn volume(path: &Path) -> io::Result<VolumeInfo> {
        let real = std::fs::canonicalize(path)?;
        let dev = std::fs::metadata(&real)?.dev();
        let real_s = real.to_string_lossy().into_owned();
        let mut best: Option<Mount> = None;
        for m in mounts() {
            let same = (m.major, m.minor) == (major(dev), minor(dev));
            let inside = real_s == m.mount_point
                || real_s.starts_with(&format!("{}/", m.mount_point.trim_end_matches('/')));
            if same
                && inside
                && best
                    .as_ref()
                    .is_none_or(|b| m.mount_point.len() > b.mount_point.len())
            {
                best = Some(m);
            }
        }
        match best {
            None => {
                let point = posix::mount_point_by_dev(&real)?;
                posix::from_statvfs(&real, &point, "", "", false)
            }
            Some(m) => {
                let remote = NETWORK_FS.contains(&m.fs_type.as_str());
                let vol =
                    posix::from_statvfs(&real, &m.mount_point, &m.fs_type, &m.source, remote)?;
                Ok(wsl_marked(&m, vol, in_wsl()))
            }
        }
    }

    pub(super) fn list() -> Vec<VolumeInfo> {
        linux_list_from(&mounts(), in_wsl(), |m| {
            posix::from_statvfs(
                Path::new(&m.mount_point),
                &m.mount_point,
                &m.fs_type,
                &m.source,
                NETWORK_FS.contains(&m.fs_type.as_str()),
            )
        })
    }
}

// --------------------------------------------------------------------------- macOS

/// The APFS Data volume: macOS presents it at "/" together with the sealed system volume.
pub const DARWIN_DATA: &str = "/System/Volumes/Data";
/// macOS 10.15+: "<path under />\t<path in the Data volume>".
pub const FIRMLINKS: &str = "/usr/share/firmlinks";

#[cfg(target_os = "macos")]
mod darwin {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;

    const MNT_LOCAL: u32 = 0x0000_1000;
    const MNT_NOWAIT: libc::c_int = 2;

    fn cstr(b: &[libc::c_char]) -> String {
        // SAFETY: fixed arrays the kernel NUL-terminates.
        unsafe { CStr::from_ptr(b.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    /// (fs type, mount point, source, is_local) via statfs(2).
    fn statfs(path: &Path) -> Option<(String, String, String, bool)> {
        let c = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: zeroed POD out struct.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        Some((
            cstr(&st.f_fstypename),
            cstr(&st.f_mntonname),
            cstr(&st.f_mntfromname),
            st.f_flags & MNT_LOCAL != 0,
        ))
    }

    /// Mount point of every mounted file system (getfsstat(2)).
    pub(crate) fn mounts() -> Vec<String> {
        for _ in 0..3 {
            // SAFETY: a NULL buffer asks for the count.
            let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, MNT_NOWAIT) };
            if count <= 0 {
                return Vec::new();
            }
            let cap = count as usize + 8;
            // SAFETY: zeroed POD structs.
            let mut buf: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; cap];
            let bytes = (cap * std::mem::size_of::<libc::statfs>()) as libc::c_int;
            let got = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, MNT_NOWAIT) };
            if got >= 0 && (got as usize) < cap {
                return buf[..got as usize]
                    .iter()
                    .map(|s| cstr(&s.f_mntonname))
                    .collect();
            }
        }
        Vec::new()
    }

    pub(super) fn volume(path: &Path) -> io::Result<VolumeInfo> {
        let Some((fs, mut point, source, local)) = statfs(path) else {
            let point = posix::mount_point_by_dev(path)?;
            return posix::from_statvfs(path, &point, "", "", false);
        };
        if point == DARWIN_DATA {
            point = "/".into();
        }
        posix::from_statvfs(path, &point, &fs, &source, !local)
    }

    pub(super) fn list() -> Vec<VolumeInfo> {
        let mut paths = vec![if Path::new(DARWIN_DATA).is_dir() {
            DARWIN_DATA.to_owned()
        } else {
            "/".to_owned()
        }];
        if let Ok(rd) = std::fs::read_dir("/Volumes") {
            let mut names: Vec<String> = rd
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .collect();
            names.sort();
            paths.extend(names.into_iter().map(|n| format!("/Volumes/{n}")));
        }
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for p in paths {
            let Ok(v) = volume(Path::new(&p)) else {
                continue;
            };
            if seen.insert(v.mount_point.clone()) {
                out.push(v);
            }
        }
        out
    }
}

/// Mount points below a mounted `root` that report the same device as `root` (macOS: the other
/// half of an APFS volume group, already in the tree through the firmlinks). Elsewhere none.
pub fn volume_group_twins(root: &str) -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt;
        volume_group_twins_in(root, &darwin::mounts(), |p| {
            std::fs::metadata(p).map(|m| m.dev())
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = root;
        Vec::new()
    }
}

/// [`volume_group_twins`] over given mounts and a device lookup (testable).
pub fn volume_group_twins_in(
    root: &str,
    mounts: &[String],
    dev_of: impl Fn(&str) -> io::Result<u64>,
) -> Vec<String> {
    let norm = |m: &str| {
        let t = m.trim_end_matches('/');
        if t.is_empty() {
            "/".to_owned()
        } else {
            t.to_owned()
        }
    };
    let mut points: Vec<String> = mounts.iter().map(|m| norm(m)).collect();
    points.sort();
    points.dedup();
    let base = norm(root);
    if !points.contains(&base) {
        return Vec::new();
    }
    let Ok(dev) = dev_of(&base) else {
        return Vec::new();
    };
    let prefix = if base == "/" {
        base.clone()
    } else {
        format!("{base}/")
    };
    points
        .into_iter()
        .filter(|p| *p != base && p.starts_with(&prefix))
        .filter(|p| dev_of(p).is_ok_and(|d| d == dev))
        .collect()
}

/// (path under "/", path inside the Data volume) per line of /usr/share/firmlinks, the longest
/// Data-volume path first.
pub fn parse_firmlinks(text: &str) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = text
        .lines()
        .filter_map(|line| {
            let (s, t) = line.split_once('\t')?;
            let s = s.trim().trim_end_matches('/');
            let t = t.trim().trim_matches('/');
            (s.starts_with('/') && !t.is_empty()).then(|| (s.to_owned(), t.to_owned()))
        })
        .collect();
    pairs.sort_by_key(|p| std::cmp::Reverse(p.1.len()));
    pairs
}

/// The firmlinks of this Mac (empty elsewhere).
pub fn darwin_firmlinks() -> &'static [(String, String)] {
    static LINKS: OnceLock<Vec<(String, String)>> = OnceLock::new();
    LINKS.get_or_init(|| {
        std::fs::read_to_string(FIRMLINKS)
            .map(|t| parse_firmlinks(&t))
            .unwrap_or_default()
    })
}

/// A path inside a volume-group twin as the firmlinks show it under "/"
/// (/System/Volumes/Data/Users/x → /Users/x); None when no firmlink leads there.
pub fn through_firmlinks(
    path: &str,
    twin: &str,
    firmlinks: Option<&[(String, String)]>,
) -> Option<String> {
    let rel = path.get(twin.len()..)?.trim_matches('/');
    let links = firmlinks.unwrap_or_else(|| darwin_firmlinks());
    for (source, target) in links {
        if rel == target || rel.starts_with(&format!("{target}/")) {
            return Some(format!("{source}{}", &rel[target.len()..]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_for_temp_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let vol = volume_for(tmp.path()).unwrap();
        assert!(vol.total > 0);
        assert!(0 <= vol.free && vol.free <= vol.total);
        assert_eq!(vol.used(), vol.total - vol.free - vol.reserved);
        assert!(vol.cluster_size > 0);
        let t = tmp.path().to_string_lossy().to_lowercase();
        assert!(
            t.starts_with(vol.mount_point.to_lowercase().trim_end_matches(['\\', '/']))
                || vol.mount_point == "/"
        );
        assert!(is_volume_root(Path::new(&vol.mount_point), &vol));
        assert!(!is_volume_root(tmp.path(), &vol));
        let proto = vol.to_protocol();
        assert_eq!(proto.used, vol.used());
    }

    #[test]
    fn list_volumes_includes_the_temp_volume() {
        let tmp = tempfile::tempdir().unwrap();
        let vols = list_volumes();
        assert!(!vols.is_empty());
        let mp = volume_for(tmp.path()).unwrap().mount_point;
        assert!(vols.iter().any(|v| v.mount_point == mp));
    }

    const FIRMLINKS_TEXT: &str = "/AppleInternal\tAppleInternal\n/Applications\tApplications\n/Library\tLibrary\n/System/Library/Caches\tSystem/Library/Caches\n/Users\tUsers\n/private\tprivate\n/usr/local\tusr/local\n\n# not a firmlink\n";

    #[test]
    fn volume_group_twins_are_mounts_below_a_root_with_its_device() {
        let mounts: Vec<String> = [
            "/",
            "/dev",
            "/System/Volumes/VM",
            "/System/Volumes/Preboot",
            "/System/Volumes/Data",
            "/Volumes/USB",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let dev = |p: &str| -> io::Result<u64> {
            Ok(match p {
                "/" | "/System/Volumes/Data" => 21,
                "/System/Volumes/VM" => 25,
                "/System/Volumes/Preboot" => 23,
                "/dev" => 30,
                "/Volumes/USB" => 40,
                _ => return Err(io::ErrorKind::NotFound.into()),
            })
        };
        assert_eq!(
            volume_group_twins_in("/", &mounts, dev),
            vec!["/System/Volumes/Data"]
        );
        assert!(volume_group_twins_in("/System/Volumes/Data", &mounts, dev).is_empty());
        assert!(volume_group_twins_in("/System", &mounts, dev).is_empty());
        assert!(volume_group_twins_in("/Volumes/USB", &mounts, dev).is_empty());
    }

    #[test]
    fn firmlinks_map_data_volume_paths_back_under_root() {
        let links = parse_firmlinks(FIRMLINKS_TEXT);
        assert!(links.contains(&("/Users".into(), "Users".into())) && links.len() == 7);
        assert_eq!(links[0].1, "System/Library/Caches");
        let data = "/System/Volumes/Data";
        let l = Some(links.as_slice());
        let f = |p: &str| through_firmlinks(&format!("{data}{p}"), data, l);
        assert_eq!(
            f("/Users/lenny/Documents/a.txt").as_deref(),
            Some("/Users/lenny/Documents/a.txt")
        );
        assert_eq!(f("/Users").as_deref(), Some("/Users"));
        assert_eq!(f("/usr/local/bin/x").as_deref(), Some("/usr/local/bin/x"));
        assert_eq!(
            f("/System/Library/Caches/c").as_deref(),
            Some("/System/Library/Caches/c")
        );
        assert_eq!(f("/.Spotlight-V100/x"), None);
        assert_eq!(f("/UsersX/y"), None);
    }

    // ------------------------------------------------------------- test_volume_wsl

    fn mount(point: &str, fs: &str, source: &str, minor: u32) -> Mount {
        Mount {
            mount_point: point.into(),
            fs_type: fs.into(),
            source: source.into(),
            major: 0,
            minor,
        }
    }

    fn mounts() -> Vec<Mount> {
        vec![
            mount("/", "ext4", "/dev/sdf", 1),
            mount("/mnt/c", "9p", "C:\\", 2),
            mount("/mnt/d", "9p", "D:\\", 3),
            mount(
                "/Docker/host",
                "9p",
                "C:\\Program Files\\Docker\\Docker\\resources",
                4,
            ),
            mount("/usr/lib/wsl/drivers", "9p", "drivers", 5),
            mount("/mnt/wslg/distro", "ext4", "/dev/sdf", 6),
            mount(
                "/mnt/wsl/docker-desktop/cli-tools",
                "iso9660",
                "/dev/loop0",
                7,
            ),
            mount("/srv/share", "nfs4", "nas:/export", 8),
        ]
    }

    fn fake(m: &Mount) -> io::Result<VolumeInfo> {
        let remote = NETWORK_FS.contains(&m.fs_type.as_str());
        Ok(VolumeInfo {
            mount_point: m.mount_point.clone(),
            device: m.source.clone(),
            fs_type: m.fs_type.clone(),
            label: m.mount_point.clone(),
            total: 100,
            free: 50,
            cluster_size: 4096,
            is_remote: remote,
            remote_path: if remote {
                m.source.clone()
            } else {
                String::new()
            },
            ..Default::default()
        })
    }

    #[test]
    fn wsl_volume_list() {
        let _lang = crate::i18n::TEST_LOCK.lock(); // the texts follow the language
        for wsl in [true, false] {
            let listed = linux_list_from(&mounts(), wsl, fake);
            let points: std::collections::HashSet<&str> =
                listed.iter().map(|v| v.mount_point.as_str()).collect();
            if wsl {
                assert_eq!(
                    points,
                    ["/", "/mnt/c", "/mnt/d", "/srv/share"]
                        .into_iter()
                        .collect()
                );
                let c = listed.iter().find(|v| v.mount_point == "/mnt/c").unwrap();
                assert_eq!(c.label, tr(TEXT_WSL_LABEL).replace("{drive}", "C:"));
                assert_eq!(c.note, tr(WSL_DRIVE_NOTE));
                assert!(c.is_remote && c.remote_path.is_empty());
                assert!(
                    listed
                        .iter()
                        .filter(|v| !v.mount_point.starts_with("/mnt/"))
                        .all(|v| v.note.is_empty())
                );
            } else {
                assert!(points.contains("/Docker/host") && points.contains("/mnt/wslg/distro"));
                assert!(listed.iter().all(|v| v.note.is_empty()));
            }
        }
    }

    #[test]
    fn wsl_drive_sources() {
        assert_eq!(wsl_drive(&mount("/mnt/c", "9p", "C:\\", 1)), "C:");
        assert_eq!(wsl_drive(&mount("/mnt/e", "drvfs", "e:", 1)), "E:");
        assert_eq!(wsl_drive(&mount("/x", "9p", "drivers", 1)), "");
        assert_eq!(wsl_drive(&mount("/x", "ext4", "C:\\", 1)), "");
    }

    #[test]
    fn root_reserved_blocks_are_neither_used_nor_free() {
        let st = StatVfs {
            frsize: 4096,
            bsize: 4096,
            blocks: 1000,
            bfree: 800,
            bavail: 750,
        };
        let v = volume_from_statvfs(st, 1, "/", "ext4", "/dev/sda1", false);
        assert_eq!(
            (v.total, v.free, v.reserved, v.used()),
            (1000 * 4096, 750 * 4096, 50 * 4096, 200 * 4096)
        );
    }

    #[test]
    fn mountinfo_lines() {
        let text = "36 35 98:0 /mnt1 /mnt/with\\040space rw,noatime master:1 - ext3 /dev/root rw\n\
                    40 1 0:50 / /mnt/c rw - 9p C:\\134 rw\n";
        let m = parse_mountinfo(text);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].mount_point, "/mnt/with space");
        assert_eq!((m[0].major, m[0].minor), (98, 0));
        assert_eq!(m[1].source, "C:\\");
        assert_eq!(wsl_drive(&m[1]), "C:");
    }
}
