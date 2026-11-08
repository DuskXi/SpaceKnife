// Checks of the files a release ships, and the Rust flags their programs are built with
// (docs/packaging.md §1 「不带构建机路径」「glibc 下限」). The Portable build and macOS build workflows
// run the checks on every package they make; it runs here too.
//
//   node tools/package_checks.mjs paths [--home DIR …] [--user NAME] TARGET …
//       No file in TARGET (folders, packages, archives) names the build machine's home folder (by
//       default this user's): as UTF-8 and as UTF-16LE (Windows resources), with either slash,
//       JSON / JS escaped, ASCII letters in any case, not followed by another name character
//       (/home/lenny does not match /home/lennyx). Zip, tar, gzip, zstd and ar (deb) files are
//       looked into, members of members too; a tar member owned by NAME (by default this user) is
//       a finding as well.
//   node tools/package_checks.mjs glibc [--max 2.34] TARGET …
//       No ELF file in TARGET (the same members) may need a newer glibc than --max (the RHEL 9
//       floor, docs/PLAN.md). Conservative, as tools/build_package.py was: any GLIBC_x.y string in
//       the file counts.
//   node tools/package_checks.mjs crt [--allow-ucrt NAME …] TARGET …
//       No Windows program or library in TARGET (PE files, the same members) imports the Visual C++
//       runtime (VCRUNTIME*.dll, MSVCP*.dll, the UCRT's api-ms-win-crt-*.dll and ucrtbase.dll, older
//       or extra runtime DLLs: CRT_DLL), directly or delay-loaded: neither the installer nor the
//       portable copies ship the Visual C++ Redistributable, so every program has the C runtime
//       linked in (+crt-static). A file named by --allow-ucrt may take the Universal CRT from the
//       system (Windows 10 and later have it): for others' prebuilt files (Electron's dxil.dll).
//
// remappedEnv() is the environment of a release build (apps/desktop/scripts/bundle.mjs, which
// `pnpm bundle` runs): rustc's --remap-path-prefix writes the home folder, CARGO_HOME, RUSTUP_HOME
// and the repository as ~, /cargo, /rustup and /spaceknife in panic messages and debug paths
// (Cargo's trim-paths is not stable), and on Windows the program keeps only its PDB's file name.
// The `paths` check over the packages is what tells that it worked. On Windows it also links the C
// runtime statically (STATIC_CRT, as .cargo/config.toml asks, which Cargo skips once
// CARGO_ENCODED_RUSTFLAGS is set); the `crt` check tells that this worked.

import { lstatSync, readdirSync, readFileSync } from "node:fs";
import { homedir, userInfo } from "node:os";
import { posix, relative, resolve, win32 } from "node:path";
import { fileURLToPath } from "node:url";
import zlib from "node:zlib";

export const GLIBC_FLOOR = "2.34"; // RHEL 9; Ubuntu 22.04 has 2.35
const ROOT = resolve(fileURLToPath(new URL("..", import.meta.url)));
const CHUNK = 64 << 20; // a file is searched in pieces of this many bytes (V8 strings have a limit)
const MAX_DEPTH = 6; // members of members of …

export class CheckError extends Error {}

// ------------------------------------------------------------------ the flags of a release build

/** rustc flags that keep the build machine's folders out of the binaries. rustc applies the last
 * matching prefix, so the home folder comes first: it catches what the others do not. */
export function remapFlags({ home = homedir(), cargoHome, rustupHome, root = ROOT, windows = process.platform === "win32" } = {}) {
  const join = (windows ? win32 : posix).join;
  const flags = [
    `--remap-path-prefix=${home}=~`,
    `--remap-path-prefix=${cargoHome || join(home, ".cargo")}=/cargo`,
    `--remap-path-prefix=${rustupHome || join(home, ".rustup")}=/rustup`,
    `--remap-path-prefix=${root}=/spaceknife`,
  ];
  if (windows) flags.push("-Clink-arg=/PDBALTPATH:%_PDB%"); // the PDB's file name only, not its full path
  return flags;
}

/** Windows (MSVC): the C runtime linked into the program, so it needs no Visual C++
 * Redistributable (the `crt` check). The same flag as .cargo/config.toml's. */
export const STATIC_CRT = "-Ctarget-feature=+crt-static";

/** The flags of a release build: remapFlags(), and on Windows STATIC_CRT. */
export function releaseFlags(opts = {}) {
  const windows = opts.windows ?? process.platform === "win32";
  return [...remapFlags({ ...opts, windows }), ...(windows ? [STATIC_CRT] : [])];
}

/** `env` with releaseFlags() added to the flags Cargo would use (CARGO_ENCODED_RUSTFLAGS, else
 * RUSTFLAGS); flags already there are not added twice, so a second call changes nothing (the
 * spaceknife-cli build inside `tauri build` gets the same flags, and shares its dependencies).
 * Cargo then ignores the rustflags of .cargo/config.toml: STATIC_CRT is among these flags for that. */
export function remappedEnv(env = process.env, { home = homedir(), root = ROOT, windows = process.platform === "win32" } = {}) {
  const flags = releaseFlags({ home, cargoHome: env.CARGO_HOME, rustupHome: env.RUSTUP_HOME, root, windows });
  const current =
    env.CARGO_ENCODED_RUSTFLAGS !== undefined
      ? env.CARGO_ENCODED_RUSTFLAGS.split("\x1f").filter(Boolean)
      : (env.RUSTFLAGS ?? "").split(/\s+/).filter(Boolean);
  const merged = [...current, ...flags.filter((f) => !current.includes(f))];
  return { ...env, CARGO_ENCODED_RUSTFLAGS: merged.join("\x1f") };
}

// ------------------------------------------------------------------ what is in a file

const startsWith = (data, bytes, at = 0) => data.length >= at + bytes.length && bytes.every((b, i) => data[at + i] === b);
const ZIP = [0x50, 0x4b, 0x03, 0x04];
const ZIP_EMPTY = [0x50, 0x4b, 0x05, 0x06];
const GZIP = [0x1f, 0x8b];
const ZSTD = [0x28, 0xb5, 0x2f, 0xfd];
const XZ = [0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00];
const AR = [...Buffer.from("!<arch>\n")];
const USTAR = [...Buffer.from("ustar")];
const ELF = [0x7f, 0x45, 0x4c, 0x46];
const MZ = [0x4d, 0x5a];

const cstr = (data, at, len) => {
  const raw = data.subarray(at, at + len);
  const nul = raw.indexOf(0);
  return raw.subarray(0, nul < 0 ? raw.length : nul).toString("utf8");
};

/** The members of a zip file: {name, data()} (stored or deflated; ZIP64 sizes and offsets read). */
function zipMembers(data, label) {
  let eocd = -1;
  for (let i = data.length - 22; i >= Math.max(0, data.length - 22 - 0xffff); i--) {
    if (data.readUInt32LE(i) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) throw new CheckError(`${label}: a zip without its central directory`);
  let count = data.readUInt16LE(eocd + 10);
  let cdOffset = data.readUInt32LE(eocd + 16);
  if (count === 0xffff || cdOffset === 0xffffffff) {
    const loc = eocd - 20;
    if (loc < 0 || data.readUInt32LE(loc) !== 0x07064b50) throw new CheckError(`${label}: a ZIP64 file without its locator`);
    const rec = Number(data.readBigUInt64LE(loc + 8));
    if (data.readUInt32LE(rec) !== 0x06064b50) throw new CheckError(`${label}: a broken ZIP64 directory`);
    count = Number(data.readBigUInt64LE(rec + 32));
    cdOffset = Number(data.readBigUInt64LE(rec + 48));
  }
  const out = [];
  let p = cdOffset;
  for (let k = 0; k < count; k++) {
    if (data.readUInt32LE(p) !== 0x02014b50) throw new CheckError(`${label}: a broken central directory`);
    const flags = data.readUInt16LE(p + 8);
    const method = data.readUInt16LE(p + 10);
    let csize = data.readUInt32LE(p + 20);
    let usize = data.readUInt32LE(p + 24);
    const nlen = data.readUInt16LE(p + 28);
    const xlen = data.readUInt16LE(p + 30);
    const clen = data.readUInt16LE(p + 32);
    let local = data.readUInt32LE(p + 42);
    const name = data.subarray(p + 46, p + 46 + nlen).toString("utf8");
    // ZIP64 extra field: the values that did not fit, in this order
    for (let x = p + 46 + nlen; x + 4 <= p + 46 + nlen + xlen; ) {
      const id = data.readUInt16LE(x);
      const size = data.readUInt16LE(x + 2);
      if (id === 0x0001) {
        let q = x + 4;
        if (usize === 0xffffffff) (usize = Number(data.readBigUInt64LE(q))), (q += 8);
        if (csize === 0xffffffff) (csize = Number(data.readBigUInt64LE(q))), (q += 8);
        if (local === 0xffffffff) local = Number(data.readBigUInt64LE(q));
      }
      x += 4 + size;
    }
    p += 46 + nlen + xlen + clen;
    if (name.endsWith("/")) continue;
    if (flags & 1) throw new CheckError(`${label} › ${name}: encrypted`);
    if (method !== 0 && method !== 8) throw new CheckError(`${label} › ${name}: compression method ${method} is not read here`);
    out.push({
      name,
      data: () => {
        if (data.readUInt32LE(local) !== 0x04034b50) throw new CheckError(`${label} › ${name}: no local header`);
        const start = local + 30 + data.readUInt16LE(local + 26) + data.readUInt16LE(local + 28);
        const raw = data.subarray(start, start + csize);
        const bytes = method === 0 ? raw : zlib.inflateRawSync(raw);
        if (bytes.length !== usize) throw new CheckError(`${label} › ${name}: ${bytes.length} bytes, the directory says ${usize}`);
        return bytes;
      },
    });
  }
  return out;
}

/** tar's numbers: octal, or base-256 when the first byte has its top bit set. */
function tarNumber(data, at, len) {
  if (data[at] & 0x80) {
    let n = 0;
    for (let i = at + 1; i < at + len; i++) n = n * 256 + data[i];
    return n;
  }
  const s = cstr(data, at, len).trim();
  return s ? parseInt(s, 8) : 0;
}

/** The entries of a tar archive: {name, link, data, uname, gname, file} — `file` for regular files
 * (GNU long names and links, pax paths and link paths read). */
function tarMembers(data, label) {
  const out = [];
  let long = {};
  let pax = {};
  for (let p = 0; p + 512 <= data.length; ) {
    if (data.subarray(p, p + 512).every((b) => b === 0)) break;
    const size = tarNumber(data, p + 124, 12);
    const type = String.fromCharCode(data[p + 156] || 0x30);
    const body = data.subarray(p + 512, p + 512 + size);
    if (body.length !== size) throw new CheckError(`${label}: a tar member runs past the end`);
    if (type === "L" || type === "K") long[type] = cstr(body, 0, body.length);
    else if (type === "x") {
      for (const m of body.toString("utf8").matchAll(/\d+ (path|linkpath)=([^\n]*)\n/g)) pax[m[1]] = m[2];
    } else if (type !== "g") {
      const prefix = cstr(data, p + 345, 155);
      const name = cstr(data, p, 100);
      out.push({
        name: pax.path ?? long.L ?? (prefix ? `${prefix}/${name}` : name),
        link: pax.linkpath ?? long.K ?? cstr(data, p + 157, 100),
        data: body,
        uname: cstr(data, p + 265, 32),
        gname: cstr(data, p + 297, 32),
        file: type === "0" || type === "7",
        mode: tarNumber(data, p + 100, 8),
        dir: type === "5",
      });
      long = {};
      pax = {};
    }
    p += 512 + Math.ceil(size / 512) * 512;
  }
  return out;
}

/** The members of an ar archive (a deb: debian-binary, control.tar.*, data.tar.*). */
function arMembers(data, label) {
  const out = [];
  for (let p = AR.length; p + 60 <= data.length; ) {
    const name = data.toString("latin1", p, p + 16).trim().replace(/\/$/, "");
    const size = parseInt(data.toString("latin1", p + 48, p + 58).trim(), 10);
    if (!Number.isFinite(size) || data.toString("latin1", p + 58, p + 60) !== "`\n") throw new CheckError(`${label}: a broken ar header`);
    out.push({ name, link: "", data: data.subarray(p + 60, p + 60 + size), file: true });
    p += 60 + size + (size % 2);
  }
  return out;
}

function decompress(kind, data, label) {
  try {
    if (kind === "gzip") return zlib.gunzipSync(data);
    if (typeof zlib.zstdDecompressSync !== "function") throw new CheckError(`${label}: zstd needs Node 22.15 or later`);
    return zlib.zstdDecompressSync(data);
  } catch (e) {
    if (e instanceof CheckError) throw e;
    return null; // it only starts like one
  }
}

/** What there is to search in a file: the file, and the members of the containers it is, as
 * {label, data, owner?, names?} — a compressed file as what it unpacks to; a tar or ar archive as
 * its members, their names and link targets, and each tar entry's owner names (its own bytes are
 * only those); a zip file as its bytes and its members. */
export function* contents(label, data, depth = 0) {
  if (depth > MAX_DEPTH) {
    yield { label, data };
    return;
  }
  if (startsWith(data, XZ)) throw new CheckError(`${label}: xz-compressed, which is not read here`);
  for (const [kind, magic] of [["gzip", GZIP], ["zstd", ZSTD]]) {
    if (!startsWith(data, magic)) continue;
    const inner = decompress(kind, data, label);
    if (inner) {
      yield* contents(label, inner, depth + 1);
      return;
    }
  }
  const tar = startsWith(data, USTAR, 257);
  const ar = startsWith(data, AR);
  if (tar || ar) {
    const members = tar ? tarMembers(data, label) : arMembers(data, label);
    yield { label: `${label} (member names)`, data: Buffer.from(members.flatMap((m) => [m.name, m.link]).join("\n")), names: true };
    for (const m of members) {
      // a tar entry's owner names (empty with --numeric-owner), once per entry
      if (tar) yield { label: `${label} › ${m.name}`, data: Buffer.alloc(0), owner: [m.uname, m.gname], names: true };
      if (m.file) yield* contents(`${label} › ${m.name}`, m.data, depth + 1);
    }
    return;
  }
  yield { label, data };
  if (startsWith(data, ZIP) || startsWith(data, ZIP_EMPTY)) {
    for (const m of zipMembers(data, label)) yield* contents(`${label} › ${m.name}`, m.data(), depth + 1);
  }
}

/** The entries of a portable archive (a zip, or a tar.gz): [{name, mode}] — `mode` the Unix
 * permissions where the archive keeps them (tar; a zip made on Unix), else null. */
export function archiveEntries(data, label = "archive") {
  if (startsWith(data, GZIP)) {
    const tar = zlib.gunzipSync(data);
    if (!startsWith(tar, USTAR, 257)) throw new CheckError(`${label}: a gzip file that holds no tar archive`);
    return tarMembers(tar, label).map((m) => ({ name: m.dir && !m.name.endsWith("/") ? `${m.name}/` : m.name, mode: m.mode }));
  }
  if (!startsWith(data, ZIP) && !startsWith(data, ZIP_EMPTY)) throw new CheckError(`${label}: neither a zip nor a tar.gz file`);
  // the names from the central directory (with the folders, which zipMembers skips)
  let eocd = -1;
  for (let i = data.length - 22; i >= Math.max(0, data.length - 22 - 0xffff); i--) {
    if (data.readUInt32LE(i) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) throw new CheckError(`${label}: a zip without its central directory`);
  const files = new Set(zipMembers(data, label).map((m) => m.name));
  let count = data.readUInt16LE(eocd + 10);
  let p = data.readUInt32LE(eocd + 16);
  if (count === 0xffff || p === 0xffffffff) {
    const rec = Number(data.readBigUInt64LE(eocd - 20 + 8));
    count = Number(data.readBigUInt64LE(rec + 32));
    p = Number(data.readBigUInt64LE(rec + 48));
  }
  const out = [];
  for (let k = 0; k < count; k++) {
    const madeBy = data.readUInt16LE(p + 4) >> 8; // 3: Unix
    const nlen = data.readUInt16LE(p + 28);
    const name = data.subarray(p + 46, p + 46 + nlen).toString("utf8");
    const external = data.readUInt32LE(p + 38);
    if (files.has(name) || name.endsWith("/")) out.push({ name, mode: madeBy === 3 ? (external >>> 16) & 0o7777 : null });
    p += 46 + nlen + data.readUInt16LE(p + 30) + data.readUInt16LE(p + 32);
  }
  return out;
}

/** Every file under `target` (a folder or a file), links not followed: [label, path]. */
function* files(target) {
  const st = lstatSync(target);
  if (st.isSymbolicLink()) return;
  if (st.isDirectory()) {
    for (const name of readdirSync(target).sort()) yield* files(resolve(target, name));
  } else if (st.isFile()) {
    yield [relative(process.cwd(), target).replaceAll("\\", "/") || target, target];
  }
}

/** contents() of every file under the targets; `count.files` and `count.members` say how many were read. */
function* everything(targets, count = { files: 0, members: 0 }) {
  for (const target of targets) {
    try {
      lstatSync(target);
    } catch {
      throw new CheckError(`${target}: no such file or folder`);
    }
    for (const [label, path] of files(resolve(target))) {
      count.files++;
      for (const c of contents(label, readFileSync(path))) {
        if (c.label !== label && !c.names) count.members++;
        yield c;
      }
    }
  }
}

// ------------------------------------------------------------------ searching

/** A pattern of these bytes: ASCII letters in either case, everything else as it is. */
function bytePattern(bytes) {
  let out = "";
  for (const b of bytes) {
    const c = String.fromCharCode(b);
    out += /[A-Za-z]/.test(c) ? `[${c.toUpperCase()}${c.toLowerCase()}]` : `\\x${b.toString(16).padStart(2, "0")}`;
  }
  return out;
}

/** The spellings of a home folder that a file may hold. */
export function homeSpellings(home) {
  const h = String(home).replace(/[\\/]+$/, "");
  const fwd = h.replaceAll("\\", "/");
  return [...new Set([h, fwd, h.replaceAll("\\", "\\\\"), fwd.replaceAll("/", "\\/")])];
}

/** One regular expression (over bytes read as latin1) for all spellings of the home folders, in UTF-8 and UTF-16LE. */
export function homeMatcher(homes) {
  const parts = [];
  let longest = 0;
  for (const home of homes) {
    const h = String(home).replace(/[\\/]+$/, "");
    if (h.replace(/^[A-Za-z]:/, "").length < 4) throw new CheckError(`${JSON.stringify(home)} is too short to look for`);
    for (const v of homeSpellings(h)) {
      const u8 = Buffer.from(v, "utf8");
      const u16 = Buffer.from(v, "utf16le");
      // not followed by another name character: /home/lenny must not match /home/lennyx
      parts.push(`${bytePattern(u8)}(?![A-Za-z0-9_])`, `${bytePattern(u16)}(?![A-Za-z0-9_]\\x00)`);
      longest = Math.max(longest, u16.length + 2);
    }
  }
  return { re: new RegExp(parts.join("|"), "g"), overlap: longest };
}

/** The matches of `re` in `data`, searched in pieces that overlap by `overlap` bytes. */
function* matches(data, re, overlap) {
  for (let start = 0; start < data.length; start += CHUNK) {
    const end = Math.min(data.length, start + CHUNK + overlap);
    const text = data.toString("latin1", start, end);
    for (const m of text.matchAll(re)) if (m.index < CHUNK) yield m;
    if (end === data.length) break;
  }
}

/** The findings of the `paths` check: [{label, hits}] and the counts searched. */
export function findBuildPaths(targets, { homes = [homedir()], user = userInfo().username } = {}) {
  const { re, overlap } = homeMatcher(homes);
  const problems = [];
  const count = { files: 0, members: 0 };
  for (const c of everything(targets, count)) {
    let hits = 0;
    for (const _ of matches(c.data, re, overlap)) hits++;
    if (hits) problems.push(`${c.label}: ${hits}`);
    if (user && c.owner?.some((o) => o && o.toLowerCase() === user.toLowerCase())) problems.push(`${c.label}: owned by ${user}`);
  }
  return { problems, ...count };
}

const GLIBC = /GLIBC_(\d+)\.(\d+)/g;
const version = (s) => s.split(".").map(Number);
const newer = (a, b) => a[0] > b[0] || (a[0] === b[0] && a[1] > b[1]);

/** The newest glibc an ELF file names ([major, minor]); null for other files and ELF files without any. */
export function glibcNeeded(data) {
  if (!startsWith(data, ELF)) return null;
  let top = null;
  for (const m of matches(data, GLIBC, 32)) {
    const v = [Number(m[1]), Number(m[2])];
    if (!top || newer(v, top)) top = v;
  }
  return top;
}

/** The findings of the `glibc` check: [label needs GLIBC_x.y], the newest seen and how many ELF files. */
export function findNewGlibc(targets, { max = GLIBC_FLOOR } = {}) {
  const floor = version(max);
  const problems = [];
  let worst = null;
  let elves = 0;
  for (const c of everything(targets)) {
    if (!startsWith(c.data, ELF)) continue;
    elves++;
    const need = glibcNeeded(c.data);
    if (!need) continue;
    if (!worst || newer(need, worst)) worst = need;
    if (newer(need, floor)) problems.push(`${c.label} needs GLIBC_${need.join(".")}`);
  }
  return { problems, worst, elves };
}

// ------------------------------------------------------------------ the C runtime of Windows programs

/** The DLLs of the Visual C++ runtime, which a Windows program built without +crt-static imports:
 * the VC runtime (VCRUNTIME140.dll, VCRUNTIME140_1.dll), the C++ library (MSVCP140*.dll), the UCRT
 * (api-ms-win-crt-*.dll, ucrtbase.dll), older ones (MSVCR120.dll …) and the extra ones (CONCRT140,
 * VCOMP140, VCCORLIB140); debug builds' too (…d.dll). Not msvcrt.dll nor msvcp_win.dll: Windows'
 * own. */
export const CRT_DLL = /^(?:vcruntime\d|msvc[pr]\d|api-ms-win-crt-|ucrtbased?\.dll$|concrt\d|vcomp\d|vccorlib\d)/i;

const MAX_IMPORTS = 4096; // descriptors read per table: a broken file ends the list

/** The DLLs a PE file (a Windows .exe, .dll or .com program; PE32 or PE32+) imports, the ones it
 * delay-loads included, as its import tables name them; null for any other file. */
export function peImports(data) {
  if (!startsWith(data, MZ) || data.length < 0x40) return null;
  const pe = data.readUInt32LE(0x3c);
  if (pe + 24 > data.length || data.readUInt32LE(pe) !== 0x00004550) return null; // "PE\0\0"
  const sectionCount = data.readUInt16LE(pe + 6);
  const optSize = data.readUInt16LE(pe + 20);
  const opt = pe + 24;
  if (optSize < 2 || opt + optSize > data.length) return null;
  const magic = data.readUInt16LE(opt);
  const plus = magic === 0x20b;
  if (!plus && magic !== 0x10b) return null;
  const dirs = opt + (plus ? 112 : 96); // the data directories, after NumberOfRvaAndSizes
  if (dirs > opt + optSize) return null;
  const dirCount = Math.min(data.readUInt32LE(dirs - 4), Math.floor((opt + optSize - dirs) / 8));
  const imageBase = plus ? data.readBigUInt64LE(opt + 24) : BigInt(data.readUInt32LE(opt + 28));
  const sections = [];
  for (let i = 0, at = opt + optSize; i < sectionCount && at + 40 <= data.length; i++, at += 40) {
    sections.push({ va: data.readUInt32LE(at + 12), size: Math.max(data.readUInt32LE(at + 8), data.readUInt32LE(at + 16)), raw: data.readUInt32LE(at + 20) });
  }
  /** The file offset of an address in the image (-1: in no section, or past the file's end). */
  const offset = (rva) => {
    const s = sections.find((s) => rva >= s.va && rva < s.va + s.size);
    const at = s ? s.raw + (rva - s.va) : -1;
    return at >= 0 && at < data.length ? at : -1;
  };
  const dllName = (rva) => {
    const at = offset(rva);
    return at < 0 ? null : cstr(data, at, 260) || null;
  };
  const table = (index, size, nameAt) => {
    if (index >= dirCount) return [];
    const rva = data.readUInt32LE(dirs + index * 8);
    let at = rva ? offset(rva) : -1;
    const names = [];
    for (let k = 0; at >= 0 && at + size <= data.length && k < MAX_IMPORTS; k++, at += size) {
      if (data.subarray(at, at + size).every((b) => b === 0)) break;
      const name = dllName(nameAt(at));
      if (name) names.push(name);
    }
    return names;
  };
  return [
    // IMAGE_IMPORT_DESCRIPTOR: its Name at 12
    ...table(1, 20, (at) => data.readUInt32LE(at + 12)),
    // the delay-load descriptor: its DllNameRVA at 4, an address in the image when Attributes says
    // so (bit 0, every linker since Visual C++ 7), else a virtual address (Visual C++ 6)
    ...table(13, 32, (at) => {
      const name = data.readUInt32LE(at + 4);
      return data.readUInt32LE(at) & 1 ? name : Number((BigInt(name) - imageBase) & 0xffffffffn);
    }),
  ];
}

/** The Universal CRT's DLLs, which Windows 10 and later have of their own (the VC runtime they don't). */
const UCRT_DLL = /^(?:api-ms-win-crt-|ucrtbase\.dll$)/i;

/** The findings of the `crt` check: [label imports …] and how many PE files there were. Files named
 * in `allowUcrt` (any folder, any case) may import the Universal CRT: others' prebuilt files that
 * link it from the system (Electron's dxil.dll), never one of ours. */
export function findCrtImports(targets, { allowUcrt = [] } = {}) {
  const allowed = new Set(allowUcrt.map((n) => n.toLowerCase()));
  const problems = [];
  let programs = 0;
  for (const c of everything(targets)) {
    const imports = peImports(c.data);
    if (!imports) continue;
    programs++;
    const name = c.label.split(/[/\\›]/).pop().trim().toLowerCase();
    const crt = [...new Set(imports.filter((n) => CRT_DLL.test(n) && !(allowed.has(name) && UCRT_DLL.test(n))))];
    if (crt.length) problems.push(`${c.label} imports ${crt.join(", ")}`);
  }
  return { problems, programs };
}

// ------------------------------------------------------------------ command line

const USAGE = `usage: node tools/package_checks.mjs paths [--home DIR …] [--user NAME] TARGET …
       node tools/package_checks.mjs glibc [--max ${GLIBC_FLOOR}] TARGET …
       node tools/package_checks.mjs crt [--allow-ucrt NAME …] TARGET …`;

function main(argv) {
  const [cmd, ...rest] = argv;
  const opts = {};
  const targets = [];
  for (let i = 0; i < rest.length; i++) {
    const a = rest[i];
    if (["--home", "--user", "--max", "--allow-ucrt"].includes(a) && i + 1 < rest.length) (opts[a] ??= []).push(rest[++i]);
    else if (a.startsWith("--")) return console.error(USAGE), 2;
    else targets.push(a);
  }
  if (!targets.length || !["paths", "glibc", "crt"].includes(cmd)) return console.error(USAGE), 2;
  // each command its own options
  const allowed = { paths: ["--home", "--user"], glibc: ["--max"], crt: ["--allow-ucrt"] }[cmd];
  if (Object.keys(opts).some((o) => !allowed.includes(o))) return console.error(USAGE), 2;
  try {
    if (cmd === "paths") {
      const homes = opts["--home"] ?? [homedir()];
      const user = opts["--user"]?.[0] ?? userInfo().username;
      const { problems, files, members } = findBuildPaths(targets, { homes, user });
      if (problems.length) {
        console.error(`the build machine's home folder (${homes.join(", ")}) or user (${user}) is in the package:\n  ${problems.join("\n  ")}`);
        return 1;
      }
      console.log(`no build-machine paths: ${files} files, ${members} archive members`);
    } else if (cmd === "crt") {
      const { problems, programs } = findCrtImports(targets, { allowUcrt: opts["--allow-ucrt"] ?? [] });
      if (problems.length) {
        console.error(`the Visual C++ runtime is not linked in (+crt-static; the packages don't ship it):\n  ${problems.join("\n  ")}`);
        return 1;
      }
      if (!programs) {
        console.error(`no Windows program in ${targets.join(", ")}`);
        return 1;
      }
      console.log(`no Visual C++ runtime DLL imported: ${programs} Windows programs and libraries`);
    } else {
      const max = opts["--max"]?.[0] ?? GLIBC_FLOOR;
      const { problems, worst, elves } = findNewGlibc(targets, { max });
      if (problems.length) {
        console.error(`glibc newer than ${max} (RHEL 9):\n  ${problems.join("\n  ")}`);
        return 1;
      }
      if (!elves) {
        console.error(`no ELF file in ${targets.join(", ")}`);
        return 1;
      }
      console.log(`glibc ≤ ${max}: ${elves} ELF files, the newest needs ${worst ? `GLIBC_${worst.join(".")}` : "none"}`);
    }
  } catch (e) {
    if (!(e instanceof CheckError)) throw e;
    console.error(e.message);
    return 1;
  }
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = main(process.argv.slice(2));
