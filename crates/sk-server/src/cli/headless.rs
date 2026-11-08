//! Headless mode: no window. The server serves the built interface over HTTP(S) for a browser, on
//! this computer or another one, next to the WebSocket on the same address (site docs:
//! headless.md). Port of Electron's `headless.ts` (the command line, how exposed an address is,
//! the terminal's texts, the addresses to print) and `headless-main.ts` (ask before opening
//! SpaceKnife to the network, start, print where to open it and who connects, stop on Ctrl+C).
//!
//! Two command lines, both read here (not by clap: the texts are 0.3.4's, in Chinese or English):
//!
//! - `spaceknife-cli headless [--listen <address>] [--yes] [--tls-cert F --tls-key F] [--ui DIR]`:
//!   an unknown option is an error;
//! - 0.3.4's own, the app's: `SpaceKnife --headless`, `SpaceKnife --listen <address>`, the options
//!   anywhere among the arguments and anything else passing by (Chromium's switches in a service
//!   unit written for 0.3.4, macOS's `-psn_…`). `spaceknife-cli` takes it when no command comes
//!   first ([`is_app_form`]); Windows' `SpaceKnife.com` (packaging/console-launcher) and the app's
//!   shells hand it on as it was typed. `--yes` or `--tls-…` without `--headless` / `--listen` is
//!   an error there, as it was.
//!
//! In a portable copy (a `data` folder next to the program) headless mode keeps its data there, as
//! the app does ([`portable_home`]), and the update checks know how SpaceKnife was installed
//! ([`install_kind_of`]).

use std::ffi::{OsStr, OsString};
use std::io::IsTerminal as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{eprint_line, eprint_part, print_line};
use crate::{ServeOptions, Tuning};

/// Port when `--listen` names none: "S" (83) and "K" (75) in ASCII.
pub const DEFAULT_PORT: u16 = 8375;
pub const LOOPBACK: &str = "127.0.0.1";

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

// ------------------------------------------------------------------ the command line

/// Which command line headless mode was started with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// `spaceknife-cli headless …`
    Subcommand,
    /// 0.3.4's app: `SpaceKnife --headless …` / `--listen …`, anywhere among other arguments
    App,
}

/// What headless mode was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub listen: Listen,
    pub yes: bool,
    pub tls: Option<(PathBuf, PathBuf)>,
    pub ui: Option<PathBuf>,
}

/// The command line read: run, show the options, or a mistake (a text's key and its `{value}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    Run(Options),
    Help,
    Error(&'static str, String),
}

/// The commands of `spaceknife-cli`: a first argument naming one is never 0.3.4's form.
const COMMANDS: [&str; 7] = [
    "scan", "run", "snapshot", "serve", "headless", "version", "help",
];

/// The options of 0.3.4's app that start (or belong to) headless mode, `--name` and `--name=…`.
const APP_OPTIONS: [&str; 5] = ["--headless", "--listen", "--yes", "--tls-cert", "--tls-key"];

/// `--name=value` → ("--name", Some(value)); anything else → (itself, None). `None` for a name
/// that isn't text (never an option).
fn split_inline(arg: &OsStr) -> Option<(&str, Option<&OsStr>)> {
    let bytes = arg.as_encoded_bytes();
    if bytes.starts_with(b"--")
        && let Some(eq) = bytes.iter().position(|&b| b == b'=')
    {
        let name = std::str::from_utf8(&bytes[..eq]).ok()?;
        // SAFETY: split right after an ASCII '=' of bytes from `as_encoded_bytes`, which
        // `from_encoded_bytes_unchecked` allows.
        let value = unsafe { OsStr::from_encoded_bytes_unchecked(&bytes[eq + 1..]) };
        return Some((name, Some(value)));
    }
    Some((arg.to_str()?, None))
}

/// Whether `spaceknife-cli`'s arguments (after the program) are 0.3.4's app's command line: no
/// command first, and one of its headless options somewhere.
pub fn is_app_form(args: &[OsString]) -> bool {
    if args
        .first()
        .and_then(|a| a.to_str())
        .is_some_and(|a| COMMANDS.contains(&a))
    {
        return false;
    }
    args.iter()
        .any(|a| split_inline(a).is_some_and(|(name, _)| APP_OPTIONS.contains(&name)))
}

/// Whether the app's own program, given these arguments, runs headless mode (or prints its
/// options) instead of opening a window: what 0.3.4's app did with them (`SpaceKnife.com` and the
/// shells decide by it).
pub fn wants_headless(args: &[OsString]) -> bool {
    args.iter().any(|a| {
        split_inline(a).is_some_and(|(name, _)| {
            APP_OPTIONS.contains(&name) || name == "--help" || name == "-h"
        })
    })
}

/// Read headless mode's options (`args`: what follows `headless`, or the app's whole command line).
pub fn parse(args: &[OsString], form: Form) -> Parsed {
    let mut headless = form == Form::Subcommand;
    let mut listen: Option<String> = None;
    let mut yes = false;
    let mut cert: Option<PathBuf> = None;
    let mut key: Option<PathBuf> = None;
    let mut ui: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        let Some((name, inline)) = split_inline(arg) else {
            if form == Form::Subcommand {
                return Parsed::Error("unknownOption", arg.to_string_lossy().into_owned());
            }
            continue;
        };
        // the option's value: after `=`, else the next argument unless it is another option
        let mut value = || -> Option<OsString> {
            if let Some(v) = inline {
                return Some(v.to_owned());
            }
            let next = args.get(i)?;
            if next.as_encoded_bytes().starts_with(b"--") {
                return None;
            }
            i += 1;
            Some(next.clone())
        };
        let blank = |v: &Option<OsString>| {
            v.as_ref()
                .is_none_or(|v| v.to_string_lossy().trim().is_empty())
        };
        match name {
            "--help" | "-h" => return Parsed::Help,
            "--headless" => headless = true,
            "--yes" => yes = true,
            "--listen" => {
                let v = value();
                if blank(&v) {
                    return Parsed::Error("listenMissing", "--listen".into());
                }
                listen = v.map(|v| v.to_string_lossy().into_owned());
            }
            "--tls-cert" | "--tls-key" => {
                let v = value();
                if blank(&v) {
                    return Parsed::Error("tlsMissing", name.into());
                }
                let path = v.map(PathBuf::from);
                if name == "--tls-cert" {
                    cert = path;
                } else {
                    key = path;
                }
            }
            "--ui" => {
                let v = value();
                if blank(&v) {
                    return Parsed::Error("uiMissing", "--ui".into());
                }
                ui = v.map(PathBuf::from);
            }
            _ if form == Form::Subcommand => {
                return Parsed::Error("unknownOption", arg.to_string_lossy().into_owned());
            }
            _ => {} // the app's form: not ours (Chromium's switches, a checkout's app path)
        }
    }
    if !headless && listen.is_none() {
        let only = if yes {
            "--yes"
        } else if cert.is_some() {
            "--tls-cert"
        } else if key.is_some() {
            "--tls-key"
        } else if ui.is_some() {
            "--ui"
        } else {
            return Parsed::Help;
        };
        return Parsed::Error("headlessOnly", only.into());
    }
    if cert.is_some() != key.is_some() {
        let missing = if cert.is_none() {
            "--tls-cert"
        } else {
            "--tls-key"
        };
        return Parsed::Error("tlsPair", missing.into());
    }
    let listen = match listen {
        None => Listen {
            host: LOOPBACK.into(),
            port: DEFAULT_PORT,
        },
        Some(text) => match parse_listen(&text) {
            Ok(l) => l,
            Err(CliError::BadAddress) => return Parsed::Error("badAddress", text),
            Err(CliError::BadPort) => return Parsed::Error("badPort", text),
        },
    };
    Parsed::Run(Options {
        listen,
        yes,
        tls: cert.zip(key),
        ui,
    })
}

/// The program's name in the texts: as it was started (`SpaceKnife` through SpaceKnife.com, the
/// app's shells), without folder or extension.
pub fn program_name(argv0: Option<&OsStr>) -> String {
    let name = argv0
        .map(Path::new)
        .and_then(Path::file_stem)
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty());
    name.unwrap_or_else(|| "spaceknife-cli".into())
}

/// The command that lists the options, for "run … --help".
fn help_command(form: Form, program: &str) -> String {
    if form == Form::App && !program.eq_ignore_ascii_case("spaceknife-cli") {
        format!("{program} --help")
    } else {
        "spaceknife-cli headless --help".into()
    }
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
            "用法：spaceknife-cli headless [--listen <地址>] [--yes] [--tls-cert <文件> --tls-key <文件>] [--ui <文件夹>]\n\n  --listen <地址>     监听这个地址：主机、主机:端口、:端口、[IPv6]:端口；0.0.0.0 是所有网络接口（默认只监听本机 127.0.0.1:{port}）\n  --yes               不再询问，直接在网络上开放（没有终端时必须加）\n  --tls-cert <文件>   HTTPS 证书（PEM），与 --tls-key 一起用\n  --tls-key <文件>    证书的私钥（PEM，不加密）\n  --ui <文件夹>       构建好的界面（默认：程序旁边的 ui 文件夹）\n  --help              显示这段说明\n\n文档：https://spaceknife.kaltsit.dev/zh/docs/headless/"
        } else {
            "Usage: spaceknife-cli headless [--listen <address>] [--yes] [--tls-cert <file> --tls-key <file>] [--ui <folder>]\n\n  --listen <address>  Listen on this address: host, host:port, :port, [IPv6]:port; 0.0.0.0 is every network interface (default: this computer only, 127.0.0.1:{port})\n  --yes               Don't ask before opening SpaceKnife to the network (needed without a terminal)\n  --tls-cert <file>   HTTPS certificate (PEM), together with --tls-key\n  --tls-key <file>    The certificate's private key (PEM, not encrypted)\n  --ui <folder>       The built interface (default: the ui folder next to the program)\n  --help              Show this help\n\nDocumentation: https://spaceknife.kaltsit.dev/docs/headless/"
        }.into(),
        "appUsage" => if zh {
            "用法：{program} [--headless] [--listen <地址>] [--yes] [--tls-cert <文件> --tls-key <文件>] [--ui <文件夹>]\n\n  --headless          无头模式：不开窗口，在浏览器里使用 SpaceKnife（默认只监听本机 127.0.0.1:{port}）\n  --listen <地址>     监听这个地址（同时启动无头模式）：主机、主机:端口、:端口、[IPv6]:端口；0.0.0.0 是所有网络接口\n  --yes               不再询问，直接在网络上开放（没有终端时必须加）\n  --tls-cert <文件>   HTTPS 证书（PEM），与 --tls-key 一起用\n  --tls-key <文件>    证书的私钥（PEM，不加密）\n  --ui <文件夹>       构建好的界面（默认：程序旁边的 ui 文件夹）\n  --help              显示这段说明\n\n文档：https://spaceknife.kaltsit.dev/zh/docs/headless/"
        } else {
            "Usage: {program} [--headless] [--listen <address>] [--yes] [--tls-cert <file> --tls-key <file>] [--ui <folder>]\n\n  --headless          Headless mode: no window, use SpaceKnife in a browser (listens on this computer only, 127.0.0.1:{port})\n  --listen <address>  Listen on this address (starts headless mode): host, host:port, :port, [IPv6]:port; 0.0.0.0 is every network interface\n  --yes               Don't ask before opening SpaceKnife to the network (needed without a terminal)\n  --tls-cert <file>   HTTPS certificate (PEM), together with --tls-key\n  --tls-key <file>    The certificate's private key (PEM, not encrypted)\n  --ui <folder>       The built interface (default: the ui folder next to the program)\n  --help              Show this help\n\nDocumentation: https://spaceknife.kaltsit.dev/docs/headless/"
        }.into(),
        "listenMissing" => if zh { "--listen 后面要写地址，例如 --listen 192.168.1.20:{port}" } else { "--listen needs an address, for example --listen 192.168.1.20:{port}" }.into(),
        "badAddress" => if zh { "无法识别的地址：{value}（例如 0.0.0.0、192.168.1.20:{port}、[::]:{port}）" } else { "Not an address: {value} (for example 0.0.0.0, 192.168.1.20:{port}, [::]:{port})" }.into(),
        "badPort" => if zh { "端口要是 0 到 65535 的整数：{value}" } else { "The port must be a whole number from 0 to 65535: {value}" }.into(),
        "tlsPair" => if zh { "--tls-cert 和 --tls-key 要一起用（缺少 {value} 的另一半）" } else { "--tls-cert and --tls-key go together ({value} is missing its pair)" }.into(),
        "tlsMissing" => if zh { "{value} 后面要写文件路径" } else { "{value} needs a file path" }.into(),
        "uiMissing" => if zh { "--ui 后面要写文件夹路径" } else { "--ui needs a folder path" }.into(),
        "unknownOption" => if zh { "无法识别的选项：{value}" } else { "Not an option of headless mode: {value}" }.into(),
        "headlessOnly" => if zh { "{value} 只用于无头模式：请同时加上 --headless 或 --listen" } else { "{value} is for headless mode only: add --headless or --listen" }.into(),
        "seeHelp" => if zh { "运行 {help} 查看所有选项。" } else { "Run {help} for the options." }.into(),
        "warnTitle" => if zh { "危险：SpaceKnife 将在网络上开放" } else { "DANGER: SpaceKnife will be open to the network" }.into(),
        "warnAll" => if zh {
            "监听 {address}：这台电脑的所有网络接口，局域网、Wi-Fi、VPN 上的任何设备都能连接，如果这台电脑有公网地址，互联网上的也能。"
        } else {
            "Listening on {address}: every network interface of this computer. Any device on the LAN, Wi-Fi or VPN can connect, and anyone on the internet if this computer has a public address."
        }.into(),
        "warnOne" => if zh { "监听 {address}：能访问这个地址的设备都能连接。" } else { "Listening on {address}: any device that reaches this address can connect." }.into(),
        "warnPower" => if zh {
            "拿到带令牌链接的人，可以用 {user} 的权限使用这台电脑：浏览文件、移到回收站或永久删除，把文件写到 {user} 能写的任何地方（导出、快照另存为、导出设置；写对了地方，就能让这台电脑运行他的程序），还能在这台电脑上弹出管理员授权的请求。"
        } else {
            "Whoever has the link with its token can use this computer with {user}'s rights: browse the files, move them to the Recycle Bin or delete them permanently, write files anywhere {user} can write (export, save a snapshot as, export the settings; in the right place that makes this computer run their program), and bring up a request for administrator rights on this computer."
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
        "tlsCert" => if zh {
            "证书 {file} 读不了：{error}。要的是 PEM 格式（-----BEGIN CERTIFICATE-----），服务器自己的证书在前，中间证书在后。"
        } else {
            "The certificate {file} can't be read: {error}. It must be PEM (-----BEGIN CERTIFICATE-----), the server's own certificate first, then the intermediate ones."
        }.into(),
        "tlsEncrypted" => if zh {
            "私钥 {file} 是加密的（有密码），SpaceKnife 读不了。先解密，例如：openssl pkey -in {file} -out 新私钥.pem（会问密码），再用新文件。"
        } else {
            "The private key {file} is encrypted (it has a password), which SpaceKnife can't read. Decrypt it first, for example: openssl pkey -in {file} -out new-key.pem (it asks for the password), and use the new file."
        }.into(),
        "tlsNoKey" => if zh {
            "{file} 里没有 PEM 格式的私钥。能用的是 -----BEGIN PRIVATE KEY-----（PKCS #8）、RSA PRIVATE KEY（PKCS #1）或 EC PRIVATE KEY（SEC1）。DER 文件、.pfx / .p12 先转换，例如：openssl pkcs12 -in 证书.pfx -nocerts -nodes -out 私钥.pem"
        } else {
            "There is no PEM private key in {file}. What works: -----BEGIN PRIVATE KEY----- (PKCS #8), RSA PRIVATE KEY (PKCS #1) or EC PRIVATE KEY (SEC1). Convert a DER file or a .pfx / .p12 first, for example: openssl pkcs12 -in cert.pfx -nocerts -nodes -out key.pem"
        }.into(),
        "tlsRefused" => if zh { "证书和私钥用不了：{error}（私钥要和证书配对）" } else { "The certificate and the key can't be used: {error} (the key must belong to the certificate)" }.into(),
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
        "connected" => if zh { "{remote} 已连接" } else { "{remote} connected" }.into(),
        "disconnected" => if zh { "{remote} 已断开" } else { "{remote} disconnected" }.into(),
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

/// What is wrong with the certificate or its key, in the terminal's language.
pub fn tls_problem(lang: Lang, err: &crate::http::TlsError) -> String {
    use crate::http::TlsError;
    let file = |p: &Path| p.display().to_string();
    match err {
        TlsError::Read(p, e) => ht(
            lang,
            "tlsRead",
            &[("error", format!("{}: {e}", p.display()))],
        ),
        TlsError::Certificate(p, e) => {
            ht(lang, "tlsCert", &[("file", file(p)), ("error", e.clone())])
        }
        TlsError::EncryptedKey(p) => ht(lang, "tlsEncrypted", &[("file", file(p))]),
        TlsError::NoKey(p) => ht(lang, "tlsNoKey", &[("file", file(p))]),
        TlsError::Refused(e) => ht(lang, "tlsRefused", &[("error", e.clone())]),
    }
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

// ------------------------------------------------------------------ where this copy is

/// This program as the system started it; on Linux and macOS with links resolved (an installed
/// `/usr/bin` entry may be one). Windows keeps the path as it is (no `\\?\` form).
fn this_program() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if cfg!(windows) {
        Some(exe)
    } else {
        Some(std::fs::canonicalize(&exe).unwrap_or(exe))
    }
}

/// The built interface the app's packages ship (`ui/`), found from the executable.
pub fn default_ui_dir() -> PathBuf {
    let dir = this_program()
        .as_deref()
        .and_then(|e| e.parent())
        .map(PathBuf::from)
        .unwrap_or_default();
    ui_candidates(&dir)
        .into_iter()
        .find(|c| c.join("index.html").is_file())
        .unwrap_or_else(|| dir.join("ui"))
}

/// Where the packages put the `ui/` resource, relative to the folder of `spaceknife-cli`:
/// - portable copies and the Windows installer: next to the program (`ui/`; the Electron shell's
///   `resources/ui/`);
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

/// The app bundle an executable belongs to (…/SpaceKnife.app/Contents/MacOS/spaceknife-cli).
pub fn app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let named = |p: &Path, name: &str| p.file_name().is_some_and(|n| n == name);
    let is_app = bundle
        .file_name()
        .is_some_and(|n| n.to_string_lossy().ends_with(".app"));
    (named(macos, "MacOS") && named(contents, "Contents") && is_app).then(|| bundle.to_path_buf())
}

/// A portable copy's data folder: `data` next to the program (on macOS next to the app bundle it
/// is in), as the app finds it (src-tauri portable.rs, 0.3.4's portable.ts). `os` is
/// `std::env::consts::OS`.
pub fn portable_home(exe: &Path, os: &str) -> Option<PathBuf> {
    let program = if os == "macos" {
        app_bundle(exe)?
    } else {
        exe.to_path_buf()
    };
    let dir = program.parent()?.join("data");
    dir.is_dir().then_some(dir)
}

/// dpkg's file list of the installed deb (Tauri names the package `space-knife`, 0.3.4 `spaceknife`).
const DPKG_LISTS: [&str; 2] = [
    "/var/lib/dpkg/info/spaceknife.list",
    "/var/lib/dpkg/info/space-knife.list",
];

/// How this copy was installed (the update checks pick its release file by it), from where the
/// program is and what lies around it, as the app's shells tell (src-tauri install.rs, 0.3.4's
/// update.ts); None when none of them: a build from source. `portable`: SpaceKnife keeps its data
/// next to it ([`portable_home`]).
pub fn install_kind_of(
    exe: &Path,
    os: &str,
    portable: bool,
    env: &dyn Fn(&str) -> Option<OsString>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<&'static str> {
    if portable {
        return Some("portable");
    }
    match os {
        "windows" => {
            let dir = exe.parent()?;
            // Tauri's NSIS installer, electron-builder's (0.3.4)
            ["uninstall.exe", "Uninstall SpaceKnife.exe"]
                .iter()
                .any(|u| exists(&dir.join(u)))
                .then_some("nsis")
        }
        "macos" => app_bundle(exe).map(|_| "dmg"),
        "linux" => {
            // the packages first: programs started from an AppImage inherit its APPIMAGE and APPDIR
            if exe.starts_with("/usr") || exe.starts_with("/opt/SpaceKnife") {
                return Some(if DPKG_LISTS.iter().any(|l| exists(Path::new(l))) {
                    "deb"
                } else {
                    "rpm"
                });
            }
            let appdir = env("APPDIR").filter(|d| !d.is_empty());
            (env("APPIMAGE").is_some_and(|a| !a.is_empty())
                && appdir.is_none_or(|d| exe.starts_with(d)))
            .then_some("appimage")
        }
        _ => None,
    }
}

// ------------------------------------------------------------------ running it

/// Headless mode from a command line: `argv` is the whole one (the program first); `form` says
/// where its options start. Returns the exit code. Call before any thread starts (a portable
/// copy's data folder is set in the environment, as the app does at its start).
pub fn cli(argv: &[OsString], form: Form) -> i32 {
    let program = program_name(argv.first().map(OsString::as_os_str));
    let args = match form {
        Form::Subcommand => argv.get(2..).unwrap_or_default(),
        Form::App => argv.get(1..).unwrap_or_default(),
    };
    let portable = prepare_home();
    super::apply_language();
    let lang = if sk_core::i18n::language() == sk_core::i18n::Language::ZhCn {
        Lang::Zh
    } else {
        Lang::En
    };
    let port = DEFAULT_PORT.to_string();
    match parse(args, form) {
        Parsed::Help => {
            let key = if form == Form::App {
                "appUsage"
            } else {
                "usage"
            };
            print_line(&ht(lang, key, &[("program", program), ("port", port)]));
            0
        }
        Parsed::Error(key, value) => {
            eprint_line(&ht(lang, key, &[("value", value), ("port", port)]));
            eprint_line(&ht(
                lang,
                "seeHelp",
                &[("help", help_command(form, &program))],
            ));
            2
        }
        Parsed::Run(options) => run(options, lang, portable.is_some()),
    }
}

/// A portable copy: SPACEKNIFE_HOME to its data folder unless set. Returns the folder.
fn prepare_home() -> Option<PathBuf> {
    if std::env::var_os("SPACEKNIFE_HOME").is_some_and(|h| !h.is_empty()) {
        return None;
    }
    let dir = portable_home(&this_program()?, std::env::consts::OS)?;
    // SAFETY: at the start of the program, before any thread exists (cli::main), as the app's
    // own main sets it (src-tauri lib.rs prepare_environment)
    unsafe { std::env::set_var("SPACEKNIFE_HOME", &dir) };
    Some(dir)
}

/// The install kind for the server: SPACEKNIFE_INSTALL_KIND when the launcher names one, else
/// from where this program is.
fn install_kind(portable: bool) -> Option<String> {
    if let Ok(k) = std::env::var("SPACEKNIFE_INSTALL_KIND")
        && !k.trim().is_empty()
    {
        return Some(k);
    }
    let exe = this_program()?;
    install_kind_of(
        &exe,
        std::env::consts::OS,
        portable,
        &|k| std::env::var_os(k),
        &|p| p.exists(),
    )
    .map(str::to_owned)
}

/// The version to show and check updates against: the launcher's (SPACEKNIFE_APP_VERSION, as the
/// app's shells pass theirs), else this build's.
pub fn app_version() -> String {
    sk_core::update::app_version().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
}

/// Local time as 0.3.4 stamped a browser's coming and going.
fn stamp() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

fn run(options: Options, lang: Lang, portable: bool) -> i32 {
    let err_tty = std::io::stderr().is_terminal();
    let out_tty = std::io::stdout().is_terminal();
    let Options {
        listen,
        yes,
        tls,
        ui,
    } = options;
    if let Some((cert, key)) = &tls
        && let Err(e) = crate::http::tls_acceptor(cert, key)
    {
        eprint_line(&tls_problem(lang, &e));
        return 2;
    }
    let ui = ui.unwrap_or_else(default_ui_dir);
    if !ui.join("index.html").is_file() {
        eprint_line(&ht(lang, "noUi", &[("path", ui.display().to_string())]));
        return 2;
    }
    let reach = exposure(&listen.host);
    if reach != Exposure::Loopback {
        let lines = warning_lines(lang, &listen, &user_name(), tls.is_some());
        eprint_line("");
        eprint_line(&paint(err_tty, "1;97;41", &format!(" ⚠ {} ", lines[0])));
        for line in &lines[1..] {
            eprint_line(&paint(err_tty, "33", &format!("  {line}")));
        }
        eprint_line("");
        if yes {
            eprint_line(&ht(lang, "yesGiven", &[]));
        } else {
            if !std::io::stdin().is_terminal() {
                eprint_line(&ht(lang, "noTerminal", &[]));
                return 2;
            }
            eprint_part(&paint(err_tty, "1", &ht(lang, "question", &[])));
            let mut answer = String::new();
            let got = std::io::stdin()
                .read_line(&mut answer)
                .ok()
                .filter(|&n| n > 0);
            if got.is_none() {
                eprint_line("");
            }
            if !confirmed(got.map(|_| answer.as_str())) {
                eprint_line(&ht(lang, "declined", &[]));
                return 1;
            }
        }
    }
    print_line(&paint(out_tty, "2", &ht(lang, "starting", &[])));
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
        eprint_line(&listen_error(lang, &listen, &e));
        return 1;
    };
    let token = crate::util::new_token();
    let version = app_version();
    // the system's language needs no passing: the server reads the same system as the app's
    // shells would (sk_core::i18n::system_language)
    let opts = ServeOptions {
        host: addr.ip(),
        port: addr.port(),
        token: token.clone(),
        static_dir: Some(ui),
        tls: tls.clone(),
        app_version: Some(version.clone()),
        install_kind: install_kind(portable),
        ..Default::default()
    };
    let announce: crate::http::SocketHook = Arc::new(move |event, remote| {
        let key = match event {
            crate::http::SocketEvent::Open => "connected",
            crate::http::SocketEvent::Close => "disconnected",
        };
        print_line(&format!(
            "{} {}",
            paint(out_tty, "2", &stamp()),
            ht(lang, key, &[("remote", remote.to_owned())])
        ));
    });
    let tuning = Tuning {
        on_socket: Some(announce),
        ..Tuning::default()
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprint_line(&ht(lang, "backendFailed", &[("error", e.to_string())]));
            return 1;
        }
    };
    rt.block_on(async move {
        let handle = match crate::start_with(opts, tuning).await {
            Ok(h) => h,
            Err(e) => {
                match e.chain().find_map(|c| c.downcast_ref::<std::io::Error>()) {
                    Some(io) => eprint_line(&listen_error(lang, &listen, io)),
                    None => eprint_line(&ht(lang, "backendFailed", &[("error", format!("{e:#}"))])),
                }
                return 1;
            }
        };
        let interfaces = if reach == Exposure::All {
            network_addresses()
        } else {
            Vec::new()
        };
        print_line("");
        print_line(&paint(
            out_tty,
            "1",
            &ht(lang, "ready", &[("version", version)]),
        ));
        print_line(&ht(lang, "openIn", &[]));
        for url in browser_urls(&listen, handle.port, &token, tls.is_some(), &interfaces) {
            print_line(&format!("  {}", paint(out_tty, "36", &url)));
        }
        if reach == Exposure::Loopback {
            print_line(&paint(
                out_tty,
                "2",
                &ht(lang, "onlyHere", &[("port", handle.port.to_string())]),
            ));
        } else if tls.is_none() {
            print_line(&paint(out_tty, "33", &ht(lang, "plainNote", &[])));
        }
        print_line(&ht(lang, "stopHint", &[]));
        print_line("");
        let server = handle.server.clone();
        let stopping = tokio::spawn(async move {
            super::serve::stop_signal(false).await;
            print_line(&ht(lang, "stopping", &[]));
            server.request_shutdown();
        });
        handle.wait().await;
        stopping.abort();
        print_line(&ht(lang, "stopped", &[]));
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

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn here(port: u16) -> Listen {
        Listen {
            host: LOOPBACK.into(),
            port,
        }
    }

    fn run_with(listen: Listen, yes: bool, tls: Option<(&str, &str)>, ui: Option<&str>) -> Parsed {
        Parsed::Run(Options {
            listen,
            yes,
            tls: tls.map(|(c, k)| (PathBuf::from(c), PathBuf::from(k))),
            ui: ui.map(PathBuf::from),
        })
    }

    fn err(key: &'static str, value: &str) -> Parsed {
        Parsed::Error(key, value.into())
    }

    #[test]
    fn the_subcommands_options() {
        let p = |a: &[&str]| parse(&os(a), Form::Subcommand);
        assert_eq!(p(&[]), run_with(here(DEFAULT_PORT), false, None, None));
        // in any order, `--name value` or `--name=value`
        assert_eq!(
            p(&["--yes", "--listen", "0.0.0.0:9", "--ui=/srv/ui"]),
            run_with(
                Listen {
                    host: "0.0.0.0".into(),
                    port: 9
                },
                true,
                None,
                Some("/srv/ui")
            )
        );
        assert_eq!(
            p(&["--tls-key=k.pem", "--tls-cert", "c.pem", "--listen=:1"]),
            run_with(here(1), false, Some(("c.pem", "k.pem")), None)
        );
        assert_eq!(
            p(&["--listen", "1", "--headless"]),
            run_with(here(1), false, None, None)
        );
        assert_eq!(p(&["--listen", "1", "--help"]), Parsed::Help);
        assert_eq!(p(&["-h"]), Parsed::Help);
        // mistakes, as 0.3.4 named them
        assert_eq!(p(&["--listen"]), err("listenMissing", "--listen"));
        assert_eq!(p(&["--listen", "--yes"]), err("listenMissing", "--listen"));
        assert_eq!(p(&["--listen="]), err("listenMissing", "--listen"));
        assert_eq!(p(&["--listen", " "]), err("listenMissing", "--listen"));
        assert_eq!(p(&["--tls-cert"]), err("tlsMissing", "--tls-cert"));
        assert_eq!(p(&["--ui"]), err("uiMissing", "--ui"));
        assert_eq!(p(&["--tls-cert", "c"]), err("tlsPair", "--tls-key"));
        assert_eq!(p(&["--tls-key", "k"]), err("tlsPair", "--tls-cert"));
        assert_eq!(p(&["--listen", "a_b"]), err("badAddress", "a_b"));
        assert_eq!(p(&["--listen", "h:99999"]), err("badPort", "h:99999"));
        // anything else is a mistake here (a typo must not listen somewhere else)
        assert_eq!(p(&["--lisen", "0.0.0.0"]), err("unknownOption", "--lisen"));
        assert_eq!(p(&["serve"]), err("unknownOption", "serve"));
    }

    #[test]
    fn the_apps_own_command_line() {
        let p = |a: &[&str]| parse(&os(a), Form::App);
        assert_eq!(
            p(&["--headless"]),
            run_with(here(DEFAULT_PORT), false, None, None)
        );
        assert_eq!(
            p(&["--listen", "9000"]),
            run_with(here(9000), false, None, None)
        );
        // its options anywhere, everything else passing by (Chromium, macOS, a checkout's app path)
        assert_eq!(
            p(&[
                "--no-sandbox",
                "-psn_0_12345",
                ".",
                "--yes",
                "--listen=9",
                "--disable-gpu"
            ]),
            run_with(here(9), true, None, None)
        );
        assert_eq!(p(&["--help", "--listen"]), Parsed::Help);
        // headless mode's options without it
        assert_eq!(p(&["--yes"]), err("headlessOnly", "--yes"));
        assert_eq!(p(&["--tls-cert", "c"]), err("headlessOnly", "--tls-cert"));
        assert_eq!(
            p(&["--tls-key", "k", "--x"]),
            err("headlessOnly", "--tls-key")
        );
        assert_eq!(p(&["--ui", "u"]), err("headlessOnly", "--ui"));
        assert_eq!(p(&["--listen"]), err("listenMissing", "--listen"));
    }

    #[test]
    fn which_command_lines_are_the_apps() {
        let app = |a: &[&str]| is_app_form(&os(a));
        assert!(app(&["--headless"]));
        assert!(app(&["--listen", "0.0.0.0"]));
        assert!(app(&["--listen=::"]));
        assert!(app(&["--ui", "x", "--yes"]));
        assert!(app(&["--no-sandbox", "--tls-cert=c"]));
        assert!(!app(&[]));
        assert!(!app(&["--help"]));
        assert!(!app(&["--version"]));
        assert!(!app(&["scan", "--headless"]));
        assert!(!app(&["headless", "--listen", "x"]));
        assert!(!app(&["help"]));
        // what opens a window and what doesn't, for the app's own program
        let headless = |a: &[&str]| wants_headless(&os(a));
        assert!(headless(&["--headless"]) && headless(&["-h"]) && headless(&["--help"]));
        assert!(headless(&["--yes"]) && headless(&["--tls-key=k"]));
        assert!(!headless(&[]) && !headless(&["--updated"]) && !headless(&["--no-sandbox"]));
    }

    #[cfg(unix)]
    #[test]
    fn values_that_are_not_text_reach_the_options_intact() {
        use std::os::unix::ffi::OsStrExt as _;
        let odd = OsStr::from_bytes(b"/srv/\xff/ui");
        let mut inline = b"--ui=".to_vec();
        inline.extend_from_slice(odd.as_bytes());
        for args in [
            vec![OsString::from("--ui"), odd.to_owned()],
            vec![OsString::from(OsStr::from_bytes(&inline))],
        ] {
            match parse(&args, Form::Subcommand) {
                Parsed::Run(o) => assert_eq!(o.ui.as_deref(), Some(Path::new(odd))),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn program_names() {
        assert_eq!(program_name(Some(OsStr::new("SpaceKnife"))), "SpaceKnife");
        assert_eq!(
            program_name(Some(OsStr::new("/usr/bin/spaceknife-cli"))),
            "spaceknife-cli"
        );
        assert_eq!(program_name(None), "spaceknife-cli");
        assert_eq!(help_command(Form::App, "SpaceKnife"), "SpaceKnife --help");
        assert_eq!(
            help_command(Form::App, "spaceknife-cli"),
            "spaceknife-cli headless --help"
        );
        assert_eq!(
            help_command(Form::Subcommand, "SpaceKnife"),
            "spaceknife-cli headless --help"
        );
    }

    /// Review issue 17: the warning says everything a token gives, writing files anywhere the
    /// account can and the elevation prompt included, not only browsing and deleting.
    #[test]
    fn the_warning_says_what_the_token_gives() {
        let all = Listen {
            host: "0.0.0.0".into(),
            port: 1,
        };
        let zh = warning_lines(Lang::Zh, &all, "lenny", false).join("\n");
        for part in [
            "lenny 的权限",
            "永久删除",
            "把文件写到 lenny 能写的任何地方",
            "导出、快照另存为、导出设置",
            "运行他的程序",
            "管理员授权",
        ] {
            assert!(zh.contains(part), "{part}: {zh}");
        }
        let en = warning_lines(Lang::En, &all, "lenny", true).join("\n");
        for part in [
            "lenny's rights",
            "delete them permanently",
            "write files anywhere lenny can write",
            "save a snapshot as",
            "run their program",
            "administrator rights",
            "encrypted with HTTPS",
        ] {
            assert!(en.contains(part), "{part}: {en}");
        }
    }

    #[test]
    fn every_text_has_both_languages() {
        for key in [
            "usage",
            "appUsage",
            "listenMissing",
            "badAddress",
            "badPort",
            "tlsPair",
            "tlsMissing",
            "uiMissing",
            "unknownOption",
            "headlessOnly",
            "seeHelp",
            "warnTitle",
            "warnAll",
            "warnOne",
            "warnPower",
            "warnToken",
            "warnPlain",
            "warnTls",
            "question",
            "yesGiven",
            "declined",
            "noTerminal",
            "tlsRead",
            "tlsCert",
            "tlsEncrypted",
            "tlsNoKey",
            "tlsRefused",
            "noUi",
            "starting",
            "backendFailed",
            "inUse",
            "notHere",
            "noAccess",
            "listenFailed",
            "ready",
            "openIn",
            "onlyHere",
            "plainNote",
            "stopHint",
            "connected",
            "disconnected",
            "stopping",
            "stopped",
        ] {
            let (zh, en) = (ht(Lang::Zh, key, &[]), ht(Lang::En, key, &[]));
            assert!(zh != key && en != key && zh != en, "{key}");
            assert!(
                zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{key}: {zh}"
            );
        }
        let usage = ht(
            Lang::En,
            "appUsage",
            &[("program", "SpaceKnife".into()), ("port", "8375".into())],
        );
        assert!(usage.starts_with("Usage: SpaceKnife [--headless] [--listen <address>]"));
        assert!(usage.contains("127.0.0.1:8375"));
    }

    #[test]
    fn tls_problems_in_words() {
        use crate::http::TlsError;
        let enc = tls_problem(Lang::Zh, &TlsError::EncryptedKey("k.pem".into()));
        assert!(
            enc.contains("加密") && enc.contains("openssl pkey -in k.pem"),
            "{enc}"
        );
        let none = tls_problem(Lang::En, &TlsError::NoKey("k.der".into()));
        assert!(
            none.contains("PKCS #8") && none.contains("openssl pkcs12"),
            "{none}"
        );
    }

    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    #[test]
    fn install_kinds() {
        let none = |_: &Path| false;
        let k = |exe: &str, os: &str, portable: bool, exists: &dyn Fn(&Path) -> bool| {
            install_kind_of(Path::new(exe), os, portable, &no_env, exists)
        };
        assert_eq!(
            k("C:\\SK\\spaceknife-cli.exe", "windows", true, &none),
            Some("portable")
        );
        let tauri = |p: &Path| p.ends_with("uninstall.exe");
        let electron = |p: &Path| p.ends_with("Uninstall SpaceKnife.exe");
        assert_eq!(
            k("C:\\SK\\spaceknife-cli.exe", "windows", false, &tauri),
            Some("nsis")
        );
        assert_eq!(
            k("C:\\SK\\spaceknife-cli.exe", "windows", false, &electron),
            Some("nsis")
        );
        assert_eq!(
            k("C:\\SK\\spaceknife-cli.exe", "windows", false, &none),
            None
        );
        assert_eq!(
            k(
                "/Applications/SpaceKnife.app/Contents/MacOS/spaceknife-cli",
                "macos",
                false,
                &none
            ),
            Some("dmg")
        );
        assert_eq!(
            k(
                "/Users/lenny/target/release/spaceknife-cli",
                "macos",
                false,
                &none
            ),
            None
        );
        let dpkg = |p: &Path| p == Path::new("/var/lib/dpkg/info/spaceknife.list");
        assert_eq!(
            k("/usr/bin/spaceknife-cli", "linux", false, &dpkg),
            Some("deb")
        );
        assert_eq!(
            k("/usr/lib/SpaceKnife/spaceknife-cli", "linux", false, &none),
            Some("rpm")
        );
        assert_eq!(
            k("/opt/SpaceKnife/spaceknife-cli", "linux", false, &none),
            Some("rpm")
        );
        assert_eq!(
            k("/home/lenny/sk/spaceknife-cli", "linux", false, &none),
            None
        );
        let appimage = |key: &str| match key {
            "APPIMAGE" => Some(OsString::from("/home/lenny/SpaceKnife.AppImage")),
            "APPDIR" => Some(OsString::from("/tmp/.mount_SpaceK1")),
            _ => None,
        };
        let exe = Path::new("/tmp/.mount_SpaceK1/usr/bin/spaceknife-cli");
        assert_eq!(
            install_kind_of(exe, "linux", false, &appimage, &none),
            Some("appimage")
        );
        // a program started from an AppImage, elsewhere
        let other = Path::new("/home/lenny/bin/spaceknife-cli");
        assert_eq!(
            install_kind_of(other, "linux", false, &appimage, &none),
            None
        );
    }

    #[test]
    fn portable_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("SpaceKnife-0.3.5-win-x64-portable");
        std::fs::create_dir_all(folder.join("data")).unwrap();
        let exe = folder.join("spaceknife-cli.exe");
        assert_eq!(portable_home(&exe, "windows"), Some(folder.join("data")));
        assert_eq!(portable_home(&exe, "linux"), Some(folder.join("data")));
        // macOS: next to the app bundle, not inside it
        let mac = tmp.path().join("mac");
        let bin = mac.join("SpaceKnife.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&bin).unwrap();
        assert_eq!(portable_home(&bin.join("spaceknife-cli"), "macos"), None);
        std::fs::create_dir_all(mac.join("data")).unwrap();
        assert_eq!(
            portable_home(&bin.join("spaceknife-cli"), "macos"),
            Some(mac.join("data"))
        );
        // no data folder, or a file of that name: not portable
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(portable_home(&plain.join("spaceknife-cli"), "linux"), None);
        std::fs::write(plain.join("data"), "").unwrap();
        assert_eq!(portable_home(&plain.join("spaceknife-cli"), "linux"), None);
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
