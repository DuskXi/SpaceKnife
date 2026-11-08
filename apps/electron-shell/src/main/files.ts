// The built interface's files (out/renderer): their types, and a URL path resolved inside the folder.
// Both the app's app:// scheme (index.ts) and headless mode's HTTP server (headless-server.ts) use it.

import { extname, isAbsolute, normalize, relative, resolve } from "node:path";

export const MIME: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".webp": "image/webp",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".gif": "image/gif",
  ".ico": "image/x-icon",
  ".woff2": "font/woff2",
  ".woff": "font/woff",
  ".json": "application/json; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
};

export const mimeOf = (file: string): string => MIME[extname(file).toLowerCase()] ?? "application/octet-stream";

/** The file a URL path names inside `dir` ("/" is index.html), or null when it would leave the
 *  folder or can't be decoded. */
export function fileIn(dir: string, pathname: string): string | null {
  let decoded: string;
  try {
    decoded = decodeURIComponent(pathname);
  } catch {
    return null;
  }
  if (decoded.includes("\0")) return null;
  const rel = normalize(decoded).replace(/^[/\\]+/, "") || "index.html";
  const file = resolve(dir, rel);
  const inside = relative(resolve(dir), file);
  if (!inside || inside.startsWith("..") || isAbsolute(inside)) return null;
  return file;
}
