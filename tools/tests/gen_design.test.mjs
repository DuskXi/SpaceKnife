// tools/gen_design.mjs: loading, themes, the generated files, and the colour contrast of both themes.
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { describe, test } from "node:test";

import * as g from "../gen_design.mjs";
import { tempFolders } from "./temp.mjs";

const tmp = tempFolders("sk-gen-design-");
const write = (dir, name, data) => writeFileSync(join(dir, name), JSON.stringify(data));
const byPath = (tokens) => Object.fromEntries(tokens.map((t) => [t.dotted, t]));
const values = (tokens) => Object.fromEntries(tokens.map((t) => [t.dotted, t.value]));

test("the repository's artefacts are up to date", () => {
  assert.deepEqual(g.stale(g.artefacts()), [], "run: node tools/gen_design.mjs");
});

test("alias resolution and type inheritance", () => {
  const dir = tmp();
  write(dir, "a.tokens.json", {
    color: { $type: "color", base: { $value: "#112233" }, alias: { $value: "{color.base}" }, chain: { $value: "{color.alias}" } },
    size: { $type: "dimension", x: { $value: { value: 4, unit: "px" } } },
  });
  const tokens = byPath(g.loadTokens(dir));
  assert.equal(tokens["color.chain"].value, "#112233");
  assert.equal(tokens["color.chain"].type, "color");
  assert.equal(tokens["size.x"].value, 4);
});

describe("invalid tokens are rejected", () => {
  for (const [data, message] of [
    [{ c: { $type: "color", a: { $value: "{c.b}" }, b: { $value: "{c.a}" } } }, /cycle/],
    [{ c: { $type: "color", a: { $value: "{c.nope}" } } }, /unknown alias/],
    [{ c: { $type: "color", a: { $value: "red" } } }, /#RRGGBB/],
    [{ c: { a: { $value: 1 } } }, /no \$type/],
    [{ c: { $type: "gradient", a: { $value: 1 } } }, /unsupported/],
    [{ c: { $type: "dimension", a: { $value: { value: 1, unit: "rem" } } } }, /unit/],
  ]) {
    test(String(message), () => {
      const dir = tmp();
      write(dir, "x.tokens.json", data);
      assert.throws(() => g.loadTokens(dir), (e) => e instanceof g.TokenError && message.test(e.message));
    });
  }
});

test("a top-level group defined twice is rejected", () => {
  const dir = tmp();
  write(dir, "a.tokens.json", { c: { $type: "number", a: { $value: 1 } } });
  write(dir, "b.tokens.json", { c: { $type: "number", b: { $value: 2 } } });
  assert.throws(() => g.loadTokens(dir), /defined twice/);
});

test("naming", () => {
  const path = ["color", "text", "onAccent"];
  assert.equal(g.kebab(path), "color-text-on-accent");
  assert.equal(g.camel(path), "colorTextOnAccent");
  assert.equal(g.pascal(["treemap", "level", "0", "headerBg"]), "TreemapLevel0HeaderBg");
});

test("the generated TypeScript holds every theme", () => {
  const ts = readFileSync(join(g.BUILD, "tokens.ts"), "utf8");
  for (const line of ['"window": "#08090B"', '"video": "#E07A1F"', '"toolbar": 48', '"swap": 200', '"holeRatio": 0.24', "  dark: tokens,", "  light: {"]) {
    assert.ok(ts.includes(line), line);
  }
  const light = values(g.loadTokens(undefined, "light"));
  assert.equal(light["color.bg.window"], "#E9EBEF");
  assert.equal(light["layout.toolbar"], 48);
  assert.equal(values(g.loadTokens())["font.family.ui"][0], "Geist");
});

// --------------------------------------------------------------------------- themes

const BASE = {
  c: { $type: "color", a: { $value: "#112233" }, alias: { $value: "{c.a}" }, g: { b: { $value: "#000000" } } },
  n: { $type: "number", x: { $value: 1 } },
};

function writeTheme(override) {
  const dir = tmp();
  write(dir, "base.tokens.json", BASE);
  mkdirSync(join(dir, "themes"));
  write(join(dir, "themes"), "light.tokens.json", override);
  return dir;
}

test("theme overrides, and aliases follow them", () => {
  const dir = writeTheme({ $description: "notes", c: { a: { $value: "#aabbcc", $extensions: {} } } });
  const dark = values(g.loadTokens(dir));
  const light = values(g.loadTokens(dir, "light"));
  assert.deepEqual([dark["c.a"], dark["c.alias"]], ["#112233", "#112233"]);
  assert.deepEqual([light["c.a"], light["c.alias"], light["c.g.b"], light["n.x"]], ["#AABBCC", "#AABBCC", "#000000", 1]);
  const themes = g.loadThemes(dir);
  assert.deepEqual([...g.themed(themes)].sort(), ["c.a", "c.alias"]);
  const css = g.emitCss(themes);
  // light on :root with everything; dark only for what differs, from the system or from data-theme
  const root = css.split("\n}\n")[0];
  for (const line of ["color-scheme: light;", "--sk-n-x: 1;", "--sk-c-a: #AABBCC;"]) assert.ok(root.includes(line), line);
  const darkBody = (i) => `color-scheme: dark;\n${i}--sk-c-a: #112233;\n${i}--sk-c-alias: #112233;\n`;
  assert.ok(css.includes(`@media (prefers-color-scheme: dark) {\n  :root:not([data-theme="light"]) {\n    ${darkBody("    ")}  }\n}`));
  assert.ok(css.includes(`[data-theme="dark"] {\n  ${darkBody("  ")}}`));
  assert.ok(css.includes('[data-theme="light"] {\n  color-scheme: light;\n  --sk-c-a: #AABBCC;\n  --sk-c-alias: #AABBCC;\n}'));
  assert.equal(css.split("--sk-n-x").length - 1, 1);
});

describe("invalid theme overrides are rejected", () => {
  for (const [override, message] of [
    [{ c: { nope: { $value: "#FFFFFF" } } }, /not in the base/],
    [{ x: { $type: "color", a: { $value: "#FFFFFF" } } }, /not in the base/],
    [{ c: { g: { $value: "#FFFFFF" } } }, /token with a token/],
    [{ c: { a: { b: { $value: "#FFFFFF" } } } }, /token with a token/],
    [{ c: { a: { $type: "dimension", $value: { value: 1, unit: "px" } } } }, /cannot change \$type/],
    [{ c: { $type: "number" } }, /cannot change \$type/],
    [{ n: { x: { $value: 2 } } }, /only color and shadow/],
    [{ c: { a: { $value: "blue" } } }, /#RRGGBB/],
  ]) {
    test(`${JSON.stringify(override)} → ${message}`, () => {
      const dir = writeTheme(override);
      assert.throws(() => g.loadTokens(dir, "light"), (e) => e instanceof g.TokenError && message.test(e.message));
    });
  }
});

test("a missing or unknown theme", () => {
  const dir = tmp();
  write(dir, "base.tokens.json", BASE);
  assert.throws(() => g.loadTokens(dir, "light"), /not found/);
  assert.throws(() => g.loadTokens(dir, "sepia"), /unknown theme/);
});

test("the light theme sets every colour itself", () => {
  // a colour added to the dark base without a light value would ship dark in the light theme
  const override = JSON.parse(readFileSync(join(g.TOKENS_DIR, g.THEMES_SUBDIR, "light.tokens.json"), "utf8"));
  const written = new Set(g.tokenPaths(override));
  const wanted = [...g.collect(g.loadTree())].filter(([, [typ, value]]) => g.THEMED_TYPES.has(typ) && !(typeof value === "string" && g.ALIAS.test(value))).map(([n]) => n);
  assert.deepEqual(wanted.filter((n) => !written.has(n)), []);
});

// the light values that are the dark ones on purpose (docs/UI-SPEC.md W7 待定 20): the six mid-tone
// class colours, the labels and dots on blocks, the selection rings, the reduced-motion badge, the
// branch palette's tiers 2 and 4, the tag markers' dark base, and the bright warning fill with its
// near-black text (status.warningFill is the dark warning colour; in light the warning text darkens)
const range = (a, b) => Array.from({ length: b - a }, (_, i) => a + i);
const SAME_AS_DARK = new Set([
  ...["audio", "doc", "exec", "game", "image", "video"].map((c) => `treemap.class.${c}`),
  "treemap.dot.onDark",
  "treemap.dot.onLight",
  ...["dark", "darkSub", "light", "lightSub"].map((k) => `treemap.label.${k}`),
  "treemap.state.newBadgeBg",
  "treemap.state.newBadgeText",
  "treemap.state.selectOuter",
  "treemap.state.selectRing",
  ...range(1, 9).flatMap((slot) => [2, 4].map((tier) => `visual.branch.${slot}.${tier}`)),
  "color.tag.base",
  "color.status.warningFill",
  "color.text.onWarning",
]);

test("the light theme keeps only the chosen dark values", () => {
  const themes = g.loadThemes();
  const varying = g.themed(themes);
  const coloured = themes.dark.filter((t) => g.THEMED_TYPES.has(t.type)).map((t) => t.dotted);
  assert.deepEqual(coloured.filter((n) => !varying.has(n)).sort(), [...SAME_AS_DARK].sort());
});

// --------------------------------------------------------------------------- contrast (both themes)

function rgba(color) {
  const [r, gg, b] = [1, 3, 5].map((i) => parseInt(color.slice(i, i + 2), 16) / 255);
  return [r, gg, b, color.length > 7 ? parseInt(color.slice(7, 9), 16) / 255 : 1];
}

function luminance(color) {
  const ch = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  const [r, gg, b] = rgba(color);
  return 0.2126 * ch(r) + 0.7152 * ch(gg) + 0.0722 * ch(b);
}

function contrast(a, b) {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

// Python's round(): halves to even
function pyRound(x) {
  const r = Math.round(x);
  return Math.abs(x % 1) === 0.5 && r % 2 !== 0 ? r - 1 : r;
}
const hex2 = (v) => pyRound(v).toString(16).toUpperCase().padStart(2, "0");

/** `top` (with its alpha) composited over the opaque `under` in sRGB, as canvas and CSS do. */
function over(top, under) {
  const t = rgba(top);
  const u = rgba(under);
  const a = t[3];
  return `#${[0, 1, 2].map((i) => hex2((t[i] * a + u[i] * (1 - a)) * 255)).join("")}`;
}

/** CSS `color-mix(in srgb, color pct%, transparent)` over `under`. */
const tint = (color, pct, under) => over(`${color.slice(0, 7)}${hex2(pct * 2.55)}`, under);

/** CSS `color-mix(in srgb, color pct%, base)` of an opaque `color` (premultiplied, as #RRGGBBAA). */
function mix(color, pct, base) {
  const p = pct / 100;
  const c = rgba(color);
  const b = rgba(base);
  const ab = b[3];
  const alpha = p + (1 - p) * ab;
  const rgb = [0, 1, 2].map((i) => (c[i] * p + b[i] * ab * (1 - p)) / alpha);
  return `#${[...rgb, alpha].map((v) => hex2(v * 255)).join("")}`;
}

const palette = (theme) => Object.fromEntries(g.loadTokens(undefined, theme).filter((t) => t.type === "color").map((t) => [t.dotted, t.value]));

const ACCENTS = ["default", "lime", "teal", "orange"];
const SURFACES = ["window", "chrome", "panel", "overlay", "raised", "hover", "active"];
const CLASSES = ["video", "image", "audio", "archive", "exec", "doc", "game", "code", "data", "other"];

// [what, ratio, required ratio]: text 4.5:1 (UI-MAIN-55; tertiary 5.6:1 on the surfaces,
// color.tokens.json), marks and outlines 3:1. A pair is measured as drawn: translucent colours over
// what they sit on, CSS color-mix() as in app.css.
const pair = (what, fg, bg, need) => [`${what}: ${fg} on ${bg}`, contrast(fg, bg), need];

function chromePairs(T) {
  const c = Object.fromEntries(Object.entries(T).filter(([k]) => k.startsWith("color.")).map(([k, v]) => [k.slice(6), v]));
  const rows = [];
  for (const text of ["primary", "secondary", "tertiary"]) {
    for (const s of [...SURFACES, "selected"]) {
      const need = text === "tertiary" && SURFACES.slice(0, 5).includes(s) ? 5.6 : 4.5;
      rows.push(pair(`text.${text} on bg.${s}`, c[`text.${text}`], c[`bg.${s}`], need));
    }
  }
  // accent tints under primary text: the current nav item / palette row / compare row, the
  // filter's current candidate, a search hit, the update card's option
  for (const [pct, base] of [[14, "panel"], [14, "overlay"], [30, "panel"], [14, "raised"]]) {
    for (const a of ACCENTS) rows.push(pair(`text.primary on accent.${a} ${pct}% / bg.${base}`, c["text.primary"], tint(c[`accent.${a}`], pct, c[`bg.${base}`]), 4.5));
  }
  for (const a of ACCENTS) {
    const accent = c[`accent.${a}`];
    rows.push(pair(`text.onAccent on accent.${a}`, c["text.onAccent"], accent, 4.5)); // UI-W7-31 ②
    // the ready update badge on hover: the accent 86 % toward white
    rows.push(pair(`text.onAccent on accent.${a} hovered`, c["text.onAccent"], over("#FFFFFF24", accent), 4.5));
    for (const s of SURFACES.slice(0, 6)) rows.push(pair(`accent.${a} as text on bg.${s}`, accent, c[`bg.${s}`], 4.5));
    for (const s of SURFACES) rows.push(pair(`accent.${a} as a mark on bg.${s}`, accent, c[`bg.${s}`], 3.0)); // focus ring, a selected card's edge, a switch
    // on its own tint: the title bar's update badge, 「当前」 on the about page, a remote chip
    for (const [pct, s] of [[16, "window"], [18, "panel"], [14, "window"]]) rows.push(pair(`accent.${a} on its ${pct}% / bg.${s}`, accent, tint(accent, pct, c[`bg.${s}`]), 4.5));
    // the update badge hovered / open and downloading: text color-mix(accent 60 %, text.primary)
    const mixed = over(`${c["text.primary"]}66`, accent);
    for (const pct of [26, 30]) rows.push(pair(`update badge text on accent.${a} ${pct}%`, mixed, tint(accent, pct, c["bg.window"]), 4.5));
  }
  for (const st of ["success", "warning", "danger", "growth"]) {
    for (const s of SURFACES.slice(0, 6)) rows.push(pair(`status.${st} on bg.${s}`, c[`status.${st}`], c[`bg.${s}`], 4.5));
  }
  // the warning's tints are of its fill (bright yellow in both themes) mixed into warningBase (none in
  // the dark theme, an opaque light yellow in the light one), its text is status.warning
  const fill = c["status.warningFill"];
  const base = c["status.warningBase"];
  const warnTint = (pct, s) => over(mix(fill, pct, base), c[`bg.${s}`]);
  for (const [pct, s] of [[14, "window"], [14, "chrome"], [14, "panel"], [16, "overlay"], [22, "panel"], [22, "hover"], [8, "window"], [8, "panel"]]) {
    rows.push(pair(`status.warning on warningFill ${pct}% / bg.${s}`, c["status.warning"], warnTint(pct, s), 4.5));
  }
  // the plain-HTTP label on that chip: the fill's 18 % over it
  for (const s of ["window", "chrome"]) rows.push(pair(`status.warning on the HTTP label / bg.${s}`, c["status.warning"], tint(fill, 18, warnTint(14, s)), 4.5));
  for (const [pct, s] of [[14, "window"], [12, "overlay"]]) rows.push(pair(`text.primary on warningFill ${pct}% / bg.${s}`, c["text.primary"], warnTint(pct, s), 4.5));
  rows.push(pair("text.onWarning on status.warningFill", c["text.onWarning"], fill, 4.5));
  // the log's error level is mixed into the panel, also on a hovered row (app.css)
  for (const [key, pct, s] of [["danger", 10, "overlay"], ["danger", 14, "panel"], ["danger", 22, "panel"], ["success", 18, "panel"], ["growth", 14, "panel"]]) {
    const color = c[`status.${key}`];
    rows.push(pair(`status.${key} on its ${pct}% / bg.${s}`, color, tint(color, pct, c[`bg.${s}`]), 4.5));
  }
  rows.push(pair("trash.outline on bg.raised (hover card)", T["treemap.trash.outline"], c["bg.raised"], 4.5));
  // the status bar's capacity meter
  rows.push(pair("meter.used on bg.chrome", c["meter.used"], c["bg.chrome"], 3.0));
  rows.push(pair("status.success on bg.chrome", c["status.success"], c["bg.chrome"], 3.0));
  // scrollbar thumbs (app.css): quiet at rest, clear under the pointer, on every surface that scrolls
  for (const s of ["window", "chrome", "panel", "overlay", "raised"]) {
    rows.push(pair(`scroll.thumb on bg.${s}`, c["scroll.thumb"], c[`bg.${s}`], 2.0));
    rows.push(pair(`scroll.thumbHover on bg.${s}`, c["scroll.thumbHover"], c[`bg.${s}`], 3.0));
  }
  // tag shapes (UI-TM-49): on the shell's surfaces in the theme's colours; on the canvas, on their
  // dark base over the least fitting block, in the dark theme's colours (text.ts)
  const blocks = [
    ...CLASSES.map((k) => T[`treemap.class.${k}`]),
    T["treemap.collapsed"],
    T["treemap.ground"],
    ...range(0, 4).map((k) => T[`treemap.level.${k}.headerBg`]),
    ...range(1, 9).flatMap((slot) => range(1, 5).map((tier) => T[`visual.branch.${slot}.${tier}`])),
  ];
  const dark = palette("dark");
  for (const tag of ["red", "yellow", "green", "blue"]) {
    for (const s of SURFACES) rows.push(pair(`tag.${tag} on bg.${s}`, c[`tag.${tag}`], c[`bg.${s}`], 3.0));
    const shape = dark[`color.tag.${tag}`];
    const under = blocks.map((b) => over(c["tag.base"], b));
    const worst = under.reduce((w, bg) => (contrast(shape, bg) < contrast(shape, w) ? bg : w));
    rows.push(pair(`tag.${tag} (dark) on tag.base over a block`, shape, worst, 3.0));
  }
  return rows;
}

function treemapPairs(T) {
  const rows = [];
  for (const k of range(0, 4)) {
    const bg = T[`treemap.level.${k}.headerBg`];
    rows.push(pair(`level.${k}.headerText`, T[`treemap.level.${k}.headerText`], bg, 4.5));
    rows.push(pair(`level.${k}.headerSub`, T[`treemap.level.${k}.headerSub`], bg, 4.5));
  }
  rows.push(pair("halo.text", T["treemap.halo.text"], T["treemap.halo.bg"], 4.5));
  rows.push(pair("halo.sub", T["treemap.halo.sub"], T["treemap.halo.bg"], 4.5));
  const lit = over(T["visual.haloOverlay"], T["visual.center.bg"]);
  for (const part of ["text", "sub"]) {
    rows.push(pair(`center.${part}`, T[`visual.center.${part}`], T["visual.center.bg"], 4.5));
    rows.push(pair(`center.${part} lit`, T[`visual.center.${part}`], lit, 4.5));
  }
  for (const kind of ["free", "unknown", "pending"]) {
    for (const part of ["text", "sub"]) {
      for (const stripe of ["stripeA", "stripeB"]) rows.push(pair(`${kind}.${part} on ${stripe}`, T[`treemap.${kind}.${part}`], T[`treemap.${kind}.${stripe}`], 4.5));
    }
  }
  rows.push(pair("trash.badgeText", T["treemap.trash.badgeText"], T["treemap.trash.badgeBg"], 4.5));
  rows.push(pair("state.newBadgeText", T["treemap.state.newBadgeText"], T["treemap.state.newBadgeBg"], 4.5));
  rows.push(pair("「新增」 badge", T["color.text.onAccent"], T["color.accent.default"], 4.5));
  for (const block of ["treemap.level.3.headerBg", "treemap.class.game", "treemap.class.data", "treemap.class.video", "treemap.ground"]) {
    rows.push(pair(`「已删除」 badge over ${block}`, T["color.text.primary"], over(T["treemap.state.deletedBadgeBg"], T[block]), 4.5));
  }
  // state layers drawn on the canvas (marks, 3:1): the change flash on the ground, the frames and
  // their headers; the breathing outline while scanning and the trash outline on its veil
  const frames = ["treemap.ground", ...range(0, 4).map((k) => `treemap.level.${k}.bg`)];
  for (const f of [...frames, ...range(0, 4).map((k) => `treemap.level.${k}.headerBg`)]) rows.push(pair(`state.flash on ${f}`, T["treemap.state.flash"], T[f], 3.0));
  for (const f of frames) {
    rows.push(pair(`accent.default 90% on ${f}`, over(`${T["color.accent.default"]}E6`, T[f]), T[f], 3.0));
    rows.push(pair(`trash.outline on trash.veil over ${f}`, T["treemap.trash.outline"], over(T["treemap.trash.veil"], T[f]), 3.0));
  }
  // the selection: the white ring or the dark ring outside it, whichever stands out more; the hover
  // inset against the block or against the seam outside it
  const filled = [...CLASSES.map((k) => `treemap.class.${k}`), "treemap.collapsed"];
  for (const f of [...frames, ...filled]) {
    const bg = T[f];
    rows.push([`selection ring on ${f}`, Math.max(contrast(T["treemap.state.selectRing"], bg), contrast(over(T["treemap.state.selectOuter"], bg), bg)), 3.0]);
  }
  for (const f of filled) {
    const inset = over(T["treemap.state.hoverInset"], T[f]);
    rows.push([`hover inset on ${f}`, Math.max(contrast(inset, T[f]), contrast(inset, T["treemap.level.0.bg"])), 3.0]);
  }
  return rows;
}

const LABELLED = [
  ...CLASSES.map((c) => `treemap.class.${c}`),
  "treemap.collapsed",
  ...range(0, 4).map((i) => `treemap.diff.grow.${i}`),
  ...range(0, 3).map((i) => `treemap.diff.shrink.${i}`),
  ...range(0, 5).map((i) => `treemap.time.${i}`),
  ...range(1, 9).flatMap((slot) => range(1, 5).map((tier) => `visual.branch.${slot}.${tier}`)),
  "visual.branch.neutral",
  ...range(1, 5).map((i) => `visual.ring.${i}`),
];
// dark shortfalls from before the light theme (TODO.md): the size line on these, time.2's name, and
// the name on a lit third tier of the odd branches (4.31–4.44)
const DARK_SHORT = new Set(["treemap.time.2", "treemap.time.2 sub", ...[1, 3, 5, 7].flatMap((s) => [`visual.branch.${s}.3 sub`, `visual.branch.${s}.3 lit`])]);

for (const theme of g.THEMES) {
  describe(`the ${theme} theme`, () => {
    const T = palette(theme);

    test("colour pairs are readable", () => {
      const low = [...chromePairs(T), ...treemapPairs(T)].filter(([, ratio, need]) => ratio < need).map(([what, ratio, need]) => `${what} ${ratio.toFixed(2)} < ${need}`);
      assert.deepEqual(low, []);
    });

    test("level contrast keeps the levels apart", () => {
      // 「层级对比」 (0–50) mixes each level's frame from level 0 by ×0–5 (treemap/paint.ts, clamped):
      // at the top of the slider no level may clip at white or black, and neighbours stay apart
      const base = rgba(T["treemap.level.0.bg"]);
      for (const times of [1, 2, 3, 5]) {
        const frames = range(0, 4).map((k) => {
          const level = rgba(T[`treemap.level.${k}.bg`]);
          const mixed = [0, 1, 2].map((i) => base[i] + (level[i] - base[i]) * times);
          assert.ok(mixed.every((v) => v >= 0 && v <= 1), `${theme} ×${times} level ${k}`);
          return `#${mixed.map((v) => hex2(v * 255)).join("")}`;
        });
        assert.ok(range(1, 4).every((k) => contrast(frames[k], frames[k - 1]) > 1.02), `${theme} ×${times} ${frames}`);
      }
    });

    test("block labels are readable", () => {
      // labels take white or near-black, whichever contrasts more with the fill (UI-TM-19); the size
      // line is that colour at 86 % / 84 %; a flame graph's lit row keeps its labels' colours
      const ink = (fill) => {
        const light = T["treemap.label.light"];
        const dark = T["treemap.label.dark"];
        return contrast(fill, light) >= contrast(fill, dark) ? [light, T["treemap.label.lightSub"]] : [dark, T["treemap.label.darkSub"]];
      };
      const checks = [];
      for (const key of LABELLED) {
        const fill = T[key];
        const [text, sub] = ink(fill);
        checks.push([key, text, fill], [`${key} sub`, over(sub, fill), fill]);
      }
      const litKeys = [
        ...range(1, 5).map((i) => `visual.ring.${i}`),
        ...range(1, 9).flatMap((slot) => range(1, 5).map((tier) => `visual.branch.${slot}.${tier}`)),
        "visual.branch.neutral",
        ...range(0, 4).map((i) => `treemap.diff.grow.${i}`),
        ...range(0, 3).map((i) => `treemap.diff.shrink.${i}`),
        "treemap.collapsed",
      ];
      for (const key of litKeys) checks.push([`${key} lit`, ink(T[key])[0], over(T["visual.haloOverlay"], T[key])]);
      const low = checks.filter(([, fg, bg]) => contrast(fg, bg) < 4.5).map(([what]) => what).sort();
      assert.deepEqual(low, theme === "dark" ? [...DARK_SHORT].sort() : []);
    });

    test("fills are distinct", () => {
      // a class without a token would be drawn in 「其他」's colour (lib/classes.ts); the built-in
      // classes are sk-protocol's (ENGLISH_NAMES lists every one)
      const src = readFileSync(join(g.ROOT, "crates", "sk-protocol", "src", "classes.rs"), "utf8");
      const block = src.slice(src.indexOf("ENGLISH_NAMES"), src.indexOf("];", src.indexOf("ENGLISH_NAMES")));
      const ids = [...block.matchAll(/\(\s*(?:"([a-z]+)"|(OTHER))\s*,/g)].map((m) => m[1] ?? "other");
      assert.equal(ids.length, 10);
      const fills = ids.map((id) => T[`treemap.class.${id}`]);
      assert.ok(fills.every(Boolean));
      assert.equal(new Set(fills).size, fills.length);
      const branch = new Set(range(1, 9).flatMap((slot) => range(1, 5).map((tier) => T[`visual.branch.${slot}.${tier}`])));
      assert.equal(branch.size, 32);
      assert.equal(T["visual.center.bg"], T["treemap.level.0.headerBg"]);
    });
  });
}

test("visualization icons", () => {
  const [, icons] = g.loadIcons();
  for (const name of ["vizTreemap", "vizFlame", "vizFlamePolar"]) assert.ok(icons.has(name), name);
  assert.ok(existsSync(join(g.BUILD, "icons", "viz-flame-polar.svg")));
});
