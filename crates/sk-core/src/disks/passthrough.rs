//! A disk's own health record on Linux, read as root (the elevated helper only, `disk_health`;
//! Python `disks/passthrough.py`).
//!
//! - NVMe: the admin command Get Log Page, log 02h (SMART / Health Information, 512 bytes, for the
//!   whole controller), through `NVME_IOCTL_ADMIN_CMD`.
//! - SATA, and SATA behind a USB bridge that translates ATA commands: IDENTIFY DEVICE (for the
//!   logical sector size) and READ LOG EXT of the Device Statistics log (04h), pages 01h (general:
//!   power-on hours, power cycles, sectors written and read) and 05h (temperature), each an ATA
//!   PASS-THROUGH (16) command through `SG_IO`.
//!
//! Only these fixed read-only commands are ever sent, on a block device opened read-only; the
//! caller names the disk, never the command. The structures are built here and the kernel calls go
//! through [`Kernel`], so the tests run them against a fake kernel. The raw pages go back to the
//! backend, which parses them ([`super::health`]).

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::io;

use crate::helper::messages::valid_linux_disk;
use crate::helper::transport::hex;
use crate::i18n::t;
use crate::tr;

/// `_IOWR('N', 0x41, struct nvme_admin_cmd)`: the 72-byte command below.
pub const NVME_IOCTL_ADMIN_CMD: u64 = 0xC048_4E41;
pub const NVME_ADMIN_GET_LOG_PAGE: u8 = 0x02;
pub const NVME_LOG_SMART: u32 = 0x02;
pub const NVME_ALL_NAMESPACES: u32 = 0xFFFF_FFFF;

pub const SG_IO: u64 = 0x2285;
pub const SG_DXFER_FROM_DEV: i32 = -3;
pub const SG_INTERFACE_ID: i32 = b'S' as i32;
pub const ATA_PASS_THROUGH_16: u8 = 0x85;
pub const ATA_PIO_DATA_IN: u8 = 4;
pub const ATA_IDENTIFY_DEVICE: u8 = 0xEC;
pub const ATA_READ_LOG_EXT: u8 = 0x2F;
pub const ATA_LOG_DEVICE_STATISTICS: u8 = 0x04;
pub const STATS_GENERAL: u8 = 0x01;
pub const STATS_TEMPERATURE: u8 = 0x05;
pub const SECTOR: usize = 512;
pub const TIMEOUT_MS: u32 = 5000;

/// Why a record could not be read.
#[derive(Debug)]
pub enum RecordError {
    /// The device did not answer the command (no pass-through, or the disk refused it).
    Passthrough(String),
    /// Not a disk the helper may open.
    Invalid(String),
    /// It cannot be opened (or the ioctl itself failed).
    Io(io::Error),
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecordError::Passthrough(s) | RecordError::Invalid(s) => f.write_str(s),
            RecordError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for RecordError {
    fn from(e: io::Error) -> Self {
        RecordError::Io(e)
    }
}

// --------------------------------------------------------------------------- NVMe

/// `struct nvme_passthru_cmd` (linux/nvme_ioctl.h).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvmeAdminCmd {
    pub opcode: u8,
    pub flags: u8,
    pub rsvd1: u16,
    pub nsid: u32,
    pub cdw2: u32,
    pub cdw3: u32,
    pub metadata: u64,
    pub addr: u64,
    pub metadata_len: u32,
    pub data_len: u32,
    pub cdw10: u32,
    pub cdw11: u32,
    pub cdw12: u32,
    pub cdw13: u32,
    pub cdw14: u32,
    pub cdw15: u32,
    pub timeout_ms: u32,
    pub result: u32,
}

/// Get Log Page into the buffer at `addr`.
pub fn nvme_get_log(addr: u64, length: u32, log_id: u32) -> NvmeAdminCmd {
    let numd = length / 4 - 1; // dwords, 0-based (NUMDL in cdw10 bits 31:16)
    NvmeAdminCmd {
        opcode: NVME_ADMIN_GET_LOG_PAGE,
        nsid: NVME_ALL_NAMESPACES,
        addr,
        data_len: length,
        cdw10: (numd & 0xFFFF) << 16 | log_id,
        timeout_ms: TIMEOUT_MS,
        ..Default::default()
    }
}

/// `struct sg_io_hdr` (scsi/sg.h).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SgIoHdr {
    pub interface_id: i32,
    pub dxfer_direction: i32,
    pub cmd_len: u8,
    pub mx_sb_len: u8,
    pub iovec_count: u16,
    pub dxfer_len: u32,
    pub dxferp: *mut c_void,
    pub cmdp: *const u8,
    pub sbp: *mut u8,
    pub timeout: u32,
    pub flags: u32,
    pub pack_id: i32,
    pub usr_ptr: *mut c_void,
    pub status: u8,
    pub masked_status: u8,
    pub msg_status: u8,
    pub sb_len_wr: u8,
    pub host_status: u16,
    pub driver_status: u16,
    pub resid: i32,
    pub duration: u32,
    pub info: u32,
}

impl Default for SgIoHdr {
    fn default() -> Self {
        Self {
            interface_id: 0,
            dxfer_direction: 0,
            cmd_len: 0,
            mx_sb_len: 0,
            iovec_count: 0,
            dxfer_len: 0,
            dxferp: std::ptr::null_mut(),
            cmdp: std::ptr::null(),
            sbp: std::ptr::null_mut(),
            timeout: 0,
            flags: 0,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        }
    }
}

/// The two kernel calls, on one open disk.
pub trait Kernel {
    /// `NVME_IOCTL_ADMIN_CMD`: the command's status (0 = success).
    fn nvme_admin(&mut self, cmd: &mut NvmeAdminCmd) -> io::Result<i32>;
    /// `SG_IO`.
    fn sg_io(&mut self, hdr: &mut SgIoHdr) -> io::Result<()>;
}

pub fn read_nvme_log(kernel: &mut dyn Kernel) -> Result<Vec<u8>, RecordError> {
    let mut buf = vec![0u8; SECTOR];
    let mut cmd = nvme_get_log(buf.as_mut_ptr() as u64, SECTOR as u32, NVME_LOG_SMART);
    let status = kernel.nvme_admin(&mut cmd)?;
    if status != 0 {
        return Err(RecordError::Passthrough(tr!(
            "NVMe 控制器拒绝了读取健康日志（状态 0x{status:x}）",
            status = status
        )));
    }
    if buf.iter().all(|&b| b == 0) {
        return Err(RecordError::Passthrough(t("NVMe 控制器返回了空的健康日志").to_owned()));
    }
    Ok(buf)
}

// --------------------------------------------------------------------------- ATA through SG_IO

/// ATA PASS-THROUGH (16), PIO data-in of one 512-byte block (SAT-3 12.2.2).
pub fn ata_pass_through(command: u8, lba_low: u8, lba_mid: u8, extend: bool) -> [u8; 16] {
    let mut cdb = [0u8; 16];
    cdb[0] = ATA_PASS_THROUGH_16;
    cdb[1] = ATA_PIO_DATA_IN << 1 | extend as u8;
    cdb[2] = 0x0E; // T_DIR from the device, BYTE_BLOCK blocks, T_LENGTH in the count field
    cdb[6] = 1; // count (7:0)
    cdb[8] = lba_low; // LBA (7:0): the log address
    cdb[10] = lba_mid; // LBA (15:8): the page
    cdb[14] = command;
    cdb
}

pub fn identify_device() -> [u8; 16] {
    ata_pass_through(ATA_IDENTIFY_DEVICE, 0, 0, false)
}

pub fn read_log_ext(log: u8, page: u8) -> [u8; 16] {
    ata_pass_through(ATA_READ_LOG_EXT, log, page, true)
}

/// The ATA status register from the sense data, when the translator put it there.
fn ata_status(sense: &[u8]) -> Option<u8> {
    if sense.len() < 8 {
        return None;
    }
    if matches!(sense[0] & 0x7F, 0x72 | 0x73) {
        // descriptor format: the ATA Status Return descriptor
        let mut at = 8;
        let end = sense.len().min(8 + sense[7] as usize);
        while at + 1 < end {
            if sense[at] == 0x09 && at + 13 < sense.len() {
                return Some(sense[at + 13]);
            }
            at += 2 + sense[at + 1] as usize;
        }
        return None;
    }
    Some(sense[4]) // fixed format: INFORMATION holds error, status, device, count
}

/// None when the command went through; else why it did not.
pub fn sg_failure(hdr: &SgIoHdr, sense: &[u8]) -> Option<String> {
    if hdr.host_status != 0 {
        return Some(tr!("主机适配器报告错误 0x{code:x}", code = hdr.host_status));
    }
    if hdr.driver_status & 0x07 != 0 {
        // anything but DRIVER_SENSE
        return Some(tr!("驱动报告错误 0x{code:x}", code = hdr.driver_status));
    }
    if hdr.status == 0 {
        return None;
    }
    if hdr.status != 0x02 || sense.len() < 4 {
        // not CHECK CONDITION
        return Some(tr!("设备状态 0x{code:x}", code = hdr.status));
    }
    let descriptor = matches!(sense[0] & 0x7F, 0x72 | 0x73);
    let key = (if descriptor { sense[1] } else { sense[2] }) & 0x0F;
    let (asc, ascq) = if descriptor {
        (sense[2], sense[3])
    } else if sense.len() > 13 {
        (sense[12], sense[13])
    } else {
        (0, 0)
    };
    if key == 0x05 {
        // ILLEGAL REQUEST
        return Some(t("设备不接受 ATA 直通命令（USB 桥接或控制器没有转发）").to_owned());
    }
    if (asc, ascq) == (0x00, 0x1D) {
        // ATA PASS THROUGH INFORMATION AVAILABLE
        if ata_status(sense).is_some_and(|s| s & 0x21 != 0) {
            // ERR or DF
            return Some(t("磁盘拒绝了这条命令（没有这份日志）").to_owned());
        }
        if key == 0x00 || key == 0x01 {
            return None; // NO SENSE / RECOVERED ERROR: done
        }
    }
    Some(tr!("设备报告 {key:x}/{asc:02x}/{ascq:02x}", key = key, asc = asc, ascq = ascq))
}

/// One 512-byte PIO data-in command through SG_IO.
pub fn ata_read(kernel: &mut dyn Kernel, cdb: &[u8; 16]) -> Result<Vec<u8>, RecordError> {
    let mut data = vec![0u8; SECTOR];
    let mut sense = [0u8; 32];
    let mut hdr = SgIoHdr {
        interface_id: SG_INTERFACE_ID,
        dxfer_direction: SG_DXFER_FROM_DEV,
        cmd_len: cdb.len() as u8,
        mx_sb_len: sense.len() as u8,
        dxfer_len: SECTOR as u32,
        dxferp: data.as_mut_ptr().cast(),
        cmdp: cdb.as_ptr(),
        sbp: sense.as_mut_ptr(),
        timeout: TIMEOUT_MS,
        ..Default::default()
    };
    kernel.sg_io(&mut hdr)?;
    let written = (hdr.sb_len_wr as usize).min(sense.len());
    if let Some(why) = sg_failure(&hdr, &sense[..written]) {
        return Err(RecordError::Passthrough(why));
    }
    if hdr.resid > 0 {
        return Err(RecordError::Passthrough(tr!("只读回 {n} 字节", n = SECTOR as i64 - hdr.resid as i64)));
    }
    Ok(data)
}

/// Word 255: when its low byte is the signature A5h, all 512 bytes sum to 0 (mod 256).
fn identify_ok(words: &[u8]) -> bool {
    words[510] != 0xA5 || words.iter().fold(0u8, |a, &b| a.wrapping_add(b)) == 0
}

/// (IDENTIFY DEVICE, Device Statistics page 01h, page 05h or None).
/// IDENTIFY DEVICE, Device Statistics page 01h, page 05h (None when the disk has none).
pub type AtaPages = (Vec<u8>, Vec<u8>, Option<Vec<u8>>);

pub fn read_ata(kernel: &mut dyn Kernel) -> Result<AtaPages, RecordError> {
    let identify = ata_read(kernel, &identify_device())?;
    if identify.iter().all(|&b| b == 0) || !identify_ok(&identify) {
        return Err(RecordError::Passthrough(t("磁盘返回的识别信息无效").to_owned()));
    }
    let general = ata_read(kernel, &read_log_ext(ATA_LOG_DEVICE_STATISTICS, STATS_GENERAL))?;
    if general[2] != STATS_GENERAL {
        // the page header names its page
        return Err(RecordError::Passthrough(t("磁盘没有设备统计日志").to_owned()));
    }
    let temperature = match ata_read(kernel, &read_log_ext(ATA_LOG_DEVICE_STATISTICS, STATS_TEMPERATURE)) {
        Ok(page) if page[2] == STATS_TEMPERATURE => Some(page),
        Ok(_) | Err(RecordError::Passthrough(_)) => None,
        Err(e) => return Err(e),
    };
    Ok((identify, general, temperature))
}

// --------------------------------------------------------------------------- the disk

/// The raw pages of disk `name` (hex): `{kind: nvme, log}` or `{kind: ata, identify, general,
/// temperature}` (temperature "" when the disk has no such page), through the kernel `open`
/// returns for it.
pub fn read_record<K: Kernel>(
    name: &str,
    open: impl FnOnce(&str) -> Result<K, RecordError>,
) -> Result<BTreeMap<String, String>, RecordError> {
    let mut kernel = open(name)?;
    let mut out = BTreeMap::new();
    if name.starts_with("nvme") {
        out.insert("kind".into(), "nvme".into());
        out.insert("log".into(), hex(&read_nvme_log(&mut kernel)?));
    } else {
        let (identify, general, temperature) = read_ata(&mut kernel)?;
        out.insert("kind".into(), "ata".into());
        out.insert("identify".into(), hex(&identify));
        out.insert("general".into(), hex(&general));
        out.insert("temperature".into(), temperature.map(|p| hex(&p)).unwrap_or_default());
    }
    Ok(out)
}

/// A whole disk's block device, opened read-only.
#[cfg(target_os = "linux")]
pub struct SystemDisk(std::os::fd::OwnedFd);

#[cfg(target_os = "linux")]
impl Kernel for SystemDisk {
    fn nvme_admin(&mut self, cmd: &mut NvmeAdminCmd) -> io::Result<i32> {
        use std::os::fd::AsRawFd;
        // SAFETY: a valid fd and a command whose buffer outlives the call.
        let rc = unsafe { libc::ioctl(self.0.as_raw_fd(), NVME_IOCTL_ADMIN_CMD as _, cmd as *mut NvmeAdminCmd) };
        if rc < 0 { Err(io::Error::last_os_error()) } else { Ok(rc) }
    }

    fn sg_io(&mut self, hdr: &mut SgIoHdr) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        // SAFETY: a valid fd and a header whose buffers outlive the call.
        let rc = unsafe { libc::ioctl(self.0.as_raw_fd(), SG_IO as _, hdr as *mut SgIoHdr) };
        if rc < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }
}

fn not_a_disk(name: &str) -> RecordError {
    RecordError::Invalid(tr!("不是一块磁盘：{name}", name = name))
}

/// Check that `name` is a whole disk's kernel name (and, on Linux, that `/sys/block/<name>`
/// exists).
pub fn check_disk_name(name: &str) -> Result<(), RecordError> {
    if !valid_linux_disk(name) || !std::path::Path::new("/sys/block").join(name).is_dir() {
        return Err(not_a_disk(name));
    }
    Ok(())
}

/// `/dev/<name>`, read-only, refusing anything that is not a whole disk's block device.
#[cfg(target_os = "linux")]
pub fn open_disk(name: &str) -> Result<SystemDisk, RecordError> {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::fs::FileTypeExt;
    check_disk_name(name)?;
    let path = std::ffi::CString::new(format!("/dev/{name}")).map_err(|_| not_a_disk(name))?;
    // SAFETY: a valid C string.
    let fd = unsafe {
        libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: we own the new descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let file = std::fs::File::from(fd);
    if !file.metadata()?.file_type().is_block_device() {
        return Err(RecordError::Invalid(tr!("不是块设备：/dev/{name}", name = name)));
    }
    Ok(SystemDisk(file.into()))
}

/// [`read_record`] on the real disk (Linux, root).
#[cfg(target_os = "linux")]
pub fn read_record_system(name: &str) -> Result<BTreeMap<String, String>, RecordError> {
    read_record(name, open_disk)
}

#[cfg(test)]
mod tests {
    use super::super::health::tests::{nvme_log, qword};
    use super::super::health::{from_pages, nvme_health};
    use super::*;

    #[test]
    fn command_layouts() {
        assert_eq!(size_of::<NvmeAdminCmd>(), 72); // struct nvme_passthru_cmd
        assert_eq!((3u64 << 30) | (72 << 16) | ((b'N' as u64) << 8) | 0x41, NVME_IOCTL_ADMIN_CMD);
        if size_of::<usize>() == 8 {
            assert_eq!(size_of::<SgIoHdr>(), 88); // struct sg_io_hdr on 64-bit Linux
            assert_eq!(std::mem::offset_of!(SgIoHdr, dxferp), 16);
            assert_eq!(std::mem::offset_of!(SgIoHdr, usr_ptr), 56);
            assert_eq!(std::mem::offset_of!(SgIoHdr, status), 64);
            assert_eq!(std::mem::offset_of!(SgIoHdr, resid), 72);
        }
        let cmd = nvme_get_log(0x1234, 512, NVME_LOG_SMART);
        assert_eq!((cmd.opcode, cmd.nsid, cmd.addr, cmd.data_len), (0x02, 0xFFFF_FFFF, 0x1234, 512));
        assert_eq!(cmd.cdw10, 0x007F_0002); // 128 dwords (0-based), log 02h
    }

    #[test]
    fn ata_commands() {
        assert_eq!(identify_device(), [0x85, 0x08, 0x0E, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0xEC, 0]);
        // READ LOG EXT, 48-bit: log address in LBA 7:0, page in 15:8
        assert_eq!(read_log_ext(0x04, 0x01), [0x85, 0x09, 0x0E, 0, 0, 0, 1, 0, 0x04, 0, 0x01, 0, 0, 0, 0x2F, 0]);
        assert_eq!(read_log_ext(0x04, 0x05)[10], 0x05);
    }

    #[test]
    fn disk_names() {
        for bad in ["sda1", "nvme0", "nvme0n1p2", "../sda", "sda/../../etc/shadow", "loop0", "dm-0", ""] {
            let err = check_disk_name(bad).unwrap_err();
            assert!(matches!(err, RecordError::Invalid(_)), "{bad}");
            assert!(err.to_string().contains("不是一块磁盘") || err.to_string().contains("disk"), "{err}");
        }
    }

    // ---------------------------------------------------------------- a fake kernel

    enum Reply {
        Data(Vec<u8>),
        Check(u8, Vec<u8>), // SCSI status, sense
    }

    struct Fake {
        nvme: Option<(Vec<u8>, i32)>,
        replies: Vec<((u8, u8), Reply)>,
        resid: i32,
    }

    impl Fake {
        fn nvme(log: Vec<u8>, status: i32) -> Self {
            Self {
                nvme: Some((log, status)),
                replies: Vec::new(),
                resid: 0,
            }
        }
        fn sat(replies: Vec<((u8, u8), Reply)>) -> Self {
            Self {
                nvme: None,
                replies,
                resid: 0,
            }
        }
    }

    impl Kernel for Fake {
        fn nvme_admin(&mut self, cmd: &mut NvmeAdminCmd) -> io::Result<i32> {
            let (log, status) = self.nvme.clone().expect("an NVMe kernel");
            assert_eq!(cmd.data_len, 512);
            // SAFETY: the address is the caller's 512-byte buffer.
            unsafe { std::ptr::copy_nonoverlapping(log.as_ptr(), cmd.addr as *mut u8, 512) };
            Ok(status)
        }

        fn sg_io(&mut self, hdr: &mut SgIoHdr) -> io::Result<()> {
            assert_eq!((hdr.interface_id, hdr.dxfer_direction), (b'S' as i32, SG_DXFER_FROM_DEV));
            if self.resid != 0 {
                hdr.resid = self.resid;
                return Ok(());
            }
            // SAFETY: the pointers are the caller's buffers of the given lengths.
            let cdb = unsafe { std::slice::from_raw_parts(hdr.cmdp, hdr.cmd_len as usize) };
            let key = (cdb[14], cdb[10]);
            let default = Reply::Check(0x02, vec![0x72, 0x05, 0x20, 0x00, 0, 0, 0, 0]);
            let reply = self.replies.iter().find(|(k, _)| *k == key).map_or(&default, |(_, r)| r);
            match reply {
                Reply::Data(d) => unsafe {
                    std::ptr::copy_nonoverlapping(d.as_ptr(), hdr.dxferp as *mut u8, d.len());
                    hdr.status = 0;
                },
                Reply::Check(status, sense) => unsafe {
                    hdr.status = *status;
                    std::ptr::copy_nonoverlapping(sense.as_ptr(), hdr.sbp, sense.len());
                    hdr.sb_len_wr = sense.len() as u8;
                    hdr.driver_status = 0x08;
                },
            }
            Ok(())
        }
    }

    fn stats_page(number: u8, entries: &[(usize, [u8; 8])]) -> Vec<u8> {
        let mut page = vec![0u8; 512];
        page[0..2].copy_from_slice(&1u16.to_le_bytes()); // revision
        page[2] = number;
        for (offset, value) in entries {
            page[*offset..offset + 8].copy_from_slice(value);
        }
        page
    }

    fn identify_words(sector_words: Option<u32>) -> Vec<u8> {
        let mut words = vec![0u8; 512];
        words[0] = 0x40;
        if let Some(w) = sector_words {
            words[212..214].copy_from_slice(&(0x4000u16 | (1 << 12)).to_le_bytes());
            words[234..238].copy_from_slice(&w.to_le_bytes());
        }
        words[510] = 0xA5;
        let sum = words.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        words[511] = 0u8.wrapping_sub(sum); // the checksum makes all 512 bytes sum to 0
        words
    }

    /// Descriptor-format sense: ABORTED COMMAND, ATA PASS THROUGH INFORMATION AVAILABLE, the ATA
    /// Status Return descriptor with ERR set.
    fn ata_error_sense() -> Vec<u8> {
        let descriptor = [0x09, 0x0C, 0x01, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x51];
        let mut s = vec![0x72, 0x0B, 0x00, 0x1D, 0, 0, 0, descriptor.len() as u8];
        s.extend_from_slice(&descriptor);
        s
    }

    fn pages(kv: &[(&str, String)]) -> BTreeMap<String, String> {
        kv.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn nvme_record() {
        let log = nvme_log(&[("units_read", 206_874_470), ("units_written", 493_120_475), ("cycles", 686), ("hours", 8070)]);
        let got = read_record("nvme0n1", |_| Ok(Fake::nvme(log.clone(), 0))).unwrap();
        assert_eq!(got, pages(&[("kind", "nvme".into()), ("log", hex(&log))]));
        let record = from_pages(&got).unwrap();
        assert_eq!(record.written_bytes, Some(493_120_475 * 512_000));
        assert!(nvme_health(&log).is_some());
        let err = read_record("nvme0n1", |_| Ok(Fake::nvme(log.clone(), 0x2))).unwrap_err();
        assert!(matches!(&err, RecordError::Passthrough(m) if m.contains("0x2")), "{err}");
        assert!(matches!(read_record("nvme0n1", |_| Ok(Fake::nvme(vec![0; 512], 0))), Err(RecordError::Passthrough(_))));
    }

    #[test]
    fn ata_record() {
        let general = stats_page(1, &[(0x08, qword(2859, true)), (0x10, qword(30322, true)), (0x18, qword(25_974_947_733, true))]);
        let temps = stats_page(5, &[(0x08, qword(31, true))]);
        let kernel = || {
            Fake::sat(vec![
                ((0xEC, 0), Reply::Data(identify_words(Some(2048)))),
                ((0x2F, 1), Reply::Data(general.clone())),
                ((0x2F, 5), Reply::Data(temps.clone())),
            ])
        };
        let got = read_record("sda", |_| Ok(kernel())).unwrap();
        assert_eq!(
            got,
            pages(&[
                ("kind", "ata".into()),
                ("identify", hex(&identify_words(Some(2048)))),
                ("general", hex(&general)),
                ("temperature", hex(&temps)),
            ])
        );
        let record = from_pages(&got).unwrap();
        assert_eq!((record.power_cycles, record.power_on_hours, record.temperature_c), (Some(2859), Some(30322), Some(31)));
        assert_eq!(record.written_bytes, Some(25_974_947_733 * 4096)); // 4 KiB logical sectors

        // no temperature page: the rest still comes back
        let no_temp = Fake::sat(vec![
            ((0xEC, 0), Reply::Data(identify_words(None))),
            ((0x2F, 1), Reply::Data(general.clone())),
            ((0x2F, 5), Reply::Check(0x02, ata_error_sense())),
        ]);
        let got = read_record("sdb", |_| Ok(no_temp)).unwrap();
        assert_eq!(got["temperature"], "");
        assert_eq!(from_pages(&got).unwrap().written_bytes, Some(25_974_947_733 * 512));
    }

    #[test]
    fn ata_refusals() {
        let general = stats_page(1, &[(0x10, qword(10, true))]);
        // a bridge that doesn't take ATA PASS-THROUGH: ILLEGAL REQUEST, INVALID FIELD IN CDB
        let err = read_record("sdc", |_| Ok(Fake::sat(vec![]))).unwrap_err();
        assert!(err.to_string().contains("ATA"), "{err}");
        // a disk without the Device Statistics log: READ LOG EXT aborted with ERR
        let no_log = Fake::sat(vec![
            ((0xEC, 0), Reply::Data(identify_words(None))),
            ((0x2F, 1), Reply::Check(0x02, ata_error_sense())),
        ]);
        let err = read_record("sdd", |_| Ok(no_log)).unwrap_err();
        assert!(err.to_string().contains("拒绝") || err.to_string().contains("refused"), "{err}");
        // a page that isn't the one asked for
        let wrong = Fake::sat(vec![((0xEC, 0), Reply::Data(identify_words(None))), ((0x2F, 1), Reply::Data(stats_page(3, &[])))]);
        assert!(matches!(read_record("sde", |_| Ok(wrong)), Err(RecordError::Passthrough(_))));
        // identify data that fails its checksum
        let mut broken = identify_words(None);
        broken[0] ^= 1;
        let bad = Fake::sat(vec![((0xEC, 0), Reply::Data(broken)), ((0x2F, 1), Reply::Data(general))]);
        assert!(matches!(read_record("sdf", |_| Ok(bad)), Err(RecordError::Passthrough(_))));
    }

    #[test]
    fn sense_decoding() {
        let mut hdr = SgIoHdr::default();
        assert_eq!(sg_failure(&hdr, b""), None);
        // ATA PASS THROUGH INFORMATION AVAILABLE without an error: done
        let mut ok = vec![0x72, 0x01, 0x00, 0x1D, 0, 0, 0, 14];
        ok.extend_from_slice(&[0x09, 0x0C, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x50]);
        hdr.status = 0x02;
        assert_eq!(sg_failure(&hdr, &ok), None);
        assert!(sg_failure(&hdr, &ata_error_sense()).is_some());
        let fixed = [0x70, 0, 0x05, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0x24, 0x00]; // fixed format, ILLEGAL REQUEST
        assert!(sg_failure(&hdr, &fixed).unwrap_or_default().contains("ATA"));
        hdr.status = 0;
        hdr.host_status = 0x07;
        assert!(sg_failure(&hdr, b"").unwrap_or_default().contains("0x7"));
        hdr.host_status = 0;
        hdr.driver_status = 0x06; // DRIVER_TIMEOUT
        assert!(sg_failure(&hdr, b"").is_some());
    }

    #[test]
    fn short_transfer() {
        let mut fake = Fake::sat(vec![]);
        fake.resid = 256;
        let err = read_record("sda", |_| Ok(fake)).unwrap_err();
        assert!(err.to_string().contains("256"), "{err}");
    }
}
