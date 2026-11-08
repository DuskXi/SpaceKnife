//! Windows: physical disks, their links and the volumes on them, without administrator rights
//! (Python `disks/windows.py`).
//!
//! Each disk is opened with no access rights at all (the storage property queries need none; no
//! sector is read):
//!
//! - `IOCTL_STORAGE_QUERY_PROPERTY`: the device descriptor (model, serial, bus, removable), seek
//!   penalty (rotational), TRIM, and the protocol-specific data: ATA IDENTIFY (SATA speeds, words
//!   76 and 77) or the NVMe Identify Controller (serial, model, firmware).
//! - The protocol-specific data also carries what the disk records about itself: the NVMe SMART /
//!   Health log (page 02h) and the ATA Device Statistics log (04h, pages 01h and 05h).
//! - `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`: the size; `IOCTL_STORAGE_GET_DEVICE_NUMBER`: the disk
//!   number of Disk Management; `IOCTL_DISK_PERFORMANCE`: bytes read and written since the boot.
//! - The device tree (SetupAPI / CfgMgr) above the disk: the PCIe link of an NVMe drive's
//!   controller, or of the add-in controller a SATA disk hangs on (DEVPKEY_PciDevice_*Link*), and
//!   for a USB disk the hub's connection query (`IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX(_V2)`).
//! - `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` on each volume: the disks under it and how much of each.

use std::mem::offset_of;
use std::ptr::{null, null_mut};

use sk_protocol::base::DateTime;
use sk_protocol::machine::{DiskHealth, DiskInterface, DiskIo, DiskLink, HealthNote, PhysicalDisk};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_DevNode_PropertyW, CM_Get_Device_IDW,
    CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW, CM_Get_Parent,
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows_sys::Win32::Foundation::{DEVPROPKEY, GENERIC_WRITE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetVolumeNameForVolumeMountPointW,
};
use windows_sys::Win32::System::Ioctl::{
    AtaDataTypeIdentify, AtaDataTypeLogPage, NVMeDataTypeIdentify, NVMeDataTypeLogPage,
    PropertyStandardQuery, ProtocolTypeAta, ProtocolTypeNvme, STORAGE_PROPERTY_ID,
    STORAGE_PROPERTY_QUERY, STORAGE_PROTOCOL_DATA_DESCRIPTOR, STORAGE_PROTOCOL_SPECIFIC_DATA,
    STORAGE_PROTOCOL_TYPE, StorageDeviceProperty, StorageDeviceProtocolSpecificProperty,
    StorageDeviceSeekPenaltyProperty, StorageDeviceTrimProperty,
};
use windows_sys::core::GUID;

use super::health::{ata_sector_size, ata_statistics, nvme_health};
use super::links::{pcie, pcie_gen_code, sata_identify, usb_windows};
use super::linux::media_bus;
use super::{Probe, new_disk};
use crate::helper::win32::{Handle, from_wide, wide};
use crate::media::{classify, system_mount};
use crate::volume::VolumeInfo;

pub const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x2D_1400;
pub const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x2D_1080;
pub const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x7_00A0;
pub const IOCTL_DISK_PERFORMANCE: u32 = 0x7_0020;
pub const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x56_0000;
pub const IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX: u32 = 0x22_0448;
pub const IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX_V2: u32 = 0x22_045C;

// STORAGE_PROTOCOL_SPECIFIC_DATA.DataType
const ATA_IDENTIFY: u32 = AtaDataTypeIdentify as u32;
const ATA_LOG: u32 = AtaDataTypeLogPage as u32;
const NVME_IDENTIFY: u32 = NVMeDataTypeIdentify as u32;
const NVME_LOG: u32 = NVMeDataTypeLogPage as u32;

const GUID_DEVINTERFACE_DISK: GUID = GUID::from_u128(0x53f56307_b6bf_11d0_94f2_00a0c91efb8b);
const GUID_DEVINTERFACE_USB_HUB: GUID = GUID::from_u128(0xf18a0e88_c30c_11d0_8815_00a0c906bed8);
/// CurrentLinkSpeed 9, Width 10, MaxSpeed 11, MaxWidth 12
const DEVPKEY_PCI: GUID = GUID::from_u128(0x3ab22e31_8264_4b4e_9af5_a8d2d8e33e62);
/// Address 30 (a USB device's port)
const DEVPKEY_DEVICE: GUID = GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0);

/// STORAGE_BUS_TYPE (winioctl.h) → interface.
fn bus_interface(bus: u32) -> DiskInterface {
    match bus {
        1 => DiskInterface::Scsi,
        2 | 3 => DiskInterface::Ata,
        7 => DiskInterface::Usb,
        8 | 16 => DiskInterface::Raid,
        10 => DiskInterface::Sas,
        11 => DiskInterface::Sata,
        12 | 13 => DiskInterface::Sd,
        14 | 15 => DiskInterface::Virtual,
        17 => DiskInterface::Nvme,
        _ => DiskInterface::Other,
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

fn open(path: &str) -> Option<Handle> {
    Handle::open(path, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, 0)
}

/// STORAGE_PROPERTY_QUERY {PropertyId, PropertyStandardQuery, AdditionalParameters = `extra`}.
fn query_input(prop: STORAGE_PROPERTY_ID, extra: &[u8]) -> Vec<u8> {
    let at = offset_of!(STORAGE_PROPERTY_QUERY, AdditionalParameters);
    let mut input = vec![0u8; (at + extra.len()).max(size_of::<STORAGE_PROPERTY_QUERY>())];
    let put = |b: &mut [u8], at: usize, v: i32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put(
        &mut input,
        offset_of!(STORAGE_PROPERTY_QUERY, PropertyId),
        prop,
    );
    put(
        &mut input,
        offset_of!(STORAGE_PROPERTY_QUERY, QueryType),
        PropertyStandardQuery,
    );
    input[at..at + extra.len()].copy_from_slice(extra);
    input
}

fn query(h: &Handle, prop: STORAGE_PROPERTY_ID, size: usize, extra: &[u8]) -> Option<Vec<u8>> {
    h.ioctl(
        IOCTL_STORAGE_QUERY_PROPERTY,
        &query_input(prop, extra),
        size,
    )
    .ok()
}

/// An offset-addressed ASCII string of a STORAGE_DEVICE_DESCRIPTOR.
fn text_at(b: &[u8], at: usize) -> String {
    if at == 0 || at >= b.len() {
        return String::new();
    }
    let end = b[at..]
        .iter()
        .position(|&c| c == 0)
        .map_or(b.len(), |p| at + p);
    String::from_utf8_lossy(&b[at..end]).trim().to_owned()
}

/// The query of StorageDeviceProtocolSpecificProperty: its AdditionalParameters are a
/// STORAGE_PROTOCOL_SPECIFIC_DATA asking for `length` bytes right after itself.
fn protocol_input(
    protocol: STORAGE_PROTOCOL_TYPE,
    data_type: u32,
    value: u32,
    length: usize,
    sub: u32,
) -> Vec<u8> {
    let spec = STORAGE_PROTOCOL_SPECIFIC_DATA {
        ProtocolType: protocol,
        DataType: data_type,
        ProtocolDataRequestValue: value,
        ProtocolDataRequestSubValue: sub,
        ProtocolDataOffset: size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>() as u32,
        ProtocolDataLength: length as u32,
        ..Default::default()
    };
    // SAFETY: ten 32-bit integers, no padding (the sizes must match for transmute to compile)
    let bytes: [u8; 40] = unsafe { std::mem::transmute(spec) };
    query_input(StorageDeviceProtocolSpecificProperty, &bytes)
}

/// The data of a protocol-specific answer: a STORAGE_PROTOCOL_DATA_DESCRIPTOR, whose
/// STORAGE_PROTOCOL_SPECIFIC_DATA says where the data is (ProtocolDataOffset, counted from the
/// start of that block) and how much of it the driver filled (ProtocolDataLength). None when
/// that is less than `length`, or the place is not in the answer.
fn protocol_payload(raw: &[u8], length: usize) -> Option<&[u8]> {
    if raw.len() < size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>() {
        return None;
    }
    let block = offset_of!(STORAGE_PROTOCOL_DATA_DESCRIPTOR, ProtocolSpecificData);
    // SAFETY: the answer holds a whole descriptor (checked above); the block is ten 32-bit
    // integers (any bytes are a value), read without assuming alignment.
    let spec: STORAGE_PROTOCOL_SPECIFIC_DATA =
        unsafe { std::ptr::read_unaligned(raw.as_ptr().add(block).cast()) };
    let offset = spec.ProtocolDataOffset as usize;
    if offset < size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>()
        || (spec.ProtocolDataLength as usize) < length
    {
        return None;
    }
    let start = block.checked_add(offset)?;
    raw.get(start..start.checked_add(length)?)
}

/// StorageDeviceProtocolSpecificProperty: `length` bytes of the protocol's data (ATA IDENTIFY or
/// a log page, NVMe Identify or a log page).
fn protocol_data(
    h: &Handle,
    protocol: STORAGE_PROTOCOL_TYPE,
    data_type: u32,
    value: u32,
    length: usize,
    sub: u32,
) -> Option<Vec<u8>> {
    let input = protocol_input(protocol, data_type, value, length, sub);
    let size = size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>() + length;
    let raw = h.ioctl(IOCTL_STORAGE_QUERY_PROPERTY, &input, size).ok()?;
    protocol_payload(&raw, length).map(<[u8]>::to_vec)
}

fn ascii(b: &[u8]) -> String {
    String::from_utf8_lossy(b)
        .replace('\0', " ")
        .trim()
        .to_owned()
}

fn prop_u32(inst: u32, fmtid: GUID, pid: u32) -> Option<u32> {
    let key = DEVPROPKEY { fmtid, pid };
    let mut kind = 0u32;
    let mut value = 0u32;
    let mut size = 4u32;
    // SAFETY: a 4-byte buffer of the size passed.
    let r = unsafe {
        CM_Get_DevNode_PropertyW(
            inst,
            &key,
            &mut kind,
            (&mut value as *mut u32).cast(),
            &mut size,
            0,
        )
    };
    (r == 0).then_some(value)
}

fn device_id(inst: u32) -> String {
    let mut buf = vec![0u16; 512];
    // SAFETY: buffer of the given length.
    if unsafe { CM_Get_Device_IDW(inst, buf.as_mut_ptr(), 512, 0) } == 0 {
        from_wide(&buf)
    } else {
        String::new()
    }
}

fn parent(inst: u32) -> Option<u32> {
    let mut p = 0u32;
    // SAFETY: out-pointer.
    (unsafe { CM_Get_Parent(&mut p, inst, 0) } == 0).then_some(p)
}

/// The device node and its ancestors, nearest first (at most 8).
fn chain(inst: u32) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut cur = Some(inst);
    while let Some(c) = cur {
        if out.len() >= 8 {
            break;
        }
        out.push((c, device_id(c).to_uppercase()));
        cur = parent(c);
    }
    out
}

/// A SetupAPI device information list, destroyed when dropped (also when a panic unwinds).
struct DevInfoList(HDEVINFO);

impl Drop for DevInfoList {
    fn drop(&mut self) {
        // SAFETY: a list SetupDiGetClassDevsW gave us, destroyed once.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

/// Disk number → (interface path, device node) of every present disk.
fn disk_nodes() -> Vec<(u32, String, u32)> {
    let mut found = Vec::new();
    // SAFETY: SetupAPI enumeration; every structure is sized as documented; the list is
    // destroyed with its guard.
    unsafe {
        let info = SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_DISK,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if info == -1 || info == 0 {
            return found;
        }
        let list = DevInfoList(info);
        let info = list.0;
        let mut index = 0;
        loop {
            let mut iface: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            iface.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(info, null(), &GUID_DEVINTERFACE_DISK, index, &mut iface)
                == 0
            {
                break;
            }
            index += 1;
            let mut need = 0u32;
            SetupDiGetDeviceInterfaceDetailW(info, &iface, null_mut(), 0, &mut need, null_mut());
            if need < 8 {
                continue;
            }
            let mut detail = vec![0u64; (need as usize).div_ceil(8)];
            // cbSize: the fixed part (8 on 64-bit: a DWORD and a WCHAR, aligned)
            *(detail.as_mut_ptr() as *mut u32) =
                size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            let mut node: SP_DEVINFO_DATA = std::mem::zeroed();
            node.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(
                info,
                &iface,
                detail.as_mut_ptr().cast(),
                need,
                null_mut(),
                &mut node,
            ) == 0
            {
                continue;
            }
            let path = crate::helper::win32::from_wide_ptr(
                (detail.as_ptr() as *const u8)
                    .add(offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath))
                    as *const u16,
            );
            let Some(h) = open(&path) else { continue };
            if let Ok(number) = h.ioctl(IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], 12)
                && number.len() >= 8
                && u32_at(&number, 0) == 7
            {
                // FILE_DEVICE_DISK
                found.push((u32_at(&number, 4), path, node.DevInst));
            }
        }
        drop(list);
    }
    found.sort_by_key(|(n, _, _)| *n);
    found.dedup_by_key(|(n, _, _)| *n);
    found
}

fn pci_link(inst: u32) -> Option<DiskLink> {
    let [g, w, mg, mw] = [9, 10, 11, 12].map(|pid| prop_u32(inst, DEVPKEY_PCI, pid));
    pcie(
        pcie_gen_code(g),
        w.filter(|&w| w != 0).map(i64::from),
        pcie_gen_code(mg),
        mw.filter(|&w| w != 0).map(i64::from),
    )
}

fn hub_path(hub: u32) -> Option<String> {
    let instance = wide(device_id(hub));
    let mut size = 0u32;
    // SAFETY: valid GUID and instance string; buffer sized from the first call.
    unsafe {
        if CM_Get_Device_Interface_List_SizeW(
            &mut size,
            &GUID_DEVINTERFACE_USB_HUB,
            instance.as_ptr(),
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        ) != 0
            || size <= 1
        {
            return None;
        }
        let mut buf = vec![0u16; size as usize];
        if CM_Get_Device_Interface_ListW(
            &GUID_DEVINTERFACE_USB_HUB,
            instance.as_ptr(),
            buf.as_mut_ptr(),
            size,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        ) != 0
        {
            return None;
        }
        Some(from_wide(&buf)).filter(|s| !s.is_empty())
    }
}

/// The link of a USB device, from its hub's connection query for its port.
fn usb_link(device: u32) -> Option<DiskLink> {
    let port = prop_u32(device, DEVPKEY_DEVICE, 30).filter(|&p| p != 0)?;
    let path = hub_path(parent(device)?)?;
    // as USBView: GENERIC_WRITE, FILE_SHARE_WRITE
    let h = Handle::open(&path, GENERIC_WRITE, FILE_SHARE_WRITE, 0).or_else(|| open(&path))?;
    // USB_NODE_CONNECTION_INFORMATION_EX (packed): ConnectionIndex(4) DeviceDescriptor(18)
    // CurrentConfigurationValue(1) Speed(1) …
    let mut q = port.to_le_bytes().to_vec();
    q.resize(512, 0);
    let ex = h
        .ioctl(IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX, &q, 512)
        .ok();
    let speed = ex.filter(|e| e.len() > 23).map(|e| e[4 + 18 + 1]);
    // USB_NODE_CONNECTION_INFORMATION_EX_V2: ConnectionIndex Length SupportedUsbProtocols Flags
    let q2: Vec<u8> = [port, 16, 0x7, 0]
        .iter()
        .flat_map(|x| x.to_le_bytes())
        .collect();
    let v2 = h
        .ioctl(IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX_V2, &q2, 16)
        .ok();
    let flags = v2.filter(|v| v.len() >= 16).map(|v| u32_at(&v, 12));
    usb_windows(speed, flags)
}

/// (the disk's own link, its controller's PCIe link) from the device tree above the disk.
fn links(inst: u32, iface: DiskInterface) -> (Option<DiskLink>, Option<DiskLink>) {
    let nodes = chain(inst);
    let above = &nodes[1.min(nodes.len())..];
    if let Some((usb, _)) = above.iter().find(|(_, id)| id.starts_with("USB\\VID_")) {
        return (usb_link(*usb), None);
    }
    let Some((pci, _)) = above.iter().find(|(_, id)| id.starts_with("PCI\\")) else {
        return (None, None);
    };
    if iface == DiskInterface::Nvme {
        return (pci_link(*pci), None);
    }
    (None, pci_link(*pci)) // a SATA / SAS controller: chipset ones report no link
}

/// DISK_PERFORMANCE: BytesRead and BytesWritten since the boot (LARGE_INTEGERs at 0 and 8).
fn disk_io(h: &Handle, since: Option<DateTime>) -> Option<DiskIo> {
    let perf = h
        .ioctl(IOCTL_DISK_PERFORMANCE, &[], 88)
        .ok()
        .filter(|p| p.len() >= 16)?;
    Some(DiskIo {
        read_bytes: u64_at(&perf, 0) as i64,
        written_bytes: u64_at(&perf, 8) as i64,
        since,
    })
}

fn disk(number: u32, path: &str, inst: u32, since: Option<DateTime>) -> Option<PhysicalDisk> {
    let h = open(path)?;
    let dev = query(&h, StorageDeviceProperty, 1024, &[]).filter(|d| d.len() >= 32)?; // STORAGE_DEVICE_DESCRIPTOR
    let removable = dev[10] != 0;
    let bus = u32_at(&dev, 28);
    let [vendor, mut model, mut firmware, mut serial] =
        [12, 16, 20, 24].map(|at| text_at(&dev, u32_at(&dev, at) as usize));
    let rotational = query(&h, StorageDeviceSeekPenaltyProperty, 12, &[])
        .filter(|s| s.len() > 8)
        .map(|s| s[8] != 0); // seek penalty
    let trim = query(&h, StorageDeviceTrimProperty, 12, &[])
        .filter(|s| s.len() > 8)
        .map(|s| s[8] != 0);
    let size = h
        .ioctl(IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], 256)
        .ok()
        .filter(|g| g.len() >= 32)
        .map(|g| u64_at(&g, 24) as i64);
    let iface = bus_interface(bus);
    let mut link = None;
    let mut health: Option<DiskHealth> = None;
    let mut note = HealthNote::None;
    match iface {
        DiskInterface::Sata => {
            let words = protocol_data(&h, ProtocolTypeAta, ATA_IDENTIFY, 0, 512, 0);
            if let Some(w) = &words {
                link = sata_identify(
                    u16::from_le_bytes([w[152], w[153]]),
                    u16::from_le_bytes([w[154], w[155]]),
                );
            }
            // AtaDataTypeLogPage: Device Statistics
            if let Some(general) = protocol_data(&h, ProtocolTypeAta, ATA_LOG, 0x04, 512, 1) {
                let temps = protocol_data(&h, ProtocolTypeAta, ATA_LOG, 0x04, 512, 5);
                health = ata_statistics(
                    &general,
                    temps.as_deref(),
                    ata_sector_size(words.as_deref().unwrap_or(&[])),
                );
            }
            note = if health.is_some() {
                HealthNote::None
            } else {
                HealthNote::Unsupported
            };
        }
        DiskInterface::Nvme => {
            // Identify, CNS = controller
            if let Some(ident) = protocol_data(&h, ProtocolTypeNvme, NVME_IDENTIFY, 1, 4096, 0) {
                let pick = |s: String, old: String| if s.is_empty() { old } else { s };
                serial = pick(ascii(&ident[4..24]), serial);
                model = pick(ascii(&ident[24..64]), model);
                firmware = pick(ascii(&ident[64..72]), firmware);
            }
            // SMART / Health (02h)
            health = protocol_data(&h, ProtocolTypeNvme, NVME_LOG, 2, 512, 0)
                .and_then(|log| nvme_health(&log));
            note = if health.is_some() {
                HealthNote::None
            } else {
                HealthNote::Unsupported
            };
        }
        DiskInterface::Usb => note = HealthNote::Usb,
        DiskInterface::Virtual | DiskInterface::Raid => {}
        _ => note = HealthNote::Unavailable,
    }
    let io = disk_io(&h, since);
    drop(h);
    let (own, controller) = links(inst, iface);
    if own.is_some() && (link.is_none() || iface != DiskInterface::Sata) {
        link = own;
    }
    let vendor_l = vendor.to_lowercase();
    Some(PhysicalDisk {
        number: Some(number as i64),
        model: model.split_whitespace().collect::<Vec<_>>().join(" "),
        vendor: if ["", "ata", "nvme"].contains(&vendor_l.as_str()) {
            String::new()
        } else {
            vendor
        },
        serial: serial.trim_end_matches('.').to_owned(),
        firmware,
        size: size.filter(|&s| s > 0),
        media: classify(media_bus(iface), rotational, removable),
        interface: iface,
        removable,
        trim,
        link,
        controller_link: controller,
        health,
        health_note: if size.unwrap_or(0) > 0 {
            note
        } else {
            HealthNote::None
        },
        io,
        ..new_disk(&format!("PhysicalDrive{number}"))
    })
}

/// `\\?\Volume{GUID}` of a mount point (no trailing backslash).
pub(crate) fn volume_name(mount_point: &str) -> Option<String> {
    let root = if mount_point.ends_with('\\') {
        mount_point.to_owned()
    } else {
        format!("{mount_point}\\")
    };
    let w = wide(&root);
    let mut buf = [0u16; 64];
    // SAFETY: buffer of the given length.
    if unsafe { GetVolumeNameForVolumeMountPointW(w.as_ptr(), buf.as_mut_ptr(), 64) } == 0 {
        return None;
    }
    Some(from_wide(&buf).trim_end_matches('\\').to_owned())
}

/// (disk number, starting offset, length) of a volume device's extents.
pub(crate) fn device_extents(device: &str) -> Vec<(u32, i64, i64)> {
    let Some(h) = open(device) else {
        return Vec::new();
    };
    let Ok(raw) = h.ioctl(IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], 8 + 24 * 32) else {
        return Vec::new();
    };
    if raw.len() < 8 {
        return Vec::new();
    }
    let count = (u32_at(&raw, 0) as usize).min((raw.len() - 8) / 24);
    (0..count)
        .map(|i| {
            let at = 8 + i * 24; // DISK_EXTENT: DiskNumber(4) pad(4) StartingOffset(8) ExtentLength(8)
            (
                u32_at(&raw, at),
                u64_at(&raw, at + 8) as i64,
                u64_at(&raw, at + 16) as i64,
            )
        })
        .collect()
}

pub fn probe(volumes: &[VolumeInfo], since: Option<DateTime>) -> Probe {
    let mut result = Probe::default();
    for (number, path, inst) in disk_nodes() {
        // one odd device must not hide the others
        match std::panic::catch_unwind(|| disk(number, &path, inst, since)) {
            Ok(Some(d)) => result.disks.push(d),
            Ok(None) => {}
            Err(_) => tracing::debug!("disk {number} probe failed"),
        }
    }
    let system = system_mount().to_lowercase();
    for v in volumes {
        if v.is_remote {
            continue; // a slot without a medium still names its disk
        }
        let extents = volume_name(&v.mount_point)
            .map(|n| device_extents(&n))
            .unwrap_or_default();
        if extents.is_empty() {
            continue;
        }
        let parts = extents
            .iter()
            .map(|&(n, _, len)| (format!("PhysicalDrive{n}"), Some(len).filter(|&l| l != 0)))
            .collect();
        result.parts.insert(v.mount_point.clone(), parts);
        result.offsets.insert(v.mount_point.clone(), extents[0].1);
        if v.mount_point.to_lowercase() == system {
            let on: Vec<String> = extents
                .iter()
                .map(|(n, _, _)| format!("PhysicalDrive{n}"))
                .collect();
            for d in &mut result.disks {
                if on.contains(&d.id) {
                    d.system = true;
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32s(b: &[u8]) -> Vec<u32> {
        b.chunks(4).map(|c| u32_at(c, 0)).collect()
    }

    /// An answer as a driver writes it: the descriptor (Version, Size), the block with the data's
    /// place, the bytes in between and the data.
    fn answer(offset: u32, length: u32, gap: usize, data: &[u8]) -> Vec<u8> {
        let mut raw = Vec::new();
        for v in [1, 48, 3, 1, 1, 0, offset, length, 0, 0, 0, 0] {
            raw.extend_from_slice(&u32::to_le_bytes(v));
        }
        raw.extend(std::iter::repeat_n(0xEE, gap));
        raw.extend_from_slice(data);
        raw
    }

    #[test]
    fn the_layouts_of_winioctl_h() {
        assert_eq!(size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>(), 40);
        assert_eq!(offset_of!(STORAGE_PROPERTY_QUERY, AdditionalParameters), 8);
        assert_eq!(size_of::<STORAGE_PROPERTY_QUERY>(), 12);
        assert_eq!(
            offset_of!(STORAGE_PROTOCOL_DATA_DESCRIPTOR, ProtocolSpecificData),
            8
        );
        assert_eq!(size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>(), 48);
        // what disk_nodes wrote by hand before: cbSize 8, the path at 4 (64-bit)
        assert_eq!(size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>(), 8);
        assert_eq!(offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath), 4);
    }

    /// This machine's disks: wherever the old fixed offset (48) found the data, the place the
    /// answer names is that same place, with the same bytes (no disk loses what it showed).
    #[test]
    fn real_answers_read_as_before() {
        for (number, path, _) in disk_nodes() {
            let Some(h) = open(&path) else { continue };
            let Some(dev) = query(&h, StorageDeviceProperty, 1024, &[]).filter(|d| d.len() >= 32)
            else {
                continue;
            };
            let asks: &[(STORAGE_PROTOCOL_TYPE, u32, u32, usize, u32)] =
                match bus_interface(u32_at(&dev, 28)) {
                    DiskInterface::Nvme => &[
                        (ProtocolTypeNvme, NVME_IDENTIFY, 1, 4096, 0),
                        (ProtocolTypeNvme, NVME_LOG, 2, 512, 0),
                    ],
                    DiskInterface::Sata => &[
                        (ProtocolTypeAta, ATA_IDENTIFY, 0, 512, 0),
                        (ProtocolTypeAta, ATA_LOG, 4, 512, 1),
                    ],
                    _ => &[],
                };
            for &(protocol, kind, value, length, sub) in asks {
                let input = protocol_input(protocol, kind, value, length, sub);
                let size = size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>() + length;
                let Ok(raw) = h.ioctl(IOCTL_STORAGE_QUERY_PROPERTY, &input, size) else {
                    continue;
                };
                if raw.len() >= 48 + length {
                    assert_eq!(
                        protocol_payload(&raw, length),
                        Some(&raw[48..48 + length]),
                        "disk {number}, {protocol}/{kind}/{value}"
                    );
                }
            }
        }
    }

    /// The disks are found as before (the device list is now closed by its guard).
    #[test]
    fn disk_nodes_are_found_twice_alike() {
        let first = disk_nodes();
        assert_eq!(disk_nodes(), first);
        assert!(
            first.windows(2).all(|w| w[0].0 < w[1].0),
            "sorted, one per number"
        );
    }

    #[test]
    fn queries_are_what_they_were() {
        // a plain property: PropertyId, PropertyStandardQuery, AdditionalParameters
        assert_eq!(
            u32s(&query_input(StorageDeviceSeekPenaltyProperty, &[])),
            [7, 0, 0]
        );
        // NVMe SMART / Health: the 40-byte block asks for the data right after itself
        assert_eq!(
            u32s(&protocol_input(ProtocolTypeNvme, NVME_LOG, 2, 512, 0)),
            [50, 0, 3, 2, 2, 0, 40, 512, 0, 0, 0, 0]
        );
        assert_eq!(
            u32s(&protocol_input(ProtocolTypeAta, ATA_LOG, 4, 512, 5)),
            [50, 0, 2, 2, 4, 5, 40, 512, 0, 0, 0, 0]
        );
    }

    #[test]
    fn the_data_is_where_the_answer_says() {
        let data: Vec<u8> = (0..16).collect();
        // right after the block, as drivers usually put it
        let raw = answer(40, 16, 0, &data);
        assert_eq!(protocol_payload(&raw, 16), Some(&data[..]));
        assert_eq!(protocol_payload(&raw, 8), Some(&data[..8]));
        // further on: the offset counts from the block, not from a fixed 48
        let raw = answer(56, 16, 16, &data);
        assert_eq!(protocol_payload(&raw, 16), Some(&data[..]));
        // less than asked for, inside the block, outside the answer: nothing
        assert_eq!(protocol_payload(&answer(40, 8, 0, &data), 16), None);
        assert_eq!(protocol_payload(&answer(4, 16, 0, &data), 16), None);
        assert_eq!(protocol_payload(&answer(40, 16, 0, &data[..10]), 16), None);
        assert_eq!(protocol_payload(&answer(u32::MAX, 16, 0, &data), 16), None);
        assert_eq!(protocol_payload(&raw[..30], 0), None);
    }
}
