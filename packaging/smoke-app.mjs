// Smoke test of a packaged app: start it, reach the backend it runs, list the drives, close it.
// Was packaging/smoke_app.py (Electron, through the DevTools port).
//
//   node packaging/smoke-app.mjs <program>                    an installed or unpacked app
//   node packaging/smoke-app.mjs --portable <unpacked portable folder>/spaceknife[.exe]
//   node packaging/smoke-app.mjs --portable <unpacked macOS archive>/SpaceKnife.app/Contents/MacOS/spaceknife
//   node packaging/smoke-app.mjs SpaceKnife_<v>_amd64.AppImage  (extracted and run: no FUSE needed)
//
// The app starts minimized (SPACEKNIFE_START_MINIMIZED) with an isolated home (a temporary folder;
// with --portable the data folder next to the program, which the app has to find by itself) and
// writes its backend's address and token to SPACEKNIFE_READY_FILE (src-tauri/src/backend.rs). The
// test waits for that, checks the backend runs in the app's own process, says session.hello with
// the token and lists the drives (volume.list), then closes the app the way the system does
// (WM_CLOSE / SIGTERM) and checks that it exits and its address closes. With --kind (implied by
// --portable) the app must take itself for that kind of install (update.status). With --portable the data
// folder must hold what the app wrote. The window itself is not looked at: the system WebView has
// no DevTools port on Linux and macOS. On Linux run it under xvfb-run. Exit status 1 on failure.

import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";

import { fail, hello, pidAlive, portOpen, sleep, stop, until } from "./lib.mjs";

const args = process.argv.slice(2);
const portable = args.includes("--portable");
const kindAt = args.indexOf("--kind");
const wantKind = kindAt >= 0 ? args[kindAt + 1] : portable ? "portable" : null;
const target = args.find((a, i) => a !== "--portable" && a !== "--kind" && i !== kindAt + 1);
if (!target) fail("usage: node packaging/smoke-app.mjs [--portable] [--kind nsis|appimage|deb|rpm|dmg] <program>");
const app = resolve(target);

/** The portable data folder of a program: next to it, on macOS next to its app bundle. */
function dataFolder(program) {
  const macos = dirname(program);
  const bundle = dirname(dirname(macos));
  if (basename(macos) === "MacOS" && bundle.endsWith(".app")) return join(dirname(bundle), "data");
  return join(macos, "data");
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
    session.close();
    stop(proc.pid);
    await until("the app to exit", () => exited, 15_000).catch(() => {
      console.log("the app did not close on request: ending it");
      stop(proc.pid, true);
      return until("the app to exit", () => exited, 15_000);
    });
    console.log(`app exited: ${JSON.stringify(exited)}`);
    await until("the backend's address to close", async () => !(await portOpen(port)) && !pidAlive(info.pid), 15_000);
    console.log("the backend stopped with the app");
  } catch (e) {
    fail(e.message, output);
  } finally {
    if (!exited) stop(proc.pid, true);
    await sleep(300);
    rmSync(scratch, { recursive: true, force: true });
  }
}

if (portable) {
  const home = dataFolder(app);
  if (!existsSync(home)) fail(`no portable data folder at ${home}`);
  await smoke(home);
  const stored = readdirSync(home).sort();
  console.log(`portable data folder holds: ${stored.join(", ")}`);
  // the shell's own files (window.rs: theme, the webview's profile) are the first it writes
  if (!stored.includes("desktop")) fail("the app did not keep its data in the portable folder");
} else {
  const home = mkdtempSync(join(tmpdir(), "sk-app-home-"));
  await smoke(home);
  rmSync(home, { recursive: true, force: true });
}
console.log("packaged app: ok");
