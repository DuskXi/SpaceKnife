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

/** The artifacts of a release's build runs, laid out as `gh run download` writes them. */
function artifacts(root, version = V, { rpmVersion = version } = {}) {
  const groups = {
    "SpaceKnife-installers-win-x64": [[`SpaceKnife_${version}_x64-setup.exe`], "SHA256SUMS-installers-win-x64.txt"],
    "SpaceKnife-installers-linux-x64": [
      [`SpaceKnife_${version}_amd64.AppImage`, `SpaceKnife_${version}_amd64.deb`, `SpaceKnife-${rpmVersion}-1.x86_64.rpm`],
      "SHA256SUMS-installers-linux-x64.txt",
    ],
  };
  for (const [target, ext] of [["win-x64", "zip"], ["win-arm64", "zip"], ["linux-x64", "tar.gz"], ["linux-arm64", "tar.gz"]]) {
    groups[`SpaceKnife-portable-${target}`] = [[`SpaceKnife-${version}-${target}-portable.${ext}`], `SHA256SUMS-portable-${target}.txt`];
  }
  for (const [a, dmg] of [["arm64", "aarch64"], ["x64", "x64"]]) {
    groups[`SpaceKnife-mac-${a}`] = [[`SpaceKnife_${version}_${dmg}.dmg`, `SpaceKnife-${version}-mac-${a}-portable.zip`], `SHA256SUMS-mac-${a}.txt`];
  }
  for (const [folder, [names, sums]] of Object.entries(groups)) {
    const d = join(root, folder);
    const lines = names.map((n) => `${put(d, n, n.repeat(3))}  ${n}\n`);
    writeFileSync(join(d, sums), lines.join(""));
  }
  return root;
}

test("a complete set becomes the release", () => {
  const t = tmp();
  const done = release.assemble(V, artifacts(join(t, "a")), join(t, "out"));
  const names = new Set(readdirSync(join(t, "out")));
  const want = new Set([...release.expected().map((e) => e.template.replace("{v}", V)), ...release.publishedSums()]);
  assert.deepEqual([...names].sort(), [...want].sort());
  assert.ok(!names.has("SHA256SUMS-installers-win-x64.txt"));
  const merged = release.parseSums(readFileSync(join(t, "out", "SHA256SUMS.txt"), "utf8"));
  assert.deepEqual([...merged.keys()].sort(), [`SpaceKnife-${V}-1.x86_64.rpm`, `SpaceKnife_${V}_amd64.AppImage`, `SpaceKnife_${V}_amd64.deb`, `SpaceKnife_${V}_x64-setup.exe`]);
  for (const [name, digest] of merged) {
    assert.equal(release.sha256(join(t, "out", name)), digest);
    assert.equal(done.hashes.get(name), digest);
  }
});

test("the release's files are the ones the update check looks for", () => {
  // crates/sk-core/src/update asset_templates: every template the app can pick is published
  const src = readFileSync(new URL("../../crates/sk-core/src/update/mod.rs", import.meta.url), "utf8");
  const body = src.slice(src.indexOf("pub fn asset_templates"), src.indexOf("pub fn asset_template("));
  const templates = new Set();
  for (const m of body.matchAll(/"(SpaceKnife[^"]*)"/g)) {
    const t = m[1].replace("{{v}}", "{v}");
    if (t.includes("{a}")) for (const a of ["x64", "arm64"]) templates.add(t.replace("{a}", a));
    else templates.add(t);
  }
  assert.ok(templates.size >= 10);
  const published = new Set(release.expected().map((e) => e.template));
  for (const t of templates) {
    // a Linux portable archive for an architecture without builds is never published
    assert.ok(published.has(t), `${t} is picked by the app but not published`);
  }
});

test("a wrong checksum, a missing file and a stray file are refused", () => {
  const t = tmp();
  const a = artifacts(join(t, "a"));
  writeFileSync(join(a, "SpaceKnife-portable-win-x64", `SpaceKnife-${V}-win-x64-portable.zip`), "changed");
  const mac = join(a, "SpaceKnife-mac-x64");
  unlinkSync(join(mac, `SpaceKnife_${V}_x64.dmg`));
  writeFileSync(join(mac, "SHA256SUMS-mac-x64.txt"), `${readFileSync(join(mac, "SHA256SUMS-mac-x64.txt"), "utf8").split("\n")[1]}\n`);
  put(join(a, "SpaceKnife-portable-win-x64"), "debug.log", "x");
  assert.throws(
    () => release.assemble(V, a, join(t, "out")),
    (e) => {
      assert.ok(e instanceof release.ReleaseError);
      assert.match(e.message, new RegExp(`SpaceKnife-${V}-win-x64-portable.zip: SHA-256`));
      assert.ok(e.message.includes(`missing: SpaceKnife_${V}_x64.dmg`));
      assert.ok(e.message.includes("not part of a release: debug.log"));
      return true;
    },
  );
  assert.ok(!existsSync(join(t, "out")));
});

test("a file of another version is not taken", () => {
  const t = tmp();
  assert.throws(() => release.assemble(V, artifacts(join(t, "a"), "0.3.9"), join(t, "out")), new RegExp(`missing: SpaceKnife_${V}_x64-setup.exe`));
});

test("a pre-release's rpm may spell the version with a tilde", () => {
  const t = tmp();
  const v = "0.5.0-beta.1";
  release.assemble(v, artifacts(join(t, "a"), v, { rpmVersion: "0.5.0~beta.1" }), join(t, "out"));
  assert.ok(existsSync(join(t, "out", "SpaceKnife-0.5.0~beta.1-1.x86_64.rpm")));
});

test("the notes get sizes, version and runs", () => {
  const t = tmp();
  const done = release.assemble(V, artifacts(join(t, "a")), join(t, "out"));
  writeFileSync(join(t, "out", `SpaceKnife_${V}_x64-setup.exe`), Buffer.alloc((3 << 20) + 600_000, 120));
  const notes = release.fillNotes(
    `## SpaceKnife {{version}}\n{{build_runs}}\n| \`x\` | {{ size:SpaceKnife_${V}_x64-setup.exe }} |\n`,
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
