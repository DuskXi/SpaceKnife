// Checks the translations (M7.5): no hard-coded Chinese left in the interface, every text has an
// English entry. Was tools/i18n_check.py + tools/i18n_scan_ts.mjs (Python backend, Electron).
//
//   node tools/i18n_check.mjs [--area desktop|core|all] [--file PATH ...] [--missing]
//
// desktop: apps/desktop/app (.ts and .vue; parsed with the app's own vue/compiler-sfc, so
//   `pnpm install` in apps/desktop first). Translated: the first argument of t("…") (a plain
//   string), or the arguments of byLang("…", "…") (both texts given in place). Everything else with
//   Chinese is a problem: template text, static attributes, template literals, other strings. A line
//   ending in `// i18n-ignore` (`<!-- i18n-ignore -->` in a template) is skipped: data that only
//   looks like text (filter aliases, search synonyms). Catalog: apps/desktop/app/locales/en/*.json.
// core: the Rust crates' message ids (the first argument of t("…"), tr!("…", …), tr_with("…", …))
//   must have an entry in crates/sk-core/locales/en/*.json. Rust keeps other Chinese text in
//   zh / en tables of its own (registries translated where shown, the terminal's texts), so only
//   the ids are checked there, not hard-coded text.
//
// Exit status 1 when anything is wrong; --missing prints the ids without a translation as JSON
// (a starting point for a catalog file).

import { readdirSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DESKTOP = join(ROOT, "apps", "desktop");
const APP = join(DESKTOP, "app");
const DESKTOP_CATALOG = join(APP, "locales", "en");
const CRATES = join(ROOT, "crates");
const CORE_CATALOG = join(CRATES, "sk-core", "locales", "en");

export const CJK = /[㐀-鿿！-｠　-〿]/;
const PLACEHOLDER = /\{(\w*)(?::[^{}]*)?\}/g;
const rel = (p) => relative(ROOT, p).replaceAll("\\", "/");

function* walk(dir, accept, skip = new Set()) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) {
      if (!skip.has(name)) yield* walk(p, accept, skip);
    } else if (accept(name)) yield p;
  }
}

// ------------------------------------------------------------------ desktop (Vue / TypeScript)

const desktopFile = (name) => /\.(ts|vue)$/.test(name) && !/\.test\.ts$/.test(name) && !name.endsWith(".d.ts") && name !== "generated.ts";

function compiler() {
  const require = createRequire(join(DESKTOP, "package.json"));
  try {
    return require("vue/compiler-sfc");
  } catch {
    return null;
  }
}

const calleeName = (call) => (call && call.type === "CallExpression" && call.callee.type === "Identifier" ? call.callee.name : null);

/** Scan one script (TS source); `line0` shifts its line numbers, `isIgnored(line)` skips lines. */
function scanScript(sfc, code, { file, line0 = 0, isIgnored, addId, report }) {
  if (!CJK.test(code)) return;
  const ast = sfc.babelParse(code, { sourceType: "module", plugins: ["typescript"], errorRecovery: true });
  const at = (node) => line0 + node.loc.start.line;
  const ignored = (node) => isIgnored(line0 + node.loc.start.line) || isIgnored(line0 + node.loc.end.line);
  const visit = (node, parent) => {
    if (!node || typeof node.type !== "string") return;
    if (node.type === "StringLiteral" && CJK.test(node.value)) {
      const fn = calleeName(parent);
      if (fn === "t" && parent.arguments[0] === node) addId(node.value, file);
      else if (fn === "byLang") {
        /* both languages given in place */
      } else if (parent && (parent.type === "TSLiteralType" || parent.type === "ImportDeclaration")) {
        /* a type or a module path */
      } else if (!ignored(node)) report(file, at(node), node.value, "string not passed to t()");
    } else if (node.type === "TemplateLiteral") {
      const value = node.quasis.map((q) => q.value.cooked ?? q.value.raw).join(node.expressions.length ? "{…}" : "");
      if (CJK.test(value)) {
        const fn = calleeName(parent);
        if (fn === "t" && parent.arguments[0] === node && !node.expressions.length) addId(value, file);
        else if (fn === "byLang") {
          /* both languages given in place */
        } else if (!ignored(node)) {
          report(file, at(node), value, node.expressions.length ? 'template with ${}: use t("…{name}…", { name })' : 'template literal: use t("…")');
        }
      }
    }
    for (const key of Object.keys(node)) {
      if (["loc", "start", "end", "leadingComments", "trailingComments", "innerComments", "extra"].includes(key)) continue;
      const child = node[key];
      if (Array.isArray(child)) for (const c of child) visit(c, node);
      else if (child && typeof child === "object" && typeof child.type === "string") visit(child, node);
    }
  };
  visit(ast.program, null);
}

// Vue's template AST node types (@vue/compiler-core NodeTypes)
const ELEMENT = 1;
const TEXT = 2;
const INTERPOLATION = 5;
const ATTRIBUTE = 6;
const DIRECTIVE = 7;

function scanVue(sfc, file, text, ctx) {
  const { descriptor } = sfc.parse(text, { filename: file });
  const lines = text.split("\n");
  const isIgnored = (n) => /(\/\/\s*i18n-ignore|<!--\s*i18n-ignore\s*-->)\s*$/.test(lines[n - 1] ?? "");
  for (const block of [descriptor.script, descriptor.scriptSetup]) {
    if (block) scanScript(sfc, block.content, { ...ctx, file, line0: block.loc.start.line - 1, isIgnored });
  }
  const template = descriptor.template?.ast;
  if (!template) return;
  const expr = (content, line) => {
    // an expression of the template, scanned as a script of its own line
    scanScript(sfc, `(${content})`, { ...ctx, file, line0: line - 1, isIgnored: () => isIgnored(line) });
  };
  const visit = (node) => {
    if (node.type === TEXT && CJK.test(node.content) && !isIgnored(node.loc.start.line)) {
      ctx.report(file, node.loc.start.line, node.content.trim(), 'template text: use {{ t("…") }}');
    } else if (node.type === INTERPOLATION && node.content?.content) {
      expr(node.content.content, node.loc.start.line);
    } else if (node.type === ELEMENT) {
      for (const p of node.props) {
        if (p.type === ATTRIBUTE && p.value && CJK.test(p.value.content) && !isIgnored(p.loc.start.line)) {
          ctx.report(file, p.loc.start.line, p.value.content, `attribute ${p.name}: use :${p.name}="t('…')"`);
        } else if (p.type === DIRECTIVE) {
          if (p.exp?.content) expr(p.exp.content, p.loc.start.line);
          if (p.arg && !p.arg.isStatic && p.arg.content) expr(p.arg.content, p.loc.start.line);
        }
      }
    }
    for (const child of node.children ?? []) visit(child);
  };
  visit(template);
}

export function scanDesktop(only) {
  const sfc = compiler();
  if (!sfc) return null;
  const msgids = new Map();
  const problems = [];
  const ctx = {
    addId: (id, file) => msgids.set(id, new Set([...(msgids.get(id) ?? []), file])),
    report: (file, line, text, why) => problems.push({ file, line, text: text.slice(0, 80), why }),
  };
  for (const path of walk(APP, desktopFile, new Set(["node_modules", "locales"]))) {
    const text = readFileSync(path, "utf8");
    if (!CJK.test(text)) continue;
    const file = rel(path);
    if (path.endsWith(".vue")) scanVue(sfc, file, text, ctx);
    else {
      const lines = text.split("\n");
      scanScript(sfc, text, { ...ctx, file, isIgnored: (n) => /\/\/\s*i18n-ignore\s*$/.test(lines[n - 1] ?? "") });
    }
  }
  return filtered(msgids, problems, only);
}

// ------------------------------------------------------------------ core (Rust)

const RUST_CALL = /(?<![\w.:])(?:t|tr!|tr_with)\(\s*"((?:[^"\\]|\\.)*)"/g;

export function scanRust(only) {
  const msgids = new Map();
  for (const path of walk(CRATES, (n) => n.endsWith(".rs"), new Set(["target", "tests", "examples", "generated"]))) {
    const text = readFileSync(path, "utf8");
    if (!CJK.test(text)) continue;
    const code = text.replace(/^\s*\/\/.*$/gm, "");
    // unit tests at the end of a file: their texts need no catalog entry
    const end = code.search(/#\[cfg\(test\)\]\s*mod\s+\w+/);
    for (const m of (end >= 0 ? code.slice(0, end) : code).matchAll(RUST_CALL)) {
      const id = JSON.parse(`"${m[1].replace(/\\'/g, "'")}"`);
      if (CJK.test(id)) msgids.set(id, new Set([...(msgids.get(id) ?? []), rel(path)]));
    }
  }
  return filtered(msgids, [], only);
}

function filtered(msgids, problems, only) {
  if (!only.size) return { msgids, problems };
  return {
    msgids: new Map([...msgids].filter(([, files]) => [...files].some((f) => only.has(f)))),
    problems: problems.filter((p) => only.has(p.file)),
  };
}

// ------------------------------------------------------------------ catalogs

export function loadCatalog(folder) {
  const merged = new Map();
  const conflicts = [];
  for (const f of readdirSync(folder).filter((n) => n.endsWith(".json")).sort()) {
    for (const [k, v] of Object.entries(JSON.parse(readFileSync(join(folder, f), "utf8")))) {
      if (merged.has(k) && merged.get(k) !== v) conflicts.push(`${f}: ${JSON.stringify(k)} = ${JSON.stringify(v)}, elsewhere ${JSON.stringify(merged.get(k))}`);
      merged.set(k, v);
    }
  }
  return { catalog: merged, conflicts };
}

const fields = (s) => [...s.matchAll(PLACEHOLDER)].map((m) => m[1]).sort().join(",");

export function checkCatalog(msgids, catalog, side) {
  const out = [];
  for (const m of [...msgids.keys()].sort()) {
    const en = catalog.get(m);
    if (!en) out.push(`${side}: no English for ${JSON.stringify(m)}`);
    else if (fields(m) !== fields(en)) out.push(`${side}: placeholders differ: ${JSON.stringify(m)} -> ${JSON.stringify(en)}`);
    else if (CJK.test(en)) out.push(`${side}: the English text still has Chinese: ${JSON.stringify(m)} -> ${JSON.stringify(en)}`);
  }
  return out;
}

// ------------------------------------------------------------------ command line

function main(argv) {
  const area = argv.includes("--area") ? argv[argv.indexOf("--area") + 1] : "all";
  if (!["desktop", "core", "all"].includes(area)) {
    console.error("usage: node tools/i18n_check.mjs [--area desktop|core|all] [--file PATH ...] [--missing]");
    return 2;
  }
  const only = new Set(argv.flatMap((a, i) => (argv[i - 1] === "--file" ? [rel(resolve(a))] : [])));
  const missingOnly = argv.includes("--missing");
  let bad = 0;
  const missing = {};
  const areas = [
    ["desktop", () => scanDesktop(only), DESKTOP_CATALOG],
    ["core", () => scanRust(only), CORE_CATALOG],
  ];
  for (const [name, scan, folder] of areas) {
    if (area !== "all" && area !== name) continue;
    const found = scan();
    if (!found) {
      console.log(`${name}: skipped (needs apps/desktop/node_modules: pnpm install there)`);
      if (area === name) bad++; // asked for by name (CI): not checking is a failure
      continue;
    }
    const { catalog, conflicts } = loadCatalog(folder);
    for (const p of found.problems) {
      console.log(`${p.file}:${p.line}: ${p.why}: ${JSON.stringify(p.text)}`);
      bad++;
    }
    for (const line of conflicts) {
      console.log(`${name} catalog conflict: ${line}`);
      bad++;
    }
    for (const line of checkCatalog(found.msgids, catalog, name)) {
      if (!missingOnly) console.log(line);
      bad++;
    }
    for (const m of found.msgids.keys()) if (!catalog.get(m)) missing[m] = "";
  }
  if (missingOnly) process.stdout.write(`${JSON.stringify(Object.fromEntries(Object.entries(missing).sort()), null, 1)}\n`);
  console.error(`${bad} problem(s)`);
  return bad ? 1 : 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = main(process.argv.slice(2));
