//! Update checks: versions, levels, channels, asset choice, checksums, state, the release list
//! fetch (ETag / 304, errors) and the verified download — against a local HTTP server.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;

type Headers = HashMap<String, String>;
type Route = Box<dyn Fn(&Headers) -> (u16, Vec<(String, String)>, Vec<u8>) + Send + Sync>;

/// A stand-in for GitHub on 127.0.0.1: `routes` answer GETs, `requests` records them.
struct LocalFeed {
    port: u16,
    routes: Arc<Mutex<HashMap<String, Route>>>,
    requests: Arc<Mutex<Vec<(String, Headers)>>>,
}

impl LocalFeed {
    fn start() -> LocalFeed {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let routes: Arc<Mutex<HashMap<String, Route>>> = Arc::default();
        let requests: Arc<Mutex<Vec<(String, Headers)>>> = Arc::default();
        let (r2, q2) = (routes.clone(), requests.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let (r3, q3) = (r2.clone(), q2.clone());
                std::thread::spawn(move || serve(conn, &r3, &q3));
            }
        });
        LocalFeed {
            port,
            routes,
            requests,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn route(
        &self,
        path: &str,
        f: impl Fn(&Headers) -> (u16, Vec<(String, String)>, Vec<u8>) + Send + Sync + 'static,
    ) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), Box::new(f));
    }

    /// Serve `body` at `path` (with a Content-Length unless `length` is false).
    fn file(&self, path: &str, body: Vec<u8>, length: bool) -> String {
        self.route(path, move |_| {
            let h = if length {
                vec![("Content-Length".into(), body.len().to_string())]
            } else {
                vec![]
            };
            (200, h, body.clone())
        });
        self.url(path)
    }

    fn hits(&self, path: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == path)
            .count()
    }
}

fn serve(
    conn: TcpStream,
    routes: &Mutex<HashMap<String, Route>>,
    requests: &Mutex<Vec<(String, Headers)>>,
) {
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let mut headers = Headers::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_owned());
        }
    }
    requests
        .lock()
        .unwrap()
        .push((path.clone(), headers.clone()));
    let response = routes.lock().unwrap().get(&path).map(|r| r(&headers));
    let (status, extra, body) = response.unwrap_or((404, vec![], b"not found".to_vec()));
    let mut out = conn;
    let mut head = format!("HTTP/1.1 {status} X\r\nConnection: close\r\n");
    for (k, v) in &extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(&body);
    let _ = out.flush();
    let _ = out.shutdown(std::net::Shutdown::Both);
}

const BASE: &str = "https://github.com/DuskXi/SpaceKnife/releases";

/// One release as GitHub's API lists it.
fn gh(tag: &str) -> Value {
    json!({
        "tag_name": tag,
        "name": format!("SpaceKnife {tag}"),
        "draft": false,
        "prerelease": false,
        "html_url": format!("{BASE}/tag/{tag}"),
        "body": "notes",
        "published_at": "2026-10-01T12:00:00Z",
        "assets": [],
    })
}

fn gh_with(tag: &str, f: impl FnOnce(&mut Value)) -> Value {
    let mut v = gh(tag);
    f(&mut v);
    v
}

fn release(version: &str, names: &[&str]) -> Release {
    Release {
        version: version.into(),
        tag: format!("v{version}"),
        name: String::new(),
        prerelease: false,
        published_at: None,
        url: format!("{BASE}/tag/v{version}"),
        notes: String::new(),
        assets: names
            .iter()
            .map(|n| Asset {
                name: (*n).into(),
                size: 10,
                url: format!("{BASE}/download/v{version}/{n}"),
            })
            .collect(),
    }
}

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

// ---------------------------------------------------------------- versions

#[test]
fn version_parsing() {
    assert_eq!(v("v0.4.0"), Version::new(0, 4, 0));
    assert_eq!(v(" V1.2.3 "), Version::new(1, 2, 3));
    assert_eq!(v("0.4.0-beta.1").pre, ["beta", "1"]);
    assert_eq!(v("1.0.0+build.7"), Version::new(1, 0, 0));
    assert_eq!(v("0.4.0-rc.2").to_string(), "0.4.0-rc.2");
    for bad in [
        "0.4",
        "latest",
        "v1.2.3.4",
        "1.02.0",
        "nightly-2026-10-01",
        "1.0.0-",
        "",
    ] {
        assert!(Version::parse(bad).is_none(), "{bad}");
    }
}

#[test]
fn version_order_is_semver() {
    let ordered = [
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-alpha.beta",
        "1.0.0-beta",
        "1.0.0-beta.2",
        "1.0.0-beta.11",
        "1.0.0-rc.1",
        "1.0.0",
        "1.0.1",
        "1.1.0",
        "2.0.0",
    ];
    let parsed: Vec<Version> = ordered.iter().map(|s| v(s)).collect();
    let mut sorted = parsed.clone();
    sorted.reverse();
    sorted.sort();
    assert_eq!(sorted, parsed);
    assert!(v("0.9.0") < v("0.10.0") && v("1.0.0") > v("1.0.0-rc.1"));
}

#[test]
fn every_part_is_a_number_of_any_length() {
    let ordered = [
        "0.3.2",
        "0.3.9",
        "0.3.10",
        "0.3.11",
        "0.3.99",
        "0.3.100",
        "0.9.12",
        "0.10.0",
        "0.99.99",
        "0.100.0",
        "1.0.0",
        "1.9.0",
        "1.10.0",
        "9.0.0",
        "10.0.0",
        "100.200.300",
    ];
    let mut parsed: Vec<Version> = ordered.iter().rev().map(|s| v(s)).collect();
    parsed.sort();
    assert_eq!(
        parsed.iter().map(Version::to_string).collect::<Vec<_>>(),
        ordered
    );
    assert!(v("0.3.10-beta.10") > v("0.3.10-beta.9"));
    let list = parse_releases(
        &json!([gh("v0.3.9"), gh("v0.3.10"), gh("v0.3.2")]),
        RELEASES_URL,
    )
    .unwrap();
    assert_eq!(
        choose(&list, UpdateChannel::Stable).unwrap().version,
        "0.3.10"
    );
}

#[test]
fn levels() {
    use UpdateLevel::*;
    for (cur, latest, want) in [
        ("0.3.9", Some("0.3.10"), Patch),
        ("0.3.10", Some("0.3.100"), Patch),
        ("0.3.100", Some("0.4.0"), Minor),
        ("0.9.12", Some("0.10.0"), Minor),
        ("0.10.0", Some("0.10.15"), Patch),
        ("0.99.99", Some("1.0.0"), Major),
        ("9.9.9", Some("10.0.0"), Major),
        ("0.3.10", Some("0.3.9"), None),
        ("0.10.0", Some("0.9.99"), None),
        ("0.3.10-beta.9", Some("0.3.10-beta.10"), Patch),
        ("0.3.0", Some("0.4.0-beta.1"), Minor),
        ("0.4.0-beta.1", Some("0.4.0"), Patch),
        ("0.4.0", Some("0.4.0"), None),
        ("v0.3.0", Some("v0.3.1"), Patch),
        ("dev", Some("0.4.0"), None),
        ("0.3.0", Option::None, None),
    ] {
        assert_eq!(level(Some(cur), latest), want, "{cur} {latest:?}");
    }
}

#[test]
fn series_that_can_be_muted() {
    assert_eq!(series(Some("0.3.2"), Some("0.4.1")).as_deref(), Some("0.4"));
    assert_eq!(
        series(Some("0.3.2"), Some("0.4.0-beta.1")).as_deref(),
        Some("0.4")
    );
    assert_eq!(series(Some("0.3.2"), Some("1.2.0")).as_deref(), Some("1"));
    assert_eq!(
        series(Some("0.9.9"), Some("0.10.0")).as_deref(),
        Some("0.10")
    );
    assert_eq!(series(Some("0.3.2"), Some("0.3.3")), None);
    assert_eq!(series(Some("0.4.0"), Some("0.4.0")), None);
    assert_eq!(series(Some("dev"), Some("0.4.0")), None);
    assert!(series_reached("0.4", Some("0.4.0")));
    assert!(series_reached("0.4", Some("0.5.1")) && series_reached("0.4", Some("1.0.0")));
    assert!(!series_reached("0.4", Some("0.3.9")));
    assert!(!series_reached("0.10", Some("0.9.0")));
    assert!(series_reached("1", Some("1.0.0")) && !series_reached("1", Some("0.9.0")));
    assert!(series_reached("junk", Some("0.3.0")));
    assert!(!series_reached("0.4", None));
}

// ---------------------------------------------------------------- releases and channels

#[test]
fn parse_releases_skips_drafts_and_odd_tags() {
    let data = json!([
        gh_with("v0.5.0", |r| r["draft"] = json!(true)),
        gh("nightly"),
        gh_with("v0.4.0-beta.1", |r| r["prerelease"] = json!(true)),
        gh("v0.4.0-rc.1"),
        gh_with("v0.3.1", |r| r["published_at"] = json!("not a date")),
        gh_with("v0.3.0", |r| {
            r["assets"] = json!([{"name": "SpaceKnife-0.3.0-Setup-x64.exe", "size": 10,
                "browser_download_url": format!("{BASE}/download/v0.3.0/SpaceKnife-0.3.0-Setup-x64.exe")}])
        }),
        "junk",
    ]);
    let got = parse_releases(&data, RELEASES_URL).unwrap();
    assert_eq!(
        got.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(),
        ["0.4.0-rc.1", "0.4.0-beta.1", "0.3.1", "0.3.0"]
    );
    assert!(got[1].prerelease && !got[0].prerelease && got[0].preview());
    assert!(got[2].published_at.is_none() && got[2].notes == "notes");
    assert_eq!(
        got[3].published_at,
        DateTime::parse_from_rfc3339("2026-10-01T12:00:00Z")
            .ok()
            .map(|d| d.with_timezone(&Utc))
    );
    assert_eq!(got[3].assets[0].name, "SpaceKnife-0.3.0-Setup-x64.exe");
    assert_eq!(got[3].assets[0].size, 10);
    assert!(
        parse_releases(&json!({"message": "Not Found"}), RELEASES_URL)
            .unwrap_err()
            .contains("release list")
    );
}

#[test]
fn choose_by_channel() {
    let pre = |t: &str| gh_with(t, |r| r["prerelease"] = json!(true));
    let releases = parse_releases(
        &json!([
            gh("v0.3.1"),
            pre("v0.4.0-beta.2"),
            pre("v0.4.0-beta.10"),
            gh("v0.3.0")
        ]),
        RELEASES_URL,
    )
    .unwrap();
    assert_eq!(
        choose(&releases, UpdateChannel::Preview).unwrap().version,
        "0.4.0-beta.10"
    );
    assert_eq!(
        choose(&releases, UpdateChannel::Stable).unwrap().version,
        "0.3.1"
    );
    assert!(choose(&[], UpdateChannel::Preview).is_none());
    let previews: Vec<Release> = releases.iter().filter(|r| r.preview()).cloned().collect();
    assert!(choose(&previews, UpdateChannel::Stable).is_none());
    assert_eq!(
        trim(&releases)
            .iter()
            .map(|r| r.version.as_str())
            .collect::<Vec<_>>(),
        ["0.4.0-beta.10", "0.4.0-beta.2", "0.3.1", "0.3.0"]
    );
}

#[test]
fn trim_keeps_the_newest_releases() {
    let mut items: Vec<Value> = (0..4)
        .flat_map(|m| (0..12).map(move |p| gh(&format!("v0.{m}.{p}"))))
        .collect();
    items.push(gh("nightly"));
    let kept = trim(&parse_releases(&Value::Array(items), RELEASES_URL).unwrap());
    assert_eq!(kept.len(), HISTORY_KEEP);
    assert_eq!(kept[0].version, "0.3.11");
    assert_eq!(kept.last().unwrap().version, "0.1.6");
}

#[test]
fn only_github_urls_are_kept() {
    let evil = gh_with("v0.4.0", |r| {
        r["html_url"] = json!("https://example.com/phish");
        r["assets"] = json!([{"name": "SpaceKnife-0.4.0-Setup-x64.exe", "size": 5, "browser_download_url": "https://x.io/a"}]);
    });
    let got = parse_releases(&json!([evil]), RELEASES_URL).unwrap();
    assert_eq!(
        got[0].url,
        "https://github.com/DuskXi/SpaceKnife/releases/tag/v0.4.0"
    );
    assert!(got[0].assets.is_empty());
    assert!(allowed_url(
        "https://github.com/DuskXi/SpaceKnife/releases/download/v1/x.exe",
        RELEASES_URL
    ));
    assert!(!allowed_url("http://github.com/x", RELEASES_URL));
    assert!(!allowed_url("https://github.com.evil.io/x", RELEASES_URL));
    let local = "http://127.0.0.1:8123/releases";
    assert!(allowed_url("http://127.0.0.1:8123/files/x.exe", local));
    assert!(!allowed_url("http://127.0.0.1:9999/files/x.exe", local));
}

#[test]
fn notes_are_capped() {
    let long: String = (0..5000)
        .map(|i| format!("- 修复第 {i} 个问题"))
        .collect::<Vec<_>>()
        .join("\n");
    let got = parse_releases(
        &json!([gh_with("v0.4.0", |r| r["body"] = json!(long))]),
        RELEASES_URL,
    )
    .unwrap();
    assert!(got[0].notes.len() <= NOTES_MAX + 4 && got[0].notes.ends_with("\n…"));
    assert_eq!(cap_notes("short", NOTES_MAX), "short");
}

#[test]
fn info() {
    let r = release("0.4.0", &["SpaceKnife-0.4.0-Setup-x64.exe"]);
    let sha = "ab".repeat(32);
    let i = to_info(&r, r.assets.first(), Some(&sha));
    assert_eq!(
        (i.version.as_str(), i.tag.as_str(), i.name.as_str()),
        ("0.4.0", "v0.4.0", "v0.4.0")
    );
    let a = i.asset.unwrap();
    assert_eq!((a.sha256.as_deref(), a.size), (Some(sha.as_str()), 10));
    assert!(to_info(&r, None, None).asset.is_none());
}

// ---------------------------------------------------------------- the file for this install

const ALL_ASSETS: &[&str] = &[
    "SpaceKnife_0.4.0_x64-setup.exe",
    "SpaceKnife_0.4.0_x64-setup.exe.sig",
    "SpaceKnife_0.4.0_amd64.AppImage",
    "SpaceKnife_0.4.0_amd64.deb",
    "SpaceKnife-0.4.0-1.x86_64.rpm",
    "SpaceKnife_0.4.0_aarch64.dmg",
    "SpaceKnife_0.4.0_x64.dmg",
    "SpaceKnife-0.4.0-Setup-x64.exe",
    "SpaceKnife-0.4.0-win-x64-portable.zip",
    "SpaceKnife-0.4.0-win-arm64-portable.zip",
    "SpaceKnife-0.4.0-linux-x64-portable.tar.gz",
    "SpaceKnife-0.4.0-linux-arm64-portable.tar.gz",
    "SpaceKnife-0.4.0-mac-arm64-portable.zip",
    "SpaceKnife-0.4.0-mac-x64-portable.zip",
    "SpaceKnife-0.4.0-x86_64.AppImage",
    "spaceknife_0.4.0_amd64.deb",
    "spaceknife-0.4.0.x86_64.rpm",
    "SpaceKnife-0.4.0-mac-arm64.dmg",
    "SpaceKnife-0.4.0-mac-x64.dmg",
    "latest.yml",
    "SHA256SUMS.txt",
    "SHA256SUMS-mac-arm64.txt",
    "SHA256SUMS-portable-win-x64.txt",
];

#[test]
fn pick_assets() {
    use InstallKind::*;
    use Shell::*;
    let r = release("0.4.0", ALL_ASSETS);
    // 0.3.4's names (its update.py asset_templates), which every later version keeps: an installed
    // 0.3.x looks for exactly these. Each kind of install is built by one shell; the other shell
    // has no file for it. The Windows installer is Tauri's (the forked NSIS template, which takes
    // over 0.3.4's electron-builder installation; 0.3.4's own update code finds it by this name).
    for (kind, shell, system, machine, name) in [
        (
            Nsis,
            Tauri,
            "win32",
            "AMD64",
            Some("SpaceKnife-0.4.0-Setup-x64.exe"),
        ),
        (
            Nsis,
            Tauri,
            "win32",
            "ARM64",
            Some("SpaceKnife-0.4.0-Setup-x64.exe"),
        ),
        (Nsis, Electron, "win32", "AMD64", None),
        (
            Portable,
            Tauri,
            "win32",
            "AMD64",
            Some("SpaceKnife-0.4.0-win-x64-portable.zip"),
        ),
        (
            Portable,
            Tauri,
            "linux",
            "x86_64",
            Some("SpaceKnife-0.4.0-linux-x64-portable.tar.gz"),
        ),
        (
            Portable,
            Tauri,
            "win32",
            "ARM64",
            Some("SpaceKnife-0.4.0-win-arm64-portable.zip"),
        ),
        (
            Portable,
            Tauri,
            "linux",
            "aarch64",
            Some("SpaceKnife-0.4.0-linux-arm64-portable.tar.gz"),
        ),
        (
            Portable,
            Tauri,
            "darwin",
            "arm64",
            Some("SpaceKnife-0.4.0-mac-arm64-portable.zip"),
        ),
        (
            Portable,
            Tauri,
            "darwin",
            "x86_64",
            Some("SpaceKnife-0.4.0-mac-x64-portable.zip"),
        ),
        (Portable, Electron, "win32", "AMD64", None),
        (Portable, Tauri, "linux", "riscv64", None),
        (
            Appimage,
            Electron,
            "linux",
            "x86_64",
            Some("SpaceKnife-0.4.0-x86_64.AppImage"),
        ),
        (Appimage, Electron, "linux", "aarch64", None),
        (Appimage, Tauri, "linux", "x86_64", None),
        (
            Deb,
            Tauri,
            "linux",
            "x86_64",
            Some("spaceknife_0.4.0_amd64.deb"),
        ),
        (Deb, Electron, "linux", "x86_64", None),
        (
            Rpm,
            Electron,
            "linux",
            "x86_64",
            Some("spaceknife-0.4.0.x86_64.rpm"),
        ),
        (Rpm, Tauri, "linux", "x86_64", None),
        (
            Dmg,
            Tauri,
            "darwin",
            "arm64",
            Some("SpaceKnife-0.4.0-mac-arm64.dmg"),
        ),
        (
            Dmg,
            Tauri,
            "darwin",
            "x86_64",
            Some("SpaceKnife-0.4.0-mac-x64.dmg"),
        ),
        (Dmg, Electron, "darwin", "arm64", None),
        (Dev, Tauri, "win32", "AMD64", None),
        (InstallKind::Unknown, Electron, "linux", "x86_64", None),
        (Deb, Shell::Unknown, "linux", "x86_64", None),
        (Nsis, Shell::Unknown, "win32", "AMD64", None),
    ] {
        let got = pick_asset(&r, kind, shell, Some(system), Some(machine)).map(|a| a.name.as_str());
        assert_eq!(got, name, "{kind:?} {shell:?} {system} {machine}");
    }
}

#[test]
fn the_names_are_the_ones_0_3_4_looks_for() {
    use InstallKind::*;
    use Shell::*;
    // v0.3.4 packages/backend/src/spaceknife/update.py asset_templates, word for word
    let table: &[(InstallKind, Shell, &str, &str, &[&str])] = &[
        (
            Nsis,
            Tauri,
            "win32",
            "AMD64",
            &["SpaceKnife-{v}-Setup-x64.exe"],
        ),
        (
            Portable,
            Tauri,
            "win32",
            "ARM64",
            &[
                "SpaceKnife-{v}-win-arm64-portable.zip",
                "SpaceKnife-{v}-win-x64-portable.zip",
            ],
        ),
        (
            Portable,
            Tauri,
            "win32",
            "AMD64",
            &["SpaceKnife-{v}-win-x64-portable.zip"],
        ),
        (
            Portable,
            Tauri,
            "linux",
            "x86_64",
            &["SpaceKnife-{v}-linux-x64-portable.tar.gz"],
        ),
        (
            Portable,
            Tauri,
            "linux",
            "aarch64",
            &["SpaceKnife-{v}-linux-arm64-portable.tar.gz"],
        ),
        (
            Portable,
            Tauri,
            "darwin",
            "arm64",
            &["SpaceKnife-{v}-mac-arm64-portable.zip"],
        ),
        (
            Portable,
            Tauri,
            "darwin",
            "x86_64",
            &["SpaceKnife-{v}-mac-x64-portable.zip"],
        ),
        (
            Appimage,
            Electron,
            "linux",
            "x86_64",
            &["SpaceKnife-{v}-x86_64.AppImage"],
        ),
        (Deb, Tauri, "linux", "x86_64", &["spaceknife_{v}_amd64.deb"]),
        (
            Rpm,
            Electron,
            "linux",
            "x86_64",
            &["spaceknife-{v}.x86_64.rpm"],
        ),
        (
            Dmg,
            Tauri,
            "darwin",
            "arm64",
            &["SpaceKnife-{v}-mac-arm64.dmg"],
        ),
        (
            Dmg,
            Tauri,
            "darwin",
            "x86_64",
            &["SpaceKnife-{v}-mac-x64.dmg"],
        ),
    ];
    for &(kind, shell, system, machine, want) in table {
        assert_eq!(
            asset_templates(kind, shell, system, machine),
            want,
            "{kind:?} {system} {machine}"
        );
    }
}

#[test]
fn the_shell_comes_from_the_launcher() {
    assert_eq!(shell_from(""), Shell::Tauri); // the Tauri app runs the backend in its own process
    assert_eq!(shell_from(" tauri "), Shell::Tauri);
    assert_eq!(shell_from("Electron"), Shell::Electron);
    assert_eq!(shell_from("qt"), Shell::Unknown);
    assert_eq!(Shell::Electron.as_str(), "electron");
}

#[test]
fn the_processor_is_read_at_run_time() {
    // an x64 build that Windows on ARM (or Rosetta) runs emulated is on an arm64 processor
    assert_eq!(windows_machine_arch(0xAA64), Some("arm64"));
    assert_eq!(windows_machine_arch(0x8664), Some("x64"));
    assert_eq!(windows_machine_arch(0x014C), Some("x86"));
    assert_eq!(windows_machine_arch(0), None);
    let m = machine();
    assert!(["x64", "arm64"].contains(&m.as_str()), "{m}");
    // without a machine, the file is the one of the processor this runs on
    let r = release(
        "0.4.0",
        &[
            "SpaceKnife-0.4.0-win-x64-portable.zip",
            "SpaceKnife-0.4.0-win-arm64-portable.zip",
            "SpaceKnife-0.4.0-linux-x64-portable.tar.gz",
            "SpaceKnife-0.4.0-linux-arm64-portable.tar.gz",
            "SpaceKnife-0.4.0-mac-x64-portable.zip",
            "SpaceKnife-0.4.0-mac-arm64-portable.zip",
        ],
    );
    let got =
        pick_asset(&r, InstallKind::Portable, Shell::Tauri, None, None).map(|a| a.name.clone());
    let os = match system() {
        "win32" => "win",
        "darwin" => "mac",
        _ => "linux",
    };
    assert!(
        got.as_deref()
            .is_some_and(|n| n.contains(&format!("-{os}-{m}-"))),
        "{got:?} on {m}"
    );
}

#[test]
fn a_portable_copy_on_windows_on_arm_keeps_the_x64_build_without_an_arm_one() {
    // a release before the ARM builds (or one whose ARM build failed): Windows runs x64 emulated
    let r = release(
        "0.4.0",
        &[
            "SpaceKnife-0.4.0-win-x64-portable.zip",
            "SpaceKnife-0.4.0-linux-x64-portable.tar.gz",
        ],
    );
    let pick = |system: &str, machine: &str| {
        pick_asset(
            &r,
            InstallKind::Portable,
            Shell::Tauri,
            Some(system),
            Some(machine),
        )
        .map(|a| a.name.clone())
    };
    assert_eq!(
        pick("win32", "ARM64").as_deref(),
        Some("SpaceKnife-0.4.0-win-x64-portable.zip")
    );
    // Linux runs no x64 build on ARM
    assert!(pick("linux", "aarch64").is_none());
    assert_eq!(
        asset_template(InstallKind::Portable, Shell::Tauri, "win32", "ARM64").as_deref(),
        Some("SpaceKnife-{v}-win-arm64-portable.zip")
    );
    assert!(asset_template(InstallKind::Dev, Shell::Tauri, "win32", "AMD64").is_none());
}

#[test]
fn the_arm_and_mac_portable_files_are_update_files() {
    for name in [
        "SpaceKnife-0.4.0-win-arm64-portable.zip",
        "SpaceKnife-0.4.0-linux-arm64-portable.tar.gz",
        "SpaceKnife-0.4.0-mac-arm64-portable.zip",
        "SpaceKnife-0.4.0-mac-x64-portable.zip",
    ] {
        assert_eq!(
            update_file_version(name).map(|v| v.to_string()).as_deref(),
            Some("0.4.0"),
            "{name}"
        );
    }
}

#[test]
fn update_checks_verify_certificates_through_the_system() {
    // the operating system's verifier (Windows CryptoAPI, macOS Security framework, the platform
    // store on Linux): a certificate the system has blocked is refused here too
    for url in [RELEASES_URL, "http://127.0.0.1:9/releases"] {
        let a = agent(url, TIMEOUT, None);
        assert_eq!(a.config().timeout_per_read(), Some(TIMEOUT)); // review issue 11
        assert!(
            matches!(
                a.config().tls_config().root_certs(),
                ureq::tls::RootCerts::PlatformVerifier
            ),
            "{url}"
        );
    }
}

#[test]
fn pick_asset_tolerates_the_packagers_spelling() {
    let r = release(
        "0.4.0-beta.1",
        &[
            "spaceknife-0.4.0~beta.1.x86_64.rpm",
            "spaceknife_0.4.0~beta.1_amd64.deb",
        ],
    );
    let pick = |k, s| pick_asset(&r, k, s, Some("linux"), Some("x86_64")).map(|a| a.name.clone());
    assert_eq!(
        pick(InstallKind::Rpm, Shell::Electron).as_deref(),
        Some("spaceknife-0.4.0~beta.1.x86_64.rpm")
    );
    assert_eq!(
        pick(InstallKind::Deb, Shell::Tauri).as_deref(),
        Some("spaceknife_0.4.0~beta.1_amd64.deb")
    );
    let nsis = |r: &Release| {
        pick_asset(
            r,
            InstallKind::Nsis,
            Shell::Tauri,
            Some("win32"),
            Some("AMD64"),
        )
        .is_none()
    };
    assert!(nsis(&release(
        "0.4.0",
        &["SpaceKnife-../../x-Setup-x64.exe"]
    )));
    assert!(nsis(&release("0.4.0", &[])));
}

// ---------------------------------------------------------------- checksums

#[test]
fn checksums() {
    let (a, b, c) = ("a".repeat(64), "B".repeat(64), "c".repeat(64));
    let text = format!(
        "{a}  SpaceKnife-0.4.0-Setup-x64.exe\n{b} *spaceknife_0.4.0_amd64.deb\n{c}  dist/electron/SpaceKnife-0.4.0-mac-arm64.dmg\nnot a checksum line\n1234  short.txt\n"
    );
    assert_eq!(
        parse_checksums(&text),
        [
            ("SpaceKnife-0.4.0-Setup-x64.exe".to_owned(), a.clone()),
            ("spaceknife_0.4.0_amd64.deb".to_owned(), "b".repeat(64)),
            ("SpaceKnife-0.4.0-mac-arm64.dmg".to_owned(), c.clone()),
        ]
    );
    assert!(is_checksum_file("SHA256SUMS.txt") && is_checksum_file("SHA256SUMS-mac-x64.txt"));
    assert!(!is_checksum_file("SHA256SUMS.txt.sig") && !is_checksum_file("notes.txt"));
    let r = release("0.4.0", ALL_ASSETS);
    let order = |name: &str| -> Vec<String> {
        checksum_files(
            &r,
            &Asset {
                name: name.into(),
                size: 0,
                url: "u".into(),
            },
        )
        .iter()
        .map(|a| a.name.clone())
        .collect()
    };
    assert_eq!(
        order("SpaceKnife-0.4.0-mac-arm64.dmg")[0],
        "SHA256SUMS-mac-arm64.txt"
    );
    assert_eq!(order("SpaceKnife_0.4.0_x64-setup.exe")[0], "SHA256SUMS.txt");
    assert_eq!(
        order("SpaceKnife-0.4.0-win-x64-portable.zip")[0],
        "SHA256SUMS-portable-win-x64.txt"
    );
    assert_eq!(order("SpaceKnife-0.4.0-Setup-x64.exe")[0], "SHA256SUMS.txt");
    assert_eq!(order("x").len(), 3);
}

// ---------------------------------------------------------------- environment and files

#[test]
fn update_file_versions() {
    let f = |n: &str| update_file_version(n).map(|v| v.to_string());
    assert_eq!(
        f("SpaceKnife-0.3.2-Setup-x64.exe").as_deref(),
        Some("0.3.2")
    );
    assert_eq!(
        f("SpaceKnife-0.4.0-beta.1-win-x64-portable.zip").as_deref(),
        Some("0.4.0-beta.1")
    );
    assert_eq!(
        f("SpaceKnife-0.3.10-linux-x64-portable.tar.gz").as_deref(),
        Some("0.3.10")
    );
    assert_eq!(
        f("SpaceKnife-0.3.2-x86_64.AppImage").as_deref(),
        Some("0.3.2")
    );
    assert_eq!(
        f("spaceknife_0.4.0~beta.1_amd64.deb").as_deref(),
        Some("0.4.0-beta.1")
    );
    assert_eq!(f("spaceknife-0.3.2.x86_64.rpm").as_deref(), Some("0.3.2"));
    assert_eq!(
        f("SpaceKnife-0.3.2-mac-arm64.dmg").as_deref(),
        Some("0.3.2")
    );
    assert_eq!(
        f("SpaceKnife_0.4.1_x64-setup.exe").as_deref(),
        Some("0.4.1")
    );
    assert_eq!(
        f("SpaceKnife_0.4.1-beta.2_amd64.AppImage").as_deref(),
        Some("0.4.1-beta.2")
    );
    assert_eq!(f("SpaceKnife_0.4.1_amd64.deb").as_deref(), Some("0.4.1"));
    assert_eq!(f("SpaceKnife-0.4.1-1.x86_64.rpm").as_deref(), Some("0.4.1"));
    assert_eq!(f("SpaceKnife_0.4.1_aarch64.dmg").as_deref(), Some("0.4.1"));
    assert_eq!(f("staging-0.3.3").as_deref(), Some("0.3.3"));
    assert_eq!(f("previous-0.3.2").as_deref(), Some("0.3.2"));
    for n in ["update-0.3.3.ps1", "update-0.3.3.sh", "update-0.3.3.log"] {
        assert_eq!(f(n).as_deref(), Some("0.3.3"));
    }
    for other in [
        "notes.txt",
        "SpaceKnife-Setup.exe",
        "staging-",
        "SpaceKnife-0.3.2-Setup-x64.exe.part",
    ] {
        assert!(f(other).is_none(), "{other}");
    }
}

#[test]
fn clean_updates_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("updates");
    std::fs::create_dir(&folder).unwrap();
    for name in [
        "SpaceKnife-0.3.1-Setup-x64.exe",
        "SpaceKnife-0.3.2-Setup-x64.exe",
        "SpaceKnife-0.3.3-Setup-x64.exe",
        "SpaceKnife-0.3.3-Setup-x64.exe.part",
        "SpaceKnife-0.4.0-beta.1-x86_64.AppImage",
        "update-0.3.2.json",
        "update-0.3.2.log",
        "readme.txt",
    ] {
        std::fs::write(folder.join(name), "x").unwrap();
    }
    for name in ["staging-0.3.2", "previous-0.3.1", "staging-0.3.3"] {
        std::fs::create_dir_all(folder.join(name).join("sub")).unwrap();
        std::fs::write(folder.join(name).join("sub/f"), "x").unwrap();
    }
    let mut removed = clean_updates(&folder, Some("0.3.2"));
    removed.sort();
    assert_eq!(
        removed,
        [
            "SpaceKnife-0.3.1-Setup-x64.exe",
            "SpaceKnife-0.3.2-Setup-x64.exe",
            "SpaceKnife-0.3.3-Setup-x64.exe.part",
            "previous-0.3.1",
            "staging-0.3.2",
            "update-0.3.2.json",
            "update-0.3.2.log",
        ]
    );
    let mut left: Vec<String> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "SpaceKnife-0.3.3-Setup-x64.exe",
            "SpaceKnife-0.4.0-beta.1-x86_64.AppImage",
            "readme.txt",
            "staging-0.3.3"
        ]
    );
    std::fs::write(folder.join("SpaceKnife-0.3.4-Setup-x64.exe.part"), "x").unwrap();
    assert_eq!(
        clean_updates(&folder, None),
        ["SpaceKnife-0.3.4-Setup-x64.exe.part"]
    );
    assert!(clean_updates(&tmp.path().join("missing"), Some("0.3.2")).is_empty());
}

#[test]
fn what_is_still_held_after_an_update_is_removed_later() {
    // a 0.3.4 portable copy updated to 0.3.5: the swap left the Electron app's files in
    // previous-0.3.4, and the new version's first cleaning ran while 0.3.4's last processes still
    // held some of them (seen on Windows: ffmpeg.dll), leaving hundreds of MB behind until the next
    // start; what is left is tried again, and nothing else
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("updates");
    std::fs::create_dir_all(folder.join("previous-0.3.4").join("locales")).unwrap();
    std::fs::write(folder.join("previous-0.3.4").join("ffmpeg.dll"), "x").unwrap();
    std::fs::create_dir_all(
        folder
            .join("staging-0.3.5")
            .join("SpaceKnife-0.3.5-win-x64-portable"),
    )
    .unwrap();
    std::fs::write(folder.join("SpaceKnife-0.3.5-win-x64-portable.zip"), "x").unwrap();
    std::fs::write(folder.join("update-0.3.5.log"), "updated").unwrap();
    let held = std::cell::Cell::new(true);
    let remove = |p: &Path| {
        if held.get() && p.file_name().is_some_and(|n| n == "previous-0.3.4") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "in use",
            ));
        }
        remove_entry(p)
    };
    let first = clean_updates_with(&folder, Some("0.3.5"), &remove);
    assert_eq!(
        first.removed,
        [
            "SpaceKnife-0.3.5-win-x64-portable.zip",
            "staging-0.3.5",
            "update-0.3.5.log"
        ]
    );
    assert_eq!(first.left, ["previous-0.3.4"]);
    // meanwhile a newer release's download started, and something else was put there
    std::fs::write(
        folder.join("SpaceKnife-0.3.6-win-x64-portable.zip.part"),
        "x",
    )
    .unwrap();
    std::fs::write(folder.join("update-0.3.4.json"), "x").unwrap();
    let again = clean_again_with(&folder, Some("0.3.5"), &first.left, &remove);
    assert_eq!(
        again,
        Cleaned {
            removed: vec![],
            left: vec!["previous-0.3.4".into()]
        }
    );
    held.set(false);
    let again = clean_again_with(&folder, Some("0.3.5"), &first.left, &remove);
    assert_eq!(again.removed, ["previous-0.3.4"]);
    assert!(again.left.is_empty());
    let mut left: Vec<String> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "SpaceKnife-0.3.6-win-x64-portable.zip.part",
            "update-0.3.4.json"
        ]
    );
    // a partial download is never tried again
    let names = ["SpaceKnife-0.3.6-win-x64-portable.zip.part".to_owned()];
    assert_eq!(
        clean_again(&folder, Some("0.3.5"), &names),
        Cleaned::default()
    );
}

// ---------------------------------------------------------------- state

#[test]
fn state_round_trip_and_damage() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("data").join("update.json");
    assert_eq!(load_state(&file, RELEASES_URL), UpdateState::default());
    let state = UpdateState {
        checked_at: DateTime::parse_from_rfc3339("2026-10-20T08:00:00Z")
            .ok()
            .map(|d| d.with_timezone(&Utc)),
        etag: Some("W/\"abc\"".into()),
        releases: vec![release("0.4.0", &["SpaceKnife-0.4.0-Setup-x64.exe"])],
        dismissed: vec!["0.4.0".into()],
        ..UpdateState::default()
    };
    save_state(&state, &file).unwrap();
    assert_eq!(load_state(&file, RELEASES_URL), state);
    assert!(!file.with_file_name("update.json.tmp").exists());
    let other = load_state(&file, "http://127.0.0.1:1/releases");
    assert!(other.releases.is_empty() && other.etag.is_none() && other.dismissed == ["0.4.0"]);
    std::fs::write(&file, "{ not json").unwrap();
    assert_eq!(load_state(&file, RELEASES_URL), UpdateState::default());
    std::fs::write(&file, r#"{"checked_at": "2026-10-20T08:00:00"}"#).unwrap(); // naive time
    assert_eq!(load_state(&file, RELEASES_URL), UpdateState::default());
}

// ---------------------------------------------------------------- the release list

const UA: &str = "SpaceKnife/0.3.0";

#[test]
fn fetch_with_etag() {
    let feed = LocalFeed::start();
    let body = json!([gh("v0.4.0"), gh("v0.3.0")]).to_string().into_bytes();
    feed.route("/releases", move |h| {
        if h.get("if-none-match").map(String::as_str) == Some("\"v1\"") {
            return (304, vec![("ETag".into(), "\"v1\"".into())], vec![]);
        }
        (
            200,
            vec![
                ("ETag".into(), "\"v1\"".into()),
                ("Content-Length".into(), body.len().to_string()),
            ],
            body.clone(),
        )
    });
    let got = fetch_releases(&feed.url("/releases"), None, UA, TIMEOUT).unwrap();
    assert_eq!(got.etag.as_deref(), Some("\"v1\""));
    assert_eq!(
        got.releases
            .unwrap()
            .iter()
            .map(|r| r.version.as_str())
            .collect::<Vec<_>>(),
        ["0.4.0", "0.3.0"]
    );
    let again = fetch_releases(&feed.url("/releases"), Some("\"v1\""), UA, TIMEOUT).unwrap();
    assert!(again.releases.is_none() && again.etag.as_deref() == Some("\"v1\""));
    let reqs = feed.requests.lock().unwrap();
    let (first, second) = (&reqs[0].1, &reqs[1].1);
    assert_eq!(first["user-agent"], UA);
    assert!(!first.contains_key("if-none-match"));
    assert_eq!(second["if-none-match"], "\"v1\"");
    assert!(!first.contains_key("authorization") && !first.contains_key("cookie"));
}

#[test]
fn fetch_errors() {
    let _g = crate::i18n::TEST_LOCK.lock();
    let feed = LocalFeed::start();
    let reset = (Utc::now().timestamp() + 600).to_string();
    feed.route("/limited", move |_| {
        (
            403,
            vec![
                ("X-RateLimit-Remaining".into(), "0".into()),
                ("X-RateLimit-Reset".into(), reset.clone()),
            ],
            br#"{"message": "API rate limit exceeded"}"#.to_vec(),
        )
    });
    feed.route("/limited-plain", |_| {
        (
            403,
            vec![],
            br#"{"message": "API rate limit exceeded for 1.2.3.4"}"#.to_vec(),
        )
    });
    feed.route("/forbidden", |_| (403, vec![], b"no".to_vec()));
    feed.route("/broken", |_| (500, vec![], b"oops".to_vec()));
    feed.file("/garbage", b"<html>not json</html>".to_vec(), true);
    feed.file("/object", br#"{"message": "Not Found"}"#.to_vec(), true);
    feed.route("/slow", |_| {
        std::thread::sleep(Duration::from_secs(1));
        (200, vec![], b"[]".to_vec())
    });
    let error = |path: &str| fetch_releases(&feed.url(path), None, UA, TIMEOUT).unwrap_err();
    let limited = error("/limited");
    assert!(
        matches!(limited, UpdateError::RateUntil { minutes: 10 | 11 }),
        "{limited:?}"
    );
    assert!(limited.message().contains("10") || limited.message().contains("11"));
    assert_eq!(error("/limited-plain"), UpdateError::Rate);
    assert_eq!(error("/forbidden"), UpdateError::Http { code: 403 });
    assert_eq!(error("/broken").message(), "GitHub 返回了错误：HTTP 500");
    assert_eq!(error("/garbage"), UpdateError::BadList);
    assert_eq!(error("/object"), UpdateError::BadList);
    let slow =
        fetch_releases(&feed.url("/slow"), None, UA, Duration::from_millis(200)).unwrap_err();
    assert_eq!(slow, UpdateError::Timeout);
    assert_eq!(slow.message(), "连接 GitHub 超时");
}

#[test]
fn fetch_offline() {
    let _g = crate::i18n::TEST_LOCK.lock();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port(); // nobody listens
    let err = fetch_releases(
        &format!("http://127.0.0.1:{port}/releases"),
        None,
        UA,
        Duration::from_secs(5),
    )
    .unwrap_err();
    assert_eq!(err.key(), "offline", "{err:?}");
    assert!(err.message().starts_with("连不上 GitHub："));
}

#[test]
fn fetch_checksum_files() {
    let _g = crate::i18n::TEST_LOCK.lock();
    let feed = LocalFeed::start();
    let digest = "d".repeat(64);
    let all = feed.file(
        "/SHA256SUMS.txt",
        format!("{}  other.exe\n", "e".repeat(64)).into_bytes(),
        true,
    );
    let mac = feed.file(
        "/SHA256SUMS-mac-arm64.txt",
        format!("{digest}  SpaceKnife-0.4.0-mac-arm64.dmg\n").into_bytes(),
        true,
    );
    let mut r = release("0.4.0", &[]);
    r.assets = vec![
        Asset {
            name: "SHA256SUMS.txt".into(),
            size: 0,
            url: all,
        },
        Asset {
            name: "SHA256SUMS-mac-arm64.txt".into(),
            size: 0,
            url: mac,
        },
    ];
    let dmg = Asset {
        name: "SpaceKnife-0.4.0-mac-arm64.dmg".into(),
        size: 0,
        url: "u".into(),
    };
    assert_eq!(
        fetch_checksums(&r, &dmg, UA, TIMEOUT).unwrap(),
        Some(digest)
    );
    assert_eq!(feed.hits("/SHA256SUMS.txt"), 0); // the likeliest file had it
    let none = Asset {
        name: "none.exe".into(),
        size: 0,
        url: "u".into(),
    };
    assert_eq!(fetch_checksums(&r, &none, UA, TIMEOUT).unwrap(), None);
    assert_eq!(
        fetch_checksums(&release("0.4.0", &[]), &dmg, UA, TIMEOUT).unwrap(),
        None
    );
    r.assets = vec![Asset {
        name: "SHA256SUMS.txt".into(),
        size: 0,
        url: feed.url("/missing"),
    }];
    let err = fetch_checksums(&r, &dmg, UA, TIMEOUT).unwrap_err();
    assert_eq!(err.key(), "sums");
    assert!(err.message().contains("HTTP 404"));
}

// ---------------------------------------------------------------- download

fn payload() -> Vec<u8> {
    (0..3 * CHUNK + 1234).map(|i| (i * 7 % 251) as u8).collect()
}

type Fetch = (Result<Option<bool>, UpdateError>, Vec<(Phase, u64, u64)>);

fn fetch_file(
    url: &str,
    dest: &Path,
    sha256: Option<&str>,
    size: u64,
    cancel_after: Option<u64>,
) -> Fetch {
    let cancel = AtomicBool::new(false);
    let mut seen = Vec::new();
    let mut progress = |phase: Phase, received: u64, total: u64| {
        seen.push((phase, received, total));
        if cancel_after.is_some_and(|c| received >= c) {
            cancel.store(true, Ordering::Relaxed);
        }
    };
    let r = download(url, dest, size, sha256, UA, &mut progress, &cancel, TIMEOUT);
    (r, seen)
}

#[test]
fn download_verified() {
    let feed = LocalFeed::start();
    let data = payload();
    let digest = hex(&Sha256::digest(&data));
    let n = data.len() as u64;
    let url = feed.file("/file.exe", data.clone(), true);
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dl").join("SpaceKnife-0.4.0-Setup-x64.exe");
    let (r, seen) = fetch_file(&url, &dest, Some(&digest.to_uppercase()), n, None);
    assert_eq!(r.unwrap(), Some(true));
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert!(!part_path(&dest).exists());
    assert_eq!(seen[0], (Phase::Downloading, 0, n));
    assert_eq!(*seen.last().unwrap(), (Phase::Downloading, n, n));
    assert!(seen.len() <= 4, "{seen:?}");
    // already there with the right hash: kept, nothing fetched
    let (r, seen) = fetch_file(&url, &dest, Some(&digest), n, None);
    assert_eq!(r.unwrap(), Some(true));
    assert_eq!(feed.hits("/file.exe"), 1);
    assert_eq!(seen[0].0, Phase::Verifying);
}

#[test]
fn nothing_is_downloaded_without_a_published_checksum() {
    // review issue 10: a file the release lists no SHA-256 for could be anything; it is never
    // fetched (an installer would run it silently)
    let _g = crate::i18n::TEST_LOCK.lock();
    let feed = LocalFeed::start();
    let data = payload();
    let url = feed.file("/file.exe", data.clone(), true);
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("SpaceKnife-0.4.0-Setup-x64.exe");
    let (r, seen) = fetch_file(&url, &dest, None, data.len() as u64, None);
    let err = r.unwrap_err();
    assert_eq!(err, UpdateError::NoChecksum);
    assert_eq!(err.key(), "no_checksum");
    assert!(err.message().contains("SHA-256"), "{}", err.message());
    assert_eq!(feed.hits("/file.exe"), 0);
    assert!(seen.is_empty());
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    // a file left there earlier is not taken either
    std::fs::write(&dest, &data).unwrap();
    let (r, _) = fetch_file(&url, &dest, None, data.len() as u64, None);
    assert_eq!(r.unwrap_err(), UpdateError::NoChecksum);
}

/// A server that answers one GET with its headers and `sent` bytes of a body it says is longer,
/// then holds the connection open for `hold` without sending anything more.
fn stalling_server(length: usize, sent: usize, hold: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            std::thread::spawn(move || {
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n\r\n");
                let _ = conn.write_all(head.as_bytes());
                let _ = conn.write_all(&vec![b'['; sent]);
                let _ = conn.flush();
                std::thread::sleep(hold);
            });
        }
    });
    format!("http://127.0.0.1:{port}/file")
}

#[test]
fn a_download_that_stalls_fails_within_the_timeout() {
    // review issue 11: the body had no read timeout, a stalled connection hung the download (and
    // every later one) until the app restarted
    let _g = crate::i18n::TEST_LOCK.lock();
    let url = stalling_server(1 << 20, 1000, Duration::from_secs(20));
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("SpaceKnife-0.4.0-Setup-x64.exe");
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let r = download(
        &url,
        &dest,
        1 << 20,
        Some(&"0".repeat(64)),
        UA,
        &mut |_, _, _| {},
        &cancel,
        Duration::from_millis(500),
    );
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    match r {
        Err(UpdateError::DownloadNet { reason }) => assert_eq!(reason, "连接 GitHub 超时"),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0); // no .part left
}

#[test]
fn a_stalled_download_can_be_cancelled() {
    let url = stalling_server(1 << 20, 1000, Duration::from_secs(20));
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("SpaceKnife-0.4.0-Setup-x64.exe");
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        c.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();
    let r = download(
        &url,
        &dest,
        1 << 20,
        Some(&"0".repeat(64)),
        UA,
        &mut |_, _, _| {},
        &cancel,
        Duration::from_secs(2),
    );
    assert_eq!(r.unwrap_err(), UpdateError::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn a_release_list_that_stalls_times_out() {
    let _g = crate::i18n::TEST_LOCK.lock();
    let url = stalling_server(100_000, 10, Duration::from_secs(20));
    let started = Instant::now();
    let err = fetch_releases(&url, None, UA, Duration::from_millis(500)).unwrap_err();
    assert_eq!(err, UpdateError::Timeout);
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    // the checksum file the same way: no download starts
    let mut r = release("0.4.0", &[]);
    r.assets = vec![Asset {
        name: "SHA256SUMS.txt".into(),
        size: 0,
        url: stalling_server(100_000, 10, Duration::from_secs(20)),
    }];
    let asset = Asset {
        name: "SpaceKnife-0.4.0-Setup-x64.exe".into(),
        size: 0,
        url: "u".into(),
    };
    let started = Instant::now();
    let err = fetch_checksums(&r, &asset, UA, Duration::from_millis(500)).unwrap_err();
    assert_eq!(err.key(), "sums", "{err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn download_mismatch_is_deleted() {
    let feed = LocalFeed::start();
    let data = payload();
    let url = feed.file("/file.exe", data.clone(), true);
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("SpaceKnife-0.4.0-Setup-x64.exe");
    let (r, _) = fetch_file(&url, &dest, Some(&"0".repeat(64)), data.len() as u64, None);
    assert_eq!(r.unwrap_err(), UpdateError::Mismatch);
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn download_cancel_and_failures() {
    let feed = LocalFeed::start();
    let data = payload();
    let n = data.len() as u64;
    let digest = hex(&Sha256::digest(&data));
    let url = feed.file("/file.exe", data.clone(), true);
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("SpaceKnife-0.4.0-Setup-x64.exe");
    let (r, _) = fetch_file(&url, &dest, Some(&digest), n, Some(1));
    assert_eq!(r.unwrap_err(), UpdateError::Cancelled);
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    let (r, _) = fetch_file(&feed.url("/missing.exe"), &dest, Some(&digest), n, None);
    assert_eq!(r.unwrap_err(), UpdateError::DownloadHttp { code: 404 });
    let short = feed.file("/short.exe", data[..1000].to_vec(), false); // no length: ends when the server closes
    let (r, _) = fetch_file(&short, &dest, Some(&digest), n, None);
    assert_eq!(
        r.unwrap_err(),
        UpdateError::Incomplete {
            received: 1000,
            expected: n
        }
    );
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn environment() {
    // only the parts that do not race with other tests reading the environment
    assert_eq!(
        system(),
        if cfg!(windows) {
            "win32"
        } else if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        }
    );
    assert_eq!(arch("AMD64"), "x64");
    assert_eq!(arch("aarch64"), "arm64");
    assert!(updates_dir().ends_with("updates"));
    assert_eq!(install_kind_from(""), InstallKind::Dev);
    assert_eq!(install_kind_from(" NSIS "), InstallKind::Nsis);
    assert_eq!(install_kind_from("snap"), InstallKind::Unknown);
    assert_eq!(feed_url_from(""), RELEASES_URL);
    assert_eq!(
        feed_url_from("http://127.0.0.1:8123/releases"),
        "http://127.0.0.1:8123/releases"
    );
    assert_eq!(feed_url_from("https://example.com/releases"), RELEASES_URL); // never another host
}
