// Release helper for the Release workflow (.github/workflows/release.yml); it runs here too.
// Was tools/release.py assemble (`check` is now the Rust example
// `cargo run -p sk-core --example release_check`, which reads a release with the app's own code).
//
//   node tools/release.mjs assemble 0.4.0 ARTIFACTS OUT --notes release-notes/0.4.0.md \
//       --notes-out notes.md [--build-run URL …]
//
// `assemble` takes the build runs' artifacts (any folder layout: one folder per artifact, as
// actions/download-artifact and `gh run download` write them), checks every checksum file against
// the files next to it, checks that exactly the release's files are there (the names the update
// check looks for: crates/sk-core/src/update `asset_templates`), copies them into OUT with the
// checksum files the release publishes (the two installer lists merged into SHA256SUMS.txt), and
// fills the notes' placeholders: {{version}}, {{size:FILE}} (MiB, rounded, written "N MB" as in
// every release so far) and {{build_runs}} (links to the runs that built the files).

import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { basename, dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const SUMS = "SHA256SUMS.txt";
export const INSTALLER_SUMS = ["SHA256SUMS-installers-win-x64.txt", "SHA256SUMS-installers-linux-x64.txt"];
const HEX64 = /^[0-9a-f]{64}$/;
const PLACEHOLDER = /\{\{([^{}]*)\}\}/g;

export class ReleaseError extends Error {}

/** Every file of a release: {template, sums} — the checksum file it is published in (null: none). */
export function expected() {
  const out = [
    // Tauri's installers (x64 only), listed in SHA256SUMS.txt
    { template: "SpaceKnife_{v}_x64-setup.exe", sums: SUMS },
    { template: "SpaceKnife_{v}_amd64.AppImage", sums: SUMS },
    { template: "SpaceKnife_{v}_amd64.deb", sums: SUMS },
    { template: "SpaceKnife-{v}-1.x86_64.rpm", sums: SUMS },
  ];
  for (const [target, ext] of [["win-x64", "zip"], ["win-arm64", "zip"], ["linux-x64", "tar.gz"], ["linux-arm64", "tar.gz"]]) {
    out.push({ template: `SpaceKnife-{v}-${target}-portable.${ext}`, sums: `SHA256SUMS-portable-${target}.txt` });
  }
  for (const [a, dmg] of [["arm64", "aarch64"], ["x64", "x64"]]) {
    out.push({ template: `SpaceKnife_{v}_${dmg}.dmg`, sums: `SHA256SUMS-mac-${a}.txt` });
    out.push({ template: `SpaceKnife-{v}-mac-${a}-portable.zip`, sums: `SHA256SUMS-mac-${a}.txt` });
  }
  return out;
}

/** The checksum files the release publishes. */
export const publishedSums = () => [...new Set(expected().map((e) => e.sums).filter(Boolean))].sort();

/** The file for `template` among `names`: the exact name for `version`, else (a pre-release) the
 * name with the version spelled as a packager may write it (- as ~ or _, which rpm wants); the app
 * picks either (update::pick_asset). A file of another version is never taken. */
export function pick(template, version, names) {
  for (const v of [version, version.replaceAll("-", "~"), version.replaceAll("-", "_")]) {
    const name = template.replace("{v}", v);
    if (names.includes(name)) return name;
  }
  return null;
}

/** `sha256sum` / `shasum -a 256` lines -> Map(file name -> hex); anything else is an error. */
export function parseSums(text) {
  const out = new Map();
  text.split(/\r?\n/).forEach((line, i) => {
    if (!line.trim()) return;
    const m = /^(\S+)\s+(.+)$/.exec(line.trim());
    if (!m || !HEX64.test(m[1].toLowerCase())) throw new ReleaseError(`line ${i + 1} is not a checksum: ${JSON.stringify(line)}`);
    const name = m[2].trim().replace(/^\*/, "").replaceAll("\\", "/").split("/").at(-1);
    if (out.has(name)) throw new ReleaseError(`${name} is listed twice`);
    out.set(name, m[1].toLowerCase());
  });
  return out;
}

export function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

/** A file size as every release so far wrote it: MiB, rounded, with "MB". */
export const mib = (size) => `${Math.max(1, Math.round(size / (1 << 20)))} MB`;

function* files(dir) {
  for (const name of readdirSync(dir).sort()) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) yield* files(p);
    else yield p;
  }
}

/** Check the artifacts and copy the release's files into `out` (empty or new): {files: Map(name -> path), hashes: Map}. */
export function assemble(version, artifacts, out) {
  if (!existsSync(artifacts) || !statSync(artifacts).isDirectory()) throw new ReleaseError(`no artifacts folder: ${artifacts}`);
  const found = new Map();
  for (const p of files(artifacts)) found.set(basename(p), [...(found.get(basename(p)) ?? []), p]);
  const problems = [];
  for (const [name, paths] of found) {
    if (paths.length > 1) problems.push(`${name} is in more than one artifact: ${paths.map((p) => basename(dirname(p))).join(", ")}`);
  }

  // every checksum file, against the files next to it
  const hashes = new Map();
  for (const [name, [path]] of found) {
    if (!name.toUpperCase().startsWith("SHA256SUMS")) continue;
    let listed;
    try {
      listed = parseSums(readFileSync(path, "utf8"));
    } catch (e) {
      problems.push(`${name}: ${e.message}`);
      continue;
    }
    if (!listed.size) problems.push(`${name} lists no file`);
    for (const [fname, digest] of listed) {
      const target = join(dirname(path), fname);
      if (!existsSync(target)) {
        problems.push(`${name} lists ${fname}, which is not next to it`);
        continue;
      }
      const actual = sha256(target);
      if (actual !== digest) problems.push(`${fname}: SHA-256 ${actual} but ${name} says ${digest}`);
      if (hashes.has(fname) && hashes.get(fname) !== digest) problems.push(`${fname} has two different checksums`);
      hashes.set(fname, digest);
    }
  }

  const names = [...found.keys()];
  const want = expected().map((e) => ({ ...e, name: pick(e.template, version, names) }));
  for (const e of want) {
    if (!e.name) problems.push(`missing: ${e.template.replace("{v}", version)}`);
    else if (e.sums && !hashes.has(e.name)) problems.push(`${e.name} is in no checksum file`);
  }
  const allowed = new Set([...want.map((e) => e.name).filter(Boolean), ...INSTALLER_SUMS, ...publishedSums()]);
  for (const name of names.sort()) if (!allowed.has(name)) problems.push(`not part of a release: ${name}`);
  for (const name of [...INSTALLER_SUMS, ...publishedSums().filter((s) => s !== SUMS)]) {
    if (!found.has(name)) problems.push(`missing checksum file: ${name}`);
  }
  if (problems.length) throw new ReleaseError(problems.join("\n"));

  mkdirSync(out, { recursive: true });
  if (readdirSync(out).length) throw new ReleaseError(`${out} is not empty`);
  const result = new Map();
  for (const e of want) {
    copyFileSync(found.get(e.name)[0], join(out, e.name));
    result.set(e.name, join(out, e.name));
  }
  for (const name of publishedSums()) {
    const dest = join(out, name);
    if (name === SUMS) {
      // the two installer lists, one file as in every release
      const merged = new Map(INSTALLER_SUMS.flatMap((s) => [...parseSums(readFileSync(found.get(s)[0], "utf8"))]));
      writeFileSync(dest, [...merged.keys()].sort().map((n) => `${merged.get(n)}  ${n}\n`).join(""));
    } else {
      copyFileSync(found.get(name)[0], dest);
    }
    result.set(name, dest);
  }
  return { files: result, hashes };
}

const runLabel = (url) => {
  const m = /\/actions\/runs\/(\d+)/.exec(url);
  return m ? `run ${m[1]}` : url;
};

/** Replace the placeholders; an unknown one or a size of a file not in the release is an error. */
export function fillNotes(text, version, fileMap, runs) {
  const problems = [];
  const out = text.replace(PLACEHOLDER, (whole, inner) => {
    const key = inner.trim();
    if (key === "version") return version;
    if (key === "build_runs") {
      if (!runs.length) problems.push("{{build_runs}} without --build-run");
      return runs.map((u) => `[${runLabel(u)}](${u})`).join(", ");
    }
    if (key.startsWith("size:")) {
      const name = key.slice(5).trim();
      if (!fileMap.has(name)) {
        problems.push(`{{size:${name}}}: not a file of this release`);
        return whole;
      }
      return mib(statSync(fileMap.get(name)).size);
    }
    problems.push(`unknown placeholder ${whole}`);
    return whole;
  });
  if (!out.includes(version)) problems.push(`the notes never mention ${version}`);
  if (problems.length) throw new ReleaseError(problems.join("\n"));
  return out;
}

// ------------------------------------------------------------------ command line

function main(argv) {
  const [cmd, version, artifacts, out] = argv.filter((a, i) => !a.startsWith("--") && !argv[i - 1]?.startsWith("--"));
  const opt = (name) => argv.flatMap((a, i) => (argv[i - 1] === name ? [a] : []));
  if (cmd !== "assemble" || !version || !artifacts || !out || !opt("--notes").length || !opt("--notes-out").length) {
    console.error("usage: node tools/release.mjs assemble VERSION ARTIFACTS OUT --notes FILE --notes-out FILE [--build-run URL …]");
    return 2;
  }
  let done;
  let notes;
  try {
    done = assemble(version, resolve(artifacts), resolve(out));
    notes = fillNotes(readFileSync(opt("--notes")[0], "utf8"), version, done.files, opt("--build-run"));
  } catch (e) {
    if (!(e instanceof ReleaseError) && !e.code) throw e;
    console.error(`release ${version}: not ready\n${e.message}`);
    return 1;
  }
  writeFileSync(opt("--notes-out")[0], notes);
  const rows = [...done.files].map(([name, path]) => [name, mib(statSync(path).size), (done.hashes.get(name) ?? "").slice(0, 16)]);
  const w = [0, 1].map((i) => Math.max(...rows.map((r) => r[i].length)));
  for (const r of rows) console.log(`${r[0].padEnd(w[0])}  ${r[1].padStart(w[1])}  ${r[2]}`);
  console.log(`${done.files.size} files in ${relative(process.cwd(), resolve(out)) || "."}, notes in ${opt("--notes-out")[0]}`);
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = main(process.argv.slice(2));
