//! User-facing events for the log panel (W8, UI-W8-22…25).
//!
//! An event is an ordinary `tracing` record with the field `event` (its source: scan, perm,
//! monitor, delete, snapshot, export, system) and, when it concerns one, the `scan_id` and absolute
//! `path` it can be located at. The full log keeps going to the log file; frontends show the records
//! that carry `event` plus anything at warning level or above. Messages are the final, localised
//! text. The bus ([`crate::log::LogBus`]) puts the fields in `LogEntry::extra`.

use std::ffi::OsStr;

use sk_native::hardscan::messages;

use crate::i18n::t;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Scan,
    Perm,
    Monitor,
    Delete,
    Snapshot,
    Export,
    System,
}

impl Source {
    pub const fn as_str(self) -> &'static str {
        match self {
            Source::Scan => "scan",
            Source::Perm => "perm",
            Source::Monitor => "monitor",
            Source::Delete => "delete",
            Source::Snapshot => "snapshot",
            Source::Export => "export",
            Source::System => "system",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EventLevel {
    #[default]
    Info,
    Warning,
    Error,
}

/// Log a user-facing event.
pub fn emit(
    source: Source,
    message: &str,
    level: EventLevel,
    scan_id: Option<&str>,
    path: Option<&str>,
) {
    let event = source.as_str();
    let scan_id = scan_id.unwrap_or("");
    let path = path.unwrap_or("");
    match level {
        EventLevel::Info => tracing::info!(event, scan_id, path, "{message}"),
        EventLevel::Warning => tracing::warn!(event, scan_id, path, "{message}"),
        EventLevel::Error => tracing::error!(event, scan_id, path, "{message}"),
    }
}

/// A file name or path as text for logs and the wire: undecodable parts escaped (`\xff` for a
/// byte that is not UTF-8 on POSIX, `\udc80` for a lone surrogate on Windows), so it never fails.
/// The same text as the protocol's names and paths ([`sk_store::names::printable`] of the bytes the
/// store keeps: WTF-8 on Windows, the raw bytes on POSIX).
pub fn printable(name: &OsStr) -> String {
    sk_store::names::printable(name.as_encoded_bytes()).into_owned()
}

const DENIED_ZH: &str = "拒绝"; // in a Chinese system's "access denied" message

/// OS error text -> the wording used in the log panel.
pub fn denied_reason(message: &str) -> String {
    let own = match message {
        m if m == messages::CASE => {
            Some("同一文件夹里另有一项与它只差大小写，按名字打开的是那一项")
        }
        m if m == messages::DUP => {
            Some("同一文件夹里另有一项在 Windows 上显示为同一个名字，按名字分不开")
        }
        m if m == messages::BADNAME => Some("名字里有 Windows 的名字里不能有的字符，按名字打不开"),
        m if m == messages::CAP => Some("只能按文件 ID 打开的文件夹一次要读的项太多，这次没有读"),
        m if m == messages::INCOMPLETE => Some("按文件 ID 打开时没有列全"),
        _ => None,
    };
    if let Some(own) = own {
        return t(own).to_owned();
    }
    let low = message.to_lowercase();
    if low.contains("denied")
        || message.contains(DENIED_ZH)
        || low.contains("permission")
        || low.contains("eacces")
        || low.contains("eperm")
    {
        return t("拒绝访问").to_owned();
    }
    let trimmed = message.trim_end_matches(['。', '.', ' ']);
    if trimmed.is_empty() {
        t("无法读取").to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons() {
        let _g = crate::i18n::TEST_LOCK.lock();
        assert_eq!(denied_reason("Access is denied. "), "拒绝访问");
        assert_eq!(denied_reason("拒绝访问。"), "拒绝访问");
        assert_eq!(
            denied_reason("The device is not ready."),
            "The device is not ready"
        );
        assert_eq!(denied_reason("。"), "无法读取");
        assert_eq!(
            denied_reason(messages::INCOMPLETE),
            "按文件 ID 打开时没有列全"
        );
    }

    #[test]
    fn printable_names() {
        assert_eq!(printable(OsStr::new("a/b")), "a/b");
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let s = std::ffi::OsString::from_wide(&[0x61, 0xDC80, 0x62]);
            assert_eq!(printable(&s), "a\\udc80b");
        }
    }

    #[test]
    fn events_carry_their_fields() {
        use std::sync::Arc;
        use tracing_subscriber::layer::SubscriberExt;
        let bus = Arc::new(crate::log::LogBus::new(10));
        let sub = tracing_subscriber::registry().with(bus.layer());
        tracing::subscriber::with_default(sub, || {
            emit(
                Source::Delete,
                "已删除 {x}",
                EventLevel::Warning,
                Some("s1"),
                Some("C:\\a"),
            );
        });
        let e = &bus.recent(1, sk_protocol::streams::LogLevel::Trace)[0];
        assert_eq!(e.message, "已删除 {x}");
        assert_eq!(e.extra["event"], "delete");
        assert_eq!(e.extra["scan_id"], "s1");
        assert_eq!(e.extra["path"], "C:\\a");
    }
}
