// Off-screen screenshots of the renderer (README, docs): run by tools/demo/screenshots.py with the
// repo's Electron as the main script. Nothing is shown on screen: the window is created hidden with
// offscreen rendering, driven through the page (clicks, keys, text) and captured to PNG files.
//
//   SHOT_URL=<page url> SHOT_SNAPSHOT=<file.sks> SHOT_OUT=<dir> electron tools/demo/screenshots.cjs
//
// Inputs come from the environment: Electron itself takes some command-line switches (a
// `--snapshot` argument makes it exit with 127 before the script runs). Progress and errors go to
// SHOT_LOG (a GUI process on Windows has no console). Only views of the mock
// snapshot are captured, never the start page: it lists the real drives of the machine; the status
// bar is cropped off (it shows the free space of the machine's system drive) and the time zone is
// UTC.

const { app, BrowserWindow, dialog } = require("electron");
const fs = require("node:fs");
const path = require("node:path");

const env = (name, fallback) => process.env[`SHOT_${name}`] || fallback;
const URL_ = env("URL");
const SNAPSHOT = env("SNAPSHOT");
if (!process.env.SHOT_URL || !process.env.SHOT_SNAPSHOT || !process.env.SHOT_OUT) app.exit(2); // run it through screenshots.py
const OUT = path.resolve(env("OUT", "."));
const SUFFIX = env("SUFFIX", "");
const WIDTH = Number(env("WIDTH", "1440"));
const HEIGHT = Number(env("HEIGHT", "900"));
const LOG = env("LOG", path.join(OUT, "screenshots.log")); // absolute paths: keep it out of the repo
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (msg) => fs.appendFileSync(LOG, `${new Date().toISOString()} ${msg}\n`);

fs.mkdirSync(OUT, { recursive: true });
// never an error dialog on the user's desktop: log and exit
dialog.showErrorBox = (title, content) => log(`${title}: ${content}`);
process.on("uncaughtException", (err) => {
  log(`uncaught: ${err && err.stack ? err.stack : err}`);
  app.exit(1);
});

app.whenReady().then(async () => {
  const win = new BrowserWindow({
    width: WIDTH,
    height: HEIGHT,
    show: false,
    webPreferences: { offscreen: true, backgroundThrottling: false },
  });
  const wc = win.webContents;
  wc.setFrameRate(30);
  const js = (code) => wc.executeJavaScript(code, true);
  const until = async (what, code, timeout = 60000) => {
    const end = Date.now() + timeout;
    for (;;) {
      const v = await js(code).catch(() => null);
      if (v) return v;
      if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
      await sleep(250);
    }
  };
  const key = async (keyCode, modifiers = []) => {
    wc.sendInputEvent({ type: "keyDown", keyCode, modifiers });
    wc.sendInputEvent({ type: "keyUp", keyCode, modifiers });
    await sleep(150);
  };
  let crop = null; // the window above the status bar
  const shot = async (name) => {
    await sleep(900);
    const image = await wc.capturePage(crop ?? undefined);
    const file = path.join(OUT, `${name}${SUFFIX}.png`);
    fs.writeFileSync(file, image.toPNG());
    log(`${file} ${image.getSize().width}x${image.getSize().height}`);
  };
  const setFilter = (text) =>
    js(`(() => { const i = document.querySelector(".filter input"); i.focus(); Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(i, ${JSON.stringify(text)}); i.dispatchEvent(new Event("input", { bubbles: true })); return true; })()`);
  let code = 0;
  try {
    log(`start ${WIDTH}x${HEIGHT}`);
    await win.loadURL(URL_);
    await until("the start page", `!!document.querySelector(".start-main")`);
    // dates in UTC, as the mock data has them: not the time zone of the machine taking the shots
    let zone = await js(`Intl.DateTimeFormat().resolvedOptions().timeZone`);
    if (zone !== "UTC") {
      wc.debugger.attach("1.3");
      await wc.debugger.sendCommand("Emulation.setTimezoneOverride", { timezoneId: "UTC" });
      wc.reload();
      await sleep(1000);
      await until("the start page (reloaded)", `!!document.querySelector(".start-main")`);
      zone = await js(`Intl.DateTimeFormat().resolvedOptions().timeZone`);
    }
    if (zone !== "UTC") throw new Error(`time zone is ${zone}, not UTC`);
    log("time zone UTC");
    await js(`window.prompt = () => ${JSON.stringify(SNAPSHOT)}; true`);
    await sleep(1500); // the start page's handlers settle (an earlier click is lost)
    const opened = await js(
      `(() => { const b = [...document.querySelectorAll("button")].filter((e) => !e.querySelector("button") && /打开快照文件|Open snapshot file/.test(e.textContent)).pop(); if (b) b.click(); return b ? b.outerHTML.slice(0, 120) : false; })()`,
    );
    if (!opened) throw new Error("no 'open snapshot file' button");
    log(`opening the snapshot: clicked ${opened}`);
    await until("the treemap", `!!document.querySelector("canvas") && !document.querySelector(".start-main")`);
    const bar = await js(`(() => { const b = document.querySelector(".statusbar"); return b ? Math.ceil(b.getBoundingClientRect().top) : innerHeight; })()`);
    crop = { x: 0, y: 0, width: WIDTH, height: bar };
    await js(`(() => { const s = document.createElement("style"); s.textContent = ".statusbar { visibility: hidden !important; }"; document.head.append(s); return true; })()`);
    await sleep(4000); // layout, the first frames, and the toasts fade

    // 1. the treemap, with the hover card over a large file
    const r = await js(`(() => { const c = [...document.querySelectorAll("canvas")].sort((a, b) => b.width * b.height - a.width * a.height)[0].getBoundingClientRect(); return { x: c.x, y: c.y, w: c.width, h: c.height }; })()`);
    wc.sendInputEvent({ type: "mouseMove", x: Math.round(r.x + r.w * 0.3), y: Math.round(r.y + r.h * 0.45) });
    await sleep(800);
    await shot("treemap");
    wc.sendInputEvent({ type: "mouseMove", x: Math.round(r.x + r.w - 2), y: Math.round(r.y - 30) });

    // 2. a filter: videos over 1 GB, the matches listed beside the treemap
    await setFilter(env("FILTER", ":class:视频;>1gb"));
    await key("Enter");
    await sleep(2500);
    await shot("filter");
    await setFilter("");
    await key("Enter");
    await key("Escape");
    await sleep(1500);

    // 3. the command palette searching the scan
    await key("K", ["control"]);
    await sleep(500);
    wc.insertText("mountain");
    await sleep(3000);
    await shot("palette");
    await key("Escape");
    await sleep(600);

    // 4. snapshot comparison: the command takes the newest other snapshot (a month earlier) as base
    await key("K", ["control"]);
    await sleep(500);
    wc.insertText(env("COMPARE_QUERY", "对比"));
    await sleep(1500);
    await key("Enter");
    await sleep(8000);
    await shot("compare");
  } catch (err) {
    log(`error: ${err && err.stack ? err.stack : err}`);
    const state = await js(`[document.querySelectorAll("canvas").length, !!document.querySelector(".start-main"), [...document.querySelectorAll(".toast, .op-toast, [role=alert], [role=status]")].map((e) => e.textContent).join(" / ")].join(" | ")`).catch(() => "");
    log(`page: canvases | start page | toasts: ${String(state).slice(0, 600)}`);
    code = 1;
  }
  app.exit(code);
});
