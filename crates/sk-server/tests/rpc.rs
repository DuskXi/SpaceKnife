//! Port of tests/test_rpc.py.

mod common;

use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sk_protocol::binary::FrameBuilder;
use sk_protocol::{ErrorCode, PROTOCOL_VERSION};
use tokio_tungstenite::tungstenite::Message;

fn code(e: ErrorCode) -> i64 {
    i64::from(e.code())
}

#[tokio::test]
async fn ping_and_info() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let pong = c.ok("system.ping", json!({"payload": "你好"})).await;
    assert_eq!(pong["payload"], "你好");
    let info = c.ok("system.info", json!({})).await;
    assert_eq!(info["pid"], std::process::id());
    assert_eq!(info["sessions"], 1);
    assert!(!info["server"]["rust"].as_str().unwrap().is_empty());
    assert!(!info["platform"]["system"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn unknown_method() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    assert_eq!(c.err("nope.method", json!({})).await.code, code(ErrorCode::MethodNotFound));
}

#[tokio::test]
async fn invalid_params_are_reported_with_details() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let e = c.err("test.sleep", json!({"payload": "x"})).await;
    assert_eq!(e.code, code(ErrorCode::InvalidParams));
    assert!(e.data.as_ref().and_then(Value::as_array).is_some_and(|a| !a.is_empty()));
    // a real method's params: unknown fields and constraints
    let e = c.err("system.ping", json!({"payload": "x", "extra": 1})).await;
    assert_eq!(e.code, code(ErrorCode::InvalidParams));
    let e = c.err("view.results", json!({"view_id": "v", "limit": 100000})).await;
    assert_eq!(e.code, code(ErrorCode::InvalidParams));
}

#[tokio::test]
async fn cancel_in_flight_request() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let (id, rx) = c.send("test.sleep", json!({"seconds": 10}));
    tokio::time::sleep(Duration::from_millis(100)).await;
    c.notify("$/cancel", json!({"id": id}));
    let r = tokio::time::timeout(Duration::from_secs(2), rx).await.unwrap().unwrap();
    assert_eq!(r.unwrap_err().code, code(ErrorCode::RequestCancelled));
    let session = srv.server.sessions().into_iter().next().unwrap();
    for _ in 0..50 {
        if session.in_flight() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(session.in_flight(), 0);
    let pong = c.ok("system.ping", json!({"payload": "still alive"})).await;
    assert_eq!(pong["payload"], "still alive");
}

#[tokio::test]
async fn concurrent_requests_complete_independently() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let (_, slow) = c.send("test.sleep", json!({"seconds": 0.3}));
    let fast = c.ok("system.ping", json!({"payload": "fast"})).await;
    assert_eq!(fast["payload"], "fast");
    let mut slow = slow;
    assert!(slow.try_recv().is_err(), "the slow request answered first");
    assert_eq!(slow.await.unwrap().unwrap(), json!({}));
}

async fn raw_session(url: &str) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut ws = raw(url, None, true).await.unwrap();
    ws.send(Message::text(hello_frame(TOKEN, PROTOCOL_VERSION, 0, &["subscriptions".to_owned()]))).await.unwrap();
    next_json(&mut ws).await;
    ws
}

async fn next_json(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) -> Value {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(&t).unwrap(),
            Some(Ok(_)) => continue,
            other => panic!("no frame: {other:?}"),
        }
    }
}

#[tokio::test]
async fn duplicate_request_id_is_rejected() {
    let srv = server().await;
    let mut ws = raw_session(&srv.url).await;
    let req = json!({"jsonrpc": "2.0", "id": 9, "method": "test.sleep", "params": {"seconds": 0.5}}).to_string();
    ws.send(Message::text(req.clone())).await.unwrap();
    ws.send(Message::text(req)).await.unwrap();
    let first = next_json(&mut ws).await;
    assert_eq!(first["id"], 9);
    assert_eq!(first["error"]["code"], code(ErrorCode::InvalidRequest));
    let second = next_json(&mut ws).await;
    assert_eq!(second, json!({"jsonrpc": "2.0", "id": 9, "result": {}}));
}

#[tokio::test]
async fn malformed_frame_after_handshake_gets_error() {
    let srv = server().await;
    let mut ws = raw_session(&srv.url).await;
    ws.send(Message::text("{oops")).await.unwrap();
    let reply = next_json(&mut ws).await;
    assert!(reply["id"].is_null());
    assert_eq!(reply["error"]["code"], code(ErrorCode::InvalidRequest));
}

#[tokio::test]
async fn inbound_messages_over_1_mib_close_the_connection() {
    let srv = server().await;
    let mut ws = raw_session(&srv.url).await;
    let big = json!({"jsonrpc": "2.0", "id": 1, "method": "system.ping", "params": {"payload": "x".repeat(2 << 20)}});
    let _ = ws.send(Message::text(big.to_string())).await;
    let mut closed = false;
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => {
                closed = true;
                break;
            }
            Ok(Some(Ok(_))) => continue,
            Err(_) => break,
        }
    }
    assert!(closed, "an oversized message must end the connection");
}

#[tokio::test]
async fn log_subscription_streams_records() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let sub = c.subscribe("log", json!({"min_level": "INFO", "backlog": 0})).await;
    tracing::debug!("debug is below the floor");
    tracing::warn!("磁盘读取 warn-marker");
    let ev = c
        .event_where("log.record", 2.0, |p| {
            assert_ne!(p["level"], "DEBUG");
            p["message"].as_str().unwrap_or("").contains("warn-marker")
        })
        .await;
    assert_eq!(ev["sub"], sub.as_str());
    assert_eq!(ev["level"], "WARNING");
    c.ok("$/unsubscribe", json!({"sub": sub})).await;
    let e = c.err("$/unsubscribe", json!({"sub": sub})).await;
    assert_eq!(e.code, code(ErrorCode::UnknownSubscription));
}

#[tokio::test]
async fn subscribe_requires_negotiated_capability() {
    let srv = server().await;
    let c = Client::open_caps(&srv.url, &[]).await;
    let e = c.err("$/subscribe", json!({"topic": "log"})).await;
    assert_eq!(e.code, code(ErrorCode::InvalidRequest));
    let c = Client::open_caps(&srv.url, &["subscriptions"]).await;
    let e = c.err("$/subscribe", json!({"topic": "log"})).await;
    assert_eq!(e.code, code(ErrorCode::InvalidRequest), "log-stream not negotiated");
}

#[tokio::test]
async fn unknown_topic() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let e = c.err("$/subscribe", json!({"topic": "nope"})).await;
    assert_eq!(e.code, code(ErrorCode::UnknownTopic));
    assert!(e.data.unwrap()["topics"].as_array().unwrap().iter().any(|t| t == "scan"));
}

#[tokio::test]
async fn binary_frames_reach_the_client() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let session = srv.server.sessions().into_iter().next().unwrap();
    let x: Vec<f32> = vec![0.0, 1.0, 2.0];
    let frame = FrameBuilder::new("drawlist", "rect.v1").column("x", &x[..]).finish().unwrap();
    session.send_binary(frame);
    let got = c.binary(2.0).await;
    let decoded = sk_protocol::binary::decode_frame(&got).unwrap();
    assert_eq!(decoded.header.stream, "drawlist");
    assert_eq!(decoded.column("x").unwrap().to_vec::<f32>().unwrap(), x);
}

#[tokio::test]
async fn responses_go_before_frames_and_events() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let session = srv.server.sessions().into_iter().next().unwrap();
    // events and frames queued before a response still leave after it
    for _ in 0..50 {
        session.emit_named("test.event", &json!({"n": 1}));
    }
    let pong = c.ok("system.ping", json!({"payload": "first"})).await;
    assert_eq!(pong["payload"], "first");
}

#[tokio::test]
async fn app_shutdown_requests_server_stop() {
    let srv = server().await;
    let c = Client::open(&srv.url).await;
    let r = c.ok("app.shutdown", json!({})).await;
    assert_eq!(r["accepted"], true);
    tokio::time::timeout(Duration::from_secs(2), srv.server.wait_shutdown()).await.unwrap();
    assert_eq!(c.closed(5.0).await, Some(1001));
    tokio::time::timeout(Duration::from_secs(10), srv.wait()).await.unwrap();
}

#[tokio::test]
async fn every_method_is_routed() {
    let srv = server().await;
    let names = srv.server.router.names();
    for m in sk_protocol::methods::METHODS.iter().filter(|m| m.name != "session.hello") {
        assert!(names.contains(&m.name), "{} has no handler", m.name);
    }
}
