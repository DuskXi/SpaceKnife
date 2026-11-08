//! Update checks, their schedule and the one-click download (`updates.v1`; Python
//! `server/update_handlers.py`).
//!
//! [`UpdateService`] belongs to the server, one per backend and shared by every session. It keeps
//! the state between runs ([`sk_core::update::UpdateState`]), runs the checks and the download,
//! and sends `update.changed` to every session that announced `updates.v1` whenever any of it
//! changes (a check starts or ends, the download moves on, a card is closed, a setting changes).
//!
//! Schedule (「自动检查更新」 on): the first automatic check comes [`FIRST_DELAY`] after start, and
//! only when the last successful one is older than [`update::CHECK_INTERVAL`]; then once per
//! interval; a failed check is tried again after [`update::RETRY_INTERVAL`]. Switching the setting
//! on or changing the channel checks right away. `update.check` always checks.
//!
//! In the real app ([`UpdateService::start`], when `Config::update_checks`) the service also cleans
//! the `updates` folder once at start, and with 「自动下载更新」 on downloads a newer release's file
//! by itself. Tests and demos without the scheduler never download by themselves.
//!
//! The network calls are blocking: they run on tokio's blocking threads.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::{Mutex, RwLock};
use sk_core::settings::SettingsStore;
use sk_core::tr;
use sk_core::update::{self as update, Asset, Fetched, Release, Shell, UpdateError, UpdateState};
use sk_protocol::CAP_UPDATES;
use sk_protocol::methods::UpdateChanged;
use sk_protocol::update::{
    DownloadStateName, InstallKind, UpdateChangedEvent, UpdateChannel, UpdateDownload, UpdateLevel,
    UpdateReleases, UpdateStatus,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::app::{Config, Server};
use crate::rpc::{RpcError, RpcResult};

/// After start, before the first automatic check.
pub const FIRST_DELAY: Duration = Duration::from_secs(60);

/// The release list: (feed, etag, user agent) → list (blocking).
pub type Fetch =
    Arc<dyn Fn(&str, Option<&str>, &str) -> Result<Fetched, UpdateError> + Send + Sync>;
/// The published SHA-256 of a release's file: (release, asset, user agent) (blocking).
pub type Checksum =
    Arc<dyn Fn(&Release, &Asset, &str) -> Result<Option<String>, UpdateError> + Send + Sync>;
/// The time (tests move it).
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

type SettingsKey = (bool, UpdateChannel, &'static str, bool);

struct Inner {
    state: UpdateState,
    file: PathBuf,
    updates_dir: PathBuf,
    kind: InstallKind,
    /// the shell hosting the interface: with the kind, it decides the release file
    shell: Shell,
    /// (system, machine) the release file is for; None: this platform, this processor (tests set them)
    platform: (Option<String>, Option<String>),
    app_version: Option<String>,
    /// downloads by itself (「自动下载更新」): the real app only (start)
    auto: bool,
    /// of the last check, None after a success
    error: Option<UpdateError>,
    checking: bool,
    started: u64,
    download: UpdateDownload,
    download_error: Option<UpdateError>,
    cancel: Arc<AtomicBool>,
    /// the download whose progress counts (a cancelled one stops reporting)
    run: u64,
    /// (version, file) → published SHA-256
    sums: Vec<((String, String), Option<String>)>,
    /// the next scheduler round checks even if not due
    force: bool,
    applied: Option<SettingsKey>,
}

/// One per backend (see the module docs); derefs to the shared [`UpdateCore`].
pub struct UpdateService {
    core: Arc<UpdateCore>,
}

impl UpdateService {
    pub fn new(config: &Config, settings: &Arc<SettingsStore>) -> UpdateService {
        UpdateService {
            core: Arc::new(UpdateCore::new(config, settings)),
        }
    }
}

impl std::ops::Deref for UpdateService {
    type Target = Arc<UpdateCore>;
    fn deref(&self) -> &Arc<UpdateCore> {
        &self.core
    }
}

/// The state and work of the update service.
pub struct UpdateCore {
    settings: Arc<SettingsStore>,
    feed: String,
    scheduled: bool,
    inner: Mutex<Inner>,
    fetch: RwLock<Fetch>,
    checksum: RwLock<Checksum>,
    clock: RwLock<Clock>,
    first_delay: Mutex<Duration>,
    server: OnceLock<Weak<Server>>,
    done: tokio::sync::watch::Sender<u64>,
    wake: Notify,
    save_lock: Mutex<()>,
    scheduler: Mutex<Option<JoinHandle<()>>>,
    download_task: Mutex<Option<JoinHandle<()>>>,
}

fn is(d: &UpdateDownload, states: &[DownloadStateName]) -> bool {
    states.contains(&d.state)
}

impl UpdateCore {
    pub fn new(config: &Config, settings: &Arc<SettingsStore>) -> UpdateCore {
        let feed = update::feed_url();
        let file = update::state_file();
        let state = update::load_state(&file, &feed);
        let kind = match &config.install_kind {
            Some(k) => update::install_kind_from(k),
            None => update::install_kind(),
        };
        let (done, _) = tokio::sync::watch::channel(0);
        UpdateCore {
            settings: settings.clone(),
            feed,
            scheduled: config.update_checks,
            inner: Mutex::new(Inner {
                state,
                file,
                updates_dir: update::updates_dir(),
                kind,
                // SPACEKNIFE_SHELL from the Electron shell; unset in the Tauri app's own process
                shell: update::shell(),
                platform: (None, None),
                app_version: config.app_version.clone().or_else(update::app_version),
                auto: false,
                error: None,
                checking: false,
                started: 0,
                download: UpdateDownload::default(),
                download_error: None,
                cancel: Arc::new(AtomicBool::new(false)),
                run: 0,
                sums: Vec::new(),
                force: false,
                applied: None,
            }),
            fetch: RwLock::new(Arc::new(|feed, etag, ua| {
                update::fetch_releases(feed, etag, ua, update::TIMEOUT)
            })),
            checksum: RwLock::new(Arc::new(|r, a, ua| {
                update::fetch_checksums(r, a, ua, update::TIMEOUT)
            })),
            clock: RwLock::new(Arc::new(Utc::now)),
            first_delay: Mutex::new(FIRST_DELAY),
            server: OnceLock::new(),
            done,
            wake: Notify::new(),
            save_lock: Mutex::new(()),
            scheduler: Mutex::new(None),
            download_task: Mutex::new(None),
        }
    }

    // ------------------------------------------------------------- seams (tests, demos)

    pub fn set_fetch(&self, f: Fetch) {
        *self.fetch.write() = f;
    }

    pub fn set_checksum(&self, f: Checksum) {
        *self.checksum.write() = f;
    }

    pub fn set_clock(&self, f: Clock) {
        *self.clock.write() = f;
    }

    pub fn set_first_delay(&self, d: Duration) {
        *self.first_delay.lock() = d;
    }

    pub fn set_install_kind(&self, kind: InstallKind) {
        self.inner.lock().kind = kind;
    }

    pub fn set_shell(&self, shell: Shell) {
        self.inner.lock().shell = shell;
    }

    /// Pick the release files of another platform (`sys.platform` names) and processor.
    pub fn set_platform(&self, system: Option<String>, machine: Option<String>) {
        self.inner.lock().platform = (system, machine);
    }

    pub fn set_app_version(&self, v: Option<String>) {
        self.inner.lock().app_version = v;
    }

    /// Download by itself after checks (what [`UpdateService::start`] turns on).
    pub fn set_auto(&self, on: bool) {
        self.inner.lock().auto = on;
    }

    /// Keep the state in `file` and download to `updates_dir` (the state is read from `file`).
    pub fn relocate(&self, file: PathBuf, updates_dir: PathBuf) {
        let mut g = self.inner.lock();
        g.state = update::load_state(&file, &self.feed);
        g.file = file;
        g.updates_dir = updates_dir;
    }

    /// The saved state (tests).
    pub fn state(&self) -> UpdateState {
        self.inner.lock().state.clone()
    }

    pub fn state_file(&self) -> PathBuf {
        self.inner.lock().file.clone()
    }

    /// Change the saved state in memory (tests: an older cache).
    pub fn edit_state(&self, f: impl FnOnce(&mut UpdateState)) {
        f(&mut self.inner.lock().state);
    }

    /// Wait until the running download (if any) has finished, its partial file removed.
    pub async fn wait_download(&self) {
        let task = self.download_task.lock().take();
        if let Some(t) = task {
            let _ = t.await;
        }
    }

    /// The scheduler task runs.
    pub fn scheduling(&self) -> bool {
        self.scheduler
            .lock()
            .as_ref()
            .is_some_and(|t| !t.is_finished())
    }

    // ------------------------------------------------------------- state

    fn server(&self) -> Option<Arc<Server>> {
        self.server.get().and_then(Weak::upgrade)
    }

    pub fn enabled(&self) -> bool {
        self.settings.get_bool("general.update_check")
    }

    pub fn auto_download(&self) -> bool {
        self.settings.get_bool("general.update_download")
    }

    pub fn channel(&self) -> UpdateChannel {
        if self.settings.get_str("general.update_channel") == "stable" {
            UpdateChannel::Stable
        } else {
            UpdateChannel::Preview
        }
    }

    fn settings_key(&self) -> SettingsKey {
        (
            self.enabled(),
            self.channel(),
            sk_core::i18n::language().as_str(),
            self.auto_download(),
        )
    }

    fn now(&self) -> DateTime<Utc> {
        (self.clock.read())()
    }

    /// The running app's version: the launcher's, else what the first session that announced
    /// `updates.v1` said in its hello.
    pub fn current(&self) -> Option<String> {
        if let Some(v) = self.inner.lock().app_version.clone() {
            return Some(v);
        }
        let server = self.server()?;
        let found = server
            .sessions()
            .into_iter()
            .find(|s| s.has_cap(CAP_UPDATES) && s.client().is_some())
            .and_then(|s| s.client().map(|c| c.version.clone()));
        if found.is_some() {
            self.inner.lock().app_version = found.clone();
        }
        found
    }

    fn user_agent(&self) -> String {
        format!(
            "SpaceKnife/{}",
            self.current()
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
        )
    }

    /// The newest release of the channel when it is newer than this app, and its file for this
    /// kind of install.
    fn newer(&self, g: &Inner, current: Option<&str>) -> Option<(Release, Option<Asset>)> {
        let release = update::choose(&g.state.releases, self.channel())?;
        if update::level(current, Some(&release.version)) == UpdateLevel::None {
            return None;
        }
        let (system, machine) = (g.platform.0.as_deref(), g.platform.1.as_deref());
        let asset = update::pick_asset(release, g.kind, g.shell, system, machine).cloned();
        Some((release.clone(), asset))
    }

    pub fn status(&self) -> UpdateStatus {
        let current = self.current();
        let due = if self.inner.lock().auto {
            self.due_in()
        } else {
            None
        }; // tests check only when asked
        let enabled = self.enabled();
        let channel = self.channel();
        let g = self.inner.lock();
        let latest = self.newer(&g, current.as_deref()).map(|(release, asset)| {
            let sha = asset.as_ref().and_then(|a| {
                g.sums
                    .iter()
                    .find(|(k, _)| k.0 == release.version && k.1 == a.name)
                    .and_then(|(_, s)| s.clone())
            });
            update::to_info(&release, asset.as_ref(), sha.as_deref())
        });
        let mut download = g.download.clone();
        if let Some(e) = &g.download_error {
            download.error = Some(e.message());
        }
        let latest_version = latest.as_ref().map(|l| l.version.clone());
        let series = latest_version
            .as_deref()
            .and_then(|v| update::series(current.as_deref(), Some(v)));
        UpdateStatus {
            current: current.clone().unwrap_or_default(),
            install_kind: g.kind,
            enabled,
            channel,
            checking: g.checking,
            checked_at: g.state.checked_at,
            error: g.error.as_ref().map(UpdateError::message),
            level: latest_version.as_deref().map_or(UpdateLevel::None, |v| {
                update::level(current.as_deref(), Some(v))
            }),
            dismissed: latest_version
                .as_ref()
                .is_some_and(|v| g.state.dismissed.contains(v)),
            muted: series.as_ref().is_some_and(|s| g.state.muted.contains(s)),
            series,
            latest,
            download,
            next_check_at: due
                .and_then(|d| chrono::TimeDelta::from_std(Duration::from_secs_f64(d)).ok())
                .map(|d| self.now() + d),
        }
    }

    /// The cached releases, newest first, for the timeline (no asset: nothing to download there).
    pub fn releases(&self) -> UpdateReleases {
        let g = self.inner.lock();
        UpdateReleases {
            releases: g
                .state
                .releases
                .iter()
                .map(|r| update::to_info(r, None, None))
                .collect(),
        }
    }

    /// `update.changed` to every session that announced `updates.v1`.
    pub fn broadcast(&self) {
        let Some(server) = self.server() else { return };
        if !server.sessions().iter().any(|s| s.has_cap(CAP_UPDATES)) {
            return;
        }
        server.broadcast::<UpdateChanged>(
            &UpdateChangedEvent {
                status: self.status(),
            },
            Some(CAP_UPDATES),
        );
    }

    async fn save(self: &Arc<Self>) {
        let (state, file) = {
            let g = self.inner.lock();
            (g.state.clone(), g.file.clone())
        };
        let me = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _g = me.save_lock.lock();
            if let Err(e) = update::save_state(&state, &file) {
                tracing::warn!("could not save the update state to {}: {e}", file.display());
            }
        })
        .await;
    }

    // ------------------------------------------------------------- check

    /// Check the release list now (joins a check that is already running).
    pub async fn check(self: &Arc<Self>) -> UpdateStatus {
        let (target, fresh) = {
            let mut g = self.inner.lock();
            if g.checking {
                (g.started, false)
            } else {
                g.checking = true;
                g.started += 1;
                (g.started, true)
            }
        };
        if fresh {
            let me = self.clone();
            tokio::spawn(async move { me.run_check(target).await });
            self.broadcast(); // checking
        }
        let mut rx = self.done.subscribe();
        let _ = rx.wait_for(|d| *d >= target).await;
        self.status()
    }

    /// The ETag to send: only for a cache trimmed as this version trims it, else the whole list
    /// comes again (an unchanged list would answer 304 and keep a shorter timeline).
    fn cached_etag(st: &UpdateState) -> Option<String> {
        (!st.releases.is_empty() && st.kept == update::HISTORY_KEEP)
            .then(|| st.etag.clone())
            .flatten()
    }

    async fn run_check(self: Arc<Self>, target: u64) {
        let etag = Self::cached_etag(&self.inner.lock().state);
        let fetch = self.fetch.read().clone();
        let (feed, ua) = (self.feed.clone(), self.user_agent());
        let got = tokio::task::spawn_blocking(move || fetch(&feed, etag.as_deref(), &ua))
            .await
            .unwrap_or_else(|e| {
                Err(UpdateError::Failed {
                    reason: e.to_string(),
                })
            });
        let now = self.now();
        let current = self.current();
        {
            let mut g = self.inner.lock();
            match got {
                Err(e) => {
                    tracing::warn!("update check failed: {e}");
                    g.error = Some(e);
                    g.state.failed_at = Some(now);
                }
                Ok(f) => {
                    if let Some(list) = f.releases {
                        g.state.releases = update::trim(&list);
                        g.state.etag = f.etag;
                        g.state.kept = update::HISTORY_KEEP;
                    }
                    g.state.checked_at = Some(now);
                    g.state.failed_at = None;
                    g.error = None;
                }
            }
            g.checking = false;
            let found = self.newer(&g, current.as_deref());
            if let Some((r, _)) = &found {
                tracing::info!(
                    "update available: {} -> {}",
                    current.as_deref().unwrap_or("?"),
                    r.version
                );
            }
            let d = &g.download;
            let idle = !is(
                d,
                &[DownloadStateName::Downloading, DownloadStateName::Verifying],
            );
            if idle
                && d.version.is_some()
                && found
                    .as_ref()
                    .is_none_or(|(r, _)| Some(&r.version) != d.version.as_ref())
            {
                g.download = UpdateDownload::default(); // another release now
                g.download_error = None;
            }
        }
        self.save().await;
        self.done.send_replace(target);
        self.broadcast();
        self.auto_fetch().await;
    }

    /// 「自动下载更新」: start downloading a newer release's file, unless it is running, done or
    /// failed for that version already (a failed one waits for the next check or a click).
    pub async fn auto_fetch(self: &Arc<Self>) {
        if !(self.inner.lock().auto && self.auto_download()) {
            return;
        }
        let current = self.current();
        let version = {
            let g = self.inner.lock();
            let Some((release, Some(_))) = self.newer(&g, current.as_deref()) else {
                return;
            };
            if let Some(s) = update::series(current.as_deref(), Some(&release.version))
                && g.state.muted.contains(&s)
            {
                return; // the user doesn't want this series: nothing by itself
            }
            let d = &g.download;
            let busy = [
                DownloadStateName::Downloading,
                DownloadStateName::Verifying,
                DownloadStateName::Done,
                DownloadStateName::Failed,
            ];
            if d.version.as_deref() == Some(release.version.as_str()) && is(d, &busy) {
                return;
            }
            release.version
        };
        if let Err(e) = self.download(&version).await {
            tracing::info!("no automatic download of {version}: {e}"); // the list changed meanwhile
        }
    }

    /// The card / dialog of `version` was closed: not shown again for it (the badge stays).
    pub async fn dismiss(self: &Arc<Self>, version: &str) -> UpdateStatus {
        let changed = {
            let mut g = self.inner.lock();
            let closed = &mut g.state.dismissed;
            if closed.iter().any(|v| v == version) {
                false
            } else {
                closed.push(version.to_owned());
                let extra = closed.len().saturating_sub(update::KEEP_DISMISSED);
                closed.drain(..extra);
                true
            }
        };
        if changed {
            self.save().await;
            self.broadcast();
        }
        self.status()
    }

    /// Turn the prompts of `series` off (`muted`) or back on. Series the running version has
    /// reached are dropped on the way.
    pub async fn mute(self: &Arc<Self>, series: &str, muted: bool) -> UpdateStatus {
        let current = self.current();
        let changed = {
            let mut g = self.inner.lock();
            let mut names: Vec<String> = g
                .state
                .muted
                .iter()
                .filter(|s| *s != series && !update::series_reached(s, current.as_deref()))
                .cloned()
                .collect();
            if muted {
                names.push(series.to_owned());
            }
            let extra = names.len().saturating_sub(update::KEEP_MUTED);
            names.drain(..extra);
            if names != g.state.muted {
                g.state.muted = names;
                true
            } else {
                false
            }
        };
        if changed {
            tracing::info!(
                "update prompts of {series}.x {}",
                if muted { "off" } else { "on again" }
            );
            self.save().await;
            self.broadcast();
            if !muted {
                self.auto_fetch().await;
            }
        }
        self.status()
    }

    // ------------------------------------------------------------- download

    /// Start downloading `version`'s file for this install (progress: `update.changed`).
    pub async fn download(self: &Arc<Self>, version: &str) -> RpcResult<UpdateStatus> {
        let current = self.current();
        let (release, asset) = {
            let g = self.inner.lock();
            match self.newer(&g, current.as_deref()) {
                Some((r, a)) if r.version == version => (r, a),
                _ => {
                    return Err(RpcError::invalid_params(tr!(
                        "{version} 不是现在可以更新到的版本",
                        version = version
                    )));
                }
            }
        };
        let Some(asset) = asset else {
            return Err(RpcError::invalid_request(tr!(
                "这个版本没有适合这种安装方式的文件，请到发布页下载"
            )));
        };
        // decide under the lock, answer after releasing it (status() takes the same lock)
        let (running, done_path) = {
            let g = self.inner.lock();
            let d = &g.download;
            if d.version.as_deref() == Some(version) {
                let running = is(
                    d,
                    &[DownloadStateName::Downloading, DownloadStateName::Verifying],
                );
                (
                    running,
                    (d.state == DownloadStateName::Done)
                        .then(|| d.path.clone())
                        .flatten(),
                )
            } else {
                (false, None)
            }
        };
        if running {
            return Ok(self.status());
        }
        if let Some(p) = done_path
            && tokio::task::spawn_blocking(move || Path::new(&p).is_file())
                .await
                .unwrap_or(false)
        {
            return Ok(self.status());
        }
        let (run, cancel) = {
            let mut g = self.inner.lock();
            g.cancel.store(true, Ordering::SeqCst); // a download of another version (the list changed)
            g.run += 1;
            g.cancel = Arc::new(AtomicBool::new(false));
            g.download = UpdateDownload {
                state: DownloadStateName::Downloading,
                version: Some(version.to_owned()),
                total: asset.size,
                ..UpdateDownload::default()
            };
            g.download_error = None;
            (g.run, g.cancel.clone())
        };
        let previous = self.download_task.lock().take();
        let me = self.clone();
        let task =
            tokio::spawn(
                async move { me.run_download(run, release, asset, cancel, previous).await },
            );
        *self.download_task.lock() = Some(task);
        self.broadcast();
        Ok(self.status())
    }

    fn progress(&self, run: u64, phase: update::Phase, received: u64, total: u64) {
        {
            let mut g = self.inner.lock();
            if run != g.run {
                return;
            }
            g.download.state = match phase {
                update::Phase::Downloading => DownloadStateName::Downloading,
                update::Phase::Verifying => DownloadStateName::Verifying,
            };
            g.download.received = received;
            g.download.total = total;
        }
        self.broadcast();
    }

    async fn run_download(
        self: Arc<Self>,
        run: u64,
        release: Release,
        asset: Asset,
        cancel: Arc<AtomicBool>,
        previous: Option<JoinHandle<()>>,
    ) {
        if let Some(p) = previous {
            let _ = p.await; // its partial file is gone before this one starts
        }
        let version = release.version.clone();
        let dest = self.inner.lock().updates_dir.join(&asset.name);
        let ua = self.user_agent();
        let key = (version.clone(), asset.name.clone());
        let known = self
            .inner
            .lock()
            .sums
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, s)| s.clone());
        let result: Result<Option<bool>, UpdateError> = async {
            let sha = match known {
                Some(s) => s,
                None => {
                    let f = self.checksum.read().clone();
                    let (r, a, u) = (release.clone(), asset.clone(), ua.clone());
                    let s = tokio::task::spawn_blocking(move || f(&r, &a, &u))
                        .await
                        .unwrap_or_else(|e| {
                            Err(UpdateError::DownloadFailed {
                                reason: e.to_string(),
                            })
                        })?;
                    self.inner.lock().sums.push((key.clone(), s.clone()));
                    s
                }
            };
            let me = self.clone();
            let (url, d, a_size) = (asset.url.clone(), dest.clone(), asset.size);
            tokio::task::spawn_blocking(move || {
                let mut progress =
                    |phase, received, total| me.progress(run, phase, received, total);
                update::download(
                    &url,
                    &d,
                    a_size,
                    sha.as_deref(),
                    &ua,
                    &mut progress,
                    &cancel,
                    update::TIMEOUT,
                )
            })
            .await
            .unwrap_or_else(|e| {
                Err(UpdateError::DownloadFailed {
                    reason: e.to_string(),
                })
            })
        }
        .await;
        let mut error = None;
        let outcome = match result {
            Err(UpdateError::Cancelled) => UpdateDownload {
                state: DownloadStateName::Cancelled,
                version: Some(version.clone()),
                ..UpdateDownload::default()
            },
            Err(e) => {
                tracing::warn!("update download failed: {e}");
                error = Some(e);
                UpdateDownload {
                    state: DownloadStateName::Failed,
                    version: Some(version.clone()),
                    ..UpdateDownload::default()
                }
            }
            Ok(verified) => {
                let size = {
                    let g = self.inner.lock();
                    if g.download.total > 0 {
                        g.download.total
                    } else {
                        asset.size
                    }
                };
                tracing::info!(
                    "update {version} downloaded to {} (verified: {verified:?})",
                    dest.display()
                );
                UpdateDownload {
                    state: DownloadStateName::Done,
                    version: Some(version.clone()),
                    received: size,
                    total: size,
                    path: Some(dest.display().to_string()),
                    verified,
                    error: None,
                }
            }
        };
        let mine = {
            let mut g = self.inner.lock();
            if g.run == run {
                g.download = outcome;
                g.download_error = error;
                true
            } else {
                false
            }
        };
        if mine {
            self.broadcast();
        }
    }

    /// Stop the running download (its partial file is deleted).
    pub fn cancel(&self) -> UpdateStatus {
        let changed = {
            let mut g = self.inner.lock();
            if is(
                &g.download,
                &[DownloadStateName::Downloading, DownloadStateName::Verifying],
            ) {
                g.cancel.store(true, Ordering::SeqCst);
                g.run += 1; // the stopped one no longer reports
                g.download.state = DownloadStateName::Cancelled;
                true
            } else {
                false
            }
        };
        if changed {
            self.broadcast();
        }
        self.status()
    }

    // ------------------------------------------------------------- schedule

    /// Seconds until the next automatic check (0: now); None while automatic checks are off.
    pub fn due_in(&self) -> Option<f64> {
        if !self.enabled() {
            return None;
        }
        let now = self.now();
        let (last, failed) = {
            let g = self.inner.lock();
            (g.state.checked_at, g.state.failed_at)
        };
        let mut due = match last {
            Some(l) if l <= now => l + update::CHECK_INTERVAL,
            _ => now, // never, or the clock moved back
        };
        if let Some(f) = failed
            && f <= now
            && last.is_none_or(|l| f > l)
        {
            due = due.max(f + update::RETRY_INTERVAL);
        }
        Some(((due - now).num_milliseconds() as f64 / 1000.0).max(0.0))
    }

    /// Look at the schedule again now (settings changed; tests after moving the clock).
    pub fn wake(&self, force: bool) {
        if force {
            self.inner.lock().force = true;
        }
        self.wake.notify_one();
    }

    /// Start the automatic checks, the cleaning of the updates folder and the automatic downloads
    /// (the real app: [`UpdateService::started`] when `Config::update_checks`; tests call it).
    pub fn start(self: &Arc<Self>) {
        self.inner.lock().auto = true;
        let mut slot = self.scheduler.lock();
        if slot.is_none() {
            let me = self.clone();
            *slot = Some(tokio::spawn(async move { me.schedule().await }));
        }
    }

    async fn schedule(self: Arc<Self>) {
        let dir = self.inner.lock().updates_dir.clone();
        let current = self.current();
        match tokio::task::spawn_blocking(move || {
            let removed = update::clean_updates(&dir, current.as_deref());
            (dir, removed)
        })
        .await
        {
            Ok((dir, removed)) if !removed.is_empty() => {
                tracing::info!("removed from {}: {}", dir.display(), removed.join(", "));
            }
            Ok(_) => {}
            Err(e) => tracing::error!("could not clean the updates folder: {e}"),
        }
        let first = *self.first_delay.lock();
        tokio::time::sleep(first).await;
        self.auto_fetch().await; // a newer release known from an earlier check: its file, if not there yet
        loop {
            let force = std::mem::take(&mut self.inner.lock().force);
            let delay = self.due_in();
            if let Some(d) = delay
                && (force || d <= 0.0)
            {
                self.check().await;
                continue;
            }
            match delay {
                Some(d) => {
                    let _ = tokio::time::timeout(Duration::from_secs_f64(d), self.wake.notified())
                        .await;
                }
                None => self.wake.notified().await,
            }
        }
    }

    /// Settings listener: switched on or another channel → check now; any of them or the language
    /// changed → tell the sessions (the error text follows the language).
    fn on_settings(self: &Arc<Self>) {
        let key = self.settings_key();
        let before = {
            let mut g = self.inner.lock();
            g.applied.replace(key)
        };
        let Some(before) = before else { return };
        if key == before {
            return;
        }
        let (enabled, channel) = (key.0, key.1);
        self.wake(enabled && (!before.0 || channel != before.1));
        self.broadcast();
        if key.3 && !before.3 && self.inner.lock().auto {
            // 「自动下载更新」 switched on
            if let Some(server) = self.server() {
                let me = self.clone();
                server.rt.spawn(async move { me.auto_fetch().await });
            }
        }
    }

    // ------------------------------------------------------------- lifecycle

    /// The server is in its `Arc`: listen to the settings; start the scheduler (when
    /// `config.update_checks`).
    pub fn started(self: &Arc<Self>, server: &Arc<Server>) {
        let _ = self.server.set(Arc::downgrade(server));
        self.inner.lock().applied = Some(self.settings_key());
        let weak = Arc::downgrade(self);
        self.settings.add_listener("updates", move |_| {
            if let Some(me) = weak.upgrade() {
                me.on_settings();
            }
        });
        if self.scheduled {
            self.start();
        }
    }

    /// The server stops.
    pub fn close(&self) {
        self.inner.lock().cancel.store(true, Ordering::SeqCst);
        if let Some(t) = self.scheduler.lock().take() {
            t.abort();
        }
        if let Some(t) = self.download_task.lock().take() {
            t.abort();
        }
    }

    /// Where update files are downloaded.
    pub fn updates_dir(&self) -> PathBuf {
        self.inner.lock().updates_dir.clone()
    }
}
