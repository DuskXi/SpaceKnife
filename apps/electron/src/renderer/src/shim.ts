// Browser mode: the page runs outside Electron and this stands in for the preload API (window.sk).
//
// - Headless mode (`SpaceKnife --headless`, `--listen`; main/headless-server.ts) serves the page.
//   Its link carries the token in the fragment (#token=…, never sent to the server); the WebSocket
//   is this page's own origin (/v1), and /sk/info, with the token, says which computer the files
//   are on. The token then leaves the address bar and stays in this tab (sessionStorage), so a
//   reload still connects. There is no window to control, no file manager or terminal to open,
//   no update to install: what needs that computer says so (and copies the path).
// - The development demo (tools/demo/m3_web.py, screenshots) puts the backend's address in the
//   link as well (#ws=…&token=…) and looks like the app, window buttons that do nothing included;
//   `&browser=1` shows it the way headless mode does.

import type { BackendState, RemoteInfo, SkApi } from "../../shared/api";

import { t } from "@/lib/i18n";
import { askPath } from "@/state/browser";

const TOKEN_KEY = "sk.headless.token";
const SOCKET_PATH = "/v1"; // spaceknife_protocol.WS_PATH

export interface HeadlessInfo extends RemoteInfo {
  platform: SkApi["platform"];
  version: string;
}

/** The token: the link's (#token=…), else the one this tab kept. */
export function linkToken(hash: string, kept: string | null): string | null {
  return new URLSearchParams(hash.replace(/^#/, "")).get("token") || kept || null;
}

/** The backend's WebSocket behind headless mode: this page's origin, ws or wss as the page. */
export const socketUrl = (loc: { protocol: string; host: string }): string => `${loc.protocol === "https:" ? "wss" : "ws"}://${loc.host}${SOCKET_PATH}`;

/** headless-server.ts's /sk/info; "unauthorized" for a wrong token (or a restarted server). */
export async function fetchInfo(fetcher: typeof fetch, token: string): Promise<HeadlessInfo | "unauthorized"> {
  const res = await fetcher("/sk/info", { headers: { authorization: `Bearer ${token}` }, cache: "no-store" });
  if (res.status === 401) return "unauthorized";
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as HeadlessInfo;
}

/** The separator of the server's paths. */
const sepOf = (platform: SkApi["platform"]) => (platform === "win32" ? "\\" : "/");

/** Where the browser's own abilities come in (tests pass their own). */
export interface BrowserEnv {
  notice(text: string): void;
  copy(text: string): void;
  open(url: string): void;
  askPath: typeof askPath;
  /** a scan ended while the tab was in the background */
  flash(): void;
}

export interface BrowserApiConfig {
  state: BackendState;
  platform: SkApi["platform"];
  version: string;
  host: SkApi["host"];
  remote: RemoteInfo | null;
  justUpdated?: boolean;
}

/** window.sk for a browser tab. */
export function browserApi(cfg: BrowserApiConfig, env: BrowserEnv): SkApi {
  const noop = () => {};
  const server = cfg.remote?.hostname || t("服务器");
  const sep = sepOf(cfg.platform);
  const start = cfg.remote?.home ? cfg.remote.home.replace(/[\\/]+$/, "") + sep : "";
  // what needs the other computer's desktop: say so and copy the path, the useful part of it
  const elsewhere = (text: string, path?: string) => {
    if (!cfg.remote) return console.info(text, path ?? "");
    if (path) env.copy(path);
    env.notice(path ? `${text} ${t("路径已复制")}` : text);
  };
  const filters = (o: { filters: { extensions: string[] }[] }) =>
    o.filters
      .flatMap((f) => f.extensions)
      .map((x) => `.${x}`)
      .join(" ");
  return {
    platform: cfg.platform,
    version: cfg.version,
    host: cfg.host,
    remote: cfg.remote,
    justUpdated: !!cfg.justUpdated,
    versions: { electron: "", chrome: "", node: "" },
    backend: async () => cfg.state,
    onBackend: () => noop,
    window: {
      minimize: noop,
      toggleMaximize: noop,
      close: noop,
      isMaximized: async () => false,
      flash: () => env.flash(),
      onMaximized: () => noop,
    },
    shell: {
      showItemInFolder: (path) => elsewhere(t("文件在 {host} 上，浏览器里打不开它所在的文件夹。", { host: server }), path),
      openPath: async (path) => (elsewhere(t("文件在 {host} 上，浏览器里打不开它。", { host: server }), path), ""),
      openTerminal: (dir) => elsewhere(t("终端要在 {host} 上打开。", { host: server }), dir),
      copyText: (text) => env.copy(text),
      openFullDiskAccess: () => elsewhere(t("请在 {host} 上打开「系统设置 › 隐私与安全性 › 完全磁盘访问权限」。", { host: server })),
      openExternal: (url) => env.open(url),
    },
    update: {
      install: async () => t("浏览器里不能安装更新：请在 {host} 上更新 SpaceKnife", { host: server }),
    },
    setLanguage: noop,
    setTheme: noop, // the browser's own prefers-color-scheme, data-theme for a choice (lib/theme.ts)
    dialog: {
      openSnapshot: () => env.askPath({ title: t("打开快照文件"), hint: t("{host} 上的快照文件（.sks）路径", { host: server }), value: start }),
      openFile: (o) => env.askPath({ title: o.title, hint: t("{host} 上的文件路径（{types}）", { host: server, types: filters(o) }), value: start }),
      saveFile: (o) => env.askPath({ title: o.title, hint: t("保存到 {host} 上的这个位置", { host: server }), value: start + o.defaultName }),
      openFolders: async () => {
        const text = await env.askPath({ title: t("选择要扫描的文件夹"), hint: t("{host} 上的文件夹路径，多个用 ; 分隔", { host: server }), value: start });
        return text ? text.split(";").map((x) => x.trim()).filter(Boolean) : [];
      },
    },
    files: {
      pathOf: () => "", // a dropped file is this computer's, not the server's
    },
  };
}

/** Copy to the clipboard: the Clipboard API needs a secure context (HTTPS or localhost); over plain
 *  HTTP on the network, the old copy command. */
function copyText(text: string): void {
  if (window.isSecureContext && navigator.clipboard) {
    void navigator.clipboard.writeText(text).catch(() => legacyCopy(text));
  } else legacyCopy(text);
}

function legacyCopy(text: string): void {
  const focused = document.activeElement as HTMLElement | null;
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.style.cssText = "position:fixed;top:0;left:0;opacity:0;pointer-events:none";
  document.body.appendChild(area);
  area.select();
  try {
    document.execCommand("copy");
  } catch {
    // nothing to copy with: the notice still shows the action
  }
  area.remove();
  focused?.focus?.();
}

const browserEnv: BrowserEnv = {
  // the store loads after the shim (main.tsx): import it when there is something to say
  notice: (text) => void import("@/state/store").then((m) => m.toast(text, 4000)),
  copy: copyText,
  open: (url) => void window.open(url, "_blank", "noopener,noreferrer"),
  askPath,
  // no taskbar to flash: a dot in the tab's title until it is looked at
  flash: () => {
    if (document.hasFocus() || document.title.startsWith("● ")) return;
    const title = document.title;
    document.title = `● ${title}`;
    window.addEventListener("focus", () => (document.title = title), { once: true });
  },
};

/** The demo as it always was: the browser's own prompt for paths (the screenshot scripts answer it). */
const demoEnv: BrowserEnv = {
  ...browserEnv,
  askPath: async (req) => window.prompt(req.title, req.value)?.trim() || null,
};

function kept(): string | null {
  try {
    return sessionStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

function keep(token: string | null): void {
  try {
    if (token) sessionStorage.setItem(TOKEN_KEY, token);
    else sessionStorage.removeItem(TOKEN_KEY);
  } catch {
    // private mode without storage: a reload needs the link again
  }
}

const guessPlatform = (): SkApi["platform"] =>
  navigator.userAgent.includes("Windows") ? "win32" : navigator.userAgent.includes("Mac") ? "darwin" : "linux";

/** Set window.sk when the page runs in a browser (no preload); resolves once it is there. */
export async function installShim(): Promise<void> {
  if (window.sk) return;
  const params = new URLSearchParams(location.hash.slice(1));
  const ws = params.get("ws");
  if (ws) {
    // the development demo
    const token = params.get("token");
    const asBrowser = params.get("browser") === "1";
    window.sk = browserApi(
      {
        state: token ? { state: "ready", info: { url: ws, token, pid: 0 } } : { state: "failed", message: t("浏览器模式：地址里缺少 #ws=…&token=…") },
        platform: (params.get("platform") as SkApi["platform"]) ?? guessPlatform(),
        version: "0.3.4-web",
        host: asBrowser ? "browser" : "app",
        remote: asBrowser ? { hostname: location.hostname, home: "", secure: location.protocol === "https:" } : null,
        justUpdated: params.get("updated") === "1", // demos: show the 「已更新」 note
      },
      asBrowser ? browserEnv : demoEnv,
    );
    return;
  }
  // headless mode
  const token = linkToken(location.hash, kept());
  const fail = (message: string): void => {
    window.sk = browserApi({ state: { state: "failed", message }, platform: guessPlatform(), version: "", host: "browser", remote: null }, browserEnv);
  };
  if (!token) return fail(t("请打开启动 SpaceKnife 的终端里显示的地址：它带有 #token=…，没有令牌无法连接。"));
  let info: HeadlessInfo | "unauthorized";
  try {
    info = await fetchInfo(fetch, token);
  } catch (err) {
    return fail(t("连不上 {origin}：{error}", { origin: location.origin, error: err instanceof Error ? err.message : String(err) }));
  }
  if (info === "unauthorized") {
    keep(null);
    return fail(t("链接里的令牌不对，或者 SpaceKnife 已经重新启动：请打开终端里新显示的地址。"));
  }
  keep(token);
  if (location.hash) history.replaceState(null, "", location.pathname + location.search);
  window.sk = browserApi(
    {
      state: { state: "ready", info: { url: socketUrl(location), token, pid: 0 } },
      platform: info.platform,
      version: info.version,
      host: "browser",
      remote: { hostname: info.hostname, home: info.home, secure: info.secure },
    },
    browserEnv,
  );
}
