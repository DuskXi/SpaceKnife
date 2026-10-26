// Updates (update.* of the backend, spaceknife_protocol.update): how this copy was installed, which
// decides the release file the backend downloads; what main checks before it uses a downloaded file;
// and what the update card's two buttons need (shared/install.ts says what each does per kind of
// install): the Windows installer's arguments, the plan for the backend that swaps a portable
// copy's files. Pure functions (the file system is injectable); index.ts does the Electron side.

import { existsSync, realpathSync, statSync } from "node:fs";
import { posix, win32 } from "node:path";

import type { InstallKind } from "../shared/install";
import type { TextKey } from "./text";

export { type InstallHow, type InstallKind, type InstallStep, installStep, stepQuits } from "../shared/install";

// The pages main opens in the browser for the renderer (release pages, the help's links to the
// website); nothing else. The list is shared with the renderer, which checks it before it asks.
export { PROJECT_PAGES, WEBSITE, projectPage } from "../shared/links";

/** The uninstaller electron-builder's NSIS installer puts next to the executable. */
const NSIS_UNINSTALLER = "Uninstall SpaceKnife.exe";
/** Where the deb and rpm packages put the app (electron-builder: /opt/<productName>). */
const PACKAGE_DIR = "/opt/SpaceKnife";
/** dpkg's file list of the installed deb (electron-builder.yml deb.packageName). */
const DPKG_LIST = "/var/lib/dpkg/info/spaceknife.list";

// The release files main may use (electron-builder.yml artifactName, tools/build_package.py); a
// development run has none. Every part is a number of any length (0.3.10, 0.10.0); deb and rpm may
// write a pre-release's "-" as "~" or "_".
const VERSION = String.raw`\d+\.\d+\.\d+(?:[-~_][0-9A-Za-z.~_-]+)?`;
const RELEASE_FILES: Partial<Record<InstallKind, RegExp>> = {
  nsis: new RegExp(String.raw`^SpaceKnife-${VERSION}-Setup-x64\.exe$`),
  dmg: new RegExp(String.raw`^SpaceKnife-${VERSION}-mac-[0-9a-z_]+\.dmg$`),
  deb: new RegExp(String.raw`^spaceknife_${VERSION}_amd64\.deb$`),
  rpm: new RegExp(String.raw`^spaceknife-${VERSION}\.x86_64\.rpm$`),
  appimage: new RegExp(String.raw`^SpaceKnife-${VERSION}-x86_64\.AppImage$`),
};
// a portable copy may move to the other architecture: the backend takes the processor's build (an x64
// copy on Windows on ARM becomes the native one; update.py asset_templates)
const ARCH = String.raw`(?:x64|arm64)`;
const PORTABLE: Partial<Record<NodeJS.Platform, RegExp>> = {
  win32: new RegExp(String.raw`^SpaceKnife-${VERSION}-win-${ARCH}-portable\.zip$`),
  linux: new RegExp(String.raw`^SpaceKnife-${VERSION}-linux-${ARCH}-portable\.tar\.gz$`),
  darwin: new RegExp(String.raw`^SpaceKnife-${VERSION}-mac-${ARCH}-portable\.zip$`),
};

/** The version in a portable archive's name (Windows, Linux or macOS; x64 or arm64), or null. */
export function portableVersion(name: string): string | null {
  return new RegExp(String.raw`^SpaceKnife-(${VERSION})-(?:(?:win|mac)-${ARCH}-portable\.zip|linux-${ARCH}-portable\.tar\.gz)$`).exec(name)?.[1] ?? null;
}

/** The Windows installer's arguments for a silent update in place, electron-builder's own flags:
 *  --updated keeps the data and skips the pages, --force-run starts the new version afterwards. The
 *  installer reads the folder and, with one installation, its scope from the registry; the scope is
 *  passed only where the folder makes it plain (a per-user and a per-machine installation at once is
 *  the case it would otherwise decide by itself, for the current user). */
export function nsisUpdateArgs(execPath: string, env: NodeJS.ProcessEnv): string[] {
  const args = ["--updated", "/S", "--force-run"];
  const dir = win32.dirname(execPath);
  const machine = [env.ProgramFiles, env["ProgramFiles(x86)"], env.ProgramW6432].some((root) => !!root && within(win32, root, dir));
  if (machine) args.push("/allusers");
  else if (env.LOCALAPPDATA && within(win32, env.LOCALAPPDATA, dir)) args.push("/currentuser");
  return args;
}

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

/** Why `file` can't be used as the downloaded update (a text of main's), or null when it can: a file
 *  directly in the backend's updates folder, links resolved, named like this install's release file. */
export function installRefusal(
  file: string,
  kind: InstallKind,
  updates: string,
  platform: NodeJS.Platform,
  files: FileProbe = nodeFiles,
): TextKey | null {
  const pattern = kind === "portable" ? PORTABLE[platform] : RELEASE_FILES[kind];
  if (!pattern) return "updateNoInstall";
  const path = platform === "win32" ? win32 : posix;
  let real: string;
  let folder: string;
  try {
    real = files.realpath(file);
    folder = files.realpath(updates);
    if (!files.isFile(real)) return "updateMissing";
  } catch {
    return "updateMissing";
  }
  if (!samePath(path.dirname(real), folder, platform)) return "updateNotInUpdates";
  return pattern.test(path.basename(real)) ? null : "updateNotAnUpdate";
}

function within(path: typeof posix, dir: string, file: string): boolean {
  const rel = path.relative(dir, file);
  return rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel);
}

/** Windows and macOS file systems ignore case (macOS: by default). */
function samePath(a: string, b: string, platform: NodeJS.Platform): boolean {
  return platform === "linux" ? a === b : a.toLowerCase() === b.toLowerCase();
}

// ------------------------------------------------------------------ a portable copy

/** What the swap of a portable copy needs: the app's folder keeps its "data"; every other entry goes
 *  to `previous`, the new version's entries come from `staged` (the archive's top folder, unpacked);
 *  then `exe` starts again (the new version, or the old one when the swap failed and was undone). */
export interface SwapPlan {
  /** the app's main process: the swap starts once it is gone */
  pid: number;
  appDir: string;
  staged: string;
  previous: string;
  exe: string;
  log: string;
}

/** The plan file for the new version's backend (`--apply-update <plan>`, spaceknife.update_swap):
 *  version 1, which every later backend keeps reading (an app hands it to the newer version). */
export function swapPlanJson(p: SwapPlan): string {
  const plan = { v: 1, pid: p.pid, app: p.appDir, new: p.staged, old: p.previous, exe: p.exe, log: p.log, keep: ["data"] };
  return `${JSON.stringify(plan, null, 2)}
`;
}

/** The new version's backend executable in its unpacked files: it does the swap (resources/backend,
 *  electron-builder.yml extraResources). */
export function swapHelper(staged: string, platform: "win32" | "linux" | "darwin"): string {
  if (platform === "darwin") return posix.join(staged, MAC_APP, "Contents", "Resources", "backend", "spaceknife-backend");
  const path = platform === "win32" ? win32 : posix;
  return path.join(staged, "resources", "backend", platform === "win32" ? "spaceknife-backend.exe" : "spaceknife-backend");
}

/** The app bundle in a macOS portable copy's folder (tools/build_package.py make_portable). */
export const MAC_APP = "SpaceKnife.app";
