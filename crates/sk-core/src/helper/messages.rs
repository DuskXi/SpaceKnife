//! Wire format between the backend and the helper (Python `helper/messages.py`, docs/native.md 3).
//!
//! A frame is `u32 little-endian length` + payload. The payload's first byte says what follows:
//! `J` one JSON control message (the types below), `B` one bulk binary frame of
//! [`sk_protocol::binary`] (MFT columns, USN records). The backend sends requests; the helper sends
//! `hello` first, then for each request any number of `progress` messages and binary frames (scans
//! and journal reads announce them with `begin`), closed by exactly one `response` or `error` with
//! the request's id. Both ends run the same code, so every message is strict and closed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tr;

pub const PROTOCOL_VERSION: i64 = 1;
pub const KIND_JSON: u8 = 0x4A; // "J"
pub const KIND_BINARY: u8 = 0x42; // "B"
/// Frame header: payload length (kind byte included) as u32 LE, then the kind byte.
pub const HEADER_LEN: usize = 5;
pub const MAX_CONTROL_BYTES: usize = 1 << 20;
pub const MAX_BINARY_BYTES: usize = 1 << 30;
pub const MAX_ID: u64 = (1 << 53) - 1;

pub const MFT_STREAM: &str = "mft";
pub const MFT_LAYOUT: &str = "mft.v1";
pub const DEFAULT_BATCH_ROWS: u64 = 1_000_000;
pub const MAX_BATCH_ROWS: u64 = 4_000_000;
/// read_usn: columns frn u64, parent_frn u64, usn i64, reason u32, attrs u32, name_off u32,
/// name_len u16 and the UTF-8 blob "names" ([`super::usn`])
pub const USN_STREAM: &str = "usn";
pub const USN_LAYOUT: &str = "usn.v1";
pub const DEFAULT_USN_ROWS: u64 = 1_000_000;
pub const MAX_USN_ROWS: u64 = 4_000_000;

/// Scalar entries of a response (`result`, `extras`): JSON null / bool / number / string only.
pub type Scalars = serde_json::Map<String, Value>;

/// The byte stream is not a valid frame sequence; the connection cannot continue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameError(pub String);

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// --------------------------------------------------------------------------- error codes

/// The helper's error codes (`Error.code`).
pub mod code {
    /// The operation needs an elevated helper.
    pub const NEEDS_ADMIN: &str = "needs_admin";
    /// Not allowed in this helper (scan_mft_image when elevated).
    pub const FORBIDDEN: &str = "forbidden";
    /// Well-framed but not a valid request.
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// Not available on this platform / volume / device.
    pub const NOT_SUPPORTED: &str = "not_supported";
    /// Not a local NTFS volume.
    pub const NOT_NTFS: &str = "not_ntfs";
    /// Reading the volume or file failed.
    pub const IO_ERROR: &str = "io_error";
    /// The volume's change journal is not active (or is being deleted).
    pub const USN_INACTIVE: &str = "usn_inactive";
    /// The checkpoint is no longer valid: journal recreated or its records overwritten.
    pub const USN_GONE: &str = "usn_gone";
    /// The MFT reader is missing or broken.
    pub const UNAVAILABLE: &str = "unavailable";
    /// Framing error; the helper closes the connection after this.
    pub const BAD_FRAME: &str = "bad_frame";
    pub const INTERNAL: &str = "internal";

    pub const ALL: [&str; 11] = [
        NEEDS_ADMIN,
        FORBIDDEN,
        INVALID_REQUEST,
        NOT_SUPPORTED,
        NOT_NTFS,
        IO_ERROR,
        USN_INACTIVE,
        USN_GONE,
        UNAVAILABLE,
        BAD_FRAME,
        INTERNAL,
    ];
}

// --------------------------------------------------------------------------- requests (backend → helper)

fn default_batch_rows() -> u64 {
    DEFAULT_BATCH_ROWS
}
fn default_usn_rows() -> u64 {
    DEFAULT_USN_ROWS
}
fn one() -> f64 {
    1.0
}

/// One operation of the whitelist.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Call {
    Ping {},
    /// Drive type, filesystem, label, serial, sector / cluster size of a local NTFS `X:`; elevated
    /// also the FSCTL_GET_NTFS_VOLUME_DATA fields.
    VolumeInfo { volume: String },
    ScanMft {
        volume: String,
        #[serde(default = "default_batch_rows")]
        batch_rows: u64,
        /// Share of time the reader may be busy (I/O limit), (0, 1].
        #[serde(default = "one")]
        duty: f64,
    },
    /// `scan_mft` on an NTFS volume image file. Test-only: refused by an elevated helper, which
    /// must never read arbitrary files.
    ScanMftImage {
        path: String,
        #[serde(default = "default_batch_rows")]
        batch_rows: u64,
        #[serde(default = "one")]
        duty: f64,
    },
    /// The change journal's state: `{journal_id, first_usn, next_usn, max_size …}`.
    QueryUsn { volume: String },
    /// Journal records from `start_usn` (a checkpoint of journal `journal_id`), as binary frames;
    /// the response's extras carry `{journal_id, next_usn, rows, truncated}`.
    ReadUsn {
        volume: String,
        start_usn: i64,
        journal_id: u64,
        #[serde(default = "default_usn_rows")]
        max_rows: u64,
        #[serde(default = "one")]
        duty: f64,
    },
    /// A disk's own health record (Linux, elevated): the raw pages as hex, `{kind: nvme, log}` or
    /// `{kind: ata, identify, general, temperature}`. Only fixed read-only commands are sent.
    DiskHealth { disk: String },
    Shutdown {},
}

impl Call {
    pub fn op(&self) -> &'static str {
        match self {
            Call::Ping {} => "ping",
            Call::VolumeInfo { .. } => "volume_info",
            Call::ScanMft { .. } => "scan_mft",
            Call::ScanMftImage { .. } => "scan_mft_image",
            Call::QueryUsn { .. } => "query_usn",
            Call::ReadUsn { .. } => "read_usn",
            Call::DiskHealth { .. } => "disk_health",
            Call::Shutdown {} => "shutdown",
        }
    }

    /// The constraints of the Python models; `Err` names the field (`call.volume: …`).
    pub fn validate(&self) -> Result<(), String> {
        let field = |name: &str, why: &str| Err(format!("call.{name}: {why}"));
        let volume_ok = |v: &str| {
            let b = v.as_bytes();
            b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
        };
        let check_volume = |v: &str| {
            if volume_ok(v) {
                Ok(())
            } else {
                field("volume", "String should match pattern '^[A-Za-z]:$'")
            }
        };
        let check_duty = |d: f64| {
            if d > 0.0 && d <= 1.0 {
                Ok(())
            } else {
                field("duty", "Input should be greater than 0 and at most 1")
            }
        };
        let check_rows = |name: &str, n: u64, max: u64| {
            if (1..=max).contains(&n) {
                Ok(())
            } else {
                field(name, &format!("Input should be between 1 and {max}"))
            }
        };
        match self {
            Call::Ping {} | Call::Shutdown {} => Ok(()),
            Call::VolumeInfo { volume } | Call::QueryUsn { volume } => check_volume(volume),
            Call::ScanMft { volume, batch_rows, duty } => {
                check_volume(volume)?;
                check_rows("batch_rows", *batch_rows, MAX_BATCH_ROWS)?;
                check_duty(*duty)
            }
            Call::ScanMftImage { path, batch_rows, duty } => {
                let n = path.chars().count();
                if n == 0 || n > 32767 {
                    return field("path", "String should have 1 to 32767 characters");
                }
                check_rows("batch_rows", *batch_rows, MAX_BATCH_ROWS)?;
                check_duty(*duty)
            }
            Call::ReadUsn { volume, start_usn, max_rows, duty, .. } => {
                check_volume(volume)?;
                if *start_usn < 0 {
                    return field("start_usn", "Input should be greater than or equal to 0");
                }
                check_rows("max_rows", *max_rows, MAX_USN_ROWS)?;
                check_duty(*duty)
            }
            Call::DiskHealth { disk } => {
                if valid_linux_disk(disk) {
                    Ok(())
                } else {
                    field("disk", "String should match pattern '^(nvme\\d{1,3}n\\d{1,3}|sd[a-z]{1,4})$'")
                }
            }
        }
    }
}

/// `nvme<1-3 digits>n<1-3 digits>` or `sd<1-4 lowercase letters>`: a whole disk by kernel name.
pub fn valid_linux_disk(name: &str) -> bool {
    fn digits(s: &str) -> bool {
        (1..=3).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    }
    if let Some(rest) = name.strip_prefix("nvme") {
        return match rest.split_once('n') {
            Some((a, b)) => digits(a) && digits(b),
            None => false,
        };
    }
    if let Some(rest) = name.strip_prefix("sd") {
        return (1..=4).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_lowercase());
    }
    false
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestType {
    #[default]
    #[serde(rename = "request")]
    Request,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(rename = "type", default)]
    pub kind: RequestType,
    pub id: u64,
    pub call: Call,
}

impl Request {
    pub fn new(id: u64, call: Call) -> Self {
        Self {
            kind: RequestType::Request,
            id,
            call,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_ID).contains(&self.id) {
            return Err("id: Input should be between 1 and 2^53 - 1".into());
        }
        self.call.validate()
    }

    /// Parse and validate a request frame body; the error is a short reason.
    pub fn parse(body: &[u8]) -> Result<Self, String> {
        let req: Request = serde_json::from_slice(body).map_err(|e| e.to_string())?;
        req.validate()?;
        Ok(req)
    }
}

// --------------------------------------------------------------------------- replies (helper → backend)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Scan,
    Send,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Scan => "scan",
            Phase::Send => "send",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Hello {
        version: i64,
        token: String,
        pid: u32,
        elevated: bool,
    },
    /// Heartbeat while a long operation runs (also keeps the client's inactivity timeout fed).
    Progress {
        id: u64,
        phase: Phase,
        elapsed: f64,
        /// Bytes of $MFT parsed / of journal read (phase scan)
        #[serde(default)]
        done: u64,
        /// Bytes of $MFT / of journal to read; 0: not known yet
        #[serde(default)]
        total: u64,
    },
    /// Sent before the bulk frames of a scan or journal read, so the client can allocate the
    /// table once.
    Begin { id: u64, rows: u64, frames: u64 },
    /// `result`: the operation's result (for scans: geometry, row/frame counts, timings);
    /// `extras`: scans and journal reads only, the scalar entries of the table (`bad_records` …;
    /// `read_usn`: `journal_id`, `next_usn`, `rows`, `truncated`).
    Response {
        id: u64,
        #[serde(default)]
        result: Scalars,
        #[serde(default)]
        extras: Scalars,
    },
    Error {
        #[serde(default)]
        id: Option<u64>,
        code: String,
        message: String,
    },
}

impl Reply {
    pub fn response(id: u64, result: Scalars, extras: Scalars) -> Self {
        Reply::Response { id, result, extras }
    }

    pub fn error(id: Option<u64>, code: &str, message: &str) -> Self {
        let mut message = message.to_owned();
        if message.chars().count() > 4096 {
            message = message.chars().take(4096).collect();
        }
        Reply::Error {
            id,
            code: code.to_owned(),
            message,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Reply::Hello { .. } => "hello",
            Reply::Progress { .. } => "progress",
            Reply::Begin { .. } => "begin",
            Reply::Response { .. } => "response",
            Reply::Error { .. } => "error",
        }
    }

    /// The constraints of the Python models.
    pub fn validate(&self) -> Result<(), String> {
        let id_ok = |id: u64| (1..=MAX_ID).contains(&id);
        let scalars_ok = |m: &Scalars| m.values().all(|v| !v.is_array() && !v.is_object());
        match self {
            Reply::Hello { token, pid, .. } => {
                if token.len() != 64 || !token.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                    return Err("token: String should match pattern '^[0-9a-f]{64}$'".into());
                }
                if *pid == 0 {
                    return Err("pid: Input should be greater than 0".into());
                }
                Ok(())
            }
            Reply::Progress { id, elapsed, .. } => {
                if !id_ok(*id) || elapsed.is_nan() || *elapsed < 0.0 {
                    return Err("progress: id or elapsed out of range".into());
                }
                Ok(())
            }
            Reply::Begin { id, frames, .. } => {
                if !id_ok(*id) || *frames < 1 {
                    return Err("begin: id or frames out of range".into());
                }
                Ok(())
            }
            Reply::Response { id, result, extras } => {
                if !id_ok(*id) {
                    return Err("response: id out of range".into());
                }
                if !scalars_ok(result) || !scalars_ok(extras) {
                    return Err("response: values must be scalars".into());
                }
                Ok(())
            }
            Reply::Error { id, code, message } => {
                if id.is_some_and(|id| !id_ok(id)) {
                    return Err("error: id out of range".into());
                }
                if !code::ALL.contains(&code.as_str()) {
                    return Err(format!("error: unknown code {code:?}"));
                }
                if message.chars().count() > 4096 {
                    return Err("error: message too long".into());
                }
                Ok(())
            }
        }
    }

    /// Parse and validate a reply frame body.
    pub fn parse(body: &[u8]) -> Result<Self, String> {
        let reply: Reply = serde_json::from_slice(body).map_err(|e| e.to_string())?;
        reply.validate()?;
        Ok(reply)
    }
}

// --------------------------------------------------------------------------- framing

pub fn frame_header(kind: u8, body_len: usize) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..4].copy_from_slice(&((body_len as u32).wrapping_add(1)).to_le_bytes());
    h[4] = kind;
    h
}

/// A JSON control frame: header and body.
pub fn json_frame<T: Serialize>(message: &T) -> ([u8; HEADER_LEN], Vec<u8>) {
    let body = serde_json::to_vec(message).expect("helper messages always serialise");
    (frame_header(KIND_JSON, body.len()), body)
}

/// Validate a frame header; returns (kind, body length).
pub fn check_header(head: &[u8; HEADER_LEN], max_binary: usize) -> Result<(u8, usize), FrameError> {
    let length = u32::from_le_bytes(head[..4].try_into().expect("4 bytes")) as usize;
    let kind = head[4];
    let limit = match kind {
        KIND_JSON => MAX_CONTROL_BYTES,
        KIND_BINARY => max_binary,
        _ => return Err(FrameError(tr!("未知的帧类型 0x{kind:02x}", kind = kind))),
    };
    if length < 1 || length - 1 > limit {
        return Err(FrameError(tr!("帧长度 {length} 超出范围", length = length)));
    }
    Ok((kind, length - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_strictly_validated() {
        let ok = Request::parse(br#"{"type":"request","id":3,"call":{"op":"scan_mft","volume":"d:"}}"#).unwrap();
        assert_eq!(ok.call.op(), "scan_mft");
        assert!(matches!(ok.call, Call::ScanMft { batch_rows: DEFAULT_BATCH_ROWS, duty, .. } if duty == 1.0));
        let bad: &[&[u8]] = &[
            br#"{"type":"request","id":1,"call":{"op":"scan_mft","volume":"C:\\"}}"#,
            br#"{"type":"request","id":1,"call":{"op":"scan_mft","volume":"CC:"}}"#,
            br#"{"type":"request","id":1,"call":{"op":"scan_mft","volume":"1:"}}"#,
            br#"{"type":"request","id":1,"call":{"op":"scan_mft","volume":"C:\n"}}"#,
            br#"{"type":"request","id":1,"call":{"op":"volume_info","volume":"\\\\.\\C:"}}"#,
            br#"{"type":"request","id":1,"call":{"op":"delete","path":"C:\\x"}}"#,
            br#"{"type":"request","id":1,"call":{"op":"ping","extra":1}}"#,
            br#"{"type":"request","id":"1","call":{"op":"ping"}}"#,
            br#"{"type":"request","id":0,"call":{"op":"ping"}}"#,
            br#"{"type":"request","id":1,"call":{"op":"scan_mft","volume":"C:","batch_rows":0}}"#,
            br#"{"type":"request","id":1,"call":{"op":"scan_mft","volume":"C:","batch_rows":"5"}}"#,
            br#"{"type":"other","id":1,"call":{"op":"ping"}}"#,
            br#"{"type":"request","id":1,"call":{"op":"ping"},"x":1}"#,
        ];
        for raw in bad {
            assert!(Request::parse(raw).is_err(), "{}", String::from_utf8_lossy(raw));
        }
        assert!(Reply::parse(br#"{"type":"hello","version":1,"token":"XYZ","pid":1,"elevated":false}"#).is_err());
        let hello = Reply::Hello {
            version: 1,
            token: "a".repeat(64),
            pid: 7,
            elevated: false,
        };
        assert_eq!(Reply::parse(&serde_json::to_vec(&hello).unwrap()).unwrap(), hello);
        assert!(Reply::parse(br#"{"type":"response","id":1,"result":{"a":[1]}}"#).is_err());
        assert!(Reply::parse(br#"{"type":"error","code":"nope","message":"x"}"#).is_err());
    }

    #[test]
    fn usn_requests_are_strictly_validated() {
        let top = u64::MAX;
        let ok = Request::parse(
            format!(r#"{{"type":"request","id":1,"call":{{"op":"read_usn","volume":"c:","start_usn":0,"journal_id":{top}}}}}"#)
                .as_bytes(),
        )
        .unwrap();
        assert!(matches!(ok.call, Call::ReadUsn { journal_id, max_rows: 1_000_000, .. } if journal_id == top));
        let again = Request::parse(&serde_json::to_vec(&ok).unwrap()).unwrap();
        assert_eq!(again, ok); // 64-bit ids survive JSON
        let big = top as u128 + 1;
        let bad = [
            r#""op":"query_usn","volume":"C:\\""#.to_owned(),
            r#""op":"query_usn","volume":"\\\\.\\C:""#.to_owned(),
            r#""op":"read_usn","volume":"C:","start_usn":-1,"journal_id":1"#.to_owned(),
            r#""op":"read_usn","volume":"C:","start_usn":"5","journal_id":1"#.to_owned(),
            format!(r#""op":"read_usn","volume":"C:","start_usn":0,"journal_id":{big}"#),
            r#""op":"read_usn","volume":"C:","start_usn":0,"journal_id":1,"max_rows":0"#.to_owned(),
            r#""op":"read_usn","volume":"C:","start_usn":0,"journal_id":1,"max_rows":4000001"#.to_owned(),
            r#""op":"read_usn","volume":"C:","start_usn":0,"journal_id":1,"duty":0"#.to_owned(),
            r#""op":"read_usn","volume":"C:","start_usn":0,"journal_id":1,"duty":1.5"#.to_owned(),
            r#""op":"read_usn","volume":"C:","start_usn":0,"journal_id":1,"path":"C:\\x""#.to_owned(),
            r#""op":"read_usn","volume":"C:","start_usn":0"#.to_owned(),
        ];
        for call in bad {
            let raw = format!(r#"{{"type":"request","id":1,"call":{{{call}}}}}"#);
            assert!(Request::parse(raw.as_bytes()).is_err(), "{raw}");
        }
        let mut extras = Scalars::new();
        extras.insert("journal_id".into(), top.into());
        extras.insert("next_usn".into(), (1u64 << 62).into());
        extras.insert("truncated".into(), true.into());
        let reply = Reply::response(3, Scalars::new(), extras);
        assert_eq!(Reply::parse(&serde_json::to_vec(&reply).unwrap()).unwrap(), reply);
    }

    #[test]
    fn disk_names() {
        for good in ["nvme0n1", "nvme12n3", "sda", "sdab"] {
            assert!(valid_linux_disk(good), "{good}");
        }
        for bad in ["sda1", "nvme0", "nvme0n1p2", "../sda", "loop0", "dm-0", "", "sdabcde", "nvme1234n1"] {
            assert!(!valid_linux_disk(bad), "{bad}");
        }
    }

    #[test]
    fn frame_header_checks() {
        assert_eq!(check_header(&frame_header(KIND_JSON, 10), 0), Ok((KIND_JSON, 10)));
        assert_eq!(check_header(&frame_header(KIND_BINARY, 100), 100), Ok((KIND_BINARY, 100)));
        assert!(check_header(&frame_header(KIND_BINARY, 101), 100).is_err());
        assert!(check_header(&frame_header(KIND_JSON, MAX_CONTROL_BYTES + 1), 0).is_err());
        assert!(check_header(&[1, 0, 0, 0, 0x00], 0).is_err());
        assert!(check_header(&[0, 0, 0, 0, KIND_JSON], 0).is_err());
    }
}
