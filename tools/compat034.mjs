// The update contract with every installed 0.3.4, checked with 0.3.4's own code (the merge plan
// 5.6 step 3): no window, no install, minutes not hours, so the Release workflow runs it on the
// assembled files before anything is published and Release check runs it on what was published.
//
//   node tools/compat034.mjs --tree V034 --version 0.3.5 --dir release/     (assembled, not published)
//   node tools/compat034.mjs --tree V034 --version 0.3.5 --github            (the published releases)
//
// --tree: a v0.3.4 source tree with its Python environment:
//     git archive --format=tar v0.3.4 | tar -x -C V034
//     uv sync --project V034 --frozen --no-default-groups --package spaceknife-backend
// (--python PY: another interpreter of that environment.)
//
// 1. 0.3.4's backend (spaceknife.update in the tree, through tools/compat034_check.py): it reads
//    the release list, must offer this version on its default channel (preview), and for every
//    kind of install of v0.3.4 tools/release.py INSTALLS must pick a file under 0.3.4's exact
//    name and find its SHA-256 in the checksum list it reads first; with --dir that checksum must
//    be the file's.
// 2. 0.3.4's window (apps/electron/src/main/update.ts in the tree, run here with its types
//    stripped): installRefusal must accept the picked file for its kind of install, as main
//    checks it before it runs or unpacks it; portableVersion must read the version.
// 3. With --dir, each portable archive is laid out as 0.3.4's replacePortable needs it: one folder
//    at the top, holding the program under the name 0.3.4 runs and the swap helper where
//    update.ts swapHelper looks (executable on Linux).
// Every file of the release that someone installs (tools/release.mjs expected) is picked by some
// kind of install — except a file of a kind of install 0.3.4 did not have (expected `since`: the
// Tauri app's Linux portable copy), which no 0.3.4 install may take, neither among the release's
// files nor alone in a release (compat034_check.py `takers`: 0.3.4's pick_asset on a release with
// just that file). Exit status 1 with the problems listed.

import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { stripTypeScriptTypes } from "node:module";
import { tmpdir } from "node:os";
import { join, posix, resolve, win32 } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { archiveEntries } from "./package_checks.mjs";
import { expected, sha256 } from "./release.mjs";

const HERE = fileURLToPath(new URL(".", import.meta.url));

/** The installs of 0.3.4's INSTALLS and the file each must find: 0.3.4's names, frozen. */
export const CONTRACT = [
  ["nsis", "win32", "AMD64", "SpaceKnife-{v}-Setup-x64.exe"],
  ["portable", "win32", "AMD64", "SpaceKnife-{v}-win-x64-portable.zip"],
  ["portable", "win32", "ARM64", "SpaceKnife-{v}-win-arm64-portable.zip"],
  ["portable", "linux", "x86_64", "SpaceKnife-{v}-linux-x64-portable.tar.gz"],
  ["portable", "linux", "aarch64", "SpaceKnife-{v}-linux-arm64-portable.tar.gz"],
  ["portable", "darwin", "arm64", "SpaceKnife-{v}-mac-arm64-portable.zip"],
  ["portable", "darwin", "x86_64", "SpaceKnife-{v}-mac-x64-portable.zip"],
  ["appimage", "linux", "x86_64", "SpaceKnife-{v}-x86_64.AppImage"],
  ["deb", "linux", "x86_64", "spaceknife_{v}_amd64.deb"],
  ["rpm", "linux", "x86_64", "spaceknife-{v}.x86_64.rpm"],
  ["dmg", "darwin", "arm64", "SpaceKnife-{v}-mac-arm64.dmg"],
  ["dmg", "darwin", "x86_64", "SpaceKnife-{v}-mac-x64.dmg"],
];

/** 0.3.4's pattern for a template, as its pick_asset builds and uses it (v0.3.4 update.py:
 * `re.compile(re.escape(template).replace(re.escape("{v}"), _V_SLOT))`, `fullmatch`, with
 * `_V_SLOT = r"[0-9A-Za-z.+~_-]+"`, case-sensitive). The Python half runs 0.3.4's own code
 * (compat034_check.py takers); this one lets the unit tests ask the same question offline. */
export function pattern034(template) {
  const escaped = template.replace(/[.*+?^${}()|[\]\\-]/g, "\\$&");
  return new RegExp(`^${escaped.replace("\\{v\\}", "[0-9A-Za-z.+~_-]+")}$`);
}

/** deb and rpm write a pre-release part with ~ (v0.3.4 tools/release.py deb_version). */
export const wantName = (kind, template, version) => template.replace("{v}", kind === "deb" || kind === "rpm" ? version.replaceAll("-", "~") : version);

/** 0.3.4's update.ts as a module: its own code, the re-exports of other files and the type-only
 * imports taken out (installRefusal and portableVersion need neither). */
export async function loadUpdateTs(source) {
  const own = source
    .split(/\r?\n/)
    .filter((l) => !/^export \{[^}]*\} from "[^"]+";$/.test(l.trim()) && !/^import type /.test(l.trim()))
    .join("\n");
  const dir = mkdtempSync(join(tmpdir(), "sk-compat034-"));
  const file = join(dir, "update034.mjs");
  writeFileSync(file, stripTypeScriptTypes(own, { mode: "strip" }));
  try {
    return await import(pathToFileURL(file).href);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** Whether 0.3.4's main would use `name`, downloaded into its updates folder, for this kind of
 * install (update.ts installRefusal with the file system answered here). */
export function acceptedBy034(mod, name, kind, platform) {
  const path = platform === "win32" ? win32 : posix;
  const updates = platform === "win32" ? "C:\\Users\\lenny\\AppData\\Local\\SpaceKnife\\updates" : "/home/lenny/.local/share/SpaceKnife/updates";
  const probe = { realpath: (p) => p, isFile: () => true };
  return mod.installRefusal(path.join(updates, name), kind, updates, platform, probe);
}

/** What 0.3.4's replacePortable needs in a portable archive (index.ts, update.ts swapHelper). */
export function portableLayoutProblems(name, entries) {
  const problems = [];
  const platform = name.includes("-win-") ? "win32" : name.includes("-mac-") ? "darwin" : "linux";
  // ditto keeps resource forks in __MACOSX/ and puts them back on unpacking: no folder of its own
  const real = entries.filter((e) => !e.name.startsWith("__MACOSX/"));
  const tops = [...new Set(real.map((e) => e.name.split("/")[0]))];
  const folders = tops.filter((t) => real.some((e) => e.name.startsWith(`${t}/`)));
  if (folders.length !== 1) return [`${name}: ${folders.length} folders at the top (${tops.join(", ")}), 0.3.4 wants one`];
  const top = folders[0];
  // the entry, or anything in it (an archive need not list its folders)
  const has = (rel) => real.find((e) => e.name === `${top}/${rel}`) ?? real.find((e) => e.name.startsWith(`${top}/${rel}/`));
  const program = { win32: "SpaceKnife.exe", linux: "spaceknife", darwin: "SpaceKnife.app" }[platform];
  // readdir(staged).includes(program): an exact, case-sensitive name at the top of the folder
  const p = has(program);
  if (!p) problems.push(`${name}: no ${top}/${program} (0.3.4 looks for that exact name)`);
  else if (platform === "linux" && p.mode != null && !(p.mode & 0o100)) problems.push(`${name}: ${program} is not executable (mode ${p.mode.toString(8)})`);
  if (platform === "darwin" && !has("SpaceKnife.app/Contents/MacOS/SpaceKnife")) problems.push(`${name}: no SpaceKnife.app/Contents/MacOS/SpaceKnife (the swap starts it again)`);
  const helper = {
    win32: "resources/backend/spaceknife-backend.exe",
    linux: "resources/backend/spaceknife-backend",
    darwin: "SpaceKnife.app/Contents/Resources/backend/spaceknife-backend",
  }[platform];
  const h = has(helper);
  if (!h) problems.push(`${name}: no ${top}/${helper} (0.3.4 runs it with --apply-update)`);
  else if (platform !== "win32" && h.mode != null && !(h.mode & 0o100)) problems.push(`${name}: ${helper} is not executable (mode ${h.mode.toString(8)})`);
  if (!has("data")) problems.push(`${name}: no data folder (what makes it portable)`);
  return problems;
}

/** Judge the result of tools/compat034_check.py; `files`: name -> path (--dir) or null. */
export function judge(result, version, mod, files) {
  const problems = [];
  if (!result.found) return [`${version} is not in the release list 0.3.4 reads`];
  if (result.offered !== version) problems.push(`0.3.4 (preview channel) offers ${result.offered}, not ${version}`);
  const picked = new Set();
  for (const [kind, system, machine, template] of CONTRACT) {
    const row = result.installs.find((r) => r.kind === kind && r.system === system && r.machine === machine);
    const label = `${kind} on ${system}/${machine}`;
    if (!row) {
      problems.push(`${label}: not checked`);
      continue;
    }
    if (row.template !== template) problems.push(`${label}: 0.3.4's template is ${row.template}, the contract says ${template}`);
    const want = wantName(kind, template, version);
    if (row.asset !== want) {
      problems.push(`${label}: 0.3.4 picks ${row.asset}, want ${want}`);
      continue;
    }
    picked.add(row.asset);
    if (row.error) problems.push(`${label}: 0.3.4 cannot read the checksums: ${row.error}`);
    else if (!row.sha256) problems.push(`${label}: 0.3.4 finds no checksum for ${row.asset} (it reads ${row.sums_order?.join(", ")})`);
    else if (files?.has(row.asset) && sha256(files.get(row.asset)) !== row.sha256) problems.push(`${row.asset}: 0.3.4 reads checksum ${row.sha256}, the file's is another`);
    const refusal = acceptedBy034(mod, row.asset, kind, system);
    if (refusal) problems.push(`${label}: 0.3.4's window refuses ${row.asset} (${refusal})`);
    if (kind === "portable" && mod.portableVersion(row.asset) !== version) problems.push(`${row.asset}: 0.3.4 reads the version ${mod.portableVersion(row.asset)}`);
  }
  for (const e of expected()) {
    if (!e.sums) continue; // the .zsync: AppImageUpdate's, not the app's
    const name = wantName(e.template.includes(".deb") ? "deb" : e.template.includes(".rpm") ? "rpm" : "", e.template, version);
    if (e.since) {
      // a kind of install 0.3.4 did not have (the Tauri app's Linux portable copy): no 0.3.4 copy
      // may take it, in this release or alone in one (compat034_check.py takers: 0.3.4's own
      // pick_asset on a release that has only this file)
      if (picked.has(name)) problems.push(`${name} (new in ${e.since}) is picked by a 0.3.4 install`);
      const takers = result.takers?.[name] ?? [];
      if (takers.length) problems.push(`${name} (new in ${e.since}) would be taken by 0.3.4: ${takers.join(", ")}`);
    } else if (!picked.has(name)) problems.push(`${name} is published but no 0.3.4 install picks it`);
  }
  return problems;
}

function main(argv) {
  const opt = (n) => {
    const i = argv.indexOf(n);
    return i >= 0 ? argv[i + 1] : null;
  };
  const tree = opt("--tree");
  const version = opt("--version");
  const dir = opt("--dir");
  const github = argv.includes("--github");
  if (!tree || !version || (!dir) === !github) {
    console.error("usage: node tools/compat034.mjs --tree V034 [--python PY] --version X.Y.Z (--dir RELEASE | --github)");
    return Promise.resolve(2);
  }
  const python = opt("--python") ?? join(resolve(tree), ".venv", process.platform === "win32" ? "Scripts/python.exe" : "bin/python");
  if (!existsSync(python)) {
    console.error(`no Python environment in ${tree} (uv sync --project ${tree} --frozen --no-default-groups --package spaceknife-backend)`);
    return Promise.resolve(2);
  }
  return (async () => {
    const check = spawnSync(python, ["-I", join(HERE, "compat034_check.py"), "--version", version, ...(dir ? ["--dir", resolve(dir)] : ["--github"])], {
      encoding: "utf8",
      maxBuffer: 16 << 20,
      env: { ...process.env, NO_PROXY: "127.0.0.1,localhost" },
    });
    if (check.status !== 0) {
      console.error(check.stderr || check.stdout);
      console.error("0.3.4's update code did not run");
      return 1;
    }
    const result = JSON.parse(check.stdout);
    const mod = await loadUpdateTs(readFileSync(join(resolve(tree), "apps", "electron", "src", "main", "update.ts"), "utf8"));
    const files = dir ? new Map(result.assets.map((n) => [n, join(resolve(dir), n)])) : null;
    const problems = judge(result, version, mod, files);
    if (files) {
      for (const [name, path] of files) {
        if (!/-portable\.(zip|tar\.gz)$/.test(name)) continue;
        try {
          problems.push(...portableLayoutProblems(name, archiveEntries(readFileSync(path), name)));
        } catch (e) {
          problems.push(`${name}: ${e.message}`);
        }
      }
    }
    for (const r of result.installs) console.log(`${r.kind.padEnd(9)} ${`${r.system}/${r.machine}`.padEnd(15)} ${String(r.asset).padEnd(46)} ${r.sha256 ? r.sha256.slice(0, 16) : r.error ?? "-"}`);
    for (const p of problems) console.log(`PROBLEM ${p}`);
    if (!problems.length) console.log(`${version}: every 0.3.4 install finds its file and checksum, and 0.3.4's window takes it`);
    return problems.length ? 1 : 0;
  })();
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = await main(process.argv.slice(2));
