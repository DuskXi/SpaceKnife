// Portable copies (the Windows zip and the Linux tar.gz; docs/packaging.md): a "data" folder next to
// the executable holds everything the app writes, as if SPACEKNIFE_HOME pointed at it.

import { statSync } from "node:fs";
import { dirname, join } from "node:path";

/** The portable data folder, or null. Packaged Windows and Linux apps only (a macOS app bundle is
 *  portable already; an AppImage runs from a read-only mount), and an explicit SPACEKNIFE_HOME wins. */
export function portableHome(execPath: string, platform: NodeJS.Platform, packaged: boolean, env: NodeJS.ProcessEnv): string | null {
  if (!packaged || platform === "darwin" || env.SPACEKNIFE_HOME) return null;
  const dir = join(dirname(execPath), "data");
  try {
    return statSync(dir).isDirectory() ? dir : null;
  } catch {
    return null;
  }
}
