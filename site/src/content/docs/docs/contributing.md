---
title: Building and contributing
description: Run SpaceKnife from source, run the same checks as CI, build the installers, find your way around the repository and its design documents, and contribute.
---

SpaceKnife is an MIT-licensed project on GitHub: [DuskXi/SpaceKnife](https://github.com/DuskXi/SpaceKnife). Every kind of help counts: trying it on your machine and reporting what breaks, translating, writing documentation and writing code.

## What you need

- [uv](https://docs.astral.sh/uv/). It installs and manages Python 3.12 for the workspace.
- Node.js 22 with pnpm, for the Electron app. The website needs Node.js 22.12 or later.
- A Rust toolchain, for the native extension (the fast folder walker and the MFT reader). It is optional for development: without it every native path falls back to Python, and scans are several times slower. The installers require it.

## Run from source

```bash
git clone https://github.com/DuskXi/SpaceKnife.git
cd SpaceKnife
uv sync --group native
pnpm -C apps/electron install
pnpm -C apps/electron dev
```

- `uv sync --group native` sets up the Python workspace and compiles the native extension. Plain `uv sync` skips the extension.
- `pnpm -C apps/electron dev` starts the Electron app. It launches the backend from the repository's environment (`.venv`); `SPACEKNIFE_PYTHON` points it at another interpreter.
- The backend on its own: `uv run spaceknife serve` prints one `ready` line with the WebSocket address and a development token. Scans in a terminal: `uv run spaceknife scan <path>` (see [Command line](../command-line/)).
- Set `SPACEKNIFE_HOME` to an empty folder to keep your experiments away from your real snapshots and settings.

## Checks before a pull request

These are the checks CI runs. Python:

```bash
uv run ruff check .
uv run ruff format --check .
uv run mypy packages/protocol/src packages/backend/src packages/client/src tools/gen_design.py
uv run pytest
```

Generated files must be up to date:

```bash
uv run python tools/gen_design.py --check
uv run spaceknife schema --check packages/protocol/schema/protocol.schema.json
uv run python tools/gen_ts_protocol.py --check
uv run python tools/gen_docs.py --check
```

The Electron app and the translations:

```bash
pnpm -C apps/electron typecheck
pnpm -C apps/electron test
uv run python tools/i18n_check.py
```

- CI runs the Python checks on Windows, Ubuntu 22.04 and macOS 14, with the native extension built, and the Electron checks with Node.js 22.
- `SPACEKNIFE_NO_NATIVE=1` makes the backend use the Python fallbacks; the tests compare the native and Python results.
- Keep tests and benchmarks small and short. Never scan or write large amounts of data on a contributor's real drives.

## Build the installers

```bash
uv sync --locked --group native --group package
pnpm -C apps/electron install
uv run python tools/build_package.py
```

The script builds the installers of the system it runs on, into `dist/electron/`: the NSIS installer on Windows; AppImage, deb and rpm on Linux; a dmg for the Mac's own architecture on macOS.

| Option | What it does |
|---|---|
| `--portable` | Also build the portable archive (Windows zip, Linux tar.gz) |
| `--no-installers` | Only the unpacked app (and the portable archive with `--portable`) |
| `--skip-backend` | Reuse the frozen backend in `dist/backend/` when only the Electron side changed |
| `--targets AppImage deb rpm` | Linux: build only some of the formats |

It runs these steps: a release build of the native extension with the build machine's paths removed; PyInstaller freezes the backend; a smoke test of the frozen backend (the native extension must load); on Linux, a check that nothing needs a glibc newer than 2.34 (RHEL 9); `electron-vite build`; the license files; `electron-builder`; a check that no packaged file contains the build machine's home folder; on macOS, a check of the ad-hoc signature.

- The rpm needs `rpmbuild`; the AppImage's `.zsync` file needs `zsyncmake` (without it the update information is still written). The first run of electron-builder downloads its tools.
- Test a packaged app with `uv run python packaging/smoke_app.py dist/electron/win-unpacked/SpaceKnife.exe` (or `linux-unpacked/spaceknife`, or an AppImage). It starts the app minimized with a temporary data folder, waits for the backend and the start page, then closes it like a user would.

The details, including the release file names, are in [docs/packaging.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/packaging.md).

## The website and these pages

```bash
pnpm -C site install
pnpm -C site dev
```

The website lives in `site/` (Astro with Starlight) and is published to GitHub Pages from `main`. These documentation pages are Markdown files: `site/src/content/docs/docs/<id>.md` in English and `site/src/content/docs/zh/docs/<id>.md` in Chinese, ordered by `site/src/docs-nav.json`. From 0.3.2 the app's help page uses the same files. Read [docs/docs-writing.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/docs-writing.md) before you edit them: it lists the Markdown features both places can show. The shortcut and settings tables between `<!-- generated:… -->` markers are written by `uv run python tools/gen_docs.py` from `palette/commands.py` and `settings.py`; don't edit them by hand, and rerun the script after changing a command or a setting. `pnpm -C site build` checks that every link points to an existing page.

## Repository layout

| Path | Contents |
|---|---|
| `packages/protocol` | The protocol: pydantic message models, binary frame codec, JSON Schema |
| `packages/backend` | The backend: WebSocket server, scanning, columnar store and snapshots, filter engine, layouts, views, monitoring, export, file operations, command line |
| `packages/native` | The native Rust extension (PyO3): parallel folder walking, MFT parsing |
| `packages/client` | Client core shared by Python front ends (no GUI dependencies) |
| `apps/electron` | The Electron app and its installer configuration (`electron-builder.yml`) |
| `design/tokens`, `design/icons` | Design tokens and icon sources; `design/build` is generated |
| `packaging` | The frozen backend (PyInstaller spec and entry point) and the smoke tests |
| `site` | The website and this documentation |
| `docs` | Design documents |
| `tools` | Code generation and developer tools: `build_package.py`, `gen_design.py`, `gen_ts_protocol.py`, `i18n_check.py`, `demo/screenshots.py` |

## Design documents

The design documents describe how each part works and the contracts between the parts. They are written in Chinese.

| Document | What it covers |
|---|---|
| [PLAN.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/PLAN.md) | The plan: decisions, architecture, modules, milestones and risks |
| [UI-SPEC.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/UI-SPEC.md) | The interface specification, requirement by requirement |
| [protocol.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/protocol.md) | The WebSocket protocol between the backend and the front ends |
| [visuals.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/visuals.md) | The visualizations: layout engines, wire formats, renderer components |
| [plugins.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/plugins.md) | Lessons and requirements for a future plugin system (not implemented) |
| [packaging.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/packaging.md) | Installers, the frozen backend, data locations, update checks |
| [memory.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/memory.md) | The memory model: cost per item, mapped storage, the watchdog |
| [i18n.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/i18n.md) | How the interface texts are translated |
| [docs-writing.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/docs-writing.md) | How to write these documentation pages |
| [native.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/native.md) | The native extension, the scan helper and the disk-load limit |
| [monitor.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/monitor.md) | Live monitoring (the file change hooks) |
| [manage.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/manage.md) | The delete basket, snapshot comparison, export, insights and settings import |
| [filter-syntax.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/filter-syntax.md) | The filter language, formally |
| [export-templates.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/export-templates.md) | The export template language, the export formats and `spaceknife run` |
| [insights.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/insights.md) | The format of the insight rules |

When you change an interface between parts, change its document first.

## How to contribute

- **Report problems**: open a [bug report](https://github.com/DuskXi/SpaceKnife/issues/new?template=bug_report.yml); what to include is in [Troubleshooting](../troubleshooting/#reporting-a-bug). Tests on a clean Windows 10, RHEL 9 or Ubuntu 22.04 are especially welcome.
- **Suggest ideas** in [Discussions › Ideas](https://github.com/DuskXi/SpaceKnife/discussions/categories/ideas) or with a [feature request](https://github.com/DuskXi/SpaceKnife/issues/new?template=feature_request.yml).
- **Pick an issue**: issues labelled [good first issue](https://github.com/DuskXi/SpaceKnife/labels/good%20first%20issue) are self-contained. Comment before you start, so nobody duplicates the work.
- **Translate**: the interface has English and Simplified Chinese; see [docs/i18n.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/i18n.md).

For pull requests:

- Match the surrounding code: naming, comment density, idioms. Python 3.12 with type hints (mypy strict); protocol messages are pydantic models; command-line output uses rich and logs use loguru.
- One change per pull request, with a short description of what and why, and tests for behaviour changes.
- Text the user sees goes through `_()` / `tr()` in the backend or `t()` in the front end, with an English entry in the catalogs.
- No personal data: examples, tests, screenshots and logs use made-up paths and names, such as `C:\Users\lenny\…` or `/home/lenny/…`.
- Windows and Linux must keep working; say so if you could only test one.

The full guide is [CONTRIBUTING.md](https://github.com/DuskXi/SpaceKnife/blob/main/CONTRIBUTING.md). By contributing you agree that your contribution is licensed under the [MIT License](https://github.com/DuskXi/SpaceKnife/blob/main/LICENSE), and everyone taking part follows the [Code of Conduct](https://github.com/DuskXi/SpaceKnife/blob/main/CODE_OF_CONDUCT.md). Security problems are reported privately, see [SECURITY.md](https://github.com/DuskXi/SpaceKnife/blob/main/SECURITY.md).
