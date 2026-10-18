// Electron main process: one frameless window, the backend child process, a few native services.

import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { dirname, extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { BrowserWindow, app, clipboard, dialog, ipcMain, protocol, shell } from "electron";

import type { BackendState } from "../shared/api";
import { type Backend, startBackend } from "./backend";

const here = dirname(fileURLToPath(import.meta.url));
const APP_ORIGIN = "app://spaceknife";
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

// an isolated home (demos, tests) also gets its own Chromium profile: storage, caches, single-instance lock
if (process.env.SPACEKNIFE_HOME) app.setPath("userData", join(resolve(process.env.SPACEKNIFE_HOME), "electron"));

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
      allowOrigins: origins,
      home: process.env.SPACEKNIFE_HOME,
      onLog: (line) => console.log(`[backend] ${line}`),
    });
    backend.proc.once("exit", (code) => setBackend({ state: "exited", code }));
    setBackend({ state: "ready", info: backend.info });
  } catch (err) {
    setBackend({ state: "failed", message: err instanceof Error ? err.message : String(err) });
  }
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
    webPreferences: {
      preload: join(here, "../preload/index.mjs"),
      contextIsolation: true,
      sandbox: false,
      spellcheck: false,
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
  ipcMain.on("sk:openTerminal", (_e, dir: string) => openTerminal(dir));
  ipcMain.handle("sk:openFolders", async () => {
    const opts = { title: "选择要扫描的文件夹", properties: ["openDirectory" as const, "multiSelections" as const] };
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
    const opts = { title: "打开快照文件", properties: ["openFile" as const], filters: [{ name: "SpaceKnife 快照", extensions: ["sks"] }] };
    const res = win ? await dialog.showOpenDialog(win, opts) : await dialog.showOpenDialog(opts);
    return res.canceled ? null : (res.filePaths[0] ?? null);
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
