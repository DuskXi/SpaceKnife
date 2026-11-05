//! What a disk records about itself, and the system's read / write counters (Python
//! `disks/health.py`; pure parsers).
//!
//! - NVMe SMART / Health Information log (log page 02h, 512 bytes, NVMe base specification):
//!   critical warning, composite temperature (K), available spare and its threshold, percentage
//!   used, data units read / written (each 1000 × 512 bytes), power cycles, power-on hours, unsafe
//!   shutdowns, media and data integrity errors.
//! - ATA Device Statistics log (log address 04h, ACS-3): page 01h General Statistics (power-on
//!   resets, power-on hours, logical sectors written / read) and page 05h Temperature Statistics
//!   (current temperature). Each statistic is a QWORD: bit 63 supported, bit 62 valid, value in
//!   the low bits.
//! - Linux `/sys/block/<disk>/stat`: sectors read (field 3) and written (field 7), 512 bytes each.
//! - The raw pages the elevated helper reads on Linux ([`super::passthrough`]), as hex.

use std::collections::BTreeMap;

use sk_protocol::base::DateTime;
use sk_protocol::machine::{DiskHealth, HealthSource};

pub const NVME_DATA_UNIT: i64 = 1000 * 512;

/// A record with nothing filled in.
pub fn empty_health(source: HealthSource) -> DiskHealth {
    DiskHealth {
        source,
        temperature_c: None,
        power_on_hours: None,
        power_cycles: None,
        read_bytes: None,
        written_bytes: None,
        percentage_used: None,
        available_spare: None,
        spare_threshold: None,
        unsafe_shutdowns: None,
        media_errors: None,
        critical_warning: None,
        read_at: None,
    }
}

/// Little-endian unsigned integer of `n` bytes (up to 16) at `at`, saturated to i64.
fn le(b: &[u8], at: usize, n: usize) -> i64 {
    let mut v: u128 = 0;
    for (i, &x) in b[at..at + n].iter().enumerate() {
        v |= (x as u128) << (8 * i);
    }
    v.min(i64::MAX as u128) as i64
}

/// The NVMe SMART / Health log page; None for a short or empty answer.
pub fn nvme_health(log: &[u8]) -> Option<DiskHealth> {
    if log.len() < 192 || log[..192].iter().all(|&b| b == 0) {
        return None;
    }
    let kelvin = le(log, 1, 2);
    Some(DiskHealth {
        critical_warning: Some(log[0] as i64),
        temperature_c: (200 < kelvin && kelvin < 500).then_some(kelvin - 273),
        available_spare: Some(log[3] as i64),
        spare_threshold: Some(log[4] as i64),
        percentage_used: Some(log[5] as i64),
        read_bytes: Some(le(log, 32, 16).saturating_mul(NVME_DATA_UNIT)),
        written_bytes: Some(le(log, 48, 16).saturating_mul(NVME_DATA_UNIT)),
        power_cycles: Some(le(log, 112, 16)),
        power_on_hours: Some(le(log, 128, 16)),
        unsafe_shutdowns: Some(le(log, 144, 16)),
        media_errors: Some(le(log, 160, 16)),
        ..empty_health(HealthSource::Nvme)
    })
}

/// A Device Statistics QWORD's value, when supported and valid.
fn stat(page: &[u8], offset: usize, bits: u32) -> Option<i64> {
    if page.len() < offset + 8 {
        return None;
    }
    let q = u64::from_le_bytes(page[offset..offset + 8].try_into().expect("8 bytes"));
    if (q >> 63) & 1 == 0 || (q >> 62) & 1 == 0 {
        return None;
    }
    Some((q & ((1u64 << bits) - 1)) as i64)
}

/// The logical sector size from IDENTIFY DEVICE (words 106 and 117-118); 512 when not given.
pub fn ata_sector_size(identify: &[u8]) -> i64 {
    if identify.len() < 512 {
        return 512;
    }
    let w106 = le(identify, 212, 2);
    if (w106 & 0xC000) == 0x4000 && w106 & (1 << 12) != 0 {
        let words = le(identify, 234, 2) | (le(identify, 236, 2) << 16);
        if words >= 256 {
            return words * 2;
        }
    }
    512
}

/// Device Statistics pages 01h and 05h; None when the disk reports nothing of use.
pub fn ata_statistics(
    general: &[u8],
    temperature: Option<&[u8]>,
    sector: i64,
) -> Option<DiskHealth> {
    let (hours, cycles) = (stat(general, 0x10, 32), stat(general, 0x08, 32));
    let (written, read) = (stat(general, 0x18, 48), stat(general, 0x28, 48));
    // a signed byte
    let temp = temperature
        .and_then(|p| stat(p, 0x08, 8))
        .map(|t| if t > 127 { t - 256 } else { t });
    if [hours, cycles, written, read, temp]
        .iter()
        .all(Option::is_none)
    {
        return None;
    }
    Some(DiskHealth {
        power_on_hours: hours,
        power_cycles: cycles,
        written_bytes: written.map(|w| w.saturating_mul(sector)),
        read_bytes: read.map(|r| r.saturating_mul(sector)),
        temperature_c: temp,
        ..empty_health(HealthSource::Ata)
    })
}

fn hex(pages: &BTreeMap<String, String>, key: &str) -> Option<Vec<u8>> {
    let v = pages.get(key)?;
    if v.is_empty() || v.len() % 2 != 0 {
        return None;
    }
    (0..v.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(v.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The record from the helper's `disk_health` answer: `{kind: nvme, log}` or `{kind: ata,
/// identify, general, temperature}` (hex). None when it holds nothing usable.
pub fn from_pages(pages: &BTreeMap<String, String>) -> Option<DiskHealth> {
    match pages.get("kind").map(String::as_str) {
        Some("nvme") => nvme_health(&hex(pages, "log")?),
        Some("ata") => {
            let general = hex(pages, "general")?;
            let sector = ata_sector_size(&hex(pages, "identify").unwrap_or_default());
            ata_statistics(&general, hex(pages, "temperature").as_deref(), sector)
        }
        _ => None,
    }
}

/// (bytes read, bytes written) from `/sys/block/<disk>/stat`.
pub fn linux_stat(text: &str) -> Option<(i64, i64)> {
    let fields: Vec<&str> = text.split_whitespace().collect();
    if fields.len() < 7 {
        return None;
    }
    Some((
        fields[2].parse::<i64>().ok()? * 512,
        fields[6].parse::<i64>().ok()? * 512,
    ))
}

/// When the system started: the counters of the system's I/O statistics start there.
pub fn boot_time() -> Option<DateTime> {
    let up = uptime()?;
    Some(chrono::Utc::now() - chrono::Duration::milliseconds((up * 1000.0) as i64))
}

fn uptime() -> Option<f64> {
    #[cfg(windows)]
    {
        // SAFETY: no preconditions.
        Some(
            unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() } as f64
                / 1000.0,
        )
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "kern.boottime"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let sec: i64 = text
            .split("sec =")
            .nth(1)?
            .split(',')
            .next()?
            .trim()
            .parse()
            .ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs_f64();
        Some(now - sec as f64)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let text = std::fs::read_to_string("/proc/uptime").ok()?;
        text.split_whitespace().next()?.parse().ok()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A SMART / Health log page with the given fields at their offsets.
    pub(crate) fn nvme_log(fields: &[(&str, u128)]) -> Vec<u8> {
        let get = |k: &str, d: u128| fields.iter().find(|(n, _)| *n == k).map_or(d, |&(_, v)| v);
        let mut b = vec![0u8; 512];
        b[0] = get("crit", 0) as u8;
        b[1..3].copy_from_slice(&(get("kelvin", 320) as u16).to_le_bytes());
        b[3] = get("spare", 100) as u8;
        b[4] = get("threshold", 10) as u8;
        b[5] = get("used", 9) as u8;
        for (offset, key) in [
            (32, "units_read"),
            (48, "units_written"),
            (112, "cycles"),
            (128, "hours"),
            (144, "unsafe"),
            (160, "media"),
        ] {
            b[offset..offset + 16].copy_from_slice(&get(key, 0).to_le_bytes());
        }
        b
    }

    pub(crate) fn qword(value: u64, valid: bool) -> [u8; 8] {
        (value | (1 << 63) | if valid { 1 << 62 } else { 0 }).to_le_bytes()
    }

    #[test]
    fn nvme_health_log() {
        let log = nvme_log(&[
            ("units_read", 206_874_470),
            ("units_written", 493_120_475),
            ("cycles", 686),
            ("hours", 8070),
            ("unsafe", 106),
        ]);
        let h = nvme_health(&log).unwrap();
        assert_eq!(h.source, HealthSource::Nvme);
        assert_eq!(
            (
                h.temperature_c,
                h.percentage_used,
                h.available_spare,
                h.spare_threshold
            ),
            (Some(47), Some(9), Some(100), Some(10))
        );
        assert_eq!(h.read_bytes, Some(206_874_470 * 512_000));
        assert_eq!(h.written_bytes, Some(493_120_475 * 512_000));
        assert_eq!(
            (
                h.power_cycles,
                h.power_on_hours,
                h.unsafe_shutdowns,
                h.media_errors,
                h.critical_warning
            ),
            (Some(686), Some(8070), Some(106), Some(0), Some(0))
        );
        assert!(nvme_health(&[0u8; 512]).is_none()); // an empty answer
        assert!(nvme_health(&[1u8; 100]).is_none()); // a short one
        assert_eq!(
            nvme_health(&nvme_log(&[("kelvin", 0)]))
                .unwrap()
                .temperature_c,
            None
        );
    }

    #[test]
    fn ata_device_statistics() {
        let mut general = vec![0u8; 512];
        general[0x08..0x10].copy_from_slice(&qword(2859, true));
        general[0x10..0x18].copy_from_slice(&qword(30322, true));
        general[0x18..0x20].copy_from_slice(&qword(25_974_947_733, true));
        general[0x28..0x30].copy_from_slice(&qword(78_116_006_871, false)); // not valid: left out
        let mut temps = vec![0u8; 512];
        temps[0x08..0x10].copy_from_slice(&qword(24, true));
        let h = ata_statistics(&general, Some(&temps), 512).unwrap();
        assert_eq!(h.source, HealthSource::Ata);
        assert_eq!(
            (h.power_cycles, h.power_on_hours, h.temperature_c),
            (Some(2859), Some(30322), Some(24))
        );
        assert_eq!(
            (h.written_bytes, h.read_bytes),
            (Some(25_974_947_733 * 512), None)
        );
        temps[0x08..0x10].copy_from_slice(&qword(0xF6, true)); // −10 °C, a signed byte
        assert_eq!(
            ata_statistics(&general, Some(&temps), 512)
                .unwrap()
                .temperature_c,
            Some(-10)
        );
        assert!(ata_statistics(&[0u8; 512], None, 512).is_none()); // nothing supported
    }

    #[test]
    fn ata_sector_sizes() {
        let mut words = vec![0u8; 512];
        assert_eq!(ata_sector_size(&words), 512);
        words[212..214].copy_from_slice(&(0x4000u16 | (1 << 12)).to_le_bytes()); // word 106
        words[234..238].copy_from_slice(&2048u32.to_le_bytes()); // words 117-118: 2048 words
        assert_eq!(ata_sector_size(&words), 4096);
        assert_eq!(ata_sector_size(b""), 512);
    }

    #[test]
    fn linux_stat_and_boot_time() {
        let line = "  305918    11872 20453866   180210   152731   149412 26389336  2209360        0   350784  2397856";
        assert_eq!(linux_stat(line), Some((20453866 * 512, 26389336 * 512)));
        assert_eq!(linux_stat(""), None);
        assert_eq!(linux_stat("a b c d e f g"), None);
        let boot = boot_time();
        assert!(boot.is_none_or(|b| b < chrono::Utc::now()));
    }

    #[test]
    fn from_pages_rejects_junk() {
        let pages = |kv: &[(&str, &str)]| {
            kv.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>()
        };
        assert!(from_pages(&pages(&[])).is_none());
        assert!(from_pages(&pages(&[("kind", "nvme"), ("log", "zz")])).is_none());
        assert!(from_pages(&pages(&[("kind", "ata"), ("general", "")])).is_none());
        assert!(from_pages(&pages(&[("kind", "scsi"), ("log", "00")])).is_none());
    }
}
