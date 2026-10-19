// Renders site/og/card.html to site/public/og-en.png and og-zh.png, off-screen (no window):
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

app.whenReady().then(async () => {
  let code = 0;
  try {
    const win = new BrowserWindow({ width: 1200, height: 630, show: false, webPreferences: { offscreen: true, backgroundThrottling: false } });
    for (const lang of ["en", "zh"]) {
      const url = `${pathToFileURL(path.join(__dirname, "card.html")).href}?lang=${lang}`;
      await win.loadURL(url);
      await win.webContents.executeJavaScript(
        `Promise.all([document.fonts.ready, ...[...document.images].map((i) => i.complete ? null : new Promise((r) => (i.onload = i.onerror = r)))]).then(() => true)`,
        true,
      );
      await new Promise((r) => setTimeout(r, 800));
      const image = await win.webContents.capturePage({ x: 0, y: 0, width: 1200, height: 630 });
      const out = path.join(__dirname, "..", "public", `og-${lang}.png`);
      fs.writeFileSync(out, image.toPNG());
      log(`${path.basename(out)} ${image.getSize().width}x${image.getSize().height}`);
    }
  } catch (err) {
    log(`error: ${err && err.stack ? err.stack : err}`);
    code = 1;
  }
  app.exit(code);
});
