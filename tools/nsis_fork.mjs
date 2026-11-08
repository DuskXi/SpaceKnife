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
//
// Exit status 1 when anything differs (each difference on a line of its own).

import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { gunzipSync } from "node:zlib";

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
  return problems;
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
  if (cmd !== "check") {
    console.error("usage: node tools/nsis_fork.mjs check [--offline] | hashes DIR");
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
