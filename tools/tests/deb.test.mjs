// tools/deb.mjs: the Tauri deb under 0.3.4's package name. The repacking itself (dpkg-deb) and the
// upgrade from the real 0.3.4 package run in CI (packaging/update-from-034.mjs deb).
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";

import { DebError, POSTINST, controlFor, desktopEntry, md5sums } from "../deb.mjs";

// what Tauri 2.12's bundler writes (tauri-bundler linux/debian.rs)
const TAURI_CONTROL = `Package: space-knife
Version: 0.3.5
Architecture: amd64
Installed-Size: 61234
Maintainer: DuskXi
Section: utils
Priority: optional
Homepage: https://github.com/DuskXi/SpaceKnife
Depends: libwebkit2gtk-4.1-0, libgtk-3-0
Description: See what fills your drives
 SpaceKnife scans drives and folders and shows what fills them.
`;

test("the package takes 0.3.4's name and pushes out a test build under Tauri's", () => {
  const out = controlFor(TAURI_CONTROL);
  assert.match(out, /^Package: spaceknife$/m);
  assert.match(out, /^Conflicts: space-knife$/m);
  assert.match(out, /^Replaces: space-knife$/m);
  // the rest as Tauri wrote it, the description's continuation line too
  for (const line of ["Version: 0.3.5", "Depends: libwebkit2gtk-4.1-0, libgtk-3-0", " SpaceKnife scans drives and folders and shows what fills them."]) {
    assert.ok(out.includes(`${line}\n`), line);
  }
  assert.ok(out.indexOf("Conflicts:") > out.indexOf("Depends:"));
  // already repacked: the same again
  assert.equal(controlFor(out), out);
  assert.throws(() => controlFor("Package: something-else\n"), DebError);
});

test("the desktop entry has 0.3.4's name and texts and Tauri's program", () => {
  const tauri = "[Desktop Entry]\nCategories=Utility\nComment=See what fills your drives\nExec=spaceknife\nStartupWMClass=spaceknife\nIcon=spaceknife\nName=SpaceKnife\nTerminal=false\nType=Application\n";
  const out = desktopEntry(tauri);
  assert.match(out, /^\[Desktop Entry\]\n/);
  for (const line of ["Exec=spaceknife", "Icon=spaceknife", "StartupWMClass=spaceknife", "GenericName=Disk Space Analyzer", "GenericName[zh_CN]=磁盘空间分析", "Keywords=disk;space;usage;treemap;storage;cleanup;磁盘;空间;", "Categories=Utility;"]) {
    assert.ok(out.split("\n").includes(line), line);
  }
  assert.throws(() => desktopEntry("[Desktop Entry]\nName=x\n"), DebError);
});

test("postinst takes out only 0.3.4's alternative", () => {
  assert.match(POSTINST, /^#!\/bin\/sh\n/);
  assert.ok(POSTINST.includes("update-alternatives --remove spaceknife /opt/SpaceKnife/spaceknife"));
  assert.ok(!/^\s*rm\s/m.test(POSTINST), "it never deletes a file itself");
  assert.ok(!POSTINST.includes("--remove-all"));
});

test("md5sums lists every file but the control files", () => {
  const root = mkdtempSync(join(tmpdir(), "sk-deb-test-"));
  after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "DEBIAN"));
  mkdirSync(join(root, "usr", "bin"), { recursive: true });
  writeFileSync(join(root, "DEBIAN", "control"), "x");
  writeFileSync(join(root, "usr", "bin", "spaceknife"), "abc");
  assert.equal(md5sums(root), "900150983cd24fb0d6963f7d28e17f72  usr/bin/spaceknife\n");
});
