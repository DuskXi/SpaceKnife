// tools/gen_pinyin.mjs: the table is up to date and the build rules hold.
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { build, names, render, TABLE } from "../gen_pinyin.mjs";

const table = () => JSON.parse(readFileSync(TABLE, "utf8"));

test("the pinyin table is up to date", () => {
  assert.equal(render(build(names(), table())), readFileSync(TABLE, "utf8"), "run: node tools/gen_pinyin.mjs");
});

test("every command name is in the table", () => {
  const t = table();
  const n = names();
  assert.deepEqual(Object.keys(t).sort(), [...n].sort());
  assert.ok(n.length > 40);
  assert.deepEqual(t["切换配色"], ["qie", "huan", "pei", "se"]);
  // the name of a constant that differs by platform: every platform's text
  for (const reveal of ["在访达中显示", "在资源管理器中显示", "在文件管理器中显示"]) assert.ok(n.includes(reveal), reveal);
});

test("names are read from the registry source", () => {
  const src = [
    'const SHOW: &str = if cfg!(windows) { "显示甲" } else { "显示乙" };',
    '    // name: "注释里的"',
    '    Def { id: "a", name: "切换", ..D },',
    '    Def { id: "b", name: SHOW, ..D },',
    'const D: Def = Def { name: "", id: "" };',
  ].join("\n");
  assert.deepEqual(names(src), ["切换", "显示乙", "显示甲"]); // by code point, as the table
});

test("a new name reuses known syllables, and an unknown or ambiguous hanzi stops it", () => {
  const known = { 切换配色: ["qie", "huan", "pei", "se"], 重新扫描: ["chong", "xin", "sao", "miao"], 重要: ["zhong", "yao"] };
  assert.deepEqual(build(["配色…", "切换配色"], known), { "配色…": ["pei", "se", "…"], 切换配色: ["qie", "huan", "pei", "se"] });
  assert.throws(() => build(["新颜色"], known), /颜 is in no other name/);
  assert.throws(() => build(["重扫"], known), /重 has several readings/);
});

test("the file has one name a line", () => {
  assert.equal(render({ 乙: ["yi"], 甲: ["jia", "…"] }), '{\n "乙": ["yi"],\n "甲": ["jia", "…"]\n}\n');
});
