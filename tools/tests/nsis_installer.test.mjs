// The forked NSIS template's own rules (apps/desktop/src-tauri/windows: installer.nsi, rendered by
// Tauri's bundler, and spaceknife.nsh), read from the source: the parts the logic test
// (packaging/nsis-logic.mjs, which runs spaceknife.nsh's functions for real on Windows) can't
// reach, because they are the template's own sections or touch the registry.
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const windows = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "apps", "desktop", "src-tauri", "windows");
const read = (name) => readFileSync(join(windows, name), "utf8");
const installer = read("installer.nsi");
const helpers = read("spaceknife.nsh");

/** The code lines of a script (comments and blank lines out), each with the Function or Section
 *  it is in ("" outside any). SK_SHARED's functions are named `${UN}Name` in the source. */
function codeLines(text) {
  const out = [];
  let inside = "";
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line || line.startsWith(";") || line.startsWith("#")) continue;
    const begin = /^(Function|Section)\s+("?)([^"\s]+)\2/.exec(line);
    if (begin) inside = begin[3];
    out.push({ line, inside });
    if (/^(FunctionEnd|SectionEnd)\b/.test(line)) inside = "";
  }
  return out;
}

/** The code lines of one Function or Section. */
function body(text, name) {
  const lines = codeLines(text).filter((l) => l.inside === name).map((l) => l.line);
  assert.ok(lines.length > 0, `no ${name}`);
  return lines;
}

/** The index of the first line of `lines` that contains `what` (-1 when none). */
const at = (lines, what) => lines.findIndex((l) => l.includes(what));

/** `first` and `then` are both in `lines`, `first` before `then` (`at` alone gives -1 for a line
 *  that is not there, which comes before every other). */
function before(lines, first, then, message) {
  const i = at(lines, first);
  const j = at(lines, then);
  assert.ok(i >= 0, `no ${first}`);
  assert.ok(j >= 0, `no ${then}`);
  assert.ok(i < j, message ?? `${first} comes after ${then}`);
}

test("nothing deletes a folder as a whole but the uninstaller's data and cache folders", () => {
  const allowed = /^"\$\{SK_(LOCALAPPDATA|APPDATA)\}\\(\$\{SK_DATA\}|\$\{BUNDLEID\}|spaceknife-electron|spaceknife-electron-updater)(\\(electron|Cache|updates))?"$/;
  const found = [];
  for (const name of readdirSync(windows).filter((n) => /\.(nsi|nsh)$/.test(n))) {
    for (const { line, inside } of codeLines(read(name))) {
      const m = /^RMDir\s+(?:\/REBOOTOK\s+)?\/r\s+(.+)$/i.exec(line);
      if (m && !(inside === "un.SkRemoveData" && allowed.test(m[1]))) found.push(`${name} (${inside || "outside a function"}): ${line}`);
    }
  }
  assert.deepEqual(found, [], "a recursive delete outside SpaceKnife's own data folders");
});

test("the uninstaller removes SpaceKnife's own folders by 0.3.4's rules", () => {
  // the user's own folders in the installer; only the logic test points them elsewhere
  assert.match(helpers, /^!ifndef SK_LOCALAPPDATA\n {2}!define SK_LOCALAPPDATA "\$LOCALAPPDATA"\n!endif$/m);
  assert.match(helpers, /^!ifndef SK_APPDATA\n {2}!define SK_APPDATA "\$APPDATA"\n!endif$/m);
  assert.doesNotMatch(installer, /SK_(LOCAL)?APPDATA/);
  const remove = body(helpers, "un.SkRemoveData");
  assert.deepEqual(remove.filter((l) => /\$(LOCALAPPDATA|APPDATA)\b/.test(l)), [], "un.SkRemoveData names the shell's folders itself");
  // electron-builder's copy of its installer goes in an update too, nothing else does
  const update = at(remove, "${If} $UpdateMode = 1");
  assert.ok(at(remove, 'RMDir /r "${SK_LOCALAPPDATA}\\spaceknife-electron-updater"') < update);
  assert.equal(remove[update + 2], "Return");
  assert.equal(remove.findIndex((l, i) => i < update && /^RMDir/.test(l) && !l.includes("spaceknife-electron-updater")), -1);
  // %APPDATA%\SpaceKnife: when empty, and as a whole only with --delete-app-data (not the box)
  assert.ok(at(remove, 'RMDir "${SK_APPDATA}\\${SK_DATA}"') > update);
  const whole = remove.filter((l) => l === 'RMDir /r "${SK_APPDATA}\\${SK_DATA}"');
  assert.equal(whole.length, 1);
  const flag = at(remove, "${If} $SkDeleteAppData = 1");
  assert.ok(flag > at(remove, "${If} $SkDeleteData = 1") && at(remove, 'RMDir /r "${SK_APPDATA}\\${SK_DATA}"') > flag);
  assert.ok(at(remove, 'RMDir /r "${SK_LOCALAPPDATA}\\${SK_DATA}"') < flag);
  // the box is $SkDeleteData only; --delete-app-data sets both, an update neither
  assert.equal(at(body(installer, "un.SkDataPageLeave"), "SkDeleteAppData"), -1);
  assert.equal(at(body(installer, "un.SkDataPage"), "SkDeleteAppData"), -1);
});

test("the shell's folder goes once its WebView2 processes have, and only those are ended", () => {
  const remove = body(helpers, "un.SkRemoveData");
  const shell = 'RMDir /r "${SK_LOCALAPPDATA}\\${BUNDLEID}"';
  // waited for (then ended) before the folder is tried at all: one still starting writes its
  // profile into a folder deleted a moment before (the Windows Server 2022 runner, WebView2 131)
  const first = at(remove, "Call un.SkEndWebView2In");
  assert.ok(first >= 0 && first < at(remove, shell), "the folder is tried before its WebView2 processes are waited for");
  assert.equal(remove[first - 1], "StrCpy $R1 ${SK_WEBVIEW2_POLLS}");
  assert.equal(remove[first - 2], 'StrCpy $R0 "${SK_LOCALAPPDATA}\\${BUNDLEID}"');
  // then a few tries, each followed by a look for one that came up meanwhile (ended, no wait)
  const tries = remove.slice(first + 1);
  const again = at(tries, "Call un.SkEndWebView2In");
  assert.ok(at(tries, shell) >= 0 && at(tries, shell) < again && tries[again - 1] === "StrCpy $R1 0");
  assert.ok(at(tries, "${If} $R9 >= ${SK_REMOVE_TRIES}") > again, "the tries are not bounded");
  assert.match(helpers, /^!ifndef SK_WEBVIEW2_POLLS\n {2}!define SK_WEBVIEW2_POLLS 40\n!endif$/m, "10 s for them to go by themselves");
  assert.match(helpers, /^!ifndef SK_WEBVIEW2_SCAN\n {2}!define SK_WEBVIEW2_SCAN un\.SkWebView2In\n!endif$/m);
  assert.doesNotMatch(installer, /SK_WEBVIEW2_(POLLS|SCAN)|SK_REMOVE_TRIES/, "installer.nsi changes the wait");
  // waited for (polls of 250 ms) before they are ended, the end bounded
  const wait = body(helpers, "un.SkEndWebView2In");
  const poll = at(wait, "${DoWhile} $R8 < $R9");
  assert.ok(poll >= 0 && wait[poll + 1] === "Sleep 250" && at(wait, 'StrCpy $R2 "end"') > poll, "they are ended without being waited for");
  assert.equal(wait.filter((l) => l === "Call ${SK_WEBVIEW2_SCAN}").length, 3);
  // which ones: msedgewebview2.exe whose --user-data-dir is the folder itself or one in it, by the
  // command line read from the handle that ends it; never another program's
  const ours = body(helpers, "un.SkWebView2Ours");
  const name = at(ours, '${If} $R5 == "msedgewebview2.exe"');
  const dir = at(ours, "Call un.SkUserDataDir");
  const inside = at(ours, '${OrIf} $R4 == "inside"');
  assert.ok(name >= 0 && name < dir && dir < inside);
  assert.equal(ours[inside - 1], '${If} $R4 == "same"');
  const wv2 = body(helpers, "un.SkWebView2In");
  const read = at(wv2, "ntdll::NtQueryInformationProcess(p r6");
  const judged = at(wv2, "Call un.SkWebView2Ours");
  const kill = at(wv2, "TerminateProcess");
  assert.ok(at(wv2, '${If} $4 == "msedgewebview2.exe"') < read && read < judged && judged < kill);
  assert.equal(wv2.filter((l) => l.includes("TerminateProcess")).length, 1);
  assert.equal(wv2[judged + 1], "${If} $R4 = 1");
  assert.equal(wv2[judged + 2], '${AndIf} $R2 == "end"', "ended without being asked to");
  assert.ok(wv2.slice(judged, kill).every((l) => !/OpenProcess/.test(l)), "ended on another handle than the one its command line came from");
});

test("the files go by name: the list an installation wrote, the next update and the uninstaller read", () => {
  const install = body(installer, "Install");
  // step 5, an update of a forked installation in place: its own list
  const fork = at(install, '${If} $SkOldKind == "fork"');
  assert.ok(fork >= 0 && at(install.slice(fork), "Call SkRemoveListed") >= 0);
  assert.ok(at(install, "Call SkRemoveListed") < at(install, "Call SkPlaceAll"));
  // the list is written once the files are in place
  assert.ok(at(install, "Call SkPlaceAll") < at(install, "Call SkWriteManifest"));
  assert.ok(at(install, 'WriteUninstaller "$INSTDIR\\uninstall.exe"') < at(install, "Call SkWriteManifest"));
  assert.ok(at(body(installer, "Uninstall"), "Call un.SkRemoveListed") >= 0);
  // installer.nsi gives spaceknife.nsh the folders, the deepest first (the bundler's
  // resources_ancestors), for the list and the staging folder
  assert.match(installer, /!macro SK_EACH_ANCESTOR OP\n\s*\{\{#each resources_ancestors\}\}\n\s*!insertmacro \$\{OP\} "\{\{this\}\}"\n\s*\{\{\/each\}\}\n!macroend/);
});

test("a folder taken on the directory page gets \\SpaceKnife when it has no SpaceKnife in it", () => {
  const leave = body(installer, "SkLeaveDirectory");
  assert.ok(at(leave, "Call SkAppendName") >= 0, "SkLeaveDirectory does not call SkAppendName");
  assert.ok(at(leave, "Call SkAppendName") < at(leave, "Call SkUnsafeDir"), "the folder is checked before the name is added");
});

test("/D= is the folder of a new installation in both scopes", () => {
  for (const name of ["SkPageScopeLeave", ".onInit"]) {
    const lines = body(installer, name);
    assert.ok(at(lines, "Call SkChooseFolder") > at(lines, "Call SkFindPrevious"), `${name}: SkChooseFolder after SkFindPrevious`);
    assert.equal(at(lines, "Call SkDefaultDir") < 0 || name === ".onInit", true, `${name} calls SkDefaultDir itself`);
  }
  // .onInit's own SkDefaultDir calls are for a folder that can't be used, after the choice
  const init = body(installer, ".onInit");
  assert.ok(init.every((l, i) => !l.includes("Call SkDefaultDir") || i > at(init, "Call SkChooseFolder")));
  const choose = body(helpers, "SkChooseFolder");
  assert.ok(at(choose, "$SkOldDir") < at(choose, "$SkDirFromCmd") && at(choose, "$SkDirFromCmd") < at(choose, "Call SkDefaultDir"));
});

test("an older WebView2 that can't be updated stops the installer before anything changes", () => {
  const section = body(installer, "WebView2");
  const minimum = at(section, "Call SkWebView2Minimum");
  assert.ok(minimum >= 0, "the WebView2 section does not check the minimum version");
  assert.ok(at(section.slice(minimum), '${SkAbort} "$(webview2AbortError)"') >= 0);
  assert.equal(at(section, "IDIGNORE"), -1, "a silent run ignores the failed update");
  assert.equal(at(section, "Quit"), -1, "Quit skips .onInstFailed, which starts the installed version again");
  // the section comes before Install, where the installed copy is first touched
  assert.ok(installer.indexOf("Section WebView2") < installer.indexOf("Section Install"));
});

test("0.3.4's folder stays on record until this version's keys replace it", () => {
  const install = body(installer, "Install");
  const removed = at(install, "Call SkRemoveElectronBuilder");
  const record = at(install, 'WriteRegStr SHCTX "${SK_INSTKEY}" "InstallLocation" "$SkOldDir"');
  const placed = at(install, "Call SkPlaceAll");
  assert.ok(removed >= 0 && removed < record && record < placed, "the record of 0.3.4's folder is not written again after its uninstaller");
  assert.equal(at(install, 'DeleteRegKey SHCTX "${SK_INSTKEY}"'), -1, "the installer deletes the record of the folder");
  assert.ok(at(install, 'WriteRegStr SHCTX "${SK_INSTKEY}" "InstallLocation" "$INSTDIR"') > placed);
});

test("the uninstaller deletes the GUID keys only when they name its own folder", () => {
  const uninstall = body(installer, "Uninstall");
  const owned = at(uninstall, "Call un.SkKeysOurs");
  assert.ok(owned >= 0, "the uninstaller does not ask whose the keys are");
  assert.equal(uninstall[owned + 1], "${If} $R0 = 1");
  const keys = uninstall.map((l, i) => [l, i]).filter(([l]) => /^DeleteRegKey SHCTX "\$\{SK_(UN)?INSTKEY\}"$/.test(l));
  assert.equal(keys.length, 2);
  const otherwise = uninstall.findIndex((l, i) => i > owned && l === "${Else}");
  for (const [, i] of keys) assert.ok(i > owned + 1 && i < otherwise, "a GUID key is deleted whatever folder it names");
  before(uninstall, 'ReadRegStr $R0 SHCTX "${SK_INSTKEY}" "InstallLocation"', "Call un.SkKeysOurs");
  // its scope: the hive whose key names this folder, never HKCU just because HKLM's does not
  assert.ok(at(body(helpers, "un.SkDetectScope"), "Call un.SkScopeFor") >= 0);
});

test("the new program is asked whether it can start before anything of the installed one is touched", () => {
  const install = body(installer, "Install");
  const check = at(install, "Call SkStartupCheck");
  before(install, "Call SkCheckStaged", "Call SkStartupCheck", "asked before the staged files are checked");
  before(install, "Call SkStartupCheck", "Call SkCloseIn", "asked after SpaceKnife is closed");
  before(install, "Call SkStartupCheck", "Call SkRemoveElectronBuilder");
  assert.equal(install[check - 1], 'StrCpy $R0 "$SkStage\\${MAINBINARYNAME}.exe"');
  assert.ok(at(install.slice(check), '${SkAbort} "$(skCantStart)"') > 0);
  // without the system's error boxes (a missing DLL, a crash), as the portable swap's check runs
  // it, and this process's own error mode back afterwards
  const startup = body(helpers, "SkStartupCheck");
  const exec = "nsExec::ExecToStack /TIMEOUT=${SK_STARTUP_CHECK_MS} '\"$R0\" --startup-check'";
  before(startup, "kernel32::SetErrorMode(i 3) i .r3", exec, "the program may show the system's error boxes");
  before(startup, "kernel32::SetErrorMode(i r2)", exec, "the program may show the system's error boxes");
  assert.equal(startup[at(startup, "kernel32::SetErrorMode(i 3) i .r3") + 1], "IntOp $2 $3 | 3");
  before(startup, exec, "kernel32::SetErrorMode(i r3)", "the installer's own error mode is not put back");
  // its own reasons in the installer's language (skCantStart shows them)
  for (const name of ["skCheckTimeout", "skCheckNotRun", "skCheckExit"]) assert.ok(at(startup, `"$(${name})"`) >= 0, `no $(${name})`);
});

test("every text of SpaceKnife's own that the installer shows is there in each of its languages", () => {
  const used = new Set([...`${installer}\n${helpers}`.matchAll(/\$\((sk\w+)\)/g)].map((m) => m[1]));
  assert.ok(used.size > 10);
  const missing = [];
  for (const name of used) {
    for (const lang of ["LANG_ENGLISH", "LANG_SIMPCHINESE"]) {
      if (!installer.includes(`\nLangString ${name} \${${lang}} "`)) missing.push(`${name} (${lang})`);
    }
  }
  assert.deepEqual(missing, []);
});

test("a failed silent update says why, and nothing half placed is started again", () => {
  // every stop of the two sections goes through SkAbort (the text SkFailed shows)
  for (const name of ["WebView2", "Install"]) {
    const bare = body(installer, name).filter((l) => /^Abort\b/.test(l));
    assert.deepEqual(bare, [], `${name}: an Abort without SkAbort`);
  }
  assert.deepEqual(body(installer, ".onInstFailed"), ["Function .onInstFailed", "Call SkFailed", "FunctionEnd"]);
  const failed = body(helpers, "SkFailed");
  before(failed, "Call SkRestartPrevious", "MessageBox", "the installed version is started before the message");
  assert.ok(at(failed, "Call SkFailureLevel") >= 0);
  const silent = at(failed, "${If} ${Silent}");
  assert.equal(failed[silent + 1], "${AndIf} $UpdateMode = 1", "a message box in a silent run that is no update");
  before(failed, "${If} ${Silent}", "kernel32::ReleaseMutex");
  before(failed, "kernel32::ReleaseMutex", "MessageBox", "the mutex is held while the message waits");
  assert.ok(at(failed, "MB_TOPMOST") >= 0);
  const level = body(helpers, "SkFailureLevel");
  assert.equal(level[at(level, "${If} $SkOldGone = 1") + 1], "SetErrorLevel 3");
  // the outer instance: the elevated copy's 3 means nothing to start again
  const init = body(installer, ".onInit");
  before(init, "Call SkRunElevated", "Call SkAfterElevatedFailed");
  assert.equal(at(init, "Call SkRestartPrevious"), -1);
  const after = body(helpers, "SkAfterElevatedFailed");
  before(after, "${If} $0 = 3", "${Else}");
  before(after, "${Else}", "Call SkRestartPrevious");
  // silent runs show a banner while WebView2 downloads; EdgeUpdate asks for rights only as an administrator
  const webview2 = body(installer, "WebView2");
  const exec = at(webview2, "ExecWait \"$6 ${WEBVIEW2INSTALLERARGS} /install\" $1");
  assert.equal(webview2[exec - 1], '!insertmacro SK_BANNER_SHOW "$(skBannerWebView2)"');
  assert.equal(webview2[exec + 1], "!insertmacro SK_BANNER_HIDE");
  const update = body(helpers, "SkWebView2Update");
  assert.ok(at(update, "&needsadmin=$2") >= 0 && at(update, "&needsadmin=true") < 0);
  assert.equal(update[at(update, "${If} $SkIsAdmin = 1") + 1], 'StrCpy $2 "true"');
});

test("the UAC prompt belongs to a window in front, which goes only once it is answered", () => {
  const leave = body(installer, "SkPageScopeLeave");
  assert.equal(at(leave, "HideWindow"), -1, "the wizard hides before the prompt");
  before(leave, "StrCpy $SkFromWizard 1", "Call SkRunElevated");
  const run = body(helpers, "${UN}SkRunElevated");
  const shell = at(run, "shell32::ShellExecuteExW");
  before(run, "user32::SetForegroundWindow(p r5)", "shell32::ShellExecuteExW", "the prompt's window is not brought to the front first");
  before(run, "shell32::ShellExecuteExW", "user32::ShowWindow(p r5, i 0)", "the window goes before the prompt is answered");
  assert.ok(at(run, '*(&l4, i 0x140, p r5, w "runas"') >= 0, "the prompt's owner window");
  // .onInit has no window of its own yet: a banner
  const init = body(installer, ".onInit");
  const elevate = at(init, "Call SkRunElevated");
  before(init, 'Banner::show "$(skBannerInstalling)"', "Call SkRunElevated");
  before(init, "Pop $SkOwnerHwnd", "Call SkRunElevated");
  assert.ok(init.findIndex((l, i) => i > elevate && l === "Banner::destroy") > elevate);
  // the elevated copy of the wizard skips the pages the outer instance showed
  const skip = body(installer, "SkSkipIfPassiveOrUpdate");
  assert.ok(at(skip, "${If} $SkInner = 1") >= 0 && skip[at(skip, "${If} $SkInner = 1") + 1] === "${AndIf} $SkFromWizard = 1");
});

test("the installer starts in its own folder, shows a wizard started hidden, and logs line by line", () => {
  const init = body(installer, ".onInit");
  assert.equal(init[1], "SetOutPath $TEMP", "the folder it was started in is kept by the outer instance");
  const hidden = at(init, "Call SkShowIfStartedHidden");
  assert.equal(init[hidden - 1], "${IfNot} ${Silent}");
  before(init, "Call SkShowIfStartedHidden", "Call SkRunElevated");
  const log = body(helpers, "${UN}SkLogLine");
  before(log, 'FileOpen $R1 "${SK_LOG_FILE}" a', "FileWriteUTF16LE");
  before(log, "FileWriteUTF16LE", "FileClose $R1");
  assert.doesNotMatch(helpers, /SkLogFile/);
});

test("a relocated installation's pins point at the new program", () => {
  const install = body(installer, "Install");
  const pins = install.map((l, i) => [l, i]).filter(([l]) => l === "Call SkMigratePins").map(([, i]) => i);
  assert.equal(pins.length, 2);
  assert.equal(install[pins[0] - 1], "Call SkMigrateShortcut");
  const migrate = body(helpers, "SkMigratePins");
  before(migrate, "SetShellVarContext current", "Call SkMigrateShortcutsIn");
  before(migrate, "Call SkMigrateShortcutsIn", "Call SkRestoreContext");
  assert.ok(at(migrate, "User Pinned\\TaskBar") >= 0 && at(migrate, "User Pinned\\StartMenu") >= 0);
  assert.ok(at(migrate, '${AndIf} $R1 != "$INSTDIR\\${MAINBINARYNAME}.exe"') >= 0, "pins of an update in place are rewritten");
});

test("0.3.4 without its registry keys is found by its files", () => {
  const find = body(helpers, "SkFindPrevious");
  const files = at(find, "Call SkFindByFiles");
  assert.ok(files > at(find, "${Else}") && at(find, "Call ${SK_DEFAULT_DIR_OF}") === files - 1);
  const eb = body(helpers, "SkIsEbFolder");
  assert.ok(at(eb, '${AndIf} ${FileExists} "$R0\\${SK_EB_UNINSTALLER}"') >= 0 && at(eb, '${AndIf} ${FileExists} "$R0\\resources\\app.asar"') >= 0, "both of 0.3.4's files");
  assert.ok(at(body(helpers, "SkDetectScope"), "Call SkIsEbFolder") >= 0);
});

test("files-0.3.4.nsh lists 0.3.4's files and every folder they are in, the deepest first", () => {
  const text = read("files-0.3.4.nsh");
  const list = (macro) => {
    const m = new RegExp(`!macro ${macro} OP\\n([\\s\\S]*?)!macroend`).exec(text);
    assert.ok(m, macro);
    return [...m[1].matchAll(/!insertmacro \$\{OP\} "([^"]+)"/g)].map((x) => x[1]);
  };
  const files = list("SK_034_EACH_FILE");
  const dirs = list("SK_034_EACH_DIR");
  assert.equal(files.length, 205); // app-64.7z's 204 and the uninstaller
  assert.equal(dirs.length, 60);
  for (const f of ["SpaceKnife.exe", "SpaceKnife.com", "Uninstall SpaceKnife.exe", "resources\\app.asar", "resources\\backend\\spaceknife-backend.exe", "locales\\zh-CN.pak"]) assert.ok(files.includes(f), f);
  const depth = (p) => p.split("\\").length;
  assert.deepEqual([...dirs].sort((a, b) => depth(b) - depth(a)).map(depth), dirs.map(depth), "folders not deepest first");
  for (const f of files) {
    const parts = f.split("\\");
    for (let i = 1; i < parts.length; i++) assert.ok(dirs.includes(parts.slice(0, i).join("\\")), `${f}: its folder is not listed`);
  }
  for (const p of [...files, ...dirs]) assert.doesNotMatch(p, /^\\|:|\.\.|\/|\$/, p);
  assert.ok(helpers.includes('!include "${__FILEDIR__}\\files-0.3.4.nsh"'));
});
