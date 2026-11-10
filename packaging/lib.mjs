// What the packaging tests share: waiting, the backend's JSON-RPC over its WebSocket (Node's own
// WebSocket, no dependencies), ending a process tree, an archive's unpacking; following a portable
// copy's swap, comparing an updated folder with the candidate, finding and ending what a test left
// running in its folders, waiting for an NSIS uninstaller's own copy; a window's page over the
// DevTools protocol, a process's command line, the processor a program is built for, the files of
// a release as a test's release list offers them (unit tests: tools/tests/packaging_lib.test.mjs).

import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, lstatSync, readdirSync, readFileSync, readlinkSync, statSync } from "node:fs";
import net from "node:net";
import { join } from "node:path";

import { expected } from "../tools/release.mjs";

export const WIN = process.platform === "win32";
export const MAC = process.platform === "darwin";

export function fail(message, output = []) {
  if (output.length) console.error(["---- output (last 80 lines)", ...output.slice(-80), "----"].join("\n"));
  console.error(`FAIL ${message}`);
  process.exit(1);
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** `check()` until it gives something truthy, at most `timeout` ms. */
export async function until(what, check, timeout = 90_000, every = 500) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await check();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out after ${Math.round(timeout / 1000)} s waiting for ${what}`);
    await sleep(every);
  }
}

export function portOpen(port) {
  return new Promise((resolve) => {
    const s = net.connect({ host: "127.0.0.1", port });
    s.setTimeout(1000);
    s.once("connect", () => (s.destroy(), resolve(true)));
    s.once("error", () => resolve(false));
    s.once("timeout", () => (s.destroy(), resolve(false)));
  });
}

export function pidAlive(pid) {
  if (WIN) {
    const out = spawnSync("tasklist", ["/FI", `PID eq ${pid}`, "/NH", "/FO", "CSV"], { encoding: "utf8" }).stdout ?? "";
    return out.includes(`"${pid}"`);
  }
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return e.code === "EPERM";
  }
}

/** The alternatives group `name` as the system shows it, or null when there is none. Debian's
 * update-alternatives answers --query. On RHEL update-alternatives is a link to `alternatives`
 * (chkconfig), which has no --query (it prints its usage and exits 2) and answers --display: a
 * tool that does not answer is passed over, not taken for "no group". `run`: spawnSync. */
export function alternativesGroup(name, run = spawnSync) {
  for (const [cmd, argv] of [
    ["update-alternatives", ["--query", name]],
    ["alternatives", ["--display", name]],
  ]) {
    const r = run(cmd, argv, { encoding: "utf8" });
    if (r.error) continue; // not this system's tool
    if (r.status === 0 && r.stdout.trim()) return r.stdout;
  }
  return null;
}

/** End a process and its children (`force`: at once; else as closing its window / Ctrl+C would). */
export function stop(pid, force = false) {
  if (WIN) spawnSync("taskkill", [...(force ? ["/F"] : []), "/T", "/PID", String(pid)], { stdio: "ignore" });
  else {
    try {
      process.kill(pid, force ? "SIGKILL" : "SIGTERM");
    } catch {
      /* gone already */
    }
  }
}

/** A JSON-RPC session with the backend: `call(method, params)`, `events` (notifications seen). */
export async function connect(url, { origin, timeout = 30_000 } = {}) {
  const ws = new WebSocket(url, origin ? { protocols: ["spaceknife.v1"], headers: { Origin: origin } } : ["spaceknife.v1"]);
  const pending = new Map();
  const events = [];
  const listeners = new Set();
  let ids = 0;
  ws.onmessage = (m) => {
    if (typeof m.data !== "string") return; // binary frames (views): not for these tests
    const msg = JSON.parse(m.data);
    if (msg.id != null && pending.has(msg.id)) {
      pending.get(msg.id)(msg);
      pending.delete(msg.id);
    } else if (msg.method) {
      events.push(msg);
      for (const l of listeners) l(msg);
    }
  };
  await new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`no connection to ${url}`)), timeout);
    ws.onopen = () => (clearTimeout(t), resolve());
    ws.onerror = () => (clearTimeout(t), reject(new Error(`the WebSocket ${url} was refused`)));
  });
  const call = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = ++ids;
      const t = setTimeout(() => reject(new Error(`${method}: no answer`)), timeout);
      pending.set(id, (msg) => {
        clearTimeout(t);
        if (msg.error) reject(new Error(`${method}: ${JSON.stringify(msg.error)}`));
        else resolve(msg.result);
      });
      ws.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
    });
  return { ws, call, events, onEvent: (l) => listeners.add(l), close: () => ws.close() };
}

/** session.hello with the launch token; returns the session. */
export async function hello(url, token, opts = {}) {
  const s = await connect(url, opts);
  const result = await s.call("session.hello", { token, protocol: "1.1", client: { name: "packaging-test", version: "0", kind: "test" } });
  return { ...s, server: result.server };
}

export function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

/** Unpack a portable archive into `dir` with the tools the app itself uses (install.rs unpack). */
export function unpack(archive, dir) {
  if (MAC) execFileSync("/usr/bin/ditto", ["-x", "-k", archive, dir], { stdio: "inherit" });
  else if (WIN) execFileSync(join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe"), ["-xf", archive, "-C", dir], { stdio: "inherit" });
  else execFileSync("tar", ["-xzf", archive, "-C", dir], { stdio: "inherit" });
}

/** Pack `name` (a folder in `dir`) as the release does (portable.mjs). */
export function pack(dir, name, archive) {
  if (MAC) execFileSync("/usr/bin/ditto", ["-c", "-k", "--sequesterRsrc", "--keepParent", join(dir, name), archive], { stdio: "inherit" });
  else if (WIN) execFileSync(join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe"), ["-a", "-c", "-f", archive, "-C", dir, name], { stdio: "inherit" });
  else execFileSync("tar", ["-czf", archive, "-C", dir, name], { stdio: "inherit" });
}

/** The program in an unpacked portable folder (on macOS inside the app bundle), under the name
 * 0.3.x copies start after an update (apps/desktop/scripts/portable.mjs). */
export function programIn(folder) {
  if (MAC) return join(folder, "SpaceKnife.app", "Contents", "MacOS", "SpaceKnife");
  return join(folder, WIN ? "SpaceKnife.exe" : "spaceknife");
}

/** The swap helper 0.3.x copies run from the new version (a copy of spaceknife-cli). */
export function helperIn(folder) {
  if (MAC) return join(folder, "SpaceKnife.app", "Contents", "Resources", "backend", "spaceknife-backend");
  return join(folder, "resources", "backend", WIN ? "spaceknife-backend.exe" : "spaceknife-backend");
}

export function cliIn(folder) {
  if (MAC) return join(folder, "SpaceKnife.app", "Contents", "MacOS", "spaceknife-cli");
  return join(folder, WIN ? "spaceknife-cli.exe" : "spaceknife-cli");
}

// ------------------------------------------------------------------ the swap of a portable copy

/** How the swap's log (sk_core::update::swap, one timestamped line per step) says it ended, from
 * its last line: "updated" (then the new version starts), "kept" (the old files back, the old
 * version starts), "refused" (the plan, or the new version can't start here: nothing was touched),
 * "start-failed" (the files are in place but the program could not be started); null while it goes
 * on. */
export function swapEnding(text) {
  const last = (text ?? "").trim().split(/\r?\n/).at(-1) ?? "";
  if (/ updated$/.test(last)) return "updated";
  if (last.includes("kept the old version")) return "kept";
  if (last.includes("not started: ")) return "refused";
  if (last.includes("could not start SpaceKnife")) return "start-failed";
  return null;
}

/** Follow the swap of a portable copy until it says how it ended: its log, read every `every` ms
 * (what was read last is kept: the new version removes the log with the rest of its update from the
 * updates folder as soon as it starts, often before a slower poll would see its last line), and
 * the new version's ready file. A ready file with a pid that is none of `oldPids` means the new
 * version runs, which the swap starts only after "updated" (0.3.4, the old version, writes none).
 * Resolves {how, text, ready}; `how` as swapEnding, or "updated" when only the ready file showed it
 * (`cleaned`: the log was gone by then). Rejects after `timeout` ms with what the log said. */
export async function watchSwap({ log, ready, oldPids = [], timeout = 180_000, every = 100 }) {
  let text = "";
  let cleaned = false;
  const readyInfo = () => {
    if (!ready) return null;
    try {
      const r = JSON.parse(readFileSync(ready, "utf8"));
      return r && !oldPids.includes(r.pid) ? r : null;
    } catch {
      return null;
    }
  };
  const deadline = Date.now() + timeout;
  for (;;) {
    try {
      text = readFileSync(log, "utf8");
    } catch {
      if (text) cleaned = true; // read before, gone now
    }
    const how = swapEnding(text);
    if (how) return { how, text, ready: readyInfo(), cleaned };
    const back = readyInfo();
    if (back) return { how: "updated", text, ready: back, cleaned };
    if (Date.now() > deadline) {
      const e = new Error(`timed out after ${Math.round(timeout / 1000)} s waiting for the swap's log to say how it ended (${log}): ${text.trim() ? `it says\n${text.trim()}` : "it is empty or was never written"}`);
      e.swapLog = text;
      throw e;
    }
    await sleep(every);
  }
}

/** The differences between an expected folder (a candidate's unpacked files) and an actual one:
 * entries missing, different (a file's bytes, a link's target, a file where a folder belongs) or
 * not expected at all. `skip`: top-level names left out of the comparison (the data folder). */
export function treeDiff(want, have, skip = []) {
  const out = { missing: [], different: [], extra: [] };
  const kind = (p) => {
    try {
      const s = lstatSync(p);
      return s.isSymbolicLink() ? "link" : s.isDirectory() ? "dir" : "file";
    } catch {
      return null;
    }
  };
  const walk = (rel) => {
    const a = join(want, rel);
    const b = join(have, rel);
    const ka = kind(a);
    const kb = kind(b);
    if (!kb) return out.missing.push(rel);
    if (ka !== kb) return out.different.push(`${rel} (${kb}, want ${ka})`);
    if (ka === "link") {
      if (readlinkSync(a) !== readlinkSync(b)) out.different.push(`${rel} (link to ${readlinkSync(b)}, want ${readlinkSync(a)})`);
    } else if (ka === "file") {
      if (sha256(a) !== sha256(b)) out.different.push(rel);
    } else {
      const names = new Set(readdirSync(a));
      for (const n of readdirSync(b)) if (!names.has(n) && !(rel === "" && skip.includes(n))) out.extra.push(join(rel, n));
      for (const n of [...names].sort()) if (!(rel === "" && skip.includes(n))) walk(join(rel, n));
    }
  };
  walk("");
  return out;
}

/** The folder `dir` and everything under it, itself first, each as `{ path, kind }` (kind "dir",
 * "link" or "file"; a link is not followed), names in order; [] when there is no such folder. */
export function treeOf(dir) {
  const out = [];
  const walk = (path) => {
    let s;
    try {
      s = lstatSync(path);
    } catch {
      return;
    }
    const kind = s.isSymbolicLink() ? "link" : s.isDirectory() ? "dir" : "file";
    out.push({ path, kind });
    if (kind === "dir") for (const n of readdirSync(path).sort()) walk(join(path, n));
  };
  walk(dir);
  return out;
}

/** What is in the folder `dir` (treeOf) that a package does not list (`owned`: the paths `rpm -ql`
 * prints): folders it does not own, which its removal leaves behind; files the app or a script
 * made. As lines "<kind> <path>". */
export function notOwned(dir, owned) {
  return treeOf(dir)
    .filter((e) => !owned.has(e.path))
    .map((e) => `${e.kind} ${e.path}`);
}

/** The built interface in an unpacked portable folder, wherever its kind keeps it: next to the
 * program (the Tauri app on Windows and Linux), in resources/ (the Electron shell) or in the app
 * bundle (macOS); null when there is none. */
export function uiIn(folder) {
  for (const rel of [["ui"], ["resources", "ui"], ["SpaceKnife.app", "Contents", "Resources", "ui"]]) {
    const index = join(folder, ...rel, "index.html");
    if (existsSync(index)) return index;
  }
  return null;
}

/** The app's own process from its backend's (the pid in its ready file): the Tauri app runs the
 * backend in its process, the Electron shell runs spaceknife-cli as its child (then the parent; told
 * apart on Linux, where the Electron shell is built). */
export function appProcessOf(backendPid) {
  if (WIN || MAC) return backendPid;
  try {
    if (readFileSync(`/proc/${backendPid}/comm`, "utf8").trim() !== "spaceknife-cli") return backendPid;
    const stat = readFileSync(`/proc/${backendPid}/stat`, "utf8");
    return Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1]); // the field after the state: ppid
  } catch {
    return backendPid;
  }
}

/** The Electron shell's files in an unpacked portable folder (resources/app.asar; on macOS in the
 * app bundle): the copy is the Electron shell, not the Tauri app. */
export function isElectron(folder) {
  return existsSync(join(folder, "resources", "app.asar")) || existsSync(join(folder, "SpaceKnife.app", "Contents", "Resources", "app.asar"));
}

/** The .asar files below `dir` (treeOf: no link followed; a folder named so is none). Electron's fs
 * takes one for a folder, so an Electron app cannot remove a folder that holds one: 0.3.4 removes
 * its staging folder that way before it unpacks an update again, and a refused swap leaves none
 * there (sk_core::update::swap leave_removable). */
export function asarFilesIn(dir) {
  return treeOf(dir)
    .filter((e) => e.kind === "file" && /\.asar$/i.test(e.path))
    .map((e) => e.path);
}

/** The processes whose program or command line lies in one of `roots` (the test's folders: what
 * it unpacked, installed and started, and whatever those started in turn, the swap helper and the
 * new version among them, in a session of their own): [{pid, cmd}]. This process is never one. */
export function processesUnder(roots) {
  const norm = (p) => (WIN ? p.toLowerCase().replaceAll("/", "\\") : p);
  const inside = roots.filter(Boolean).map(norm);
  const hit = (s) => !!s && inside.some((r) => norm(s).includes(r));
  const out = [];
  if (WIN) {
    const script = "Get-CimInstance Win32_Process | ForEach-Object { '{0}\t{1}\t{2}' -f $_.ProcessId, $_.ExecutablePath, $_.CommandLine }";
    const r = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8", maxBuffer: 64 << 20 });
    for (const line of (r.stdout ?? "").split(/\r?\n/)) {
      const [pid, exe, cmd] = line.split("\t");
      if (Number(pid) && Number(pid) !== process.pid && (hit(exe) || hit(cmd))) out.push({ pid: Number(pid), cmd: cmd || exe });
    }
  } else if (MAC) {
    const r = spawnSync("ps", ["-axww", "-o", "pid=,stat=,command="], { encoding: "utf8", maxBuffer: 64 << 20 });
    for (const line of (r.stdout ?? "").split("\n")) {
      const m = /^\s*(\d+)\s+(\S+)\s+(.*)$/.exec(line);
      if (m && Number(m[1]) !== process.pid && hit(m[3])) out.push({ pid: Number(m[1]), cmd: `[${m[2]}] ${m[3]}` });
    }
  } else {
    for (const d of readdirSync("/proc")) {
      const pid = Number(d);
      if (!pid || pid === process.pid) continue;
      let exe = "";
      let cmd = "";
      let state = "?";
      try {
        exe = readlinkSync(`/proc/${pid}/exe`);
      } catch {
        // another user's, or gone
      }
      try {
        cmd = readFileSync(`/proc/${pid}/cmdline`, "utf8").replaceAll("\0", " ").trim();
        const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
        state = stat.slice(stat.lastIndexOf(")") + 2).split(" ")[0];
      } catch {
        continue;
      }
      if (hit(exe) || hit(cmd)) out.push({ pid, cmd: `[${state}] ${cmd || exe}` });
    }
  }
  return out;
}

/** Until the NSIS uninstaller of the folder `dir` has done its work. Started as Windows starts it
 * (no `_?=`), an NSIS uninstaller copies itself to %TEMP%\~nsu*.tmp\Au_.exe, starts the copy with
 * `_?=<dir>\` and exits before the copy has done anything (NSIS exehead/Main.c), so the exit of
 * the one started says nothing: this waits until no process runs from `dir` or has it on its
 * command line (the copy, and what it starts), at most `timeout` ms. `list`: processesUnder. */
export async function untilUninstalled(dir, { timeout = 180_000, every = 1000, list = processesUnder } = {}) {
  let left = [];
  await until(`the uninstaller of ${dir} to finish`, () => (left = list([dir])).length === 0, timeout, every).catch((e) => {
    if (!left.length) throw e;
    throw new Error(`the uninstaller of ${dir} is still at work after ${Math.round(timeout / 1000)} s: ${left.map((p) => `${p.pid} ${p.cmd}`).join("; ")}`);
  });
}

/** End every process in `roots` (processesUnder), and their children, at once; again while some
 * are left (a parent may start one as it goes). Returns what was found the first time. */
export async function stopUnder(roots, { tries = 5 } = {}) {
  const first = processesUnder(roots);
  let left = first;
  for (let i = 0; i < tries && left.length; i++) {
    for (const p of left) stop(p.pid, true);
    await sleep(500);
    left = processesUnder(roots).filter((p) => !p.cmd.startsWith("[Z]"));
  }
  return first;
}

// ------------------------------------------------------------------ the update tests' other helpers

/** The command line of a process (Windows: CIM; Linux: /proc; macOS: ps), "" when it can't be read. */
export function commandLine(pid) {
  if (WIN) {
    const r = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `(Get-CimInstance Win32_Process -Filter "ProcessId=${Number(pid)}").CommandLine`], { encoding: "utf8" });
    return (r.stdout ?? "").trim();
  }
  if (MAC) return (spawnSync("ps", ["-o", "command=", "-p", String(pid)], { encoding: "utf8" }).stdout ?? "").trim();
  try {
    return readFileSync(`/proc/${pid}/cmdline`, "utf8").replaceAll("\0", " ").trim();
  } catch {
    return "";
  }
}

/** The processor a program is built for, from its header (PE, ELF, Mach-O): x64, arm64, or what
 * the header says. */
export function machineOf(file) {
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

/** Two versions compared (`a` < `b`: negative): the numbers first, then a version without a
 * pre-release part above one with it, then the pre-release parts as text; `-` or `~` before it. */
export function compareVersions(a, b) {
  const split = (v) => {
    const [core, ...pre] = String(v).replace(/^v/, "").split(/[-~]/);
    return { nums: core.split(".").map(Number), pre: pre.join("-") };
  };
  const x = split(a);
  const y = split(b);
  for (let i = 0; i < 3; i++) if ((x.nums[i] ?? 0) !== (y.nums[i] ?? 0)) return (x.nums[i] ?? 0) - (y.nums[i] ?? 0);
  if (x.pre === y.pre) return 0;
  if (!x.pre) return 1;
  if (!y.pre) return -1;
  return x.pre < y.pre ? -1 : 1;
}

/** The files of release `version` as a test's release list offers them, as GitHub's does: every
 * name the release publishes (tools/release.mjs expected; one that came with a later version left
 * out) and its checksum files, each file listed in the one the release lists it in. `real` maps
 * names to files on disk (the version's file for the install under test, its version spelled as
 * the file spells it: deb and rpm write a pre-release's - as ~); every other name is a stand-in of
 * a few bytes of its own, never an installable file: a copy that takes the wrong name downloads
 * something that is not its update (its checksum is listed: the download itself passes), which a
 * test's check of the name catches. `digest(path)`: a real file's SHA-256. Returns
 * [{ name, path, sha256 } | { name, body, sha256 }], the checksum files last. */
export function releaseFiles(version, real = new Map(), digest = sha256) {
  const hash = (body) => createHash("sha256").update(body).digest("hex");
  const out = [];
  const lists = new Map();
  for (const e of expected()) {
    if (e.since && compareVersions(version, e.since) < 0) continue;
    const names = [version, version.replaceAll("-", "~"), version.replaceAll("-", "_")].map((v) => e.template.replace("{v}", v));
    const name = names.find((n) => real.has(n)) ?? names[0];
    let file;
    if (real.has(name)) file = { name, path: real.get(name), sha256: digest(real.get(name)) };
    else {
      const body = Buffer.from(`stand-in for ${name}: not a file of this install\n`);
      file = { name, body, sha256: hash(body) };
    }
    out.push(file);
    if (e.sums) lists.set(e.sums, `${lists.get(e.sums) ?? ""}${file.sha256}  ${name}\n`);
  }
  const unknown = [...real.keys()].filter((n) => !out.some((f) => f.name === n));
  if (unknown.length) throw new Error(`not a file of release ${version}: ${unknown.join(", ")}`);
  for (const [name, text] of [...lists].sort(([a], [b]) => (a < b ? -1 : 1))) {
    const body = Buffer.from(text);
    out.push({ name, body, sha256: hash(body) });
  }
  return out;
}

/** The ports in the DevToolsActivePort files below `roots` (at most `depth` folders down) written
 * at or after `since` (ms), the newest first. Chromium writes one into its profile folder when it
 * listens for the DevTools protocol (an Electron app's userData; WebView2's EBWebView in the
 * window's data folder). */
export function devtoolsPorts(roots, since = 0, depth = 4) {
  const found = [];
  const walk = (dir, left) => {
    let names = [];
    try {
      names = readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of names) {
      const p = join(dir, e.name);
      if (e.isDirectory() && left > 0) walk(p, left - 1);
      else if (e.isFile() && e.name === "DevToolsActivePort") {
        try {
          const m = statSync(p).mtimeMs;
          const port = Number(readFileSync(p, "utf8").split(/\s+/)[0]);
          if (m >= since && port) found.push({ port, m });
        } catch {
          // being written, or gone
        }
      }
    }
  };
  for (const r of roots) walk(r, depth);
  return found.sort((a, b) => b.m - a.m).map((f) => f.port);
}

/** A page of an app's window over the DevTools protocol (Chromium's: an Electron window, or
 * WebView2's when WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS gives it a port). `ports()` gives the ports
 * to try ([] while there are none; it throws to end a wait that is pointless, the app having
 * exited); `accept(evaluate)` says whether a page is the one wanted (the old version's window, not
 * the new one's). Resolves { evaluate, close, port }: evaluate(expression) is its value, promises
 * awaited; a page that stops answering (a reload, a renderer put to sleep while the window is
 * minimized on a desktop nobody looks at) is woken, or found again, and asked again. */
export async function devtoolsPage(ports, { what = "the window", accept = async () => true, timeout = 120_000 } = {}) {
  let ws = null;
  let ids = 0;
  let port = null;
  const pending = new Map();
  const open = (url) =>
    new Promise((ok, no) => {
      const socket = new WebSocket(url);
      socket.onopen = () => ok(socket);
      socket.onerror = () => no(new Error("the DevTools socket was refused"));
    });
  const attach = (socket) => {
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
  };
  const send = (method, params, wait) =>
    new Promise((ok, no) => {
      const id = ++ids;
      const t = setTimeout(() => (pending.delete(id), no(new Error(`no answer to ${method} ${JSON.stringify(params).slice(0, 80)}`))), wait);
      pending.set(id, (msg) => {
        clearTimeout(t);
        const r = msg.result;
        if (msg.error || r?.exceptionDetails) no(new Error(JSON.stringify(msg.error ?? r.exceptionDetails)));
        else ok(r);
      });
      ws.send(JSON.stringify({ id, method, params }));
    });
  const raw = async (expression) => (await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }, 15_000))?.result?.value;
  async function connect() {
    await until(what, async () => {
      for (const p of await ports()) {
        const pages = await fetch(`http://127.0.0.1:${p}/json/list`, { signal: AbortSignal.timeout(5000) })
          .then((r) => r.json())
          .catch(() => []);
        for (const t of pages.filter((t) => t.type === "page" && t.webSocketDebuggerUrl)) {
          let socket;
          try {
            socket = await open(t.webSocketDebuggerUrl);
          } catch {
            continue;
          }
          attach(socket);
          const ok = await accept(raw).catch(() => false);
          if (ok) {
            port = p;
            return true;
          }
          socket.close();
          ws = null;
        }
      }
      return false;
    }, timeout, 1000);
  }
  async function evaluate(expression, { tries = 4 } = {}) {
    for (let i = 1; ; i++) {
      try {
        if (!ws) await connect();
        return await raw(expression);
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
  return { evaluate, close: () => ws?.close(), port: () => port };
}
