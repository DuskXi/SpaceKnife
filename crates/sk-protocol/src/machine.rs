//! This computer's disks (`machine.disks`, the 「本机磁盘」 page).
//!
//! Every physical disk the system reports, scanned or not: model, capacity, the storage media, the
//! interface and its link (the negotiated rate and the most the device supports), and which volumes
//! live on it. Every volume of the start page, with the disks under it and what SpaceKnife knows of
//! it from a scan open in this backend or from its newest snapshot. Anything a system does not tell
//! is left empty, never guessed.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::base::DateTime;
use crate::macros::str_enum;
use crate::scan::{ClassStat, MediaName, TreeMethod, VolumeInfo};
use crate::validate::Validate;

str_enum! {
    pub enum DiskInterface {
        Nvme = "nvme",
        Sata = "sata",
        Sas = "sas",
        Scsi = "scsi",
        Ata = "ata",
        Usb = "usb",
        Sd = "sd",
        Thunderbolt = "thunderbolt",
        Virtual = "virtual",
        Raid = "raid",
        Other = "other",
        Unknown = "unknown",
    }
}

impl Default for DiskInterface {
    fn default() -> Self {
        DiskInterface::Unknown
    }
}

str_enum! {
    pub enum LinkKind {
        Pcie = "pcie",
        Sata = "sata",
        Usb = "usb",
    }
}

/// A link: the negotiated rate and, when the system tells it, the most the device supports.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiskLink {
    pub kind: LinkKind,
    /// SATA / USB: the negotiated line rate in Mb/s (SATA 1500, 3000, 6000; USB 12, 480, 5000, 10000, 20000)
    #[serde(default)]
    pub rate_mbps: Option<i64>,
    /// SATA / USB: the most the device (SATA: and its port) supports; None when not known
    #[serde(default)]
    pub max_rate_mbps: Option<i64>,
    /// PCIe: the negotiated generation, 1-6
    #[serde(default)]
    pub pcie_gen: Option<i64>,
    /// PCIe: the negotiated lanes
    #[serde(default)]
    pub pcie_width: Option<i64>,
    /// PCIe: the most the device supports
    #[serde(default)]
    pub max_pcie_gen: Option<i64>,
    /// PCIe: the most lanes the device supports
    #[serde(default)]
    pub max_pcie_width: Option<i64>,
}

/// A volume's share of a disk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiskPart {
    pub mount_point: String,
    /// How much of the disk it takes; None when not known
    #[serde(default)]
    pub bytes: Option<i64>,
}

str_enum! {
    pub enum HealthSource {
        Nvme = "nvme",
        Ata = "ata",
    }
}

/// What the disk records about itself: the NVMe SMART / Health log (log page 02h) or the ATA
/// Device Statistics log (04h). Fields the disk doesn't keep are None.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiskHealth {
    pub source: HealthSource,
    /// Now, in °C
    #[serde(default)]
    pub temperature_c: Option<i64>,
    #[serde(default)]
    pub power_on_hours: Option<i64>,
    #[serde(default)]
    pub power_cycles: Option<i64>,
    /// Read over the disk's life
    #[serde(default)]
    pub read_bytes: Option<i64>,
    /// Written over the disk's life
    #[serde(default)]
    pub written_bytes: Option<i64>,
    /// NVMe: the share of its rated endurance used, the vendor's estimate (can pass 100)
    #[serde(default)]
    pub percentage_used: Option<i64>,
    /// NVMe: spare blocks left, %
    #[serde(default)]
    pub available_spare: Option<i64>,
    /// NVMe: the spare % below which it warns
    #[serde(default)]
    pub spare_threshold: Option<i64>,
    #[serde(default)]
    pub unsafe_shutdowns: Option<i64>,
    /// NVMe: unrecovered data integrity errors
    #[serde(default)]
    pub media_errors: Option<i64>,
    /// NVMe: critical warning bits, 0 = none
    #[serde(default)]
    pub critical_warning: Option<i64>,
    /// When the record was read with administrator rights (Linux: machine.read_health); None: read just now. The temperature is always the current one.
    #[serde(default)]
    pub read_at: Option<DateTime>,
}

str_enum! {
    pub enum HealthNote {
        None = "",
        Usb = "usb",
        Unsupported = "unsupported",
        NeedsAdmin = "needs_admin",
        Unavailable = "unavailable",
    }
}

impl Default for HealthNote {
    fn default() -> Self {
        HealthNote::None
    }
}

/// Read and written since the system started (the operating system's counters).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiskIo {
    pub read_bytes: i64,
    pub written_bytes: i64,
    /// When the counters started: the system's boot
    #[serde(default)]
    pub since: Option<DateTime>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PhysicalDisk {
    /// The system's name for it: PhysicalDrive3 (Windows), nvme0n1 (Linux), disk2 (macOS)
    pub id: String,
    /// Windows: the disk number of Disk Management
    #[serde(default)]
    pub number: Option<i64>,
    /// As the device reports it; '' when it doesn't
    pub model: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub serial: String,
    #[serde(default)]
    pub firmware: String,
    /// Bytes; None while no medium is in (an empty card reader)
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub media: MediaName,
    #[serde(default)]
    pub interface: DiskInterface,
    #[serde(default)]
    pub removable: bool,
    /// Whether the disk takes TRIM / UNMAP; None when not known
    #[serde(default)]
    pub trim: Option<bool>,
    /// The disk's own link: PCIe (NVMe), SATA or USB
    #[serde(default)]
    pub link: Option<DiskLink>,
    /// The PCIe link of the controller a SATA disk hangs on, when the system tells it: an add-in card's lanes can be slower than the disk's SATA link
    #[serde(default)]
    pub controller_link: Option<DiskLink>,
    /// Holds the system volume
    #[serde(default)]
    pub system: bool,
    /// The volumes on it, in disk order when known
    #[serde(default)]
    pub parts: Vec<DiskPart>,
    /// What the disk records about itself
    #[serde(default)]
    pub health: Option<DiskHealth>,
    /// Why `health` is missing or partial: behind a USB bridge that doesn't pass it on, the disk keeps no such log, the system needs administrator rights for it, or it has no way to it
    #[serde(default)]
    pub health_note: HealthNote,
    /// Read and written since the system started
    #[serde(default)]
    pub io: Option<DiskIo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FolderStat {
    pub name: String,
    pub bytes: i64,
}

str_enum! {
    pub enum VolumeScanSource {
        Scan = "scan",
        Snapshot = "snapshot",
        Kept = "kept",
    }
}

/// What SpaceKnife knows of a volume from a scan of its root.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeScan {
    /// A scan open in this backend; what was kept of the volume's last complete scan (its classes and folders too, across closed tabs and restarts); or the newest snapshot (counts only)
    pub source: VolumeScanSource,
    /// The open scan (its tab can be shown)
    #[serde(default)]
    pub scan_id: Option<String>,
    /// The snapshot file
    #[serde(default)]
    pub snapshot: Option<String>,
    #[serde(default)]
    pub running: bool,
    /// When the scan finished (or started, while it runs)
    #[serde(default)]
    pub at: Option<DateTime>,
    #[serde(default)]
    pub files: i64,
    #[serde(default)]
    pub dirs: Option<i64>,
    #[serde(default)]
    pub bytes_alloc: i64,
    #[serde(default)]
    pub method: Option<TreeMethod>,
    /// Open and kept scans: the largest file classes
    #[serde(default)]
    pub classes: Vec<ClassStat>,
    /// Open and kept scans: the largest top-level folders
    #[serde(default)]
    pub folders: Vec<FolderStat>,
}

/// A complete snapshot of a whole volume: how much it held then.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotPoint {
    pub at: DateTime,
    pub bytes_alloc: i64,
    pub files: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeOverview {
    pub volume: VolumeInfo,
    /// Ids of the disks it lives on ([]: none known)
    #[serde(default)]
    pub disks: Vec<String>,
    #[serde(default)]
    pub scan: Option<VolumeScan>,
    /// The complete snapshots of the whole volume, newest first (at most 12): how it grew
    #[serde(default)]
    pub history: Vec<SnapshotPoint>,
}

/// The file classes of every volume whose mix is known (an open complete scan, or what was kept
/// of the last one), added up.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClassTotal {
    /// Every class with bytes, largest first
    pub classes: Vec<ClassStat>,
    /// The scanned bytes of those volumes
    pub bytes: i64,
    /// Their mount points
    pub volumes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MachineDisks {
    pub disks: Vec<PhysicalDisk>,
    pub volumes: Vec<VolumeOverview>,
    pub collected_at: DateTime,
    /// None until a volume's mix is known
    #[serde(default)]
    pub classes: Option<ClassTotal>,
}

/// Params of `machine.read_health`. Lenient like the Python model (a Payload): unknown fields are
/// ignored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReadHealthParams {
    /// Ids of the disks to read; []: every disk whose record needs administrator rights
    #[serde(default)]
    pub disks: Vec<String>,
}

impl Validate for ReadHealthParams {}

str_enum! {
    pub enum ReadHealthOutcome {
        Read = "read",
        Denied = "denied",
        Unavailable = "unavailable",
        Nothing = "nothing",
    }
}

/// ``read``: the records were read (a disk that did not answer says why in its health_note);
/// ``denied``: the user closed the password prompt; ``unavailable``: no way to ask for administrator
/// rights here (``message`` says why); ``nothing``: no disk needed it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReadHealthResult {
    pub outcome: ReadHealthOutcome,
    #[serde(default)]
    pub message: String,
    pub machine: MachineDisks,
}
