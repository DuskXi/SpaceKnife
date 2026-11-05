//! Small helpers: random ids and tokens, constant-time comparison.

use sha2::{Digest, Sha256};

/// `n` random bytes as lowercase hex.
pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("the system's random source");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// A fresh session token: 32 random bytes, URL-safe base64 without padding (as Python's
/// `secrets.token_urlsafe(32)`).
pub fn new_token() -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("the system's random source");
    let mut out = String::with_capacity(43);
    for chunk in buf.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// Equal, in time independent of where they differ (both hashed first: lengths leak nothing).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let (ha, hb) = (Sha256::digest(a), Sha256::digest(b));
    ha.iter()
        .zip(hb.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// The OS's message for an I/O error, without Rust's " (os error N)" suffix.
pub fn os_message(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

/// A volume as snapshots keep it (the field names of the Python `VolumeInfo.to_dict`).
pub fn volume_to_json(v: &sk_core::volume::VolumeInfo) -> serde_json::Value {
    serde_json::json!({
        "mount_point": v.mount_point,
        "device": v.device,
        "fs_type": v.fs_type,
        "label": v.label,
        "total": v.total,
        "free": v.free,
        "cluster_size": v.cluster_size,
        "is_remote": v.is_remote,
        "is_removable": v.is_removable,
        "serial": v.serial,
        "dev": v.dev,
        "remote_path": v.remote_path,
        "reason": v.reason,
        "note": v.note,
        "reserved": v.reserved,
    })
}

/// [`volume_to_json`] read back (missing fields: defaults).
pub fn volume_from_json(v: &serde_json::Value) -> sk_core::volume::VolumeInfo {
    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let i = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    sk_core::volume::VolumeInfo {
        mount_point: s("mount_point"),
        device: s("device"),
        fs_type: s("fs_type"),
        label: s("label"),
        total: i("total"),
        free: i("free"),
        cluster_size: i("cluster_size"),
        is_remote: b("is_remote"),
        is_removable: b("is_removable"),
        serial: s("serial"),
        dev: v.get("dev").and_then(|x| x.as_u64()).unwrap_or(0),
        remote_path: s("remote_path"),
        reason: s("reason"),
        note: s("note"),
        reserved: i("reserved"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_ids() {
        let t = new_token();
        assert_eq!(t.len(), 43);
        assert!(
            t.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_ne!(t, new_token());
        assert_eq!(random_hex(4).len(), 8);
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
