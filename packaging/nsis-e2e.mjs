// The Windows installer end to end, on a CI runner only: install silently for this user, run the
// installed app (smoke-app.mjs --kind nsis: it must take itself for an installer copy, whose updates
// are the next setup.exe), update it in place the way the app does (install.rs nsis_update_args,
// 0.3.4's arguments: --updated /S --force-run /currentuser), uninstall silently and check the
// program is gone and the data stays. The installer is Tauri's with the forked NSIS template
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
// the interface gone, a file the user put there kept, no staging folder left, the app started
// again with --updated and the installer's environment. It installs into this user's profile, so
// it refuses to run outside GitHub Actions. Exit status 1 on failure.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { WIN, fail, pidAlive, sleep, stop, until } from "./lib.mjs";

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
const data = join(process.env.LOCALAPPDATA, "SpaceKnife"); // the backend's data (paths.rs), not the program's folder
const startMenu = join(process.env.APPDATA, "Microsoft", "Windows", "Start Menu", "Programs", "SpaceKnife.lnk");
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
  const stale = join(dir, "ui", "stale-from-an-earlier-build.js");
  const mine = join(dir, "keep-me.txt");
  writeFileSync(stale, "//\n");
  writeFileSync(mine, "the user's\n");
  const scratch = mkdtempSync(join(tmpdir(), "sk-nsis-e2e-"));
  const ready = join(scratch, "ready.json");
  const env = { ...process.env, SPACEKNIFE_READY_FILE: ready, SPACEKNIFE_START_MINIMIZED: "1", SPACEKNIFE_HOME: join(scratch, "home") };
  run(setup, UPDATE_ARGS, "updating in place", env);
  await installed("updated in place");
  if (existsSync(stale)) fail(`the update left ${stale}`);
  if (!existsSync(mine)) fail(`the update took ${mine}`);
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

// what the user keeps: a marker in the data folder survives a silent uninstall
mkdirSync(data, { recursive: true });
const marker = join(data, "nsis-e2e-keep.txt");
writeFileSync(marker, "kept\n");
run(uninstaller, ["/S", "/currentuser"], "uninstalling");
// the uninstaller copies itself away and goes on in the background
await until("the program to be removed", () => !existsSync(exe) && !existsSync(join(dir, "spaceknife-cli.exe")), 120_000).catch((e) => fail(e.message));
await sleep(2000);
if (entry()) fail("the uninstall entry is left");
if (location()) fail(`HKCU\\Software\\${GUID} is left`);
if (existsSync(startMenu)) fail(`${startMenu} is left`);
if (!existsSync(marker)) fail(`a silent uninstall took ${data}`);
console.log(`after uninstalling, ${dir} ${existsSync(dir) ? `holds: ${readdirSync(dir).join(", ") || "(nothing)"}` : "is gone"}; the data folder stays`);
console.log("installer: ok");
