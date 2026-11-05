//! Backend side (Python `helper/client.py`): launch the helper, authenticate it, and talk to it.
//!
//! ```no_run
//! # use sk_core::helper::{HelperClient, LaunchOptions, MftOptions};
//! let helper = HelperClient::launch(LaunchOptions::default())?; // UAC prompt unless already admin
//! let table = helper.scan_mft("C:", MftOptions::default(), None)?; // the same MftTable as a local parse
//! let journal = helper.query_usn("C:")?; // checkpoint: journal_id, next_usn
//! helper.close();
//! # Ok::<(), sk_core::helper::HelperError>(())
//! ```
//!
//! Blocking: run it on a worker thread (`spawn_blocking`). One request is in flight at a time (a
//! lock serialises callers). [`HelperClient::abort`] (any thread) tears the connection down and
//! makes a running call fail at once. Use [`SharedHelper`] to keep one helper per backend and reuse
//! it across scans.

use std::ffi::OsString;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;
use sk_native::mft::MftTable;
use sk_protocol::binary::{DecodedFrame, decode_frame};
use sk_protocol::envelope::RequestId;

use super::auth::{is_elevated, new_token, peer_is_launched, tokens_match, write_token_file};
use super::errors::HelperError;
use super::launch::{Elevator, HelperProcess, helper_command, start_elevated, start_subprocess};
use super::messages::*;
use super::service::{BulkTable, IDLE_TIMEOUT};
use super::transport::{ChanError, Channel, Listener};
use super::usn::{UsnRows, UsnTable};
use crate::i18n::t;
use crate::tr;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Includes the time the UAC prompt is open.
pub const CONNECT_TIMEOUT_UAC: Duration = Duration::from_secs(60);
/// Includes typing the password in polkit's dialog.
pub const CONNECT_TIMEOUT_POLKIT: Duration = Duration::from_secs(180);
/// Inactivity; a busy helper sends a heartbeat every second.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const ACCEPT_SLICE: Duration = Duration::from_millis(250);

/// `(phase, seconds since the request, share of the $MFT / journal read or None)`.
pub type ProgressFn<'a> = &'a mut dyn FnMut(Phase, f64, Option<f64>);

/// How [`HelperClient::launch`] starts the helper.
#[derive(Clone)]
pub struct LaunchOptions {
    /// Elevated: the UAC prompt (Windows) or polkit's password dialog (Linux), both skipped when
    /// this process already has administrator rights. `false`: a plain subprocess.
    pub elevate: bool,
    /// How long to wait for the helper to connect (default by mode: 20 s, 60 s UAC, 180 s polkit).
    pub connect_timeout: Option<Duration>,
    pub request_timeout: Duration,
    /// The helper exits after this long without a request.
    pub idle_timeout: Duration,
    /// Replaces `[current_exe, "helper"]` (tests).
    pub command: Option<Vec<OsString>>,
    /// How an elevated helper is started (tests replace the UAC call).
    pub elevator: Elevator,
    /// Pretend this process's elevation (tests).
    #[doc(hidden)]
    pub assume_elevated: Option<bool>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            elevate: true,
            connect_timeout: None,
            request_timeout: REQUEST_TIMEOUT,
            idle_timeout: IDLE_TIMEOUT,
            command: None,
            elevator: Elevator::default(),
            assume_elevated: None,
        }
    }
}

impl LaunchOptions {
    pub fn unelevated() -> Self {
        Self {
            elevate: false,
            ..Self::default()
        }
    }
}

/// `batch_rows` / `duty` of an MFT scan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MftOptions {
    /// Rows per bulk frame.
    pub batch_rows: u64,
    /// In (0, 1]: the `$MFT` reader is busy that share of the time (the scan's I/O limit).
    pub duty: f64,
}

impl Default for MftOptions {
    fn default() -> Self {
        Self {
            batch_rows: DEFAULT_BATCH_ROWS,
            duty: 1.0,
        }
    }
}

#[derive(Default)]
struct State {
    next_id: u64,
    broken: Option<String>,
    closed: bool,
}

/// What the bulk part of an exchange delivers to its consumer.
enum Bulk<'a, 'f> {
    Begin { rows: u64 },
    Frame(&'a DecodedFrame<'f>),
}

/// A connected, authenticated helper. Create with [`HelperClient::launch`].
pub struct HelperClient {
    chan: Arc<Channel>,
    process: Mutex<Box<dyn HelperProcess>>,
    pid: u32,
    elevated: bool,
    request_timeout: Duration,
    io: Mutex<()>,
    state: Mutex<State>,
    last_scan: Mutex<Scalars>,
    last_usn: Mutex<Scalars>,
}

impl HelperClient {
    // ------------------------------------------------------------------ launch

    /// Start a helper and wait until it has connected and authenticated. `Denied` when the user
    /// declines, `Unavailable` for every other failure.
    pub fn launch(opts: LaunchOptions) -> Result<HelperClient, HelperError> {
        let elevated_now = opts.assume_elevated.unwrap_or_else(is_elevated);
        let uac = opts.elevate && !elevated_now;
        let prompt = if cfg!(windows) { CONNECT_TIMEOUT_UAC } else { CONNECT_TIMEOUT_POLKIT };
        let timeout = opts.connect_timeout.unwrap_or(if uac { prompt } else { CONNECT_TIMEOUT });
        let deadline = Instant::now() + timeout;
        let token = new_token();
        let mut listener = Listener::new()
            .map_err(|e| HelperError::unavailable(tr!("无法创建助手通道：{reason}", reason = e.to_string()), "unavailable"))?;
        let mut token_path: Option<PathBuf> = None;
        let mut process: Option<Box<dyn HelperProcess>> = None;
        let mut chan: Option<Channel> = None;
        let result = (|| {
            let start_failed =
                |e: std::io::Error| HelperError::unavailable(tr!("无法启动扫描助手：{reason}", reason = e.to_string()), "unavailable");
            let path = write_token_file(&token, None).map_err(start_failed)?;
            token_path = Some(path.clone());
            let mut argv = match &opts.command {
                Some(c) => c.clone(),
                None => helper_command().map_err(start_failed)?,
            };
            argv.extend(
                [
                    "--pipe",
                    listener.address(),
                    "--token-file",
                    &path.to_string_lossy(),
                    "--server-pid",
                    &std::process::id().to_string(),
                    "--idle-timeout",
                    &format!("{}", opts.idle_timeout.as_secs_f64()),
                ]
                .map(OsString::from),
            );
            let proc = if uac { start_elevated(&argv, &opts.elevator, timeout)? } else { start_subprocess(&argv)? };
            let proc = process.insert(proc);
            let c = chan.insert(accept(&mut listener, proc.as_mut(), deadline)?);
            handshake(c, proc.as_mut(), &token, deadline)
        })();
        listener.close();
        if let Some(path) = token_path {
            let _ = std::fs::remove_file(path);
        }
        match result {
            Ok((pid, elevated)) => {
                tracing::info!("scan helper ready: pid {pid}, elevated {elevated}");
                Ok(HelperClient {
                    chan: Arc::new(chan.expect("connected")),
                    process: Mutex::new(process.expect("started")),
                    pid,
                    elevated,
                    request_timeout: opts.request_timeout,
                    io: Mutex::new(()),
                    state: Mutex::new(State::default()),
                    last_scan: Mutex::new(Scalars::new()),
                    last_usn: Mutex::new(Scalars::new()),
                })
            }
            Err(e) => {
                if let Some(c) = chan {
                    c.close();
                }
                if let Some(mut p) = process {
                    reap(p.as_mut(), Duration::ZERO);
                }
                Err(e)
            }
        }
    }

    // ------------------------------------------------------------------ state

    /// Pid of the helper (from its hello, verified against the OS).
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn elevated(&self) -> bool {
        self.elevated
    }

    pub fn alive(&self) -> bool {
        {
            let st = self.state.lock();
            if st.closed || st.broken.is_some() {
                return false;
            }
        }
        !self.chan.is_closed() && self.process.lock().poll().is_none()
    }

    /// Geometry, rows, frames, names_bytes, wire_bytes and scan/send_seconds of the last finished
    /// MFT scan.
    pub fn last_scan(&self) -> Scalars {
        self.last_scan.lock().clone()
    }

    /// first/start/target_usn, reads, bytes_read, skipped, rows, frames, names_bytes, wire_bytes,
    /// read_seconds, send_seconds of the last finished `read_usn`.
    pub fn last_usn(&self) -> Scalars {
        self.last_usn.lock().clone()
    }

    /// Wait for the helper process to exit (tests, shutdown); its exit code.
    pub fn wait_exit(&self, timeout: Duration) -> Option<i32> {
        self.process.lock().wait(timeout)
    }

    fn break_off(&self, reason: &str) {
        {
            let mut st = self.state.lock();
            if st.broken.is_none() {
                st.broken = Some(reason.to_owned());
                tracing::warn!("{}", tr!("扫描助手连接中断：{reason}", reason = reason));
            }
        }
        self.chan.close();
    }

    /// Tear the connection down from any thread: a running call fails at once (`Unavailable`),
    /// the helper exits. Python: cancelling the task that awaits a request.
    pub fn abort(&self) {
        self.break_off(t("请求被中止"));
    }

    fn check_usable(&self) -> Result<(), HelperError> {
        let st = self.state.lock();
        if st.closed {
            return Err(HelperError::unavailable(t("扫描助手已关闭"), "closed"));
        }
        if let Some(why) = &st.broken {
            return Err(HelperError::unavailable(tr!("扫描助手不可用：{reason}", reason = why), "exited"));
        }
        Ok(())
    }

    // ------------------------------------------------------------------ exchange

    fn read(&self) -> Result<(u8, Vec<u8>), HelperError> {
        let timeout = Some(self.request_timeout);
        match self.chan.read_frame(timeout, timeout, MAX_BINARY_BYTES) {
            Ok(frame) => Ok(frame),
            Err(ChanError::Timeout) => {
                self.break_off(t("响应超时"));
                Err(HelperError::unavailable(t("扫描助手无响应"), "timeout"))
            }
            Err(ChanError::Frame(e)) => {
                self.break_off(&tr!("无效帧：{reason}", reason = &e.0));
                Err(HelperError::unavailable(tr!("扫描助手发送了无效帧：{reason}", reason = &e.0), "protocol"))
            }
            Err(e) => {
                self.break_off(&tr!("连接断开：{reason}", reason = e.to_string()));
                Err(HelperError::unavailable(t("扫描助手已退出"), "exited"))
            }
        }
    }

    fn protocol_error(&self, what: &str) -> HelperError {
        self.break_off(what);
        HelperError::unavailable(tr!("扫描助手协议错误：{reason}", reason = what), "protocol")
    }

    /// Send one request; feed `begin` and its bulk frames to `on_bulk` (Break: skip the rest of the
    /// stream, keep the helper); returns the response's `(result, extras)` (an error reply is an
    /// error).
    fn exchange(
        &self,
        call: Call,
        mut progress: Option<ProgressFn<'_>>,
        stream: (&str, &str),
        on_bulk: &mut dyn FnMut(Bulk<'_, '_>) -> Result<ControlFlow<()>, HelperError>,
    ) -> Result<(Scalars, Scalars), HelperError> {
        if let Err(why) = call.validate() {
            return Err(HelperError::error(tr!("无效的参数：{reason}", reason = why), code::INVALID_REQUEST));
        }
        let _io = self.io.lock();
        self.check_usable()?;
        let id = {
            let mut st = self.state.lock();
            st.next_id += 1;
            st.next_id
        };
        if let Err(e) = self.chan.send_message(&Request::new(id, call), Some(self.request_timeout)) {
            self.break_off(&tr!("发送失败：{reason}", reason = e.to_string()));
            return Err(HelperError::unavailable(t("扫描助手已退出"), "exited"));
        }
        let rid = RequestId::from(id);
        let mut draining = false;
        loop {
            let (kind, body) = self.read()?;
            if kind == KIND_BINARY {
                if draining {
                    continue;
                }
                let frame = decode_frame(&body)
                    .map_err(|e| self.protocol_error(&tr!("无效的数据帧：{reason}", reason = e.0)))?;
                if (frame.header.stream.as_str(), frame.header.layout.as_str()) != stream
                    || frame.header.request_id.as_ref() != Some(&rid)
                {
                    return Err(self.protocol_error(&tr!(
                        "意外的数据帧 {stream}/{layout}",
                        stream = &frame.header.stream,
                        layout = &frame.header.layout
                    )));
                }
                if on_bulk(Bulk::Frame(&frame))?.is_break() {
                    draining = true;
                }
                continue;
            }
            let reply = Reply::parse(&body)
                .map_err(|why| {
                    tracing::debug!("helper reply: {why}");
                    self.protocol_error(&tr!("无法解析的消息（{count} 处错误）", count = 1))
                })?;
            match reply {
                Reply::Progress { id: rid_, phase, elapsed, done, total } if rid_ == id => {
                    if let Some(p) = progress.as_mut() {
                        let read = (total > 0).then(|| (done as f64 / total as f64).min(1.0));
                        p(phase, elapsed, read);
                    }
                }
                Reply::Begin { id: rid_, rows, .. } if rid_ == id => {
                    if !draining && on_bulk(Bulk::Begin { rows })?.is_break() {
                        draining = true;
                    }
                }
                Reply::Response { id: rid_, result, extras } if rid_ == id => return Ok((result, extras)),
                Reply::Error { id: eid, code: c, message } if eid.is_none() || eid == Some(id) => {
                    if c == code::BAD_FRAME {
                        // the helper closes the connection after this
                        self.break_off(&message);
                        return Err(HelperError::unavailable(message, "protocol"));
                    }
                    return Err(HelperError::error(message, c));
                }
                other => {
                    return Err(self.protocol_error(&tr!("意外的 {kind} 消息", kind = other.type_name())));
                }
            }
        }
    }

    fn simple(&self, call: Call) -> Result<Scalars, HelperError> {
        let mut unexpected = |_: Bulk<'_, '_>| Err(self.protocol_error(t("意外的数据帧")));
        self.exchange(call, None, (MFT_STREAM, MFT_LAYOUT), &mut unexpected).map(|(result, _)| result)
    }

    /// Copy the batches straight into a table allocated from `begin` (no second copy).
    fn table<T: BulkTable>(&self, call: Call, progress: Option<ProgressFn<'_>>) -> Result<(T, Scalars, Scalars), HelperError> {
        let mut table: Option<T> = None;
        let mut declared = 0u64;
        let mut got = 0u64;
        let (result, extras) = self.exchange(call, progress, (T::STREAM, T::LAYOUT), &mut |item| {
            match item {
                Bulk::Begin { rows } => {
                    let mut t = T::default();
                    t.reserve(rows as usize);
                    table = Some(t);
                    declared = rows;
                }
                Bulk::Frame(frame) => {
                    let Some(t) = table.as_mut() else {
                        return Err(self.protocol_error(t("数据帧之前没有 begin")));
                    };
                    let n = frame.header.count;
                    if got + n > declared {
                        return Err(self.protocol_error(&tr!("数据行数超过声明的 {rows}", rows = declared)));
                    }
                    add_checked(t, frame).map_err(|why| self.protocol_error(&why))?;
                    got += n;
                }
            }
            Ok(ControlFlow::Continue(()))
        })?;
        let Some(table) = table else {
            return Err(HelperError::unavailable(t("扫描助手没有返回数据"), "protocol"));
        };
        if got != declared {
            return Err(HelperError::unavailable(
                tr!("扫描助手协议错误：{reason}", reason = tr!("声明 {rows} 行，收到 {got} 行", rows = declared, got = got)),
                "protocol",
            ));
        }
        Ok((table, result, extras))
    }

    // ------------------------------------------------------------------ operations

    /// `{pid, elevated, version, platform}` of the helper.
    pub fn ping(&self) -> Result<Scalars, HelperError> {
        self.simple(Call::Ping {})
    }

    /// Drive type, filesystem, label, serial, sector/cluster size of a local NTFS `X:`; with
    /// administrator rights also record_size, mft_start_lcn, mft_valid_data_length …
    pub fn volume_info(&self, volume: &str) -> Result<Scalars, HelperError> {
        self.simple(Call::VolumeInfo { volume: volume.into() })
    }

    /// Read and parse the `$MFT` of `volume` in the helper; the whole table, as a local parse
    /// returns it. Geometry and timings are in [`last_scan`](Self::last_scan) afterwards.
    pub fn scan_mft(&self, volume: &str, opts: MftOptions, progress: Option<ProgressFn<'_>>) -> Result<MftTable, HelperError> {
        let call = Call::ScanMft {
            volume: volume.into(),
            batch_rows: opts.batch_rows,
            duty: opts.duty,
        };
        self.mft_table(call, progress)
    }

    /// [`scan_mft`](Self::scan_mft) on an NTFS volume image file (unelevated helpers only; tests).
    pub fn scan_mft_image(&self, path: &str, opts: MftOptions, progress: Option<ProgressFn<'_>>) -> Result<MftTable, HelperError> {
        let call = Call::ScanMftImage {
            path: path.into(),
            batch_rows: opts.batch_rows,
            duty: opts.duty,
        };
        self.mft_table(call, progress)
    }

    fn mft_table(&self, call: Call, progress: Option<ProgressFn<'_>>) -> Result<MftTable, HelperError> {
        let (mut table, result, extras) = self.table::<MftTable>(call, progress)?;
        table.bad_records = extras.get("bad_records").and_then(Value::as_u64).unwrap_or(0);
        *self.last_scan.lock() = result;
        Ok(table)
    }

    /// Read and parse the `$MFT` of `volume` in the helper; hand over batches of rows (each with its
    /// own `names`, `name_off` relative to it). `Break` skips the rest of the stream; the helper
    /// stays usable.
    pub fn stream_mft(
        &self,
        volume: &str,
        opts: MftOptions,
        progress: Option<ProgressFn<'_>>,
        on_batch: &mut dyn FnMut(MftTable) -> ControlFlow<()>,
    ) -> Result<(), HelperError> {
        let call = Call::ScanMft {
            volume: volume.into(),
            batch_rows: opts.batch_rows,
            duty: opts.duty,
        };
        self.batches(call, progress, on_batch)
    }

    /// [`stream_mft`](Self::stream_mft) on an NTFS volume image file (unelevated helpers only).
    pub fn stream_mft_image(
        &self,
        path: &str,
        opts: MftOptions,
        progress: Option<ProgressFn<'_>>,
        on_batch: &mut dyn FnMut(MftTable) -> ControlFlow<()>,
    ) -> Result<(), HelperError> {
        let call = Call::ScanMftImage {
            path: path.into(),
            batch_rows: opts.batch_rows,
            duty: opts.duty,
        };
        self.batches(call, progress, on_batch)
    }

    fn batches(
        &self,
        call: Call,
        progress: Option<ProgressFn<'_>>,
        on_batch: &mut dyn FnMut(MftTable) -> ControlFlow<()>,
    ) -> Result<(), HelperError> {
        let (result, _) = self.exchange(call, progress, (MFT_STREAM, MFT_LAYOUT), &mut |item| {
            let Bulk::Frame(frame) = item else {
                return Ok(ControlFlow::Continue(()));
            };
            let mut batch = MftTable::default();
            add_checked(&mut batch, frame).map_err(|why| self.protocol_error(&why))?;
            Ok(on_batch(batch))
        })?;
        *self.last_scan.lock() = result;
        Ok(())
    }

    /// The change journal of a local NTFS `X:` (administrator rights): `{volume, journal_id,
    /// first_usn, next_usn, lowest_valid_usn, max_usn, max_size, allocation_delta}`.
    /// `(journal_id, next_usn)` is the checkpoint to [`read_usn`](Self::read_usn) from later.
    /// Error `usn_inactive` when the volume has no active journal.
    pub fn query_usn(&self, volume: &str) -> Result<Scalars, HelperError> {
        self.simple(Call::QueryUsn { volume: volume.into() })
    }

    /// Journal records from the checkpoint `(journal_id, start_usn)` up to the journal's end (at
    /// most `max_rows`; `truncated` then says to read again from `next_usn`). Error `usn_gone` when
    /// the checkpoint is no longer valid, `usn_inactive` when the journal is off. Statistics are in
    /// [`last_usn`](Self::last_usn) afterwards.
    pub fn read_usn(
        &self,
        volume: &str,
        start_usn: i64,
        journal_id: u64,
        max_rows: u64,
        duty: f64,
        progress: Option<ProgressFn<'_>>,
    ) -> Result<UsnTable, HelperError> {
        let call = Call::ReadUsn {
            volume: volume.into(),
            start_usn,
            journal_id,
            max_rows,
            duty,
        };
        let (rows, result, extras) = self.table::<UsnRows>(call, progress)?;
        *self.last_usn.lock() = result;
        Ok(UsnTable {
            rows,
            journal_id: extras.get("journal_id").and_then(Value::as_u64).unwrap_or(journal_id),
            next_usn: extras.get("next_usn").and_then(Value::as_i64).unwrap_or(start_usn),
            truncated: extras.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    /// The raw health pages of a Linux disk (kernel name, e.g. `nvme0n1`, `sda`), as hex:
    /// `{kind: "nvme", log}` or `{kind: "ata", identify, general, temperature}`. Elevated helpers on
    /// Linux only; error `not_supported` when the device does not answer.
    pub fn disk_health(&self, disk: &str) -> Result<Scalars, HelperError> {
        self.simple(Call::DiskHealth { disk: disk.into() })
    }

    // ------------------------------------------------------------------ lifecycle

    /// Ask the helper to exit, close the channel and reap the process (idempotent).
    pub fn close(&self) {
        if self.state.lock().closed {
            return;
        }
        let usable = self.state.lock().broken.is_none() && !self.chan.is_closed();
        if usable && let Some(_io) = self.io.try_lock() {
            let id = {
                let mut st = self.state.lock();
                st.next_id += 1;
                st.next_id
            };
            let quick = Some(Duration::from_secs(2));
            if self.chan.send_message(&Request::new(id, Call::Shutdown {}), quick).is_ok() {
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline {
                    match self.chan.read_frame(quick, quick, MAX_BINARY_BYTES) {
                        Ok((KIND_JSON, body)) => {
                            if matches!(Reply::parse(&body), Ok(Reply::Response { id: r, .. }) if r == id) {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }
        }
        self.state.lock().closed = true;
        self.chan.close();
        reap(self.process.lock().as_mut(), CLOSE_TIMEOUT);
    }
}

impl Drop for HelperClient {
    fn drop(&mut self) {
        self.close();
    }
}

/// Append one frame to `table`: the names blob, then the columns with `name_off` rebased.
fn add_checked<T: BulkTable>(table: &mut T, frame: &DecodedFrame<'_>) -> Result<(), String> {
    let blob = frame.blob("names").ok_or_else(|| t("缺少名字数据").to_owned())?;
    let base = table.names().len();
    if base + blob.len() > u32::MAX as usize {
        return Err(t("名字总长超过 4 GiB").to_owned());
    }
    let start = table.rows();
    table.add_frame(frame, base as u32)?;
    let (off, len) = (&table.name_off()[start..], &table.name_len()[start..]);
    if off.iter().zip(len).any(|(&o, &l)| o as usize - base + l as usize > blob.len()) {
        return Err(t("名字偏移越界").to_owned());
    }
    table.names_mut().extend_from_slice(blob);
    Ok(())
}

/// Wait for the helper to connect, giving up early if it exits.
fn accept(listener: &mut Listener, process: &mut dyn HelperProcess, deadline: Instant) -> Result<Channel, HelperError> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(HelperError::unavailable(t("扫描助手没有按时连接"), "timeout"));
        }
        match listener.accept(ACCEPT_SLICE.min(deadline - now)) {
            Ok(chan) => return Ok(chan),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(HelperError::unavailable(tr!("扫描助手鉴权失败：{reason}", reason = e.to_string()), "auth"));
            }
            Err(e) => {
                return Err(HelperError::unavailable(
                    tr!("等待扫描助手连接失败：{reason}", reason = e.to_string()),
                    "exited",
                ));
            }
        }
        if let Some(code) = process.poll() {
            if let Some(err) = process.exit_error(code) {
                return Err(err); // pkexec: refused or dismissed
            }
            return Err(HelperError::unavailable(tr!("扫描助手启动后退出（退出码 {code}）", code = code), "exited"));
        }
    }
}

fn auth_failed(why: &str) -> HelperError {
    let message = tr!("扫描助手鉴权失败：{reason}", reason = why);
    tracing::warn!("{message}");
    HelperError::unavailable(message, "auth")
}

/// First frame must be a hello with our token, from the process we launched. Returns (pid, elevated).
fn handshake(chan: &Channel, process: &mut dyn HelperProcess, token: &str, deadline: Instant) -> Result<(u32, bool), HelperError> {
    let left = deadline.saturating_duration_since(Instant::now());
    let timeout = left.min(HELLO_TIMEOUT).max(Duration::from_millis(500));
    let (kind, body) = match chan.read_frame(Some(timeout), Some(timeout), 0) {
        Ok(f) => f,
        Err(ChanError::Timeout) => return Err(HelperError::unavailable(t("扫描助手没有发送 hello"), "timeout")),
        Err(ChanError::Frame(e)) => return Err(auth_failed(&tr!("首帧无效（{reason}）", reason = &e.0))),
        Err(e) => {
            return Err(HelperError::unavailable(
                tr!("扫描助手在握手时断开：{reason}", reason = e.to_string()),
                "exited",
            ));
        }
    };
    let hello = if kind == KIND_JSON { Reply::parse(&body).ok() } else { None };
    let Some(Reply::Hello { version, token: theirs, pid, elevated }) = hello else {
        return Err(auth_failed(t("首帧不是 hello")));
    };
    if version != PROTOCOL_VERSION {
        return Err(auth_failed(&tr!("协议版本 {got}，需要 {need}", got = version, need = PROTOCOL_VERSION)));
    }
    if !tokens_match(&theirs, token) {
        return Err(auth_failed(t("令牌不符")));
    }
    if pid != chan.peer_pid() {
        return Err(auth_failed(&tr!(
            "声称的进程号 {claimed} 与实际连接方 {peer} 不符",
            claimed = pid,
            peer = chan.peer_pid()
        )));
    }
    if !peer_is_launched(chan.peer_pid(), process.pid()) {
        return Err(auth_failed(&tr!(
            "连接方 {peer} 不是启动的进程 {pid}",
            peer = chan.peer_pid(),
            pid = process.pid()
        )));
    }
    Ok((pid, elevated))
}

/// Wait up to `grace` for the helper to exit, then kill it; release the handle.
fn reap(process: &mut dyn HelperProcess, grace: Duration) {
    if !grace.is_zero() && process.poll().is_none() {
        process.wait(grace);
    }
    if process.poll().is_none() {
        process.terminate();
        process.wait(Duration::from_secs(2));
    }
    process.close();
}

// --------------------------------------------------------------------------- shared helper

/// At most one helper per backend: launched on first use, reused while it lives.
///
/// A helper exits after 10 idle minutes; the next [`get`](Self::get) then launches (and, elevated,
/// prompts) again.
pub struct SharedHelper {
    options: LaunchOptions,
    client: Mutex<Option<Arc<HelperClient>>>,
}

impl SharedHelper {
    /// `options.elevate` is replaced by the argument of each [`get`](Self::get).
    pub fn new(options: LaunchOptions) -> Self {
        Self {
            options,
            client: Mutex::new(None),
        }
    }

    /// The live helper, if any.
    pub fn current(&self) -> Option<Arc<HelperClient>> {
        self.client.lock().as_ref().filter(|c| c.alive()).cloned()
    }

    /// The live helper, or a newly launched one.
    pub fn get(&self, elevate: bool) -> Result<Arc<HelperClient>, HelperError> {
        let mut slot = self.client.lock();
        if let Some(c) = slot.as_ref()
            && c.alive()
            && (c.elevated() || !elevate)
        {
            return Ok(c.clone());
        }
        if let Some(old) = slot.take() {
            old.close();
        }
        let opts = LaunchOptions {
            elevate,
            ..self.options.clone()
        };
        let client = Arc::new(HelperClient::launch(opts)?);
        *slot = Some(client.clone());
        Ok(client)
    }

    pub fn close(&self) {
        if let Some(c) = self.client.lock().take() {
            c.close();
        }
    }
}
