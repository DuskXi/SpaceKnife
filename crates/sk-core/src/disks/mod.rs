//! This computer's physical disks and which volumes live on them (the 「本机磁盘」 page; Python
//! `spaceknife.disks`).
//!
//! [`probe`] asks the system, without administrator rights and without reading a sector:
//!
//! - Windows ([`windows`]): storage property queries on each disk, the PCIe link of its controller
//!   and the USB hub's connection query (CfgMgr), the disk extents of each volume.
//! - Linux ([`linux`]): sysfs (`/sys/block`, the PCI device's link, `ata_link`, the USB device)
//!   and `/dev/disk/by-id`.
//! - macOS ([`darwin`]): `diskutil`, `system_profiler` and `ioreg`.
//!
//! Nothing here may fail the page: a probe that breaks gives fewer facts, never an error.
//! [`passthrough`] reads a Linux disk's own health record as root (the helper's `disk_health`);
//! [`health::from_pages`] turns the helper's answer into a [`DiskHealth`](sk_protocol::machine::DiskHealth).

use std::collections::BTreeMap;

use sk_protocol::machine::{DiskInterface, HealthNote, PhysicalDisk};
use sk_protocol::scan::MediaName;

use crate::volume::VolumeInfo;

pub mod darwin;
pub mod health;
pub mod links;
pub mod linux;
#[cfg(target_os = "macos")]
mod mac_smart;
pub mod passthrough;
#[cfg(windows)]
pub mod windows;

/// What [`probe`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    pub disks: Vec<PhysicalDisk>,
    /// mount point → [(disk id, bytes of the volume on that disk or None)], in disk order
    pub parts: BTreeMap<String, Vec<(String, Option<i64>)>>,
    /// mount point → where the volume starts on its first disk (orders the volumes on a disk)
    pub offsets: BTreeMap<String, i64>,
}

/// A disk with only its id filled in.
pub fn new_disk(id: &str) -> PhysicalDisk {
    PhysicalDisk {
        id: id.to_owned(),
        number: None,
        model: String::new(),
        vendor: String::new(),
        serial: String::new(),
        firmware: String::new(),
        size: None,
        media: MediaName::Unknown,
        interface: DiskInterface::Unknown,
        removable: false,
        trim: None,
        link: None,
        controller_link: None,
        system: false,
        parts: Vec::new(),
        health: None,
        health_note: HealthNote::None,
        io: None,
    }
}

/// The disks and the volumes' place on them (blocking: run it on a worker thread).
pub fn probe(volumes: &[VolumeInfo]) -> Probe {
    let run = || {
        let since = health::boot_time();
        #[cfg(windows)]
        {
            windows::probe(volumes, since)
        }
        #[cfg(target_os = "macos")]
        {
            darwin::probe(volumes, since)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            linux::probe(volumes, &linux::RealSysfs, since)
        }
    };
    // a broken probe must never break the page
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| {
        tracing::warn!("disk probe failed");
        Probe::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_this_machine() {
        // read-only: whatever this system reports, the probe never fails and stays consistent
        let volumes = crate::volume::list_volumes();
        let found = probe(&volumes);
        let ids: Vec<&str> = found.disks.iter().map(|d| d.id.as_str()).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len());
        for (mount, parts) in &found.parts {
            assert!(volumes.iter().any(|v| &v.mount_point == mount), "{mount}");
            assert!(
                parts.iter().all(|(d, _)| ids.contains(&d.as_str())),
                "{parts:?}"
            );
        }
        if cfg!(windows) {
            assert!(
                found
                    .disks
                    .iter()
                    .all(|d| Some(d.id.clone()) == d.number.map(|n| format!("PhysicalDrive{n}")))
            );
            assert!(found.disks.iter().filter(|d| d.system).count() <= 1);
            println!(
                "disks: {:#?}",
                found
                    .disks
                    .iter()
                    .map(|d| (&d.id, &d.model, d.media, &d.link, &d.health))
                    .collect::<Vec<_>>()
            );
        }
    }
}
