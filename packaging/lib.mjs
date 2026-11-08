// What the packaging tests share: waiting, the backend's JSON-RPC over its WebSocket (Node's own
// WebSocket, no dependencies), ending a process tree, an archive's unpacking.

import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
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

/** The program in an unpacked portable folder (on macOS inside the app bundle). */
export function programIn(folder) {
  if (MAC) return join(folder, "SpaceKnife.app", "Contents", "MacOS", "SpaceKnife");
  return join(folder, WIN ? "SpaceKnife.exe" : "spaceknife");
}

export function cliIn(folder) {
  if (MAC) return join(folder, "SpaceKnife.app", "Contents", "MacOS", "spaceknife-cli");
  return join(folder, WIN ? "spaceknife-cli.exe" : "spaceknife-cli");
}
