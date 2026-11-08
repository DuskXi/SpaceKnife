// End to end: an installed SpaceKnife 0.3.4 (the real files of the v0.3.4 release) updates itself
// to a candidate release, the way users' copies will (docs: the merge plan, 5.6). 0.3.4 is the old
// Electron app with the Python backend; it picks the newest release by version, then its file by
// 0.3.4's names (crates/sk-core update asset_templates keeps them), checks it against the
// release's SHA256SUMS*.txt and installs it its own way. Nothing here helps it along: the test
// serves a release list on 127.0.0.1 (SPACEKNIFE_UPDATE_FEED: v0.3.4 and the candidate, both
// pre-releases, as on GitHub), starts 0.3.4 minimized with its DevTools port on and clicks the
// update badge and 「无缝更新」 in its window like a user; for the packages, whose 0.3.4 card only
// opens the file in the system's installer, the test does that step (apt-get / dnf / ditto).
//
//   node packaging/update-from-034.mjs portable OLD.zip|OLD.tar.gz NEW.zip|NEW.tar.gz   (Windows, Linux under xvfb-run, macOS)
//       [--expect-machine arm64]   (the processor the updated program is built for: an x64 copy
//       on an ARM computer moves to the arm64 build)
//   node packaging/update-from-034.mjs nsis OLD-Setup.exe NEW-Setup.exe [--allusers]   (Windows, CI only)
//   node packaging/update-from-034.mjs appimage OLD.AppImage NEW.AppImage              (Linux under xvfb-run, CI only)
//   node packaging/update-from-034.mjs deb OLD.deb NEW.deb                             (Ubuntu with sudo, CI only)
//   node packaging/update-from-034.mjs rpm OLD.rpm NEW.rpm                             (rockylinux:9 as root, CI only)
//   node packaging/update-from-034.mjs dmg OLD.dmg NEW.dmg                             (macOS, CI only)
//
// Each new file's checksum list is the one the release publishes it in (SHA256SUMS.txt for the
// installers, SHA256SUMS-portable-<target>.txt, SHA256SUMS-mac-<arch>.txt), written here from the
// file itself; --sums DIR takes the release's own lists instead (the Release workflow's assembled
// folder). What every leg checks afterwards: 0.3.4 is gone; the new version runs (its own
// backend, a new process, its ready file) and says it is the candidate's version and the same kind
// of install; the setting changed through 0.3.4's backend and the marker files in the data, config
// and cache folders are still there, byte for byte; nothing of the update is left in the updates
// folder. Then per kind: portable (the swap's log says "updated", or the new version came back
// before it could be read — it removes the log as it starts; the folder holds exactly the
// candidate's files, byte for byte, and the data folder: nothing of 0.3.4 is left, and whatever the
// candidate has — SpaceKnife.com on Windows, the Electron shell's own files in its Linux portable
// copy — is the candidate's), nsis (one uninstall entry, the same folder, the shortcuts, data kept
// on uninstall), appimage (the file is the candidate, no .new), deb / rpm (one package, the old
// /opt/SpaceKnife gone, /usr/bin/spaceknife is the new program, one desktop entry, a clean
// removal), dmg (bundle id, version, executable, signature).
//
// On a failure or a timeout it prints what there is to know (the swap's log as last read, its
// plan, the updates and app folders, the processes still running from the test's folders, the new
// version's own logs), then ends every process it started or that runs from its folders — the swap
// helper and the new version run in sessions of their own — and every leg has a deadline
// (SK_E2E_DEADLINE_S, default 1200), after which the same happens: no step waits for a runner's
// timeout.
//
// It starts windows (minimized) and, for the installers, installs into this user's profile or the
// system: everything but the portable leg runs on a CI runner only. On Windows, run the portable
// leg from packaging/hidden-desktop.ps1 so that no window reaches the screen. Exit status 1 on
// failure; SK_KEEP=1 keeps the work folder.

import { execFileSync, spawn, spawnSync } from "node:child_process";
import { createReadStream, existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import http from "node:http";
import { homedir, tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";

import { MAC, WIN, appProcessOf, fail, hello, helperIn, isElectron, pidAlive, processesUnder, programIn, sha256, sleep, stop, stopUnder, treeDiff, uiIn, unpack, until, watchSwap } from "./lib.mjs";

const OLD = "0.3.4";
const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const option = (name) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : null;
};
const VALUED = ["--sums", "--expect-machine"]; // options that take a value
const [mode, oldFile, newFile] = args.filter((a, i) => !a.startsWith("--") && !VALUED.includes(args[i - 1]));
const MODES = ["portable", "nsis", "appimage", "deb", "rpm", "dmg"];
if (!MODES.includes(mode) || !oldFile || !newFile || !existsSync(oldFile) || !existsSync(newFile)) {
  fail(`usage: node packaging/update-from-034.mjs ${MODES.join("|")} OLD NEW [--sums DIR] [--expect-machine x64|arm64] [--allusers]`);
}
if (mode !== "portable" && process.env.GITHUB_ACTIONS !== "true") fail(`the ${mode} leg installs SpaceKnife on this machine: it runs on a CI runner only`);
const oldPath = resolve(oldFile);
const newPath = resolve(newFile);
const NEW_VERSION = versionOf(basename(newPath));
if (!NEW_VERSION) fail(`not a release file name: ${basename(newPath)}`);
const step = (text) => console.log(`\n== ${new Date().toISOString().slice(11, 19)} ${text}`);
const output = [];
const procs = new Set();
// the new version's processes, started by 0.3.4 or the system, not by this test: ended on the way out
const started = new Set();
// resolved: macOS reports /var/folders/… as /private/var/folders/… in paths it hands out, and Windows
// the long names of RUNNER~1-like short ones (the processes found by path on the way out)
const work = realpathSync.native(mkdtempSync(join(tmpdir(), "sk-from-034-")));
// the folders whose processes are ended on the way out (the work folder; an installer's folder)
const roots = new Set([work]);
// what a failure report shows: the leg's folders as it learns them, the swap's log as last read
const seen = { folder: null, updates: null, data: null, swapLog: "" };

/** The version in a release file name (0.3.4's names). */
function versionOf(name) {
  const v = String.raw`(\d+\.\d+\.\d+(?:[-~][0-9A-Za-z.]+)?)`;
  for (const re of [
    new RegExp(`^SpaceKnife-${v}-(?:win|linux|mac)-(?:x64|arm64)-portable\\.(?:zip|tar\\.gz)$`),
    new RegExp(`^SpaceKnife-${v}-linux-(?:x64|arm64)-tauri\\.tar\\.gz$`), // the Tauri app's Linux portable copy (0.3.5)
    new RegExp(`^SpaceKnife-${v}-Setup-x64\\.exe$`),
    new RegExp(`^SpaceKnife-${v}-x86_64\\.AppImage$`),
    new RegExp(`^spaceknife_${v}_amd64\\.deb$`),
    new RegExp(`^spaceknife-${v}\\.x86_64\\.rpm$`),
    new RegExp(`^SpaceKnife-${v}-mac-(?:x64|arm64)\\.dmg$`),
  ]) {
    const m = re.exec(name);
    if (m) return m[1].replace("~", "-");
  }
  return null;
}

/** The processor a program is built for, from its header (PE, ELF, Mach-O): x64, arm64, or what
 * the header says. */
function machineOf(file) {
  const head = readFileSync(file).subarray(0, 4096);
  if (head[0] === 0x4d && head[1] === 0x5a) {
    const pe = head.readUInt32LE(0x3c);
    return { 0x8664: "x64", 0xaa64: "arm64", 0x14c: "x86" }[head.readUInt16LE(pe + 4)] ?? "unknown";
  }
  if (head.readUInt32BE(0) === 0x7f454c46) return { 62: "x64", 183: "arm64" }[head.readUInt16LE(0x12)] ?? "unknown";
  if (head.readUInt32LE(0) === 0xfeedfacf) return { 0x01000007: "x64", 0x0100000c: "arm64" }[head.readUInt32LE(4)] ?? "unknown";
  if (head.readUInt32BE(0) === 0xcafebabe) return "universal";
  return "unknown";
}

/** The checksum list a release publishes `name` in (tools/release.mjs expected). */
function sumsName(name) {
  let m = /-((?:win|linux)-(?:x64|arm64))-portable\./.exec(name);
  if (m) return `SHA256SUMS-portable-${m[1]}.txt`;
  m = /-(linux-(?:x64|arm64))-tauri\.tar\.gz$/.exec(name);
  if (m) return `SHA256SUMS-tauri-${m[1]}.txt`;
  m = /-mac-(x64|arm64)(?:-portable\.zip|\.dmg)$/.exec(name);
  if (m) return `SHA256SUMS-mac-${m[1]}.txt`;
  return "SHA256SUMS.txt";
}

// ------------------------------------------------------------------ the release list

/** GitHub's release list on 127.0.0.1: v0.3.4 and the candidate, both pre-releases (0.3.4's
 * default channel is preview), the candidate's file and its checksum list served from disk. */
async function serveFeed(file) {
  const files = new Map([[basename(file), file]]);
  const sums = sumsName(basename(file));
  const given = option("--sums");
  const sumsFile = join(work, "feed", sums);
  mkdirSync(dirname(sumsFile), { recursive: true });
  if (given) writeFileSync(sumsFile, readFileSync(join(resolve(given), sums)));
  else writeFileSync(sumsFile, `${sha256(file)}  ${basename(file)}\n`);
  files.set(sums, sumsFile);
  const server = http.createServer((req, res) => {
    const base = `http://127.0.0.1:${server.address().port}`;
    const url = new URL(req.url, base);
    if (url.pathname === "/releases") {
      const release = (version, assets) => ({
        tag_name: `v${version}`,
        name: `SpaceKnife ${version}`,
        draft: false,
        prerelease: true,
        html_url: `${base}/tag/v${version}`,
        published_at: new Date().toISOString(),
        body: "The end-to-end test's release.",
        assets: assets.map((n) => ({ name: n, size: statSync(files.get(n)).size, browser_download_url: `${base}/files/${n}` })),
      });
      const body = JSON.stringify([release(NEW_VERSION, [...files.keys()]), release(OLD, [])]);
      res.writeHead(200, { "content-type": "application/json", "content-length": Buffer.byteLength(body) }).end(body);
      return;
    }
    const path = url.pathname.startsWith("/files/") ? files.get(decodeURIComponent(url.pathname.slice(7))) : null;
    if (!path) return res.writeHead(404).end();
    console.log(`  feed: GET ${url.pathname}`);
    res.writeHead(200, { "content-length": statSync(path).size });
    createReadStream(path).pipe(res);
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const feed = `http://127.0.0.1:${server.address().port}/releases`;
  console.log(`release list on ${feed}: ${[...files.keys()].join(", ")}`);
  return { server, feed };
}

// ------------------------------------------------------------------ processes

function start(program, argv, env) {
  const p = spawn(program, argv, { env, stdio: ["ignore", "pipe", "pipe"] });
  for (const s of [p.stdout, p.stderr]) s.on("data", (d) => output.push(...String(d).split(/\r?\n/).filter(Boolean)));
  p.exited = null;
  p.on("exit", (code, signal) => (p.exited = { code, signal }));
  p.on("error", (e) => (p.exited = { error: String(e) }));
  procs.add(p);
  return p;
}

/** The command line of a process (Windows: CIM; Linux: /proc; macOS: ps). */
function commandLine(pid) {
  if (WIN) {
    const r = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `(Get-CimInstance Win32_Process -Filter "ProcessId=${pid}").CommandLine`], { encoding: "utf8" });
    return (r.stdout ?? "").trim();
  }
  if (MAC) return (spawnSync("ps", ["-o", "command=", "-p", String(pid)], { encoding: "utf8" }).stdout ?? "").trim();
  try {
    return readFileSync(`/proc/${pid}/cmdline`, "utf8").replaceAll("\0", " ").trim();
  } catch {
    return "";
  }
}

/** 0.3.4's Electron app: its DevTools port, and a window that keeps answering while minimized
 * (Chromium otherwise puts the page of a hidden or occluded window to sleep, and a window on a
 * desktop nobody looks at counts as occluded). */
const ELECTRON_ARGS = [
  "--remote-debugging-port=0",
  "--disable-renderer-backgrounding",
  "--disable-backgrounding-occluded-windows",
  "--disable-background-timer-throttling",
  "--disable-features=CalculateNativeWinOcclusion",
];

/** The environment for 0.3.4 and what it starts: the test's release list, minimized, the ready
 * file the new version writes (0.3.4 writes none), never a SPACEKNIFE_HOME of this shell. */
function appEnv(feed, ready, extra = {}) {
  const env = { ...process.env, SPACEKNIFE_UPDATE_FEED: feed, SPACEKNIFE_START_MINIMIZED: "1", SPACEKNIFE_READY_FILE: ready, ...extra };
  delete env.SPACEKNIFE_HOME;
  return env;
}

// ------------------------------------------------------------------ 0.3.4's window (DevTools protocol)

async function devtools(portFile, proc) {
  const port = await until("0.3.4's DevTools port", () => {
    if (proc?.exited) throw new Error(`0.3.4 exited early: ${JSON.stringify(proc.exited)}`);
    try {
      return Number(readFileSync(portFile, "utf8").split(/\s+/)[0]) || null;
    } catch {
      return null;
    }
  }, 120_000);
  let ws = null;
  let ids = 0;
  const pending = new Map();
  async function connect() {
    const target = await until("0.3.4's window", async () => {
      const pages = await fetch(`http://127.0.0.1:${port}/json/list`).then((r) => r.json()).catch(() => []);
      return pages.find((t) => t.type === "page")?.webSocketDebuggerUrl ?? null;
    }, 90_000);
    const socket = new WebSocket(target);
    await new Promise((ok, no) => ((socket.onopen = ok), (socket.onerror = () => no(new Error("the DevTools socket was refused")))));
    socket.onmessage = (m) => {
      const msg = JSON.parse(m.data);
      pending.get(msg.id)?.(msg);
      pending.delete(msg.id);
    };
    socket.onclose = () => {
      for (const done of pending.values()) done({ error: { message: "the DevTools socket closed" } });
      pending.clear();
      if (ws === socket) ws = null;
    };
    ws = socket;
  }
  const send = (method, params, timeout) =>
    new Promise((ok, no) => {
      const id = ++ids;
      const t = setTimeout(() => (pending.delete(id), no(new Error(`no answer to ${method} ${JSON.stringify(params).slice(0, 80)}`))), timeout);
      pending.set(id, (msg) => {
        clearTimeout(t);
        const r = msg.result;
        if (msg.error || r?.exceptionDetails) no(new Error(JSON.stringify(msg.error ?? r.exceptionDetails)));
        else ok(r);
      });
      ws.send(JSON.stringify({ id, method, params }));
    });
  /** `expression`'s value in the window; a window that stopped answering (a reload, a renderer
   * that went to sleep while minimized on a desktop nobody looks at) is woken and asked again. */
  async function evaluate(expression, { tries = 4 } = {}) {
    for (let i = 1; ; i++) {
      try {
        if (!ws) await connect();
        const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }, 15_000);
        return r?.result?.value;
      } catch (e) {
        if (i >= tries) throw e;
        console.log(`  devtools: ${e.message}; again`);
        try {
          if (ws) await send("Page.setWebLifecycleState", { state: "active" }, 5_000);
        } catch {
          try {
            ws?.close();
          } catch {
            // gone
          }
          ws = null;
        }
        await sleep(1000);
      }
    }
  }
  await connect();
  return { evaluate, close: () => ws?.close() };
}

/** Through 0.3.4's own backend (the window's connection info): change a setting, as a user would. */
async function changeSetting(page) {
  const state = await until("0.3.4's backend", () => page.evaluate("window.sk.backend().then(s => s.state === 'starting' ? null : s)"));
  if (state.state !== "ready") throw new Error(`0.3.4's backend: ${JSON.stringify(state)}`);
  console.log(`0.3.4 runs: window ${await page.evaluate("window.sk.version")}, backend ${state.info.url}`);
  const s = await hello(state.info.url, state.info.token, { origin: "app://spaceknife" });
  await s.call("settings.update", { values: { "general.language": "en" } });
  s.close();
  console.log("0.3.4: general.language set to en");
}

/** Wait for the update badge (the first automatic check is a minute after the start, the download
 * right after it), then click it and 「无缝更新」 (v0.3.4 packaging/update_e2e.py restart_and_update). */
async function clickSeamless(page) {
  await until(`0.3.4's badge for ${NEW_VERSION} to be ready`, () => page.evaluate("!!document.querySelector('.update-badge.ready')"), 300_000, 1000);
  console.log(`badge: ${JSON.stringify(await page.evaluate("document.querySelector('.update-badge').textContent"))}`);
  await page.evaluate("document.querySelector('.update-badge').click(), true");
  const button = "document.querySelector('.update-card .uc-choice.seamless')";
  await until("the update card", () => page.evaluate(`!!${button}`), 30_000);
  console.log(`card: ${JSON.stringify(await page.evaluate("document.querySelector('.update-card').innerText"))}`);
  await page.evaluate(`${button}.click(), true`).catch(() => {}); // the window goes away while it answers
}

// ------------------------------------------------------------------ what every leg checks

/** Marker files in the data, config and cache folders, before 0.3.4 starts. */
function seed(folders) {
  const out = new Map();
  for (const [what, dir] of Object.entries(folders)) {
    mkdirSync(dir, { recursive: true });
    const file = join(dir, `e2e-from-034-${what}.txt`);
    writeFileSync(file, `kept through the update to ${NEW_VERSION} (${what})\n`);
    out.set(file, sha256(file));
  }
  return out;
}

function expectSeeded(seeded) {
  for (const [file, digest] of seeded) {
    if (!existsSync(file) || sha256(file) !== digest) throw new Error(`${file} did not survive the update`);
  }
  console.log(`the ${seeded.size} marker files are there, byte for byte`);
}

/** The new version's backend (its ready file, a new process), and what it says. */
async function newVersion(ready, oldPids, kind) {
  const info = await until(`${NEW_VERSION} to start (its ready file)`, () => {
    try {
      const r = JSON.parse(readFileSync(ready, "utf8"));
      return oldPids.includes(r.pid) ? null : r;
    } catch {
      return null;
    }
  }, 180_000);
  started.add(info.pid);
  started.add(appProcessOf(info.pid));
  const s = await hello(info.url, info.token);
  const status = await s.call("update.status");
  if (status.current !== NEW_VERSION) throw new Error(`the running version says ${status.current}, not ${NEW_VERSION}`);
  if (status.install_kind !== kind) throw new Error(`the running version takes itself for ${status.install_kind}, not ${kind}`);
  const settings = await s.call("settings.get");
  if (settings.values?.["general.language"] !== "en") throw new Error(`the setting changed in 0.3.4 is ${JSON.stringify(settings.values?.["general.language"])}`);
  console.log(`${NEW_VERSION} runs (pid ${info.pid}, ${kind}); the setting changed in 0.3.4 is kept`);
  return { info, session: s, status };
}

/** The updates folder holds nothing of the update once the new version has cleaned it. */
async function expectCleanUpdates(updates) {
  const left = () => (existsSync(updates) ? readdirSync(updates) : []).filter((n) => /^(previous-|staging-|update-)/.test(n) || versionOf(n));
  await until(`the updates folder to be cleaned (${updates})`, () => left().length === 0, 120_000).catch(() => {
    throw new Error(`still in ${updates}: ${left().join(", ")}`);
  });
  console.log(`updates folder: ${existsSync(updates) ? readdirSync(updates).join(", ") || "(empty)" : "(none)"}`);
}

// ------------------------------------------------------------------ the legs

async function portable() {
  const m = /^SpaceKnife-[^-]+(?:-[0-9A-Za-z.]+)?-((?:win|linux|mac)-(?:x64|arm64))-portable\.(zip|tar\.gz)$/.exec(basename(oldPath));
  if (!m || !basename(oldPath).startsWith(`SpaceKnife-${OLD}-`)) throw new Error(`not 0.3.4's portable archive: ${basename(oldPath)}`);
  step(`unpack ${basename(oldPath)}`);
  mkdirSync(join(work, "copy"));
  unpack(oldPath, join(work, "copy"));
  const tops = readdirSync(join(work, "copy"));
  if (tops.length !== 1) throw new Error(`the archive has ${tops.join(", ")} at the top`);
  const folder = join(work, "copy", tops[0]);
  const program = programIn(folder); // what 0.3.4 runs, and what the swap starts again
  if (!existsSync(program)) throw new Error(`no ${program}`);
  const data = join(folder, "data");
  seen.folder = folder;
  seen.data = data;
  const seeded = seed({ data: join(data, "data"), config: join(data, "config"), cache: join(data, "cache"), top: data });
  // the candidate's files, unpacked as the release has them: what the folder must hold afterwards
  mkdirSync(join(work, "candidate"));
  unpack(newPath, join(work, "candidate"));
  const candTops = readdirSync(join(work, "candidate")).filter((n) => n !== "__MACOSX");
  if (candTops.length !== 1) throw new Error(`the candidate has ${candTops.join(", ")} at the top`);
  const candidate = join(work, "candidate", candTops[0]);
  const shell = isElectron(candidate) ? "the Electron shell" : "the Tauri app";
  console.log(`the candidate is ${shell} (${basename(newPath)})`);
  for (const f of [programIn(candidate), helperIn(candidate)]) if (!existsSync(f)) throw new Error(`the candidate has no ${f}`);
  if (!uiIn(candidate)) throw new Error(`the candidate has no interface (ui/index.html where its kind keeps it) in ${candidate}`);

  const { server, feed } = await serveFeed(newPath);
  try {
    step(`start 0.3.4 and click 「无缝更新」`);
    const ready = join(work, "ready.json");
    const app = start(program, ELECTRON_ARGS, appEnv(feed, ready));
    const page = await devtools(join(data, "electron", "DevToolsActivePort"), app);
    await changeSetting(page);
    await clickSeamless(page);
    await until("0.3.4 to quit", () => app.exited, 90_000);
    console.log(`0.3.4 quit: ${JSON.stringify(app.exited)}`);

    step("the swap");
    const updates = join(data, "data", "updates");
    seen.updates = updates;
    const log = join(updates, `update-${NEW_VERSION}.log`);
    // read often and kept: the new version removes the log with the rest of its update as soon as
    // it starts; its ready file says it came back when the log's last line was missed
    const swap = await watchSwap({ log, ready, oldPids: [app.pid], timeout: 180_000 }).catch((e) => {
      seen.swapLog = e.swapLog ?? seen.swapLog;
      throw e;
    });
    seen.swapLog = swap.text;
    console.log(swap.text.trim() || "(the log was gone before it could be read)");
    if (swap.cleaned) console.log("(the new version removed the log as it started: its ready file says how the swap ended)");
    if (swap.how !== "updated") throw new Error(`the swap ended ${swap.how}, not with "updated"`);

    step(`${NEW_VERSION} comes back`);
    const back = await newVersion(ready, [app.pid], "portable");
    const appPid = appProcessOf(back.info.pid);
    const cmd = commandLine(appPid);
    if (cmd && !cmd.includes("--updated")) throw new Error(`started without --updated: ${cmd}`);
    // an x64 copy on an ARM computer moves to the arm64 build (--expect-machine arm64)
    const machine = option("--expect-machine");
    if (machine && machineOf(program) !== machine) throw new Error(`${program} is built for ${machineOf(program)}, not ${machine}`);
    console.log(`${basename(program)} is built for ${machineOf(program)}`);
    // exactly the candidate's files (and the data folder): nothing of 0.3.4 left, and what the
    // candidate has is the candidate's, byte for byte
    const diff = treeDiff(candidate, folder, ["data"]);
    const problems = [
      ...diff.missing.map((f) => `missing: ${f}`),
      ...diff.different.map((f) => `not the candidate's: ${f}`),
      ...diff.extra.map((f) => `not in the candidate (left by ${OLD}?): ${f}`),
    ];
    if (problems.length) throw new Error(`the folder is not the candidate after the update:\n  ${problems.slice(0, 40).join("\n  ")}${problems.length > 40 ? `\n  … ${problems.length - 40} more` : ""}`);
    if (isElectron(folder) !== isElectron(candidate)) throw new Error(`the folder is ${isElectron(folder) ? "" : "not "}the Electron shell, the candidate is ${shell}`);
    console.log(`the folder holds exactly the candidate's files (${shell}) and its data folder`);
    expectSeeded(seeded);
    await expectCleanUpdates(updates);
    back.session.close();
    stop(appPid);
    await until("the updated app to quit", () => !pidAlive(appPid) && !pidAlive(back.info.pid), 30_000).catch(() => {
      stop(appPid, true);
      stop(back.info.pid, true);
    });
  } finally {
    server.close();
  }
  console.log(`\nportable ${OLD} -> ${NEW_VERSION}: ok`);
}

// The Windows installer: 0.3.4 runs the new setup with electron-builder's own flags
// (`--updated /S --force-run` and the scope: v0.3.4 update.ts nsisUpdateArgs) and quits.
const UNINSTALL_GUID = "3cb3f1fc-eef4-5813-9008-c29b5510458c"; // electron-builder's uuid5 of the appId

function reg(args) {
  const r = spawnSync("reg.exe", args, { encoding: "utf8" });
  return r.status === 0 ? r.stdout : "";
}

function uninstallEntries() {
  const out = [];
  for (const hive of ["HKCU", "HKLM"]) {
    for (const view of ["/reg:64", "/reg:32"]) {
      const text = reg(["query", `${hive}\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${UNINSTALL_GUID}`, view]);
      if (!text) continue;
      const value = (n) => new RegExp(`^\\s+${n}\\s+REG_\\w+\\s+(.*)$`, "m").exec(text)?.[1]?.trim();
      out.push({ hive, view, version: value("DisplayVersion"), location: value("InstallLocation"), uninstall: value("QuietUninstallString") ?? value("UninstallString") });
    }
  }
  // the same key seen through both views of HKCU is one entry
  return out.filter((e, i) => out.findIndex((o) => o.hive === e.hive && o.location === e.location) === i);
}

function shortcutTarget(lnk) {
  const r = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `(New-Object -ComObject WScript.Shell).CreateShortcut('${lnk.replace(/'/g, "''")}').TargetPath`], { encoding: "utf8" });
  return (r.stdout ?? "").trim();
}

async function nsis() {
  const allUsers = flag("--allusers");
  const scope = allUsers ? "/allusers" : "/currentuser";
  step(`install 0.3.4 (${scope})`);
  const run = (file, argv, what) => {
    console.log(`> ${basename(file)} ${argv.join(" ")}`);
    const r = spawnSync(file, argv, { stdio: "inherit", timeout: 600_000 });
    if (r.status !== 0) throw new Error(`${what}: exit code ${r.status} ${r.error ?? ""}`);
  };
  run(oldPath, ["/S", scope], "installing 0.3.4");
  const before = await until("0.3.4's uninstall entry", () => uninstallEntries().find((e) => e.version === OLD), 60_000);
  const dir = before.location;
  roots.add(dir);
  const program = join(dir, "SpaceKnife.exe");
  console.log(`0.3.4 in ${dir} (${before.hive})`);
  const local = process.env.LOCALAPPDATA;
  const dataDir = join(local, "SpaceKnife");
  seen.folder = dir;
  seen.data = dataDir;
  seen.updates = join(dataDir, "updates");
  const seeded = seed({ data: dataDir, cache: join(dataDir, "Cache") });
  const { server, feed } = await serveFeed(newPath);
  try {
    step("start 0.3.4 and click 「无缝更新」");
    const ready = join(work, "ready.json");
    const app = start(program, ELECTRON_ARGS, appEnv(feed, ready));
    const page = await devtools(join(process.env.APPDATA, "SpaceKnife", "electron", "DevToolsActivePort"), app);
    await changeSetting(page);
    await clickSeamless(page);
    await until("0.3.4 to quit", () => app.exited, 90_000);

    step(`${NEW_VERSION} installed over it and started`);
    const back = await newVersion(ready, [app.pid], "nsis");
    const entries = uninstallEntries();
    if (entries.length !== 1) throw new Error(`uninstall entries: ${JSON.stringify(entries)}`);
    const [entry] = entries;
    if (entry.version !== NEW_VERSION || entry.hive !== before.hive) throw new Error(`the uninstall entry: ${JSON.stringify(entry)}`);
    if (resolve(entry.location).toLowerCase() !== resolve(dir).toLowerCase()) throw new Error(`installed in ${entry.location}, not ${dir}`);
    if (resolve(dir).toLowerCase() === resolve(dataDir).toLowerCase()) throw new Error("the program is in the data folder");
    const startMenu = allUsers ? join(process.env.ProgramData, "Microsoft", "Windows", "Start Menu", "Programs") : join(process.env.APPDATA, "Microsoft", "Windows", "Start Menu", "Programs");
    const desktop = allUsers ? join(process.env.PUBLIC, "Desktop") : join(process.env.USERPROFILE, "Desktop");
    for (const lnk of [join(startMenu, "SpaceKnife.lnk"), join(desktop, "SpaceKnife.lnk")]) {
      const target = existsSync(lnk) ? shortcutTarget(lnk) : "";
      if (resolve(target || ".").toLowerCase() !== resolve(program).toLowerCase()) throw new Error(`${lnk} points at ${target || "(missing)"}`);
    }
    expectSeeded(seeded);
    await expectCleanUpdates(join(dataDir, "updates"));
    back.session.close();
    stop(back.info.pid, true);

    step(`uninstall ${NEW_VERSION} silently: the program goes, the data stays`);
    const uninstaller = join(dir, "Uninstall SpaceKnife.exe");
    run(uninstaller, ["/S", scope], "uninstalling");
    await until("the program to be removed", () => !existsSync(program), 120_000);
    if (uninstallEntries().length) throw new Error("an uninstall entry is left");
    expectSeeded(seeded);
  } finally {
    server.close();
  }
  console.log(`\nWindows installer ${OLD} -> ${NEW_VERSION} (${scope}): ok`);
}

// The AppImage: 0.3.4 copies the new file over its own ($APPIMAGE) and starts it with --updated.
async function appimage() {
  const home = join(work, "home");
  mkdirSync(home);
  const target = join(work, basename(oldPath)); // the user's copy, under the old name
  execFileSync("cp", [oldPath, target]);
  execFileSync("chmod", ["755", target]);
  const seeded = seed({ data: join(home, ".local", "share", "SpaceKnife"), config: join(home, ".config", "SpaceKnife"), cache: join(home, ".cache", "SpaceKnife") });
  const { server, feed } = await serveFeed(newPath);
  try {
    step("start the 0.3.4 AppImage and click 「无缝更新」");
    const ready = join(work, "ready.json");
    const env = appEnv(feed, ready, { HOME: home, XDG_CONFIG_HOME: join(home, ".config"), XDG_DATA_HOME: join(home, ".local", "share"), XDG_CACHE_HOME: join(home, ".cache") });
    const app = start(target, [...ELECTRON_ARGS, "--no-sandbox"], env);
    const page = await devtools(join(home, ".config", "SpaceKnife", "electron", "DevToolsActivePort"), app);
    await changeSetting(page);
    await clickSeamless(page);
    await until("0.3.4 to quit", () => app.exited, 90_000);
    const back = await newVersion(ready, [app.pid], "appimage");
    if (sha256(target) !== sha256(newPath)) throw new Error(`${target} is not the new AppImage`);
    if (existsSync(`${target}.new`)) throw new Error(`${target}.new is left`);
    expectSeeded(seeded);
    await expectCleanUpdates(join(home, ".local", "share", "SpaceKnife", "updates"));
    back.session.close();
    stop(back.info.pid, true);
  } finally {
    server.close();
  }
  console.log(`\nAppImage ${OLD} -> ${NEW_VERSION}: ok`);
}

/** deb / rpm: what the system's installer does with the file 0.3.4 downloaded and opened. */
async function packaged(kind) {
  const sudo = process.getuid?.() === 0 ? [] : ["sudo"];
  const sh = (argv, what) => {
    console.log(`> ${argv.join(" ")}`);
    const r = spawnSync(argv[0], argv.slice(1), { stdio: "inherit", timeout: 900_000 });
    if (r.status !== 0) throw new Error(`${what}: exit code ${r.status}`);
  };
  const tool = kind === "deb" ? ["apt-get", "install", "-y"] : ["dnf", "install", "-y"];
  step(`install 0.3.4's ${kind}`);
  for (const r of ["/opt/SpaceKnife", "/usr/bin/spaceknife", "/usr/lib/spaceknife"]) roots.add(r);
  sh([...sudo, ...tool, oldPath], `installing 0.3.4's ${kind}`);
  const query = () =>
    kind === "deb"
      ? spawnSync("dpkg-query", ["-W", "-f", "${Package} ${Version}\\n", "spaceknife", "space-knife"], { encoding: "utf8" }).stdout.trim().split("\n").filter((l) => /\d/.test(l))
      : spawnSync("rpm", ["-q", "--qf", "%{NAME} %{VERSION}\\n", "spaceknife", "space-knife"], { encoding: "utf8" }).stdout.trim().split("\n").filter((l) => !l.includes("not installed"));
  console.log(`installed: ${query().join(", ")}`);
  const home = homedir();
  const seeded = seed({ data: join(home, ".local", "share", "SpaceKnife"), config: join(home, ".config", "SpaceKnife"), cache: join(home, ".cache", "SpaceKnife") });

  step(`install ${basename(newPath)} over it`);
  sh([...sudo, ...tool, newPath], `installing the new ${kind}`);
  const now = query();
  if (now.length !== 1 || now[0] !== `spaceknife ${NEW_VERSION}`) throw new Error(`installed packages: ${JSON.stringify(now)}`);
  // the deb is the Tauri app now (/usr/bin/spaceknife a file of its own, 0.3.4's /opt/SpaceKnife
  // and its alternative gone); the rpm is the Electron shell, laid out as 0.3.4's (/opt/SpaceKnife,
  // /usr/bin/spaceknife an alternative of it, which 0.3.4's %postun must not have taken away:
  // apps/electron-shell/build/linux/rpm-posttrans.sh)
  const bin = "/usr/bin/spaceknife";
  if (!existsSync(bin)) throw new Error(`no ${bin} after the upgrade (the old package's scripts took it?)`);
  const real = realpathSync(bin);
  const alt = alternatives();
  if (kind === "deb") {
    if (existsSync("/opt/SpaceKnife")) throw new Error(`/opt/SpaceKnife is left: ${readdirSync("/opt/SpaceKnife").join(", ")}`);
    if (real.startsWith("/opt/") || lstatSync(bin).isSymbolicLink()) throw new Error(`${bin} still leads to ${real}`);
    if (alt) throw new Error(`the alternatives group spaceknife is left:\n${alt}`);
  } else {
    if (real !== "/opt/SpaceKnife/spaceknife") throw new Error(`${bin} leads to ${real}`);
    if (!alt?.includes("/opt/SpaceKnife/spaceknife")) throw new Error(`the alternatives group spaceknife: ${alt}`);
  }
  const desktops = readdirSync("/usr/share/applications").filter((n) => /spaceknife/i.test(n));
  if (desktops.length !== 1 || desktops[0] !== "spaceknife.desktop") throw new Error(`desktop entries: ${desktops.join(", ")}`);
  console.log(`one package (${now[0]}); ${bin} -> ${real}; ${desktops[0]}`);
  expectSeeded(seeded);

  step(`start ${NEW_VERSION}`);
  const { server, feed } = await serveFeed(newPath);
  try {
    const ready = join(work, "ready.json");
    // the rpm is the Electron shell: as root (the container) Chromium wants --no-sandbox
    const argv = kind === "rpm" && process.getuid?.() === 0 ? ["--no-sandbox"] : [];
    const app = start(bin, argv, appEnv(feed, ready));
    // the setting was not changed through 0.3.4 here: the package leg checks the package, the
    // in-app legs the setting
    const info = await until(`${NEW_VERSION} to start`, () => {
      try {
        return JSON.parse(readFileSync(ready, "utf8"));
      } catch {
        return app.exited ? Promise.reject(new Error(`it exited: ${JSON.stringify(app.exited)}`)) : null;
      }
    }, 120_000);
    started.add(info.pid);
    const s = await hello(info.url, info.token);
    const status = await s.call("update.status");
    if (status.current !== NEW_VERSION || status.install_kind !== kind) throw new Error(`it says ${status.current}, ${status.install_kind}`);
    console.log(`${NEW_VERSION} runs as a ${kind} install`);
    s.close();
    stop(info.pid, true);
  } finally {
    server.close();
  }

  step(`remove ${kind}`);
  sh([...sudo, ...(kind === "deb" ? ["apt-get", "remove", "-y", "spaceknife"] : ["dnf", "remove", "-y", "spaceknife"])], "removing");
  if (existsSync(bin) || lstatSafe(bin)) throw new Error(`${bin} is left after the removal`);
  if (alternatives()) throw new Error("an alternatives group spaceknife is left");
  if (existsSync("/opt/SpaceKnife")) throw new Error("/opt/SpaceKnife is left after the removal");
  expectSeeded(seeded);
  console.log(`\n${kind} ${OLD} -> ${NEW_VERSION}: ok`);
}

/** The alternatives group spaceknife as the system shows it (Debian: update-alternatives --query,
 * RHEL: alternatives --display), or null when there is none. */
function alternatives() {
  for (const [cmd, argv] of [
    ["update-alternatives", ["--query", "spaceknife"]],
    ["alternatives", ["--display", "spaceknife"]],
  ]) {
    const r = spawnSync(cmd, argv, { encoding: "utf8" });
    if (r.error) continue; // not this system's tool
    return r.status === 0 && r.stdout.trim() ? r.stdout : null;
  }
  return null;
}

function lstatSafe(p) {
  try {
    return lstatSync(p);
  } catch {
    return null;
  }
}

/** dmg: 0.3.4 opens the disk image; the user drags the new app over the old one (Finder's 「替换」). */
async function dmg() {
  const apps = join(work, "Applications");
  mkdirSync(apps);
  const attach = (image) => {
    const mnt = mkdtempSync(join(work, "mnt-"));
    execFileSync("hdiutil", ["attach", "-nobrowse", "-readonly", "-mountpoint", mnt, image], { stdio: "inherit" });
    return mnt;
  };
  const install = (image) => {
    const mnt = attach(image);
    try {
      rmSync(join(apps, "SpaceKnife.app"), { recursive: true, force: true });
      execFileSync("/usr/bin/ditto", [join(mnt, "SpaceKnife.app"), join(apps, "SpaceKnife.app")], { stdio: "inherit" });
    } finally {
      execFileSync("hdiutil", ["detach", mnt], { stdio: "inherit" });
    }
  };
  step("0.3.4's disk image, then the new one over it");
  install(oldPath);
  install(newPath);
  const plist = join(apps, "SpaceKnife.app", "Contents", "Info.plist");
  const read = (key) => execFileSync("/usr/libexec/PlistBuddy", ["-c", `Print :${key}`, plist], { encoding: "utf8" }).trim();
  if (read("CFBundleIdentifier") !== "io.github.duskxi.spaceknife") throw new Error(`bundle id ${read("CFBundleIdentifier")}`);
  if (read("CFBundleShortVersionString") !== NEW_VERSION) throw new Error(`version ${read("CFBundleShortVersionString")}`);
  if (read("CFBundleExecutable") !== "SpaceKnife") throw new Error(`executable ${read("CFBundleExecutable")}`);
  execFileSync("codesign", ["--verify", "--deep", "--strict", join(apps, "SpaceKnife.app")], { stdio: "inherit" });
  const { server, feed } = await serveFeed(newPath);
  try {
    const ready = join(work, "ready.json");
    const app = start(join(apps, "SpaceKnife.app", "Contents", "MacOS", "SpaceKnife"), [], appEnv(feed, ready));
    const info = await until(`${NEW_VERSION} to start`, () => {
      try {
        return JSON.parse(readFileSync(ready, "utf8"));
      } catch {
        return app.exited ? Promise.reject(new Error(`it exited: ${JSON.stringify(app.exited)}`)) : null;
      }
    }, 120_000);
    started.add(info.pid);
    const s = await hello(info.url, info.token);
    const status = await s.call("update.status");
    if (status.current !== NEW_VERSION || status.install_kind !== "dmg") throw new Error(`it says ${status.current}, ${status.install_kind}`);
    s.close();
    stop(info.pid, true);
  } finally {
    server.close();
  }
  console.log(`\ndmg ${OLD} -> ${NEW_VERSION}: ok`);
}

// ------------------------------------------------------------------ on the way out

/** The last `n` lines of a text file, or null. */
function tail(file, n = 40) {
  try {
    return readFileSync(file, "utf8").trimEnd().split(/\r?\n/).slice(-n).join("\n");
  } catch {
    return null;
  }
}

/** The files below `dir` whose name matches `re` (at most `depth` folders down). */
function find(dir, re, depth = 4) {
  const out = [];
  const walk = (d, left) => {
    let names = [];
    try {
      names = readdirSync(d, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of names) {
      const p = join(d, e.name);
      if (e.isDirectory() && left > 0) walk(p, left - 1);
      else if (e.isFile() && re.test(e.name)) out.push(p);
    }
  };
  walk(dir, depth);
  return out;
}

const listing = (dir) => {
  try {
    return readdirSync(dir).sort().join(", ") || "(empty)";
  } catch (e) {
    return `(${e.code})`;
  }
};

/** What there is to know when a leg fails: the swap's log as last read (and as it is now), its
 * plan, the folders, the processes still running from the test's folders, the new version's logs. */
function diagnose() {
  const lines = ["---- diagnosis"];
  const say = (title, body) => body != null && lines.push(`-- ${title}`, String(body).trimEnd() || "(empty)");
  if (seen.updates) {
    const log = join(seen.updates, `update-${NEW_VERSION}.log`);
    say(`the swap's log as last read (${log})`, seen.swapLog || "(never read)");
    say("the swap's log now", tail(log, 80) ?? "(gone)");
    say(`the swap's plan (update-${NEW_VERSION}.json)`, tail(join(seen.updates, `update-${NEW_VERSION}.json`), 40) ?? "(gone)");
    say(`the updates folder ${seen.updates}`, listing(seen.updates));
  }
  if (seen.folder) say(`the app folder ${seen.folder}`, listing(seen.folder));
  const running = processesUnder([...roots]);
  say("processes running from the test's folders", running.map((p) => `${p.pid} ${p.cmd}`).join("\n") || "(none)");
  // the new version's own logs (the backend's, the shell's): the newest few, their ends
  const logs = seen.data ? find(seen.data, /\.log$/i).filter((f) => !/[\\/]updates[\\/]/.test(f)) : [];
  logs.sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  for (const f of logs.slice(0, 3)) say(`log ${f}`, tail(f, 40));
  lines.push("----");
  console.error(lines.join("\n"));
}

/** End what the test started, and what still runs from its folders (the swap helper and the new
 * version are in sessions of their own). */
async function cleanup() {
  for (const p of procs) if (!p.exited) stop(p.pid, true);
  for (const pid of started) if (pidAlive(pid)) stop(pid, true);
  const left = await stopUnder([...roots]);
  if (left.length) console.log(`ended what still ran from the test's folders: ${left.map((p) => `${p.pid} ${p.cmd}`).join("; ")}`);
}

let leaving = false;
/** Report, clean up and exit (once). */
async function leave(code, message) {
  if (leaving) return;
  leaving = true;
  if (code !== 0) {
    try {
      diagnose();
    } catch (e) {
      console.error(`(the diagnosis failed: ${e.message})`);
    }
  }
  try {
    await cleanup();
  } catch (e) {
    console.error(`(cleaning up failed: ${e.message})`);
  }
  await sleep(1500);
  if (!process.env.SK_KEEP) {
    try {
      // the WebView's last processes may hold its profile a little after the app has gone
      rmSync(work, { recursive: true, force: true, maxRetries: 20, retryDelay: 500 });
    } catch (e) {
      console.log(`(${work} stays behind: ${e.code} ${e.path ?? ""})`);
    }
  }
  if (code !== 0) fail(message, output);
  process.exit(0);
}

const deadline = Number(process.env.SK_E2E_DEADLINE_S || 1200);
const watchdog = setTimeout(() => void leave(1, `the ${mode} leg did not finish within ${deadline} s`), deadline * 1000);
for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => void leave(1, `ended by ${signal}`));

try {
  console.log(`${mode}: ${basename(oldPath)} -> ${basename(newPath)} (${NEW_VERSION}), work folder ${work}`);
  if (mode === "portable") await portable();
  else if (mode === "nsis") await nsis();
  else if (mode === "appimage") await appimage();
  else if (mode === "dmg") await dmg();
  else await packaged(mode);
  clearTimeout(watchdog);
  await leave(0);
} catch (e) {
  clearTimeout(watchdog);
  await leave(1, e.stack ?? e.message);
}
