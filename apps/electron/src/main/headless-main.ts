// Headless mode, put together: index.ts runs this instead of opening the window. Ask before opening
// SpaceKnife to the network, start the backend (on 127.0.0.1, as always), serve the interface and
// forward the WebSocket (headless-server.ts), print where to open it, and stop on Ctrl+C.

import { readFile } from "node:fs/promises";
import { homedir, hostname, networkInterfaces, userInfo } from "node:os";

import { app } from "electron";

import type { Backend } from "./backend";
import { readLine } from "./console";
import {
  type CommandLine,
  HEADLESS_ORIGIN,
  type Lang,
  browserUrls,
  confirmed,
  exposure,
  ht,
  langOf,
  listenError,
  questionInput,
  warningLines,
} from "./headless";
import { type HeadlessInfo, type HeadlessServer, createHeadlessServer } from "./headless-server";

type Headless = Extract<CommandLine, { mode: "headless" }>;

export interface HeadlessDeps {
  rendererDir: string;
  /** the app's web panel SDK (resources/plugin-sdk): plugins' panels load it */
  pluginSdkDir?: string;
  /** start the backend, allowing these WebSocket origins */
  startBackend(allowOrigins: string[], onLog: (line: string) => void): Promise<Backend>;
}

/** Before the app is ready: no GPU, no Dock icon. Chromium's own --headless switch is what lets the
 *  app start on a Linux machine without a display (X11 or Wayland), so `--listen` alone adds it. */
export function prepareHeadless(): void {
  if (!app.commandLine.hasSwitch("headless")) app.commandLine.appendSwitch("headless");
  app.commandLine.appendSwitch("disable-gpu");
  app.disableHardwareAcceleration();
  if (process.platform === "darwin") app.dock?.hide();
}

export function systemLang(): Lang {
  let tag: string | undefined;
  try {
    tag = app.getPreferredSystemLanguages()[0] ?? app.getLocale();
  } catch {
    tag = undefined;
  }
  return langOf(tag || process.env.LC_ALL || process.env.LANG || Intl.DateTimeFormat().resolvedOptions().locale);
}

function userName(): string {
  try {
    return userInfo().username;
  } catch {
    return process.env.USERNAME ?? process.env.USER ?? "?";
  }
}

/** Exit once what was written to the terminal is out (writes to a Windows console are asynchronous). */
export function exitFlushed(code: number): void {
  let pending = 2;
  const done = () => {
    if (--pending === 0) app.exit(code);
  };
  process.stdout.write("", done);
  process.stderr.write("", done);
}

const tty = (stream: NodeJS.WriteStream) => !!stream.isTTY;
const paint = (stream: NodeJS.WriteStream, code: string, text: string) => (tty(stream) ? `\x1b[${code}m${text}\x1b[0m` : text);

/** Backend log lines worth showing in the terminal: warnings and errors (with their tracebacks).
 *  Everything is in the log file as usual. */
export function logFilter(): (line: string) => boolean {
  let showing = false;
  return (line) => {
    const level = /^\S+ \| (\w+)\s*\|/.exec(line)?.[1];
    if (level) showing = level === "WARNING" || level === "ERROR" || level === "CRITICAL";
    return showing;
  };
}

export async function runHeadless(cli: Headless, deps: HeadlessDeps): Promise<void> {
  const lang = systemLang();
  const out = (s = "") => process.stdout.write(`${s}\n`);
  const err = (s = "") => process.stderr.write(`${s}\n`);
  const reach = exposure(cli.listen.host);
  // a packaged SpaceKnife.exe started straight from a terminal: the terminal didn't wait for it
  const straightExe = process.platform === "win32" && app.isPackaged && process.env.SPACEKNIFE_CONSOLE !== "1";

  let tls: { cert: Buffer; key: Buffer } | null = null;
  if (cli.tls) {
    try {
      tls = { cert: await readFile(cli.tls.cert), key: await readFile(cli.tls.key) };
    } catch (e) {
      err(ht(lang, "tlsRead", { error: e instanceof Error ? e.message : String(e) }));
      return exitFlushed(2);
    }
  }

  if (reach !== "loopback") {
    const [title, ...rest] = warningLines(lang, cli.listen, userName(), tls !== null);
    err();
    err(paint(process.stderr, "1;97;41", ` ⚠ ${title} `));
    for (const line of rest) err(paint(process.stderr, "33", `  ${line}`));
    err();
    if (cli.yes) err(ht(lang, "yesGiven"));
    else {
      const from = questionInput(process.platform, process.env, !!process.stdin.isTTY);
      if (!from) {
        err(ht(lang, "noTerminal"));
        if (straightExe) err(ht(lang, "windowsHint"));
        return exitFlushed(2);
      }
      process.stderr.write(paint(process.stderr, "1", ht(lang, "question")));
      const answer = await readLine(from);
      if (answer === null) err();
      if (!confirmed(answer)) {
        err(ht(lang, "declined"));
        return exitFlushed(1);
      }
    }
  }

  out(paint(process.stdout, "2", ht(lang, "starting")));
  const show = logFilter();
  let backend: Backend;
  try {
    backend = await deps.startBackend([HEADLESS_ORIGIN], (line) => show(line) && err(paint(process.stderr, "2", `[backend] ${line}`)));
  } catch (e) {
    err(ht(lang, "backendFailed", { error: e instanceof Error ? e.message : String(e) }));
    return exitFlushed(1);
  }

  const info: HeadlessInfo = {
    platform: process.platform as HeadlessInfo["platform"],
    version: app.getVersion(),
    hostname: hostname(),
    home: homedir(),
    secure: tls !== null,
  };
  const stamp = () => new Date().toTimeString().slice(0, 8);
  const server: HeadlessServer = createHeadlessServer({
    rendererDir: deps.rendererDir,
    backendUrl: backend.info.url,
    token: backend.info.token,
    info,
    tls,
    plugins: deps.pluginSdkDir ? { dirs: backend.pluginDirs, sdkDir: deps.pluginSdkDir } : null,
    onSocket: (event, remote) => out(`${paint(process.stdout, "2", stamp())} ${ht(lang, event === "open" ? "connected" : "disconnected", { remote })}`),
  });
  let port: number;
  try {
    port = await server.listen(cli.listen);
  } catch (e) {
    err(listenError(lang, cli.listen, e as NodeJS.ErrnoException));
    await backend.stop();
    return exitFlushed(1);
  }

  const interfaces = Object.values(networkInterfaces()).flatMap((list) => list ?? []);
  out();
  out(paint(process.stdout, "1", ht(lang, "ready", { version: info.version })));
  out(ht(lang, "openIn"));
  for (const url of browserUrls(cli.listen, port, backend.info.token, tls !== null, interfaces)) out(`  ${paint(process.stdout, "36", url)}`);
  if (reach === "loopback") out(paint(process.stdout, "2", ht(lang, "onlyHere", { port })));
  else if (!tls) out(paint(process.stdout, "33", ht(lang, "plainNote")));
  if (straightExe) out(paint(process.stdout, "2", ht(lang, "windowsHint")));
  out(ht(lang, "stopHint"));
  out();

  let stopping = false;
  const stop = async (code: number, note?: string) => {
    if (stopping) return;
    stopping = true;
    if (note) err(note);
    out(ht(lang, "stopping"));
    await server.close();
    await backend.stop();
    out(ht(lang, "stopped"));
    exitFlushed(code);
  };
  backend.proc.once("exit", (code) => void stop(1, ht(lang, "backendExited", { code: String(code) })));
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"] as const) process.on(signal, () => void stop(0));
  // Electron's own shutdown (a signal it handles, the system logging off): stop cleanly first
  app.on("before-quit", (event) => {
    if (stopping) return;
    event.preventDefault();
    void stop(0);
  });
}
