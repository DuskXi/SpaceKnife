// packaging/lib.mjs: what the packaging end-to-end tests share — following a portable copy's swap
// by its log and the new version's ready file, comparing an updated folder with the candidate,
// finding and ending the processes a test left behind.
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { appendFileSync, existsSync, mkdirSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { join, sep } from "node:path";
import { test } from "node:test";

import { alternativesGroup, asarFilesIn, isElectron, notOwned, pidAlive, processesUnder, sleep, stopUnder, swapEnding, treeDiff, treeOf, uiIn, untilUninstalled, watchSwap } from "../../packaging/lib.mjs";
import { tempFolders } from "./temp.mjs";

const tmp = tempFolders("sk-packaging-lib-");
const line = (msg) => `2026-11-08 10:00:00 ${msg}\n`;

test("every way the swap's log can end is recognised", () => {
  const log = (msg) => line("the new version starts here (webview 2.44.0)") + line("waiting for SpaceKnife (42) to quit") + line(msg);
  assert.equal(swapEnding(log("updated")), "updated");
  assert.equal(swapEnding(log("SpaceKnife did not quit: kept the old version")), "kept");
  assert.equal(swapEnding(log("kept the old version")), "kept");
  assert.equal(swapEnding(line("not started: the new version can't start on this computer: spaceknife did not answer --startup-check within 20 s")), "refused");
  // the files are in place, the program did not start: the last line, which no wait may miss
  assert.equal(swapEnding(log("updated") + line("could not start SpaceKnife: Permission denied (os error 13)")), "start-failed");
  assert.equal(swapEnding(log("kept the old version") + line("could not start SpaceKnife: x")), "start-failed");
  for (const going of ["waiting for SpaceKnife (42) to quit", "busy: resources: Access is denied. (os error 5)", "could not move the new files in: x"]) {
    assert.equal(swapEnding(log(going)), null, going);
  }
  assert.equal(swapEnding(""), null);
  // the .asar files taken out of the unpacked version (leave_removable) come before the last line
  const took = line("took resources/app.asar out of /x/updates/staging-0.3.6/y (an Electron app can't remove a folder that holds one)");
  assert.equal(swapEnding(took + line("not started: /x holds what is not SpaceKnife's: notes.txt")), "refused");
  assert.equal(swapEnding(log("could not move the new files in: x") + took + line("kept the old version")), "kept");
  assert.equal(swapEnding(log("waiting for SpaceKnife (42) to quit") + took), null);
  // the words are the helper's own (crates/sk-core/src/update/swap.rs): renaming one there must fail here
  const swap = readFileSync(new URL("../../crates/sk-core/src/update/swap.rs", import.meta.url), "utf8");
  for (const said of ['log.say("updated")', '"kept the old version"', '"SpaceKnife did not quit: kept the old version"', '"not started: {e}"', '"could not start SpaceKnife: {e}"', "(an Electron app can't remove a folder that holds one)"]) {
    assert.ok(swap.includes(said), `swap.rs no longer says ${said}`);
  }
});

test("a swap whose log is removed right after its last line still ends: the new version's ready file says so", async () => {
  const t = tmp();
  const log = join(t, "update-0.3.5.log");
  const ready = join(t, "ready.json");
  writeFileSync(log, line("the new version starts here (webview 2.44.0)") + line("waiting for SpaceKnife (42) to quit"));
  const watching = watchSwap({ log, ready, oldPids: [42], timeout: 5_000, every: 100 });
  await sleep(250);
  // the helper writes its last line and starts the new version, which at once clears the updates
  // folder of everything of its version, the log among it (sk_core::update::clean_updates)
  appendFileSync(log, line("updated"));
  rmSync(log);
  writeFileSync(ready, JSON.stringify({ pid: 4243, url: "ws://127.0.0.1:1/", token: "t" }));
  const r = await watching;
  assert.equal(r.how, "updated");
  assert.equal(r.ready.pid, 4243);
  assert.ok(r.cleaned);
  assert.match(r.text, /waiting for SpaceKnife \(42\) to quit/);
});

test("a ready file of the old version's process is not the new version", async () => {
  const t = tmp();
  const log = join(t, "update-0.3.5.log");
  const ready = join(t, "ready.json");
  writeFileSync(log, line("waiting for SpaceKnife (42) to quit"));
  writeFileSync(ready, JSON.stringify({ pid: 42 }));
  await assert.rejects(watchSwap({ log, ready, oldPids: [42], timeout: 400, every: 50 }), (e) => e.message.includes("waiting for SpaceKnife (42) to quit") && e.swapLog.includes("(42)"));
});

test("a swap that could not start the program ends the wait at once", async () => {
  const t = tmp();
  const log = join(t, "update-0.3.5.log");
  writeFileSync(log, line("updated") + line("could not start SpaceKnife: Permission denied (os error 13)"));
  const started = Date.now();
  const r = await watchSwap({ log, timeout: 5_000 });
  assert.equal(r.how, "start-failed");
  assert.ok(Date.now() - started < 2_000);
});

test("a swap that never ends times out with what its log said, or that it said nothing", async () => {
  const t = tmp();
  await assert.rejects(watchSwap({ log: join(t, "none.log"), timeout: 200, every: 50 }), /empty or was never written/);
});

test("an updated folder is compared with the candidate, file by file", () => {
  const t = tmp();
  const want = join(t, "candidate");
  const have = join(t, "copy");
  for (const root of [want, have]) {
    mkdirSync(join(root, "resources", "backend"), { recursive: true });
    writeFileSync(join(root, "SpaceKnife.com"), "new launcher");
    writeFileSync(join(root, "resources", "backend", "spaceknife-backend.exe"), "helper");
  }
  mkdirSync(join(have, "data", "data"), { recursive: true });
  writeFileSync(join(have, "data", "data", "settings.json"), "{}");
  assert.deepEqual(treeDiff(want, have, ["data"]), { missing: [], different: [], extra: [] });
  // an old launcher left in place, a file of the old app nobody took away, one the new version lacks
  writeFileSync(join(have, "SpaceKnife.com"), "0.3.4's launcher");
  writeFileSync(join(have, "resources", "app.asar"), "0.3.4");
  writeFileSync(join(want, "LICENSE.txt"), "MIT");
  assert.deepEqual(treeDiff(want, have, ["data"]), { missing: ["LICENSE.txt"], different: ["SpaceKnife.com"], extra: [join("resources", "app.asar")] });
  // without skipping it, the data folder is not the candidate's
  assert.deepEqual(treeDiff(want, have).extra, ["data", join("resources", "app.asar")]);
});

test("links are compared by their target", (t) => {
  if (process.platform === "win32") return t.skip("links need privileges on Windows");
  const d = tmp();
  for (const [root, target] of [["a", "Versions/A"], ["b", "Versions/B"]]) {
    mkdirSync(join(d, root), { recursive: true });
    symlinkSync(target, join(d, root, "Current"));
  }
  assert.deepEqual(treeDiff(join(d, "a"), join(d, "b")).different, ["Current (link to Versions/B, want Versions/A)"]);
});

test("what a package does not own in its folder is found: the folders its removal leaves, files made later", () => {
  const d = tmp();
  try {
    const root = join(d, "SpaceKnife");
    const ui = join(root, "resources", "ui");
    mkdirSync(ui, { recursive: true });
    writeFileSync(join(root, "spaceknife"), "x");
    writeFileSync(join(ui, "index.html"), "x");
    // a package that lists its files only (0.3.4's rpm): its folders stay after a removal
    const files = new Set([join(root, "spaceknife"), join(ui, "index.html")]);
    assert.deepEqual(notOwned(root, files), [`dir ${root}`, `dir ${join(root, "resources")}`, `dir ${ui}`]);
    // one that owns its folders: nothing, until a file is made there or another's folder is left
    const all = new Set([...files, root, join(root, "resources"), ui]);
    assert.deepEqual(notOwned(root, all), []);
    mkdirSync(join(root, "resources", "backend", "_internal"), { recursive: true });
    writeFileSync(join(root, "core"), "x");
    if (process.platform !== "win32") symlinkSync("/etc", join(root, "etc")); // not followed
    assert.deepEqual(notOwned(root, all), [
      `file ${join(root, "core")}`,
      ...(process.platform !== "win32" ? [`link ${join(root, "etc")}`] : []),
      `dir ${join(root, "resources", "backend")}`,
      `dir ${join(root, "resources", "backend", "_internal")}`,
    ]);
    assert.deepEqual(treeOf(join(d, "nothing")), []);
    assert.deepEqual(notOwned(join(d, "nothing"), all), []);
  } finally {
    rmSync(d, { recursive: true, force: true });
  }
});

test("the interface and the shell are found wherever a kind of portable copy keeps them", () => {
  const t = tmp();
  const put = (rel) => {
    mkdirSync(join(t, ...rel.slice(0, -1)), { recursive: true });
    writeFileSync(join(t, ...rel), "x");
  };
  assert.equal(uiIn(t), null);
  assert.equal(isElectron(t), false);
  put(["SpaceKnife.app", "Contents", "Resources", "ui", "index.html"]);
  assert.equal(uiIn(t), join(t, "SpaceKnife.app", "Contents", "Resources", "ui", "index.html"));
  put(["resources", "ui", "index.html"]);
  put(["resources", "app.asar"]);
  assert.equal(uiIn(t), join(t, "resources", "ui", "index.html"));
  assert.equal(isElectron(t), true);
});

test("the .asar files an Electron app could not remove are found, and only those", (t) => {
  const dir = tmp();
  const put = (rel, text = "x") => {
    mkdirSync(join(dir, ...rel.slice(0, -1)), { recursive: true });
    writeFileSync(join(dir, ...rel), text);
  };
  assert.deepEqual(asarFilesIn(join(dir, "missing")), []);
  put(["top", "resources", "app.asar"]);
  put(["top", "resources", "deep", "Other.ASAR"]);
  put(["top", "resources", "app.asar.unpacked", "native.node"]);
  put(["top", "resources", "folder.asar", "inside.txt"]); // a folder named so: Electron leaves it alone
  put(["top", "resources", "notes.asar.txt"]);
  assert.deepEqual(asarFilesIn(dir), [join(dir, "top", "resources", "app.asar"), join(dir, "top", "resources", "deep", "Other.ASAR")]);
  // a link is not followed
  const outside = tmp();
  writeFileSync(join(outside, "theirs.asar"), "x");
  try {
    symlinkSync(outside, join(dir, "top", "linked"), "junction");
  } catch (e) {
    return t.skip(`no link here: ${e.code}`);
  }
  assert.equal(asarFilesIn(dir).length, 2);
});

test("the alternatives group is read the way each system's tool answers", () => {
  // what the tools answer (spawnSync's result): Debian's update-alternatives knows --query; Red
  // Hat's update-alternatives is its `alternatives` (chkconfig), which prints its usage on stdout
  // for --query and exits 2, and answers --display
  const missing = { error: Object.assign(new Error("spawnSync ENOENT"), { code: "ENOENT" }) };
  const usage = { status: 2, stdout: "alternatives version 1.24 - Copyright (C) 2001 Red Hat, Inc.\nusage: alternatives --install <link> <name> <path> <priority>\n", stderr: "" };
  const debian = "Name: spaceknife\nLink: /usr/bin/spaceknife\nStatus: auto\nBest: /opt/SpaceKnife/spaceknife\nValue: /opt/SpaceKnife/spaceknife\n\nAlternative: /opt/SpaceKnife/spaceknife\nPriority: 100\n";
  const redhat = "spaceknife - status is auto.\n link currently points to /opt/SpaceKnife/spaceknife\n/opt/SpaceKnife/spaceknife - priority 100\nCurrent `best' version is /opt/SpaceKnife/spaceknife.\n";
  const system = (answers) => {
    const asked = [];
    const run = (cmd, argv) => {
      asked.push([cmd, ...argv].join(" "));
      return answers[cmd] ?? missing;
    };
    return { run, asked };
  };
  const cases = [
    ["Debian, registered", { "update-alternatives": { status: 0, stdout: debian, stderr: "" } }, debian],
    ["Debian, none", { "update-alternatives": { status: 2, stdout: "", stderr: "update-alternatives: error: no alternatives for spaceknife\n" } }, null],
    ["Red Hat, registered", { "update-alternatives": usage, alternatives: { status: 0, stdout: redhat, stderr: "" } }, redhat],
    ["Red Hat, none", { "update-alternatives": usage, alternatives: { status: 2, stdout: "", stderr: "failed to read link /usr/bin/spaceknife: No such file or directory\n" } }, null],
    ["no tool", {}, null],
  ];
  for (const [what, answers, want] of cases) {
    const { run, asked } = system(answers);
    assert.equal(alternativesGroup("spaceknife", run), want, what);
    assert.equal(asked[0], "update-alternatives --query spaceknife", what);
  }
});

test("the processes a test leaves in its folders are found and ended", async () => {
  // as the tests resolve their folders: the final, long path (Windows reports no 8.3 short names)
  const t = realpathSync.native(tmp());
  // a program run from the test's folder (the swap helper, the new version: in a session of their own)
  const script = join(t, "copy", "stays.mjs");
  mkdirSync(join(t, "copy"), { recursive: true });
  writeFileSync(script, "setInterval(() => {}, 1000);\n");
  const child = spawn(process.execPath, [script], { stdio: "ignore", detached: true });
  child.unref();
  try {
    await sleep(300);
    const found = processesUnder([t]);
    assert.ok(found.some((p) => p.pid === child.pid), JSON.stringify(found));
    assert.ok(!processesUnder([join(t, "elsewhere")]).some((p) => p.pid === child.pid));
    const ended = await stopUnder([t]);
    assert.ok(ended.some((p) => p.pid === child.pid));
    await sleep(300);
    assert.ok(!pidAlive(child.pid));
  } finally {
    try {
      process.kill(child.pid, "SIGKILL");
    } catch {
      // gone
    }
  }
});

test("an NSIS uninstaller is done when its own copy has ended, not when the one started exits", async () => {
  const t = realpathSync.native(tmp());
  const dir = join(t, "Programs", "SpaceKnife");
  mkdirSync(dir, { recursive: true });
  const done = join(t, "done.txt");
  // the copy as NSIS starts it: from elsewhere (%TEMP%\~nsuA.tmp\Au_.exe), the folder only on its
  // command line (`_?=<dir>\`), and its work done at the very end
  const copy = spawn(process.execPath, ["-e", "setTimeout(() => require('node:fs').writeFileSync(process.argv[1], 'done'), 1500)", done, `_?=${dir}${sep}`], { stdio: "ignore" });
  try {
    await sleep(300);
    await untilUninstalled(dir, { every: 100 });
    assert.ok(existsSync(done), "it came back while the copy was at work");
  } finally {
    try {
      process.kill(copy.pid, "SIGKILL");
    } catch {
      // gone
    }
    rmSync(t, { recursive: true, force: true });
  }
  // one that never ends: named in the timeout's message
  const stuck = [{ pid: 7, cmd: `${String.raw`C:\Temp\~nsuA.tmp\Au_.exe /S _?=C:\Programs\SpaceKnife`}\\` }];
  await assert.rejects(untilUninstalled(String.raw`C:\Programs\SpaceKnife`, { timeout: 200, every: 50, list: () => stuck }), /still at work after 0 s: 7 C:\\Temp\\~nsuA\.tmp\\Au_\.exe/);
});
