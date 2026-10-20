// Electron main process: one frameless window, the backend child process, a few native services.

import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { dirname, extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { BrowserWindow, app, clipboard, dialog, ipcMain, protocol, shell } from "electron";

import type { BackendState } from "../shared/api";
import { type Backend, bundledBackend, startBackend } from "./backend";
import { portableHome } from "./portable";
import { type Lang, mt, setLang } from "./text";
import { installKind, installRefusal, projectPage } from "./update";

const here = dirname(fileURLToPath(import.meta.url));
const APP_ORIGIN = "app://spaceknife";
const APP_ID = "io.github.duskxi.spaceknife"; // electron-builder.yml appId: taskbar grouping, shortcuts
const RENDERER_DIR = join(here, "../renderer");
const MIME: Record<string, string> = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".mjs": "text/javascript",
  ".css": "text/css",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".woff2": "font/woff2",
  ".json": "application/json",
};

// A portable copy keeps its data next to the executable: the backend inherits SPACEKNIFE_HOME.
const portable = portableHome(process.execPath, process.platform, app.isPackaged, process.env);
if (portable) process.env.SPACEKNIFE_HOME = portable;
// what the backend's update check downloads, and what main may start once it is downloaded
const INSTALL_KIND = installKind(process.execPath, process.platform, app.isPackaged, process.env, portable);

// The Chromium profile (storage, caches) lives in an "electron" folder of SpaceKnife's own: an isolated
// home (demos, tests, portable copies) gets its own; otherwise <appData>/SpaceKnife/electron, apart from
// the backend's files (on Linux the backend's config folder is ~/.config/SpaceKnife as well).
app.setPath(
  "userData",
  process.env.SPACEKNIFE_HOME ? join(resolve(process.env.SPACEKNIFE_HOME), "electron") : join(app.getPath("appData"), "SpaceKnife", "electron"),
);
if (process.platform === "win32") app.setAppUserModelId(APP_ID);

protocol.registerSchemesAsPrivileged([
  { scheme: "app", privileges: { standard: true, secure: true, supportFetchAPI: true, codeCache: true } },
]);

let backend: Backend | null = null;
let backendState: BackendState = { state: "starting" };
let win: BrowserWindow | null = null;

function repoRoot(): string {
  return process.env.SPACEKNIFE_REPO ? resolve(process.env.SPACEKNIFE_REPO) : resolve(app.getAppPath(), "../..");
}

function setBackend(state: BackendState): void {
  backendState = state;
  win?.webContents.send("sk:backend", state);
}

async function launchBackend(): Promise<void> {
  const devUrl = process.env.ELECTRON_RENDERER_URL;
  const origins = devUrl ? [new URL(devUrl).origin] : [];
  try {
    backend = await startBackend({
      repoRoot: repoRoot(),
      bundled: app.isPackaged ? bundledBackend(process.resourcesPath) : null,
      allowOrigins: origins,
      home: process.env.SPACEKNIFE_HOME,
      locale: app.getPreferredSystemLanguages()[0] ?? app.getLocale(),
      appVersion: app.getVersion(),
      installKind: INSTALL_KIND,
      downloadDir: downloadsDir(),
      onLog: (line) => console.log(`[backend] ${line}`),
    });
    backend.proc.once("exit", (code) => setBackend({ state: "exited", code }));
    setBackend({ state: "ready", info: backend.info });
  } catch (err) {
    setBackend({ state: "failed", message: err instanceof Error ? err.message : String(err) });
  }
}

/** The user's download folder (the backend saves updates there); null when the OS names none. */
function downloadsDir(): string | null {
  try {
    return app.getPath("downloads");
  } catch {
    return null;
  }
}

/** The app icon for the window (Linux; a packaged Windows app shows its executable's icon). */
function windowIcon(): string | undefined {
  if (process.platform === "linux") return join(app.getAppPath(), "resources", "icon.png");
  if (process.platform === "win32" && !app.isPackaged) return join(app.getAppPath(), "build", "icon.ico");
  return undefined; // macOS: the bundle icon
}

function createWindow(): void {
  const mac = process.platform === "darwin";
  win = new BrowserWindow({
    width: 1440,
    height: 900,
    minWidth: 1024,
    minHeight: 640,
    show: false,
    frame: mac,
    titleBarStyle: mac ? "hiddenInset" : "hidden",
    backgroundColor: "#08090B",
    title: "SpaceKnife",
    icon: windowIcon(),
    webPreferences: {
      preload: join(here, "../preload/index.mjs"),
      contextIsolation: true,
      sandbox: false,
      spellcheck: false,
      additionalArguments: [`--sk-app-version=${app.getVersion()}`], // window.sk.version (preload)
    },
  });
  // SPACEKNIFE_START_MINIMIZED=1 (milestone demos): appear in the taskbar without taking the focus
  win.once("ready-to-show", () => (process.env.SPACEKNIFE_START_MINIMIZED === "1" ? win?.minimize() : win?.show()));
  win.on("maximize", () => win?.webContents.send("sk:maximized", true));
  win.on("unmaximize", () => win?.webContents.send("sk:maximized", false));
  win.on("closed", () => (win = null));
  if (process.env.ELECTRON_RENDERER_URL) void win.loadURL(process.env.ELECTRON_RENDERER_URL);
  else void win.loadURL(`${APP_ORIGIN}/index.html`);
}

function registerIpc(): void {
  ipcMain.handle("sk:backend", () => backendState);
  ipcMain.on("sk:win", (_e, action: string) => {
    if (!win) return;
    if (action === "minimize") win.minimize();
    else if (action === "toggleMaximize") (win.isMaximized() ? win.unmaximize() : win.maximize());
    else if (action === "close") win.close();
    else if (action === "flash" && !win.isFocused()) {
      if (process.platform === "darwin") app.dock?.bounce("informational");
      else win.flashFrame(true); // Windows taskbar flash / X11 urgency hint; stops when focused
    }
  });
  app.on("browser-window-focus", (_e, w) => w.flashFrame(false));
  ipcMain.handle("sk:isMaximized", () => win?.isMaximized() ?? false);
  ipcMain.on("sk:showItemInFolder", (_e, path: string) => shell.showItemInFolder(path));
  ipcMain.handle("sk:openPath", (_e, path: string) => shell.openPath(path));
  ipcMain.on("sk:copyText", (_e, text: string) => clipboard.writeText(text));
  ipcMain.on("sk:setLanguage", (_e, lang: Lang) => setLang(lang));
  ipcMain.on("sk:openTerminal", (_e, dir: string) => openTerminal(dir));
  // one fixed settings pane, not a URL from the renderer (spaceknife.privacy.FULL_DISK_ACCESS_SETTINGS)
  ipcMain.on("sk:openFullDiskAccess", () => {
    if (process.platform === "darwin") void shell.openExternal("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles");
  });
  ipcMain.on("sk:openExternal", (_e, url: unknown) => {
    const page = projectPage(url);
    if (page) void shell.openExternal(page);
    else console.warn(`[shell] not opening ${String(url)}: not a page of the project on GitHub`);
  });
  ipcMain.handle("sk:installUpdate", (_e, path: unknown) => installUpdate(String(path)));
  ipcMain.handle("sk:openFolders", async () => {
    const opts = { title: mt("pickFolders"), properties: ["openDirectory" as const, "multiSelections" as const] };
    const res = win ? await dialog.showOpenDialog(win, opts) : await dialog.showOpenDialog(opts);
    return res.canceled ? [] : res.filePaths;
  });
  ipcMain.handle("sk:openFile", async (_e, o: { title: string; filters: { name: string; extensions: string[] }[] }) => {
    const opts = { title: String(o.title), properties: ["openFile" as const], filters: o.filters };
    const res = win ? await dialog.showOpenDialog(win, opts) : await dialog.showOpenDialog(opts);
    return res.canceled ? null : (res.filePaths[0] ?? null);
  });
  ipcMain.handle("sk:saveFile", async (_e, o: { title: string; filters: { name: string; extensions: string[] }[]; defaultName: string }) => {
    const opts = { title: String(o.title), defaultPath: String(o.defaultName), filters: o.filters };
    const res = win ? await dialog.showSaveDialog(win, opts) : await dialog.showSaveDialog(opts);
    return res.canceled || !res.filePath ? null : res.filePath;
  });
  ipcMain.handle("sk:openSnapshot", async () => {
    const opts = { title: mt("openSnapshot"), properties: ["openFile" as const], filters: [{ name: mt("snapshotFilter"), extensions: ["sks"] }] };
    const res = win ? await dialog.showOpenDialog(win, opts) : await dialog.showOpenDialog(opts);
    return res.canceled ? null : (res.filePaths[0] ?? null);
  });
}

/** Start the update the backend downloaded (update.download): "" when started, else why not. */
async function installUpdate(file: string): Promise<string> {
  const downloads = downloadsDir();
  const refusal = downloads ? installRefusal(file, INSTALL_KIND, downloads, process.platform) : "updateNotInDownloads";
  if (refusal) {
    console.warn(`[update] not starting ${file}: ${refusal}`);
    return mt(refusal);
  }
  if (INSTALL_KIND === "nsis") return startInstaller(file);
  // a disk image mounts (the user drags the new app over the old one), a package opens in the
  // system's installer: either replaces files of this app, so it quits once the file is open
  const error = await shell.openPath(file);
  if (error) return mt("updateStartFailed", { error });
  setTimeout(() => app.quit(), 500);
  return "";
}

/** The Windows installer closes the app itself and replaces it; quitting stops the backend first. */
function startInstaller(file: string): Promise<string> {
  return new Promise((done) => {
    const child = spawn(file, [], { detached: true, stdio: "ignore" });
    child.once("error", (err) => done(mt("updateStartFailed", { error: err.message })));
    child.once("spawn", () => {
      child.unref();
      done("");
      app.quit();
    });
  });
}

function openTerminal(dir: string): void {
  if (process.platform === "win32") spawn("cmd.exe", ["/c", "start", "", "wt.exe", "-d", dir], { detached: true, shell: false });
  else if (process.platform === "darwin") spawn("open", ["-a", "Terminal", dir], { detached: true });
  else spawn("x-terminal-emulator", [], { cwd: dir, detached: true });
}

function serveRenderer(): void {
  protocol.handle("app", async (request) => {
    const url = new URL(request.url);
    const rel = normalize(decodeURIComponent(url.pathname)).replace(/^[/\\]+/, "") || "index.html";
    const file = resolve(RENDERER_DIR, rel);
    if (!file.startsWith(RENDERER_DIR)) return new Response("forbidden", { status: 403 });
    try {
      const body = await readFile(file);
      return new Response(body, { headers: { "content-type": MIME[extname(file)] ?? "application/octet-stream" } });
    } catch {
      return new Response("not found", { status: 404 });
    }
  });
}

app.whenReady().then(() => {
  serveRenderer();
  registerIpc();
  createWindow();
  void launchBackend();
});

app.on("window-all-closed", () => app.quit());

let quitting = false;
app.on("before-quit", (event) => {
  if (quitting || !backend) return;
  event.preventDefault();
  quitting = true;
  void backend.stop().finally(() => app.quit());
});
