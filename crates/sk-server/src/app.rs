//! The server: [`start`] binds, serves `/v1` (and, headless, the interface), owns the sessions and
//! the managers every session shares (Python `server/app.py`).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use sk_core::log::LogBus;
use sk_core::settings::SettingsStore;
use sk_protocol::methods::SettingsChanged;
use sk_protocol::session::ServerInfo;
use sk_protocol::settings::SettingsChangedEvent;
use sk_protocol::{ALL_CAPABILITIES, CloseCode, WS_PATH};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::compares::CompareManager;
use crate::monitors::MonitorManager;
use crate::ops_service::OpsService;
use crate::rpc::Router;
use crate::scans::ScanManager;
use crate::session::Session;
use crate::topics::TopicRegistry;
use crate::updates::UpdateService;
use crate::views::ViewManager;
use crate::volume_summaries::VolumeSummaries;

/// Origins a server on a loopback address accepts besides those of [`ServeOptions::allow_origins`]:
/// the app's own window (Tauri) and the Electron shell's app scheme. A server on any other address
/// doesn't take them (they would be another computer's app). A request without `Origin` (a native
/// client) is accepted too, except in headless mode ([`crate::http::ws_origin_ok`]).
pub const DEFAULT_ORIGINS: [&str; 4] = [
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
    "app://spaceknife",
];

/// How [`start`] serves (the contract with the GUI and the CLI).
#[derive(Clone, Debug)]
pub struct ServeOptions {
    pub host: IpAddr,
    /// 0: a free port
    pub port: u16,
    /// the secret `session.hello` must carry (and headless `/sk/info` the Bearer token)
    pub token: String,
    /// more allowed WebSocket origins (a dev server)
    pub allow_origins: Vec<String>,
    /// the running app's version (update checks); None: the first `updates.v1` client's
    pub app_version: Option<String>,
    /// `nsis` / `portable` / `appimage` / `deb` / `rpm` / `dmg`; None: `dev`
    pub install_kind: Option<String>,
    /// the system's locale as the launcher read it (the interface language «system»)
    pub system_locale: Option<String>,
    /// headless: serve the built interface from this folder (HTTP + `/sk/info` + `/v1`)
    pub static_dir: Option<PathBuf>,
    /// HTTPS / WSS: (certificate chain PEM, private key PEM)
    pub tls: Option<(PathBuf, PathBuf)>,
}

impl Default for ServeOptions {
    fn default() -> Self {
        ServeOptions {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            token: crate::util::new_token(),
            allow_origins: Vec::new(),
            app_version: None,
            install_kind: None,
            system_locale: None,
            static_dir: None,
            tls: None,
        }
    }
}

/// Adds or overrides methods before the server starts ([`Tuning::routes`]).
pub type RoutesHook = Box<dyn FnOnce(&mut Router) + Send>;

/// Knobs beyond [`ServeOptions`] (tests and embedders).
pub struct Tuning {
    pub hello_timeout: Duration,
    pub max_inbound_bytes: usize,
    pub max_outbound_chunk_bytes: usize,
    pub capabilities: Vec<String>,
    /// automatic update checks and downloads (the real app; not tests)
    pub update_checks: bool,
    /// start disk-load sampling (the W2 baseline)
    pub io_sampling: bool,
    /// add or override methods before the server starts (tests: `test.sleep`)
    pub routes: Option<RoutesHook>,
    /// the settings file (None: `config_dir()/settings.json`)
    pub settings_file: Option<PathBuf>,
    /// the log bus the `log` topic reads (None: [`LogBus::global`])
    pub log_bus: Option<Arc<LogBus>>,
    /// Test seam: every scan's [`sk_core::scan::ScanOptions::fault`] (a panic in a step of the
    /// scan, to see the server go on).
    #[doc(hidden)]
    pub scan_fault: Option<sk_core::scan::ScanFault>,
    /// Test seam: the disk-load sampler (None: the platform's,
    /// [`sk_core::io::default_sampler`]); a slow one shows what waits for its start.
    #[doc(hidden)]
    pub io_sampler: Option<IoSamplerFactory>,
    /// what a server reachable from elsewhere allows each client (headless mode, an address other
    /// than loopback: [`crate::http::serve_guarded`])
    pub limits: crate::http::Limits,
    /// told when a browser's WebSocket opens and closes (headless mode prints it); only for the
    /// connections [`crate::http::serve_guarded`] serves, which know their remote address
    pub on_socket: Option<crate::http::SocketHook>,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            hello_timeout: Duration::from_secs(5),
            max_inbound_bytes: 1 << 20,
            max_outbound_chunk_bytes: 4 << 20,
            capabilities: ALL_CAPABILITIES.iter().map(|s| (*s).to_owned()).collect(),
            update_checks: true,
            io_sampling: true,
            routes: None,
            settings_file: None,
            log_bus: None,
            scan_fault: None,
            io_sampler: None,
            limits: crate::http::Limits::default(),
            on_socket: None,
        }
    }
}

/// Makes the disk-load sampler ([`Tuning::io_sampler`]).
pub type IoSamplerFactory = Arc<dyn Fn() -> Box<dyn sk_core::io::DiskSampler> + Send + Sync>;

impl Tuning {
    /// What tests use: a short hello timeout, no update checks, no disk sampling.
    pub fn for_tests() -> Tuning {
        Tuning {
            hello_timeout: Duration::from_millis(500),
            update_checks: false,
            io_sampling: false,
            ..Tuning::default()
        }
    }
}

/// The server's fixed configuration.
pub struct Config {
    pub token: String,
    pub host: IpAddr,
    pub allowed_origins: Vec<String>,
    pub hello_timeout: Duration,
    pub max_inbound_bytes: usize,
    pub max_outbound_chunk_bytes: usize,
    pub capabilities: Vec<String>,
    pub update_checks: bool,
    pub io_sampling: bool,
    pub app_version: Option<String>,
    pub install_kind: Option<String>,
    pub static_dir: Option<PathBuf>,
    pub secure: bool,
    /// [`Tuning::scan_fault`]
    pub scan_fault: Option<sk_core::scan::ScanFault>,
    /// [`Tuning::io_sampler`]
    pub io_sampler: Option<IoSamplerFactory>,
    pub on_socket: Option<crate::http::SocketHook>,
}

/// Everything the sessions share.
pub struct Server {
    pub config: Config,
    pub router: Router,
    pub topics: TopicRegistry,
    pub settings: Arc<SettingsStore>,
    pub log_bus: Arc<LogBus>,
    pub scans: ScanManager,
    pub views: ViewManager,
    pub monitors: MonitorManager,
    pub compares: CompareManager,
    pub ops: OpsService,
    pub updates: UpdateService,
    pub summaries: VolumeSummaries,
    pub server_info: ServerInfo,
    pub schema_hash: String,
    pub started_at: sk_protocol::DateTime,
    sessions: Mutex<Vec<Weak<Session>>>,
    background: Mutex<Vec<JoinHandle<()>>>,
    shutdown: Notify,
    shutdown_requested: AtomicBool,
    port: std::sync::atomic::AtomicU16,
    /// the tokio runtime the server runs on (callbacks from other threads spawn here)
    pub rt: tokio::runtime::Handle,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("url", &self.url())
            .finish_non_exhaustive()
    }
}

impl Server {
    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    /// `ws://host:port/v1` (`wss://` with TLS).
    pub fn url(&self) -> String {
        let host = match self.config.host {
            IpAddr::V6(h) => format!("[{h}]"),
            IpAddr::V4(h) => h.to_string(),
        };
        let scheme = if self.config.secure { "wss" } else { "ws" };
        format!("{scheme}://{host}:{}{WS_PATH}", self.port())
    }

    /// Open sessions.
    pub fn sessions(&self) -> Vec<Arc<Session>> {
        self.sessions
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }

    pub(crate) fn add_session(&self, s: &Arc<Session>) {
        let mut g = self.sessions.lock();
        g.retain(|w| w.strong_count() > 0);
        g.push(Arc::downgrade(s));
    }

    pub(crate) fn remove_session(self: &Arc<Self>, s: &Arc<Session>) {
        self.sessions
            .lock()
            .retain(|w| w.strong_count() > 0 && !std::ptr::eq(w.as_ptr(), Arc::as_ptr(s)));
        self.views.close_session(&s.id);
        self.compares.close_session(self, &s.id);
    }

    /// Send an event to every session (optionally only those with a capability).
    pub fn broadcast<E: sk_protocol::methods::Event>(
        &self,
        payload: &E::Payload,
        cap: Option<&str>,
    ) {
        for s in self.sessions() {
            if cap.is_none_or(|c| s.has_cap(c)) {
                s.emit::<E>(payload);
            }
        }
    }

    /// Keep a background task until the server closes.
    pub fn spawn_background(&self, fut: impl std::future::Future<Output = ()> + Send + 'static) {
        let task = self.rt.spawn(fut);
        self.background.lock().push(task);
    }

    /// Ask the server to stop (the `wait` of its handle returns once it did).
    pub fn request_shutdown(&self) {
        if !self.shutdown_requested.swap(true, Ordering::SeqCst) {
            tracing::info!("shutdown requested");
            self.shutdown.notify_waiters();
        }
    }

    pub fn shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::SeqCst)
    }

    /// Resolves once a shutdown was requested.
    pub async fn wait_shutdown(&self) {
        loop {
            let n = self.shutdown.notified();
            if self.shutdown_requested() {
                return;
            }
            n.await;
        }
    }

    async fn close(self: &Arc<Self>) {
        for s in self.sessions() {
            s.close(CloseCode::GoingAway, "server shutting down");
        }
        for t in self.background.lock().drain(..) {
            t.abort();
        }
        self.updates.close();
        self.scans.close(self).await;
        // give the sessions a moment to say goodbye
        for _ in 0..50 {
            if self.sessions().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tracing::info!("server stopped");
    }
}

/// A started server.
pub struct ServerHandle {
    /// `ws://host:port/v1`
    pub url: String,
    pub port: u16,
    pub pid: u32,
    /// where update files are downloaded (the launcher starts only files from there)
    pub updates_dir: PathBuf,
    pub server: Arc<Server>,
    task: JoinHandle<()>,
}

impl ServerHandle {
    /// Ask the server to stop (sessions are closed with 1001).
    pub fn shutdown(&self) {
        self.server.request_shutdown();
    }

    /// Resolves once the server stopped (after [`ServerHandle::shutdown`], `app.shutdown`, …).
    pub async fn wait(self) {
        let _ = self.task.await;
    }
}

/// Start serving with [`Tuning::default`]; returns once listening.
pub async fn start(opts: ServeOptions) -> anyhow::Result<ServerHandle> {
    start_with(opts, Tuning::default()).await
}

/// Start serving; returns once listening.
pub async fn start_with(opts: ServeOptions, tuning: Tuning) -> anyhow::Result<ServerHandle> {
    if opts.token.len() < 16 {
        anyhow::bail!("token must be at least 16 characters");
    }
    if !opts.host.is_loopback() {
        tracing::warn!(
            "binding to non-loopback host {} exposes the backend to the network",
            opts.host
        );
    }
    if let Some(locale) = opts.system_locale.as_deref().filter(|l| !l.is_empty()) {
        // a value, never the environment: the server may run inside the GUI process, whose
        // toolkit threads read the environment (getenv) meanwhile; a SPACEKNIFE_SYSTEM_LOCALE the
        // process was started with still comes first
        sk_core::i18n::set_system_locale(Some(locale));
    }
    let tls = match &opts.tls {
        Some((cert, key)) => Some(crate::http::tls_acceptor(cert, key)?),
        None => None,
    };
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(opts.host, opts.port)).await?;
    let port = listener.local_addr()?.port();

    let origins = allowed_origins(opts.host, &opts.allow_origins);
    // reachable from elsewhere, or a browser's page: guarded as 0.3.4's headless server was
    let guarded = guarded(&opts, tls.is_some());
    let limits = tuning.limits;
    let config = Config {
        token: opts.token.clone(),
        host: opts.host,
        allowed_origins: origins,
        hello_timeout: tuning.hello_timeout,
        max_inbound_bytes: tuning.max_inbound_bytes,
        max_outbound_chunk_bytes: tuning.max_outbound_chunk_bytes,
        capabilities: tuning.capabilities,
        update_checks: tuning.update_checks,
        io_sampling: tuning.io_sampling,
        app_version: opts.app_version.clone(),
        install_kind: opts.install_kind.clone(),
        static_dir: opts.static_dir.clone(),
        secure: tls.is_some(),
        scan_fault: tuning.scan_fault,
        io_sampler: tuning.io_sampler,
        on_socket: tuning.on_socket,
    };
    let server = build(config, tuning.routes, tuning.settings_file, tuning.log_bus);
    server.port.store(port, Ordering::SeqCst);
    after_start(&server);

    let app = crate::http::router(server.clone());
    let srv = server.clone();
    let task = tokio::spawn(async move {
        let stop = srv.clone();
        let shutdown = async move { stop.wait_shutdown().await };
        if guarded {
            crate::http::serve_guarded(listener, app, tls, limits, shutdown).await;
        } else if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
        {
            tracing::error!("serving stopped: {e}");
        }
        srv.close().await;
    });
    tracing::info!("listening on {}", server.url());
    Ok(ServerHandle {
        url: server.url(),
        port,
        pid: std::process::id(),
        updates_dir: server.updates.updates_dir(),
        server,
        task,
    })
}

/// The WebSocket origins a server on `host` lets in by name: the app's own windows only on a
/// loopback address, and those the launcher names.
pub fn allowed_origins(host: IpAddr, extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = if host.to_canonical().is_loopback() {
        DEFAULT_ORIGINS.iter().map(|s| (*s).to_owned()).collect()
    } else {
        Vec::new()
    };
    out.extend(extra.iter().cloned());
    out
}

/// Whether a server is served by [`crate::http::serve_guarded`]: headless mode (a browser's page,
/// HTTPS) or an address other computers reach. The app's own backend on loopback keeps axum's
/// server.
pub fn guarded(opts: &ServeOptions, tls: bool) -> bool {
    opts.static_dir.is_some() || tls || !opts.host.to_canonical().is_loopback()
}

fn build(
    config: Config,
    routes: Option<RoutesHook>,
    settings_file: Option<PathBuf>,
    log_bus: Option<Arc<LogBus>>,
) -> Arc<Server> {
    let mut router = Router::new();
    let mut topics = TopicRegistry::default();
    crate::handlers::install(&mut router, &mut topics);
    if let Some(f) = routes {
        f(&mut router);
    }
    let settings = Arc::new(SettingsStore::new(settings_file));
    sk_core::i18n::set_language(settings.language());
    crate::handlers::classes::load_saved();
    let policy = sk_store::Policy::global();
    policy.set_columns_dir(sk_core::paths::cache_dir().join("columns"));
    let stale = policy.cleanup_stale();
    if stale > 0 {
        tracing::debug!("removed {stale} stale column files");
    }
    let rt = tokio::runtime::Handle::current();
    Arc::new(Server {
        scans: ScanManager::new(&config, &settings),
        views: ViewManager::new(),
        monitors: MonitorManager::new(),
        compares: CompareManager::new(&config, &settings),
        ops: OpsService::new(&config, &settings),
        updates: UpdateService::new(&config, &settings),
        summaries: VolumeSummaries::new(&config, &settings),
        server_info: ServerInfo {
            name: "spaceknife-backend".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            rust: env!("SK_RUSTC_VERSION").to_owned(),
            platform: platform_name().to_owned(),
        },
        schema_hash: sk_protocol::schema::schema_hash().to_owned(),
        started_at: chrono::Utc::now(),
        config,
        router,
        topics,
        settings,
        log_bus: log_bus.unwrap_or_else(LogBus::global),
        sessions: Mutex::new(Vec::new()),
        background: Mutex::new(Vec::new()),
        shutdown: Notify::new(),
        shutdown_requested: AtomicBool::new(false),
        port: std::sync::atomic::AtomicU16::new(0),
        rt,
    })
}

/// Settings listeners, background tasks: once the server is in its `Arc`.
fn after_start(server: &Arc<Server>) {
    let weak = Arc::downgrade(server);
    server.settings.add_listener("server", move |st| {
        let Some(server) = weak.upgrade() else { return };
        sk_core::i18n::set_language(st.language()); // first: the state is built in the new language
        let state = st.state();
        server.broadcast::<SettingsChanged>(&SettingsChangedEvent { state }, None);
        crate::handlers::settings::apply_live(&server, st);
    });
    crate::handlers::settings::apply_live(server, &server.settings);
    // external edits of the settings file
    let weak = Arc::downgrade(server);
    server.spawn_background(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let Some(server) = weak.upgrade() else { return };
            let st = server.settings.clone();
            drop(server);
            let _ = tokio::task::spawn_blocking(move || st.poll()).await;
        }
    });
    if server.config.io_sampling {
        server.scans.start_io(server);
    }
    server.updates.started(server);
    server.summaries.started(server);
    crate::diag::start(server);
}

/// `sys.platform` as the Python backend reported it.
pub fn platform_name() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// The app's own windows (tauri://, app://) are this computer's: not on a network address,
    /// where they would be any other computer's app.
    #[test]
    fn the_apps_origins_only_on_loopback() {
        let dev = vec!["http://localhost:3000".to_owned()];
        for host in ["127.0.0.1", "::1", "::ffff:127.0.0.1"] {
            let all = allowed_origins(ip(host), &dev);
            assert!(all.iter().any(|o| o == "tauri://localhost"), "{host}");
            assert!(all.iter().any(|o| o == "app://spaceknife"), "{host}");
            assert!(all.contains(&dev[0]), "{host}");
        }
        for host in ["0.0.0.0", "::", "192.168.1.20", "fd00::5"] {
            assert_eq!(allowed_origins(ip(host), &dev), dev, "{host}");
        }
    }

    #[test]
    fn what_is_guarded() {
        let at = |host: &str, ui: bool| ServeOptions {
            host: ip(host),
            static_dir: ui.then(|| PathBuf::from("ui")),
            ..ServeOptions::default()
        };
        // the app's backend on this computer: as before
        assert!(!guarded(&at("127.0.0.1", false), false));
        assert!(!guarded(&at("::1", false), false));
        // a browser's page, HTTPS, an address others reach
        assert!(guarded(&at("127.0.0.1", true), false));
        assert!(guarded(&at("127.0.0.1", false), true));
        assert!(guarded(&at("0.0.0.0", false), false));
        assert!(guarded(&at("192.168.1.20", false), false));
    }
}
