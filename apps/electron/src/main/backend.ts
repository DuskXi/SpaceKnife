// Runs the Python backend as a child process (docs/protocol.md §1).
//
// The token travels on stdin (never argv/env); the backend prints one JSON "ready" line on stdout
// and exits gracefully when its stdin closes, so quitting the app (or crashing) stops it.

import { spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync } from "node:fs";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";

import type { BackendInfo } from "../shared/api";

export interface BackendOptions {
  repoRoot: string;
  allowOrigins: string[];
  home?: string;
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

export function startBackend(opts: BackendOptions): Promise<Backend> {
  const token = randomBytes(24).toString("base64url");
  const args = ["-X", "utf8", "-m", "spaceknife", "serve", "--token-stdin", "--log-format", "plain"];
  for (const origin of opts.allowOrigins) args.push("--allow-origin", origin);
  const env: NodeJS.ProcessEnv = { ...process.env, PYTHONIOENCODING: "utf-8" };
  if (opts.home) env.SPACEKNIFE_HOME = resolve(opts.home);
  const proc = spawn(pythonFor(opts.repoRoot), args, {
    cwd: opts.repoRoot,
    env,
    stdio: ["pipe", "pipe", "pipe"],
    windowsHide: true,
  });
  proc.stdin!.write(token + "\n");
  if (proc.stderr) {
    createInterface({ input: proc.stderr }).on("line", (line) => opts.onLog?.(line));
  }
  return new Promise((resolveReady, reject) => {
    const timer = setTimeout(() => reject(new Error("后端 30 秒内没有就绪")), 30_000);
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
        reject(new Error(`后端就绪行无法解析：${line} (${String(err)})`));
      }
    });
    proc.once("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(`后端启动失败（退出码 ${code}）`));
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
