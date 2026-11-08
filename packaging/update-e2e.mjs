// End-to-end test of 「无缝更新」 for a portable copy. Was packaging/update_e2e.py portable (Electron).
//
//   node packaging/update-e2e.mjs target/portable/SpaceKnife-<v>-win-x64-portable.zip
//   xvfb-run -a node packaging/update-e2e.mjs target/portable/SpaceKnife-<v>-linux-x64-portable.tar.gz
//   node packaging/update-e2e.mjs target/portable/SpaceKnife-<v>-mac-arm64-portable.zip
//
// A release list on 127.0.0.1 (SPACEKNIFE_UPDATE_FEED) offers the next patch version, whose file is
// this build again (renamed, with a marker file added) with its SHA256SUMS. The unpacked copy is
// started minimized; through its backend (the WebSocket, as the window talks to it) the test checks
// that the update is found under the release's file name with its checksum, downloads it and waits
// until it is verified. Then it does what the window's 「无缝更新」 button does (src-tauri
// install.rs replace_portable): unpacks the file into the updates folder, writes the swap plan and
// starts *the new version's* program with --apply-update, which answers {"ok": true}; the app is
// closed, the new program swaps the files (sk_core::update::swap) and starts the app again with
// --updated. The test checks that the new files are in place, the data folder stayed, the app came
// back (a new backend) and that the old files are cleared out of the
// updates folder (the new version's files stay there: this build calls itself the old version).
//
// The window's own part (the button, the unpacking and plan code) is covered by install.rs's unit
// tests; the system WebView offers no DevTools port on Linux and macOS to click it. Exit status 1
// on failure. It starts SpaceKnife windows (minimized), so run it on a CI runner or a test machine.

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import http from "node:http";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";

import { cliIn, fail, hello, pack, pidAlive, programIn, sha256, sleep, stop, unpack, until } from "./lib.mjs";

const NAME = /^SpaceKnife-(\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?)-((?:win|linux|mac)-(?:x64|arm64))-portable\.(zip|tar\.gz)$/;
const archive = resolve(process.argv[2] ?? "");
const m = NAME.exec(basename(archive));
if (!m || !existsSync(archive)) fail("usage: node packaging/update-e2e.mjs <portable archive>");
const [, version, target, ext] = m;
const [maj, min, patch] = version.split("-")[0].split(".").map(Number);
const next = `${maj}.${min}.${patch + 1}`;
const step = (text) => console.log(`\n== ${text}`);

const work = mkdtempSync(join(tmpdir(), "sk-update-e2e-"));
const output = [];
const procs = new Set();

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

let server;
try {
  step(`${basename(archive)}: unpack, and make ${next} of the same build`);
  mkdirSync(join(work, "install"));
  unpack(archive, join(work, "install"));
  const folder = join(work, "install", `SpaceKnife-${version}-${target}-portable`);
  if (!existsSync(join(folder, "data"))) throw new Error(`${folder} has no data folder`);
  const nextName = `SpaceKnife-${next}-${target}-portable`;
  mkdirSync(join(work, "next"));
  unpack(archive, join(work, "next"));
  renameSync(join(work, "next", `SpaceKnife-${version}-${target}-portable`), join(work, "next", nextName));
  writeFileSync(join(work, "next", nextName, "NEW_BUILD.txt"), next);
  mkdirSync(join(work, "feed"));
  const file = `${nextName}.${ext}`;
  pack(join(work, "next"), nextName, join(work, "feed", file));
  const digest = sha256(join(work, "feed", file));
  const sums = `SHA256SUMS-portable-${target}.txt`;
  writeFileSync(join(work, "feed", sums), `${digest}  ${file}\n`);

  // the release list, as GitHub's API gives it
  server = http.createServer((req, res) => {
    const base = `http://127.0.0.1:${server.address().port}`;
    if (req.url.startsWith("/releases")) {
      const asset = (name) => ({ name, size: statSync(join(work, "feed", name)).size, browser_download_url: `${base}/files/${name}` });
      const list = [{ tag_name: `v${next}`, name: `SpaceKnife ${next}`, draft: false, prerelease: false, html_url: `${base}/release`, published_at: new Date().toISOString(), body: "test", assets: [asset(file), asset(sums)] }];
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(list));
    } else if (req.url.startsWith("/files/")) {
      const name = decodeURIComponent(req.url.slice(7));
      if (![file, sums].includes(name)) return res.writeHead(404).end();
      res.writeHead(200, { "content-length": statSync(join(work, "feed", name)).size });
      res.end(readFileSync(join(work, "feed", name)));
    } else res.writeHead(404).end();
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const feed = `http://127.0.0.1:${server.address().port}/releases`;
  console.log(`release list on ${feed}: ${file} (${digest.slice(0, 16)}…)`);

  step(`start ${version} and find ${next}`);
  const ready = join(work, "ready.json");
  const env = { ...process.env, SPACEKNIFE_UPDATE_FEED: feed, SPACEKNIFE_READY_FILE: ready, SPACEKNIFE_START_MINIMIZED: "1" };
  delete env.SPACEKNIFE_HOME;
  const app = startApp(programIn(folder), env);
  const info = await readyInfo(ready, "the backend");
  const session = await hello(info.url, info.token);
  let status = await session.call("update.check");
  status = await until("the update check", async () => {
    const s = await session.call("update.status");
    return s.checking ? null : s;
  });
  if (status.install_kind !== "portable") throw new Error(`install kind ${status.install_kind}, not portable`);
  if (status.latest?.version !== next) throw new Error(`the newest release seen is ${JSON.stringify(status.latest)} (error: ${status.error})`);
  // the checksum is read when the download starts (verified below)
  if (status.latest.asset?.name !== file || (status.latest.asset.sha256 ?? digest) !== digest) throw new Error(`its file: ${JSON.stringify(status.latest.asset)}`);
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
  if (done.verified !== true || sha256(done.path) !== digest) throw new Error(`download: ${JSON.stringify(done)}`);
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
  const log = join(updates, `update-${next}.log`);
  writeFileSync(plan, `${JSON.stringify({ v: 1, pid: app.pid, app: folder, new: staged, old: previous, exe: programIn(folder), log, keep: ["data"] }, null, 2)}\n`);
  rmSync(ready, { force: true });
  const helper = spawn(programIn(staged), ["--apply-update", plan], { cwd: updates, env, detached: true, stdio: ["ignore", "pipe", "ignore"] });
  const said = await new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error("--apply-update did not answer")), 30_000);
    createInterface({ input: helper.stdout }).once("line", (line) => (clearTimeout(t), resolve(line)));
    helper.once("exit", (code) => (clearTimeout(t), reject(new Error(`--apply-update exited with ${code}`))));
  });
  if (JSON.parse(said).ok !== true) throw new Error(`--apply-update said ${said}`);
  helper.stdout.destroy();
  helper.unref();
  console.log(`--apply-update: ${said.trim()}`);
  stop(app.pid);
  await until("the app to quit", () => app.exited, 20_000).catch(() => (stop(app.pid, true), until("the app to quit", () => app.exited, 15_000)));
  console.log(`the app quit: ${JSON.stringify(app.exited)}`);

  step(`${next} comes back`);
  const again = await readyInfo(ready, "the updated app's backend", 120_000);
  if (again.pid === info.pid) throw new Error("the same backend answered");
  if (!existsSync(join(folder, "NEW_BUILD.txt"))) throw new Error(`the new files are not in ${folder}:\n${existsSync(log) ? readFileSync(log, "utf8") : "(no log)"}`);
  if (!existsSync(join(folder, "data", "desktop"))) throw new Error("the data folder did not stay");
  if (!existsSync(cliIn(folder))) throw new Error("spaceknife-cli is missing after the swap");
  console.log(`new files in place, data kept; the app runs again (backend pid ${again.pid})`);
  const s2 = await hello(again.url, again.token);
  // clean_updates: everything of the running version or an older one (the copy says it is still
  // the old version: it is the same build; the staged newer files stay)
  await until("the old files to be cleared from the updates folder", () => !existsSync(previous), 60_000).catch(() => {
    throw new Error(`still in ${updates}: ${readdirSync(updates).join(", ")}`);
  });
  console.log(`updates folder: ${readdirSync(updates).join(", ") || "(empty)"}`);
  s2.close();
  stop(again.pid);
  await until("the updated app to quit", () => !pidAlive(again.pid), 20_000).catch(() => stop(again.pid, true));
  console.log("\nportable update: ok");
} catch (e) {
  fail(e.message, output);
} finally {
  server?.close();
  for (const p of procs) if (!p.exited) stop(p.pid, true);
  await sleep(1000);
  if (!process.env.SK_KEEP) rmSync(work, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
}
