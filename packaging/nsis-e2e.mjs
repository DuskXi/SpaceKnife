// The Windows installer end to end, on a CI runner only: install silently for this user, run the
// installed app (smoke-app.mjs --kind nsis: it must take itself for an installer copy, whose updates
// are the next setup.exe), uninstall silently and check the program is gone and the data stays.
// The installer is the Electron shell's (electron-builder's NSIS; electron.yml), laid out as 0.3.4's
// was: %LOCALAPPDATA%\Programs\SpaceKnife\SpaceKnife.exe with spaceknife-cli.exe next to it,
// "Uninstall SpaceKnife.exe", the uninstall entry under electron-builder's GUID for the appId. The
// update from the real 0.3.4 installer is packaging/update-from-034.mjs nsis.
//
//   node packaging/nsis-e2e.mjs target/electron-shell/dist/SpaceKnife-<v>-Setup-x64.exe [--previous <older Setup-x64.exe>]
//
// --previous installs that one first and upgrades it with this one, as an update does (0.3.x's
// update.ts nsisUpdateArgs: --updated /S --force-run /currentuser). It installs into this user's
// profile, so it refuses to run outside GitHub Actions. Exit status 1 on failure.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { WIN, fail, sleep, until } from "./lib.mjs";

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
const dir = join(process.env.LOCALAPPDATA, "Programs", "SpaceKnife");
const exe = join(dir, "SpaceKnife.exe");
const uninstaller = join(dir, "Uninstall SpaceKnife.exe");
const data = join(process.env.LOCALAPPDATA, "SpaceKnife"); // the backend's data (paths.rs), not the program's folder
const here = fileURLToPath(new URL(".", import.meta.url));

function run(file, argv, what) {
  console.log(`> ${file} ${argv.join(" ")}`);
  const r = spawnSync(file, argv, { stdio: "inherit", timeout: 300_000 });
  if (r.status !== 0) fail(`${what}: exit code ${r.status} ${r.error ?? ""}`);
}

function entry() {
  const r = spawnSync("reg.exe", ["query", `HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${GUID}`, "/v", "DisplayVersion"], { encoding: "utf8" });
  return r.status === 0 ? /DisplayVersion\s+REG_SZ\s+(\S+)/.exec(r.stdout)?.[1] ?? "?" : null;
}

async function installed(what) {
  await until(`${what}: ${exe}`, () => existsSync(exe) && existsSync(uninstaller), 60_000);
  if (!existsSync(join(dir, "spaceknife-cli.exe"))) fail(`${what}: no spaceknife-cli.exe next to the program`);
  console.log(`${what}: ${readdirSync(dir).join(", ")} (uninstall entry ${entry()})`);
}

if (previous) {
  run(previous, ["/S", "/currentuser"], "installing the previous version");
  await installed("previous version");
  run(setup, ["--updated", "/S", "--force-run", "/currentuser"], "upgrading it");
} else {
  run(setup, ["/S", "/currentuser"], "installing");
}
await installed("installed");
if (!entry()) fail(`no uninstall entry HKCU\\…\\Uninstall\\${GUID}`);

const smoke = spawnSync(process.execPath, [join(here, "smoke-app.mjs"), "--kind", "nsis", exe], { stdio: "inherit" });
if (smoke.status !== 0) fail("the installed app's smoke test failed");

// what the user keeps: a marker in the data folder survives a silent uninstall
mkdirSync(data, { recursive: true });
const marker = join(data, "nsis-e2e-keep.txt");
writeFileSync(marker, "kept\n");
run(uninstaller, ["/S", "/currentuser"], "uninstalling");
// the uninstaller copies itself away and goes on in the background
await until("the program to be removed", () => !existsSync(exe) && !existsSync(join(dir, "spaceknife-cli.exe")), 120_000).catch((e) => fail(e.message));
await sleep(2000);
if (entry()) fail("the uninstall entry is left");
if (!existsSync(marker)) fail(`a silent uninstall took ${data}`);
console.log(`after uninstalling, ${dir} ${existsSync(dir) ? `holds: ${readdirSync(dir).join(", ") || "(nothing)"}` : "is gone"}; the data folder stays`);
console.log("installer: ok");
