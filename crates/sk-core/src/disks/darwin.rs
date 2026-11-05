//! macOS: physical disks, their links and the volumes on them (no root needed; Python
//! `disks/darwin.py`).
//!
//! - `diskutil list -plist`: the whole disks; `diskutil info -plist diskN`: model, protocol, solid
//!   state, removable, size, whether it is physical (APFS containers are synthesized disks).
//! - `diskutil info -plist <mount point>`: the volume's disk (`APFSPhysicalStores` for APFS, else
//!   `ParentWholeDisk`).
//! - `system_profiler -json` (NVMe, SATA, USB): serial, firmware, TRIM and the links (NVMe "8.0
//!   GT/s" and "x4", the SATA controller's negotiated speed, a USB device's speed), by BSD name.
//! - `ioreg -r -c IOBlockStorageDriver -a`: each disk's bytes read and written since the boot.
//!
//! The parsing is in pure functions over the decoded plist / JSON (as [`serde_json::Value`]: see
//! [`plist_to_json`]), tested on every platform with recorded shapes.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use sk_protocol::machine::{DiskInterface, DiskLink, LinkKind, PhysicalDisk};

use super::links::{gbps_text, pcie, pcie_gen_text, pcie_width_text, rate_link, usb_mac};
use super::linux::media_bus;
use super::new_disk;
use crate::media::classify;

static WHOLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(disk\d+)").expect("regex"));

fn whole_disk_name(s: &str) -> bool {
    WHOLE.find(s).is_some_and(|m| m.end() == s.len())
}

/// A plist value as JSON (data becomes null, dates RFC 3339 strings).
pub fn plist_to_json(v: &plist::Value) -> Value {
    match v {
        plist::Value::Array(a) => Value::Array(a.iter().map(plist_to_json).collect()),
        plist::Value::Dictionary(d) => Value::Object(
            d.iter()
                .map(|(k, v)| (k.clone(), plist_to_json(v)))
                .collect(),
        ),
        plist::Value::Boolean(b) => Value::Bool(*b),
        plist::Value::Integer(i) => i
            .as_signed()
            .map(Value::from)
            .or_else(|| i.as_unsigned().map(Value::from))
            .unwrap_or(Value::Null),
        plist::Value::Real(f) => {
            serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number)
        }
        plist::Value::String(s) => Value::String(s.clone()),
        plist::Value::Date(d) => Value::String(d.to_xml_format()),
        _ => Value::Null,
    }
}

/// A plist document (XML or binary) as JSON; None when it is not one.
pub fn parse_plist(raw: &[u8]) -> Option<Value> {
    plist::Value::from_reader(std::io::Cursor::new(raw))
        .ok()
        .map(|v| plist_to_json(&v))
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}

fn positive_int(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_i64).filter(|&n| n > 0)
}

pub fn interface_of(protocol: &str) -> DiskInterface {
    let p = protocol.to_lowercase();
    if p.contains("pci") || p.contains("fabric") || p.contains("nvme") {
        DiskInterface::Nvme
    } else if p.contains("usb") {
        DiskInterface::Usb
    } else if p.contains("sata") {
        DiskInterface::Sata
    } else if p.contains("thunderbolt") {
        DiskInterface::Thunderbolt
    } else if p.contains("secure digital") || p == "sd" {
        DiskInterface::Sd
    } else if p.contains("image") || p.contains("virtual") {
        DiskInterface::Virtual
    } else if p.contains("sas") {
        DiskInterface::Sas
    } else if p.is_empty() {
        DiskInterface::Unknown
    } else {
        DiskInterface::Other
    }
}

/// A physical disk from `diskutil info -plist diskN` (None for a synthesized one).
pub fn disk_from_info(info: &Value) -> Option<PhysicalDisk> {
    let ident = text(info.get("DeviceIdentifier"));
    if !whole_disk_name(&ident)
        || info.get("VirtualOrPhysical").and_then(Value::as_str) == Some("Virtual")
    {
        return None;
    }
    let iface = interface_of(&text(info.get("BusProtocol")));
    let rotational = match info.get("SolidState") {
        None | Some(Value::Null) => None,
        Some(v) => Some(!truthy(Some(v))),
    };
    let removable = truthy(info.get("Removable"))
        || truthy(info.get("RemovableMedia"))
        || truthy(info.get("Ejectable"));
    let size = positive_int(info.get("TotalSize"))
        .or_else(|| positive_int(info.get("Size")))
        .or_else(|| positive_int(info.get("IOKitSize")));
    let model = [info.get("MediaName"), info.get("IORegistryEntryName")]
        .into_iter()
        .map(text)
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    Some(PhysicalDisk {
        model: model
            .replace(" Media", "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        size,
        media: classify(
            media_bus(iface),
            rotational,
            removable && iface == DiskInterface::Usb,
        ),
        interface: iface,
        removable,
        ..new_disk(&ident)
    })
}

/// What `system_profiler` tells of one disk.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProfileFacts {
    pub serial: Option<String>,
    pub firmware: Option<String>,
    pub model: Option<String>,
    pub trim: Option<bool>,
    pub link: Option<DiskLink>,
}

/// Every object in the profile with the objects above it (nearest last).
fn walk<'a>(
    node: &'a Value,
    parents: &mut Vec<&'a Value>,
    out: &mut Vec<(&'a Value, Vec<&'a Value>)>,
) {
    match node {
        Value::Object(map) => {
            out.push((node, parents.clone()));
            parents.push(node);
            for v in map.values() {
                walk(v, parents, out);
            }
            parents.pop();
        }
        Value::Array(items) => {
            for item in items {
                walk(item, parents, out);
            }
        }
        _ => {}
    }
}

fn yes(v: &Value) -> bool {
    let t = match v {
        Value::String(s) => s.to_lowercase(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string().to_lowercase(),
    };
    ["yes", "spsata_yes", "true", "1"].contains(&t.as_str()) || t.ends_with("_yes")
}

/// BSD name → serial, firmware, model, trim, link from `system_profiler -json`.
pub fn profile_facts(profile: &Value) -> BTreeMap<String, ProfileFacts> {
    let mut facts: BTreeMap<String, ProfileFacts> = BTreeMap::new();
    let mut all = Vec::new();
    walk(profile, &mut Vec::new(), &mut all);
    for (d, parents) in all {
        let Some(bsd) = d.get("bsd_name").and_then(Value::as_str) else {
            continue;
        };
        if !whole_disk_name(bsd) {
            continue;
        }
        let f = facts.entry(bsd.to_owned()).or_default();
        for (key, slot) in [
            ("device_serial", &mut f.serial),
            ("device_revision", &mut f.firmware),
            ("device_model", &mut f.model),
        ] {
            if let Some(s) = d
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                slot.get_or_insert_with(|| s.to_owned());
            }
        }
        if let Some(trim) = d
            .get("spnvme_trim_support")
            .or_else(|| d.get("spsata_trim_support"))
        {
            f.trim.get_or_insert(yes(trim));
        }
        let mut link = None;
        if d.get("spnvme_linkspeed").is_some() || d.get("spnvme_linkwidth").is_some() {
            link = pcie(
                pcie_gen_text(&text(d.get("spnvme_linkspeed"))),
                pcie_width_text(&text(d.get("spnvme_linkwidth"))),
                None,
                None,
            );
        } else {
            // the SATA controller, or the USB device holding the medium
            for above in parents.iter().rev() {
                if let Some(neg) = above.get("spsata_negotiatedlinkspeed") {
                    let rate = gbps_text(&text(Some(neg)));
                    let top = gbps_text(&text(above.get("spsata_linkspeed")));
                    link = (rate.is_some() || top.is_some())
                        .then(|| rate_link(LinkKind::Sata, rate, top));
                    break;
                }
                if let Some(speed) = above
                    .get("device_speed")
                    .or_else(|| above.get("USBDeviceKeyLinkSpeed"))
                    .and_then(Value::as_str)
                {
                    link = usb_mac(speed);
                    break;
                }
            }
        }
        if link.is_some() && f.link.is_none() {
            f.link = link;
        }
    }
    facts
}

/// The disks of a volume from `diskutil info -plist <mount point>`: APFS volumes share their
/// container (no size of their own on the disk); others take their partition.
pub fn volume_disks(info: &Value) -> Vec<(String, Option<i64>)> {
    if let Some(stores) = info
        .get("APFSPhysicalStores")
        .and_then(Value::as_array)
        .filter(|s| !s.is_empty())
    {
        return stores
            .iter()
            .filter_map(|s| s.get("APFSPhysicalStore").and_then(Value::as_str))
            .filter_map(|s| WHOLE.captures(s).map(|m| (m[1].to_owned(), None)))
            .collect();
    }
    let parent = text(info.get("ParentWholeDisk"));
    if !whole_disk_name(&parent) {
        return Vec::new();
    }
    let size = positive_int(info.get("TotalSize")).or_else(|| positive_int(info.get("Size")));
    vec![(parent, size)]
}

/// BSD name → (bytes read, bytes written) from `ioreg -r -c IOBlockStorageDriver -a`: each
/// driver's Statistics, for the whole-disk IOMedia right under it.
pub fn io_counters(tree: &Value) -> BTreeMap<String, (i64, i64)> {
    let mut out = BTreeMap::new();
    let mut all = Vec::new();
    walk(tree, &mut Vec::new(), &mut all);
    for (d, _) in all {
        let Some(stats) = d.get("Statistics").filter(|s| s.is_object()) else {
            continue;
        };
        let (Some(read), Some(written)) = (
            stats.get("Bytes (Read)").and_then(Value::as_i64),
            stats.get("Bytes (Write)").and_then(Value::as_i64),
        ) else {
            continue;
        };
        let mut children = Vec::new();
        if let Some(c) = d.get("IORegistryEntryChildren") {
            walk(c, &mut Vec::new(), &mut children);
        }
        for (child, _) in children {
            if let Some(bsd) = child
                .get("BSD Name")
                .and_then(Value::as_str)
                .filter(|b| whole_disk_name(b))
            {
                out.entry(bsd.to_owned()).or_insert((read, written));
                break;
            }
        }
    }
    out
}

#[cfg(target_os = "macos")]
pub(crate) fn run_plist(program: &str, args: &[&str]) -> Option<Value> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_plist(&out.stdout)
}

#[cfg(target_os = "macos")]
pub fn probe(
    volumes: &[crate::volume::VolumeInfo],
    since: Option<sk_protocol::base::DateTime>,
) -> super::Probe {
    use sk_protocol::machine::{DiskIo, HealthNote};

    use super::health::nvme_health;

    let mut result = super::Probe::default();
    let listing = run_plist("diskutil", &["list", "-plist"]).unwrap_or(Value::Null);
    let whole: Vec<String> = listing
        .get("WholeDisks")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter(|s| whole_disk_name(s))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let profile = std::process::Command::new("system_profiler")
        .args([
            "-json",
            "SPNVMeDataType",
            "SPSerialATADataType",
            "SPUSBDataType",
            "SPUSBHostDataType",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .unwrap_or(Value::Null);
    let facts = profile_facts(&profile);
    let counters = io_counters(
        &run_plist("ioreg", &["-r", "-c", "IOBlockStorageDriver", "-a"]).unwrap_or(Value::Null),
    );
    let logs = super::mac_smart::nvme_smart_logs();
    for ident in whole {
        let Some(info) = run_plist("diskutil", &["info", "-plist", &ident]) else {
            continue;
        };
        let Some(mut disk) = disk_from_info(&info) else {
            continue;
        };
        let f = facts.get(&ident).cloned().unwrap_or_default();
        let health = logs.get(&ident).and_then(|l| nvme_health(l));
        disk.serial = f.serial.unwrap_or_default();
        disk.firmware = f.firmware.unwrap_or_default();
        if disk.model.is_empty() {
            disk.model = f.model.unwrap_or_default();
        }
        disk.trim = f.trim;
        disk.link = f.link;
        disk.health_note = if health.is_some() {
            HealthNote::None
        } else if disk.interface == DiskInterface::Usb {
            HealthNote::Usb
        } else {
            HealthNote::Unavailable
        };
        disk.health = health;
        disk.io = counters.get(&ident).map(|&(r, w)| DiskIo {
            read_bytes: r,
            written_bytes: w,
            since,
        });
        result.disks.push(disk);
    }
    let known: Vec<String> = result.disks.iter().map(|d| d.id.clone()).collect();
    for v in volumes {
        if v.is_remote || !v.reason.is_empty() {
            continue;
        }
        let info =
            run_plist("diskutil", &["info", "-plist", &v.mount_point]).unwrap_or(Value::Null);
        let parts: Vec<_> = volume_disks(&info)
            .into_iter()
            .filter(|(d, _)| known.contains(d))
            .collect();
        if parts.is_empty() {
            continue;
        }
        if v.mount_point == "/" {
            for d in &mut result.disks {
                if parts.iter().any(|(id, _)| *id == d.id) {
                    d.system = true;
                }
            }
        }
        result.parts.insert(v.mount_point.clone(), parts);
    }
    if result.disks.is_empty() {
        tracing::debug!("no physical disks from diskutil");
    }
    result
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sk_protocol::scan::MediaName;

    use super::*;

    #[test]
    fn mac_disk_info() {
        let nvme = disk_from_info(&json!({
            "DeviceIdentifier": "disk0", "BusProtocol": "PCI-Express", "MediaName": "APPLE SSD AP0512Q",
            "SolidState": true, "Removable": false, "Internal": true, "TotalSize": 500277790720u64,
            "VirtualOrPhysical": "Physical"
        }))
        .unwrap();
        assert_eq!(
            (
                nvme.id.as_str(),
                nvme.model.as_str(),
                nvme.interface,
                nvme.media,
                nvme.size
            ),
            (
                "disk0",
                "APPLE SSD AP0512Q",
                DiskInterface::Nvme,
                MediaName::NvmeSsd,
                Some(500277790720)
            )
        );
        let stick = disk_from_info(&json!({
            "DeviceIdentifier": "disk4", "BusProtocol": "USB", "MediaName": "SanDisk Ultra Media",
            "SolidState": false, "Removable": true, "Ejectable": true, "Size": 61530439680u64
        }))
        .unwrap();
        assert_eq!(
            (stick.model.as_str(), stick.interface, stick.removable),
            ("SanDisk Ultra", DiskInterface::Usb, true)
        );
        assert!(
            disk_from_info(&json!({"DeviceIdentifier": "disk3", "VirtualOrPhysical": "Virtual"}))
                .is_none()
        ); // APFS
        assert!(disk_from_info(&json!({"DeviceIdentifier": "disk0s2"})).is_none());
        let kinds: Vec<_> = ["Apple Fabric", "SATA", "Thunderbolt", "Disk Image", ""]
            .map(interface_of)
            .to_vec();
        assert_eq!(
            kinds,
            [
                DiskInterface::Nvme,
                DiskInterface::Sata,
                DiskInterface::Thunderbolt,
                DiskInterface::Virtual,
                DiskInterface::Unknown
            ]
        );
    }

    #[test]
    fn mac_profile_facts() {
        let profile = json!({
            "SPNVMeDataType": [{"_name": "Generic SSD Controller", "_items": [{
                "_name": "Samsung SSD 970 EVO Plus 1TB", "bsd_name": "disk2",
                "device_model": "Samsung SSD 970 EVO Plus 1TB", "device_revision": "2B2QEXM7",
                "device_serial": "S4EWNX0N123456", "spnvme_linkspeed": "8.0 GT/s", "spnvme_linkwidth": "x4",
                "spnvme_trim_support": "Yes"}]}],
            "SPSerialATADataType": [{"_name": "Intel 8 Series Chipset", "spsata_negotiatedlinkspeed": "6 Gigabit",
                "spsata_linkspeed": "6 Gigabit", "_items": [{"_name": "Samsung SSD 860 EVO 1TB", "bsd_name": "disk0",
                "device_serial": "S3Z9NB0K123456", "spsata_trim_support": "spsata_yes"}]}],
            "SPUSBDataType": [{"_name": "USB31Bus", "_items": [{"_name": "Ultra", "device_speed": "super_speed",
                "serial_num": "4C530001", "Media": [{"_name": "SanDisk Ultra", "bsd_name": "disk4",
                "volumes": [{"bsd_name": "disk4s1"}]}]}]}]
        });
        let facts = profile_facts(&profile);
        assert_eq!(facts["disk2"].serial.as_deref(), Some("S4EWNX0N123456"));
        assert_eq!(facts["disk2"].trim, Some(true));
        assert_eq!(facts["disk2"].link, pcie(Some(3), Some(4), None, None));
        assert_eq!(
            facts["disk0"].link,
            Some(rate_link(LinkKind::Sata, Some(6000), Some(6000)))
        );
        assert_eq!(facts["disk0"].trim, Some(true));
        assert_eq!(
            facts["disk4"].link,
            Some(rate_link(LinkKind::Usb, Some(5000), None))
        );
        assert!(!facts.contains_key("disk4s1")); // volumes are not disks
    }

    #[test]
    fn mac_volume_disks() {
        let apfs = json!({"APFSPhysicalStores": [{"APFSPhysicalStore": "disk0s2"}], "ParentWholeDisk": "disk3"});
        assert_eq!(volume_disks(&apfs), vec![("disk0".to_owned(), None)]); // shares its container
        let exfat = json!({"ParentWholeDisk": "disk4", "TotalSize": 61530439680u64});
        assert_eq!(
            volume_disks(&exfat),
            vec![("disk4".to_owned(), Some(61530439680))]
        );
        assert_eq!(volume_disks(&json!({})), vec![]);
    }

    #[test]
    fn mac_io_counters() {
        let tree = json!([
            {"IOObjectClass": "IOBlockStorageDriver",
             "Statistics": {"Bytes (Read)": 4_000_000, "Bytes (Write)": 9_000_000, "Operations (Read)": 10},
             "IORegistryEntryChildren": [{"BSD Name": "disk0",
                "IORegistryEntryChildren": [{"BSD Name": "disk0s1"}, {"BSD Name": "disk0s2"}]}]},
            {"IOObjectClass": "IOBlockStorageDriver", "Statistics": {"Bytes (Read)": "n/a"}}
        ]);
        assert_eq!(
            io_counters(&tree),
            BTreeMap::from([("disk0".to_owned(), (4_000_000, 9_000_000))])
        );
        assert!(io_counters(&Value::Null).is_empty());
    }

    #[test]
    fn plist_documents() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>WholeDisks</key><array><string>disk0</string></array>
<key>Size</key><integer>42</integer><key>Blob</key><data>AAEC</data></dict></plist>"#;
        let v = parse_plist(xml).unwrap();
        assert_eq!(
            v,
            json!({"WholeDisks": ["disk0"], "Size": 42, "Blob": null})
        );
        assert!(parse_plist(b"not a plist").is_none());
    }
}
