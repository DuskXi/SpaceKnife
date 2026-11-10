// tools/compat034.mjs: judging what 0.3.4's own update code finds in a release. 0.3.4's update.ts
// is the frozen copy in fixtures/ (git show v0.3.4:apps/electron/src/main/update.ts); the Python
// half runs in the Release workflows, with v0.3.4's environment.
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { CONTRACT, acceptedBy034, judge, loadUpdateTs, pattern034, portableLayoutProblems, wantName } from "../compat034.mjs";
import { expected } from "../release.mjs";

const V = "0.3.5";
const mod = await loadUpdateTs(readFileSync(new URL("./fixtures/v0.3.4-update.ts", import.meta.url), "utf8"));

/** What tools/compat034_check.py prints when 0.3.4 finds everything. */
function goodResult(version = V) {
  return {
    found: true,
    offered: version,
    assets: [],
    installs: CONTRACT.map(([kind, system, machine, template]) => ({ kind, system, machine, template, asset: wantName(kind, template, version), sha256: "a".repeat(64), sums_order: ["SHA256SUMS.txt"] })),
  };
}

test("0.3.4's window takes the release's files under 0.3.4's names", () => {
  for (const [kind, system, , template] of CONTRACT) {
    assert.equal(acceptedBy034(mod, wantName(kind, template, V), kind, system), null, `${kind} ${system}`);
  }
  // the names Tauri's bundler gives its files: 0.3.4 would never run them
  for (const [name, kind, platform] of [
    ["SpaceKnife_0.3.5_x64-setup.exe", "nsis", "win32"],
    ["SpaceKnife_0.3.5_amd64.AppImage", "appimage", "linux"],
    ["SpaceKnife_0.3.5_amd64.deb", "deb", "linux"],
    ["SpaceKnife-0.3.5-1.x86_64.rpm", "rpm", "linux"],
    ["SpaceKnife_0.3.5_aarch64.dmg", "dmg", "darwin"],
  ]) {
    assert.equal(acceptedBy034(mod, name, kind, platform), "updateNotAnUpdate", name);
  }
  assert.equal(mod.portableVersion(`SpaceKnife-${V}-win-arm64-portable.zip`), V);
});

test("a release 0.3.4 updates to passes", () => {
  assert.deepEqual(judge(goodResult(), V, mod, null), []);
});

test("what 0.3.4 would not find, check or run is a problem", () => {
  const r = goodResult();
  r.offered = "0.3.4";
  r.installs.find((i) => i.kind === "deb").asset = null; // renamed: 0.3.4 picks nothing
  r.installs.find((i) => i.kind === "rpm").sha256 = null; // not in the list 0.3.4 reads
  r.installs.find((i) => i.kind === "nsis").error = "取不到这个版本发布的校验值（HTTP 404），没有下载";
  const problems = judge(r, V, mod, null);
  assert.ok(problems.some((p) => p.includes("offers 0.3.4")));
  assert.ok(problems.some((p) => p.startsWith("deb on linux/x86_64: 0.3.4 picks null")));
  assert.ok(problems.some((p) => p.includes("spaceknife_0.3.5_amd64.deb is published but no 0.3.4 install picks it")));
  assert.ok(problems.some((p) => p.startsWith("rpm on linux/x86_64: 0.3.4 finds no checksum")));
  assert.ok(problems.some((p) => p.startsWith("nsis on win32/AMD64: 0.3.4 cannot read the checksums")));
  assert.deepEqual(judge({ found: false }, V, mod, null), [`${V} is not in the release list 0.3.4 reads`]);
});

test("a pre-release's packages are looked for with a tilde", () => {
  assert.equal(wantName("deb", "spaceknife_{v}_amd64.deb", "0.4.0-beta.1"), "spaceknife_0.4.0~beta.1_amd64.deb");
  assert.equal(wantName("portable", "SpaceKnife-{v}-win-x64-portable.zip", "0.4.0-beta.1"), "SpaceKnife-0.4.0-beta.1-win-x64-portable.zip");
  assert.deepEqual(judge(goodResult("0.4.0-beta.1"), "0.4.0-beta.1", mod, null), []);
});

test("portable archives are laid out as 0.3.4's swap needs them", () => {
  const top = `SpaceKnife-${V}-win-x64-portable`;
  const win = [`${top}/`, `${top}/SpaceKnife.exe`, `${top}/spaceknife-cli.exe`, `${top}/resources/backend/spaceknife-backend.exe`, `${top}/data/README.txt`].map((name) => ({ name, mode: null }));
  assert.deepEqual(portableLayoutProblems(`${top}.zip`, win), []);
  // Tauri's own program name: 0.3.4 compares the names exactly
  const lower = win.map((e) => ({ ...e, name: e.name.replace("SpaceKnife.exe", "spaceknife.exe") }));
  assert.match(portableLayoutProblems(`${top}.zip`, lower).join("\n"), /no .*\/SpaceKnife\.exe/);
  // no swap helper: 0.3.4 stops with updateUnpackFailed
  assert.match(portableLayoutProblems(`${top}.zip`, win.filter((e) => !e.name.includes("backend"))).join("\n"), /spaceknife-backend\.exe/);
  assert.match(portableLayoutProblems(`${top}.zip`, [...win, { name: "other/x", mode: null }]).join("\n"), /2 folders at the top/);

  const ltop = `SpaceKnife-${V}-linux-x64-portable`;
  const linux = [`${ltop}/spaceknife`, `${ltop}/resources/backend/spaceknife-backend`, `${ltop}/data/README.txt`].map((name) => ({ name, mode: 0o755 }));
  assert.deepEqual(portableLayoutProblems(`${ltop}.tar.gz`, linux), []);
  const noExec = linux.map((e) => (e.name.endsWith("spaceknife-backend") ? { ...e, mode: 0o644 } : e));
  assert.match(portableLayoutProblems(`${ltop}.tar.gz`, noExec).join("\n"), /not executable/);

  const mtop = `SpaceKnife-${V}-mac-arm64-portable`;
  const app = `${mtop}/SpaceKnife.app/Contents`;
  const mac = [`${app}/MacOS/SpaceKnife`, `${app}/MacOS/spaceknife-cli`, `${app}/Resources/backend/spaceknife-backend`, `${mtop}/data/README.txt`, `__MACOSX/${mtop}/._data`].map((name) => ({ name, mode: 0o755 }));
  assert.deepEqual(portableLayoutProblems(`${mtop}.zip`, mac), []);
  const tauriName = mac.map((e) => ({ ...e, name: e.name.replace("MacOS/SpaceKnife", "MacOS/spaceknife") }));
  assert.match(portableLayoutProblems(`${mtop}.zip`, tauriName).join("\n"), /Contents\/MacOS\/SpaceKnife/);
});

test("0.3.4's own patterns never take a file of a kind of install 0.3.4 did not have", () => {
  // v0.3.4 update.py pick_asset: re.escape(template) with {v} as [0-9A-Za-z.+~_-]+, fullmatch,
  // case-sensitive, for every template of every kind; the Tauri app's Linux portable copy must
  // stay out of reach of every 0.3.4 copy (one on RHEL 9 has no WebKitGTK 4.1 to run it)
  const fresh = expected().filter((e) => e.since);
  assert.ok(fresh.length >= 2);
  for (const e of fresh) {
    for (const v of [V, "0.4.0-beta.1", "0.4.0~beta.1", "1.0.0+build.7", "10.20.30"]) {
      const name = e.template.replace("{v}", v);
      for (const [kind, , , template] of CONTRACT) assert.ok(!pattern034(template).test(name), `0.3.4's ${kind} ${template} takes ${name}`);
    }
  }
  // and 0.3.4's window would not run it either, were it in its updates folder
  for (const a of ["x64", "arm64"]) {
    const name = `SpaceKnife-${V}-linux-${a}-tauri.tar.gz`;
    assert.equal(acceptedBy034(mod, name, "portable", "linux"), "updateNotAnUpdate", name);
    assert.equal(mod.portableVersion(name), null);
  }
  // the pattern is 0.3.4's: it takes its own names, also with a version spelled otherwise
  assert.ok(pattern034("SpaceKnife-{v}-linux-x64-portable.tar.gz").test(`SpaceKnife-${V}-linux-x64-portable.tar.gz`));
  assert.ok(pattern034("spaceknife_{v}_amd64.deb").test("spaceknife_0.4.0~beta.1_amd64.deb"));
  assert.ok(!pattern034("SpaceKnife-{v}-linux-x64-portable.tar.gz").test(`spaceknife-${V}-linux-x64-portable.tar.gz`));
  assert.ok(!pattern034("SpaceKnife-{v}-linux-x64-portable.tar.gz").test(`SpaceKnife-${V}-linux-x64-portable.tar.gz.sig`));
});

test("a file of a kind 0.3.4 did not have, taken by a 0.3.4 install, is a problem", () => {
  const tauri = `SpaceKnife-${V}-linux-x64-tauri.tar.gz`;
  // what tools/compat034_check.py says 0.3.4's pick_asset takes from a release with just that file
  const ok = { ...goodResult(), takers: { [tauri]: [], [`SpaceKnife-${V}-linux-x64-portable.tar.gz`]: ["portable linux/x86_64"] } };
  assert.deepEqual(judge(ok, V, mod, null), []);
  const taken = { ...goodResult(), takers: { [tauri]: ["portable linux/x86_64"] } };
  assert.match(judge(taken, V, mod, null).join("\n"), /linux-x64-tauri\.tar\.gz.*0\.3\.4.*portable linux\/x86_64/);
  const picked = goodResult();
  picked.installs.find((i) => i.kind === "portable" && i.machine === "x86_64" && i.system === "linux").asset = tauri;
  assert.ok(judge(picked, V, mod, null).some((p) => p.includes(tauri) && p.includes("0.3.4")));
});
