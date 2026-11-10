# Contributing to SpaceKnife

**English** · [简体中文](#参与贡献)

Thanks for your interest! SpaceKnife is a small, MIT-licensed project and every kind of help counts: trying it on your machine, reporting what breaks, translating, writing docs and writing code.

## Ways to help

- **Try it and report problems.** Testing on a clean Windows 10, RHEL 9, Ubuntu 22.04 or another distribution is especially valuable right now. Open a [bug report](https://github.com/DuskXi/SpaceKnife/issues/new?template=bug_report.yml).
- **Suggest ideas** in [Discussions › Ideas](https://github.com/DuskXi/SpaceKnife/discussions/categories/ideas) or with a [feature request](https://github.com/DuskXi/SpaceKnife/issues/new?template=feature_request.yml).
- **Pick an issue.** Issues labelled [good first issue](https://github.com/DuskXi/SpaceKnife/labels/good%20first%20issue) are self-contained; comment on one before you start so nobody duplicates work.
- **Translate.** The interface has English and Simplified Chinese; see [docs/i18n.md](docs/i18n.md) for how texts are organised (in Chinese).
- **Ask questions** in [Discussions › Q&A](https://github.com/DuskXi/SpaceKnife/discussions/categories/q-a).

## Development setup

You need:

- [rustup](https://rustup.rs/): the first time cargo runs in the repository it installs the Rust version `rust-toolchain.toml` pins (edition 2024), with clippy and rustfmt; CI uses the same one;
- Node.js 24 and pnpm 10;
- on Linux, the libraries the Tauri app builds against: `libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev patchelf`; on Windows, the MSVC build tools and the WebView2 Runtime (Windows 11 has it); on macOS, the Xcode command line tools;
- only for the live parity run (below): Python 3.12 and [uv](https://docs.astral.sh/uv/).

```bash
cargo build
```

```bash
cd apps/desktop && pnpm install
```

```bash
pnpm tauri dev
```

The last command (in `apps/desktop`) starts the Tauri app; the backend (`crates/sk-server`) runs inside the app process. More commands (CLI scans with `spaceknife-cli`, demos, installers) are in the [README](README.md#development) and [docs/packaging.md](docs/packaging.md).

### Two shells, one interface

The interface in `apps/desktop/app` (Nuxt 4 + Vue 3) runs in two shells. It talks to its host only through `window.sk` (`app/shared/api.ts`), so it never needs to know which one it is in:

| Shell | Folder | Where it ships | The backend |
|---|---|---|---|
| Tauri 2 (the default) | `apps/desktop/src-tauri` | Windows installer and portables, macOS, the deb, the small Linux portable | inside the app (`sk_server::start`) |
| Electron (a fallback where Tauri can't run) | `apps/electron-shell` (main process and preload only) | the RHEL 9 rpm (no WebKitGTK 4.1 there), the AppImage, the Linux portable under 0.3.4's file name | the child process `spaceknife-cli serve --token-stdin` |

A change to the interface has to work in both. To try the Electron shell from a checkout:

```bash
cargo build -p sk-server --bin spaceknife-cli
pnpm -C apps/desktop generate
pnpm -C apps/electron-shell install
pnpm -C apps/electron-shell dev
```

It loads the built interface from `apps/desktop/.output/public` and starts `target/debug/spaceknife-cli` (or `target/release`; `SPACEKNIFE_CLI` names another one). Set `SPACEKNIFE_HOME` to an empty folder to keep your experiments away from your real snapshots and settings.

### The parity gate

The Rust backend speaks the same protocol as 0.3.4's Python backend, and `tests/parity` keeps it that way: the same scenarios run against both over the WebSocket protocol and their normalized answers are compared ([tests/parity/README.md](tests/parity/README.md)). Two ways to run it:

- **Replay** (part of `cargo test --workspace`): `cargo test -p sk-server --test parity_replay` compares `spaceknife-cli serve` with the answers recorded from 0.3.4 in `tests/parity/golden/<platform>/`. No Python needed.
- **Live**, against a worktree of `v0.3.4`: `uv run tests/parity/parity.py run --ref <0.3.4 worktree> --cand target/debug/spaceknife-cli`. Re-record the goldens with `record` on Windows and Linux after you change a scenario.

A difference is either a bug in the Rust backend (fix it, with a test that fails first) or a deliberate change, which goes into `tests/parity/known_diffs.json` with the reason.

## Before you open a pull request

Run the same checks as CI. Rust (the workspace, then the Windows console launcher `SpaceKnife.com`, a workspace of its own):

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

```bash
cargo clippy --manifest-path packaging/console-launcher/Cargo.toml --all-targets -- -D warnings && cargo test --manifest-path packaging/console-launcher/Cargo.toml
```

Generated files, the tools and the translations:

```bash
cargo run -p sk-protocol --bin gen-ts -- --check && node tools/gen_design.mjs --check && node tools/gen_pinyin.mjs --check
```

```bash
node --test "tools/tests/*.test.mjs" && node tools/i18n_check.mjs
```

The interface and the Electron shell:

```bash
cd apps/desktop && pnpm typecheck && pnpm test
```

```bash
cd apps/electron-shell && pnpm typecheck && pnpm test
```

If you changed the documentation or the website, also run `pnpm -C site build`. A bug fix comes with a test that fails without it.

Guidelines:

- **Match the surrounding code**: naming, comment density, idioms. Rust formatted with `cargo fmt` and clean under clippy; protocol messages are the serde types in `crates/sk-protocol` (the TypeScript side is generated with `cargo run -p sk-protocol --bin gen-ts`); logs use `tracing`. The interface is Vue 3 with TypeScript.
- **Keep pull requests focused**: one change per PR, with a short description of what and why, and tests for behaviour changes.
- **Texts are translated**: user-visible text goes through `t()` / `tr!` (Rust, catalogs in `crates/sk-core/locales/en`) or `t()` (interface), with the English entry in the catalogs; `node tools/i18n_check.mjs` finds what is missing.
- **Be gentle with disks**: tests and benchmarks must stay small and short; never scan or write large amounts of data on a contributor's real drives.
- **No personal data**: examples, tests, screenshots and logs use made-up paths and names (`C:\Users\lenny\…`, `/home/u/…`), never your own. README screenshots come from mock data (`node tools/demo/screenshots.mjs`).
- **Cross-platform**: Windows and Linux must keep working; mention it if you could only test one.

By contributing you agree that your contribution is licensed under the [MIT License](LICENSE). Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md). Security problems are reported privately, see [SECURITY.md](SECURITY.md).

---

## 参与贡献

感谢关注！SpaceKnife 是一个使用 MIT 许可证的小项目，任何形式的帮助都有价值：在你的电脑上试用、报告问题、翻译、写文档、写代码。

### 可以怎么帮忙

- **试用并报告问题**：现在尤其需要在干净的 Windows 10、RHEL 9、Ubuntu 22.04 或其他发行版上测试。请提交 [问题报告](https://github.com/DuskXi/SpaceKnife/issues/new?template=bug_report.yml)。
- **提建议**：在 [Discussions › Ideas](https://github.com/DuskXi/SpaceKnife/discussions/categories/ideas) 讨论，或提交 [功能建议](https://github.com/DuskXi/SpaceKnife/issues/new?template=feature_request.yml)。
- **认领 issue**：标着 [good first issue](https://github.com/DuskXi/SpaceKnife/labels/good%20first%20issue) 的问题相对独立；开始前先在下面留言，避免重复劳动。
- **翻译**：界面有英文和简体中文，文案的组织方式见 [docs/i18n.md](docs/i18n.md)。
- **提问**：在 [Discussions › Q&A](https://github.com/DuskXi/SpaceKnife/discussions/categories/q-a)。

### 开发环境

需要 [rustup](https://rustup.rs/)（第一次在仓库里运行 cargo 时，它会装上 `rust-toolchain.toml` 固定的 Rust 版本（edition 2024）和 clippy、rustfmt；CI 用的是同一个）、Node.js 24 和 pnpm 10；Linux 上 Tauri 应用还需要上方英文部分列出的几个系统库，Windows 上需要 MSVC 生成工具和 WebView2 Runtime（Windows 11 自带），macOS 上需要 Xcode 命令行工具；只有在线运行对等关卡（见下）时才需要 Python 3.12 和 [uv](https://docs.astral.sh/uv/)。安装与启动命令同上方英文部分（`cargo build`、在 `apps/desktop` 里 `pnpm install`、`pnpm tauri dev`；后端在应用进程内运行），更多命令见 [README](README.zh-CN.md#开发环境) 和 [docs/packaging.md](docs/packaging.md)。

**两个外壳，同一套界面**：`apps/desktop/app` 里的界面（Nuxt 4 + Vue 3）跑在两个外壳里，只经 `window.sk`（`app/shared/api.ts`）和宿主打交道，不需要知道自己在哪个外壳里。

| 外壳 | 位置 | 用在哪些发布文件 | 后端 |
|---|---|---|---|
| Tauri 2（默认） | `apps/desktop/src-tauri` | Windows 安装包和便携版、macOS、deb、小巧的 Linux 便携版 | 在应用进程里（`sk_server::start`） |
| Electron（只在 Tauri 跑不了的地方兜底） | `apps/electron-shell`（只有主进程和 preload） | RHEL 9 的 rpm（那里没有 WebKitGTK 4.1）、AppImage、沿用 0.3.4 文件名的 Linux 便携版 | 子进程 `spaceknife-cli serve --token-stdin` |

改界面要两个外壳都能用。从源码试 Electron 外壳：`cargo build -p sk-server --bin spaceknife-cli`，`pnpm -C apps/desktop generate`，然后 `pnpm -C apps/electron-shell install`、`pnpm -C apps/electron-shell dev`。它从 `apps/desktop/.output/public` 加载构建好的界面，启动 `target/debug/spaceknife-cli`（或 `target/release` 里的；`SPACEKNIFE_CLI` 可以另指一个）。把 `SPACEKNIFE_HOME` 设成一个空文件夹，试验就不会碰到你真实的快照和设置。

**对等关卡**：Rust 后端和 0.3.4 的 Python 后端说同一个协议，`tests/parity` 守着这一点：同样的场景经 WebSocket 协议分别跑在两个后端上，规范化后的回答逐项比较（[tests/parity/README.md](tests/parity/README.md)）。两种跑法：

- **回放**（包含在 `cargo test --workspace` 里）：`cargo test -p sk-server --test parity_replay` 拿 `spaceknife-cli serve` 和 `tests/parity/golden/<平台>/` 里从 0.3.4 录下的回答比较，不需要 Python。
- **在线**，对着 `v0.3.4` 的 worktree：`uv run tests/parity/parity.py run --ref <0.3.4 的 worktree> --cand target/debug/spaceknife-cli`。改了场景之后，在 Windows 和 Linux 上用 `record` 重新录金样。

出现差异时，要么是 Rust 后端的缺陷（修掉，并带一个修之前会失败的测试），要么是有意的改变，写进 `tests/parity/known_diffs.json` 并注明原因。

### 提交 PR 之前

运行与 CI 相同的检查（命令见上方英文部分）：Rust 工作区的 fmt、clippy、test，`packaging/console-launcher`（Windows 的 `SpaceKnife.com`，自成一个工作区）的 clippy 和 test，生成文件的检查（`gen-ts --check`、`gen_design.mjs --check`、`gen_pinyin.mjs --check`），`tools/tests` 和 `i18n_check.mjs`，`apps/desktop` 与 `apps/electron-shell` 各自的 `pnpm typecheck && pnpm test`；改了文档或官网时再跑 `pnpm -C site build`。修缺陷要带一个没有这个修复就会失败的测试。约定：

- **与周围代码保持一致**：命名、注释密度、写法。Rust 代码用 `cargo fmt` 格式化、clippy 无警告；协议消息是 `crates/sk-protocol` 里的 serde 类型（TypeScript 一侧用 `cargo run -p sk-protocol --bin gen-ts` 生成）；日志用 `tracing`。界面是 Vue 3 加 TypeScript。
- **PR 保持专注**：一个 PR 做一件事，简短说明做了什么、为什么；行为变化要带测试。
- **文案要翻译**：用户看得到的文字经过 `t()` / `tr!`（Rust，词表在 `crates/sk-core/locales/en`）或 `t()`（界面），并在词表里补英文；`node tools/i18n_check.mjs` 会找出遗漏。
- **爱护磁盘**：测试和基准要小而短，不要在贡献者真实的磁盘上大量扫描或写入。
- **不要放个人信息**：示例、测试、截图和日志里只用虚构的路径和名字（`C:\Users\lenny\…`、`/home/u/…`），不要用你自己的。README 截图来自模拟数据（`node tools/demo/screenshots.mjs`）。
- **跨平台**：Windows 和 Linux 都要能用；只能测试其中一个时请在 PR 里说明。

提交贡献即表示你同意以 [MIT 许可证](LICENSE) 授权你的贡献。所有参与者都遵守 [行为准则](CODE_OF_CONDUCT.md)。安全问题请私下报告，见 [SECURITY.md](SECURITY.md)。
