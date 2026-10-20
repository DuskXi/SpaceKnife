// Updates (update.* of the backend, spaceknife_protocol.update): how this copy was installed, which
// decides the release file the backend downloads, and what main checks before it starts a downloaded
// file. Pure functions (the file system is injectable); index.ts does the Electron side.

import { existsSync, realpathSync, statSync } from "node:fs";
import { posix, win32 } from "node:path";

import type { TextKey } from "./text";

/** How this copy was installed (spaceknife_protocol.update.InstallKind; SPACEKNIFE_INSTALL_KIND). */
export type InstallKind = "nsis" | "portable" | "appimage" | "deb" | "rpm" | "dmg" | "dev" | "unknown";

/** The pages main opens in the browser for the renderer (release pages); nothing else. */
export const PROJECT_PAGES = "https://github.com/DuskXi/SpaceKnife/";

/** The uninstaller electron-builder's NSIS installer puts next to the executable. */
const NSIS_UNINSTALLER = "Uninstall SpaceKnife.exe";
/** Where the deb and rpm packages put the app (electron-builder: /opt/<productName>). */
const PACKAGE_DIR = "/opt/SpaceKnife";
/** dpkg's file list of the installed deb (electron-builder.yml deb.packageName). */
const DPKG_LIST = "/var/lib/dpkg/info/spaceknife.list";

// The release files main may start (electron-builder.yml artifactName); a portable archive or an
// AppImage has no install step (the renderer shows it in its folder), nor has a development run.
// every part is a number of any length (0.3.10, 0.10.0); deb and rpm may write a pre-release's "-"
// as "~" or "_"
const VERSION = String.raw`\d+\.\d+\.\d+(?:[-~_][0-9A-Za-z.~_-]+)?`;
const INSTALLERS: Partial<Record<InstallKind, RegExp>> = {
  nsis: new RegExp(String.raw`^SpaceKnife-${VERSION}-Setup-x64\.exe$`),
  dmg: new RegExp(String.raw`^SpaceKnife-${VERSION}-mac-[0-9a-z_]+\.dmg$`),
  deb: new RegExp(String.raw`^spaceknife_${VERSION}_amd64\.deb$`),
  rpm: new RegExp(String.raw`^spaceknife-${VERSION}\.x86_64\.rpm$`),
};

/** The kind of install, from where the executable is and what lies around it. `portableHome` is
 *  portable.ts's answer (a portable copy with its data folder). */
export function installKind(
  execPath: string,
  platform: NodeJS.Platform,
  packaged: boolean,
  env: NodeJS.ProcessEnv,
  portableHome: string | null,
  exists: (path: string) => boolean = existsSync,
): InstallKind {
  if (!packaged) return "dev";
  if (portableHome) return "portable";
  if (platform === "win32") return exists(win32.join(win32.dirname(execPath), NSIS_UNINSTALLER)) ? "nsis" : "unknown";
  if (platform === "darwin") return "dmg";
  if (platform !== "linux") return "unknown";
  // the packages first: programs started from an AppImage inherit its APPIMAGE and APPDIR
  if (within(posix, PACKAGE_DIR, execPath)) return exists(DPKG_LIST) ? "deb" : "rpm";
  if (env.APPIMAGE && (!env.APPDIR || within(posix, env.APPDIR, execPath))) return "appimage";
  return "unknown";
}

export interface FileProbe {
  /** The path with symbolic links resolved; throws when it doesn't exist. */
  realpath(path: string): string;
  isFile(path: string): boolean;
}

const nodeFiles: FileProbe = {
  realpath: (path) => realpathSync.native(path),
  isFile: (path) => statSync(path).isFile(),
};

/** Why `file` can't be started as the downloaded update (a text of main's), or null when it can: a
 *  file directly in the download folder, links resolved, named like this install's release file. */
export function installRefusal(
  file: string,
  kind: InstallKind,
  downloads: string,
  platform: NodeJS.Platform,
  files: FileProbe = nodeFiles,
): TextKey | null {
  const pattern = INSTALLERS[kind];
  if (!pattern) return "updateNoInstall";
  const path = platform === "win32" ? win32 : posix;
  let real: string;
  let folder: string;
  try {
    real = files.realpath(file);
    folder = files.realpath(downloads);
    if (!files.isFile(real)) return "updateMissing";
  } catch {
    return "updateMissing";
  }
  if (!samePath(path.dirname(real), folder, platform)) return "updateNotInDownloads";
  return pattern.test(path.basename(real)) ? null : "updateNotAnUpdate";
}

/** The address to open when `url` is one of the project's pages on GitHub (normalised), else null. */
export function projectPage(url: unknown): string | null {
  if (typeof url !== "string") return null;
  try {
    const href = new URL(url).href; // resolves "..", lowercases the host
    return href.startsWith(PROJECT_PAGES) ? href : null;
  } catch {
    return null;
  }
}

function within(path: typeof posix, dir: string, file: string): boolean {
  const rel = path.relative(dir, file);
  return rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel);
}

/** Windows and macOS file systems ignore case (macOS: by default). */
function samePath(a: string, b: string, platform: NodeJS.Platform): boolean {
  return platform === "linux" ? a === b : a.toLowerCase() === b.toLowerCase();
}
