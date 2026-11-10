// `pnpm package:portable` (Linux, after `pnpm run package`, or `node scripts/package.mjs --linux dir
// --arm64` on an ARM machine): the Electron shell's portable copy for Linux, under 0.3.4's name, so
// that every 0.3.4 portable copy on Linux updates to it — RHEL 9 and its rebuilds have no WebKitGTK
// 4.1 for the Tauri app, whose own portable copy is SpaceKnife-<v>-linux-<arch>-tauri.tar.gz
// (apps/desktop/scripts/portable.mjs):
//
//   target/electron-shell/portable/SpaceKnife-<v>-linux-<x64|arm64>-portable.tar.gz
//
// One folder at the top: electron-builder's unpacked app (dist/linux-unpacked, or
// linux-arm64-unpacked: `spaceknife`, the Electron program; `spaceknife-cli` next to it;
// `resources/ui`, `resources/licenses`), the swap helper 0.3.x copies run from the new version
// (`resources/backend/spaceknife-backend`, a copy of spaceknife-cli: `--apply-update <plan v1>`,
// sk_core::update::swap) and an empty "data" folder, which makes it portable (src/main/portable.ts).
// That is what 0.3.4's 「无缝更新」 checks before anything moves (v0.3.4 index.ts replacePortable,
// update.ts swapHelper): the program under the exact name it runs, the helper, executable. The
// archive's members are owned by root, by number (no user name of the build machine in it,
// tools/package_checks.mjs).
//
//   node scripts/portable.mjs [--arch x64|arm64]   (default: this machine's)

import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const shell = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(shell, "../..");

/** The program, the console binary and the swap helper, relative to the folder. */
export const PROGRAM = "spaceknife";
export const CLI = "spaceknife-cli";
export const HELPER = join("resources", "backend", "spaceknife-backend");
/** What the unpacked app must hold (electron-builder.yml: executableName, extraFiles, extraResources). */
const NEEDED = [PROGRAM, CLI, join("resources", "app.asar"), join("resources", "ui", "index.html"), join("resources", "licenses")];

export const NOTE =
  "SpaceKnife keeps everything it writes in this folder (settings, snapshots, logs).\n" +
  "SpaceKnife 把它写的所有东西（设置、快照、日志）都放在这个文件夹里。\n";

/** The archive's top folder (and file name without .tar.gz) for a version and architecture. */
export const portableName = (version, arch) => `SpaceKnife-${version}-linux-${arch}-portable`;

/** electron-builder's unpacked app for an architecture (an x64 build's folder doesn't name it). */
export const unpackedDir = (dist, arch) => join(dist, arch === "x64" ? "linux-unpacked" : `linux-${arch}-unpacked`);

/** Lay out the portable folder `folder` from the unpacked app (which stays as it is). */
export function layout(unpacked, folder) {
  for (const need of NEEDED) {
    if (!existsSync(join(unpacked, need))) throw new Error(`${join(unpacked, need)} is missing: run the package step first (electron-builder.yml)`);
  }
  rmSync(folder, { recursive: true, force: true });
  mkdirSync(dirname(folder), { recursive: true });
  // links and modes as electron-builder made them (chrome-sandbox, the program)
  cpSync(unpacked, folder, { recursive: true, verbatimSymlinks: true, preserveTimestamps: true });
  mkdirSync(dirname(join(folder, HELPER)), { recursive: true });
  copyFileSync(join(folder, CLI), join(folder, HELPER));
  for (const p of [PROGRAM, CLI, HELPER]) chmodSync(join(folder, p), 0o755);
  mkdirSync(join(folder, "data"), { recursive: true });
  writeFileSync(join(folder, "data", "README.txt"), NOTE);
  return folder;
}

/** Pack `name` (a folder in `work`) as a tar.gz owned by root, by number. */
export function pack(work, name, archive) {
  rmSync(archive, { force: true });
  execFileSync("tar", ["--owner=0", "--group=0", "--numeric-owner", "-czf", archive, "-C", work, name], { stdio: "inherit" });
  return archive;
}

function main(argv) {
  if (process.platform !== "linux") throw new Error("the Electron shell's portable copy is built on Linux only");
  const at = argv.indexOf("--arch");
  const arch = at >= 0 ? argv[at + 1] : process.arch === "arm64" ? "arm64" : "x64";
  if (!["x64", "arm64"].includes(arch)) throw new Error(`no portable copy for ${arch}`);
  const version = JSON.parse(readFileSync(join(shell, "package.json"), "utf8")).version;
  const dist = join(root, "target", "electron-shell", "dist");
  const work = join(root, "target", "electron-shell", "portable");
  const name = portableName(version, arch);
  layout(unpackedDir(dist, arch), join(work, name));
  const archive = pack(work, name, join(work, `${name}.tar.gz`));
  console.log(`[portable] ${archive} (${Math.round(statSync(archive).size / 2 ** 20)} MB)`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2));
