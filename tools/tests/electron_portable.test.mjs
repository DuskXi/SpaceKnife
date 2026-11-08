// apps/electron-shell/scripts/portable.mjs: the Electron shell's portable copy for Linux, under
// 0.3.4's name, laid out as 0.3.4's 「无缝更新」 needs it (tools/compat034.mjs
// portableLayoutProblems), with the swap helper and the data folder.
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";

import { CLI, HELPER, NOTE, PROGRAM, layout, pack, portableName, unpackedDir } from "../../apps/electron-shell/scripts/portable.mjs";
import { pattern034, portableLayoutProblems } from "../compat034.mjs";
import { archiveEntries } from "../package_checks.mjs";

const POSIX = process.platform !== "win32";

/** electron-builder's unpacked Linux app, as far as the portable copy looks at it. */
function unpacked(root) {
  const files = {
    [PROGRAM]: "electron",
    [CLI]: "cli",
    "chrome-sandbox": "sandbox",
    "resources/app.asar": "asar",
    "resources/ui/index.html": "<!doctype html>",
    "resources/licenses/LICENSE.txt": "MIT",
    "locales/en-US.pak": "pak",
  };
  for (const [rel, text] of Object.entries(files)) {
    mkdirSync(dirname(join(root, rel)), { recursive: true });
    writeFileSync(join(root, rel), text, { mode: rel === PROGRAM || rel === CLI ? 0o755 : 0o644 });
  }
  return root;
}

test("the name is 0.3.4's for the Linux portable copy, of either architecture", () => {
  for (const arch of ["x64", "arm64"]) {
    const name = `${portableName("0.3.5", arch)}.tar.gz`;
    assert.equal(name, `SpaceKnife-0.3.5-linux-${arch}-portable.tar.gz`);
    assert.ok(pattern034(`SpaceKnife-{v}-linux-${arch}-portable.tar.gz`).test(name), name);
  }
  assert.ok(unpackedDir("/d", "x64").endsWith("linux-unpacked"));
  assert.ok(unpackedDir("/d", "arm64").endsWith("linux-arm64-unpacked"));
});

test("the folder holds the app, the swap helper 0.3.4 runs, and a data folder", () => {
  const t = mkdtempSync(join(tmpdir(), "sk-eportable-"));
  const folder = layout(unpacked(join(t, "linux-unpacked")), join(t, "work", portableName("0.3.5", "x64")));
  // the helper is spaceknife-cli under the name 0.3.4 looks for
  assert.equal(readFileSync(join(folder, HELPER), "utf8"), "cli");
  assert.equal(readFileSync(join(folder, "data", "README.txt"), "utf8"), NOTE);
  assert.equal(readFileSync(join(folder, "resources", "app.asar"), "utf8"), "asar");
  if (POSIX) for (const p of [PROGRAM, CLI, HELPER]) assert.ok(statSync(join(folder, p)).mode & 0o100, `${p} is not executable`);
  // the unpacked app must be complete
  assert.throws(() => layout(join(t, "nothing"), join(t, "work2", "x")), /is missing/);
});

test("the archive is what 0.3.4's 「无缝更新」 takes", (t) => {
  if (process.platform !== "linux") return t.skip("packed with GNU tar on Linux only");
  const tmp = mkdtempSync(join(tmpdir(), "sk-eportable-"));
  const name = portableName("0.3.5", "arm64");
  layout(unpacked(join(tmp, "linux-arm64-unpacked")), join(tmp, "work", name));
  const archive = pack(join(tmp, "work"), name, join(tmp, `${name}.tar.gz`));
  const entries = archiveEntries(readFileSync(archive), archive);
  assert.deepEqual(portableLayoutProblems(`${name}.tar.gz`, entries), []);
  assert.ok(entries.some((e) => e.name === `${name}/resources/ui/index.html`));
});
