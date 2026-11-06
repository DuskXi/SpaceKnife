//! The HTTP side: `/v1` (WebSocket upgrade with path, origin and subprotocol checks) and, in
//! headless mode, the interface itself (the Electron `headless-server.ts`, served directly):
//!
//! | path | |
//! |---|---|
//! | `GET /`, `/assets/…` | the built interface, public; `index.html` with its CSP `connect-src` set to this server's own `ws://` / `wss://` address, `no-store`, `frame-ancestors 'none'` |
//! | `GET /sk/info` | `{platform, version, hostname, home, secure}`, only with `Authorization: Bearer <token>` |
//! | `GET /v1` | the WebSocket; a browser's `Origin` must be `<scheme>://<Host>` (its own page) |

use std::io;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::Router as HttpRouter;
use axum::body::Body;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use sk_protocol::{SUBPROTOCOL, WS_PATH};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

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

/// `index.html` with a CSP letting the page reach this server's WebSocket by the name it used.
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

fn header_str(h: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

async fn ws_endpoint(
    State(server): State<Arc<Server>>,
    method: Method,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if method != Method::GET {
        return plain(StatusCode::NOT_FOUND, "Not Found\n");
    }
    let origin = header_str(&headers, header::ORIGIN);
    let allowed = match origin {
        None => true,
        Some(o) => {
            server.config.allowed_origins.iter().any(|a| a == o)
                || (server.config.static_dir.is_some()
                    && same_origin(
                        Some(o),
                        header_str(&headers, header::HOST),
                        server.config.secure,
                    ))
        }
    };
    if !allowed {
        return plain(StatusCode::FORBIDDEN, "Forbidden\n");
    }
    let ws = match ws {
        Ok(ws) => ws,
        Err(_) if server.config.static_dir.is_some() => {
            let mut r = plain(StatusCode::UPGRADE_REQUIRED, "Upgrade Required\n");
            r.headers_mut()
                .insert(header::UPGRADE, HeaderValue::from_static("websocket"));
            return r;
        }
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
    let limit = server.config.max_inbound_bytes;
    ws.protocols([SUBPROTOCOL])
        .max_message_size(limit)
        .max_frame_size(limit)
        .on_upgrade(move |socket| crate::session::run(server, socket))
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

pub fn hostname() -> String {
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
        {
            return v;
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| "localhost".to_owned())
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
        let html = served_index(&String::from_utf8_lossy(&body), host);
        r = Response::new(if head {
            Body::empty()
        } else {
            Body::from(html)
        });
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        r.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors 'none'"),
        );
    } else {
        r = Response::new(if head {
            Body::empty()
        } else {
            Body::from(body)
        });
        let cache = if uri.path().starts_with("/assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    }
    secure_headers(r.headers_mut());
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    r
}

// ---------------------------------------------------------------------------------- TLS

pub(crate) fn tls_acceptor(cert: &Path, key: &Path) -> anyhow::Result<TlsAcceptor> {
    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .map_err(|e| anyhow::anyhow!("cannot read the certificate {}: {e}", cert.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("invalid certificate {}: {e}", cert.display()))?;
    let key = PrivateKeyDer::from_pem_file(key)
        .map_err(|e| anyhow::anyhow!("invalid key {}: {e}", key.display()))?;
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = tokio_rustls::rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// A listener that completes TLS handshakes in the background and hands out finished streams.
pub(crate) struct TlsListener {
    rx: tokio::sync::mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
}

impl TlsListener {
    pub(crate) fn new(listener: TcpListener, acceptor: TlsAcceptor) -> TlsListener {
        let local = listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 0)));
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            loop {
                let (tcp, addr) = match listener.accept().await {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::debug!("accept failed: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let acceptor = acceptor.clone();
                let sender = tx.clone();
                tokio::spawn(async move {
                    let hs = tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        acceptor.accept(tcp),
                    )
                    .await;
                    match hs {
                        Ok(Ok(stream)) => {
                            let _ = sender.send((stream, addr)).await;
                        }
                        Ok(Err(e)) => tracing::debug!("TLS handshake with {addr} failed: {e}"),
                        Err(_) => tracing::debug!("TLS handshake with {addr} timed out"),
                    }
                });
                if tx.is_closed() {
                    break;
                }
            }
        });
        TlsListener { rx, local }
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(x) => x,
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
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
}
