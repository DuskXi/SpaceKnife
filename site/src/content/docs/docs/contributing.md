---
title: Building and contributing
description: Run SpaceKnife from source, run the same checks as CI, build the installers, find your way around the repository and its design documents, and contribute.
---

SpaceKnife is an MIT-licensed project on GitHub: [DuskXi/SpaceKnife](https://github.com/DuskXi/SpaceKnife). Every kind of help counts: trying it on your machine and reporting what breaks, translating, writing documentation and writing code.

## What you need

- A stable [Rust](https://rustup.rs/) toolchain (edition 2024) for the backend, the command line and the app's native side.
- Node.js 24 with pnpm 10, for the app's interface (Nuxt 4 + Vue 3) and its two shells, Tauri 2 and Electron. The website needs Node.js 22.12 or later.
- On Linux, the libraries Tauri builds against: `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, `libssl-dev` and `patchelf` (Ubuntu and Debian package names). On Windows, Microsoft Edge WebView2 (Windows 11 has it) and the MSVC build tools; on macOS, the Xcode command line tools.

## Run from source

```bash
git clone https://github.com/DuskXi/SpaceKnife.git
cd SpaceKnife
cargo build
cd apps/desktop
pnpm install
pnpm tauri dev
```

- `pnpm tauri dev` builds and starts the app. The backend runs inside the app's process (`sk_server::start`), so there is no second program to start.
- `pnpm dev` serves only the interface in a browser at `http://localhost:3000`, without a backend.
- The backend on its own: `cargo run -p sk-server --bin spaceknife-cli -- serve` prints one `ready` line with the WebSocket address and a token. Scans in a terminal: `cargo run -p sk-server --bin spaceknife-cli -- scan <path>` (see [Command line](../command-line/)); add `--release` for realistic speed.
- The same interface also runs in the Electron shell (`apps/electron-shell`), which the rpm, the AppImage and the Linux portable that brings its own engine use, because RHEL 9 has no WebKitGTK 4.1. To try it: `cargo build -p sk-server --bin spaceknife-cli`, `pnpm -C apps/desktop generate`, then `pnpm -C apps/electron-shell install` and `pnpm -C apps/electron-shell dev`. Its backend is the child process `spaceknife-cli serve`. The interface talks to either shell only through `window.sk` (`app/shared/api.ts`), and a change to it has to work in both.
- Set `SPACEKNIFE_HOME` to an empty folder to keep your experiments away from your real snapshots and settings.

## Checks before a pull request

These are the checks CI runs. Rust:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Generated files must be up to date:

```bash
node tools/gen_design.mjs --check
cargo run -p sk-protocol --bin gen-ts -- --check
```

The Windows console launcher `SpaceKnife.com` is a small workspace of its own:

```bash
cargo clippy --manifest-path packaging/console-launcher/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path packaging/console-launcher/Cargo.toml
```

The interface, the Electron shell, the tools and the translations:

```bash
pnpm -C apps/desktop typecheck
pnpm -C apps/desktop test
pnpm -C apps/electron-shell typecheck
pnpm -C apps/electron-shell test
node --test "tools/tests/*.test.mjs"
node tools/i18n_check.mjs
```

- `cargo test --workspace` includes the parity gate's replay (`cargo test -p sk-server --test parity_replay`): the backend's answers over the protocol, compared with those 0.3.4's Python backend gave to the same scenarios. The live comparison against a `v0.3.4` worktree needs Python 3.12 and uv; how to run it and how to record the answers again is in [tests/parity/README.md](https://github.com/DuskXi/SpaceKnife/blob/main/tests/parity/README.md).
- A bug fix comes with a test that fails without it.
- Keep tests and benchmarks small and short. Never scan or write large amounts of data on a contributor's real drives.

## Build the installers

```bash
cd apps/desktop
pnpm install
pnpm bundle
pnpm bundle:portable
```

`pnpm bundle` builds the bundles of the system it runs on with Tauri, into `target/release/bundle/`: the NSIS installer on Windows; AppImage, deb and rpm on Linux; the app and a dmg on macOS. Before Tauri runs, `apps/desktop/scripts/prepare-bundle.mjs` builds `spaceknife-cli` (installed next to the app) and writes the license files. `pnpm bundle:portable`, after `pnpm bundle`, packs the portable archive into `target/portable/`. A release publishes the Tauri builds of the Windows installer, the deb, the macOS app and dmg and the portables; the rpm, the AppImage and the Linux portable under 0.3.4's file name come from the Electron shell instead: `pnpm -C apps/electron-shell package` builds them into `target/electron-shell/dist`.

- Test a packaged app with `node packaging/smoke-app.mjs` (either shell), headless mode with `node packaging/smoke-headless.mjs`, and a portable copy's update with `node packaging/update-e2e.mjs`. `node packaging/update-from-034.mjs` updates a real 0.3.4 to a build; CI runs it for every kind of install.
- The release file names are a contract with every installed copy: 0.3.4 looks for its file under exactly these names, so they never change.

The details, including the release file names, are in [docs/packaging.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/packaging.md).

## The website and these pages

```bash
pnpm -C site install
pnpm -C site dev
```

The website lives in `site/` (Astro with Starlight) and is published to GitHub Pages from `main`. These documentation pages are Markdown files: `site/src/content/docs/docs/<id>.md` in English and `site/src/content/docs/zh/docs/<id>.md` in Chinese, ordered by `site/src/docs-nav.json`. From 0.3.2 the app's help page uses the same files. Read [docs/docs-writing.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/docs-writing.md) before you edit them: it lists the Markdown features both places can show. The shortcut and settings tables between `<!-- generated:… -->` markers mirror the command list (`crates/sk-core/src/palette/commands.rs`) and the settings registry (`crates/sk-core/src/settings/registry.rs`); until a generator reads them from the Rust code, they are kept up to date by hand: change them together with a command or a setting. `pnpm -C site build` checks that every link points to an existing page.

## Repository layout

| Path | Contents |
|---|---|
| `crates/sk-protocol` | The protocol: message types, binary frame codec, the TypeScript types and JSON Schema it generates |
| `crates/sk-store` | The columnar store of a scan and the snapshot files |
| `crates/sk-native` | Parallel folder walking and MFT parsing |
| `crates/sk-core` | Scanning, the filter engine, layouts, views, monitoring, export, file operations, settings, updates |
| `crates/sk-server` | The WebSocket server and the `spaceknife-cli` command line |
| `apps/desktop` | The app: the interface (`app/`, Nuxt + Vue) and the Tauri shell and bundle configuration (`src-tauri/`) |
| `apps/electron-shell` | The Electron shell: main process and preload, no interface of its own |
| `design/tokens`, `design/icons` | Design tokens and icon sources; `design/build` is generated |
| `packaging` | The tests of packaged apps (smoke, headless, updates, also from 0.3.4) and the Windows console launcher `SpaceKnife.com` |
| `tests/parity` | The parity gate: the same protocol scenarios against 0.3.4's backend and the Rust one |
| `site` | The website and this documentation |
| `docs` | Design documents |
| `tools` | Code generation and developer tools: `gen_design.mjs`, `gen_pinyin.mjs`, `i18n_check.mjs`, `release.mjs`, `demo/screenshots.mjs` |

## Design documents

The design documents describe how each part works and the contracts between the parts. They are written in Chinese.

| Document | What it covers |
|---|---|
| [PLAN.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/PLAN.md) | The plan: decisions, architecture, modules, milestones and risks |
| [UI-SPEC.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/UI-SPEC.md) | The interface specification, requirement by requirement |
| [protocol.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/protocol.md) | The WebSocket protocol between the backend and the front ends |
| [visuals.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/visuals.md) | The visualizations: layout engines, wire formats, renderer components |
| [plugins.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/plugins.md) | Lessons and requirements for a future plugin system (not implemented) |
| [packaging.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/packaging.md) | Installers, portable archives, data locations, update checks |
| [memory.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/memory.md) | The memory model: cost per item, mapped storage, the watchdog |
| [i18n.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/i18n.md) | How the interface texts are translated |
| [docs-writing.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/docs-writing.md) | How to write these documentation pages |
| [native.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/native.md) | The native scanning code, the scan helper and the disk-load limit |
| [monitor.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/monitor.md) | Live monitoring (the file change hooks) |
| [manage.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/manage.md) | The delete basket, snapshot comparison, export, insights and settings import |
| [filter-syntax.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/filter-syntax.md) | The filter language, formally |
| [export-templates.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/export-templates.md) | The export template language, the export formats and `spaceknife-cli run` |
| [insights.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/insights.md) | The format of the insight rules |

When you change an interface between parts, change its document first.

## How to contribute

- **Report problems**: open a [bug report](https://github.com/DuskXi/SpaceKnife/issues/new?template=bug_report.yml); what to include is in [Troubleshooting](../troubleshooting/#reporting-a-bug). Tests on a clean Windows 10, RHEL 9 or Ubuntu 22.04 are especially welcome.
- **Suggest ideas** in [Discussions › Ideas](https://github.com/DuskXi/SpaceKnife/discussions/categories/ideas) or with a [feature request](https://github.com/DuskXi/SpaceKnife/issues/new?template=feature_request.yml).
- **Pick an issue**: issues labelled [good first issue](https://github.com/DuskXi/SpaceKnife/labels/good%20first%20issue) are self-contained. Comment before you start, so nobody duplicates the work.
- **Translate**: the interface has English and Simplified Chinese; see [docs/i18n.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/i18n.md).

For pull requests:

- Match the surrounding code: naming, comment density, idioms. Rust that passes `cargo fmt` and `cargo clippy -D warnings`; protocol messages are types in `sk-protocol`; logs use `tracing`. The interface is TypeScript with Vue's `<script setup>`.
- One change per pull request, with a short description of what and why, and tests for behaviour changes.
- Text the user sees goes through `t()` / `tr!` in the backend or `t()` in the interface, with an English entry in the catalogs.
- No personal data: examples, tests, screenshots and logs use made-up paths and names, such as `C:\Users\lenny\…` or `/home/lenny/…`.
- Windows and Linux must keep working; say so if you could only test one.

The full guide is [CONTRIBUTING.md](https://github.com/DuskXi/SpaceKnife/blob/main/CONTRIBUTING.md). By contributing you agree that your contribution is licensed under the [MIT License](https://github.com/DuskXi/SpaceKnife/blob/main/LICENSE), and everyone taking part follows the [Code of Conduct](https://github.com/DuskXi/SpaceKnife/blob/main/CODE_OF_CONDUCT.md). Security problems are reported privately, see [SECURITY.md](https://github.com/DuskXi/SpaceKnife/blob/main/SECURITY.md).
