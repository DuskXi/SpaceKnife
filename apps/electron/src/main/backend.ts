// Runs the Python backend as a child process (docs/protocol.md §1).
//
// The token travels on stdin (never argv/env); the backend prints one JSON "ready" line on stdout
// and exits gracefully when its stdin closes, so quitting the app (or crashing) stops it.
// An installed app (M8) runs the frozen backend it ships (resources/backend, PyInstaller);
// a checkout runs `python -m spaceknife` from the repo's uv environment.

import { spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";

import type { BackendInfo } from "../shared/api";
import { mt } from "./text";
import type { InstallKind } from "./update";

export interface BackendOptions {
  repoRoot: string;
  /** the frozen backend of an installed app (bundledBackend); else the repo's Python */
  bundled?: string | null;
  allowOrigins: string[];
  home?: string;
  /** OS UI language (app.getPreferredSystemLanguages), for 「跟随系统」 */
  locale?: string;
  /** for the update checks (update.*): the app's version, how it was installed (update.ts), where
   *  downloads go */
  appVersion?: string;
  installKind?: InstallKind;
  downloadDir?: string | null;
  onLog?: (line: string) => void;
}

export interface Backend {
  info: BackendInfo;
  proc: ChildProcess;
  stop(): Promise<void>;
}

/** The interpreter of the repo's uv environment, or SPACEKNIFE_PYTHON. */
export function pythonFor(repoRoot: string): string {
  const explicit = process.env.SPACEKNIFE_PYTHON;
  if (explicit) return explicit;
  const venv = process.platform === "win32" ? join(repoRoot, ".venv", "Scripts", "python.exe") : join(repoRoot, ".venv", "bin", "python");
  if (existsSync(venv)) return venv;
  return process.platform === "win32" ? "python.exe" : "python3";
}

/** resources/backend/spaceknife-backend[.exe] of a packaged app (electron-builder.yml), if present. */
export function bundledBackend(resourcesPath: string): string | null {
  const exe = join(resourcesPath, "backend", process.platform === "win32" ? "spaceknife-backend.exe" : "spaceknife-backend");
  return existsSync(exe) ? exe : null;
}

export function startBackend(opts: BackendOptions): Promise<Backend> {
  const token = randomBytes(24).toString("base64url");
  // the frozen backend is built with -X utf8 (packaging/spaceknife-backend.spec)
  const args = opts.bundled ? ["serve"] : ["-X", "utf8", "-m", "spaceknife", "serve"];
  args.push("--token-stdin", "--log-format", "plain");
  for (const origin of opts.allowOrigins) args.push("--allow-origin", origin);
  const env: NodeJS.ProcessEnv = { ...process.env, PYTHONIOENCODING: "utf-8" };
  if (opts.home) env.SPACEKNIFE_HOME = resolve(opts.home);
  // 「界面语言 · 跟随系统」 (M7.5): the OS UI language as Electron sees it
  if (opts.locale) env.SPACEKNIFE_SYSTEM_LOCALE = opts.locale;
  if (opts.appVersion) env.SPACEKNIFE_APP_VERSION = opts.appVersion;
  if (opts.installKind) env.SPACEKNIFE_INSTALL_KIND = opts.installKind;
  if (opts.downloadDir) env.SPACEKNIFE_DOWNLOAD_DIR = opts.downloadDir;
  const proc = spawn(opts.bundled ?? pythonFor(opts.repoRoot), args, {
    cwd: opts.bundled ? dirname(opts.bundled) : opts.repoRoot,
    env,
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
        const ready = JSON.parse(line) as { url: string; pid: number };
        resolveReady({
          info: { url: ready.url, token, pid: ready.pid },
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
    if (proc.exitCode !== null) return done();
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
