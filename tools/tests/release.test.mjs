// tools/release.mjs: checking the build runs' artifacts, collecting the release, filling the notes.
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import * as release from "../release.mjs";

const V = "0.3.5";
const SCRIPT = fileURLToPath(new URL("../release.mjs", import.meta.url));
const tmp = () => mkdtempSync(join(tmpdir(), "sk-release-"));

function put(folder, name, data) {
  mkdirSync(folder, { recursive: true });
  writeFileSync(join(folder, name), data);
  return createHash("sha256").update(data).digest("hex");
}

/** The artifacts of a release's build runs, laid out as `gh run download` writes them: the Tauri
 * builds (portable archives, the deb, the macOS files) and the Electron shell's installers. */
function artifacts(root, version = V, { pkgVersion = version } = {}) {
  const groups = {
    // [files listed in the checksum file, the checksum file, files in no list]
    "SpaceKnife-electron-win-x64": [[`SpaceKnife-${version}-Setup-x64.exe`], "SHA256SUMS-installers-electron-win-x64.txt", []],
    "SpaceKnife-electron-linux-x64": [
      [`SpaceKnife-${version}-x86_64.AppImage`, `spaceknife-${pkgVersion}.x86_64.rpm`],
      "SHA256SUMS-installers-electron-linux-x64.txt",
      [`SpaceKnife-${version}-x86_64.AppImage.zsync`],
    ],
    "SpaceKnife-installers-linux-x64": [[`spaceknife_${pkgVersion}_amd64.deb`], "SHA256SUMS-installers-linux-x64.txt", []],
  };
  for (const [target, ext] of [["win-x64", "zip"], ["win-arm64", "zip"], ["linux-x64", "tar.gz"], ["linux-arm64", "tar.gz"]]) {
    groups[`SpaceKnife-portable-${target}`] = [[`SpaceKnife-${version}-${target}-portable.${ext}`], `SHA256SUMS-portable-${target}.txt`, []];
  }
  for (const a of ["arm64", "x64"]) {
    groups[`SpaceKnife-mac-${a}`] = [[`SpaceKnife-${version}-mac-${a}.dmg`, `SpaceKnife-${version}-mac-${a}-portable.zip`], `SHA256SUMS-mac-${a}.txt`, []];
  }
  for (const [folder, [names, sums, unlisted]] of Object.entries(groups)) {
    const d = join(root, folder);
    const lines = names.map((n) => `${put(d, n, n.repeat(3))}  ${n}\n`);
    writeFileSync(join(d, sums), lines.join(""));
    for (const n of unlisted) put(d, n, n.repeat(2));
  }
  return root;
}

test("a complete set becomes the release", () => {
  const t = tmp();
  const done = release.assemble(V, artifacts(join(t, "a")), join(t, "out"));
  const names = new Set(readdirSync(join(t, "out")));
  const want = new Set([...release.expected().map((e) => e.template.replace("{v}", V)), ...release.publishedSums()]);
  assert.deepEqual([...names].sort(), [...want].sort());
  assert.ok(!names.has("SHA256SUMS-installers-electron-win-x64.txt"));
  const merged = release.parseSums(readFileSync(join(t, "out", "SHA256SUMS.txt"), "utf8"));
  // the four installers, as in 0.3.4's SHA256SUMS.txt (not the .zsync)
  assert.deepEqual([...merged.keys()].sort(), [`SpaceKnife-${V}-Setup-x64.exe`, `SpaceKnife-${V}-x86_64.AppImage`, `spaceknife-${V}.x86_64.rpm`, `spaceknife_${V}_amd64.deb`]);
  for (const [name, digest] of merged) {
    assert.equal(release.sha256(join(t, "out", name)), digest);
    assert.equal(done.hashes.get(name), digest);
  }
});

test("the release's files are 0.3.4's, for good", () => {
  // v0.3.4 tools/release.py `expected`: an installed 0.3.x looks for exactly these names
  const v034 = [
    ["SpaceKnife-{v}-Setup-x64.exe", "SHA256SUMS.txt"],
    ["SpaceKnife-{v}-x86_64.AppImage", "SHA256SUMS.txt"],
    ["spaceknife_{v}_amd64.deb", "SHA256SUMS.txt"],
    ["spaceknife-{v}.x86_64.rpm", "SHA256SUMS.txt"],
    ["SpaceKnife-{v}-x86_64.AppImage.zsync", null],
    ["SpaceKnife-{v}-win-x64-portable.zip", "SHA256SUMS-portable-win-x64.txt"],
    ["SpaceKnife-{v}-win-arm64-portable.zip", "SHA256SUMS-portable-win-arm64.txt"],
    ["SpaceKnife-{v}-linux-x64-portable.tar.gz", "SHA256SUMS-portable-linux-x64.txt"],
    ["SpaceKnife-{v}-linux-arm64-portable.tar.gz", "SHA256SUMS-portable-linux-arm64.txt"],
    ["SpaceKnife-{v}-mac-arm64.dmg", "SHA256SUMS-mac-arm64.txt"],
    ["SpaceKnife-{v}-mac-arm64-portable.zip", "SHA256SUMS-mac-arm64.txt"],
    ["SpaceKnife-{v}-mac-x64.dmg", "SHA256SUMS-mac-x64.txt"],
    ["SpaceKnife-{v}-mac-x64-portable.zip", "SHA256SUMS-mac-x64.txt"],
  ];
  assert.deepEqual(
    release.expected().map((e) => [e.template, e.sums]),
    v034,
  );
  // and the checksum files 0.3.4's release published
  assert.deepEqual(release.publishedSums(), [
    "SHA256SUMS-mac-arm64.txt",
    "SHA256SUMS-mac-x64.txt",
    "SHA256SUMS-portable-linux-arm64.txt",
    "SHA256SUMS-portable-linux-x64.txt",
    "SHA256SUMS-portable-win-arm64.txt",
    "SHA256SUMS-portable-win-x64.txt",
    "SHA256SUMS.txt",
  ]);
});

test("the release's files are the ones the update check looks for", () => {
  // crates/sk-core/src/update asset_templates: every template the app can pick is published, by
  // the shell that picks it
  const src = readFileSync(new URL("../../crates/sk-core/src/update/mod.rs", import.meta.url), "utf8");
  const body = src.slice(src.indexOf("pub fn asset_templates"), src.indexOf("pub fn asset_template("));
  const templates = new Map();
  let shell = null;
  for (const line of body.split("\n")) {
    const s = /Shell::(Tauri|Electron)\)/.exec(line);
    if (s) shell = s[1].toLowerCase();
    for (const m of line.matchAll(/"((?:SpaceKnife|spaceknife)[^"]*)"/g)) {
      const t = m[1].replace("{{v}}", "{v}");
      for (const a of t.includes("{a}") ? ["x64", "arm64"] : [null]) templates.set(a ? t.replace("{a}", a) : t, shell);
    }
  }
  assert.ok(templates.size >= 12, [...templates.keys()].join(", "));
  const published = new Map(release.expected().map((e) => [e.template, e.shell]));
  for (const [t, s] of templates) {
    assert.ok(published.has(t), `${t} is picked by the app but not published`);
    assert.equal(published.get(t), s, `${t}: published from the ${published.get(t)} shell, picked by the ${s} shell`);
  }
});

test("a wrong checksum, a missing file and a stray file are refused", () => {
  const t = tmp();
  const a = artifacts(join(t, "a"));
  writeFileSync(join(a, "SpaceKnife-portable-win-x64", `SpaceKnife-${V}-win-x64-portable.zip`), "changed");
  const mac = join(a, "SpaceKnife-mac-x64");
  unlinkSync(join(mac, `SpaceKnife-${V}-mac-x64.dmg`));
  writeFileSync(join(mac, "SHA256SUMS-mac-x64.txt"), `${readFileSync(join(mac, "SHA256SUMS-mac-x64.txt"), "utf8").split("\n")[1]}\n`);
  put(join(a, "SpaceKnife-portable-win-x64"), "debug.log", "x");
  // the Tauri bundler's own name for the deb is no release file
  put(join(a, "SpaceKnife-portable-linux-x64"), `SpaceKnife_${V}_amd64.deb`, "x"); // another folder: names differ in case only
  assert.throws(
    () => release.assemble(V, a, join(t, "out")),
    (e) => {
      assert.ok(e instanceof release.ReleaseError);
      assert.match(e.message, new RegExp(`SpaceKnife-${V}-win-x64-portable.zip: SHA-256`));
      assert.ok(e.message.includes(`missing: SpaceKnife-${V}-mac-x64.dmg`));
      assert.ok(e.message.includes("not part of a release: debug.log"));
      assert.ok(e.message.includes(`not part of a release: SpaceKnife_${V}_amd64.deb`));
      return true;
    },
  );
  assert.ok(!existsSync(join(t, "out")));
});

test("an installer goes into SHA256SUMS.txt, so it must be in an installer list", () => {
  const t = tmp();
  const a = artifacts(join(t, "a"));
  // the deb listed in a portable list only: 0.3.4 reads SHA256SUMS.txt for it
  const deb = `spaceknife_${V}_amd64.deb`;
  const folder = join(a, "SpaceKnife-installers-linux-x64");
  unlinkSync(join(folder, "SHA256SUMS-installers-linux-x64.txt"));
  writeFileSync(join(folder, "SHA256SUMS-portable-linux-x64.txt"), `${release.sha256(join(folder, deb))}  ${deb}\n`);
  assert.throws(() => release.assemble(V, a, join(t, "out")), (e) => e.message.includes(`${deb} is in no SHA256SUMS-installers-*.txt`));
});

test("a file of another version is not taken", () => {
  const t = tmp();
  assert.throws(() => release.assemble(V, artifacts(join(t, "a"), "0.3.9"), join(t, "out")), new RegExp(`missing: SpaceKnife-${V}-Setup-x64.exe`));
});

test("a pre-release's packages may spell the version with a tilde", () => {
  const t = tmp();
  const v = "0.5.0-beta.1";
  release.assemble(v, artifacts(join(t, "a"), v, { pkgVersion: "0.5.0~beta.1" }), join(t, "out"));
  assert.ok(existsSync(join(t, "out", "spaceknife-0.5.0~beta.1.x86_64.rpm")));
  assert.ok(existsSync(join(t, "out", "spaceknife_0.5.0~beta.1_amd64.deb")));
});

test("the notes get sizes, version and runs", () => {
  const t = tmp();
  const done = release.assemble(V, artifacts(join(t, "a")), join(t, "out"));
  writeFileSync(join(t, "out", `SpaceKnife-${V}-Setup-x64.exe`), Buffer.alloc((3 << 20) + 600_000, 120));
  const notes = release.fillNotes(
    `## SpaceKnife {{version}}\n{{build_runs}}\n| \`x\` | {{ size:SpaceKnife-${V}-Setup-x64.exe }} |\n`,
    V,
    done.files,
    ["https://github.com/DuskXi/SpaceKnife/actions/runs/123"],
  );
  assert.ok(notes.includes(`## SpaceKnife ${V}`));
  assert.ok(notes.includes("[run 123](https://github.com/DuskXi/SpaceKnife/actions/runs/123)"));
  assert.ok(notes.includes("| 4 MB |")); // 3.57 MiB rounds to 4, written MB as in every release
});

test("bad placeholders are refused", () => {
  const t = tmp();
  const done = release.assemble(V, artifacts(join(t, "a")), join(t, "out"));
  assert.throws(
    () => release.fillNotes(`${V} {{size:nope.exe}} {{colour}} {{build_runs}}`, V, done.files, []),
    (e) => e.message.includes("nope.exe") && e.message.includes("{{colour}}") && e.message.includes("--build-run"),
  );
});

test("the command line", () => {
  const t = tmp();
  const a = artifacts(join(t, "a"));
  writeFileSync(join(t, "n.md"), "SpaceKnife {{version}}");
  const run = () => spawnSync(process.execPath, [SCRIPT, "assemble", V, a, join(t, "out"), "--notes", join(t, "n.md"), "--notes-out", join(t, "filled.md")], { encoding: "utf8" });
  assert.equal(run().status, 0);
  assert.equal(readFileSync(join(t, "filled.md"), "utf8"), `SpaceKnife ${V}`);
  assert.equal(run().status, 1); // out is not empty now
});
