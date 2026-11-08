// The Windows installer's own logic (apps/desktop/src-tauri/windows/spaceknife.nsh), run for real
// without installing anything: packaging/nsis-logic/logic-test.nsi is compiled with the NSIS of
// Tauri's bundler and run once per case — 0.3.4's and Tauri's command lines (what each flag turns
// on, the elevated copy's arguments), the folder guard (never the data folder, never a system
// folder), the folder's name (\SpaceKnife added as 0.3.4 did) and choice (/D= in either scope),
// and the staging of the new files (SkMakeStage / SkCheckStaged / SkPlaceAll / SkDropStage) with
// real files in a temporary folder: a new folder renamed into place in one go (and kept as it is
// when a file went missing after that), an update in place file by file (the files the earlier
// build listed gone, the user's kept), an incomplete staging that stops with nothing changed, a
// held file that is reported, a parent that takes no folder (staged elsewhere and copied), a
// leftover staging folder (emptied by name only), a folder that is not a staging folder never
// removed; the list of installed files (SkWriteManifest / SkRemoveListed: nothing off the list,
// nothing outside the folder), 0.3.4's own files (SkRemove034Files) and when its uninstaller may
// run (SkOldUninstallerUse), the WebView2 minimum (SkWebView2Minimum, with stand-ins for reading
// and updating the runtime), shortcuts (.lnk files in the sandbox: SkMigrateShortcut's target and
// AppUserModelID), what the uninstaller removes of SpaceKnife's own folders (un.SkRemoveData, in
// the test program's own uninstaller, with a Local and a Roaming folder in the sandbox: 0.3.4's
// rules for an update, the 「你的数据」 box and --delete-app-data) — and that the app the installer
// starts gets its environment (cmd.exe stands in for it: on your own desktop run this from
// packaging/hidden-desktop.ps1, so its console window stays out of sight). Windows only; it writes
// in a temporary folder only, never the registry.
//
//   node packaging/nsis-logic.mjs [--makensis PATH] [--utils PATH] [--helpers PATH]
//
// makensis: --makensis, $MAKENSIS, target/.tauri/NSIS (bundle > useLocalToolsDir) or the bundler's
// %LOCALAPPDATA%\tauri\NSIS. utils.nsh: --utils, else the one `pnpm bundle` wrote next to the
// rendered installer (target/release/nsis/x64). Exit status 1 when a case says something else.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync } from "node:fs";
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

// by its long path: a CI runner's %TEMP% is C:\Users\RUNNER~1\…, and a shell link names its target
// by the long path whatever form it was made with (IShellLink::GetPath), while IsShortcutTarget
// compares the strings. The installer's own paths are long ones: $INSTDIR from the shell's folders
// or the uninstall entry, the shortcuts' targets as 0.3.4 made them from that same $INSTDIR.
const sandbox = realpathSync.native(mkdtempSync(join(tmpdir(), "sk-nsis-logic-")));
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
  function run(kind, cmdline = "", extra = {}, program = exe) {
    const out = join(sandbox, `out-${++n}.txt`);
    const box = join(sandbox, `box-${n}`);
    mkdirSync(box);
    const r = spawnSync(program, cmdline ? [cmdline] : [], {
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
  /** `why`: keys of `got` that say why (HRESULTs and the like), printed when the case fails. */
  function expect(what, got, want, why = []) {
    const wrong = Object.entries(want).filter(([k, v]) => (v instanceof RegExp ? !v.test(got.get(k) ?? "") : got.get(k) !== v));
    if (wrong.length) {
      failures++;
      console.log(`not ok - ${what}`);
      for (const [k, v] of wrong) console.log(`    ${k}: ${JSON.stringify(got.get(k))}, wanted ${v instanceof RegExp ? v : JSON.stringify(v)}`);
      for (const k of why) console.log(`    ${k}: ${got.get(k) ?? "(not reported)"}`);
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
    // emptied by the names this build stages, never as a whole: one with other names is not this
    // build's to delete, it stays and the files are staged in the plug-ins folder instead
    const { got } = run("leftover");
    expect("a staging folder another build left is kept (staged elsewhere)", got, { ...placed, staged_in: /\\temp\\ns[^\\]+\.tmp\\stage$/i, made: "1", move: "0", leftover: "0", leftover_kept: "1", staging_left: "1" });
  }
  {
    const { got, box } = run("leftover-known");
    expect("a staging folder this build left is emptied and used", got, { ...placed, staged_in: `${box}\\Programs\\SpaceKnife.sk-staging`, made: "1", move: "1", leftover: "0" });
  }
  {
    // the folder was renamed into place whole, then the program is missing (a scanner took it):
    // what is there stays, nothing falls back to the file by file placement (which deleted it)
    const { got } = run("vanished");
    expect("a whole-folder rename never falls back", got, { failed: "SpaceKnife.exe", exe: "<none>", entry: "new ui\\_nuxt\\entry.js", com: "new SpaceKnife.com", instdir: "1", stage: "", staging_left: "0" });
  }
  {
    const { got } = run("held");
    expect("an installed file stays when its staged one went missing", got, { failed: "SpaceKnife.com", com: "old com", exe: "new SpaceKnife.exe", entry: "new ui\\_nuxt\\entry.js", user: "the user's", stage: "", staging_left: "0" });
  }
  {
    // .lnk files in the sandbox: the one of the earlier folder points here and gets the
    // AppUserModelID too (the taskbar groups the running app with it), the one already pointing
    // here gets it, another program's is left alone, a missing one is not made
    const { got, box } = run("shortcuts");
    const aumid = (lnk) => (readFileSync(join(box, lnk)).includes(Buffer.from("io.github.duskxi.spaceknife", "utf16le")) ? "1" : "0");
    expect(
      "SkMigrateShortcut",
      new Map([...got, ["moved_aumid", aumid("moved.lnk")], ["here_aumid", aumid("here.lnk")], ["other_aumid", aumid("other.lnk")]]),
      {
        moved: "1",
        moved_aumid: "1",
        here: "1",
        here_aumid: "1",
        other: "1",
        other_aumid: "0",
        gone: "0",
      },
      ["moved", "here", "other"].flatMap((lnk) => [`${lnk}_why`, `${lnk}_want`]),
    );
  }
  {
    // 0.3.4 in a folder it shared (its uninstaller can't be used there): exactly the files the
    // real 0.3.4 installer put there go, and the folders they leave empty
    const { got } = run("remove034");
    expect("SkRemove034Files deletes 0.3.4's files only", got, { exe: "0", uninstaller: "0", asar: "0", backend: "0", dll: "0", license: "0", licenses: "0", locales: "0", mytool: "1", notes: "1", keep: "1", folder: "1" });
  }
  {
    // a folder taken on the directory page without "SpaceKnife" in it gets \SpaceKnife (0.3.4's
    // rule); after the scope page, /D= wins over the default folder in either scope, an earlier
    // installation's folder over both
    const { got } = run("folders");
    expect("SkAppendName and SkChooseFolder", got, {
      "append D:\\Tools": "D:\\Tools\\SpaceKnife",
      "append D:\\Tools\\": "D:\\Tools\\SpaceKnife",
      "append D:\\": "D:\\SpaceKnife",
      "append C:\\Program Files": "C:\\Program Files\\SpaceKnife",
      "append D:\\Apps\\SpaceKnife": "D:\\Apps\\SpaceKnife",
      "append D:\\apps\\spaceknife": "D:\\apps\\spaceknife",
      "append D:\\SpaceKnife-tools\\bin": "D:\\SpaceKnife-tools\\bin",
      "choose user||D:\\Mine\\SpaceKnife": "D:\\Mine\\SpaceKnife",
      "choose machine||D:\\Mine\\SpaceKnife": "D:\\Mine\\SpaceKnife",
      "choose user|D:\\Old\\SpaceKnife|D:\\Mine\\SpaceKnife": "D:\\Old\\SpaceKnife",
      "choose user||": /\\Programs\\SpaceKnife$/i,
      "choose machine||": `${pf}\\SpaceKnife`,
    });
  }
  {
    // the list an installation wrote: exactly its files go, then its folders that are empty; a
    // line that would reach outside the folder, a wildcard, a / or an empty line does nothing
    const { got } = run("listed");
    expect("SkRemoveListed", got, { exe: "0", index: "0", old: "0", nuxt: "0", user: "1", ui: "1", licenses: "0", keep: "1", victim: "1", victim2: "1", list: "0", folder: "1", nolist: "1", kept_r0: /\\Programs\\SpaceKnife$/ });
  }
  {
    const { got, box } = run("manifest");
    const written = readFileSync(join(box, "written.txt"));
    const lines = written.toString("utf16le").replace(/^\uFEFF/, "").split("\r\n");
    const want = ["SpaceKnife.exe", "SpaceKnife.com", "spaceknife-cli.exe", "ui\\index.html", "ui\\_nuxt\\entry.js", "licenses\\LICENSE.txt", "ui\\_nuxt\\", "licenses\\", "ui\\", ""];
    const listOk = written[0] === 0xff && written[1] === 0xfe && JSON.stringify(lines) === JSON.stringify(want) ? "1" : JSON.stringify(lines);
    expect("SkWriteManifest, then SkRemoveListed with it", new Map([...got, ["list_written", listOk]]), { list_written: "1", exe: "0", entry: "0", nuxt: "0", licenses: "0", user: "1", list: "0" });
  }
  {
    // 0.3.4's uninstaller ends with RMDir /r on its folder: only where that folder is SpaceKnife's
    const { got, box } = run("olduninst");
    expect("SkOldUninstallerUse", got, {
      [`old ${box}\\Apps\\SpaceKnife`]: "",
      [`old ${box}\\Tools`]: "unnamed",
      [`old ${box}\\Gone\\SpaceKnife`]: "missing",
      [`old ${local}\\SpaceKnife`]: "unsafe data",
    });
  }
  for (const [runtime, short, updates, what] of [
    ["142.0.3595.80", "", "0", "a new enough WebView2 is left alone"],
    ["89.0.774.44", "", "0", "the minimum itself is enough"],
    ["88.0.705.81|142.0.3595.80", "", "1", "an older one is updated, then it is enough"],
    ["88.0.705.81|88.0.705.81", "88.0.705.81", "1", "an older one that stays older stops the installer"],
    ["89.0.774.9|89.0.774.9", "89.0.774.9", "1", "89.0.774.9 is older than 89.0.774.44"],
  ]) {
    const { got } = run("webview2", "", { SK_LOGIC_WV2: runtime });
    expect(`SkWebView2Minimum: ${what}`, got, { short, updates });
  }
  {
    const { got } = run("missing", "", { SK_LOGIC_SKIP: "ui\\_nuxt\\entry.js" });
    expect("an incomplete staging stops with nothing changed (and is removed)", got, { missing: "ui\\_nuxt\\entry.js", exe: "<none>", entry: "<none>", staging_left: "0", stage: "" });
  }
  {
    // the earlier build's files by its list (installer.nsi step 5): the stale interface file it
    // listed goes, the user's files stay (one in the root, one in its ui folder)
    const { got } = run("inplace");
    expect("an update in place: file by file, the stale interface gone, the user's files kept", got, { ...placed, move: "1", user: "the user's", user_ui: "1", old_list: "0" });
  }
  {
    const { got } = run("locked");
    expect("a file held by another program is reported (the rest goes in)", got, { failed: "SpaceKnife.com", com: "old com", exe: "new SpaceKnife.exe", entry: "new ui\\_nuxt\\entry.js", user: "the user's", user_ui: "1", staging_left: "0", stage: "" });
  }
  {
    const { got } = run("fallback");
    expect("no folder beside it: staged in the plug-ins folder and copied", got, { ...placed, staged_in: /\\temp\\ns[^\\]+\.tmp\\stage$/i, made: "1", move: "0", user: "<none>", staging_left: "1", blocker: "not a folder" });
  }
  {
    const { got } = run("dropguard");
    expect("SkDropStage leaves a folder it did not make", got, { kept: "1" });
  }
  {
    // What the uninstaller removes of SpaceKnife's own folders (un.SkRemoveData), by the rules of
    // 0.3.4's uninstaller (v0.3.4 apps/electron/build/installer.nsh, and electron-builder's own
    // --delete-app-data part): run in this program's uninstaller, in the sandbox's Local and
    // Roaming folders. It deletes folders, so only with a helpers file whose un.SkRemoveData names
    // no folder of the user's: SK_LOCALAPPDATA and SK_APPDATA, which logic-test.nsi points here.
    const source = readFileSync(helpers, "utf8");
    const fn = /^Function un\.SkRemoveData\r?\n([\s\S]*?)^FunctionEnd/m.exec(source);
    const code = fn ? fn[1].split(/\r?\n/).filter((l) => !/^\s*;/.test(l)).join("\n") : "";
    if (!fn || /\$(LOCALAPPDATA|APPDATA|PROFILE|TEMP)\b/i.test(code) || !/^!ifndef SK_LOCALAPPDATA$/m.test(source) || !/^!ifndef SK_APPDATA$/m.test(source)) {
      throw new Error(`${helpers}: un.SkRemoveData does not work in SK_LOCALAPPDATA and SK_APPDATA only; not run (it would delete the user's own folders)`);
    }
    const uninstaller = join(sandbox, "logic-un.exe");
    const { got: written } = run("uninstaller", "", { SK_LOGIC_UNINSTALLER: uninstaller });
    if (written.get("written") !== "1" || !existsSync(uninstaller)) throw new Error("the test program wrote no uninstaller");
    const kept = { updater: "1", cache: "1", updates: "1", snapshots: "1", settings: "1", local: "1", shell_local: "1", electron: "1", roaming: "1", roaming_other: "1", electron_old: "1", shell_roaming: "1", neighbours: "1", neighbours_roaming: "1" };
    // an uninstall: electron-builder's copy of its installer, 0.3.4's Chromium profiles, the
    // shell's folders, the backend's cache and updates; %APPDATA%\SpaceKnife only when empty
    const caches = { ...kept, updater: "0", cache: "0", updates: "0", shell_local: "0", electron: "0", electron_old: "0", shell_roaming: "0" };
    const data = { snapshots: "0", settings: "0", local: "0" };
    const roaming = { roaming: "0", roaming_other: "0" };
    for (const [what, cmdline, env, want] of [
      ["an uninstall: the caches go, %APPDATA%\\SpaceKnife with something else in it stays", "/S", { SK_LOGIC_ROAMING_OTHER: "1" }, caches],
      ["an uninstall: an empty %APPDATA%\\SpaceKnife goes", "/S", {}, { ...caches, ...roaming }],
      ["the box: an empty %APPDATA%\\SpaceKnife goes", "/S", { SK_LOGIC_BOX: "1" }, { ...caches, ...data, ...roaming }],
      ["--delete-app-data: the data and %APPDATA%\\SpaceKnife as a whole", "/S --delete-app-data", { SK_LOGIC_ROAMING_OTHER: "1" }, { ...caches, ...data, ...roaming }],
    ]) {
      const { got } = run("removedata", `${cmdline} _?=${sandbox}\\Programs\\SpaceKnife`, env, uninstaller);
      expect(`un.SkRemoveData: ${what}`, got, want, ["error", "update", "deletedata"]);
    }
  }
  {
    // the app the installer starts after an update gets the installer's environment (0.3.4's
    // update test passes its ready file and release list so), unless the installer runs elevated
    // through UAC: then it starts as the desktop user, with that user's own environment
    const mark = `sk-logic-${process.pid}-${Date.now()}`;
    const { got } = run("start", "", { SK_LOGIC_MARK: mark });
    const elevated = got.get("elevation") === "2";
    expect(`SkStartApp (elevation type ${got.get("elevation")})`, got, elevated ? { started: /./ } : { started: mark });
  }
} catch (e) {
  failures++;
  console.log(`not ok - ${e.message}`);
} finally {
  rmSync(sandbox, { recursive: true, force: true, maxRetries: 10, retryDelay: 300 });
}
console.log(failures ? `\nthe installer's logic: ${failures} case(s) failed` : "\nthe installer's logic: ok");
process.exit(failures ? 1 : 0);
