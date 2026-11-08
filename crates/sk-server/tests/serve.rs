//! Port of tests/test_serve_process.py as in-process tests of the `start` contract (the stdin /
//! ready-line plumbing of `spaceknife-cli serve` is the cli's), plus headless mode (the Electron
//! `headless-server.ts` behaviour, now served directly).

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use sk_server::ServeOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
async fn the_launchers_locale_is_a_value_not_the_environment() {
    // the server may run inside the window program, whose toolkit threads read the environment:
    // the locale the launcher read goes to the interface language as a value
    let srv = server_with(
        ServeOptions {
            token: TOKEN.to_owned(),
            system_locale: Some("en-US".into()),
            ..ServeOptions::default()
        },
        |_| {},
    )
    .await;
    assert_eq!(std::env::var_os("SPACEKNIFE_SYSTEM_LOCALE"), None);
    assert_eq!(sk_core::i18n::system_locale().as_deref(), Some("en-US"));
    srv.shutdown();
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

/// A plain HTTP/1.1 request; (status, headers, body).
async fn http(port: u16, path: &str, headers: &[(&str, &str)]) -> (u16, String, Vec<u8>) {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let mut req = format!("GET {path} HTTP/1.1\r\nConnection: close\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&buf[..split]).into_owned();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, head.to_ascii_lowercase(), buf[split + 4..].to_vec())
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
    assert!(head.contains("content-security-policy: frame-ancestors 'none'"));
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
