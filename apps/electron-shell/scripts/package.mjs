// `pnpm package`: the Electron shell's packages (electron-builder.yml): on Windows the installer,
// on Linux the rpm (RHEL 9) and the AppImage, in target/electron-shell/dist. Steps:
//
// 1. spaceknife-cli and the licences: apps/desktop/scripts/prepare-bundle.mjs (a release build with
//    the build machine's folders remapped, as for the Tauri bundles), copied to
//    target/electron-shell/stage; on Windows also SpaceKnife.com (packaging/console-launcher).
// 2. the interface: apps/desktop's Nuxt build (`pnpm generate` there when it is missing).
// 3. main and preload (electron-vite build), then electron-builder for this platform.
//
//   node scripts/package.mjs [electron-builder options …]   (e.g. --linux rpm, --win nsis)
//   SPACEKNIFE_PACKAGE_REUSE=1: take spaceknife-cli, SpaceKnife.com and the licences as already staged

import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readdirSync, rmSync, cpSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const shell = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(shell, "../..");
const desktop = join(root, "apps", "desktop");
const stage = join(root, "target", "electron-shell", "stage");
const win = process.platform === "win32";
const exe = win ? ".exe" : "";

const run = (cmd, args, opts = {}) => {
  console.log(`[package] ${cmd} ${args.join(" ")}`);
  // pnpm and electron-builder are .cmd files on Windows: through cmd.exe, with fixed arguments
  if (win && opts.cmdShim) execFileSync("cmd.exe", ["/d", "/s", "/c", [cmd, ...args].join(" ")], { stdio: "inherit", cwd: opts.cwd ?? shell, env: opts.env ?? process.env });
  else execFileSync(cmd, args, { stdio: "inherit", cwd: opts.cwd ?? shell, env: opts.env ?? process.env });
};

// ------------------------------------------------------------------ 1. the programs and licences

if (process.env.SPACEKNIFE_PACKAGE_REUSE !== "1") {
  rmSync(stage, { recursive: true, force: true });
  mkdirSync(stage, { recursive: true });
  run(process.execPath, [join(desktop, "scripts", "prepare-bundle.mjs")], { cwd: desktop });
  const bundle = join(desktop, "src-tauri", "target-bundle");
  const cli = readdirSync(bundle).find((n) => n.startsWith("spaceknife-cli-") && (!win || n.endsWith(".exe")));
  if (!cli) throw new Error(`no spaceknife-cli in ${bundle}`);
  copyFileSync(join(bundle, cli), join(stage, `spaceknife-cli${exe}`));
  cpSync(join(bundle, "licenses"), join(stage, "licenses"), { recursive: true });
  // prepare-bundle.mjs builds SpaceKnife.com on Windows too (packaging/console-launcher, its own
  // small workspace, with the same remapped build paths)
  if (win) copyFileSync(join(bundle, "SpaceKnife.com"), join(stage, "SpaceKnife.com"));
}
for (const need of [`spaceknife-cli${exe}`, "licenses", ...(win ? ["SpaceKnife.com"] : [])]) {
  if (!existsSync(join(stage, need))) throw new Error(`${join(stage, need)} is missing`);
}

// ------------------------------------------------------------------ 2. the interface

if (!existsSync(join(desktop, ".output", "public", "index.html"))) run("pnpm", ["generate"], { cwd: desktop, cmdShim: true });

// ------------------------------------------------------------------ 3. main, preload, packages

run("pnpm", ["exec", "electron-vite", "build"], { cmdShim: true });
run("pnpm", ["exec", "electron-builder", "--publish", "never", ...process.argv.slice(2)], { cmdShim: true });
console.log(`[package] ${join(root, "target", "electron-shell", "dist")}`);
