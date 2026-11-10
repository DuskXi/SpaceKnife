// Portable copies (the Windows zip, the Linux tar.gz and the macOS zip; docs/packaging.md): a "data"
// folder next to the program holds everything the app writes, as if SPACEKNIFE_HOME pointed at it.
// On macOS the program is the app bundle: the folder is next to SpaceKnife.app.

import { statSync } from "node:fs";
import { basename, dirname, join } from "node:path";

/** The app bundle an executable belongs to (…/SpaceKnife.app/Contents/MacOS/SpaceKnife), or null. */
export function appBundle(execPath: string): string | null {
  const macos = dirname(execPath);
  const bundle = dirname(dirname(macos));
  return basename(macos) === "MacOS" && basename(dirname(macos)) === "Contents" && bundle.endsWith(".app") ? bundle : null;
}

/** Whether macOS runs this copy from somewhere else (App Translocation, "Gatekeeper Path
 *  Randomization"): an app still quarantined, opened where it was unpacked, runs from a read-only
 *  copy under /private/var/folders/…/AppTranslocation/, where the data folder next to it is not. */
export function isTranslocated(path: string): boolean {
  return path.includes("/AppTranslocation/");
}

/** The original of a translocated path, from the mount table (`/sbin/mount`'s output): the copy is a
 *  read-only nullfs mount of the original, a line `<original> on <mount point> (nullfs, …)`. */
export function untranslocate(path: string, mountTable: string): string | null {
  let best: { point: string; source: string } | null = null;
  for (const line of mountTable.split("\n")) {
    const m = /^(.+?) on (\/.*\/AppTranslocation\/.*?) \(nullfs[,)]/.exec(line.trim());
    if (!m) continue;
    const source = m[1]!;
    const point = m[2]!;
    if ((path === point || path.startsWith(`${point}/`)) && (!best || point.length > best.point.length)) best = { point, source };
  }
  return best ? best.source + path.slice(best.point.length) : null;
}

/** The portable data folder, or null. Packaged apps only (an AppImage runs from a read-only mount and
 *  has none next to it), and an explicit SPACEKNIFE_HOME wins. A translocated macOS copy looks next
 *  to its original (`original`: where the mount table says it is), which the user can write to. */
export function portableHome(
  execPath: string,
  platform: NodeJS.Platform,
  packaged: boolean,
  env: NodeJS.ProcessEnv,
  original: (path: string) => string | null = () => null,
): string | null {
  if (!packaged || env.SPACEKNIFE_HOME) return null;
  let program = execPath;
  if (platform === "darwin") {
    const bundle = appBundle(execPath);
    const here = bundle && isTranslocated(bundle) ? original(bundle) : bundle;
    if (!here) return null;
    program = here;
  }
  const dir = join(dirname(program), "data");
  try {
    return statSync(dir).isDirectory() ? dir : null;
  } catch {
    return null;
  }
}
