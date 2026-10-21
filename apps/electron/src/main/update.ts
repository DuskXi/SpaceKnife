// Updates (update.* of the backend, spaceknife_protocol.update): how this copy was installed, which
// decides the release file the backend downloads; what main checks before it uses a downloaded file;
// and how 「重启并更新」 puts the new version in place for each kind of install (the Windows installer
// run silently, a portable copy's files swapped by a script once the app is gone, an AppImage
// replaced). Pure functions (the file system is injectable); index.ts does the Electron side.

import { existsSync, realpathSync, statSync } from "node:fs";
import { posix, win32 } from "node:path";

import type { TextKey } from "./text";

// The pages main opens in the browser for the renderer (release pages, the help's links to the
// website); nothing else. The list is shared with the renderer, which checks it before it asks.
export { PROJECT_PAGES, WEBSITE, projectPage } from "../shared/links";

/** How this copy was installed (spaceknife_protocol.update.InstallKind; SPACEKNIFE_INSTALL_KIND). */
export type InstallKind = "nsis" | "portable" | "appimage" | "deb" | "rpm" | "dmg" | "dev" | "unknown";

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
const PORTABLE: Partial<Record<NodeJS.Platform, RegExp>> = {
  win32: new RegExp(String.raw`^SpaceKnife-${VERSION}-win-x64-portable\.zip$`),
  linux: new RegExp(String.raw`^SpaceKnife-${VERSION}-linux-x64-portable\.tar\.gz$`),
};

/** How 「重启并更新」 puts the downloaded version in place: the Windows installer runs silently and
 *  starts it; a portable copy's files or an AppImage are replaced and it starts; a disk image or a
 *  package opens in the system's installer (the app quits); anything else has no install step. */
export type RestartMode = "installer" | "replace" | "open" | "none";

export function restartMode(kind: InstallKind): RestartMode {
  switch (kind) {
    case "nsis":
      return "installer";
    case "portable":
    case "appimage":
      return "replace";
    case "dmg":
    case "deb":
    case "rpm":
      return "open";
    default:
      return "none";
  }
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

/** The script that swaps a portable copy's files after the app has quit: PowerShell on Windows, sh
 *  on Linux. It waits for the app, moves the old files out (retrying while Windows still holds them),
 *  moves the new ones in, undoes both when a step fails, and starts the app with --updated (the new
 *  version) or without (the old one). The backend removes `previous`, `staged`, the script and its log
 *  once the new version runs (spaceknife.update.clean_updates). */
/** The script file's bytes: Windows PowerShell 5.1 reads a .ps1 without a byte order mark in the
 * ANSI code page, so the paths in it (a portable copy under 「D:\\软件」) need UTF-8 with a BOM. */
export function swapScriptFile(platform: "win32" | "linux", text: string): Buffer {
  return Buffer.from(platform === "win32" ? `\ufeff${text}` : text, "utf8");
}

/** How to start the swap so it outlives the app without a window. On Windows a detached
 * powershell.exe (no console) never runs its script, and an attached one dies with the app (libuv's
 * job object). So an attached, hidden PowerShell starts the real one with Start-Process: that one
 * leaves the job (silent breakaway) and outlives the app. `wait`: the app quits only once the starter
 * is done. On Linux the shell runs detached. */
export function swapLauncher(platform: "win32" | "linux", script: string): { cmd: string; args: string[]; wait: boolean } {
  if (platform !== "win32") return { cmd: "/bin/sh", args: [script], wait: false };
  // Start-Process joins an argument array with spaces and no quotes, so the arguments go as one
  // string, the script's path in double quotes (a path can't hold one); the whole string in single
  // quotes for the starter's -Command
  const inner = `-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "${script}"`;
  const start = `Start-Process -FilePath powershell.exe -WindowStyle Hidden -ArgumentList '${inner.replace(/'/g, "''")}'`;
  return { cmd: "powershell.exe", args: ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", start], wait: true };
}

export function swapScript(platform: "win32" | "linux", plan: SwapPlan): string {
  return platform === "win32" ? powershellSwap(plan) : shSwap(plan);
}

const psQuote = (s: string) => `'${s.replace(/'/g, "''")}'`;
const shQuote = (s: string) => `'${s.replace(/'/g, "'\\''")}'`;

function powershellSwap(p: SwapPlan): string {
  return [
    "# SpaceKnife: a portable copy's update (written by the app; removed once the new version runs)",
    "$ErrorActionPreference = 'Stop'",
    `$App = ${psQuote(p.appDir)}`,
    `$New = ${psQuote(p.staged)}`,
    `$Old = ${psQuote(p.previous)}`,
    `$Exe = ${psQuote(p.exe)}`,
    `$Log = ${psQuote(p.log)}`,
    "function Log([string]$m) { Add-Content -LiteralPath $Log -Value ((Get-Date).ToString('u') + ' ' + $m) }",
    "function Move-Entries([string]$from, [string]$to) {",
    "  New-Item -ItemType Directory -Force -Path $to | Out-Null",
    "  foreach ($e in @(Get-ChildItem -LiteralPath $from -Force | Where-Object { $_.Name -ne 'data' })) {",
    "    Move-Item -LiteralPath $e.FullName -Destination (Join-Path $to $e.Name) -Force",
    "  }",
    "}",
    `Log 'waiting for SpaceKnife (${p.pid}) to quit'`,
    `try { Wait-Process -Id ${p.pid} -Timeout 60 -ErrorAction Stop } catch { }`,
    "# the new version must be complete before anything is moved",
    "if (-not (Test-Path -LiteralPath (Join-Path $New (Split-Path -Leaf $Exe)) -PathType Leaf)) {",
    "  Log 'the new version is incomplete: kept the old version'",
    "  Start-Process -FilePath $Exe",
    "  exit 1",
    "}",
    "$ok = $false",
    "for ($i = 0; $i -lt 60 -and -not $ok; $i++) {",
    "  try { Move-Entries $App $Old; $ok = $true } catch { Log ('busy: ' + $_.Exception.Message); Start-Sleep -Milliseconds 500 }",
    "}",
    "if ($ok) {",
    "  try { Move-Entries $New $App } catch {",
    "    Log ('could not move the new files in: ' + $_.Exception.Message)",
    "    try { Move-Entries $App $New } catch { Log ('could not take them out again: ' + $_.Exception.Message) }",
    "    $ok = $false",
    "  }",
    "}",
    "if (-not $ok) {",
    "  try { Move-Entries $Old $App } catch { Log ('could not restore the old files: ' + $_.Exception.Message) }",
    "  Log 'kept the old version'",
    "  Start-Process -FilePath $Exe",
    "} else {",
    "  Log 'updated'",
    "  Start-Process -FilePath $Exe -ArgumentList '--updated'",
    "}",
    "",
  ].join("\r\n");
}

function shSwap(p: SwapPlan): string {
  return [
    "#!/bin/sh",
    "# SpaceKnife: a portable copy's update (written by the app; removed once the new version runs)",
    `APP=${shQuote(p.appDir)}`,
    `NEW=${shQuote(p.staged)}`,
    `OLD=${shQuote(p.previous)}`,
    `EXE=${shQuote(p.exe)}`,
    `LOG=${shQuote(p.log)}`,
    `log() { printf '%s %s\\n' "$(date -u '+%Y-%m-%d %H:%M:%S')" "$1" >> "$LOG"; }`,
    "move_entries() {",
    '  [ -d "$1" ] || return 1',
    '  mkdir -p "$2" || return 1',
    '  for e in "$1"/* "$1"/.[!.]* "$1"/..?*; do',
    '    [ -e "$e" ] || [ -L "$e" ] || continue',
    '    [ "$(basename "$e")" = data ] && continue',
    '    mv -f "$e" "$2"/ || return 1',
    "  done",
    "}",
    `log 'waiting for SpaceKnife (${p.pid}) to quit'`,
    "n=0",
    `while kill -0 ${p.pid} 2>/dev/null && [ $n -lt 300 ]; do sleep 0.2; n=$((n + 1)); done`,
    "# the new version must be complete before anything is moved",
    'if [ ! -f "$NEW/$(basename "$EXE")" ]; then',
    '  log "the new version is incomplete: kept the old version"',
    '  nohup "$EXE" >/dev/null 2>&1 &',
    "  exit 1",
    "fi",
    "ok=0",
    'if move_entries "$APP" "$OLD"; then',
    '  if move_entries "$NEW" "$APP"; then ok=1; else log "could not move the new files in"; move_entries "$APP" "$NEW"; fi',
    "fi",
    'if [ "$ok" = 1 ]; then',
    "  log updated",
    '  nohup "$EXE" --updated >/dev/null 2>&1 &',
    "else",
    '  move_entries "$OLD" "$APP" || log "could not restore the old files"',
    '  log "kept the old version"',
    '  nohup "$EXE" >/dev/null 2>&1 &',
    "fi",
    "",
  ].join("\n");
}
