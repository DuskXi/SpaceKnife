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

You need [uv](https://docs.astral.sh/uv/) (it manages Python 3.12), Node.js 22 with pnpm, and, for the native scanner, a Rust toolchain.

```bash
uv sync --group native
```

```bash
pnpm -C apps/electron install
```

```bash
pnpm -C apps/electron dev
```

The last command starts the Electron app; it launches the Python backend from the repository's environment. More commands (CLI scans, demos, benchmarks, installers) are in the [README](README.md#development) and [docs/packaging.md](docs/packaging.md).

## Before you open a pull request

Run the same checks as CI:

```bash
uv run ruff check . && uv run ruff format --check . && uv run mypy packages/protocol/src packages/backend/src packages/client/src tools/gen_design.py && uv run python tools/gen_design.py --check && uv run pytest
```

```bash
pnpm -C apps/electron typecheck && pnpm -C apps/electron test
```

```bash
uv run python tools/i18n_check.py
```

Guidelines:

- **Match the surrounding code**: naming, comment density, idioms. Python 3.12 with type hints (mypy strict); protocol messages are pydantic models; command-line output uses rich and logs use loguru.
- **Keep pull requests focused**: one change per PR, with a short description of what and why, and tests for behaviour changes.
- **Texts are translated**: user-visible text goes through `_()` / `tr()` (backend) or `t()` (frontend), with the English entry in the catalogs; `tools/i18n_check.py` finds what is missing.
- **Be gentle with disks**: tests and benchmarks must stay small and short; never scan or write large amounts of data on a contributor's real drives.
- **No personal data**: examples, tests, screenshots and logs use made-up paths and names (`C:\Users\lenny\…`, `/home/u/…`), never your own. README screenshots come from mock data (`tools/demo/screenshots.py`).
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

需要 [uv](https://docs.astral.sh/uv/)（由它管理 Python 3.12）、Node.js 22 和 pnpm；原生扫描器还需要 Rust 工具链。安装与启动命令同上方英文部分（`uv sync --group native`、`pnpm -C apps/electron install`、`pnpm -C apps/electron dev`），更多命令见 [README](README.zh-CN.md#开发环境) 和 [docs/packaging.md](docs/packaging.md)。

### 提交 PR 之前

运行与 CI 相同的检查（命令见上方英文部分）。约定：

- **与周围代码保持一致**：命名、注释密度、写法。Python 3.12 加类型标注（mypy strict）；协议消息用 pydantic；命令行输出用 rich，日志用 loguru。
- **PR 保持专注**：一个 PR 做一件事，简短说明做了什么、为什么；行为变化要带测试。
- **文案要翻译**：用户看得到的文字经过 `_()` / `tr()`（后端）或 `t()`（前端），并在词表里补英文；`tools/i18n_check.py` 会找出遗漏。
- **爱护磁盘**：测试和基准要小而短，不要在贡献者真实的磁盘上大量扫描或写入。
- **不要放个人信息**：示例、测试、截图和日志里只用虚构的路径和名字（`C:\Users\lenny\…`、`/home/u/…`），不要用你自己的。README 截图来自模拟数据（`tools/demo/screenshots.py`）。
- **跨平台**：Windows 和 Linux 都要能用；只能测试其中一个时请在 PR 里说明。

提交贡献即表示你同意以 [MIT 许可证](LICENSE) 授权你的贡献。所有参与者都遵守 [行为准则](CODE_OF_CONDUCT.md)。安全问题请私下报告，见 [SECURITY.md](SECURITY.md)。
