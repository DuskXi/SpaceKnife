//! Snapshot files (`.sks`, format 3): a complete scan, columnar, optionally compressed.
//!
//! ```text
//! b"SKSNAP\0\x03"            8-byte magic (the last byte is the format)
//! u32 little-endian H        header length
//! H bytes                    SnapshotHeader as JSON (ASCII)
//! data                       frames; offsets in the header are relative to the first data byte
//! ```
//!
//! Every column but `name_off` is one block, its rows' little-endian bytes; the names are one
//! block holding the store's own image, `\0name` per node in id order (`name_off` follows from
//! `name_len` on load). A block is cut into frames of at most [`FRAME_BYTES`] raw bytes, each
//! stored as it is ([`Codec::Raw`]) or as one zstd frame with a checksum ([`Codec::Zstd`]):
//! frames are written and read in parallel, and decompressed straight into the buffers the
//! loaded store then owns (no second copy). Loading checks lengths and the structure (parents,
//! child ranges, names) before the store is used.
//!
//! Files are written to a temporary name and renamed, so a crash never leaves a half-written
//! snapshot behind; frames are compressed a few at a time (at most [`SAVE_THREADS`] threads) and
//! streamed into the file, the header (JSON, padded with spaces) goes last into room reserved
//! for it. Format 3 is not compatible with the Python backend's formats 1 and 2.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::names::{normcase, normpath};
use crate::store::{Nodes, Store, StoreBuilder, StoreOptions};
use crate::types::*;

#[cfg(target_endian = "big")]
compile_error!("snapshots store little-endian columns: big-endian targets are not supported");

pub const MAGIC: &[u8; 8] = b"SKSNAP\x00\x03";
pub const FORMAT_VERSION: u32 = 3;
pub const SUFFIX: &str = "sks";
pub const KEEP_RECENT: usize = 10;
/// Raw bytes per frame.
pub const FRAME_BYTES: usize = 4 << 20;
/// Most zstd threads a save uses (each holds a frame and its compressed copy while it works).
pub const SAVE_THREADS: usize = 4;
const MAX_HEADER: u32 = 64 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Raw,
    #[default]
    Zstd,
}

/// One frame: `[offset, stored length, raw length]`.
pub type Frame = [u64; 3];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockInfo {
    /// numpy notation (`"i4"`, `"u2"`, ...; `"u1"` for the names)
    pub dtype: String,
    pub raw_length: u64,
    pub codec: Codec,
    pub frames: Vec<Frame>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotHeader {
    pub format: u32,
    pub app_version: String,
    /// unix seconds
    pub created_at: u64,
    pub roots: Vec<String>,
    pub node_count: u64,
    #[serde(default)]
    pub volumes: Vec<Value>,
    #[serde(default)]
    pub stats: serde_json::Map<String, Value>,
    pub ext_table: Vec<String>,
    pub columns: BTreeMap<String, BlockInfo>,
    pub names: BlockInfo,
    /// USN / FSEvents resume points
    #[serde(default)]
    pub checkpoints: serde_json::Map<String, Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("not a SpaceKnife snapshot (bad magic)")]
    BadMagic,
    #[error("snapshot format {0} is not supported by this build")]
    Format(u32),
    #[error("invalid snapshot header: {0}")]
    Header(String),
    #[error("snapshot is truncated")]
    Truncated,
    #[error("snapshot is damaged: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What [`save`] writes besides the store.
#[derive(Clone, Debug)]
pub struct SaveOptions {
    pub roots: Vec<String>,
    pub volumes: Vec<Value>,
    pub stats: serde_json::Map<String, Value>,
    pub checkpoints: serde_json::Map<String, Value>,
    pub codec: Codec,
    /// zstd level (1 is fast and already shrinks a scan to about a quarter)
    pub level: i32,
    /// unix seconds (None: now)
    pub created_at: Option<u64>,
    pub app_version: String,
    /// compressor threads (0: [`default_threads`]; the server may pass the process's usable
    /// CPUs, `sk_native::cpus::usable()`, capped at [`SAVE_THREADS`])
    pub threads: usize,
}

impl Default for SaveOptions {
    fn default() -> Self {
        SaveOptions {
            roots: Vec::new(),
            volumes: Vec::new(),
            stats: Default::default(),
            checkpoints: Default::default(),
            codec: Codec::Zstd,
            level: 1,
            created_at: None,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            threads: 0,
        }
    }
}

impl SaveOptions {
    pub fn roots(roots: impl IntoIterator<Item = impl Into<String>>) -> Self {
        SaveOptions {
            roots: roots.into_iter().map(Into::into).collect(),
            ..Default::default()
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What one frame is cut from.
enum Source {
    Col(Col, std::ops::Range<usize>),
    Names(usize, std::ops::Range<usize>), // run index, byte range in it
}

impl Source {
    fn raw_len(&self) -> u64 {
        match self {
            Source::Col(c, r) => (r.len() * c.item()) as u64,
            Source::Names(_, r) => r.len() as u64,
        }
    }
}

/// More digits than any offset or length the header will hold: the header is written last, into
/// room reserved (with these in its numbers) before the data, padded with spaces.
const PLACEHOLDER: u64 = 1_000_000_000_000_000;

/// Compressor threads of a save by default: at most [`SAVE_THREADS`], fewer on a machine with
/// fewer CPUs (the server may pass `sk_native::cpus::usable()` instead, see
/// [`SaveOptions::threads`]).
pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, SAVE_THREADS)
}

/// Writes `nodes` to `path` (atomically: a temporary file, then a rename; nothing is left behind
/// when it fails). Safe while a scan appends (the atomic columns are read value by value), but a
/// snapshot is meant for a complete tree.
///
/// The rows, columns and name chunks are those of `nodes`, fixed when its guard was taken; hold
/// that guard (a [`crate::Reader`]) for the whole save, so no change reaches the rows meanwhile.
/// (Python saved without the lock first and again under it when the tree changed; in Rust the
/// reader is required, and since a waiting writer no longer holds up other readers, a long save
/// only delays the changes.)
///
/// Frames are compressed by at most [`SaveOptions::threads`] threads a few at a time and go
/// straight into the file in order (nothing beyond those frames is held in memory); the header is
/// written last into room reserved after the magic. The format is unchanged.
pub fn save(
    nodes: &Nodes,
    path: &Path,
    opts: &SaveOptions,
) -> Result<SnapshotHeader, SnapshotError> {
    let n = nodes.len();
    let runs: Vec<&[u8]> = nodes.name_runs().collect();
    // the frames, block by block
    let mut blocks: Vec<(String, &'static str, u64, Vec<Source>)> = Vec::new();
    for c in Col::ALL {
        if c == Col::NameOff {
            continue;
        }
        let rows = (FRAME_BYTES / c.item()).max(1);
        let frames = (0..n)
            .step_by(rows)
            .map(|a| Source::Col(c, a..(a + rows).min(n)))
            .collect();
        blocks.push((
            c.name().to_string(),
            c.dtype(),
            (n * c.item()) as u64,
            frames,
        ));
    }
    let mut name_frames = Vec::new();
    let mut name_bytes = 0u64;
    for (k, run) in runs.iter().enumerate() {
        name_bytes += run.len() as u64;
        for a in (0..run.len()).step_by(FRAME_BYTES) {
            name_frames.push(Source::Names(k, a..(a + FRAME_BYTES).min(run.len())));
        }
    }
    let raw_of = |s: &Source| -> std::borrow::Cow<'_, [u8]> {
        match s {
            Source::Col(c, r) => nodes.column_bytes(*c, r.clone()),
            Source::Names(k, r) => std::borrow::Cow::Borrowed(&runs[*k][r.clone()]),
        }
    };
    let sources: Vec<&Source> = blocks
        .iter()
        .flat_map(|b| b.3.iter())
        .chain(name_frames.iter())
        .collect();

    // the header, given each frame's (offset, stored length) in `sources` order
    let created_at = opts.created_at.unwrap_or_else(now_secs);
    let ext_table: Vec<String> = nodes.ext_table().iter().map(|e| e.to_string()).collect();
    let header_of = |stored: &dyn Fn(usize) -> (u64, u64)| -> SnapshotHeader {
        let mut i = 0usize;
        let mut frames_of = |list: &[Source]| -> Vec<Frame> {
            list.iter()
                .map(|s| {
                    let (offset, len) = stored(i);
                    i += 1;
                    [offset, len, s.raw_len()]
                })
                .collect()
        };
        let mut columns = BTreeMap::new();
        for (name, dtype, raw, list) in &blocks {
            let frames = frames_of(list);
            columns.insert(
                name.clone(),
                BlockInfo {
                    dtype: dtype.to_string(),
                    raw_length: *raw,
                    codec: opts.codec,
                    frames,
                },
            );
        }
        let names = BlockInfo {
            dtype: "u1".into(),
            raw_length: name_bytes,
            codec: opts.codec,
            frames: frames_of(&name_frames),
        };
        SnapshotHeader {
            format: FORMAT_VERSION,
            app_version: opts.app_version.clone(),
            created_at,
            roots: opts.roots.clone(),
            node_count: n as u64,
            volumes: opts.volumes.clone(),
            stats: opts.stats.clone(),
            ext_table: ext_table.clone(),
            columns,
            names,
            checkpoints: opts.checkpoints.clone(),
        }
    };
    let to_json = |h: &SnapshotHeader| {
        serde_json::to_vec(h).map_err(|e| SnapshotError::Header(e.to_string()))
    };
    let reserve = to_json(&header_of(&|_| (PLACEHOLDER, PLACEHOLDER)))?.len();
    if reserve > MAX_HEADER as usize {
        return Err(SnapshotError::Header("header too large".into()));
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    let result = (|| -> Result<SnapshotHeader, SnapshotError> {
        let file = File::create(&tmp)?;
        let mut out = io::BufWriter::with_capacity(1 << 20, file);
        out.write_all(MAGIC)?;
        out.write_all(&(reserve as u32).to_le_bytes())?;
        out.write_all(&vec![b' '; reserve])?;
        let mut stored: Vec<u64> = Vec::with_capacity(sources.len());
        match opts.codec {
            Codec::Raw => {
                for s in &sources {
                    let raw = raw_of(s);
                    out.write_all(&raw)?;
                    stored.push(raw.len() as u64);
                }
            }
            Codec::Zstd => {
                let threads = if opts.threads == 0 {
                    default_threads()
                } else {
                    opts.threads
                };
                let level = opts.level;
                let compress_all = |batch: &[&Source]| -> io::Result<Vec<Vec<u8>>> {
                    batch
                        .par_iter()
                        .map(|s| compress(&raw_of(s), level))
                        .collect()
                };
                let out = &mut out;
                let stored = &mut stored;
                let mut write_all = |packed: Vec<Vec<u8>>| -> io::Result<()> {
                    for frame in packed {
                        out.write_all(&frame)?;
                        stored.push(frame.len() as u64);
                    }
                    Ok(())
                };
                // a batch compresses while the one before it is written
                let mut stream = || -> io::Result<()> {
                    let mut batches = sources.chunks(threads * 2);
                    let mut ready = match batches.next() {
                        Some(b) => compress_all(b)?,
                        None => return Ok(()),
                    };
                    for b in batches {
                        let (written, next) = rayon::join(
                            || write_all(std::mem::take(&mut ready)),
                            || compress_all(b),
                        );
                        written?;
                        ready = next?;
                    }
                    write_all(ready)
                };
                match rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .thread_name(|i| format!("snapshot-zstd-{i}"))
                    .build()
                {
                    Ok(pool) => pool.install(stream)?,
                    Err(_) => stream()?,
                }
            }
        }
        let mut offsets = Vec::with_capacity(stored.len());
        let mut at = 0u64;
        for len in &stored {
            offsets.push(at);
            at += len;
        }
        let header = header_of(&|i| (offsets[i], stored[i]));
        let head = to_json(&header)?;
        if head.len() > reserve {
            // cannot happen: every number is below the placeholder
            return Err(SnapshotError::Header(
                "snapshot header outgrew its room".into(),
            ));
        }
        let mut file = out.into_inner().map_err(|e| e.into_error())?;
        file.seek(SeekFrom::Start((MAGIC.len() + 4) as u64))?;
        file.write_all(&head)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        Ok(header)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn compress(raw: &[u8], level: i32) -> io::Result<Vec<u8>> {
    let mut enc = zstd::stream::Encoder::new(Vec::with_capacity(raw.len() / 3 + 64), level)?;
    enc.include_checksum(true)?;
    enc.set_pledged_src_size(Some(raw.len() as u64))?;
    enc.write_all(raw)?;
    enc.finish()
}

/// The header of a snapshot file.
pub fn read_header(path: &Path) -> Result<SnapshotHeader, SnapshotError> {
    let mut file = File::open(path)?;
    Ok(read_header_from(&mut file)?.0)
}

fn read_header_from(file: &mut File) -> Result<(SnapshotHeader, u64), SnapshotError> {
    let mut prefix = [0u8; 12];
    if file.read_exact(&mut prefix).is_err() || prefix[..6] != MAGIC[..6] || prefix[6] != 0 {
        return Err(SnapshotError::BadMagic);
    }
    if prefix[7] as u32 != FORMAT_VERSION {
        return Err(SnapshotError::Format(prefix[7] as u32));
    }
    let len = u32::from_le_bytes(prefix[8..12].try_into().unwrap());
    if len > MAX_HEADER {
        return Err(SnapshotError::Header("header too large".into()));
    }
    let mut head = vec![0u8; len as usize];
    file.read_exact(&mut head)
        .map_err(|_| SnapshotError::Truncated)?;
    let header: SnapshotHeader =
        serde_json::from_slice(&head).map_err(|e| SnapshotError::Header(e.to_string()))?;
    if header.format != FORMAT_VERSION {
        return Err(SnapshotError::Format(header.format));
    }
    Ok((header, 12 + len as u64))
}

type Job<'a> = (&'a mut [u8], Frame, Codec);

/// Cuts a block's destination into its frames' pieces.
fn split_frames<'a>(
    dst: &'a mut [u8],
    info: &BlockInfo,
    data: u64,
    size: u64,
    jobs: &mut Vec<Job<'a>>,
) -> Result<(), SnapshotError> {
    let total: u64 = info.frames.iter().map(|f| f[2]).sum();
    if total != dst.len() as u64 {
        return Err(SnapshotError::Corrupt(
            "a block's frames do not add up".into(),
        ));
    }
    let mut rest: &'a mut [u8] = dst;
    for f in &info.frames {
        if data
            .checked_add(f[0])
            .and_then(|v| v.checked_add(f[1]))
            .is_none_or(|end| end > size)
        {
            return Err(SnapshotError::Truncated);
        }
        if info.codec == Codec::Raw && f[1] != f[2] {
            return Err(SnapshotError::Corrupt(
                "a raw frame with a stored length".into(),
            ));
        }
        let (head, tail) = std::mem::take(&mut rest).split_at_mut(f[2] as usize);
        rest = tail;
        jobs.push((head, *f, info.codec));
    }
    Ok(())
}

fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let got = file.seek_read(&mut buf[done..], offset + done as u64)?;
            if got == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            done += got;
        }
        Ok(())
    }
}

/// The store of a snapshot file, with default options.
pub fn load(path: &Path) -> Result<(Store, SnapshotHeader), SnapshotError> {
    load_with(path, StoreOptions::default())
}

/// The store of a snapshot file. Each frame is read (and decompressed) straight into the buffer
/// the store then owns, in parallel: opening a snapshot needs about the store itself.
pub fn load_with(
    path: &Path,
    opts: StoreOptions,
) -> Result<(Store, SnapshotHeader), SnapshotError> {
    let mut file = File::open(path)?;
    let (header, data) = read_header_from(&mut file)?;
    let size = file.metadata()?.len();
    let n = usize::try_from(header.node_count)
        .map_err(|_| SnapshotError::Header("too many nodes".into()))?;
    if n == 0 || n > i32::MAX as usize {
        return Err(SnapshotError::Header("bad node count".into()));
    }
    for (name, info) in &header.columns {
        let Some(c) = Col::from_name(name) else {
            continue;
        };
        if info.dtype != c.dtype() || info.raw_length != (n * c.item()) as u64 {
            return Err(SnapshotError::Header(format!(
                "column {name} has the wrong type or length"
            )));
        }
    }
    for required in [Col::Parent, Col::Kind, Col::NameLen] {
        if !header.columns.contains_key(required.name()) {
            return Err(SnapshotError::Header(format!(
                "snapshot lacks the column {}",
                required.name()
            )));
        }
    }
    let image_len =
        usize::try_from(header.names.raw_length).map_err(|_| SnapshotError::Truncated)?;
    let mut builder = StoreBuilder::new(n, image_len, opts);
    builder.set_ext_table(header.ext_table.clone());
    {
        let (cols, image) = builder.raw_parts();
        let mut jobs: Vec<Job<'_>> = Vec::new();
        for (c, dst) in cols {
            if let Some(info) = header.columns.get(c.name()) {
                split_frames(dst, info, data, size, &mut jobs)?;
            }
        }
        split_frames(image, &header.names, data, size, &mut jobs)?;
        let file = &file;
        jobs.into_par_iter()
            .try_for_each(|(dst, f, codec)| -> Result<(), SnapshotError> {
                let at = data + f[0];
                match codec {
                    Codec::Raw => read_at(file, dst, at).map_err(|_| SnapshotError::Truncated),
                    Codec::Zstd => {
                        let mut src = vec![0u8; f[1] as usize];
                        read_at(file, &mut src, at).map_err(|_| SnapshotError::Truncated)?;
                        let got = zstd::bulk::decompress_to_buffer(&src, dst).map_err(|e| {
                            SnapshotError::Corrupt(format!("a frame does not decompress: {e}"))
                        })?;
                        if got != dst.len() {
                            return Err(SnapshotError::Corrupt(
                                "a frame has the wrong length".into(),
                            ));
                        }
                        Ok(())
                    }
                }
            })?;
    }
    let store = builder
        .finish()
        .map_err(|e| SnapshotError::Corrupt(e.to_string()))?;
    Ok((store, header))
}

// --------------------------------------------------------------------------- automatic files

/// Stable folder name for a set of scan roots: `<label>-<12 hex digits>`.
pub fn root_key(roots: &[String]) -> String {
    let mut norm: Vec<Vec<u8>> = roots
        .iter()
        .map(|r| normcase(&normpath(r.as_bytes())).into_owned())
        .collect();
    norm.sort();
    let joined = norm.join(&b'\n');
    // FNV-1a, 64 bits
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in &joined {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let first = roots.first().map(String::as_str).unwrap_or("");
    let trimmed = first.trim_end_matches(['\\', '/']);
    let base = trimmed
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(first);
    let label: String = base
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let label: String = label.trim_matches('_').chars().take(24).collect();
    let label = if label.is_empty() {
        "root".to_string()
    } else {
        label
    };
    format!("{label}-{:012x}", h >> 16)
}

/// `YYYYmmdd-HHMMSS` of unix seconds (UTC).
pub fn stamp(secs: u64) -> String {
    let (y, m, d) = civil_from_days((secs / 86400) as i64);
    let s = secs % 86400;
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// Unix seconds of a [`stamp`], also with the `_NNN` [`auto_path`] adds to a second's later
/// snapshots.
pub fn parse_stamp(text: &str) -> Option<u64> {
    let text = match text.split_once('_') {
        Some((stamp, n)) if !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()) => stamp,
        Some(_) => return None,
        None => text,
    };
    let b = text.as_bytes();
    if b.len() != 15
        || b[8] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| i == 8 || c.is_ascii_digit())
    {
        return None;
    }
    let num = |a: usize, z: usize| text[a..z].parse::<i64>().ok();
    let (y, m, d) = (num(0, 4)?, num(4, 6)?, num(6, 8)?);
    let (hh, mm, ss) = (num(9, 11)?, num(11, 13)?, num(13, 15)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let days = days_from_civil(y, m as u32, d as u32);
    u64::try_from(days * 86400 + hh * 3600 + mm * 60 + ss).ok()
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + (m <= 2) as i64;
    (y, m, d)
}

/// ISO (year, week) of a day number (days since 1970-01-01).
fn iso_week(days: i64) -> (i64, i64) {
    let weekday = (days + 3).rem_euclid(7); // 0 = Monday
    let thursday = days - weekday + 3;
    let (y, _, _) = civil_from_days(thursday);
    let jan1 = days_from_civil(y, 1, 1);
    (y, (thursday - jan1) / 7 + 1)
}

/// `<base>/<root key>/<stamp>.sks`; a snapshot of the same second there already (a manual save
/// right after the auto-save) makes it `<stamp>_001.sks`, `_002` … — later names sort after
/// earlier ones ([`parse_stamp`] reads them, [`prune`] and the newest-first listings follow).
pub fn auto_path(roots: &[String], when: Option<u64>, base: &Path) -> PathBuf {
    let folder = base.join(root_key(roots));
    let stamp = stamp(when.unwrap_or_else(now_secs));
    let first = folder.join(format!("{stamp}.{SUFFIX}"));
    if !first.exists() {
        return first;
    }
    (1..1000)
        .map(|n| folder.join(format!("{stamp}_{n:03}.{SUFFIX}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// Applies retention to one root's snapshot folder (the newest `keep_recent`, plus the newest of
/// each older ISO week); returns the deleted files.
pub fn prune(folder: &Path, keep_recent: usize) -> io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == SUFFIX))
        .collect();
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    let mut weeks = std::collections::HashSet::new();
    let mut deleted = Vec::new();
    for (i, f) in files.iter().enumerate() {
        if i < keep_recent {
            continue;
        }
        let Some(secs) = f.file_stem().and_then(|s| s.to_str()).and_then(parse_stamp) else {
            continue; // not ours: kept
        };
        if weeks.insert(iso_week((secs / 86400) as i64)) {
            continue;
        }
        std::fs::remove_file(f)?;
        deleted.push(f.clone());
    }
    Ok(deleted)
}

/// All snapshots under `base` (`<base>/*/*.sks`), newest first.
pub fn list_snapshots(base: &Path) -> Vec<(PathBuf, SnapshotHeader)> {
    let mut out = Vec::new();
    let Ok(folders) = std::fs::read_dir(base) else {
        return out;
    };
    for folder in folders.flatten() {
        let Ok(files) = std::fs::read_dir(folder.path()) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().is_some_and(|e| e == SUFFIX)
                && let Ok(h) = read_header(&p)
            {
                out.push((p, h));
            }
        }
    }
    out.sort_by(|a, b| {
        b.1.created_at
            .cmp(&a.1.created_at)
            .then_with(|| b.0.cmp(&a.0))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_roundtrip() {
        for secs in [0u64, 86399, 951782400, 1_767_225_600, 4_102_444_800] {
            assert_eq!(parse_stamp(&stamp(secs)), Some(secs));
        }
        assert_eq!(stamp(1_767_225_600), "20260101-000000");
        assert_eq!(parse_stamp("20260101-000000_001"), Some(1_767_225_600));
        assert_eq!(parse_stamp("20260101-000000_"), None);
        assert_eq!(parse_stamp("20260101-000000_x1"), None);
        // 2026-01-01 is a Thursday: ISO week 1 of 2026; 2027-01-01 is a Friday: week 53 of 2026
        assert_eq!(iso_week(days_from_civil(2026, 1, 1)), (2026, 1));
        assert_eq!(iso_week(days_from_civil(2027, 1, 1)), (2026, 53));
        assert_eq!(iso_week(days_from_civil(2024, 12, 30)), (2025, 1));
    }

    #[test]
    fn root_keys() {
        let k = root_key(&["C:\\".to_string()]);
        assert!(k.starts_with("C-"), "{k}");
        let tail = |k: String| k.rsplit('-').next().unwrap().to_string();
        assert_eq!(
            tail(root_key(&["/a".into(), "/b".into()])),
            tail(root_key(&["/b".into(), "/a".into()]))
        );
        assert!(root_key(&["/".into()]).starts_with("root-"));
    }
}
