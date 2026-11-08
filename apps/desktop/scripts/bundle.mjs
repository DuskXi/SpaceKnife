// `pnpm bundle`: the release build. `tauri build --config src-tauri/tauri.bundle.json` with the
// build machine's folders remapped in every Rust binary it makes (tools/package_checks.mjs
// remappedEnv: no home folder or user name in panic messages and debug paths), the app and the
// spaceknife-cli that prepare-bundle.mjs builds alike. Further arguments go to `tauri build`:
//
//   node scripts/bundle.mjs [--no-bundle] [tauri build options …]

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { remappedEnv } from "../../../tools/package_checks.mjs";

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(import.meta.url);
const pkg = require.resolve("@tauri-apps/cli/package.json");
const tauri = join(dirname(pkg), JSON.parse(readFileSync(pkg, "utf8")).bin.tauri);

// macOS: the swap helper 0.3.x portable copies run goes into the app bundle as a resource
// (Contents/Resources/backend/spaceknife-backend; prepare-bundle.mjs makes and signs it)
const macos = process.platform === "darwin" ? ["--config", "src-tauri/tauri.bundle.macos.json"] : [];
// Windows: the forked NSIS template (src-tauri/windows/installer.nsi + spaceknife.nsh), which takes
// over 0.3.4's electron-builder installation, and SpaceKnife.com next to the program
const windows = process.platform === "win32" ? ["--config", "src-tauri/tauri.bundle.windows.json"] : [];
const args = ["build", "--config", "src-tauri/tauri.bundle.json", ...macos, ...windows, ...process.argv.slice(2)];
console.log(`[bundle] tauri ${args.join(" ")} (build paths remapped)`);
const r = spawnSync(process.execPath, [tauri, ...args], { cwd: desktop, stdio: "inherit", env: remappedEnv(process.env) });
if (r.error) throw r.error;
process.exitCode = r.status ?? 1;
