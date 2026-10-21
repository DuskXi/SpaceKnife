// Off-screen recording of a short demo (README GIF, website and social video: hover, zoom, the
// flame graph and the polar flame graph, a filter, the palette, a comparison): run by
// tools/demo/screenshots.py --video with the repo's Electron as the main script. Nothing is shown on
// screen. The page is driven like screenshots.cjs (mock snapshot, status bar cropped off, UTC) and a
// timer saves the latest off-screen frame at a fixed rate to SHOT_FRAMES as JPEG files; ffmpeg turns
// them into a GIF and an MP4. Off-screen frames have no mouse pointer, so the page gets a drawn one.
//
//   SHOT_URL=… SHOT_SNAPSHOT=… SHOT_FRAMES=<dir> SHOT_LOG=<file> electron tools/demo/demo.cjs

const { app, BrowserWindow, dialog } = require("electron");
const fs = require("node:fs");
const path = require("node:path");

const env = (name, fallback) => process.env[`SHOT_${name}`] || fallback;
if (!process.env.SHOT_URL || !process.env.SHOT_SNAPSHOT || !process.env.SHOT_FRAMES) app.exit(2); // run it through screenshots.py
const FRAMES = path.resolve(env("FRAMES", "."));
const LOG = env("LOG", path.join(FRAMES, "demo.log"));
const WIDTH = 1440;
const HEIGHT = 900;
const FPS = 20;
const OUT_W = 1280;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (msg) => fs.appendFileSync(LOG, `${new Date().toISOString()} ${msg}\n`);

fs.mkdirSync(FRAMES, { recursive: true });
dialog.showErrorBox = (title, content) => log(`${title}: ${content}`);
process.on("uncaughtException", (err) => {
  log(`uncaught: ${err && err.stack ? err.stack : err}`);
  app.exit(1);
});

// a pointer and a click ripple drawn into the page (off-screen frames have no cursor)
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

app.whenReady().then(async () => {
  const win = new BrowserWindow({ width: WIDTH, height: HEIGHT, show: false, webPreferences: { offscreen: true, backgroundThrottling: false } });
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
  let mouse = { x: 720, y: 450 };
  const move = (x, y) => {
    mouse = { x: Math.round(x), y: Math.round(y) };
    wc.sendInputEvent({ type: "mouseMove", ...mouse });
  };
  const glide = async (x, y, ms = 700) => {
    const from = { ...mouse };
    const steps = Math.max(1, Math.round(ms / 25));
    for (let i = 1; i <= steps; i++) {
      const t = i / steps;
      const e = t < 0.5 ? 2 * t * t : 1 - (-2 * t + 2) ** 2 / 2;
      move(from.x + (x - from.x) * e, from.y + (y - from.y) * e);
      await sleep(ms / steps);
    }
  };
  const click = async (count = 1) => {
    for (let n = 1; n <= count; n++) {
      wc.sendInputEvent({ type: "mouseDown", ...mouse, button: "left", clickCount: n });
      wc.sendInputEvent({ type: "mouseUp", ...mouse, button: "left", clickCount: n });
      await sleep(70);
    }
  };
  const key = async (keyCode, modifiers = []) => {
    wc.sendInputEvent({ type: "keyDown", keyCode, modifiers });
    wc.sendInputEvent({ type: "keyUp", keyCode, modifiers });
    await sleep(120);
  };
  const type = async (text, ms = 70) => {
    for (const ch of text) {
      wc.insertText(ch);
      await sleep(ms);
    }
  };

  // frames: the newest off-screen paint, saved FPS times a second
  let latest = null;
  let crop = null;
  let frame = 0;
  let timer = null;
  wc.on("paint", (_e, _dirty, image) => (latest = image));
  const record = () => {
    timer = setInterval(() => {
      if (!latest) return;
      const img = (crop ? latest.crop(crop) : latest).resize({ width: OUT_W, quality: "best" });
      fs.writeFileSync(path.join(FRAMES, `f${String(frame++).padStart(5, "0")}.jpg`), img.toJPEG(92));
    }, 1000 / FPS);
  };

  let code = 0;
  try {
    log("start");
    await win.loadURL(env("URL"));
    await until("the start page", `!!document.querySelector(".start-main")`);
    const zone = await js(`Intl.DateTimeFormat().resolvedOptions().timeZone`);
    if (zone !== "UTC") throw new Error(`time zone is ${zone}, not UTC`);
    await js(`window.prompt = () => ${JSON.stringify(env("SNAPSHOT"))}; true`);
    await sleep(1500);
    const opened = await js(
      `(() => { const b = [...document.querySelectorAll("button")].filter((e) => !e.querySelector("button") && /打开快照文件|Open snapshot file/.test(e.textContent)).pop(); if (b) b.click(); return !!b; })()`,
    );
    if (!opened) throw new Error("no 'open snapshot file' button");
    await until("the treemap", `!!document.querySelector("canvas") && !document.querySelector(".start-main")`);
    const bar = await js(`(() => { const b = document.querySelector(".statusbar"); return b ? Math.ceil(b.getBoundingClientRect().top) : innerHeight; })()`);
    crop = { x: 0, y: 0, width: WIDTH, height: bar - (bar % 2) };
    await js(POINTER);
    move(1000, 300);
    await sleep(4000); // layout, first frames, toasts gone: recording starts on a settled screen
    const canvas = () => js(`(() => { const c = [...document.querySelectorAll("canvas")].sort((a, b) => b.width * b.height - a.width * a.height)[0].getBoundingClientRect(); return { x: c.x, y: c.y, w: c.width, h: c.height }; })()`);
    const r = await canvas();
    const at = (fx, fy) => [r.x + r.w * fx, r.y + r.h * fy];
    // the toolbar's visualization switch: treemap, flame graph, polar flame graph
    const VIS = `[...document.querySelectorAll(".segmented.icon-only [role=radio]")]`;
    const visButtons = () => js(`${VIS}.map((b) => { const r = b.getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })`);
    const pickVisual = (i) => js(`(() => { const b = ${VIS}[${i}]; if (b) b.click(); return !!b; })()`);
    // what the hover card names under (x, y)
    const titleAt = async (x, y) => {
      move(x, y);
      await sleep(420);
      return js(`document.querySelector(".hovercard .hc-title")?.textContent ?? ""`);
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
    // root (the same mock data and window give the same layout during the recording)
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
    move(1000, 300);
    await sleep(3000);

    record();
    log("recording");
    // 1. hover across the drive
    await sleep(600);
    await glide(...at(0.62, 0.3), 900);
    await sleep(600);
    await glide(...at(0.3, 0.45), 900);
    await sleep(700);
    // 2. zoom into Videos (double-click its title) and back out
    await glide(r.x + 40, r.y + 8, 600);
    await sleep(300);
    await click(2);
    await sleep(1700);
    await glide(...at(0.45, 0.5), 800);
    await sleep(1000);
    await key("Backspace");
    await sleep(1400);
    // 3. the flame graph from the toolbar: hover, zoom into a folder (the bars stretch) and back
    const vis = await visButtons();
    await glide(vis[1].x, vis[1].y, 900);
    await sleep(250);
    await click();
    await sleep(1400);
    await glide(flame.x + fr.w * 0.35, flame.y - 40, 900);
    await sleep(800);
    await glide(flame.x, flame.y, 700);
    await sleep(400);
    await click(2);
    await sleep(1800);
    await glide(fr.x + fr.w * 0.55, flame.y - 60, 800);
    await sleep(900);
    await key("Backspace");
    await sleep(1400);
    // 4. the polar flame graph: the same tree in rings; zoom into a sector and back
    await glide(vis[2].x, vis[2].y, 900);
    await sleep(250);
    await click();
    await sleep(1500);
    await glide(polar.x + (polar.x - (pr.x + pr.w / 2)) * 0.6, polar.y + (polar.y - (pr.y + pr.h / 2)) * 0.6, 900);
    await sleep(900);
    await glide(polar.x, polar.y, 700);
    await sleep(400);
    await click(2);
    await sleep(2000);
    await glide(pr.x + pr.w * 0.38, pr.y + pr.h * 0.6, 800);
    await sleep(900);
    await key("Backspace");
    await sleep(1400);
    // back to the treemap
    await glide(vis[0].x, vis[0].y, 800);
    await sleep(250);
    await click();
    await sleep(1400);
    // 3. a filter, typed
    const f = await js(`(() => { const i = document.querySelector(".filter input").getBoundingClientRect(); return { x: i.x + 40, y: i.y + i.height / 2 }; })()`);
    await glide(f.x, f.y, 700);
    await click();
    await sleep(300);
    await type(env("FILTER", ":class:视频;>1gb"), 75);
    await key("Enter");
    await glide(...at(0.75, 0.62), 900);
    await sleep(2200);
    await key("A", ["control"]);
    await key("Backspace");
    await key("Enter");
    await key("Escape");
    await sleep(900);
    // 4. the command palette
    await key("K", ["control"]);
    await sleep(400);
    await type("mountain", 90);
    await sleep(1800);
    await key("Escape");
    await sleep(500);
    // 5. compare with the snapshot from five weeks earlier
    await key("K", ["control"]);
    await sleep(400);
    await type(env("COMPARE_QUERY", "对比"), 90);
    await sleep(700);
    await key("Enter");
    await sleep(2600);
    await glide(...at(0.2, 0.3), 900);
    await sleep(2000);
    clearInterval(timer);
    log(`frames ${frame} (${(frame / FPS).toFixed(1)} s)`);
  } catch (err) {
    log(`error: ${err && err.stack ? err.stack : err}`);
    code = 1;
  }
  if (timer) clearInterval(timer);
  app.exit(code);
});
