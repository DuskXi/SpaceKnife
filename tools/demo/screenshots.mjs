// README / website screenshots and the demo video from mock data: nothing from the real disks,
// nothing shown on screen.
//
//   node tools/demo/screenshots.mjs --home C:\Users\Public\sk-screens --out docs/images
//
// Writes the two synthetic snapshots of the `mock_snapshot` example (crates/sk-store/examples) into
// --home (an isolated SPACEKNIFE_HOME, wiped first; pick a neutral folder: paths under it can show
// up in toasts), starts `spaceknife-cli headless` for that home serving the built interface
// (apps/desktop/.output/public: `pnpm generate` in apps/desktop first), and drives a headless
// Chrome or Edge through the DevTools protocol (Node's own fetch and WebSocket, no packages): the
// treemap with a hover card, a filter, the command palette and a snapshot comparison, as PNG files.
//
//   --lang en|zh-CN|both  the interface language (en adds `*-en` to the names; default zh-CN)
//   --views               the three visualizations of the same volume instead (treemap, flame graph,
//                         polar flame graph; details panel hidden, 1440 × 760 at 1.25×, detail 5) as
//                         WebP for the website: site/src/assets/visualizations/{treemap,flame,polar}[-en].webp
//   --video               record the short demo instead (hover, zoom into a folder, the flame graph and
//                         the polar flame graph, a typed filter, the palette, a comparison): frames from
//                         the page's screencast, encoded by ffmpeg (on PATH) into demo[-en].mp4 and .gif
//   --out DIR             default docs/images (--views: site/src/assets/visualizations)
//   --browser PATH        Chrome / Edge / Chromium (default: $CHROME_PATH, then the usual install places)
//   --cli PATH            a built spaceknife-cli (default: `cargo build -p sk-server --bin spaceknife-cli`)
//   --ui DIR              the built interface (default apps/desktop/.output/public)
//   --theme dark|light    default dark, as the published pictures
//   --width/--height/--zoom/--level  the page size in CSS pixels, pixels per CSS pixel, and the detail
//                         level for --views (also SHOT_WIDTH, SHOT_HEIGHT, SHOT_ZOOM, SHOT_LEVEL)
//
// Only views of the mock snapshot are captured, never the start page (it lists the machine's real
// drives); the status bar is cropped off (it shows the free space of the machine's system drive) and
// the page runs in UTC, as the mock data has its dates. The page is opened in the demo form of the
// browser shim (apps/desktop/app/shim.ts: `#ws=…&token=…&platform=win32`), so it looks like the app
// window and asks for the snapshot's path with window.prompt, which this script answers.

import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const VIEWS_OUT = path.join(ROOT, "site", "src", "assets", "visualizations");
const WEBP_QUALITY = 90;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (msg) => console.error(`${new Date().toISOString().slice(11, 23)} ${msg}`);

// ------------------------------------------------------------------ command line

const { values: opt } = parseArgs({
  options: {
    home: { type: "string" },
    out: { type: "string" },
    lang: { type: "string", default: "zh-CN" },
    video: { type: "boolean", default: false },
    views: { type: "boolean", default: false },
    browser: { type: "string" },
    cli: { type: "string" },
    ui: { type: "string" },
    theme: { type: "string", default: process.env.SHOT_THEME || "dark" },
    width: { type: "string" },
    height: { type: "string" },
    zoom: { type: "string" },
    level: { type: "string" },
    help: { type: "boolean", short: "h", default: false },
  },
});

function usage(code) {
  const text = fs.readFileSync(fileURLToPath(import.meta.url), "utf8").split(/\r?\n/);
  const end = text.findIndex((l) => !l.startsWith("//"));
  console.error(text.slice(0, end).map((l) => l.replace(/^\/\/ ?/, "")).join("\n"));
  process.exit(code);
}

if (opt.help) usage(0);
if (!opt.home || !["zh-CN", "en", "both"].includes(opt.lang) || (opt.video && opt.views)) usage(2);

const SET = opt.video ? "video" : opt.views ? "views" : "readme";
const num = (flag, env, fallback) => Number(flag ?? process.env[`SHOT_${env}`] ?? fallback);
const WIDTH = num(opt.width, "WIDTH", 1440);
const HEIGHT = num(opt.height, "HEIGHT", SET === "views" ? 760 : 900);
const ZOOM = SET === "video" ? 1 : num(opt.zoom, "ZOOM", SET === "views" ? 1.25 : 1);
const LEVEL = num(opt.level, "LEVEL", 5);
const HOME = path.resolve(opt.home);
const OUT = path.resolve(opt.out ?? (SET === "views" ? VIEWS_OUT : path.join(ROOT, "docs", "images")));
const UI = path.resolve(opt.ui ?? path.join(ROOT, "apps", "desktop", ".output", "public"));

// ------------------------------------------------------------------ tools

function run(cmd, args, { capture = false } = {}) {
  const r = spawnSync(cmd, args, { cwd: ROOT, encoding: "utf8", stdio: capture ? ["ignore", "pipe", "inherit"] : "inherit", maxBuffer: 64 << 20 });
  if (r.error) throw r.error;
  if (r.status !== 0) throw new Error(`${cmd} ${args.join(" ")} exited with ${r.status}`);
  return r.stdout;
}

/** spaceknife-cli: the one given, else built now (cargo prints where it put it). */
function cliPath() {
  if (opt.cli) return path.resolve(opt.cli);
  log("cargo build -p sk-server --bin spaceknife-cli");
  let out;
  try {
    out = run("cargo", ["build", "-p", "sk-server", "--bin", "spaceknife-cli", "--message-format=json-render-diagnostics"], { capture: true });
  } catch (err) {
    // typically the executable is running (Windows keeps it locked): use the one there is
    const built = path.join(process.env.CARGO_TARGET_DIR ?? path.join(ROOT, "target"), "debug", `spaceknife-cli${process.platform === "win32" ? ".exe" : ""}`);
    if (!fs.existsSync(built)) throw err;
    log(`${err.message}; using the existing ${built} (pass --cli to choose)`);
    return built;
  }
  for (const line of out.split(/\r?\n/).reverse()) {
    if (!line.startsWith("{")) continue;
    const m = JSON.parse(line);
    if (m.reason === "compiler-artifact" && m.target?.name === "spaceknife-cli" && m.executable) return m.executable;
  }
  throw new Error("cargo built no spaceknife-cli");
}

/** A Chromium-family browser: --browser, $CHROME_PATH, then the usual install places. */
function browserPath() {
  const given = opt.browser ?? process.env.CHROME_PATH;
  if (given) return given;
  const pf = [process.env.PROGRAMFILES, process.env["PROGRAMFILES(X86)"], process.env.LOCALAPPDATA].filter(Boolean);
  const candidates =
    process.platform === "win32"
      ? pf.flatMap((d) => [
          path.join(d, "Google", "Chrome", "Application", "chrome.exe"),
          path.join(d, "Microsoft", "Edge", "Application", "msedge.exe"),
          path.join(d, "Chromium", "Application", "chrome.exe"),
        ])
      : process.platform === "darwin"
        ? [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
          ]
        : ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge"].flatMap((n) =>
            (process.env.PATH ?? "").split(path.delimiter).map((d) => path.join(d, n)),
          );
  const found = candidates.find((c) => fs.existsSync(c));
  if (!found) throw new Error("no Chrome or Edge found: pass --browser PATH or set CHROME_PATH");
  return found;
}

function which(name) {
  const exts = process.platform === "win32" ? (process.env.PATHEXT ?? ".EXE").split(";") : [""];
  for (const dir of (process.env.PATH ?? "").split(path.delimiter)) {
    for (const ext of exts) {
      const p = path.join(dir, name + ext.toLowerCase());
      if (dir && fs.existsSync(p)) return p;
    }
  }
  return null;
}

/** Waits for a child's stdout to match; the match, or an error once it exits or times out. */
function waitOutput(child, re, timeout = 120000) {
  return new Promise((resolve, reject) => {
    let text = "";
    const done = (err, m) => {
      clearTimeout(timer);
      child.stdout.off("data", onData);
      child.off("exit", onExit);
      err ? reject(err) : resolve(m);
    };
    const onData = (chunk) => {
      text += chunk;
      const m = text.match(re);
      if (m) done(null, m);
    };
    const onExit = (code) => done(new Error(`exited with ${code} before it was ready:\n${text}`));
    const timer = setTimeout(() => done(new Error(`not ready after ${timeout / 1000} s:\n${text}`)), timeout);
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", onData);
    child.on("exit", onExit);
  });
}

function stop(child) {
  if (!child || child.exitCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    const t = setTimeout(resolve, 10000);
    child.once("exit", () => (clearTimeout(t), resolve()));
    child.kill();
  });
}

// ------------------------------------------------------------------ the DevTools protocol

class Cdp {
  static async connect(url) {
    const ws = new WebSocket(url);
    await new Promise((resolve, reject) => {
      ws.addEventListener("open", resolve, { once: true });
      ws.addEventListener("error", () => reject(new Error(`cannot connect to ${url}`)), { once: true });
    });
    return new Cdp(ws);
  }

  constructor(ws) {
    this.ws = ws;
    this.next = 1;
    this.pending = new Map();
    this.handlers = new Map();
    ws.addEventListener("message", (e) => {
      const m = JSON.parse(e.data);
      if (m.id) {
        const p = this.pending.get(m.id);
        this.pending.delete(m.id);
        if (m.error) p?.reject(new Error(`${p.method}: ${m.error.message}`));
        else p?.resolve(m.result);
      } else for (const cb of this.handlers.get(m.method) ?? []) cb(m.params);
    });
    ws.addEventListener("close", () => {
      for (const p of this.pending.values()) p.reject(new Error("the browser closed the connection"));
      this.pending.clear();
    });
  }

  send(method, params = {}) {
    const id = this.next++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject, method });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  on(method, cb) {
    this.handlers.set(method, [...(this.handlers.get(method) ?? []), cb]);
  }

  close() {
    this.ws.close();
  }
}

/** A headless browser with a temporary profile; resolves to { proc, cdp, profile } on its page. */
async function launchBrowser(exe, lang) {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), "sk-shots-"));
  const args = [
    "--headless=new",
    "--remote-debugging-port=0",
    `--user-data-dir=${profile}`,
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-extensions",
    "--disable-sync",
    "--disable-features=Translate,MediaRouter",
    "--hide-scrollbars",
    "--mute-audio",
    "--force-color-profile=srgb",
    "--disable-background-timer-throttling",
    "--disable-renderer-backgrounding",
    "--disable-backgrounding-occluded-windows",
    `--lang=${lang === "en" ? "en-US" : "zh-CN"}`,
    `--window-size=${Math.round(WIDTH * ZOOM)},${Math.round(HEIGHT * ZOOM)}`,
    "about:blank",
  ];
  const proc = spawn(exe, args, { stdio: "ignore" });
  const portFile = path.join(profile, "DevToolsActivePort");
  const end = Date.now() + 30000;
  while (!fs.existsSync(portFile) || !fs.readFileSync(portFile, "utf8").includes("\n")) {
    if (proc.exitCode !== null) throw new Error(`${exe} exited with ${proc.exitCode}`);
    if (Date.now() > end) throw new Error(`${exe} opened no DevTools port`);
    await sleep(100);
  }
  const port = fs.readFileSync(portFile, "utf8").split(/\r?\n/)[0];
  let page;
  while (!page) {
    const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    page = list.find((t) => t.type === "page");
    if (!page) await sleep(100);
  }
  return { proc, profile, cdp: await Cdp.connect(page.webSocketDebuggerUrl) };
}

async function closeBrowser(b) {
  if (!b) return;
  try {
    await b.cdp.send("Browser.close");
  } catch {
    // already gone
  }
  b.cdp.close();
  await stop(b.proc);
  for (let i = 0; i < 20; i++) {
    try {
      fs.rmSync(b.profile, { recursive: true, force: true });
      return;
    } catch {
      await sleep(250); // the browser's last files are still open
    }
  }
}

/** The page: evaluate, wait, keys, mouse, text. */
function pageApi(cdp) {
  const js = async (code) => {
    const r = await cdp.send("Runtime.evaluate", { expression: code, awaitPromise: true, returnByValue: true, userGesture: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
    return r.result.value;
  };
  const until = async (what, code, timeout = 60000) => {
    const end = Date.now() + timeout;
    for (;;) {
      const v = await js(code).catch(() => null);
      if (v) return v;
      if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
      await sleep(250);
    }
  };
  // key names as KeyboardEvent.key, with the code and Windows key code the page may look at
  const KEYS = {
    Enter: ["Enter", 13, "\r"],
    Escape: ["Escape", 27],
    Backspace: ["Backspace", 8],
    "=": ["Equal", 187, "="],
    "-": ["Minus", 189, "-"],
  };
  const MOD = { alt: 1, control: 2, meta: 4, shift: 8 };
  const key = async (name, modifiers = [], pause = 150) => {
    const letter = /^[A-Za-z]$/.test(name);
    const [code, vk, text] = letter ? [`Key${name.toUpperCase()}`, name.toUpperCase().charCodeAt(0), name.toLowerCase()] : KEYS[name];
    const mods = modifiers.reduce((m, k) => m | MOD[k], 0);
    const k = letter ? name.toLowerCase() : name;
    const typed = text && !(mods & (MOD.control | MOD.alt | MOD.meta));
    const base = { key: k, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk, modifiers: mods };
    await cdp.send("Input.dispatchKeyEvent", { type: typed ? "keyDown" : "rawKeyDown", ...base, ...(typed ? { text, unmodifiedText: text } : {}) });
    await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", ...base });
    await sleep(pause);
  };
  const insertText = (text) => cdp.send("Input.insertText", { text });
  let mouse = { x: WIDTH / 2, y: HEIGHT / 2 };
  // mouse positions are CSS pixels of the page
  const move = (x, y) => {
    mouse = { x: Math.round(x), y: Math.round(y) };
    return cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", ...mouse });
  };
  const glide = async (x, y, ms = 700) => {
    const from = { ...mouse };
    const steps = Math.max(1, Math.round(ms / 25));
    for (let i = 1; i <= steps; i++) {
      const t = i / steps;
      const e = t < 0.5 ? 2 * t * t : 1 - (-2 * t + 2) ** 2 / 2;
      await move(from.x + (x - from.x) * e, from.y + (y - from.y) * e);
      await sleep(ms / steps);
    }
  };
  const click = async (count = 1) => {
    for (let n = 1; n <= count; n++) {
      await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", ...mouse, button: "left", buttons: 1, clickCount: n });
      await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", ...mouse, button: "left", buttons: 0, clickCount: n });
      await sleep(70);
    }
  };
  const type = async (text, ms = 70) => {
    for (const ch of text) {
      await insertText(ch);
      await sleep(ms);
    }
  };
  return { js, until, key, insertText, move, glide, click, type };
}

const LARGEST_CANVAS = `(() => { const c = [...document.querySelectorAll("canvas")].sort((a, b) => b.width * b.height - a.width * a.height)[0].getBoundingClientRect(); return { x: c.x, y: c.y, w: c.width, h: c.height }; })()`;
const FILTER_INPUT = `document.querySelector(".filter:not(.pane-filter) input") ?? document.querySelector(".filter input")`;

// ------------------------------------------------------------------ the picture sets

/** README and landing page: the treemap with a hover card, a filter, the palette, a comparison. */
async function readme(p, shot, lang) {
  const r = await p.js(LARGEST_CANVAS);
  // 1. the treemap, with the hover card over a large file
  await p.move(r.x + r.w * 0.3, r.y + r.h * 0.45);
  await sleep(800);
  await shot("treemap");
  await p.move(r.x + r.w - 2, r.y - 30);

  // 2. a filter: videos over 1 GB, the matches listed beside the treemap
  const setFilter = (text) =>
    p.js(
      `(() => { const i = ${FILTER_INPUT}; i.focus(); Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(i, ${JSON.stringify(text)}); i.dispatchEvent(new Event("input", { bubbles: true })); return true; })()`,
    );
  await setFilter(lang === "en" ? ":class:Video;>1gb" : ":class:视频;>1gb");
  await p.key("Enter");
  await sleep(2500);
  await shot("filter");
  await setFilter("");
  await p.key("Enter");
  await p.key("Escape");
  await sleep(1500);

  // 3. the command palette searching the scan
  await p.key("K", ["control"]);
  await sleep(500);
  await p.insertText("mountain");
  await sleep(3000);
  await shot("palette");
  await p.key("Escape");
  await sleep(600);

  // 4. snapshot comparison: the command takes the newest other snapshot (a month earlier) as base
  await p.key("K", ["control"]);
  await sleep(500);
  await p.insertText(lang === "en" ? "compare" : "对比");
  await sleep(1500);
  await p.key("Enter");
  await sleep(8000);
  await shot("compare");
}

/** The website's visualization pages: the treemap, the flame graph and the polar flame graph of the
 *  same zoom root (the whole volume), each with the coloring a new view gives it, no hover, the
 *  details panel hidden so the picture has the window's width. */
async function views(p, shot) {
  const hidden = await p.js(
    `(() => { const b = [...document.querySelectorAll("button")].find((e) => /^(隐藏详情面板|Hide details panel)$/.test(e.getAttribute("aria-label") ?? "")); if (b) b.click(); return !!b; })()`,
  );
  if (!hidden) throw new Error("no 'hide details panel' button");
  // and the button that shows it again, which would sit on the picture's top right corner
  await p.js(`(() => { const s = document.createElement("style"); s.textContent = ".inspector-open { visibility: hidden !important; }"; document.head.append(s); return true; })()`);
  await p.move(WIDTH - 2, HEIGHT - 2);
  // the detail level of the view (a new view starts at 5, the setting's default)
  for (let i = 5; i !== LEVEL; i += Math.sign(LEVEL - 5)) await p.key(LEVEL > 5 ? "=" : "-", ["control"]);
  const CURRENT = `[...document.querySelectorAll('[role="radio"][aria-checked="true"]')].map((e) => e.getAttribute("aria-label")).join(" | ")`;
  await sleep(2500);
  for (const [i, name] of ["treemap", "flame", "polar"].entries()) {
    if (i > 0) {
      const before = await p.js(CURRENT);
      await p.key("V");
      await p.until(`the ${name} view`, `${JSON.stringify(before)} !== ${CURRENT}`, 10000);
      await sleep(2500); // the cross-fade and the labels
    }
    log(`${name}: ${await p.js(CURRENT)}`);
    await shot(name);
  }
}

// a pointer and a click ripple drawn into the page (headless frames have no cursor)
const POINTER = `(() => {
  const s = document.createElement("style");
  s.textContent = \`.sk-cursor{position:fixed;left:0;top:0;width:22px;height:22px;z-index:2147483647;pointer-events:none;transform:translate(-3px,-2px);filter:drop-shadow(0 2px 3px rgb(0 0 0 / .6))}
  .sk-ripple{position:fixed;width:34px;height:34px;margin:-17px 0 0 -17px;border-radius:50%;border:2px solid #d4ff3a;z-index:2147483646;pointer-events:none;animation:skr .5s ease-out forwards}
  @keyframes skr{from{transform:scale(.3);opacity:1}to{transform:scale(1.4);opacity:0}}\`;
  document.head.append(s);
  const c = document.createElement("div");
  c.className = "sk-cursor";
  c.innerHTML = '<svg viewBox="0 0 22 22" width="22" height="22"><path d="M3 2l14 11-6.5 1 3.5 6.5-2.6 1.4-3.5-6.6L3 19z" fill="#fff" stroke="#0b0c10" stroke-width="1.4" stroke-linejoin="round"/></svg>';
  document.body.append(c);
  addEventListener("mousemove", (e) => (c.style.left = e.clientX + "px", c.style.top = e.clientY + "px"), true);
  addEventListener("mousedown", (e) => { const r = document.createElement("div"); r.className = "sk-ripple"; r.style.left = e.clientX + "px"; r.style.top = e.clientY + "px"; document.body.append(r); setTimeout(() => r.remove(), 600); }, true);
  return true;
})()`;

const FPS = 20;
const VIDEO_W = 1280;

/** The short demo: the page's screencast, the newest frame saved FPS times a second as JPEG. */
async function video(p, cdp, frames, lang) {
  fs.rmSync(frames, { recursive: true, force: true });
  fs.mkdirSync(frames, { recursive: true });
  await p.js(POINTER);
  await p.move(1000, 300);
  await sleep(1000);
  const canvas = () => p.js(LARGEST_CANVAS);
  const r = await canvas();
  const at = (fx, fy) => [r.x + r.w * fx, r.y + r.h * fy];
  // the toolbar's visualization switch: treemap, flame graph, polar flame graph
  const VIS = `[...document.querySelectorAll(".segmented.icon-only [role=radio]")]`;
  const visButtons = () => p.js(`${VIS}.map((b) => { const r = b.getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })`);
  const pickVisual = (i) => p.js(`(() => { const b = ${VIS}[${i}]; if (b) b.click(); return !!b; })()`);
  // what the hover card names under (x, y)
  const titleAt = async (x, y) => {
    await p.move(x, y);
    await sleep(420);
    return p.js(`document.querySelector(".hovercard .hc-title")?.textContent ?? ""`);
  };
  // walk from (x, y) in steps of (dx, dy): the first title is the zoom root, the next one a folder
  // right under it; its middle along the walk is where to point (found by name, so either language)
  const childAlong = async (x, y, dx, dy, steps) => {
    let root = null;
    let child = null;
    let first = -1;
    let last = -1;
    for (let i = 0; i < steps; i++) {
      const name = await titleAt(x + dx * i, y + dy * i);
      if (!name) {
        if (child) break;
        continue;
      }
      if (root === null || (child === null && name === root)) {
        root = name;
        continue;
      }
      if (child === null) {
        child = name;
        first = i;
      }
      if (name !== child) break;
      last = i;
    }
    if (child === null) throw new Error(`no folder found under ${root}`);
    const mid = (first + last) / 2;
    return { x: x + dx * mid, y: y + dy * mid, name: child };
  };

  // before recording: where the flame graph and the polar flame graph draw a big folder under the
  // root (the same mock data and page size give the same layout during the recording)
  if (!(await pickVisual(1))) throw new Error("no visualization switch in the toolbar");
  await sleep(2500);
  const fr = await canvas();
  const flame = await childAlong(fr.x + fr.w * 0.1, fr.y + fr.h - 3, 0, -6, 40); // root bar at the bottom
  log(`flame: ${flame.name} at ${Math.round(flame.x)},${Math.round(flame.y)}`);
  await pickVisual(2);
  await sleep(2500);
  const pr = await canvas();
  const ray = (35 * Math.PI) / 180; // clockwise from twelve o'clock, where the biggest sectors start
  const polar = await childAlong(pr.x + pr.w / 2, pr.y + pr.h / 2, 6 * Math.sin(ray), -6 * Math.cos(ray), 60);
  log(`polar: ${polar.name} at ${Math.round(polar.x)},${Math.round(polar.y)}`);
  await pickVisual(0);
  await p.move(1000, 300);
  await sleep(3000);

  // frames: the screencast sends one per paint (acknowledged, or it stops); a timer keeps the newest
  let latest = null;
  let frame = 0;
  cdp.on("Page.screencastFrame", (f) => {
    latest = f.data;
    cdp.send("Page.screencastFrameAck", { sessionId: f.sessionId }).catch(() => {});
  });
  await cdp.send("Page.startScreencast", { format: "jpeg", quality: 92, maxWidth: VIDEO_W, maxHeight: Math.round((HEIGHT * VIDEO_W) / WIDTH), everyNthFrame: 1 });
  await sleep(500);
  const timer = setInterval(() => {
    if (latest) fs.writeFileSync(path.join(frames, `f${String(frame++).padStart(5, "0")}.jpg`), Buffer.from(latest, "base64"));
  }, 1000 / FPS);
  log("recording");
  try {
    // 1. hover across the drive
    await sleep(600);
    await p.glide(...at(0.62, 0.3), 900);
    await sleep(600);
    await p.glide(...at(0.3, 0.45), 900);
    await sleep(700);
    // 2. zoom into Videos (double-click its title) and back out
    await p.glide(r.x + 40, r.y + 8, 600);
    await sleep(300);
    await p.click(2);
    await sleep(1700);
    await p.glide(...at(0.45, 0.5), 800);
    await sleep(1000);
    await p.key("Backspace");
    await sleep(1400);
    // 3. the flame graph from the toolbar: hover, zoom into a folder (the bars stretch) and back
    const vis = await visButtons();
    await p.glide(vis[1].x, vis[1].y, 900);
    await sleep(250);
    await p.click();
    await sleep(1400);
    await p.glide(flame.x + fr.w * 0.35, flame.y - 40, 900);
    await sleep(800);
    await p.glide(flame.x, flame.y, 700);
    await sleep(400);
    await p.click(2);
    await sleep(1800);
    await p.glide(fr.x + fr.w * 0.55, flame.y - 60, 800);
    await sleep(900);
    await p.key("Backspace");
    await sleep(1400);
    // 4. the polar flame graph: the same tree in rings; zoom into a sector and back
    await p.glide(vis[2].x, vis[2].y, 900);
    await sleep(250);
    await p.click();
    await sleep(1500);
    await p.glide(polar.x + (polar.x - (pr.x + pr.w / 2)) * 0.6, polar.y + (polar.y - (pr.y + pr.h / 2)) * 0.6, 900);
    await sleep(900);
    await p.glide(polar.x, polar.y, 700);
    await sleep(400);
    await p.click(2);
    await sleep(2000);
    await p.glide(pr.x + pr.w * 0.38, pr.y + pr.h * 0.6, 800);
    await sleep(900);
    await p.key("Backspace");
    await sleep(1400);
    // back to the treemap
    await p.glide(vis[0].x, vis[0].y, 800);
    await sleep(250);
    await p.click();
    await sleep(1400);
    // 5. a filter, typed
    const f = await p.js(`(() => { const i = (${FILTER_INPUT}).getBoundingClientRect(); return { x: i.x + 40, y: i.y + i.height / 2 }; })()`);
    await p.glide(f.x, f.y, 700);
    await p.click();
    await sleep(300);
    await p.type(lang === "en" ? ":class:Video;>1gb" : ":class:视频;>1gb", 75);
    await p.key("Enter");
    await p.glide(...at(0.75, 0.62), 900);
    await sleep(2200);
    await p.js(`(() => { const i = ${FILTER_INPUT}; i.focus(); i.select(); return true; })()`);
    await p.key("Backspace");
    await p.key("Enter");
    await p.key("Escape");
    await sleep(900);
    // 6. the command palette
    await p.key("K", ["control"]);
    await sleep(400);
    await p.type("mountain", 90);
    await sleep(1800);
    await p.key("Escape");
    await sleep(500);
    // 7. compare with the snapshot from five weeks earlier
    await p.key("K", ["control"]);
    await sleep(400);
    await p.type(lang === "en" ? "compare" : "对比", 90);
    await sleep(700);
    await p.key("Enter");
    await sleep(2600);
    await p.glide(...at(0.2, 0.3), 900);
    await sleep(2000);
  } finally {
    clearInterval(timer);
    await cdp.send("Page.stopScreencast").catch(() => {});
  }
  log(`frames ${frame} (${(frame / FPS).toFixed(1)} s)`);
}

/** MP4 (1280 px, H.264, plays everywhere) and GIF (880 px, 12 fps, optimised palette) of the frames,
 *  the status bar cropped off (`bar`: its top in CSS pixels). */
function encode(frames, bar, suffix) {
  const ffmpeg = which("ffmpeg");
  if (!ffmpeg) throw new Error("ffmpeg is not on PATH");
  fs.mkdirSync(OUT, { recursive: true });
  const crop = `crop=iw:trunc(ih*${bar / HEIGHT}/2)*2:0:0`;
  const base = [ffmpeg, "-y", "-loglevel", "error", "-framerate", String(FPS), "-i", path.join(frames, "f%05d.jpg")];
  const mp4 = path.join(OUT, `demo${suffix}.mp4`);
  const gif = path.join(OUT, `demo${suffix}.gif`);
  // H.264 with yuv420p needs even sizes
  run(base[0], [...base.slice(1), "-vf", `${crop},scale=trunc(iw/2)*2:trunc(ih/2)*2`, "-c:v", "libx264", "-preset", "slow", "-crf", "23", "-pix_fmt", "yuv420p", "-movflags", "+faststart", mp4]);
  const palette = `${crop},fps=12,scale=880:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=200:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle`;
  run(base[0], [...base.slice(1), "-vf", palette, gif]);
  for (const f of [mp4, gif]) console.log(`${f}  ${(fs.statSync(f).size / 2 ** 20).toFixed(1)} MB`);
}

// ------------------------------------------------------------------ one language

async function capture(cli, browserExe, snapshot, lang) {
  const suffix = lang === "en" ? "-en" : "";
  let server;
  let browser;
  try {
    // the backend, serving the interface, for the demo home
    server = spawn(cli, ["headless", "--listen", "127.0.0.1:0", "--ui", UI], {
      env: { ...process.env, SPACEKNIFE_HOME: HOME, SPACEKNIFE_LANGUAGE: lang, NO_COLOR: "1" },
      stdio: ["ignore", "pipe", "inherit"],
    });
    const m = await waitOutput(server, /https?:\/\/[^\s/]+:(\d+)\/#token=([^\s]+)/);
    const port = m[1];
    const token = decodeURIComponent(m[2]);
    // the demo form of the shim: the app's window look, window.prompt for paths
    const url = `http://127.0.0.1:${port}/#ws=${encodeURIComponent(`ws://127.0.0.1:${port}/v1`)}&token=${encodeURIComponent(token)}&platform=win32`;
    log(`${lang}: backend on port ${port}`);

    browser = await launchBrowser(browserExe, lang);
    const { cdp } = browser;
    const p = pageApi(cdp);
    await cdp.send("Page.enable");
    await cdp.send("Runtime.enable");
    // window and capture sizes: CSS pixels, drawn at ZOOM pixels each
    await cdp.send("Emulation.setDeviceMetricsOverride", { width: WIDTH, height: HEIGHT, deviceScaleFactor: ZOOM, mobile: false });
    // dates in UTC, as the mock data has them: not the time zone of the machine taking the shots
    await cdp.send("Emulation.setTimezoneOverride", { timezoneId: "UTC" });
    // the page follows prefers-color-scheme: fixed (dark by default, as the published pictures)
    await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: opt.theme === "light" ? "light" : "dark" }] });
    // a headless page has no focus of its own: focus events and :focus as in a window in front
    await cdp.send("Emulation.setFocusEmulationEnabled", { enabled: true });
    await cdp.send("Page.navigate", { url });
    await p.until("the start page", `!!document.querySelector(".start-main")`);
    const zone = await p.js(`Intl.DateTimeFormat().resolvedOptions().timeZone`);
    if (zone !== "UTC") throw new Error(`time zone is ${zone}, not UTC`);
    const dpr = await p.js(`window.devicePixelRatio`);
    if (Math.abs(dpr - ZOOM) > 0.01) throw new Error(`the page is drawn at ${dpr}x, not ${ZOOM}x`);

    // open the newest mock snapshot: the start page's button asks for the path (window.prompt here)
    await p.js(`window.prompt = () => ${JSON.stringify(snapshot)}; true`);
    await sleep(1500); // the start page's handlers settle (an earlier click is lost)
    const opened = await p.js(
      `(() => { const b = [...document.querySelectorAll("button")].filter((e) => !e.querySelector("button") && /打开快照文件…|Open snapshot file…/.test(e.textContent)).pop(); if (b) b.click(); return b ? b.outerHTML.slice(0, 120) : false; })()`,
    );
    if (!opened) throw new Error("no 'open snapshot file' button");
    log(`opening the snapshot: clicked ${opened}`);
    await p.until("the treemap", `!!document.querySelector("canvas") && !document.querySelector(".start-main")`);
    const bar = await p.js(`(() => { const b = document.querySelector(".statusbar"); return b ? Math.ceil(b.getBoundingClientRect().top) : innerHeight; })()`);
    await p.js(`(() => { const s = document.createElement("style"); s.textContent = ".statusbar { visibility: hidden !important; }"; document.head.append(s); return true; })()`);
    await sleep(4000); // layout, the first frames, and the toasts fade

    const webp = SET === "views";
    const shot = async (name) => {
      await sleep(900);
      const r = await cdp.send("Page.captureScreenshot", {
        format: webp ? "webp" : "png",
        ...(webp ? { quality: WEBP_QUALITY } : {}),
        clip: { x: 0, y: 0, width: WIDTH, height: bar, scale: 1 },
        captureBeyondViewport: false,
      });
      fs.mkdirSync(OUT, { recursive: true });
      const file = path.join(OUT, `${name}${suffix}.${webp ? "webp" : "png"}`);
      fs.writeFileSync(file, Buffer.from(r.data, "base64"));
      console.log(`${file}  ${Math.round(WIDTH * ZOOM)}x${Math.floor(bar * ZOOM)}, ${(fs.statSync(file).size / 1024).toFixed(0)} KB`);
    };
    if (SET === "readme") await readme(p, shot, lang);
    else if (SET === "views") await views(p, shot);
    else {
      const frames = path.join(HOME, `frames-${lang}`);
      await video(p, cdp, frames, lang);
      encode(frames, bar, suffix);
    }
    return 0;
  } catch (err) {
    log(`error: ${err && err.stack ? err.stack : err}`);
    if (browser) {
      const state = await pageApi(browser.cdp)
        .js(`[document.querySelectorAll("canvas").length, !!document.querySelector(".start-main"), [...document.querySelectorAll(".toast, .op-toast, [role=alert], [role=status]")].map((e) => e.textContent).join(" / ")].join(" | ")`)
        .catch(() => "");
      log(`page: canvases | start page | toasts: ${String(state).slice(0, 600)}`);
    }
    return 1;
  } finally {
    await closeBrowser(browser);
    await stop(server);
  }
}

// ------------------------------------------------------------------ main

async function main() {
  if (!fs.existsSync(path.join(UI, "index.html"))) {
    console.error(`no built interface in ${UI}: run \`pnpm generate\` in apps/desktop first (or pass --ui)`);
    return 1;
  }
  // --home is wiped: refuse a folder that holds anything but an earlier run's files
  if (fs.existsSync(HOME)) {
    const known = new Set(["data", "config", "cache", "frames-zh-CN", "frames-en"]);
    const other = fs.readdirSync(HOME).filter((n) => !known.has(n));
    if (other.length) {
      console.error(`${HOME} holds other files (${other.slice(0, 5).join(", ")}): give an empty or new folder`);
      return 2;
    }
    fs.rmSync(HOME, { recursive: true, force: true });
  }
  fs.mkdirSync(HOME, { recursive: true });
  const browserExe = browserPath();
  const cli = cliPath();
  log("cargo run -p sk-store --example mock_snapshot");
  const out = run("cargo", ["run", "-q", "-p", "sk-store", "--example", "mock_snapshot", "--", "--home", HOME], { capture: true });
  process.stderr.write(out);
  const snaps = [...out.matchAll(/^(.*\.sks)\s{2}/gm)].map((m) => m[1]);
  if (!snaps.length) throw new Error("mock_snapshot wrote no snapshot");
  const newest = snaps.at(-1);
  log(`browser ${browserExe}`);
  let code = 0;
  for (const lang of opt.lang === "both" ? ["zh-CN", "en"] : [opt.lang]) code |= await capture(cli, browserExe, newest, lang);
  return code;
}

main().then(
  (code) => process.exit(code),
  (err) => {
    console.error(err && err.stack ? err.stack : err);
    process.exit(1);
  },
);
