//! One WebSocket connection: handshake, request dispatch, cancellation, the outbound queue
//! (Python `server/session.py`).
//!
//! Outbound order: responses first, then binary frames (bulk data), then events; when the client
//! falls far behind ([`MAX_QUEUED_EVENTS`]) new events are dropped, never responses or frames.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::Serialize;
use sk_protocol::envelope::encode_error_code;
use sk_protocol::methods::{CANCEL, Event, SESSION_HELLO};
use sk_protocol::session::{ClientInfo, HelloParams, HelloResult, Limits};
use sk_protocol::streams::CancelParams;
use sk_protocol::{
    CloseCode, ErrorCode, Message as RpcMessage, PROTOCOL_VERSION, RequestId, encode_notification,
    encode_response, parse_message, parse_params, protocol_major,
};
use tokio::sync::{Notify, oneshot};

use crate::app::Server;
use crate::rpc::{Call, RpcError, not_found};
use crate::topics::Subscription;
use crate::util::{constant_time_eq, random_hex};

/// Events queued at most; later ones are dropped until the client catches up.
pub const MAX_QUEUED_EVENTS: usize = 10_000;
/// Keep-alive pings.
pub const PING_INTERVAL: Duration = Duration::from_secs(20);

/// Where an outbound frame queues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Response,
    Bulk,
    Event,
}

enum Out {
    Text(Utf8Bytes),
    Binary(Bytes),
}

impl Out {
    fn len(&self) -> usize {
        match self {
            Out::Text(t) => t.len(),
            Out::Binary(b) => b.len(),
        }
    }
}

#[derive(Default)]
struct Queues {
    responses: VecDeque<Out>,
    bulk: VecDeque<Out>,
    events: VecDeque<Out>,
    close: Option<(u16, String)>,
    closed: bool,
}

/// The outbound queue the writer task drains.
#[derive(Default)]
struct Outbox {
    q: Mutex<Queues>,
    ready: Notify,
}

impl Outbox {
    fn push(&self, priority: Priority, out: Out) -> bool {
        {
            let mut q = self.q.lock();
            if q.closed {
                return false;
            }
            match priority {
                Priority::Response => q.responses.push_back(out),
                Priority::Bulk => q.bulk.push_back(out),
                Priority::Event => {
                    if q.events.len() >= MAX_QUEUED_EVENTS {
                        return false;
                    }
                    q.events.push_back(out)
                }
            }
        }
        self.ready.notify_one();
        true
    }

    /// The next frame by priority; a close request once the responses before it are out.
    fn pop(&self) -> Next {
        let mut q = self.q.lock();
        if let Some(o) = q.responses.pop_front() {
            return Next::Out(o);
        }
        if let Some(c) = q.close.take() {
            q.closed = true;
            return Next::Close(c);
        }
        match q.bulk.pop_front().or_else(|| q.events.pop_front()) {
            Some(o) => Next::Out(o),
            None if q.closed => Next::Done,
            None => Next::Wait,
        }
    }
}

enum Next {
    Out(Out),
    Close((u16, String)),
    /// the connection ended and nothing is left to send
    Done,
    Wait,
}

/// One connection's state, shared by its request tasks.
pub struct Session {
    pub id: String,
    hello: OnceLock<(ClientInfo, Vec<String>)>,
    out: Outbox,
    tasks: Mutex<HashMap<RequestId, oneshot::Sender<()>>>,
    subs: Mutex<HashMap<String, Subscription>>,
    closing: AtomicBool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Session {
    fn new() -> Session {
        Session {
            id: random_hex(8),
            hello: OnceLock::new(),
            out: Outbox::default(),
            tasks: Mutex::new(HashMap::new()),
            subs: Mutex::new(HashMap::new()),
            closing: AtomicBool::new(false),
        }
    }

    /// The client of the handshake.
    pub fn client(&self) -> Option<&ClientInfo> {
        self.hello.get().map(|h| &h.0)
    }

    /// Negotiated capabilities, sorted.
    pub fn capabilities(&self) -> &[String] {
        self.hello.get().map_or(&[], |h| h.1.as_slice())
    }

    pub fn has_cap(&self, cap: &str) -> bool {
        self.capabilities().iter().any(|c| c == cap)
    }

    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }

    /// Queue an event of type `E` (dropped while the client is far behind).
    pub fn emit<E: Event>(&self, payload: &E::Payload) {
        self.emit_named(E::NAME, payload);
    }

    /// Queue an event by name.
    pub fn emit_named<P: Serialize + ?Sized>(&self, name: &str, payload: &P) {
        if self.is_closing() {
            return;
        }
        let frame = encode_notification(name, payload);
        self.out.push(Priority::Event, Out::Text(frame.into()));
    }

    /// Queue a binary frame (`sk_protocol::binary`).
    pub fn send_binary(&self, frame: impl Into<Bytes>) {
        self.out.push(Priority::Bulk, Out::Binary(frame.into()));
    }

    /// Queue an already encoded response frame.
    pub fn send_response(&self, frame: String) {
        self.out.push(Priority::Response, Out::Text(frame.into()));
    }

    fn send_error(&self, id: Option<&RequestId>, e: &RpcError) {
        self.send_response(encode_error_code(id, e.code, &e.message, e.data.as_ref()));
    }

    /// Close the connection (after the responses already queued).
    pub fn close(&self, code: CloseCode, reason: &str) {
        self.close_code(code.code(), reason);
    }

    fn close_code(&self, code: u16, reason: &str) {
        self.out
            .q
            .lock()
            .close
            .get_or_insert((code, reason.to_owned()));
        self.out.ready.notify_one();
    }

    /// Queued frames and their bytes (diagnostics).
    pub fn outbox_stats(&self) -> (usize, usize) {
        let q = self.out.q.lock();
        let all = q.responses.iter().chain(&q.bulk).chain(&q.events);
        let mut n = 0;
        let mut bytes = 0;
        for o in all {
            n += 1;
            bytes += o.len();
        }
        (n, bytes)
    }

    pub fn add_subscription(&self, sub: Subscription) {
        self.subs.lock().insert(sub.id.clone(), sub);
    }

    pub fn remove_subscription(&self, id: &str) -> bool {
        let sub = self.subs.lock().remove(id);
        match sub {
            Some(s) => {
                s.close();
                true
            }
            None => false,
        }
    }

    /// Requests in flight (tests).
    pub fn in_flight(&self) -> usize {
        self.tasks.lock().len()
    }
}

// ---------------------------------------------------------------------------------- handshake

async fn send_now(ws: &mut WebSocket, frame: String) -> bool {
    ws.send(Message::Text(frame.into())).await.is_ok()
}

async fn reject(mut ws: WebSocket, code: CloseCode, reason: &str) {
    let frame = CloseFrame {
        code: code.code(),
        reason: reason.into(),
    };
    if ws.send(Message::Close(Some(frame))).await.is_ok() {
        // wait for the peer's close (bounded)
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(Ok(m)) = ws.recv().await {
                if matches!(m, Message::Close(_)) {
                    break;
                }
            }
        })
        .await;
    }
}

fn err_frame(
    id: Option<&RequestId>,
    code: ErrorCode,
    message: &str,
    data: Option<&serde_json::Value>,
) -> String {
    encode_error_code(id, code.code().into(), message, data)
}

/// Runs the handshake; None when the connection was rejected (and closed).
async fn handshake(server: &Server, mut ws: WebSocket) -> Option<(WebSocket, Session)> {
    let raw = match tokio::time::timeout(server.config.hello_timeout, ws.recv()).await {
        Err(_) => {
            reject(
                ws,
                CloseCode::HandshakeTimeout,
                "session.hello not received in time",
            )
            .await;
            return None;
        }
        Ok(None) | Ok(Some(Err(_))) => return None,
        Ok(Some(Ok(m))) => m,
    };
    let text = match raw {
        Message::Text(t) => t,
        Message::Close(_) => return None,
        _ => {
            reject(
                ws,
                CloseCode::ProtocolError,
                "first frame must be a session.hello text frame",
            )
            .await;
            return None;
        }
    };
    let msg = match parse_message(text.as_str()) {
        Ok(m) => m,
        Err(e) => {
            send_now(&mut ws, err_frame(None, ErrorCode::ParseError, &e.0, None)).await;
            reject(ws, CloseCode::ProtocolError, "malformed handshake").await;
            return None;
        }
    };
    let req = match msg {
        RpcMessage::Request(r) if r.method == SESSION_HELLO => r,
        other => {
            let id = match &other {
                RpcMessage::Request(r) => Some(r.id.clone()),
                _ => None,
            };
            let frame = err_frame(
                id.as_ref(),
                ErrorCode::NotHandshaken,
                "session.hello must come first",
                None,
            );
            send_now(&mut ws, frame).await;
            reject(ws, CloseCode::Unauthorized, "not handshaken").await;
            return None;
        }
    };
    let hello: HelloParams = match parse_params(req.params.as_deref()) {
        Ok(h) => h,
        Err(e) => {
            let frame = err_frame(
                Some(&req.id),
                ErrorCode::InvalidParams,
                "invalid session.hello params",
                Some(&e.data()),
            );
            send_now(&mut ws, frame).await;
            reject(ws, CloseCode::ProtocolError, "invalid session.hello").await;
            return None;
        }
    };
    if !constant_time_eq(hello.token.as_bytes(), server.config.token.as_bytes()) {
        tracing::warn!("rejected a connection with a wrong token");
        send_now(
            &mut ws,
            err_frame(
                Some(&req.id),
                ErrorCode::Unauthorized,
                "invalid token",
                None,
            ),
        )
        .await;
        reject(ws, CloseCode::Unauthorized, "invalid token").await;
        return None;
    }
    if protocol_major(&hello.protocol) != protocol_major(PROTOCOL_VERSION) {
        let msg = format!(
            "server speaks protocol {PROTOCOL_VERSION}, client sent {}",
            hello.protocol
        );
        let data = serde_json::json!({ "server": PROTOCOL_VERSION });
        send_now(
            &mut ws,
            err_frame(
                Some(&req.id),
                ErrorCode::ProtocolMismatch,
                &msg,
                Some(&data),
            ),
        )
        .await;
        reject(
            ws,
            CloseCode::VersionMismatch,
            "protocol major version mismatch",
        )
        .await;
        return None;
    }
    let mut caps: Vec<String> = hello
        .capabilities
        .iter()
        .filter(|c| server.config.capabilities.iter().any(|s| s == *c))
        .cloned()
        .collect();
    caps.sort();
    caps.dedup();
    let session = Session::new();
    let result = HelloResult {
        protocol: PROTOCOL_VERSION.to_owned(),
        server: server.server_info.clone(),
        session_id: session.id.clone(),
        capabilities: caps.clone(),
        schema_hash: server.schema_hash.clone(),
        limits: Limits {
            max_inbound_message_bytes: server.config.max_inbound_bytes as i64,
            max_outbound_chunk_bytes: server.config.max_outbound_chunk_bytes as i64,
            hello_timeout_s: server.config.hello_timeout.as_secs_f64(),
        },
    };
    if !send_now(&mut ws, encode_response(&req.id, &result)).await {
        return None;
    }
    tracing::info!(
        "session {} opened: {} {} {} caps={}",
        session.id,
        hello.client.kind.as_str(),
        hello.client.name,
        hello.client.version,
        if caps.is_empty() {
            "-".to_owned()
        } else {
            caps.join(",")
        }
    );
    let _ = session.hello.set((hello.client, caps));
    Some((ws, session))
}

// ---------------------------------------------------------------------------------- running

/// Serve one upgraded connection until it closes.
pub(crate) async fn run(server: Arc<Server>, ws: WebSocket) {
    let Some((ws, session)) = handshake(&server, ws).await else {
        return;
    };
    let session = Arc::new(session);
    server.add_session(&session);
    let (mut sink, mut stream) = ws.split();

    let writer = {
        let session = session.clone();
        tokio::spawn(async move {
            let mut ping = tokio::time::interval(PING_INTERVAL);
            ping.tick().await;
            loop {
                let next = session.out.pop();
                match next {
                    Next::Out(out) => {
                        let msg = match out {
                            Out::Text(t) => Message::Text(t),
                            Out::Binary(b) => Message::Binary(b),
                        };
                        if sink.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Next::Close((code, reason)) => {
                        let _ = sink
                            .send(Message::Close(Some(CloseFrame {
                                code,
                                reason: reason.into(),
                            })))
                            .await;
                        break;
                    }
                    Next::Done => break,
                    Next::Wait => {
                        tokio::select! {
                            _ = session.out.ready.notified() => {}
                            _ = ping.tick() => {
                                if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        })
    };

    while let Some(frame) = stream.next().await {
        let frame = match frame {
            Ok(f) => f,
            Err(e) => {
                let text = e.to_string();
                if text.contains("too long") || text.contains("too big") || text.contains("limit") {
                    session.close_code(1009, "message too big");
                }
                break;
            }
        };
        match frame {
            Message::Text(text) => on_text(&server, &session, text.as_str()),
            Message::Binary(_) => {
                session.close(
                    CloseCode::ProtocolError,
                    "clients must not send binary frames in protocol v1",
                );
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }

    // teardown
    session.closing.store(true, Ordering::SeqCst);
    let subs: Vec<Subscription> = session.subs.lock().drain().map(|(_, s)| s).collect();
    for s in subs {
        s.close();
    }
    session.tasks.lock().clear(); // dropping the senders cancels the requests
    {
        let mut q = session.out.q.lock();
        q.closed = true;
        q.responses.clear();
        q.bulk.clear();
        q.events.clear();
    }
    session.out.ready.notify_one();
    let _ = tokio::time::timeout(Duration::from_secs(1), writer).await;
    server.remove_session(&session);
    if let Some(c) = session.client() {
        tracing::info!(
            "session {} closed ({} {})",
            session.id,
            c.kind.as_str(),
            c.name
        );
    }
}

fn on_text(server: &Arc<Server>, session: &Arc<Session>, text: &str) {
    let msg = match parse_message(text) {
        Ok(m) => m,
        Err(e) => {
            session.send_error(None, &RpcError::invalid_request(e.0));
            return;
        }
    };
    match msg {
        RpcMessage::Request(req) => start_request(server, session, req),
        RpcMessage::Notification(n) => {
            if n.method == CANCEL {
                match parse_params::<CancelParams>(n.params.as_deref()) {
                    Ok(p) => {
                        if let Some(tx) = session.tasks.lock().remove(&p.id) {
                            let _ = tx.send(());
                        }
                    }
                    Err(_) => tracing::debug!("session {} sent an invalid $/cancel", session.id),
                }
            } else {
                tracing::debug!(
                    "session {} sent unknown notification {}",
                    session.id,
                    n.method
                );
            }
        }
        _ => tracing::debug!(
            "session {} ignored an unexpected response frame",
            session.id
        ),
    }
}

fn start_request(server: &Arc<Server>, session: &Arc<Session>, req: sk_protocol::Request) {
    if req.method == SESSION_HELLO {
        session.send_error(
            Some(&req.id),
            &RpcError::invalid_request("session already established"),
        );
        return;
    }
    let Some(handler) = server.router.get(&req.method).cloned() else {
        session.send_error(Some(&req.id), &not_found(&req.method));
        return;
    };
    let (tx, mut rx) = oneshot::channel::<()>();
    {
        let mut tasks = session.tasks.lock();
        if tasks.contains_key(&req.id) {
            drop(tasks);
            let e =
                RpcError::invalid_request(format!("request id {} is already in flight", req.id));
            session.send_error(Some(&req.id), &e);
            return;
        }
        tasks.insert(req.id.clone(), tx);
    }
    let call = Call {
        server: server.clone(),
        session: session.clone(),
        id: req.id.clone(),
    };
    let session = session.clone();
    let method = req.method;
    tokio::spawn(async move {
        use futures_util::FutureExt as _;
        let id = call.id.clone();
        // a panic in the handler (a bug) answers this request with an internal error; without
        // this the task just ends: no answer, and the id stays "in flight" in the session
        let params = req.params;
        let fut = std::panic::AssertUnwindSafe(async move { handler(call, params).await })
            .catch_unwind()
            .map(|r| {
                r.unwrap_or_else(|payload| {
                    let what = payload
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_owned())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_owned());
                    Err(RpcError::internal(format!(
                        "internal error (panic: {what})"
                    )))
                })
            });
        let out = tokio::select! {
            r = fut => Some(r),
            _ = &mut rx => None,
        };
        // still ours: forget it (a cancel took it out already)
        session.tasks.lock().remove(&id);
        match out {
            Some(Ok(frame)) => session.send_response(frame),
            Some(Err(e)) => {
                if e.code == i64::from(ErrorCode::InternalError.code()) {
                    tracing::error!("handler for {method} failed: {}", e.message);
                }
                session.send_error(Some(&id), &e);
            }
            None => {
                if !session.is_closing() {
                    session.send_error(Some(&id), &RpcError::cancelled());
                }
            }
        }
    });
}
