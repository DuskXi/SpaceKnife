// The portable archive of a release (after `pnpm bundle`): one folder with the program, the console
// binary (on Windows also SpaceKnife.com, its console launcher), the built interface for headless
// mode (`ui`, spaceknife-cli's default --ui), the licences and an empty "data" folder (what makes it portable: lib.rs prepare_environment,
// src-tauri/src/portable.rs), packed as the update checks expect it (crates/sk-core update
// asset_templates, src-tauri/src/update.rs) and as install.rs unpacks it (one folder at the top):
//
//   target/portable/SpaceKnife-<v>-win-<x64|arm64>-portable.zip       (Windows; bsdtar's zip)
//   target/portable/SpaceKnife-<v>-linux-<x64|arm64>-tauri.tar.gz     (Linux: 0.3.4's name,
//       -linux-<arch>-portable.tar.gz, is the Electron shell's portable copy, which 0.3.4's copies
//       update to, apps/electron-shell/scripts/portable.mjs; this one is never offered to them)
//   target/portable/SpaceKnife-<v>-mac-<x64|arm64>-portable.zip       (macOS: SpaceKnife.app, which
//       holds spaceknife-cli, next to data; ditto's zip, which keeps the bundle's links, attributes
//       and signature, as the app's own ditto unpacks it)
//
// The layout (the same on Linux, for this app's own 「无缝更新」) is also a contract with every 0.3.x
// portable copy on Windows and macOS, whose 「无缝更新」 unpacks the newest
// release's archive and checks it before anything moves (v0.3.4 apps/electron/src/main index.ts
// replacePortable, update.ts swapHelper): one folder at the top holding the program under the name
// 0.3.x runs (SpaceKnife.exe on Windows, spaceknife on Linux, SpaceKnife.app on macOS, whose
// executable is Contents/MacOS/SpaceKnife), and the program that swaps the files at
// resources/backend/spaceknife-backend[.exe] (Contents/Resources/backend/spaceknife-backend in the
// app bundle on macOS). That is a copy of spaceknife-cli: it takes `--apply-update <plan>` (plan
// version 1, sk_core::update::swap) as its first argument. Every later archive keeps both.
//
//   node scripts/portable.mjs      (CARGO_TARGET_DIR respected)

import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const desktop = resolve(here, "..");
const root = resolve(desktop, "../..");
const version = JSON.parse(readFileSync(join(desktop, "src-tauri", "tauri.conf.json"), "utf8")).version;
const arch = process.arch === "arm64" ? "arm64" : "x64";
const os = { win32: "win", linux: "linux", darwin: "mac" }[process.platform];
if (!os) throw new Error(`no portable archive for ${process.platform}`);
const exe = process.platform === "win32" ? ".exe" : "";
const targetDir = process.env.CARGO_TARGET_DIR ? resolve(root, process.env.CARGO_TARGET_DIR) : join(root, "target");
const release = join(targetDir, "release");

/** The program's name in the folder: what a 0.3.x copy starts again after its update. */
const PROGRAM = { win: "SpaceKnife.exe", linux: "spaceknife", mac: "SpaceKnife.app" };
/** The swap helper, relative to the folder (on macOS inside the app bundle, tauri.bundle.macos.json). */
const HELPER = {
  win: join("resources", "backend", "spaceknife-backend.exe"),
  linux: join("resources", "backend", "spaceknife-backend"),
  mac: join("SpaceKnife.app", "Contents", "Resources", "backend", "spaceknife-backend"),
};

const name = `SpaceKnife-${version}-${os}-${arch}-${os === "linux" ? "tauri" : "portable"}`;
const work = join(root, "target", "portable");
const folder = join(work, name);
rmSync(folder, { recursive: true, force: true });
mkdirSync(join(folder, "data"), { recursive: true });
if (os === "mac") {
  // the app bundle `pnpm bundle` made (spaceknife-cli, the licences and the swap helper are inside it)
  const app = join(release, "bundle", "macos", "SpaceKnife.app");
  if (!existsSync(app)) throw new Error(`${app} is missing: run pnpm bundle first`);
  execFileSync("/usr/bin/ditto", [app, join(folder, "SpaceKnife.app")], { stdio: "inherit" });
  for (const inside of [join("SpaceKnife.app", "Contents", "MacOS", "SpaceKnife"), HELPER.mac]) {
    if (!existsSync(join(folder, inside))) throw new Error(`the app bundle has no ${inside} (tauri.macos.conf.json mainBinaryName, tauri.bundle.macos.json)`);
  }
} else {
  // the cargo binary under the program's name (Windows: SpaceKnife.exe, as 0.3.x checks it, case
  // and all; tauri.windows.conf.json's mainBinaryName names the build so too, the same file there)
  copyFileSync(join(release, `spaceknife${exe}`), join(folder, PROGRAM[os]));
  copyFileSync(join(release, `spaceknife-cli${exe}`), join(folder, `spaceknife-cli${exe}`));
  // Windows: `SpaceKnife --headless …` in a terminal (prepare-bundle.mjs builds it)
  if (os === "win") copyFileSync(join(desktop, "src-tauri", "target-bundle", "SpaceKnife.com"), join(folder, "SpaceKnife.com"));
  mkdirSync(dirname(join(folder, HELPER[os])), { recursive: true });
  copyFileSync(join(release, `spaceknife-cli${exe}`), join(folder, HELPER[os]));
  if (os === "linux") {
    for (const p of [PROGRAM.linux, "spaceknife-cli", HELPER.linux]) chmodSync(join(folder, p), 0o755);
  }
  cpSync(join(desktop, "src-tauri", "target-bundle", "licenses"), join(folder, "licenses"), { recursive: true });
  // `spaceknife-cli headless` serves the interface from `ui` next to it
  cpSync(join(desktop, ".output", "public"), join(folder, "ui"), { recursive: true, dereference: true });
}
writeFileSync(
  join(folder, "data", "README.txt"),
  "SpaceKnife keeps everything it writes in this folder (settings, snapshots, logs).\r\n" +
    "SpaceKnife 把它写的所有东西（设置、快照、日志）都放在这个文件夹里。\r\n",
);

const archive = join(work, os === "linux" ? `${name}.tar.gz` : `${name}.zip`);
rmSync(archive, { force: true });
if (os === "win") {
  // Windows' own bsdtar (System32), not a GNU tar earlier on the PATH (Git's), which takes C: for a host
  const bsdtar = join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe");
  execFileSync(bsdtar, ["-a", "-c", "-f", archive, "-C", work, name], { stdio: "inherit" });
} else if (os === "mac") {
  execFileSync("/usr/bin/ditto", ["-c", "-k", "--sequesterRsrc", "--keepParent", folder, archive], { stdio: "inherit" });
} else {
  // owned by root, by number: no user name of the build machine in the archive (tools/package_checks.mjs)
  execFileSync("tar", ["--owner=0", "--group=0", "--numeric-owner", "-czf", archive, "-C", work, name], { stdio: "inherit" });
}
console.log(`[portable] ${archive}`);
