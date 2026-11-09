// Runs the backend, `spaceknife-cli serve --token-stdin`, as a child process (docs/protocol.md §1;
// crates/sk-server/src/cli/serve.rs), the same contract 0.3.4 had with its Python backend:
//
// The token travels on stdin (never argv/env); the backend prints one JSON "ready" line on stdout
// and exits gracefully when its stdin closes, so quitting the app (or crashing) stops it. How the
// app runs (its version, how it was installed, this shell, the system's language) goes in the
// environment, as 0.3.4 passed it.
//
// An installed app runs the spaceknife-cli next to its executable (electron-builder.yml extraFiles);
// a checkout the one cargo built (target/release, else target/debug), or SPACEKNIFE_CLI.

import { spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync } from "node:fs";
import { dirname, posix, resolve, win32 } from "node:path";
import { createInterface } from "node:readline";

import type { BackendInfo } from "../../../desktop/app/shared/api";
import { mt } from "./text";
import type { InstallKind } from "./update";

/** SPACEKNIFE_SHELL: which shell started the backend (the update checks pick that shell's files). */
export const SHELL = "electron";

export interface BackendOptions {
  /** the spaceknife-cli executable (cliPath) */
  cli: string;
  allowOrigins: string[];
  home?: string;
  /** OS UI language (app.getPreferredSystemLanguages), for 「跟随系统」 */
  locale?: string;
  /** for the update checks (update.*): the app's version, how it was installed (update.ts) */
  appVersion?: string;
  installKind?: InstallKind;
  /** where the backend writes its address and token once it listens (takeReadyFile): the first
   *  backend of the app only */
  readyFile?: string;
  onLog?: (line: string) => void;
}

export interface Backend {
  info: BackendInfo;
  /** where the backend downloads updates (its ready line): main uses update files from there only */
  updatesDir: string | null;
  proc: ChildProcess;
  stop(): Promise<void>;
}

const exeName = (platform: NodeJS.Platform) => (platform === "win32" ? "spaceknife-cli.exe" : "spaceknife-cli");

/** The spaceknife-cli to run: SPACEKNIFE_CLI; next to the executable of an installed app; in a
 *  checkout cargo's release build, else its debug build (the release path when neither exists, for
 *  the error). */
export function cliPath(
  execPath: string,
  platform: NodeJS.Platform,
  packaged: boolean,
  env: NodeJS.ProcessEnv,
  repoRoot: string,
  exists: (path: string) => boolean = existsSync,
): string {
  const path = platform === "win32" ? win32 : posix;
  if (env.SPACEKNIFE_CLI) return path.resolve(env.SPACEKNIFE_CLI);
  const name = exeName(platform);
  if (packaged) return path.join(path.dirname(execPath), name);
  const target = env.CARGO_TARGET_DIR ? path.resolve(repoRoot, env.CARGO_TARGET_DIR) : path.join(repoRoot, "target");
  const builds = ["release", "debug"].map((profile) => path.join(target, profile, name));
  return builds.find(exists) ?? builds[0]!;
}

/** SPACEKNIFE_READY_FILE (end-to-end tests, demos, the swap watching a new version, sk_core
 *  update::swap Ready): taken out of `env` — index.ts does it for process.env before anything else
 *  runs — so that nothing the app starts inherits it; the first backend writes it (--ready-file),
 *  later ones don't. The swap removes the file's folder once the new version has spoken, and that
 *  place in a shared temp folder may be anyone's afterwards. The full path, or undefined. */
export function takeReadyFile(env: NodeJS.ProcessEnv): string | undefined {
  const file = env.SPACEKNIFE_READY_FILE;
  delete env.SPACEKNIFE_READY_FILE;
  return file ? resolve(file) : undefined;
}

/** spaceknife-cli's arguments: the backend for a window, logging plainly on stderr; with a ready file
 *  (takeReadyFile) its address and token go there too, as the Tauri shell writes them
 *  (src-tauri/src/backend.rs). */
export function backendArgs(allowOrigins: readonly string[], readyFile?: string): string[] {
  const args = ["serve", "--token-stdin", "--log-format", "plain"];
  for (const origin of allowOrigins) args.push("--allow-origin", origin);
  if (readyFile) args.push("--ready-file", resolve(readyFile));
  return args;
}

/** The backend's environment: this process's, with how the app runs (never a ready file: that is
 *  an argument, for the first backend only). */
export function backendEnv(opts: Omit<BackendOptions, "cli" | "allowOrigins" | "onLog" | "readyFile">, base: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...base, SPACEKNIFE_SHELL: SHELL };
  takeReadyFile(env);
  if (opts.home) env.SPACEKNIFE_HOME = resolve(opts.home);
  // 「界面语言 · 跟随系统」 (M7.5): the OS UI language as Electron sees it
  if (opts.locale) env.SPACEKNIFE_SYSTEM_LOCALE = opts.locale;
  if (opts.appVersion) env.SPACEKNIFE_APP_VERSION = opts.appVersion;
  if (opts.installKind) env.SPACEKNIFE_INSTALL_KIND = opts.installKind;
  return env;
}

export function startBackend(opts: BackendOptions): Promise<Backend> {
  if (!existsSync(opts.cli)) return Promise.reject(new Error(mt("backendMissing", { path: opts.cli })));
  const token = randomBytes(24).toString("base64url");
  const proc = spawn(opts.cli, backendArgs(opts.allowOrigins, opts.readyFile), {
    cwd: dirname(opts.cli),
    env: backendEnv(opts, process.env),
    stdio: ["pipe", "pipe", "pipe"],
    windowsHide: true,
  });
  proc.stdin!.write(token + "\n");
  if (proc.stderr) {
    createInterface({ input: proc.stderr }).on("line", (line) => opts.onLog?.(line));
  }
  return new Promise((resolveReady, reject) => {
    const timer = setTimeout(() => reject(new Error(mt("backendTimeout"))), 30_000);
    const lines = createInterface({ input: proc.stdout! });
    lines.once("line", (line) => {
      clearTimeout(timer);
      try {
        const ready = JSON.parse(line) as { url: string; pid: number; updates_dir?: string };
        resolveReady({
          info: { url: ready.url, token, pid: ready.pid },
          updatesDir: ready.updates_dir ?? null,
          proc,
          stop: () => stop(proc),
        });
      } catch (err) {
        reject(new Error(mt("backendBadLine", { line, error: String(err) })));
      }
    });
    proc.once("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(mt("backendExited", { code: String(code) })));
    });
    proc.once("error", (err) => {
      clearTimeout(timer);
      reject(err);
    });
  });
}

function stop(proc: ChildProcess): Promise<void> {
  return new Promise((done) => {
    if (proc.exitCode !== null || proc.signalCode !== null) return done();
    const killer = setTimeout(() => {
      proc.kill();
      done();
    }, 5000);
    proc.once("exit", () => {
      clearTimeout(killer);
      done();
    });
    proc.stdin?.end(); // EOF: the backend shuts down gracefully
  });
}
