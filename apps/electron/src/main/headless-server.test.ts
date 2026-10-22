import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { type Server, type Socket, connect, createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { fileIn } from "./files";
import { HEADLESS_ORIGIN } from "./headless";
import { type HeadlessServer, bearerOk, createHeadlessServer, sameOrigin, servedIndex } from "./headless-server";

const TOKEN = "t".repeat(32);
const INDEX = `<!doctype html><meta http-equiv="Content-Security-Policy" content="default-src 'self'; connect-src 'self' ws://127.0.0.1:*; style-src 'self'; frame-src skplugin:" /><div id="root"></div>`;

/** A stand-in for the backend: records the upgrade request it gets, answers 101, then echoes. */
function fakeBackend(): Promise<{ server: Server; port: number; requests: string[] }> {
  const requests: string[] = [];
  const server = createServer((sock: Socket) => {
    let head = "";
    let upgraded = false;
    sock.on("data", (chunk) => {
      if (upgraded) return void sock.write(chunk); // echo
      head += chunk.toString("latin1");
      const end = head.indexOf("\r\n\r\n");
      if (end < 0) return;
      requests.push(head.slice(0, end));
      upgraded = true;
      sock.write("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n");
      const rest = head.slice(end + 4);
      if (rest) sock.write(rest);
    });
  });
  return new Promise((done) => server.listen(0, "127.0.0.1", () => done({ server, port: (server.address() as { port: number }).port, requests })));
}

/** A raw upgrade request to the headless server; resolves with everything it sent back once `until` matches. */
function upgrade(port: number, headers: Record<string, string>, send = "", until = /\r\n\r\n/): Promise<string> {
  return new Promise((done, fail) => {
    const sock = connect(port, "127.0.0.1", () => {
      const lines = ["GET /v1 HTTP/1.1", ...Object.entries(headers).map(([k, v]) => `${k}: ${v}`)];
      sock.write(`${lines.join("\r\n")}\r\n\r\n${send}`);
    });
    let got = "";
    const timer = setTimeout(() => (sock.destroy(), done(got)), 2000);
    sock.on("data", (c) => {
      got += c.toString("latin1");
      if (until.test(got)) {
        clearTimeout(timer);
        sock.destroy();
        done(got);
      }
    });
    sock.on("error", fail);
    sock.on("close", () => (clearTimeout(timer), done(got)));
  });
}

describe("headless server", () => {
  let dir: string;
  let backend: Awaited<ReturnType<typeof fakeBackend>>;
  let headless: HeadlessServer;
  let port: number;
  let base: string;
  const opened: string[] = [];

  beforeAll(async () => {
    dir = mkdtempSync(join(tmpdir(), "sk-headless-"));
    mkdirSync(join(dir, "renderer", "assets"), { recursive: true });
    writeFileSync(join(dir, "renderer", "index.html"), INDEX);
    writeFileSync(join(dir, "renderer", "assets", "index-abc.js"), "console.log(1)");
    writeFileSync(join(dir, "secret.txt"), "outside");
    // a plugin with a web panel, and the panel SDK
    mkdirSync(join(dir, "plugins", "example.panel", "web"), { recursive: true });
    writeFileSync(join(dir, "plugins", "example.panel", "plugin.toml"), 'id = "example.panel"');
    writeFileSync(join(dir, "plugins", "example.panel", "web", "index.html"), "<html><head></head><body>panel</body></html>");
    mkdirSync(join(dir, "sdk"));
    writeFileSync(join(dir, "sdk", "bridge.js"), "export default {};");
    writeFileSync(join(dir, "sdk", "guard.js"), "/* guard */");
    backend = await fakeBackend();
    headless = createHeadlessServer({
      rendererDir: join(dir, "renderer"),
      backendUrl: `ws://127.0.0.1:${backend.port}/v1`,
      token: TOKEN,
      info: { platform: "linux", version: "9.9.9", hostname: "nas", home: "/home/lenny", secure: false },
      tls: null,
      onSocket: (event, remote) => opened.push(`${event} ${remote}`),
      plugins: { dirs: { installed: join(dir, "plugins"), builtin: null, trust: null }, sdkDir: join(dir, "sdk") },
    });
    port = await headless.listen({ host: "127.0.0.1", port: 0 });
    base = `http://127.0.0.1:${port}`;
  });

  afterAll(async () => {
    await headless.close();
    backend.server.close();
    rmSync(dir, { recursive: true, force: true });
  });

  it("serves the page with its policy naming this server, never cached nor framed", async () => {
    const res = await fetch(`${base}/`);
    expect(res.status).toBe(200);
    expect(res.headers.get("cache-control")).toBe("no-store");
    expect(res.headers.get("content-security-policy")).toBe("frame-ancestors 'none'");
    expect(res.headers.get("x-frame-options")).toBe("DENY");
    const html = await res.text();
    expect(html).toContain(`connect-src 'self' ws://127.0.0.1:${port} wss://127.0.0.1:${port};`);
    expect(html).not.toContain("ws://127.0.0.1:*");
  });

  it("frames only its own pages (plugins' panels)", async () => {
    const html = await (await fetch(`${base}/`)).text();
    expect(html).toContain("frame-src 'self'");
    expect(html).not.toContain("skplugin:");
  });

  it("serves a plugin's panel sandboxed, without network, framed only by this server's page", async () => {
    const res = await fetch(`${base}/plugins/example.panel/web/index.html`);
    expect(res.status).toBe(200);
    expect(res.headers.get("x-frame-options")).toBeNull();
    const csp = res.headers.get("content-security-policy") ?? "";
    expect(csp).toContain(`script-src http://127.0.0.1:${port}/plugins/example.panel/web/;`);
    expect(csp).toContain(`frame-ancestors http://127.0.0.1:${port};`);
    expect(csp).toContain("sandbox allow-scripts");
    expect(csp).toContain("connect-src 'none'");
    expect(await res.text()).toBe('<html><head><script src="/plugins/example.panel/web/_sk/guard.js"></script></head><body>panel</body></html>');
    const sdk = await fetch(`${base}/plugins/example.panel/web/_sk/bridge.js`);
    expect(sdk.status).toBe(200);
    expect(sdk.headers.get("access-control-allow-origin")).toBe("*");
    expect((await fetch(`${base}/plugins/example.panel/web/../plugin.toml`)).status).toBe(404);
    expect((await fetch(`${base}/plugins/example.panel/web/%2e%2e/plugin.toml`)).status).toBe(404);
    expect((await fetch(`${base}/plugins/no.such/web/index.html`)).status).toBe(404);
    expect((await fetch(`${base}/plugins/..%2f..%2fsecret/web/x`)).status).toBe(404);
  });

  it("serves the bundles, cached for good, and nothing outside the folder", async () => {
    const js = await fetch(`${base}/assets/index-abc.js`);
    expect(js.status).toBe(200);
    expect(js.headers.get("content-type")).toContain("text/javascript");
    expect(js.headers.get("cache-control")).toContain("immutable");
    expect((await fetch(`${base}/..%2fsecret.txt`)).status).toBe(404);
    expect((await fetch(`${base}/assets/%2e%2e/%2e%2e/secret.txt`)).status).toBe(404);
    expect((await fetch(`${base}/missing.js`)).status).toBe(404);
    expect((await fetch(`${base}/`, { method: "POST" })).status).toBe(405);
  });

  it("tells /sk/info only with the token", async () => {
    expect((await fetch(`${base}/sk/info`)).status).toBe(401);
    expect((await fetch(`${base}/sk/info`, { headers: { authorization: "Bearer wrong" } })).status).toBe(401);
    const res = await fetch(`${base}/sk/info`, { headers: { authorization: `Bearer ${TOKEN}` } });
    expect(res.status).toBe(200);
    expect(res.headers.get("cache-control")).toBe("no-store");
    expect(await res.json()).toEqual({ platform: "linux", version: "9.9.9", hostname: "nas", home: "/home/lenny", secure: false });
  });

  it("refuses a WebSocket from another site's page", async () => {
    const reply = await upgrade(port, { Host: `127.0.0.1:${port}`, Origin: "http://evil.example", Upgrade: "websocket", Connection: "Upgrade" });
    expect(reply).toMatch(/^HTTP\/1\.1 403/);
    expect(backend.requests).toHaveLength(0);
  });

  it("refuses other paths", async () => {
    const sock = await new Promise<string>((done) => {
      const s = connect(port, "127.0.0.1", () => s.write(`GET /other HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nOrigin: http://127.0.0.1:${port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n`));
      let got = "";
      s.on("data", (c) => (got += c.toString()));
      s.on("close", () => done(got));
    });
    expect(sock).toMatch(/^HTTP\/1\.1 404/);
  });

  it("forwards the page's own WebSocket to the backend with the backend's origin, both ways", async () => {
    const reply = await upgrade(
      port,
      { Host: `127.0.0.1:${port}`, Origin: `http://127.0.0.1:${port}`, Upgrade: "websocket", Connection: "Upgrade", "X-Forwarded-For": "6.6.6.6", "Sec-WebSocket-Protocol": "spaceknife.v1" },
      "PING",
      /PING/,
    );
    expect(reply).toMatch(/^HTTP\/1\.1 101/);
    expect(reply).toContain("PING"); // echoed back through the pipe
    const req = backend.requests.at(-1)!;
    expect(req.split("\r\n")[0]).toBe("GET /v1 HTTP/1.1");
    expect(req).toContain(`Host: 127.0.0.1:${backend.port}`);
    expect(req).toContain(`Origin: ${HEADLESS_ORIGIN}`);
    expect(req).toContain("Sec-WebSocket-Protocol: spaceknife.v1");
    expect(req).not.toContain("6.6.6.6");
    expect(req).not.toContain(`Origin: http://127.0.0.1:${port}`);
    expect(opened[0]).toBe("open 127.0.0.1");
  });
});

describe("checks", () => {
  it("compares the bearer token", () => {
    expect(bearerOk(`Bearer ${TOKEN}`, TOKEN)).toBe(true);
    expect(bearerOk(`bearer  ${TOKEN}`, TOKEN)).toBe(true);
    expect(bearerOk(TOKEN, TOKEN)).toBe(false);
    expect(bearerOk(undefined, TOKEN)).toBe(false);
    expect(bearerOk("Bearer short", TOKEN)).toBe(false);
  });

  it("wants the Origin of the Host asked for", () => {
    expect(sameOrigin("http://192.168.1.20:8375", "192.168.1.20:8375", false)).toBe(true);
    expect(sameOrigin("http://NAS.lan:8375", "nas.lan:8375", false)).toBe(true);
    expect(sameOrigin("https://[fd00::5]:8375", "[fd00::5]:8375", true)).toBe(true);
    expect(sameOrigin("http://192.168.1.20:8375", "192.168.1.20:8375", true)).toBe(false);
    expect(sameOrigin("http://evil.example", "192.168.1.20:8375", false)).toBe(false);
    expect(sameOrigin(undefined, "192.168.1.20:8375", false)).toBe(false);
    expect(sameOrigin("http://a b", "a b", false)).toBe(false);
  });

  it("puts the Host into the page's connect-src", () => {
    expect(servedIndex(INDEX, "nas:8375")).toContain("connect-src 'self' ws://nas:8375 wss://nas:8375; style-src");
    expect(servedIndex(INDEX, "nas:8375")).toContain("frame-src 'self'");
  });

  it("keeps paths inside the folder", () => {
    const dir = join(tmpdir(), "r");
    expect(fileIn(dir, "/")).toBe(join(dir, "index.html"));
    expect(fileIn(dir, "/assets/a.js")).toBe(join(dir, "assets", "a.js"));
    // ".." stops at the folder, as it does at a site's root
    expect(fileIn(dir, "/../x")).toBe(join(dir, "x"));
    expect(fileIn(dir, "/%2e%2e/%2e%2e/x")).toBe(join(dir, "x"));
    // a backslash separates on Windows only; inside the folder either way
    expect(fileIn(dir, "/a/..%5c..%5cx")?.startsWith(join(dir, "."))).toBe(true);
    expect(fileIn(dir, "/%E0%A4%A")).toBeNull();
    expect(fileIn(dir, "/a%00b")).toBeNull();
  });
});
