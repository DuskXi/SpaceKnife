---
title: 构建与参与开发
description: 从源码运行 SpaceKnife、运行与 CI 相同的检查、构建安装包，了解仓库结构和设计文档，以及怎样参与贡献。
---

SpaceKnife 是 GitHub 上一个使用 MIT 许可证的项目：[DuskXi/SpaceKnife](https://github.com/DuskXi/SpaceKnife)。任何形式的帮助都有价值：在你的电脑上试用并报告问题、翻译、写文档、写代码。

## 需要什么

- [uv](https://docs.astral.sh/uv/)：由它为整个工作区安装和管理 Python 3.12。
- Node.js 22 和 pnpm，用于 Electron 应用；官网需要 Node.js 22.12 或更新的版本。
- Rust 工具链，用于原生扩展（快速的目录遍历器和 MFT 读取）。开发时可以没有：所有原生路径都有 Python 回退，只是扫描慢好几倍。构建安装包时必须有。

## 从源码运行

```bash
git clone https://github.com/DuskXi/SpaceKnife.git
cd SpaceKnife
uv sync --group native
pnpm -C apps/electron install
pnpm -C apps/electron dev
```

- `uv sync --group native` 安装 Python 工作区并编译原生扩展；只写 `uv sync` 时不编译原生扩展。
- `pnpm -C apps/electron dev` 启动 Electron 应用，它用仓库里的环境（`.venv`）启动后端；`SPACEKNIFE_PYTHON` 可以指定别的解释器。
- 单独运行后端：`uv run spaceknife serve` 输出一行 `ready`，里面有 WebSocket 地址和开发用的令牌。在终端里扫描：`uv run spaceknife scan <路径>`（见[命令行](../command-line/)）。
- 把 `SPACEKNIFE_HOME` 设为一个空文件夹，试验就不会碰到你真实的快照和设置。

## 提交 PR 之前的检查

以下就是 CI 运行的检查。Python：

```bash
uv run ruff check .
uv run ruff format --check .
uv run mypy packages/protocol/src packages/backend/src packages/client/src tools/gen_design.py
uv run pytest
```

生成的文件必须是最新的：

```bash
uv run python tools/gen_design.py --check
uv run spaceknife schema --check packages/protocol/schema/protocol.schema.json
uv run python tools/gen_ts_protocol.py --check
uv run python tools/gen_docs.py --check
```

Electron 应用与翻译：

```bash
pnpm -C apps/electron typecheck
pnpm -C apps/electron test
uv run python tools/i18n_check.py
```

- CI 在 Windows、Ubuntu 22.04 和 macOS 14 上带着原生扩展运行 Python 检查，用 Node.js 22 运行 Electron 检查。
- `SPACEKNIFE_NO_NATIVE=1` 让后端使用 Python 回退；测试会比对原生和 Python 两条路径的结果。
- 测试和基准要小而短，不要在贡献者真实的磁盘上大量扫描或写入。

## 构建安装包

```bash
uv sync --locked --group native --group package
pnpm -C apps/electron install
uv run python tools/build_package.py
```

在哪个系统上运行就构建哪个系统的安装包，输出到 `dist/electron/`：Windows 出 NSIS 安装包；Linux 出 AppImage、deb 和 rpm；macOS 按本机架构出 dmg。

| 选项 | 作用 |
|---|---|
| `--portable` | 另外构建便携包（Windows zip、Linux tar.gz） |
| `--no-installers` | 只出免安装目录（加 `--portable` 时还有便携包） |
| `--skip-backend` | 只改了 Electron 时，复用 `dist/backend/` 里已冻结的后端 |
| `--targets AppImage deb rpm` | Linux：只构建其中几种格式 |

脚本依次执行：原生扩展的发布构建（去掉构建机的路径）；用 PyInstaller 冻结后端；冻结后端的冒烟测试（原生扩展必须能加载）；Linux 上检查所有文件需要的 glibc 不超过 2.34（RHEL 9）；`electron-vite build`；许可证文件；`electron-builder`；检查打包的文件里不含构建机的家目录；macOS 上检查 ad-hoc 签名。

- rpm 需要 `rpmbuild`；AppImage 的 `.zsync` 文件需要 `zsyncmake`（没有时仍会写入更新信息）。electron-builder 第一次运行时会下载它用的工具。
- 用 `uv run python packaging/smoke_app.py dist/electron/win-unpacked/SpaceKnife.exe`（或 `linux-unpacked/spaceknife`、AppImage）测试打包好的应用：它用临时的数据文件夹最小化启动应用，等后端就绪、启动页出现，再像用户一样关闭。

细节（包括发布文件的命名）见 [docs/packaging.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/packaging.md)。

## 官网与这些文档

```bash
pnpm -C site install
pnpm -C site dev
```

官网在 `site/`（Astro 加 Starlight），从 `main` 分支发布到 GitHub Pages。这些文档页都是 Markdown 文件：英文在 `site/src/content/docs/docs/<id>.md`，中文在 `site/src/content/docs/zh/docs/<id>.md`，顺序由 `site/src/docs-nav.json` 决定。0.3.2 起，软件内的帮助页用的也是这些文件。修改之前请先读 [docs/docs-writing.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/docs-writing.md)，里面列出了两边都能显示的 Markdown 写法。`<!-- generated:… -->` 标记之间的快捷键表和设置表由 `uv run python tools/gen_docs.py` 从 `palette/commands.py`、`settings.py` 生成，不要手改；改了命令或设置后重新运行它。`pnpm -C site build` 会检查每个链接指向的页面是否存在。

## 仓库结构

| 路径 | 内容 |
|---|---|
| `packages/protocol` | 协议：pydantic 消息模型、二进制帧编解码、JSON Schema |
| `packages/backend` | 后端：WebSocket 服务、扫描、按列存储与快照、过滤引擎、布局、视图、实时监控、导出、文件操作、命令行 |
| `packages/native` | Rust 原生扩展（PyO3）：并行遍历目录、解析 MFT |
| `packages/client` | Python 前端共用的客户端核心（不依赖 GUI） |
| `apps/electron` | Electron 应用及安装包配置（`electron-builder.yml`） |
| `design/tokens`、`design/icons` | 设计令牌与图标源文件；`design/build` 是生成的 |
| `packaging` | 冻结的后端（PyInstaller 配置与入口）和冒烟测试 |
| `site` | 官网和这份文档 |
| `docs` | 设计文档 |
| `tools` | 代码生成和开发工具：`build_package.py`、`gen_design.py`、`gen_ts_protocol.py`、`i18n_check.py`、`demo/screenshots.py` |

## 设计文档

设计文档说明每个部分怎么工作，以及各部分之间的接口约定。

| 文档 | 内容 |
|---|---|
| [PLAN.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/PLAN.md) | 开发计划：已定的决定、总体架构、各模块、里程碑与风险 |
| [UI-SPEC.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/UI-SPEC.md) | 界面需求规格，逐条列出 |
| [protocol.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/protocol.md) | 后端与前端之间的 WebSocket 协议 |
| [visuals.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/visuals.md) | 可视化：布局引擎、线上格式、绘制组件 |
| [plugins.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/plugins.md) | 为将来的插件系统总结的经验与设计要求（尚未实现） |
| [packaging.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/packaging.md) | 安装包、冻结的后端、数据位置、更新检查 |
| [memory.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/memory.md) | 内存模型：每项开销、映射存储、看门狗 |
| [i18n.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/i18n.md) | 界面文字怎样翻译 |
| [docs-writing.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/docs-writing.md) | 这些用户文档的写作约定 |
| [native.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/native.md) | 原生扩展、扫描助手与磁盘负载限速 |
| [monitor.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/monitor.md) | 实时监控（文件变化钩子） |
| [manage.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/manage.md) | 删除篮、快照对比、导出、洞察规则与设置导入导出 |
| [filter-syntax.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/filter-syntax.md) | 过滤语言的正式规范 |
| [export-templates.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/export-templates.md) | 导出模板语言、导出格式与 `spaceknife run` |
| [insights.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/insights.md) | 洞察规则的格式 |

修改各部分之间的接口时，先改对应的文档。

## 怎样参与

- **报告问题**：提交[问题报告](https://github.com/DuskXi/SpaceKnife/issues/new?template=bug_report.yml)，需要附上什么见[故障排除](../troubleshooting/#报告问题)。现在尤其需要在干净的 Windows 10、RHEL 9 或 Ubuntu 22.04 上的测试。
- **提建议**：在 [Discussions › Ideas](https://github.com/DuskXi/SpaceKnife/discussions/categories/ideas) 讨论，或提交[功能建议](https://github.com/DuskXi/SpaceKnife/issues/new?template=feature_request.yml)。
- **认领 issue**：标着 [good first issue](https://github.com/DuskXi/SpaceKnife/labels/good%20first%20issue) 的问题相对独立。开始前先留言，避免重复劳动。
- **翻译**：界面有英文和简体中文，见 [docs/i18n.md](https://github.com/DuskXi/SpaceKnife/blob/main/docs/i18n.md)。

提交 PR 时：

- 与周围代码保持一致：命名、注释密度、写法。Python 3.12 加类型标注（mypy strict）；协议消息用 pydantic；命令行输出用 rich，日志用 loguru。
- 一个 PR 做一件事，简短说明做了什么、为什么；行为变化要带测试。
- 用户看得到的文字经过后端的 `_()` / `tr()` 或前端的 `t()`，并在词表里补上英文。
- 不要放个人信息：示例、测试、截图和日志里只用虚构的路径和名字，例如 `C:\Users\lenny\…`、`/home/lenny/…`。
- Windows 和 Linux 都要能用；只能测试其中一个时请说明。

完整的说明见 [CONTRIBUTING.md](https://github.com/DuskXi/SpaceKnife/blob/main/CONTRIBUTING.md)。提交贡献即表示你同意以 [MIT 许可证](https://github.com/DuskXi/SpaceKnife/blob/main/LICENSE)授权你的贡献；所有参与者都遵守[行为准则](https://github.com/DuskXi/SpaceKnife/blob/main/CODE_OF_CONDUCT.md)。安全问题请私下报告，见 [SECURITY.md](https://github.com/DuskXi/SpaceKnife/blob/main/SECURITY.md)。
