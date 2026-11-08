// The Windows installer's own logic (apps/desktop/src-tauri/windows/spaceknife.nsh), run for real
// without installing anything: packaging/nsis-logic/logic-test.nsi is compiled with the NSIS of
// Tauri's bundler and run once per case — 0.3.4's and Tauri's command lines (what each flag turns
// on, the elevated copy's arguments), the folder guard (never the data folder, never a system
// folder), and the staging of the new files (SkMakeStage / SkCheckStaged / SkPlaceAll /
// SkDropStage) with real files in a temporary folder: a new folder renamed into place in one go,
// an update in place file by file (a stale interface file gone, a file of the user's kept), an
// incomplete staging that stops with nothing changed, a held file that is reported, a parent that
// takes no folder (staged elsewhere and copied), a leftover staging folder, a folder that is not
// a staging folder never removed. Windows only; it writes in a temporary folder only.
//
//   node packaging/nsis-logic.mjs [--makensis PATH] [--utils PATH] [--helpers PATH]
//
// makensis: --makensis, $MAKENSIS, target/.tauri/NSIS (bundle > useLocalToolsDir) or the bundler's
// %LOCALAPPDATA%\tauri\NSIS. utils.nsh: --utils, else the one `pnpm bundle` wrote next to the
// rendered installer (target/release/nsis/x64). Exit status 1 when a case says something else.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const args = process.argv.slice(2);
const option = (name) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : null;
};

if (process.platform !== "win32") {
  console.error("the installer's logic test runs on Windows (NSIS's own runtime)");
  process.exit(1);
}

// makensis wants Windows paths (backslashes) in its !include and !addplugindir
const first = (...paths) => {
  const p = paths.find((p) => p && existsSync(p));
  return p ? resolve(p) : null;
};
const targetDir = process.env.CARGO_TARGET_DIR ? resolve(root, process.env.CARGO_TARGET_DIR) : join(root, "target");
const makensis = first(option("--makensis"), process.env.MAKENSIS, join(targetDir, ".tauri", "NSIS", "makensis.exe"), process.env.LOCALAPPDATA && join(process.env.LOCALAPPDATA, "tauri", "NSIS", "makensis.exe"));
const utils = first(option("--utils"), join(targetDir, "release", "nsis", "x64", "utils.nsh"));
const helpers = resolve(option("--helpers") ?? join(root, "apps", "desktop", "src-tauri", "windows", "spaceknife.nsh"));
if (!makensis) fail("no makensis: pass --makensis, or run `pnpm bundle` (Tauri's bundler downloads NSIS) first");
if (!utils) fail("no Tauri utils.nsh: pass --utils, or run `pnpm bundle` first (it writes target/release/nsis/x64/utils.nsh)");
const plugins = join(dirname(makensis), "Plugins", "x86-unicode", "additional");

function fail(message) {
  console.error(`FAIL ${message}`);
  process.exit(1);
}

const sandbox = mkdtempSync(join(tmpdir(), "sk-nsis-logic-"));
const exe = join(sandbox, "logic-test.exe");
let failures = 0;
try {
  // ------------------------------------------------------------------ compile
  const env = { ...process.env };
  delete env.NSISDIR;
  delete env.NSISCONFDIR;
  const c = spawnSync(makensis, ["-INPUTCHARSET", "UTF8", "-V2", `-DSK_HELPERS=${helpers}`, `-DSK_UTILS=${utils}`, `-DSK_PLUGINS=${plugins}`, `-DSK_OUT=${exe}`, join(here, "nsis-logic", "logic-test.nsi")], { encoding: "utf8", env });
  if (c.status !== 0 || !existsSync(exe)) fail(`makensis (${makensis}): exit ${c.status}\n${c.stdout}\n${c.stderr}`);
  console.log(`compiled with ${makensis}: ${helpers}`);

  // ------------------------------------------------------------------ run
  const temp = join(sandbox, "temp");
  mkdirSync(temp);
  let n = 0;
  /** One run: the case, its command line (as the process gets it, verbatim) -> Map key -> value. */
  function run(kind, cmdline = "", extra = {}) {
    const out = join(sandbox, `out-${++n}.txt`);
    const box = join(sandbox, `box-${n}`);
    mkdirSync(box);
    const r = spawnSync(exe, cmdline ? [cmdline] : [], {
      windowsVerbatimArguments: true,
      encoding: "utf8",
      timeout: 60_000,
      env: { ...process.env, TMP: temp, TEMP: temp, SK_LOGIC_CASE: kind, SK_LOGIC_OUT: out, SK_LOGIC_SANDBOX: box, ...extra },
    });
    if (!existsSync(out)) throw new Error(`${kind} ${cmdline}: no output (exit ${r.status} ${r.error ?? ""})`);
    const text = readFileSync(out).toString("utf16le").replace(/^\uFEFF/, "");
    const got = new Map();
    for (const line of text.split(/\r?\n/).filter(Boolean)) {
      const at = line.startsWith("dir ") ? line.lastIndexOf("=") : line.indexOf("=");
      got.set(line.slice(0, at), line.slice(at + 1));
    }
    return { got, box };
  }
  function expect(what, got, want) {
    const wrong = Object.entries(want).filter(([k, v]) => (v instanceof RegExp ? !v.test(got.get(k) ?? "") : got.get(k) !== v));
    if (wrong.length) {
      failures++;
      console.log(`not ok - ${what}`);
      for (const [k, v] of wrong) console.log(`    ${k}: ${JSON.stringify(got.get(k))}, wanted ${v instanceof RegExp ? v : JSON.stringify(v)}`);
    } else console.log(`ok - ${what}`);
  }

  // 0.3.4's flags (v0.3.4 update.ts nsisUpdateArgs, its uninstaller's) and Tauri's; the elevated
  // copy gets the same ones, its own flag and the scope, /D= last
  const none = { passive: "0", update: "0", noshortcut: "0", run: "0", runargs: "", scope: "", forced: "0", inner: "0", keepshortcuts: "0", deletedata: "0" };
  const flags = [
    ["--updated /S --force-run /currentuser", { update: "1", run: "1", runargs: "--updated", scope: "user", forced: "1", keepshortcuts: "1", flag: "/currentuser", elevated: "--updated /S --force-run /currentuser --sk-elevated /currentuser" }],
    ["--updated /S --force-run /allusers", { update: "1", run: "1", runargs: "--updated", scope: "machine", forced: "1", keepshortcuts: "1", flag: "/allusers", elevated: "--updated /S --force-run /allusers --sk-elevated /allusers" }],
    ["--updated /S --force-run", { update: "1", run: "1", runargs: "--updated", keepshortcuts: "1", elevated: /^--updated \/S --force-run --sk-elevated \/(currentuser|allusers)$/ }],
    ["/S /UPDATE /R /ARGS --updated", { update: "1", run: "1", runargs: "--updated", keepshortcuts: "1" }],
    ["/S /R /ARGS --minimized", { run: "1", runargs: "--minimized" }],
    ["/S /P /NS", { passive: "1", noshortcut: "1" }],
    ["/S /KEEP_APP_DATA /currentuser --keep-shortcuts --updated _?=C:\\Program Files\\Space Knife", { update: "1", runargs: "--updated", scope: "user", forced: "1", keepshortcuts: "1", flag: "/currentuser" }],
    ["/S --delete-app-data", { deletedata: "1" }],
    ["/S --updated --delete-app-data", { update: "1", runargs: "--updated", keepshortcuts: "1", deletedata: "0" }],
    ["/S /R --updated --no-desktop-shortcut", { update: "1", noshortcut: "1", run: "1", runargs: "--updated", keepshortcuts: "1" }],
    ["/s --UPDATED /r /AllUsers /D=C:\\Some Dir\\SpaceKnife", { update: "1", run: "1", runargs: "--updated", scope: "machine", forced: "1", keepshortcuts: "1", elevated: "/s --UPDATED /r /AllUsers --sk-elevated /allusers /D=C:\\Some Dir\\SpaceKnife" }],
    ["/S --sk-elevated /allusers", { scope: "machine", forced: "1", inner: "1" }],
  ];
  for (const [cmdline, want] of flags) {
    const { got } = run("flags", cmdline);
    expect(`flags: ${cmdline}`, got, { ...none, ...want });
  }
  console.log(`(the registry here: ${run("flags", "/S").got.get("previous")})`);

  // the folder guard (SkUnsafeDir): the data folder and the shell's folder, what holds them or
  // lies in them, and the system's folders are never a place to install into
  const { got: dirs } = run("dirs");
  const local = dirs.get("localappdata");
  const pf = dirs.get("programfiles64");
  const appdata = dirs.get("appdata");
  const want = {
    [`${local}\\SpaceKnife`]: "data",
    [`${local}\\SpaceKnife\\`]: "data",
    [`${local}\\spaceknife`]: "data",
    [`${local}\\SpaceKnife\\app`]: "data",
    [`${local}\\SpaceKnifeX`]: "",
    [`${local}\\io.github.duskxi.spaceknife`]: "data",
    [`${appdata}\\SpaceKnife\\electron`]: "data",
    [local]: "data",
    [`${local}\\Programs`]: "system",
    [`${local}\\Programs\\SpaceKnife`]: "",
    [`${local}\\Programs\\..\\SpaceKnife`]: "data",
    [pf]: "system",
    [`${pf}\\SpaceKnife`]: "",
    ["C:\\"]: "system",
    ["C:"]: "system",
    ["D:\\Apps\\SpaceKnife"]: "",
  };
  expect("the folder guard", dirs, Object.fromEntries(Object.entries(want).map(([k, v]) => [`dir ${k}`, v])));
  const sys = [...dirs].filter(([k]) => /^dir .*(\\Windows\\SpaceKnife|\\Users\\[^\\]+)$/i.test(k));
  expect("the profile and Windows", new Map(sys.map(([k, v]) => [k.replace(/^dir .*\\Windows\\SpaceKnife$/i, "windows").replace(/^dir .*\\Users\\[^\\]+$/i, "profile"), v])), { windows: "system", profile: "data" });

  // the staging (installer.nsi steps 1 and 6)
  const placed = { missing: "", failed: "", exe: "new SpaceKnife.exe", entry: "new ui\\_nuxt\\entry.js", com: "new SpaceKnife.com", stale: "0", staging_left: "0", stage: "" };
  {
    const { got, box } = run("fresh");
    expect("a new folder: staged beside it, renamed into place in one go", got, { ...placed, staged_in: `${box}\\Programs\\SpaceKnife.sk-staging`, made: "1", move: "1", user: "<none>" });
  }
  {
    const { got } = run("leftover");
    expect("a staging folder an earlier run left is emptied first", got, { ...placed, made: "1", leftover: "0" });
  }
  {
    const { got } = run("missing", "", { SK_LOGIC_SKIP: "ui\\_nuxt\\entry.js" });
    expect("an incomplete staging stops with nothing changed (and is removed)", got, { missing: "ui\\_nuxt\\entry.js", exe: "<none>", entry: "<none>", staging_left: "0", stage: "" });
  }
  {
    const { got } = run("inplace");
    expect("an update in place: file by file, the stale interface gone, the user's file kept", got, { ...placed, move: "1", user: "the user's" });
  }
  {
    const { got } = run("locked");
    expect("a file held by another program is reported (the rest goes in)", got, { failed: "SpaceKnife.com", com: "old com", exe: "new SpaceKnife.exe", entry: "new ui\\_nuxt\\entry.js", user: "the user's", staging_left: "0", stage: "" });
  }
  {
    const { got } = run("fallback");
    expect("no folder beside it: staged in the plug-ins folder and copied", got, { ...placed, staged_in: /\\temp\\ns[^\\]+\.tmp\\stage$/i, made: "1", move: "0", user: "<none>", staging_left: "1", blocker: "not a folder" });
  }
  {
    const { got } = run("dropguard");
    expect("SkDropStage leaves a folder it did not make", got, { kept: "1" });
  }
} catch (e) {
  failures++;
  console.log(`not ok - ${e.message}`);
} finally {
  rmSync(sandbox, { recursive: true, force: true, maxRetries: 10, retryDelay: 300 });
}
console.log(failures ? `\nthe installer's logic: ${failures} case(s) failed` : "\nthe installer's logic: ok");
process.exit(failures ? 1 : 0);
