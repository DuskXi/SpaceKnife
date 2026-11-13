// Smoke test of a packaged app: start it, reach the backend it runs, list the drives, close it.
// Was packaging/smoke_app.py (Electron, through the DevTools port).
//
//   node packaging/smoke-app.mjs <program>                    an installed or unpacked app
//   node packaging/smoke-app.mjs --portable <unpacked portable folder>/SpaceKnife.exe   (Linux: …/spaceknife)
//   node packaging/smoke-app.mjs --portable <unpacked macOS archive>/SpaceKnife.app/Contents/MacOS/SpaceKnife
//   node packaging/smoke-app.mjs SpaceKnife_<v>_amd64.AppImage  (extracted and run: no FUSE needed)
//
// The app starts minimized (SPACEKNIFE_START_MINIMIZED) with an isolated home (a temporary folder;
// with --portable the data folder next to the program, which the app has to find by itself) and
// writes its backend's address and token to SPACEKNIFE_READY_FILE (the Tauri shell's backend in
// src-tauri/src/backend.rs, the Electron shell's spaceknife-cli serve --ready-file). The test waits
// for that, says session.hello with the token and lists the drives (volume.list). It changes 「主题」
// to dark, light and back to 「跟随系统」 through the settings: the page hands each to the shell, whose
// theme.json must follow (it is written once the window took the theme; a shell that hangs on the
// way never writes it). Then it closes the app the way the system does
// (WM_CLOSE / SIGTERM) and checks that it exits by itself and its address closes. With --kind (implied by
// --portable) the app must take itself for that kind of install (update.status). With --portable the data
// folder must hold what the app wrote. The window itself is not looked at: the system WebView has
// no DevTools port on Linux and macOS. On Linux run it under xvfb-run. Exit status 1 on failure.

import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";

import { appProcessOf, askToClose, fail, hello, pidAlive, portOpen, sleep, stop, until } from "./lib.mjs";

const args = process.argv.slice(2);
const portable = args.includes("--portable");
const kindAt = args.indexOf("--kind");
const wantKind = kindAt >= 0 ? args[kindAt + 1] : portable ? "portable" : null;
const target = args.find((a, i) => a !== "--portable" && a !== "--kind" && (kindAt < 0 || i !== kindAt + 1));
if (!target) fail("usage: node packaging/smoke-app.mjs [--portable] [--kind nsis|appimage|deb|rpm|dmg] <program>");
const app = resolve(target);

/** The portable data folder of a program: next to it, on macOS next to its app bundle. */
function dataFolder(program) {
  const macos = dirname(program);
  const bundle = dirname(dirname(macos));
  if (basename(macos) === "MacOS" && bundle.endsWith(".app")) return join(dirname(bundle), "data");
  return join(macos, "data");
}

/** The theme the shell keeps in its home: the Tauri app's desktop/theme.json (window.rs), the
 * Electron shell's electron/theme.json (its userData); null while neither says one. */
function shellTheme(home) {
  for (const file of [join(home, "desktop", "theme.json"), join(home, "electron", "theme.json")]) {
    try {
      const theme = JSON.parse(readFileSync(file, "utf8"))?.theme;
      if (typeof theme === "string") return theme;
    } catch {
      // not there, or being replaced
    }
  }
  return null;
}

async function smoke(home) {
  const scratch = mkdtempSync(join(tmpdir(), "sk-app-smoke-"));
  const ready = join(scratch, "ready.json");
  const env = { ...process.env, SPACEKNIFE_START_MINIMIZED: "1", SPACEKNIFE_READY_FILE: ready };
  if (portable) delete env.SPACEKNIFE_HOME; // the app finds <its folder>/data by itself
  else env.SPACEKNIFE_HOME = home;
  if (app.endsWith(".AppImage")) env.APPIMAGE_EXTRACT_AND_RUN = "1";
  const output = [];
  const proc = spawn(app, [], { env, stdio: ["ignore", "pipe", "pipe"] });
  for (const s of [proc.stdout, proc.stderr]) s.on("data", (d) => output.push(...String(d).split(/\r?\n/).filter(Boolean)));
  let exited = null;
  proc.on("exit", (code, signal) => (exited = { code, signal }));
  let failure = null;
  try {
    const info = await until("the backend's ready file", () => {
      if (exited) throw new Error(`the app exited early: ${JSON.stringify(exited)}`);
      if (!existsSync(ready)) return null;
      try {
        return JSON.parse(readFileSync(ready, "utf8"));
      } catch {
        return null; // still being written
      }
    });
    const port = Number(new URL(info.url).port);
    console.log(`backend ready: ${info.url} (pid ${info.pid}, the app is ${proc.pid})`);
    const session = await hello(info.url, info.token);
    console.log(`session.hello: ${session.server.name} ${session.server.version}`);
    const { volumes } = await session.call("volume.list");
    if (!volumes?.length) throw new Error("volume.list lists no drive");
    console.log(`volume.list: ${volumes.length} drive(s)`);
    // what kind of install the app takes itself for (install.rs): the file its updates look for
    const { install_kind: kind } = await session.call("update.status");
    console.log(`install kind: ${kind}`);
    if (wantKind && kind !== wantKind) throw new Error(`the app takes itself for a ${kind} install, not ${wantKind}`);
    // 「主题」 (UI-W7-31): the page applies the setting and hands it to the shell (set_theme), which
    // changes the window's theme and then keeps the choice in theme.json. 0.3.5's Tauri app on
    // Windows deadlocked there at the first change that changed the window's colours, and at every
    // start after it. A page that connects late takes the setting when it does.
    for (const theme of ["dark", "light", "system"]) {
      await session.call("settings.update", { values: { "appearance.theme": theme } });
      await until(`the window to take the theme "${theme}"`, () => shellTheme(home) === theme, 60_000).catch((e) => {
        throw new Error(`${e.message}: the shell's theme.json says ${JSON.stringify(shellTheme(home))} (the shell hung on the way?)`);
      });
    }
    console.log("theme: dark, light, system: the shell took each");
    session.close();
    // an AppImage started by its runtime: the program is the runtime's child, and SIGTERM to the
    // runtime would not reach it; the runtime exits with it
    const program = app.endsWith(".AppImage") ? appProcessOf(info.pid) : proc.pid;
    // asked as the system asks (WM_CLOSE to its windows, as the installer closing a running
    // SpaceKnife sends it, spaceknife.nsh SkPostClose; SIGTERM): it must go by itself. On Windows
    // 0.3.5 and 0.3.6 did not, as the WM_CLOSE reached tao's event-loop window too (window.rs
    // keep_event_target), and this test, which asked with taskkill, ended them without a word
    console.log(`asked to close (${askToClose(program)} window(s) or a signal)`);
    const closed = await until("the app to exit", () => exited, 15_000).then(
      () => true,
      () => false,
    );
    if (!closed) {
      console.log("the app did not close on request: ending it");
      stop(program, true);
      if (program !== proc.pid) stop(proc.pid, true);
      await until("the app to exit", () => exited, 15_000);
      throw new Error("the app did not close on request (WM_CLOSE / SIGTERM): it had to be ended");
    }
    console.log(`app exited: ${JSON.stringify(exited)}`);
    await until("the backend's address to close", async () => !(await portOpen(port)) && !pidAlive(info.pid), 15_000);
    console.log("the backend stopped with the app");
  } catch (e) {
    failure = e;
  } finally {
    if (!exited) stop(proc.pid, true);
    await sleep(300);
    rmSync(scratch, { recursive: true, force: true, maxRetries: 10, retryDelay: 300 });
  }
  // once the scratch folder is gone (fail ends the process, which skips a finally)
  if (failure) fail(failure.message, output);
}

if (portable) {
  const home = dataFolder(app);
  if (!existsSync(home)) fail(`no portable data folder at ${home}`);
  await smoke(home);
  const stored = readdirSync(home).sort();
  console.log(`portable data folder holds: ${stored.join(", ")}`);
  // the shell's own files are the first it writes: the Tauri app's (window.rs: theme, the webview's
  // profile) in desktop/, the Electron shell's (Chromium's profile, the theme) in electron/
  if (!stored.includes("desktop") && !stored.includes("electron")) fail("the app did not keep its data in the portable folder");
} else {
  const home = mkdtempSync(join(tmpdir(), "sk-app-home-"));
  // also when the smoke test fails: fail ends the process
  process.once("exit", () => rmSync(home, { recursive: true, force: true, maxRetries: 10, retryDelay: 300 }));
  await smoke(home);
}
console.log("packaged app: ok");
