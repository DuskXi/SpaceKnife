// SpaceKnife web panel bridge (docs/plugin-dev.md 「Web 面板」). A panel imports it from its own
// address: `import sk from "./_sk/bridge.js"`. No build step, no dependencies.
//
// The panel runs in a sandboxed iframe with no network: everything goes through one MessagePort
// the app hands over after the panel says hello. `sk.ready` resolves once it is there, with
// { plugin, lang, theme, context }.
//
// Mock mode: open the panel by itself in a browser, from a plain static server with this SDK in
// web/_sk/ (SpaceKnife's own routes give panels no network, so not through them), with
// `?mock=fixtures.json`; calls are answered from that file (relative to the page):
// { "context": {...}, "lang": "zh-CN", "calls": { "<method>": <result> },
// "core": { "<core method>": <result> }, "states": { "<name>": <value> },
// "events": { "<name>": [<payload>, ...] } }.

const parentWindow = window.parent !== window ? window.parent : null;
const params = new URLSearchParams(location.search);
const mockFile = params.get("mock");

let port = null;
let nextId = 1;
const pending = new Map();
const eventHandlers = new Map();
const stateHandlers = new Map();
const stateValues = new Map();
const subs = new Map();
const contextHandlers = new Set();
const langHandlers = new Set();
const themeHandlers = new Set();
const treeHandlers = new Set();
let gotContext = false;
const info = { plugin: "", lang: document.documentElement.lang || "zh-CN", theme: null, context: {}, units: 1024 };
let mock = null;

let markReady;
const ready = new Promise((resolve) => (markReady = resolve));
let isReady = false;

function applyTheme(theme) {
  info.theme = theme;
  const vars = theme && theme.vars ? theme.vars : {};
  for (const [k, v] of Object.entries(vars)) if (k.startsWith("--sk-")) document.documentElement.style.setProperty(k, String(v));
  const mode = theme && theme.mode === "light" ? "light" : "dark";
  document.documentElement.dataset.skTheme = mode;
  document.documentElement.style.colorScheme = mode;
  for (const cb of themeHandlers) cb(mode);
}

function applyLang(lang) {
  info.lang = lang === "en" ? "en" : "zh-CN";
  document.documentElement.lang = info.lang === "en" ? "en" : "zh-CN";
  for (const cb of langHandlers) cb(info.lang);
}

function onPortMessage(ev) {
  const m = ev.data;
  if (!m || typeof m !== "object") return;
  switch (m.type) {
    case "result":
    case "error": {
      const p = pending.get(m.id);
      if (!p) return;
      pending.delete(m.id);
      if (p.signal && p.onAbort) p.signal.removeEventListener("abort", p.onAbort);
      if (m.type === "result") p.resolve(m.value);
      else p.reject(Object.assign(new Error(m.message || "error"), { code: m.code }));
      return;
    }
    case "event":
      for (const cb of eventHandlers.get(m.name) || []) cb(m.payload);
      return;
    case "state":
      stateValues.set(m.name, m.value);
      for (const cb of stateHandlers.get(m.name) || []) cb(m.value);
      return;
    case "item": {
      const cb = subs.get(m.id);
      if (cb) cb(m.value);
      return;
    }
    case "context":
      info.context = Object.assign({}, m);
      delete info.context.type;
      gotContext = true;
      for (const cb of contextHandlers) cb(info.context);
      maybeReady();
      return;
    case "tree":
      for (const cb of treeHandlers) cb({ scan_id: m.scan_id });
      return;
    case "theme":
      applyTheme({ mode: m.mode, vars: m.vars });
      maybeReady();
      return;
    case "lang":
      if (m.units === 1000 || m.units === 1024) info.units = m.units;
      applyLang(m.lang);
      return;
  }
}

function maybeReady() {
  // ready once the theme and the first context are here (sk.context is never {} after sk.ready)
  if (isReady || !info.theme || !gotContext) return;
  isReady = true;
  markReady(info);
}

function send(msg, transfer) {
  if (!port) throw Object.assign(new Error("not connected to SpaceKnife"), { code: -32006 });
  port.postMessage(msg, transfer || []);
}

function request(msg, signal) {
  if (mock) return Promise.resolve(answerMock(msg));
  return ready.then(
    () =>
      new Promise((resolve, reject) => {
        const id = nextId++;
        const entry = { resolve, reject, signal: null, onAbort: null };
        if (signal) {
          if (signal.aborted) return reject(Object.assign(new Error("cancelled"), { code: -32800 }));
          entry.signal = signal;
          entry.onAbort = () => {
            pending.delete(id);
            try {
              send({ type: "cancel", id });
            } catch (_) {
              /* gone */
            }
            reject(Object.assign(new Error("cancelled"), { code: -32800 }));
          };
          signal.addEventListener("abort", entry.onAbort, { once: true });
        }
        pending.set(id, entry);
        try {
          send(Object.assign({ id }, msg));
        } catch (err) {
          pending.delete(id);
          reject(err);
        }
      }),
  );
}

function sameParams(a, b) {
  return JSON.stringify(a || {}) === JSON.stringify(b || {});
}

function answerMock(msg) {
  if (msg.type === "call") {
    const table = msg.method.startsWith("core:") ? mock.core || {} : mock.calls || {};
    const key = msg.method.startsWith("core:") ? msg.method.slice(5) : msg.method;
    if (!(key in table)) throw Object.assign(new Error(`mock: no answer for ${msg.method}`), { code: -32601 });
    // an answer per params: "calls": { "find": [{ "params": { "q": "a" }, "result": … }, …] }
    const answer = table[key];
    if (Array.isArray(answer) && answer.length && answer.every((x) => x && typeof x === "object" && "result" in x)) {
      const hit = answer.find((x) => sameParams(x.params, msg.params)) || answer.find((x) => !("params" in x));
      if (!hit) throw Object.assign(new Error(`mock: no answer for ${msg.method} with these params`), { code: -32602 });
      return hit.result;
    }
    return answer;
  }
  if (msg.type === "host") {
    console.info("[sk mock] host", msg.op, msg.args);
    return null;
  }
  return null;
}

// --- connecting

window.addEventListener("message", (ev) => {
  const d = ev.data;
  if (ev.source !== parentWindow || !d || d.sk !== 1 || d.type !== "connect" || !ev.ports || !ev.ports[0]) return;
  if (port) port.close();
  port = ev.ports[0];
  info.plugin = String(d.plugin || "");
  port.onmessage = onPortMessage;
  // where this document is: the app refuses a bridge to a page that is not one of the plugin's own
  port.postMessage({ type: "ready", href: location.href });
  // what the panel listens to already (it may have asked before the port came)
  for (const name of eventHandlers.keys()) port.postMessage({ type: "on", name });
  for (const name of stateHandlers.keys()) port.postMessage({ type: "state", name });
});

if (mockFile) {
  fetch(new URL(mockFile, location.href))
    .then((r) => r.json())
    .then((data) => {
      mock = data || {};
      applyLang(mock.lang || info.lang);
      info.context = mock.context || {};
      gotContext = true;
      for (const [name, value] of Object.entries(mock.states || {})) stateValues.set(name, value);
      applyTheme({ mode: mock.theme === "light" ? "light" : "dark", vars: {} }); // "theme": "light" in the fixtures
      maybeReady();
      for (const [name, list] of Object.entries(mock.events || {})) {
        (list || []).forEach((payload, i) => setTimeout(() => (eventHandlers.get(name) || []).forEach((cb) => cb(payload)), 200 * (i + 1)));
      }
    })
    .catch((err) => console.error("[sk mock] cannot read", mockFile, err));
} else if (parentWindow) {
  parentWindow.postMessage({ sk: 1, type: "hello" }, "*");
}

// --- the API

const host = (op) => (args) => request({ type: "host", op, args: args || {} });
const refArgs = (ref) => (typeof ref === "number" ? { node: ref } : ref && typeof ref === "object" ? { node: ref } : {});

const sk = {
  ready,
  get plugin() {
    return info.plugin;
  },
  get lang() {
    return info.lang;
  },
  get context() {
    return info.context;
  },
  /** One of this plugin's methods by its local name. */
  call: (name, params, opts) => request({ type: "call", method: name, params: params || {} }, opts && opts.signal),
  /** A built-in method a panel may use (tree.node, tree.summary, filter.parse…). */
  core: (method, params, opts) => request({ type: "call", method: `core:${method}`, params: params || {} }, opts && opts.signal),
  /** One of this plugin's events; returns the unsubscribe. */
  on(name, cb) {
    let set = eventHandlers.get(name);
    if (!set) {
      eventHandlers.set(name, (set = new Set()));
      if (port) port.postMessage({ type: "on", name });
    }
    set.add(cb);
    return () => {
      set.delete(cb);
      if (!set.size) {
        eventHandlers.delete(name);
        if (port) port.postMessage({ type: "off", name });
      }
    };
  },
  /** One of this plugin's shared states: the value now, then every change. */
  state(name, cb) {
    let set = stateHandlers.get(name);
    if (!set) {
      stateHandlers.set(name, (set = new Set()));
      if (port) port.postMessage({ type: "state", name });
    }
    set.add(cb);
    if (stateValues.has(name)) cb(stateValues.get(name));
    return () => set.delete(cb);
  },
  /** One of this plugin's streams; columns come as typed arrays. Returns the unsubscribe. */
  subscribe(name, params, cb) {
    const id = nextId++;
    subs.set(id, cb);
    ready.then(() => !mock && send({ type: "sub", id, topic: name, params: params || null }));
    return () => {
      subs.delete(id);
      if (port) port.postMessage({ type: "unsub", id });
    };
  },
  /** The scan, view and selection changed; called at once with the current one. */
  onContext(cb) {
    contextHandlers.add(cb);
    if (isReady) cb(info.context);
    return () => contextHandlers.delete(cb);
  },
  onLang(cb) {
    langHandlers.add(cb);
    return () => langHandlers.delete(cb);
  },
  /** The app's theme changed ("dark" | "light"); the --sk-* variables already follow. For what a
   * canvas paints by itself. */
  onTheme(cb) {
    themeHandlers.add(cb);
    return () => themeHandlers.delete(cb);
  },
  /** The tree of the context's scan changed (a rescan, files changed, the scan went on). */
  onTree(cb) {
    treeHandlers.add(cb);
    return () => treeHandlers.delete(cb);
  },
  host: {
    /** Select a node in its scan's view; { stay: true }: without bringing that tab forward. */
    select: (ref, opts) => host("select")(Object.assign(refArgs(ref), opts && opts.stay ? { stay: true } : {})),
    zoom: (ref) => host("zoom")(refArgs(ref)),
    filter: (expr) => host("filter")({ expr }),
    command: (command) => host("command")({ command }),
    toast: (text) => host("toast")({ text }),
    reveal: (ref) => host("reveal")(refArgs(ref)),
    open: (ref) => host("open")(refArgs(ref)),
    terminal: (ref) => host("terminal")(refArgs(ref)),
    url: (url) => host("url")({ url }),
    copy: (text) => host("copy")({ text }),
    basket: (nodes, reason, scanId) => host("basket")({ nodes, reason: reason || "", scan_id: scanId }),
  },
  /** Numbers as the app writes them (the user's size unit: 1000 or 1024). */
  format: {
    size(bytes) {
      const units = ["B", "KB", "MB", "GB", "TB", "PB"];
      const base = info.units;
      let v = Number(bytes);
      let i = 0;
      while (Math.abs(v) >= base && i < units.length - 1) {
        v /= base;
        i++;
      }
      if (i === 0) return `${Math.round(v)} B`;
      if (Math.abs(Number(v.toFixed(1))) >= base && i < units.length - 1) {
        v /= base;
        i++;
      }
      return `${v.toFixed(1)} ${units[i]}`;
    },
    count: (n) => Number(n).toLocaleString("en-US"),
    /** unix seconds as YYYY-MM-DD HH:mm (local time) */
    date(seconds) {
      if (!seconds) return "—";
      const d = new Date(seconds * 1000);
      const p = (n) => String(n).padStart(2, "0");
      return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
    },
  },
  /** The text in the interface language. */
  t: (zh, en, fields) => {
    const text = info.lang === "en" ? en : zh;
    return fields ? text.replace(/\{(\w+)\}/g, (m, k) => (k in fields ? String(fields[k]) : m)) : text;
  },
  /** Ask for this height (px, 40–800) where the panel sits in the details column or a dialog. */
  resize(height) {
    if (port) port.postMessage({ type: "resize", height });
  },
};

export default sk;
