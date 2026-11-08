// The Windows installer end to end, on a CI runner only: install silently for this user, run the
// installed app (smoke-app.mjs --kind nsis: it must take itself for an installer copy, whose updates
// are the next setup.exe), update it in place the way the app does (install.rs nsis_update_args,
// 0.3.4's arguments: --updated /S --force-run /currentuser), uninstall silently and, once the
// uninstaller's own copy has ended, check the program, its entry and shortcuts, the cache and the
// updates are gone and the data stays. The installer is Tauri's with the forked NSIS template
// (apps/desktop/src-tauri/windows; windows-installer.yml), laid out as 0.3.4's was:
// %LOCALAPPDATA%\Programs\SpaceKnife\SpaceKnife.exe with spaceknife-cli.exe and SpaceKnife.com next
// to it, the uninstaller uninstall.exe, the keys under electron-builder's GUID for the appId
// (Software\{GUID} InstallLocation, Uninstall\{GUID}). The update from the real 0.3.4 installer is
// packaging/update-from-034.mjs nsis.
//
//   node packaging/nsis-e2e.mjs <SpaceKnife-<v>-Setup-x64.exe> [--previous <older Setup-x64.exe>]
//
// --previous installs that one first and upgrades it with this one, as an update does. The update
// in place checks what an update of a forked installation must do: the same folder, a stale file of
// the interface that the earlier build's list of files names gone, files the user put there (in
// the folder, in its ui folder) kept, no staging folder left, the app started again with --updated
// and the installer's environment. It installs into this user's profile, so
// it refuses to run outside GitHub Actions. Exit status 1 on failure.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { WIN, fail, pidAlive, stop, until, untilUninstalled } from "./lib.mjs";

if (!WIN) fail("the installer test runs on Windows");
if (process.env.GITHUB_ACTIONS !== "true") fail("this installs and uninstalls SpaceKnife in your profile: it runs on a CI runner only");
const args = process.argv.slice(2);
const at = args.indexOf("--previous");
const previous = at >= 0 ? resolve(args[at + 1]) : null;
// the one argument that is not --previous or its value (at is -1 without --previous)
const given = args.find((a, i) => at < 0 || (i !== at && i !== at + 1));
const setup = given ? resolve(given) : "";
if (!setup || !existsSync(setup) || !statSync(setup).isFile()) fail("usage: node packaging/nsis-e2e.mjs <Setup-x64.exe> [--previous <older Setup-x64.exe>]");

const GUID = "3cb3f1fc-eef4-5813-9008-c29b5510458c"; // electron-builder: uuid5 of the appId io.github.duskxi.spaceknife
const UPDATE_ARGS = ["--updated", "/S", "--force-run", "/currentuser"]; // install.rs nsis_update_args, 0.3.4's
const dir = join(process.env.LOCALAPPDATA, "Programs", "SpaceKnife");
const exe = join(dir, "SpaceKnife.exe");
const uninstaller = join(dir, "uninstall.exe");
const MANIFEST = "installed-files.txt"; // spaceknife.nsh SK_MANIFEST: what the installer put in the folder
const data = join(process.env.LOCALAPPDATA, "SpaceKnife"); // the backend's data (paths.rs), not the program's folder
const startMenu = join(process.env.APPDATA, "Microsoft", "Windows", "Start Menu", "Programs", "SpaceKnife.lnk");
const desktop = join(process.env.USERPROFILE, "Desktop", "SpaceKnife.lnk");
const here = fileURLToPath(new URL(".", import.meta.url));

function run(file, argv, what, env = process.env) {
  console.log(`> ${file} ${argv.join(" ")}`);
  const r = spawnSync(file, argv, { stdio: "inherit", timeout: 300_000, env });
  if (r.status !== 0) fail(`${what}: exit code ${r.status} ${r.error ?? ""}${log()}`);
}

/** The installer's own log (spaceknife.nsh SkLogLine): %TEMP%\SpaceKnife-install.log, UTF-16. */
function log() {
  const file = join(process.env.TEMP, "SpaceKnife-install.log");
  return existsSync(file) ? `\n---- ${file}\n${readFileSync(file).toString("utf16le").replace(/^﻿/, "").trim()}\n----` : "";
}

function reg(key, value) {
  const r = spawnSync("reg.exe", ["query", key, "/v", value, "/reg:64"], { encoding: "utf8" });
  return r.status === 0 ? new RegExp(`^\\s+${value}\\s+REG_\\w+\\s+(.*)$`, "m").exec(r.stdout)?.[1]?.trim() ?? "?" : null;
}
const entry = () => reg(`HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${GUID}`, "DisplayVersion");
const location = () => reg(`HKCU\\Software\\${GUID}`, "InstallLocation");

function shortcutTarget(lnk) {
  const r = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `(New-Object -ComObject WScript.Shell).CreateShortcut('${lnk.replace(/'/g, "''")}').TargetPath`], { encoding: "utf8" });
  return (r.stdout ?? "").trim();
}

function commandLine(pid) {
  const r = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `(Get-CimInstance Win32_Process -Filter "ProcessId=${pid}").CommandLine`], { encoding: "utf8" });
  return (r.stdout ?? "").trim();
}

async function installed(what) {
  await until(`${what}: ${exe}`, () => existsSync(exe) && existsSync(uninstaller), 60_000).catch((e) => fail(`${e.message}${log()}`));
  for (const f of ["spaceknife-cli.exe", "SpaceKnife.com", join("ui", "index.html"), join("licenses", "LICENSE.txt")]) {
    if (!existsSync(join(dir, f))) fail(`${what}: no ${f} next to the program`);
  }
  const loc = location();
  if (!loc || resolve(loc).toLowerCase() !== resolve(dir).toLowerCase()) fail(`${what}: HKCU\\Software\\${GUID} InstallLocation is ${loc}, not ${dir}`);
  if (existsSync(`${dir}.sk-staging`)) fail(`${what}: the staging folder ${dir}.sk-staging is left`);
  const target = existsSync(startMenu) ? shortcutTarget(startMenu) : "";
  if (resolve(target || ".").toLowerCase() !== exe.toLowerCase()) fail(`${what}: ${startMenu} points at ${target || "(missing)"}`);
  console.log(`${what}: ${readdirSync(dir).join(", ")} (uninstall entry ${entry()})`);
}

if (previous) {
  run(previous, ["/S", "/currentuser"], "installing the previous version");
  await installed("previous version");
  run(setup, UPDATE_ARGS, "upgrading it");
} else {
  run(setup, ["/S", "/currentuser"], "installing");
}
await installed("installed");
if (!entry()) fail(`no uninstall entry HKCU\\…\\Uninstall\\${GUID}`);

const smoke = spawnSync(process.execPath, [join(here, "smoke-app.mjs"), "--kind", "nsis", exe], { stdio: "inherit" });
if (smoke.status !== 0) fail("the installed app's smoke test failed");

// an update in place, as the installed app runs it: the installer starts the new version with
// --updated and its own environment (the ready file tells it from any other start)
{
  // a stale interface file an earlier build installed: on the list of files that build's installer
  // wrote (spaceknife.nsh SkWriteManifest, UTF-16 LE), which is what an update removes; files of
  // the user's — in the folder, and in its ui folder, on no list — stay
  const stale = join(dir, "ui", "stale-from-an-earlier-build.js");
  const mine = join(dir, "keep-me.txt");
  const mineInUi = join(dir, "ui", "keep-me-too.txt");
  const list = join(dir, MANIFEST);
  if (!existsSync(list)) fail(`no ${list}: the installer wrote no list of its files`);
  const listed = readFileSync(list).toString("utf16le").replace(/^﻿/, "");
  if (!listed.split("\r\n").includes("SpaceKnife.exe") || !listed.includes("ui\\index.html\r\n")) fail(`${list} does not list the program and its interface:\n${listed}`);
  writeFileSync(list, Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(`ui\\stale-from-an-earlier-build.js\r\n${listed}`, "utf16le")]));
  writeFileSync(stale, "//\n");
  writeFileSync(mine, "the user's\n");
  writeFileSync(mineInUi, "the user's\n");
  const scratch = mkdtempSync(join(tmpdir(), "sk-nsis-e2e-"));
  const ready = join(scratch, "ready.json");
  const env = { ...process.env, SPACEKNIFE_READY_FILE: ready, SPACEKNIFE_START_MINIMIZED: "1", SPACEKNIFE_HOME: join(scratch, "home") };
  run(setup, UPDATE_ARGS, "updating in place", env);
  await installed("updated in place");
  if (existsSync(stale)) fail(`the update left ${stale}, which its list names`);
  if (!existsSync(mine)) fail(`the update took ${mine}`);
  if (!existsSync(mineInUi)) fail(`the update took ${mineInUi}, which no list names`);
  if (readFileSync(list).toString("utf16le").includes("stale-from-an-earlier-build")) fail(`${list} still lists the earlier build's file`);
  rmSync(mineInUi, { force: true });
  const info = await until("the updated app to start (its ready file)", () => {
    try {
      return JSON.parse(readFileSync(ready, "utf8"));
    } catch {
      return null;
    }
  }, 120_000).catch((e) => fail(`${e.message}${log()}`));
  const cmd = commandLine(info.pid);
  console.log(`started again: pid ${info.pid}: ${cmd}`);
  stop(info.pid, true);
  await until("the updated app to quit", () => !pidAlive(info.pid), 30_000).catch(() => {});
  if (!cmd.includes("--updated")) fail(`the installer started the app without --updated: ${cmd}`);
  rmSync(mine, { force: true });
  rmSync(scratch, { recursive: true, force: true, maxRetries: 10, retryDelay: 500 });
}

// what a silent uninstall keeps and takes, as 0.3.4's did: the data folder stays (snapshots,
// settings, logs), the backend's cache and the downloaded updates go
mkdirSync(data, { recursive: true });
const marker = join(data, "nsis-e2e-keep.txt");
writeFileSync(marker, "kept\n");
const caches = [join(data, "Cache"), join(data, "updates")];
for (const c of caches) {
  mkdirSync(c, { recursive: true });
  writeFileSync(join(c, "nsis-e2e-gone.txt"), "removed by the uninstaller\n");
}
run(uninstaller, ["/S", "/currentuser"], "uninstalling");
// the one started has copied itself away and exited: the copy does the work
await untilUninstalled(dir).catch((e) => fail(`${e.message}${log()}`));
for (const f of [exe, join(dir, "spaceknife-cli.exe"), join(dir, MANIFEST)]) if (existsSync(f)) fail(`the uninstaller left ${f}`);
if (entry()) fail("the uninstall entry is left");
if (location()) fail(`HKCU\\Software\\${GUID} is left`);
for (const lnk of [startMenu, desktop]) if (existsSync(lnk)) fail(`${lnk} is left`);
if (!existsSync(marker)) fail(`a silent uninstall took ${data}`);
for (const c of caches) if (existsSync(c)) fail(`a silent uninstall left ${c}: ${readdirSync(c).join(", ") || "(empty)"}`);
rmSync(marker, { force: true });
console.log(`after uninstalling, ${dir} ${existsSync(dir) ? `holds: ${readdirSync(dir).join(", ") || "(nothing)"}` : "is gone"}; the data folder stays, its cache and updates are gone`);
console.log("installer: ok");
