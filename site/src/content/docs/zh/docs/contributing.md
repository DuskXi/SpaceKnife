---
title: 构建与参与开发
description: 从源码运行 SpaceKnife、运行与 CI 相同的检查、构建安装包，了解仓库结构和设计文档，以及怎样参与贡献。
---

SpaceKnife 是 GitHub 上一个使用 MIT 许可证的项目：[DuskXi/SpaceKnife](https://github.com/DuskXi/SpaceKnife)。任何形式的帮助都有价值：在你的电脑上试用并报告问题、翻译、写文档、写代码。

## 需要什么

- [rustup](https://rustup.rs/)，用于后端、命令行和应用的原生部分：第一次在仓库里运行 cargo 时，它会装上 `rust-toolchain.toml` 固定的 Rust 版本（edition 2024，带 clippy 和 rustfmt），和 CI 用的是同一个。
- Node.js 24 和 pnpm 10，用于应用的界面（Nuxt 4 + Vue 3）和它的两个外壳：Tauri 2 与 Electron；官网需要 Node.js 22.12 或更新的版本。
- Linux 上还要 Tauri 编译时用到的库：`libwebkit2gtk-4.1-dev`、`libgtk-3-dev`、`libayatana-appindicator3-dev`、`librsvg2-dev`、`libssl-dev` 和 `patchelf`（Ubuntu / Debian 的包名）。Windows 上需要 Microsoft Edge WebView2（Windows 11 自带）和 MSVC 生成工具；macOS 上需要 Xcode 命令行工具。

## 从源码运行

```bash
git clone https://github.com/DuskXi/SpaceKnife.git
cd SpaceKnife
cargo build
cd apps/desktop
pnpm install
pnpm tauri dev
```

- `pnpm tauri dev` 编译并启动应用。后端在应用的进程里运行（`sk_server::start`），不需要另外启动程序。
- `pnpm dev` 只在浏览器里提供界面（`http://localhost:3000`），没有后端。
- 单独运行后端：`cargo run -p sk-server --bin spaceknife-cli -- serve` 输出一行 `ready`，里面有 WebSocket 地址和令牌。在终端里扫描：`cargo run -p sk-server --bin spaceknife-cli -- scan <路径>`（见[命令行](../command-line/)）；想看真实的速度就加 `--release`。
- 同一套界面也跑在 Electron 外壳（`apps/electron-shell`）里：RHEL 9 没有 WebKitGTK 4.1，所以 rpm、AppImage 和自带引擎的 Linux 便携版用它。试用：`cargo build -p sk-server --bin spaceknife-cli`，`pnpm -C apps/desktop generate`，然后 `pnpm -C apps/electron-shell install`、`pnpm -C apps/electron-shell dev`。它的后端是子进程 `spaceknife-cli serve`。界面只经 `window.sk`（`app/shared/api.ts`）和外壳打交道，改界面要两个外壳都能用。
- 把 `SPACEKNIFE_HOME` 设为一个空文件夹，试验就不会碰到你真实的快照和设置。

## 提交 PR 之前的检查

以下就是 CI 运行的检查。Rust：

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

生成的文件必须是最新的：

```bash
node tools/gen_design.mjs --check
cargo run -p sk-protocol --bin gen-ts -- --check
```

Windows 的控制台启动器 `SpaceKnife.com` 自成一个小工作区：

```bash
cargo clippy --manifest-path packaging/console-launcher/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path packaging/console-launcher/Cargo.toml
```

界面、Electron 外壳、工具与翻译：

```bash
pnpm -C apps/desktop typecheck
pnpm -C apps/desktop test
pnpm -C apps/electron-shell typecheck
pnpm -C apps/electron-shell test
node --test "tools/tests/*.test.mjs"
node tools/i18n_check.mjs
```

- `cargo test --workspace` 里包含对等关卡的回放（`cargo test -p sk-server --test parity_replay`）：后端经协议给出的回答，和 0.3.4 的 Python 后端对同样场景的回答比较。对着 `v0.3.4` 的 worktree 在线比较需要 Python 3.12 和 uv；怎么跑、怎么重新录下回答，见 [tests/parity/README.md](https://github.com/DuskXi/SpaceKnife/blob/main/tests/parity/README.md)。
- 修缺陷要带一个没有这个修复就会失败的测试。
- 测试和基准要小而短，不要在贡献者真实的磁盘上大量扫描或写入。

## 构建安装包

```bash
cd apps/desktop
pnpm install
pnpm bundle
pnpm bundle:portable
```

`pnpm bundle` 用 Tauri 构建当前系统的安装包，输出到 `target/release/bundle/`：Windows 出 NSIS 安装包；Linux 出 AppImage、deb 和 rpm；macOS 出 app 和 dmg。Tauri 运行之前，`apps/desktop/scripts/prepare-bundle.mjs` 先构建 `spaceknife-cli`（装在应用旁边）并写好许可证文件。`pnpm bundle:portable` 在 `pnpm bundle` 之后把便携包打进 `target/portable/`。发布时，Windows 安装包、deb、macOS 的 app 和 dmg 以及便携版用 Tauri 的构建；rpm、AppImage 和沿用 0.3.4 文件名的 Linux 便携版则来自 Electron 外壳：`pnpm -C apps/electron-shell package` 把它们构建到 `target/electron-shell/dist`。

- 用 `node packaging/smoke-app.mjs` 测试打包好的应用（两个外壳都行），`node packaging/smoke-headless.mjs` 测试无头模式，`node packaging/update-e2e.mjs` 测试便携版的更新。`node packaging/update-from-034.mjs` 把真实的 0.3.4 更新到一个构建，CI 对每种安装方式都跑一遍。
- 发布文件的名字是和每一份已安装副本的约定：0.3.4 只按这些确切的名字找自己的文件，所以它们永远不改。

细节（包括发布文件的命名）见 [docs/packaging.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/packaging.md)。

## 官网与这些文档

```bash
pnpm -C site install
pnpm -C site dev
```

官网在 `site/`（Astro 加 Starlight），从 `main` 分支发布到 GitHub Pages。这些文档页都是 Markdown 文件：英文在 `site/src/content/docs/docs/<id>.md`，中文在 `site/src/content/docs/zh/docs/<id>.md`，顺序由 `site/src/docs-nav.json` 决定。0.3.2 起，软件内的帮助页用的也是这些文件。修改之前请先读 [docs/docs-writing.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/docs-writing.md)，里面列出了两边都能显示的 Markdown 写法。`<!-- generated:… -->` 标记之间的快捷键表和设置表对应命令列表（`crates/sk-core/src/palette/commands.rs`）和设置注册表（`crates/sk-core/src/settings/registry.rs`）；在有从 Rust 代码生成它们的工具之前，这两张表手工维护：改命令或设置时一起改。`pnpm -C site build` 会检查每个链接指向的页面是否存在。

## 仓库结构

| 路径 | 内容 |
|---|---|
| `crates/sk-protocol` | 协议：消息类型、二进制帧编解码，以及由它生成的 TypeScript 类型和 JSON Schema |
| `crates/sk-store` | 扫描结果的按列存储与快照文件 |
| `crates/sk-native` | 并行遍历目录、解析 MFT |
| `crates/sk-core` | 扫描、过滤引擎、布局、视图、实时监控、导出、文件操作、设置、更新 |
| `crates/sk-server` | WebSocket 服务和 `spaceknife-cli` 命令行 |
| `apps/desktop` | 应用：界面（`app/`，Nuxt + Vue）和 Tauri 外壳及打包配置（`src-tauri/`） |
| `apps/electron-shell` | Electron 外壳：主进程和 preload，没有自己的界面 |
| `design/tokens`、`design/icons` | 设计令牌与图标源文件；`design/build` 是生成的 |
| `packaging` | 打包后应用的测试（冒烟、无头模式、更新，包括从 0.3.4 更新），以及 Windows 的控制台启动器 `SpaceKnife.com` |
| `tests/parity` | 对等关卡：同一批协议场景分别跑在 0.3.4 的后端和 Rust 后端上 |
| `site` | 官网和这份文档 |
| `docs` | 设计文档 |
| `tools` | 代码生成和开发工具：`gen_design.mjs`、`gen_pinyin.mjs`、`i18n_check.mjs`、`release.mjs`、`demo/screenshots.mjs` |

## 设计文档

设计文档说明每个部分怎么工作，以及各部分之间的接口约定。

| 文档 | 内容 |
|---|---|
| [PLAN.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/PLAN.md) | 开发计划：已定的决定、总体架构、各模块、里程碑与风险 |
| [UI-SPEC.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/UI-SPEC.md) | 界面需求规格，逐条列出 |
| [protocol.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/protocol.md) | 后端与前端之间的 WebSocket 协议 |
| [visuals.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/visuals.md) | 可视化：布局引擎、线上格式、绘制组件 |
| [plugins.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/plugins.md) | 为将来的插件系统总结的经验与设计要求（尚未实现） |
| [packaging.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/packaging.md) | 安装包、便携包、数据位置、更新检查 |
| [memory.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/memory.md) | 内存模型：每项开销、映射存储、看门狗 |
| [i18n.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/i18n.md) | 界面文字怎样翻译 |
| [docs-writing.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/docs-writing.md) | 这些用户文档的写作约定 |
| [native.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/native.md) | 原生扫描代码、扫描助手与磁盘负载限速 |
| [monitor.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/monitor.md) | 实时监控（文件变化钩子） |
| [manage.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/manage.md) | 删除篮、快照对比、导出、洞察规则与设置导入导出 |
| [filter-syntax.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/filter-syntax.md) | 过滤语言的正式规范 |
| [export-templates.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/export-templates.md) | 导出模板语言、导出格式与 `spaceknife-cli run` |
| [insights.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/insights.md) | 洞察规则的格式 |

修改各部分之间的接口时，先改对应的文档。

## 怎样参与

- **报告问题**：提交[问题报告](https://github.com/DuskXi/SpaceKnife/issues/new?template=bug_report.yml)，需要附上什么见[故障排除](../troubleshooting/#报告问题)。现在尤其需要在干净的 Windows 10、RHEL 9 或 Ubuntu 22.04 上的测试。
- **提建议**：在 [Discussions › Ideas](https://github.com/DuskXi/SpaceKnife/discussions/categories/ideas) 讨论，或提交[功能建议](https://github.com/DuskXi/SpaceKnife/issues/new?template=feature_request.yml)。
- **认领 issue**：标着 [good first issue](https://github.com/DuskXi/SpaceKnife/labels/good%20first%20issue) 的问题相对独立。开始前先留言，避免重复劳动。
- **翻译**：界面有英文和简体中文，见 [docs/i18n.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/i18n.md)。

提交 PR 时：

- 与周围代码保持一致：命名、注释密度、写法。Rust 代码要通过 `cargo fmt` 和 `cargo clippy -D warnings`；协议消息是 `sk-protocol` 里的类型；日志用 `tracing`。界面用 TypeScript 和 Vue 的 `<script setup>`。
- 一个 PR 做一件事，简短说明做了什么、为什么；行为变化要带测试。
- 用户看得到的文字经过后端的 `t()` / `tr!` 或界面的 `t()`，并在词表里补上英文。
- 不要放个人信息：示例、测试、截图和日志里只用虚构的路径和名字，例如 `C:\Users\lenny\…`、`/home/lenny/…`。
- Windows 和 Linux 都要能用；只能测试其中一个时请说明。

完整的说明见 [CONTRIBUTING.md](https://github.com/DuskXi/SpaceKnife/blob/main/CONTRIBUTING.md)。提交贡献即表示你同意以 [MIT 许可证](https://github.com/DuskXi/SpaceKnife/blob/main/LICENSE)授权你的贡献；所有参与者都遵守[行为准则](https://github.com/DuskXi/SpaceKnife/blob/main/CODE_OF_CONDUCT.md)。安全问题请私下报告，见 [SECURITY.md](https://github.com/DuskXi/SpaceKnife/blob/main/SECURITY.md)。
