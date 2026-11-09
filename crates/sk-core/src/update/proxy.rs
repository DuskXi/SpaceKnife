//! The proxy of the update check and the download: what 0.3.4's urllib did (`getproxies`,
//! `proxy_bypass`), so that a copy started from the Start menu, Finder or the Dock — which has no
//! proxy variables — still gets through a proxy the system is set to.
//!
//! 1. The environment first, as ureq reads it (`ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY`, either
//!    case, with `NO_PROXY`): set, it decides alone.
//! 2. Else the system's settings ([`system_route`]):
//!    - Windows: the user's Internet settings as WinHTTP reads them
//!      (`WinHttpGetIEProxyConfigForCurrentUser`). 「自动检测设置」 (WPAD) and a setup script (PAC)
//!      are asked through `WinHttpGetProxyForUrl` for the very URL; when that finds nothing (no
//!      WPAD on the network, the script can't be fetched) the fixed proxy server applies, with its
//!      bypass list (`<local>`, `*.example.com`). Of a list the `https=` entry is taken, else one
//!      for every protocol; a SOCKS-only setting is not followed (ureq is built without SOCKS).
//!    - macOS: the network's proxies as `SCDynamicStoreCopyProxies` gives them, read through
//!      `scutil --proxy` (which prints that very dictionary): the secure web proxy (HTTPS) unless
//!      the host is excepted (`ExceptionsList` patterns, `ExcludeSimpleHostnames`). An automatic
//!      configuration (PAC) is not evaluated: like 0.3.4, the fixed proxy or none.
//!    - Linux and the rest: the environment only (0.3.4 too).
//!
//!    `NO_PROXY` of the environment still holds for a system proxy.
//!
//! A loopback feed (tests, demos) never goes through a proxy (mod.rs `agent`).

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Where a request goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// The environment's proxy settings decide (ureq's own reading of them)
    Env,
    /// Straight to the server
    Direct,
    /// Through this proxy (`http://host:port`)
    Via(String),
}

/// The proxy variables ureq reads (`ureq::Proxy::try_from_env`).
pub const ENV_VARS: [&str; 6] = [
    "ALL_PROXY",
    "all_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
];

/// Whether the environment (`get` reads a variable) names a proxy: then it decides alone.
pub fn env_has_proxy(get: &dyn Fn(&str) -> Option<String>) -> bool {
    ENV_VARS
        .iter()
        .any(|v| get(v).is_some_and(|value| !value.trim().is_empty()))
}

/// Whether `host` is on a `NO_PROXY` list (comma separated, as ureq reads it: `*` everything,
/// `.example.com` / `*.example.com` its subdomains, `example.*` / `example.` a prefix, else the
/// exact name; no case).
pub fn no_proxy_matches(list: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    list.split(',')
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .any(|e| {
            if e == "*" {
                true
            } else if let Some(suffix) = e.strip_prefix('*') {
                host.ends_with(suffix)
            } else if e.starts_with('.') {
                host.ends_with(&e)
            } else if let Some(prefix) = e.strip_suffix('*') {
                host.starts_with(prefix)
            } else if e.ends_with('.') {
                host.starts_with(&e)
            } else {
                host == e
            }
        })
}

/// `pattern` with `*` (any run) and `?` (one character) against `text`, without case.
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// The proxy for an `https` request out of a Windows proxy list (`ProxyServer`, or what a PAC
/// file answered): `host:port` for every protocol, or `http=…;https=…;socks=…` (also separated by
/// white space, maybe with a scheme `https://host:port`). The `https=` entry, else the first one
/// for every protocol; None when there is neither (a SOCKS-only or http-only setting).
pub fn pick_from_list(list: &str) -> Option<String> {
    let entries: Vec<&str> = list
        .split([';', ' ', '\t', '\r', '\n'])
        .map(str::trim)
        .filter(|e| !e.is_empty() && !e.eq_ignore_ascii_case("DIRECT"))
        .collect();
    let named = |scheme: &str| {
        entries.iter().find_map(|e| {
            let (k, v) = e.split_once('=')?;
            k.trim().eq_ignore_ascii_case(scheme).then(|| v.trim())
        })
    };
    let plain = || entries.iter().copied().find(|e| !e.contains('='));
    let server = named("https").or_else(plain)?;
    if server.is_empty() {
        return None;
    }
    Some(if server.contains("://") {
        server.to_owned()
    } else {
        format!("http://{server}")
    })
}

/// Whether Windows' bypass list (`ProxyOverride`: `;`-separated, `<local>` for names without a
/// dot, patterns with `*` and `?`, maybe with a scheme or a port) sends `host` straight.
pub fn ie_bypassed(bypass: &str, host: &str) -> bool {
    bypass
        .split([';', ' ', '\t', '\r', '\n'])
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .any(|entry| {
            if entry.eq_ignore_ascii_case("<local>") {
                return !host.contains('.');
            }
            let entry = entry.split_once("://").map_or(entry, |(_, rest)| rest);
            // a port after the name (not an IPv6 address's colons)
            let entry = match entry.rsplit_once(':') {
                Some((name, port))
                    if !name.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    name
                }
                _ => entry,
            };
            glob(entry, host)
        })
}

/// The user's Internet settings on Windows, as `WinHttpGetIEProxyConfigForCurrentUser` gives them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IeConfig {
    /// 「自动检测设置」 (WPAD)
    pub auto_detect: bool,
    /// 「使用设置脚本」: the PAC file's URL
    pub auto_config_url: Option<String>,
    /// 「使用代理服务器」: the list (None when it is off)
    pub proxy: Option<String>,
    pub bypass: Option<String>,
}

/// What a PAC file (found by WPAD or named in the settings) answers for a URL: `Ok(Some(list))`
/// a proxy list, `Ok(None)` straight, `Err` when none could be fetched or run.
pub type PacAnswer = Result<Option<String>, String>;

/// The route on Windows for a request to `host` (`pac` asks WinHTTP for the URL): the automatic
/// settings first, as the system's own clients take them; when they give nothing, the fixed proxy
/// and its bypass list.
pub fn windows_route(cfg: &IeConfig, host: &str, pac: &dyn Fn(&IeConfig) -> PacAnswer) -> Route {
    if cfg.auto_detect
        || cfg
            .auto_config_url
            .as_deref()
            .is_some_and(|u| !u.is_empty())
    {
        match pac(cfg) {
            Ok(Some(list)) => {
                if let Some(proxy) = pick_from_list(&list) {
                    return Route::Via(proxy);
                }
                return Route::Direct;
            }
            Ok(None) => return Route::Direct,
            Err(e) => tracing::debug!("no proxy from the automatic settings: {e}"),
        }
    }
    match cfg.proxy.as_deref() {
        Some(list) if !cfg.bypass.as_deref().is_some_and(|b| ie_bypassed(b, host)) => {
            pick_from_list(list).map_or(Route::Direct, Route::Via)
        }
        _ => Route::Direct,
    }
}

/// macOS's proxy settings, as `scutil --proxy` prints `SCDynamicStoreCopyProxies`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MacProxies {
    /// The secure web proxy (HTTPS) when it is on: host, port (0: none given)
    pub https: Option<(String, u16)>,
    pub exceptions: Vec<String>,
    pub exclude_simple: bool,
    /// An automatic configuration (PAC) is on: not evaluated
    pub pac: bool,
}

/// Read `scutil --proxy`'s output (`<dictionary> { Key : value … ExceptionsList : <array> { 0 :
/// *.local … } }`).
pub fn parse_scutil(text: &str) -> MacProxies {
    let mut keys: Vec<(String, String)> = Vec::new();
    let mut exceptions = Vec::new();
    let mut in_exceptions = false;
    for line in text.lines() {
        let line = line.trim();
        if in_exceptions {
            if line.starts_with('}') {
                in_exceptions = false;
            } else if let Some((_, v)) = line.split_once(" : ") {
                exceptions.push(v.trim().to_owned());
            }
            continue;
        }
        let Some((k, v)) = line.split_once(" : ") else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if k == "ExceptionsList" && v.starts_with("<array>") {
            in_exceptions = true;
        } else {
            keys.push((k.to_owned(), v.to_owned()));
        }
    }
    let get = |k: &str| keys.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let on = |k: &str| get(k).is_some_and(|v| v == "1");
    let https = (on("HTTPSEnable"))
        .then(|| get("HTTPSProxy"))
        .flatten()
        .filter(|h| !h.is_empty())
        .map(|h| {
            let port = get("HTTPSPort").and_then(|p| p.parse().ok()).unwrap_or(0);
            (h.to_owned(), port)
        });
    MacProxies {
        https,
        exceptions,
        exclude_simple: on("ExcludeSimpleHostnames"),
        pac: on("ProxyAutoConfigEnable"),
    }
}

/// The route on macOS for a request to `host` (0.3.4's `_scproxy` and `proxy_bypass_macosx_sysconf`).
pub fn mac_route(p: &MacProxies, host: &str) -> Route {
    let Some((server, port)) = &p.https else {
        return Route::Direct;
    };
    let excepted =
        (p.exclude_simple && !host.contains('.')) || p.exceptions.iter().any(|e| glob(e, host));
    if excepted {
        return Route::Direct;
    }
    Route::Via(if *port > 0 {
        format!("http://{server}:{port}")
    } else {
        format!("http://{server}")
    })
}

/// The host of `url` (lower-cased, no brackets), or "".
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let netloc = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let hostport = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let host = if let Some(stripped) = hostport.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    host.to_ascii_lowercase()
}

/// How long a system route is kept (a check, then the checksums and the file, ask within seconds).
const KEEP: Duration = Duration::from_secs(60);
static CACHE: Mutex<Option<(Instant, String, Route)>> = Mutex::new(None);

/// The route for `url`: [`Route::Env`] when the environment names a proxy, else the system's
/// (kept for a minute per host), `NO_PROXY` applied.
pub fn route(url: &str) -> Route {
    let env = |k: &str| std::env::var(k).ok();
    if env_has_proxy(&env) {
        return Route::Env;
    }
    let host = host_of(url);
    if ["NO_PROXY", "no_proxy"]
        .iter()
        .any(|v| env(v).is_some_and(|list| no_proxy_matches(&list, &host)))
    {
        return Route::Direct;
    }
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, h, r)) = cache.as_ref()
        && *h == host
        && at.elapsed() < KEEP
    {
        return r.clone();
    }
    let found = system_route(url, &host);
    if let Route::Via(p) = &found {
        tracing::info!("update requests to {host} go through the system's proxy {p}");
    }
    *cache = Some((Instant::now(), host, found.clone()));
    found
}

/// The system's proxy for `url` (its `host`): see the module docs.
#[cfg(windows)]
pub fn system_route(url: &str, host: &str) -> Route {
    match win::ie_config() {
        Some(cfg) => windows_route(&cfg, host, &|cfg| win::pac(cfg, url)),
        None => Route::Direct,
    }
}

#[cfg(target_os = "macos")]
pub fn system_route(_url: &str, host: &str) -> Route {
    let out = std::process::Command::new("/usr/sbin/scutil")
        .arg("--proxy")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(out) if out.status.success() => {
            let p = parse_scutil(&String::from_utf8_lossy(&out.stdout));
            if p.pac {
                tracing::debug!("the system's automatic proxy configuration is not evaluated");
            }
            mac_route(&p, host)
        }
        _ => Route::Direct,
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn system_route(_url: &str, _host: &str) -> Route {
    Route::Direct
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{GetLastError, GlobalFree};
    use windows_sys::Win32::Networking::WinHttp::{
        WINHTTP_ACCESS_TYPE_NAMED_PROXY, WINHTTP_ACCESS_TYPE_NO_PROXY,
        WINHTTP_AUTO_DETECT_TYPE_DHCP, WINHTTP_AUTO_DETECT_TYPE_DNS_A,
        WINHTTP_AUTOPROXY_AUTO_DETECT, WINHTTP_AUTOPROXY_CONFIG_URL, WINHTTP_AUTOPROXY_OPTIONS,
        WINHTTP_CURRENT_USER_IE_PROXY_CONFIG, WINHTTP_PROXY_INFO, WinHttpCloseHandle,
        WinHttpGetIEProxyConfigForCurrentUser, WinHttpGetProxyForUrl, WinHttpOpen,
        WinHttpSetTimeouts,
    };

    use super::{IeConfig, PacAnswer};

    /// A string WinHTTP allocated, read and freed (null: None).
    ///
    /// # Safety
    /// `p` is null or a NUL-terminated wide string from GlobalAlloc that nothing else frees.
    unsafe fn take(p: *mut u16) -> Option<String> {
        if p.is_null() {
            return None;
        }
        // SAFETY: as the caller promises: NUL-terminated, ours to free.
        unsafe {
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            GlobalFree(p.cast());
            Some(s)
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    pub fn ie_config() -> Option<IeConfig> {
        let mut raw = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
        // SAFETY: the out-struct is ours; WinHTTP fills its strings with GlobalAlloc, freed by take.
        unsafe {
            if WinHttpGetIEProxyConfigForCurrentUser(&mut raw) == 0 {
                return None;
            }
            Some(IeConfig {
                auto_detect: raw.fAutoDetect != 0,
                auto_config_url: take(raw.lpszAutoConfigUrl),
                proxy: take(raw.lpszProxy),
                bypass: take(raw.lpszProxyBypass),
            })
        }
    }

    /// Ask WinHTTP what the automatic settings say for `url` (WPAD, the PAC file): at most a few
    /// seconds for the script's download.
    pub fn pac(cfg: &IeConfig, url: &str) -> PacAnswer {
        let agent = wide("SpaceKnife");
        let script = cfg.auto_config_url.as_deref().map(wide);
        let target = wide(url);
        // SAFETY: plain WinHTTP calls on a session handle we open and close; every string lives
        // through the calls; the answer's strings are freed by take.
        unsafe {
            let session = WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_NO_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                0,
            );
            if session.is_null() {
                return Err(format!("WinHttpOpen: error {}", GetLastError()));
            }
            WinHttpSetTimeouts(session, 5000, 5000, 5000, 5000);
            let mut opts = WINHTTP_AUTOPROXY_OPTIONS {
                fAutoLogonIfChallenged: 1,
                ..Default::default()
            };
            if cfg.auto_detect {
                opts.dwFlags |= WINHTTP_AUTOPROXY_AUTO_DETECT;
                opts.dwAutoDetectFlags =
                    WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A;
            }
            if let Some(script) = &script {
                opts.dwFlags |= WINHTTP_AUTOPROXY_CONFIG_URL;
                opts.lpszAutoConfigUrl = script.as_ptr();
            }
            let mut info = WINHTTP_PROXY_INFO::default();
            let ok = WinHttpGetProxyForUrl(session, target.as_ptr(), &mut opts, &mut info) != 0;
            let error = GetLastError();
            WinHttpCloseHandle(session);
            if !ok {
                return Err(format!("WinHttpGetProxyForUrl: error {error}"));
            }
            let proxy = take(info.lpszProxy);
            let _ = take(info.lpszProxyBypass);
            if info.dwAccessType == WINHTTP_ACCESS_TYPE_NAMED_PROXY {
                Ok(proxy)
            } else {
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_decides_alone_when_it_names_a_proxy() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert!(!env_has_proxy(&env(&[])));
        assert!(!env_has_proxy(&env(&[
            ("NO_PROXY", "*"),
            ("HTTPS_PROXY", " ")
        ])));
        for var in ENV_VARS {
            let vars: &'static [(&'static str, &'static str)] = match var {
                "ALL_PROXY" => &[("ALL_PROXY", "http://p:1")],
                "all_proxy" => &[("all_proxy", "http://p:1")],
                "HTTPS_PROXY" => &[("HTTPS_PROXY", "http://p:1")],
                "https_proxy" => &[("https_proxy", "http://p:1")],
                "HTTP_PROXY" => &[("HTTP_PROXY", "http://p:1")],
                _ => &[("http_proxy", "http://p:1")],
            };
            assert!(env_has_proxy(&env(vars)), "{var}");
        }
    }

    #[test]
    fn no_proxy_as_ureq_reads_it() {
        let list = "localhost, .internal.example, *.corp, intra*, github.com";
        assert!(no_proxy_matches(list, "localhost"));
        assert!(no_proxy_matches(list, "a.internal.example"));
        assert!(!no_proxy_matches(list, "internal.example"));
        assert!(no_proxy_matches(list, "x.y.corp"));
        assert!(no_proxy_matches(list, "intranet"));
        assert!(no_proxy_matches(list, "GitHub.com"));
        assert!(!no_proxy_matches(list, "api.github.com"));
        assert!(no_proxy_matches("*", "api.github.com"));
        assert!(!no_proxy_matches("", "api.github.com"));
    }

    #[test]
    fn a_windows_proxy_list() {
        assert_eq!(
            pick_from_list("proxy.corp:8080").as_deref(),
            Some("http://proxy.corp:8080")
        );
        assert_eq!(
            pick_from_list("http=web.corp:80;https=secure.corp:8443;ftp=f:21").as_deref(),
            Some("http://secure.corp:8443")
        );
        assert_eq!(
            pick_from_list("HTTPS=secure.corp:8443 http=web.corp:80").as_deref(),
            Some("http://secure.corp:8443")
        );
        // a list for every protocol after another one's entry
        assert_eq!(
            pick_from_list("ftp=f:21; all.corp:3128").as_deref(),
            Some("http://all.corp:3128")
        );
        // a proxy that is itself spoken to over TLS keeps its scheme
        assert_eq!(
            pick_from_list("https=https://secure.corp:443").as_deref(),
            Some("https://secure.corp:443")
        );
        // what a PAC file answered (WinHTTP: the first is tried first)
        assert_eq!(
            pick_from_list("pac1.corp:8080; pac2.corp:8080").as_deref(),
            Some("http://pac1.corp:8080")
        );
        // http only, SOCKS only: not for an https request
        assert_eq!(pick_from_list("http=web.corp:80"), None);
        assert_eq!(pick_from_list("socks=s.corp:1080"), None);
        assert_eq!(pick_from_list(""), None);
    }

    #[test]
    fn windows_bypass_list() {
        let b = "<local>;*.corp.example;10.*;https://build.example:443;github.co?";
        assert!(ie_bypassed(b, "intranet"));
        assert!(!ie_bypassed(b, "api.github.com"));
        assert!(ie_bypassed(b, "files.corp.example"));
        assert!(ie_bypassed(b, "FILES.Corp.Example"));
        assert!(ie_bypassed(b, "10.1.2.3"));
        assert!(ie_bypassed(b, "build.example"));
        assert!(ie_bypassed(b, "github.com"));
        assert!(!ie_bypassed(b, "github.co"));
        assert!(!ie_bypassed("", "intranet"));
    }

    #[test]
    fn windows_automatic_settings_come_first() {
        let fixed = IeConfig {
            proxy: Some("http=web:80;https=fixed.corp:8080".into()),
            bypass: Some("<local>;*.github.com".into()),
            ..Default::default()
        };
        let never = |_: &IeConfig| -> PacAnswer { panic!("no automatic settings") };
        assert_eq!(
            windows_route(&fixed, "github.com", &never),
            Route::Via("http://fixed.corp:8080".into())
        );
        assert_eq!(
            windows_route(&fixed, "api.github.com", &never),
            Route::Direct
        );
        assert_eq!(
            windows_route(&IeConfig::default(), "github.com", &never),
            Route::Direct
        );
        // 「自动检测设置」 with a PAC file found: its answer, the fixed proxy aside
        let auto = IeConfig {
            auto_detect: true,
            ..fixed.clone()
        };
        let found = |_: &IeConfig| -> PacAnswer { Ok(Some("pac.corp:3128".into())) };
        assert_eq!(
            windows_route(&auto, "api.github.com", &found),
            Route::Via("http://pac.corp:3128".into())
        );
        let direct = |_: &IeConfig| -> PacAnswer { Ok(None) };
        assert_eq!(windows_route(&auto, "github.com", &direct), Route::Direct);
        // nothing found (no WPAD here): the fixed proxy
        let none = |_: &IeConfig| -> PacAnswer { Err("error 12180".into()) };
        assert_eq!(
            windows_route(&auto, "github.com", &none),
            Route::Via("http://fixed.corp:8080".into())
        );
        // a setup script's URL is asked too
        let script = IeConfig {
            auto_config_url: Some("http://wpad.corp/proxy.pac".into()),
            ..Default::default()
        };
        let asked = std::cell::Cell::new(false);
        let check = |cfg: &IeConfig| -> PacAnswer {
            asked.set(cfg.auto_config_url.as_deref() == Some("http://wpad.corp/proxy.pac"));
            Ok(Some("script.corp:8080; DIRECT".into()))
        };
        assert_eq!(
            windows_route(&script, "github.com", &check),
            Route::Via("http://script.corp:8080".into())
        );
        assert!(asked.get());
    }

    #[test]
    fn macos_proxies_as_scutil_prints_them() {
        let text = "<dictionary> {
  ExceptionsList : <array> {
    0 : *.local
    1 : 169.254/16
    2 : *.corp.example
  }
  ExcludeSimpleHostnames : 1
  FTPPassive : 1
  HTTPEnable : 1
  HTTPPort : 3128
  HTTPProxy : web.corp.example
  HTTPSEnable : 1
  HTTPSPort : 8443
  HTTPSProxy : secure.corp.example
  ProxyAutoConfigEnable : 0
}
";
        let p = parse_scutil(text);
        assert_eq!(
            p,
            MacProxies {
                https: Some(("secure.corp.example".into(), 8443)),
                exceptions: vec![
                    "*.local".into(),
                    "169.254/16".into(),
                    "*.corp.example".into()
                ],
                exclude_simple: true,
                pac: false,
            }
        );
        assert_eq!(
            mac_route(&p, "api.github.com"),
            Route::Via("http://secure.corp.example:8443".into())
        );
        assert_eq!(mac_route(&p, "intranet"), Route::Direct);
        assert_eq!(mac_route(&p, "mirror.corp.example"), Route::Direct);
        // the web proxy (HTTP) alone does not carry https, as the system's own clients do it
        let http_only = parse_scutil(
            "<dictionary> {\n  HTTPEnable : 1\n  HTTPProxy : web\n  HTTPSEnable : 0\n  HTTPSProxy : secure\n}\n",
        );
        assert_eq!(http_only.https, None);
        assert_eq!(mac_route(&http_only, "github.com"), Route::Direct);
        // no settings at all; a PAC file (not evaluated)
        assert_eq!(parse_scutil("<dictionary> {\n}\n"), MacProxies::default());
        let pac = parse_scutil(
            "<dictionary> {\n  ProxyAutoConfigEnable : 1\n  ProxyAutoConfigURLString : http://wpad/proxy.pac\n}\n",
        );
        assert!(pac.pac && pac.https.is_none());
        // a port not given
        let bare = parse_scutil("<dictionary> {\n  HTTPSEnable : 1\n  HTTPSProxy : secure\n}\n");
        assert_eq!(
            mac_route(&bare, "github.com"),
            Route::Via("http://secure".into())
        );
    }

    #[test]
    fn globs() {
        assert!(glob("*.local", "printer.local"));
        assert!(!glob("*.local", "local"));
        assert!(glob("10.*.*.1", "10.0.0.1"));
        assert!(glob("a?c", "ABC"));
        assert!(glob("*", ""));
        assert!(!glob("a", ""));
        assert!(glob("*a*b*", "xxaxxbxx"));
    }

    #[test]
    fn hosts_of_urls() {
        assert_eq!(host_of("https://api.github.com/repos/x"), "api.github.com");
        assert_eq!(host_of("https://user@GitHub.com:443/x"), "github.com");
        assert_eq!(host_of("http://[::1]:8080/r"), "::1");
    }

    #[test]
    fn the_system_route_is_read_without_failing() {
        // whatever this machine is set to: an answer, quickly enough for an update check
        let started = Instant::now();
        let r = system_route("https://api.github.com/", "api.github.com");
        assert!(started.elapsed() < Duration::from_secs(30), "{r:?}");
    }
}
