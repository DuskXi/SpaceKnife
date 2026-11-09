// The Electron shell's main process: one frameless window with the interface (apps/desktop, the Nuxt
// build) served from app://spaceknife, the backend `spaceknife-cli serve` as a child process
// (backend.ts), and the native services behind the IPC channels the page's host adapter calls
// (apps/desktop/app/lib/electronHost.ts, through the preload's window.skElectron). The Tauri shell
// (apps/desktop/src-tauri) does the same with Tauri; the interface is the same build in both.
//
// 0.3.4's headless launch forms (`--headless`, `--listen …`, `--help`, anywhere among the arguments)
// run spaceknife-cli with them, and with this process's terminal, instead of a window (cli.ts).

import { execFileSync, spawn } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { mkdir, readFile, readdir, rename, rm, writeFile } from "node:fs/promises";
import { constants } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

import { BrowserWindow, Menu, app, clipboard, dialog, ipcMain, nativeTheme, protocol, session, shell } from "electron";

import type { BackendState } from "../../../desktop/app/shared/api";
import { WINDOW_BG } from "../../../desktop/app/shared/theme";
import { appImageEnv, nodeFiles as appImageFiles, carriedSwitches, placeAppImage, startupCheck } from "./appimage";
import { type Backend, type BackendOptions, backendEnv, cliPath, startBackend, takeReadyFile } from "./backend";
import { headlessArgs, isStartupCheck, startupCheckAnswer } from "./cli";
import { contentSecurityPolicy, inlineScriptHashes } from "./csp";
import { fileIn, mimeOf } from "./files";
import { appBundle, portableHome, untranslocate } from "./portable";
import { terminalCommands } from "./terminal";
import { type Lang, mt, setLang } from "./text";
import { ThemeKeeper } from "./theme";
import { type InstallHow, installKind, installRefusal, installStep, MAC_APP, nsisUpdateArgs, portableVersion, projectPage, swapHelpers, swapPlanJson } from "./update";

// The swap helper's question before it puts this version of a portable copy in place
// (cli.ts STARTUP_CHECK): the libraries loaded, so the answer is yes; then the end. Answered
// before any other line of this file runs: the version asked is still unpacked in the updates
// folder, where its portable data folder, a Chromium profile (data/electron) or the app:// scheme
// do not belong. The module's evaluation stops here (an await that never settles), so nothing
// below runs; `end` quits.
if (isStartupCheck(process.argv.slice(1))) {
  const end = () => app.exit(0);
  setTimeout(end, 2000); // the answer may not be read
  process.stdout.write(startupCheckAnswer(process.versions), end);
  await new Promise<never>(() => {});
}

// SPACEKNIFE_READY_FILE (end-to-end tests, demos, the swap watching this new version): out of this
// process's environment before anything starts, so that nothing the app starts inherits it (a
// backend started again later, an update's helper, a terminal); the first backend writes it
// (backend.ts takeReadyFile)
let readyFile = takeReadyFile(process.env);

const here = dirname(fileURLToPath(import.meta.url));
const APP_HOST = "spaceknife";
const APP_ORIGIN = `app://${APP_HOST}`; // as 0.3.4: the page's storage stays where it was
const APP_ID = "io.github.duskxi.spaceknife"; // electron-builder.yml appId: taskbar grouping, shortcuts
// --headless / --listen / --help: no window, spaceknife-cli's headless mode in this terminal (cli.ts)
const HEADLESS = headlessArgs(process.argv.slice(1));
// a checkout only: the interface from Nuxt's dev server (`pnpm dev` in apps/desktop)
const DEV_URL = !app.isPackaged && process.env.SPACEKNIFE_DEV_URL ? new URL(process.env.SPACEKNIFE_DEV_URL) : null;

/** macOS: where a translocated copy really is (portable.ts), from the mount table; null when unknown. */
function originalPath(path: string): string | null {
  try {
    return untranslocate(path, execFileSync("/sbin/mount", { encoding: "utf8", timeout: 3000 }));
  } catch {
    return null;
  }
}

// A portable copy keeps its data next to the executable: the backend inherits SPACEKNIFE_HOME.
const portable = portableHome(process.execPath, process.platform, app.isPackaged, process.env, originalPath);
if (portable) process.env.SPACEKNIFE_HOME = portable;
// what the backend's update check downloads, and what main may start once it is downloaded
const INSTALL_KIND = installKind(process.execPath, process.platform, app.isPackaged, process.env, portable);
// started by 「无缝更新」 (the Windows installer passes it after a silent update): the page says so once
const JUST_UPDATED = process.argv.includes("--updated");

// The Chromium profile (storage, caches) lives in an "electron" folder of SpaceKnife's own, as in
// 0.3.4 (nothing of the page's is lost on the way): an isolated home (demos, tests, portable copies)
// gets its own; otherwise <appData>/SpaceKnife/electron, apart from the backend's files (on Linux
// the backend's config folder is ~/.config/SpaceKnife as well).
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

/** The repository of a checkout (SPACEKNIFE_REPO, else two folders above this app). */
function repoRoot(): string {
  return process.env.SPACEKNIFE_REPO ? resolve(process.env.SPACEKNIFE_REPO) : resolve(app.getAppPath(), "../..");
}

/** The built interface: resources/ui of an installed app (electron-builder.yml extraResources, where
 *  `spaceknife-cli headless` finds it too); apps/desktop/.output/public in a checkout; SPACEKNIFE_UI. */
function uiDir(): string {
  if (process.env.SPACEKNIFE_UI) return resolve(process.env.SPACEKNIFE_UI);
  if (app.isPackaged) return join(process.resourcesPath, "ui");
  return join(repoRoot(), "apps", "desktop", ".output", "public");
}

const cli = () => cliPath(process.execPath, process.platform, app.isPackaged, process.env, repoRoot());

/** The system's interface language as Electron sees it ("" when it can't tell). */
function systemLocale(): string {
  try {
    return app.getPreferredSystemLanguages()[0] ?? app.getLocale() ?? "";
  } catch {
    return "";
  }
}

function backendOptions(allowOrigins: string[], onLog: (line: string) => void): BackendOptions {
  return {
    cli: cli(),
    allowOrigins,
    home: process.env.SPACEKNIFE_HOME,
    locale: systemLocale(),
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
  const origins = DEV_URL ? [DEV_URL.origin] : [];
  setBackend({ state: "starting" });
  // the ready file goes to the first backend only (takeReadyFile)
  const ready = readyFile;
  readyFile = undefined;
  try {
    backend = await startBackend({ ...backendOptions(origins, (line) => console.log(`[backend] ${line}`)), readyFile: ready });
    backend.proc.once("exit", (code) => setBackend({ state: "exited", code }));
    setBackend({ state: "ready", info: backend.info });
  } catch (err) {
    setBackend({ state: "failed", message: err instanceof Error ? err.message : String(err) });
  }
}

// The theme (W7 外观 · 主题, theme.ts): the last choice is in <userData>/theme.json, restored before the
// window is created.
const themeFile = () => join(app.getPath("userData"), "theme.json");
let themes: ThemeKeeper | null = null;

function restoreTheme(): ThemeKeeper {
  let text: string | null = null;
  try {
    text = readFileSync(themeFile(), "utf8");
  } catch {
    // never chosen: 「跟随系统」
  }
  return new ThemeKeeper(
    {
      native: nativeTheme,
      setBackground: (color) => win?.setBackgroundColor(color),
      write: async (text) => {
        const file = themeFile();
        await mkdir(dirname(file), { recursive: true });
        // whole or not at all: a start never reads half a file
        await writeFile(`${file}.tmp`, text);
        await rename(`${file}.tmp`, file);
      },
      warn: (message) => console.warn(message),
    },
    text,
  );
}

// (restored before any window: light, as where nothing tells, is only a fallback)
const windowBackground = () => themes?.background() ?? WINDOW_BG.light;

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
    backgroundColor: windowBackground(),
    title: "SpaceKnife",
    icon: windowIcon(),
    webPreferences: {
      preload: join(here, "../preload/index.mjs"),
      contextIsolation: true,
      sandbox: false,
      spellcheck: false,
      // window.skElectron.version and .justUpdated (preload)
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
  const origin = DEV_URL ? DEV_URL.origin : APP_ORIGIN;
  win.webContents.on("will-navigate", (event, url) => {
    if (!url.startsWith(`${origin}/`)) event.preventDefault();
  });
  win.webContents.on("will-attach-webview", (event) => event.preventDefault());
  if (DEV_URL) void win.loadURL(DEV_URL.href);
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
  ipcMain.on("sk:showItemInFolder", (_e, path: unknown) => {
    if (typeof path === "string") shell.showItemInFolder(path);
  });
  ipcMain.handle("sk:openPath", (_e, path: unknown) => (typeof path === "string" ? shell.openPath(path) : ""));
  ipcMain.on("sk:copyText", (_e, text: unknown) => clipboard.writeText(String(text)));
  ipcMain.on("sk:setLanguage", (_e, lang: Lang) => setLang(lang));
  // the page applies the setting to itself; here the window, Electron's UI and the file
  ipcMain.on("sk:setTheme", (_e, theme: unknown) => themes?.set(theme));
  // the system's theme changed (or themeSource did): the window's own background follows
  nativeTheme.on("updated", () => themes?.updated());
  ipcMain.on("sk:openTerminal", (_e, dir: unknown) => openTerminal(dir));
  // one fixed settings pane, not a URL from the page (spaceknife.privacy.FULL_DISK_ACCESS_SETTINGS)
  ipcMain.on("sk:openFullDiskAccess", () => {
    if (process.platform === "darwin") void shell.openExternal("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles");
  });
  ipcMain.on("sk:openExternal", (_e, url: unknown) => {
    const page = projectPage(url);
    if (page) void shell.openExternal(page);
    else console.warn(`[shell] not opening ${String(url)}: not a page of the project (GitHub, the website)`);
  });
  ipcMain.handle("sk:installUpdate", (_e, path: unknown, how: unknown) => installUpdate(String(path), how === "classic" ? "classic" : "seamless"));
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

/** One of the update card's two buttons with the file the backend downloaded (update.download):
 *  「无缝更新」 or the classic way (shared/install.ts installStep). Only ever from a click in the card,
 *  never by itself. "" when it is under way, else why not (and the app goes on as it was). */
async function installUpdate(file: string, how: InstallHow): Promise<string> {
  const updates = backend?.updatesDir ?? null;
  const refusal = updates ? installRefusal(file, INSTALL_KIND, updates, process.platform) : "updateMissing";
  if (refusal) {
    console.warn(`[update] not using ${file}: ${refusal}`);
    return mt(refusal);
  }
  const step = installStep(INSTALL_KIND, how);
  console.info(`[update] ${how} (${step ?? "nothing"}) with ${file}`);
  try {
    switch (step) {
      case "silent":
        // no wizard, the data stays, the installer starts the new version (update.ts nsisUpdateArgs)
        return await restartInto(() => launch(file, nsisUpdateArgs(process.execPath, process.env)));
      case "wizard":
        // the installer with its pages: it upgrades in place (the old uninstaller runs with
        // --updated, the data stays); SpaceKnife is gone before it gets that far
        return await restartInto(() => launch(file, []));
      case "swap":
        return await replacePortable(file, updates!);
      case "replace":
        return await replaceAppImage(file);
      case "open": {
        // a disk image mounts (the user drags the new app over the old one), a package opens in the
        // system's installer: either replaces files of this app, so it quits once the file is open
        const error = await shell.openPath(file);
        if (error) return mt("updateStartFailed", { error });
        setTimeout(() => app.quit(), 500);
        return "";
      }
      case "reveal":
        shell.showItemInFolder(file); // unpacked or put in place by hand: SpaceKnife keeps running
        return "";
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
    console.warn(`[update] could not start the update: ${String(err)}`);
    quitting = false;
    void launchBackend();
    return mt("updateStartFailed", { error: err instanceof Error ? err.message : String(err) });
  }
  app.exit(0);
  return "";
}

/** Start a program on its own (it outlives the app); resolves once it runs. `env`: this process's
 *  when not given. */
function launch(file: string, args: string[], env?: NodeJS.ProcessEnv): Promise<void> {
  return new Promise((done, fail) => {
    const child = spawn(file, args, { detached: true, stdio: "ignore", windowsHide: true, env });
    child.once("error", fail);
    child.once("spawn", () => {
      child.unref();
      done();
    });
  });
}

/** An AppImage (appimage.ts): the new file is copied next to $APPIMAGE and has to answer
 *  --startup-check with the environment and switches its start gets; then it takes the old one's
 *  place (a rename: the running app keeps the old file open), the old file stays as
 *  $APPIMAGE.previous, and the new one starts. Needs a folder the user can write to. */
async function replaceAppImage(file: string): Promise<string> {
  const target = process.env.APPIMAGE;
  if (!target) return mt("updateNoInstall");
  const env = appImageEnv(process.env);
  const switches = carriedSwitches(process.argv.slice(1));
  const placed = await placeAppImage(file, target, appImageFiles, (program) => startupCheck(program, switches, env));
  if (!placed.ok) {
    console.warn(`[update] ${basename(file)} does not take the place of ${target}: ${placed.error}`);
    return mt(placed.key, { error: placed.error });
  }
  console.info(`[update] ${target} is the new version; the old one is ${placed.previous}`);
  return restartInto(async () => {
    try {
      await launch(target, [...switches, "--updated"], env);
    } catch (err) {
      // it could not even be started: the old file goes back (this app goes on, restartInto)
      await rename(placed.previous, target).catch((e: unknown) => console.warn(`[update] ${placed.previous} stays: ${String(e)}`));
      throw err;
    }
  });
}

/** A portable copy: unpack the new version next to the backend's downloads, then the new version's
 *  spaceknife-cli swaps the files once the app has quit (sk_core::update::swap) and starts it again.
 *  No script: SpaceKnife's own program does it. The "data" folder stays. */
async function replacePortable(file: string, updates: string): Promise<string> {
  const version = portableVersion(basename(file));
  const platform = process.platform;
  if (!version || !portable || (platform !== "win32" && platform !== "linux" && platform !== "darwin")) return mt("updateNoInstall");
  const staging = join(updates, `staging-${version}`);
  try {
    await rm(staging, { recursive: true, force: true });
    await mkdir(staging, { recursive: true });
    await unpack(file, staging);
  } catch (err) {
    console.warn(`[update] could not unpack ${file}: ${String(err)}`);
    return mt("updateUnpackFailed", { error: err instanceof Error ? err.message : String(err) });
  }
  const top = (await readdir(staging, { withFileTypes: true })).filter((e) => e.isDirectory());
  const staged = top.length === 1 ? join(staging, top[0]!.name) : null;
  // the new version's program: the executable, on macOS the app bundle
  const program = platform === "darwin" ? MAC_APP : basename(process.execPath);
  if (!staged || !(await readdir(staged)).includes(program)) {
    const found = top.map((e) => e.name).join(", ") || "nothing";
    console.warn(`[update] ${basename(file)} is not one folder with ${program}: ${found}`);
    return mt("updateUnpackFailed", { error: basename(file) });
  }
  const helper = swapHelpers(staged, platform).find((h) => existsSync(h));
  if (!helper) {
    console.warn(`[update] ${basename(file)} has no ${swapHelpers(staged, platform).join(" nor ")}`);
    return mt("updateUnpackFailed", { error: basename(file) });
  }
  // the copy's folder holds its data folder; on macOS the executable is in the bundle there, which a
  // translocated copy (portable.ts) runs from somewhere else: the swap starts the one in the folder
  const appDir = dirname(portable);
  const bundle = appBundle(process.execPath);
  const exe = platform === "darwin" && bundle ? join(appDir, basename(bundle), "Contents", "MacOS", basename(process.execPath)) : process.execPath;
  const plan = {
    pid: process.pid,
    appDir,
    staged,
    previous: join(updates, `previous-${app.getVersion()}`),
    exe,
    log: join(updates, `update-${version}.log`),
  };
  await rm(plan.previous, { recursive: true, force: true }); // left by an attempt that was undone
  const planFile = join(updates, `update-${version}.json`);
  await writeFile(planFile, swapPlanJson(plan), "utf8");
  console.info(`[update] ${version} unpacked: its spaceknife-cli swaps the files once the app has quit (${planFile})`);
  return restartInto(() => startSwap(helper, planFile, updates));
}

/** Start the new version's program in its --apply-update mode (sk_core::update::swap): detached, so
 *  it outlives the app. Resolves once it says the plan is usable; fails when it says otherwise, exits
 *  or doesn't answer within 30 s (then the app goes on as it was). */
function startSwap(helper: string, plan: string, cwd: string): Promise<void> {
  return new Promise((done, fail) => {
    const child = spawn(helper, ["--apply-update", plan], { cwd, detached: true, stdio: ["ignore", "pipe", "ignore"], windowsHide: true });
    const lines = createInterface({ input: child.stdout! });
    let settled = false;
    const finish = (err?: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      lines.close();
      child.stdout?.destroy();
      if (err) {
        child.kill();
        fail(err);
      } else {
        child.unref();
        done();
      }
    };
    const timer = setTimeout(() => finish(new Error(`${basename(helper)} did not answer`)), 30_000);
    lines.once("line", (line) => {
      let said: { ok?: unknown; error?: unknown } = {};
      try {
        said = JSON.parse(line) as typeof said;
      } catch {
        // not its answer: the error below
      }
      finish(said.ok === true ? undefined : new Error(typeof said.error === "string" ? said.error : line));
    });
    child.once("error", (err) => finish(err));
    child.once("exit", (code) => finish(new Error(`${basename(helper)} exited with ${code}`)));
  });
}

/** Unpack a portable archive (zip on Windows and macOS, tar.gz on Linux) with the system's tools:
 *  Windows 10 1803 and later have bsdtar, which reads zip and refuses entries leaving the folder
 *  (PowerShell's Expand-Archive where it is missing); macOS's ditto keeps an app bundle's symbolic
 *  links and attributes as the archive's own ditto made them. */
function unpack(file: string, into: string): Promise<void> {
  const run = (cmd: string, args: string[]) =>
    new Promise<void>((done, fail) => {
      const child = spawn(cmd, args, { stdio: "ignore", windowsHide: true });
      child.once("error", fail);
      child.once("exit", (code) => (code === 0 ? done() : fail(new Error(`${cmd} exited with ${code}`))));
    });
  if (process.platform === "darwin") return run("/usr/bin/ditto", ["-x", "-k", file, into]);
  if (process.platform !== "win32") return run("tar", ["-xzf", file, "-C", into]);
  return run("tar.exe", ["-xf", file, "-C", into]).catch(() =>
    run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `Expand-Archive -LiteralPath '${file.replace(/'/g, "''")}' -DestinationPath '${into.replace(/'/g, "''")}' -Force`]),
  );
}

/** 「在终端中打开」: the first terminal of terminal.ts that starts, never through a shell. */
function openTerminal(dir: unknown): void {
  if (typeof dir !== "string") return;
  let folder = false;
  try {
    folder = statSync(dir).isDirectory();
  } catch {
    // gone: nothing to open
  }
  const tries = folder ? terminalCommands(process.platform, dir) : [];
  if (!tries.length) {
    console.warn(`[shell] no terminal for ${dir}: ${folder ? "a folder no terminal can open" : "not a folder"}`);
    return;
  }
  const attempt = (i: number): void => {
    const t = tries[i];
    if (!t) {
      console.warn(`[shell] no terminal for ${dir}: none of ${tries.map((c) => c.file).join(", ")} could start`);
      return;
    }
    // a terminal is a window of its own: shown, and it outlives the app
    const child = spawn(t.file, t.args, { cwd: t.cwd, detached: true, stdio: "ignore", windowsHide: false });
    child.once("error", (err: NodeJS.ErrnoException) => {
      if (err.code === "ENOENT") attempt(i + 1);
      else console.warn(`[shell] could not start ${t.file}: ${String(err)}`);
    });
    child.once("spawn", () => child.unref());
  };
  attempt(0);
}

/** The interface's files on app://spaceknife, each page with its Content-Security-Policy (csp.ts). */
function serveInterface(dir: string): void {
  protocol.handle("app", async (request) => {
    const url = new URL(request.url);
    const file = url.host === APP_HOST ? fileIn(dir, url.pathname) : null;
    if (!file) return new Response("forbidden", { status: 403 });
    let body: Buffer;
    try {
      body = await readFile(file);
    } catch {
      return new Response("not found", { status: 404 });
    }
    const headers: Record<string, string> = { "content-type": mimeOf(file), "x-content-type-options": "nosniff" };
    if (/\.html?$/i.test(file)) headers["content-security-policy"] = contentSecurityPolicy(inlineScriptHashes(body.toString("utf8")));
    return new Response(new Uint8Array(body), { headers });
  });
}

/** spaceknife-cli with 0.3.4's headless command line and this process's terminal; the app ends with
 *  its exit code. Its texts name this program (argv0), as 0.3.4's did. Ctrl+C
 *  and the signals a service manager sends reach it (it stops the server and says so). */
function runHeadless(args: string[]): void {
  // no GPU, no Dock icon; Chromium's own --headless lets Electron start on a Linux machine without a
  // display (X11 or Wayland), as 0.3.4's headless mode did
  if (!app.commandLine.hasSwitch("headless")) app.commandLine.appendSwitch("headless");
  app.commandLine.appendSwitch("disable-gpu");
  app.disableHardwareAcceleration();
  if (process.platform === "darwin") app.dock?.hide();
  void app.whenReady().then(() => {
    const program = cli();
    const env = backendEnv({ home: process.env.SPACEKNIFE_HOME, locale: systemLocale(), appVersion: app.getVersion(), installKind: INSTALL_KIND }, process.env);
    const child = spawn(program, args, { argv0: process.execPath, stdio: "inherit", env, windowsHide: true });
    child.once("error", (err) => {
      process.stderr.write(`SpaceKnife: ${mt("backendMissing", { path: program })} (${err.message})\n`);
      app.exit(1);
    });
    child.once("exit", (code, signal) => app.exit(code ?? (signal ? 128 + (constants.signals[signal] ?? 0) : 1)));
    for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"] as const) {
      process.on(signal, () => {
        if (child.exitCode === null && child.signalCode === null) child.kill(signal);
      });
    }
  });
}

if (HEADLESS) {
  runHeadless(HEADLESS);
} else {
  void app.whenReady().then(() => {
    // no menu bar and none of Electron's default shortcuts (reload, developer tools) on Windows and
    // Linux, as in the Tauri shell; macOS keeps its application menu (Edit: copy and paste)
    if (process.platform !== "darwin") Menu.setApplicationMenu(null);
    // the page asks for nothing (notifications, camera, location…): nothing is granted
    session.defaultSession.setPermissionRequestHandler((_wc, _permission, callback) => callback(false));
    serveInterface(uiDir());
    themes = restoreTheme();
    registerIpc();
    createWindow();
    void launchBackend();
  });
  app.on("window-all-closed", () => app.quit());
}

let quitting = false;
app.on("before-quit", (event) => {
  if (quitting || !backend) return;
  event.preventDefault();
  quitting = true;
  void backend.stop().finally(() => app.quit());
});
