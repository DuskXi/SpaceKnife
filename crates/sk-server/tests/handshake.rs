//! Port of tests/test_handshake.py.

mod common;

use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use sk_protocol::{CloseCode, ErrorCode, PROTOCOL_VERSION};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn recv_json(ws: &mut Ws) -> Value {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(&t).unwrap(),
            Some(Ok(Message::Close(c))) => panic!("closed: {c:?}"),
            Some(Ok(_)) => continue,
            other => panic!("no frame: {other:?}"),
        }
    }
}

/// The close code the server sends next.
async fn close_code(ws: &mut Ws) -> Option<u16> {
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("closed in time")
        {
            Some(Ok(Message::Close(c))) => return c.map(|c| c.code.into()),
            Some(Ok(_)) => continue,
            _ => return None,
        }
    }
}

fn caps(c: &[&str]) -> Vec<String> {
    c.iter().map(|s| (*s).to_owned()).collect()
}

#[tokio::test]
async fn successful_hello_negotiates_capabilities() {
    let srv = server().await;
    let c = Client::open_with(&srv.url, TOKEN, caps(&["subscriptions", "made-up"]), None)
        .await
        .unwrap();
    assert_eq!(c.hello["protocol"], PROTOCOL_VERSION);
    assert_eq!(
        c.hello["capabilities"],
        serde_json::json!(["subscriptions"])
    );
    assert_eq!(c.hello["schema_hash"], sk_protocol::schema::schema_hash());
    assert!(!c.hello["session_id"].as_str().unwrap().is_empty());
    assert!(!c.hello["server"]["rust"].as_str().unwrap().is_empty());
    let caps_all: Vec<&str> = srv
        .server
        .config
        .capabilities
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(caps_all, sk_protocol::ALL_CAPABILITIES.to_vec());
    c.close().await;
}

#[tokio::test]
async fn wrong_token_is_rejected_with_4401() {
    let srv = server().await;
    let mut ws = raw(&srv.url, None, true).await.unwrap();
    ws.send(Message::text(hello_frame(
        &"x".repeat(32),
        PROTOCOL_VERSION,
        0,
        &caps(&["subscriptions"]),
    )))
    .await
    .unwrap();
    let reply = recv_json(&mut ws).await;
    assert_eq!(reply["error"]["code"], ErrorCode::Unauthorized.code());
    assert_eq!(
        close_code(&mut ws).await,
        Some(CloseCode::Unauthorized.code())
    );
}

#[tokio::test]
async fn client_sees_remote_error_on_bad_token() {
    let srv = server().await;
    let e = Client::open_with(&srv.url, &"y".repeat(32), all_caps(), None)
        .await
        .err()
        .unwrap();
    assert_eq!(e.code, i64::from(ErrorCode::Unauthorized.code()));
}

#[tokio::test]
async fn missing_hello_times_out_with_4408() {
    let srv = server().await;
    let mut ws = raw(&srv.url, None, true).await.unwrap();
    assert_eq!(
        close_code(&mut ws).await,
        Some(CloseCode::HandshakeTimeout.code())
    );
}

#[tokio::test]
async fn request_before_hello_is_rejected() {
    let srv = server().await;
    let mut ws = raw(&srv.url, None, true).await.unwrap();
    ws.send(Message::text(
        r#"{"jsonrpc":"2.0","id":5,"method":"system.info"}"#,
    ))
    .await
    .unwrap();
    let reply = recv_json(&mut ws).await;
    assert_eq!(reply["id"], 5);
    assert_eq!(reply["error"]["code"], ErrorCode::NotHandshaken.code());
    assert_eq!(
        close_code(&mut ws).await,
        Some(CloseCode::Unauthorized.code())
    );
}

#[tokio::test]
async fn major_version_mismatch_closes_with_4426() {
    let srv = server().await;
    let mut ws = raw(&srv.url, None, true).await.unwrap();
    ws.send(Message::text(hello_frame(
        TOKEN,
        "2.0",
        0,
        &caps(&["subscriptions"]),
    )))
    .await
    .unwrap();
    let reply = recv_json(&mut ws).await;
    assert_eq!(reply["error"]["code"], ErrorCode::ProtocolMismatch.code());
    assert_eq!(
        close_code(&mut ws).await,
        Some(CloseCode::VersionMismatch.code())
    );
}

#[tokio::test]
async fn invalid_hello_params_close_with_4400() {
    let srv = server().await;
    let mut ws = raw(&srv.url, None, true).await.unwrap();
    ws.send(Message::text(
        r#"{"jsonrpc":"2.0","id":1,"method":"session.hello","params":{"token":"short"}}"#,
    ))
    .await
    .unwrap();
    let reply = recv_json(&mut ws).await;
    assert_eq!(reply["error"]["code"], ErrorCode::InvalidParams.code());
    assert_eq!(
        close_code(&mut ws).await,
        Some(CloseCode::ProtocolError.code())
    );
}

#[tokio::test]
async fn older_minor_version_is_accepted() {
    let srv = server().await;
    let mut ws = raw(&srv.url, None, true).await.unwrap();
    ws.send(Message::text(hello_frame(
        TOKEN,
        "1.0",
        0,
        &caps(&["subscriptions"]),
    )))
    .await
    .unwrap();
    let reply = recv_json(&mut ws).await;
    assert_eq!(reply["result"]["protocol"], PROTOCOL_VERSION);
    assert_eq!(
        reply["result"]["capabilities"],
        serde_json::json!(["subscriptions"])
    );
}

fn status_of(e: WsError) -> u16 {
    match e {
        WsError::Http(r) => r.status().as_u16(),
        other => panic!("not an HTTP refusal: {other}"),
    }
}

#[tokio::test]
async fn missing_subprotocol_is_rejected() {
    let srv = server().await;
    let e = raw(&srv.url, None, false).await.err().unwrap();
    assert_eq!(status_of(e), 400);
}

#[tokio::test]
async fn wrong_path_gets_404() {
    let srv = server().await;
    let url = srv.url.trim_end_matches("/v1").to_owned() + "/other";
    let e = raw(&url, None, true).await.err().unwrap();
    assert_eq!(status_of(e), 404);
}

#[tokio::test]
async fn foreign_origin_gets_403() {
    let srv = server().await;
    let e = raw(&srv.url, Some("https://evil.example"), true)
        .await
        .err()
        .unwrap();
    assert_eq!(status_of(e), 403);
}

#[tokio::test]
async fn app_origins_are_allowed() {
    let srv = server().await;
    for origin in [
        "app://spaceknife",
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
    ] {
        let c = Client::open_with(&srv.url, TOKEN, all_caps(), Some(origin))
            .await
            .unwrap();
        c.close().await;
    }
}

#[tokio::test]
async fn configured_origin_is_allowed() {
    let opts = sk_server::ServeOptions {
        token: TOKEN.to_owned(),
        allow_origins: vec!["http://localhost:3000".to_owned()],
        ..Default::default()
    };
    let srv = server_with(opts, |_| {}).await;
    let c = Client::open_with(&srv.url, TOKEN, all_caps(), Some("http://localhost:3000"))
        .await
        .unwrap();
    c.close().await;
}
