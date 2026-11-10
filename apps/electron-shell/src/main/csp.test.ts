import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { contentSecurityPolicy, inlineScriptHashes } from "./csp";

const sha = (text: string) => `'sha256-${createHash("sha256").update(text, "utf8").digest("base64")}'`;

// the shape of Nuxt's generated index.html (apps/desktop/.output/public)
const PAGE = [
  '<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8">',
  '<script type="importmap">{"imports":{"#entry":"/_nuxt/A.js"}}</script>',
  '<script src="/theme-boot.js"></script>',
  '<script type="module" src="/_nuxt/A.js" crossorigin></script>',
  "</head><body><div id=\"__nuxt\"></div>",
  '<script>window.__NUXT__={};window.__NUXT__.config={public:{},app:{baseURL:"/"}}</script>',
  '<script type="application/json" data-nuxt-data="nuxt-app" id="__NUXT_DATA__">[{"a":1},2]</script>',
  "</body></html>",
].join("");

describe("the Content-Security-Policy of app://", () => {
  it("hashes the inline scripts a browser runs, the import map included, not the JSON data or scripts with src", () => {
    expect(inlineScriptHashes(PAGE)).toEqual([sha('{"imports":{"#entry":"/_nuxt/A.js"}}'), sha('window.__NUXT__={};window.__NUXT__.config={public:{},app:{baseURL:"/"}}')]);
    expect(inlineScriptHashes("<p>no scripts</p>")).toEqual([]);
    expect(inlineScriptHashes("<SCRIPT TYPE='module'>import x from '/a.js'</SCRIPT >")).toEqual([sha("import x from '/a.js'")]);
    expect(inlineScriptHashes('<script type="text/plain">x</script>')).toEqual([]);
  });

  it("allows the page's own origin, those hashes and the backend's WebSocket; no plugins, frames, forms or base", () => {
    const csp = contentSecurityPolicy(["'sha256-a'", "'sha256-b'", "'sha256-a'"]);
    const directives = Object.fromEntries(csp.split("; ").map((d) => [d.split(" ")[0], d.split(" ").slice(1).join(" ")]));
    expect(directives).toEqual({
      "default-src": "'self'",
      "script-src": "'self' 'sha256-a' 'sha256-b'",
      "style-src": "'self' 'unsafe-inline'",
      "font-src": "'self' data:",
      "img-src": "'self' data:",
      "connect-src": "'self' ws://127.0.0.1:*",
      "object-src": "'none'",
      "base-uri": "'none'",
      "form-action": "'none'",
      "frame-ancestors": "'none'",
    });
    expect(csp).not.toContain("unsafe-eval");
    expect(directives["script-src"]).not.toContain("unsafe-inline");
  });

  it("covers every inline script of the interface as generated, when it is built", () => {
    const built = join(dirname(fileURLToPath(import.meta.url)), "../../../desktop/.output/public/index.html");
    if (!existsSync(built)) return; // `pnpm generate` in apps/desktop makes it
    const html = readFileSync(built, "utf8");
    const inline = [...html.matchAll(/<script(?![^>]*\bsrc=)(?![^>]*application\/json)[^>]*>([\s\S]*?)<\/script>/g)].map((m) => sha(m[1]!));
    expect(inline.length).toBeGreaterThan(0);
    expect(inlineScriptHashes(html)).toEqual(inline);
  });
});
