// Serving plugins' web panels (plugin-files.ts): which file a path names, which folder a plugin is
// in, the policy every panel file carries, the guard first in its HTML.

import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { isPluginId, panelPath, panelPolicy, pluginFolder, servePanelFile, withGuard } from "./plugin-files";

const exists = async (p: string) => existsSync(p);

function plugins(): { root: string; dirs: { installed: string; builtin: string; trust: string }; sdk: string; dev: string } {
  const root = mkdtempSync(join(tmpdir(), "sk-panels-"));
  const installed = join(root, "plugins");
  const builtin = join(root, "builtin");
  const sdk = join(root, "sdk");
  const dev = join(root, "work", "dev.plugin");
  for (const [dir, id] of [
    [installed, "example.panel"],
    [builtin, "builtin.board"],
  ] as const) {
    mkdirSync(join(dir, id, "web", "sub"), { recursive: true });
    writeFileSync(join(dir, id, "plugin.toml"), `id = "${id}"`);
    writeFileSync(join(dir, id, "web", "index.html"), "<!doctype html><html><head><meta charset=utf-8></head><body>hi</body></html>");
    writeFileSync(join(dir, id, "web", "sub", "app.js"), "export {};");
  }
  writeFileSync(join(installed, "example.panel", "secret.txt"), "not served");
  mkdirSync(join(dev, "web"), { recursive: true });
  writeFileSync(join(dev, "plugin.toml"), 'id = "dev.plugin"');
  writeFileSync(join(dev, "web", "index.html"), "<p>dev</p>");
  mkdirSync(sdk);
  for (const f of ["bridge.js", "base.css", "sk.d.ts", "guard.js"]) writeFileSync(join(sdk, f), `/* ${f} */`);
  const trust = join(root, "plugins.json");
  writeFileSync(trust, JSON.stringify({ version: 1, plugins: {}, dev: [dev] }));
  return { root, dirs: { installed, builtin, trust }, sdk, dev };
}

describe("paths", () => {
  it("names the SDK's files under any _sk/, the panel's own otherwise", () => {
    expect(panelPath("/_sk/bridge.js")).toEqual({ sdk: "bridge.js" });
    expect(panelPath("/sub/_sk/base.css")).toEqual({ sdk: "base.css" });
    expect(panelPath("/_sk/../../secret")).toBeNull();
    expect(panelPath("/_sk/other.js")).toBeNull();
    expect(panelPath("/index.html")).toEqual({ web: "/index.html" });
    expect(panelPath("/%E0%A4%A")).toBeNull();
  });

  it("knows a plugin id", () => {
    expect(isPluginId("example.dupes")).toBe(true);
    expect(isPluginId("acme.zip-source")).toBe(true);
    expect(isPluginId("single")).toBe(false);
    expect(isPluginId("../x.y")).toBe(false);
    expect(isPluginId("a.b".padEnd(40, "c"))).toBe(false);
  });
});

describe("folders", () => {
  it("finds a development link first, then installed, then shipped; nothing for an unknown id", async () => {
    const p = plugins();
    expect(await pluginFolder(p.dirs, "dev.plugin", exists)).toBe(p.dev);
    expect(await pluginFolder(p.dirs, "example.panel", exists)).toBe(join(p.dirs.installed, "example.panel"));
    expect(await pluginFolder(p.dirs, "builtin.board", exists)).toBe(join(p.dirs.builtin, "builtin.board"));
    expect(await pluginFolder(p.dirs, "no.such", exists)).toBeNull();
    expect(await pluginFolder({ installed: null, builtin: null, trust: null }, "example.panel", exists)).toBeNull();
  });
});

describe("serving", () => {
  const serve = (p: ReturnType<typeof plugins>, id: string, rest: string) =>
    servePanelFile({ dirs: p.dirs, sdkDir: p.sdk, id, rest, self: `skplugin://${id}`, ancestors: ["app://spaceknife"], guardSrc: "/_sk/guard.js", exists });

  it("serves the panel's web/ files with its policy, the guard first in HTML", async () => {
    const p = plugins();
    const r = await serve(p, "example.panel", "/index.html");
    expect(r.status).toBe(200);
    expect(String(r.body)).toMatch(/^<!doctype html><html><head><script src="\/_sk\/guard.js"><\/script><meta/);
    expect(r.headers["content-security-policy"]).toBe(panelPolicy("skplugin://example.panel", ["app://spaceknife"]));
    expect(r.headers["access-control-allow-origin"]).toBe("*");
    expect(String((await serve(p, "example.panel", "/sub/app.js")).body)).toBe("export {};");
    expect((await serve(p, "example.panel", "/_sk/bridge.js")).status).toBe(200);
  });

  it("serves nothing outside web/, of another plugin, or of an unknown one", async () => {
    const p = plugins();
    expect((await serve(p, "example.panel", "/../secret.txt")).status).toBe(404);
    expect((await serve(p, "example.panel", "/%2e%2e/secret.txt")).status).toBe(404);
    expect((await serve(p, "example.panel", "/../../builtin/builtin.board/web/index.html")).status).toBe(404);
    expect((await serve(p, "no.such", "/index.html")).status).toBe(404);
    expect((await serve(p, "example.panel", "/missing.js")).status).toBe(404);
  });

  it("gives a panel no network, frames, forms or workers, and a sandbox of its own", () => {
    const csp = panelPolicy("skplugin://example.panel", ["app://spaceknife"]);
    for (const d of ["default-src 'none'", "connect-src 'none'", "frame-src 'none'", "worker-src 'none'", "object-src 'none'", "form-action 'none'", "base-uri 'none'", "sandbox allow-scripts", "frame-ancestors app://spaceknife", "script-src skplugin://example.panel;"]) {
      expect(csp).toContain(d);
    }
    expect(csp).not.toContain("'self'");
    expect(csp).not.toContain("unsafe-eval");
    expect(panelPolicy("x", [])).toContain("frame-ancestors 'none'");
  });

  it("puts the guard before anything, with or without a head", () => {
    expect(withGuard("<html><body>x</body></html>", "/g.js")).toBe('<html><script src="/g.js"></script><body>x</body></html>');
    expect(withGuard("<p>x</p>", "/g.js")).toBe('<script src="/g.js"></script><p>x</p>');
    expect(withGuard('<HEAD lang="x"><script src="a.js"></script>', "/g.js")).toBe('<HEAD lang="x"><script src="/g.js"></script><script src="a.js"></script>');
  });
});
