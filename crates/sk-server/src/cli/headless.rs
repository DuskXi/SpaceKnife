//! Headless mode (`headless`, `--headless`, `--listen <address>`): no window. The server serves
//! the built interface over HTTP(S) for a browser, on this computer or another one, next to the
//! WebSocket on the same address (site docs: headless.md). Port of Electron's `headless.ts` (the
//! command line, how exposed an address is, the terminal's texts, the addresses to print) and
//! `headless-main.ts` (ask before opening SpaceKnife to the network, start, print where to open
//! it, stop on Ctrl+C).

use std::io::{IsTerminal as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs as _};
use std::path::{Path, PathBuf};

use clap::Args;

use crate::ServeOptions;

/// Port when `--listen` names none: "S" (83) and "K" (75) in ASCII.
pub const DEFAULT_PORT: u16 = 8375;
pub const LOOPBACK: &str = "127.0.0.1";

#[derive(Debug, Args)]
pub struct HeadlessArgs {
    /// Listen on this address: host, host:port, :port, [IPv6]:port; 0.0.0.0 is every network interface.
    #[arg(long)]
    pub listen: Option<String>,
    /// Don't ask before opening SpaceKnife to the network (needed without a terminal).
    #[arg(long)]
    pub yes: bool,
    /// HTTPS certificate (PEM), together with --tls-key.
    #[arg(long)]
    pub tls_cert: Option<PathBuf>,
    /// The certificate's private key (PEM).
    #[arg(long)]
    pub tls_key: Option<PathBuf>,
    /// The built interface to serve (default: `ui` next to this executable).
    #[arg(long)]
    pub ui: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listen {
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliError {
    BadAddress,
    BadPort,
}

/// loopback: only this computer; all: every interface (0.0.0.0, ::); network: one address others reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exposure {
    Loopback,
    All,
    Network,
}

fn is_v4(s: &str) -> bool {
    s.parse::<Ipv4Addr>().is_ok()
}
fn is_v6(s: &str) -> bool {
    s.parse::<Ipv6Addr>().is_ok()
}

fn is_hostname(h: &str) -> bool {
    let h = h.strip_suffix('.').unwrap_or(h);
    if h.is_empty() || h.len() > 253 {
        return false;
    }
    h.split('.').all(|label| {
        let b = label.as_bytes();
        !b.is_empty()
            && b.len() <= 63
            && b[0].is_ascii_alphanumeric()
            && b[b.len() - 1].is_ascii_alphanumeric()
            && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
    })
}

/// `--listen`'s value: `HOST`, `HOST:PORT`, `:PORT` or `PORT` (this computer), `[IPv6]`,
/// `[IPv6]:PORT`, or a bare IPv6 address (`::`). Port 0 lets the system pick a free one.
pub fn parse_listen(text: &str) -> Result<Listen, CliError> {
    let v = text.trim();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (host, port): (String, Option<String>);
    if digits(v) {
        host = LOOPBACK.into();
        port = Some(v.into());
    } else if let Some(rest) = v.strip_prefix('[') {
        let Some(close) = rest.find(']') else {
            return Err(CliError::BadAddress);
        };
        let inner = &rest[..close];
        let after = &rest[close + 1..];
        if !is_v6(inner) {
            return Err(CliError::BadAddress);
        }
        host = inner.into();
        port = match after.strip_prefix(':') {
            None if after.is_empty() => None,
            None => return Err(CliError::BadAddress),
            Some("") => return Err(CliError::BadPort),
            Some(p) if digits(p) => Some(p.into()),
            Some(_) => return Err(CliError::BadAddress),
        };
    } else if v.matches(':').count() > 1 {
        if !is_v6(v) {
            return Err(CliError::BadAddress);
        }
        host = v.into();
        port = None;
    } else if let Some(at) = v.find(':') {
        let h = &v[..at];
        host = if h.is_empty() {
            LOOPBACK.into()
        } else {
            h.into()
        };
        let p = &v[at + 1..];
        if !digits(p) {
            return Err(CliError::BadPort);
        }
        port = Some(p.into());
    } else {
        host = v.into();
        port = None;
    }
    if !is_v4(&host) && !is_v6(&host) && !is_hostname(&host) {
        return Err(CliError::BadAddress);
    }
    let port = match port {
        None => DEFAULT_PORT,
        Some(p) => p.parse::<u16>().map_err(|_| CliError::BadPort)?,
    };
    Ok(Listen {
        host: host.strip_suffix('.').unwrap_or(&host).to_owned(),
        port,
    })
}

/// How far an address reaches. A host name other than localhost counts as the network: what it
/// resolves to isn't known here.
pub fn exposure(host: &str) -> Exposure {
    let h = host.to_lowercase();
    let h = h.trim_start_matches('[').trim_end_matches(']');
    if h == "localhost" || h == "::1" || (is_v4(h) && h.starts_with("127.")) {
        return Exposure::Loopback;
    }
    if let Some(v4) = h.strip_prefix("::ffff:")
        && is_v4(v4)
        && v4.starts_with("127.")
    {
        return Exposure::Loopback;
    }
    if h == "0.0.0.0" || (is_v6(h) && h.chars().all(|c| c == '0' || c == ':')) {
        return Exposure::All;
    }
    Exposure::Network
}

/// A host as it appears in a URL (IPv6 in brackets).
pub fn url_host(host: &str) -> String {
    if is_v6(host) {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

/// An address in the terminal's texts: host:port, IPv6 bracketed.
pub fn show_listen(l: &Listen) -> String {
    format!("{}:{}", url_host(&l.host), l.port)
}

/// A network address of this computer.
#[derive(Clone, Debug)]
pub struct NetworkAddress {
    pub address: IpAddr,
    pub internal: bool,
}

fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The addresses to open in a browser: the one listened on, or for 0.0.0.0 / :: each address of
/// this computer that others can reach (IPv4; IPv6 too for ::, without link-local ones), then this
/// computer's own. The token rides in the fragment, which browsers don't send to the server.
pub fn browser_urls(
    listen: &Listen,
    port: u16,
    token: &str,
    secure: bool,
    interfaces: &[NetworkAddress],
) -> Vec<String> {
    let scheme = if secure { "https" } else { "http" };
    let url = |host: &str| {
        format!(
            "{scheme}://{}:{port}/#token={}",
            url_host(host),
            encode_uri_component(token)
        )
    };
    if exposure(&listen.host) != Exposure::All {
        return vec![url(if listen.host == LOOPBACK {
            "localhost"
        } else {
            &listen.host
        })];
    }
    let v6 = listen.host != "0.0.0.0";
    let mut hosts: Vec<String> = Vec::new();
    for a in interfaces {
        if a.internal {
            continue;
        }
        let text = a.address.to_string();
        let ok = match a.address {
            IpAddr::V4(_) => true,
            IpAddr::V6(_) => {
                v6 && !text.to_lowercase().starts_with("fe8")
                    && !text.to_lowercase().starts_with("fe9")
                    && !text.to_lowercase().starts_with("fea")
                    && !text.to_lowercase().starts_with("feb")
            }
        };
        if ok && !hosts.contains(&text) {
            hosts.push(text);
        }
    }
    let mut out: Vec<String> = hosts.iter().map(|h| url(h)).collect();
    out.push(url("localhost"));
    out
}

/// Y / yes continues (full-width letters too, as an input method may type them); anything else,
/// an empty line or no answer at all stops.
pub fn confirmed(answer: Option<&str>) -> bool {
    let Some(a) = answer else { return false };
    // NFKC for the full-width forms: U+FF01..U+FF5E are ASCII + 0xFEE0
    let folded: String = a
        .chars()
        .map(|c| match c as u32 {
            0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            0x3000 => ' ',
            _ => c,
        })
        .collect();
    matches!(folded.trim().to_lowercase().as_str(), "y" | "yes")
}

// ------------------------------------------------------------------ the terminal's texts

/// The terminal's language: zh-CN for a Chinese system language, English otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

/// A terminal text in the language, `{name}` placeholders filled in.
pub fn ht(lang: Lang, key: &str, fields: &[(&str, String)]) -> String {
    let zh = lang == Lang::Zh;
    let text: String = match key {
        "usage" => if zh {
            "用法：spaceknife-cli headless [--listen <地址>] [--yes] [--tls-cert <文件> --tls-key <文件>] [--ui <文件夹>]\n\n  --listen <地址>     监听这个地址：主机、主机:端口、:端口、[IPv6]:端口；0.0.0.0 是所有网络接口（默认只监听本机 127.0.0.1:{port}）\n  --yes               不再询问，直接在网络上开放（没有终端时必须加）\n  --tls-cert <文件>   HTTPS 证书（PEM），与 --tls-key 一起用\n  --tls-key <文件>    证书的私钥（PEM）\n  --ui <文件夹>       构建好的界面（默认：程序旁边的 ui 文件夹）\n\n文档：https://spaceknife.kaltsit.dev/zh/docs/headless/"
        } else {
            "Usage: spaceknife-cli headless [--listen <address>] [--yes] [--tls-cert <file> --tls-key <file>] [--ui <folder>]\n\n  --listen <address>  Listen on this address: host, host:port, :port, [IPv6]:port; 0.0.0.0 is every network interface (default: this computer only, 127.0.0.1:{port})\n  --yes               Don't ask before opening SpaceKnife to the network (needed without a terminal)\n  --tls-cert <file>   HTTPS certificate (PEM), together with --tls-key\n  --tls-key <file>    The certificate's private key (PEM)\n  --ui <folder>       The built interface (default: the ui folder next to the program)\n\nDocumentation: https://spaceknife.kaltsit.dev/docs/headless/"
        }.into(),
        "badAddress" => if zh { "无法识别的地址：{value}（例如 0.0.0.0、192.168.1.20:{port}、[::]:{port}）" } else { "Not an address: {value} (for example 0.0.0.0, 192.168.1.20:{port}, [::]:{port})" }.into(),
        "badPort" => if zh { "端口要是 0 到 65535 的整数：{value}" } else { "The port must be a whole number from 0 to 65535: {value}" }.into(),
        "tlsPair" => if zh { "--tls-cert 和 --tls-key 要一起用（缺少 {value} 的另一半）" } else { "--tls-cert and --tls-key go together ({value} is missing its pair)" }.into(),
        "seeHelp" => if zh { "运行 spaceknife-cli headless --help 查看所有选项。" } else { "Run spaceknife-cli headless --help for the options." }.into(),
        "warnTitle" => if zh { "危险：SpaceKnife 将在网络上开放" } else { "DANGER: SpaceKnife will be open to the network" }.into(),
        "warnAll" => if zh {
            "监听 {address}：这台电脑的所有网络接口，局域网、Wi-Fi、VPN 上的任何设备都能连接，如果这台电脑有公网地址，互联网上的也能。"
        } else {
            "Listening on {address}: every network interface of this computer. Any device on the LAN, Wi-Fi or VPN can connect, and anyone on the internet if this computer has a public address."
        }.into(),
        "warnOne" => if zh { "监听 {address}：能访问这个地址的设备都能连接。" } else { "Listening on {address}: any device that reaches this address can connect." }.into(),
        "warnPower" => if zh {
            "拿到带令牌链接的人，可以用 {user} 的权限浏览这台电脑上的文件、移到回收站，或永久删除。"
        } else {
            "Whoever has the link with its token can browse the files on this computer with {user}'s rights, move them to the Recycle Bin, or delete them permanently."
        }.into(),
        "warnToken" => if zh { "令牌是唯一的保护：只发给你信任的人，不要在公共网络上开放。" } else { "The token is the only protection: give the link only to people you trust, and don't do this on a public network." }.into(),
        "warnPlain" => if zh {
            "这是明文 HTTP：同一网络上的人可以看到令牌、路径和文件名。要加密请用 --tls-cert / --tls-key，或改用 SSH 隧道（不加 --listen）。"
        } else {
            "This is plain HTTP: people on the same network can read the token, the paths and the file names. Use --tls-cert / --tls-key to encrypt, or an SSH tunnel (without --listen)."
        }.into(),
        "warnTls" => if zh { "流量经 HTTPS 加密。" } else { "Traffic is encrypted with HTTPS." }.into(),
        "question" => if zh { "确定要开放吗？输入 Y 继续，N 退出 [y/N]：" } else { "Open it to the network? Type Y to go on, N to quit [y/N]: " }.into(),
        "yesGiven" => if zh { "已加 --yes：不再询问。" } else { "--yes given: not asking." }.into(),
        "declined" => if zh { "已取消，没有监听任何地址。" } else { "Cancelled: nothing is listening." }.into(),
        "noTerminal" => if zh {
            "没有终端可以回答：要在网络上开放，请加上 --yes（表示你了解上面的风险）。"
        } else {
            "There is no terminal to answer in: to open SpaceKnife to the network, add --yes (you accept the risk above)."
        }.into(),
        "tlsRead" => if zh { "无法读取证书或私钥：{error}" } else { "Can't read the certificate or the key: {error}" }.into(),
        "noUi" => if zh { "找不到构建好的界面：{path}（用 --ui 指定文件夹）" } else { "The built interface isn't there: {path} (name the folder with --ui)" }.into(),
        "starting" => if zh { "正在启动后端…" } else { "Starting the backend…" }.into(),
        "backendFailed" => if zh { "后端没有启动：{error}" } else { "The backend didn't start: {error}" }.into(),
        "inUse" => if zh { "端口 {port} 已被占用：换一个（--listen {host}:端口），或写 :0 让系统挑一个空闲端口。" } else { "Port {port} is in use: pick another (--listen {host}:port), or :0 for any free port." }.into(),
        "notHere" => if zh { "{host} 不是这台电脑的地址。" } else { "{host} isn't an address of this computer." }.into(),
        "noAccess" => if zh { "没有权限监听端口 {port}（1024 以下的端口通常需要管理员权限）。" } else { "No permission to listen on port {port} (ports below 1024 usually need administrator rights)." }.into(),
        "listenFailed" => if zh { "无法监听 {address}：{error}" } else { "Can't listen on {address}: {error}" }.into(),
        "ready" => if zh { "SpaceKnife 无头模式 · {version}" } else { "SpaceKnife headless mode · {version}" }.into(),
        "openIn" => if zh { "在浏览器里打开：" } else { "Open in a browser:" }.into(),
        "onlyHere" => if zh {
            "只有这台电脑能访问。要从别的设备访问，用 --listen（或 SSH 隧道转发端口 {port}）。"
        } else {
            "Only this computer can connect. To use it from another device, use --listen (or forward port {port} through an SSH tunnel)."
        }.into(),
        "plainNote" => if zh { "明文 HTTP：令牌和文件名在网络上可见。" } else { "Plain HTTP: the token and the file names can be read on the network." }.into(),
        "stopHint" => if zh { "按 Ctrl+C 停止。" } else { "Press Ctrl+C to stop." }.into(),
        "stopping" => if zh { "正在停止…" } else { "Stopping…" }.into(),
        "stopped" => if zh { "已停止。" } else { "Stopped." }.into(),
        other => other.into(),
    };
    let mut out = text;
    for (k, v) in fields {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// The warning before listening on the network, line by line (without colours).
pub fn warning_lines(lang: Lang, listen: &Listen, user: &str, secure: bool) -> Vec<String> {
    let address = show_listen(listen);
    vec![
        ht(lang, "warnTitle", &[]),
        ht(
            lang,
            if exposure(&listen.host) == Exposure::All {
                "warnAll"
            } else {
                "warnOne"
            },
            &[("address", address)],
        ),
        ht(lang, "warnPower", &[("user", user.to_owned())]),
        ht(lang, "warnToken", &[]),
        ht(lang, if secure { "warnTls" } else { "warnPlain" }, &[]),
    ]
}

/// Why listening failed, from the error's kind.
pub fn listen_error(lang: Lang, listen: &Listen, err: &std::io::Error) -> String {
    let fields = [
        ("port", listen.port.to_string()),
        ("host", url_host(&listen.host)),
        ("address", show_listen(listen)),
        ("error", err.to_string()),
    ];
    let key = match err.kind() {
        std::io::ErrorKind::AddrInUse => "inUse",
        std::io::ErrorKind::AddrNotAvailable => "notHere",
        std::io::ErrorKind::PermissionDenied => "noAccess",
        _ => "listenFailed",
    };
    ht(lang, key, &fields)
}

fn user_name() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "?".into())
}

fn paint(tty: bool, code: &str, text: &str) -> String {
    if tty {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

/// The built interface the app bundle ships (`tauri.bundle.json` resource `ui/`), found from the executable.
pub fn default_ui_dir() -> PathBuf {
    let exe = std::env::current_exe().ok();
    // an installed /usr/bin entry may be a link: look from where the program really is
    let exe = exe.map(|e| std::fs::canonicalize(&e).unwrap_or(e));
    let dir = exe
        .as_deref()
        .and_then(|e| e.parent())
        .map(PathBuf::from)
        .unwrap_or_default();
    ui_candidates(&dir)
        .into_iter()
        .find(|c| c.join("index.html").is_file())
        .unwrap_or_else(|| dir.join("ui"))
}

/// Where Tauri's bundles put the `ui/` resource, relative to the folder of `spaceknife-cli`:
/// - portable copies and the Windows installer: next to the program (`ui/`, older layouts `resources/ui/`);
/// - deb / rpm (`/usr/bin` → `/usr/lib/SpaceKnife`) and the AppImage (`usr/bin` → `usr/lib/SpaceKnife`);
/// - macOS (`SpaceKnife.app/Contents/MacOS` → `Contents/Resources`).
pub fn ui_candidates(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![dir.join("ui"), dir.join("resources").join("ui")];
    if let Some(up) = dir.parent() {
        out.push(up.join("lib").join("SpaceKnife").join("ui"));
        out.push(up.join("lib").join("spaceknife").join("ui"));
        out.push(up.join("Resources").join("ui"));
    }
    out
}

/// `spaceknife-cli headless`: returns the exit code.
pub fn headless(args: HeadlessArgs) -> i32 {
    super::apply_language();
    let lang = if sk_core::i18n::language() == sk_core::i18n::Language::ZhCn {
        Lang::Zh
    } else {
        Lang::En
    };
    let port = DEFAULT_PORT.to_string();
    let err_tty = std::io::stderr().is_terminal();
    let out_tty = std::io::stdout().is_terminal();
    let fail = |key: &str, value: &str| {
        eprintln!(
            "{}",
            ht(
                lang,
                key,
                &[("value", value.to_owned()), ("port", port.clone())]
            )
        );
        eprintln!("{}", ht(lang, "seeHelp", &[]));
        2
    };
    if args.tls_cert.is_some() != args.tls_key.is_some() {
        return fail(
            "tlsPair",
            if args.tls_cert.is_none() {
                "--tls-cert"
            } else {
                "--tls-key"
            },
        );
    }
    let listen = match args.listen.as_deref() {
        None => Listen {
            host: LOOPBACK.into(),
            port: DEFAULT_PORT,
        },
        Some(text) => match parse_listen(text) {
            Ok(l) => l,
            Err(CliError::BadAddress) => return fail("badAddress", text),
            Err(CliError::BadPort) => return fail("badPort", text),
        },
    };
    let tls = match (&args.tls_cert, &args.tls_key) {
        (Some(c), Some(k)) => {
            for f in [c, k] {
                if let Err(e) = std::fs::read(f) {
                    eprintln!(
                        "{}",
                        ht(
                            lang,
                            "tlsRead",
                            &[("error", format!("{}: {e}", f.display()))]
                        )
                    );
                    return 2;
                }
            }
            Some((c.clone(), k.clone()))
        }
        _ => None,
    };
    let ui = args.ui.clone().unwrap_or_else(default_ui_dir);
    if !ui.join("index.html").is_file() {
        eprintln!(
            "{}",
            ht(lang, "noUi", &[("path", ui.display().to_string())])
        );
        return 2;
    }
    let reach = exposure(&listen.host);
    if reach != Exposure::Loopback {
        let lines = warning_lines(lang, &listen, &user_name(), tls.is_some());
        eprintln!();
        eprintln!(
            "{}",
            paint(err_tty, "1;97;41", &format!(" ⚠ {} ", lines[0]))
        );
        for line in &lines[1..] {
            eprintln!("{}", paint(err_tty, "33", &format!("  {line}")));
        }
        eprintln!();
        if args.yes {
            eprintln!("{}", ht(lang, "yesGiven", &[]));
        } else {
            if !std::io::stdin().is_terminal() {
                eprintln!("{}", ht(lang, "noTerminal", &[]));
                return 2;
            }
            eprint!("{}", paint(err_tty, "1", &ht(lang, "question", &[])));
            let _ = std::io::stderr().flush();
            let mut answer = String::new();
            let got = std::io::stdin()
                .read_line(&mut answer)
                .ok()
                .filter(|&n| n > 0);
            if got.is_none() {
                eprintln!();
            }
            if !confirmed(got.map(|_| answer.as_str())) {
                eprintln!("{}", ht(lang, "declined", &[]));
                return 1;
            }
        }
    }
    println!("{}", paint(out_tty, "2", &ht(lang, "starting", &[])));
    super::serve::init_logging("WARNING", "plain");
    let addr: Option<SocketAddr> = match listen.host.parse::<IpAddr>() {
        Ok(ip) => Some(SocketAddr::new(ip, listen.port)),
        Err(_) => (listen.host.as_str(), listen.port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next()),
    };
    let Some(addr) = addr else {
        let e = std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "cannot resolve");
        eprintln!("{}", listen_error(lang, &listen, &e));
        return 1;
    };
    let token = crate::util::new_token();
    let opts = ServeOptions {
        host: addr.ip(),
        port: addr.port(),
        token: token.clone(),
        static_dir: Some(ui),
        tls: tls.clone(),
        app_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        ..Default::default()
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{}", ht(lang, "backendFailed", &[("error", e.to_string())]));
            return 1;
        }
    };
    rt.block_on(async move {
        let handle = match crate::start(opts).await {
            Ok(h) => h,
            Err(e) => {
                match e.chain().find_map(|c| c.downcast_ref::<std::io::Error>()) {
                    Some(io) => eprintln!("{}", listen_error(lang, &listen, io)),
                    None => eprintln!(
                        "{}",
                        ht(lang, "backendFailed", &[("error", format!("{e:#}"))])
                    ),
                }
                return 1;
            }
        };
        let interfaces = if reach == Exposure::All {
            network_addresses()
        } else {
            Vec::new()
        };
        println!();
        println!(
            "{}",
            paint(
                out_tty,
                "1",
                &ht(
                    lang,
                    "ready",
                    &[("version", env!("CARGO_PKG_VERSION").to_owned())]
                )
            )
        );
        println!("{}", ht(lang, "openIn", &[]));
        for url in browser_urls(&listen, handle.port, &token, tls.is_some(), &interfaces) {
            println!("  {}", paint(out_tty, "36", &url));
        }
        if reach == Exposure::Loopback {
            println!(
                "{}",
                paint(
                    out_tty,
                    "2",
                    &ht(lang, "onlyHere", &[("port", handle.port.to_string())])
                )
            );
        } else if tls.is_none() {
            println!("{}", paint(out_tty, "33", &ht(lang, "plainNote", &[])));
        }
        println!("{}", ht(lang, "stopHint", &[]));
        println!();
        let _ = std::io::stdout().flush();
        let server = handle.server.clone();
        let stopping = tokio::spawn(async move {
            super::serve::stop_signal(false).await;
            println!("{}", ht(lang, "stopping", &[]));
            server.request_shutdown();
        });
        handle.wait().await;
        stopping.abort();
        println!("{}", ht(lang, "stopped", &[]));
        0
    })
}

/// This computer's network addresses (for the links printed for 0.0.0.0 / ::).
pub fn network_addresses() -> Vec<NetworkAddress> {
    imp::addresses()
}

#[cfg(windows)]
mod imp {
    use super::NetworkAddress;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use windows_sys::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GetAdaptersAddresses, IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6,
    };

    pub fn addresses() -> Vec<NetworkAddress> {
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size: u32 = 16 * 1024;
        let mut buf: Vec<u64> = Vec::new();
        for _ in 0..4 {
            buf = vec![0u64; (size as usize).div_ceil(8)];
            // SAFETY: a buffer of `size` bytes, 8-aligned.
            let rc = unsafe {
                GetAdaptersAddresses(
                    AF_UNSPEC as u32,
                    flags,
                    std::ptr::null(),
                    buf.as_mut_ptr().cast(),
                    &mut size,
                )
            };
            if rc == 0 {
                break;
            }
            if rc != ERROR_BUFFER_OVERFLOW {
                return Vec::new();
            }
        }
        let mut out = Vec::new();
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        // SAFETY: the list GetAdaptersAddresses wrote into `buf`, which outlives the walk.
        unsafe {
            while !p.is_null() {
                let a = &*p;
                let internal = a.IfType == IF_TYPE_SOFTWARE_LOOPBACK;
                // as Node's os.networkInterfaces(): adapters that are up
                let mut u = if a.OperStatus == IfOperStatusUp {
                    a.FirstUnicastAddress
                } else {
                    std::ptr::null_mut()
                };
                while !u.is_null() {
                    let sa = (*u).Address.lpSockaddr;
                    if !sa.is_null() {
                        let ip = match (*sa).sa_family {
                            AF_INET => {
                                let v4 = &*(sa as *const SOCKADDR_IN);
                                Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(
                                    v4.sin_addr.S_un.S_addr,
                                ))))
                            }
                            AF_INET6 => {
                                let v6 = &*(sa as *const SOCKADDR_IN6);
                                Some(IpAddr::V6(Ipv6Addr::from(v6.sin6_addr.u.Byte)))
                            }
                            _ => None,
                        };
                        if let Some(ip) = ip {
                            out.push(NetworkAddress {
                                address: ip,
                                internal: internal || ip.is_loopback(),
                            });
                        }
                    }
                    u = (*u).Next;
                }
                p = a.Next;
            }
        }
        out
    }
}

#[cfg(unix)]
mod imp {
    use super::NetworkAddress;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    pub fn addresses() -> Vec<NetworkAddress> {
        let mut out = Vec::new();
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: getifaddrs fills a list freed with freeifaddrs below.
        unsafe {
            if libc::getifaddrs(&mut head) != 0 {
                return out;
            }
            let mut p = head;
            while !p.is_null() {
                let ifa = &*p;
                let internal = ifa.ifa_flags & libc::IFF_LOOPBACK as libc::c_uint != 0;
                if !ifa.ifa_addr.is_null() {
                    let family = (*ifa.ifa_addr).sa_family as libc::c_int;
                    let ip = if family == libc::AF_INET {
                        let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                        Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr))))
                    } else if family == libc::AF_INET6 {
                        let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                        Some(IpAddr::V6(Ipv6Addr::from(sa.sin6_addr.s6_addr)))
                    } else {
                        None
                    };
                    if let Some(ip) = ip {
                        out.push(NetworkAddress {
                            address: ip,
                            internal: internal || ip.is_loopback(),
                        });
                    }
                }
                p = ifa.ifa_next;
            }
            libc::freeifaddrs(head);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_values() {
        let l = |h: &str, p: u16| {
            Ok(Listen {
                host: h.into(),
                port: p,
            })
        };
        assert_eq!(parse_listen("9000"), l(LOOPBACK, 9000));
        assert_eq!(parse_listen(":0"), l(LOOPBACK, 0));
        assert_eq!(parse_listen("0.0.0.0"), l("0.0.0.0", DEFAULT_PORT));
        assert_eq!(parse_listen("192.168.1.20:8080"), l("192.168.1.20", 8080));
        assert_eq!(parse_listen("[::]:9"), l("::", 9));
        assert_eq!(parse_listen("::"), l("::", DEFAULT_PORT));
        assert_eq!(parse_listen("myhost.lan."), l("myhost.lan", DEFAULT_PORT));
        assert_eq!(parse_listen("[::]:"), Err(CliError::BadPort));
        assert_eq!(parse_listen("host:x"), Err(CliError::BadPort));
        assert_eq!(parse_listen("host:70000"), Err(CliError::BadPort));
        assert_eq!(parse_listen("[nope]"), Err(CliError::BadAddress));
        assert_eq!(parse_listen("bad_host"), Err(CliError::BadAddress));
    }

    #[test]
    fn exposure_and_urls() {
        assert_eq!(exposure("127.0.0.5"), Exposure::Loopback);
        assert_eq!(exposure("localhost"), Exposure::Loopback);
        assert_eq!(exposure("::ffff:127.0.0.1"), Exposure::Loopback);
        assert_eq!(exposure("0.0.0.0"), Exposure::All);
        assert_eq!(exposure("::"), Exposure::All);
        assert_eq!(exposure("192.168.1.2"), Exposure::Network);
        let loop_ = Listen {
            host: LOOPBACK.into(),
            port: 8375,
        };
        assert_eq!(
            browser_urls(&loop_, 8375, "a b", false, &[]),
            ["http://localhost:8375/#token=a%20b"]
        );
        let all = Listen {
            host: "::".into(),
            port: 1,
        };
        let ifs = [
            NetworkAddress {
                address: "10.0.0.2".parse().unwrap(),
                internal: false,
            },
            NetworkAddress {
                address: "fe80::1".parse().unwrap(),
                internal: false,
            },
            NetworkAddress {
                address: "2001:db8::1".parse().unwrap(),
                internal: false,
            },
            NetworkAddress {
                address: "127.0.0.1".parse().unwrap(),
                internal: true,
            },
        ];
        assert_eq!(
            browser_urls(&all, 1, "t", true, &ifs),
            [
                "https://10.0.0.2:1/#token=t",
                "https://[2001:db8::1]:1/#token=t",
                "https://localhost:1/#token=t"
            ]
        );
    }

    #[test]
    fn answers() {
        assert!(confirmed(Some("y\n")) && confirmed(Some(" YES ")) && confirmed(Some("ｙ")));
        assert!(!confirmed(Some("")) && !confirmed(None) && !confirmed(Some("n")));
    }
}

#[cfg(test)]
mod ui_dir_tests {
    use super::ui_candidates;
    use std::path::Path;

    #[test]
    fn finds_the_bundles_ui_folder_in_every_install_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let layouts: [(&str, &str); 4] = [
            ("portable", "ui"),                   // next to the program
            ("usr/bin", "usr/lib/SpaceKnife/ui"), // deb, rpm, AppImage
            (
                "SpaceKnife.app/Contents/MacOS",
                "SpaceKnife.app/Contents/Resources/ui",
            ), // macOS
            ("old", "resources/ui"),
        ];
        for (bin, ui) in layouts {
            let root = tmp.path().join(bin.replace('/', "_"));
            let bin_dir = root.join(bin);
            let ui_dir = if ui == "ui" || ui == "resources/ui" {
                bin_dir.join(ui)
            } else {
                root.join(ui)
            };
            std::fs::create_dir_all(&ui_dir).unwrap();
            std::fs::create_dir_all(&bin_dir).unwrap();
            std::fs::write(ui_dir.join("index.html"), "<!doctype html>").unwrap();
            let found = ui_candidates(&bin_dir)
                .into_iter()
                .find(|c| c.join("index.html").is_file());
            assert_eq!(
                found
                    .as_deref()
                    .map(Path::canonicalize)
                    .transpose()
                    .unwrap(),
                Some(ui_dir.canonicalize().unwrap()),
                "{bin}"
            );
        }
    }
}
