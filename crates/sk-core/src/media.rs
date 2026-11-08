//! Storage media of a volume: NVMe / SATA SSD, spinning disk, USB, network (port of
//! `spaceknife.scan.media`, UI-W1-16, UI-W2-37).
//!
//! Read-only probes that need no elevation:
//!
//! * Windows: `IOCTL_STORAGE_QUERY_PROPERTY` on `\\.\C:` (bus type + seek penalty).
//! * Linux: `/sys/class/block/<dev>` (rotational flag, the device path tells NVMe / USB / MMC).
//! * macOS: `diskutil info -plist` (SolidState, BusProtocol).
//!
//! Anything that cannot be determined is [`Media::Unknown`]; callers never fail because of it.
//!
//! Public API (stable): [`Media`], [`classify`], [`media_of`], [`system_mount`].

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// The media kinds (the protocol's `MediaName`: `nvme_ssd`, `hdd`, …).
pub use sk_protocol::scan::MediaName as Media;

/// Combines a bus name (`nvme`, `sata`, `usb`, `sd`, `virtual`, …) and the rotational flag.
pub fn classify(bus: &str, rotational: Option<bool>, removable: bool) -> Media {
    match bus {
        "nvme" => Media::NvmeSsd,
        "usb" => {
            if rotational == Some(true) {
                Media::UsbHdd
            } else if removable {
                Media::UsbFlash
            } else {
                Media::UsbSsd
            }
        }
        "sd" => Media::SdCard,
        "virtual" => Media::Virtual,
        _ => match rotational {
            Some(true) => Media::Hdd,
            Some(false) if bus == "sata" => Media::SataSsd,
            Some(false) => Media::Ssd,
            None => Media::Unknown,
        },
    }
}

/// Media of a volume (cached: it does not change while the device stays connected).
pub fn media_of(mount_point: &str, device: &str, is_remote: bool) -> Media {
    if is_remote {
        return Media::Network;
    }
    static CACHE: OnceLock<Mutex<HashMap<(String, String), Media>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (mount_point.to_owned(), device.to_owned());
    if let Some(&m) = cache.lock().unwrap().get(&key) {
        return m;
    }
    // a probe must never break the volume list
    let got = std::panic::catch_unwind(|| probe(mount_point, device)).unwrap_or(Media::Unknown);
    let mut c = cache.lock().unwrap();
    if c.len() >= 64 {
        c.clear();
    }
    c.insert(key, got);
    got
}

fn probe(mount_point: &str, device: &str) -> Media {
    #[cfg(windows)]
    {
        let _ = device;
        win_media(mount_point)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = device;
        darwin_media(mount_point)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = mount_point;
        linux_media(device)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (mount_point, device);
        Media::Unknown
    }
}

/// Mount point of the system volume (selected by default on the start page, UI-W1-19).
pub fn system_mount() -> String {
    if cfg!(windows) {
        let drive = std::env::var("SYSTEMDRIVE").unwrap_or_else(|_| "C:".into());
        format!("{}\\", drive.trim_end_matches('\\'))
    } else {
        "/".into()
    }
}

// --------------------------------------------------------------------------- Windows

// STORAGE_BUS_TYPE (winioctl.h)
const BUS_USB: u32 = 7;
const BUS_SATA: u32 = 11;
const BUS_SD: u32 = 12;
const BUS_MMC: u32 = 13;
const BUS_VIRTUAL: u32 = 14;
const BUS_FILE_VIRTUAL: u32 = 15;
const BUS_NVME: u32 = 17;

/// The bus name of a `STORAGE_BUS_TYPE`.
pub fn bus_name(bus_type: u32) -> &'static str {
    match bus_type {
        BUS_NVME => "nvme",
        BUS_USB => "usb",
        BUS_SATA => "sata",
        BUS_SD | BUS_MMC => "sd",
        BUS_VIRTUAL | BUS_FILE_VIRTUAL => "virtual",
        _ => "other",
    }
}

#[cfg(windows)]
fn win_media(root: &str) -> Media {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
    use windows_sys::Win32::System::IO::DeviceIoControl;

    let drive = root.trim_end_matches('\\');
    if drive.len() != 2 || drive.as_bytes()[1] != b':' {
        return Media::Unknown;
    }
    let path: Vec<u16> = format!("\\\\.\\{drive}")
        .encode_utf16()
        .chain([0])
        .collect();
    // access 0: the storage property queries work without read rights (no elevation)
    // SAFETY: NUL-terminated path; share read|write, OPEN_EXISTING.
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            0,
            3,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Media::Unknown;
    }
    let query = |prop: u32, size: usize| -> Option<Vec<u8>> {
        let q: [u32; 3] = [prop, 0, 0]; // STORAGE_PROPERTY_QUERY: id, PropertyStandardQuery
        let mut out = vec![0u8; size];
        let mut got = 0u32;
        // SAFETY: buffers of the stated sizes.
        let ok = unsafe {
            DeviceIoControl(
                h,
                0x2D1400, // IOCTL_STORAGE_QUERY_PROPERTY
                q.as_ptr().cast(),
                12,
                out.as_mut_ptr().cast(),
                size as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then(|| {
            out.truncate(got as usize);
            out
        })
    };
    let device = query(0, 1024); // StorageDeviceProperty -> STORAGE_DEVICE_DESCRIPTOR
    let seek = query(7, 12); // StorageDeviceSeekPenaltyProperty
    // SAFETY: a handle we opened.
    unsafe { CloseHandle(h) };
    let bus_type = device
        .as_ref()
        .filter(|d| d.len() >= 32)
        .map(|d| u32::from_le_bytes(d[28..32].try_into().unwrap()))
        .unwrap_or(0);
    let removable = device.as_ref().is_some_and(|d| d.len() > 10 && d[10] != 0);
    let rotational = seek.as_ref().filter(|s| s.len() > 8).map(|s| s[8] != 0);
    classify(bus_name(bus_type), rotational, removable)
}

// --------------------------------------------------------------------------- Linux (pure file reads)

fn read_trim(path: &Path) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// sysfs directory of the whole disk behind a block device (partitions, dm/LVM resolved).
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn linux_disk(dev: &str, depth: u32) -> Option<std::path::PathBuf> {
    let real = std::fs::canonicalize(dev).unwrap_or_else(|_| dev.into());
    let name = real.file_name()?.to_string_lossy().into_owned();
    let mut path = std::fs::canonicalize(format!("/sys/class/block/{name}")).ok()?;
    if !path.is_dir() {
        return None;
    }
    let slaves = path.join("slaves");
    if depth < 16
        && let Ok(rd) = std::fs::read_dir(&slaves)
    {
        let mut inner: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        inner.sort();
        if let Some(first) = inner.first() {
            return linux_disk(&format!("/dev/{first}"), depth + 1);
        }
    }
    if path.join("partition").exists() {
        path = path.parent()?.to_path_buf();
    }
    Some(path)
}

/// vendor / model of hypervisors' emulated disks (they often claim to rotate: WSL's ext4 VHDX does)
const VIRTUAL_DISKS: &[&str] = &[
    "msft virtual disk",
    "vmware",
    "qemu",
    "vbox",
    "virtual disk",
];

/// A disk's sysfs folder names a hypervisor's emulated disk.
pub fn virtual_disk(disk: &Path) -> bool {
    let who = format!(
        "{} {}",
        read_trim(&disk.join("device").join("vendor")),
        read_trim(&disk.join("device").join("model"))
    )
    .to_lowercase();
    let who = who.split_whitespace().collect::<Vec<_>>().join(" ");
    VIRTUAL_DISKS.iter().any(|v| who.contains(v))
}

#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn linux_media(device: &str) -> Media {
    if !device.starts_with("/dev/") {
        return Media::Unknown;
    }
    let Some(disk) = linux_disk(device, 0) else {
        return Media::Unknown;
    };
    let name = disk
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ds = disk.to_string_lossy();
    let rot = read_trim(&disk.join("queue").join("rotational"));
    let rotational = if rot.is_empty() {
        None
    } else {
        Some(rot == "1")
    };
    let removable = read_trim(&disk.join("removable")) == "1";
    let bus = if ds.contains("/usb") {
        "usb"
    } else if name.starts_with("nvme") {
        "nvme"
    } else if name.starts_with("mmcblk") {
        "sd"
    } else if ["loop", "zram", "vd", "xvd"]
        .iter()
        .any(|p| name.starts_with(p))
        || ds.contains("/virtual/")
        || virtual_disk(&disk)
    {
        "virtual"
    } else if name.starts_with("sd") && disk.join("device").join("scsi_disk").is_dir() {
        "sata"
    } else {
        "other"
    };
    classify(bus, rotational, removable)
}

// --------------------------------------------------------------------------- macOS

/// The keys of `diskutil info -plist` the classification needs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DiskutilInfo {
    pub bus_protocol: String,
    pub solid_state: Option<bool>,
    pub virtual_or_physical: String,
    pub ejectable: bool,
}

/// Reads the keys of a `diskutil info -plist` document (top-level dict).
pub fn parse_diskutil_plist(xml: &str) -> Option<DiskutilInfo> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let dict = doc.descendants().find(|n| n.has_tag_name("dict"))?;
    let mut out = DiskutilInfo::default();
    let kids: Vec<_> = dict.children().filter(|n| n.is_element()).collect();
    for pair in kids.windows(2) {
        if !pair[0].has_tag_name("key") {
            continue;
        }
        let v = pair[1];
        let text = v.text().unwrap_or("").to_owned();
        let boolean = match v.tag_name().name() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
        match pair[0].text().unwrap_or("") {
            "BusProtocol" => out.bus_protocol = text,
            "SolidState" => out.solid_state = boolean,
            "VirtualOrPhysical" => out.virtual_or_physical = text,
            "Ejectable" => out.ejectable = boolean == Some(true),
            _ => {}
        }
    }
    Some(out)
}

/// The media `diskutil info` describes.
pub fn classify_diskutil(info: &DiskutilInfo) -> Media {
    let protocol = info.bus_protocol.to_lowercase();
    let solid = info.solid_state;
    let rotational = solid.map(|s| !s);
    let bus = if protocol.contains("usb") {
        "usb"
    } else if (protocol.contains("pci") || protocol.contains("fabric")) && solid == Some(true) {
        "nvme"
    } else if protocol.contains("sata") {
        "sata"
    } else {
        "other"
    };
    if info.virtual_or_physical == "Virtual" && bus == "other" {
        return if rotational.is_none() {
            Media::Virtual
        } else {
            classify("other", rotational, false)
        };
    }
    classify(bus, rotational, info.ejectable && bus == "usb")
}

#[cfg(target_os = "macos")]
fn darwin_media(mount_point: &str) -> Media {
    let Ok(out) = std::process::Command::new("diskutil")
        .args(["info", "-plist", mount_point])
        .output()
    else {
        return Media::Unknown;
    };
    if !out.status.success() {
        return Media::Unknown;
    }
    parse_diskutil_plist(&String::from_utf8_lossy(&out.stdout))
        .map(|i| classify_diskutil(&i))
        .unwrap_or(Media::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_buses() {
        assert_eq!(classify("nvme", None, false), Media::NvmeSsd);
        assert_eq!(classify("usb", Some(true), false), Media::UsbHdd);
        assert_eq!(classify("usb", Some(false), true), Media::UsbFlash);
        assert_eq!(classify("usb", None, false), Media::UsbSsd);
        assert_eq!(classify("sd", None, false), Media::SdCard);
        assert_eq!(classify("virtual", Some(true), false), Media::Virtual);
        assert_eq!(classify("sata", Some(false), false), Media::SataSsd);
        assert_eq!(classify("other", Some(false), false), Media::Ssd);
        assert_eq!(classify("sata", Some(true), false), Media::Hdd);
        assert_eq!(classify("other", None, false), Media::Unknown);
    }

    #[test]
    fn hypervisor_disks_are_virtual() {
        let tmp = tempfile::tempdir().unwrap();
        for (vendor, model, virt) in [
            ("Msft    ", "Virtual Disk    ", true),
            ("QEMU", "QEMU HARDDISK", true),
            ("ATA", "WDC WD40EFRX-68N", false),
        ] {
            let disk = tmp.path().join(model.trim().replace(' ', "_"));
            std::fs::create_dir_all(disk.join("device")).unwrap();
            std::fs::write(disk.join("device/vendor"), vendor).unwrap();
            std::fs::write(disk.join("device/model"), model).unwrap();
            assert_eq!(virtual_disk(&disk), virt, "{model}");
        }
    }

    #[test]
    fn diskutil_plist() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
            <key>BusProtocol</key><string>Apple Fabric</string>
            <key>Ejectable</key><false/>
            <key>SolidState</key><true/>
            <key>VirtualOrPhysical</key><string>Physical</string></dict></plist>"#;
        let info = parse_diskutil_plist(xml).unwrap();
        assert_eq!(info.solid_state, Some(true));
        assert_eq!(classify_diskutil(&info), Media::NvmeSsd);
        let usb = DiskutilInfo {
            bus_protocol: "USB".into(),
            solid_state: None,
            ejectable: true,
            ..Default::default()
        };
        assert_eq!(classify_diskutil(&usb), Media::UsbFlash);
    }

    #[test]
    fn media_of_the_system_volume_does_not_fail() {
        let m = media_of(&system_mount(), "", false);
        assert!(Media::ALL.contains(&m));
        assert_eq!(media_of("x", "y", true), Media::Network);
    }
}
