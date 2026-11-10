// End to end: an installed release (OLD, the real files of the version about to be released or
// already out) updates itself to a real build of the next version (NEW), with its own code, the
// way users' copies will. Once a version is out its update code is frozen: every later version is
// found, checked, downloaded and handed to the system or to the new version's swap helper by
// that code (docs/packaging.md, 9: the contract 0.3.5 set). The update from 0.3.4 has its own
// test (update-from-034.mjs); update-e2e.mjs updates a build to a renamed copy of itself. Here
// both sides are real builds: OLD's code finds NEW's real file, NEW's code puts itself in place.
//
//   node packaging/update-next.mjs portable OLD.zip|OLD.tar.gz NEW.zip|NEW.tar.gz   (Windows; Linux under xvfb-run; macOS)
//       [--expect-machine x64|arm64]   (the processor the updated program is built for: an x64
//       copy on an ARM computer moves to the arm64 build)
//       [--click window|test]          (default: window where the window has a DevTools port)
//   node packaging/update-next.mjs nsis OLD-Setup.exe NEW-Setup.exe        (Windows)
//   node packaging/update-next.mjs appimage OLD.AppImage NEW.AppImage      (Linux under xvfb-run)
//   node packaging/update-next.mjs deb OLD.deb NEW.deb                     (Ubuntu with sudo, under xvfb-run)
//   node packaging/update-next.mjs rpm OLD.rpm NEW.rpm                     (rockylinux:9 as root, under xvfb-run)
//   node packaging/update-next.mjs dmg OLD.dmg NEW.dmg                     (macOS)
//
// The versions come from the file names (the release's names, tools/release.mjs expected). A
// release list on 127.0.0.1 (SPACEKNIFE_UPDATE_FEED) offers OLD and NEW, both pre-releases as
// GitHub has every 0.x release, each with every file and checksum list a release publishes:
// NEW's file for this kind of install is the real one, every other name a stand-in of a few
// bytes (lib.mjs releaseFiles), so a copy that took another kind's name would not get its update.
// OLD is started minimized; through its backend (its ready file) the test changes a setting, as a
// user would, and waits until OLD has found NEW by itself (its first check is a minute after the
// start), picked exactly this install's file, downloaded it and found its SHA-256 right.
//
// Then 「无缝更新」, where the card has it:
// - clicked like a user, where OLD's window has a DevTools port (--click window): the Electron
//   shell (--remote-debugging-port) and the Tauri app on Windows (WebView2 takes the port from
//   WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS, or, where a runtime leaves that aside for the app's own
//   arguments, from its per-program policy AdditionalBrowserArguments, which the test sets for
//   SpaceKnife.exe on the runner and takes out again; a window that answers on neither is updated
//   the test's way below, which the log says in a line of its own). The test clicks the title
//   bar's badge and the card's 「无缝更新」 (twice when the card asks once more because a scan
//   runs); what happens next is
//   OLD's shell's own code: the portable copy's unpacking, plan and swap helper (NEW's program), the
//   installer started with OLD's arguments, the AppImage put in place;
// - done by the test where the system's web view has no DevTools port (--click test: the Tauri
//   app on Linux and macOS): what the button does (src-tauri install.rs replace_portable), the
//   unpacking, the plan and NEW's program started with --apply-update, as update-e2e.mjs does.
//   The test's ready file goes to the swap helper: the helper hands it to NEW, as it does for a
//   test or a demo (sk_core::update::swap Ready).
// The packages (deb, rpm, dmg) have no 「无缝更新」: OLD only finds, downloads and checks the file,
// the test does what the system's installer does with it (apt-get install, dnf install, the disk
// image's app dragged over the old one: hdiutil and ditto).
//
// The portable copy is first clicked with a file and another app of the user's next to it: NEW's
// swap helper refuses the folder, nothing outside the data moves, OLD runs on (and says why on its
// card), no .asar is left in what it unpacked; then without them and with a file the system puts
// in folders, which stays. The installer leg puts a file on OLD's list of installed files that NEW
// does not have, and files of the user's in the folder and its ui folder: the first goes, the
// others stay.
//
// What every leg checks afterwards: NEW runs, started with --updated where the update starts it
// (portable, nsis, appimage); its update.status says NEW and the same kind of install; the setting
// changed in OLD and the marker files in the data, config and cache folders are there, byte for
// byte; nothing of the update is left in the updates folder (previous-OLD, staging-NEW,
// update-NEW.*, the downloaded file: NEW's clean_updates); per kind: the portable folder holds
// exactly NEW's files (and the data folder, and the system's file), the installer's uninstall entry
// is one and NEW's, in the same folder, its list of files NEW's, the shortcuts; the AppImage is NEW,
// no .new, the old one kept as .previous; one package, none of OLD's files left; the disk image's
// app is NEW's, signed. Then the next hop: the release list gets NEW+1 (NEW's file under the next
// version's name, with its checksum list), and NEW's own check finds it under exactly this
// install's name and downloads it with the right SHA-256 (not installed).
//
// NEW's backend: where it was started by the update without the test's ready file (both shells
// take SPACEKNIFE_READY_FILE out of their environment, so OLD hands it to nothing), the test finds
// NEW by its process (its command line: --updated), waits until the update's helper has ended and
// NEW has cleaned the updates folder, and then reaches its backend through its window (WebView2:
// NEW inherits the port) or, where it has no DevTools port (the Electron shell is started without
// switches), quits it as a user quits it and starts it again from the same place with the test's
// ready file. The log says which.
//
// On a failure or a timeout it prints what there is to know (the swap's log as last read, the
// updates and app folders, the processes still running from the test's folders, the apps' own
// logs, the installer's log), then ends every process it started or that runs from its folders;
// every leg has a deadline (SK_E2E_DEADLINE_S, default 1500). It starts windows and installs, so
// it runs on a CI runner only (GITHUB_ACTIONS=true; SK_E2E_ANYWHERE=1 on a test machine). Exit
// status 1 on failure; SK_KEEP=1 keeps the work folder.

import { execFileSync, spawn, spawnSync } from "node:child_process";
import { copyFileSync, createReadStream, existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, readlinkSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import { tmpdir } from "node:os";
import { basename, dirname, join, relative, resolve } from "node:path";
import { createInterface } from "node:readline";

import { expected } from "../tools/release.mjs";
import {
  MAC,
  WIN,
  alternativesGroup,
  appProcessOf,
  asarFilesIn,
  cliIn,
  commandLine,
  compareVersions,
  devtoolsPage,
  devtoolsPorts,
  fail,
  hello,
  helperIn,
  isElectron,
  machineOf,
  notOwned,
  pidAlive,
  processesUnder,
  programIn,
  releaseFiles,
  sha256,
  sleep,
  stop,
  stopUnder,
  swapEnding,
  treeDiff,
  treeOf,
  uiIn,
  unpack,
  until,
  untilUninstalled,
  watchSwap,
} from "./lib.mjs";

const args = process.argv.slice(2);
const option = (name) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : null;
};
const VALUED = ["--expect-machine", "--click"]; // options that take a value
const [mode, oldFile, newFile] = args.filter((a, i) => !a.startsWith("--") && !VALUED.includes(args[i - 1]));
const MODES = ["portable", "nsis", "appimage", "deb", "rpm", "dmg"];
if (!MODES.includes(mode) || !oldFile || !newFile || !existsSync(oldFile) || !existsSync(newFile)) {
  fail(`usage: node packaging/update-next.mjs ${MODES.join("|")} OLD NEW [--expect-machine x64|arm64] [--click window|test]`);
}
if (process.env.GITHUB_ACTIONS !== "true" && process.env.SK_E2E_ANYWHERE !== "1") fail(`the ${mode} leg starts SpaceKnife's windows and installs it: it runs on a CI runner only (SK_E2E_ANYWHERE=1 on a test machine)`);
const oldPath = resolve(oldFile);
const newPath = resolve(newFile);
const OLD = versionOf(basename(oldPath));
const NEW = versionOf(basename(newPath));
if (!OLD || !NEW) fail(`not release file names: ${basename(oldPath)}, ${basename(newPath)}`);
if (compareVersions(NEW, OLD) <= 0) fail(`${basename(newPath)} (${NEW}) is not newer than ${basename(oldPath)} (${OLD})`);
// the release after NEW: NEW's file under its name (the next hop)
const NEXT = (() => {
  const [maj, min, patch] = NEW.split(/[-~]/)[0].split(".").map(Number);
  return `${maj}.${min}.${patch + 1}`;
})();
const NEW_NAME = basename(newPath);
const NEXT_NAME = templateOf(NEW_NAME, NEW).replace("{v}", NEXT);
if (templateOf(basename(oldPath), OLD).replace(/-(win|linux|mac)-(x64|arm64)-/, "-$1-ARCH-") !== templateOf(NEW_NAME, NEW).replace(/-(win|linux|mac)-(x64|arm64)-/, "-$1-ARCH-")) {
  fail(`${basename(oldPath)} and ${NEW_NAME} are not files of the same kind of install`);
}
const step = (text) => console.log(`\n== ${new Date().toISOString().slice(11, 19)} ${text}`);
const output = [];
const procs = new Set();
// NEW's processes, started by the update or the system rather than by the test: ended on the way out
const started = new Set();
// resolved: macOS hands out /var/folders/… as /private/var/folders/…, Windows the long names
const work = realpathSync.native(mkdtempSync(join(tmpdir(), "sk-next-")));
// the folders whose processes are ended on the way out
const roots = new Set([work]);
// what a failure report shows: the leg's folders as it learns them, the swap's log as last read
const seen = { folder: null, updates: null, data: [], swapLog: "", installLog: false };

/** The version in a release file name (tools/release.mjs expected), "-" for a deb's or rpm's "~". */
function versionOf(name) {
  const v = String.raw`(\d+\.\d+\.\d+(?:[-~][0-9A-Za-z.]+)?)`;
  for (const e of expected()) {
    const [head, tail] = e.template.split("{v}").map((s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
    const m = new RegExp(`^${head}${v}${tail}$`).exec(name);
    if (m) return m[1].replace("~", "-");
  }
  return null;
}

/** The template (tools/release.mjs expected) of a release file of `version`. */
function templateOf(name, version) {
  for (const e of expected()) {
    for (const v of [version, version.replaceAll("-", "~"), version.replaceAll("-", "_")]) if (e.template.replace("{v}", v) === name) return e.template;
  }
  throw new Error(`${name} is not a file of release ${version}`);
}

// ------------------------------------------------------------------ the release list

/** GitHub's release list on 127.0.0.1: OLD and NEW (later NEXT too), pre-releases as every 0.x
 * release is (release.yml), each with every file a release publishes (releaseFiles). */
async function serveFeed() {
  const digests = new Map();
  const digest = (p) => (digests.has(p) ? digests.get(p) : digests.set(p, sha256(p)).get(p));
  const releases = new Map();
  releases.set(OLD, releaseFiles(OLD, new Map()));
  releases.set(NEW, releaseFiles(NEW, new Map([[NEW_NAME, newPath]]), digest));
  const server = http.createServer((req, res) => {
    const base = `http://127.0.0.1:${server.address().port}`;
    const url = new URL(req.url, base);
    if (url.pathname === "/releases") {
      const list = [...releases.keys()]
        .sort((a, b) => compareVersions(b, a))
        .map((version) => ({
          tag_name: `v${version}`,
          name: `SpaceKnife ${version}`,
          draft: false,
          prerelease: version.startsWith("0.") || /[-~]/.test(version),
          html_url: `${base}/tag/v${version}`,
          published_at: new Date().toISOString(),
          body: `The end-to-end test's release ${version}.`,
          assets: releases.get(version).map((f) => ({ name: f.name, size: f.path ? statSync(f.path).size : f.body.length, browser_download_url: `${base}/files/${version}/${encodeURIComponent(f.name)}` })),
        }));
      const body = JSON.stringify(list);
      res.writeHead(200, { "content-type": "application/json", "content-length": Buffer.byteLength(body) }).end(body);
      return;
    }
    const m = /^\/files\/([^/]+)\/([^/]+)$/.exec(url.pathname);
    const file = m ? releases.get(m[1])?.find((f) => f.name === decodeURIComponent(m[2])) : null;
    if (!file) return res.writeHead(404).end();
    console.log(`  feed: GET ${url.pathname}${file.path ? "" : " (a stand-in)"}`);
    if (file.path) {
      res.writeHead(200, { "content-length": statSync(file.path).size });
      createReadStream(file.path).pipe(res);
    } else res.writeHead(200, { "content-length": file.body.length }).end(file.body);
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const url = `http://127.0.0.1:${server.address().port}/releases`;
  console.log(`release list on ${url}: ${OLD}, ${NEW} (${releases.get(NEW).length} files, ${NEW_NAME} the real one)`);
  return {
    url,
    server,
    digest: digest(newPath),
    addNext() {
      releases.set(NEXT, releaseFiles(NEXT, new Map([[NEXT_NAME, newPath]]), digest));
      console.log(`the release list now has ${NEXT} too: ${NEXT_NAME} is ${NEW_NAME}`);
    },
  };
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

/** The environment for SpaceKnife: the test's release list, minimized, the test's ready file
 * (none: null), never a SPACEKNIFE_HOME of this shell. */
function appEnv(feed, ready, extra = {}) {
  const env = { ...process.env, SPACEKNIFE_UPDATE_FEED: feed, SPACEKNIFE_START_MINIMIZED: "1", ...extra };
  if (ready) env.SPACEKNIFE_READY_FILE = ready;
  else delete env.SPACEKNIFE_READY_FILE;
  delete env.SPACEKNIFE_HOME;
  return env;
}

/** Chromium's page of a window that keeps answering while minimized (it otherwise puts the page of
 * a hidden or occluded window to sleep, and a window on a desktop nobody looks at counts as one). */
const AWAKE = ["--disable-renderer-backgrounding", "--disable-backgrounding-occluded-windows", "--disable-background-timer-throttling", "--disable-features=CalculateNativeWinOcclusion"];
/** The Electron shell: its DevTools port (DevToolsActivePort in its userData). */
const ELECTRON_ARGS = ["--remote-debugging-port=0", ...AWAKE];
/** WebView2 (the Tauri app on Windows): the port from the environment, which what the app starts
 * inherits (the swap helper, the installer, NEW). Some runtimes (156 on Windows Server 2025 and
 * Windows 11 on ARM) leave the variable aside for the arguments the app gives (wry's): the same
 * arguments go into WebView2's own per-program setting as well (webview2Policy). */
const webview2Args = (port) => [`--remote-debugging-port=${port}`, ...AWAKE].join(" ");
const webview2Env = (port) => ({ WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: webview2Args(port) });
const WEBVIEW2_POLICY = "HKCU\\Software\\Policies\\Microsoft\\Edge\\WebView2\\AdditionalBrowserArguments";
let webview2PolicySet = false;

/** WebView2's AdditionalBrowserArguments for SpaceKnife.exe (the program's name: OLD and NEW alike),
 * on this CI runner, for this leg: taken out again on the way out. */
function webview2Policy(port) {
  const r = spawnSync("reg.exe", ["add", WEBVIEW2_POLICY, "/v", "SpaceKnife.exe", "/t", "REG_SZ", "/d", webview2Args(port), "/f"], { encoding: "utf8" });
  if (r.status !== 0) console.log(`(could not set ${WEBVIEW2_POLICY}: ${r.stderr?.trim()})`);
  else webview2PolicySet = true;
}

async function freePort() {
  const s = net.createServer();
  await new Promise((r) => s.listen(0, "127.0.0.1", r));
  const port = s.address().port;
  await new Promise((r) => s.close(r));
  return port;
}

/** A ready file's connection info (the backend writes it once it listens), not of `notPids`. */
function readyOf(file, proc, what, timeout = 120_000, notPids = []) {
  return until(what, () => {
    try {
      const r = JSON.parse(readFileSync(file, "utf8"));
      if (r?.url && !notPids.includes(r.pid)) return r;
    } catch {
      // not there yet, or being written
    }
    if (proc?.exited) throw new Error(`${what}: it exited first: ${JSON.stringify(proc.exited)}`);
    return null;
  }, timeout);
}

/** The program of a command line ("[S] cmd" from processesUnder on Linux and macOS). */
function argv0(cmd) {
  const c = cmd.replace(/^\[\S+\]\s+/, "");
  const m = /^"([^"]+)"|^(\S+)/.exec(c);
  return m ? (m[1] ?? m[2]) : "";
}

/** The app's main processes started with --updated among those running from `where` (not
 * Chromium's own --type= processes, not an installer or an uninstaller's copy). */
function updatedApps(where, name) {
  const same = (a, b) => (WIN ? a.toLowerCase() === b.toLowerCase() : a === b);
  return processesUnder(where).filter((p) => same(basename(argv0(p.cmd).replaceAll("\\", "/")), name) && /(^|\s)--updated(\s|$)/.test(p.cmd) && !/--type=/.test(p.cmd));
}

/** The swap helpers (--apply-update) running from `where`. */
const swapHelpers = (where) => processesUnder(where).filter((p) => /(^|\s)--apply-update(\s|$)/.test(p.cmd));

// ------------------------------------------------------------------ OLD's window (DevTools protocol)

/** The window of `version` (window.sk.version) over the DevTools protocol: the port it was given
 * (WebView2), or what DevToolsActivePort below `dirs` says, written at or after `since`. */
function openWindow(version, { port = null, dirs = [], since = 0, proc = null, timeout = 120_000 }) {
  return devtoolsPage(
    async () => {
      if (proc?.exited) throw new Error(`it exited: ${JSON.stringify(proc.exited)}`);
      return [...(port ? [port] : []), ...devtoolsPorts(dirs, since)];
    },
    { what: `${version}'s window (its DevTools port)`, timeout, accept: async (ev) => (await ev("window.sk ? window.sk.version : null")) === version },
  );
}

const CARD_ERROR = "(document.querySelector('.update-card [role=alert]') || {}).innerText || ''";

/** Wait for the update badge (OLD's first check is a minute after the start, the download right
 * after it), then click it and 「无缝更新」, and once more when the card asks for it (a scan runs:
 * UpdateCard.vue go). A card still open from an attempt that did not start gets its button clicked
 * once it is back (OLD restarts its backend then). */
async function clickSeamless(page) {
  const button = "document.querySelector('.update-card .uc-choice.seamless')";
  const ready = await until(
    `${OLD}'s badge for ${NEW} to be ready`,
    () => page.evaluate(`${button} ? "button" : document.querySelector('.update-card') ? null : document.querySelector('.update-badge.ready') ? "badge" : null`),
    300_000,
    1000,
  );
  if (ready === "badge") {
    console.log(`badge: ${JSON.stringify(await page.evaluate("document.querySelector('.update-badge').textContent"))}`);
    await page.evaluate("document.querySelector('.update-badge').click(), true");
    await until("the update card", () => page.evaluate(`!!${button}`), 30_000);
  }
  console.log(`card: ${JSON.stringify(await page.evaluate("document.querySelector('.update-card').innerText"))}`);
  await page.evaluate(`${button}.click(), true`).catch(() => {}); // the window goes away while it answers
  await sleep(700);
  const confirm = await page.evaluate(`!!document.querySelector('.update-card .uc-choice.seamless.confirm')`, { tries: 1 }).catch(() => false);
  if (confirm) {
    console.log("the card asks once more (a scan runs): clicked again");
    await page.evaluate(`${button}.click(), true`).catch(() => {});
  }
}

// ------------------------------------------------------------------ OLD's backend

/** OLD runs (its ready file): the version and the kind of install it says, and a setting changed
 * through its backend, as a user would. */
async function oldRuns(ready, proc, kind) {
  const info = await readyOf(ready, proc, `${OLD} to start (its ready file)`);
  const s = await hello(info.url, info.token);
  const status = await s.call("update.status");
  if (status.current !== OLD) throw new Error(`the old version says it is ${status.current}, not ${OLD}`);
  if (status.install_kind !== kind) throw new Error(`${OLD} takes itself for ${status.install_kind}, not ${kind}`);
  await s.call("settings.update", { values: { "general.language": "en" } });
  console.log(`${OLD} runs (backend pid ${info.pid}, ${kind}); general.language set to en`);
  return { info, session: s };
}

/** OLD finds NEW by itself (its first check is a minute after the start), picks exactly this
 * install's file, downloads it (「自动下载更新」, on by default; asked for when it has not started
 * within half a minute) and finds its SHA-256 right. Returns the downloaded file. */
async function expectFound(session, feed) {
  step(`${OLD} finds ${NEW}, picks ${NEW_NAME}, downloads and checks it`);
  const status = await until(`${OLD} to find ${NEW}`, async () => {
    const s = await session.call("update.status");
    return s.latest?.version === NEW ? s : null;
  }, 240_000, 1000).catch(async (e) => {
    throw new Error(`${e.message}: update.status ${JSON.stringify(await session.call("update.status").catch(() => null))}`);
  });
  if (status.latest.asset?.name !== NEW_NAME) throw new Error(`${OLD} picked ${JSON.stringify(status.latest.asset)} from ${NEW}, not ${NEW_NAME}`);
  const t0 = Date.now();
  let asked = false;
  const done = await until(`${OLD} to download and check ${NEW_NAME}`, async () => {
    const st = await session.call("update.status");
    const d = st.download;
    if (d.state === "failed" || d.state === "cancelled") throw new Error(`the download ${d.state}: ${d.error ?? st.error}`);
    if (d.state === "done" && d.version === NEW) return st;
    if (!asked && Date.now() - t0 > 30_000 && !["downloading", "verifying"].includes(d.state)) {
      asked = true;
      console.log(`(${OLD} did not start the download by itself: asked for it)`);
      await session.call("update.download", { version: NEW });
    }
    return null;
  }, 300_000, 1000);
  const d = done.download;
  if (d.verified !== true) throw new Error(`${OLD} did not verify ${NEW_NAME}: ${JSON.stringify(d)}`);
  if (basename(d.path ?? "") !== NEW_NAME) throw new Error(`${OLD} downloaded ${d.path}, not ${NEW_NAME}`);
  if (sha256(d.path) !== feed.digest) throw new Error(`${d.path} is not ${NEW_NAME}'s bytes`);
  if (done.latest?.asset?.sha256 !== feed.digest) throw new Error(`${OLD} read ${done.latest?.asset?.sha256} as ${NEW_NAME}'s SHA-256, not ${feed.digest}`);
  console.log(`${OLD} picked ${NEW_NAME}, read its SHA-256 ${feed.digest.slice(0, 16)}… from the release's list, downloaded and verified it: ${d.path}`);
  return d.path;
}

// ------------------------------------------------------------------ NEW, once in place

/** Marker files in the data, config and cache folders, before OLD starts. */
function seed(folders) {
  const out = new Map();
  for (const [what, dir] of Object.entries(folders)) {
    mkdirSync(dir, { recursive: true });
    const file = join(dir, `e2e-next-${what}.txt`);
    writeFileSync(file, `kept through the update from ${OLD} to ${NEW} (${what})\n`);
    out.set(file, sha256(file));
  }
  seen.data.push(...Object.values(folders));
  return out;
}

function expectSeeded(seeded) {
  for (const [file, digest] of seeded) {
    if (!existsSync(file) || sha256(file) !== digest) throw new Error(`${file} did not survive the update`);
  }
  console.log(`the ${seeded.size} marker files are there, byte for byte`);
}

/** Through NEW's backend: its version, its kind of install, the setting changed in OLD. */
async function expectNew(session, kind) {
  const status = await session.call("update.status");
  if (status.current !== NEW) throw new Error(`the running version says it is ${status.current}, not ${NEW}`);
  if (status.install_kind !== kind) throw new Error(`${NEW} takes itself for ${status.install_kind}, not ${kind}`);
  const settings = await session.call("settings.get");
  if (settings.values?.["general.language"] !== "en") throw new Error(`the setting changed in ${OLD} is ${JSON.stringify(settings.values?.["general.language"])}`);
  console.log(`${NEW} runs as a ${kind} install; the setting changed in ${OLD} is kept`);
}

/** Nothing of the update is left in the updates folder once NEW has cleaned it (clean_updates:
 * everything of the running version or an older one; its first try at the start may find files
 * still held, which it tries again a little later). */
async function expectCleanUpdates(updates) {
  const left = () => (existsSync(updates) ? readdirSync(updates) : []).filter((n) => /^(previous|staging|update|rollback)-/.test(n) || n.endsWith(".part") || versionOf(n));
  await until(`the updates folder to be cleaned (${updates})`, () => left().length === 0, 150_000).catch(() => {
    throw new Error(`still in ${updates}: ${left().join(", ")}`);
  });
  console.log(`updates folder: ${existsSync(updates) ? readdirSync(updates).join(", ") || "(empty)" : "(none)"}`);
}

/** The next hop: the release list gets NEXT (NEW's file under its name, with its checksum list);
 * NEW's own check finds it under exactly this install's name and downloads it with the right
 * SHA-256. Not installed. */
async function nextHop(feed, session, updates) {
  step(`the next release, ${NEXT}: ${NEW} finds it`);
  feed.addNext();
  await session.call("update.check");
  const status = await until(`${NEW}'s check`, async () => {
    const s = await session.call("update.status");
    return s.checking ? null : s;
  }, 90_000);
  if (status.latest?.version !== NEXT) throw new Error(`${NEW} sees ${JSON.stringify(status.latest?.version ?? null)} as the newest release, not ${NEXT} (error: ${status.error})`);
  if (status.latest.asset?.name !== NEXT_NAME) throw new Error(`${NEW} picked ${JSON.stringify(status.latest.asset)} from ${NEXT}, not ${NEXT_NAME}`);
  // downloaded by itself (「自动下载更新」), or asked for
  await sleep(2000);
  const now = (await session.call("update.status")).download;
  if (!(now.version === NEXT && ["downloading", "verifying", "done"].includes(now.state))) await session.call("update.download", { version: NEXT });
  const done = await until(`${NEW} to download ${NEXT_NAME}`, async () => {
    const st = await session.call("update.status");
    const d = st.download;
    if (d.state === "failed" || d.state === "cancelled") throw new Error(`the download ${d.state}: ${d.error ?? st.error}`);
    return d.state === "done" && d.version === NEXT ? st : null;
  }, 300_000, 1000);
  const d = done.download;
  if (d.verified !== true || basename(d.path ?? "") !== NEXT_NAME || dirname(resolve(d.path)) !== resolve(updates)) throw new Error(`the download of ${NEXT}: ${JSON.stringify(d)} (want ${NEXT_NAME} in ${updates}, verified)`);
  if (sha256(d.path) !== feed.digest || done.latest?.asset?.sha256 !== feed.digest) throw new Error(`${NEXT_NAME}: SHA-256 ${done.latest?.asset?.sha256}, the file ${sha256(d.path)}, want ${feed.digest}`);
  console.log(`${NEW} found ${NEXT}: ${NEXT_NAME}, SHA-256 ${feed.digest.slice(0, 16)}… from its list, downloaded and verified (not installed)`);
}

/** NEW's backend after an update started it without the test's ready file: through its window
 * when it has a DevTools port (WebView2: NEW inherits OLD's environment, the port with it), else
 * NEW is quit as a user quits it and started again from the same place with the test's ready file
 * (the Electron shell starts NEW without switches). `main`: NEW's process the update started. */
async function newBackend({ feed, main, port = null, dirs = [], since = 0, program, argv = [], extra = {} }) {
  if (port) {
    try {
      const page = await openWindow(NEW, { port, dirs, since, timeout: 90_000 });
      const state = await until(`${NEW}'s backend`, () => page.evaluate("window.sk.backend().then(s => s.state === 'starting' ? null : s)"), 60_000);
      page.close();
      if (state.state !== "ready") throw new Error(`${NEW}'s backend: ${JSON.stringify(state)}`);
      console.log(`${NEW}'s window answers on its DevTools port; its backend: ${state.info.url} (pid ${state.info.pid})`);
      started.add(state.info.pid);
      return { info: state.info, session: await hello(state.info.url, state.info.token), how: "window" };
    } catch (e) {
      console.log(`(${NEW}'s window has no DevTools port the test can reach: ${e.message})`);
    }
  }
  console.log(`(${NEW}'s backend can't be reached as it runs: it is quit as a user quits it and started again from the same place, with the test's ready file)`);
  stop(main.pid);
  await until(`${NEW} (${main.pid}) to quit`, () => !pidAlive(main.pid), 60_000).catch(() => {
    console.log(`(${main.pid} did not quit: ended)`);
    stop(main.pid, true);
  });
  await sleep(2000);
  const ready = join(work, "ready-again.json");
  rmSync(ready, { force: true });
  const proc = start(program, argv, appEnv(feed.url, ready, extra));
  const info = await readyOf(ready, proc, `${NEW} started again (its ready file)`);
  started.add(info.pid);
  started.add(appProcessOf(info.pid));
  return { info, session: await hello(info.url, info.token), how: "again", proc };
}

/** NEW's process the update started (its command line has --updated), once the update's helper
 * has ended (it waits until NEW says it runs, and undoes the update otherwise) and NEW kept it. */
async function newStarted(where, name, updates) {
  const main = await until(`${NEW} to run (${name} with --updated)`, () => updatedApps(where, name)[0], 180_000);
  started.add(main.pid);
  console.log(`${NEW} started by the update: ${main.pid} ${main.cmd}`);
  await until("the update's helper to end", () => swapHelpers([work, ...where]).length === 0, 150_000).catch((e) => {
    throw new Error(`${e.message}: ${swapHelpers([work, ...where]).map((p) => `${p.pid} ${p.cmd}`).join("; ")}`);
  });
  const note = join(updates, `rollback-${NEW}.json`);
  if (existsSync(note)) throw new Error(`the update was undone: ${readFileSync(note, "utf8")}`);
  if (!pidAlive(main.pid)) throw new Error(`${NEW} (${main.pid}) is gone`);
  return main;
}

/** Follow a portable copy's swap by its log until it says how it ended; a log NEW removed (it
 * cleans the updates folder as it starts) after it was read, with NEW running, says "updated". */
async function swapEnded(log, where, name, timeout = 180_000) {
  let text = "";
  let read = false;
  const deadline = Date.now() + timeout;
  for (;;) {
    try {
      text = readFileSync(log, "utf8");
      read = true;
      seen.swapLog = text;
    } catch {
      if (read && updatedApps(where, name).length) return { how: "updated", text, cleaned: true };
    }
    const how = swapEnding(text);
    if (how) return { how, text };
    if (Date.now() > deadline) throw new Error(`timed out after ${Math.round(timeout / 1000)} s waiting for the swap's log to say how it ended (${log}): ${text.trim() || "(empty or never written)"}`);
    await sleep(100);
  }
}

// ------------------------------------------------------------------ the portable copy

async function portable() {
  const kindOf = (name) => /-(portable|tauri)\.(zip|tar\.gz)$/.exec(name)?.[1];
  if (!kindOf(basename(oldPath))) throw new Error(`not a portable archive: ${basename(oldPath)}`);
  step(`unpack ${basename(oldPath)}`);
  mkdirSync(join(work, "copy"));
  unpack(oldPath, join(work, "copy"));
  const tops = readdirSync(join(work, "copy")).filter((n) => n !== "__MACOSX");
  if (tops.length !== 1) throw new Error(`the archive has ${tops.join(", ")} at the top`);
  const folder = join(work, "copy", tops[0]);
  const program = programIn(folder);
  const name = basename(program);
  if (!existsSync(program)) throw new Error(`no ${program}`);
  const data = join(folder, "data");
  const updates = join(data, "data", "updates");
  seen.folder = folder;
  seen.updates = updates;
  const seeded = seed({ data: join(data, "data"), config: join(data, "config"), cache: join(data, "cache"), top: data });
  const electron = isElectron(folder);
  // NEW's files, unpacked as the release has them: what the folder must hold afterwards
  mkdirSync(join(work, "candidate"));
  unpack(newPath, join(work, "candidate"));
  const candTops = readdirSync(join(work, "candidate")).filter((n) => n !== "__MACOSX");
  if (candTops.length !== 1) throw new Error(`${NEW_NAME} has ${candTops.join(", ")} at the top`);
  const candidate = join(work, "candidate", candTops[0]);
  if (isElectron(candidate) !== electron) throw new Error(`${basename(oldPath)} is ${electron ? "" : "not "}the Electron shell, ${NEW_NAME} is${electron ? " not" : ""}`);
  if (!uiIn(candidate) || !existsSync(programIn(candidate))) throw new Error(`${NEW_NAME} has no program or interface where it belongs`);
  let click = option("--click") ?? (WIN || electron ? "window" : "test");
  console.log(`${electron ? "the Electron shell" : "the Tauri app"}'s portable copy; 「无缝更新」 ${click === "window" ? `clicked in ${OLD}'s window` : `done by the test as the button does it (${MAC ? "macOS's" : "WebKitGTK's"} web view has no DevTools port)`}`);

  const feed = await serveFeed();
  try {
    step(`start ${OLD}`);
    const ready = join(work, "ready.json");
    const port = WIN ? await freePort() : null;
    const extra = WIN ? webview2Env(port) : {};
    if (WIN && click === "window") webview2Policy(port);
    const dirs = [join(data, electron ? "electron" : "desktop")];
    const startedAt = Date.now();
    const app = start(program, electron && click === "window" ? ELECTRON_ARGS : [], appEnv(feed.url, ready, extra));
    const old = await oldRuns(ready, app, "portable");
    const file = await expectFound(old.session, feed);
    old.session.close();
    const log = join(updates, `update-${NEW}.log`);
    const staging = join(updates, `staging-${NEW}`);
    // a file and another app of the user's next to OLD (a folder of tools, ~/Desktop): NEW's swap
    // helper moves only SpaceKnife's own files, so it does not start and nothing moves; OLD says
    // why and runs on (sk_core::update::swap sort_names)
    const foreign = [join(folder, "e2e-notes.txt"), join(folder, MAC ? "Other.app" : "OtherTool")];
    writeFileSync(foreign[0], "the user's notes\n");
    mkdirSync(join(foreign[1], "Contents"), { recursive: true });
    writeFileSync(join(foreign[1], "Contents", "Info.plist"), "another app\n");
    const outsideData = () => treeOf(folder).filter((e) => !e.path.startsWith(data)).map((e) => `${e.kind} ${e.path}`);
    const before = outsideData();
    const programBefore = sha256(program);
    const systemFile = join(folder, WIN ? "desktop.ini" : MAC ? ".DS_Store" : ".directory");
    let page = null;
    let swap = null; // the test's 「无缝更新」 (--click test)
    if (click === "window") {
      try {
        // up long before: OLD has run for a minute and more by now
        page = await openWindow(OLD, { port, dirs, since: startedAt, proc: app, timeout: WIN ? 60_000 : 120_000 });
      } catch (e) {
        if (!WIN || app.exited) throw e;
        console.log(`\n!! ${OLD}'s window has no DevTools port the test can reach here (${e.message}): 「无缝更新」 is done by the test as the button does it (install.rs replace_portable), not clicked\n`);
        click = "test";
      }
    }
    const readyNew = click === "test" ? join(work, "ready-new.json") : null;

    if (click === "window") {
      step(`click 「无缝更新」 in ${OLD}'s window, with the user's files next to it`);
      await clickSeamless(page);
      const refusal = await until("the swap to refuse the folder with the user's files", () => {
        const text = existsSync(log) ? readFileSync(log, "utf8") : "";
        if (/ updated$/m.test(text)) throw new Error(`the swap went ahead with the user's files next to the app:\n${text}`);
        return text.includes("holds what is not SpaceKnife's") ? text : null;
      }, 180_000);
      seen.swapLog = refusal;
      console.log(refusal.trim());
      await sleep(3000);
      const said = await page.evaluate(CARD_ERROR).catch(() => "");
      console.log(`${OLD}'s card says: ${JSON.stringify(said)}`);
    } else {
      step(`「无缝更新」 as ${OLD}'s button does it (src-tauri install.rs replace_portable), with the user's files next to it`);
      swap = shellSwap({ app, folder, updates, file, electron, env: appEnv(feed.url, readyNew, extra) });
      const refused = await swap.attempt(false);
      if (refused.code !== 2 || refused.said.ok !== false || !refused.said.error?.includes("e2e-notes.txt") || !refused.said.error.includes(basename(foreign[1]))) {
        throw new Error(`with the user's files next to it ${NEW}'s --apply-update said ${refused.line} (exit ${refused.code})`);
      }
      seen.swapLog = existsSync(log) ? readFileSync(log, "utf8") : "";
      console.log(seen.swapLog.trim());
      if (!seen.swapLog.includes("holds what is not SpaceKnife's")) throw new Error(`the swap's log does not say why it did not start:\n${seen.swapLog}`);
      console.log(`${NEW}'s swap helper refused: ${refused.said.error}`);
    }
    // nothing moved, OLD runs on; no .asar left where it unpacked: an Electron app (0.3.4 among
    // them) could not remove that folder before its next attempt (sk_core::update::swap
    // leave_removable)
    if (app.exited) throw new Error(`${OLD} quit although the swap did not start: ${JSON.stringify(app.exited)}`);
    const after = outsideData();
    const changes = [...before.filter((e) => !after.includes(e)).map((e) => `gone: ${e}`), ...after.filter((e) => !before.includes(e)).map((e) => `new: ${e}`)];
    if (changes.length || sha256(program) !== programBefore) throw new Error(`the refused swap changed the folder: ${changes.slice(0, 20).join(", ") || basename(program)}`);
    if (existsSync(join(updates, `previous-${OLD}`))) throw new Error(`previous-${OLD} was made although nothing was to move`);
    const asarLeft = asarFilesIn(staging);
    if (asarLeft.length) throw new Error(`the refused swap left ${asarLeft.join(", ")}: an Electron app can't remove ${staging} to try again`);
    if (electron && !seen.swapLog.includes("an Electron app can't remove a folder that holds one")) throw new Error(`the swap's log does not say it took the .asar files out of ${staging}`);
    console.log(`nothing moved, ${OLD} runs on, no .asar left in ${staging}`);

    step(`without them, and with a file the system put there: 「无缝更新」 again`);
    for (const f of foreign) rmSync(f, { recursive: true, force: true });
    writeFileSync(systemFile, "the system's\n");
    rmSync(log, { force: true }); // its last line says how the next attempt ends
    const clickedAt = Date.now();
    if (click === "window") {
      const errorBefore = await page.evaluate(CARD_ERROR).catch(() => "");
      await clickSeamless(page);
      await until(`${OLD} to quit`, async () => {
        if (app.exited) return true;
        // OLD unpacks again and starts the swap: when it can't, its card says why and it runs on
        const said = await page.evaluate(CARD_ERROR, { tries: 1 }).catch(() => "");
        if (said && said !== errorBefore) throw new Error(`${OLD}'s second attempt did not start the update: ${said}`);
        return false;
      }, 120_000);
      page.close();
    } else {
      const ok = await swap.attempt(true);
      if (ok.said.ok !== true) throw new Error(`${NEW}'s --apply-update said ${ok.line}`);
      console.log(`${NEW}'s --apply-update: ${ok.line}`);
      stop(app.pid); // the app quits once the helper has taken over (install.rs restart_into)
      await until(`${OLD} to quit`, () => app.exited, 30_000).catch(() => (stop(app.pid, true), until(`${OLD} to quit`, () => app.exited, 15_000)));
    }
    console.log(`${OLD} quit: ${JSON.stringify(app.exited)}`);

    step("the swap");
    let main;
    let back;
    if (readyNew) {
      const ended = await watchSwap({ log, ready: readyNew, oldPids: [old.info.pid], timeout: 180_000 }).catch((e) => {
        seen.swapLog = e.swapLog ?? seen.swapLog;
        throw e;
      });
      seen.swapLog = ended.text;
      console.log(ended.text.trim() || "(the log was gone before it could be read)");
      if (ended.how !== "updated") throw new Error(`the swap ended ${ended.how}, not with "updated"`);
      const info = ended.ready ?? (await readyOf(readyNew, null, `${NEW} to start (the ready file the helper handed it)`, 120_000, [old.info.pid]));
      started.add(info.pid);
      started.add(appProcessOf(info.pid));
      main = { pid: appProcessOf(info.pid), cmd: commandLine(appProcessOf(info.pid)) };
      back = { info, session: await hello(info.url, info.token), how: "ready file" };
      console.log(`${NEW} started by the swap: ${main.pid} ${main.cmd}`);
      if (main.cmd && !/(^|\s)--updated(\s|$)/.test(main.cmd)) throw new Error(`${NEW} was started without --updated: ${main.cmd}`);
    } else {
      const ended = await swapEnded(log, [folder], name);
      console.log(ended.text.trim() || "(the log was gone before it could be read)");
      if (ended.how !== "updated") throw new Error(`the swap ended ${ended.how}, not with "updated"`);
      main = await newStarted([folder], name, updates);
      back = await newBackend({ feed, main, port, dirs, since: clickedAt, program, extra });
    }

    step(`${NEW} runs`);
    await expectNew(back.session, "portable");
    const machine = option("--expect-machine");
    if (machine && machineOf(program) !== machine) throw new Error(`${program} is built for ${machineOf(program)}, not ${machine}`);
    console.log(`${basename(program)} is built for ${machineOf(program)}`);
    if (!existsSync(systemFile) || readFileSync(systemFile, "utf8") !== "the system's\n") throw new Error(`${basename(systemFile)} did not stay where it was`);
    const diff = treeDiff(candidate, folder, ["data", basename(systemFile)]);
    const problems = [...diff.missing.map((f) => `missing: ${f}`), ...diff.different.map((f) => `not ${NEW}'s: ${f}`), ...diff.extra.map((f) => `not in ${NEW} (left by ${OLD}?): ${f}`)];
    if (problems.length) throw new Error(`the folder is not ${NEW} after the update:\n  ${problems.slice(0, 40).join("\n  ")}${problems.length > 40 ? `\n  … ${problems.length - 40} more` : ""}`);
    console.log(`the folder holds exactly ${NEW}'s files, its data folder and ${basename(systemFile)}`);
    expectSeeded(seeded);
    await expectCleanUpdates(updates);
    await nextHop(feed, back.session, updates);
    back.session.close();
    await quit(back.how === "again" ? back.proc.pid : main.pid, [folder]);
  } finally {
    feed.server.close();
  }
  console.log(`\nportable ${OLD} -> ${NEW}: ok`);
}

/** What 「无缝更新」 does on a portable copy where the test does it (src-tauri install.rs
 * replace_portable; the Electron shell's index.ts replacePortable): NEW unpacked into
 * staging-NEW (afresh for each attempt), the plan, and NEW's own program (the Tauri app's
 * executable; the Electron shell's spaceknife-cli) started with --apply-update in the updates
 * folder. attempt(keep): its first line and exit code; `keep` lets it go on by itself. */
function shellSwap({ app, folder, updates, file, electron, env }) {
  const staging = join(updates, `staging-${NEW}`);
  const previous = join(updates, `previous-${OLD}`);
  const plan = join(updates, `update-${NEW}.json`);
  const log = join(updates, `update-${NEW}.log`);
  return {
    async attempt(keep) {
      rmSync(staging, { recursive: true, force: true });
      mkdirSync(staging, { recursive: true });
      unpack(file, staging);
      const tops = readdirSync(staging).filter((n) => n !== "__MACOSX" && statSync(join(staging, n)).isDirectory());
      if (tops.length !== 1) throw new Error(`${basename(file)} is not one folder: ${tops.join(", ")}`);
      const staged = join(staging, tops[0]);
      rmSync(previous, { recursive: true, force: true });
      writeFileSync(plan, `${JSON.stringify({ v: 1, pid: app.pid, app: folder, new: staged, old: previous, exe: programIn(folder), log, keep: ["data"] }, null, 2)}\n`);
      const swapper = electron ? (existsSync(cliIn(staged)) ? cliIn(staged) : helperIn(staged)) : programIn(staged);
      // the Electron shell's own environment names the copy's data folder (SPACEKNIFE_HOME)
      const helper = spawn(swapper, ["--apply-update", plan], { cwd: updates, env: electron ? { ...env, SPACEKNIFE_HOME: join(folder, "data") } : env, detached: keep, stdio: ["ignore", "pipe", "ignore"] });
      const said = await new Promise((ok, no) => {
        const t = setTimeout(() => no(new Error(`${basename(swapper)} --apply-update did not answer`)), 60_000);
        let line = null;
        createInterface({ input: helper.stdout }).once("line", (l) => {
          line = l;
          if (keep) (clearTimeout(t), ok({ line, code: null }));
        });
        // "close", not "exit": its output may still be on its way when it has exited
        helper.once("close", (code) => (clearTimeout(t), ok({ line, code })));
      });
      if (keep) {
        helper.stdout.destroy();
        helper.unref();
      }
      let parsed = {};
      try {
        parsed = JSON.parse(said.line ?? "{}");
      } catch {
        // not its answer
      }
      return { ...said, said: parsed };
    },
  };
}

/** Quit SpaceKnife as a user does (its window closed: SIGTERM, taskkill without /F), then end what
 * is left of it in `where`. */
async function quit(pid, where) {
  stop(pid);
  await until("SpaceKnife to quit", () => !pidAlive(pid), 30_000).catch(() => stop(pid, true));
  await sleep(1000);
  await stopUnder(where);
}

// ------------------------------------------------------------------ the Windows installer

// Tauri's NSIS installer with the forked template (apps/desktop/src-tauri/windows): electron-builder's
// keys, uninstall.exe, the list of installed files (spaceknife.nsh SkWriteManifest)
const UNINSTALL_GUID = "3cb3f1fc-eef4-5813-9008-c29b5510458c";
const UNINSTALLER = "uninstall.exe";
const MANIFEST = "installed-files.txt";

function reg(argv) {
  const r = spawnSync("reg.exe", argv, { encoding: "utf8" });
  return r.status === 0 ? r.stdout : "";
}

function installLocation(hive) {
  const text = reg(["query", `${hive}\\Software\\${UNINSTALL_GUID}`, "/v", "InstallLocation", "/reg:64"]);
  return /^\s+InstallLocation\s+REG_\w+\s+(.*)$/m.exec(text)?.[1]?.trim().replace(/^"(.*)"$/, "$1") || null;
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
  return out.filter((e, i) => out.findIndex((o) => o.hive === e.hive && o.location === e.location) === i);
}

function powershell(script) {
  const r = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8" });
  return (r.stdout ?? "").trim();
}
const shortcutTarget = (lnk) => powershell(`(New-Object -ComObject WScript.Shell).CreateShortcut('${lnk.replace(/'/g, "''")}').TargetPath`);
const productVersion = (file) => powershell(`(Get-Item -LiteralPath '${file.replace(/'/g, "''")}').VersionInfo.ProductVersion`);

/** The installer's list of the files it put in the folder: UTF-16 LE with a BOM, CRLF, one path
 * relative to the folder per line (folders end with \). */
const readList = (file) =>
  readFileSync(file)
    .toString("utf16le")
    .replace(/^\uFEFF/, "")
    .split("\r\n")
    .filter(Boolean);
const writeList = (file, lines) => writeFileSync(file, Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(lines.map((l) => `${l}\r\n`).join(""), "utf16le")]));

function run(file, argv, what) {
  console.log(`> ${basename(file)} ${argv.join(" ")}`);
  const r = spawnSync(file, argv, { stdio: "inherit", timeout: 600_000 });
  if (r.status !== 0) throw new Error(`${what}: exit code ${r.status} ${r.error ?? ""}`);
}

async function nsis() {
  if (!WIN) throw new Error("the installer leg runs on Windows");
  const scope = "/currentuser";
  seen.installLog = true;
  step(`install ${OLD} (${scope})`);
  run(oldPath, ["/S", scope], `installing ${OLD}`);
  const before = await until(`${OLD}'s uninstall entry`, () => uninstallEntries().find((e) => e.version === OLD), 60_000);
  const dir = installLocation(before.hive);
  if (!dir) throw new Error(`${OLD} wrote no InstallLocation in ${before.hive}\\Software\\${UNINSTALL_GUID}`);
  roots.add(dir);
  const program = join(dir, "SpaceKnife.exe");
  if (!existsSync(program) || !existsSync(join(dir, UNINSTALLER))) throw new Error(`${OLD} is not in ${dir}: ${existsSync(dir) ? readdirSync(dir).join(", ") : "(no folder)"}`);
  console.log(`${OLD} in ${dir} (${before.hive}, ${productVersion(program)})`);
  const local = process.env.LOCALAPPDATA;
  const dataDir = join(local, "SpaceKnife");
  const updates = join(dataDir, "updates");
  const shellDir = join(local, "io.github.duskxi.spaceknife"); // Tauri's app_local_data_dir: the window's profile
  seen.folder = dir;
  seen.updates = updates;
  roots.add(updates);
  // a file only OLD had (on the list OLD's installer wrote), the user's files (on no list)
  const list = join(dir, MANIFEST);
  if (!existsSync(list)) throw new Error(`${OLD} wrote no ${list}`);
  const stale = `ui\\stale-from-${OLD}.js`;
  writeList(list, [stale, ...readList(list)]);
  const oldList = readList(list);
  writeFileSync(join(dir, stale), "//\n");
  const mine = ["keep-me.txt", "ui\\keep-me-too.txt"];
  for (const f of mine) writeFileSync(join(dir, f), "the user's\n");
  const seeded = seed({ data: dataDir, cache: join(dataDir, "Cache"), updates });
  const feed = await serveFeed();
  try {
    step(`start ${OLD}`);
    const ready = join(work, "ready.json");
    const port = await freePort();
    const extra = webview2Env(port);
    webview2Policy(port);
    const startedAt = Date.now();
    const app = start(program, [], appEnv(feed.url, ready, extra));
    const old = await oldRuns(ready, app, "nsis");
    const file = await expectFound(old.session, feed);
    old.session.close();
    let page = null;
    try {
      page = await openWindow(OLD, { port, dirs: [shellDir], since: startedAt, proc: app, timeout: 60_000 });
    } catch (e) {
      if (app.exited) throw e;
      console.log(`\n!! ${OLD}'s window has no DevTools port the test can reach here (${e.message}): 「无缝更新」 is done by the test as the button does it (install.rs), not clicked\n`);
    }
    const clickedAt = Date.now();
    // the test's 「无缝更新」: the ready file goes to the installer, which starts NEW with its environment
    const readyNew = page ? null : join(work, "ready-new.json");
    if (page) {
      step(`click 「无缝更新」 in ${OLD}'s window`);
      await clickSeamless(page);
    } else {
      // install.rs Step::Silent: the downloaded installer with 0.3.4's arguments (nsis_update_args;
      // the folder is under LOCALAPPDATA), started on its own, then the app quits
      const argv = ["--updated", "/S", "--force-run", "/currentuser"];
      step(`「无缝更新」 as ${OLD}'s button does it: ${basename(file)} ${argv.join(" ")}, then ${OLD} quits`);
      spawn(file, argv, { cwd: dirname(program), detached: true, stdio: "ignore", env: appEnv(feed.url, readyNew, extra) }).unref();
      stop(app.pid);
    }
    await until(`${OLD} to quit`, () => app.exited, 90_000);
    page?.close();
    console.log(`${OLD} quit: ${JSON.stringify(app.exited)}`);

    step(`${NEW}'s installer, started by ${OLD}, then ${NEW}`);
    const main = await until(`${NEW} to be started by its installer (--updated)`, () => updatedApps([dir], "SpaceKnife.exe")[0], 300_000);
    started.add(main.pid);
    console.log(`${NEW} started by the installer: ${main.pid} ${main.cmd}`);
    await until("the installer to end", () => !processesUnder([updates]).some((p) => /-Setup-/i.test(p.cmd)), 120_000);
    let back;
    if (readyNew) {
      const info = await readyOf(readyNew, null, `${NEW} to start (the ready file the installer handed it)`, 120_000, [old.info.pid]);
      started.add(info.pid);
      back = { info, session: await hello(info.url, info.token), how: "ready file" };
    } else back = await newBackend({ feed, main, port, dirs: [shellDir], since: clickedAt, program, extra });

    step(`${NEW} is installed over ${OLD}`);
    await expectNew(back.session, "nsis");
    const entries = uninstallEntries();
    if (entries.length !== 1) throw new Error(`uninstall entries: ${JSON.stringify(entries)}`);
    const [entry] = entries;
    if (entry.version !== NEW || entry.hive !== before.hive) throw new Error(`the uninstall entry: ${JSON.stringify(entry)}`);
    const now = installLocation(entry.hive);
    if (!now || resolve(now).toLowerCase() !== resolve(dir).toLowerCase()) throw new Error(`installed in ${now}, not ${dir}`);
    if (!entry.uninstall?.toLowerCase().includes(`\\${UNINSTALLER}`)) throw new Error(`the uninstall entry runs ${entry.uninstall}, not ${UNINSTALLER}`);
    for (const f of [UNINSTALLER, "spaceknife-cli.exe", "SpaceKnife.com", join("ui", "index.html")]) if (!existsSync(join(dir, f))) throw new Error(`no ${f} in ${dir} after the update`);
    if (existsSync(`${dir}.sk-staging`)) throw new Error(`the installer's staging folder ${dir}.sk-staging is left`);
    const version = productVersion(program);
    if (!version.startsWith(NEW)) throw new Error(`${program} is ${version}, not ${NEW}`);
    // the list is NEW's: what it names is there, nothing of OLD's list that NEW lacks is left, the
    // user's files stay, and nothing else is in the folder
    const newList = readList(list);
    if (!newList.includes("SpaceKnife.exe") || !newList.includes("ui\\index.html")) throw new Error(`${list} does not list the program and its interface`);
    if (newList.includes(stale) || existsSync(join(dir, stale))) throw new Error(`${stale}, on ${OLD}'s list only, is ${existsSync(join(dir, stale)) ? "still there" : "on the new list"}`);
    const files = (l) => l.filter((f) => !f.endsWith("\\"));
    const missing = files(newList).filter((f) => !existsSync(join(dir, f)));
    const leftOld = files(oldList).filter((f) => !newList.includes(f) && existsSync(join(dir, f)));
    const mineGone = mine.filter((f) => !existsSync(join(dir, f)));
    const known = new Set([...files(newList), ...mine, MANIFEST, UNINSTALLER].map((f) => f.toLowerCase()));
    const extraFiles = treeOf(dir)
      .filter((e) => e.kind === "file")
      .map((e) => relative(dir, e.path))
      .filter((f) => !known.has(f.toLowerCase()));
    const problems = [...missing.map((f) => `listed, missing: ${f}`), ...leftOld.map((f) => `${OLD}'s, left: ${f}`), ...mineGone.map((f) => `the user's, gone: ${f}`), ...extraFiles.map((f) => `on no list: ${f}`)];
    if (problems.length) throw new Error(`${dir} after the update:\n  ${problems.slice(0, 40).join("\n  ")}`);
    console.log(`${dir}: ${files(newList).length} files on ${NEW}'s list, all there; ${OLD}'s ${stale} gone; the user's files kept (${version})`);
    const startMenu = join(process.env.APPDATA, "Microsoft", "Windows", "Start Menu", "Programs", "SpaceKnife.lnk");
    const desktop = join(process.env.USERPROFILE, "Desktop", "SpaceKnife.lnk");
    for (const lnk of [startMenu, desktop]) {
      const target = existsSync(lnk) ? shortcutTarget(lnk) : "";
      if (resolve(target || ".").toLowerCase() !== resolve(program).toLowerCase()) throw new Error(`${lnk} points at ${target || "(missing)"}`);
    }
    console.log("the shortcuts point at the program");
    expectSeeded(seeded);
    await expectCleanUpdates(updates);
    await nextHop(feed, back.session, updates);
    back.session.close();
    await quit(back.how === "again" ? back.proc.pid : main.pid, [dir]);

    step(`uninstall ${NEW} (cleaning up)`);
    run(join(dir, UNINSTALLER), ["/S", scope], "uninstalling");
    await untilUninstalled(dir);
    if (uninstallEntries().length) throw new Error(`an uninstall entry is left: ${JSON.stringify(uninstallEntries())}`);
  } finally {
    feed.server.close();
  }
  console.log(`\nWindows installer ${OLD} -> ${NEW} (${scope}): ok`);
}

// ------------------------------------------------------------------ the AppImage

/** The AppImage's app started with --updated: the program in its mount (`spaceknife`, not
 * Chromium's --type= processes) whose parent or child is the AppImage's runtime (the process whose
 * command line still names `target`, serving the mount), the runtime last. Not by the program's
 * environment: Chromium writes its process title over it. */
function appImageApps(target) {
  const all = [];
  for (const d of readdirSync("/proc")) {
    const pid = Number(d);
    if (!pid || pid === process.pid) continue;
    try {
      const cmd = readFileSync(`/proc/${pid}/cmdline`, "utf8").replaceAll("\0", " ").trim();
      const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
      all.push({ pid, cmd, ppid: Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1]) });
    } catch {
      // another user's, or gone
    }
  }
  const updated = (p) => /(^|\s)--updated(\s|$)/.test(p.cmd) && !/--type=/.test(p.cmd);
  const runtimes = all.filter((p) => updated(p) && argv0(p.cmd) === target);
  const parent = new Map(all.map((p) => [p.pid, p.ppid]));
  const up = (pid) => {
    const out = [];
    for (let i = 0; i < 5 && parent.get(pid) > 1; i++) out.push((pid = parent.get(pid)));
    return out;
  };
  // the runtime's ancestors (it may serve the mount as a child of the program) and descendants
  const above = new Set(runtimes.flatMap((r) => up(r.pid)));
  const runtime = new Set(runtimes.map((r) => r.pid));
  const apps = all.filter((p) => updated(p) && basename(argv0(p.cmd)) === "spaceknife" && !runtime.has(p.pid) && (above.has(p.pid) || up(p.pid).some((a) => runtime.has(a))));
  return [...apps, ...runtimes];
}

async function appimage() {
  const home = join(work, "home");
  mkdirSync(home);
  const homeEnv = { HOME: home, XDG_CONFIG_HOME: join(home, ".config"), XDG_DATA_HOME: join(home, ".local", "share"), XDG_CACHE_HOME: join(home, ".cache") };
  const target = join(work, basename(oldPath)); // the user's copy, under its own name
  copyFileSync(oldPath, target);
  execFileSync("chmod", ["755", target]);
  const updates = join(home, ".local", "share", "SpaceKnife", "updates");
  seen.updates = updates;
  const seeded = seed({ data: join(home, ".local", "share", "SpaceKnife"), config: join(home, ".config", "SpaceKnife"), cache: join(home, ".cache", "SpaceKnife") });
  const feed = await serveFeed();
  try {
    step(`start the ${OLD} AppImage`);
    const ready = join(work, "ready.json");
    const startedAt = Date.now();
    const app = start(target, [...ELECTRON_ARGS, "--no-sandbox"], appEnv(feed.url, ready, homeEnv));
    const old = await oldRuns(ready, app, "appimage");
    await expectFound(old.session, feed);
    old.session.close();
    step(`click 「无缝更新」 in ${OLD}'s window`);
    const page = await openWindow(OLD, { dirs: [join(home, ".config", "SpaceKnife", "electron")], since: startedAt, proc: app });
    await clickSeamless(page);
    await until(`${OLD} to quit`, async () => {
      if (app.exited) return true;
      const said = await page.evaluate(CARD_ERROR, { tries: 1 }).catch(() => "");
      if (said) throw new Error(`${OLD} did not start the update: ${said}`);
      return false;
    }, 120_000);
    page.close();
    console.log(`${OLD} quit: ${JSON.stringify(app.exited)}`);

    step(`${NEW} in the AppImage's place`);
    // OLD asked the new file --startup-check as $APPIMAGE.new, kept the old one as .previous
    if (sha256(target) !== feed.digest) throw new Error(`${target} is not ${NEW_NAME}`);
    if (existsSync(`${target}.new`)) throw new Error(`${target}.new is left`);
    if (!existsSync(`${target}.previous`) || sha256(`${target}.previous`) !== sha256(oldPath)) throw new Error(`${target}.previous is not ${OLD}'s AppImage`);
    console.log(`${basename(target)} is ${NEW_NAME}, ${basename(target)}.previous is ${OLD}'s, no .new`);
    const main = await until(`${NEW} to run (--updated)`, () => appImageApps(target)[0], 120_000);
    started.add(main.pid);
    console.log(`${NEW} started by ${OLD}: ${main.pid} ${main.cmd}`);
    // NEW's backend runs once it has cleaned the updates folder of the file OLD downloaded
    await until(`${NEW} to clean the updates folder`, () => !existsSync(join(updates, NEW_NAME)), 120_000);
    const back = await newBackend({ feed, main, program: target, argv: ["--no-sandbox"], extra: homeEnv });
    await expectNew(back.session, "appimage");
    expectSeeded(seeded);
    await expectCleanUpdates(updates);
    await nextHop(feed, back.session, updates);
    back.session.close();
    await quit(back.proc?.pid ?? main.pid, [work]);
  } finally {
    feed.server.close();
  }
  console.log(`\nAppImage ${OLD} -> ${NEW}: ok`);
}

// ------------------------------------------------------------------ deb, rpm

/** Where the deb and the rpm put the program. */
const PACKAGE_ROOTS = ["/opt/SpaceKnife", "/usr/bin/spaceknife", "/usr/lib/spaceknife"];

function lstatSafe(p) {
  try {
    return lstatSync(p);
  } catch {
    return null;
  }
}

function shown(lines, max = 60) {
  const out = lines.slice(0, max);
  if (lines.length > max) out.push(`… ${lines.length - max} more`);
  return out.map((l) => `  ${l}`).join("\n");
}

/** deb / rpm: OLD finds, downloads and checks NEW's package; the test does what the system's
 * installer does with it (OLD's card opens it there: the deb from a copy in the download folder). */
async function packaged(kind) {
  const root = process.getuid?.() === 0;
  const sudo = root ? [] : ["sudo"];
  const sh = (argv, what) => {
    console.log(`> ${argv.join(" ")}`);
    const r = spawnSync(argv[0], argv.slice(1), { stdio: "inherit", timeout: 900_000 });
    if (r.status !== 0) throw new Error(`${what}: exit code ${r.status}`);
  };
  const query = () =>
    kind === "deb"
      ? spawnSync("dpkg-query", ["-W", "-f", "${Package} ${Version}\\n", "spaceknife", "space-knife"], { encoding: "utf8" }).stdout.trim().split("\n").filter((l) => /\d/.test(l))
      : spawnSync("rpm", ["-q", "--qf", "%{NAME} %{VERSION}\\n", "spaceknife", "space-knife"], { encoding: "utf8" }).stdout.trim().split("\n").filter((l) => l && !l.includes("not installed"));
  const owned = () => {
    const r = kind === "deb" ? spawnSync("dpkg-query", ["-L", "spaceknife"], { encoding: "utf8" }) : spawnSync("rpm", ["-ql", "spaceknife"], { encoding: "utf8" });
    if (r.status !== 0) throw new Error(`the package's files: exit ${r.status} ${r.stderr ?? ""}`);
    return r.stdout.split("\n").filter((l) => l.startsWith("/"));
  };
  const tool = kind === "deb" ? ["apt-get", "install", "-y"] : ["dnf", "install", "-y"];
  const bin = "/usr/bin/spaceknife";
  for (const r of PACKAGE_ROOTS) roots.add(r);

  step(`install ${OLD}'s ${kind}`);
  sh([...sudo, ...tool, oldPath], `installing ${OLD}'s ${kind}`);
  if (JSON.stringify(query()) !== JSON.stringify([`spaceknife ${OLD.replace("-", "~")}`])) throw new Error(`installed: ${JSON.stringify(query())}`);
  // OLD's files (not its folders): none may be left that NEW's package does not have
  const oldFiles = owned().filter((p) => lstatSafe(p) && !lstatSafe(p).isDirectory());
  console.log(`installed: ${query().join(", ")} (${oldFiles.length} files)`);
  const home = join(work, "home");
  const homeEnv = { HOME: home, XDG_CONFIG_HOME: join(home, ".config"), XDG_DATA_HOME: join(home, ".local", "share"), XDG_CACHE_HOME: join(home, ".cache") };
  const updates = join(home, ".local", "share", "SpaceKnife", "updates");
  seen.updates = updates;
  const seeded = seed({ data: join(home, ".local", "share", "SpaceKnife"), config: join(home, ".config", "SpaceKnife"), cache: join(home, ".cache", "SpaceKnife") });
  // the rpm is the Electron shell: as root (the container) Chromium wants --no-sandbox
  const argv = kind === "rpm" && root ? ["--no-sandbox"] : [];
  const feed = await serveFeed();
  try {
    step(`start ${OLD}`);
    const ready = join(work, "ready.json");
    const app = start(bin, argv, appEnv(feed.url, ready, homeEnv));
    const old = await oldRuns(ready, app, kind);
    const file = await expectFound(old.session, feed);
    old.session.close();
    await quitPackaged(app, OLD);

    step(`install ${basename(file)}, the file ${OLD} downloaded (what the system's installer does with it)`);
    sh([...sudo, ...tool, file], `installing ${NEW}'s ${kind}`);
    const now = query();
    if (now.length !== 1 || now[0] !== `spaceknife ${NEW.replace("-", "~")}`) throw new Error(`installed packages: ${JSON.stringify(now)}`);
    const newFiles = new Set(owned());
    const left = oldFiles.filter((p) => !newFiles.has(p) && lstatSafe(p));
    if (left.length) throw new Error(`${OLD}'s files left after the upgrade:\n${shown(left)}`);
    if (!existsSync(bin)) throw new Error(`no ${bin} after the upgrade`);
    const desktops = readdirSync("/usr/share/applications").filter((n) => /spaceknife/i.test(n));
    if (desktops.length !== 1) throw new Error(`desktop entries: ${desktops.join(", ")}`);
    if (kind === "rpm") {
      // the Electron shell, laid out as 0.3.4's: /usr/bin/spaceknife the alternatives group's link
      // to /opt/SpaceKnife/spaceknife; everything in /opt/SpaceKnife the package's
      if (realpathSync(bin) !== "/opt/SpaceKnife/spaceknife") throw new Error(`${bin} leads to ${realpathSync(bin)}`);
      if (readlinkSync(bin) !== "/etc/alternatives/spaceknife" || !alternativesGroup("spaceknife")?.includes("/opt/SpaceKnife/spaceknife")) throw new Error(`${bin} is not the alternatives group's link: ${readlinkSync(bin)}`);
      const strays = notOwned("/opt/SpaceKnife", newFiles);
      if (strays.length) throw new Error(`in /opt/SpaceKnife but not the package's:\n${shown(strays)}`);
    }
    console.log(`one package (${now[0]}); none of ${OLD}'s files left; ${bin} -> ${realpathSync(bin)}; ${desktops[0]}`);

    step(`start ${NEW}`);
    const ready2 = join(work, "ready-new.json");
    const app2 = start(bin, argv, appEnv(feed.url, ready2, homeEnv));
    const info = await readyOf(ready2, app2, `${NEW} to start (its ready file)`);
    started.add(info.pid);
    const session = await hello(info.url, info.token);
    await expectNew(session, kind);
    expectSeeded(seeded);
    await expectCleanUpdates(updates);
    await nextHop(feed, session, updates);
    session.close();
    if (kind === "rpm") {
      const strays = notOwned("/opt/SpaceKnife", new Set(owned()));
      if (strays.length) throw new Error(`while it runs, in /opt/SpaceKnife but not the package's:\n${shown(strays)}`);
    }
    await quitPackaged(app2, NEW);
  } finally {
    feed.server.close();
  }
  step(`remove ${kind} (cleaning up)`);
  sh([...sudo, ...(kind === "deb" ? ["apt-get", "remove", "-y", "spaceknife"] : ["dnf", "remove", "-y", "spaceknife"])], "removing");
  console.log(`\n${kind} ${OLD} -> ${NEW}: ok`);
}

/** Quit the app as a user does before its package is replaced: SIGTERM, then nothing may run from
 * the package's folders (zombies aside: a container's first process reaps none). */
async function quitPackaged(app, version) {
  stop(app.pid);
  await until(`${version} to quit on SIGTERM`, () => app.exited, 60_000);
  const left = () => processesUnder(PACKAGE_ROOTS).filter((p) => !p.cmd.startsWith("[Z]"));
  await until("nothing to run from the package's folders", () => left().length === 0, 30_000).catch((e) => {
    throw new Error(`${e.message}, after ${version} quit: ${left().map((p) => `${p.pid} ${p.cmd}`).join("; ")}`);
  });
  console.log(`${version} quit (${JSON.stringify(app.exited)})`);
}

// ------------------------------------------------------------------ the disk image

/** dmg: OLD finds, downloads and checks NEW's disk image (its card opens it); the user drags the
 * app over the old one (Finder's 「替换」: the old bundle goes, the new one is copied). */
async function dmg() {
  const apps = join(work, "Applications");
  mkdirSync(apps);
  const bundle = join(apps, "SpaceKnife.app");
  const attach = (image) => {
    const mnt = mkdtempSync(join(work, "mnt-"));
    execFileSync("hdiutil", ["attach", "-nobrowse", "-readonly", "-mountpoint", mnt, image], { stdio: "inherit" });
    return mnt;
  };
  const detach = (mnt) => execFileSync("hdiutil", ["detach", mnt], { stdio: "inherit" });
  const install = (image) => {
    const mnt = attach(image);
    try {
      rmSync(bundle, { recursive: true, force: true });
      execFileSync("/usr/bin/ditto", [join(mnt, "SpaceKnife.app"), bundle], { stdio: "inherit" });
      const diff = treeDiff(join(mnt, "SpaceKnife.app"), bundle);
      const problems = [...diff.missing, ...diff.different, ...diff.extra];
      if (problems.length) throw new Error(`${bundle} is not the disk image's app: ${problems.slice(0, 20).join(", ")}`);
    } finally {
      detach(mnt);
    }
  };
  step(`${OLD}'s disk image: the app into ${apps}`);
  install(oldPath);
  // the backend's folders here, through XDG_* (paths.rs: macOS takes them too)
  const xdg = join(work, "xdg");
  const homeEnv = { XDG_DATA_HOME: join(xdg, "data"), XDG_CONFIG_HOME: join(xdg, "config"), XDG_CACHE_HOME: join(xdg, "cache") };
  const updates = join(xdg, "data", "SpaceKnife", "updates");
  seen.updates = updates;
  const seeded = seed({ data: join(xdg, "data", "SpaceKnife"), config: join(xdg, "config", "SpaceKnife"), cache: join(xdg, "cache", "SpaceKnife") });
  const exe = join(bundle, "Contents", "MacOS", "SpaceKnife");
  const feed = await serveFeed();
  try {
    step(`start ${OLD}`);
    const ready = join(work, "ready.json");
    const app = start(exe, [], appEnv(feed.url, ready, homeEnv));
    const old = await oldRuns(ready, app, "dmg");
    const file = await expectFound(old.session, feed);
    old.session.close();
    stop(app.pid);
    await until(`${OLD} to quit`, () => app.exited, 60_000).catch(() => (stop(app.pid, true), until(`${OLD} to quit`, () => app.exited, 15_000)));

    step(`${basename(file)}, the file ${OLD} downloaded: its app over the old one`);
    install(file);
    const plist = join(bundle, "Contents", "Info.plist");
    const read = (key) => execFileSync("/usr/libexec/PlistBuddy", ["-c", `Print :${key}`, plist], { encoding: "utf8" }).trim();
    if (read("CFBundleIdentifier") !== "io.github.duskxi.spaceknife") throw new Error(`bundle id ${read("CFBundleIdentifier")}`);
    if (read("CFBundleShortVersionString") !== NEW) throw new Error(`version ${read("CFBundleShortVersionString")}`);
    if (read("CFBundleExecutable") !== "SpaceKnife") throw new Error(`executable ${read("CFBundleExecutable")}`);
    execFileSync("codesign", ["--verify", "--deep", "--strict", bundle], { stdio: "inherit" });
    console.log(`${bundle}: ${NEW}, exactly the disk image's app, signed`);

    step(`start ${NEW}`);
    const ready2 = join(work, "ready-new.json");
    const app2 = start(exe, [], appEnv(feed.url, ready2, homeEnv));
    const info = await readyOf(ready2, app2, `${NEW} to start (its ready file)`);
    started.add(info.pid);
    const session = await hello(info.url, info.token);
    await expectNew(session, "dmg");
    expectSeeded(seeded);
    await expectCleanUpdates(updates);
    await nextHop(feed, session, updates);
    session.close();
    await quit(app2.pid, [work]);
  } finally {
    feed.server.close();
  }
  console.log(`\ndmg ${OLD} -> ${NEW}: ok`);
}

// ------------------------------------------------------------------ on the way out

function tail(file, n = 40) {
  try {
    const buf = readFileSync(file);
    const text = buf[0] === 0xff && buf[1] === 0xfe ? buf.toString("utf16le").replace(/^\uFEFF/, "") : buf.toString("utf8");
    return text.trimEnd().split(/\r?\n/).slice(-n).join("\n");
  } catch {
    return null;
  }
}

function find(dir, re, depth = 5) {
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

/** What there is to know when a leg fails. */
function diagnose() {
  const lines = ["---- diagnosis"];
  const say = (title, body) => body != null && lines.push(`-- ${title}`, String(body).trimEnd() || "(empty)");
  if (seen.updates) {
    const log = join(seen.updates, `update-${NEW}.log`);
    say(`the swap's log as last read (${log})`, seen.swapLog || "(never read)");
    say("the swap's log now", tail(log, 80) ?? "(gone)");
    say(`the updates folder ${seen.updates}`, listing(seen.updates));
    const note = join(seen.updates, `rollback-${NEW}.json`);
    if (existsSync(note)) say(`the rollback note ${note}`, tail(note, 40));
  }
  if (seen.folder) say(`the app folder ${seen.folder}`, listing(seen.folder));
  const running = processesUnder([...roots]);
  say("processes running from the test's folders", running.map((p) => `${p.pid} ${p.cmd}`).join("\n") || "(none)");
  const logs = [...new Set(seen.data)].flatMap((d) => find(d, /\.log$/i)).filter((f) => !/[\\/]updates[\\/]/.test(f));
  logs.sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  for (const f of logs.slice(0, 4)) say(`log ${f}`, tail(f, 40));
  if (seen.installLog && process.env.TEMP) say("the installer's log", tail(join(process.env.TEMP, "SpaceKnife-install.log"), 60));
  lines.push("----");
  console.error(lines.join("\n"));
}

async function cleanup() {
  for (const p of procs) if (!p.exited) stop(p.pid, true);
  for (const pid of started) if (pidAlive(pid)) stop(pid, true);
  if (mode === "appimage") for (const p of appImageApps(join(work, basename(oldPath)))) stop(p.pid, true);
  const left = await stopUnder([...roots]);
  if (left.length) console.log(`ended what still ran from the test's folders: ${left.map((p) => `${p.pid} ${p.cmd}`).join("; ")}`);
  if (webview2PolicySet) spawnSync("reg.exe", ["delete", WEBVIEW2_POLICY, "/v", "SpaceKnife.exe", "/f"], { stdio: "ignore" });
}

let leaving = false;
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
      rmSync(work, { recursive: true, force: true, maxRetries: 20, retryDelay: 500 });
    } catch (e) {
      console.log(`(${work} stays behind: ${e.code} ${e.path ?? ""})`);
    }
  }
  if (code !== 0) fail(message, output);
  process.exit(0);
}

const deadline = Number(process.env.SK_E2E_DEADLINE_S || 1500);
const watchdog = setTimeout(() => void leave(1, `the ${mode} leg did not finish within ${deadline} s`), deadline * 1000);
for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => void leave(1, `ended by ${signal}`));

try {
  console.log(`${mode}: ${basename(oldPath)} (${OLD}) -> ${NEW_NAME} (${NEW}), then ${NEXT_NAME}; work folder ${work}`);
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
