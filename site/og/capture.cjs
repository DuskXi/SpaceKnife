// Renders site/og/card.html to site/public/og-en.png and og-zh.png (1200×630), and to
// design/social/github-social-preview.png (the repository's social preview: laid out at 1280×640,
// rendered at 2× so its text stays sharp), off-screen (no window):
//
//   ../apps/electron/node_modules/electron/dist/electron site/og/capture.cjs
//
// Progress and errors go to site/og/capture.log; an error never opens a dialog.

const { app, BrowserWindow, dialog } = require("electron");
const fs = require("node:fs");
const path = require("node:path");
const { pathToFileURL } = require("node:url");

const LOG = path.join(__dirname, "capture.log");
const log = (msg) => fs.appendFileSync(LOG, `${msg}\n`);
dialog.showErrorBox = (title, content) => log(`${title}: ${content}`);
process.on("uncaughtException", (err) => {
  log(`uncaught: ${err && err.stack ? err.stack : err}`);
  app.exit(1);
});

const ROOT = path.join(__dirname, "..", "..");
const JOBS = [
  { lang: "en", w: 1200, h: 630, scale: 1, out: path.join(ROOT, "site", "public", "og-en.png") },
  { lang: "zh", w: 1200, h: 630, scale: 1, out: path.join(ROOT, "site", "public", "og-zh.png") },
  // GitHub: 1280×640 recommended, under 1 MB
  { lang: "en", w: 1280, h: 640, scale: 2, out: path.join(ROOT, "design", "social", "github-social-preview.png") },
];

app.on("window-all-closed", () => {}); // one window per job: closing one must not quit

app.whenReady().then(async () => {
  let code = 0;
  try {
    for (const job of JOBS) {
      const [w, h] = [job.w * job.scale, job.h * job.scale];
      const win = new BrowserWindow({ width: w, height: h, show: false, webPreferences: { offscreen: true, backgroundThrottling: false } });
      win.setContentSize(w, h);
      const url = `${pathToFileURL(path.join(__dirname, "card.html")).href}?lang=${job.lang}&w=${job.w}&h=${job.h}`;
      await win.loadURL(url);
      win.webContents.setZoomFactor(job.scale);
      await win.webContents.executeJavaScript(
        `Promise.all([document.fonts.ready, ...[...document.images].map((i) => i.complete ? null : new Promise((r) => (i.onload = i.onerror = r)))]).then(() => true)`,
        true,
      );
      await new Promise((r) => setTimeout(r, 800));
      const image = await win.webContents.capturePage({ x: 0, y: 0, width: w, height: h });
      fs.writeFileSync(job.out, image.toPNG());
      log(`${path.basename(job.out)} ${image.getSize().width}x${image.getSize().height}`);
      win.destroy();
    }
  } catch (err) {
    log(`error: ${err && err.stack ? err.stack : err}`);
    code = 1;
  }
  app.exit(code);
});
