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
  assert.ok(at(section.slice(minimum), 'Abort "$(webview2AbortError)"') >= 0);
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
