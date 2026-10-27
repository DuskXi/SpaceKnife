// Scans the Electron sources for Chinese text (M7.5): what is translated with t(), and what is
// left hard-coded. Prints one JSON document: {"msgids": [...], "problems": [{file, line, text, why}]}.
//
//   node tools/i18n_scan_ts.mjs <apps/electron/src>
//
// Translated: the first argument of t("…") (a plain string), or the arguments of byLang("…", "…")
// (both texts given in place). Everything else with Chinese is a problem: JSX text, string
// attributes, template literals, other strings. A line ending in `// i18n-ignore` is skipped (data
// that only looks like text: filter aliases, search synonyms).
//
// Parser: @babel/parser from apps/electron's dependencies (TypeScript 7 has no JS compiler API).
import { readFileSync, readdirSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { join, relative, resolve } from "node:path";

const root = resolve(process.argv[2]);
const appDir = resolve(root, "..");
function loadParser() {
  const req = createRequire(join(appDir, "package.json"));
  try {
    return req("@babel/parser");
  } catch {
    const pnpm = join(appDir, "node_modules", ".pnpm");
    const dir = readdirSync(pnpm).find((d) => d.startsWith("@babel+parser@"));
    if (!dir) throw new Error("@babel/parser not found under apps/electron/node_modules");
    return req(join(pnpm, dir, "node_modules", "@babel", "parser"));
  }
}
const { parse } = loadParser();

const CJK = /[㐀-鿿！-｠　-〿]/;
const SKIP_DIRS = new Set(["node_modules", "out", "dist", "locales"]);
// main/text.ts and main/headless.ts: the main process keeps its own zh / en tables (dialogs, the terminal)
const SKIP_FILES = new Set(["text.ts", "headless.ts"]);

function* files(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) {
      if (!SKIP_DIRS.has(name)) yield* files(p);
    } else if (/\.(ts|tsx)$/.test(name) && !(SKIP_FILES.has(name) && dir.endsWith("main")) && !/\.test\.tsx?$/.test(name) && !name.endsWith(".d.ts") && name !== "generated.ts") {
      yield p;
    }
  }
}

const msgids = new Map(); // msgid -> files
const problems = [];

const calleeName = (call) => (call && call.type === "CallExpression" && call.callee.type === "Identifier" ? call.callee.name : null);

for (const file of files(root)) {
  const text = readFileSync(file, "utf8");
  if (!CJK.test(text)) continue;
  const ast = parse(text, { sourceType: "module", plugins: ["typescript", ...(file.endsWith(".tsx") ? ["jsx"] : [])], errorRecovery: true });
  const lines = text.split("\n");
  const rel = relative(root, file).replaceAll("\\", "/");
  const ignored = (node) => /\/\/\s*i18n-ignore\s*$/.test(lines[node.loc.start.line - 1] ?? "") || /\/\/\s*i18n-ignore\s*$/.test(lines[node.loc.end.line - 1] ?? "");
  const addId = (value) => msgids.set(value, [...(msgids.get(value) ?? []), rel]);
  const report = (node, value, why) => {
    if (!ignored(node)) problems.push({ file: rel, line: node.loc.start.line, text: value.slice(0, 80), why });
  };

  const visit = (node, parent) => {
    if (!node || typeof node.type !== "string") return;
    if (node.type === "StringLiteral") {
      const value = node.value;
      if (CJK.test(value)) {
        const fn = calleeName(parent);
        if (fn === "t" && parent.arguments[0] === node) addId(value);
        else if (fn === "byLang") {
          /* both languages given in place */
        } else if (parent && parent.type === "JSXAttribute") report(node, value, 'JSX attribute: use attr={t("…")}');
        else if (parent && (parent.type === "TSLiteralType" || parent.type === "ImportDeclaration")) {
          /* a type or a module path */
        } else report(node, value, "string not passed to t()");
      }
    } else if (node.type === "TemplateLiteral") {
      const value = node.quasis.map((q) => q.value.cooked ?? q.value.raw).join(node.expressions.length ? "{…}" : "");
      if (CJK.test(value)) {
        const fn = calleeName(parent);
        if (node.expressions.length) report(node, value, 'template with ${}: use t("…{name}…", { name })');
        else if (fn === "t" && parent.arguments[0] === node) addId(value);
        else if (fn !== "byLang") report(node, value, 'template literal: use t("…")');
      }
    } else if (node.type === "JSXText") {
      const value = node.value.trim();
      if (value && CJK.test(value)) report(node, value, 'JSX text: use {t("…")}');
    }
    for (const key of Object.keys(node)) {
      if (key === "loc" || key === "start" || key === "end" || key === "leadingComments" || key === "trailingComments" || key === "innerComments") continue;
      const child = node[key];
      if (Array.isArray(child)) for (const c of child) visit(c, node);
      else if (child && typeof child === "object" && typeof child.type === "string") visit(child, node);
    }
  };
  visit(ast.program, null);
}

process.stdout.write(JSON.stringify({ msgids: Object.fromEntries([...msgids].sort()), problems }, null, 1));
