// The AppImage's 「无缝更新」 (index.ts replaceAppImage): the downloaded file is copied next to
// $APPIMAGE, asked whether it can start on this machine — `<new> --startup-check` (cli.ts
// STARTUP_CHECK), the question the portable swap asks its new version too (sk_core::update::swap
// startup_check), with the environment and the switches the real start gets — and only then takes
// $APPIMAGE's place; the old file stays as `$APPIMAGE.previous` (until the next update), so that a
// new version that passed the check and still doesn't come up can be put back by renaming one file.
// The file system and the check are injectable; index.ts does the Electron side.

import { spawn } from "node:child_process";
import { chmod, copyFile, link, rename, rm } from "node:fs/promises";
import { basename, dirname } from "node:path";

import { STARTUP_CHECK } from "./cli";
import type { TextKey } from "./text";

/** How long the new AppImage has to answer (sk_core::update::swap STARTUP_CHECK_TIMEOUT): mounting
 *  it and starting Electron, well within it even from a slow disk. */
export const STARTUP_CHECK_TIMEOUT_MS = 20_000;
/** What is kept of each of the check's outputs. */
const OUTPUT_LIMIT = 64 * 1024;
/** How long the check's output may stay open once it has ended: a process it started (Chromium's
 *  zygote, the AppImage's mount) may hold the pipes a little longer. */
const OUTPUT_GRACE_MS = 500;

/** The new file while it is checked, next to the old one (the same file system: one rename). */
export const nextOf = (target: string): string => `${target}.new`;
/** The old file once the new one has taken its place. */
export const previousOf = (target: string): string => `${target}.previous`;

/** What the AppImage runtime sets for the program it runs; the new file's runtime sets its own. */
const RUNTIME_VARS = new Set(["APPDIR", "APPIMAGE", "ARGV0", "OWD"]);

/** The environment the new AppImage gets, for the check and the start alike: this process's,
 *  without what the running AppImage set for itself — APPDIR, APPIMAGE, ARGV0, OWD, and every
 *  entry of a variable (a path list: PATH, LD_LIBRARY_PATH, XDG_DATA_DIRS…, which the AppImage's
 *  AppRun extends with its own folders) inside the old APPDIR. That mount goes away when this app
 *  quits: the new version must not find anything there during the check that it lacks afterwards.
 *  Never a ready file (backend.ts takeReadyFile). */
export function appImageEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const appdir = (env.APPDIR ?? "").replace(/\/+$/, "");
  const inside = (entry: string) => entry === appdir || entry.startsWith(`${appdir}/`);
  const out: NodeJS.ProcessEnv = {};
  for (const [name, value] of Object.entries(env)) {
    if (value === undefined || RUNTIME_VARS.has(name) || name === "SPACEKNIFE_READY_FILE") continue;
    if (!appdir || !value.includes(appdir)) {
      out[name] = value;
      continue;
    }
    const entries = value.split(":");
    const kept = entries.filter((e) => !inside(e));
    if (kept.length === entries.length) out[name] = value;
    else if (kept.some((e) => e !== "")) out[name] = kept.join(":");
  }
  return out;
}

/** The switches of this run the new version's start needs as well: `--no-sandbox` (root, or a
 *  system that restricts user namespaces), without which it would not start where this one does. */
export function carriedSwitches(args: readonly string[]): string[] {
  return args.includes("--no-sandbox") ? ["--no-sandbox"] : [];
}

/** The last line of a program's stderr that says something, from the loader's own words on
 *  ("…: error while loading shared libraries: …" → from "error"), at most 300 characters
 *  (sk_core::update::swap last_line). */
export function lastLine(stderr: string): string | null {
  const line = stderr
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l !== "")
    .at(-1);
  if (line === undefined) return null;
  const at = line.indexOf("error while loading");
  return [...(at >= 0 ? line.slice(at) : line)].slice(0, 300).join("");
}

/** What the program's answer means (sk_core::update::swap judge_startup): null when it can start,
 *  else why not, in words the update card shows. */
export function judgeStartup(name: string, code: number | null, signal: string | null, stdout: string, stderr: string): string | null {
  const first = stdout
    .split(/\r?\n/)
    .map((l) => l.trim())
    .find((l) => l !== "");
  let said: Record<string, unknown> | null = null;
  try {
    const value: unknown = first === undefined ? null : JSON.parse(first);
    if (value && typeof value === "object" && !Array.isArray(value)) said = value as Record<string, unknown>;
  } catch {
    // not its answer: what it did says the rest
  }
  if (said?.ok === true && code === 0) return null;
  if (said?.ok === false) return typeof said.error === "string" && said.error ? said.error : `${name} says it can't start`;
  const why =
    code === 0
      ? `${name} did not answer ${STARTUP_CHECK}`
      : signal
        ? `${name} was ended by signal ${signal}`
        : code !== null
          ? `${name} ended with exit code ${code}`
          : `${name} ended without an exit code`;
  const last = lastLine(stderr);
  return last !== null && code !== 0 ? `${why}: ${last}` : why;
}

/** Run `command args…` as the startup check: its standard input closed, its output read, in a
 *  session of its own (Unix; what it started goes with it when it doesn't answer), for at most
 *  `timeoutMs`. Null when it can start, else why not. */
export function runStartupCheck(command: string, args: readonly string[], env: NodeJS.ProcessEnv, timeoutMs = STARTUP_CHECK_TIMEOUT_MS): Promise<string | null> {
  const name = basename(command);
  return new Promise((resolve) => {
    const unix = process.platform !== "win32";
    let child: ReturnType<typeof spawn>;
    try {
      child = spawn(command, [...args], { cwd: dirname(command), env, stdio: ["ignore", "pipe", "pipe"], detached: unix, windowsHide: true });
    } catch (err) {
      resolve(`${name} can't be run: ${err instanceof Error ? err.message : String(err)}`);
      return;
    }
    let out = "";
    let errText = "";
    const keep = (kept: string, chunk: Buffer) => (kept.length < OUTPUT_LIMIT ? (kept + chunk.toString("utf8")).slice(0, OUTPUT_LIMIT) : kept);
    child.stdout?.on("data", (c: Buffer) => (out = keep(out, c)));
    child.stderr?.on("data", (c: Buffer) => (errText = keep(errText, c)));
    let settled = false;
    let grace: NodeJS.Timeout | undefined;
    const finish = (why: () => string | null) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      clearTimeout(grace);
      child.stdout?.destroy();
      child.stderr?.destroy();
      resolve(why());
    };
    const timer = setTimeout(() => {
      endGroup(child);
      const last = lastLine(errText);
      finish(() => `${name} did not answer ${STARTUP_CHECK} within ${timeoutMs / 1000} s${last === null ? "" : ` (it said: ${last})`}`);
    }, timeoutMs);
    child.once("error", (err) => finish(() => `${name} can't be run: ${err.message}`));
    child.once("exit", (code, signal) => {
      const judged = () => judgeStartup(name, code, signal, out, errText);
      // the pipes close with the program unless something it started holds them: what it wrote is
      // taken without waiting for them
      child.once("close", () => finish(judged));
      grace = setTimeout(() => finish(judged), OUTPUT_GRACE_MS);
    });
  });
}

/** End a process started in a session of its own and what it started: SIGTERM to the group (an
 *  AppImage's mount goes away cleanly), SIGKILL after `graceMs` while the program still runs. */
export function endGroup(child: ReturnType<typeof spawn>, graceMs = 3000): void {
  const pid = child.pid;
  if (pid === undefined) return;
  if (process.platform === "win32") {
    child.kill();
    return;
  }
  const send = (signal: NodeJS.Signals) => {
    try {
      process.kill(-pid, signal);
    } catch {
      // gone already
    }
  };
  send("SIGTERM");
  setTimeout(() => {
    if (child.exitCode === null && child.signalCode === null) send("SIGKILL");
  }, graceMs).unref();
}

/** The startup check's arguments: the question, then the switches the start gets. */
export const startupCheckArgs = (switches: readonly string[]): string[] => [STARTUP_CHECK, ...switches];

/** The startup check's environment: the start's, with every symbol bound at once (LD_BIND_NOW, as
 *  the portable swap asks), so that a library too old for the program fails here, not in the
 *  window. */
export const startupCheckEnv = (env: NodeJS.ProcessEnv): NodeJS.ProcessEnv => ({ ...env, LD_BIND_NOW: "1" });

/** The new AppImage's startup check (`program` is the copy next to $APPIMAGE). */
export function startupCheck(program: string, switches: readonly string[], env: NodeJS.ProcessEnv, timeoutMs = STARTUP_CHECK_TIMEOUT_MS): Promise<string | null> {
  return runStartupCheck(program, startupCheckArgs(switches), startupCheckEnv(env), timeoutMs);
}

/** What putting the new AppImage in place does to files; every step may throw. */
export interface AppImageFiles {
  copyFile(from: string, to: string): Promise<void>;
  chmod(path: string, mode: number): Promise<void>;
  /** a second name for the same file */
  link(existing: string, name: string): Promise<void>;
  rename(from: string, to: string): Promise<void>;
  /** gone, whether it was there or not */
  remove(path: string): Promise<void>;
}

export const nodeFiles: AppImageFiles = {
  copyFile: (from, to) => copyFile(from, to),
  chmod: (path, mode) => chmod(path, mode),
  link: (existing, name) => link(existing, name),
  rename: (from, to) => rename(from, to),
  remove: (path) => rm(path, { force: true }),
};

export type Placed = { ok: true; previous: string } | { ok: false; key: Extract<TextKey, "updateReplaceFailed" | "updateCantStart">; error: string };

const message = (err: unknown) => (err instanceof Error ? err.message : String(err));

/** Put the downloaded AppImage `file` in the place of `target` ($APPIMAGE): copied next to it as
 *  `target.new` and made executable; asked `check` (null: it can start) — when it can't, it is
 *  removed and nothing else changes; then the old file gets its second name `target.previous` (a
 *  hard link, so that `target` never goes missing; on a file system without them the old file is
 *  renamed) and the new one is renamed over `target`. The running app keeps its old file open. */
export async function placeAppImage(file: string, target: string, files: AppImageFiles, check: (program: string) => Promise<string | null>): Promise<Placed> {
  const next = nextOf(target);
  const previous = previousOf(target);
  const drop = () => files.remove(next).catch(() => {});
  try {
    await files.remove(next); // left by an attempt that was cut short
    await files.copyFile(file, next);
    await files.chmod(next, 0o755);
  } catch (err) {
    await drop();
    return { ok: false, key: "updateReplaceFailed", error: message(err) };
  }
  const refused = await check(next);
  if (refused !== null) {
    await drop();
    return { ok: false, key: "updateCantStart", error: refused };
  }
  try {
    await files.remove(previous); // the one before: one old version is kept
  } catch (err) {
    await drop();
    return { ok: false, key: "updateReplaceFailed", error: message(err) };
  }
  let linked = true;
  try {
    await files.link(target, previous);
  } catch {
    linked = false;
  }
  try {
    if (!linked) await files.rename(target, previous);
  } catch (err) {
    await drop();
    return { ok: false, key: "updateReplaceFailed", error: message(err) };
  }
  try {
    await files.rename(next, target);
  } catch (err) {
    // the old file back under its own name (renamed away above), or its second name dropped
    if (linked) await files.remove(previous).catch(() => {});
    else await files.rename(previous, target).catch(() => {});
    await drop();
    return { ok: false, key: "updateReplaceFailed", error: message(err) };
  }
  return { ok: true, previous };
}
