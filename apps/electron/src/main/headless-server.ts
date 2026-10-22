// Headless mode's HTTP server: the built interface (out/renderer) and, on the same address, the
// backend's WebSocket. The backend itself stays on 127.0.0.1; this server forwards /v1 to it, so the
// browser talks to one origin and nothing but this server is reachable from the network.
//
//   GET /, /assets/…   the interface's files (public: there is nothing secret in them)
//   GET /sk/info       what the interface needs before it connects (platform, version, host name);
//                      only with "Authorization: Bearer <token>"
//   GET /v1 (Upgrade)  the backend's WebSocket. The browser's Origin must be this server's own origin
//                      (the Host it asked for), the backend sees HEADLESS_ORIGIN, and session.hello
//                      still needs the token (docs/protocol.md §1).
//   GET /plugins/<id>/web/…  a plugin's web panel files (plugin-files.ts): each with a policy that
//                      sandboxes it (an opaque origin, never this page's, even opened by itself), no
//                      network, framed only by this server's page; …/_sk/… the panel SDK.

import { createHash, timingSafeEqual } from "node:crypto";
import { readFile } from "node:fs/promises";
import { type IncomingMessage, type Server, type ServerResponse, createServer as createHttpServer } from "node:http";
import { createServer as createHttpsServer } from "node:https";
import { type Socket, connect } from "node:net";
import type { Duplex } from "node:stream";

import { existsSync } from "node:fs";

import { fileIn, mimeOf } from "./files";
import { HEADLESS_ORIGIN, type Listen } from "./headless";
import { isPluginId, type PluginDirs, servePanelFile } from "./plugin-files";

const WS_PATH = "/v1"; // spaceknife_protocol.WS_PATH
const MAX_SOCKETS = 64;

/** What the interface asks for before connecting (shim.ts). */
export interface HeadlessInfo {
  platform: "win32" | "darwin" | "linux";
  version: string;
  /** this computer's name, shown in the browser: the files are there */
  hostname: string;
  /** the user's home folder there, where a file path typed in the browser starts */
  home: string;
  secure: boolean;
}

export interface HeadlessServerOptions {
  rendererDir: string;
  /** the backend's WebSocket (its ready line), on this computer */
  backendUrl: string;
  token: string;
  info: HeadlessInfo;
  tls: { cert: Buffer; key: Buffer } | null;
  /** a browser connected to the backend, or went away */
  onSocket?: (event: "open" | "close", remote: string) => void;
  /** where plugins are (the backend's ready line) and the app's panel SDK: their web panels */
  plugins?: { dirs: PluginDirs; sdkDir: string } | null;
}

export interface HeadlessServer {
  server: Server;
  /** listen; resolves with the port (the system's pick for port 0) */
  listen(where: Listen): Promise<number>;
  /** stop listening and drop every connection, forwarded WebSockets included */
  close(): Promise<void>;
}

/** A Host header worth echoing into the page's policy: a name or an address, an optional port. */
const HOST_HEADER = /^(?:[A-Za-z0-9.-]+|\[[0-9A-Fa-f:.]+\])(?::\d{1,5})?$/;

const sha = (s: string) => createHash("sha256").update(s).digest();

/** The token in "Authorization: Bearer …", compared in constant time. */
export function bearerOk(header: string | undefined, token: string): boolean {
  const m = /^Bearer\s+(\S+)$/i.exec(header ?? "");
  return !!m && timingSafeEqual(sha(m[1]!), sha(token));
}

/** The browser's Origin must be the origin it loaded the page from: the scheme and the Host it
 *  asked for. Another site's page (cross-site WebSocket hijacking) carries its own. */
export function sameOrigin(origin: string | undefined, host: string | undefined, secure: boolean): boolean {
  if (!origin || !host || !HOST_HEADER.test(host)) return false;
  return origin.toLowerCase() === `${secure ? "https" : "http"}://${host.toLowerCase()}`;
}

/** index.html with its Content-Security-Policy letting the page reach this server's WebSocket by the
 *  name it used (CSP3 browsers match ws: to 'self' already; older ones need it spelled out), and
 *  frame only this server's own pages (plugins' panels): a panel can't take its frame elsewhere. */
export function servedIndex(html: string, host: string): string {
  return html.replace(/connect-src [^;"]*/, `connect-src 'self' ws://${host} wss://${host}`).replace(/frame-src [^;"]*/, "frame-src 'self'");
}

const PANEL_PATH = /^\/plugins\/([^/]+)\/web(\/.*)?$/;

const SECURITY_HEADERS = {
  "x-content-type-options": "nosniff",
  "referrer-policy": "no-referrer",
  "x-frame-options": "DENY",
  "cross-origin-opener-policy": "same-origin",
  "cross-origin-resource-policy": "same-origin",
};

export function createHeadlessServer(o: HeadlessServerOptions): HeadlessServer {
  const backend = new URL(o.backendUrl);
  const secure = o.tls !== null;
  const sockets = new Set<Duplex>();
  const handler = (req: IncomingMessage, res: ServerResponse) => void respond(o, req, res, secure);
  const server: Server = o.tls ? createHttpsServer({ cert: o.tls.cert, key: o.tls.key }, handler) : createHttpServer(handler);
  server.headersTimeout = 20_000;
  server.requestTimeout = 30_000;

  server.on("upgrade", (req: IncomingMessage, socket: Duplex, head: Buffer) => {
    const reject = (status: number, text: string) => {
      socket.end(`HTTP/1.1 ${status} ${text}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`);
    };
    const path = (req.url ?? "").split("?")[0];
    if (req.method !== "GET" || path !== WS_PATH) return reject(404, "Not Found");
    if (!sameOrigin(req.headers.origin, req.headers.host, secure)) return reject(403, "Forbidden");
    if (sockets.size >= MAX_SOCKETS) return reject(503, "Service Unavailable");
    const remote = remoteOf(req.socket);
    const upstream: Socket = connect({ host: backend.hostname.replace(/^\[|\]$/g, ""), port: Number(backend.port) });
    sockets.add(socket);
    sockets.add(upstream);
    let opened = false;
    const drop = () => {
      socket.destroy();
      upstream.destroy();
      if (sockets.delete(socket) && opened) o.onSocket?.("close", remote);
      sockets.delete(upstream);
    };
    socket.on("error", drop);
    socket.on("close", drop);
    upstream.on("error", drop);
    upstream.on("close", drop);
    upstream.once("connect", () => {
      upstream.setNoDelay(true);
      const lines = [`GET ${WS_PATH} HTTP/1.1`, `Host: ${backend.host}`, `Origin: ${HEADLESS_ORIGIN}`];
      const raw = req.rawHeaders;
      for (let i = 0; i + 1 < raw.length; i += 2) {
        if (/^(host|origin|forwarded|x-forwarded-.*)$/i.test(raw[i]!)) continue;
        lines.push(`${raw[i]}: ${raw[i + 1]}`);
      }
      upstream.write(`${lines.join("\r\n")}\r\n\r\n`);
      if (head.length) upstream.write(head);
      socket.pipe(upstream);
      upstream.pipe(socket);
      opened = true;
      o.onSocket?.("open", remote);
    });
  });

  return {
    server,
    listen: (where) =>
      new Promise((done, fail) => {
        server.once("error", fail);
        server.listen({ host: where.host, port: where.port }, () => {
          server.off("error", fail);
          const addr = server.address();
          done(typeof addr === "object" && addr ? addr.port : where.port);
        });
      }),
    close: () =>
      new Promise((done) => {
        for (const s of sockets) s.destroy();
        sockets.clear();
        server.close(() => done());
        server.closeAllConnections();
      }),
  };
}

function remoteOf(socket: Socket): string {
  const addr = (socket.remoteAddress ?? "?").replace(/^::ffff:/, "");
  return addr.includes(":") ? `[${addr}]` : addr;
}

async function respond(o: HeadlessServerOptions, req: IncomingMessage, res: ServerResponse, secure: boolean): Promise<void> {
  const send = (status: number, body: string | Buffer, headers: Record<string, string> = {}) => {
    res.writeHead(status, { ...SECURITY_HEADERS, "content-type": "text/plain; charset=utf-8", ...headers });
    res.end(req.method === "HEAD" ? undefined : body);
  };
  if (req.method !== "GET" && req.method !== "HEAD") return send(405, "Method Not Allowed\n", { allow: "GET, HEAD" });
  const host = req.headers.host ?? "";
  if (!HOST_HEADER.test(host)) return send(400, "Bad Request\n");
  const pathname = new URL(req.url ?? "/", `${secure ? "https" : "http"}://${host}`).pathname;

  if (pathname === "/sk/info") {
    if (!bearerOk(req.headers.authorization, o.token)) return send(401, "Unauthorized\n", { "www-authenticate": "Bearer", "cache-control": "no-store" });
    return send(200, JSON.stringify(o.info), { "content-type": "application/json; charset=utf-8", "cache-control": "no-store" });
  }
  if (pathname === WS_PATH) return send(426, "Upgrade Required\n", { upgrade: "websocket" });

  const panel = PANEL_PATH.exec(pathname);
  if (panel) {
    const id = panel[1]!;
    if (!o.plugins || !isPluginId(id)) return send(404, "Not Found\n");
    const origin = `${secure ? "https" : "http"}://${host}`;
    const prefix = `${origin}/plugins/${id}/web/`;
    const r = await servePanelFile({
      dirs: o.plugins.dirs,
      sdkDir: o.plugins.sdkDir,
      id,
      rest: panel[2] ?? "/",
      self: prefix,
      ancestors: [origin],
      guardSrc: `/plugins/${id}/web/_sk/guard.js`,
      exists: async (p) => existsSync(p),
    });
    // not the app's own headers: a panel is framed by the app (no X-Frame-Options: DENY) and loads
    // its files from an opaque origin (no same-origin resource policy)
    res.writeHead(r.status, r.headers);
    res.end(req.method === "HEAD" ? undefined : r.body);
    return;
  }

  const file = fileIn(o.rendererDir, pathname);
  if (!file) return send(404, "Not Found\n");
  let body: Buffer;
  try {
    body = await readFile(file);
  } catch {
    return send(404, "Not Found\n");
  }
  const type = mimeOf(file);
  if (type.startsWith("text/html")) {
    // the page: always the current one, never in a frame
    return send(200, servedIndex(body.toString("utf8"), host), {
      "content-type": type,
      "cache-control": "no-store",
      "content-security-policy": "frame-ancestors 'none'",
    });
  }
  // the bundles have hashed names (assets/index-abc123.js): cached for good
  const cache = pathname.startsWith("/assets/") ? "public, max-age=31536000, immutable" : "no-cache";
  return send(200, body, { "content-type": type, "cache-control": cache });
}
