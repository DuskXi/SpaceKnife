// Smoke test of headless mode: no window, the interface served over HTTP by spaceknife-cli.
// Was packaging/smoke_headless.py (Electron's --headless).
//
//   node packaging/smoke-headless.mjs <unpacked portable folder>/spaceknife-cli[.exe]
//   node packaging/smoke-headless.mjs target/release/spaceknife-cli --ui apps/desktop/.output/public
//
// Starts `spaceknife-cli headless --listen 127.0.0.1:0 [extra args]` with an isolated home and reads
// the address it prints. Then: the page is served; /sk/info answers only with the token; the
// WebSocket on the same address refuses another site's Origin (403) and, with the page's own,
// reaches the backend, whose session.hello takes the token. Finally it stops the program the way a
// terminal does (SIGINT; on Windows, where Node can't send Ctrl+C to another process, the process
// is ended) and checks that the address is closed. No display needed. Exit status 1 on failure.

import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import http from "node:http";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

import { WIN, fail, hello, portOpen, sleep, stop, until } from "./lib.mjs";

const URL_LINE = /(https?):\/\/localhost:(\d+)\/#token=([A-Za-z0-9_%-]+)/;

const [cli, ...extra] = process.argv.slice(2);
if (!cli) fail("usage: node packaging/smoke-headless.mjs <spaceknife-cli> [extra arguments]");
const home = mkdtempSync(join(tmpdir(), "sk-headless-smoke-"));
const env = { ...process.env, SPACEKNIFE_HOME: home, SPACEKNIFE_LANGUAGE: "en", LANG: "en_US.UTF-8" };
delete env.DISPLAY;
delete env.WAYLAND_DISPLAY;
const output = [];
const proc = spawn(resolve(cli), ["headless", "--listen", "127.0.0.1:0", ...extra], { env, stdio: ["ignore", "pipe", "pipe"] });
let exited = null;
proc.on("exit", (code, signal) => (exited = { code, signal }));
let found = null;
for (const s of [proc.stdout, proc.stderr]) {
  s.on("data", (d) => {
    for (const line of String(d).split(/\r?\n/)) {
      if (!line) continue;
      output.push(line);
      found ??= URL_LINE.exec(line.replace(/\x1b\[[0-9;]*m/g, ""));
    }
  });
}

/** GET (or an upgrade attempt) on the server: {status, body}. */
function get(url, headers = {}) {
  return new Promise((resolve, reject) => {
    const req = http.get(url, { headers, timeout: 15_000 }, (res) => {
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () => resolve({ status: res.statusCode, body: Buffer.concat(chunks).toString("utf8") }));
    });
    req.on("upgrade", (res, socket) => (socket.destroy(), resolve({ status: res.statusCode, body: "" })));
    req.on("error", reject);
    req.on("timeout", () => req.destroy(new Error(`${url}: no answer`)));
  });
}

try {
  const m = await until("the address it prints", () => {
    if (exited) throw new Error(`spaceknife-cli exited with ${JSON.stringify(exited)} before printing its address`);
    return found;
  });
  const [, scheme, port, token] = [m[0], m[1], Number(m[2]), decodeURIComponent(m[3])];
  if (scheme !== "http") throw new Error(`expected plain HTTP on 127.0.0.1, got ${scheme}`);
  const base = `http://127.0.0.1:${port}`;
  console.log(`listening on ${base}`);

  const page = await get(`${base}/`);
  if (page.status !== 200 || !/<html/i.test(page.body) || !page.body.includes("<script")) throw new Error(`the page: HTTP ${page.status}`);
  if ((await get(`${base}/sk/info`)).status !== 401) throw new Error("/sk/info answered without the token");
  const info = await get(`${base}/sk/info`, { Authorization: `Bearer ${token}` });
  const platform = JSON.parse(info.status === 200 ? info.body : "{}").platform;
  const want = { win32: "win32", darwin: "darwin" }[process.platform] ?? "linux";
  if (platform !== want) throw new Error(`/sk/info: HTTP ${info.status} ${info.body.slice(0, 200)}`);
  console.log(`/sk/info: ${platform}`);

  // another site's page may not open the WebSocket
  const upgrade = {
    Connection: "Upgrade",
    Upgrade: "websocket",
    "Sec-WebSocket-Version": "13",
    "Sec-WebSocket-Key": randomBytes(16).toString("base64"),
    "Sec-WebSocket-Protocol": "spaceknife.v1",
  };
  const evil = await get(`${base}/v1`, { ...upgrade, Origin: "http://evil.example" });
  if (evil.status !== 403) throw new Error(`another site's WebSocket: HTTP ${evil.status}, not 403`);
  const session = await hello(`ws://127.0.0.1:${port}/v1`, token, { origin: base });
  console.log(`session.hello: ${session.server.name} ${session.server.version}`);
  session.close();

  // stop it as a terminal would
  if (WIN) stop(proc.pid, true);
  else proc.kill("SIGINT");
  await until("spaceknife-cli to stop", () => exited, 30_000);
  if (!WIN && !(exited.code === 0 || exited.code === 130 || exited.signal === "SIGINT")) throw new Error(`stopped with ${JSON.stringify(exited)}`);
  await until("the address to close", async () => !(await portOpen(port)), 10_000);
  console.log(`stopped (${JSON.stringify(exited)}); headless smoke test passed`);
} catch (e) {
  fail(e.message, output);
} finally {
  if (!exited) stop(proc.pid, true);
  for (let i = 0; i < 10; i++) {
    try {
      rmSync(home, { recursive: true, force: true });
      break;
    } catch {
      await sleep(500); // the backend may still hold its log for a moment
    }
  }
}
