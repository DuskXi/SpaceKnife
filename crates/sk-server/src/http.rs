//! The HTTP side: `/v1` (WebSocket upgrade with path, origin and subprotocol checks) and, in
//! headless mode, the interface itself (the Electron `headless-server.ts`, served directly):
//!
//! | path | |
//! |---|---|
//! | `GET /`, `/_nuxt/…` | the built interface, public. The page: `no-store`, a `Content-Security-Policy` header that lets only its own inline scripts run (their hashes), reach this server's own `ws://` / `wss://` address and nothing frame it. Nuxt's bundles (hashed names): cached for good; the rest revalidated with an `ETag` |
//! | `GET /sk/info` | `{platform, version, hostname, home, secure}`, only with `Authorization: Bearer <token>` |
//! | `GET /v1` | the WebSocket. In headless mode a browser's `Origin` must be `<scheme>://<Host>` (its own page); a request without one is refused, as 0.3.4's server refused it |
//!
//! A server reachable from elsewhere (headless mode, any address but loopback) is served by
//! [`serve_guarded`], as 0.3.4's headless server was guarded: at most [`Limits::max_sockets`]
//! connections at a time, request headers within [`Limits::header_timeout`] (an idle connection
//! is closed after it too), each request answered within [`Limits::request_timeout`].

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router as HttpRouter;
use axum::body::Body;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::{Extension, State, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use sha2::{Digest as _, Sha256};
use sk_protocol::{SUBPROTOCOL, WS_PATH};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio_rustls::TlsAcceptor;

use crate::app::Server;
use crate::util::constant_time_eq;

pub(crate) fn router(server: Arc<Server>) -> HttpRouter {
    let mut r = HttpRouter::new().route(WS_PATH, any(ws_endpoint));
    if server.config.static_dir.is_some() {
        r = r.route("/sk/info", any(info)).fallback(static_file);
    } else {
        r = r.fallback(|| async { (StatusCode::NOT_FOUND, "Not Found\n") });
    }
    r.with_state(server)
}

const SECURITY_HEADERS: [(&str, &str); 5] = [
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("x-frame-options", "DENY"),
    ("cross-origin-opener-policy", "same-origin"),
    ("cross-origin-resource-policy", "same-origin"),
];

fn plain(status: StatusCode, text: &'static str) -> Response {
    let mut r = (status, text).into_response();
    secure_headers(r.headers_mut());
    r
}

fn secure_headers(h: &mut HeaderMap) {
    for (k, v) in SECURITY_HEADERS {
        h.insert(k, HeaderValue::from_static(v));
    }
}

/// A `Host` header worth echoing into the page's policy: a name or an address, an optional port.
pub fn host_ok(host: &str) -> bool {
    let port = if let Some(rest) = host.strip_prefix('[') {
        let Some((addr, tail)) = rest.split_once(']') else {
            return false;
        };
        if addr.is_empty()
            || !addr
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
        {
            return false;
        }
        tail
    } else {
        let (name, port) = match host.find(':') {
            Some(i) => (&host[..i], &host[i..]),
            None => (host, ""),
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return false;
        }
        port
    };
    match port.strip_prefix(':') {
        None => port.is_empty(),
        Some(digits) => {
            (1..=5).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
        }
    }
}

/// The browser's `Origin` is the origin it loaded the page from: the scheme and the `Host` it asked
/// for (another site's page carries its own: cross-site WebSocket hijacking).
pub fn same_origin(origin: Option<&str>, host: Option<&str>, secure: bool) -> bool {
    let (Some(origin), Some(host)) = (origin, host) else {
        return false;
    };
    if !host_ok(host) {
        return false;
    }
    let scheme = if secure { "https" } else { "http" };
    origin.eq_ignore_ascii_case(&format!("{scheme}://{host}"))
}

/// Whether a WebSocket may open. `allowed`: the origins let in by name (the app's own windows,
/// only on a loopback address, and `allow_origins`). In headless mode the page's own origin is let
/// in too, and a request without an `Origin` is not: a browser always sends one, and 0.3.4's
/// headless server never forwarded one without. Otherwise a native client without one is let in
/// (the token still guards the session).
pub fn ws_origin_ok(
    origin: Option<&str>,
    host: Option<&str>,
    headless: bool,
    secure: bool,
    allowed: &[String],
) -> bool {
    match origin {
        None => !headless,
        Some(o) => {
            allowed.iter().any(|a| a == o) || (headless && same_origin(Some(o), host, secure))
        }
    }
}

/// `index.html` with a CSP letting the page reach this server's WebSocket by the name it used
/// (for a page that brings its own `<meta>` policy, as 0.3.4's did).
pub fn served_index(html: &str, host: &str) -> String {
    let Some(start) = html.find("connect-src ") else {
        return html.to_owned();
    };
    let end = html[start..]
        .find([';', '"'])
        .map_or(html.len(), |e| start + e);
    format!(
        "{}connect-src 'self' ws://{host} wss://{host}{}",
        &html[..start],
        &html[end..]
    )
}

fn html_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
}

/// The text of the page's inline `<script>` elements (those without `src`), as written.
pub fn inline_scripts(html: &str) -> Vec<&str> {
    let bytes = html.as_bytes();
    let lower = html.to_ascii_lowercase(); // the same byte offsets
    let len = bytes.len();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find("<script") {
        let mut j = at + i + "<script".len();
        // `<scripts>` or `<script-x>` is another element
        if j < len && !(html_space(bytes[j]) || bytes[j] == b'>' || bytes[j] == b'/') {
            at = j;
            continue;
        }
        // the attributes, up to the tag's `>` (a quoted value may hold one)
        let mut has_src = false;
        loop {
            while j < len && (html_space(bytes[j]) || bytes[j] == b'/') {
                j += 1;
            }
            if j >= len {
                return out;
            }
            if bytes[j] == b'>' {
                j += 1;
                break;
            }
            let name = j;
            j += 1; // a name may start with any character but those skipped above
            while j < len && !(html_space(bytes[j]) || matches!(bytes[j], b'/' | b'>' | b'=')) {
                j += 1;
            }
            if &lower[name..j] == "src" {
                has_src = true;
            }
            let mut k = j;
            while k < len && html_space(bytes[k]) {
                k += 1;
            }
            if k < len && bytes[k] == b'=' {
                k += 1;
                while k < len && html_space(bytes[k]) {
                    k += 1;
                }
                if k < len && (bytes[k] == b'"' || bytes[k] == b'\'') {
                    let quote = bytes[k];
                    k += 1;
                    while k < len && bytes[k] != quote {
                        k += 1;
                    }
                    k = (k + 1).min(len);
                } else {
                    while k < len && !html_space(bytes[k]) && bytes[k] != b'>' {
                        k += 1;
                    }
                }
                j = k;
            }
        }
        let end = lower[j..].find("</script").map_or(len, |e| j + e);
        if !has_src {
            out.push(&html[j..end]);
        }
        at = end;
    }
    out
}

/// Standard base64 with padding (RFC 4648 §4), as CSP hashes are written.
pub fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(ABC[(n >> shift) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `'sha256-…'` of a script's text as the browser hashes it: after the HTML parser turned CR LF and
/// lone CR into LF, and NUL into U+FFFD, encoded as UTF-8.
pub fn script_hash(text: &str) -> String {
    let normal = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\0', "\u{FFFD}");
    format!("'sha256-{}'", base64(&Sha256::digest(normal.as_bytes())))
}

/// The page's `Content-Security-Policy` (what 0.3.4's page carried in its `<meta>`, and the
/// app's window has from Tauri): scripts from this server and the page's own inline ones (Nuxt's
/// import map and config, by their hashes), styles also inline, fonts and pictures also as `data:`,
/// connections to this server only (its WebSocket by the name the browser used), no `<base>`
/// elsewhere, never in a frame.
pub fn page_csp(html: &str, host: &str) -> String {
    let mut script = String::from("'self'");
    let mut seen = Vec::new();
    for text in inline_scripts(html) {
        let hash = script_hash(text);
        if !seen.contains(&hash) {
            script.push(' ');
            script.push_str(&hash);
            seen.push(hash);
        }
    }
    format!(
        "default-src 'self'; script-src {script}; style-src 'self' 'unsafe-inline'; \
         font-src 'self' data:; img-src 'self' data:; connect-src 'self' ws://{host} wss://{host}; \
         base-uri 'self'; frame-ancestors 'none'"
    )
}

fn header_str(h: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

/// Where a connection comes from (a request extension of [`serve_guarded`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Remote(pub SocketAddr);

/// A browser's WebSocket opened or closed (headless mode prints them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketEvent {
    Open,
    Close,
}

/// Called with [`SocketEvent`]s and the remote address as [`remote_label`] writes it.
pub type SocketHook = Arc<dyn Fn(SocketEvent, &str) + Send + Sync>;

/// A remote address as 0.3.4 printed it: the IP alone, IPv4 without its `::ffff:` form, IPv6 in
/// brackets.
pub fn remote_label(addr: SocketAddr) -> String {
    match addr.ip().to_canonical() {
        std::net::IpAddr::V4(v4) => v4.to_string(),
        std::net::IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

/// Says [`SocketEvent::Close`] when dropped: also when the session ends in a panic.
struct Announced(Option<(SocketHook, String)>);

impl Drop for Announced {
    fn drop(&mut self) {
        if let Some((hook, who)) = &self.0 {
            hook(SocketEvent::Close, who);
        }
    }
}

async fn ws_endpoint(
    State(server): State<Arc<Server>>,
    method: Method,
    headers: HeaderMap,
    remote: Option<Extension<Remote>>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if method != Method::GET {
        return plain(StatusCode::NOT_FOUND, "Not Found\n");
    }
    let headless = server.config.static_dir.is_some();
    if headless && ws.is_err() {
        // a page asking for the socket's address, as 0.3.4 answered it
        let mut r = plain(StatusCode::UPGRADE_REQUIRED, "Upgrade Required\n");
        r.headers_mut()
            .insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        return r;
    }
    if !ws_origin_ok(
        header_str(&headers, header::ORIGIN),
        header_str(&headers, header::HOST),
        headless,
        server.config.secure,
        &server.config.allowed_origins,
    ) {
        return plain(StatusCode::FORBIDDEN, "Forbidden\n");
    }
    let ws = match ws {
        Ok(ws) => ws,
        Err(e) => return e.into_response(),
    };
    let offered = headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|p| p.trim() == SUBPROTOCOL);
    if !offered {
        return (
            StatusCode::BAD_REQUEST,
            format!("subprotocol {SUBPROTOCOL:?} required\n"),
        )
            .into_response();
    }
    let hook = server
        .config
        .on_socket
        .clone()
        .zip(remote.map(|Extension(Remote(a))| remote_label(a)));
    let limit = server.config.max_inbound_bytes;
    ws.protocols([SUBPROTOCOL])
        .max_message_size(limit)
        .max_frame_size(limit)
        .on_upgrade(move |socket| async move {
            if let Some((h, who)) = &hook {
                h(SocketEvent::Open, who);
            }
            let _close = Announced(hook);
            crate::session::run(server, socket).await;
        })
}

#[derive(serde::Serialize)]
struct HeadlessInfo {
    platform: &'static str,
    version: String,
    hostname: String,
    home: String,
    secure: bool,
}

fn bearer_ok(h: &HeaderMap, token: &str) -> bool {
    let Some(v) = header_str(h, header::AUTHORIZATION) else {
        return false;
    };
    let mut parts = v.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some(scheme), Some(t), None) if scheme.eq_ignore_ascii_case("bearer") => {
            constant_time_eq(t.as_bytes(), token.as_bytes())
        }
        _ => false,
    }
}

/// This computer's name as the system tells it (Node's `os.hostname()`, which 0.3.4 showed):
/// `gethostname()` on Linux and macOS, the DNS host name on Windows; the environment and
/// `/etc/hostname` only when that fails.
pub fn hostname() -> String {
    if let Some(name) = imp::system_hostname().filter(|n| !n.is_empty()) {
        return name;
    }
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
        {
            return v;
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".to_owned())
}

#[cfg(unix)]
mod imp {
    pub fn system_hostname() -> Option<String> {
        let mut buf = vec![0u8; 256];
        // SAFETY: a writable buffer of the length passed.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
        if rc != 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).into_owned())
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::System::SystemInformation::{
        ComputerNameDnsHostname, GetComputerNameExW,
    };

    pub fn system_hostname() -> Option<String> {
        let mut size: u32 = 0;
        // SAFETY: a size query (null buffer) first, then a buffer of the size it asked for.
        unsafe {
            GetComputerNameExW(ComputerNameDnsHostname, std::ptr::null_mut(), &mut size);
            if size == 0 {
                return None;
            }
            let mut buf = vec![0u16; size as usize];
            if GetComputerNameExW(ComputerNameDnsHostname, buf.as_mut_ptr(), &mut size) == 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&buf[..size as usize]))
        }
    }
}

async fn info(State(server): State<Arc<Server>>, method: Method, headers: HeaderMap) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return plain(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed\n");
    }
    if !bearer_ok(&headers, &server.config.token) {
        let mut r = plain(StatusCode::UNAUTHORIZED, "Unauthorized\n");
        r.headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return r;
    }
    let body = HeadlessInfo {
        platform: crate::app::platform_name(),
        version: server
            .config
            .app_version
            .clone()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned()),
        hostname: hostname(),
        home: sk_core::paths::home_dir().display().to_string(),
        secure: server.config.secure,
    };
    let mut r = Response::new(Body::from(serde_json::to_vec(&body).unwrap_or_default()));
    secure_headers(r.headers_mut());
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// The file below `root` a URL path names (`/` → `index.html`); None for anything leaving it.
pub fn file_in(root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path)?;
    let rel = decoded.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let mut out = root.to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if rel.contains('\\') || rel.contains('\0') {
        return None;
    }
    Some(out)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

pub fn mime_of(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("wasm") => "application/wasm",
        Some("txt") | Some("md") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// How long a browser keeps a file of the interface. Nuxt's bundles (`/_nuxt/`, and 0.3.4's
/// `/assets/`) have their content's hash in their names: for good. Nuxt's build manifest
/// (`/_nuxt/builds/`) and the files at the top keep their names across versions: asked again each
/// time (cheaply, with the [`etag`]).
pub fn cache_control(url_path: &str) -> &'static str {
    let hashed = (url_path.starts_with("/_nuxt/") && !url_path.starts_with("/_nuxt/builds/"))
        || url_path.starts_with("/assets/");
    if hashed {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// A file's entity tag: from its content, so a rebuilt interface is never taken for the old one.
pub fn etag(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

/// Whether `If-None-Match` names `tag` (weak comparison, RFC 9110 §13.1.2) or is `*`.
pub fn none_match(if_none_match: &str, tag: &str) -> bool {
    if_none_match.split(',').map(str::trim).any(|t| {
        t == "*" || t.strip_prefix("W/").unwrap_or(t) == tag.strip_prefix("W/").unwrap_or(tag)
    })
}

async fn static_file(
    State(server): State<Arc<Server>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if method != Method::GET && method != Method::HEAD {
        let mut r = plain(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed\n");
        r.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return r;
    }
    let host = header_str(&headers, header::HOST).unwrap_or("");
    if !host_ok(host) {
        return plain(StatusCode::BAD_REQUEST, "Bad Request\n");
    }
    let Some(root) = &server.config.static_dir else {
        return plain(StatusCode::NOT_FOUND, "Not Found\n");
    };
    let Some(file) = file_in(root, uri.path()) else {
        return plain(StatusCode::NOT_FOUND, "Not Found\n");
    };
    let body = match tokio::fs::read(&file).await {
        Ok(b) => b,
        Err(_) => return plain(StatusCode::NOT_FOUND, "Not Found\n"),
    };
    let mime = mime_of(&file);
    let head = method == Method::HEAD;
    let mut r;
    if mime.starts_with("text/html") {
        // the page: always the current one, its policy for the name the browser used
        let html = served_index(&String::from_utf8_lossy(&body), host);
        let policy = HeaderValue::from_str(&page_csp(&html, host))
            .unwrap_or_else(|_| HeaderValue::from_static("frame-ancestors 'none'"));
        r = Response::new(if head {
            Body::empty()
        } else {
            Body::from(html)
        });
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        r.headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, policy);
    } else {
        let tag = etag(&body);
        let fresh =
            header_str(&headers, header::IF_NONE_MATCH).is_some_and(|v| none_match(v, &tag));
        r = Response::new(if head || fresh {
            Body::empty()
        } else {
            Body::from(body)
        });
        if fresh {
            *r.status_mut() = StatusCode::NOT_MODIFIED;
        }
        r.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control(uri.path())),
        );
        if let Ok(v) = HeaderValue::from_str(&tag) {
            r.headers_mut().insert(header::ETAG, v);
        }
    }
    secure_headers(r.headers_mut());
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    r
}

// ---------------------------------------------------------------------------------- TLS

/// Why the certificate or its key can't be used.
#[derive(Debug)]
pub enum TlsError {
    /// a file can't be read
    Read(PathBuf, io::Error),
    /// no PEM `CERTIFICATE` in the certificate file, or one that can't be decoded
    Certificate(PathBuf, String),
    /// a password-protected key (`ENCRYPTED PRIVATE KEY`, or OpenSSL's older `Proc-Type: 4,ENCRYPTED`)
    EncryptedKey(PathBuf),
    /// no PEM private key in the key file (DER, PKCS #12 and other formats are not read)
    NoKey(PathBuf),
    /// the key doesn't belong to the certificate, or the pair is refused
    Refused(String),
}

impl std::fmt::Display for TlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TlsError::Read(p, e) => write!(f, "cannot read {}: {e}", p.display()),
            TlsError::Certificate(p, e) => write!(f, "invalid certificate {}: {e}", p.display()),
            TlsError::EncryptedKey(p) => write!(
                f,
                "the private key {} is encrypted (password-protected): decrypt it first",
                p.display()
            ),
            TlsError::NoKey(p) => write!(
                f,
                "no PEM private key in {} (PKCS #8 \"PRIVATE KEY\", PKCS #1 \"RSA PRIVATE KEY\" or SEC1 \"EC PRIVATE KEY\")",
                p.display()
            ),
            TlsError::Refused(e) => write!(f, "the certificate and key can't be used: {e}"),
        }
    }
}

impl std::error::Error for TlsError {}

/// The private key of a PEM file: PKCS #8 (`PRIVATE KEY`), PKCS #1 (`RSA PRIVATE KEY`) or SEC1
/// (`EC PRIVATE KEY`), unencrypted (rustls; 0.3.4's Node read more, through OpenSSL).
pub fn read_key(path: &Path) -> Result<rustls_pki_types::PrivateKeyDer<'static>, TlsError> {
    use rustls_pki_types::PrivateKeyDer;
    use rustls_pki_types::pem::PemObject;
    let bytes = std::fs::read(path).map_err(|e| TlsError::Read(path.to_path_buf(), e))?;
    let text = String::from_utf8_lossy(&bytes);
    if text.contains("-----BEGIN ENCRYPTED PRIVATE KEY-----")
        || text.contains("Proc-Type: 4,ENCRYPTED")
    {
        return Err(TlsError::EncryptedKey(path.to_path_buf()));
    }
    PrivateKeyDer::from_pem_slice(&bytes).map_err(|_| TlsError::NoKey(path.to_path_buf()))
}

/// The certificate chain of a PEM file, the server's own first.
pub fn read_certificates(
    path: &Path,
) -> Result<Vec<rustls_pki_types::CertificateDer<'static>>, TlsError> {
    use rustls_pki_types::CertificateDer;
    use rustls_pki_types::pem::PemObject;
    let bytes = std::fs::read(path).map_err(|e| TlsError::Read(path.to_path_buf(), e))?;
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&bytes)
        .collect::<Result<_, _>>()
        .map_err(|e| TlsError::Certificate(path.to_path_buf(), e.to_string()))?;
    if certs.is_empty() {
        return Err(TlsError::Certificate(
            path.to_path_buf(),
            "no PEM \"CERTIFICATE\" in it".into(),
        ));
    }
    Ok(certs)
}

pub fn tls_acceptor(cert: &Path, key: &Path) -> Result<TlsAcceptor, TlsError> {
    let certs = read_certificates(cert)?;
    let key = read_key(key)?;
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = tokio_rustls::rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| TlsError::Refused(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| TlsError::Refused(e.to_string()))?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

// ------------------------------------------------------------------ serving beyond this computer

/// What [`serve_guarded`] allows: 0.3.4's headless server (`headless-server.ts`: 64 sockets,
/// `headersTimeout` 20 s, `requestTimeout` 30 s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// connections open at a time, pages and WebSockets together; more are turned away (503)
    pub max_sockets: usize,
    /// to send a request's headers, and how long a connection may sit idle between requests
    pub header_timeout: Duration,
    /// to answer a request
    pub request_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_sockets: 64,
            header_timeout: Duration::from_secs(20),
            request_timeout: Duration::from_secs(30),
        }
    }
}

/// The TLS handshake's time.
const TLS_HANDSHAKE: Duration = Duration::from_secs(10);

/// A connection's IO holding its place among [`Limits::max_sockets`] for as long as it lives (a
/// WebSocket takes the IO along after the upgrade).
struct Counted<IO> {
    io: IO,
    _slot: OwnedSemaphorePermit,
}

impl<IO: AsyncRead + Unpin> AsyncRead for Counted<IO> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl<IO: AsyncWrite + Unpin> AsyncWrite for Counted<IO> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }
}

/// Serve `app` on `listener` (with TLS when `tls`) until `shutdown`, within `limits`. Each request
/// carries its [`Remote`].
pub(crate) async fn serve_guarded(
    listener: TcpListener,
    app: HttpRouter,
    tls: Option<TlsAcceptor>,
    limits: Limits,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    let slots = Arc::new(Semaphore::new(limits.max_sockets));
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut conns = tokio::task::JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
            accepted = listener.accept() => {
                let (tcp, addr) = match accepted {
                    Ok(x) => x,
                    Err(e) => {
                        // out of file descriptors and the like: try again shortly
                        tracing::debug!("accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let Ok(slot) = slots.clone().try_acquire_owned() else {
                    tracing::debug!("{addr} turned away: {} connections open", limits.max_sockets);
                    refuse(tcp, tls.is_none());
                    continue;
                };
                let _ = tcp.set_nodelay(true);
                conns.spawn(connection(tcp, addr, slot, tls.clone(), app.clone(), limits, stop_rx.clone()));
            }
        }
    }
    drop(listener);
    let _ = stop_tx.send(true);
    // requests under way finish (each within its timeout), idle connections close at once
    let drained = tokio::time::timeout(Duration::from_secs(5), async {
        while conns.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        conns.abort_all();
    }
}

/// One connection over the limit: told 503 when that fits in the socket's buffer (plain HTTP),
/// then closed.
fn refuse(tcp: TcpStream, plain_http: bool) {
    if plain_http {
        let _ = tcp.try_write(
            b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        );
    }
    drop(tcp);
}

async fn connection(
    tcp: TcpStream,
    addr: SocketAddr,
    slot: OwnedSemaphorePermit,
    tls: Option<TlsAcceptor>,
    app: HttpRouter,
    limits: Limits,
    stop: watch::Receiver<bool>,
) {
    match tls {
        None => {
            serve_connection(
                Counted {
                    io: tcp,
                    _slot: slot,
                },
                addr,
                app,
                limits,
                stop,
            )
            .await;
        }
        Some(acceptor) => match tokio::time::timeout(TLS_HANDSHAKE, acceptor.accept(tcp)).await {
            Ok(Ok(stream)) => {
                serve_connection(
                    Counted {
                        io: stream,
                        _slot: slot,
                    },
                    addr,
                    app,
                    limits,
                    stop,
                )
                .await;
            }
            Ok(Err(e)) => tracing::debug!("TLS handshake with {addr} failed: {e}"),
            Err(_) => tracing::debug!("TLS handshake with {addr} timed out"),
        },
    }
}

async fn serve_connection<IO>(
    io: IO,
    addr: SocketAddr,
    app: HttpRouter,
    limits: Limits,
    mut stop: watch::Receiver<bool>,
) where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service =
        hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
            req.extensions_mut().insert(Remote(addr));
            let app = app.clone();
            async move {
                Ok::<_, Infallible>(answer(app, req.map(Body::new), limits.request_timeout).await)
            }
        });
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(limits.header_timeout);
    let conn = builder
        .serve_connection(hyper_util::rt::TokioIo::new(io), service)
        .with_upgrades();
    tokio::pin!(conn);
    tokio::select! {
        served = conn.as_mut() => {
            if let Err(e) = served {
                tracing::debug!("connection with {addr}: {e}");
            }
        }
        // (the guard `wait_for` returns is dropped in there, before the connection is awaited)
        _ = async { let _ = stop.wait_for(|stop| *stop).await; } => {
            conn.as_mut().graceful_shutdown();
            let _ = conn.await;
        }
    }
}

/// `service`'s answer to `req`, or 408 when it takes longer than `limit`.
pub async fn answer<S>(service: S, req: axum::extract::Request, limit: Duration) -> Response
where
    S: tower::Service<axum::extract::Request, Response = Response, Error = Infallible>,
{
    use tower::ServiceExt as _;
    match tokio::time::timeout(limit, service.oneshot(req)).await {
        Ok(Ok(r)) => r,
        Ok(Err(never)) => match never {},
        Err(_) => plain(StatusCode::REQUEST_TIMEOUT, "Request Timeout\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_origins() {
        assert!(host_ok("localhost:8080"));
        assert!(host_ok("192.168.1.2"));
        assert!(host_ok("[::1]:443"));
        assert!(!host_ok("evil.com/x"));
        assert!(!host_ok("a:b"));
        assert!(!host_ok(""));
        assert!(same_origin(
            Some("http://box:8080"),
            Some("box:8080"),
            false
        ));
        assert!(!same_origin(
            Some("https://box:8080"),
            Some("box:8080"),
            false
        ));
        assert!(!same_origin(
            Some("http://evil:8080"),
            Some("box:8080"),
            false
        ));
        assert!(!same_origin(None, Some("box"), false));
    }

    #[test]
    fn index_policy_and_paths() {
        let html = r#"<meta http-equiv="Content-Security-Policy" content="default-src 'self'; connect-src 'self' ws://127.0.0.1:*; img-src 'self'">"#;
        let out = served_index(html, "box:8080");
        assert!(out.contains("connect-src 'self' ws://box:8080 wss://box:8080; img-src"));
        let root = Path::new("/srv/ui");
        assert_eq!(file_in(root, "/"), Some(root.join("index.html")));
        assert_eq!(
            file_in(root, "/assets/a.js"),
            Some(root.join("assets").join("a.js"))
        );
        assert_eq!(file_in(root, "/../secret"), None);
        assert_eq!(file_in(root, "/%2e%2e/secret"), None);
    }

    #[test]
    fn websocket_origins() {
        let app: Vec<String> = crate::app::DEFAULT_ORIGINS
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let host = Some("box:8375");
        // headless: the page's own origin, or one let in by name; never none at all
        assert!(ws_origin_ok(
            Some("http://box:8375"),
            host,
            true,
            false,
            &[]
        ));
        assert!(ws_origin_ok(
            Some("HTTP://BOX:8375"),
            host,
            true,
            false,
            &[]
        ));
        assert!(!ws_origin_ok(
            Some("https://box:8375"),
            host,
            true,
            false,
            &[]
        ));
        assert!(ws_origin_ok(
            Some("https://box:8375"),
            host,
            true,
            true,
            &[]
        ));
        assert!(!ws_origin_ok(None, host, true, false, &app));
        assert!(!ws_origin_ok(
            Some("http://evil.example"),
            host,
            true,
            false,
            &app
        ));
        assert!(ws_origin_ok(
            Some("tauri://localhost"),
            host,
            true,
            false,
            &app
        ));
        // on a network address the app's own origins aren't let in (crate::app::allowed_origins)
        assert!(!ws_origin_ok(
            Some("tauri://localhost"),
            host,
            true,
            false,
            &[]
        ));
        assert!(!ws_origin_ok(
            Some("app://spaceknife"),
            host,
            false,
            false,
            &[]
        ));
        // the app's backend: by name only, and native clients without one
        assert!(ws_origin_ok(None, host, false, false, &app));
        assert!(ws_origin_ok(
            Some("app://spaceknife"),
            host,
            false,
            false,
            &app
        ));
        assert!(!ws_origin_ok(
            Some("http://box:8375"),
            host,
            false,
            false,
            &app
        ));
        let dev = vec!["http://localhost:3000".to_owned()];
        assert!(ws_origin_ok(
            Some("http://localhost:3000"),
            host,
            false,
            false,
            &dev
        ));
    }

    #[test]
    fn base64_is_rfc_4648() {
        for (raw, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(raw.as_bytes()), enc);
        }
        assert_eq!(base64(&[0xfb, 0xff, 0xbf]), "+/+/");
    }

    #[test]
    fn inline_scripts_of_a_page() {
        let html = concat!(
            "<html><head><script type=\"importmap\">{\"a\":1}</script>",
            "<SCRIPT SRC='/x.js'></SCRIPT><script data-src=\"no\">one()</script>",
            "<scripts>not a script</scripts><script data-x=\"a>b\" defer>two()</script>",
            "<script src=/y.js async></script><script\n>three()</script >",
            "<noscript>x</noscript><script>unterminated",
        );
        assert_eq!(
            inline_scripts(html),
            ["{\"a\":1}", "one()", "two()", "three()", "unterminated"]
        );
        assert!(inline_scripts("<p>no scripts</p>").is_empty());
        assert!(inline_scripts("<script src=\"a.js\" ").is_empty());
        assert_eq!(inline_scripts("<script>中文</script>"), ["中文"]);
    }

    #[test]
    fn script_hashes_follow_the_html_parser() {
        // printf 'a\nb' | openssl dgst -sha256 -binary | base64
        let lf = "'sha256-fhj3NzEbLcOy8mndeDlrA1HxT7Zu+oefdoyyMYGIPHg='";
        assert_eq!(script_hash("a\nb"), lf);
        assert_eq!(script_hash("a\r\nb"), lf);
        assert_eq!(script_hash("a\rb"), lf);
    }

    #[test]
    fn page_policy() {
        let html = "<script>x()</script><script src=/a.js></script><script>x()</script><script>y()</script>";
        let csp = page_csp(html, "[::1]:8375");
        assert_eq!(
            csp,
            format!(
                "default-src 'self'; script-src 'self' {} {}; style-src 'self' 'unsafe-inline'; \
                 font-src 'self' data:; img-src 'self' data:; connect-src 'self' ws://[::1]:8375 wss://[::1]:8375; \
                 base-uri 'self'; frame-ancestors 'none'",
                script_hash("x()"),
                script_hash("y()")
            )
        );
        assert!(HeaderValue::from_str(&csp).is_ok());
    }

    #[test]
    fn caching_and_entity_tags() {
        assert_eq!(
            cache_control("/_nuxt/CLVEf1ON.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            cache_control("/_nuxt/entry.CdKPIdRF.css"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            cache_control("/assets/index-abc.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(cache_control("/_nuxt/builds/latest.json"), "no-cache");
        assert_eq!(cache_control("/_nuxt/builds/meta/x.json"), "no-cache");
        assert_eq!(cache_control("/theme-boot.js"), "no-cache");
        assert_eq!(cache_control("/_nuxtx.js"), "no-cache");
        let tag = etag(b"one");
        assert_eq!(tag.len(), 34);
        assert_ne!(tag, etag(b"two"));
        assert!(none_match(&tag, &tag));
        assert!(none_match(&format!("W/{tag}"), &tag));
        assert!(none_match(&format!("\"a\", {tag} , \"b\""), &tag));
        assert!(none_match("*", &tag));
        assert!(!none_match("\"a\"", &tag));
        assert!(!none_match("", &tag));
    }

    #[test]
    fn remote_addresses_as_0_3_4_printed_them() {
        assert_eq!(
            remote_label("192.168.1.5:5000".parse().unwrap()),
            "192.168.1.5"
        );
        assert_eq!(
            remote_label("[::ffff:10.0.0.2]:1".parse().unwrap()),
            "10.0.0.2"
        );
        assert_eq!(remote_label("[fd00::5]:1".parse().unwrap()), "[fd00::5]");
    }

    /// The name `hostname` prints: what 0.3.4 showed (Node's os.hostname()). Not "localhost" on
    /// macOS, not the upper-case NetBIOS name on Windows.
    #[test]
    fn the_host_name_is_the_systems() {
        let Ok(out) = std::process::Command::new("hostname").output() else {
            return; // a system without the command
        };
        let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !out.status.success() || name.is_empty() {
            return;
        }
        assert_eq!(hostname(), name);
    }

    fn pem(label: &str, body: &str) -> String {
        format!("-----BEGIN {label}-----\n{body}\n-----END {label}-----\n")
    }

    #[test]
    fn key_formats() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, text).unwrap();
            p
        };
        // what rustls reads: PKCS #8, PKCS #1, SEC1 in PEM
        for label in ["PRIVATE KEY", "RSA PRIVATE KEY", "EC PRIVATE KEY"] {
            let p = write("k.pem", &pem(label, "AAAA"));
            assert!(read_key(&p).is_ok(), "{label}");
        }
        // what it doesn't: said plainly
        let p = write("enc.pem", &pem("ENCRYPTED PRIVATE KEY", "AAAA"));
        assert!(matches!(read_key(&p), Err(TlsError::EncryptedKey(_))));
        let old = "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,00\n\nAAAA\n-----END RSA PRIVATE KEY-----\n";
        let p = write("old.pem", old);
        assert!(matches!(read_key(&p), Err(TlsError::EncryptedKey(_))));
        let p = write("cert-only.pem", &pem("CERTIFICATE", "AAAA"));
        assert!(matches!(read_key(&p), Err(TlsError::NoKey(_))));
        let p = write("der.key", "\u{30}\u{82}binary");
        let e = read_key(&p).unwrap_err();
        assert!(e.to_string().contains("PKCS #8"), "{e}");
        assert!(matches!(
            read_key(&dir.path().join("missing.pem")),
            Err(TlsError::Read(..))
        ));
        let p = write("no-cert.pem", &pem("PRIVATE KEY", "AAAA"));
        assert!(matches!(
            read_certificates(&p),
            Err(TlsError::Certificate(..))
        ));
        let p = write("cert.pem", &pem("CERTIFICATE", "AAAA"));
        assert_eq!(read_certificates(&p).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_slow_answer_ends_in_408() {
        let slow = tower::service_fn(|_req: axum::extract::Request| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok::<_, Infallible>(Response::new(Body::empty()))
        });
        let started = std::time::Instant::now();
        let r = answer(
            slow,
            axum::extract::Request::new(Body::empty()),
            Duration::from_millis(100),
        )
        .await;
        assert_eq!(r.status(), StatusCode::REQUEST_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(2));
        let quick = tower::service_fn(|_req: axum::extract::Request| async {
            Ok::<_, Infallible>(Response::new(Body::empty()))
        });
        let r = answer(
            quick,
            axum::extract::Request::new(Body::empty()),
            Duration::from_millis(100),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
    }
}
