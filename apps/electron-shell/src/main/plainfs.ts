// Removing folders the way the file system has them. Electron's fs (and fs/promises) takes an .asar
// file for a folder, the archive inside it: `rm(folder, { recursive: true })` cannot remove a folder
// that holds one (ENOTEMPTY on Linux, EBUSY on Windows, where Electron holds the archive open). The
// updates folder does: a portable copy of the Electron shell unpacked there has resources/app.asar
// (0.3.4, which is Electron too and frozen, never got past it: sk_core::update::swap takes the .asar
// files out of a staging folder it leaves behind for it). Electron's original-fs is the fs without
// that; Node's own everywhere else (the unit tests).

import { rm as nodeRm } from "node:fs/promises";
import { createRequire } from "node:module";

export type Rm = typeof nodeRm;

/** fs/promises' rm that removes an .asar file as the file it is: Electron's original-fs inside
 *  Electron (`versions.electron`), Node's own elsewhere. `load` is Node's require. */
export function plainRm(versions: { readonly electron?: string } = process.versions, load: (id: string) => unknown = createRequire(import.meta.url)): Rm {
  if (!versions.electron) return nodeRm;
  return (load("original-fs") as typeof import("node:fs")).promises.rm;
}

/** Remove `path` and all it holds, .asar files too; nothing when it is not there. */
export function removeTree(path: string, rm: Rm = plainRm()): Promise<void> {
  return rm(path, { recursive: true, force: true });
}
