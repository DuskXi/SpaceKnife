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
// AppUserModelID), closing what runs from a folder (SkCloseIn, on copies of the test program in the
// sandbox standing in for SpaceKnife's programs and for other ones: only SpaceKnife's go, one that
// can't be ended stops it), the files an update in place replaces (SkCheckReplaceable), what the
// uninstaller removes of SpaceKnife's own folders (un.SkRemoveData, in the test program's own
// uninstaller, with a Local and a Roaming folder in the sandbox: 0.3.4's rules for an update, the
// 「你的数据」 box and --delete-app-data; the WebView2 processes of the shell's folder waited for
// before it goes, then ended, another program's left alone: which ones are the shell's by their
// command lines (un.SkUserDataDir, un.SkWebView2Ours, strings only), a list of stand-ins that
// write their profile while the uninstaller runs (still starting, gone by themselves, staying, one
// that can't be ended; no process runs), and two cases with copies of cmd.exe under
// msedgewebview2.exe's name holding their lockfile), a 0.3.4 whose registry keys are gone found
// by its files (SkFindByFiles), which GUID keys the uninstaller may delete and its scope
// (un.SkKeysOurs / un.SkScopeFor, from values it is given), the new program's --startup-check
// (SkStartupCheck, stand-ins answering as it does), the exit code of a failed run and what the
// outer instance then starts again (SkFailureLevel, SkAfterElevatedFailed), the pins of a
// relocated installation (SkMigrateShortcutsIn on a folder of the sandbox), a start with SW_HIDE
// (SkShowIfStartedHidden), the log opened for each line — and that the app the installer starts
// gets its environment (cmd.exe stands in for it: on your own desktop run this from
// packaging/hidden-desktop.ps1, so its console window and the stand-ins' window stay out of sight). Windows only; it writes in a temporary folder only, never the registry, and checks
// first that the functions its cases run name none of the user's own folders, keys or pins.
//
//   node packaging/nsis-logic.mjs [--makensis PATH] [--utils PATH] [--helpers PATH]
//
// makensis: --makensis, $MAKENSIS, target/.tauri/NSIS (bundle > useLocalToolsDir) or the bundler's
// %LOCALAPPDATA%\tauri\NSIS. utils.nsh: --utils, else the one `pnpm bundle` wrote next to the
// rendered installer (target/release/nsis/x64). Exit status 1 when a case says something else.

import { spawn, spawnSync } from "node:child_process";
import { closeSync, constants, copyFileSync, existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
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
/** Stand-ins for WebView2's processes (the "removedata" cases): ended before the sandbox goes. */
const fakes = [];
try {
  // ------------------------------------------------------------------ compile
  const env = { ...process.env };
  delete env.NSISDIR;
  delete env.NSISCONFDIR;
  const c = spawnSync(makensis, ["-INPUTCHARSET", "UTF8", "-V2", `-DSK_HELPERS=${helpers}`, `-DSK_UTILS=${utils}`, `-DSK_PLUGINS=${plugins}`, `-DSK_OUT=${exe}`, join(here, "nsis-logic", "logic-test.nsi")], { encoding: "utf8", env });
  if (c.status !== 0 || !existsSync(exe)) fail(`makensis (${makensis}): exit ${c.status}\n${c.stdout}\n${c.stderr}`);
  console.log(`compiled with ${makensis}: ${helpers}`);

  // ------------------------------------------------------------------ the sandbox holds
  // The cases run spaceknife.nsh's functions for real: none of those they call may name the user's
  // own installation, registry keys or pinned shortcuts itself (installer.nsi, their caller, does).
  // Checked in the source before anything runs.
  {
    const source = readFileSync(helpers, "utf8");
    const test = readFileSync(join(here, "nsis-logic", "logic-test.nsi"), "utf8");
    const body = (name) => {
      const m = new RegExp(`^Function ${name.replace(/[.$]/g, "\\$&")}\\r?\\n([\\s\\S]*?)^FunctionEnd`, "m").exec(source);
      return m ? m[1].split(/\r?\n/).filter((l) => !/^\s*;/.test(l)).join("\n") : null;
    };
    const registry = /\b(Read|Write|Delete|Enum)Reg\w*|\bDeleteRegKey\b|\bDeleteRegValue\b/i;
    const userFolders = /\$(APPDATA|LOCALAPPDATA|SMPROGRAMS|SMSTARTUP|STARTMENU|DESKTOP|QUICKLAUNCH|PROFILE|PROGRAMFILES\w*)\b/i;
    const rules = [
      // "pins": the folder it is given only
      ["SkMigrateShortcutsIn", userFolders],
      // "probe034": the folders it is given only
      ["SkFindByFiles", new RegExp(`${userFolders.source}|SkDefaultDir`, "i")],
      ["SkIsEbFolder", userFolders],
      // "keys": the values it is given, never the registry
      ["un.SkKeysOurs", registry],
      ["un.SkIsInstDir", registry],
      ["un.SkScopeFor", registry],
      // "outer", "failcode"
      ["SkAfterElevatedFailed", registry],
      ["SkFailureLevel", registry],
      ["SkStartupCheck", registry],
    ];
    const broken = [];
    for (const [fn, forbidden] of rules) {
      const code = body(fn);
      if (code === null) broken.push(`${fn}: not found`);
      else if (forbidden.test(code)) broken.push(`${fn} names ${forbidden.exec(code)[0]}`);
    }
    // SkDetectScope and SkFindPrevious (the "flags" case) look for 0.3.4's files only where
    // SK_DEFAULT_DIR_OF says, which the test points into its sandbox
    for (const fn of ["SkDetectScope", "SkFindPrevious"]) if (/Call SkDefaultDir/.test(body(fn) ?? "")) broken.push(`${fn} looks into the default folder itself`);
    if (!/^!define SK_DEFAULT_DIR_OF TestDefaultDirOf$/m.test(test)) broken.push("logic-test.nsi does not point SK_DEFAULT_DIR_OF into the sandbox");
    // SkMigratePins names the user's pinned folders: never called here
    if (/Call SkMigratePins/.test(test)) broken.push("logic-test.nsi calls SkMigratePins (the user's own pins)");
    // PUT opens its file with $0 (and $1): text from those would be the file's handle (a stand-in's
    // process id, which a case then ends or waits for)
    const put = /!insertmacro PUT "[^"]*" "[^"]*\$[01](?!\d)/.exec(test);
    if (put) broken.push(`logic-test.nsi writes $0 or $1 with PUT (the file handle): ${put[0]}`);
    if (broken.length) throw new Error(`not run, the cases would reach outside the sandbox: ${broken.join("; ")}`);
  }

  // ------------------------------------------------------------------ run
  const temp = join(sandbox, "temp");
  mkdirSync(temp);
  let n = 0;
  /**
   * One run: the case, its command line (as the process gets it, verbatim) -> Map key -> value.
   * `program`: what runs (the test program, or the uninstaller it wrote); `exeAt`: it runs from a
   * copy at this path in the case's folder; `prepare(box)` runs once the case's folder (and that
   * copy) is there, before the program starts.
   */
  function run(kind, cmdline = "", extra = {}, { program = exe, exeAt = null, prepare = () => {}, timeout = 60_000, windowsHide = false } = {}) {
    const c = setUp(kind, extra, { program, exeAt, prepare });
    const r = spawnSync(c.program, cmdline ? [cmdline] : [], { windowsVerbatimArguments: true, windowsHide, encoding: "utf8", timeout, env: c.env });
    return { got: output(kind, cmdline, c.out, r.status, r.error), box: c.box, status: r.status };
  }
  /**
   * run() without waiting in a blocking call, for a case whose runner acts while the program runs:
   * `prepare(box)` may return more of the environment, `during(box)` starts what the runner does
   * meanwhile.
   */
  function runAsync(kind, cmdline = "", extra = {}, { program = exe, prepare = () => {}, during = () => {}, timeout = 120_000 } = {}) {
    const c = setUp(kind, extra, { program, prepare });
    return new Promise((done, fail) => {
      const child = spawn(c.program, cmdline ? [cmdline] : [], { windowsVerbatimArguments: true, stdio: "ignore", env: c.env });
      let error = null;
      const timer = setTimeout(() => child.kill(), timeout);
      child.on("error", (e) => (error = e));
      child.on("close", (status) => {
        clearTimeout(timer);
        try {
          done({ got: output(kind, cmdline, c.out, status, error), box: c.box, status });
        } catch (e) {
          fail(e);
        }
      });
      during(c.box);
    });
  }
  /** A case's output file, folder, program (`exeAt`: a copy at this path in the folder) and environment. */
  function setUp(kind, extra, { program = exe, exeAt = null, prepare = () => {} }) {
    const out = join(sandbox, `out-${++n}.txt`);
    const box = join(sandbox, `box-${n}`);
    mkdirSync(box);
    if (exeAt) {
      const copy = join(box, exeAt);
      mkdirSync(dirname(copy), { recursive: true });
      copyFileSync(program, copy);
      program = copy;
    }
    const more = prepare(box) ?? {};
    return { out, box, program, env: { ...process.env, TMP: temp, TEMP: temp, SK_LOGIC_CASE: kind, SK_LOGIC_OUT: out, SK_LOGIC_SANDBOX: box, ...extra, ...more } };
  }
  /** What a case wrote: key -> value. */
  function output(kind, cmdline, out, status, error) {
    if (!existsSync(out)) throw new Error(`${kind} ${cmdline}: no output (exit ${status} ${error ?? ""})`);
    const text = readFileSync(out).toString("utf16le").replace(/^\uFEFF/, "");
    const got = new Map();
    for (const line of text.split(/\r?\n/).filter(Boolean)) {
      const at = line.startsWith("dir ") ? line.lastIndexOf("=") : line.indexOf("=");
      got.set(line.slice(0, at), line.slice(at + 1));
    }
    return got;
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
  const none = { passive: "0", update: "0", noshortcut: "0", run: "0", runargs: "", scope: "", forced: "0", inner: "0", wizard: "0", keepshortcuts: "0", deletedata: "0", deleteappdata: "0" };
  const flags = [
    ["--updated /S --force-run /currentuser", { update: "1", run: "1", runargs: "--updated", scope: "user", forced: "1", keepshortcuts: "1", flag: "/currentuser", elevated: "--updated /S --force-run /currentuser --sk-elevated /currentuser" }],
    ["--updated /S --force-run /allusers", { update: "1", run: "1", runargs: "--updated", scope: "machine", forced: "1", keepshortcuts: "1", flag: "/allusers", elevated: "--updated /S --force-run /allusers --sk-elevated /allusers" }],
    ["--updated /S --force-run", { update: "1", run: "1", runargs: "--updated", keepshortcuts: "1", elevated: /^--updated \/S --force-run --sk-elevated \/(currentuser|allusers)$/ }],
    ["/S /UPDATE /R /ARGS --updated", { update: "1", run: "1", runargs: "--updated", keepshortcuts: "1" }],
    ["/S /R /ARGS --minimized", { run: "1", runargs: "--minimized" }],
    ["/S /P /NS", { passive: "1", noshortcut: "1" }],
    ["/S /KEEP_APP_DATA /currentuser --keep-shortcuts --updated _?=C:\\Program Files\\Space Knife", { update: "1", runargs: "--updated", scope: "user", forced: "1", keepshortcuts: "1", flag: "/currentuser" }],
    ["/S --delete-app-data", { deletedata: "1", deleteappdata: "1" }],
    ["/S --updated --delete-app-data", { update: "1", runargs: "--updated", keepshortcuts: "1", deletedata: "0" }],
    ["/S /R --updated --no-desktop-shortcut", { update: "1", noshortcut: "1", run: "1", runargs: "--updated", keepshortcuts: "1" }],
    ["/s --UPDATED /r /AllUsers /D=C:\\Some Dir\\SpaceKnife", { update: "1", run: "1", runargs: "--updated", scope: "machine", forced: "1", keepshortcuts: "1", elevated: "/s --UPDATED /r /AllUsers --sk-elevated /allusers /D=C:\\Some Dir\\SpaceKnife" }],
    ["/S --sk-elevated /allusers", { scope: "machine", forced: "1", inner: "1" }],
    // the elevated copy the scope page starts: it skips the pages the outer instance showed
    ["--sk-elevated /allusers --sk-wizard /D=C:\\Some Dir\\SpaceKnife", { scope: "machine", forced: "1", inner: "1", wizard: "1", elevated: "--sk-elevated /allusers --sk-wizard --sk-elevated /allusers /D=C:\\Some Dir\\SpaceKnife" }],
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
    // SkCloseIn (installer.nsi step 2, the uninstaller), on copies of the test program standing in
    // for programs: SpaceKnife's that run from the folder go — asked first (the one with a window
    // closes it and quits), then ended (by the path, as 0.3.4's installer did) — and nothing else
    // does: the user's own program in that folder, a SpaceKnife.exe in a folder whose name starts
    // the same, a program that holds one of the folder's files (0.3.4's uninstaller and
    // SkCheckReplaceable find that one), another copy of the installer, and the installer itself:
    // this case runs from the folder (an installer in 0.3.4's data folder, an uninstaller in place)
    const { got, box } = run("close", "", {}, { exeAt: "Programs\\SpaceKnife\\SpaceKnife-Setup.exe", timeout: 120_000 });
    const ready = Object.fromEntries(["polite", "backend", "user", "other", "holder", "twin"].map((k) => [`ready_${k}`, "1"]));
    expect(
      "SkCloseIn: SpaceKnife's programs in the folder, and nothing else",
      got,
      { ...ready, exedir: `${box}\\Programs\\SpaceKnife`, result: "", polite: "ended 10", backend: "ended 1", user: "running", other: "running", holder: "running", twin: "running" },
      ["given"],
    );
  }
  {
    // one it can't end (as an elevated SpaceKnife is to an installer that is not): the rest is
    // ended, and SkCloseIn says so (the installer stops with nothing changed) and logs why
    const { got } = run("close-busy", "", {}, { timeout: 120_000 });
    const logFile = join(temp, "SpaceKnife-install.log");
    const log = existsSync(logFile) ? readFileSync(logFile).toString("utf16le") : "";
    const logged = /could not end: SpaceKnife\.exe \(\d+\): error 5\b/.test(log) ? "1" : "0";
    expect(
      "SkCloseIn: one that can't be ended",
      new Map([...got, ["logged", logged]]),
      { ready_stubborn: "1", ready_backend: "1", result: "busy", stubborn: "running", backend: "ended 1", logged: "1" },
      ["debug_off", "given"],
    );
  }
  {
    // an update in place (installer.nsi step 2): no file it replaces may be held by a program that
    // lets no one delete it (what follows is not undone); one shared for deleting too may go
    const { got } = run("replaceable");
    expect("SkCheckReplaceable", got, { held: "ui\\index.html", shared: "", free: "" });
  }
  {
    // 0.3.4 whose registry keys are gone (its files are all there is): found by its uninstaller and
    // its app.asar, in the folder /D= gives, else in the scope's default one (a folder of the
    // sandbox here), with its version read from its program; one of the two files is not enough
    const { got, box } = run("probe034");
    const eb = (dir, def) => `eb|${box}\\${dir}|${box}\\${dir}\\Uninstall SpaceKnife.exe|0.3.4|${def ? `${box}\\${def}` : ""}`;
    const nothing = (def) => `||||${def ? `${box}\\${def}` : ""}`;
    expect("SkFindByFiles: 0.3.4 without its keys", got, {
      default: eb("Default\\SpaceKnife", "Default\\SpaceKnife"),
      cmd: eb("Mine\\SpaceKnife", "Default\\SpaceKnife"),
      cmd_empty: eb("Default\\SpaceKnife", "Default\\SpaceKnife"),
      asar_only: nothing("AsarOnly\\SpaceKnife"),
      uninstaller_only: nothing("UninstallerOnly\\SpaceKnife"),
      empty: nothing("Empty\\SpaceKnife"),
      none: nothing(""),
      missing: nothing("Gone\\SpaceKnife"),
      kept: "1",
    });
  }
  {
    // a failed run's exit code: 3 once the earlier version is gone (the elevated copy tells its
    // outer instance so), NSIS's own otherwise (2: a run the script stopped, here by Quit)
    for (const [gone, code] of [["1", 3], ["0", 2], ["", 2]]) {
      const { got, status } = run("failcode", "", { SK_LOGIC_GONE: gone });
      expect(`SkFailureLevel: the earlier version gone = "${gone}"`, new Map([...got, ["status", String(status)]]), { gone, status: String(code) });
    }
  }
  {
    // the outer instance after its elevated copy failed: the installed version is started again
    // (a stand-in in the sandbox), but not after exit code 3 (it is gone, the new one is not whole)
    for (const [code, restarted] of [["2", "1"], ["-1", "1"], ["3", "0"]]) {
      const { got } = run("outer", "", { SK_LOGIC_INNER_CODE: code });
      // elevated through UAC, the stand-in starts as the desktop user without this environment
      const want = got.get("elevation") === "2" && restarted === "1" ? /./ : restarted;
      expect(`SkAfterElevatedFailed: the elevated copy exited with ${code}`, got, { restarted: want });
    }
  }
  {
    // a relocated installation's pins (a folder of the sandbox standing in for User Pinned\TaskBar):
    // the old program's points here with the AppUserModelID, another program's and a file that is
    // no shortcut stay, a folder that is not there is nothing
    const { got, box } = run("pins");
    const aumid = (lnk) => (readFileSync(join(box, "pins", lnk)).includes(Buffer.from("io.github.duskxi.spaceknife", "utf16le")) ? "1" : "0");
    expect(
      "SkMigrateShortcutsIn",
      new Map([...got, ["moved_aumid", aumid("SpaceKnife.lnk")], ["other_aumid", aumid("Mine.lnk")]]),
      { moved: "1", moved_aumid: "1", other: "1", other_aumid: "0", notes: "the user's", no_pins: "0", kept_r1: `${box}\\old\\SpaceKnife.exe`, kept_r2: `${box}\\pins` },
      ["moved_why", "moved_want"],
    );
  }
  {
    // the new program asked --startup-check before anything is touched (stand-ins answer as the
    // real one does): its own words when it can't start, its exit code when it says nothing, a
    // time limit (and it is ended: the stand-in's own process id, checked after), a program that
    // can't be run; the installer's own error mode is what it was afterwards (the program runs
    // without the system's error boxes, SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX: checked in
    // the source, an NSIS stand-in sets its own mode)
    const { got } = run("startup", "", {}, { timeout: 120_000 });
    const kept = got.get("mode_before") === got.get("mode_after") ? "1" : `${got.get("mode_before")} -> ${got.get("mode_after")}`;
    expect("SkStartupCheck", new Map([...got, ["mode_kept", kept]]), {
      mode_kept: "1",
      check_ok: "",
      check_old: "SpaceKnife needs the Microsoft Edge WebView2 Runtime 89.0.774.44 or later; this computer has 88.0.705.81.",
      check_missing: "SpaceKnife needs the Microsoft Edge WebView2 Runtime, which is not installed (error 0x80070002).",
      check_noanswer: "it ended with exit code 0.",
      check_crash: "it ended with exit code -1073741515.",
      check_hang: "it did not answer within 3000 ms.",
      hang_process: "ended",
      check_absent: "it could not be run.",
    });
  }
  {
    // started as 0.3.4 starts the installer (Node's windowsHide: SW_HIDE in STARTUPINFO), a window
    // that is never shown takes the startup's SW_HIDE, so that the wizard's first ShowWindow shows
    // it; started otherwise, nothing happens
    const hidden = run("hidden", "", {}, { windowsHide: true }).got;
    expect("SkShowIfStartedHidden: started with windowsHide", hidden, { usewindow: "1", show: "0", consumed: "1" });
    const shown = run("hidden").got;
    expect("SkShowIfStartedHidden: started without it", shown, { consumed: "0" });
  }
  {
    // the log is opened for each line: another instance (the elevated copy) writes between two of
    // this one's lines; the error flag is what it was
    const mark = `sk-log-${process.pid}-${Date.now()}`;
    const { got } = run("log", "", { SK_LOGIC_MARK: mark });
    const logFile = join(temp, "SpaceKnife-install.log");
    const lines = (existsSync(logFile) ? readFileSync(logFile).toString("utf16le") : "").split("\r\n").filter((l) => l.startsWith(mark));
    const order = JSON.stringify(lines.slice(0, 3)) === JSON.stringify([`${mark} first`, `${mark} second`, `${mark} third`]) ? "1" : JSON.stringify(lines);
    expect("SkLogLine: one line at a time", new Map([...got, ["order", order]]), { child: /^\d+$/, order: "1", errors_after: "0", errors_kept: "1" });
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
    // It waits for and ends WebView2's processes through SK_WEBVIEW2_SCAN, which logic-test.nsi
    // points at its list of stand-ins: no process of the system's is ended there (nor opened), and
    // un.SkWebView2Ours, which says which ones are SpaceKnife's, only reads what it is given. The
    // system's processes are looked at in the two stand-in cases only (SK_LOGIC_WV2_FAKE empty).
    {
      const test = readFileSync(join(here, "nsis-logic", "logic-test.nsi"), "utf8");
      const bodyIn = (text, name) => {
        const m = new RegExp(`^Function ${name.replace(/[.$]/g, "\\$&")}\\r?\\n([\\s\\S]*?)^FunctionEnd`, "m").exec(text);
        return m ? m[1].split(/\r?\n/).filter((l) => !/^\s*;/.test(l)).join("\n") : null;
      };
      const reaching = /System::|TerminateProcess|OpenProcess|\bExec(Wait|Shell)?\b|nsExec|\bKill/i;
      const broken = [];
      if (!/^!define SK_WEBVIEW2_SCAN un\.TestWebView2In$/m.test(test)) broken.push("logic-test.nsi does not point SK_WEBVIEW2_SCAN at its stand-ins");
      if (!/^!ifndef SK_WEBVIEW2_SCAN$/m.test(source)) broken.push("spaceknife.nsh does not take SK_WEBVIEW2_SCAN from the test");
      if (/Call un\.SkWebView2In\b/.test(bodyIn(source, "un.SkEndWebView2In") ?? "Call un.SkWebView2In")) broken.push("un.SkEndWebView2In calls un.SkWebView2In itself");
      for (const [text, fn] of [[test, "un.TestWebView2In"], [test, "un.Wv2Lines"], [source, "un.SkWebView2Ours"], [source, "un.SkUserDataDir"]]) {
        const b = bodyIn(text, fn);
        if (b === null) broken.push(`${fn}: not found`);
        else if (reaching.test(b)) broken.push(`${fn} names ${reaching.exec(b)[0]}`);
      }
      // the system's processes only without a list
      const fake = bodyIn(test, "un.TestWebView2In") ?? "";
      if (!/^\s*ReadEnvStr \$0 SK_LOGIC_WV2_FAKE\n\s*\$\{If\} \$0 == ""\n\s*Pop \$0\n\s*Call un\.SkWebView2In\n\s*Return\n/m.test(fake) || fake.match(/Call un\.SkWebView2In/g)?.length !== 1) {
        broken.push("un.TestWebView2In looks at the system's processes with a list of stand-ins");
      }
      if (broken.length) throw new Error(`not run, the WebView2 cases would reach outside the sandbox: ${broken.join("; ")}`);
    }
    // the cases that are not about WebView2: an empty list of stand-ins
    const noWebView2 = join(sandbox, "wv2-none.txt");
    writeFileSync(noWebView2, "");
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
      ["an update: electron-builder's copy of its installer goes, nothing else", "/S /KEEP_APP_DATA /currentuser --keep-shortcuts --updated", { SK_LOGIC_ROAMING_OTHER: "1" }, { ...kept, updater: "0" }],
      ["an update with --delete-app-data: the same, the data stays", "/S --updated --delete-app-data", { SK_LOGIC_ROAMING_OTHER: "1" }, { ...kept, updater: "0" }],
      ["an uninstall: the caches go, %APPDATA%\\SpaceKnife with something else in it stays", "/S", { SK_LOGIC_ROAMING_OTHER: "1" }, caches],
      ["an uninstall: an empty %APPDATA%\\SpaceKnife goes", "/S", {}, { ...caches, ...roaming }],
      ["the box: the data goes, %APPDATA%\\SpaceKnife with something else in it stays", "/S", { SK_LOGIC_BOX: "1", SK_LOGIC_ROAMING_OTHER: "1" }, { ...caches, ...data }],
      ["the box: an empty %APPDATA%\\SpaceKnife goes", "/S", { SK_LOGIC_BOX: "1" }, { ...caches, ...data, ...roaming }],
      ["--delete-app-data: the data and %APPDATA%\\SpaceKnife as a whole", "/S --delete-app-data", { SK_LOGIC_ROAMING_OTHER: "1" }, { ...caches, ...data, ...roaming }],
      ["--delete-app-data with the box unticked: %APPDATA%\\SpaceKnife as a whole, the data stays", "--delete-app-data", { SK_LOGIC_BOX: "0", SK_LOGIC_ROAMING_OTHER: "1" }, { ...caches, ...roaming }],
      ["--delete-app-data, the program in the data folder: that stays", "/S --delete-app-data", { SK_LOGIC_ROAMING_OTHER: "1", SK_LOGIC_INSTDIR: "Local\\SpaceKnife\\app" }, { ...caches, ...roaming, instdir_user: "1" }],
      ["--delete-app-data, the program in %APPDATA%\\SpaceKnife: that stays", "/S --delete-app-data", { SK_LOGIC_INSTDIR: "Roaming\\SpaceKnife\\app" }, { ...caches, ...data, roaming_other: "0", instdir_user: "1" }],
    ]) {
      const { got } = run("removedata", `${cmdline} _?=${sandbox}\\Programs\\SpaceKnife`, { ...env, SK_LOGIC_WV2_FAKE: noWebView2 }, { program: uninstaller });
      expect(`un.SkRemoveData: ${what}`, got, want, ["error", "update", "deletedata", "deleteappdata"]);
    }

    // which GUID keys the uninstaller deletes: only those that name its own folder (or none), never
    // another installation's in the same hive; and its scope from them, by where the folder is when
    // neither names it. From the values it is given (the case reads no registry; $INSTDIR is a folder
    // of the sandbox).
    {
      const { got, box } = run("keys", `/S _?=${sandbox}\\Programs\\SpaceKnife`, {}, { program: uninstaller });
      expect("un.SkKeysOurs and un.SkScopeFor", got, {
        scope_kept_r1: `${box}\\Programs\\SpaceKnife-x`,
        owned_empty: "1",
        owned_same: "1",
        owned_slash: "1",
        owned_quoted: "1",
        owned_case: "1",
        owned_dot: "1",
        owned_short: "1",
        owned_alike: "0",
        owned_parent: "0",
        owned_child: "0",
        owned_elsewhere: "0",
        scope_hklm: "machine",
        scope_hkcu: "user",
        scope_neither: "user",
        scope_none: "user",
        scope_programfiles: "machine",
        scope_programfiles_other: "machine",
      }, ["error", "instdir"]);
    }

    // Which processes are the shell's WebView2 (un.SkUserDataDir, un.SkWebView2Ours): command lines
    // as WebView2's own processes get them (the value quoted), as Crashpad's handler gets one with a
    // space in it (the whole argument quoted), unquoted, at the start, after a tab, in another case,
    // through "..", the folder itself; and what is not: another program's folder, one whose name
    // only begins like the shell's, the folder that holds the shell's, a program that is not
    // WebView2, the switch inside another switch's value, an unquoted folder cut at its space, only
    // the first --user-data-dir. Strings only: no process is looked at.
    {
      const lines = [];
      const { got } = run("wv2lines", `/S _?=${sandbox}\\Programs\\SpaceKnife`, {}, {
        program: uninstaller,
        prepare: (box) => {
          const local = join(box, "Local");
          const shell = join(local, "io.github.duskxi.spaceknife");
          const profile = join(shell, "webview", "EBWebView");
          const runtime = '"C:\\Program Files (x86)\\Microsoft\\EdgeWebView\\Application\\131.0.2903.86\\msedgewebview2.exe"';
          const word = (s) => s.split(/[ \t]/)[0];
          const spaced = join(shell, "web view", "EBWebView");
          const other = join(local, "Another.App", "EBWebView");
          for (const [key, name, cmd, dir, ours] of [
            ["browser", "msedgewebview2.exe", `${runtime} --embedded-browser-webview=1 --webview-exe-name=SpaceKnife.exe --user-data-dir="${profile}" --noerrdialogs`, profile, "1"],
            ["renderer", "msedgewebview2.exe", `${runtime} --type=renderer --user-data-dir="${profile}" --webview-exe-name=SpaceKnife.exe /prefetch:1`, profile, "1"],
            ["crashpad", "msedgewebview2.exe", `${runtime} --type=crashpad-handler "--user-data-dir=${spaced}" /prefetch:4 "--database=${spaced}\\Crashpad"`, spaced, "1"],
            ["bare", "msedgewebview2.exe", `${runtime} --type=gpu-process --user-data-dir=${profile} --gpu-preferences=UAAAAAAAAADgAAAEAAAAAAAAAAAAAAAAAABgAAEAAAA`, word(profile), word(profile) === profile ? "1" : "0"],
            ["start", "msedgewebview2.exe", `--user-data-dir="${profile}"`, profile, "1"],
            ["tab", "msedgewebview2.exe", `${runtime}\t--user-data-dir="${profile}"\t--type=utility`, profile, "1"],
            ["case", "MSEdgeWebView2.EXE", `${runtime} --USER-DATA-DIR="${profile.toUpperCase()}"`, profile.toUpperCase(), "1"],
            ["dotdot", "msedgewebview2.exe", `${runtime} --user-data-dir="${local}\\Another.App\\..\\io.github.duskxi.spaceknife\\webview\\EBWebView"`, `${local}\\Another.App\\..\\io.github.duskxi.spaceknife\\webview\\EBWebView`, "1"],
            ["itself", "msedgewebview2.exe", `${runtime} --user-data-dir="${shell}"`, shell, "1"],
            ["other", "msedgewebview2.exe", `${runtime} --user-data-dir="${other}"`, other, "0"],
            ["alike", "msedgewebview2.exe", `${runtime} --user-data-dir="${shell}-beta\\EBWebView"`, `${shell}-beta\\EBWebView`, "0"],
            ["parent", "msedgewebview2.exe", `${runtime} --user-data-dir="${local}"`, local, "0"],
            ["notwv2", "cmd.exe", `cmd.exe /d /c rem --user-data-dir="${profile}"`, profile, "0"],
            ["nodir", "msedgewebview2.exe", `${runtime} --type=utility --utility-sub-type=network.mojom.NetworkService`, "", "0"],
            ["value", "msedgewebview2.exe", `${runtime} --js-flags=--user-data-dir=${profile} --log-file="${profile}\\debug.log"`, "", "0"],
            ["cut", "msedgewebview2.exe", `${runtime} --user-data-dir=${local}\\Another App\\EBWebView`, word(`${local}\\Another`), "0"],
            ["firstwins", "msedgewebview2.exe", `${runtime} --user-data-dir="${other}" --user-data-dir="${profile}"`, other, "0"],
            ["empty", "msedgewebview2.exe", `${runtime} --user-data-dir= --type=renderer`, "", "0"],
          ]) {
            lines.push([key, dir, ours]);
            writeFileSync(join(box, "lines.txt"), `${key}|${shell}|${name}|${cmd}\r\n`, { encoding: "utf16le", flag: "a" });
          }
          return { SK_LOGIC_WV2_LINES: join(box, "lines.txt") };
        },
      });
      expect(
        "un.SkUserDataDir and un.SkWebView2Ours: the shell's WebView2 by its command line",
        got,
        Object.fromEntries(lines.flatMap(([key, dir, ours]) => [[`wv2dir_${key}`, dir], [`wv2ours_${key}`, ours]])),
        ["error"],
      );
    }

    // WebView2's processes, a list of stand-ins (un.TestWebView2In: no process runs, none of the
    // system's is looked at), and what they do meanwhile, played by the runner: one of the shell's
    // writes its profile (Local State, lockfile, Variations) `at` ms after the uninstaller started,
    // as a WebView2 that is still starting does (the app was ended right after it started: what
    // the Windows Server 2022 runner showed), and `exits` ms after that start it exits by itself,
    // writing Local State on its way out (null: only when ended). What is left of the shell's
    // folder is looked at once the last of them has done all that, after the uninstaller ended.
    const flagsOf = (got, extra) => new Map([...got, ...Object.entries(extra)]);
    const fakeWebView2 = (box, procs) => {
      const dir = join(box, "wv2-fake");
      mkdirSync(dir);
      const list = procs.map((p) => `${p.pid}|${p.name ?? "msedgewebview2.exe"}|${p.endable === false ? 0 : 1}|${p.cmd}\r\n`).join("");
      writeFileSync(join(dir, "procs.txt"), list, "utf16le");
      for (const p of procs) writeFileSync(join(dir, `${p.pid}.alive`), "");
      const write = (p, names) => {
        if (!existsSync(join(dir, `${p.pid}.alive`))) return;
        mkdirSync(p.profile, { recursive: true });
        for (const name of names) writeFileSync(join(p.profile, name), "{}");
      };
      const timers = [];
      let start = 0;
      let last = 0;
      return {
        env: { SK_LOGIC_WV2_FAKE: join(dir, "procs.txt") },
        ended: (pid) => (existsSync(join(dir, `${pid}.ended`)) ? "1" : "0"),
        during: () => {
          start = Date.now();
          for (const p of procs) {
            if (p.profile) timers.push(setTimeout(() => write(p, ["Local State", "lockfile", "Variations"]), p.at ?? 300));
            if (p.exits != null) {
              timers.push(
                setTimeout(() => {
                  if (p.profile) {
                    write(p, ["Local State"]);
                    rmSync(join(p.profile, "lockfile"), { force: true });
                  }
                  rmSync(join(dir, `${p.pid}.alive`), { force: true });
                }, p.exits),
              );
            }
            last = Math.max(last, p.profile ? (p.at ?? 300) : 0, p.exits ?? 0);
          }
        },
        /** once the uninstaller has ended: until the last of them has done what it does */
        settled: () => new Promise((done) => setTimeout(done, Math.max(0, start + last + 200 - Date.now()))),
        stop: () => timers.forEach(clearTimeout),
      };
    };
    const logFile = join(temp, "SpaceKnife-install.log");
    const logSize = () => (existsSync(logFile) ? readFileSync(logFile).length : 0);
    const logSince = (size) => (existsSync(logFile) ? readFileSync(logFile).subarray(size).toString("utf16le") : "");
    /** A "removedata" case with stand-ins: what it wrote, the shell's folder afterwards, its log lines. */
    const withFakes = async (cmdline, procsOf) => {
      let fake, shell;
      const size = logSize();
      const r = await runAsync("removedata", `${cmdline} _?=${sandbox}\\Programs\\SpaceKnife`, { SK_LOGIC_ROAMING_OTHER: "1" }, {
        program: uninstaller,
        prepare: (box) => {
          shell = join(box, "Local", "io.github.duskxi.spaceknife");
          fake = fakeWebView2(box, procsOf(box, shell));
          return fake.env;
        },
        during: () => fake.during(),
      });
      await fake.settled();
      fake.stop();
      return { ...r, fake, shellAfter: existsSync(shell) ? "1" : "0", log: logSince(size) };
    };
    const has = (log, re) => (re.test(log) ? "1" : "0");
    // the stand-ins' command lines: as WebView2's browser process and Crashpad's handler get theirs
    const runtime = '"C:\\Program Files (x86)\\Microsoft\\EdgeWebView\\Application\\131.0.2903.86\\msedgewebview2.exe"';
    const browser = (userData) => `${runtime} --embedded-browser-webview=1 --webview-exe-name=SpaceKnife.exe --user-data-dir="${userData}" --noerrdialogs`;
    const crashpad = (userData) => `${runtime} --type=crashpad-handler "--user-data-dir=${userData}" /prefetch:4`;
    const others = (box) => {
      const local = join(box, "Local");
      return [
        { pid: 9101, cmd: browser(join(local, "Another.App", "EBWebView")) },
        { pid: 9102, cmd: browser(join(local, "io.github.duskxi.spaceknife-beta", "EBWebView")) },
        { pid: 9103, name: "cmd.exe", cmd: `cmd.exe /d /c rem --user-data-dir="${join(local, "io.github.duskxi.spaceknife", "webview", "EBWebView")}"` },
        { pid: 9104, cmd: `${runtime} --type=utility --utility-sub-type=network.mojom.NetworkService` },
      ];
    };
    const untouched = (fake) => Object.fromEntries([9101, 9102, 9103, 9104].map((pid) => [`ended_${pid}`, fake.ended(pid)]));
    const notEnded = { ended_9101: "0", ended_9102: "0", ended_9103: "0", ended_9104: "0" };
    {
      // still starting, then gone by themselves: waited for (not ended), and only then the folder
      // goes, so nothing they wrote is left (an uninstaller that tried the folder first and found
      // it gone left their profile behind)
      const { got, fake, shellAfter, log } = await withFakes("/S", (box, shell) => [
        { pid: 9001, cmd: browser(join(shell, "webview", "EBWebView")), profile: join(shell, "webview", "EBWebView"), at: 300, exits: 2200 },
        { pid: 9002, cmd: crashpad(join(shell, "webview", "EBWebView")), profile: join(shell, "webview", "EBWebView", "Crashpad"), at: 400, exits: 2000 },
        ...others(box),
      ]);
      expect(
        "un.SkRemoveData: WebView2 of the shell's folder still starting: waited for until it has gone, then the folder",
        flagsOf(got, {
          shell_after: shellAfter,
          ended_9001: fake.ended(9001),
          ended_9002: fake.ended(9002),
          ...untouched(fake),
          waited: has(log, /went by itself after about \d+ ms/),
          logged: has(log, /still running: msedgewebview2\.exe 9001, 9002;/),
        }),
        { ...caches, shell_after: "0", ended_9001: "0", ended_9002: "0", ...notEnded, waited: "1", logged: "1" },
      );
    }
    {
      // one that stays: ended after the wait (3 s here, 10 s in the installer), also with its
      // profile in a folder with a space in it (Crashpad's quoting) or named through ".."; another
      // program's, one of a folder that only begins like the shell's, a program that is not
      // WebView2 and a WebView2 without a profile folder are never ended
      const { got, fake, shellAfter, log } = await withFakes("/S", (box, shell) => [
        { pid: 9001, cmd: browser(join(shell, "webview", "EBWebView")), profile: join(shell, "webview", "EBWebView"), at: 300, exits: null },
        { pid: 9002, cmd: crashpad(join(shell, "web view", "EBWebView")), profile: join(shell, "web view", "EBWebView"), at: 300, exits: null },
        { pid: 9003, cmd: browser(`${join(box, "Local", "Another.App")}\\..\\io.github.duskxi.spaceknife\\webview\\EBWebView`), exits: null },
        ...others(box),
      ]);
      expect(
        "un.SkRemoveData: WebView2 of the shell's folder that stays is ended, nothing else is",
        flagsOf(got, {
          shell_after: shellAfter,
          ended_9001: fake.ended(9001),
          ended_9002: fake.ended(9002),
          ended_9003: fake.ended(9003),
          ...untouched(fake),
          logged: has(log, /still running after about 3000 ms: msedgewebview2\.exe 9001, 9002, 9003; ending it/),
          ended_logged: has(log, /ended msedgewebview2\.exe 9002 \(--user-data-dir [^)]*\\web view\\EBWebView\)/),
        }),
        { ...caches, shell_after: "0", ended_9001: "1", ended_9002: "1", ended_9003: "1", ...notEnded, logged: "1", ended_logged: "1" },
      );
    }
    {
      // one that can't be ended (an elevated SpaceKnife's, from an uninstaller that is not): said
      // once in the log, the rest goes on
      const { got, fake, log } = await withFakes("/S", (box, shell) => [
        { pid: 9001, cmd: browser(join(shell, "webview", "EBWebView")), profile: join(shell, "webview", "EBWebView"), at: 300, exits: null, endable: false },
        ...others(box),
      ]);
      const said = (log.match(/could not end WebView2 of [^\r\n]*: msedgewebview2\.exe 9001: error 5/g) ?? []).length;
      expect(
        "un.SkRemoveData: WebView2 of the shell's folder that can't be ended is logged once",
        flagsOf(got, { ended_9001: fake.ended(9001), ...untouched(fake), said: String(said) }),
        { ...caches, ended_9001: "0", ...notEnded, said: "1" },
      );
    }
    {
      // none of the shell's: nothing waited for, another program's left alone
      const { got, fake, log } = await withFakes("/S", (box) => others(box));
      expect(
        "un.SkRemoveData: no WebView2 of the shell's folder: not waited for",
        flagsOf(got, { ...untouched(fake), none: has(log, /no WebView2 process has its profile in /), waited: has(log, /waiting for it to go/) }),
        { ...caches, ...notEnded, none: "1", waited: "0" },
      );
    }
    {
      // an update (0.3.4's installer runs this one's uninstaller with --updated): the shell's
      // folder stays, its WebView2 (the new version's window, maybe) is not waited for or ended
      const { got, fake } = await withFakes("/S /KEEP_APP_DATA /currentuser --keep-shortcuts --updated", (box, shell) => [
        { pid: 9001, cmd: browser(join(shell, "webview", "EBWebView")), exits: null },
        ...others(box),
      ]);
      expect("un.SkRemoveData: an update leaves the shell's WebView2 alone", flagsOf(got, { ended_9001: fake.ended(9001), ...untouched(fake) }), {
        ...kept,
        updater: "0",
        ended_9001: "0",
        ...notEnded,
      });
    }

    // The same with real processes, the two cases that look at the system's: stand-ins (cmd.exe
    // under msedgewebview2.exe's name, started with a --user-data-dir as WebView2 is) hold their
    // folder's lockfile, opened without sharing as Chromium does. Those of the shell's folder go by
    // themselves or are ended (TerminateProcess); another program's, and one of a folder whose name
    // only begins like the shell's, are never touched. They are the only processes whose
    // --user-data-dir is in the sandbox.
    const wv2 = join(sandbox, "wv2", "msedgewebview2.exe");
    mkdirSync(dirname(wv2));
    copyFileSync(join(process.env.SystemRoot ?? "C:\\Windows", "System32", "cmd.exe"), wv2);
    /** A stand-in with its --user-data-dir: `/k` stays (until its stdin ends), else `/c <command> &`. */
    const webview2 = (userData, how) => {
      mkdirSync(userData, { recursive: true });
      const fd = openSync(join(userData, "lockfile"), constants.O_WRONLY | constants.O_CREAT | 0x10000000); // UV_FS_O_EXLOCK: no sharing
      try {
        const child = spawn(wv2, [`/d ${how} rem --user-data-dir="${userData}"`], { windowsVerbatimArguments: true, windowsHide: true, stdio: ["pipe", fd, "ignore"] });
        child.on("error", () => {});
        fakes.push(child);
        return child;
      } finally {
        closeSync(fd);
      }
    };
    const alive = (child) => {
      try {
        process.kill(child.pid, 0);
        return true;
      } catch {
        return false;
      }
    };
    const exitCode = (child) =>
      child.exitCode !== null ? child.exitCode : new Promise((done) => (child.once("exit", (code) => done(code)), setTimeout(() => done("running"), 5000)));
    {
      let own, other, alike;
      const { got, box } = run("removedata", `/S _?=${sandbox}\\Programs\\SpaceKnife`, { SK_LOGIC_ROAMING_OTHER: "1", SK_LOGIC_WV2_FAKE: "" }, {
        program: uninstaller,
        prepare: (box) => {
          own = webview2(join(box, "Local", "io.github.duskxi.spaceknife", "EBWebView"), "/k");
          other = webview2(join(box, "Local", "Another.App", "EBWebView"), "/k");
          alike = webview2(join(box, "Local", "io.github.duskxi.spaceknife-beta", "EBWebView"), "/k");
        },
      });
      const state = {
        own_ended: alive(own) ? "0" : "1",
        other_running: alive(other) ? "1" : "0",
        alike_running: alive(alike) ? "1" : "0",
        other_folder: existsSync(join(box, "Local", "Another.App", "EBWebView", "lockfile")) ? "1" : "0",
        alike_folder: existsSync(join(box, "Local", "io.github.duskxi.spaceknife-beta", "EBWebView", "lockfile")) ? "1" : "0",
      };
      expect("un.SkRemoveData: WebView2 still holds the shell's folder: its processes are ended, another program's never", flagsOf(got, state), {
        ...caches,
        own_ended: "1",
        other_running: "1",
        alike_running: "1",
        other_folder: "1",
        alike_folder: "1",
      });
      for (const child of [other, alike]) child.stdin.end();
      await Promise.all([other, alike].map(exitCode));
    }
    {
      // ends by itself after about 2 s (ping, whose output goes nowhere; the lockfile stays held
      // until then): waited for, not ended (an ended one exits with 1)
      let own;
      const { got } = run("removedata", `/S _?=${sandbox}\\Programs\\SpaceKnife`, { SK_LOGIC_ROAMING_OTHER: "1", SK_LOGIC_WV2_FAKE: "" }, {
        program: uninstaller,
        prepare: (box) => {
          own = webview2(join(box, "Local", "io.github.duskxi.spaceknife", "EBWebView"), "/c ping -n 3 127.0.0.1 >nul &");
        },
      });
      const code = await exitCode(own);
      expect("un.SkRemoveData: WebView2 that goes by itself is waited for", flagsOf(got, { own_exit: String(code) }), { ...caches, own_exit: "0" });
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
  for (const child of fakes) if (child.exitCode === null && child.signalCode === null) child.kill();
  rmSync(sandbox, { recursive: true, force: true, maxRetries: 10, retryDelay: 300 });
}
console.log(failures ? `\nthe installer's logic: ${failures} case(s) failed` : "\nthe installer's logic: ok");
process.exit(failures ? 1 : 0);
