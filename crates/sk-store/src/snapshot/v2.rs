//! Formats 1 and 2 (`SKSNAP\0\x01`): the snapshots of SpaceKnife 0.3.x, the Python backend.
//! They are read as they are, on demand (nothing is converted on disk); every save writes
//! format 3.
//!
//! ```text
//! b"SKSNAP\0\x01"            8-byte magic (format 1 and 2 alike; the header says which)
//! u32 little-endian H        header length
//! H bytes                    the header: ASCII JSON (`ensure_ascii`), padded with spaces
//! data                       per column, then the names: one zstd frame each
//! ```
//!
//! The header (pydantic's `SnapshotHeader` of `store/snapshot.py`) names each block by
//! `offset`/`length` (relative to the first data byte) and `raw_length`, a column's `dtype` in
//! numpy notation with its byte order (`"<i4"`, `"|u1"`), `created_at` as an ISO 8601 time. Its
//! text came from file names, so it may hold lone surrogates as JSON escapes (`"\udcff"`), which
//! no Rust string holds: they are read as U+FFFD (as everything else shows such names). The
//! columns are those of format 3 (same names, types, kinds and flags), plus `name_off`. The names
//! are the store's own image, `\0name` per node in id order (format 2), or the names back to back
//! in any order that `name_off`/`name_len` index (format 1): an image that is not in id order is
//! laid out anew.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};

use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use super::{BlockInfo, Codec, MAX_RATIO, SnapshotError, SnapshotHeader, read_at_most};
use crate::store::{Store, StoreBuilder, StoreOptions};
use crate::types::Col;

pub const MAGIC: &[u8; 8] = b"SKSNAP\x00\x01";
/// The newest format of the Python backend.
pub const FORMAT_VERSION: u32 = 2;

#[derive(Deserialize)]
struct Block {
    offset: u64,
    length: u64,
    raw_length: u64,
    #[serde(default)]
    dtype: String,
}

fn format_default() -> u32 {
    FORMAT_VERSION
}

#[derive(Deserialize)]
struct Header {
    #[serde(default = "format_default")]
    format: u32,
    #[serde(default)]
    app_version: String,
    created_at: Value,
    roots: Vec<String>,
    node_count: u64,
    #[serde(default)]
    volumes: Vec<Value>,
    #[serde(default)]
    stats: serde_json::Map<String, Value>,
    ext_table: Vec<String>,
    columns: BTreeMap<String, Block>,
    names: Block,
    #[serde(default)]
    checkpoints: serde_json::Map<String, Value>,
}

/// The JSON text with what serde_json does not read but Python's `json` wrote: a lone
/// surrogate escape becomes `�`; `NaN`, `Infinity`, `-Infinity` become `null`.
pub(super) fn readable_json(text: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let needs = text.windows(3).any(|w| {
        (w[0] == b'\\' && (w[1] == b'u' || w[1] == b'U') && (w[2] | 0x20) == b'd')
            || w == b"NaN"
            || w == b"Inf"
    });
    if !needs {
        return std::borrow::Cow::Borrowed(text);
    }
    let hex4 = |i: usize| -> Option<u32> {
        let s = std::str::from_utf8(text.get(i..i + 4)?).ok()?;
        u32::from_str_radix(s, 16).ok()
    };
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    while i < text.len() {
        let b = text[i];
        if in_string {
            match b {
                b'"' => {
                    in_string = false;
                    out.push(b);
                    i += 1;
                }
                b'\\' if text.get(i + 1) == Some(&b'u') => {
                    let Some(u) = hex4(i + 2) else {
                        out.push(b); // not an escape serde_json reads either: its error stands
                        i += 1;
                        continue;
                    };
                    if (0xD800..0xDC00).contains(&u) {
                        let low = (text.get(i + 6) == Some(&b'\\')
                            && text.get(i + 7) == Some(&b'u'))
                        .then(|| hex4(i + 8))
                        .flatten();
                        if low.is_some_and(|l| (0xDC00..0xE000).contains(&l)) {
                            out.extend_from_slice(&text[i..i + 12]); // a pair: as it is
                            i += 12;
                        } else {
                            out.extend_from_slice(b"\\ufffd");
                            i += 6;
                        }
                    } else if (0xDC00..0xE000).contains(&u) {
                        out.extend_from_slice(b"\\ufffd");
                        i += 6;
                    } else {
                        out.extend_from_slice(&text[i..i + 6]);
                        i += 6;
                    }
                }
                b'\\' => {
                    out.extend_from_slice(&text[i..(i + 2).min(text.len())]);
                    i += 2;
                }
                _ => {
                    out.push(b);
                    i += 1;
                }
            }
        } else {
            let rest = &text[i..];
            if b == b'"' {
                in_string = true;
                out.push(b);
                i += 1;
            } else if rest.starts_with(b"-Infinity") {
                out.extend_from_slice(b"null");
                i += 9;
            } else if rest.starts_with(b"Infinity") {
                out.extend_from_slice(b"null");
                i += 8;
            } else if rest.starts_with(b"NaN") {
                out.extend_from_slice(b"null");
                i += 3;
            } else {
                out.push(b);
                i += 1;
            }
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Unix seconds of what pydantic read as a datetime: an ISO 8601 text (`2026-11-08T02:46:06.12Z`,
/// an offset, a space for the `T`, or a date alone; without an offset, as UTC) or a number.
fn unix_seconds(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_f64() {
        return (n >= 0.0 && n.is_finite()).then_some(n as u64);
    }
    let s = v.as_str()?.trim();
    let b = s.as_bytes();
    let num = |a: usize, z: usize| -> Option<i64> {
        let part = s.get(a..z)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut secs = 0i64;
    let mut rest = &s[10..];
    if !rest.is_empty() {
        if !(rest.starts_with('T') || rest.starts_with('t') || rest.starts_with(' ')) {
            return None;
        }
        rest = &rest[1..];
        let t = rest.as_bytes();
        if t.len() < 5 || t[2] != b':' {
            return None;
        }
        let hh: i64 = rest[0..2].parse().ok()?;
        let mm: i64 = rest[3..5].parse().ok()?;
        let mut ss = 0i64;
        rest = &rest[5..];
        if let Some(r) = rest.strip_prefix(':') {
            ss = r.get(0..2)?.parse().ok()?;
            rest = &r[2..];
            if let Some(r) = rest.strip_prefix(['.', ',']) {
                let digits = r.bytes().take_while(u8::is_ascii_digit).count();
                rest = &r[digits..];
            }
        }
        if hh > 23 || mm > 59 || ss > 60 {
            return None;
        }
        secs = hh * 3600 + mm * 60 + ss;
        // the offset: what to subtract to get UTC
        let offset = match rest {
            "" | "Z" | "z" => 0,
            _ => {
                let sign = match rest.as_bytes()[0] {
                    b'+' => 1,
                    b'-' => -1,
                    _ => return None,
                };
                let o = rest[1..].replace(':', "");
                if o.len() < 2 || !o.bytes().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                let oh: i64 = o[0..2].parse().ok()?;
                let om: i64 = o.get(2..4).map_or(Some(0), |x| x.parse().ok())?;
                sign * (oh * 3600 + om * 60)
            }
        };
        secs -= offset;
    }
    let days = super::days_from_civil(y, m as u32, d as u32);
    u64::try_from(days * 86400 + secs).ok()
}

/// numpy's dtype text without its byte order (`"<i4"` → `"i4"`); None for big-endian ones.
fn plain_dtype(dtype: &str) -> Option<&str> {
    match dtype.as_bytes().first() {
        Some(b'<' | b'|' | b'=') => Some(&dtype[1..]),
        Some(b'>') => None,
        _ => Some(dtype),
    }
}

fn item_of(dtype: &str) -> Option<usize> {
    match dtype {
        "u1" | "i1" => Some(1),
        "u2" | "i2" => Some(2),
        "u4" | "i4" => Some(4),
        "u8" | "i8" => Some(8),
        _ => None,
    }
}

fn block_info(b: &Block, dtype: &str) -> BlockInfo {
    BlockInfo {
        dtype: dtype.to_owned(),
        raw_length: b.raw_length,
        codec: Codec::Zstd,
        frames: vec![[b.offset, b.length, b.raw_length]],
    }
}

/// The header of a format 1 or 2 file, as a [`SnapshotHeader`]: each block one zstd frame,
/// dtypes without their byte order, `created_at` in unix seconds.
pub(super) fn parse_header(head: &[u8]) -> Result<SnapshotHeader, SnapshotError> {
    let text = readable_json(head);
    let h: Header =
        serde_json::from_slice(&text).map_err(|e| SnapshotError::Header(e.to_string()))?;
    if !(1..=FORMAT_VERSION).contains(&h.format) {
        return Err(SnapshotError::Format(h.format));
    }
    let created_at = unix_seconds(&h.created_at)
        .ok_or_else(|| SnapshotError::Header("created_at is not a time".into()))?;
    let mut columns = BTreeMap::new();
    for (name, b) in &h.columns {
        let dtype = plain_dtype(&b.dtype).ok_or_else(|| {
            SnapshotError::Header(format!("column {name} is big-endian ({})", b.dtype))
        })?;
        columns.insert(name.clone(), block_info(b, dtype));
    }
    Ok(SnapshotHeader {
        format: h.format,
        app_version: h.app_version,
        created_at,
        roots: h.roots,
        node_count: h.node_count,
        volumes: h.volumes,
        stats: h.stats,
        ext_table: h.ext_table,
        columns,
        names: block_info(&h.names, "u1"),
        checkpoints: h.checkpoints,
    })
}

/// A block's one frame, read from the file at `data + offset` (within `size`).
struct Frame {
    at: u64,
    stored: u64,
    raw: u64,
}

fn frame_of(info: &BlockInfo, data: u64, size: u64) -> Result<Frame, SnapshotError> {
    let [f] = info.frames.as_slice() else {
        return Err(SnapshotError::Corrupt(
            "a block of more than one frame".into(),
        ));
    };
    let at = data
        .checked_add(f[0])
        .filter(|&at| at.checked_add(f[1]).is_some_and(|end| end <= size))
        .ok_or(SnapshotError::Truncated)?;
    if f[2] > f[1].saturating_mul(MAX_RATIO) {
        return Err(SnapshotError::Corrupt(
            "a block says it holds more than its bytes can".into(),
        ));
    }
    Ok(Frame {
        at,
        stored: f[1],
        raw: f[2],
    })
}

/// Reads `[at, at + len)` of a file by position (several threads at once).
struct Span<'a> {
    file: &'a File,
    at: u64,
    end: u64,
}

impl Read for Span<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let want = (self.end - self.at).min(buf.len() as u64) as usize;
        if want == 0 {
            return Ok(0);
        }
        let got = read_at_most(self.file, &mut buf[..want], self.at)?;
        self.at += got as u64;
        Ok(got)
    }
}

/// Decompresses a block's frame into `dst`, which it must fill exactly (like 0.3.4: one frame,
/// nothing left over in it).
fn inflate(file: &File, f: &Frame, dst: &mut [u8]) -> Result<(), SnapshotError> {
    let span = Span {
        file,
        at: f.at,
        end: f.at + f.stored,
    };
    let bad = |e: io::Error| match e.kind() {
        io::ErrorKind::UnexpectedEof => {
            SnapshotError::Corrupt("snapshot block has the wrong length".into())
        }
        _ => SnapshotError::Corrupt(format!("snapshot block is damaged: {e}")),
    };
    let mut dec = zstd::stream::read::Decoder::new(span)
        .map_err(bad)?
        .single_frame();
    dec.read_exact(dst).map_err(bad)?;
    let mut more = [0u8; 1];
    if dec.read(&mut more).map_err(bad)? != 0 {
        return Err(SnapshotError::Corrupt(
            "snapshot block has the wrong length".into(),
        ));
    }
    Ok(())
}

fn inflate_vec(file: &File, f: &Frame) -> Result<Vec<u8>, SnapshotError> {
    let len = usize::try_from(f.raw).map_err(|_| SnapshotError::Truncated)?;
    let mut v = Vec::new();
    v.try_reserve_exact(len)
        .map_err(|_| SnapshotError::TooLarge)?;
    v.resize(len, 0);
    inflate(file, f, &mut v)?;
    Ok(v)
}

/// Offsets as `u64`s from a `name_off` block's bytes.
fn offsets(raw: &[u8], item: usize, signed: bool) -> Vec<u64> {
    raw.chunks_exact(item)
        .map(|b| match (item, signed) {
            (4, false) => u32::from_le_bytes(b.try_into().unwrap()) as u64,
            (4, true) => i32::from_le_bytes(b.try_into().unwrap()).max(0) as u64,
            (8, false) => u64::from_le_bytes(b.try_into().unwrap()),
            _ => i64::from_le_bytes(b.try_into().unwrap()).max(0) as u64,
        })
        .collect()
}

/// Whether `name_off` points at each name of a `\0name` image in id order (what 0.3.x always
/// wrote), read a piece at a time (no copy of the column).
fn offsets_in_order(
    file: &File,
    f: &Frame,
    item: usize,
    signed: bool,
    lens: &[u16],
) -> Result<bool, SnapshotError> {
    let span = Span {
        file,
        at: f.at,
        end: f.at + f.stored,
    };
    let bad = |e: io::Error| SnapshotError::Corrupt(format!("snapshot block is damaged: {e}"));
    let mut dec = zstd::stream::read::Decoder::new(span)
        .map_err(bad)?
        .single_frame();
    let mut buf = vec![0u8; item * 8192];
    let mut at = 1u64;
    let mut in_order = true;
    for piece in lens.chunks(8192) {
        let raw = &mut buf[..piece.len() * item];
        dec.read_exact(raw).map_err(bad)?;
        for (o, &l) in offsets(raw, item, signed).into_iter().zip(piece) {
            in_order &= o == at;
            at += l as u64 + 1;
        }
    }
    let mut more = [0u8; 1];
    if dec.read(&mut more).map_err(bad)? != 0 {
        return Err(SnapshotError::Corrupt(
            "snapshot block has the wrong length".into(),
        ));
    }
    Ok(in_order)
}

/// The store of a format 1 or 2 file (`header` from [`parse_header`]; `data` is where its blocks
/// start). Columns are decompressed straight into the buffers the store then owns, in parallel.
pub(super) fn load(
    file: &File,
    header: &SnapshotHeader,
    data: u64,
    opts: StoreOptions,
) -> Result<Store, SnapshotError> {
    let size = file.metadata()?.len();
    let n = usize::try_from(header.node_count)
        .map_err(|_| SnapshotError::Header("too many nodes".into()))?;
    if n == 0 || n > i32::MAX as usize {
        return Err(SnapshotError::Header("bad node count".into()));
    }
    let mut blocks: Vec<(Col, Frame)> = Vec::new();
    let mut name_off: Option<(Frame, usize, bool)> = None;
    for (name, info) in &header.columns {
        let Some(c) = Col::from_name(name) else {
            continue; // a column this build does not know: left out, like 0.3.4 did
        };
        let item = item_of(&info.dtype);
        let fits = if c == Col::NameOff {
            item.is_some_and(|i| i == 4 || i == 8)
        } else {
            info.dtype == c.dtype()
        };
        if !fits || Some(info.raw_length) != item.map(|i| (n * i) as u64) {
            return Err(SnapshotError::Header(format!(
                "column {name} has the wrong type or length"
            )));
        }
        let f = frame_of(info, data, size)?;
        if c == Col::NameOff {
            name_off = Some((f, item.unwrap_or(4), info.dtype.starts_with('i')));
        } else {
            blocks.push((c, f));
        }
    }
    for required in [Col::Parent, Col::Kind, Col::NameOff, Col::NameLen] {
        if !header.columns.contains_key(required.name()) {
            return Err(SnapshotError::Header(format!(
                "snapshot lacks the column {}",
                required.name()
            )));
        }
    }
    let names = frame_of(&header.names, data, size)?;
    let (off_frame, off_item, off_signed) = name_off.expect("name_off is required");

    // the names' layout first: lengths (2 bytes a node) and whether the image is in id order
    let len_at = blocks
        .iter()
        .position(|(c, _)| *c == Col::NameLen)
        .expect("name_len is required");
    let lens_raw = inflate_vec(file, &blocks[len_at].1)?;
    let lens: Vec<u16> = lens_raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| u16::from_le_bytes(b))
        .collect();
    let image_len: u64 = lens.iter().map(|&l| l as u64 + 1).sum();
    // every name's bytes are in the names block (back to back, or each after its separator)
    if image_len - n as u64 > names.raw {
        return Err(SnapshotError::Corrupt(
            "the names are longer than their block".into(),
        ));
    }
    let image_len = usize::try_from(image_len).map_err(|_| SnapshotError::Truncated)?;
    let in_order = header.format >= 2
        && names.raw == image_len as u64
        && offsets_in_order(file, &off_frame, off_item, off_signed, &lens)?;

    let mut builder = StoreBuilder::try_new(n, image_len, opts).ok_or(SnapshotError::TooLarge)?;
    builder.set_ext_table(header.ext_table.clone());
    {
        let (cols, image) = builder.raw_parts();
        let mut jobs: Vec<(&mut [u8], &Frame)> = Vec::new();
        for (c, dst) in cols {
            if c == Col::NameLen {
                dst.copy_from_slice(&lens_raw);
            } else if let Some((_, f)) = blocks.iter().find(|(b, _)| *b == c) {
                jobs.push((dst, f));
            }
        }
        if in_order {
            jobs.push((image, &names));
            jobs.into_par_iter()
                .try_for_each(|(dst, f)| inflate(file, f, dst))?;
        } else {
            jobs.into_par_iter()
                .try_for_each(|(dst, f)| inflate(file, f, dst))?;
            // laid out anew: `\0name` per node in id order from wherever `name_off` points
            let off = offsets(&inflate_vec(file, &off_frame)?, off_item, off_signed);
            let all = inflate_vec(file, &names)?;
            let mut pos = 0usize;
            for (&o, &l) in off.iter().zip(&lens) {
                let (o, l) = (o as usize, l as usize);
                let name = o
                    .checked_add(l)
                    .and_then(|end| all.get(o..end))
                    .ok_or_else(|| SnapshotError::Corrupt("a name outside the names".into()))?;
                image[pos] = 0;
                image[pos + 1..pos + 1 + l].copy_from_slice(name);
                pos += l + 1;
            }
        }
    }
    builder
        .finish()
        .map_err(|e| SnapshotError::Corrupt(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_python_wrote_reads() {
        let esc = |u: u32| format!("\\u{u:04x}");
        // a lone low surrogate, a pair, a high one alone, an escaped backslash before `udcff`
        let text = format!(
            r#"{{"a":"x{}{}{}y{}z\\udcff","b":NaN,"c":[Infinity,-Infinity],"d":"NaN"}}"#,
            esc(0xDCFF),
            esc(0xD83D),
            esc(0xDE42),
            esc(0xD800)
        );
        let v: Value = serde_json::from_slice(&readable_json(text.as_bytes())).unwrap();
        assert_eq!(v["a"], "x\u{fffd}\u{1f642}y\u{fffd}z\\udcff");
        assert!(v["b"].is_null() && v["c"][0].is_null() && v["c"][1].is_null());
        assert_eq!(v["d"], "NaN");
        let plain = format!(r#"{{"a":"{}"}}"#, esc(0xE9));
        assert!(matches!(
            readable_json(plain.as_bytes()),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn times_pydantic_wrote_read() {
        let t = |s: &str| unix_seconds(&Value::from(s));
        assert_eq!(t("2026-01-01T00:00:00Z"), Some(1_767_225_600));
        assert_eq!(t("2026-01-01T00:00:00.999999Z"), Some(1_767_225_600));
        assert_eq!(t("2026-01-01T08:00:00+08:00"), Some(1_767_225_600));
        assert_eq!(t("2025-12-31T19:00:00-05:00"), Some(1_767_225_600));
        assert_eq!(t("2026-01-01 00:00:00"), Some(1_767_225_600));
        assert_eq!(t("2026-01-01"), Some(1_767_225_600));
        assert_eq!(
            unix_seconds(&Value::from(1_767_225_600.5)),
            Some(1_767_225_600)
        );
        assert_eq!(t("yesterday"), None);
        assert_eq!(t("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn dtypes_lose_their_byte_order() {
        assert_eq!(plain_dtype("<i4"), Some("i4"));
        assert_eq!(plain_dtype("|u1"), Some("u1"));
        assert_eq!(plain_dtype("u2"), Some("u2"));
        assert_eq!(plain_dtype(">i8"), None);
    }
}
