// What the packaging tests share: waiting, the backend's JSON-RPC over its WebSocket (Node's own
// WebSocket, no dependencies), ending a process tree, an archive's unpacking; following a portable
// copy's swap, comparing an updated folder with the candidate, finding and ending what a test left
// running in its folders, waiting for an NSIS uninstaller's own copy (unit tests:
// tools/tests/packaging_lib.test.mjs).

import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, lstatSync, readdirSync, readFileSync, readlinkSync } from "node:fs";
import net from "node:net";
import { join } from "node:path";

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
