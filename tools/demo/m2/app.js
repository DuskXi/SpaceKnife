// SpaceKnife M2 体验台：直接用协议 v1 驱动后端的视图与过滤，并在 canvas 上画出绘制列表 drawlist.v1。
// 这不是 M3 的正式界面，只用来体验 M2 的布局、导航、过滤和标签。

const $ = (id) => document.getElementById(id);
const params = new URLSearchParams(location.hash.slice(1));
const WS_URL = params.get("ws");
const TOKEN = params.get("token");

// ------------------------------------------------------------------ protocol

class Rpc {
  constructor() {
    this.id = 1;
    this.pending = new Map();
    this.events = new Map();
    this.onBinary = () => {};
    this.onClose = () => {};
  }
  connect(url, token) {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url, "spaceknife.v1");
      ws.binaryType = "arraybuffer";
      this.ws = ws;
      ws.onopen = async () => {
        try {
          const hello = await this.call("session.hello", {
            token, protocol: "1.0",
            client: { name: "m2-playground", version: "0.2.0", kind: "electron" },
            capabilities: ["binary-frames.v1", "subscriptions", "cancel", "log-stream"],
          });
          resolve(hello);
        } catch (e) { reject(e); }
      };
      ws.onerror = () => reject(new Error("无法连接后端"));
      ws.onclose = (ev) => this.onClose(ev);
      ws.onmessage = (ev) => {
        if (typeof ev.data !== "string") { this.onBinary(decodeFrame(ev.data)); return; }
        const msg = JSON.parse(ev.data);
        if ("id" in msg && (msg.result !== undefined || msg.error !== undefined)) {
          const p = this.pending.get(msg.id);
          if (!p) return;
          this.pending.delete(msg.id);
          msg.error ? p.reject(Object.assign(new Error(msg.error.message), { code: msg.error.code })) : p.resolve(msg.result);
        } else if (msg.method) {
          (this.events.get(msg.method) || []).forEach((fn) => fn(msg.params));
        }
      };
    });
  }
  call(method, params = {}) {
    const id = this.id++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
    });
  }
  on(method, fn) {
    if (!this.events.has(method)) this.events.set(method, []);
    this.events.get(method).push(fn);
  }
}

const DTYPES = { "<u1": Uint8Array, "<i1": Int8Array, "<u2": Uint16Array, "<i2": Int16Array, "<u4": Uint32Array,
  "<i4": Int32Array, "<u8": BigUint64Array, "<i8": BigInt64Array, "<f4": Float32Array, "<f8": Float64Array };

function decodeFrame(buf) {
  const view = new DataView(buf);
  const hlen = view.getUint32(0, true);
  const header = JSON.parse(new TextDecoder().decode(new Uint8Array(buf, 4, hlen)));
  let start = 4 + hlen;
  start += (8 - (start % 8)) % 8;
  const cols = {};
  for (const [name, spec] of Object.entries(header.columns)) {
    cols[name] = new DTYPES[spec.dtype](buf, start + spec.offset, header.count);
  }
  const blobs = {};
  for (const [name, spec] of Object.entries(header.blobs || {})) {
    blobs[name] = new Uint8Array(buf, start + spec.offset, spec.length);
  }
  return { header, cols, blobs };
}

function drawNames(frame) {
  const dec = new TextDecoder();
  const blob = frame.blobs.names;
  const lens = frame.cols.name_len;
  const out = new Array(lens.length);
  let off = 0;
  for (let i = 0; i < lens.length; i++) {
    out[i] = lens[i] ? dec.decode(blob.subarray(off, off + lens[i])) : "";
    off += lens[i];
  }
  return out;
}

// ------------------------------------------------------------------ tokens and formatting

const css = getComputedStyle(document.documentElement);
const tok = (name) => css.getPropertyValue(`--sk-${name}`).trim();
const LEVELS = [0, 1, 2, 3].map((i) => ({
  bg: tok(`treemap-level-${i}-bg`), border: tok(`treemap-level-${i}-border`), headerBg: tok(`treemap-level-${i}-header-bg`),
  headerText: tok(`treemap-level-${i}-header-text`), headerSub: tok(`treemap-level-${i}-header-sub`),
}));
const T = {
  ground: tok("treemap-ground"), collapsed: tok("treemap-collapsed"), haloBg: tok("treemap-halo-bg"), haloText: tok("treemap-halo-text"),
  haloSub: tok("treemap-halo-sub"), light: tok("treemap-label-light"), lightSub: tok("treemap-label-light-sub"), dark: tok("treemap-label-dark"),
  darkSub: tok("treemap-label-dark-sub"), ring: tok("treemap-state-select-ring"), ringOuter: tok("treemap-state-select-outer"),
  hover: tok("treemap-state-hover-inset"), free: ["stripe-a", "stripe-b", "border", "text", "sub"].map((k) => tok(`treemap-free-${k}`)),
  unknown: ["stripe-a", "stripe-b", "border", "text", "sub"].map((k) => tok(`treemap-unknown-${k}`)),
  pending: ["stripe-a", "stripe-b", "border", "text", "sub"].map((k) => tok(`treemap-pending-${k}`)),
  tags: [null, tok("color-tag-red"), tok("color-tag-yellow"), tok("color-tag-green"), tok("color-tag-blue")],
  tagBase: tok("color-tag-base"),
};
// 时间配色的色板还没定稿（UI-SPEC T 节待定 12），这里用单一色相的五档临时色
const TONES = ["#5EC8B8", "#3F9C90", "#2E766D", "#21534D", "#183A36"];
const UI_FONT = tok("font-family-ui");
const MONO_FONT = tok("font-family-mono");

function fmtSize(n) {
  n = Number(n);
  const u = ["B", "KB", "MB", "GB", "TB", "PB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return i === 0 ? `${n} B` : `${n.toFixed(n >= 100 ? 0 : 1)} ${u[i]}`;
}
const fmtInt = (n) => Number(n).toLocaleString("zh-CN");

function lum(hex) {
  const v = hex.replace("#", "").slice(0, 6);
  const c = [0, 2, 4].map((i) => parseInt(v.slice(i, i + 2), 16) / 255).map((x) => (x <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4));
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
}
const contrast = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
const textOn = (bg) => (contrast(bg, T.light) >= contrast(bg, T.dark) ? [T.light, T.lightSub] : [T.dark, T.darkSub]);

// ------------------------------------------------------------------ state

const rpc = new Rpc();
const S = {
  scanId: null, viewId: null, state: null, info: null, frame: null, names: [], seq: 0,
  vocab: null, classIds: [], hover: -1, binaries: new Map(), waiters: new Map(), colorMode: "class",
  parse: null, completionIndex: 0,
};

rpc.onBinary = (fr) => {
  if (fr.header.layout !== "drawlist.v1") return;
  const key = `${fr.header.stream}#${fr.header.seq}`;
  const w = S.waiters.get(key);
  if (w) { S.waiters.delete(key); w(fr); } else { S.binaries.set(key, fr); if (S.binaries.size > 16) S.binaries.delete(S.binaries.keys().next().value); }
};

function frameFor(info) {
  const key = `view:${info.view_id}#${info.seq}`;
  const got = S.binaries.get(key);
  if (got) { S.binaries.delete(key); return Promise.resolve(got); }
  return new Promise((resolve) => S.waiters.set(key, resolve));
}

async function show(info) {
  if (!info || info.view_id !== S.viewId || info.seq <= S.seq) return;
  const fr = await frameFor(info);
  if (info.seq <= S.seq) return;
  S.seq = info.seq; S.info = info; S.frame = fr; S.names = drawNames(fr);
  draw();
  renderChrome();
}

async function viewCall(method, extra = {}) {
  if (!S.viewId) return null;
  const upd = await rpc.call(method, { view_id: S.viewId, ...extra });
  S.state = upd.state;
  if (upd.frame) await show(upd.frame);
  else renderChrome(upd.filter);
  if (upd.filter !== undefined) renderFilterInfo(upd.filter);
  return upd;
}

// ------------------------------------------------------------------ drawing

const canvas = $("canvas");
const ctx = canvas.getContext("2d");
const KIND = { FILE: 0, FOLDER: 1, COLLAPSED: 2, PENDING: 3, SMALL: 4, MORE: 5, FREE: 6, UNKNOWN: 7 };
const F = { MATCH: 1, DIM: 2, HIT: 4, TAGGED: 8, SELECTED: 16, PATH: 32 };
const patterns = {};

function stripes(a, b, width, angle, dpr) {
  const key = `${a}${b}${width}${angle}${dpr}`;
  if (patterns[key]) return patterns[key];
  const w = Math.max(2, Math.round(width * dpr));
  const size = w * 2;
  const tile = new OffscreenCanvas(size, size);
  const t = tile.getContext("2d");
  t.fillStyle = a; t.fillRect(0, 0, size, size);
  t.fillStyle = b;
  t.beginPath();
  if (angle === 135) { t.moveTo(0, 0); t.lineTo(w, 0); t.lineTo(size, size - w); t.lineTo(size, size); t.lineTo(size - w, size); t.lineTo(0, w); }
  else { t.moveTo(size, 0); t.lineTo(size - w, 0); t.lineTo(0, size - w); t.lineTo(0, size); t.lineTo(w, size); t.lineTo(size, w); }
  t.closePath(); t.fill();
  return (patterns[key] = ctx.createPattern(tile, "repeat"));
}

function dots(color, dpr) {
  const key = `dots${color}${dpr}`;
  if (patterns[key]) return patterns[key];
  const s = Math.round(6 * dpr);
  const tile = new OffscreenCanvas(s, s);
  const t = tile.getContext("2d");
  t.fillStyle = color; t.fillRect(0, 0, Math.max(1, Math.round(dpr)), Math.max(1, Math.round(dpr)));
  return (patterns[key] = ctx.createPattern(tile, "repeat"));
}

function rr(x, y, w, h, r) { ctx.beginPath(); ctx.roundRect(x, y, w, h, Math.min(r, w / 2, h / 2)); }

function fit(text, max) {
  if (ctx.measureText(text).width <= max) return text;
  let lo = 0, hi = text.length;
  while (lo < hi) { const mid = (lo + hi + 1) >> 1; ctx.measureText(text.slice(0, mid) + "…").width <= max ? (lo = mid) : (hi = mid - 1); }
  return lo ? text.slice(0, lo) + "…" : "";
}

function fill(kind, cls, tone) {
  if (kind === KIND.COLLAPSED || kind === KIND.MORE) return T.collapsed;
  if (S.colorMode === "flat") return tok("treemap-class-other");
  if (S.colorMode === "time" && (kind === KIND.FILE || kind === KIND.SMALL)) return TONES[tone] || TONES[4];
  return tok(`treemap-class-${S.classIds[cls] || "other"}`);
}

function draw() {
  const fr = S.frame, info = S.info;
  if (!fr || !info) return;
  $("empty").hidden = fr.header.count > 0;
  const c = fr.cols, dpr = info.dpr;
  if (canvas.width !== info.width || canvas.height !== info.height) { canvas.width = info.width; canvas.height = info.height; }
  ctx.fillStyle = T.ground; ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.textBaseline = "alphabetic";
  const n = fr.header.count;
  const b = Math.max(1, Math.round(dpr)), header = Math.round(18 * dpr);
  let selected = -1;
  for (let i = 0; i < n; i++) {
    const kind = c.kind[i], x = c.x[i], y = c.y[i], w = c.w[i], h = c.h[i], flags = c.flags[i];
    if (flags & F.SELECTED) selected = i;
    const dim = flags & F.DIM;
    ctx.globalAlpha = dim ? (kind === KIND.FOLDER ? 0.45 : kind >= KIND.FREE ? 0.55 : 0.12) : 1;
    const name = S.names[i];
    const lbl = c.label[i];
    if (kind === KIND.FOLDER) {
      const L = LEVELS[Math.min(c.depth[i], 4) - 1];
      rr(x, y, w, h, 5 * dpr); ctx.fillStyle = L.bg; ctx.fill();
      ctx.lineWidth = b; ctx.strokeStyle = L.border; ctx.stroke();
      const halo = flags & F.PATH;
      ctx.fillStyle = halo ? T.haloBg : L.headerBg;
      rr(x + b, y + b, w - 2 * b, header, 4 * dpr); ctx.fill();
      const size = fmtSize(c.size[i]);
      ctx.font = `${flags & F.HIT ? 700 : 600} ${11 * dpr}px ${UI_FONT}`;
      const maxW = w - 11 * dpr - (c.tag[i] ? 22 * dpr : 0);
      ctx.save(); ctx.font = `${10.5 * dpr}px ${MONO_FONT}`; const sw = ctx.measureText(size).width; ctx.restore();
      const label = fit(name, Math.max(0, maxW - sw - 6 * dpr));
      ctx.fillStyle = halo ? T.haloText : L.headerText;
      ctx.fillText(label, x + 6 * dpr, y + b + 13 * dpr);
      const nw = ctx.measureText(label).width;
      ctx.font = `${10.5 * dpr}px ${MONO_FONT}`;
      ctx.fillStyle = halo ? T.haloSub : L.headerSub;
      if (nw + sw + 6 * dpr <= maxW) ctx.fillText(size, x + 6 * dpr + nw + 6 * dpr, y + b + 13 * dpr);
    } else {
      let bg, text, sub, line1 = name, line2 = fmtSize(c.size[i]), weight = 500, radius = 3;
      if (kind === KIND.FREE || kind === KIND.UNKNOWN || kind === KIND.PENDING) {
        const P = kind === KIND.FREE ? T.free : kind === KIND.UNKNOWN ? T.unknown : T.pending;
        rr(x, y, w, h, 5 * dpr);
        ctx.fillStyle = stripes(P[0], P[1], kind === KIND.UNKNOWN ? 3 : kind === KIND.PENDING ? 8 : 7, kind === KIND.UNKNOWN ? 45 : 135, dpr); ctx.fill();
        ctx.setLineDash(kind === KIND.FREE ? [] : [4 * dpr, 3 * dpr]); ctx.lineWidth = b; ctx.strokeStyle = P[2]; ctx.stroke(); ctx.setLineDash([]);
        [text, sub] = [P[3], P[4]];
        if (kind === KIND.FREE) line1 = "可用空间";
        if (kind === KIND.UNKNOWN) line1 = info.reason.startsWith("scan") && info.reason !== "scan-final" ? "未知空间 · 尚未扫描" : "未知空间";
        if (kind === KIND.PENDING) line2 = "等待扫描…";
        weight = kind === KIND.PENDING ? 600 : 500;
      } else {
        bg = fill(kind, c.cls[i], c.tone[i]);
        radius = kind === KIND.FILE ? 3 : 5;
        rr(x, y, w, h, radius * dpr); ctx.fillStyle = bg; ctx.fill();
        [text, sub] = textOn(bg);
        if (kind === KIND.SMALL || kind === KIND.MORE) {
          ctx.fillStyle = dots(lum(bg) > 0.18 ? "#00000059" : "#FFFFFF40", dpr); ctx.fill();
          line1 = kind === KIND.SMALL ? `${fmtInt(c.count[i])} 个小项` : `+${fmtInt(c.count[i])} 项`;
          weight = 400;
        }
        if (kind === KIND.COLLAPSED) weight = flags & F.HIT ? 700 : 600;
      }
      if (lbl & 1 || kind >= KIND.SMALL) {
        const cw = w / dpr, ch = h / dpr;
        if (cw >= 30 && ch >= 15) {
          const big = lbl & 4, short = lbl & 8 || ch < 22;
          const fs = short ? 10.5 : big ? 13 : 11;
          const pad = (big ? 6 : 4) * dpr;
          const right = (c.tag[i] ? 22 : 5) * dpr;
          ctx.font = `${weight} ${fs * dpr}px ${UI_FONT}`;
          ctx.fillStyle = text;
          const y1 = short ? y + h / 2 + fs * dpr * 0.36 : y + pad + fs * dpr * 0.92;
          ctx.fillText(fit(line1, w - 6 * dpr - right), x + 6 * dpr, y1);
          if (!short && (lbl & 2 || (kind >= KIND.SMALL && cw >= 42 && ch >= 31))) {
            ctx.font = `${(big ? 11.5 : 10.5) * dpr}px ${MONO_FONT}`;
            ctx.fillStyle = sub;
            ctx.fillText(fit(line2, w - 11 * dpr), x + 6 * dpr, y1 + (big ? 16 : 14) * dpr);
          }
        }
      }
    }
    if (c.tag[i] && w >= 22 * dpr && h >= 18 * dpr) drawTag(c.tag[i], x + w - 18 * dpr, y + (kind === KIND.FOLDER ? 2 : 4) * dpr, dpr);
  }
  ctx.globalAlpha = 1;
  if (S.hover >= 0 && S.hover < n) {
    const i = S.hover;
    ctx.lineWidth = b; ctx.strokeStyle = T.hover;
    rr(c.x[i] + b / 2, c.y[i] + b / 2, c.w[i] - b, c.h[i] - b, 3 * dpr); ctx.stroke();
  }
  if (selected >= 0) {
    const i = selected, r = Math.round(2 * dpr);
    ctx.save(); ctx.shadowColor = "#000000A6"; ctx.shadowBlur = 32 * dpr; ctx.shadowOffsetY = 12 * dpr;
    ctx.lineWidth = r; ctx.strokeStyle = T.ringOuter; rr(c.x[i] - 1.5 * r, c.y[i] - 1.5 * r, c.w[i] + 3 * r, c.h[i] + 3 * r, 6 * dpr); ctx.stroke(); ctx.restore();
    ctx.lineWidth = r; ctx.strokeStyle = T.ring; rr(c.x[i] - r / 2, c.y[i] - r / 2, c.w[i] + r, c.h[i] + r, 5 * dpr); ctx.stroke();
  }
}

function drawTag(t, x, y, dpr) {
  ctx.globalAlpha = 1;
  ctx.fillStyle = T.tagBase; rr(x, y, 14 * dpr, 14 * dpr, 4 * dpr); ctx.fill();
  ctx.fillStyle = T.tags[t];
  const cx = x + 7 * dpr, cy = y + 7 * dpr, s = dpr;
  ctx.beginPath();
  if (t === 1) { ctx.moveTo(cx, cy - 4.5 * s); ctx.lineTo(cx + 4.5 * s, cy + 3.5 * s); ctx.lineTo(cx - 4.5 * s, cy + 3.5 * s); }
  else if (t === 2) { ctx.roundRect(cx - 4 * s, cy - 4 * s, 8 * s, 8 * s, s); }
  else if (t === 3) { ctx.arc(cx, cy, 4 * s, 0, Math.PI * 2); }
  else { ctx.moveTo(cx, cy - 5 * s); ctx.lineTo(cx + 5 * s, cy); ctx.lineTo(cx, cy + 5 * s); ctx.lineTo(cx - 5 * s, cy); }
  ctx.closePath(); ctx.fill();
}

// ------------------------------------------------------------------ hit testing, tooltip, selection details

function hit(ev) {
  const fr = S.frame, info = S.info;
  if (!fr || !info) return -1;
  const rect = canvas.getBoundingClientRect();
  const px = (ev.clientX - rect.left) * (info.width / rect.width);
  const py = (ev.clientY - rect.top) * (info.height / rect.height);
  const c = fr.cols;
  for (let i = fr.header.count - 1; i >= 0; i--) {
    if (px >= c.x[i] && py >= c.y[i] && px < c.x[i] + c.w[i] && py < c.y[i] + c.h[i]) return i;
  }
  return -1;
}

let tipTimer = 0;
canvas.addEventListener("mousemove", (ev) => {
  const i = hit(ev);
  if (i !== S.hover) { S.hover = i; draw(); $("tip").hidden = true; clearTimeout(tipTimer); if (i >= 0) tipTimer = setTimeout(() => tooltip(i, ev), 450); }
});
canvas.addEventListener("mouseleave", () => { S.hover = -1; draw(); $("tip").hidden = true; clearTimeout(tipTimer); });
canvas.addEventListener("click", (ev) => { const i = hit(ev); if (i >= 0) viewCall("view.select", { key: S.frame.cols.key[i] }).then(details); });
canvas.addEventListener("dblclick", (ev) => { const i = hit(ev); if (i >= 0) viewCall("view.zoom", { key: S.frame.cols.key[i] }); });

async function nodeInfo(i) {
  const c = S.frame.cols, kind = c.kind[i];
  if (kind >= KIND.SMALL) return null;
  return rpc.call("tree.node", { scan_id: S.scanId, node: c.node[i] });
}

async function tooltip(i, ev) {
  if (i !== S.hover || !S.frame) return;
  const c = S.frame.cols, kind = c.kind[i];
  let html;
  if (kind === KIND.SMALL || kind === KIND.MORE) html = `<b>${kind === KIND.SMALL ? `${fmtInt(c.count[i])} 个小项` : `+${fmtInt(c.count[i])} 项`}</b><span class="mono">${fmtSize(c.size[i])} · 双击铺开</span>`;
  else if (kind === KIND.FREE) html = `<b>可用空间</b><span class="mono">${fmtSize(c.size[i])}</span>`;
  else if (kind === KIND.UNKNOWN) html = `<b>未知空间</b><span class="mono">${fmtSize(c.size[i])} · 系统报告的已用减去已扫描合计</span>`;
  else {
    const r = await nodeInfo(i);
    if (i !== S.hover) return;
    const t = r.node.mtime ? new Date(r.node.mtime * 1000).toLocaleString("zh-CN") : "未知";
    html = `<b>${esc(r.node.name)}</b><div class="mono">${esc(r.path)}</div><div class="mono">占用 ${fmtSize(r.node.alloc)} · 大小 ${fmtSize(r.node.size)}${r.node.kind === "dir" ? ` · ${fmtInt(r.node.files)} 个文件` : ""}</div><div class="mono">修改于 ${t}</div><div class="muted">双击放大 · 1–4 打标签</div>`;
  }
  const tip = $("tip");
  tip.innerHTML = html; tip.hidden = false;
  const stage = $("stage").getBoundingClientRect();
  tip.style.left = `${Math.min(ev.clientX - stage.left + 14, stage.width - 430)}px`;
  tip.style.top = `${Math.min(ev.clientY - stage.top + 16, stage.height - 120)}px`;
}

async function details() {
  const c = S.frame?.cols;
  const row = S.info?.selected_row;
  const panel = $("detail-panel");
  if (row == null || !c) { panel.textContent = "单击树图里的块"; panel.className = "muted"; return; }
  const kind = c.kind[row];
  if (kind >= KIND.SMALL) { panel.innerHTML = `<div class="big">${fmtSize(c.size[row])}</div><div>${kind === KIND.SMALL ? `${fmtInt(c.count[row])} 个小项` : kind === KIND.MORE ? `+${fmtInt(c.count[row])} 项` : kind === KIND.FREE ? "可用空间" : "未知空间"}</div>`; panel.className = ""; return; }
  const r = await nodeInfo(row);
  panel.className = "";
  panel.innerHTML = `<div class="big">${fmtSize(r.node.alloc)}</div><div><b>${esc(r.node.name)}</b></div><div class="mono muted">${esc(r.path)}</div><div class="mono muted">${r.node.kind === "dir" ? `${fmtInt(r.node.files)} 个文件 · ${fmtInt(r.node.dirs)} 个文件夹` : `逻辑大小 ${fmtSize(r.node.size)}`}</div>`;
}

const esc = (s) => String(s).replace(/[&<>"]/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[ch]);

// ------------------------------------------------------------------ chrome

function renderChrome() {
  const st = S.state, info = S.info;
  if (!st) return;
  $("back").disabled = !st.can_back; $("forward").disabled = !st.can_forward; $("up").disabled = !st.can_up;
  $("home").disabled = !st.can_up && !st.can_back;
  $("detail").textContent = st.detail;
  S.colorMode = st.color_mode;
  document.querySelectorAll("#color-mode button").forEach((b) => b.classList.toggle("on", b.dataset.v === st.color_mode));
  document.querySelectorAll("#filter-mode button").forEach((b) => b.classList.toggle("on", b.dataset.v === st.filter_mode));
  $("free").classList.toggle("on", st.show_free); $("unknown").classList.toggle("on", st.show_unknown);
  if (info) {
    $("crumbs").innerHTML = info.breadcrumb.map((cb, k) => {
      const label = cb.label.startsWith("small:") ? `${fmtInt(cb.label.slice(6))} 个小项` : cb.label.startsWith("more:") ? `+${fmtInt(cb.label.slice(5))} 项` : cb.label || "全部";
      return `${k ? "<i>›</i>" : ""}<a data-k="${k}">${esc(label)}</a>`;
    }).join("");
    $("frame-info").innerHTML = `第 ${info.seq} 帧 · ${fmtInt(info.rows)} 个矩形 · 布局 ${info.layout_ms} ms<br>画布 ${info.width}×${info.height} 物理像素 @${info.dpr}<br>原因：${info.reason} · 已扫描 ${fmtSize(info.scanned)}`;
    renderFilterInfo(info.filter);
  }
}

function renderFilterInfo(f) {
  const floater = $("floater");
  if (!f || !f.active) { floater.hidden = true; return; }
  floater.hidden = false;
  $("match-text").textContent = `匹配 ${fmtInt(f.files)} 项 · ${fmtSize(f.alloc)}${f.stale ? "（表达式有误，显示的是上一次的结果）" : ""}`;
}

// ------------------------------------------------------------------ filter box

const input = $("filter");
let filterTimer = 0;

async function onFilterInput() {
  const text = input.value;
  const cursor = input.selectionStart;
  const res = await rpc.call("filter.parse", { text, cursor });
  if (input.value !== text) return;
  S.parse = res;
  mirror(res);
  completion(res);
  chips(res);
  clearTimeout(filterTimer);
  filterTimer = setTimeout(() => viewCall("view.filter", { text, cursor }), 120);
}

function mirror(res) {
  const text = res.text;
  const cls = new Array(text.length).fill("");
  for (const t of res.tokens) for (let k = t.start; k < t.end; k++) cls[k] = `t-${t.kind}`;
  const err = new Array(text.length).fill(false);
  for (const d of res.diagnostics) if (d.severity === "error") for (let k = d.start; k < Math.max(d.end, d.start + 1) && k < text.length; k++) err[k] = true;
  let html = "", k = 0;
  while (k < text.length) {
    let j = k;
    while (j < text.length && cls[j] === cls[k] && err[j] === err[k]) j++;
    html += `<span class="${cls[k]}${err[k] ? " t-error" : ""}">${esc(text.slice(k, j))}</span>`;
    k = j;
  }
  $("filter-mirror").innerHTML = html;
  $("filter-mirror").scrollLeft = input.scrollLeft;
}

const CTX_NAMES = { keyword: "关键字", class: "文件类别", tag: "标签", attr: "属性", unit: "单位", prefix: "条件开头" };

function completion(res) {
  const box = $("completion");
  const c = res.completion;
  if (!c || !c.candidates.length || (c.candidates.length === 1 && c.candidates[0] === c.typed) || document.activeElement !== input) { box.hidden = true; return; }
  S.completionIndex = Math.min(S.completionIndex, c.candidates.length - 1);
  box.innerHTML = `<div class="head">${CTX_NAMES[c.context] || c.context} · ↑↓ 选择 · Tab 补全</div>` +
    c.candidates.slice(0, 40).map((cand, k) => `<div data-k="${k}" class="${k === S.completionIndex ? "on" : ""}">${esc(cand)}</div>`).join("");
  box.hidden = false;
}

function accept(k) {
  const c = S.parse?.completion;
  if (!c) return;
  const cand = c.candidates[k];
  input.value = input.value.slice(0, c.start) + cand + input.value.slice(c.end);
  const pos = c.start + cand.length;
  input.setSelectionRange(pos, pos);
  S.completionIndex = 0;
  onFilterInput();
}

function chips(res) {
  const nf = res.normal;
  const el = $("chips");
  $("canonical").textContent = nf && nf.canonical ? `规范形式：${nf.canonical}` : "";
  $("diag").innerHTML = res.diagnostics.map((d) => `<div>${esc(d.code)}：「${esc(d.text)}」${d.expected.length ? ` 可用：${esc(d.expected.slice(0, 8).join(" "))}` : ""}</div>`).join("");
  if (!nf || !nf.canonical) { el.innerHTML = res.pending ? `<span class="pill pending">${res.pending.category || "条件"} 选择中…</span>` : "（空）"; return; }
  const pill = (k, vs, j = "或") => `<span class="pill"><i>${k}</i>${vs.map(esc).join(` <i>${j}</i> `)}</span>`;
  const b = (r) => [r.lower && `${r.lower.op} ${r.lower.number} ${r.lower.unit.toUpperCase()}`, r.upper && `${r.upper.op} ${r.upper.number} ${r.upper.unit.toUpperCase()}`].filter(Boolean);
  let html = "";
  if (nf.masks.length) html += pill("名称", nf.masks);
  if (nf.classes.length) html += pill("类别", nf.classes);
  if (nf.folders.length) html += pill("文件夹", nf.folders);
  if (nf.tags) html += pill("标签", nf.tags);
  for (const r of nf.sizes) html += pill(r.field === "disk" ? "占用" : "大小", b(r), "且");
  for (const r of nf.ages) html += pill({ modify: "修改", create: "创建", access: "访问" }[r.field], b(r), "且");
  if (nf.attrs) html += pill("属性", [...nf.attrs.required.map((a) => `有 ${a}`), ...nf.attrs.forbidden.map((a) => `无 ${a}`)], "且");
  const ex = [...nf.not_masks, ...nf.not_classes.map((k) => `:class:${k}`), ...nf.not_folders.map((f) => `\\${f}`), ...nf.not_tags, ...nf.not_attrs.map((a) => `:attr:${a.required.join("+")}`)];
  if (ex.length) html += pill("排除", ex);
  if (res.pending) html += `<span class="pill pending">${res.pending.category || "条件"} 选择中…</span>`;
  el.innerHTML = html;
}

input.addEventListener("input", () => { S.completionIndex = 0; onFilterInput(); });
input.addEventListener("scroll", () => ($("filter-mirror").scrollLeft = input.scrollLeft));
input.addEventListener("keyup", (ev) => { if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(ev.key)) onFilterInput(); });
input.addEventListener("blur", () => setTimeout(() => ($("completion").hidden = true), 150));
input.addEventListener("keydown", (ev) => {
  const box = $("completion"), c = S.parse?.completion;
  if (!box.hidden && c) {
    if (ev.key === "ArrowDown" || ev.key === "ArrowUp") { S.completionIndex = (S.completionIndex + (ev.key === "ArrowDown" ? 1 : -1) + c.candidates.length) % c.candidates.length; completion(S.parse); ev.preventDefault(); return; }
    if (ev.key === "Tab") { accept(S.completionIndex); ev.preventDefault(); return; }
  }
  if (ev.key === "Escape") { if (!box.hidden) box.hidden = true; else if (input.value) { input.value = ""; onFilterInput(); } else input.blur(); ev.preventDefault(); }
  if (ev.key === "Enter") { clearTimeout(filterTimer); viewCall("view.filter", { text: input.value, cursor: input.selectionStart }); }
  ev.stopPropagation();
});
$("completion").addEventListener("mousedown", (ev) => { const k = ev.target.dataset.k; if (k !== undefined) { ev.preventDefault(); accept(Number(k)); } });

// ------------------------------------------------------------------ toolbar and keys

$("back").onclick = () => viewCall("view.back");
$("forward").onclick = () => viewCall("view.forward");
$("up").onclick = () => viewCall("view.up");
$("home").onclick = () => viewCall("view.home");
$("detail-minus").onclick = () => setDetail(-1);
$("detail-plus").onclick = () => setDetail(1);
$("free").onclick = () => viewCall("view.set", { settings: { show_free: !S.state.show_free } });
$("unknown").onclick = () => viewCall("view.set", { settings: { show_unknown: !S.state.show_unknown } });
document.querySelectorAll("#color-mode button").forEach((bt) => (bt.onclick = () => viewCall("view.set", { settings: { color_mode: bt.dataset.v } })));
document.querySelectorAll("#filter-mode button").forEach((bt) => (bt.onclick = () => viewCall("view.set", { settings: { filter_mode: bt.dataset.v } })));
document.querySelectorAll(".tags button").forEach((bt) => (bt.onclick = () => viewCall("view.tag", { color: bt.dataset.tag || null })));
$("crumbs").addEventListener("click", async (ev) => {
  const k = ev.target.dataset.k;
  if (k === undefined || !S.info) return;
  const steps = S.info.breadcrumb.length - 1 - Number(k);
  for (let s = 0; s < steps; s++) await viewCall("view.up");
});
const setDetail = (d) => S.state && viewCall("view.set", { settings: { detail: Math.max(1, Math.min(15, S.state.detail + d)) } });

window.addEventListener("keydown", (ev) => {
  if (!S.viewId || ev.target === $("path")) return;
  const k = ev.key;
  if (k === "/" || (ev.ctrlKey && k.toLowerCase() === "f")) { input.focus(); ev.preventDefault(); return; }
  if (k === "Backspace") { viewCall(ev.shiftKey ? "view.forward" : "view.back"); ev.preventDefault(); }
  else if (ev.altKey && k === "ArrowRight") viewCall("view.forward");
  else if (ev.altKey && k === "ArrowUp") viewCall("view.up");
  else if (ev.ctrlKey && k === "Home") viewCall("view.home");
  else if (ev.ctrlKey && (k === "=" || k === "+")) { setDetail(1); ev.preventDefault(); }
  else if (ev.ctrlKey && k === "-") { setDetail(-1); ev.preventDefault(); }
  else if (k === "Enter" && S.state?.selected != null) viewCall("view.zoom", { key: S.state.selected });
  else if (k === "Escape") viewCall("view.select", { key: null }).then(details);
  else if (!ev.ctrlKey && !ev.altKey && (k === "f" || k === "F")) $("free").click();
  else if (!ev.ctrlKey && !ev.altKey && (k === "u" || k === "U")) $("unknown").click();
  else if (!ev.ctrlKey && !ev.altKey && (k === "c" || k === "C")) {
    const order = ["class", "flat", "time"];
    viewCall("view.set", { settings: { color_mode: order[(order.indexOf(S.state.color_mode) + 1) % 3] } });
  } else if ("01234".includes(k) && k.length === 1) viewCall("view.tag", { color: [null, "red", "yellow", "green", "blue"][Number(k)] });
});

let resizeTimer = 0;
new ResizeObserver(() => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => S.viewId && viewCall("view.set", { settings: { viewport: viewport() } }), 80);
}).observe($("stage"));

function viewport() {
  const r = $("stage").getBoundingClientRect();
  return { width: Math.max(50, r.width), height: Math.max(50, r.height), dpr: window.devicePixelRatio || 1 };
}

// ------------------------------------------------------------------ scans and snapshots

async function openView(scanId) {
  if (S.viewId) { await rpc.call("view.close", { view_id: S.viewId }).catch(() => {}); }
  S.scanId = scanId; S.seq = 0; S.frame = null;
  const upd = await rpc.call("view.open", { scan_id: scanId, viewport: viewport() });
  S.viewId = upd.state.view_id; S.state = upd.state;
  await show(upd.frame);
  if (input.value) onFilterInput();
}

let scanSub = null;
async function startScan() {
  const path = $("path").value.trim();
  if (!path) return;
  try {
    const st = await rpc.call("scan.start", { roots: [path] });
    if (scanSub) await rpc.call("$/unsubscribe", { sub: scanSub }).catch(() => {});
    scanSub = (await rpc.call("$/subscribe", { topic: "scan", params: { scan_id: st.scan_id } })).sub;
    await openView(st.scan_id);
  } catch (e) { $("status").textContent = `无法开始：${e.message}`; }
}

rpc.on("scan.progress", (p) => {
  const s = p.status;
  if (s.scan_id !== S.scanId) return;
  const pct = s.progress != null ? ` ${(s.progress * 100).toFixed(1)}%` : "";
  $("status").textContent = s.state === "scanning"
    ? `扫描中${pct} · ${fmtInt(s.items)} 项 · ${fmtSize(s.bytes_alloc)} · 每秒 ${fmtInt(Math.round(s.rate))} 项`
    : `${s.state === "done" ? "完成" : s.state} · ${fmtInt(s.items)} 项 · ${fmtSize(s.bytes_alloc)} · ${s.elapsed_s.toFixed(1)} 秒`;
  if (s.state !== "scanning") refreshSnapshots();
});
rpc.on("view.frame", (ev) => show(ev.frame));

async function refreshSnapshots() {
  const res = await rpc.call("snapshot.list");
  const sel = $("snapshots");
  sel.innerHTML = `<option value="">打开快照…（${res.snapshots.length}）</option>` + res.snapshots.map((s) =>
    `<option value="${esc(s.path)}">${new Date(s.created_at).toLocaleString("zh-CN")} · ${esc(s.roots.join("; "))} · ${fmtSize(s.bytes_alloc)}</option>`).join("");
}
$("snapshots").onchange = async (ev) => {
  const path = ev.target.value;
  if (!path) return;
  const st = await rpc.call("snapshot.load", { path });
  $("status").textContent = `已打开快照 · ${fmtInt(st.items)} 项 · ${fmtSize(st.bytes_alloc)}`;
  ev.target.value = "";
  await openView(st.scan_id);
};
$("scan").onclick = startScan;
$("path").addEventListener("keydown", (ev) => { if (ev.key === "Enter") startScan(); });

// ------------------------------------------------------------------ boot

(async () => {
  if (!WS_URL || !TOKEN) { $("status").textContent = "缺少后端地址或 token：请用 tools/demo/m2_demo.py 启动"; return; }
  try {
    const hello = await rpc.connect(WS_URL, TOKEN);
    $("status").textContent = `已连接 · ${hello.server.name} ${hello.server.version}`;
    S.vocab = await rpc.call("filter.vocab");
    S.classIds = S.vocab.classes.map((c) => c.id);
    $("path").value = params.get("path") || "";
    await refreshSnapshots();
  } catch (e) { $("status").textContent = `连接失败：${e.message}`; }
  rpc.onClose = () => ($("status").textContent = "与后端的连接已断开");
})();
