// tools/nsis_fork.mjs: the forked NSIS template's pin on tauri-cli / tauri-bundler.
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { crc32, gzipSync } from "node:zlib";

import * as fork from "../nsis_fork.mjs";

/** A tar archive (ustar headers; `long` names as a GNU long-name entry, `pax` ones as a pax path). */
function tar(entries) {
  const blocks = [];
  const header = (name, size, type) => {
    const h = Buffer.alloc(512);
    h.write(name.slice(0, 100), 0, "utf8");
    h.write("0000644\0", 100);
    h.write("0000000\0", 108);
    h.write("0000000\0", 116);
    h.write(`${size.toString(8).padStart(11, "0")}\0`, 124);
    h.write("00000000000\0", 136);
    h.write("        ", 148);
    h.write(type, 156);
    h.write("ustar\0", 257);
    h.write("00", 263);
    let sum = 0;
    for (const b of h) sum += b;
    h.write(`${sum.toString(8).padStart(6, "0")}\0 `, 148);
    return h;
  };
  const pad = (b) => Buffer.concat([b, Buffer.alloc((512 - (b.length % 512)) % 512)]);
  for (const { name, data, long, pax } of entries) {
    const body = Buffer.from(data);
    if (long) {
      const n = Buffer.from(`${name}\0`);
      blocks.push(header("././@LongLink", n.length, "L"), pad(n));
    } else if (pax) {
      const rec = ` path=${name}\n`;
      let len = rec.length + 2;
      if (`${len}${rec}`.length !== len) len += 1;
      const p = Buffer.from(`${len}${rec}`);
      blocks.push(header("PaxHeader", p.length, "x"), pad(p));
    }
    blocks.push(header(long || pax ? "x" : name, body.length, "0"), pad(body));
  }
  blocks.push(Buffer.alloc(1024));
  return gzipSync(Buffer.concat(blocks));
}

const PIN = {
  tauriCli: "2.12.4",
  tauriBundler: "2.10.4",
  nsis: "3.13",
  nsisTauriUtils: "0.5.3",
  files: {},
};
const MOD = 'const NSIS_URL: &str = "https://example.invalid/nsis-3.13/nsis-3.13.zip";\nconst U: &str = "https://example.invalid/nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll";\n';
const TEMPLATE = "Unicode true\nManifestDPIAware true\n!define X \"{{x}}\"\n";
const UTILS = "!macro SetContext\n!macroend\n";

function pinFor(files) {
  return { ...PIN, files: Object.fromEntries([...files].map(([p, b]) => [p, fork.sha256(fork.lf(b))])) };
}

const crateFiles = () =>
  new Map([
    ["src/bundle/windows/nsis/installer.nsi", Buffer.from(TEMPLATE)],
    ["src/bundle/windows/nsis/utils.nsh", Buffer.from(UTILS)],
    ["src/bundle/windows/nsis/mod.rs", Buffer.from(MOD)],
  ]);

test("untar reads ustar, GNU long-name and pax entries", () => {
  const long = `tauri-bundler-2.10.4/${"deep/".repeat(25)}file.nsh`;
  const files = fork.untar(
    tar([
      { name: "tauri-bundler-2.10.4/src/a.nsi", data: "a\n" },
      { name: long, data: "long\n", long: true },
      { name: "tauri-bundler-2.10.4/src/pax.nsh", data: "pax\n", pax: true },
    ]),
  );
  assert.deepEqual([...files.keys()], ["tauri-bundler-2.10.4/src/a.nsi", long, "tauri-bundler-2.10.4/src/pax.nsh"]);
  assert.equal(files.get(long).toString(), "long\n");
});

test("the repository's fork, lockfile and pin agree", () => {
  const pin = fork.readPin();
  assert.match(pin.tauriCli, /^\d+\.\d+\.\d+$/);
  const problems = fork.offlineProblems(pin, {
    packageJson: readFileSync(join(fork.DESKTOP, "package.json"), "utf8"),
    lockfile: readFileSync(join(fork.DESKTOP, "pnpm-lock.yaml"), "utf8"),
    template: readFileSync(join(fork.FORK, "installer.nsi"), "utf8"),
    helpers: readFileSync(join(fork.FORK, "spaceknife.nsh"), "utf8"),
  });
  assert.deepEqual(problems, []);
  // the bundle configuration uses the fork
  const conf = JSON.parse(readFileSync(join(fork.DESKTOP, "src-tauri", "tauri.bundle.windows.json"), "utf8"));
  assert.equal(conf.bundle.windows.nsis.template, "windows/installer.nsi");
  assert.equal(conf.bundle.windows.nsis.installerHooks, "windows/spaceknife.nsh");
  // its tools (NSIS, WebView2's bootstrapper) in <target>/.tauri, where `nsis_fork.mjs tools` and
  // the workflow's cache put them, not in %LOCALAPPDATA%\tauri
  assert.equal(conf.bundle.useLocalToolsDir, true);
  // every file the fork follows is pinned, the template first of all
  for (const f of ["installer.nsi", "utils.nsh", "FileAssociation.nsh", "languages/English.nsh", "languages/SimpChinese.nsh", "mod.rs"]) {
    assert.match(pin.files[`src/bundle/windows/nsis/${f}`] ?? "", /^[0-9a-f]{64}$/, f);
  }
});

test("a tauri-cli the fork was not written for is a problem", () => {
  const lock = (v) => `importers:\n\n  .:\n    devDependencies:\n      '@tauri-apps/cli':\n        specifier: ${v}\n        version: ${v}\n`;
  const pkg = (v) => JSON.stringify({ devDependencies: { "@tauri-apps/cli": v } });
  const heads = {
    template: "; a fork of tauri-bundler 2.10.4's NSIS template (… the bundler of tauri-cli 2.12.4)",
    helpers: "; Pinned to tauri-cli 2.12.4 / tauri-bundler 2.10.4 / NSIS 3.13 / nsis_tauri_utils 0.5.3.",
  };
  const pin = { ...PIN, files: { "src/bundle/windows/nsis/installer.nsi": "0".repeat(64) } };
  assert.deepEqual(fork.offlineProblems(pin, { packageJson: pkg("2.12.4"), lockfile: lock("2.12.4"), ...heads }), []);
  const bumped = fork.offlineProblems(pin, { packageJson: pkg("^2.13.0"), lockfile: lock("2.13.1"), ...heads });
  assert.equal(bumped.length, 2);
  assert.match(bumped.join("\n"), /asks for @tauri-apps\/cli \^2\.13\.0/);
  assert.match(bumped.join("\n"), /locks @tauri-apps\/cli 2\.13\.1/);
  const stale = fork.offlineProblems({ ...pin, tauriBundler: "2.11.0" }, { packageJson: pkg("2.12.4"), lockfile: lock("2.12.4"), ...heads });
  assert.match(stale.join("\n"), /installer\.nsi does not say/);
  assert.match(stale.join("\n"), /spaceknife\.nsh does not say/);
});

test("an upstream template that changed is a problem, line ends are not", () => {
  const files = crateFiles();
  const pin = pinFor(files);
  assert.deepEqual(fork.crateProblems(pin, files), []);
  const crlf = new Map([...files].map(([p, b]) => [p, Buffer.from(b.toString().replaceAll("\n", "\r\n"))]));
  assert.deepEqual(fork.crateProblems(pin, crlf), []);
  const changed = new Map(files).set("src/bundle/windows/nsis/installer.nsi", Buffer.from(`${TEMPLATE}!define NEW 1\n`));
  assert.match(fork.crateProblems(pin, changed).join("\n"), /installer\.nsi: SHA-256 .* merge the upstream change/);
  const gone = new Map(files);
  gone.delete("src/bundle/windows/nsis/utils.nsh");
  assert.match(fork.crateProblems(pin, gone).join("\n"), /has no src\/bundle\/windows\/nsis\/utils\.nsh/);
  const nsis = new Map(files).set("src/bundle/windows/nsis/mod.rs", Buffer.from(MOD.replaceAll("3.13", "3.14")));
  assert.match(fork.crateProblems(pin, nsis).join("\n"), /does not use NSIS 3\.13/);
});

test("the CLI's program must embed the pinned files (LF or CRLF)", () => {
  const files = crateFiles();
  const pin = pinFor(files);
  const crlf = (s) => s.replaceAll("\n", "\r\n");
  const built = Buffer.from(`\0garbage\0${crlf(TEMPLATE)}\0more\0${crlf(UTILS)}\0`);
  assert.deepEqual(fork.binaryProblems(pin, files, built, "cli.node"), []);
  const linux = Buffer.from(`\0${TEMPLATE}\0${UTILS}\0`);
  assert.deepEqual(fork.binaryProblems(pin, files, linux, "cli.node"), []);
  const other = Buffer.from(`\0${TEMPLATE.replace("{{x}}", "{{y}}")}\0${UTILS}\0`);
  assert.match(fork.binaryProblems(pin, files, other, "cli.node").join("\n"), /cli\.node does not embed .*installer\.nsi/);
});

test("check: crates.io's crate, checked against the index, against the pin", async () => {
  const dir = mkdtempSync(join(tmpdir(), "sk-nsis-fork-"));
  const forkDir = join(dir, "windows");
  cpSync(fork.FORK, forkDir, { recursive: true });
  cpSync(join(fork.DESKTOP, "package.json"), join(dir, "package.json"));
  cpSync(join(fork.DESKTOP, "pnpm-lock.yaml"), join(dir, "pnpm-lock.yaml"));
  const files = crateFiles();
  const real = fork.readPin();
  const pin = { ...pinFor(files), tauriCli: real.tauriCli, tauriBundler: real.tauriBundler, nsis: real.nsis, nsisTauriUtils: real.nsisTauriUtils };
  writeFileSync(join(forkDir, "upstream.json"), JSON.stringify(pin));
  const v = pin.tauriBundler;
  const crate = tar([...files].map(([p, b]) => ({ name: `tauri-bundler-${v}/${p}`, data: b })));
  const fetcher = (cksum, req = `=${v}`) => async (url, as) => {
    if (url.includes("/dependencies")) return { dependencies: [{ crate_id: "tauri-bundler", kind: "normal", req }, { crate_id: "tauri-utils", kind: "normal", req: "~2" }] };
    if (url.startsWith("https://index.crates.io/")) return [JSON.stringify({ vers: "0.1.0", cksum: "x" }), JSON.stringify({ vers: v, cksum })].join("\n");
    if (url.endsWith(".crate")) return crate;
    throw new Error(`unexpected ${url} ${as}`);
  };
  const quiet = () => {};
  const opts = { dir: forkDir, desktop: dir, log: quiet };
  assert.deepEqual(await fork.check({ ...opts, fetcher: fetcher(fork.sha256(crate)) }), []);
  await assert.rejects(fork.check({ ...opts, fetcher: fetcher("0".repeat(64)) }), /the index says/);
  const loose = await fork.check({ ...opts, fetcher: fetcher(fork.sha256(crate), `^${v}`) });
  assert.match(loose.join("\n"), /asks for tauri-bundler \^/);
});

// ------------------------------------------------------------------ the bundler's NSIS, fetched once

/** A zip file (stored members) of `files` (name -> content). */
function zip(files) {
  const local = [];
  const central = [];
  let at = 0;
  for (const [name, content] of Object.entries(files)) {
    const data = Buffer.from(content);
    const n = Buffer.from(name);
    const crc = crc32(data);
    const head = Buffer.alloc(30);
    head.writeUInt32LE(0x04034b50, 0);
    head.writeUInt16LE(20, 4);
    head.writeUInt32LE(crc, 14);
    head.writeUInt32LE(data.length, 18);
    head.writeUInt32LE(data.length, 22);
    head.writeUInt16LE(n.length, 26);
    local.push(head, n, data);
    const dir = Buffer.alloc(46);
    dir.writeUInt32LE(0x02014b50, 0);
    dir.writeUInt16LE(20, 4);
    dir.writeUInt16LE(20, 6);
    dir.writeUInt32LE(crc, 16);
    dir.writeUInt32LE(data.length, 20);
    dir.writeUInt32LE(data.length, 24);
    dir.writeUInt16LE(n.length, 28);
    dir.writeUInt32LE(at, 42);
    central.push(dir, n);
    at += head.length + n.length + data.length;
  }
  const cd = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(Object.keys(files).length, 8);
  end.writeUInt16LE(Object.keys(files).length, 10);
  end.writeUInt32LE(cd.length, 12);
  end.writeUInt32LE(at, 16);
  return Buffer.concat([...local, cd, end]);
}

const sha1 = (b) => createHash("sha1").update(b).digest("hex").toUpperCase();

/** NSIS as the bundler's zip has it (`nsis-<v>/`), the plugin, and a pin of their SHA-1s. */
function nsisRelease(extra = {}) {
  const inner = { "NSIS.exe": "nsis gui", ...Object.fromEntries(fork.NSIS_REQUIRED.filter((f) => !f.endsWith("nsis_tauri_utils.dll")).map((f) => [f, `content of ${f}`])), ...extra };
  const archive = zip(Object.fromEntries(Object.entries(inner).map(([p, c]) => [`nsis-3.13/${p}`, c])));
  const dll = Buffer.from("MZ the plugin");
  const pin = { ...PIN, nsisSha1: sha1(archive), nsisExeSha1: sha1(Buffer.from(inner["NSIS.exe"])), nsisTauriUtilsSha1: sha1(dll) };
  return { archive, dll, pin };
}

/** A fetcher that answers the bundler's URLs, failing the first `fails` times each with 504. */
function releaseFetcher({ archive, dll, pin }, { fails = 0, webview2 = Buffer.from("MZ webview2 bootstrapper") } = {}) {
  const urls = fork.nsisDownloads(pin);
  const asked = [];
  const left = new Map();
  const fetcher = async (url) => {
    asked.push(url);
    const n = left.get(url) ?? fails;
    if (n > 0) {
      left.set(url, n - 1);
      throw new fork.ForkError(`${url}: HTTP 504`);
    }
    if (url === urls.nsis.url) return archive;
    if (url === urls.utils.url) return dll;
    if (url === fork.WEBVIEW2_BOOTSTRAPPER_URL) {
      if (!webview2) throw new fork.ForkError(`${url}: HTTP 503`);
      return webview2;
    }
    throw new Error(`unexpected ${url}`);
  };
  return { fetcher, asked };
}

const quietly = { wait: 0, log: () => {} };

test("the NSIS the bundler downloads is pinned with the SHA-1s it checks", () => {
  const pin = fork.readPin();
  for (const k of ["nsisSha1", "nsisExeSha1", "nsisTauriUtilsSha1"]) assert.match(pin[k] ?? "", /^[0-9A-F]{40}$/, k);
  const urls = fork.nsisDownloads(pin);
  assert.equal(urls.nsis.url, `https://github.com/tauri-apps/binary-releases/releases/download/nsis-${pin.nsis}/nsis-${pin.nsis}.zip`);
  assert.equal(urls.utils.url, `https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v${pin.nsisTauriUtils}/nsis_tauri_utils.dll`);
  // the crate's mod.rs must name the same SHA-1s and required files
  const { pin: fake } = nsisRelease();
  const required = `#[cfg(target_os = "windows")]\nconst NSIS_REQUIRED_FILES: &[&str] = &[\n${fork.NSIS_REQUIRED.map((f) => `  "${f}",`).join("\n")}\n];\n#[cfg(not(target_os = "windows"))]\nconst NSIS_REQUIRED_FILES: &[&str] = &["Plugins/x86-unicode/additional/nsis_tauri_utils.dll"];\n`;
  const mod = `${MOD}const NSIS_SHA1: &str = "${fake.nsisSha1}";\nconst NSIS_EXE_SHA1: &str = "${fake.nsisExeSha1}";\nconst NSIS_TAURI_UTILS_SHA1: &str = "${fake.nsisTauriUtilsSha1}";\n${required}`;
  const files = new Map(crateFiles()).set("src/bundle/windows/nsis/mod.rs", Buffer.from(mod));
  const pinned = { ...pinFor(files), nsisSha1: fake.nsisSha1, nsisExeSha1: fake.nsisExeSha1, nsisTauriUtilsSha1: fake.nsisTauriUtilsSha1 };
  assert.deepEqual(fork.crateProblems(pinned, files), []);
  const other = fork.crateProblems({ ...pinned, nsisSha1: "0".repeat(40) }, files);
  assert.match(other.join("\n"), /does not check NSIS's download against SHA-1 0{40}/);
  const fewer = new Map(files).set("src/bundle/windows/nsis/mod.rs", Buffer.from(mod.replace('  "Include/x64.nsh",\n', "")));
  assert.match(fork.crateProblems({ ...pinned, files: pinFor(fewer).files }, fewer).join("\n"), /no longer needs Include\/x64\.nsh/);
});

test("the bundler's NSIS is laid out as it would do it, fetched with retries once, then kept", async () => {
  const tools = join(mkdtempSync(join(tmpdir(), "sk-nsis-tools-")), ".tauri");
  const release = nsisRelease();
  const { fetcher, asked } = releaseFetcher(release, { fails: 2 });
  assert.equal(await fork.prepareTools(tools, { pin: release.pin, fetcher, ...quietly }), "fetched");
  const nsis = join(tools, "NSIS");
  assert.ok(fork.nsisReady(nsis, release.pin));
  assert.equal(readFileSync(join(nsis, "Plugins", "x86-unicode", "additional", "nsis_tauri_utils.dll")).toString(), "MZ the plugin");
  assert.equal(readFileSync(join(nsis, "Include", "MUI2.nsh")).toString(), "content of Include/MUI2.nsh");
  assert.equal(readFileSync(join(tools, "MicrosoftEdgeWebview2Setup.exe")).toString(), "MZ webview2 bootstrapper");
  assert.deepEqual(readdirSync(tools).sort(), ["MicrosoftEdgeWebview2Setup.exe", "NSIS", "downloads"]);
  assert.deepEqual(readdirSync(join(tools, "downloads")).sort(), ["nsis-3.13.zip", "nsis_tauri_utils-0.5.3.dll"]);
  const urls = fork.nsisDownloads(release.pin);
  assert.equal(asked.filter((u) => u === urls.nsis.url).length, 3, "two 504s, then the file");
  // kept (the cache): nothing of NSIS is fetched; the folder is unpacked afresh, so a file of it
  // that changed meanwhile is put right
  asked.length = 0;
  writeFileSync(join(nsis, "Bin", "makensis.exe"), "changed");
  assert.equal(await fork.prepareTools(tools, { pin: release.pin, fetcher, ...quietly }), "cached");
  assert.ok(!asked.includes(urls.nsis.url) && !asked.includes(urls.utils.url), asked.join(" "));
  assert.equal(readFileSync(join(nsis, "Bin", "makensis.exe")).toString(), "content of Bin/makensis.exe");
  // a kept download with another SHA-1 is fetched again
  writeFileSync(join(tools, "downloads", "nsis-3.13.zip"), "not the zip");
  assert.equal(await fork.prepareTools(tools, { pin: release.pin, fetcher, ...quietly }), "fetched");
  assert.ok(fork.nsisReady(nsis, release.pin));
  // the folder the bundler checks: another NSIS.exe is not taken
  writeFileSync(join(nsis, "NSIS.exe"), "an older NSIS");
  assert.ok(!fork.nsisReady(nsis, release.pin));
});

test("a download that is not the pinned one, or that names a path outside, leaves nothing", async () => {
  const release = nsisRelease();
  for (const [what, bad, kept] of [
    ["another zip", { ...release, archive: zip({ "nsis-3.13/NSIS.exe": "other" }) }, []],
    ["another plugin", { ...release, dll: Buffer.from("MZ other") }, ["nsis-3.13.zip"]],
  ]) {
    const tools = join(mkdtempSync(join(tmpdir(), "sk-nsis-tools-")), ".tauri");
    const { fetcher } = releaseFetcher(bad);
    await assert.rejects(fork.prepareTools(tools, { pin: release.pin, fetcher, ...quietly }), /SHA-1/, what);
    assert.ok(!existsSync(join(tools, "NSIS")), what);
    // only what had the pinned SHA-1 is kept
    const downloads = join(tools, "downloads");
    assert.deepEqual(existsSync(downloads) ? readdirSync(downloads) : [], kept, what);
  }
  const climbing = nsisRelease({ "../../outside.txt": "x" });
  const tools = join(mkdtempSync(join(tmpdir(), "sk-nsis-tools-")), ".tauri");
  await assert.rejects(fork.prepareTools(tools, { pin: climbing.pin, fetcher: releaseFetcher(climbing).fetcher, ...quietly }), /outside/);
  assert.ok(!existsSync(join(tools, "NSIS")));
  assert.ok(!existsSync(join(tools, "..", "outside.txt")));
  // too many failures: the last error, nothing left
  const down = join(mkdtempSync(join(tmpdir(), "sk-nsis-tools-")), ".tauri");
  await assert.rejects(fork.prepareTools(down, { pin: release.pin, fetcher: releaseFetcher(release, { fails: 9 }).fetcher, tries: 3, ...quietly }), /HTTP 504/);
  assert.ok(!existsSync(join(down, "NSIS")));
});

test("WebView2's bootstrapper is fetched fresh, an older copy kept when that fails", async () => {
  const release = nsisRelease();
  const tools = join(mkdtempSync(join(tmpdir(), "sk-nsis-tools-")), ".tauri");
  const down = { webview2: null };
  // none there, none to be had: an error (NSIS itself is in place)
  await assert.rejects(fork.prepareTools(tools, { pin: release.pin, fetcher: releaseFetcher(release, down).fetcher, ...quietly }), /HTTP 503/);
  assert.ok(fork.nsisReady(join(tools, "NSIS"), release.pin));
  assert.ok(!existsSync(join(tools, "MicrosoftEdgeWebview2Setup.exe")));
  mkdirSync(tools, { recursive: true });
  writeFileSync(join(tools, "MicrosoftEdgeWebview2Setup.exe"), "MZ an older one");
  assert.equal(await fork.prepareTools(tools, { pin: release.pin, fetcher: releaseFetcher(release, down).fetcher, ...quietly }), "cached");
  assert.equal(readFileSync(join(tools, "MicrosoftEdgeWebview2Setup.exe")).toString(), "MZ an older one");
  // not a program: refused, the older one stays
  await fork.prepareTools(tools, { pin: release.pin, fetcher: releaseFetcher(release, { webview2: Buffer.from("<html>error</html>") }).fetcher, ...quietly });
  assert.equal(readFileSync(join(tools, "MicrosoftEdgeWebview2Setup.exe")).toString(), "MZ an older one");
  await fork.prepareTools(tools, { pin: release.pin, fetcher: releaseFetcher(release).fetcher, ...quietly });
  assert.equal(readFileSync(join(tools, "MicrosoftEdgeWebview2Setup.exe")).toString(), "MZ webview2 bootstrapper");
});
