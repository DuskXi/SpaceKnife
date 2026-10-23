// Plugins' web panels, served (docs/plugins.md 「Web 面板」): the app's skplugin://<id>/<path> scheme
// (index.ts) and headless mode's /plugins/<id>/web/<path> (headless-server.ts) both map to
// <plugin folder>/web/<path>, and …/_sk/<file> to the app's own panel SDK (resources/plugin-sdk:
// bridge.js, base.css, sk.d.ts, guard.js). Every response carries a content security policy that
// leaves a panel no network, no frames, no forms and a sandbox of its own; the HTML gets guard.js
// first, before any of the plugin's scripts.

import { readFile } from "node:fs/promises";
import { basename, join } from "node:path";

import { fileIn, mimeOf } from "./files";

/** The id rule of plugins (docs/plugins.md 「命名与上限」). */
export const PLUGIN_ID = /^[a-z][a-z0-9]*(-[a-z0-9]+)*(\.[a-z][a-z0-9]*(-[a-z0-9]+)*)+$/;
export const isPluginId = (id: string): boolean => id.length <= 32 && PLUGIN_ID.test(id);

/** The SDK's files a panel may load. */
export const SDK_FILES = new Set(["bridge.js", "base.css", "sk.d.ts", "guard.js"]);

/** Where the backend says plugins are (its ready line): installed, shipped, the trust file with the
 * development links. */
export interface PluginDirs {
  installed: string | null;
  builtin: string | null;
  trust: string | null;
}

/** What a path inside a plugin's web space names: one of the SDK's files, or a file of its web/. */
export function panelPath(rest: string): { sdk: string } | { web: string } | null {
  let decoded: string;
  try {
    decoded = decodeURIComponent(rest);
  } catch {
    return null;
  }
  const parts = decoded.split(/[\\/]+/).filter(Boolean);
  const at = parts.indexOf("_sk");
  if (at >= 0) {
    const file = parts.slice(at + 1).join("/");
    return SDK_FILES.has(file) ? { sdk: file } : null;
  }
  return { web: rest };
}

/** The development links of the trust file (plugins.json "dev"), folders named after their id. */
export async function devLinks(trust: string | null): Promise<string[]> {
  if (!trust) return [];
  try {
    const data = JSON.parse(await readFile(trust, "utf8")) as { dev?: unknown };
    return Array.isArray(data.dev) ? data.dev.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/** The folder of plugin `id`: a development link first (what `plugin dev` serves), then the
 * installed one, then the shipped one; null when there is none. */
export async function pluginFolder(dirs: PluginDirs, id: string, exists: (path: string) => Promise<boolean>): Promise<string | null> {
  if (!isPluginId(id)) return null;
  for (const dev of await devLinks(dirs.trust)) if (basename(dev.replace(/[\\/]+$/, "")) === id && (await exists(join(dev, "plugin.toml")))) return dev;
  for (const root of [dirs.installed, dirs.builtin]) {
    if (!root) continue;
    const folder = join(root, id);
    if (await exists(join(folder, "plugin.toml"))) return folder;
  }
  return null;
}

/** The content security policy of a panel's files. `self` is the panel's own address prefix
 * (skplugin://<id> or http://host/plugins/<id>/web/), `ancestors` who may frame it. */
export function panelPolicy(self: string, ancestors: readonly string[]): string {
  return [
    "default-src 'none'",
    `script-src ${self}`,
    `style-src ${self} 'unsafe-inline'`,
    `img-src ${self} data: blob:`,
    `font-src ${self} data:`,
    `media-src ${self} blob:`,
    "connect-src 'none'",
    "frame-src 'none'",
    "child-src 'none'",
    "worker-src 'none'",
    "object-src 'none'",
    "manifest-src 'none'",
    "form-action 'none'",
    "base-uri 'none'",
    `frame-ancestors ${ancestors.join(" ") || "'none'"}`,
    "sandbox allow-scripts",
    // WebRTC is outside connect-src: guard.js takes it away and plugin check refuses it ("webrtc
    // 'block'" is left out: Chromium does not know it and logs an error for every panel)
  ].join("; ");
}

/** Response headers of a panel file. */
export function panelHeaders(type: string, self: string, ancestors: readonly string[]): Record<string, string> {
  return {
    "content-type": type,
    "content-security-policy": panelPolicy(self, ancestors),
    // an opaque origin loads the panel's module scripts in CORS mode
    "access-control-allow-origin": "*",
    "cross-origin-resource-policy": "cross-origin",
    "x-content-type-options": "nosniff",
    "referrer-policy": "no-referrer",
    "cache-control": "no-cache",
    "permissions-policy": "camera=(), microphone=(), geolocation=(), display-capture=(), usb=(), serial=(), hid=(), bluetooth=(), clipboard-read=(), clipboard-write=()",
  };
}

/** The panel's HTML with the guard first: before anything of the plugin runs. */
export function withGuard(html: string, src: string): string {
  const tag = `<script src="${src}"></script>`;
  const head = /<head[^>]*>/i.exec(html);
  if (head) return html.slice(0, head.index + head[0].length) + tag + html.slice(head.index + head[0].length);
  const doc = /<html[^>]*>/i.exec(html);
  if (doc) return html.slice(0, doc.index + doc[0].length) + tag + html.slice(doc.index + doc[0].length);
  return tag + html;
}

export interface Served {
  status: number;
  body: Buffer | string;
  headers: Record<string, string>;
}

/** One request for a panel's file: plugin `id`, `rest` the path after its prefix. */
export async function servePanelFile(o: {
  dirs: PluginDirs;
  sdkDir: string;
  id: string;
  rest: string;
  self: string;
  ancestors: readonly string[];
  /** the guard's address as the panel's HTML names it */
  guardSrc: string;
  exists(path: string): Promise<boolean>;
}): Promise<Served> {
  const plain = (status: number, text: string): Served => ({ status, body: text, headers: { "content-type": "text/plain; charset=utf-8", "x-content-type-options": "nosniff" } });
  const what = panelPath(o.rest);
  if (!what) return plain(404, "not found");
  let file: string | null;
  if ("sdk" in what) file = join(o.sdkDir, what.sdk);
  else {
    const folder = await pluginFolder(o.dirs, o.id, o.exists);
    file = folder ? fileIn(join(folder, "web"), what.web) : null;
  }
  if (!file) return plain(404, "not found");
  let body: Buffer;
  try {
    body = await readFile(file);
  } catch {
    return plain(404, "not found");
  }
  const type = mimeOf(file);
  const headers = panelHeaders(type, o.self, o.ancestors);
  if (type.startsWith("text/html")) return { status: 200, body: withGuard(body.toString("utf8"), o.guardSrc), headers };
  return { status: 200, body, headers };
}
