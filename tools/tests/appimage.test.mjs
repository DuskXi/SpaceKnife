// tools/appimage.mjs: the AppImage's update information (ELF section .upd_info). zsyncmake runs in
// the Electron shell's CI build.
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";

import { AppImageError, UPDATE_INFO, embedUpdateInfo, readUpdateInfo } from "../appimage.mjs";

/** A little-endian ELF64 file with an empty .upd_info section of `size` bytes and some payload
 * after it (an AppImage's runtime, then its squashfs). */
function elf({ size = 1024, sections = [".upd_info", ".sha256_sig"] } = {}) {
  const names = Buffer.from(`\0${[...sections, ".shstrtab"].join("\0")}\0`, "latin1");
  const bodies = sections.map(() => Buffer.alloc(size));
  let at = 64;
  const placed = bodies.map((b) => {
    const here = at;
    at += b.length;
    return here;
  });
  const strAt = at;
  at += names.length;
  const shoff = at;
  const count = sections.length + 2;
  const table = Buffer.alloc(64 * count);
  let nameOff = 1;
  sections.forEach((s, i) => {
    const e = (i + 1) * 64;
    table.writeUInt32LE(nameOff, e);
    table.writeBigUInt64LE(BigInt(placed[i]), e + 24);
    table.writeBigUInt64LE(BigInt(size), e + 32);
    nameOff += s.length + 1;
  });
  const se = (count - 1) * 64;
  table.writeUInt32LE(nameOff, se);
  table.writeBigUInt64LE(BigInt(strAt), se + 24);
  table.writeBigUInt64LE(BigInt(names.length), se + 32);
  const head = Buffer.alloc(64);
  head.writeUInt32BE(0x7f454c46, 0);
  head[4] = 2; // ELF64
  head[5] = 1; // little endian
  head.writeBigUInt64LE(BigInt(shoff), 0x28);
  head.writeUInt16LE(64, 0x3a);
  head.writeUInt16LE(count, 0x3c);
  head.writeUInt16LE(count - 1, 0x3e);
  return Buffer.concat([head, ...bodies, names, table, Buffer.from("hsqs the squashfs image")]);
}

test("the update information goes into .upd_info and nowhere else", () => {
  const dir = mkdtempSync(join(tmpdir(), "sk-appimage-"));
  after(() => rmSync(dir, { recursive: true, force: true }));
  const file = join(dir, "SpaceKnife-0.3.5-x86_64.AppImage");
  const before = elf();
  writeFileSync(file, before);
  assert.equal(readUpdateInfo(file), "");
  embedUpdateInfo(file);
  assert.equal(readUpdateInfo(file), UPDATE_INFO);
  const written = readFileSync(file);
  assert.equal(written.length, before.length);
  const changed = [...written].map((b, i) => (b !== before[i] ? i : -1)).filter((i) => i >= 0);
  assert.ok(changed.length > 0 && changed.every((i) => i >= 64 && i < 64 + 1024), "only the section's bytes change");
  // again: the same file
  embedUpdateInfo(file);
  assert.deepEqual(readFileSync(file), written);
});

test("0.3.4's update information, word for word", () => {
  // v0.3.4 tools/build_package.py APPIMAGE_UPDATE_INFO
  assert.equal(UPDATE_INFO, "gh-releases-zsync|DuskXi|SpaceKnife|latest-all|SpaceKnife-*-x86_64.AppImage.zsync");
});

test("a runtime without the section, or one too small, is refused", () => {
  const dir = mkdtempSync(join(tmpdir(), "sk-appimage-"));
  after(() => rmSync(dir, { recursive: true, force: true }));
  const none = join(dir, "none.AppImage");
  writeFileSync(none, elf({ sections: [".text"] }));
  assert.throws(() => embedUpdateInfo(none), (e) => e instanceof AppImageError && e.message.includes(".upd_info"));
  const small = join(dir, "small.AppImage");
  writeFileSync(small, elf({ size: 16 }));
  assert.throws(() => embedUpdateInfo(small), /longer than \.upd_info/);
  const text = join(dir, "text.AppImage");
  writeFileSync(text, "#!/bin/sh\n");
  assert.throws(() => readUpdateInfo(text), AppImageError);
});
