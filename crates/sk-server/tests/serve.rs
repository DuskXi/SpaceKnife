//! Port of tests/test_serve_process.py as in-process tests of the `start` contract (the stdin /
//! ready-line plumbing of `spaceknife-cli serve` is the cli's), plus headless mode (the Electron
//! `headless-server.ts` behaviour, now served directly).

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use sk_server::ServeOptions;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[tokio::test]
async fn start_returns_once_listening_and_shutdown_ends_it() {
    let srv = server().await;
    assert!(srv.url.starts_with("ws://127.0.0.1:"));
    assert!(srv.url.ends_with("/v1"));
    assert_eq!(srv.url, format!("ws://127.0.0.1:{}/v1", srv.port));
    assert_eq!(srv.pid, std::process::id());
    let c = Client::open(&srv.url).await;
    assert_eq!(
        c.ok("system.ping", json!({"payload": "e2e"})).await["payload"],
        "e2e"
    );
    srv.shutdown();
    assert_eq!(
        c.closed(5.0).await,
        Some(1001),
        "sessions are told the server goes away"
    );
    tokio::time::timeout(Duration::from_secs(10), srv.wait())
        .await
        .expect("stopped");
}

#[tokio::test]
async fn generated_token_and_app_shutdown() {
    let opts = ServeOptions::default();
    let token = opts.token.clone();
    assert!(token.len() >= 32);
    let srv = server_with(opts, |_| {}).await;
    let c = Client::open_with(&srv.url, &token, all_caps(), None)
        .await
        .unwrap();
    assert_eq!(c.ok("app.shutdown", json!({})).await["accepted"], true);
    tokio::time::timeout(Duration::from_secs(10), srv.wait())
        .await
        .expect("stopped");
}

#[tokio::test]
async fn short_tokens_are_refused() {
    init();
    let r = sk_server::start(ServeOptions {
        token: "short".into(),
        ..ServeOptions::default()
    })
    .await;
    assert!(r.is_err());
}

#[tokio::test]
async fn threaded_scan_in_a_started_server() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let tmp = tempfile::tempdir().unwrap();
    let facts = build_tree(&tmp.path().join("root"));
    let st = c
        .ok(
            "scan.start",
            json!({"roots": [facts.root], "workers": 2, "auto_snapshot": false}),
        )
        .await;
    let status = wait_done(&c, st["scan_id"].as_str().unwrap()).await;
    assert_eq!(status["state"], "done");
    assert_eq!(status["files"], facts.files);
    srv.shutdown();
    tokio::time::timeout(Duration::from_secs(10), srv.wait())
        .await
        .expect("stopped");
}

#[tokio::test]
async fn headless_serves_the_interface_info_and_websocket() {
    let ui = tempfile::tempdir().unwrap();
    std::fs::write(
        ui.path().join("index.html"),
        r#"<html><head><meta http-equiv="Content-Security-Policy" content="default-src 'self'; connect-src 'self' ws://127.0.0.1:*; img-src 'self'"></head></html>"#,
    )
    .unwrap();
    std::fs::create_dir(ui.path().join("assets")).unwrap();
    std::fs::write(
        ui.path().join("assets").join("index-abc.js"),
        "console.log(1)",
    )
    .unwrap();
    let opts = ServeOptions {
        token: TOKEN.to_owned(),
        static_dir: Some(ui.path().to_path_buf()),
        app_version: Some("9.9.9".into()),
        ..ServeOptions::default()
    };
    let srv = server_with(opts, |_| {}).await;
    let port = srv.port;
    let host = format!("127.0.0.1:{port}");

    // the page: public, its CSP lets it reach this server's WebSocket, never cached or framed
    let (status, head, body) = http(port, "/", &[]).await;
    assert_eq!(status, 200);
    let html = String::from_utf8(body).unwrap();
    assert!(
        html.contains(&format!(
            "connect-src 'self' ws://{host} wss://{host}; img-src"
        )),
        "{html}"
    );
    assert!(head.contains("cache-control: no-store"));
    assert!(
        header(&head, "content-security-policy")
            .is_some_and(|csp| csp.ends_with("; frame-ancestors 'none'")),
        "{head}"
    );
    assert!(head.contains("x-frame-options: deny"));
    let (status, head, body) = http(port, "/assets/index-abc.js", &[]).await;
    assert_eq!(status, 200);
    assert_eq!(body, b"console.log(1)");
    assert!(head.contains("immutable"));
    assert_eq!(http(port, "/nope.js", &[]).await.0, 404);
    assert_eq!(http(port, "/../Cargo.toml", &[]).await.0, 404);
    assert_eq!(http(port, "/", &[("Host", "evil host")]).await.0, 400);

    // what the page needs before it connects: only with the token
    let (status, head, _) = http(port, "/sk/info", &[]).await;
    assert_eq!(status, 401);
    assert!(head.contains("www-authenticate: bearer"));
    assert_eq!(
        http(
            port,
            "/sk/info",
            &[("Authorization", "Bearer wrong-token-0123456789")]
        )
        .await
        .0,
        401
    );
    let bearer = format!("Bearer {TOKEN}");
    let (status, head, body) = http(port, "/sk/info", &[("Authorization", &bearer)]).await;
    assert_eq!(status, 200);
    assert!(head.contains("no-store"));
    let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(info["version"], "9.9.9");
    assert_eq!(info["secure"], false);
    assert!(["win32", "darwin", "linux"].contains(&info["platform"].as_str().unwrap()));
    assert!(!info["home"].as_str().unwrap().is_empty());

    // /v1 without an upgrade
    assert_eq!(http(port, "/v1", &[]).await.0, 426);

    // the browser's Origin must be the page's own; the session still needs the token
    let own = format!("http://{host}");
    let c = Client::open_with(&srv.url, TOKEN, all_caps(), Some(&own))
        .await
        .unwrap();
    assert_eq!(
        c.ok("system.ping", json!({"payload": "browser"})).await["payload"],
        "browser"
    );
    let e = raw(&srv.url, Some("http://evil.example"), true)
        .await
        .err()
        .unwrap();
    match e {
        tokio_tungstenite::tungstenite::Error::Http(r) => assert_eq!(r.status().as_u16(), 403),
        other => panic!("{other}"),
    }
    srv.shutdown();
}

/// A built interface as Nuxt generates it: inline scripts (an import map, the app's config, a
/// JSON data block), an external one, a bundle folder with hashed names and the build manifest.
fn nuxt_ui() -> tempfile::TempDir {
    let ui = tempfile::tempdir().unwrap();
    std::fs::write(
        ui.path().join("index.html"),
        concat!(
            r##"<!DOCTYPE html><html><head><script type="importmap">{"imports":{"#entry":"/_nuxt/e.js"}}</script>"##,
            r#"<script src="/theme-boot.js"></script><script type="module" src="/_nuxt/e.js" crossorigin></script></head>"#,
            r#"<body><div id="__nuxt"></div><script>window.__NUXT__={};</script>"#,
            r#"<script type="application/json" id="__NUXT_DATA__">a"#,
            "\r\n",
            r#"b</script></body></html>"#,
        ),
    )
    .unwrap();
    std::fs::write(ui.path().join("theme-boot.js"), "/* theme */").unwrap();
    std::fs::create_dir_all(ui.path().join("_nuxt").join("builds")).unwrap();
    std::fs::write(ui.path().join("_nuxt").join("e.js"), "console.log(1)").unwrap();
    std::fs::write(
        ui.path().join("_nuxt").join("builds").join("latest.json"),
        r#"{"id":"x"}"#,
    )
    .unwrap();
    ui
}

async fn headless_server(ui: &std::path::Path) -> sk_server::ServerHandle {
    server_with(
        ServeOptions {
            token: TOKEN.to_owned(),
            static_dir: Some(ui.to_path_buf()),
            ..ServeOptions::default()
        },
        |_| {},
    )
    .await
}

/// `GET /` on a fresh connection; the answer as text ("" when the server closed it unanswered).
async fn get_page(port: u16) -> String {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let _ = s
        .write_all(
            format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await;
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf))
        .await
        .expect("answered or closed in time");
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn headless_page_policy_lets_only_its_own_inline_scripts_run() {
    let ui = nuxt_ui();
    let srv = headless_server(ui.path()).await;
    let port = srv.port;
    let (status, head, _) = http(port, "/", &[("Host", &format!("box.lan:{port}"))]).await;
    assert_eq!(status, 200);
    let csp = header(&head, "content-security-policy").expect("a policy header");
    let directives: Vec<&str> = csp.split(';').map(str::trim).collect();
    // the hashes are the browser's: sha256 of each inline script's text, CR LF read as LF
    // (`head` is in lower case, so are they here)
    let script = directives
        .iter()
        .find(|d| d.starts_with("script-src "))
        .expect("script-src");
    for hash in [
        "'sha256-6GSog8QLVHQCT/W+GEjD0XuzaGpZgv9kUPmnRSRNHsI='", // the import map
        "'sha256-UZWx7PcANLfxqg+tuiEQiu6bijg7tUWbRmoVtInX1K4='", // window.__NUXT__
        "'sha256-fhj3NzEbLcOy8mndeDlrA1HxT7Zu+oefdoyyMYGIPHg='", // the data block (a CR LF b)
    ] {
        assert!(
            script.contains(&hash.to_ascii_lowercase()),
            "{hash} in {script}"
        );
    }
    assert!(script.starts_with("script-src 'self' "), "{script}");
    assert!(!script.contains("unsafe"), "{script}");
    assert!(directives.contains(&"default-src 'self'"), "{csp}");
    let connect = format!("connect-src 'self' ws://box.lan:{port} wss://box.lan:{port}");
    assert!(directives.contains(&connect.as_str()), "{csp}");
    assert!(directives.contains(&"frame-ancestors 'none'"), "{csp}");
    assert!(
        directives.contains(&"style-src 'self' 'unsafe-inline'"),
        "{csp}"
    );
    srv.shutdown();
}

#[tokio::test]
async fn headless_websocket_needs_the_pages_origin() {
    let ui = nuxt_ui();
    let srv = headless_server(ui.path()).await;
    // a program without an Origin (0.3.4's server forwarded only its own page's)
    match raw(&srv.url, None, true).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            assert_eq!(r.status().as_u16(), 403)
        }
        other => panic!("no Origin was let in: {:?}", other.map(|_| ())),
    }
    // the page's own still works, as do the app's own windows on this computer
    let own = format!("http://127.0.0.1:{}", srv.port);
    assert!(raw(&srv.url, Some(&own), true).await.is_ok());
    assert!(raw(&srv.url, Some("tauri://localhost"), true).await.is_ok());
    srv.shutdown();
}

#[tokio::test]
async fn headless_bundles_are_cached_for_good_and_the_rest_revalidated() {
    let ui = nuxt_ui();
    let srv = headless_server(ui.path()).await;
    let port = srv.port;
    // Nuxt's bundles have hashed names: kept for good
    let (status, head, body) = http(port, "/_nuxt/e.js", &[]).await;
    assert_eq!((status, body.as_slice()), (200, &b"console.log(1)"[..]));
    assert_eq!(
        header(&head, "cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    let etag = header(&head, "etag").expect("an etag").to_owned();
    // the build manifest and the files at the top keep their names: asked again each time, and
    // answered with 304 when nothing changed
    for path in ["/_nuxt/builds/latest.json", "/theme-boot.js"] {
        let (status, head, _) = http(port, path, &[]).await;
        assert_eq!(status, 200, "{path}");
        assert_eq!(header(&head, "cache-control"), Some("no-cache"), "{path}");
        let tag = header(&head, "etag").expect("an etag").to_owned();
        let (status, head, body) = http(port, path, &[("If-None-Match", &tag)]).await;
        assert_eq!(status, 304, "{path}");
        assert!(body.is_empty());
        assert_eq!(header(&head, "etag"), Some(tag.as_str()));
        assert!(head.contains("x-content-type-options: nosniff"));
    }
    let either = format!("\"other\", {etag}");
    assert_eq!(
        http(port, "/_nuxt/e.js", &[("If-None-Match", &either)])
            .await
            .0,
        304
    );
    assert_eq!(
        http(port, "/_nuxt/e.js", &[("If-None-Match", "\"other\"")])
            .await
            .0,
        200
    );
    // the page itself is never kept
    let (_, head, _) = http(port, "/", &[]).await;
    assert_eq!(header(&head, "cache-control"), Some("no-store"));
    srv.shutdown();
}

#[tokio::test]
async fn headless_takes_at_most_64_connections() {
    let ui = nuxt_ui();
    let srv = headless_server(ui.path()).await;
    let port = srv.port;
    let mut held = Vec::new();
    for _ in 0..64 {
        held.push(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap(),
        );
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    // one more is turned away at once, with 503 when it can still be told
    let answer = get_page(port).await;
    assert!(
        answer.is_empty() || answer.starts_with("HTTP/1.1 503"),
        "{answer}"
    );
    // once one goes, a new one is served
    drop(held.pop());
    let mut served = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if get_page(port).await.starts_with("HTTP/1.1 200") {
            served = true;
            break;
        }
    }
    assert!(served);
    drop(held);
    srv.shutdown();
}

/// Read until the server closes the connection; None if it is still open after `secs`.
async fn closed_within(s: &mut tokio::net::TcpStream, secs: u64) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(secs), s.read_to_end(&mut buf))
        .await
        .ok()
        .map(|_| buf)
}

/// 0.3.4's headersTimeout: a client that doesn't finish its request headers in time, or sits on
/// an idle connection, is let go (slow clients can't hold the 64 places).
#[tokio::test]
async fn headless_lets_go_of_slow_and_idle_connections() {
    let ui = nuxt_ui();
    let srv = server_tuned(
        ServeOptions {
            token: TOKEN.to_owned(),
            static_dir: Some(ui.path().to_path_buf()),
            ..ServeOptions::default()
        },
        |t| t.limits.header_timeout = Duration::from_millis(300),
    )
    .await;
    let port = srv.port;
    // nothing sent at all
    let mut silent = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    assert!(closed_within(&mut silent, 5).await.is_some());
    // headers that never end
    let mut slow = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    slow.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Slow: ")
        .await
        .unwrap();
    let answer = closed_within(&mut slow, 5).await.expect("closed");
    assert!(!String::from_utf8_lossy(&answer).contains(" 200 "));
    // one answered request, then idle
    let mut idle = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    idle.write_all(
        format!("GET /theme-boot.js HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let answer = closed_within(&mut idle, 5).await.expect("closed once idle");
    assert!(answer.starts_with(b"HTTP/1.1 200"));
    srv.shutdown();
}

/// The test certificate (tests/tls: a leaf for localhost / 127.0.0.1 signed by a test CA, made
/// with openssl for these tests only).
fn tls_file(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("tls")
        .join(name)
}

/// A request over HTTPS to 127.0.0.1:`port`, trusting the test CA: the raw answer.
async fn https(port: u16, request: String) -> String {
    use rustls_pki_types::pem::PemObject as _;
    use tokio_rustls::rustls;
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pki_types::CertificateDer::pem_file_iter(tls_file("ca.pem")).unwrap() {
        roots.add(cert.unwrap()).unwrap();
    }
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config));
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let mut tls = connector.connect(name, tcp).await.expect("TLS handshake");
    tls.write_all(request.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    // the server may close without close_notify after `Connection: close`: what came is enough
    let _ = tokio::time::timeout(Duration::from_secs(10), tls.read_to_end(&mut buf)).await;
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn headless_over_https() {
    let ui = nuxt_ui();
    let srv = server_with(
        ServeOptions {
            token: TOKEN.to_owned(),
            static_dir: Some(ui.path().to_path_buf()),
            tls: Some((tls_file("cert.pem"), tls_file("key.pem"))),
            ..ServeOptions::default()
        },
        |_| {},
    )
    .await;
    let port = srv.port;
    assert!(srv.url.starts_with("wss://"), "{}", srv.url);
    let page = https(
        port,
        format!("GET / HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    let lower = page.to_ascii_lowercase();
    assert!(
        lower.contains(&format!("wss://localhost:{port}; base-uri 'self'")),
        "{page}"
    );
    // the WebSocket upgrade over TLS, from the page's own https origin
    let upgrade = https(
        port,
        format!(
            "GET /v1 HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: https://localhost:{port}\r\n\
             Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: spaceknife.v1\r\n\r\n"
        ),
    )
    .await;
    assert!(upgrade.starts_with("HTTP/1.1 101"), "{upgrade}");
    // plain HTTP on the HTTPS port gets nothing
    let plain = get_page(port).await;
    assert!(!plain.contains(" 200 "), "{plain}");
    srv.shutdown();
}

/// The app's own backend on loopback is served as before: no Origin needed (a native client),
/// no limit on connections.
#[tokio::test]
async fn the_apps_backend_is_not_guarded() {
    let srv = server_tuned(
        ServeOptions {
            token: TOKEN.to_owned(),
            ..ServeOptions::default()
        },
        |t| t.limits.max_sockets = 1,
    )
    .await;
    let a = raw(&srv.url, None, true).await.expect("no Origin");
    let b = raw(&srv.url, Some("tauri://localhost"), true)
        .await
        .expect("a second socket");
    drop((a, b));
    srv.shutdown();
}

#[tokio::test]
async fn without_a_static_dir_nothing_else_is_served() {
    let srv = server().await;
    assert_eq!(http(srv.port, "/", &[]).await.0, 404);
    assert_eq!(
        http(
            srv.port,
            "/sk/info",
            &[("Authorization", &format!("Bearer {TOKEN}"))]
        )
        .await
        .0,
        404
    );
    srv.shutdown();
}
