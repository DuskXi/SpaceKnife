// tools/nsis_fork.mjs: the forked NSIS template's pin on tauri-cli / tauri-bundler.
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { cpSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { gzipSync } from "node:zlib";

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
