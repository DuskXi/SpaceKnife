// The pinyin of every palette command name: crates/sk-core/src/palette/pinyin.json (embedded by
// palette/commands.rs, which finds a command by its name, the name's full pinyin and its initials).
// Was tools/gen_pinyin.py (pypinyin); no dictionary here:
//
// - the names are read from commands.rs: every `name: "…"` of the registry, and for `name: CONST`
//   every text the constant can take on any platform (`if cfg!(…) { "…" } else { "…" }`);
// - a name already in the table keeps its syllables (they were worked out with its context, which
//   matters for 重 chóng / zhòng and the like);
// - a new name takes each character's syllable from the names already in the table; a character
//   that is not a hanzi is itself. A hanzi the table doesn't have, or has with two readings, stops
//   the script: write that name's row in pinyin.json by hand (one syllable per character) and run
//   it again, which keeps it.
//
//   node tools/gen_pinyin.mjs          # rewrite the table (names gone from the registry drop out)
//   node tools/gen_pinyin.mjs --check  # exit 1 if it is out of date

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const COMMANDS = join(ROOT, "crates", "sk-core", "src", "palette", "commands.rs");
export const TABLE = join(ROOT, "crates", "sk-core", "src", "palette", "pinyin.json");

const HANZI = /\p{Script=Han}/u;
const unquote = (lit) => JSON.parse(lit);

/** Every command name on every platform, sorted as Python sorts strings (by code point). */
export function names(source = readFileSync(COMMANDS, "utf8")) {
  const code = source.replace(/^\s*\/\/.*$/gm, "");
  const out = new Set();
  for (const m of code.matchAll(/\bname:\s*("(?:[^"\\]|\\.)*")/g)) if (m[1] !== '""') out.add(unquote(m[1])); // "": the defaults
  for (const m of code.matchAll(/\bname:\s*([A-Z][A-Z0-9_]*)\b/g)) {
    const def = new RegExp(`const\\s+${m[1]}\\s*:\\s*&str\\s*=([\\s\\S]*?);`).exec(code);
    if (!def) throw new Error(`commands.rs: no const ${m[1]}`);
    for (const lit of def[1].replace(/cfg!\([^)]*\)/g, "").matchAll(/"(?:[^"\\]|\\.)*"/g)) out.add(unquote(lit[0]));
  }
  return [...out].sort(byCodePoint);
}

function byCodePoint(a, b) {
  const x = [...a];
  const y = [...b];
  for (let i = 0; i < Math.min(x.length, y.length); i++) {
    const d = x[i].codePointAt(0) - y[i].codePointAt(0);
    if (d) return d;
  }
  return x.length - y.length;
}

/** {name: syllables} for `wanted`, from `table`; throws listing what needs a hand-written row. */
export function build(wanted, table) {
  const readings = new Map(); // hanzi -> Set of syllables
  for (const [name, syl] of Object.entries(table)) {
    [...name].forEach((ch, i) => {
      if (!HANZI.test(ch) || syl[i] === undefined) return;
      if (!readings.has(ch)) readings.set(ch, new Set());
      readings.get(ch).add(syl[i]);
    });
  }
  const out = {};
  const problems = [];
  for (const name of wanted) {
    if (table[name] && table[name].length === [...name].length) {
      out[name] = table[name];
      continue;
    }
    out[name] = [...name].map((ch) => {
      if (!HANZI.test(ch)) return ch;
      const r = readings.get(ch);
      if (r?.size === 1) return [...r][0];
      problems.push(`${name}: ${ch} ${r ? `has several readings (${[...r].join(", ")})` : "is in no other name"}`);
      return "?";
    });
  }
  if (problems.length) throw new Error(`write these rows of pinyin.json by hand, then run again:\n${problems.join("\n")}`);
  return out;
}

/** The file: one name a line, as json.dumps(ensure_ascii=False) writes it. */
export function render(table) {
  const json = (v) => JSON.stringify(v).replaceAll('","', '", "');
  const rows = Object.keys(table)
    .sort(byCodePoint)
    .map((name) => ` ${JSON.stringify(name)}: ${json(table[name])}`);
  return `{\n${rows.join(",\n")}\n}\n`;
}

function main(argv) {
  const current = readFileSync(TABLE, "utf8");
  let text;
  try {
    text = render(build(names(), JSON.parse(current)));
  } catch (e) {
    console.error(e.message);
    return 1;
  }
  if (argv.includes("--check")) {
    if (text !== current) {
      console.error("pinyin.json is out of date: run node tools/gen_pinyin.mjs");
      return 1;
    }
    return 0;
  }
  writeFileSync(TABLE, text);
  console.log(`wrote ${TABLE}`);
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = main(process.argv.slice(2));
