//! Test fixtures: an in-process server and a WebSocket client (tokio-tungstenite).
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sk_protocol::methods::Method;
use sk_server::app::Tuning;
use sk_server::rpc::{RpcError, Router};
use sk_server::{ServeOptions, ServerHandle};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub const TOKEN: &str = "test-token-0123456789abcdef";

/// A private SPACEKNIFE_HOME for this test binary (never the real user's data), Chinese source
/// texts, and a tracing subscriber feeding the global log bus.
pub fn init() -> PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    static LOG: Once = Once::new();
    let home = HOME
        .get_or_init(|| {
            let dir = tempfile::Builder::new().prefix("sk-home-").tempdir().expect("temp dir").keep();
            // SAFETY: set once, before any server of this test binary starts
            unsafe {
                std::env::set_var("SPACEKNIFE_HOME", &dir);
                std::env::set_var("SPACEKNIFE_LANGUAGE", "zh-CN");
            }
            dir
        })
        .clone();
    LOG.call_once(|| {
        use tracing_subscriber::layer::SubscriberExt;
        let bus = sk_core::log::LogBus::global();
        let sub = tracing_subscriber::registry().with(bus.layer());
        let _ = tracing::subscriber::set_global_default(sub);
    });
    home
}

#[derive(Serialize, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SleepParams {
    pub seconds: f64,
}

/// Start a server for a test: hello timeout 0.5 s, no update checks, the `test.sleep` method.
pub async fn server() -> ServerHandle {
    server_with(ServeOptions { token: TOKEN.to_owned(), ..ServeOptions::default() }, |_| {}).await
}

/// [`server`] with other options and more routes.
pub async fn server_with(opts: ServeOptions, routes: impl FnOnce(&mut Router) + Send + 'static) -> ServerHandle {
    init();
    let mut tuning = Tuning::for_tests();
    tuning.routes = Some(Box::new(move |r: &mut Router| {
        r.add_raw("test.sleep", |call, raw| async move {
            let text = raw.as_deref().map_or("{}", |r| r.get());
            let p: SleepParams = serde_json::from_str(text).map_err(|e| {
                RpcError::invalid_params("invalid params")
                    .with_data(json!([{"loc": [], "msg": e.to_string(), "type": "value_error"}]))
            })?;
            if !(0.0..=30.0).contains(&p.seconds) {
                return Err(RpcError::invalid_params("invalid params").with_data(json!([{"loc": ["seconds"]}])));
            }
            tokio::time::sleep(Duration::from_secs_f64(p.seconds)).await;
            Ok(sk_protocol::encode_response(&call.id, &json!({})))
        });
        routes(r);
    }));
    sk_server::start_with(opts, tuning).await.expect("server starts")
}

/// An error answer.
#[derive(Debug, Clone)]
pub struct Remote {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, Remote>>>>>;

/// A protocol client: requests, events, binary frames.
pub struct Client {
    tx: mpsc::UnboundedSender<Message>,
    pending: Pending,
    next: std::sync::atomic::AtomicU64,
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, Value)>>,
    binary: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    pub hello: Value,
    closed: Arc<tokio::sync::Notify>,
    pub close_code: Arc<Mutex<Option<u16>>>,
}

pub fn all_caps() -> Vec<String> {
    sk_protocol::ALL_CAPABILITIES.iter().map(|s| (*s).to_owned()).collect()
}

/// Open a raw WebSocket to `url` with the subprotocol (and an optional origin).
pub async fn raw(
    url: &str,
    origin: Option<&str>,
    subprotocol: bool,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Error,
> {
    let mut req = url.into_client_request()?;
    if subprotocol {
        req.headers_mut().insert("Sec-WebSocket-Protocol", HeaderValue::from_static(sk_protocol::SUBPROTOCOL));
    }
    if let Some(o) = origin {
        req.headers_mut().insert("Origin", HeaderValue::from_str(o).unwrap());
    }
    tokio_tungstenite::connect_async(req).await.map(|(ws, _)| ws)
}

pub fn hello_frame(token: &str, protocol: &str, id: u64, caps: &[String]) -> String {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "session.hello",
        "params": {"token": token, "protocol": protocol,
                   "client": {"name": "raw", "version": "0", "kind": "test"},
                   "capabilities": caps}
    })
    .to_string()
}

impl Client {
    pub async fn open(url: &str) -> Client {
        Client::open_with(url, TOKEN, all_caps(), None).await.expect("hello")
    }

    pub async fn open_caps(url: &str, caps: &[&str]) -> Client {
        Client::open_with(url, TOKEN, caps.iter().map(|s| (*s).to_owned()).collect(), None).await.expect("hello")
    }

    pub async fn open_with(url: &str, token: &str, caps: Vec<String>, origin: Option<&str>) -> Result<Client, Remote> {
        let ws = raw(url, origin, true).await.expect("connect");
        let (mut sink, mut stream) = ws.split();
        sink.send(Message::text(hello_frame(token, sk_protocol::PROTOCOL_VERSION, 0, &caps))).await.unwrap();
        let first = loop {
            match stream.next().await {
                Some(Ok(Message::Text(t))) => break serde_json::from_str::<Value>(&t).unwrap(),
                Some(Ok(_)) => continue,
                other => panic!("no hello answer: {other:?}"),
            }
        };
        if let Some(e) = first.get("error") {
            return Err(Remote {
                code: e["code"].as_i64().unwrap(),
                message: e["message"].as_str().unwrap_or("").to_owned(),
                data: e.get("data").cloned(),
            });
        }
        let hello = first["result"].clone();
        let (tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (bin_tx, bin_rx) = mpsc::unbounded_channel();
        let closed = Arc::new(tokio::sync::Notify::new());
        let close_code = Arc::new(Mutex::new(None));
        tokio::spawn(async move {
            while let Some(m) = out_rx.recv().await {
                if sink.send(m).await.is_err() {
                    break;
                }
            }
        });
        {
            let pending = pending.clone();
            let closed = closed.clone();
            let close_code = close_code.clone();
            tokio::spawn(async move {
                while let Some(m) = stream.next().await {
                    match m {
                        Ok(Message::Text(t)) => {
                            let v: Value = serde_json::from_str(&t).unwrap();
                            if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
                                let _ = ev_tx.send((method.to_owned(), v["params"].clone()));
                            } else if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
                                let slot = pending.lock().unwrap().remove(&id);
                                if let Some(slot) = slot {
                                    let r = match v.get("error") {
                                        Some(e) => Err(Remote {
                                            code: e["code"].as_i64().unwrap(),
                                            message: e["message"].as_str().unwrap_or("").to_owned(),
                                            data: e.get("data").cloned(),
                                        }),
                                        None => Ok(v["result"].clone()),
                                    };
                                    let _ = slot.send(r);
                                }
                            }
                        }
                        Ok(Message::Binary(b)) => {
                            let _ = bin_tx.send(b.to_vec());
                        }
                        Ok(Message::Close(c)) => {
                            *close_code.lock().unwrap() = c.map(|c| c.code.into());
                            break;
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                pending.lock().unwrap().clear();
                closed.notify_waiters();
            });
        }
        Ok(Client {
            tx,
            pending,
            next: std::sync::atomic::AtomicU64::new(1),
            events: tokio::sync::Mutex::new(ev_rx),
            binary: tokio::sync::Mutex::new(bin_rx),
            hello,
            closed,
            close_code,
        })
    }

    /// Send a request; its id.
    pub fn send(&self, method: &str, params: Value) -> (u64, oneshot::Receiver<Result<Value, Remote>>) {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let _ = self.tx.send(Message::text(frame.to_string()));
        (id, rx)
    }

    pub fn notify(&self, method: &str, params: Value) {
        let frame = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let _ = self.tx.send(Message::text(frame.to_string()));
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, Remote> {
        let (_, rx) = self.send(method, params);
        tokio::time::timeout(Duration::from_secs(60), rx)
            .await
            .unwrap_or_else(|_| panic!("{method} timed out"))
            .unwrap_or_else(|_| Err(Remote { code: 0, message: "connection closed".into(), data: None }))
    }

    /// A typed call.
    pub async fn call_m<M: Method>(&self, params: &M::Params) -> Result<M::Result, Remote>
    where
        M::Result: DeserializeOwned,
    {
        let v = self.call(M::NAME, serde_json::to_value(params).unwrap()).await?;
        Ok(serde_json::from_value(v).unwrap_or_else(|e| panic!("{} result: {e}", M::NAME)))
    }

    /// A call that must succeed.
    pub async fn ok(&self, method: &str, params: Value) -> Value {
        match self.call(method, params).await {
            Ok(v) => v,
            Err(e) => panic!("{method} failed: {e:?}"),
        }
    }

    /// A call that must fail; its error.
    pub async fn err(&self, method: &str, params: Value) -> Remote {
        match self.call(method, params).await {
            Ok(v) => panic!("{method} succeeded: {v}"),
            Err(e) => e,
        }
    }

    /// The next event named `name` within `secs` (others are skipped).
    pub async fn event(&self, name: &str, secs: f64) -> Value {
        self.event_where(name, secs, |_| true).await
    }

    pub async fn event_where(&self, name: &str, secs: f64, pred: impl Fn(&Value) -> bool) -> Value {
        let mut rx = self.events.lock().await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(secs);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some((m, p))) if m == name && pred(&p) => return p,
                Ok(Some(_)) => continue,
                Ok(None) => panic!("connection closed while waiting for {name}"),
                Err(_) => panic!("no {name} event within {secs} s"),
            }
        }
    }

    /// Events received so far (drained).
    pub async fn drain_events(&self) -> Vec<(String, Value)> {
        let mut rx = self.events.lock().await;
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    pub async fn binary(&self, secs: f64) -> Vec<u8> {
        let mut rx = self.binary.lock().await;
        tokio::time::timeout(Duration::from_secs_f64(secs), rx.recv())
            .await
            .expect("a binary frame in time")
            .expect("connection open")
    }

    /// Binary frames received so far (drained).
    pub async fn drain_binary(&self) -> Vec<Vec<u8>> {
        let mut rx = self.binary.lock().await;
        let mut out = Vec::new();
        while let Ok(b) = rx.try_recv() {
            out.push(b);
        }
        out
    }

    pub async fn subscribe(&self, topic: &str, params: Value) -> String {
        let v = self.ok("$/subscribe", json!({"topic": topic, "params": params})).await;
        v["sub"].as_str().unwrap().to_owned()
    }

    pub async fn close(self) {
        let _ = self.tx.send(Message::Close(None));
        let _ = tokio::time::timeout(Duration::from_secs(2), self.closed.notified()).await;
    }

    /// Resolves once the server closed the connection.
    pub async fn closed(&self, secs: f64) -> Option<u16> {
        let _ = tokio::time::timeout(Duration::from_secs_f64(secs), self.closed.notified()).await;
        *self.close_code.lock().unwrap()
    }
}

/// A folder tree on disk for scans: `files` as (relative path, size).
pub fn make_tree(root: &std::path::Path, files: &[(&str, usize)]) {
    for (rel, size) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, vec![b'x'; *size]).unwrap();
    }
}

/// A small standard tree: `a/` (two files), `b/c/` (one file), a file at the top.
pub fn sample_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    make_tree(
        dir.path(),
        &[("a/one.txt", 10_000), ("a/two.jpg", 50_000), ("b/c/three.mp4", 200_000), ("top.bin", 1_000)],
    );
    dir
}

/// Start a scan of `path` (in-process, one thread) and wait for it to finish; its status.
pub async fn scan(client: &Client, path: &std::path::Path) -> Value {
    let st = client
        .ok("scan.start", json!({"roots": [path.to_string_lossy()], "workers": 0, "auto_snapshot": false}))
        .await;
    let id = st["scan_id"].as_str().unwrap().to_owned();
    wait_done(client, &id).await
}

pub async fn wait_done(client: &Client, id: &str) -> Value {
    for _ in 0..600 {
        let st = client.ok("scan.status", json!({"scan_id": id})).await;
        let state = st["state"].as_str().unwrap();
        if state != "scanning" && state != "queued" {
            // the finish (compaction, summaries) may still run: wait until it is through
            tokio::time::sleep(Duration::from_millis(50)).await;
            return client.ok("scan.status", json!({"scan_id": id})).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("scan {id} did not finish");
}

/// The JSON header of a binary frame.
pub fn frame_header(buf: &[u8]) -> Value {
    let n = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
    serde_json::from_slice(&buf[4..4 + n]).unwrap()
}

/// What [`build_tree`] made.
pub struct TreeFacts {
    pub root: PathBuf,
    pub files: i64,
    /// folders below the root
    pub dirs: i64,
    pub size: i64,
}

/// A synthetic tree with exactly known contents (Python `tree_fixture.build_tree`).
pub fn build_tree(root: &std::path::Path) -> TreeFacts {
    let mut files: Vec<(String, usize)> = vec![
        ("a/one.bin".into(), 1000),
        ("a/two.bin".into(), 5000),
        ("a/sub/movie.mp4".into(), 123456),
        ("c.txt".into(), 10),
        ("中文 文件夹/照片.jpg".into(), 2048),
        ("deep/l1/l2/l3/l4/x.dat".into(), 777),
        ("empty.dat".into(), 0),
    ];
    files.extend((0..300).map(|i| (format!("many/f{i:03}.log"), 100 + i)));
    let extra = ["b", "b/inner"];
    let mut dirs: std::collections::BTreeSet<String> = extra.iter().map(|s| (*s).to_owned()).collect();
    for (rel, size) in &files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, vec![b'x'; *size]).unwrap();
        let mut parent = std::path::Path::new(rel).parent();
        while let Some(d) = parent.filter(|d| !d.as_os_str().is_empty()) {
            dirs.insert(d.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"));
            parent = d.parent();
        }
    }
    for d in extra {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    TreeFacts {
        root: root.to_path_buf(),
        files: files.len() as i64,
        dirs: dirs.len() as i64,
        size: files.iter().map(|f| f.1 as i64).sum(),
    }
}

/// Subscribe to a scan and wait for its final status (with `snapshot` when `snapshot`).
pub async fn wait_final(client: &Client, scan_id: &str, snapshot: bool) -> Value {
    client.subscribe("scan", json!({"scan_id": scan_id})).await;
    let id = scan_id.to_owned();
    let ev = client
        .event_where("scan.progress", 60.0, move |p| {
            let s = &p["status"];
            s["scan_id"] == id.as_str()
                && s["state"] != "scanning"
                && s["state"] != "queued"
                && (!snapshot || !s["snapshot"].is_null())
        })
        .await;
    ev["status"].clone()
}
