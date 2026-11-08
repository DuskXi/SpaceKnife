// The Windows installer end to end, on a CI runner only: install silently for this user, run the
// installed app (smoke-app.mjs --kind nsis: it must take itself for an installer copy, whose
// updates are the next setup.exe), uninstall silently and check the program is gone. Was the nsis
// mode of packaging/update_e2e.py (Electron's NSIS installer).
//
//   node packaging/nsis-e2e.mjs target/release/bundle/nsis/SpaceKnife_<v>_x64-setup.exe [--previous <older setup.exe>]
//
// --previous installs that one first and upgrades it with this one, as an update does
// (install.rs nsis_update_args: /S /UPDATE). It installs into this user's profile, so it refuses to
// run outside GitHub Actions. Exit status 1 on failure.

import { spawnSync } from "node:child_process";
import { existsSync, readdirSync, statSync } from "node:fs";
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
if (!setup || !existsSync(setup) || !statSync(setup).isFile()) fail("usage: node packaging/nsis-e2e.mjs <setup.exe> [--previous <older setup.exe>]");

// Tauri's per-user NSIS installer: $LOCALAPPDATA\<productName>
const dir = join(process.env.LOCALAPPDATA, "SpaceKnife");
const exe = join(dir, "spaceknife.exe");
const here = fileURLToPath(new URL(".", import.meta.url));

function run(file, argv, what) {
  console.log(`> ${file} ${argv.join(" ")}`);
  const r = spawnSync(file, argv, { stdio: "inherit", timeout: 300_000 });
  if (r.status !== 0) fail(`${what}: exit code ${r.status} ${r.error ?? ""}`);
}

async function installed(what) {
  await until(`${what}: ${exe}`, () => existsSync(exe) && existsSync(join(dir, "uninstall.exe")), 60_000);
  for (const f of ["spaceknife-cli.exe", "licenses", "ui"]) if (!existsSync(join(dir, f))) fail(`${what}: no ${f} next to the program`);
  console.log(`${what}: ${readdirSync(dir).join(", ")}`);
}

if (previous) {
  run(previous, ["/S"], "installing the previous version");
  await installed("previous version");
  run(setup, ["/S", "/UPDATE"], "upgrading it");
} else {
  run(setup, ["/S"], "installing");
}
await installed("installed");

const smoke = spawnSync(process.execPath, [join(here, "smoke-app.mjs"), "--kind", "nsis", exe], { stdio: "inherit" });
if (smoke.status !== 0) fail("the installed app's smoke test failed");

run(join(dir, "uninstall.exe"), ["/S"], "uninstalling");
// the uninstaller copies itself away and goes on in the background
await until("the program to be removed", () => !existsSync(exe) && !existsSync(join(dir, "spaceknife-cli.exe")), 120_000).catch((e) => fail(e.message));
await sleep(2000);
console.log(`after uninstalling, ${dir} ${existsSync(dir) ? `holds: ${readdirSync(dir).join(", ") || "(nothing)"}` : "is gone"}`);
console.log("installer: ok");
