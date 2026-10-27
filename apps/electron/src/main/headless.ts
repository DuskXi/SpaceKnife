// Headless mode (`--headless`, `--listen <address>`): no window. The backend runs as usual and the
// interface is served over HTTP for a browser, on this computer or another one
// (site/src/content/docs/docs/headless.md). This module holds the pure parts: the command line, how
// exposed an address is, the terminal's texts and the addresses to print. headless-server.ts serves
// the files and forwards the WebSocket; headless-main.ts puts it together.

import { isIPv4, isIPv6 } from "node:net";

/** Port when `--listen` names none: "S" (83) and "K" (75) in ASCII. */
export const DEFAULT_PORT = 8375;
/** The Origin the forwarded WebSocket carries to the backend (`--allow-origin`): the browser's own
 *  origin is checked against the Host it asked for (headless-server.ts), the backend sees this one. */
export const HEADLESS_ORIGIN = "http://headless.spaceknife.invalid";
export const LOOPBACK = "127.0.0.1";

export interface Listen {
  host: string;
  port: number;
}

export interface TlsFiles {
  cert: string;
  key: string;
}

export type CliError = "listenMissing" | "badAddress" | "badPort" | "tlsPair" | "tlsMissing" | "headlessOnly";

export type CommandLine =
  | { mode: "window" }
  | { mode: "help" }
  | { mode: "error"; error: CliError; value: string }
  | { mode: "headless"; listen: Listen; yes: boolean; tls: TlsFiles | null };

/** loopback: only this computer; all: every interface (0.0.0.0, ::); network: one address others reach. */
export type Exposure = "loopback" | "all" | "network";

/**
 * The options of the app's executable, anywhere among its arguments (Chromium's own switches, the
 * app's path in a checkout and macOS's -psn_… pass by). `--listen` implies headless mode;
 * `--headless` alone listens on this computer only.
 */
export function parseCommandLine(args: readonly string[]): CommandLine {
  let headless = false;
  let listen: string | null = null;
  let yes = false;
  let cert: string | null = null;
  let key: string | null = null;
  for (let i = 0; i < args.length; i++) {
    const arg = args[i]!;
    const [name, inline] = arg.startsWith("--") && arg.includes("=") ? [arg.slice(0, arg.indexOf("=")), arg.slice(arg.indexOf("=") + 1)] : [arg, null];
    const value = (): string | null => {
      if (inline !== null) return inline;
      const next = args[i + 1];
      if (next === undefined || next.startsWith("--")) return null;
      i++;
      return next;
    };
    switch (name) {
      case "--help":
      case "-h":
        return { mode: "help" };
      case "--headless":
        headless = true;
        break;
      case "--yes":
        yes = true;
        break;
      case "--listen": {
        const v = value();
        if (v === null || !v.trim()) return { mode: "error", error: "listenMissing", value: "--listen" };
        listen = v;
        break;
      }
      case "--tls-cert":
      case "--tls-key": {
        const v = value();
        if (v === null || !v.trim()) return { mode: "error", error: "tlsMissing", value: name };
        if (name === "--tls-cert") cert = v;
        else key = v;
        break;
      }
    }
  }
  if (!headless && listen === null) {
    if (yes) return { mode: "error", error: "headlessOnly", value: "--yes" };
    if (cert !== null || key !== null) return { mode: "error", error: "headlessOnly", value: cert !== null ? "--tls-cert" : "--tls-key" };
    return { mode: "window" };
  }
  if ((cert === null) !== (key === null)) return { mode: "error", error: "tlsPair", value: cert === null ? "--tls-cert" : "--tls-key" };
  let where: Listen = { host: LOOPBACK, port: DEFAULT_PORT };
  if (listen !== null) {
    const parsed = parseListen(listen);
    if (typeof parsed === "string") return { mode: "error", error: parsed, value: listen };
    where = parsed;
  }
  return { mode: "headless", listen: where, yes, tls: cert !== null && key !== null ? { cert, key } : null };
}

const HOSTNAME = /^(?=.{1,253}$)[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)*\.?$/;

/**
 * `--listen`'s value: `HOST`, `HOST:PORT`, `:PORT` or `PORT` (this computer), `[IPv6]`,
 * `[IPv6]:PORT`, or a bare IPv6 address (`::`). Port 0 lets the system pick a free one.
 */
export function parseListen(text: string): Listen | "badAddress" | "badPort" {
  const v = text.trim();
  let host: string;
  let port: string | undefined;
  if (/^\d+$/.test(v)) {
    host = LOOPBACK;
    port = v;
  } else if (v.startsWith("[")) {
    const m = /^\[([^\]]+)\](?::(\d*))?$/.exec(v);
    if (!m || !isIPv6(m[1]!)) return "badAddress";
    host = m[1]!;
    port = m[2];
    if (port === "") return "badPort";
  } else if ((v.match(/:/g) ?? []).length > 1) {
    if (!isIPv6(v)) return "badAddress";
    host = v;
  } else if (v.includes(":")) {
    const at = v.indexOf(":");
    host = v.slice(0, at) || LOOPBACK;
    port = v.slice(at + 1);
    if (!/^\d+$/.test(port)) return "badPort";
  } else {
    host = v;
  }
  if (!isIPv4(host) && !isIPv6(host) && !HOSTNAME.test(host)) return "badAddress";
  const n = port === undefined ? DEFAULT_PORT : Number(port);
  if (!Number.isInteger(n) || n < 0 || n > 65535) return "badPort";
  return { host: host.replace(/\.$/, ""), port: n };
}

/** How far an address reaches. A host name other than localhost counts as the network: what it
 *  resolves to isn't known here. */
export function exposure(host: string): Exposure {
  const h = host.toLowerCase().replace(/^\[|\]$/g, "");
  if (h === "localhost" || h === "::1" || (isIPv4(h) && h.startsWith("127.")) || /^::ffff:127\.\d+\.\d+\.\d+$/.test(h)) return "loopback";
  if (h === "0.0.0.0" || (isIPv6(h) && /^[0:]+$/.test(h))) return "all";
  return "network";
}

/** A host as it appears in a URL (IPv6 in brackets). */
export const urlHost = (host: string): string => (isIPv6(host) ? `[${host}]` : host);

/** An address in the terminal's texts: host:port, IPv6 bracketed. */
export const showListen = (l: Listen): string => `${urlHost(l.host)}:${l.port}`;

export interface NetworkAddress {
  address: string;
  family: string | number;
  internal: boolean;
}

/**
 * The addresses to open in a browser: the one listened on, or for 0.0.0.0 / :: each address of this
 * computer that others can reach (IPv4; IPv6 too for ::, without link-local ones), then this
 * computer's own. The token rides in the fragment, which browsers don't send to the server.
 */
export function browserUrls(listen: Listen, port: number, token: string, secure: boolean, interfaces: readonly NetworkAddress[]): string[] {
  const scheme = secure ? "https" : "http";
  const url = (host: string) => `${scheme}://${urlHost(host)}:${port}/#token=${encodeURIComponent(token)}`;
  const reach = exposure(listen.host);
  if (reach !== "all") return [url(listen.host === LOOPBACK ? "localhost" : listen.host)];
  const v6 = listen.host !== "0.0.0.0";
  const hosts: string[] = [];
  for (const a of interfaces) {
    if (a.internal) continue;
    const family = String(a.family);
    if (family === "IPv4" || family === "4") hosts.push(a.address);
    else if (v6 && (family === "IPv6" || family === "6") && !/^fe[89ab]/i.test(a.address)) hosts.push(a.address);
  }
  return [...new Set(hosts)].map(url).concat(url("localhost"));
}

/** Y / yes continues (full-width letters too, as an input method may type them); anything else,
 *  an empty line or no answer at all stops. */
export function confirmed(answer: string | null): boolean {
  if (answer === null) return false;
  return /^(y|yes)$/i.test(answer.normalize("NFKC").trim());
}

/** Where headless mode can ask its question: a terminal on stdin; on Windows the console that
 *  SpaceKnife.com waits in (packaging/console-launcher); else nowhere. */
export function questionInput(platform: string, env: Readonly<Record<string, string | undefined>>, stdinIsTTY: boolean): "stdin" | "console" | null {
  if (stdinIsTTY) return "stdin";
  if (platform === "win32" && env.SPACEKNIFE_CONSOLE === "1") return "console";
  return null;
}

// ------------------------------------------------------------------ the terminal's texts

export type Lang = "zh-CN" | "en";

const TEXT = {
  usage: {
    "zh-CN": [
      "用法：SpaceKnife [--headless] [--listen <地址>] [--yes] [--tls-cert <文件> --tls-key <文件>]",
      "",
      "  --headless          无头模式：不开窗口，在浏览器里使用 SpaceKnife（默认只监听本机 127.0.0.1:{port}）",
      "  --listen <地址>     监听这个地址（同时启动无头模式）：主机、主机:端口、:端口、[IPv6]:端口；0.0.0.0 是所有网络接口",
      "  --yes               不再询问，直接在网络上开放（没有终端时必须加）",
      "  --tls-cert <文件>   HTTPS 证书（PEM），与 --tls-key 一起用",
      "  --tls-key <文件>    证书的私钥（PEM）",
      "  --help              显示这段说明",
      "",
      "文档：https://spaceknife.kaltsit.dev/zh/docs/headless/",
    ],
    en: [
      "Usage: SpaceKnife [--headless] [--listen <address>] [--yes] [--tls-cert <file> --tls-key <file>]",
      "",
      "  --headless          Headless mode: no window, use SpaceKnife in a browser (listens on this computer only, 127.0.0.1:{port})",
      "  --listen <address>  Listen on this address (starts headless mode): host, host:port, :port, [IPv6]:port; 0.0.0.0 is every network interface",
      "  --yes               Don't ask before opening SpaceKnife to the network (needed without a terminal)",
      "  --tls-cert <file>   HTTPS certificate (PEM), together with --tls-key",
      "  --tls-key <file>    The certificate's private key (PEM)",
      "  --help              Show this help",
      "",
      "Documentation: https://spaceknife.kaltsit.dev/docs/headless/",
    ],
  },
  listenMissing: { "zh-CN": "--listen 后面要写地址，例如 --listen 192.168.1.20:{port}", en: "--listen needs an address, for example --listen 192.168.1.20:{port}" },
  badAddress: { "zh-CN": "无法识别的地址：{value}（例如 0.0.0.0、192.168.1.20:{port}、[::]:{port}）", en: "Not an address: {value} (for example 0.0.0.0, 192.168.1.20:{port}, [::]:{port})" },
  badPort: { "zh-CN": "端口要是 0 到 65535 的整数：{value}", en: "The port must be a whole number from 0 to 65535: {value}" },
  tlsPair: { "zh-CN": "--tls-cert 和 --tls-key 要一起用（缺少 {value} 的另一半）", en: "--tls-cert and --tls-key go together ({value} is missing its pair)" },
  tlsMissing: { "zh-CN": "{value} 后面要写文件路径", en: "{value} needs a file path" },
  headlessOnly: { "zh-CN": "{value} 只用于无头模式：请同时加上 --headless 或 --listen", en: "{value} is for headless mode only: add --headless or --listen" },
  seeHelp: { "zh-CN": "运行 SpaceKnife --help 查看所有选项。", en: "Run SpaceKnife --help for the options." },
  warnTitle: { "zh-CN": "危险：SpaceKnife 将在网络上开放", en: "DANGER: SpaceKnife will be open to the network" },
  warnAll: {
    "zh-CN": "监听 {address}：这台电脑的所有网络接口，局域网、Wi-Fi、VPN 上的任何设备都能连接，如果这台电脑有公网地址，互联网上的也能。",
    en: "Listening on {address}: every network interface of this computer. Any device on the LAN, Wi-Fi or VPN can connect, and anyone on the internet if this computer has a public address.",
  },
  warnOne: {
    "zh-CN": "监听 {address}：能访问这个地址的设备都能连接。",
    en: "Listening on {address}: any device that reaches this address can connect.",
  },
  warnPower: {
    "zh-CN": "拿到带令牌链接的人，可以用 {user} 的权限浏览这台电脑上的文件、移到回收站，或永久删除。",
    en: "Whoever has the link with its token can browse the files on this computer with {user}'s rights, move them to the Recycle Bin, or delete them permanently.",
  },
  warnToken: {
    "zh-CN": "令牌是唯一的保护：只发给你信任的人，不要在公共网络上开放。",
    en: "The token is the only protection: give the link only to people you trust, and don't do this on a public network.",
  },
  warnPlain: {
    "zh-CN": "这是明文 HTTP：同一网络上的人可以看到令牌、路径和文件名。要加密请用 --tls-cert / --tls-key，或改用 SSH 隧道（只用 --headless）。",
    en: "This is plain HTTP: people on the same network can read the token, the paths and the file names. Use --tls-cert / --tls-key to encrypt, or an SSH tunnel (--headless alone).",
  },
  warnTls: { "zh-CN": "流量经 HTTPS 加密。", en: "Traffic is encrypted with HTTPS." },
  question: { "zh-CN": "确定要开放吗？输入 Y 继续，N 退出 [y/N]：", en: "Open it to the network? Type Y to go on, N to quit [y/N]: " },
  yesGiven: { "zh-CN": "已加 --yes：不再询问。", en: "--yes given: not asking." },
  declined: { "zh-CN": "已取消，没有监听任何地址。", en: "Cancelled: nothing is listening." },
  noTerminal: {
    "zh-CN": "没有终端可以回答：要在网络上开放，请加上 --yes（表示你了解上面的风险）。",
    en: "There is no terminal to answer in: to open SpaceKnife to the network, add --yes (you accept the risk above).",
  },
  windowsHint: {
    "zh-CN": "提示：SpaceKnife.exe 是窗口程序，终端不会等它。去掉 .exe，运行 SpaceKnife --headless …（同一文件夹里的 SpaceKnife.com），终端就会等待、能回答 Y/N、能用 Ctrl+C 停止。",
    en: "Tip: SpaceKnife.exe is a windowed program, so the terminal doesn't wait for it. Leave out .exe and run SpaceKnife --headless … (SpaceKnife.com, in the same folder): the terminal then waits, takes the Y/N answer and stops it with Ctrl+C.",
  },
  tlsRead: { "zh-CN": "无法读取证书或私钥：{error}", en: "Can't read the certificate or the key: {error}" },
  starting: { "zh-CN": "正在启动后端…", en: "Starting the backend…" },
  backendFailed: { "zh-CN": "后端没有启动：{error}", en: "The backend didn't start: {error}" },
  backendExited: { "zh-CN": "后端已退出（退出码 {code}），无头模式停止。", en: "The backend exited (exit code {code}): headless mode stops." },
  inUse: { "zh-CN": "端口 {port} 已被占用：换一个（--listen {host}:端口），或写 :0 让系统挑一个空闲端口。", en: "Port {port} is in use: pick another (--listen {host}:port), or :0 for any free port." },
  notHere: { "zh-CN": "{host} 不是这台电脑的地址。", en: "{host} isn't an address of this computer." },
  noAccess: { "zh-CN": "没有权限监听端口 {port}（1024 以下的端口通常需要管理员权限）。", en: "No permission to listen on port {port} (ports below 1024 usually need administrator rights)." },
  listenFailed: { "zh-CN": "无法监听 {address}：{error}", en: "Can't listen on {address}: {error}" },
  ready: { "zh-CN": "SpaceKnife 无头模式 · {version}", en: "SpaceKnife headless mode · {version}" },
  openIn: { "zh-CN": "在浏览器里打开：", en: "Open in a browser:" },
  onlyHere: { "zh-CN": "只有这台电脑能访问。要从别的设备访问，用 --listen（或 SSH 隧道转发端口 {port}）。", en: "Only this computer can connect. To use it from another device, use --listen (or forward port {port} through an SSH tunnel)." },
  plainNote: { "zh-CN": "明文 HTTP：令牌和文件名在网络上可见。", en: "Plain HTTP: the token and the file names can be read on the network." },
  stopHint: { "zh-CN": "按 Ctrl+C 停止。", en: "Press Ctrl+C to stop." },
  connected: { "zh-CN": "{remote} 已连接", en: "{remote} connected" },
  disconnected: { "zh-CN": "{remote} 已断开", en: "{remote} disconnected" },
  stopping: { "zh-CN": "正在停止…", en: "Stopping…" },
  stopped: { "zh-CN": "已停止。", en: "Stopped." },
} as const;

export type HeadlessText = keyof typeof TEXT;

/** A terminal text in the language, placeholders filled in. */
export function ht(lang: Lang, key: HeadlessText, fields: Record<string, string | number> = {}): string {
  const text = TEXT[key][lang];
  const fill = (s: string) => s.replace(/\{(\w+)\}/g, (m, k: string) => (k in fields ? String(fields[k]) : m));
  return Array.isArray(text) ? (text as readonly string[]).map(fill).join("\n") : fill(text as string);
}

/** zh-CN for a Chinese system language tag, English otherwise. */
export const langOf = (tag: string | undefined): Lang => ((tag ?? "").toLowerCase().replace("_", "-").startsWith("zh") ? "zh-CN" : "en");

/** The warning before listening on the network, line by line (without colours). */
export function warningLines(lang: Lang, listen: Listen, user: string, secure: boolean): string[] {
  const address = showListen(listen);
  return [
    ht(lang, "warnTitle"),
    ht(lang, exposure(listen.host) === "all" ? "warnAll" : "warnOne", { address }),
    ht(lang, "warnPower", { user }),
    ht(lang, "warnToken"),
    ht(lang, secure ? "warnTls" : "warnPlain"),
  ];
}

/** Why listening failed, from the error's code. */
export function listenError(lang: Lang, listen: Listen, err: NodeJS.ErrnoException): string {
  const fields = { port: listen.port, host: urlHost(listen.host), address: showListen(listen), error: err.message };
  if (err.code === "EADDRINUSE") return ht(lang, "inUse", fields);
  if (err.code === "EADDRNOTAVAIL") return ht(lang, "notHere", fields);
  if (err.code === "EACCES") return ht(lang, "noAccess", fields);
  return ht(lang, "listenFailed", fields);
}
