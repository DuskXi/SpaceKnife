// What the app's bundles carry besides the app itself (tauri.bundle.json; `pnpm bundle` runs this as
// part of beforeBuildCommand, after `pnpm generate`):
//
// - src-tauri/target-bundle/spaceknife-cli-<target triple>[.exe]: the console binary (crates/sk-server), shipped
//   next to the app as Tauri's externalBin (which wants the target triple in the name);
// - src-tauri/target-bundle/licenses/: LICENSE.txt (SpaceKnife, MIT), THIRD-PARTY-rust.txt (the crates linked
//   into spaceknife and spaceknife-cli, from `cargo metadata`) and THIRD-PARTY-ui.txt (the npm
//   packages of the interface: the production dependencies, a superset of what is bundled, and the
//   fonts, OFL-1.1). Identical licence texts are printed once, with the packages that share them.
//
//   node scripts/prepare-bundle.mjs [--no-cli]      (TAURI_ENV_TARGET_TRIPLE / CARGO_TARGET_DIR respected)

import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const desktop = resolve(here, "..");
const root = resolve(desktop, "../..");
// not dist/: Nuxt makes that a link to .output/public, which the app embeds
const out = join(desktop, "src-tauri", "target-bundle");
const RULE = "=".repeat(78);
const LICENSE_FILE = /^(licen[cs]e|copying|notice)/i;
const win = process.platform === "win32";

const run = (cmd, args, opts = {}) => execFileSync(cmd, args, { cwd: root, encoding: "utf8", maxBuffer: 256 << 20, shell: false, ...opts });

function hostTriple() {
  const line = run("rustc", ["-vV"]).split(/\r?\n/).find((l) => l.startsWith("host: "));
  if (!line) throw new Error("rustc -vV says no host triple");
  return line.slice(6).trim();
}

const triple = process.env.TAURI_ENV_TARGET_TRIPLE || hostTriple();
const exe = triple.includes("windows") ? ".exe" : "";

// ------------------------------------------------------------------ the console binary

function buildCli() {
  const args = ["build", "--release", "-p", "sk-server", "--bin", "spaceknife-cli"];
  const explicitTarget = !!process.env.TAURI_ENV_TARGET_TRIPLE && process.env.TAURI_ENV_TARGET_TRIPLE !== hostTriple();
  if (explicitTarget) args.push("--target", triple);
  console.log(`[bundle] cargo ${args.join(" ")}`);
  execFileSync("cargo", args, { cwd: root, stdio: "inherit" });
  const targetDir = process.env.CARGO_TARGET_DIR ? resolve(root, process.env.CARGO_TARGET_DIR) : join(root, "target");
  const built = join(targetDir, ...(explicitTarget ? [triple] : []), "release", `spaceknife-cli${exe}`);
  const dest = join(out, `spaceknife-cli-${triple}${exe}`);
  copyFileSync(built, dest);
  console.log(`[bundle] ${dest}`);
}

// ------------------------------------------------------------------ licences

/** packages: { name, version, license, texts: string[] }[] */
function render(title, packages) {
  const lines = [title, RULE, ""];
  packages.sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
  for (const p of packages) lines.push(`${p.name} ${p.version} — ${p.license || "see licence text"}`);
  const byText = new Map();
  const missing = [];
  for (const p of packages) {
    if (!p.texts.length) missing.push(`${p.name} ${p.version}`);
    for (const t of p.texts) {
      const key = t.replace(/\r\n/g, "\n").trim();
      if (!byText.has(key)) byText.set(key, []);
      byText.get(key).push(`${p.name} ${p.version}`);
    }
  }
  for (const [text, names] of byText) lines.push("", RULE, [...new Set(names)].join(", "), RULE, "", text);
  if (missing.length) lines.push("", RULE, "Without a licence file in the package (see the licence named above):", RULE, "", ...missing);
  return `${lines.join("\n")}\n`;
}

function licenceTexts(dir) {
  let names = [];
  try {
    names = readdirSync(dir, { withFileTypes: true }).filter((e) => e.isFile() && LICENSE_FILE.test(e.name)).map((e) => e.name).sort();
  } catch {
    return [];
  }
  return names.map((n) => readFileSync(join(dir, n), "utf8"));
}

function rustPackages() {
  const meta = JSON.parse(run("cargo", ["metadata", "--format-version", "1", "--locked", "--filter-platform", triple]));
  const byId = new Map(meta.packages.map((p) => [p.id, p]));
  const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
  const start = meta.packages.filter((p) => p.name === "spaceknife-desktop" || p.name === "sk-server").map((p) => p.id);
  const seen = new Set();
  const todo = [...start];
  while (todo.length) {
    const id = todo.pop();
    if (seen.has(id)) continue;
    seen.add(id);
    for (const dep of nodes.get(id)?.deps ?? []) {
      // linked code only: normal dependencies (build scripts and proc macros run at build time)
      if (dep.dep_kinds.some((k) => k.kind === null)) todo.push(dep.pkg);
    }
  }
  const workspace = new Set(meta.workspace_members);
  return [...seen]
    .filter((id) => !workspace.has(id))
    .map((id) => byId.get(id))
    .filter((p) => p && !p.targets.every((t) => t.kind.includes("proc-macro")))
    .map((p) => ({ name: p.name, version: p.version, license: p.license ?? "", texts: licenceTexts(dirname(p.manifest_path)) }));
}

function npmPackages() {
  // pnpm is a .cmd on Windows: through cmd.exe (fixed arguments)
  const args = ["licenses", "list", "--prod", "--json"];
  const text = win ? run("cmd.exe", ["/d", "/s", "/c", `pnpm ${args.join(" ")}`], { cwd: desktop }) : run("pnpm", args, { cwd: desktop });
  const list = JSON.parse(text);
  const packages = [];
  for (const [license, entries] of Object.entries(list)) {
    for (const e of entries) {
      e.versions.forEach((version, i) => {
        packages.push({ name: e.name, version, license, texts: licenceTexts(e.paths[i] ?? e.paths[0]) });
      });
    }
  }
  return packages;
}

function licences() {
  const dir = join(out, "licenses");
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  copyFileSync(join(root, "LICENSE"), join(dir, "LICENSE.txt"));
  writeFileSync(join(dir, "THIRD-PARTY-rust.txt"), render("Rust crates linked into spaceknife and spaceknife-cli", rustPackages()));
  writeFileSync(join(dir, "THIRD-PARTY-ui.txt"), render("JavaScript packages and fonts of the interface (production dependencies)", npmPackages()));
  console.log(`[bundle] ${dir}`);
}

mkdirSync(out, { recursive: true });
if (!process.argv.includes("--no-cli")) buildCli();
licences();
if (!existsSync(join(out, `spaceknife-cli-${triple}${exe}`))) console.warn(`[bundle] no spaceknife-cli-${triple}${exe} yet (--no-cli)`);
