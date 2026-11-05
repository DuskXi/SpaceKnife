//! Human-readable formatting shared by the CLI and server-side labels, in the current interface
//! language (M7.5).
//!
//! Units follow the platform convention (UI-SPEC G13): binary multiples on Windows and Linux,
//! decimal on macOS — always labelled KB/MB/GB/TB like the system file managers do.

use chrono::{DateTime, Utc};

use crate::i18n::t;
use crate::tr;

pub const UNIT_BASE: u32 = if cfg!(target_os = "macos") { 1000 } else { 1024 };
const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// `1.5 MB`, `123 B`, `-2.0 KB` in base [`UNIT_BASE`].
pub fn size(n: f64) -> String {
    size_in(n, UNIT_BASE)
}

/// [`size`] with an explicit base (1000 or 1024, setting 「大小单位」).
pub fn size_in(n: f64, base: u32) -> String {
    let base = base as f64;
    let mut value = n.abs();
    let mut unit = 0;
    while value >= base && unit < UNITS.len() - 1 {
        value /= base;
        unit += 1;
    }
    let sign = if n < 0.0 { "-" } else { "" };
    if unit == 0 {
        return format!("{sign}{} B", value as i64);
    }
    let digits = if value >= 100.0 { 0 } else { 1 };
    format!("{sign}{value:.digits$} {}", UNITS[unit])
}

/// `1,234,567`.
pub fn count(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `350 毫秒`, `12.5 秒`, `3 分 5 秒`.
pub fn duration(seconds: f64) -> String {
    if seconds < 1.0 {
        return tr!("{n} 毫秒", n = format!("{:.0}", seconds * 1000.0));
    }
    if seconds < 60.0 {
        return tr!("{n} 秒", n = format!("{seconds:.1}"));
    }
    let total = seconds as i64;
    tr!("{minutes} 分 {seconds} 秒", minutes = total / 60, seconds = total % 60)
}

/// Relative age of a unix timestamp in the current language, e.g. `3 天前`.
pub fn age(ts: i64, now: Option<DateTime<Utc>>) -> String {
    if ts <= 0 {
        return t("未知").to_owned();
    }
    let now = now.unwrap_or_else(Utc::now).timestamp() as f64;
    let delta = now - ts as f64;
    const DAY: f64 = 86400.0;
    if delta < 60.0 {
        return t("刚刚").to_owned();
    }
    if delta < 3600.0 {
        return tr!("{n} 分钟前", n = (delta / 60.0) as i64);
    }
    if delta < DAY {
        return tr!("{n} 小时前", n = (delta / 3600.0) as i64);
    }
    if delta < DAY * 30.0 {
        return tr!("{n} 天前", n = (delta / DAY) as i64);
    }
    if delta < DAY * 365.0 {
        return tr!("{n} 个月前", n = (delta / (DAY * 30.0)) as i64);
    }
    tr!("{n} 年前", n = (delta / (DAY * 365.0)) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_counts() {
        let _g = crate::i18n::TEST_LOCK.lock();
        assert_eq!(size_in(0.0, 1024), "0 B");
        assert_eq!(size_in(1023.0, 1024), "1023 B");
        assert_eq!(size_in(1536.0, 1024), "1.5 KB");
        assert_eq!(size_in(-1536.0, 1024), "-1.5 KB");
        assert_eq!(size_in(150.0 * 1024.0 * 1024.0, 1024), "150 MB");
        assert_eq!(size_in(836_000_000_000.0, 1000), "836 GB");
        assert_eq!(count(1234567), "1,234,567");
        assert_eq!(count(-1000), "-1,000");
        assert_eq!(count(12), "12");
        assert_eq!(duration(0.35), "350 毫秒");
        assert_eq!(duration(12.53), "12.5 秒");
        assert_eq!(duration(185.0), "3 分 5 秒");
        let now = DateTime::from_timestamp(1_000_000_000, 0);
        assert_eq!(age(0, now), "未知");
        assert_eq!(age(1_000_000_000 - 30, now), "刚刚");
        assert_eq!(age(1_000_000_000 - 3 * 86400, now), "3 天前");
        assert_eq!(age(1_000_000_000 - 800 * 86400, now), "2 年前");
    }
}
