// The Tauri app's deb, repacked as the package 0.3.4 users have (the Release workflow runs it on
// the deb `pnpm bundle` makes; Linux, dpkg-deb):
//
//   node tools/deb.mjs IN.deb OUT.deb
//
// Tauri's bundler names the package after the product, in kebab case ("space-knife"), and has no
// option for it. Installed over 0.3.4's "spaceknife" that would be a second package with the same
// /usr/bin/spaceknife; under 0.3.4's name it is an ordinary upgrade, which removes 0.3.4's
// /opt/SpaceKnife. What changes:
//
// - control: Package spaceknife; Conflicts / Replaces space-knife (a test build under Tauri's name
//   goes); the rest as Tauri wrote it.
// - the desktop entry: /usr/share/applications/spaceknife.desktop, the name 0.3.4's had (launchers
//   pinned to it keep working), with Tauri's Exec, Icon and StartupWMClass and 0.3.4's
//   GenericName, Chinese texts and Keywords; Tauri's own SpaceKnife.desktop goes.
// - /usr/share/doc/space-knife becomes /usr/share/doc/spaceknife.
// - postinst: 0.3.4's postinst made /usr/bin/spaceknife an alternative of /opt/SpaceKnife/spaceknife
//   (update-alternatives); 0.3.4's postrm takes that out when it is upgraded, after this package's
//   files are unpacked, and update-alternatives leaves the regular file this package puts at
//   /usr/bin/spaceknife alone. Should the registration still be there, postinst removes it (the
//   program stays).
// - md5sums written again for the files as they are now.

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, existsSync, lstatSync, mkdtempSync, readdirSync, readFileSync, renameSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const PACKAGE = "spaceknife";
const TAURI_PACKAGE = "space-knife";
export const DESKTOP = "spaceknife.desktop";

export class DebError extends Error {}

/** Tauri's control file with 0.3.4's package name. */
export function controlFor(text) {
  const fields = [];
  for (const line of text.replace(/\r\n/g, "\n").split("\n")) {
    if (!line) continue;
    if (/^\s/.test(line) && fields.length) fields[fields.length - 1][1] += `\n${line}`;
    else {
      const m = /^([A-Za-z0-9-]+):\s?(.*)$/.exec(line);
      if (!m) throw new DebError(`not a control field: ${JSON.stringify(line)}`);
      fields.push([m[1], m[2]]);
    }
  }
  const get = (k) => fields.find(([f]) => f.toLowerCase() === k.toLowerCase());
  const pkg = get("Package");
  if (!pkg) throw new DebError("the control file names no package");
  if (pkg[1] !== TAURI_PACKAGE && pkg[1] !== PACKAGE) throw new DebError(`unexpected package ${pkg[1]}`);
  pkg[1] = PACKAGE;
  for (const key of ["Conflicts", "Replaces"]) {
    const f = get(key);
    const list = f ? f[1].split(",").map((s) => s.trim()).filter(Boolean) : [];
    if (!list.some((d) => d.split(/[\s(]/)[0] === TAURI_PACKAGE)) list.push(TAURI_PACKAGE);
    if (f) f[1] = list.join(", ");
    else fields.splice(fields.indexOf(get("Depends") ?? pkg) + 1, 0, [key, list.join(", ")]);
  }
  return `${fields.map(([k, v]) => `${k}: ${v}`).join("\n")}\n`;
}

/** The desktop entry: what Tauri's says to start (Exec, Icon, StartupWMClass), with 0.3.4's texts. */
export function desktopEntry(tauri) {
  const value = (key) => new RegExp(`^${key}=(.*)$`, "m").exec(tauri.replace(/\r\n/g, "\n"))?.[1];
  const exec = value("Exec");
  if (!exec) throw new DebError("Tauri's desktop entry has no Exec");
  const lines = [
    "[Desktop Entry]",
    "Type=Application",
    "Name=SpaceKnife",
    "GenericName=Disk Space Analyzer",
    "GenericName[zh_CN]=磁盘空间分析",
    "Comment=See what fills your drives — a live treemap with filters, snapshot comparison and cleanup",
    "Comment[zh_CN]=看清是什么占满了磁盘：实时树图、过滤、快照对比与清理",
    "Keywords=disk;space;usage;treemap;storage;cleanup;磁盘;空间;",
    `Exec=${exec}`,
    `Icon=${value("Icon") ?? PACKAGE}`,
    `StartupWMClass=${value("StartupWMClass") ?? PACKAGE}`,
    "Terminal=false",
    "Categories=Utility;",
  ];
  return `${lines.join("\n")}\n`;
}

export const POSTINST = `#!/bin/sh
# spaceknife: 0.3.4's package made /usr/bin/spaceknife an alternative of /opt/SpaceKnife/spaceknife;
# its postrm takes that out when it is upgraded. Should it still be registered, take it out here:
# update-alternatives leaves /usr/bin/spaceknife alone, a regular file of this package.
set -e
if [ "$1" = configure ] && command -v update-alternatives >/dev/null 2>&1; then
  if update-alternatives --list spaceknife 2>/dev/null | grep -qx /opt/SpaceKnife/spaceknife; then
    update-alternatives --remove spaceknife /opt/SpaceKnife/spaceknife || true
  fi
fi
exit 0
`;

/** md5sums for every regular file under `root` but DEBIAN, sorted, as dpkg-deb lists them. */
export function md5sums(root) {
  const out = [];
  const walk = (dir) => {
    for (const name of readdirSync(dir).sort()) {
      const p = join(dir, name);
      const st = lstatSync(p);
      if (st.isDirectory()) {
        if (p !== join(root, "DEBIAN")) walk(p);
      } else if (st.isFile()) {
        out.push(`${createHash("md5").update(readFileSync(p)).digest("hex")}  ${relative(root, p).replaceAll("\\", "/")}`);
      }
    }
  };
  walk(root);
  return `${out.join("\n")}\n`;
}

/** Repack `input` (Tauri's deb) as `output`. */
export function repack(input, output) {
  const work = mkdtempSync(join(tmpdir(), "sk-deb-"));
  const root = join(work, "root");
  try {
    execFileSync("dpkg-deb", ["-R", input, root], { stdio: "inherit" });
    const control = join(root, "DEBIAN", "control");
    writeFileSync(control, controlFor(readFileSync(control, "utf8")));
    const apps = join(root, "usr", "share", "applications");
    const entries = existsSync(apps) ? readdirSync(apps).filter((n) => n.endsWith(".desktop")) : [];
    if (entries.length !== 1) throw new DebError(`Tauri's deb has ${entries.length} desktop entries: ${entries.join(", ")}`);
    const tauri = readFileSync(join(apps, entries[0]), "utf8");
    unlinkSync(join(apps, entries[0]));
    writeFileSync(join(apps, DESKTOP), desktopEntry(tauri));
    const doc = join(root, "usr", "share", "doc");
    if (existsSync(join(doc, TAURI_PACKAGE))) renameSync(join(doc, TAURI_PACKAGE), join(doc, PACKAGE));
    for (const s of ["preinst", "prerm", "postrm"]) {
      if (existsSync(join(root, "DEBIAN", s))) throw new DebError(`Tauri's deb has a ${s} now: look at it before repacking`);
    }
    const postinst = join(root, "DEBIAN", "postinst");
    if (existsSync(postinst)) throw new DebError("Tauri's deb has a postinst now: look at it before repacking");
    writeFileSync(postinst, POSTINST);
    chmodSync(postinst, 0o755);
    writeFileSync(join(root, "DEBIAN", "md5sums"), md5sums(root));
    rmSync(output, { force: true });
    // gzip: every dpkg reads it, and so do the package checks (tools/package_checks.mjs)
    execFileSync("dpkg-deb", ["--root-owner-group", "-Zgzip", "-z9", "--build", root, output], { stdio: "inherit" });
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

function main(argv) {
  const [input, output] = argv;
  if (!input || !output || !existsSync(input)) {
    console.error("usage: node tools/deb.mjs IN.deb OUT.deb");
    return 2;
  }
  try {
    repack(resolve(input), resolve(output));
  } catch (e) {
    if (!(e instanceof DebError)) throw e;
    console.error(`deb: ${e.message}`);
    return 1;
  }
  console.log(`[deb] ${output}`);
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = main(process.argv.slice(2));
