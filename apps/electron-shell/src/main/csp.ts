// The Content-Security-Policy of the interface's pages on app:// (index.ts). The Tauri shell gets the
// same policy from tauri.conf.json (app.security.csp), where Tauri adds the hashes of the inline
// scripts itself; here they are computed from each page as it is served. Nuxt's generated page has
// two inline scripts (the import map and window.__NUXT__) and one JSON data block, which runs nothing
// and needs no hash. Everything else comes from the page's own origin; the WebSocket goes to the
// backend on 127.0.0.1.

import { createHash } from "node:crypto";

/** Script types a browser runs (or reads as an import map): these need a hash when inline. */
const RUN_TYPES = new Set(["", "module", "importmap", "text/javascript", "application/javascript"]);

/** "'sha256-…'" of each inline script of an HTML page that a browser would run, in page order. */
export function inlineScriptHashes(html: string): string[] {
  const out: string[] = [];
  for (const m of html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script\s*>/gi)) {
    const attrs = m[1] ?? "";
    if (/\bsrc\s*=/i.test(attrs)) continue;
    const type = /\btype\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/i.exec(attrs);
    const kind = (type?.[1] ?? type?.[2] ?? type?.[3] ?? "").trim().toLowerCase();
    if (!RUN_TYPES.has(kind)) continue;
    const digest = createHash("sha256").update(m[2] ?? "", "utf8").digest("base64");
    out.push(`'sha256-${digest}'`);
  }
  return out;
}

/** The policy for a page with these inline script hashes. */
export function contentSecurityPolicy(scriptHashes: readonly string[]): string {
  return [
    "default-src 'self'",
    ["script-src 'self'", ...new Set(scriptHashes)].join(" "),
    "style-src 'self' 'unsafe-inline'",
    "font-src 'self' data:",
    "img-src 'self' data:",
    "connect-src 'self' ws://127.0.0.1:*",
    "object-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
    "frame-ancestors 'none'",
  ].join("; ");
}
