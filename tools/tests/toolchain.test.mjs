// rust-toolchain.toml pins the Rust of every build: the workflows install exactly that one, so CI
// lints and tests with the version developers have (a newer stable's lints once failed CI only).
//   node --test "tools/tests/*.test.mjs"

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const ROOT = fileURLToPath(new URL("../..", import.meta.url));
const WORKFLOWS = join(ROOT, ".github", "workflows");

/** The workflows: name -> text. */
const workflows = () => Object.fromEntries(readdirSync(WORKFLOWS).filter((n) => /\.ya?ml$/.test(n)).map((n) => [n, readFileSync(join(WORKFLOWS, n), "utf8")]));

/** The jobs of a workflow: name -> its lines (the keys two spaces in under `jobs:`). */
function jobs(text) {
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((l) => l === "jobs:");
  const out = {};
  let name = null;
  for (const line of lines.slice(start + 1)) {
    if (/^\S/.test(line)) break;
    const m = /^ {2}([A-Za-z0-9_-]+):\s*$/.exec(line);
    if (m) out[(name = m[1])] = [];
    else if (name) out[name].push(line);
  }
  return out;
}

test("the toolchain is pinned to a release, with clippy and rustfmt", () => {
  const toml = readFileSync(join(ROOT, "rust-toolchain.toml"), "utf8");
  const channel = /^channel\s*=\s*"([^"]+)"/m.exec(toml)?.[1];
  assert.match(channel ?? "", /^\d+\.\d+\.\d+$/, "an exact version, not stable or a date");
  const components = /^components\s*=\s*\[([^\]]*)\]/m.exec(toml)?.[1] ?? "";
  for (const c of ["clippy", "rustfmt"]) assert.ok(components.includes(`"${c}"`), `${c} in components`);
});

test("no workflow picks a Rust of its own", () => {
  for (const [name, text] of Object.entries(workflows())) {
    // rustup update/default/toolchain install stable, rustup-init --default-toolchain stable, the
    // actions that install a channel named in the workflow
    const own = /rustup\s+(update|default|toolchain\s+install)\s+(stable|beta|nightly|\d)|--default-toolchain\s+(?!none\b)\S|dtolnay\/rust-toolchain|actions-rs\/toolchain|RUSTUP_TOOLCHAIN|cargo\s+\+/;
    assert.doesNotMatch(text, own, `${name} chooses a toolchain instead of rust-toolchain.toml's`);
  }
});

test("every job that builds Rust installs the pinned toolchain first", () => {
  let seen = 0;
  for (const [name, text] of Object.entries(workflows())) {
    for (const [job, lines] of Object.entries(jobs(text))) {
      const body = lines.join("\n");
      // the jobs that build Rust cache it (Swatinem/rust-cache)
      const cache = lines.findIndex((l) => l.includes("Swatinem/rust-cache"));
      if (cache < 0) {
        assert.doesNotMatch(body, /\bcargo (build|test|clippy|run)\b/, `${name} ${job} runs cargo without the toolchain step and the cache`);
        continue;
      }
      seen += 1;
      const install = lines.findIndex((l) => /rustup toolchain install --no-self-update\s*(#.*)?$/.test(l));
      assert.ok(install >= 0 && install < cache, `${name} ${job}: rustup toolchain install (rust-toolchain.toml) before the cache`);
    }
  }
  assert.ok(seen >= 6, `only ${seen} jobs build Rust: the job split above no longer reads the workflows`);
});
