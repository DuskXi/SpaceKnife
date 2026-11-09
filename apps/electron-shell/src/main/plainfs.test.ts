import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { rm } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";

import { plainRm, removeTree } from "./plainfs";

/** An asar archive as @electron/asar writes one: the header's size and the header (JSON) as
 *  Chromium pickles, then the files' bytes. */
function asarArchive(files: Record<string, string>): Buffer {
  const header: { files: Record<string, { size: number; offset: string }> } = { files: {} };
  const bodies: Buffer[] = [];
  let offset = 0;
  for (const [name, text] of Object.entries(files)) {
    const body = Buffer.from(text);
    header.files[name] = { size: body.length, offset: String(offset) };
    offset += body.length;
    bodies.push(body);
  }
  const json = Buffer.from(JSON.stringify(header));
  const padded = Math.ceil(json.length / 4) * 4;
  const head = Buffer.alloc(8 + padded);
  head.writeUInt32LE(4 + padded, 0);
  head.writeUInt32LE(json.length, 4);
  json.copy(head, 8);
  const size = Buffer.alloc(8);
  size.writeUInt32LE(4, 0);
  size.writeUInt32LE(head.length, 4);
  return Buffer.concat([size, head, ...bodies]);
}

/** An updates folder's staging folder as the shell unpacks an update of the Electron shell's
 *  portable copy there: its resources/app.asar a real archive. */
function staging(dir: string): string {
  const resources = join(dir, "SpaceKnife-0.3.6-linux-x64-portable", "resources");
  mkdirSync(join(resources, "backend"), { recursive: true });
  writeFileSync(join(resources, "app.asar"), asarArchive({ "package.json": '{"name":"spaceknife"}', "index.js": "1" }));
  writeFileSync(join(resources, "backend", "spaceknife-backend"), "the swap helper");
  return join(resources, "app.asar");
}

/** The Electron binary, when it is installed (CI's unit tests skip its download). */
function electronBinary(): string | null {
  try {
    const path = createRequire(import.meta.url)("electron") as unknown;
    return typeof path === "string" && existsSync(path) ? path : null;
  } catch {
    return null;
  }
}

describe("plainfs", () => {
  const dirs: string[] = [];
  const work = () => {
    const d = mkdtempSync(join(tmpdir(), "sk-plainfs-"));
    dirs.push(d);
    return d;
  };
  afterEach(() => {
    for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  });

  it("is Node's own rm outside Electron and original-fs's inside it", () => {
    expect(plainRm({})).toBe(rm);
    const asked: string[] = [];
    const theirs = async () => {};
    expect(plainRm({ electron: "44.9.3" }, (id) => (asked.push(id), { promises: { rm: theirs } }))).toBe(theirs);
    expect(asked).toEqual(["original-fs"]);
  });

  it("removes a folder and the .asar files in it; a folder that is not there is no error", async () => {
    const dir = join(work(), "staging-0.3.6");
    staging(dir);
    await removeTree(dir);
    expect(existsSync(dir)).toBe(false);
    await expect(removeTree(dir)).resolves.toBeUndefined();
  });

  // Electron's own fs as the shell's main process has it (ELECTRON_RUN_AS_NODE keeps its .asar
  // handling): its rm fails on a staging folder holding an archive, removeTree does not
  const electron = electronBinary();
  it.skipIf(!electron)("removes in Electron what Electron's fs cannot", () => {
    const dir = work();
    // two folders: on Windows the archive Electron's fs opened stays open in that process
    const theirs = join(dir, "staging-0.3.6");
    const asar = staging(theirs);
    const ours = join(dir, "staging-0.3.7");
    staging(ours);
    const script = join(dir, "remove.mjs");
    writeFileSync(
      script,
      `import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
const [mod, theirs, asar, ours] = process.argv.slice(2);
const fs = createRequire(import.meta.url)("fs");
const said = { folder: fs.statSync(asar).isDirectory() };
try { await fs.promises.rm(theirs, { recursive: true, force: true }); said.electronFs = "removed"; } catch (e) { said.electronFs = e.code; }
const { removeTree } = await import(pathToFileURL(mod).href);
try { await removeTree(ours); said.removeTree = "removed"; } catch (e) { said.removeTree = e.code; }
console.log(JSON.stringify(said));
`,
    );
    const mod = fileURLToPath(new URL("./plainfs.ts", import.meta.url));
    const run = spawnSync(electron!, [script, mod, theirs, asar, ours], { env: { ...process.env, ELECTRON_RUN_AS_NODE: "1" }, encoding: "utf8", timeout: 60_000 });
    const line = run.stdout.trim().split(/\r?\n/).at(-1) ?? "";
    expect(line, `${run.status} ${run.stderr}`).toMatch(/^\{/);
    const said = JSON.parse(line) as { folder: boolean; electronFs: string; removeTree: string };
    expect(said.folder).toBe(true); // an archive: a folder to Electron's fs
    expect(said.electronFs).not.toBe("removed"); // ENOTEMPTY on Linux, EBUSY on Windows
    expect(said.removeTree).toBe("removed");
    expect(existsSync(ours)).toBe(false);
  });
});
