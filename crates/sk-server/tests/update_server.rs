//! Port of tests/test_update_server.py: update.* over the real WebSocket — checks with a fake
//! release list, update.changed only to sessions with updates.v1, closing a card, the verified
//! download (local HTTP server), the schedule.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sk_core::update::{Asset, Fetched, Release, Shell, UpdateError};
use sk_protocol::ErrorCode;
use sk_protocol::update::InstallKind;
use sk_server::ServerHandle;

const PAGE: &str = "https://github.com/DuskXi/SpaceKnife/releases";

/// One test at a time: the servers of this binary share the settings file.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Default)]
struct Gh {
    releases: Vec<Release>,
    error: Option<UpdateError>,
    calls: usize,
    agents: Vec<String>,
    etags: Vec<Option<String>>,
}

type FakeGh = Arc<Mutex<Gh>>;

fn fake(srv: &ServerHandle) -> FakeGh {
    let gh: FakeGh = Arc::default();
    let g = gh.clone();
    srv.server
        .updates
        .set_fetch(Arc::new(move |_feed, etag, ua| {
            let mut g = g.lock().unwrap();
            g.calls += 1;
            g.agents.push(ua.to_owned());
            g.etags.push(etag.map(str::to_owned));
            if let Some(e) = &g.error {
                return Err(e.clone());
            }
            Ok(Fetched {
                releases: Some(g.releases.clone()),
                etag: Some(format!("\"{}\"", g.calls)),
            })
        }));
    // the Windows installer is the Tauri app's (0.3.4's name, the update contract)
    srv.server.updates.set_install_kind(InstallKind::Nsis);
    srv.server.updates.set_shell(Shell::Tauri);
    gh
}

fn rel(version: &str, assets: Vec<Asset>, prerelease: bool, notes: &str) -> Release {
    Release {
        version: version.to_owned(),
        tag: format!("v{version}"),
        name: format!("SpaceKnife {version}"),
        prerelease,
        published_at: None,
        url: format!("{PAGE}/tag/v{version}"),
        notes: notes.to_owned(),
        assets,
    }
}

fn installer(version: &str) -> Asset {
    let name = format!("SpaceKnife-{version}-Setup-x64.exe");
    Asset {
        url: format!("{PAGE}/download/v{version}/{name}"),
        name,
        size: 10,
    }
}

/// A server for one test: its own update state and updates folder, default settings, the app
/// version 0.3.0.
async fn setup() -> (ServerHandle, tempfile::TempDir) {
    let srv = server().await;
    let dir = tempfile::tempdir().unwrap();
    srv.server.settings.reset(None).unwrap();
    srv.server
        .updates
        .relocate(dir.path().join("update.json"), dir.path().join("updates"));
    srv.server.updates.set_app_version(Some("0.3.0".to_owned()));
    (srv, dir)
}

async fn stop(srv: ServerHandle) {
    srv.shutdown();
    srv.wait().await;
}

async fn until(within: f64, mut cond: impl FnMut() -> bool) {
    let end = tokio::time::Instant::now() + Duration::from_secs_f64(within);
    while !cond() {
        assert!(
            tokio::time::Instant::now() < end,
            "condition not reached in time"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn updates_caps_off() -> Vec<String> {
    all_caps()
        .into_iter()
        .filter(|c| c != sk_protocol::CAP_UPDATES)
        .collect()
}

fn set(srv: &ServerHandle, key: &str, value: Value) {
    let v: sk_protocol::settings::SettingValue = serde_json::from_value(value).unwrap();
    srv.server
        .settings
        .update(&[(key.to_owned(), v)].into_iter().collect(), None)
        .unwrap();
}

// ------------------------------------------------------------------- a local feed

#[derive(Default)]
struct FeedState {
    files: HashMap<String, Vec<u8>>,
    hits: HashMap<String, usize>,
    stall: Option<(String, Arc<tokio::sync::Notify>)>,
}

struct Feed {
    port: u16,
    st: Arc<Mutex<FeedState>>,
    task: tokio::task::JoinHandle<()>,
}

impl Feed {
    async fn start() -> Feed {
        let st: Arc<Mutex<FeedState>> = Arc::default();
        let s = st.clone();
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let s = s.clone();
            async move {
                let path = uri.path().to_owned();
                let stall = {
                    let mut g = s.lock().unwrap();
                    *g.hits.entry(path.clone()).or_default() += 1;
                    g.stall
                        .as_ref()
                        .filter(|(p, _)| *p == path)
                        .map(|(_, n)| n.clone())
                };
                if let Some(n) = stall {
                    let _ = tokio::time::timeout(Duration::from_secs(10), n.notified()).await;
                }
                let body = s.lock().unwrap().files.get(&path).cloned();
                match body {
                    Some(b) => (axum::http::StatusCode::OK, b),
                    None => (axum::http::StatusCode::NOT_FOUND, b"not found".to_vec()),
                }
            }
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let _ = axum::serve(l, app).await;
        });
        Feed { port, st, task }
    }

    fn file(&self, path: &str, body: &[u8]) -> String {
        self.st
            .lock()
            .unwrap()
            .files
            .insert(path.to_owned(), body.to_vec());
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn hits(&self, path: &str) -> usize {
        self.st.lock().unwrap().hits.get(path).copied().unwrap_or(0)
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn listing(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default()
}

/// Events of `name` until one satisfies `pred` (the others are returned before it).
async fn update_event(c: &Client, pred: impl Fn(&Value) -> bool) -> Value {
    c.event_where("update.changed", 10.0, |p| pred(&p["status"]))
        .await["status"]
        .clone()
}

// ------------------------------------------------------------------- tests

#[tokio::test]
async fn check_levels_events_and_dismiss() {
    let _s = SERIAL.lock().await;
    let (srv, _dir) = setup().await;
    let gh = fake(&srv);
    gh.lock().unwrap().releases = vec![rel("0.3.1", vec![installer("0.3.1")], false, "- 修复")];
    let ui = Client::open(&srv.url).await;
    let old = Client::open_with(&srv.url, TOKEN, updates_caps_off(), None)
        .await
        .unwrap();

    let st = ui.ok("update.status", json!({})).await;
    assert_eq!(
        (st["current"].as_str(), st["install_kind"].as_str()),
        (Some("0.3.0"), Some("nsis"))
    );
    assert_eq!(
        (st["enabled"].as_bool(), st["channel"].as_str()),
        (Some(true), Some("preview"))
    );
    assert_eq!(st["level"], "none");
    assert!(st["latest"].is_null() && st["checked_at"].is_null() && st["checking"] == false);

    // patch: the badge only
    let st = ui.ok("update.check", json!({})).await;
    assert_eq!(st["level"], "patch");
    assert_eq!(st["latest"]["version"], "0.3.1");
    assert_eq!(st["latest"]["notes"], "- 修复");
    assert_eq!(st["latest"]["url"], format!("{PAGE}/tag/v0.3.1"));
    assert_eq!(
        st["latest"]["asset"]["name"],
        "SpaceKnife-0.3.1-Setup-x64.exe"
    );
    assert!(!st["checked_at"].is_null() && st["error"].is_null() && st["dismissed"] == false);
    {
        let g = gh.lock().unwrap();
        assert_eq!(g.agents, vec!["SpaceKnife/0.3.0"]);
        assert_eq!(g.etags, vec![None]);
    }
    let first = update_event(&ui, |_| true).await;
    assert_eq!(first["checking"], true);
    update_event(&ui, |s| s["level"] == "patch" && s["checking"] == false).await;

    // minor, then closing its card; the next check sends the cached list's ETag
    gh.lock().unwrap().releases = vec![rel("0.4.0", vec![installer("0.4.0")], false, "")];
    let st = ui.ok("update.check", json!({})).await;
    assert_eq!(st["level"], "minor");
    assert_eq!(
        gh.lock().unwrap().etags.last().unwrap().as_deref(),
        Some("\"1\"")
    );
    let st = ui.ok("update.dismiss", json!({"version": "0.4.0"})).await;
    assert_eq!(
        (st["dismissed"].as_bool(), st["level"].as_str()),
        (Some(true), Some("minor"))
    );
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(srv.server.updates.state_file()).unwrap())
            .unwrap();
    assert_eq!(saved["dismissed"], json!(["0.4.0"]));
    assert_eq!(saved["etag"], "\"2\"");
    update_event(&ui, |s| s["dismissed"] == true).await;

    // major: a new version, so a new card
    gh.lock().unwrap().releases = vec![
        rel("1.0.0", vec![installer("1.0.0")], false, ""),
        rel("0.4.0", vec![], false, ""),
    ];
    let st = ui.ok("update.check", json!({})).await;
    assert_eq!(
        (st["level"].as_str(), st["latest"]["version"].as_str()),
        (Some("major"), Some("1.0.0"))
    );
    assert_eq!(st["dismissed"], false);

    // the stable channel skips a newer pre-release (from the cache, no new request)
    gh.lock().unwrap().releases = vec![
        rel("1.1.0-beta.1", vec![], true, ""),
        rel("1.0.0", vec![], false, ""),
    ];
    let st = ui.ok("update.check", json!({})).await;
    assert_eq!(st["latest"]["version"], "1.1.0-beta.1");
    let calls = gh.lock().unwrap().calls;
    set(&srv, "general.update_channel", json!("stable")); // no scheduler in tests
    let st = ui.ok("update.status", json!({})).await;
    assert_eq!(
        (st["channel"].as_str(), st["latest"]["version"].as_str()),
        (Some("stable"), Some("1.0.0"))
    );
    assert_eq!(gh.lock().unwrap().calls, calls);

    // a failed check keeps what the last one found and says why
    gh.lock().unwrap().error = Some(UpdateError::Rate);
    let st = ui.ok("update.check", json!({})).await;
    assert_eq!(st["error"], "GitHub 暂时限制了查询次数，请过一会儿再试");
    assert!(!st["latest"].is_null());

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        old.drain_events()
            .await
            .iter()
            .all(|(m, _)| m != "update.changed")
    ); // no updates.v1

    let e = ui.err("update.download", json!({"version": "0.9.0"})).await;
    assert_eq!(e.code, i64::from(ErrorCode::InvalidParams.code()));
    stop(srv).await;
}

#[tokio::test]
async fn current_version_comes_from_the_first_updates_session() {
    let _s = SERIAL.lock().await;
    let (srv, _dir) = setup().await;
    srv.server.updates.set_app_version(None);
    use futures_util::{SinkExt, StreamExt};
    let mut ws = raw(&srv.url, None, true).await.unwrap();
    let hello = json!({"jsonrpc": "2.0", "id": 0, "method": "session.hello", "params": {
        "token": TOKEN, "protocol": "1.1",
        "client": {"name": "ui", "version": "0.3.0", "kind": "test"},
        "capabilities": ["updates.v1"]}});
    ws.send(tokio_tungstenite::tungstenite::Message::text(
        hello.to_string(),
    ))
    .await
    .unwrap();
    let _ = ws.next().await;
    assert_eq!(srv.server.updates.status().current, "0.3.0");
    stop(srv).await;
}

#[tokio::test]
async fn download_over_the_server() {
    let _s = SERIAL.lock().await;
    let (srv, dir) = setup().await;
    let payload = b"installer bytes ".repeat(50_000);
    let digest = hex(&payload);
    let gh = fake(&srv);
    let feed = Feed::start().await;
    let exe = "SpaceKnife-0.4.0-Setup-x64.exe";
    let sums = feed.file("/SHA256SUMS.txt", format!("{digest}  {exe}\n").as_bytes());
    gh.lock().unwrap().releases = vec![rel(
        "0.4.0",
        vec![
            Asset {
                name: exe.into(),
                size: payload.len() as u64,
                url: feed.file(&format!("/{exe}"), &payload),
            },
            Asset {
                name: "SHA256SUMS.txt".into(),
                size: 100,
                url: sums,
            },
        ],
        false,
        "",
    )];
    let ui = Client::open(&srv.url).await;
    ui.ok("update.check", json!({})).await;
    let st = ui.ok("update.download", json!({"version": "0.4.0"})).await;
    assert!(["downloading", "done"].contains(&st["download"]["state"].as_str().unwrap()));
    assert_eq!(st["download"]["version"], "0.4.0");
    let done = update_event(&ui, |s| s["download"]["state"] == "done").await;
    assert_eq!(done["download"]["verified"], true);
    assert_eq!(done["download"]["received"], payload.len());
    let dest = dir.path().join("updates").join(exe);
    assert_eq!(done["download"]["path"], dest.display().to_string());
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert_eq!(done["latest"]["asset"]["sha256"], digest);

    // asked again: the file is there, nothing is fetched
    let st = ui.ok("update.download", json!({"version": "0.4.0"})).await;
    assert_eq!(st["download"]["state"], "done");
    assert_eq!(feed.hits(&format!("/{exe}")), 1);
    assert_eq!(
        ui.ok("update.cancel", json!({})).await["download"]["state"],
        "done"
    ); // nothing running

    // no file for this kind of install: the release page only
    srv.server.updates.set_install_kind(InstallKind::Dev);
    let e = ui.err("update.download", json!({"version": "0.4.0"})).await;
    assert_eq!(e.code, i64::from(ErrorCode::InvalidRequest.code()));
    stop(srv).await;
}

#[tokio::test]
async fn a_release_without_a_checksum_for_the_file_is_not_downloaded() {
    // review issue 10: no SHA256SUMS line for the file, nothing to check it against; the file is
    // never fetched, so nothing unverified can be installed
    let _s = SERIAL.lock().await;
    let (srv, dir) = setup().await;
    let payload = b"installer bytes ".repeat(1000);
    let gh = fake(&srv);
    let feed = Feed::start().await;
    let exe = "SpaceKnife-0.4.0-Setup-x64.exe";
    let ui = Client::open(&srv.url).await;
    for sums in [
        None, // no checksum file at all
        Some(format!(
            "{}  SpaceKnife-0.4.0-win-x64-portable.zip\n",
            "a".repeat(64)
        )), // not for it
    ] {
        let mut assets = vec![Asset {
            name: exe.into(),
            size: payload.len() as u64,
            url: feed.file(&format!("/{exe}"), &payload),
        }];
        if let Some(text) = &sums {
            assets.push(Asset {
                name: "SHA256SUMS.txt".into(),
                size: 100,
                url: feed.file("/SHA256SUMS.txt", text.as_bytes()),
            });
        }
        gh.lock().unwrap().releases = vec![rel("0.4.0", assets, false, "")];
        ui.ok("update.check", json!({})).await;
        ui.ok("update.download", json!({"version": "0.4.0"})).await;
        let failed = update_event(&ui, |s| s["download"]["state"] == "failed").await;
        assert_eq!(
            failed["download"]["error"],
            "这个版本的发布没有列出这个文件的 SHA-256 校验值，没有下载"
        );
        assert!(failed["download"]["path"].is_null());
        assert_eq!(feed.hits(&format!("/{exe}")), 0, "{sums:?}");
        assert!(listing(&dir.path().join("updates")).is_empty());
    }
    stop(srv).await;
}

#[tokio::test]
async fn each_shell_takes_the_file_built_for_it() {
    // the rpm and the AppImage are the Electron shell's, the Windows installer, the deb and the
    // portable copies Tauri's; an install never moves to a file of the other shell
    // (SPACEKNIFE_SHELL)
    let _s = SERIAL.lock().await;
    let (srv, _dir) = setup().await;
    let gh = fake(&srv);
    let asset = |name: &str| Asset {
        name: name.into(),
        size: 10,
        url: format!("{PAGE}/download/v0.4.0/{name}"),
    };
    gh.lock().unwrap().releases = vec![rel(
        "0.4.0",
        vec![
            asset("spaceknife-0.4.0.x86_64.rpm"),
            asset("spaceknife_0.4.0_amd64.deb"),
            asset("SpaceKnife-0.4.0-x86_64.AppImage"),
            asset("SpaceKnife-0.4.0-Setup-x64.exe"),
        ],
        false,
        "",
    )];
    let ui = Client::open(&srv.url).await;
    let updates = &srv.server.updates;
    updates.set_platform(Some("linux".into()), Some("x86_64".into())); // the x64 packages
    for (kind, shell, want) in [
        (
            InstallKind::Rpm,
            Shell::Electron,
            Some("spaceknife-0.4.0.x86_64.rpm"),
        ),
        (InstallKind::Rpm, Shell::Tauri, None),
        (
            InstallKind::Appimage,
            Shell::Electron,
            Some("SpaceKnife-0.4.0-x86_64.AppImage"),
        ),
        (InstallKind::Appimage, Shell::Tauri, None),
        (
            InstallKind::Deb,
            Shell::Tauri,
            Some("spaceknife_0.4.0_amd64.deb"),
        ),
        (InstallKind::Deb, Shell::Electron, None),
        (
            InstallKind::Nsis,
            Shell::Tauri,
            Some("SpaceKnife-0.4.0-Setup-x64.exe"),
        ),
        (InstallKind::Nsis, Shell::Electron, None),
    ] {
        updates.set_install_kind(kind);
        updates.set_shell(shell);
        let st = ui.ok("update.check", json!({})).await;
        assert_eq!(st["latest"]["version"], "0.4.0");
        assert_eq!(
            st["latest"]["asset"]["name"].as_str(),
            want,
            "{kind:?} {shell:?}"
        );
    }
    stop(srv).await;
}

fn published(feed: &Feed, version: &str, payload: &[u8], sums: &str) -> Release {
    let exe = format!("SpaceKnife-{version}-Setup-x64.exe");
    rel(
        version,
        vec![
            Asset {
                url: feed.file(&format!("/{version}/{exe}"), payload),
                name: exe,
                size: payload.len() as u64,
            },
            Asset {
                name: "SHA256SUMS.txt".into(),
                size: 100,
                url: feed.file(&format!("/{version}/SHA256SUMS.txt"), sums.as_bytes()),
            },
        ],
        false,
        "",
    )
}

#[tokio::test]
async fn download_mismatch_and_cancel() {
    let _s = SERIAL.lock().await;
    let (srv, dir) = setup().await;
    let downloads = dir.path().join("updates");
    let payload = vec![b'x'; 3 * sk_core::update::CHUNK];
    let gh = fake(&srv);
    let feed = Feed::start().await;
    let ui = Client::open(&srv.url).await;
    // the published checksum does not match: nothing is kept
    gh.lock().unwrap().releases = vec![published(
        &feed,
        "0.4.0",
        &payload,
        &format!("{}  SpaceKnife-0.4.0-Setup-x64.exe", "0".repeat(64)),
    )];
    ui.ok("update.check", json!({})).await;
    ui.ok("update.download", json!({"version": "0.4.0"})).await;
    let failed = update_event(&ui, |s| s["download"]["state"] == "failed").await;
    assert_eq!(
        failed["download"]["error"],
        "下载的文件与发布的 SHA-256 校验值不符，已删除"
    );
    assert!(failed["download"]["path"].is_null());
    assert!(listing(&downloads).is_empty());

    // a download that stalls before its first byte, cancelled meanwhile
    gh.lock().unwrap().releases = vec![published(
        &feed,
        "0.4.1",
        &payload,
        &format!("{}  SpaceKnife-0.4.1-Setup-x64.exe", hex(&payload)),
    )];
    let exe = "/0.4.1/SpaceKnife-0.4.1-Setup-x64.exe";
    let go_on = Arc::new(tokio::sync::Notify::new());
    feed.st.lock().unwrap().stall = Some((exe.to_owned(), go_on.clone()));
    let st = ui.ok("update.check", json!({})).await;
    assert_eq!(st["download"]["state"], "idle"); // another release now: the failed download is forgotten
    let st = ui.ok("update.download", json!({"version": "0.4.1"})).await;
    assert_eq!(st["download"]["state"], "downloading");
    until(10.0, || feed.hits(exe) == 1).await;
    // asking again while it runs answers with the running download (this used to deadlock on the state lock)
    let again = tokio::time::timeout(
        Duration::from_secs(10),
        ui.ok("update.download", json!({"version": "0.4.1"})),
    )
    .await;
    assert_eq!(
        again.expect("update.download hung while downloading")["download"]["state"],
        "downloading"
    );
    let st = ui.ok("update.cancel", json!({})).await;
    assert_eq!(st["download"]["state"], "cancelled");
    go_on.notify_one();
    tokio::time::timeout(Duration::from_secs(10), srv.server.updates.wait_download())
        .await
        .unwrap();
    assert!(listing(&downloads).is_empty(), "{:?}", listing(&downloads));
    assert_eq!(
        ui.ok("update.status", json!({})).await["download"]["state"],
        "cancelled"
    );
    stop(srv).await;
}

#[tokio::test]
async fn schedule() {
    let _s = SERIAL.lock().await;
    let (srv, _dir) = setup().await;
    let svc = &srv.server.updates;
    let gh = fake(&srv);
    gh.lock().unwrap().releases = vec![rel("0.4.0", vec![], false, "")];
    let now: Arc<Mutex<DateTime<Utc>>> = Arc::new(Mutex::new(
        Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap(),
    ));
    let n = now.clone();
    svc.set_clock(Arc::new(move || *n.lock().unwrap()));
    svc.set_first_delay(Duration::from_millis(50));
    let hour = chrono::TimeDelta::hours(1);
    let advance = |d: chrono::TimeDelta| *now.lock().unwrap() += d;
    let calls = || gh.lock().unwrap().calls;
    let settle = || async {
        svc.wake(false);
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    let checked = |n: usize| {
        let gh = gh.clone();
        async move {
            until(10.0, || {
                gh.lock().unwrap().calls == n && !svc.status().checking
            })
            .await;
            tokio::time::sleep(Duration::from_millis(30)).await; // the check's time recorded
        }
    };

    // never checked: the first round checks
    svc.start();
    checked(1).await;
    assert_eq!(svc.due_in(), Some(24.0 * 3600.0));

    // within the day: nothing; a day after the last success: checks
    advance(hour * 23);
    settle().await;
    assert_eq!(calls(), 1);
    advance(hour);
    svc.wake(false);
    checked(2).await;

    // a failure is retried after 6 hours, not sooner
    gh.lock().unwrap().error = Some(UpdateError::Offline {
        reason: "test".into(),
    });
    advance(hour * 24);
    svc.wake(false);
    checked(3).await;
    assert_eq!(svc.due_in(), Some(6.0 * 3600.0));
    assert_eq!(svc.status().error.as_deref(), Some("连不上 GitHub：test"));
    advance(hour * 5);
    settle().await;
    assert_eq!(calls(), 3);
    advance(hour);
    svc.wake(false);
    checked(4).await;
    gh.lock().unwrap().error = None;

    // off: never, however long ago
    set(&srv, "general.update_check", json!(false));
    advance(hour * 72);
    settle().await;
    assert_eq!(calls(), 4);
    assert_eq!(svc.due_in(), None);

    // switched on: checks (it is due); switched on again or another channel: right away
    set(&srv, "general.update_check", json!(true));
    checked(5).await;
    settle().await;
    assert_eq!(calls(), 5);
    assert_eq!(svc.due_in(), Some(24.0 * 3600.0));
    set(&srv, "general.update_check", json!(false));
    set(&srv, "general.update_check", json!(true));
    checked(6).await;
    set(&srv, "general.update_channel", json!("stable"));
    checked(7).await;
    settle().await;
    assert_eq!(calls(), 7);
    assert_eq!(svc.due_in(), Some(24.0 * 3600.0));
    stop(srv).await;
}

#[tokio::test]
async fn the_real_app_schedules_and_stops() {
    let _s = SERIAL.lock().await;
    init();
    let mut tuning = sk_server::Tuning::for_tests();
    tuning.update_checks = true;
    let opts = sk_server::ServeOptions {
        token: TOKEN.to_owned(),
        ..Default::default()
    };
    let srv = sk_server::start_with(opts, tuning).await.unwrap();
    let gh = fake(&srv);
    assert!(srv.server.updates.scheduling());
    let svc = srv.server.updates.clone();
    stop(srv).await;
    assert!(!svc.scheduling());
    assert_eq!(gh.lock().unwrap().calls, 0); // the first check waits a minute
    let plain = server().await;
    assert!(!plain.server.updates.scheduling()); // tests and tools: no automatic checks
    stop(plain).await;
}

/// Right after a portable copy's update the old version's last processes may still hold some of
/// its files in previous-<old>; the cleaning at start leaves those and tries them again later
/// (seen updating the real 0.3.4: ffmpeg.dll, hundreds of MB left until the next start).
#[cfg(windows)]
#[tokio::test]
async fn what_the_old_version_still_held_is_cleaned_again() {
    use std::os::windows::fs::OpenOptionsExt;
    let _s = SERIAL.lock().await;
    let (srv, dir) = setup().await;
    let svc = srv.server.updates.clone();
    let updates = dir.path().join("updates");
    let old = updates.join("previous-0.2.9");
    std::fs::create_dir_all(old.join("locales")).unwrap();
    std::fs::write(old.join("ffmpeg.dll"), "x").unwrap();
    std::fs::write(updates.join("update-0.3.0.log"), "updated").unwrap();
    // open as a closing program has its DLLs open: no deleting it meanwhile
    const FILE_SHARE_READ: u32 = 1;
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(old.join("ffmpeg.dll"))
        .unwrap();
    svc.set_clean_delays(vec![Duration::from_millis(300); 20]);
    svc.set_first_delay(Duration::from_secs(3600));
    svc.start();
    until(10.0, || !updates.join("update-0.3.0.log").exists()).await;
    assert!(old.join("ffmpeg.dll").exists());
    drop(held);
    until(10.0, || !old.exists()).await;
    stop(srv).await;
}

#[tokio::test]
async fn state_survives_a_restart() {
    let _s = SERIAL.lock().await;
    let (srv, dir) = setup().await;
    let gh = fake(&srv);
    gh.lock().unwrap().releases = vec![rel("0.4.0", vec![], false, "")];
    let ui = Client::open(&srv.url).await;
    ui.ok("update.check", json!({})).await;
    ui.ok("update.dismiss", json!({"version": "0.4.0"})).await;
    let again = sk_server::updates::UpdateService::new(&srv.server.config, &srv.server.settings); // the next start
    again.relocate(dir.path().join("update.json"), dir.path().join("updates"));
    again.set_app_version(Some("0.3.0".into()));
    let st = again.status();
    assert_eq!(st.level, sk_protocol::update::UpdateLevel::Minor);
    assert!(st.dismissed);
    assert_eq!(st.checked_at, srv.server.updates.state().checked_at);
    assert_eq!(again.state().etag.as_deref(), Some("\"1\""));
    assert!((again.due_in().unwrap() - 24.0 * 3600.0).abs() < 60.0);
    stop(srv).await;
}

#[tokio::test]
async fn timeline_and_next_check() {
    let _s = SERIAL.lock().await;
    let (srv, _dir) = setup().await;
    let gh = fake(&srv);
    gh.lock().unwrap().releases = vec![
        rel("0.3.0", vec![], false, "- first"),
        rel("0.4.0-beta.1", vec![], true, ""),
        rel("0.3.1", vec![installer("0.3.1")], false, ""),
    ];
    let ui = Client::open(&srv.url).await;
    assert_eq!(
        ui.ok("update.releases", json!({})).await["releases"],
        json!([])
    ); // nothing checked yet
    let st = ui.ok("update.check", json!({})).await;
    assert!(!st["checked_at"].is_null() && st["next_check_at"].is_null()); // no automatic checks here
    // the real app (without its download): the next check is a day later
    set(&srv, "general.update_download", json!(false));
    srv.server.updates.set_auto(true);
    let st = srv.server.updates.status();
    let (checked, next) = (st.checked_at.unwrap(), st.next_check_at.unwrap());
    assert!(((next - checked).num_seconds() - 24 * 3600).abs() < 5);
    let got = ui.ok("update.releases", json!({})).await;
    let versions: Vec<&str> = got["releases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, ["0.4.0-beta.1", "0.3.1", "0.3.0"]); // newest first, the older too
    assert_eq!(got["releases"][0]["prerelease"], true);
    assert_eq!(got["releases"][2]["notes"], "- first");
    assert!(
        got["releases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["asset"].is_null())
    );
    set(&srv, "general.update_check", json!(false));
    assert!(ui.ok("update.status", json!({})).await["next_check_at"].is_null());
    stop(srv).await;
}

#[tokio::test]
async fn a_shorter_cache_is_fetched_again_whole() {
    let _s = SERIAL.lock().await;
    let (srv, _dir) = setup().await;
    let gh = fake(&srv);
    gh.lock().unwrap().releases = vec![
        rel("0.3.1", vec![], false, ""),
        rel("0.3.0", vec![], false, ""),
    ];
    let ui = Client::open(&srv.url).await;
    ui.ok("update.check", json!({})).await;
    ui.ok("update.check", json!({})).await;
    assert_eq!(
        gh.lock().unwrap().etags,
        vec![None, Some("\"1\"".to_owned())]
    );
    // what 0.3.2 and older left: one release per channel, no "kept"
    srv.server.updates.edit_state(|s| s.kept = 0);
    ui.ok("update.check", json!({})).await;
    assert_eq!(*gh.lock().unwrap().etags.last().unwrap(), None);
    assert_eq!(
        srv.server.updates.state().kept,
        sk_core::update::HISTORY_KEEP
    );
    let got = ui.ok("update.releases", json!({})).await;
    let versions: Vec<&str> = got["releases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, ["0.3.1", "0.3.0"]);
    stop(srv).await;
}

#[tokio::test]
async fn muting_a_series() {
    let _s = SERIAL.lock().await;
    let (srv, _dir) = setup().await;
    let svc = &srv.server.updates;
    let gh = fake(&srv);
    gh.lock().unwrap().releases = vec![rel("0.4.0", vec![installer("0.4.0")], false, "")];
    let ui = Client::open(&srv.url).await;
    let st = ui.ok("update.check", json!({})).await;
    assert_eq!(
        (
            st["level"].as_str(),
            st["series"].as_str(),
            st["muted"].as_bool()
        ),
        (Some("minor"), Some("0.4"), Some(false))
    );

    let st = ui.ok("update.mute", json!({"series": "0.4"})).await;
    assert_eq!(
        (st["muted"].as_bool(), st["series"].as_str()),
        (Some(true), Some("0.4"))
    );
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(svc.state_file()).unwrap()).unwrap();
    assert_eq!(saved["muted"], json!(["0.4"]));
    update_event(&ui, |s| s["muted"] == true).await;

    // every later release of the series stays quiet, and nothing is downloaded by itself
    svc.set_auto(true);
    gh.lock().unwrap().releases = vec![rel("0.4.2", vec![installer("0.4.2")], false, "")];
    let st = ui.ok("update.check", json!({})).await;
    assert_eq!(
        (st["latest"]["version"].as_str(), st["muted"].as_bool()),
        (Some("0.4.2"), Some(true))
    );
    assert_eq!(
        svc.status().download.state,
        sk_protocol::update::DownloadStateName::Idle
    );

    // another series speaks up again; muting a major one takes all its minor releases
    svc.set_auto(false);
    gh.lock().unwrap().releases = vec![rel("0.5.0", vec![installer("0.5.0")], false, "")];
    let st = ui.ok("update.check", json!({})).await;
    assert_eq!(
        (st["series"].as_str(), st["muted"].as_bool()),
        (Some("0.5"), Some(false))
    );
    gh.lock().unwrap().releases = vec![rel("1.2.0", vec![installer("1.2.0")], false, "")];
    let st = ui.ok("update.check", json!({})).await;
    assert_eq!(
        (
            st["level"].as_str(),
            st["series"].as_str(),
            st["muted"].as_bool()
        ),
        (Some("major"), Some("1"), Some(false))
    );
    assert_eq!(
        ui.ok("update.mute", json!({"series": "1"})).await["muted"],
        true
    );
    gh.lock().unwrap().releases = vec![rel("1.3.1", vec![installer("1.3.1")], false, "")];
    assert_eq!(ui.ok("update.check", json!({})).await["muted"], true);

    // back on: the prompts return
    assert_eq!(
        ui.ok("update.mute", json!({"series": "1", "muted": false}))
            .await["muted"],
        false
    );
    assert_eq!(svc.state().muted, vec!["0.4"]);

    // a series the running version reached is dropped the next time the list changes
    svc.set_app_version(Some("0.4.1".into()));
    ui.ok("update.mute", json!({"series": "2"})).await;
    assert_eq!(svc.state().muted, vec!["2"]);

    let e = ui
        .err("update.mute", json!({"series": "0.x", "muted": true}))
        .await;
    assert_eq!(e.code, i64::from(ErrorCode::InvalidParams.code()));
    stop(srv).await;
}

#[tokio::test]
async fn downloads_by_itself_in_the_real_app() {
    let _s = SERIAL.lock().await;
    let (srv, dir) = setup().await;
    let svc = &srv.server.updates;
    let payload = b"new installer ".repeat(1000);
    let gh = fake(&srv);
    let feed = Feed::start().await;
    let exe = "SpaceKnife-0.4.0-Setup-x64.exe";
    let sums = format!(
        "{}  {exe}
",
        hex(&payload)
    );
    gh.lock().unwrap().releases = vec![rel(
        "0.4.0",
        vec![
            Asset {
                name: exe.into(),
                size: payload.len() as u64,
                url: feed.file(&format!("/{exe}"), &payload),
            },
            Asset {
                name: "SHA256SUMS.txt".into(),
                size: sums.len() as u64,
                url: feed.file("/SHA256SUMS.txt", sums.as_bytes()),
            },
        ],
        false,
        "",
    )];
    let ui = Client::open(&srv.url).await;
    // tests and demos (no scheduler): a check finds it, nothing is downloaded
    let st = ui.ok("update.check", json!({})).await;
    assert!(!st["latest"].is_null());
    assert_eq!(st["download"]["state"], "idle");
    // the real app with 「自动下载更新」 on: the next check starts the download
    set(&srv, "general.update_download", json!(true));
    svc.set_auto(true);
    ui.ok("update.check", json!({})).await;
    use sk_protocol::update::DownloadStateName as D;
    until(10.0, || svc.status().download.state == D::Done).await;
    let done = svc.status().download;
    let dest = dir.path().join("updates").join(exe);
    assert_eq!(
        done.path.as_deref(),
        Some(dest.display().to_string().as_str())
    );
    assert_eq!(done.verified, Some(true));
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    // done for this version: another check does not fetch it again
    ui.ok("update.check", json!({})).await;
    assert_eq!(svc.status().download.state, D::Done);
    assert_eq!(feed.hits(&format!("/{exe}")), 1);
    // switched off: a newer release is found but not downloaded
    set(&srv, "general.update_download", json!(false));
    gh.lock().unwrap().releases = vec![rel("0.5.0", vec![installer("0.5.0")], false, "")];
    let st = ui.ok("update.check", json!({})).await;
    assert_eq!(
        (
            st["latest"]["version"].as_str(),
            st["download"]["state"].as_str()
        ),
        (Some("0.5.0"), Some("idle"))
    );
    stop(srv).await;
}
