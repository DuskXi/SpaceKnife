//! Links of disks and their controllers, from what each system reports (Python `disks/links.py`;
//! pure functions).
//!
//! - SATA: ATA IDENTIFY DEVICE word 76 (the speeds the device supports, bits 1-3 = 1.5 / 3 / 6
//!   Gb/s) and word 77 (bits 3:1, the negotiated speed; ACS-3); Linux's `sata_spd` ("6.0 Gbps").
//! - PCIe: Windows' DEVPKEY_PciDevice_*LinkSpeed codes (1 = 2.5 GT/s … 6 = 64 GT/s, that is the
//!   generation), Linux's `current_link_speed` ("16.0 GT/s PCIe") and macOS' "8.0 GT/s" / "x4".
//! - USB: Windows' connection speed code and the SuperSpeed flags of the hub's connection query,
//!   Linux's `speed` in Mb/s ("480", "5000") and `version` ("3.20").

use std::sync::LazyLock;

use regex::Regex;
use sk_protocol::machine::{DiskLink, LinkKind};

fn link(kind: LinkKind) -> DiskLink {
    DiskLink {
        kind,
        rate_mbps: None,
        max_rate_mbps: None,
        pcie_gen: None,
        pcie_width: None,
        max_pcie_gen: None,
        max_pcie_width: None,
    }
}

/// A SATA or USB link: negotiated rate and the most supported (Mb/s).
pub fn rate_link(kind: LinkKind, rate: Option<i64>, max: Option<i64>) -> DiskLink {
    DiskLink {
        rate_mbps: rate,
        max_rate_mbps: max,
        ..link(kind)
    }
}

fn sata_mbps(gen_: u16) -> Option<i64> {
    match gen_ {
        1 => Some(1500),
        2 => Some(3000),
        3 => Some(6000),
        _ => None,
    }
}

/// The SATA link from IDENTIFY DEVICE words 76 and 77 (None for a device that reports none).
pub fn sata_identify(word76: u16, word77: u16) -> Option<DiskLink> {
    if word76 == 0 || word76 == 0xFFFF {
        return None;
    }
    let top_gen = (1..=3).filter(|g| word76 & (1 << g) != 0).max();
    let current = if word77 != 0 && word77 != 0xFFFF {
        (word77 >> 1) & 0x7
    } else {
        0
    };
    let rate = sata_mbps(current);
    let top = top_gen.and_then(sata_mbps);
    if rate.is_none() && top.is_none() {
        return None;
    }
    Some(rate_link(LinkKind::Sata, rate, top))
}

fn number(s: &str) -> Option<f64> {
    s.parse::<f64>().ok()
}

/// Mb/s from "6.0 Gbps" (Linux sata_spd), "3 Gigabit" (macOS) or "1.5 Gb/s"; None otherwise.
pub fn gbps_text(text: &str) -> Option<i64> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(\d+(?:\.\d+)?)\s*(?:Gbps|Gb/s|Gigabit|G)").expect("regex")
    });
    let m = RE.captures(text)?;
    Some((number(&m[1])? * 1000.0).round() as i64)
}

/// Windows' link speed code is the generation (1 = 2.5 GT/s … 6 = 64 GT/s).
pub fn pcie_gen_code(code: Option<u32>) -> Option<i64> {
    code.filter(|c| (1..=6).contains(c)).map(i64::from)
}

/// The generation from "16.0 GT/s PCIe" (Linux) or "8.0 GT/s" (macOS); None for "Unknown".
pub fn pcie_gen_text(text: &str) -> Option<i64> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(\d+(?:\.\d+)?)\s*GT/s").expect("regex"));
    let v = number(&RE.captures(text)?[1])?;
    [
        (2.5, 1),
        (5.0, 2),
        (8.0, 3),
        (16.0, 4),
        (32.0, 5),
        (64.0, 6),
    ]
    .iter()
    .find(|(gts, _)| *gts == v)
    .map(|&(_, g)| g)
}

/// Lanes from "4" (Linux) or "x4" (macOS).
pub fn pcie_width_text(text: &str) -> Option<i64> {
    let t = text.trim();
    let t = t.strip_prefix(['x', 'X']).unwrap_or(t);
    if t.is_empty() || t.len() > 2 || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let w: i64 = t.parse().ok()?;
    [1, 2, 4, 8, 12, 16, 32].contains(&w).then_some(w)
}

pub fn pcie(
    gen_: Option<i64>,
    width: Option<i64>,
    max_gen: Option<i64>,
    max_width: Option<i64>,
) -> Option<DiskLink> {
    if gen_.is_none() && width.is_none() {
        return None;
    }
    Some(DiskLink {
        pcie_gen: gen_,
        pcie_width: width,
        max_pcie_gen: max_gen,
        max_pcie_width: max_width,
        ..link(LinkKind::Pcie)
    })
}

// USB_CONNECTION_INFORMATION_EX_V2 flags (usbioctl.h)
pub const OPERATING_SUPERSPEED: u32 = 0x1;
pub const SUPERSPEED_CAPABLE: u32 = 0x2;
pub const OPERATING_SUPERSPEED_PLUS: u32 = 0x4;
pub const SUPERSPEED_PLUS_CAPABLE: u32 = 0x8;

/// The USB link from the hub's connection query: the speed code, and the V2 flags when known.
///
/// SuperSpeedPlus is reported as 10 Gb/s (Windows doesn't tell 10 from 20 Gb/s here).
pub fn usb_windows(speed: Option<u8>, flags: Option<u32>) -> Option<DiskLink> {
    let f = flags.unwrap_or(0);
    let rate = if f & OPERATING_SUPERSPEED_PLUS != 0 {
        Some(10000)
    } else if f & OPERATING_SUPERSPEED != 0 {
        Some(5000)
    } else {
        // UsbLowSpeed (1.5 Mb/s, shown as 1) … UsbSuperSpeed
        speed.and_then(|s| match s {
            0 => Some(1),
            1 => Some(12),
            2 => Some(480),
            3 => Some(5000),
            _ => None,
        })
    };
    let top = if f & SUPERSPEED_PLUS_CAPABLE != 0 {
        Some(10000)
    } else if f & SUPERSPEED_CAPABLE != 0 {
        Some(5000)
    } else {
        None
    };
    if rate.is_none() && top.is_none() {
        return None;
    }
    Some(rate_link(LinkKind::Usb, rate, top))
}

/// The USB link from sysfs: `speed` in Mb/s; `version` says a USB 3 device that runs slower
/// supports at least 5 Gb/s (the exact most is not in sysfs).
pub fn usb_linux(speed: &str, version: &str) -> Option<DiskLink> {
    let rate = (number(speed.trim())?.round() as i64).max(1);
    let spec = number(version.trim()).unwrap_or(0.0);
    let top = (spec >= 3.0 && rate < 5000).then_some(5000);
    Some(rate_link(LinkKind::Usb, Some(rate), top))
}

/// macOS' "device_speed" ("high_speed", "super_speed") or link speed text ("Up to 5 Gb/s").
pub fn usb_mac(text: &str) -> Option<DiskLink> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(\d+(?:\.\d+)?)\s*(Gb|Mb)/s").expect("regex"));
    let rate = match text.trim().to_lowercase().as_str() {
        "low_speed" => Some(1),
        "full_speed" => Some(12),
        "high_speed" => Some(480),
        "super_speed" => Some(5000),
        "super_speed_plus" => Some(10000),
        "super_speed_plus_by2" => Some(20000),
        _ => RE.captures(text).and_then(|m| {
            let v = number(&m[1])?;
            Some((v * if &m[2] == "Gb" { 1000.0 } else { 1.0 }).round() as i64)
        }),
    };
    rate.filter(|&r| r != 0)
        .map(|r| rate_link(LinkKind::Usb, Some(r), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sata(rate: Option<i64>, max: Option<i64>) -> Option<DiskLink> {
        Some(rate_link(LinkKind::Sata, rate, max))
    }
    fn usb(rate: Option<i64>, max: Option<i64>) -> Option<DiskLink> {
        Some(rate_link(LinkKind::Usb, rate, max))
    }

    #[test]
    fn sata_identify_words() {
        // recorded: a Micron SATA SSD at 6 Gb/s (w76 0xb50e: Gen1-3, w77 0x46: current Gen3)
        assert_eq!(sata_identify(0xB50E, 0x0046), sata(Some(6000), Some(6000)));
        assert_eq!(sata_identify(0x000E, 0x0004), sata(Some(3000), Some(6000))); // 6 Gb/s disk, 3 Gb/s port
        assert_eq!(sata_identify(0x0006, 0), sata(None, Some(3000)));
        assert_eq!(sata_identify(0, 0), None);
        assert_eq!(sata_identify(0xFFFF, 0x46), None);
    }

    #[test]
    fn link_texts() {
        assert_eq!(gbps_text("6.0 Gbps"), Some(6000));
        assert_eq!(gbps_text("1.5 Gbps"), Some(1500));
        assert_eq!(gbps_text("3 Gigabit"), Some(3000));
        assert_eq!(gbps_text("<unknown>"), None);
        let gens: Vec<_> = ["2.5 GT/s PCIe", "8.0 GT/s", "16.0 GT/s PCIe", "Unknown"]
            .map(pcie_gen_text)
            .to_vec();
        assert_eq!(gens, vec![Some(1), Some(3), Some(4), None]);
        assert_eq!(pcie_width_text("4"), Some(4));
        assert_eq!(pcie_width_text("x16"), Some(16));
        assert_eq!(pcie_width_text("3"), None);
        assert_eq!(pcie_width_text(""), None);
        assert_eq!(pcie_gen_code(Some(4)), Some(4));
        assert_eq!(pcie_gen_code(Some(0)), None);
        assert_eq!(pcie_gen_code(Some(9)), None);
        assert_eq!(pcie(None, None, Some(5), Some(4)), None);
    }

    #[test]
    fn usb_links() {
        // recorded: a USB 3 card reader behind a USB 2 hub port (high speed, SuperSpeed capable)
        assert_eq!(usb_windows(Some(2), Some(0x2)), usb(Some(480), Some(5000)));
        assert_eq!(usb_windows(Some(3), Some(0x3)), usb(Some(5000), Some(5000)));
        assert_eq!(
            usb_windows(Some(3), Some(0xF)),
            usb(Some(10000), Some(10000))
        );
        assert_eq!(usb_windows(Some(2), None), usb(Some(480), None));
        assert_eq!(usb_windows(None, None), None);
        assert_eq!(usb_linux("480", " 3.00"), usb(Some(480), Some(5000)));
        assert_eq!(usb_linux("5000", " 3.20"), usb(Some(5000), None));
        assert_eq!(usb_linux("480", " 2.00"), usb(Some(480), None));
        assert_eq!(usb_linux("", "3.00"), None);
        assert_eq!(usb_mac("super_speed"), usb(Some(5000), None));
        assert_eq!(usb_mac("Up to 10 Gb/s"), usb(Some(10000), None));
        assert_eq!(usb_mac("unknown"), None);
    }
}
