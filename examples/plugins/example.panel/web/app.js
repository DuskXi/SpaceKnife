// The panel (docs/plugin-dev.md 「Web 面板」). sk is the app's bridge: sk.call reaches this
// plugin's own methods, sk.core a few built-in ones; there is no network. Types: sk-plugin.d.ts.
// On its own in a browser: web/index.html?mock=fixtures.json from a static server.
// @ts-check
import sk from "./_sk/bridge.js";

const CANCELLED = -32800; // a newer show() replaced this one (histogram is latest-wins)

async function show() {
  document.getElementById("title").textContent = sk.t("按最后修改的年份", "By year of last change");
  const ctx = sk.context;
  let hist, summary;
  try {
    [hist, summary] = await Promise.all([
      sk.call("histogram", {}),
      // tree.summary: a node of the scan (0: all of it)
      ctx.scan_id ? sk.core("tree.summary", { scan_id: ctx.scan_id, node: 0 }) : Promise.resolve(null),
    ]);
  } catch (err) {
    if (err && err.code === CANCELLED) return;
    throw err;
  }
  if (summary) {
    document.getElementById("summary").textContent = sk.t("共 {files} 个文件", "{files} files in all", {
      files: sk.format.count(summary.node.files), // tree.summary: { node: NodeInfo, path, … }
    });
  }
  const max = Math.max(1, ...hist.years.map((y) => y.bytes));
  const bars = document.getElementById("bars");
  bars.replaceChildren(
    ...hist.years.map((y) => {
      const row = document.createElement("div");
      row.className = "row";
      const label = document.createElement("span");
      label.textContent = String(y.year);
      const bar = document.createElement("div");
      bar.className = "bar";
      bar.style.width = `${(100 * y.bytes) / max}%`;
      bar.title = sk.t("{n} 个文件", "{n} files", { n: y.files });
      const value = document.createElement("span");
      value.className = "size";
      value.textContent = sk.format.size(y.bytes); // the user's unit (1000 or 1024)
      row.append(label, bar, value);
      return row;
    }),
  );
}

sk.ready.then(show);
sk.onContext(show); // another scan, a rescan
sk.onLang(show);
