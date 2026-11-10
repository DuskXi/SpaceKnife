// The AppImage's update information and its .zsync, as every release since 0.3.x publishes them
// (was v0.3.4 tools/build_package.py embed_update_info / make_zsync). The Electron shell's build
// runs it on the AppImage electron-builder makes (Linux):
//
//   node tools/appimage.mjs finish SpaceKnife-<v>-x86_64.AppImage [--no-zsync]
//   node tools/appimage.mjs show SpaceKnife-<v>-x86_64.AppImage
//
// AppImageUpdate and similar tools read the update information from the runtime's ELF section
// .upd_info (AppImage specification): the newest release or pre-release of this repository
// (`latest` would skip pre-releases) and the .zsync published next to its AppImage. `finish`
// writes it into the section (which the runtime carries empty) and then makes
// SpaceKnife-<v>-x86_64.AppImage.zsync next to the file with zsyncmake (package zsync); the
// .zsync names the AppImage by its file name, which those tools look for next to it.
// SpaceKnife's own update check needs neither (it reads SHA256SUMS.txt).

import { execFileSync } from "node:child_process";
import { closeSync, existsSync, openSync, readSync, writeSync } from "node:fs";
import { basename, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const UPDATE_INFO = "gh-releases-zsync|DuskXi|SpaceKnife|latest-all|SpaceKnife-*-x86_64.AppImage.zsync";

export class AppImageError extends Error {}

function readAt(fd, at, length) {
  const buf = Buffer.alloc(length);
  const n = readSync(fd, buf, 0, length, at);
  if (n !== length) throw new AppImageError(`short read at ${at}`);
  return buf;
}

/** {at, size} of a section of a little-endian ELF64 file (an open fd), or null. */
export function elfSection(fd, name) {
  const head = readAt(fd, 0, 64);
  if (head.readUInt32BE(0) !== 0x7f454c46 || head[4] !== 2 || head[5] !== 1) throw new AppImageError("not a little-endian ELF64 file");
  const shoff = Number(head.readBigUInt64LE(0x28));
  const entsize = head.readUInt16LE(0x3a);
  const count = head.readUInt16LE(0x3c);
  const names = head.readUInt16LE(0x3e);
  if (entsize < 64 || !count || names >= count) throw new AppImageError("no section headers");
  const table = readAt(fd, shoff, entsize * count);
  const header = (i) => ({
    name: table.readUInt32LE(i * entsize),
    at: Number(table.readBigUInt64LE(i * entsize + 24)),
    size: Number(table.readBigUInt64LE(i * entsize + 32)),
  });
  const strtab = header(names);
  const strings = readAt(fd, strtab.at, strtab.size);
  for (let i = 0; i < count; i++) {
    const h = header(i);
    const end = strings.indexOf(0, h.name);
    if (strings.toString("latin1", h.name, end < 0 ? strings.length : end) === name) return { at: h.at, size: h.size };
  }
  return null;
}

/** The update information the file carries ("" when the section is empty). */
export function readUpdateInfo(path) {
  const fd = openSync(path, "r");
  try {
    const found = elfSection(fd, ".upd_info");
    if (!found) throw new AppImageError(`${basename(path)}: its runtime has no .upd_info section`);
    const raw = readAt(fd, found.at, found.size);
    const nul = raw.indexOf(0);
    return raw.toString("utf8", 0, nul < 0 ? raw.length : nul);
  } finally {
    closeSync(fd);
  }
}

/** Write `info` into the runtime's .upd_info section (padded with NULs), and check it stuck. */
export function embedUpdateInfo(path, info = UPDATE_INFO) {
  const fd = openSync(path, "r+");
  try {
    const found = elfSection(fd, ".upd_info");
    if (!found) throw new AppImageError(`${basename(path)}: its runtime has no .upd_info section`);
    const raw = Buffer.from(info, "utf8");
    if (raw.length >= found.size) throw new AppImageError(`update information longer than .upd_info (${raw.length} >= ${found.size} bytes)`);
    const padded = Buffer.alloc(found.size);
    raw.copy(padded);
    writeSync(fd, padded, 0, padded.length, found.at);
  } finally {
    closeSync(fd);
  }
  if (readUpdateInfo(path) !== info) throw new AppImageError(`${basename(path)}: the update information did not stick`);
}

/** SpaceKnife-<v>-x86_64.AppImage.zsync next to the AppImage (zsyncmake, in its folder). */
export function makeZsync(path) {
  const name = basename(path);
  execFileSync("zsyncmake", ["-u", name, "-o", `${name}.zsync`, name], { cwd: dirname(path), stdio: "inherit" });
  return resolve(dirname(path), `${name}.zsync`);
}

function main(argv) {
  const [cmd, file] = argv;
  if (!["finish", "show"].includes(cmd) || !file || !existsSync(file)) {
    console.error("usage: node tools/appimage.mjs finish|show FILE.AppImage [--no-zsync]");
    return 2;
  }
  try {
    if (cmd === "show") {
      console.log(readUpdateInfo(file) || "(no update information)");
      return 0;
    }
    if (!/^SpaceKnife-[0-9A-Za-z.+~_-]+-x86_64\.AppImage$/.test(basename(file))) throw new AppImageError(`${basename(file)} is not named as the release names its AppImage`);
    embedUpdateInfo(file);
    console.log(`[appimage] ${basename(file)}: ${UPDATE_INFO}`);
    if (!argv.includes("--no-zsync")) console.log(`[appimage] ${makeZsync(resolve(file))}`);
  } catch (e) {
    if (!(e instanceof AppImageError)) throw e;
    console.error(`appimage: ${e.message}`);
    return 1;
  }
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = main(process.argv.slice(2));
