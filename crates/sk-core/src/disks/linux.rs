//! Linux: physical disks, their links and the volumes on them, from sysfs (no root needed; Python
//! `disks/linux.py`).
//!
//! - `/sys/block/<disk>`: size, `queue/rotational`, `removable`, `queue/discard_max_bytes` (TRIM),
//!   `device/model` / `vendor` / `serial` / `firmware_rev` / `rev`.
//! - Where the disk hangs (the real path of `/sys/block/<disk>`): NVMe: the PCI function before
//!   `/nvme/` (`current_link_speed`, `current_link_width` and the `max_` pair); SATA: `/ataN/` →
//!   `/sys/class/ata_link/linkN/sata_spd` and the controller's PCI link; USB: the USB device
//!   directory (`speed` in Mb/s, `version`); SD: `mmcblk`.
//! - `/dev/disk/by-id`: the serial of a SATA disk (`ata-<model>_<serial>`).
//! - Volumes: their device's partition (`start`, `size`) or device-mapper / md `slaves`.
//! - Since the boot: `/sys/block/<disk>/stat`; the temperature: the disk's hwmon (`nvme`, or
//!   `drivetemp` for SATA). The disk's own lifetime counters need root (an NVMe or ATA command).
//!
//! All file access goes through [`Sysfs`] (POSIX path strings), so the tests run on a fake tree on
//! any system.

use std::sync::LazyLock;

use regex::Regex;
use sk_protocol::base::DateTime;
use sk_protocol::machine::{
    DiskHealth, DiskInterface, DiskIo, DiskLink, HealthNote, HealthSource, LinkKind, PhysicalDisk,
};

use super::health::{empty_health, linux_stat};
use super::links::{gbps_text, pcie, pcie_gen_text, pcie_width_text, rate_link, usb_linux};
use super::{Probe, new_disk};
use crate::media::{Media, classify};
use crate::volume::VolumeInfo;

/// Logical, optical, floppy devices.
pub const SKIP: [&str; 9] = ["loop", "ram", "zram", "dm-", "md", "sr", "fd", "nbd", "zd"];
const VIRTUAL: [&str; 5] = [
    "msft virtual disk",
    "vmware",
    "qemu",
    "vbox",
    "virtual disk",
];

/// The filesystem as sysfs walks need it (tests pass a fake).
pub trait Sysfs {
    /// The real path (symbolic links resolved).
    fn real(&self, path: &str) -> String;
    /// The file's text, trimmed; "" when it cannot be read.
    fn read(&self, path: &str) -> String;
    /// Entry names, sorted; empty when not a directory.
    fn listdir(&self, path: &str) -> Vec<String>;
    fn exists(&self, path: &str) -> bool;
    /// The link's target as written; "" when not a link.
    fn readlink(&self, path: &str) -> String;
}

/// The real filesystem.
pub struct RealSysfs;

impl Sysfs for RealSysfs {
    fn real(&self, path: &str) -> String {
        std::fs::canonicalize(path)
            .map_or_else(|_| path.to_owned(), |p| p.to_string_lossy().into_owned())
    }
    fn read(&self, path: &str) -> String {
        std::fs::read(path)
            .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
            .unwrap_or_default()
    }
    fn listdir(&self, path: &str) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(path)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }
    fn exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }
    fn readlink(&self, path: &str) -> String {
        std::fs::read_link(path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

pub(crate) fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(crate) fn dirname(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(i) => &path[..i],
        None => "",
    }
}

fn int(text: &str) -> Option<i64> {
    text.parse().ok()
}

fn is_pci_function(name: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$").expect("regex")
    });
    RE.is_match(name)
}

/// The PCI function directory right above `before` in a sysfs device path.
fn pci_dir(path: &str, before: &str) -> Option<String> {
    let head = path.split_once(before)?.0;
    is_pci_function(basename(head)).then(|| head.to_owned())
}

fn pci_link(fs: &dyn Sysfs, pci: Option<&str>) -> Option<DiskLink> {
    let pci = pci?;
    pcie(
        pcie_gen_text(&fs.read(&format!("{pci}/current_link_speed"))),
        pcie_width_text(&fs.read(&format!("{pci}/current_link_width"))),
        pcie_gen_text(&fs.read(&format!("{pci}/max_link_speed"))),
        pcie_width_text(&fs.read(&format!("{pci}/max_link_width"))),
    )
}

/// The deepest USB device directory (2-1, 2-1.4) above the disk.
fn usb_dir(path: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+-\d+(\.\d+)*$").expect("regex"));
    let parts: Vec<&str> = path.split('/').collect();
    (1..parts.len())
        .rev()
        .find(|&i| RE.is_match(parts[i]))
        .map(|i| parts[..=i].join("/"))
}

/// The serial in /dev/disk/by-id/ata-<model>_<serial> (or scsi-SATA_…) of disk `name`.
fn by_id_serial(fs: &dyn Sysfs, name: &str, model: &str) -> String {
    let folder = "/dev/disk/by-id";
    let model_part = model.split_whitespace().collect::<Vec<_>>().join("_");
    for entry in fs.listdir(folder) {
        if entry.contains("-part") || basename(&fs.readlink(&format!("{folder}/{entry}"))) != name {
            continue;
        }
        for prefix in ["ata-", "scsi-SATA_"] {
            if let Some(rest) = entry.strip_prefix(prefix) {
                if !model_part.is_empty()
                    && let Some(serial) = rest.strip_prefix(&format!("{model_part}_"))
                {
                    return serial.to_owned();
                }
                return rest.rsplit('_').next().unwrap_or(rest).to_owned();
            }
        }
    }
    String::new()
}

fn ata_port(path: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/ata(\d+)/").expect("regex"));
    RE.captures(path).map(|m| m[1].to_owned())
}

fn interface(fs: &dyn Sysfs, name: &str, path: &str) -> DiskInterface {
    if name.starts_with("nvme") || path.contains("/nvme/") {
        return DiskInterface::Nvme;
    }
    if path.contains("/usb") {
        return DiskInterface::Usb;
    }
    if name.starts_with("mmcblk") {
        return DiskInterface::Sd;
    }
    if name.starts_with("vd")
        || name.starts_with("xvd")
        || ["/virtio", "/vmbus", "/xen", "/virtual/"]
            .iter()
            .any(|s| path.contains(s))
    {
        return DiskInterface::Virtual;
    }
    let who = format!(
        "{} {}",
        fs.read(&format!("{path}/device/vendor")),
        fs.read(&format!("{path}/device/model"))
    )
    .to_lowercase()
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ");
    if VIRTUAL.iter().any(|v| who.contains(v)) {
        return DiskInterface::Virtual;
    }
    if let Some(port) = ata_port(path) {
        return if fs
            .read(&format!("/sys/class/ata_link/link{port}/sata_spd"))
            .is_empty()
        {
            DiskInterface::Ata
        } else {
            DiskInterface::Sata
        };
    }
    if path.contains("/end_device-") || path.contains("/sas_") {
        return DiskInterface::Sas;
    }
    if path.contains("/host") && path.contains("/target") {
        return DiskInterface::Scsi;
    }
    DiskInterface::Other
}

/// The bus names [`classify`] knows.
pub(crate) fn media_bus(interface: DiskInterface) -> &'static str {
    match interface {
        DiskInterface::Nvme => "nvme",
        DiskInterface::Usb => "usb",
        DiskInterface::Sata => "sata",
        DiskInterface::Sd => "sd",
        DiskInterface::Virtual | DiskInterface::Raid => "virtual",
        _ => "other",
    }
}

/// °C from the disk's hwmon: the NVMe controller's `hwmonN` or drivetemp's `hwmon/hwmonN`.
fn hwmon_temp(fs: &dyn Sysfs, dev: &str) -> Option<i64> {
    for folder in [dev.to_owned(), format!("{dev}/hwmon")] {
        for entry in fs.listdir(&folder) {
            if entry.starts_with("hwmon")
                && entry != "hwmon"
                && let Some(milli) = int(&fs.read(&format!("{folder}/{entry}/temp1_input")))
            {
                return Some((milli as f64 / 1000.0).round() as i64);
            }
        }
    }
    None
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn disk(fs: &dyn Sysfs, name: &str, since: Option<DateTime>) -> Option<PhysicalDisk> {
    let path = fs.real(&format!("/sys/block/{name}"));
    let sectors = int(&fs.read(&format!("{path}/size")));
    let removable = fs.read(&format!("{path}/removable")) == "1";
    if sectors.unwrap_or(0) == 0 && !removable {
        return None; // an absent device (nbd, an unused md slot)
    }
    let rot = fs.read(&format!("{path}/queue/rotational"));
    let rotational = if rot.is_empty() {
        None
    } else {
        Some(rot == "1")
    };
    let discard = int(&fs.read(&format!("{path}/queue/discard_max_bytes")));
    let dev = format!("{path}/device");
    let iface = interface(fs, name, &path);
    let read = |p: &str| fs.read(&format!("{dev}/{p}"));
    let mut model = Some(read("model"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| read("name"));
    let mut vendor = read("vendor");
    let mut serial = read("serial");
    let firmware = Some(read("firmware_rev"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| read("rev"));
    let mut link = None;
    let mut controller = None;
    match iface {
        DiskInterface::Nvme => link = pci_link(fs, pci_dir(&path, "/nvme/").as_deref()),
        DiskInterface::Sata | DiskInterface::Ata => {
            if let Some(port) = ata_port(&path) {
                let rate = gbps_text(&fs.read(&format!("/sys/class/ata_link/link{port}/sata_spd")));
                link = rate.map(|r| rate_link(LinkKind::Sata, Some(r), None));
                controller = pci_link(fs, pci_dir(&path, &format!("/ata{port}/")).as_deref());
            }
            if serial.is_empty() {
                serial = by_id_serial(fs, name, &model);
            }
        }
        DiskInterface::Usb => {
            if let Some(usb) = usb_dir(&path) {
                link = usb_linux(
                    &fs.read(&format!("{usb}/speed")),
                    &fs.read(&format!("{usb}/version")),
                );
                if model.is_empty() {
                    model = fs.read(&format!("{usb}/product"));
                    if vendor.is_empty() {
                        vendor = fs.read(&format!("{usb}/manufacturer"));
                    }
                }
                if serial.is_empty() {
                    serial = fs.read(&format!("{usb}/serial"));
                }
            }
        }
        DiskInterface::Sd => {
            if let Some(s) = serial.strip_prefix("0x") {
                serial = s.to_owned();
            }
        }
        _ => {}
    }
    let io = linux_stat(&fs.read(&format!("{path}/stat"))).map(|(r, w)| DiskIo {
        read_bytes: r,
        written_bytes: w,
        since,
    });
    let media = classify(media_bus(iface), rotational, removable);
    let mut health: Option<DiskHealth> = None;
    let mut note = HealthNote::None;
    match iface {
        DiskInterface::Nvme | DiskInterface::Sata | DiskInterface::Ata => {
            if let Some(temp) = hwmon_temp(fs, &dev) {
                let source = if iface == DiskInterface::Nvme {
                    HealthSource::Nvme
                } else {
                    HealthSource::Ata
                };
                health = Some(DiskHealth {
                    temperature_c: Some(temp),
                    ..empty_health(source)
                });
            }
            note = HealthNote::NeedsAdmin; // the lifetime counters: an NVMe or ATA command, root only
        }
        DiskInterface::Usb => {
            // a drive in an enclosure may pass ATA commands through its bridge (root, on
            // request); sticks and card readers don't
            note = if matches!(media, Media::UsbHdd | Media::UsbSsd) && name.starts_with("sd") {
                HealthNote::NeedsAdmin
            } else {
                HealthNote::Usb
            };
        }
        _ => {}
    }
    let vendor_up = vendor.trim().to_uppercase();
    let has_size = sectors.unwrap_or(0) != 0;
    Some(PhysicalDisk {
        model: squash(&model),
        vendor: if ["", "ATA", "NVME"].contains(&vendor_up.as_str()) {
            String::new()
        } else {
            squash(&vendor)
        },
        serial: serial.trim().to_owned(),
        firmware: firmware.trim().to_owned(),
        size: sectors.filter(|&s| s != 0).map(|s| s * 512),
        media,
        interface: iface,
        removable,
        trim: discard.map(|d| d > 0),
        link,
        controller_link: controller,
        health,
        health_note: if has_size { note } else { HealthNote::None },
        io,
        ..new_disk(name)
    })
}

/// (disk, bytes on it, start) under a block device: a partition's disk, a whole disk, or the disks
/// under a device-mapper / md device.
fn leaves(fs: &dyn Sysfs, name: &str, depth: usize) -> Vec<(String, Option<i64>, Option<i64>)> {
    let path = fs.real(&format!("/sys/class/block/{name}"));
    if depth > 8 || !fs.exists(&path) {
        return Vec::new();
    }
    let size = int(&fs.read(&format!("{path}/size")))
        .filter(|&s| s != 0)
        .map(|s| s * 512);
    if fs.exists(&format!("{path}/partition")) {
        let start = int(&fs.read(&format!("{path}/start"))).map(|s| s * 512);
        return vec![(basename(dirname(&path)).to_owned(), size, start)];
    }
    let slaves = fs.listdir(&format!("{path}/slaves"));
    if slaves.is_empty() {
        return vec![(name.to_owned(), size, Some(0))];
    }
    let found: Vec<_> = slaves
        .iter()
        .flat_map(|s| leaves(fs, s, depth + 1))
        .collect();
    if found.len() == 1 {
        // one disk under it (LVM on one partition, LUKS): this volume's size
        let (disk, inner, start) = found.into_iter().next().expect("one");
        let smallest = [size, inner].into_iter().flatten().min();
        return vec![(disk, smallest, start)];
    }
    // spread over several: unknown split
    found
        .into_iter()
        .map(|(d, _, start)| (d, None, start))
        .collect()
}

pub fn probe(volumes: &[VolumeInfo], fs: &dyn Sysfs, since: Option<DateTime>) -> Probe {
    let mut result = Probe::default();
    for name in fs.listdir("/sys/block") {
        if SKIP.iter().any(|s| name.starts_with(s)) {
            continue;
        }
        // one odd device must not hide the others
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| disk(fs, &name, since))) {
            Ok(Some(d)) => result.disks.push(d),
            Ok(None) => {}
            Err(_) => tracing::debug!("disk {name} probe failed"),
        }
    }
    let known: std::collections::HashSet<String> =
        result.disks.iter().map(|d| d.id.clone()).collect();
    for v in volumes {
        if v.is_remote || !v.device.starts_with("/dev/") {
            continue;
        }
        let found: Vec<_> = leaves(fs, basename(&fs.real(&v.device)), 0)
            .into_iter()
            .filter(|leaf| known.contains(&leaf.0))
            .collect();
        if found.is_empty() {
            continue;
        }
        result.parts.insert(
            v.mount_point.clone(),
            found.iter().map(|(d, s, _)| (d.clone(), *s)).collect(),
        );
        if let Some(start) = found[0].2 {
            result.offsets.insert(v.mount_point.clone(), start);
        }
        if v.mount_point == "/" {
            let on: Vec<&String> = found.iter().map(|(d, _, _)| d).collect();
            for d in &mut result.disks {
                if on.contains(&&d.id) {
                    d.system = true;
                }
            }
        }
    }
    result
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// A tree of files and symbolic links (absolute or relative targets), POSIX paths.
    #[derive(Default)]
    pub(crate) struct FakeFs {
        pub files: BTreeMap<String, String>,
        pub links: BTreeMap<String, String>,
    }

    fn normpath(path: &str) -> String {
        let mut out: Vec<&str> = Vec::new();
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    out.pop();
                }
                p => out.push(p),
            }
        }
        format!("/{}", out.join("/"))
    }

    impl FakeFs {
        pub fn file(&mut self, path: &str, text: &str) {
            self.files.insert(path.into(), text.into());
        }
        pub fn link(&mut self, path: &str, target: &str) {
            self.links.insert(path.into(), target.into());
        }
        fn all(&self) -> impl Iterator<Item = &String> {
            self.files.keys().chain(self.links.keys())
        }
    }

    impl Sysfs for FakeFs {
        fn real(&self, path: &str) -> String {
            let mut out = String::new();
            for part in path.trim_matches('/').split('/') {
                out = format!("{out}/{part}");
                for _ in 0..16 {
                    let Some(target) = self.links.get(&out) else {
                        break;
                    };
                    out = if target.starts_with('/') {
                        normpath(target)
                    } else {
                        normpath(&format!("{}/{target}", dirname(&out)))
                    };
                }
            }
            out
        }
        fn read(&self, path: &str) -> String {
            self.files
                .get(&self.real(path))
                .map(|s| s.trim().to_owned())
                .unwrap_or_default()
        }
        fn listdir(&self, path: &str) -> Vec<String> {
            let base = format!("{}/", self.real(path).trim_end_matches('/'));
            let mut names: Vec<String> = self
                .all()
                .filter_map(|p| p.strip_prefix(&base))
                .map(|rest| rest.split('/').next().unwrap_or(rest).to_owned())
                .collect();
            names.sort();
            names.dedup();
            names
        }
        fn exists(&self, path: &str) -> bool {
            let r = self.real(path);
            self.files.contains_key(&r) || self.all().any(|p| p.starts_with(&format!("{r}/")))
        }
        fn readlink(&self, path: &str) -> String {
            self.links.get(path).cloned().unwrap_or_default()
        }
    }

    fn disk_at(
        fs: &mut FakeFs,
        name: &str,
        path: &str,
        sectors: i64,
        rot: &str,
        removable: &str,
    ) -> String {
        fs.link(&format!("/sys/block/{name}"), path);
        fs.link(&format!("/sys/class/block/{name}"), path);
        fs.file(&format!("{path}/size"), &sectors.to_string());
        fs.file(&format!("{path}/queue/rotational"), rot);
        fs.file(&format!("{path}/removable"), removable);
        fs.file(
            &format!("{path}/queue/discard_max_bytes"),
            if rot == "1" { "0" } else { "2147450880" },
        );
        path.to_owned()
    }

    fn part(fs: &mut FakeFs, disk_path: &str, name: &str, start: i64, sectors: i64) {
        let path = format!("{disk_path}/{name}");
        fs.link(&format!("/sys/class/block/{name}"), &path);
        fs.file(&format!("{path}/partition"), "1");
        fs.file(&format!("{path}/start"), &start.to_string());
        fs.file(&format!("{path}/size"), &sectors.to_string());
    }

    fn pci(fs: &mut FakeFs, path: &str, speed: &str, width: &str, top: &str, top_width: &str) {
        fs.file(&format!("{path}/current_link_speed"), speed);
        fs.file(&format!("{path}/current_link_width"), width);
        fs.file(&format!("{path}/max_link_speed"), top);
        fs.file(&format!("{path}/max_link_width"), top_width);
    }

    pub(crate) fn vol(mount: &str, device: &str) -> VolumeInfo {
        VolumeInfo {
            mount_point: mount.into(),
            device: device.into(),
            fs_type: "ext4".into(),
            total: 1_000_000_000_000,
            free: 100_000_000_000,
            cluster_size: 4096,
            ..Default::default()
        }
    }

    pub(crate) fn machine() -> FakeFs {
        let mut fs = FakeFs::default();
        // an NVMe drive (PCIe 5.0 capable, in a 4.0 slot) holding the system
        let nvme_pci = "/sys/devices/pci0000:00/0000:00:1d.0/0000:3d:00.0";
        let nvme = disk_at(
            &mut fs,
            "nvme0n1",
            &format!("{nvme_pci}/nvme/nvme0/nvme0n1"),
            7814037168,
            "0",
            "0",
        );
        fs.link(&format!("{nvme}/device"), &format!("{nvme_pci}/nvme/nvme0"));
        fs.file(
            &format!("{nvme_pci}/nvme/nvme0/model"),
            "ZHITAI TiPro9000 4TB                    ",
        );
        fs.file(
            &format!("{nvme_pci}/nvme/nvme0/serial"),
            "NVME0000000000000001  ",
        );
        fs.file(&format!("{nvme_pci}/nvme/nvme0/firmware_rev"), "ZTA31003");
        pci(
            &mut fs,
            nvme_pci,
            "16.0 GT/s PCIe",
            "4",
            "32.0 GT/s PCIe",
            "4",
        );
        part(&mut fs, &nvme, "nvme0n1p1", 2048, 1048576);
        part(&mut fs, &nvme, "nvme0n1p2", 1050624, 7812986544);
        // a SATA SSD on an add-in controller with one PCIe 2.0 lane
        let card = "/sys/devices/pci0000:00/0000:00:1c.4/0000:03:00.0";
        let sda = disk_at(
            &mut fs,
            "sda",
            &format!("{card}/ata5/host4/target4:0:0/4:0:0:0/block/sda"),
            3907029168,
            "0",
            "0",
        );
        let sda_dev = format!("{card}/ata5/host4/target4:0:0/4:0:0:0");
        fs.link(&format!("{sda}/device"), &sda_dev);
        fs.file(&format!("{sda_dev}/vendor"), "ATA     ");
        fs.file(&format!("{sda_dev}/model"), "CT2000MX500SSD1 ");
        fs.file(&format!("{sda_dev}/rev"), "M3CR");
        fs.file("/sys/class/ata_link/link5/sata_spd", "6.0 Gbps");
        pci(&mut fs, card, "5.0 GT/s PCIe", "1", "5.0 GT/s PCIe", "1");
        fs.link(
            "/dev/disk/by-id/ata-CT2000MX500SSD1_0000E0000001",
            "../../sda",
        );
        fs.link(
            "/dev/disk/by-id/ata-CT2000MX500SSD1_0000E0000001-part1",
            "../../sda1",
        );
        part(&mut fs, &sda, "sda1", 2048, 3907026944);
        // a hard disk on the chipset's SATA (no link of its own), under LVM
        let chip = "/sys/devices/pci0000:00/0000:00:17.0";
        let sdb = disk_at(
            &mut fs,
            "sdb",
            &format!("{chip}/ata1/host0/target0:0:0/0:0:0:0/block/sdb"),
            7814037168,
            "1",
            "0",
        );
        fs.link(
            &format!("{sdb}/device"),
            &format!("{chip}/ata1/host0/target0:0:0/0:0:0:0"),
        );
        fs.file(
            &format!("{chip}/ata1/host0/target0:0:0/0:0:0:0/model"),
            "WDC WD40EFRX-68N32N0",
        );
        fs.file("/sys/class/ata_link/link1/sata_spd", "3.0 Gbps");
        pci(&mut fs, chip, "Unknown", "0", "Unknown", "0");
        part(&mut fs, &sdb, "sdb1", 2048, 7814035087);
        let lv = "/sys/devices/virtual/block/dm-0";
        fs.link("/sys/block/dm-0", lv);
        fs.link("/sys/class/block/dm-0", lv);
        fs.file(&format!("{lv}/size"), "4000000000");
        fs.link(&format!("{lv}/slaves/sdb1"), &format!("{sdb}/sdb1"));
        fs.link("/dev/mapper/vg-media", "../dm-0");
        // a USB 3 card reader on a USB 2 port
        let reader = "/sys/devices/pci0000:00/0000:00:14.0/usb1/1-4/1-4.2";
        let sdc_dev = format!("{reader}/1-4.2:1.0/host6/target6:0:0/6:0:0:0");
        let sdc = disk_at(
            &mut fs,
            "sdc",
            &format!("{sdc_dev}/block/sdc"),
            998244352,
            "0",
            "1",
        );
        fs.link(&format!("{sdc}/device"), &sdc_dev);
        fs.file(&format!("{sdc_dev}/vendor"), "Generic ");
        fs.file(&format!("{sdc_dev}/model"), "STORAGE DEVICE  ");
        fs.file(&format!("{reader}/speed"), "480");
        fs.file(&format!("{reader}/version"), " 3.00");
        fs.file(&format!("{reader}/serial"), "000000001532");
        // an SD card on the SoC, a virtio disk, a loop device and an empty nbd
        let mmc = "/sys/devices/platform/soc/fe340000.mmc/mmc_host/mmc0/mmc0:aaaa";
        let sd = disk_at(
            &mut fs,
            "mmcblk0",
            &format!("{mmc}/block/mmcblk0"),
            124735488,
            "0",
            "0",
        );
        fs.link(&format!("{sd}/device"), mmc);
        fs.file(&format!("{mmc}/name"), "SC64G");
        fs.file(&format!("{mmc}/serial"), "0x1234abcd");
        disk_at(
            &mut fs,
            "vda",
            "/sys/devices/pci0000:00/0000:00:04.0/virtio1/block/vda",
            20971520,
            "1",
            "0",
        );
        disk_at(
            &mut fs,
            "loop0",
            "/sys/devices/virtual/block/loop0",
            1000,
            "0",
            "0",
        );
        disk_at(
            &mut fs,
            "nbd0",
            "/sys/devices/virtual/block/nbd0",
            0,
            "0",
            "0",
        );
        fs
    }

    fn by_id(p: &Probe) -> BTreeMap<String, PhysicalDisk> {
        p.disks.iter().map(|d| (d.id.clone(), d.clone())).collect()
    }

    #[test]
    fn linux_disks() {
        let fs = machine();
        let volumes = [
            vol("/", "/dev/nvme0n1p2"),
            vol("/boot/efi", "/dev/nvme0n1p1"),
            vol("/data", "/dev/sda1"),
            vol("/media", "/dev/mapper/vg-media"),
            vol("/run/media/card", "/dev/sdc"),
            vol("/mnt/share", "server:/export"),
        ];
        let found = probe(&volumes, &fs, None);
        let disks = by_id(&found);
        assert_eq!(
            disks.keys().cloned().collect::<Vec<_>>(),
            ["mmcblk0", "nvme0n1", "sda", "sdb", "sdc", "vda"]
        );

        let nvme = &disks["nvme0n1"];
        assert_eq!(
            (
                nvme.model.as_str(),
                nvme.serial.as_str(),
                nvme.firmware.as_str()
            ),
            ("ZHITAI TiPro9000 4TB", "NVME0000000000000001", "ZTA31003")
        );
        assert_eq!(
            (
                nvme.interface,
                nvme.media,
                nvme.size,
                nvme.trim,
                nvme.system
            ),
            (
                DiskInterface::Nvme,
                Media::NvmeSsd,
                Some(7814037168 * 512),
                Some(true),
                true
            )
        );
        assert_eq!(nvme.link, pcie(Some(4), Some(4), Some(5), Some(4)));

        let sata = &disks["sda"];
        assert_eq!(
            (
                sata.model.as_str(),
                sata.vendor.as_str(),
                sata.serial.as_str(),
                sata.media
            ),
            ("CT2000MX500SSD1", "", "0000E0000001", Media::SataSsd)
        );
        assert_eq!(sata.link, Some(rate_link(LinkKind::Sata, Some(6000), None)));
        assert_eq!(
            sata.controller_link,
            pcie(Some(2), Some(1), Some(2), Some(1))
        );

        let hdd = &disks["sdb"];
        assert_eq!(
            (hdd.media, hdd.trim, &hdd.controller_link),
            (Media::Hdd, Some(false), &None)
        ); // chipset: no link reported
        assert_eq!(hdd.link, Some(rate_link(LinkKind::Sata, Some(3000), None)));

        let reader = &disks["sdc"];
        assert_eq!(
            (
                reader.interface,
                reader.vendor.as_str(),
                reader.model.as_str(),
                reader.serial.as_str()
            ),
            (
                DiskInterface::Usb,
                "Generic",
                "STORAGE DEVICE",
                "000000001532"
            )
        );
        assert!(reader.removable);
        assert_eq!(
            reader.link,
            Some(rate_link(LinkKind::Usb, Some(480), Some(5000)))
        );

        let card = &disks["mmcblk0"];
        assert_eq!(
            (
                card.interface,
                card.media,
                card.model.as_str(),
                card.serial.as_str()
            ),
            (DiskInterface::Sd, Media::SdCard, "SC64G", "1234abcd")
        );
        assert_eq!(
            (disks["vda"].interface, disks["vda"].media),
            (DiskInterface::Virtual, Media::Virtual)
        );

        // volumes: their disk, their size on it, and where they start
        assert_eq!(
            found.parts["/"],
            vec![("nvme0n1".to_owned(), Some(7812986544 * 512))]
        );
        assert_eq!(found.offsets["/boot/efi"], 2048 * 512);
        assert!(found.offsets["/boot/efi"] < found.offsets["/"]);
        assert_eq!(
            found.parts["/data"],
            vec![("sda".to_owned(), Some(3907026944 * 512))]
        );
        assert_eq!(
            found.parts["/media"],
            vec![("sdb".to_owned(), Some(4000000000 * 512))]
        ); // the LV, not the whole PV
        assert_eq!(
            found.parts["/run/media/card"],
            vec![("sdc".to_owned(), Some(998244352 * 512))]
        ); // no partitions
        assert!(!found.parts.contains_key("/mnt/share"));
    }

    #[test]
    fn linux_volume_over_two_disks() {
        let mut fs = machine();
        let lv = "/sys/devices/virtual/block/dm-1";
        fs.link("/sys/class/block/dm-1", lv);
        fs.file(&format!("{lv}/size"), "9000000000");
        let sda1 = fs.real("/sys/class/block/sda1");
        let sdb1 = fs.real("/sys/class/block/sdb1");
        fs.link(&format!("{lv}/slaves/sda1"), &sda1);
        fs.link(&format!("{lv}/slaves/sdb1"), &sdb1);
        fs.link("/dev/mapper/vg-big", "../dm-1");
        let found = probe(&[vol("/big", "/dev/mapper/vg-big")], &fs, None);
        assert_eq!(
            found.parts["/big"],
            vec![("sda".to_owned(), None), ("sdb".to_owned(), None)]
        ); // split unknown
    }

    #[test]
    fn linux_health_and_counters() {
        let mut fs = machine();
        let nvme_ctrl = "/sys/devices/pci0000:00/0000:00:1d.0/0000:3d:00.0/nvme/nvme0";
        fs.file(&format!("{nvme_ctrl}/hwmon3/temp1_input"), "47850");
        let nvme = fs.real("/sys/block/nvme0n1");
        fs.file(&format!("{nvme}/stat"), "1 0 2000 0 1 0 4000 0 0 0 0");
        let sda_dev = fs.real("/sys/block/sda/device");
        fs.file(&format!("{sda_dev}/hwmon/hwmon5/temp1_input"), "31000");
        let disks = by_id(&probe(&[], &fs, None));
        let nvme = &disks["nvme0n1"];
        let h = nvme.health.as_ref().unwrap();
        assert_eq!((h.source, h.temperature_c), (HealthSource::Nvme, Some(48)));
        assert_eq!(
            (h.written_bytes, nvme.health_note),
            (None, HealthNote::NeedsAdmin)
        ); // lifetime: root only
        let io = nvme.io.as_ref().unwrap();
        assert_eq!((io.read_bytes, io.written_bytes), (2000 * 512, 4000 * 512));
        assert_eq!(
            disks["sda"].health.as_ref().unwrap().temperature_c,
            Some(31)
        );
        assert_eq!(
            (disks["sdb"].health.is_none(), disks["sdb"].health_note),
            (true, HealthNote::NeedsAdmin)
        ); // no drivetemp
        assert_eq!(
            (disks["sdc"].health.is_none(), disks["sdc"].health_note),
            (true, HealthNote::Usb)
        );
    }

    #[test]
    fn linux_drive_in_an_enclosure_can_be_read_as_root() {
        let mut fs = machine();
        let sdc = fs.real("/sys/block/sdc");
        fs.file(&format!("{sdc}/removable"), "0");
        fs.file(&format!("{sdc}/queue/rotational"), "1");
        let drive = by_id(&probe(&[], &fs, None))["sdc"].clone();
        assert_eq!(
            (drive.media, drive.health_note),
            (Media::UsbHdd, HealthNote::NeedsAdmin)
        );
    }
}
