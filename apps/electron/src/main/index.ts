// Electron main process: one frameless window, the backend child process, a few native services.

import { spawn } from "node:child_process";
import { chmod, copyFile, mkdir, readFile, readdir, rename, rm, writeFile } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { BrowserWindow, app, clipboard, dialog, ipcMain, protocol, shell } from "electron";

import type { BackendState } from "../shared/api";
import { type Backend, type BackendOptions, bundledBackend, startBackend } from "./backend";
import { fileIn, mimeOf } from "./files";
import { DEFAULT_PORT, ht, parseCommandLine } from "./headless";
import { exitFlushed, prepareHeadless, runHeadless, systemLang } from "./headless-main";
import { portableHome } from "./portable";
import { type Lang, mt, setLang } from "./text";
import { installKind, installRefusal, nsisUpdateArgs, projectPage, restartMode, swapLauncher, swapScript, swapScriptFile } from "./update";

const here = dirname(fileURLToPath(import.meta.url));
const APP_ORIGIN = "app://spaceknife";
const APP_ID = "io.github.duskxi.spaceknife"; // electron-builder.yml appId: taskbar grouping, shortcuts
const RENDERER_DIR = join(here, "../renderer");
// --headless / --listen: no window, the interface in a browser (headless.ts); --help: the options
const CLI = parseCommandLine(process.argv.slice(1));

// A portable copy keeps its data next to the executable: the backend inherits SPACEKNIFE_HOME.
const portable = portableHome(process.execPath, process.platform, app.isPackaged, process.env);
if (portable) process.env.SPACEKNIFE_HOME = portable;
// what the backend's update check downloads, and what main may start once it is downloaded
const INSTALL_KIND = installKind(process.execPath, process.platform, app.isPackaged, process.env, portable);
// started by 「重启并更新」 (the Windows installer passes it after a silent update): the renderer says so once
const JUST_UPDATED = process.argv.includes("--updated");

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

/** How the backend starts, in the window and in headless mode alike. */
function backendOptions(allowOrigins: string[], onLog: (line: string) => void): BackendOptions {
  return {
    repoRoot: repoRoot(),
    bundled: app.isPackaged ? bundledBackend(process.resourcesPath) : null,
    allowOrigins,
    home: process.env.SPACEKNIFE_HOME,
    locale: app.getPreferredSystemLanguages()[0] ?? app.getLocale(),
    appVersion: app.getVersion(),
    installKind: INSTALL_KIND,
    onLog,
  };
}

function setBackend(state: BackendState): void {
  backendState = state;
  win?.webContents.send("sk:backend", state);
}

async function launchBackend(): Promise<void> {
  const devUrl = process.env.ELECTRON_RENDERER_URL;
  const origins = devUrl ? [new URL(devUrl).origin] : [];
  try {
    backend = await startBackend(backendOptions(origins, (line) => console.log(`[backend] ${line}`)));
    backend.proc.once("exit", (code) => setBackend({ state: "exited", code }));
    setBackend({ state: "ready", info: backend.info });
  } catch (err) {
    setBackend({ state: "failed", message: err instanceof Error ? err.message : String(err) });
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
      // window.sk.version and window.sk.justUpdated (preload)
      additionalArguments: [`--sk-app-version=${app.getVersion()}`, ...(JUST_UPDATED ? ["--sk-updated"] : [])],
    },
  });
  // SPACEKNIFE_START_MINIMIZED=1 (milestone demos): appear in the taskbar without taking the focus
  win.once("ready-to-show", () => (process.env.SPACEKNIFE_START_MINIMIZED === "1" ? win?.minimize() : win?.show()));
  win.on("maximize", () => win?.webContents.send("sk:maximized", true));
  win.on("unmaximize", () => win?.webContents.send("sk:maximized", false));
  win.on("closed", () => (win = null));
  // a link never opens a window nor leaves the app (the help page sends its links to sk:openExternal;
  // this catches what slips past it, a dropped file included)
  win.webContents.setWindowOpenHandler(({ url }) => {
    const page = projectPage(url);
    if (page) void shell.openExternal(page);
    return { action: "deny" };
  });
  const origin = process.env.ELECTRON_RENDERER_URL ? new URL(process.env.ELECTRON_RENDERER_URL).origin : APP_ORIGIN;
  win.webContents.on("will-navigate", (event, url) => {
    if (!url.startsWith(`${origin}/`)) event.preventDefault();
  });
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
    else console.warn(`[shell] not opening ${String(url)}: not a page of the project (GitHub, the website)`);
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

/** 「重启并更新」 with the file the backend downloaded (update.download): "" when the app is on its way
 *  out to the new version, else why not (and the app goes on as it was). */
async function installUpdate(file: string): Promise<string> {
  const updates = backend?.updatesDir ?? null;
  const refusal = updates ? installRefusal(file, INSTALL_KIND, updates, process.platform) : "updateMissing";
  if (refusal) {
    console.warn(`[update] not using ${file}: ${refusal}`);
    return mt(refusal);
  }
  try {
    switch (restartMode(INSTALL_KIND)) {
      case "installer":
        // silent: no wizard, the data stays, the installer starts the new version (update.ts)
        return await restartInto(() => launch(file, nsisUpdateArgs(process.execPath, process.env)));
      case "replace":
        return INSTALL_KIND === "appimage" ? await replaceAppImage(file) : await replacePortable(file, updates!);
      case "open": {
        // a disk image mounts (the user drags the new app over the old one), a package opens in the
        // system's installer: either replaces files of this app, so it quits once the file is open
        const error = await shell.openPath(file);
        if (error) return mt("updateStartFailed", { error });
        setTimeout(() => app.quit(), 500);
        return "";
      }
      default:
        return mt("updateNoInstall");
    }
  } catch (err) {
    console.warn(`[update] ${file}: ${String(err)}`);
    return mt("updateStartFailed", { error: err instanceof Error ? err.message : String(err) });
  }
}

/** Stop the backend (its files must be free before they are replaced), start what puts the new
 *  version in place, and exit. When that can't start, the backend comes back and the app goes on. */
async function restartInto(start: () => Promise<void>): Promise<string> {
  const running = backend;
  backend = null;
  quitting = true; // before-quit: nothing left to stop
  await running?.stop();
  try {
    await start();
  } catch (err) {
    quitting = false;
    void launchBackend();
    return mt("updateStartFailed", { error: err instanceof Error ? err.message : String(err) });
  }
  app.exit(0);
  return "";
}

/** Start a program on its own (it outlives the app); resolves once it runs. */
function launch(file: string, args: string[]): Promise<void> {
  return new Promise((done, fail) => {
    const child = spawn(file, args, { detached: true, stdio: "ignore", windowsHide: true });
    child.once("error", fail);
    child.once("spawn", () => {
      child.unref();
      done();
    });
  });
}

/** An AppImage: the new file takes the old one's place (a rename: the running app keeps the old
 *  file open), then the new one starts. Needs a folder the user can write to. */
async function replaceAppImage(file: string): Promise<string> {
  const target = process.env.APPIMAGE;
  if (!target) return mt("updateNoInstall");
  const next = `${target}.new`;
  try {
    await copyFile(file, next);
    await chmod(next, 0o755);
    await rename(next, target);
  } catch (err) {
    await rm(next, { force: true });
    return mt("updateReplaceFailed", { error: err instanceof Error ? err.message : String(err) });
  }
  return restartInto(() => launch(target, ["--updated"]));
}

/** A portable copy: unpack the new version next to the backend's downloads, then a script swaps the
 *  files once the app has quit (update.ts swapScript) and starts it again. The "data" folder stays. */
async function replacePortable(file: string, updates: string): Promise<string> {
  const version = /^SpaceKnife-(.+)-(?:win|linux)-x64-portable\.(?:zip|tar\.gz)$/.exec(basename(file))?.[1];
  const platform = process.platform;
  if (!version || (platform !== "win32" && platform !== "linux")) return mt("updateNoInstall");
  const staging = join(updates, `staging-${version}`);
  try {
    await rm(staging, { recursive: true, force: true });
    await mkdir(staging, { recursive: true });
    await unpack(file, staging);
  } catch (err) {
    return mt("updateUnpackFailed", { error: err instanceof Error ? err.message : String(err) });
  }
  const top = (await readdir(staging, { withFileTypes: true })).filter((e) => e.isDirectory());
  const staged = top.length === 1 ? join(staging, top[0]!.name) : null;
  if (!staged || !(await readdir(staged)).includes(basename(process.execPath))) {
    return mt("updateUnpackFailed", { error: basename(file) });
  }
  const script = join(updates, `update-${version}.${platform === "win32" ? "ps1" : "sh"}`);
  const plan = {
    pid: process.pid,
    appDir: dirname(process.execPath),
    staged,
    previous: join(updates, `previous-${app.getVersion()}`),
    exe: process.execPath,
    log: join(updates, `update-${version}.log`),
  };
  await writeFile(script, swapScriptFile(platform, swapScript(platform, plan)));
  const how = swapLauncher(platform, script);
  return restartInto(() => (how.wait ? runToEnd(how.cmd, how.args) : launch(how.cmd, how.args)));
}

/** Run a hidden helper and wait until it is done (the starter of the Windows swap); fails on a
 * non-zero exit or after 30 s. */
function runToEnd(cmd: string, args: string[]): Promise<void> {
  return new Promise((done, fail) => {
    const child = spawn(cmd, args, { stdio: "ignore", windowsHide: true });
    const timer = setTimeout(() => {
      child.kill();
      fail(new Error(`${cmd} did not finish`));
    }, 30_000);
    child.once("error", (err) => {
      clearTimeout(timer);
      fail(err);
    });
    child.once("exit", (code) => {
      clearTimeout(timer);
      if (code === 0) done();
      else fail(new Error(`${cmd} exited with ${code}`));
    });
  });
}

/** Unpack a portable archive (zip on Windows, tar.gz on Linux) with the system's tar: Windows 10 1803
 *  and later have bsdtar, which reads zip and refuses entries leaving the folder; PowerShell's
 *  Expand-Archive where it is missing. */
function unpack(file: string, into: string): Promise<void> {
  const run = (cmd: string, args: string[]) =>
    new Promise<void>((done, fail) => {
      const child = spawn(cmd, args, { stdio: "ignore", windowsHide: true });
      child.once("error", fail);
      child.once("exit", (code) => (code === 0 ? done() : fail(new Error(`${cmd} exited with ${code}`))));
    });
  if (process.platform !== "win32") return run("tar", ["-xzf", file, "-C", into]);
  return run("tar.exe", ["-xf", file, "-C", into]).catch(() =>
    run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `Expand-Archive -LiteralPath '${file.replace(/'/g, "''")}' -DestinationPath '${into.replace(/'/g, "''")}' -Force`]),
  );
}

function openTerminal(dir: string): void {
  if (process.platform === "win32") spawn("cmd.exe", ["/c", "start", "", "wt.exe", "-d", dir], { detached: true, shell: false });
  else if (process.platform === "darwin") spawn("open", ["-a", "Terminal", dir], { detached: true });
  else spawn("x-terminal-emulator", [], { cwd: dir, detached: true });
}

function serveRenderer(): void {
  protocol.handle("app", async (request) => {
    const file = fileIn(RENDERER_DIR, new URL(request.url).pathname);
    if (!file) return new Response("forbidden", { status: 403 });
    try {
      const body = await readFile(file);
      return new Response(body, { headers: { "content-type": mimeOf(file) } });
    } catch {
      return new Response("not found", { status: 404 });
    }
  });
}

if (CLI.mode === "help" || CLI.mode === "error") {
  const lang = systemLang();
  if (CLI.mode === "help") process.stdout.write(`${ht(lang, "usage", { port: DEFAULT_PORT })}\n`);
  else process.stderr.write(`${ht(lang, CLI.error, { value: CLI.value, port: DEFAULT_PORT })}\n${ht(lang, "seeHelp")}\n`);
  exitFlushed(CLI.mode === "help" ? 0 : 2);
} else if (CLI.mode === "headless") {
  prepareHeadless();
  void app.whenReady().then(() =>
    runHeadless(CLI, {
      rendererDir: RENDERER_DIR,
      startBackend: (origins, onLog) => startBackend(backendOptions(origins, onLog)),
    }),
  );
} else {
  app.whenReady().then(() => {
    serveRenderer();
    registerIpc();
    createWindow();
    void launchBackend();
  });
}

app.on("window-all-closed", () => app.quit());

let quitting = false;
app.on("before-quit", (event) => {
  if (quitting || !backend) return;
  event.preventDefault();
  quitting = true;
  void backend.stop().finally(() => app.quit());
});
