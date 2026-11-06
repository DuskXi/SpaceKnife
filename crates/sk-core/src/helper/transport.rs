//! Local byte channels between the backend and the helper (Python `helper/transport.py`).
//!
//! Windows: a named pipe `\\.\pipe\spaceknife-helper-<hex>` (single instance, local clients only,
//! DACL for this user); POSIX: a unix-domain socket in a private 0700 directory. The backend
//! (unprivileged) is always the server and owns the address.
//!
//! Both ends use the same blocking [`Channel`] (the backend runs it on its own threads). Timeouts
//! are *inactivity* timeouts: each underlying read or write must make progress in time, so a large
//! frame is never cut short just because it is large. Every channel knows the pid of the process at
//! the other end (from the OS, not from anything the peer says). [`Channel::close`] may be called
//! from any thread and aborts a blocked read or write.

use std::io;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::messages::{FrameError, HEADER_LEN, check_header, json_frame};
use crate::i18n::t;
use crate::tr;

pub const PIPE_PREFIX: &str = r"\\.\pipe\spaceknife-helper-";
pub const SOCKET_NAME: &str = "helper.sock";

/// Why a channel operation failed.
#[derive(Debug)]
pub enum ChanError {
    /// The peer closed the channel before the first byte (a clean end).
    Eof,
    /// No progress within the timeout.
    Timeout,
    /// Closed here, reset, or broken in the middle of a frame.
    Closed(String),
    /// The byte stream is not a valid frame sequence.
    Frame(FrameError),
    Io(io::Error),
}

impl std::fmt::Display for ChanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChanError::Eof => f.write_str(t("对端已关闭")),
            ChanError::Timeout => f.write_str(t("读写超时")),
            ChanError::Closed(why) => f.write_str(why),
            ChanError::Frame(e) => f.write_str(&e.0),
            ChanError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for ChanError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ChanError::Timeout,
            io::ErrorKind::UnexpectedEof => ChanError::Eof,
            io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected => ChanError::Closed(e.to_string()),
            _ => ChanError::Io(e),
        }
    }
}

fn incomplete() -> ChanError {
    ChanError::Closed(t("帧不完整：对端已关闭").to_owned())
}

/// Whether `address` is one this module could have created (the helper refuses others).
pub fn valid_address(address: &str) -> bool {
    #[cfg(windows)]
    {
        address.strip_prefix(PIPE_PREFIX).is_some_and(|hex| {
            hex.len() == 32 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
    }
    #[cfg(unix)]
    {
        let path = std::path::Path::new(address);
        let dir_ok = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("spaceknife-helper-"))
            .is_some_and(|rest| {
                !rest.is_empty()
                    && rest
                        .bytes()
                        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'z' | b'_'))
            });
        path.is_absolute() && path.file_name().is_some_and(|n| n == SOCKET_NAME) && dir_ok
    }
}

/// A connected channel (either end).
pub struct Channel {
    inner: imp::Conn,
    peer_pid: u32,
}

impl Channel {
    /// Pid of the process at the other end, from the OS.
    pub fn peer_pid(&self) -> u32 {
        self.peer_pid
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// Abort pending I/O and release the channel (idempotent, callable from any thread).
    pub fn close(&self) {
        self.inner.close()
    }

    /// Fill `buf`. `Eof` if the peer closed before the first byte, `Closed` after.
    pub fn recv_exact(&self, buf: &mut [u8], timeout: Option<Duration>) -> Result<(), ChanError> {
        let mut got = 0;
        while got < buf.len() {
            match self.inner.read(&mut buf[got..], timeout) {
                Ok(0) => {
                    return Err(if got > 0 {
                        incomplete()
                    } else {
                        ChanError::Eof
                    });
                }
                Ok(n) => got += n,
                Err(e) => {
                    let e = ChanError::from(e);
                    return Err(match e {
                        ChanError::Eof if got > 0 => incomplete(),
                        other => other,
                    });
                }
            }
        }
        Ok(())
    }

    /// Write every buffer, in order.
    pub fn send(&self, buffers: &[&[u8]], timeout: Option<Duration>) -> Result<(), ChanError> {
        for buf in buffers {
            let mut sent = 0;
            while sent < buf.len() {
                match self.inner.write(&buf[sent..], timeout) {
                    Ok(0) => return Err(ChanError::Closed(t("管道已断开").to_owned())),
                    Ok(n) => sent += n,
                    Err(e) => {
                        return Err(match ChanError::from(e) {
                            ChanError::Eof => ChanError::Closed(t("管道已断开").to_owned()),
                            other => other,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// One frame; `first_timeout` bounds the wait for its first byte (the idle timeout),
    /// `timeout` every later read.
    pub fn read_frame(
        &self,
        first_timeout: Option<Duration>,
        timeout: Option<Duration>,
        max_binary: usize,
    ) -> Result<(u8, Vec<u8>), ChanError> {
        let mut head = [0u8; HEADER_LEN];
        self.recv_exact(&mut head[..1], first_timeout)?;
        match self.recv_exact(&mut head[1..], timeout) {
            Err(ChanError::Eof) => return Err(incomplete()),
            r => r?,
        }
        let (kind, len) = check_header(&head, max_binary).map_err(ChanError::Frame)?;
        let mut body = vec![0u8; len];
        if len > 0 {
            match self.recv_exact(&mut body, timeout) {
                Err(ChanError::Eof) => return Err(incomplete()),
                r => r?,
            }
        }
        Ok((kind, body))
    }

    pub fn send_message<T: Serialize>(
        &self,
        message: &T,
        timeout: Option<Duration>,
    ) -> Result<(), ChanError> {
        let (head, body) = json_frame(message);
        self.send(&[&head, &body], timeout)
    }
}

/// Helper side: connect to the backend's listener (only to an address it could have made).
pub fn connect(address: &str, timeout: Duration) -> io::Result<Channel> {
    if !valid_address(address) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            tr!(
                "无效的助手地址：{address}",
                address = format!("{address:?}")
            ),
        ));
    }
    imp::connect(address, timeout)
}

/// The backend's end, created before the helper is launched; accepts exactly one connection.
pub struct Listener {
    inner: imp::Listen,
    address: String,
}

impl Listener {
    pub fn new() -> io::Result<Self> {
        let (inner, address) = imp::Listen::new()?;
        Ok(Self { inner, address })
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// Wait up to `wait` for the helper to connect (`TimedOut` otherwise; may retry).
    pub fn accept(&mut self, wait: Duration) -> io::Result<Channel> {
        self.inner.accept(wait)
    }

    pub fn close(&mut self) {
        self.inner.close()
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.inner.close();
    }
}

/// Sleep in small steps until `deadline` or `done()`.
#[allow(dead_code)]
fn wait_until(deadline: Instant, step: Duration) -> bool {
    let now = Instant::now();
    if now >= deadline {
        return false;
    }
    std::thread::sleep(step.min(deadline - now));
    true
}

// --------------------------------------------------------------------------- Windows

#[cfg(windows)]
mod imp {
    use super::super::win32::{self, Pipe};
    use super::*;

    pub struct Conn(Pipe);

    impl Conn {
        pub fn read(&self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<usize> {
            self.0.read(buf, timeout)
        }
        pub fn write(&self, buf: &[u8], timeout: Option<Duration>) -> io::Result<usize> {
            self.0.write(buf, timeout)
        }
        pub fn close(&self) {
            self.0.close()
        }
        pub fn is_closed(&self) -> bool {
            self.0.is_closed()
        }
    }

    pub struct Listen(Option<Pipe>);

    impl Listen {
        pub fn new() -> io::Result<(Self, String)> {
            let address = format!("{PIPE_PREFIX}{}", hex(&win32::random_bytes(16)));
            let pipe = win32::create_pipe_server(&address)?;
            Ok((Self(Some(pipe)), address))
        }

        pub fn accept(&mut self, wait: Duration) -> io::Result<Channel> {
            let Some(pipe) = &self.0 else {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    t("监听已关闭"),
                ));
            };
            pipe.accept(Some(wait))?;
            let pipe = self.0.take().expect("checked above");
            let peer_pid = pipe.client_pid()?;
            Ok(Channel {
                inner: Conn(pipe),
                peer_pid,
            })
        }

        pub fn close(&mut self) {
            if let Some(p) = self.0.take() {
                p.close();
            }
        }
    }

    pub fn connect(address: &str, timeout: Duration) -> io::Result<Channel> {
        let pipe = win32::connect_pipe(address, timeout)?;
        let peer_pid = pipe.server_pid()?;
        Ok(Channel {
            inner: Conn(pipe),
            peer_pid,
        })
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

// --------------------------------------------------------------------------- POSIX

#[cfg(unix)]
mod imp {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// (pid, uid) of the process at the other end of a connected unix socket.
    fn peer_cred(sock: &UnixStream) -> io::Result<(u32, Option<u32>)> {
        let fd = sock.as_raw_fd();
        #[cfg(target_os = "linux")]
        {
            let mut cred = libc::ucred {
                pid: 0,
                uid: 0,
                gid: 0,
            };
            let mut len = size_of::<libc::ucred>() as libc::socklen_t;
            // SAFETY: valid fd and out-buffer of the given length.
            let rc = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    (&mut cred as *mut libc::ucred).cast(),
                    &mut len,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok((cred.pid as u32, Some(cred.uid)))
        }
        #[cfg(not(target_os = "linux"))]
        {
            // macOS: SOL_LOCAL (0), LOCAL_PEERPID (2); the 0700 socket directory already limits who
            // can connect
            let mut pid: libc::pid_t = 0;
            let mut len = size_of::<libc::pid_t>() as libc::socklen_t;
            // SAFETY: valid fd and out-buffer of the given length.
            let rc = unsafe {
                libc::getsockopt(fd, 0, 2, (&mut pid as *mut libc::pid_t).cast(), &mut len)
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok((pid as u32, None))
        }
    }

    pub struct Conn {
        sock: UnixStream,
        closed: AtomicBool,
    }

    impl Conn {
        fn new(sock: UnixStream) -> io::Result<Self> {
            sock.set_nonblocking(false)?;
            Ok(Self {
                sock,
                closed: AtomicBool::new(false),
            })
        }

        fn timeouts(&self, timeout: Option<Duration>, write: bool) -> io::Result<()> {
            let t = timeout.map(|d| d.max(Duration::from_millis(1)));
            let set = if write {
                self.sock.set_write_timeout(t)
            } else {
                self.sock.set_read_timeout(t)
            };
            match set {
                // macOS (BSD) refuses setsockopt with EINVAL once the peer has closed the
                // connection: the read or write that follows reports that (EOF, EPIPE) and never
                // blocks, so go on to it rather than fail with "invalid argument"
                Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(()),
                other => other,
            }
        }

        fn check(&self) -> io::Result<()> {
            if self.closed.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    t("套接字已关闭"),
                ));
            }
            Ok(())
        }

        pub fn read(&self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<usize> {
            use std::io::Read as _;
            self.check()?;
            self.timeouts(timeout, false)?;
            loop {
                match (&self.sock).read(buf) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == io::ErrorKind::ConnectionReset => return Ok(0),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, t("套接字读写超时")));
                    }
                    Ok(0) if self.closed.load(Ordering::Acquire) => {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            t("套接字已关闭"),
                        ));
                    }
                    other => return other,
                }
            }
        }

        /// Write some of `buf`. A blocked write wakes every [`WRITE_SLICE`] to see whether the
        /// peer is gone: on macOS a peer that shut its end down leaves the writer blocked on a
        /// full buffer (no EPIPE, as Linux gives) until the timeout.
        pub fn write(&self, buf: &[u8], timeout: Option<Duration>) -> io::Result<usize> {
            use std::io::Write as _;
            const WRITE_SLICE: Duration = Duration::from_millis(200);
            self.check()?;
            let deadline = timeout.map(|t| std::time::Instant::now() + t);
            let chunk = buf.len().min(8 << 20);
            loop {
                let slice = match deadline {
                    Some(d) => d.saturating_duration_since(std::time::Instant::now()),
                    None => WRITE_SLICE,
                };
                if slice.is_zero() {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, t("套接字读写超时")));
                }
                self.timeouts(Some(slice.min(WRITE_SLICE)), true)?;
                match (&self.sock).write(&buf[..chunk]) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        if self.peer_gone() {
                            return Err(io::Error::new(io::ErrorKind::BrokenPipe, t("管道已断开")));
                        }
                    }
                    other => return other,
                }
            }
        }

        /// Whether the peer closed its end: a peek at what it sent finds the end of the stream.
        fn peer_gone(&self) -> bool {
            use std::os::fd::AsRawFd as _;
            let mut byte = 0u8;
            // SAFETY: one byte into a local; MSG_PEEK leaves the data for the reader.
            let n = unsafe {
                libc::recv(
                    self.sock.as_raw_fd(),
                    (&raw mut byte).cast(),
                    1,
                    libc::MSG_PEEK | libc::MSG_DONTWAIT,
                )
            };
            n == 0
                || (n < 0
                    && matches!(
                        io::Error::last_os_error().raw_os_error(),
                        Some(libc::ECONNRESET | libc::ENOTCONN | libc::EPIPE)
                    ))
        }

        pub fn close(&self) {
            if !self.closed.swap(true, Ordering::AcqRel) {
                let _ = self.sock.shutdown(std::net::Shutdown::Both);
            }
        }

        pub fn is_closed(&self) -> bool {
            self.closed.load(Ordering::Acquire)
        }
    }

    pub struct Listen {
        sock: Option<UnixListener>,
        dir: Option<PathBuf>,
    }

    impl Listen {
        pub fn new() -> io::Result<(Self, String)> {
            let mut bytes = [0u8; 8];
            {
                use std::io::Read as _;
                std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
            }
            let dir = std::env::temp_dir().join(format!("spaceknife-helper-{}", hex(&bytes)));
            std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
            let address = dir.join(SOCKET_NAME);
            let mut me = Self {
                sock: None,
                dir: Some(dir),
            };
            let sock = UnixListener::bind(&address).inspect_err(|_| me.close())?;
            sock.set_nonblocking(true).inspect_err(|_| me.close())?;
            me.sock = Some(sock);
            Ok((me, address.to_string_lossy().into_owned()))
        }

        pub fn accept(&mut self, wait: Duration) -> io::Result<Channel> {
            let Some(sock) = &self.sock else {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    t("监听已关闭"),
                ));
            };
            let deadline = Instant::now() + wait;
            let conn = loop {
                match sock.accept() {
                    Ok((conn, _)) => break conn,
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::Interrupted =>
                    {
                        if !wait_until(deadline, Duration::from_millis(10)) {
                            return Err(io::Error::new(io::ErrorKind::TimedOut, t("等待连接超时")));
                        }
                    }
                    Err(e) => return Err(e),
                }
            };
            let (pid, uid) = peer_cred(&conn)?;
            // SAFETY: no preconditions.
            let me = unsafe { libc::getuid() };
            if let Some(uid) = uid
                && uid != me
                && uid != 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    tr!("拒绝了用户 {uid} 的连接", uid = uid),
                ));
            }
            self.close(); // one connection only: nobody else may connect after the helper
            Ok(Channel {
                inner: Conn::new(conn)?,
                peer_pid: pid,
            })
        }

        pub fn close(&mut self) {
            self.sock = None;
            if let Some(dir) = self.dir.take() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    pub fn connect(address: &str, timeout: Duration) -> io::Result<Channel> {
        let _ = timeout; // a local socket connects at once or fails
        let sock = UnixStream::connect(address)?;
        let (pid, _uid) = peer_cred(&sock)?;
        Ok(Channel {
            inner: Conn::new(sock)?,
            peer_pid: pid,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_validated() {
        let mut listener = Listener::new().unwrap();
        assert!(valid_address(listener.address()), "{}", listener.address());
        #[cfg(windows)]
        assert!(
            listener.address().starts_with(PIPE_PREFIX)
                && listener.address().len() == PIPE_PREFIX.len() + 32
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::path::Path::new(listener.address()).parent().unwrap();
            assert_eq!(
                std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        listener.close();
        for bad in [
            r"C:\Windows\System32\config\SAM",
            r"\\.\pipe\other",
            r"\\.\C:",
            "/etc/passwd",
            "relative.sock",
            "",
        ] {
            assert!(!valid_address(bad), "{bad}");
        }
    }

    #[test]
    fn a_channel_pair_carries_frames_and_closes() {
        let mut listener = Listener::new().unwrap();
        let address = listener.address().to_owned();
        let client = std::thread::spawn(move || {
            let chan = connect(&address, Duration::from_secs(5)).unwrap();
            assert_eq!(chan.peer_pid(), std::process::id());
            let (kind, body) = chan
                .read_frame(
                    Some(Duration::from_secs(5)),
                    Some(Duration::from_secs(5)),
                    0,
                )
                .unwrap();
            assert_eq!(
                (kind, body.as_slice()),
                (super::super::messages::KIND_JSON, b"{\"a\":1}".as_slice())
            );
            chan.send_message(&serde_json::json!({"b": 2}), Some(Duration::from_secs(5)))
                .unwrap();
            // the other end closes: a clean EOF
            assert!(matches!(
                chan.read_frame(Some(Duration::from_secs(5)), None, 0),
                Err(ChanError::Eof)
            ));
        });
        let mut chan = None;
        for _ in 0..100 {
            match listener.accept(Duration::from_millis(100)) {
                Ok(c) => {
                    chan = Some(c);
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
                Err(e) => panic!("{e}"),
            }
        }
        let chan = chan.expect("connected");
        assert_eq!(chan.peer_pid(), std::process::id());
        chan.send_message(&serde_json::json!({"a": 1}), Some(Duration::from_secs(5)))
            .unwrap();
        let (_, body) = chan
            .read_frame(
                Some(Duration::from_secs(5)),
                Some(Duration::from_secs(5)),
                0,
            )
            .unwrap();
        assert_eq!(body, b"{\"b\":2}");
        // a read with a short timeout times out
        assert!(matches!(
            chan.read_frame(Some(Duration::from_millis(50)), None, 0),
            Err(ChanError::Timeout)
        ));
        chan.close();
        assert!(chan.is_closed());
        client.join().unwrap();
        assert!(chan.send(&[b"x"], Some(Duration::from_secs(1))).is_err());
    }

    #[test]
    fn close_from_another_thread_aborts_a_blocked_read() {
        let mut listener = Listener::new().unwrap();
        let address = listener.address().to_owned();
        let peer = std::thread::spawn(move || {
            let chan = connect(&address, Duration::from_secs(5)).unwrap();
            std::thread::sleep(Duration::from_millis(800));
            drop(chan);
        });
        let chan = loop {
            match listener.accept(Duration::from_millis(100)) {
                Ok(c) => break std::sync::Arc::new(c),
                Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
                Err(e) => panic!("{e}"),
            }
        };
        let c2 = chan.clone();
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            c2.close();
        });
        let t0 = Instant::now();
        let err = chan
            .read_frame(Some(Duration::from_secs(10)), None, 0)
            .unwrap_err();
        assert!(t0.elapsed() < Duration::from_millis(700), "{err:?}");
        assert!(
            matches!(err, ChanError::Closed(_) | ChanError::Eof),
            "{err:?}"
        );
        closer.join().unwrap();
        peer.join().unwrap();
    }
}
