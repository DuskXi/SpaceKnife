//! The helper's side (Python `helper/service.py` and `helper/__main__.py`): a whitelist of
//! read-only operations behind the authenticated channel.
//!
//! Requests are handled one at a time on the main thread. While a long operation runs, a small
//! thread sends `progress` heartbeats (frames are written under one lock, so they interleave
//! cleanly with the bulk frames). Bulk frames are encoded on the main thread while the previous
//! one is written by a writer thread. Nothing here opens anything for writing.

use std::ffi::OsString;
use std::io;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;
use sk_native::mft::{MftTable, VolumeInfo as MftGeometry};
use sk_protocol::binary::{Column, FrameBuilder};
use sk_protocol::envelope::RequestId;

use super::auth::{is_elevated, read_token_file};
use super::messages::*;
use super::transport::{ChanError, Channel, connect};
use super::usn::{self, ReadOptions, UsnDevice, UsnError, UsnRows};
use crate::i18n::{self, t};
use crate::tr;

pub const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
pub const IO_TIMEOUT: Duration = Duration::from_secs(120);
pub const HEARTBEAT: Duration = Duration::from_secs(1);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Name bytes per bulk frame, so frames stay far below the size limit.
pub const NAMES_BUDGET: usize = 256 << 20;
#[cfg(windows)]
const LOCAL_DRIVES: [u32; 3] = [2, 3, 6]; // removable, fixed, RAM disk

/// An operation failed with one of the helper's error codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpError {
    pub code: &'static str,
    pub message: String,
}

impl OpError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<UsnError> for OpError {
    fn from(e: UsnError) -> Self {
        OpError::new(e.code, e.message)
    }
}

/// Writing to the backend failed: the helper stops.
#[derive(Debug)]
pub struct ChannelClosed(pub String);

// --------------------------------------------------------------------------- seams

/// `scan_mft_image(path, duty, progress)` → (table, geometry scalars).
pub type ScanImageFn = Box<
    dyn Fn(&str, f64, &mut dyn FnMut(u64, u64)) -> io::Result<(MftTable, Scalars)> + Send + Sync,
>;
pub type OpenUsnFn = Box<dyn Fn(&str) -> Result<Box<dyn UsnDevice>, UsnError> + Send + Sync>;
pub type CheckNtfsFn = Box<dyn Fn(&str) -> Result<Scalars, OpError> + Send + Sync>;

/// Replacements for the parts of the service that touch the system (tests only): the elevation
/// the helper claims, the image scanner, the journal, the NTFS check.
#[doc(hidden)]
#[derive(Default)]
pub struct Hooks {
    pub elevated: Option<bool>,
    pub scan_image: Option<ScanImageFn>,
    pub open_usn: Option<OpenUsnFn>,
    pub check_ntfs: Option<CheckNtfsFn>,
}

/// The geometry of an MFT read as scalars.
pub fn geometry_scalars(g: &MftGeometry) -> Scalars {
    let mut m = Scalars::new();
    m.insert("bytes_per_sector".into(), g.bytes_per_sector.into());
    m.insert("cluster_size".into(), g.cluster_size.into());
    m.insert("record_size".into(), g.record_size.into());
    m.insert("mft_lcn".into(), g.mft_lcn.into());
    m.insert("mft_size".into(), g.mft_size.into());
    m.insert("mft_valid_size".into(), g.mft_valid_size.into());
    m.insert("mft_fragments".into(), g.mft_fragments.into());
    m.insert("volume_size".into(), g.volume_size.into());
    m
}

fn real_scan_image(
    path: &str,
    duty: f64,
    progress: &mut dyn FnMut(u64, u64),
) -> io::Result<(MftTable, Scalars)> {
    let opts = sk_native::mft::ScanOptions {
        duty,
        progress: Some(progress),
        ..Default::default()
    };
    let (table, geo) = sk_native::mft::scan_mft_image(path, opts)?;
    Ok((table, geometry_scalars(&geo)))
}

fn real_scan_volume(
    volume: &str,
    duty: f64,
    progress: &mut dyn FnMut(u64, u64),
) -> io::Result<(MftTable, Scalars)> {
    let opts = sk_native::mft::ScanOptions {
        duty,
        progress: Some(progress),
        ..Default::default()
    };
    let (table, geo) = sk_native::mft::scan_mft(volume, opts)?;
    Ok((table, geometry_scalars(&geo)))
}

/// Facts about a local NTFS volume `X:`; an error for anything else.
pub fn check_ntfs(volume: &str) -> Result<Scalars, OpError> {
    #[cfg(windows)]
    {
        let d = super::win32::volume_details(volume);
        let upper = volume.to_uppercase();
        if !LOCAL_DRIVES.contains(&d.drive_type) {
            return Err(OpError::new(
                code::NOT_NTFS,
                tr!("{volume} 不是本地卷", volume = &upper),
            ));
        }
        if d.device.starts_with("\\??\\") {
            return Err(OpError::new(
                code::NOT_NTFS,
                tr!("{volume} 是 SUBST 映射的文件夹，不是卷", volume = &upper),
            ));
        }
        if d.fs_type != "NTFS" {
            let fs = if d.fs_type.is_empty() {
                t("无法识别").to_owned()
            } else {
                d.fs_type.clone()
            };
            return Err(OpError::new(
                code::NOT_NTFS,
                tr!("{volume} 不是 NTFS 卷（{fs}）", volume = &upper, fs = fs),
            ));
        }
        Ok(d.scalars())
    }
    #[cfg(not(windows))]
    {
        let _ = volume;
        Err(OpError::new(code::NOT_SUPPORTED, t("卷信息只支持 Windows")))
    }
}

/// The error a failed `$MFT` read is reported with.
fn mft_failure(e: io::Error) -> OpError {
    match e.kind() {
        io::ErrorKind::PermissionDenied => OpError::new(
            code::NEEDS_ADMIN,
            tr!("需要管理员权限：{reason}", reason = e.to_string()),
        ),
        io::ErrorKind::Unsupported => OpError::new(
            code::NOT_SUPPORTED,
            tr!("MFT 读取不可用：{reason}", reason = e.to_string()),
        ),
        _ => OpError::new(
            code::IO_ERROR,
            tr!("读取 $MFT 失败：{reason}", reason = e.to_string()),
        ),
    }
}

/// The platform name `ping` reports (Python's `sys.platform`).
pub fn platform_name() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

// --------------------------------------------------------------------------- bulk tables

/// A columnar table sent as bulk frames: named fixed-width columns plus a `names` blob addressed
/// by `name_off` / `name_len`.
pub trait BulkTable: Default {
    const STREAM: &'static str;
    const LAYOUT: &'static str;
    /// The column names, in wire order (`name_off` / `name_len` among them).
    const COLUMNS: &'static [&'static str];

    fn rows(&self) -> usize;
    fn name_off(&self) -> &[u32];
    fn name_len(&self) -> &[u16];
    fn names(&self) -> &[u8];
    /// Add the columns of rows `r` to a frame, `name_off` replaced by `rebased`.
    fn push_columns<'a>(&'a self, b: &mut FrameBuilder<'a>, r: Range<usize>, rebased: &'a [u32]);
    /// Room for `rows` rows.
    fn reserve(&mut self, rows: usize);
    /// Append one decoded frame (its names blob already appended at `base`); the rows added.
    fn add_frame(
        &mut self,
        f: &sk_protocol::binary::DecodedFrame<'_>,
        base: u32,
    ) -> Result<usize, String>;
    fn names_mut(&mut self) -> &mut Vec<u8>;
}

fn extend_column<T: sk_protocol::binary::Element>(
    out: &mut Vec<T>,
    f: &sk_protocol::binary::DecodedFrame<'_>,
    name: &str,
    rows: usize,
) -> Result<(), String> {
    let view = f
        .column(name)
        .ok_or_else(|| tr!("缺少列 {name}", name = name))?;
    if view.dtype != T::DTYPE || view.count != rows {
        return Err(tr!("列 {key} 的长度或类型不一致", key = name));
    }
    out.extend(
        view.bytes
            .chunks_exact(view.dtype.size())
            .map(T::from_le_slice),
    );
    Ok(())
}

macro_rules! bulk_table {
    ($ty:ty, $stream:expr, $layout:expr, { $($field:ident : $t:ty),* $(,)? }) => {
        impl BulkTable for $ty {
            const STREAM: &'static str = $stream;
            const LAYOUT: &'static str = $layout;
            const COLUMNS: &'static [&'static str] = &[$(stringify!($field)),*];

            fn rows(&self) -> usize {
                self.name_len.len()
            }
            fn name_off(&self) -> &[u32] {
                &self.name_off
            }
            fn name_len(&self) -> &[u16] {
                &self.name_len
            }
            fn names(&self) -> &[u8] {
                &self.names
            }
            fn names_mut(&mut self) -> &mut Vec<u8> {
                &mut self.names
            }
            fn push_columns<'a>(&'a self, b: &mut FrameBuilder<'a>, r: Range<usize>, rebased: &'a [u32]) {
                $(
                    if stringify!($field) == "name_off" {
                        b.push_column("name_off", Column::from(rebased));
                    } else {
                        b.push_column(stringify!($field), Column::from(&self.$field[r.clone()]));
                    }
                )*
            }
            fn reserve(&mut self, rows: usize) {
                $( self.$field.reserve_exact(rows); )*
            }
            fn add_frame(&mut self, f: &sk_protocol::binary::DecodedFrame<'_>, base: u32) -> Result<usize, String> {
                if f.columns.len() != Self::COLUMNS.len() {
                    return Err(t("各批数据的列不一致").to_owned());
                }
                let rows = f.header.count as usize;
                let start = self.name_len.len();
                $( extend_column::<$t>(&mut self.$field, f, stringify!($field), rows)?; )*
                for off in &mut self.name_off[start..] {
                    *off = off.checked_add(base).ok_or_else(|| t("名字总长超过 4 GiB").to_owned())?;
                }
                Ok(rows)
            }
        }
    };
}

bulk_table!(MftTable, MFT_STREAM, MFT_LAYOUT, {
    record: u64, parent: u64, name_off: u32, name_len: u16, kind: u8, flags: u32, size: i64, alloc: i64,
    ads_size: i64, ads_alloc: i64, mtime: i64, ctime: i64, atime: i64, nlink: u16,
});

bulk_table!(UsnRows, USN_STREAM, USN_LAYOUT, {
    frn: u64, parent_frn: u64, usn: i64, reason: u32, attrs: u32, name_off: u32, name_len: u16,
});

/// Row ranges of at most `batch_rows` rows and (unless one name is larger) `names_budget` bytes.
pub fn batch_bounds(name_len: &[u16], batch_rows: usize, names_budget: usize) -> Vec<Range<usize>> {
    let n = name_len.len();
    if n == 0 {
        return std::iter::once(0..0).collect(); // one empty frame still carries the column dtypes
    }
    let batch_rows = batch_rows.max(1);
    let mut out = Vec::new();
    let mut a = 0;
    while a < n {
        let mut bytes = 0usize;
        let mut b = a;
        while b < n && b - a < batch_rows {
            let next = bytes + name_len[b] as usize;
            if next > names_budget && b > a {
                break;
            }
            bytes = next;
            b += 1;
            if bytes > names_budget {
                break; // one oversized name: a frame of its own
            }
        }
        out.push(a..b);
        a = b;
    }
    out
}

/// The names of one batch as their own blob, plus offsets into it.
///
/// A contiguous (or nearly so) range is sliced; scattered names are gathered in row order.
pub fn rebase_names<'a>(
    names: &'a [u8],
    off: &[u32],
    len: &[u16],
) -> Result<(std::borrow::Cow<'a, [u8]>, Vec<u32>), String> {
    if off.is_empty() {
        return Ok((std::borrow::Cow::Borrowed(&[]), Vec::new()));
    }
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    let mut total = 0usize;
    for (&o, &l) in off.iter().zip(len) {
        lo = lo.min(o as usize);
        hi = hi.max(o as usize + l as usize);
        total += l as usize;
    }
    if hi > names.len() {
        return Err(t("MFT 表的名字偏移越界").to_owned());
    }
    if hi - lo <= 2 * total + 4096 {
        let rebased = off.iter().map(|&o| o - lo as u32).collect();
        return Ok((std::borrow::Cow::Borrowed(&names[lo..hi]), rebased));
    }
    let mut blob = Vec::with_capacity(total);
    let mut rebased = Vec::with_capacity(off.len());
    for (&o, &l) in off.iter().zip(len) {
        rebased.push(blob.len() as u32);
        blob.extend_from_slice(&names[o as usize..o as usize + l as usize]);
    }
    Ok((std::borrow::Cow::Owned(blob), rebased))
}

/// One bulk frame of rows `r` of `table` (the names rebased into the frame's own blob).
pub fn encode_batch<T: BulkTable>(
    table: &T,
    r: Range<usize>,
    seq: u64,
    is_final: bool,
    request_id: &RequestId,
    out: &mut Vec<u8>,
) -> Result<(), String> {
    let (blob, rebased) = rebase_names(
        table.names(),
        &table.name_off()[r.clone()],
        &table.name_len()[r.clone()],
    )?;
    let mut b = FrameBuilder::new(T::STREAM, T::LAYOUT)
        .seq(seq)
        .is_final(is_final)
        .request_id(Some(request_id));
    table.push_columns(&mut b, r, &rebased);
    b.push_blob("names", &blob);
    b.finish_into(out).map_err(|e| e.0)
}

// --------------------------------------------------------------------------- the service

pub struct Service<'c> {
    chan: &'c Channel,
    elevated: bool,
    io_timeout: Duration,
    write_lock: Mutex<()>,
    hooks: Hooks,
}

struct Beat {
    phase: AtomicU8, // 0 scan, 1 send
    done: AtomicU64,
    total: AtomicU64,
}

impl Beat {
    fn reading(&self, done: u64, total: u64) {
        self.done.store(done, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }
}

type OpResult = Result<(Scalars, Scalars), OpError>;

impl<'c> Service<'c> {
    pub fn new(chan: &'c Channel, elevated: bool, io_timeout: Duration, hooks: Hooks) -> Self {
        Self {
            chan,
            elevated,
            io_timeout,
            write_lock: Mutex::new(()),
            hooks,
        }
    }

    // ------------------------------------------------------------------ sending

    fn send_frame(&self, buffers: &[&[u8]]) -> Result<(), ChannelClosed> {
        let _guard = self.write_lock.lock();
        self.chan
            .send(buffers, Some(self.io_timeout))
            .map_err(|e| ChannelClosed(e.to_string()))
    }

    pub fn send(&self, reply: &Reply) -> Result<(), ChannelClosed> {
        let (head, body) = json_frame(reply);
        self.send_frame(&[&head, &body])
    }

    fn error(&self, id: Option<u64>, code: &str, message: &str) -> Result<(), ChannelClosed> {
        self.send(&Reply::error(id, code, message))
    }

    // ------------------------------------------------------------------ loop

    /// Handle requests until shutdown, EOF, idle timeout or a broken stream.
    pub fn serve(&self, idle_timeout: Duration) {
        if let Err(e) = self.serve_inner(idle_timeout) {
            tracing::debug!("connection closed: {}", e.0);
        }
    }

    fn serve_inner(&self, idle_timeout: Duration) -> Result<(), ChannelClosed> {
        loop {
            let (kind, body) = match self.chan.read_frame(
                Some(idle_timeout),
                Some(self.io_timeout),
                MAX_CONTROL_BYTES,
            ) {
                Ok(frame) => frame,
                Err(ChanError::Timeout) => {
                    tracing::info!("idle for {:.0} s, exiting", idle_timeout.as_secs_f64());
                    return Ok(());
                }
                Err(ChanError::Eof) => {
                    tracing::debug!("the backend disconnected");
                    return Ok(());
                }
                Err(ChanError::Frame(e)) => {
                    tracing::warn!("{}", tr!("收到无效帧：{reason}", reason = &e.0));
                    self.error(None, code::BAD_FRAME, &e.0)?;
                    return Ok(());
                }
                Err(e) => {
                    tracing::debug!("reading a request failed: {e}");
                    return Ok(());
                }
            };
            if kind != KIND_JSON {
                self.error(None, code::INVALID_REQUEST, t("助手只接受 JSON 请求帧"))?;
                continue;
            }
            let request = match Request::parse(&body) {
                Ok(r) => r,
                Err(why) => {
                    let message = tr!("无效请求：{details}", details = why);
                    self.error(guess_id(&body), code::INVALID_REQUEST, &message)?;
                    continue;
                }
            };
            if !self.dispatch(request)? {
                return Ok(());
            }
        }
    }

    fn dispatch(&self, request: Request) -> Result<bool, ChannelClosed> {
        let id = request.id;
        tracing::debug!("request {id} {}", request.call.op());
        let outcome: OpResult = match &request.call {
            Call::Shutdown {} => {
                self.send(&Reply::response(id, Scalars::new(), Scalars::new()))?;
                return Ok(false);
            }
            Call::Ping {} => {
                let mut m = Scalars::new();
                m.insert("pid".into(), std::process::id().into());
                m.insert("elevated".into(), self.elevated.into());
                m.insert("version".into(), PROTOCOL_VERSION.into());
                m.insert("platform".into(), platform_name().into());
                Ok((m, Scalars::new()))
            }
            Call::VolumeInfo { volume } => self.volume_info(volume).map(|r| (r, Scalars::new())),
            Call::QueryUsn { volume } => self.query_usn(volume).map(|r| (r, Scalars::new())),
            Call::ReadUsn {
                volume,
                start_usn,
                journal_id,
                max_rows,
                duty,
            } => self.read_usn(
                id,
                volume,
                *start_usn,
                *journal_id,
                *max_rows as usize,
                *duty,
            )?,
            Call::ScanMft {
                volume,
                batch_rows,
                duty,
            } => self.scan_mft(id, volume, *batch_rows as usize, *duty)?,
            Call::ScanMftImage {
                path,
                batch_rows,
                duty,
            } => self.scan_mft_image(id, path, *batch_rows as usize, *duty)?,
            Call::DiskHealth { disk } => self.disk_health(disk).map(|r| (r, Scalars::new())),
        };
        match outcome {
            Ok((result, extras)) => self.send(&Reply::response(id, result, extras))?,
            Err(e) => self.error(Some(id), e.code, &e.message)?,
        }
        Ok(true)
    }

    // ------------------------------------------------------------------ operations

    fn check_ntfs(&self, volume: &str) -> Result<Scalars, OpError> {
        match &self.hooks.check_ntfs {
            Some(f) => f(volume),
            None => check_ntfs(volume),
        }
    }

    fn volume_info(&self, volume: &str) -> Result<Scalars, OpError> {
        let details = self.check_ntfs(volume)?;
        let mut result = Scalars::new();
        result.insert("volume".into(), volume.to_uppercase().into());
        result.extend(details);
        result.insert("elevated".into(), self.elevated.into());
        #[cfg(windows)]
        if self.elevated {
            let data = super::win32::ntfs_volume_data(volume).map_err(|e| {
                OpError::new(
                    code::IO_ERROR,
                    tr!("读取 NTFS 卷参数失败：{reason}", reason = e.to_string()),
                )
            })?;
            result.extend(data);
        }
        Ok(result)
    }

    fn scan_mft(
        &self,
        id: u64,
        volume: &str,
        batch_rows: usize,
        duty: f64,
    ) -> Result<OpResult, ChannelClosed> {
        if !self.elevated {
            return Ok(Err(OpError::new(code::NEEDS_ADMIN, t("需要管理员权限"))));
        }
        if !cfg!(windows) {
            return Ok(Err(OpError::new(
                code::NOT_SUPPORTED,
                t("MFT 快速扫描只支持 Windows NTFS 卷"),
            )));
        }
        if let Err(e) = self.check_ntfs(volume) {
            return Ok(Err(e));
        }
        let volume = volume.to_uppercase();
        self.scan(
            id,
            batch_rows,
            &|duty, progress| real_scan_volume(&volume, duty, progress),
            duty,
        )
    }

    fn scan_mft_image(
        &self,
        id: u64,
        path: &str,
        batch_rows: usize,
        duty: f64,
    ) -> Result<OpResult, ChannelClosed> {
        if self.elevated {
            return Ok(Err(OpError::new(
                code::FORBIDDEN,
                t("scan_mft_image 只在非提权助手中可用"),
            )));
        }
        match &self.hooks.scan_image {
            Some(f) => self.scan(
                id,
                batch_rows,
                &|duty, progress| f(path, duty, progress),
                duty,
            ),
            None => self.scan(
                id,
                batch_rows,
                &|duty, progress| real_scan_image(path, duty, progress),
                duty,
            ),
        }
    }

    /// Read and parse the `$MFT` (streaming: never all of it in memory), then stream the columns.
    #[allow(clippy::type_complexity)]
    fn scan(
        &self,
        id: u64,
        batch_rows: usize,
        scanner: &(dyn Fn(f64, &mut dyn FnMut(u64, u64)) -> io::Result<(MftTable, Scalars)> + Sync),
        duty: f64,
    ) -> Result<OpResult, ChannelClosed> {
        self.with_heartbeat(id, |beat| {
            let t0 = Instant::now();
            let (table, geometry) = match scanner(duty, &mut |d, t| beat.reading(d, t)) {
                Ok(r) => r,
                Err(e) => return Ok(Err(mft_failure(e))),
            };
            let t1 = Instant::now();
            beat.phase.store(1, Ordering::Relaxed);
            let mut extras = Scalars::new();
            extras.insert("bad_records".into(), table.bad_records.into());
            let mut result = geometry;
            result.extend(self.send_table(id, &table, batch_rows)?);
            drop(table);
            result.insert("scan_seconds".into(), (t1 - t0).as_secs_f64().into());
            result.insert("send_seconds".into(), t1.elapsed().as_secs_f64().into());
            Ok(Ok((result, extras)))
        })
    }

    fn disk_health(&self, disk: &str) -> Result<Scalars, OpError> {
        if !cfg!(target_os = "linux") {
            return Err(OpError::new(
                code::NOT_SUPPORTED,
                t("助手只在 Linux 上读取磁盘自己的健康记录"),
            ));
        }
        if !self.elevated {
            return Err(OpError::new(code::NEEDS_ADMIN, t("需要管理员权限")));
        }
        #[cfg(target_os = "linux")]
        {
            use crate::disks::passthrough::{RecordError, read_record_system};
            return match read_record_system(disk) {
                Ok(pages) => Ok(pages
                    .into_iter()
                    .map(|(k, v)| (k, Value::from(v)))
                    .collect()),
                Err(RecordError::Passthrough(why)) => Err(OpError::new(code::NOT_SUPPORTED, why)),
                Err(RecordError::Invalid(why)) => Err(OpError::new(
                    code::IO_ERROR,
                    tr!("读取 {disk} 失败：{reason}", disk = disk, reason = why),
                )),
                Err(RecordError::Io(e)) => Err(OpError::new(
                    code::IO_ERROR,
                    tr!(
                        "读取 {disk} 失败：{reason}",
                        disk = disk,
                        reason = e.to_string()
                    ),
                )),
            };
        }
        #[allow(unreachable_code)]
        {
            let _ = disk;
            Err(OpError::new(
                code::NOT_SUPPORTED,
                t("助手只在 Linux 上读取磁盘自己的健康记录"),
            ))
        }
    }

    /// `X:` of a local NTFS volume whose journal the (elevated) helper may read.
    fn usn_volume(&self, volume: &str) -> Result<String, OpError> {
        if !self.elevated {
            return Err(OpError::new(code::NEEDS_ADMIN, t("需要管理员权限")));
        }
        self.check_ntfs(volume)?;
        Ok(volume.to_uppercase())
    }

    fn open_usn(&self, volume: &str) -> Result<Box<dyn UsnDevice>, UsnError> {
        match &self.hooks.open_usn {
            Some(f) => f(volume),
            None => usn::open_volume(volume),
        }
    }

    fn query_usn(&self, volume: &str) -> Result<Scalars, OpError> {
        let volume = self.usn_volume(volume)?;
        let journal = self.open_usn(&volume)?.query()?;
        let mut m = Scalars::new();
        m.insert("volume".into(), volume.into());
        m.extend(journal.scalars());
        Ok(m)
    }

    /// Read the journal from the checkpoint (at most `max_rows` records), then stream the rows.
    fn read_usn(
        &self,
        id: u64,
        volume: &str,
        start_usn: i64,
        journal_id: u64,
        max_rows: usize,
        duty: f64,
    ) -> Result<OpResult, ChannelClosed> {
        let volume = match self.usn_volume(volume) {
            Ok(v) => v,
            Err(e) => return Ok(Err(e)),
        };
        self.with_heartbeat(id, |beat| {
            let t0 = Instant::now();
            let read = self.open_usn(&volume).and_then(|mut device| {
                let opts = ReadOptions {
                    max_rows,
                    duty,
                    progress: Some(&mut |d, t| beat.reading(d, t)),
                };
                usn::read_journal(device.as_mut(), start_usn, journal_id, opts)
            });
            let (table, stats) = match read {
                Ok(r) => r,
                Err(e) => return Ok(Err(e.into())),
            };
            let t1 = Instant::now();
            beat.phase.store(1, Ordering::Relaxed);
            let sent = self.send_table(id, &table.rows, DEFAULT_BATCH_ROWS as usize)?;
            let extras = table.extras();
            drop(table);
            let mut result = Scalars::new();
            result.insert("volume".into(), volume.clone().into());
            result.extend(stats.scalars());
            result.extend(sent);
            result.insert("read_seconds".into(), (t1 - t0).as_secs_f64().into());
            result.insert("send_seconds".into(), t1.elapsed().as_secs_f64().into());
            Ok(Ok((result, extras)))
        })
    }

    /// Bulk frames of `batch_rows` rows; frame k+1 is encoded while frame k is written.
    fn send_table<T: BulkTable + Sync>(
        &self,
        id: u64,
        table: &T,
        batch_rows: usize,
    ) -> Result<Scalars, ChannelClosed> {
        let rows = table.rows();
        let bounds = batch_bounds(table.name_len(), batch_rows, NAMES_BUDGET);
        let frames = bounds.len();
        self.send(&Reply::Begin {
            id,
            rows: rows as u64,
            frames: frames as u64,
        })?;
        let request_id = RequestId::from(id);
        let mut wire = 0u64;
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(0);
        let (back_tx, back_rx) = mpsc::channel::<Vec<u8>>();
        let mut failure: Option<OpError> = None;
        let written = std::thread::scope(|s| {
            let writer = std::thread::Builder::new()
                .name("sk-helper-send".into())
                .spawn_scoped(s, move || -> Result<(), ChannelClosed> {
                    for buf in rx {
                        let head = frame_header(KIND_BINARY, buf.len());
                        self.send_frame(&[&head, &buf])?;
                        let _ = back_tx.send(buf);
                    }
                    Ok(())
                })
                .expect("spawn the frame writer");
            for (seq, r) in bounds.iter().enumerate() {
                let mut buf = back_rx.try_recv().unwrap_or_default();
                if let Err(why) = encode_batch(
                    table,
                    r.clone(),
                    seq as u64,
                    seq + 1 == frames,
                    &request_id,
                    &mut buf,
                ) {
                    failure = Some(OpError::new(code::INTERNAL, why));
                    break;
                }
                wire += buf.len() as u64;
                if tx.send(buf).is_err() {
                    break; // the writer failed: its error comes from join
                }
            }
            drop(tx);
            writer
                .join()
                .unwrap_or_else(|_| Err(ChannelClosed("frame writer panicked".into())))
        });
        written?;
        if let Some(e) = failure {
            // the stream cannot be completed: the backend sees the connection end
            return Err(ChannelClosed(e.message));
        }
        let mut stats = Scalars::new();
        stats.insert("rows".into(), (rows as u64).into());
        stats.insert("frames".into(), (frames as u64).into());
        stats.insert("names_bytes".into(), (table.names().len() as u64).into());
        stats.insert("wire_bytes".into(), wire.into());
        Ok(stats)
    }

    fn with_heartbeat<R>(&self, id: u64, f: impl FnOnce(&Beat) -> R) -> R {
        let beat = Beat {
            phase: AtomicU8::new(0),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
        };
        let start = Instant::now();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        std::thread::scope(|s| {
            let beat = &beat;
            std::thread::Builder::new()
                .name("sk-helper-beat".into())
                .spawn_scoped(s, move || {
                    while let Err(mpsc::RecvTimeoutError::Timeout) = stop_rx.recv_timeout(HEARTBEAT)
                    {
                        let progress = Reply::Progress {
                            id,
                            phase: if beat.phase.load(Ordering::Relaxed) == 0 {
                                Phase::Scan
                            } else {
                                Phase::Send
                            },
                            elapsed: start.elapsed().as_secs_f64(),
                            done: beat.done.load(Ordering::Relaxed),
                            total: beat.total.load(Ordering::Relaxed),
                        };
                        if self.send(&progress).is_err() {
                            return; // the main thread notices on its next write
                        }
                    }
                })
                .expect("spawn the heartbeat");
            let r = f(beat);
            drop(stop_tx);
            r
        })
    }
}

fn guess_id(body: &[u8]) -> Option<u64> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let id = v.get("id")?.as_u64()?;
    (1..=MAX_ID).contains(&id).then_some(id)
}

// --------------------------------------------------------------------------- entry point

const USAGE: &str = "usage: spaceknife-cli helper --pipe ADDRESS --token-file PATH --server-pid PID [--idle-timeout SECONDS] [--log-level LEVEL] [--system-locale TAG]";

struct Args {
    pipe: String,
    token_file: PathBuf,
    server_pid: u32,
    idle_timeout: f64,
    log_level: String,
    /// the OS locale the backend's launcher read (the language of the helper's messages)
    system_locale: Option<String>,
}

/// A locale tag as passed with `--system-locale` (`zh_CN.UTF-8`, `en-US`, `zh-Hans@x`).
pub fn is_locale_tag(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.@".contains(&b))
}

fn parse_args(args: &[OsString]) -> Result<Args, String> {
    let mut pipe = None;
    let mut token_file = None;
    let mut server_pid = None;
    let mut idle_timeout = IDLE_TIMEOUT.as_secs_f64();
    let mut log_level = "INFO".to_owned();
    let mut system_locale = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let flag = flag.to_string_lossy();
        let (name, inline) = match flag.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_owned(), Some(OsString::from(v))),
            _ => (flag.to_string(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{name}: expected a value"))
        };
        match name.as_str() {
            "--pipe" => pipe = Some(value()?.to_string_lossy().into_owned()),
            "--token-file" => token_file = Some(PathBuf::from(value()?)),
            "--server-pid" => {
                server_pid = Some(
                    value()?
                        .to_string_lossy()
                        .parse::<u32>()
                        .map_err(|e| format!("--server-pid: {e}"))?,
                )
            }
            "--idle-timeout" => {
                idle_timeout = value()?
                    .to_string_lossy()
                    .parse::<f64>()
                    .map_err(|e| format!("--idle-timeout: {e}"))?
            }
            "--log-level" => {
                let v = value()?.to_string_lossy().to_uppercase();
                if !["TRACE", "DEBUG", "INFO", "WARNING", "ERROR"].contains(&v.as_str()) {
                    return Err(format!("--log-level: invalid choice {v:?}"));
                }
                log_level = v;
            }
            "--system-locale" => {
                let v = value()?.to_string_lossy().into_owned();
                if !is_locale_tag(&v) {
                    return Err(format!("--system-locale: invalid tag {v:?}"));
                }
                system_locale = Some(v);
            }
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }
    Ok(Args {
        pipe: pipe.ok_or("--pipe is required")?,
        token_file: token_file.ok_or("--token-file is required")?,
        server_pid: server_pid.ok_or("--server-pid is required")?,
        idle_timeout,
        log_level,
        system_locale,
    })
}

fn setup_logging(level: &str) {
    let level = match level {
        "TRACE" => tracing::Level::TRACE,
        "DEBUG" => tracing::Level::DEBUG,
        "WARNING" => tracing::Level::WARN,
        "ERROR" => tracing::Level::ERROR,
        _ => tracing::Level::INFO,
    };
    // the app may have installed a subscriber already (then that one is used)
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .with_ansi(false)
        .with_target(false)
        .try_init();
}

/// The `helper` subcommand: the read-only scan helper, started by the backend only. `args` are the
/// arguments after the subcommand. Exit codes: 0 normal (shutdown, backend gone, idle), 2 bad
/// arguments or token file, 3 cannot reach or trust the backend.
pub fn run_main(args: &[OsString]) -> i32 {
    run_main_with(args, Hooks::default())
}

/// [`run_main`] with test seams.
#[doc(hidden)]
pub fn run_main_with(args: &[OsString], hooks: Hooks) -> i32 {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(why) => {
            eprintln!("{USAGE}\nhelper: error: {why}");
            return 2;
        }
    };
    // Its own process, often elevated: the user's settings may not be readable, so the system's
    // language (as the backend's launcher saw it, when it says); the backend shows the helper's
    // error messages as they come.
    i18n::set_system_locale(args.system_locale.as_deref());
    i18n::set_language(i18n::system_language());
    setup_logging(&args.log_level);
    let token = match read_token_file(&args.token_file) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(
                "{}",
                tr!("无法读取令牌文件：{reason}", reason = e.to_string())
            );
            return 2;
        }
    };
    if !(args.idle_timeout > 0.0 && args.idle_timeout <= 24.0 * 3600.0) {
        tracing::error!("{}", t("--idle-timeout 超出范围"));
        return 2;
    }
    let chan = match connect(&args.pipe, CONNECT_TIMEOUT) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("{}", tr!("无法连接后端：{reason}", reason = e.to_string()));
            return 3;
        }
    };
    let code = (|| {
        if chan.peer_pid() != args.server_pid {
            tracing::error!(
                "{}",
                tr!(
                    "连接的另一端是进程 {peer}，不是后端 {server}；不发送令牌",
                    peer = chan.peer_pid(),
                    server = args.server_pid
                )
            );
            return 3;
        }
        let elevated = hooks.elevated.unwrap_or_else(is_elevated);
        let service = Service::new(&chan, elevated, IO_TIMEOUT, hooks);
        let hello = Reply::Hello {
            version: PROTOCOL_VERSION,
            token,
            pid: std::process::id(),
            elevated,
        };
        if let Err(e) = service.send(&hello) {
            tracing::error!("{}", tr!("发送 hello 失败：{reason}", reason = e.0));
            return 3;
        }
        tracing::debug!(
            "connected to backend {} (elevated: {elevated})",
            args.server_pid
        );
        service.serve(Duration::from_secs_f64(args.idle_timeout));
        0
    })();
    chan.close();
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use sk_protocol::binary::decode_frame;

    pub(crate) fn synth(rows: usize, shuffle: bool) -> MftTable {
        let mut t = MftTable::default();
        let mut off = 0u32;
        for i in 0..rows {
            let len = (3 + i % 13) as u16;
            t.record.push(i as u64 + 16);
            t.parent.push((i / 5) as u64 + 5);
            t.name_off.push(off);
            t.name_len.push(len);
            off += len as u32;
            t.kind.push((i % 4 == 0) as u8);
            t.flags.push(((i * 7) & 0xFFFF) as u32);
            t.size.push(i as i64 * 1000);
            t.alloc.push(i as i64 * 1024);
            t.ads_size.push((i % 3) as i64);
            t.ads_alloc.push((i % 3) as i64 * 8);
            t.mtime.push(1_700_000_000 + i as i64);
            t.ctime.push(1_600_000_000 + i as i64);
            t.atime.push(1_750_000_000 - i as i64);
            t.nlink.push((1 + i % 2) as u16);
        }
        t.names = (0..off as usize).map(|i| b'a' + (i % 26) as u8).collect();
        t.bad_records = 7;
        if shuffle {
            // a fixed permutation (rows out of name order: exercises the gather path)
            let mut perm: Vec<usize> = (0..rows).collect();
            let mut x = 0x9E37_79B9_7F4A_7C15u64;
            for i in (1..rows).rev() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                perm.swap(i, (x % (i as u64 + 1)) as usize);
            }
            macro_rules! permute {
                ($($f:ident),*) => { $( t.$f = perm.iter().map(|&p| t.$f[p]).collect(); )* };
            }
            permute!(
                record, parent, name_off, name_len, kind, flags, size, alloc, ads_size, ads_alloc,
                mtime, ctime, atime, nlink
            );
        }
        t
    }

    pub(crate) fn gather(names: &[u8], off: &[u32], len: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        for (&o, &l) in off.iter().zip(len) {
            out.extend_from_slice(&names[o as usize..o as usize + l as usize]);
        }
        out
    }

    #[test]
    fn batches_split_by_rows_and_name_bytes() {
        let lengths = [100u16; 10];
        assert_eq!(batch_bounds(&lengths, 4, 10_000), vec![0..4, 4..8, 8..10]);
        assert_eq!(
            batch_bounds(&lengths, 100, 250),
            vec![0..2, 2..4, 4..6, 6..8, 8..10]
        );
        assert_eq!(
            batch_bounds(&lengths, 100, 50),
            (0..10).map(|i| i..i + 1).collect::<Vec<_>>()
        ); // one oversized name each
        assert_eq!(batch_bounds(&[], 5, 10), vec![0..0]);
    }

    #[test]
    fn rebase_names_slices_or_gathers() {
        let table = synth(5000, true);
        for (a, b) in [(0, 5000), (100, 1100), (4999, 5000), (7, 7)] {
            let (off, len) = (&table.name_off[a..b], &table.name_len[a..b]);
            let (blob, new_off) = rebase_names(&table.names, off, len).unwrap();
            assert_eq!(gather(&blob, &new_off, len), gather(&table.names, off, len));
            if b - a > 10 {
                assert_eq!(blob.len(), len.iter().map(|&l| l as usize).sum::<usize>()); // gathered, not over-sliced
            }
        }
        let ordered = synth(1000, false);
        let (blob, new_off) = rebase_names(
            &ordered.names,
            &ordered.name_off[10..20],
            &ordered.name_len[10..20],
        )
        .unwrap();
        assert_eq!(new_off[0], 0);
        assert_eq!(
            blob.len(),
            ordered.name_len[10..20]
                .iter()
                .map(|&l| l as usize)
                .sum::<usize>()
        );
        assert!(rebase_names(b"abc", &[2], &[5]).is_err());
    }

    #[test]
    fn frames_roundtrip_through_the_assembler() {
        let table = synth(3000, true);
        let mut joined = MftTable::default();
        joined.reserve(3000);
        let rid = RequestId::from(9u64);
        let mut buf = Vec::new();
        for (seq, r) in [0..1000, 1000..2500, 2500..3000].into_iter().enumerate() {
            encode_batch(&table, r, seq as u64, seq == 2, &rid, &mut buf).unwrap();
            let f = decode_frame(&buf).unwrap();
            assert_eq!(f.header.stream, "mft");
            assert_eq!(f.header.layout, "mft.v1");
            assert_eq!(f.header.request_id, Some(rid.clone()));
            let blob = f.blob("names").unwrap();
            let base = joined.names.len() as u32;
            joined.names.extend_from_slice(blob);
            joined.add_frame(&f, base).unwrap();
        }
        assert_eq!(joined.record, table.record);
        assert_eq!(joined.nlink, table.nlink);
        assert_eq!(
            gather(&joined.names, &joined.name_off, &joined.name_len),
            gather(&table.names, &table.name_off, &table.name_len)
        );
        // an empty table still makes one frame with every column
        let empty = MftTable::default();
        encode_batch(&empty, 0..0, 0, true, &rid, &mut buf).unwrap();
        let f = decode_frame(&buf).unwrap();
        assert_eq!(f.columns.len(), MftTable::COLUMNS.len());
        assert_eq!(f.header.count, 0);
    }

    #[test]
    fn helper_arguments() {
        let ok = parse_args(
            &[
                "--pipe",
                "p",
                "--token-file",
                "/t",
                "--server-pid",
                "12",
                "--idle-timeout=0.5",
            ]
            .map(OsString::from),
        )
        .unwrap();
        assert_eq!(
            (ok.pipe.as_str(), ok.server_pid, ok.idle_timeout),
            ("p", 12, 0.5)
        );
        assert_eq!(ok.system_locale, None);
        let base = ["--pipe", "p", "--token-file", "/t", "--server-pid", "12"];
        let with = |extra: &[&str]| {
            let all: Vec<OsString> = base.iter().chain(extra).map(OsString::from).collect();
            parse_args(&all)
        };
        assert_eq!(
            with(&["--system-locale", "zh_CN.UTF-8"])
                .unwrap()
                .system_locale
                .as_deref(),
            Some("zh_CN.UTF-8")
        );
        assert!(with(&["--system-locale", "en US"]).is_err());
        assert!(with(&["--system-locale", ""]).is_err());
        assert!(with(&["--system-locale"]).is_err());
        assert!(parse_args(&["--pipe", "p"].map(OsString::from)).is_err());
        assert!(parse_args(&["--bogus", "1"].map(OsString::from)).is_err());
        assert!(
            parse_args(
                &["--pipe", "p", "--token-file", "/t", "--server-pid", "x"].map(OsString::from)
            )
            .is_err()
        );
    }

    #[test]
    fn guessing_request_ids() {
        assert_eq!(guess_id(br#"{"id":7,"call":{}}"#), Some(7));
        assert_eq!(guess_id(br#"{"id":"7"}"#), None);
        assert_eq!(guess_id(b"not json"), None);
        assert_eq!(guess_id(br#"{"id":0}"#), None);
    }
}
