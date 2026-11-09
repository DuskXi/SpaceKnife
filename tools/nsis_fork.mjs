// The Windows installer's forked NSIS template (apps/desktop/src-tauri/windows: installer.nsi and
// spaceknife.nsh, docs in their headers) follows tauri-bundler's own template, the version the
// locked tauri-cli pins (windows/upstream.json). This checks that the pin still holds, so a
// tauri-cli update can't change what the fork was written against without someone merging it:
//
//   node tools/nsis_fork.mjs check [--offline]
//       - apps/desktop/package.json and pnpm-lock.yaml lock @tauri-apps/cli at upstream.json's
//         tauriCli, and the fork's headers name the pinned versions;
//       - crates.io: that tauri-cli depends on exactly tauri-bundler = upstream.json's
//         tauriBundler; the crate (checked against the crates.io index) holds the files the fork
//         follows with the SHA-256 recorded (LF line ends), and its NSIS and nsis_tauri_utils
//         are the pinned ones;
//       - the CLI the build installed (apps/desktop/node_modules, when there) embeds exactly
//         those files: what `tauri build` would have used without the fork.
//     --offline skips crates.io and the CLI (the lockfile and the headers only).
//   node tools/nsis_fork.mjs hashes DIR
//       the hashes for upstream.json from an unpacked tauri-bundler crate.
//   node tools/nsis_fork.mjs tools DIR
//       what the bundler downloads for an NSIS bundle, put in its tools folder DIR beforehand
//       (`<target>/.tauri`: tauri.bundle.windows.json useLocalToolsDir): DIR/NSIS laid out as the
//       bundler's get_and_extract_nsis does, from upstream.json's NSIS and nsis_tauri_utils, their
//       SHA-1s checked (the ones the bundler checks: `check` holds them to its mod.rs), each
//       download tried a few times; nothing when DIR/NSIS already passes the bundler's own checks
//       (a cache). And WebView2's bootstrapper (DIR/MicrosoftEdgeWebview2Setup.exe), fetched fresh
//       with retries, an older copy kept when that fails. Then `tauri build` downloads nothing.
//
// Exit status 1 when anything differs (each difference on a line of its own).

import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { gunzipSync } from "node:zlib";

import { zipMembers } from "./package_checks.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const DESKTOP = join(ROOT, "apps", "desktop");
export const FORK = join(DESKTOP, "src-tauri", "windows");
const AGENT = "SpaceKnife-nsis-fork-check (https://github.com/DuskXi/SpaceKnife)";
/** What the CLI embeds (include_str!/include_bytes! in the bundler's nsis/mod.rs): the files its
 * binary must hold verbatim (with LF or CRLF line ends, by the machine it was built on). */
const EMBEDDED = /\.(nsi|nsh)$/;

export class ForkError extends Error {}

/** LF line ends: the crate has them, a CLI built on Windows has CRLF. */
export const lf = (buf) => Buffer.from(buf.toString("latin1").replaceAll("\r\n", "\n"), "latin1");
export const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");

/** The files of a (gzipped) tar archive: Map(path -> Buffer). ustar names with their prefix, GNU
 * long names and pax paths. */
export function untar(gz) {
  const tar = gunzipSync(gz);
  const out = new Map();
  let longName = null;
  for (let at = 0; at + 512 <= tar.length; ) {
    const head = tar.subarray(at, at + 512);
    if (head.every((b) => b === 0)) break;
    const str = (from, len) => head.subarray(from, from + len).toString("utf8").replace(/\0.*$/s, "");
    const size = parseInt(str(124, 12).trim() || "0", 8);
    const type = String.fromCharCode(head[156] || 0x30);
    const body = tar.subarray(at + 512, at + 512 + size);
    at += 512 + Math.ceil(size / 512) * 512;
    if (type === "L") {
      longName = body.toString("utf8").replace(/\0.*$/s, "");
      continue;
    }
    if (type === "x") {
      const m = /(?:^|\n)\d+ path=([^\n]*)\n/.exec(body.toString("utf8"));
      if (m) longName = m[1];
      continue;
    }
    const prefix = str(345, 155);
    const name = longName ?? (prefix ? `${prefix}/${str(0, 100)}` : str(0, 100));
    longName = null;
    if (type === "0" || type === "\0") out.set(name, Buffer.from(body));
  }
  return out;
}

export function readPin(dir = FORK) {
  return JSON.parse(readFileSync(join(dir, "upstream.json"), "utf8"));
}

/** The @tauri-apps/cli version apps/desktop asks for and the one its lockfile resolved. */
export function lockedCli(packageJson, lockfile) {
  const pkg = JSON.parse(packageJson);
  const asked = pkg.devDependencies?.["@tauri-apps/cli"] ?? pkg.dependencies?.["@tauri-apps/cli"] ?? null;
  const m = /\n\s+'?@tauri-apps\/cli'?:\s*\n\s+specifier:\s*([^\n]+)\n\s+version:\s*([^\s(]+)/.exec(lockfile);
  return { asked, specifier: m?.[1]?.trim() ?? null, locked: m?.[2]?.trim() ?? null };
}

/** The problems of the pin against the lockfile and the fork's own headers (no network). */
export function offlineProblems(pin, { packageJson, lockfile, template, helpers }) {
  const problems = [];
  const cli = lockedCli(packageJson, lockfile);
  if (cli.asked !== pin.tauriCli) problems.push(`apps/desktop/package.json asks for @tauri-apps/cli ${cli.asked}, the fork follows ${pin.tauriCli} (pin it exactly)`);
  if (cli.locked !== pin.tauriCli) problems.push(`apps/desktop/pnpm-lock.yaml locks @tauri-apps/cli ${cli.locked}, the fork follows ${pin.tauriCli}`);
  if (!template.includes(`tauri-bundler ${pin.tauriBundler}'s NSIS template`) || !template.includes(`tauri-cli ${pin.tauriCli}`)) {
    problems.push(`windows/installer.nsi does not say it is tauri-bundler ${pin.tauriBundler}'s template of tauri-cli ${pin.tauriCli}`);
  }
  const pinned = `Pinned to tauri-cli ${pin.tauriCli} / tauri-bundler ${pin.tauriBundler} / NSIS ${pin.nsis} / nsis_tauri_utils ${pin.nsisTauriUtils}`;
  if (!helpers.includes(pinned)) problems.push(`windows/spaceknife.nsh does not say "${pinned}"`);
  if (!Object.keys(pin.files ?? {}).some((f) => f.endsWith("/installer.nsi"))) problems.push("upstream.json pins no installer.nsi");
  return problems;
}

/** The problems of an unpacked tauri-bundler crate (Map path -> Buffer, paths without the crate's
 * top folder) against the pin. */
export function crateProblems(pin, files) {
  const problems = [];
  for (const [path, want] of Object.entries(pin.files)) {
    const got = files.get(path);
    if (!got) problems.push(`tauri-bundler ${pin.tauriBundler} has no ${path}`);
    else if (sha256(lf(got)) !== want) problems.push(`tauri-bundler ${pin.tauriBundler} ${path}: SHA-256 ${sha256(lf(got))}, the fork follows ${want}: merge the upstream change into windows/ and update upstream.json`);
  }
  const mod = files.get("src/bundle/windows/nsis/mod.rs")?.toString("utf8") ?? "";
  if (!mod.includes(`/nsis-${pin.nsis}.zip`)) problems.push(`tauri-bundler ${pin.tauriBundler} does not use NSIS ${pin.nsis}`);
  if (!mod.includes(`nsis_tauri_utils-v${pin.nsisTauriUtils}/`)) problems.push(`tauri-bundler ${pin.tauriBundler} does not use nsis_tauri_utils ${pin.nsisTauriUtils}`);
  // what `tools` lays out must pass the bundler's own checks: the same SHA-1s, the same files
  const upper = mod.toUpperCase();
  for (const [key, what] of [
    ["nsisSha1", "NSIS's download"],
    ["nsisExeSha1", "NSIS.exe"],
    ["nsisTauriUtilsSha1", "nsis_tauri_utils.dll"],
  ]) {
    if (pin[key] && !upper.includes(`"${pin[key].toUpperCase()}"`)) problems.push(`tauri-bundler ${pin.tauriBundler} does not check ${what} against SHA-1 ${pin[key]} (upstream.json ${key})`);
  }
  if (pin.nsisSha1) {
    const required = /NSIS_REQUIRED_FILES: &\[&str\] = &\[([^\]]*)\]/.exec(mod)?.[1] ?? "";
    const listed = [...required.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    for (const f of NSIS_REQUIRED.filter((f) => !listed.includes(f))) problems.push(`tauri-bundler ${pin.tauriBundler} no longer needs ${f} (tools/nsis_fork.mjs NSIS_REQUIRED)`);
    for (const f of listed.filter((f) => !NSIS_REQUIRED.includes(f))) problems.push(`tauri-bundler ${pin.tauriBundler} needs ${f} too (tools/nsis_fork.mjs NSIS_REQUIRED)`);
  }
  return problems;
}

// ------------------------------------------------------------------ the bundler's NSIS, beforehand

/** The files the bundler (Windows) requires in its NSIS folder, else it downloads NSIS again. */
export const NSIS_REQUIRED = [
  "makensis.exe",
  "Bin/makensis.exe",
  "Stubs/lzma-x86-unicode",
  "Stubs/lzma_solid-x86-unicode",
  "Plugins/x86-unicode/additional/nsis_tauri_utils.dll",
  "Include/MUI2.nsh",
  "Include/FileFunc.nsh",
  "Include/x64.nsh",
  "Include/nsDialogs.nsh",
  "Include/WinMessages.nsh",
  "Include/Win/COM.nsh",
  "Include/Win/Propkey.nsh",
  "Include/Win/RestartManager.nsh",
];
const UTILS_DLL = "Plugins/x86-unicode/additional/nsis_tauri_utils.dll";
/** tauri-bundler's (windows/util.rs WEBVIEW2_BOOTSTRAPPER_URL); saved as below in its tools folder. */
export const WEBVIEW2_BOOTSTRAPPER_URL = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";
const WEBVIEW2_BOOTSTRAPPER = "MicrosoftEdgeWebview2Setup.exe";

export const sha1 = (buf) => createHash("sha1").update(buf).digest("hex").toUpperCase();

/** What the bundler downloads (its nsis/mod.rs NSIS_URL, NSIS_TAURI_UTILS_URL) and their SHA-1s. */
export function nsisDownloads(pin) {
  return {
    nsis: { url: `https://github.com/tauri-apps/binary-releases/releases/download/nsis-${pin.nsis}/nsis-${pin.nsis}.zip`, sha1: pin.nsisSha1 },
    utils: { url: `https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v${pin.nsisTauriUtils}/nsis_tauri_utils.dll`, sha1: pin.nsisTauriUtilsSha1 },
  };
}

const sha1Of = (p) => {
  try {
    return sha1(readFileSync(p));
  } catch {
    return null;
  }
};

/** Whether the bundler takes `dir` (its tools folder's NSIS) as it is: every required file there,
 * NSIS.exe and the plugin with the SHA-1s it checks (bundle_project, nsis/mod.rs). */
export function nsisReady(dir, pin) {
  return (
    NSIS_REQUIRED.every((f) => existsSync(join(dir, f))) &&
    sha1Of(join(dir, "NSIS.exe")) === pin.nsisExeSha1?.toUpperCase() &&
    sha1Of(join(dir, UTILS_DLL)) === pin.nsisTauriUtilsSha1?.toUpperCase()
  );
}

/** GET `url` (bytes), at most `timeout` ms. */
async function download(url, timeout = 120_000) {
  const r = await fetch(url, { headers: { "user-agent": AGENT }, signal: AbortSignal.timeout(timeout) });
  if (!r.ok) throw new ForkError(`${url}: HTTP ${r.status}`);
  return Buffer.from(await r.arrayBuffer());
}

/** `fetcher(url)`, tried `tries` times, waiting `wait` ms and twice as long each time between. */
async function fetchRetrying(url, { fetcher, tries, wait, log }) {
  for (let i = 1; ; i++) {
    try {
      return await fetcher(url);
    } catch (e) {
      if (i >= tries) throw e;
      const ms = wait * 2 ** (i - 1);
      log(`${e.message}: trying again in ${Math.round(ms / 1000)} s (${i + 1} of ${tries})`);
      await new Promise((r) => setTimeout(r, ms));
    }
  }
}

/** `fetchRetrying`, the bytes checked against `sha1`. */
async function fetchChecked({ url, sha1: want }, opts) {
  if (!want) throw new ForkError(`no SHA-1 pinned for ${url} (upstream.json)`);
  const data = await fetchRetrying(url, opts);
  if (sha1(data) !== want.toUpperCase()) throw new ForkError(`${url}: SHA-1 ${sha1(data)}, upstream.json pins ${want}`);
  return data;
}

/** A pinned download kept in `downloads` (a cache): the copy there when its SHA-1 is the pinned
 * one, else fetched (`fetchChecked`) and kept. [bytes, whether it was fetched] */
async function keptDownload(spec, name, downloads, opts) {
  const kept = join(downloads, name);
  if (existsSync(kept)) {
    const data = readFileSync(kept);
    if (sha1(data) === spec.sha1?.toUpperCase()) return [data, false];
    opts.log(`${kept}: SHA-1 ${sha1(data)}, not the pinned one: fetched again`);
  }
  const data = await fetchChecked(spec, opts);
  mkdirSync(downloads, { recursive: true });
  writeFileSync(`${kept}.part`, data);
  renameSync(`${kept}.part`, kept);
  return [data, true];
}

/** The tools folder `tools` as the bundler would leave it after downloading (the `tools` command).
 * NSIS's zip and the plugin, SHA-1s checked, are kept in `downloads` (the workflow caches that
 * folder): "cached" when both were there, "fetched" when one was downloaded. NSIS is unpacked
 * afresh each time from the checked zip, beside the NSIS folder and then renamed into place (no
 * file of an earlier folder is trusted; a run that stops halfway leaves no folder the bundler
 * would take). WebView2's bootstrapper is fetched each time; when that fails, an older copy is
 * kept (the bundler keeps whatever is there), with none it is an error. */
export async function prepareTools(tools, { pin = readPin(), fetcher = download, tries = 5, wait = 5000, log = console.log, downloads = join(tools, "downloads") } = {}) {
  const opts = { fetcher, tries, wait, log };
  mkdirSync(tools, { recursive: true });
  const dir = join(tools, "NSIS");
  const { nsis, utils } = nsisDownloads(pin);
  const [archive, fetchedZip] = await keptDownload(nsis, `nsis-${pin.nsis}.zip`, downloads, opts);
  const [dll, fetchedDll] = await keptDownload(utils, `nsis_tauri_utils-${pin.nsisTauriUtils}.dll`, downloads, opts);
  const state = fetchedZip || fetchedDll ? "fetched" : "cached";
  const tmp = join(tools, `.NSIS-${process.pid}`);
  rmSync(tmp, { recursive: true, force: true });
  try {
    const top = `nsis-${pin.nsis}/`;
    for (const m of zipMembers(archive, `nsis-${pin.nsis}.zip`)) {
      const rel = m.name.startsWith(top) ? m.name.slice(top.length) : null;
      if (!rel || isAbsolute(rel) || rel.split(/[\\/]/).some((s) => s === ".." || s === "" || s.includes(":"))) throw new ForkError(`nsis-${pin.nsis}.zip: ${m.name} is outside ${top}`);
      const out = join(tmp, ...rel.split("/"));
      mkdirSync(dirname(out), { recursive: true });
      writeFileSync(out, m.data());
    }
    const plugin = join(tmp, ...UTILS_DLL.split("/"));
    mkdirSync(dirname(plugin), { recursive: true });
    writeFileSync(plugin, dll);
    if (!nsisReady(tmp, pin)) throw new ForkError(`nsis-${pin.nsis}.zip unpacked is not what the bundler checks for (NSIS.exe SHA-1 ${sha1Of(join(tmp, "NSIS.exe"))}, upstream.json nsisExeSha1 ${pin.nsisExeSha1})`);
    rmSync(dir, { recursive: true, force: true });
    renameSync(tmp, dir);
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
  log(`${dir}: NSIS ${pin.nsis} and nsis_tauri_utils ${pin.nsisTauriUtils} (${state === "cached" ? "kept" : "fetched"}, SHA-1s checked)`);
  const bootstrapper = join(tools, WEBVIEW2_BOOTSTRAPPER);
  try {
    const data = await fetchRetrying(WEBVIEW2_BOOTSTRAPPER_URL, opts);
    if (data.length < 2 || data[0] !== 0x4d || data[1] !== 0x5a) throw new ForkError(`${WEBVIEW2_BOOTSTRAPPER_URL}: not a program (${data.length} bytes)`);
    writeFileSync(`${bootstrapper}.part`, data);
    renameSync(`${bootstrapper}.part`, bootstrapper);
    log(`${bootstrapper}: fetched`);
  } catch (e) {
    rmSync(`${bootstrapper}.part`, { force: true });
    if (!existsSync(bootstrapper)) throw e;
    log(`${e.message}: the bootstrapper already there stays`);
  }
  return state;
}

/** The problems of the CLI's native program (a Buffer) against the crate's files it embeds. */
export function binaryProblems(pin, files, binary, name) {
  const problems = [];
  for (const path of Object.keys(pin.files).filter((p) => EMBEDDED.test(p))) {
    const text = lf(files.get(path) ?? Buffer.alloc(0));
    const crlf = Buffer.from(text.toString("latin1").replaceAll("\n", "\r\n"), "latin1");
    if (!text.length || (binary.indexOf(text) < 0 && binary.indexOf(crlf) < 0)) problems.push(`${name} does not embed tauri-bundler ${pin.tauriBundler}'s ${path}: it is not the CLI the fork follows`);
  }
  return problems;
}

async function get(url, as = "bytes") {
  const r = await fetch(url, { headers: { "user-agent": AGENT } });
  if (!r.ok) throw new ForkError(`${url}: HTTP ${r.status}`);
  return as === "json" ? r.json() : as === "text" ? r.text() : Buffer.from(await r.arrayBuffer());
}

/** tauri-cli's requirement on tauri-bundler, the crate and its unpacked files (from crates.io). */
export async function fetchUpstream(pin, fetcher = get) {
  const problems = [];
  const deps = await fetcher(`https://crates.io/api/v1/crates/tauri-cli/${pin.tauriCli}/dependencies`, "json");
  const req = deps.dependencies?.find((d) => d.crate_id === "tauri-bundler" && d.kind === "normal")?.req;
  if (req !== `=${pin.tauriBundler}`) problems.push(`tauri-cli ${pin.tauriCli} asks for tauri-bundler ${req}, not =${pin.tauriBundler}`);
  const index = await fetcher("https://index.crates.io/ta/ur/tauri-bundler", "text");
  const entry = index.split("\n").filter(Boolean).map((l) => JSON.parse(l)).find((e) => e.vers === pin.tauriBundler);
  if (!entry) throw new ForkError(`crates.io has no tauri-bundler ${pin.tauriBundler}`);
  const crate = await fetcher(`https://static.crates.io/crates/tauri-bundler/tauri-bundler-${pin.tauriBundler}.crate`);
  if (sha256(crate) !== entry.cksum) throw new ForkError(`tauri-bundler-${pin.tauriBundler}.crate: SHA-256 ${sha256(crate)}, the index says ${entry.cksum}`);
  const top = `tauri-bundler-${pin.tauriBundler}/`;
  const files = new Map([...untar(crate)].filter(([p]) => p.startsWith(top)).map(([p, b]) => [p.slice(top.length), b]));
  return { problems, files };
}

/** The native programs of the @tauri-apps/cli installed for apps/desktop (one per platform package
 * found; none when node_modules isn't there). */
export function installedCliBinaries(desktop = DESKTOP) {
  const out = [];
  let cliPkg;
  try {
    cliPkg = createRequire(join(desktop, "package.json")).resolve("@tauri-apps/cli/package.json");
  } catch {
    return out;
  }
  const req = createRequire(cliPkg);
  const meta = JSON.parse(readFileSync(cliPkg, "utf8"));
  for (const name of Object.keys(meta.optionalDependencies ?? {})) {
    let pkg;
    try {
      pkg = req.resolve(`${name}/package.json`);
    } catch {
      continue;
    }
    for (const f of readdirSync(dirname(pkg)).filter((f) => f.endsWith(".node"))) out.push({ name: `${name}/${f}`, version: JSON.parse(readFileSync(pkg, "utf8")).version, path: join(dirname(pkg), f) });
  }
  return out;
}

export async function check({ offline = false, fetcher = get, dir = FORK, desktop = DESKTOP, log = console.log } = {}) {
  const pin = readPin(dir);
  const problems = offlineProblems(pin, {
    packageJson: readFileSync(join(desktop, "package.json"), "utf8"),
    lockfile: readFileSync(join(desktop, "pnpm-lock.yaml"), "utf8"),
    template: readFileSync(join(dir, "installer.nsi"), "utf8"),
    helpers: readFileSync(join(dir, "spaceknife.nsh"), "utf8"),
  });
  log(`the fork follows tauri-cli ${pin.tauriCli} / tauri-bundler ${pin.tauriBundler} (NSIS ${pin.nsis}, nsis_tauri_utils ${pin.nsisTauriUtils})`);
  if (offline) return problems;
  const upstream = await fetchUpstream(pin, fetcher);
  problems.push(...upstream.problems, ...crateProblems(pin, upstream.files));
  log(`crates.io: tauri-bundler ${pin.tauriBundler}, ${Object.keys(pin.files).length} files compared`);
  const binaries = installedCliBinaries(desktop);
  if (!binaries.length) log("no @tauri-apps/cli installed in apps/desktop: its program is not compared");
  for (const b of binaries) {
    if (b.version !== pin.tauriCli) problems.push(`${b.name} is version ${b.version}, not ${pin.tauriCli}`);
    problems.push(...binaryProblems(pin, upstream.files, readFileSync(b.path), b.name));
    log(`${b.name} ${b.version}: compared`);
  }
  return problems;
}

/** The pin's file hashes from an unpacked tauri-bundler crate folder. */
export function hashes(dir, pin = readPin()) {
  const out = {};
  for (const path of Object.keys(pin.files)) {
    const p = join(dir, path);
    out[path] = existsSync(p) ? sha256(lf(readFileSync(p))) : null;
  }
  return out;
}

async function main(argv) {
  const [cmd, arg] = argv.filter((a) => !a.startsWith("--"));
  if (cmd === "hashes" && arg) {
    console.log(JSON.stringify(hashes(resolve(arg)), null, 2));
    return 0;
  }
  if (cmd === "tools" && arg) {
    try {
      await prepareTools(resolve(arg));
      return 0;
    } catch (e) {
      // a download that failed every time, a SHA-1 that differs, an archive that is not NSIS's
      console.error(`PROBLEM ${e.message}`);
      return 1;
    }
  }
  if (cmd !== "check") {
    console.error("usage: node tools/nsis_fork.mjs check [--offline] | hashes DIR | tools DIR");
    return 2;
  }
  let problems;
  try {
    problems = await check({ offline: argv.includes("--offline") });
  } catch (e) {
    if (!(e instanceof ForkError) && !(e instanceof TypeError)) throw e;
    problems = [e.message];
  }
  for (const p of problems) console.error(`PROBLEM ${p}`);
  console.log(problems.length ? `the fork's pin does not hold (${problems.length})` : "the fork's pin holds");
  return problems.length ? 1 : 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = await main(process.argv.slice(2));
