// tools/package_checks.mjs: the remapped Rust flags of a release build, and the checks over the
// packages (no build-machine home folder in any file or archive member, glibc ≤ 2.34 in every ELF).
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import zlib from "node:zlib";

import * as checks from "../package_checks.mjs";

const SCRIPT = fileURLToPath(new URL("../package_checks.mjs", import.meta.url));
const WIN_HOME = "C:\\Users\\lenny";
const HOME = "/home/lenny";
const tmp = () => mkdtempSync(join(tmpdir(), "sk-checks-"));

function put(folder, name, data) {
  mkdirSync(join(folder, name, ".."), { recursive: true });
  writeFileSync(join(folder, name), data);
  return join(folder, name);
}

/** A zip file of [name, data, deflate?] entries; `zip64` writes the end record the ZIP64 way. */
function zip(entries, { zip64 = false } = {}) {
  const locals = [];
  const central = [];
  let offset = 0;
  for (const [name, data, deflate = true] of entries) {
    const n = Buffer.from(name);
    const body = deflate ? zlib.deflateRawSync(data) : data;
    const crc = zlib.crc32(data);
    const head = Buffer.alloc(30);
    head.writeUInt32LE(0x04034b50, 0);
    head.writeUInt16LE(20, 4);
    head.writeUInt16LE(deflate ? 8 : 0, 8);
    head.writeUInt32LE(crc, 14);
    head.writeUInt32LE(body.length, 18);
    head.writeUInt32LE(data.length, 22);
    head.writeUInt16LE(n.length, 26);
    const dir = Buffer.alloc(46);
    dir.writeUInt32LE(0x02014b50, 0);
    dir.writeUInt16LE(20, 4);
    dir.writeUInt16LE(20, 6);
    dir.writeUInt16LE(deflate ? 8 : 0, 10);
    dir.writeUInt32LE(crc, 16);
    dir.writeUInt32LE(body.length, 20);
    dir.writeUInt32LE(data.length, 24);
    dir.writeUInt16LE(n.length, 28);
    dir.writeUInt32LE(offset, 42);
    locals.push(head, n, body);
    central.push(dir, n);
    offset += head.length + n.length + body.length;
  }
  const cd = Buffer.concat(central);
  const tail = [];
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0);
  if (zip64) {
    const rec = Buffer.alloc(56);
    rec.writeUInt32LE(0x06064b50, 0);
    rec.writeBigUInt64LE(44n, 4);
    rec.writeBigUInt64LE(BigInt(entries.length), 24);
    rec.writeBigUInt64LE(BigInt(entries.length), 32);
    rec.writeBigUInt64LE(BigInt(cd.length), 40);
    rec.writeBigUInt64LE(BigInt(offset), 48);
    const loc = Buffer.alloc(20);
    loc.writeUInt32LE(0x07064b50, 0);
    loc.writeBigUInt64LE(BigInt(offset + cd.length), 8);
    loc.writeUInt32LE(1, 16);
    tail.push(rec, loc);
    eocd.writeUInt16LE(0xffff, 8);
    eocd.writeUInt16LE(0xffff, 10);
    eocd.writeUInt32LE(0xffffffff, 12);
    eocd.writeUInt32LE(0xffffffff, 16);
  } else {
    eocd.writeUInt16LE(entries.length, 8);
    eocd.writeUInt16LE(entries.length, 10);
    eocd.writeUInt32LE(cd.length, 12);
    eocd.writeUInt32LE(offset, 16);
  }
  return Buffer.concat([...locals, cd, ...tail, eocd]);
}

/** A ustar archive of {name, data, type, uname, gname, link} entries (type "0" by default). */
function tar(entries) {
  const out = [];
  for (const { name, data = Buffer.alloc(0), type = "0", uname = "", gname = "", link = "" } of entries) {
    const h = Buffer.alloc(512);
    h.write(name, 0);
    h.write("0000644\0", 100);
    h.write("0001750\0", 108);
    h.write("0001750\0", 116);
    h.write(`${data.length.toString(8).padStart(11, "0")}\0`, 124);
    h.write("00000000000\0", 136);
    h.write("        ", 148);
    h.write(type, 156);
    h.write(link, 157);
    h.write("ustar\x0000", 257);
    h.write(uname, 265);
    h.write(gname, 297);
    let sum = 0;
    for (const b of h) sum += b;
    h.write(`${sum.toString(8).padStart(6, "0")}\0 `, 148);
    out.push(h, data, Buffer.alloc((512 - (data.length % 512)) % 512));
  }
  out.push(Buffer.alloc(1024));
  return Buffer.concat(out);
}

/** An ar archive (a deb) of [name, data] members. */
function ar(members) {
  const out = [Buffer.from("!<arch>\n")];
  for (const [name, data] of members) {
    const h = `${`${name}/`.padEnd(16)}${"0".padEnd(12)}${"0".padEnd(6)}${"0".padEnd(6)}${"100644".padEnd(8)}${String(data.length).padEnd(10)}\`\n`;
    out.push(Buffer.from(h, "latin1"), data, Buffer.alloc(data.length % 2, "\n"));
  }
  return Buffer.concat(out);
}

/** An ELF file that names these glibc versions. */
const elf = (...versions) => Buffer.concat([Buffer.from([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1]), Buffer.alloc(57), Buffer.from(versions.map((v) => `GLIBC_${v}\0`).join("")), Buffer.from("libc.so.6\0")]);

const cli = (...args) => spawnSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8" });

test("a release build remaps the home folder, CARGO_HOME, RUSTUP_HOME and the repository", () => {
  const win = checks.remapFlags({ home: WIN_HOME, root: "C:\\Users\\lenny\\src\\SpaceKnife", windows: true });
  assert.deepEqual(win, [
    "--remap-path-prefix=C:\\Users\\lenny=~",
    "--remap-path-prefix=C:\\Users\\lenny\\.cargo=/cargo",
    "--remap-path-prefix=C:\\Users\\lenny\\.rustup=/rustup",
    "--remap-path-prefix=C:\\Users\\lenny\\src\\SpaceKnife=/spaceknife",
    "-Clink-arg=/PDBALTPATH:%_PDB%",
  ]);
  const posix = checks.remapFlags({ home: HOME, cargoHome: "/opt/cargo", rustupHome: "/opt/rustup", root: "/home/lenny/sk", windows: false });
  assert.deepEqual(posix, [
    "--remap-path-prefix=/home/lenny=~",
    "--remap-path-prefix=/opt/cargo=/cargo",
    "--remap-path-prefix=/opt/rustup=/rustup",
    "--remap-path-prefix=/home/lenny/sk=/spaceknife",
  ]);
});

test("the remapped environment keeps Cargo's flags and adds nothing twice", () => {
  const opts = { home: HOME, root: "/home/lenny/sk", windows: false };
  const flags = checks.remapFlags({ ...opts, cargoHome: "/opt/cargo" });
  const env = checks.remappedEnv({ RUSTFLAGS: "-C target-cpu=x86-64-v2  --cfg x", CARGO_HOME: "/opt/cargo", PATH: "/bin" }, opts);
  assert.deepEqual(env.CARGO_ENCODED_RUSTFLAGS.split("\x1f"), ["-C", "target-cpu=x86-64-v2", "--cfg", "x", ...flags]);
  assert.equal(env.PATH, "/bin");
  // the spaceknife-cli build inside `tauri build` runs it again: the same flags
  assert.deepEqual(checks.remappedEnv(env, opts), env);
  // CARGO_ENCODED_RUSTFLAGS wins over RUSTFLAGS, as in Cargo
  const enc = checks.remappedEnv({ CARGO_ENCODED_RUSTFLAGS: "-Cdebuginfo=1", RUSTFLAGS: "ignored" }, opts);
  assert.equal(enc.CARGO_ENCODED_RUSTFLAGS.split("\x1f")[0], "-Cdebuginfo=1");
  assert.ok(!enc.CARGO_ENCODED_RUSTFLAGS.includes("ignored"));
});

test("the home folder is found in every spelling, and only as a whole name", () => {
  const t = tmp();
  const found = (text, home, encoding = "utf8") => {
    const file = put(t, "f.bin", Buffer.concat([Buffer.from("\0\0junk "), Buffer.from(text, encoding), Buffer.from(" junk\0")]));
    return checks.findBuildPaths([file], { homes: [home], user: "" }).problems.length > 0;
  };
  for (const text of [
    "C:\\Users\\lenny\\.cargo\\registry\\src\\x.rs",
    "c:/users/LENNY/src/main.rs",
    '{"p":"C:\\\\Users\\\\lenny\\\\AppData"}',
    "C:\\Users\\lenny",
  ]) {
    assert.ok(found(text, WIN_HOME), text);
  }
  assert.ok(found("C:\\Users\\Lenny\\x.pdb", WIN_HOME, "utf16le"), "UTF-16");
  assert.ok(!found("C:\\Users\\lennyx\\x", WIN_HOME));
  assert.ok(!found("C:\\Users\\lennyx\\x", WIN_HOME, "utf16le"));
  assert.ok(!found("C:\\Users\\runneradmin\\x", WIN_HOME));
  for (const text of ["/home/lenny/.cargo/registry", '"\\/home\\/lenny\\/x"', "/home/lenny"]) assert.ok(found(text, HOME), text);
  assert.ok(!found("/home/lenny2/x", HOME));
  assert.ok(!found("/home/lennyfoo", HOME));
  assert.ok(found("/home/lenny/x", `${HOME}/`), "a trailing slash on the home folder");
  assert.throws(() => checks.homeMatcher(["/"]), /too short/);
});

test("files, zip members, tar.gz members and deb members are searched; links are not followed", (t) => {
  const dir = tmp();
  const pkg = join(dir, "pkg");
  const leak = Buffer.from(`panicked at ${HOME}/.cargo/registry/src/lib.rs:1:1`);
  put(pkg, "clean.txt", "nothing here");
  put(pkg, "bin/spaceknife", Buffer.concat([elf("2.17"), leak]));
  put(
    pkg,
    "SpaceKnife-0.3.5-win-x64-portable.zip",
    zip([
      ["SpaceKnife/spaceknife.exe", Buffer.from(`${HOME}/target/release/deps/spaceknife.pdb`, "utf16le")],
      ["SpaceKnife/licenses/LICENSE.txt", Buffer.from("MIT"), false],
      ["SpaceKnife/ui/index.html", Buffer.from("<!doctype html>")],
    ]),
  );
  put(
    pkg,
    "SpaceKnife-0.3.5-linux-x64-portable.tar.gz",
    zlib.gzipSync(
      tar([
        { name: "SpaceKnife/", type: "5" },
        { name: "SpaceKnife/spaceknife", data: leak },
        { name: "SpaceKnife/README.txt", data: Buffer.from("fine"), uname: "lenny", gname: "lenny" },
        { name: "SpaceKnife/cli", type: "2", link: `${HOME}/bin/cli` },
      ]),
    ),
  );
  const data = zlib.gzipSync(tar([{ name: "./usr/bin/spaceknife-cli", data: leak }]));
  put(pkg, "spaceknife_0.3.5_amd64.deb", ar([["debian-binary", Buffer.from("2.0\n")], ["control.tar.gz", zlib.gzipSync(tar([{ name: "./control", data: Buffer.from("Package: spaceknife\n") }]))], ["data.tar.gz", data]]));
  put(dir, "outside/secret.txt", leak);
  try {
    symlinkSync(join(dir, "outside", "secret.txt"), join(pkg, "link.txt"));
  } catch {
    t.diagnostic("no symbolic links here: the link case is not tested");
  }

  const { problems, files, members } = checks.findBuildPaths([pkg], { homes: [HOME], user: "lenny" });
  const rel = (p) => p.replace(/^.*?pkg\//, "");
  assert.deepEqual(problems.map(rel).sort(), [
    "SpaceKnife-0.3.5-linux-x64-portable.tar.gz (member names): 1",
    "SpaceKnife-0.3.5-linux-x64-portable.tar.gz › SpaceKnife/README.txt: owned by lenny",
    "SpaceKnife-0.3.5-linux-x64-portable.tar.gz › SpaceKnife/spaceknife: 1",
    "SpaceKnife-0.3.5-win-x64-portable.zip › SpaceKnife/spaceknife.exe: 1",
    "bin/spaceknife: 1",
    "spaceknife_0.3.5_amd64.deb › data.tar.gz › ./usr/bin/spaceknife-cli: 1",
  ]);
  assert.equal(files, 5); // the link is not read
  assert.equal(members, 8); // zip 3, tar.gz 2 (files only), deb 3

  // the command line: exit 1 and the findings; a folder without them passes
  const bad = cli("paths", "--home", HOME, "--user", "lenny", pkg);
  assert.equal(bad.status, 1, bad.stderr);
  assert.match(bad.stderr, /home folder \(\/home\/lenny\) or user \(lenny\) is in the package/);
  assert.match(bad.stderr, /data\.tar\.gz › \.\/usr\/bin\/spaceknife-cli: 1/);
  const missing = cli("paths", "--home", HOME, "--user", "lenny", join(pkg, "clean.txt"), join(pkg, "not-built.zip"));
  assert.equal(missing.status, 1, "a target that is not there is an error");
  assert.match(missing.stderr, /not-built\.zip: no such file or folder/);
  const clean = cli("paths", "--home", HOME, "--user", "lenny", join(pkg, "clean.txt"));
  assert.equal(clean.status, 0, clean.stderr);
  assert.match(clean.stdout, /no build-machine paths: 1 files/);
});

test("ZIP64 directories are read, and archives that cannot be read are errors", () => {
  const dir = tmp();
  const z = put(dir, "a.zip", zip([["x/y.txt", Buffer.from(`${HOME}/x`)]], { zip64: true }));
  assert.deepEqual(
    checks.findBuildPaths([z], { homes: [HOME], user: "" }).problems.map((p) => p.replace(/^.*a\.zip/, "a.zip")),
    ["a.zip › x/y.txt: 1"],
  );
  const xz = put(dir, "b.tar.xz", Buffer.from([0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00, 1, 2, 3]));
  assert.throws(() => checks.findBuildPaths([xz], { homes: [HOME] }), /xz-compressed/);
  const r = cli("paths", "--home", HOME, xz);
  assert.equal(r.status, 1);
  assert.match(r.stderr, /xz-compressed/);
});

test("every ELF file of a package needs at most glibc 2.34; other files do not count", () => {
  const dir = tmp();
  const pkg = join(dir, "pkg");
  put(pkg, "usr/bin/spaceknife", elf("2.2.5", "2.17", "2.34"));
  put(pkg, "usr/share/doc/notes.txt", "built against GLIBC_2.99"); // not an ELF file
  put(pkg, "x.tar.gz", zlib.gzipSync(tar([{ name: "lib/libgcc_s.so.1", data: elf("2.14", "2.35") }])));
  assert.deepEqual(checks.glibcNeeded(elf("2.3.4", "2.28", "2.4")), [2, 28]);
  assert.equal(checks.glibcNeeded(Buffer.from("GLIBC_2.40")), null);

  const { problems, worst, elves } = checks.findNewGlibc([pkg]);
  assert.deepEqual(problems.map((p) => p.replace(/^.*?pkg\//, "")), ["x.tar.gz › lib/libgcc_s.so.1 needs GLIBC_2.35"]);
  assert.deepEqual(worst, [2, 35]);
  assert.equal(elves, 2);
  assert.deepEqual(checks.findNewGlibc([pkg], { max: "2.35" }).problems, []);

  const r = cli("glibc", pkg);
  assert.equal(r.status, 1);
  assert.match(r.stderr, /libgcc_s\.so\.1 needs GLIBC_2\.35/);
  const ok = cli("glibc", join(pkg, "usr"));
  assert.equal(ok.status, 0, ok.stderr);
  assert.match(ok.stdout, /1 ELF files, the newest needs GLIBC_2\.34/);
  // a folder without any ELF file is the wrong folder, not a pass
  const none = cli("glibc", join(pkg, "usr", "share"));
  assert.equal(none.status, 1);
  assert.match(none.stderr, /no ELF file/);
  assert.equal(cli("glibc").status, 2);
});
