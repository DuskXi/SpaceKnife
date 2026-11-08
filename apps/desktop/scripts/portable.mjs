// The portable archive of a release (after `pnpm bundle`): one folder with the program, the console
// binary, the built interface for headless mode (`ui`, spaceknife-cli's default --ui), the licences
// and an empty "data" folder (what makes it portable: lib.rs prepare_environment,
// src-tauri/src/portable.rs), packed as the update checks expect it (crates/sk-core update
// asset_template, src-tauri/src/update.rs) and as install.rs unpacks it (one folder at the top):
//
//   target/portable/SpaceKnife-<v>-win-<x64|arm64>-portable.zip       (Windows; bsdtar's zip)
//   target/portable/SpaceKnife-<v>-linux-<x64|arm64>-portable.tar.gz  (Linux)
//   target/portable/SpaceKnife-<v>-mac-<x64|arm64>-portable.zip       (macOS: SpaceKnife.app, which
//       holds spaceknife-cli, next to data; ditto's zip, which keeps the bundle's links, attributes
//       and signature, as the app's own ditto unpacks it)
//
//   node scripts/portable.mjs      (CARGO_TARGET_DIR respected)

import { execFileSync } from "node:child_process";
import { copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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

const name = `SpaceKnife-${version}-${os}-${arch}-portable`;
const work = join(root, "target", "portable");
const folder = join(work, name);
rmSync(folder, { recursive: true, force: true });
mkdirSync(join(folder, "data"), { recursive: true });
if (os === "mac") {
  // the app bundle `pnpm bundle` made (spaceknife-cli and the licences are inside it)
  const app = join(release, "bundle", "macos", "SpaceKnife.app");
  if (!existsSync(app)) throw new Error(`${app} is missing: run pnpm bundle first`);
  execFileSync("/usr/bin/ditto", [app, join(folder, "SpaceKnife.app")], { stdio: "inherit" });
} else {
  copyFileSync(join(release, `spaceknife${exe}`), join(folder, `spaceknife${exe}`));
  copyFileSync(join(release, `spaceknife-cli${exe}`), join(folder, `spaceknife-cli${exe}`));
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
