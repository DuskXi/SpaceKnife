// End-to-end test of 「无缝更新」 for a portable copy. Was packaging/update_e2e.py portable (Electron).
//
//   node packaging/update-e2e.mjs target/portable/SpaceKnife-<v>-win-x64-portable.zip
//   xvfb-run -a node packaging/update-e2e.mjs target/portable/SpaceKnife-<v>-linux-x64-tauri.tar.gz          (the Tauri app)
//   xvfb-run -a node packaging/update-e2e.mjs target/electron-shell/portable/SpaceKnife-<v>-linux-x64-portable.tar.gz   (the Electron shell)
//   node packaging/update-e2e.mjs target/portable/SpaceKnife-<v>-mac-arm64-portable.zip
//
// A release list on 127.0.0.1 (SPACEKNIFE_UPDATE_FEED) offers the next patch version, whose file is
// this build again (renamed, with a marker file added) with its SHA256SUMS. On Linux, where both
// shells have a portable copy, the release also has the other shell's archive under its name (a
// stand-in that is no archive at all): each copy must update to its own kind only (crates/sk-core
// update asset_templates). The unpacked copy is started minimized; through its backend (the
// WebSocket, as the window talks to it) the test checks that the update is found under the
// release's file name with its checksum, downloads it and waits until it is verified. Then it does
// what the window's 「无缝更新」 button does (src-tauri install.rs replace_portable; the Electron
// shell's index.ts replacePortable): unpacks the file into the updates folder, writes the swap plan
// and starts the new version's swap program with --apply-update (the Tauri app's own executable;
// the Electron shell's spaceknife-cli). First with a file and another app of the user's next to the
// program: the swap moves only SpaceKnife's own files, so it answers {"ok": false} naming them, and
// nothing moves, the app runs on; only the .asar files of the unpacked version are gone (an Electron
// app, 0.3.4 among them, can't remove a folder holding one), and the next attempt unpacks it again,
// as the shells do. Then without them, and with a file the system puts in folders
// (desktop.ini, .DS_Store, .directory), which must stay where it is: it answers {"ok": true}; the app is closed, the new
// program swaps the files (sk_core::update::swap) and starts the app again with --updated. The test
// checks that the new files are in place, the data folder stayed, the app came back (a new backend)
// and that the old files are cleared out of the updates folder (the new version's files stay there:
// this build calls itself the old version).
//
// The window's own part (the button, the unpacking and plan code) is covered by install.rs's unit
// tests; the system WebView offers no DevTools port on Linux and macOS to click it. Exit status 1
// on failure, after everything it started (and whatever runs from its folders) is ended. It starts
// SpaceKnife windows (minimized), so run it on a CI runner or a test machine.

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import http from "node:http";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";

import { appProcessOf, asarFilesIn, cliIn, fail, hello, helperIn, isElectron, pack, pidAlive, programIn, sha256, sleep, stop, stopUnder, unpack, until } from "./lib.mjs";
import { spawnSync } from "node:child_process";

/** A portable archive's name: version, target, kind (`portable`: 0.3.4's name; `tauri`: the Tauri
 * app's Linux copy), extension. */
const NAME = /^SpaceKnife-(\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?)-((?:win|linux|mac)-(?:x64|arm64))-(portable|tauri)\.(zip|tar\.gz)$/;
const archive = resolve(process.argv[2] ?? "");
const m = NAME.exec(basename(archive));
if (!m || !existsSync(archive) || (m[3] === "tauri" && !m[2].startsWith("linux-"))) fail("usage: node packaging/update-e2e.mjs <portable archive>");
const [, version, target, kind, ext] = m;
const [maj, min, patch] = version.split("-")[0].split(".").map(Number);
const next = `${maj}.${min}.${patch + 1}`;
const step = (text) => console.log(`\n== ${text}`);
/** The checksum list a release publishes an archive of this kind in (tools/release.mjs expected). */
const sumsOf = (k) => `SHA256SUMS-${k}-${target}.txt`;

// the long, final path (Windows: no 8.3 short names, as the system reports the programs it runs)
const work = realpathSync.native(mkdtempSync(join(tmpdir(), "sk-update-e2e-")));
const output = [];
const procs = new Set();
const started = new Set();

function startApp(program, env) {
  const p = spawn(program, [], { env, stdio: ["ignore", "pipe", "pipe"] });
  for (const s of [p.stdout, p.stderr]) s.on("data", (d) => output.push(...String(d).split(/\r?\n/).filter(Boolean)));
  p.exited = null;
  p.on("exit", (code, signal) => (p.exited = { code, signal }));
  procs.add(p);
  return p;
}

async function readyInfo(file, what, timeout = 90_000) {
  return until(what, () => {
    if (!existsSync(file)) return null;
    try {
      return JSON.parse(readFileSync(file, "utf8"));
    } catch {
      return null;
    }
  }, timeout);
}

/** The one folder at the top of an unpacked archive. */
function topFolder(dir) {
  const tops = readdirSync(dir).filter((n) => n !== "__MACOSX");
  if (tops.length !== 1) throw new Error(`${dir} has ${tops.join(", ")} at the top, not one folder`);
  return join(dir, tops[0]);
}

let server;
let log = null;
try {
  step(`${basename(archive)}: unpack, and make ${next} of the same build`);
  mkdirSync(join(work, "install"));
  unpack(archive, join(work, "install"));
  const folder = topFolder(join(work, "install"));
  if (basename(folder) !== `SpaceKnife-${version}-${target}-${kind}`) throw new Error(`the archive's folder is ${basename(folder)}`);
  if (!existsSync(join(folder, "data"))) throw new Error(`${folder} has no data folder`);
  const electron = isElectron(folder);
  console.log(`${electron ? "the Electron shell" : "the Tauri app"}'s portable copy`);
  // what 0.3.x copies run from a new version to swap the files: spaceknife-cli, which takes
  // --apply-update first (without a plan it says so and touches nothing)
  const answer = spawnSync(helperIn(folder), ["--apply-update"], { encoding: "utf8", timeout: 30_000 });
  if (answer.status !== 2 || JSON.parse(answer.stdout.trim()).ok !== false) throw new Error(`${helperIn(folder)} --apply-update: ${answer.status} ${answer.stdout}`);
  console.log(`the swap helper answers: ${answer.stdout.trim()}`);
  // a file of the user's in the data folder, which the swap must leave where it is
  const marker = join(folder, "data", "e2e-kept.txt");
  writeFileSync(marker, "kept through the update\n");
  const nextName = `SpaceKnife-${next}-${target}-${kind}`;
  mkdirSync(join(work, "next"));
  unpack(archive, join(work, "next"));
  renameSync(topFolder(join(work, "next")), join(work, "next", nextName));
  writeFileSync(join(work, "next", nextName, "NEW_BUILD.txt"), next);
  mkdirSync(join(work, "feed"));
  const file = `${nextName}.${ext}`;
  pack(join(work, "next"), nextName, join(work, "feed", file));
  const digest = sha256(join(work, "feed", file));
  const sums = sumsOf(kind);
  writeFileSync(join(work, "feed", sums), `${digest}  ${file}\n`);
  const offered = [file, sums];
  // Linux: the other shell's portable copy of the same version, which this copy must not take (a
  // stand-in: no archive, and listed in its own checksum list)
  if (target.startsWith("linux-")) {
    const other = kind === "tauri" ? "portable" : "tauri";
    const decoy = `SpaceKnife-${next}-${target}-${other}.${ext}`;
    writeFileSync(join(work, "feed", decoy), "the other shell's portable copy: not this copy's update\n");
    writeFileSync(join(work, "feed", sumsOf(other)), `${sha256(join(work, "feed", decoy))}  ${decoy}\n`);
    offered.push(decoy, sumsOf(other));
    console.log(`the release also has ${decoy} (the other shell's), which this copy must not take`);
  }

  // the release list, as GitHub's API gives it
  server = http.createServer((req, res) => {
    const base = `http://127.0.0.1:${server.address().port}`;
    if (req.url.startsWith("/releases")) {
      const asset = (name) => ({ name, size: statSync(join(work, "feed", name)).size, browser_download_url: `${base}/files/${name}` });
      const list = [{ tag_name: `v${next}`, name: `SpaceKnife ${next}`, draft: false, prerelease: false, html_url: `${base}/release`, published_at: new Date().toISOString(), body: "test", assets: offered.map(asset) }];
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(list));
    } else if (req.url.startsWith("/files/")) {
      const name = decodeURIComponent(req.url.slice(7));
      if (!offered.includes(name)) return res.writeHead(404).end();
      console.log(`  feed: GET ${req.url}`);
      res.writeHead(200, { "content-length": statSync(join(work, "feed", name)).size });
      res.end(readFileSync(join(work, "feed", name)));
    } else res.writeHead(404).end();
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const feed = `http://127.0.0.1:${server.address().port}/releases`;
  console.log(`release list on ${feed}: ${offered.join(", ")} (${digest.slice(0, 16)}…)`);

  step(`start ${version} and find ${next}`);
  const ready = join(work, "ready.json");
  const env = { ...process.env, SPACEKNIFE_UPDATE_FEED: feed, SPACEKNIFE_READY_FILE: ready, SPACEKNIFE_START_MINIMIZED: "1" };
  delete env.SPACEKNIFE_HOME;
  const app = startApp(programIn(folder), env);
  const info = await readyInfo(ready, "the backend");
  started.add(info.pid);
  const session = await hello(info.url, info.token);
  let status = await session.call("update.check");
  status = await until("the update check", async () => {
    const s = await session.call("update.status");
    return s.checking ? null : s;
  });
  if (status.install_kind !== "portable") throw new Error(`install kind ${status.install_kind}, not portable`);
  if (status.latest?.version !== next) throw new Error(`the newest release seen is ${JSON.stringify(status.latest)} (error: ${status.error})`);
  // the checksum is read when the download starts (verified below)
  if (status.latest.asset?.name !== file || (status.latest.asset.sha256 ?? digest) !== digest) throw new Error(`its file: ${JSON.stringify(status.latest.asset)} (want ${file})`);
  console.log(`update found: ${status.current} -> ${status.latest.version}, ${file}`);

  step("download and verify");
  // the app may have started it by itself (automatic download): then it is followed, not asked again
  await sleep(2000);
  const before = (await session.call("update.status")).download;
  if (before.version !== next || !["downloading", "verifying", "done"].includes(before.state)) await session.call("update.download", { version: next });
  else console.log(`the app is already downloading it (${before.state})`);
  const done = await until("the download", async () => {
    const d = (await session.call("update.status")).download;
    if (d.state === "failed" || d.state === "cancelled") throw new Error(`download ${d.state}: ${d.error}`);
    return d.state === "done" ? d : null;
  }, 120_000);
  if (done.verified !== true || sha256(done.path) !== digest || basename(done.path) !== file) throw new Error(`download: ${JSON.stringify(done)}`);
  console.log(`downloaded and verified: ${done.path}`);
  session.close();

  step("「无缝更新」: unpack, plan, the new version's --apply-update");
  const updates = dirname(done.path);
  const staging = join(updates, `staging-${next}`);
  rmSync(staging, { recursive: true, force: true });
  mkdirSync(staging, { recursive: true });
  unpack(done.path, staging);
  const staged = join(staging, nextName);
  const previous = join(updates, `previous-${version}`);
  const plan = join(updates, `update-${next}.json`);
  log = join(updates, `update-${next}.log`);
  writeFileSync(plan, `${JSON.stringify({ v: 1, pid: app.pid, app: folder, new: staged, old: previous, exe: programIn(folder), log, keep: ["data"] }, null, 2)}\n`);
  rmSync(ready, { force: true });
  // started by the app (install.rs start_swap; the Electron shell's startSwap), it has the app's
  // environment: SPACEKNIFE_HOME set to the copy's data folder, which the swap must not pass on
  const appEnv = { ...env, SPACEKNIFE_HOME: join(folder, "data") };
  // the program the shell starts for it: the Tauri app's own executable, the Electron shell's
  // spaceknife-cli (update.ts swapHelpers)
  const swapper = electron ? cliIn(staged) : programIn(staged);

  // a file and another app of the user's next to SpaceKnife (a folder of tools, ~/Desktop): the
  // swap moves only SpaceKnife's own files, so it does not start at all and nothing moves
  // (sk_core::update::swap sort_names); the answer names them
  const foreign = [join(folder, "e2e-notes.txt"), join(folder, process.platform === "darwin" ? "Other.app" : "OtherTool")];
  writeFileSync(foreign[0], "the user's notes\n");
  mkdirSync(join(foreign[1], "Contents"), { recursive: true });
  writeFileSync(join(foreign[1], "Contents", "Info.plist"), "another app\n");
  const programBefore = sha256(programIn(folder));
  const refusal = spawn(swapper, ["--apply-update", plan], { cwd: updates, env: appEnv, stdio: ["ignore", "pipe", "ignore"] });
  const refused = await new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error("--apply-update did not answer the folder with the user's files")), 60_000);
    let line = null;
    createInterface({ input: refusal.stdout }).once("line", (l) => (line = l));
    // "close", not "exit": its output may still be on its way when it has exited
    refusal.once("close", (code) => (clearTimeout(t), resolve({ code, line })));
  });
  const refusedSaid = JSON.parse(refused.line ?? "{}");
  if (refused.code !== 2 || refusedSaid.ok !== false || !refusedSaid.error?.includes("e2e-notes.txt") || !refusedSaid.error.includes(basename(foreign[1])))
    throw new Error(`with the user's files next to it --apply-update said ${refused.line} (exit ${refused.code})`);
  const refusedLog = existsSync(log) ? readFileSync(log, "utf8") : "";
  if (!refusedLog.includes("holds what is not SpaceKnife's")) throw new Error(`the swap's log does not say why it did not start:\n${refusedLog}`);
  if (existsSync(previous) || existsSync(join(folder, "NEW_BUILD.txt")) || sha256(programIn(folder)) !== programBefore) throw new Error("the refused swap moved files");
  if (!existsSync(foreign[0]) || !existsSync(join(foreign[1], "Contents", "Info.plist"))) throw new Error("the user's files next to the app did not stay");
  if (app.exited) throw new Error(`the app quit: ${JSON.stringify(app.exited)}`);
  console.log(`with the user's files next to it nothing moved: ${refusedSaid.error}`);
  // the unpacked version is left so that any app can remove it before it tries again: the refusal
  // took its .asar files out, and nothing else (sk_core::update::swap leave_removable) — Electron's
  // fs takes one for a folder, and 0.3.4, an Electron app, removes its staging folder that way
  const asarLeft = asarFilesIn(staging);
  if (asarLeft.length) throw new Error(`the refused swap left ${asarLeft.join(", ")}: an Electron app can't remove ${staging}`);
  if (electron && !refusedLog.includes("an Electron app can't remove a folder that holds one")) throw new Error(`the swap's log does not say it took the .asar files out:\n${refusedLog}`);
  if (!existsSync(programIn(staged)) || !existsSync(join(staged, "NEW_BUILD.txt"))) throw new Error(`the refused swap took more than the .asar files out of ${staged}`);
  if (electron) console.log(`the unpacked version's .asar files are gone (${staging} can be removed by an Electron app), the rest is there`);
  // the next attempt starts over, as both shells and 0.3.4 do: the staging folder removed, the
  // update unpacked again
  rmSync(staging, { recursive: true, force: true });
  mkdirSync(staging, { recursive: true });
  unpack(done.path, staging);
  // without them it goes ahead; a file the system put there stays where it is
  for (const f of foreign) rmSync(f, { recursive: true, force: true });
  const systemFile = join(folder, process.platform === "win32" ? "desktop.ini" : process.platform === "darwin" ? ".DS_Store" : ".directory");
  writeFileSync(systemFile, "the system's\n");

  const helper = spawn(swapper, ["--apply-update", plan], { cwd: updates, env: appEnv, detached: true, stdio: ["ignore", "pipe", "ignore"] });
  const said = await new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error("--apply-update did not answer")), 30_000);
    createInterface({ input: helper.stdout }).once("line", (line) => (clearTimeout(t), resolve(line)));
    helper.once("exit", (code) => (clearTimeout(t), reject(new Error(`--apply-update exited with ${code}`))));
  });
  if (JSON.parse(said).ok !== true) throw new Error(`--apply-update said ${said}`);
  helper.stdout.destroy();
  helper.unref();
  console.log(`${basename(swapper)} --apply-update: ${said.trim()}`);
  stop(app.pid);
  await until("the app to quit", () => app.exited, 20_000).catch(() => (stop(app.pid, true), until("the app to quit", () => app.exited, 15_000)));
  console.log(`the app quit: ${JSON.stringify(app.exited)}`);

  step(`${next} comes back`);
  const again = await readyInfo(ready, "the updated app's backend", 120_000);
  started.add(again.pid);
  started.add(appProcessOf(again.pid));
  if (again.pid === info.pid) throw new Error("the same backend answered");
  if (!existsSync(join(folder, "NEW_BUILD.txt"))) throw new Error(`the new files are not in ${folder}`);
  if (!existsSync(marker)) throw new Error("the data folder did not stay");
  if (!existsSync(cliIn(folder))) throw new Error("spaceknife-cli is missing after the swap");
  if (!existsSync(systemFile) || readFileSync(systemFile, "utf8") !== "the system's\n") throw new Error(`${basename(systemFile)} did not stay where it was`);
  console.log(`new files in place, data and ${basename(systemFile)} kept; the app runs again (backend pid ${again.pid})`);
  const s2 = await hello(again.url, again.token);
  // a portable copy still: the next update finds its file (sk_core::update::swap::app_env)
  const kindNow = (await s2.call("update.status")).install_kind;
  if (kindNow !== "portable") throw new Error(`after the swap the app takes itself for ${kindNow}, not a portable copy`);
  // clean_updates: everything of the running version or an older one (the copy says it is still
  // the old version: it is the same build; the staged newer files stay)
  await until("the old files to be cleared from the updates folder", () => !existsSync(previous), 60_000).catch(() => {
    throw new Error(`still in ${updates}: ${readdirSync(updates).join(", ")}`);
  });
  console.log(`updates folder: ${readdirSync(updates).join(", ") || "(empty)"}`);
  s2.close();
  const appNow = appProcessOf(again.pid);
  stop(appNow);
  await until("the updated app to quit", () => !pidAlive(appNow) && !pidAlive(again.pid), 20_000).catch(() => {
    stop(appNow, true);
    stop(again.pid, true);
  });
  console.log("\nportable update: ok");
} catch (e) {
  // what the swap said (it is in the updates folder until the new version clears it)
  if (log && existsSync(log)) console.error(`---- the swap's log (${log})\n${readFileSync(log, "utf8").trimEnd()}\n----`);
  await cleanup();
  fail(e.message, output);
}
await cleanup();
process.exit(0);

/** End what the test started and whatever still runs from its folder; remove the folder. */
async function cleanup() {
  server?.close();
  for (const p of procs) if (!p.exited) stop(p.pid, true);
  for (const pid of started) if (pidAlive(pid)) stop(pid, true);
  const left = await stopUnder([work]);
  if (left.length) console.log(`ended what still ran from ${work}: ${left.map((p) => `${p.pid} ${p.cmd}`).join("; ")}`);
  await sleep(1000);
  if (!process.env.SK_KEEP) rmSync(work, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
}
