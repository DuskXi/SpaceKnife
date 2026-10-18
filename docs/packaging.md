# 打包与发布（M8）

Electron 版（UI v1）的安装包：Windows NSIS；Linux AppImage / deb / rpm。macOS 打包、签名与公证暂不做（见 [TODO.md](../TODO.md)）。

| 产物（`dist/electron/`） | 平台 | 大小（0.3.0） |
|---|---|---|
| `SpaceKnife-<版本>-Setup-x64.exe` | Windows 10+ x64，NSIS，未签名 | 135 MB |
| `SpaceKnife-<版本>-x86_64.AppImage` | Linux x64，免安装 | 164 MB |
| `spaceknife_<版本>_amd64.deb` | Ubuntu 22.04+ / Debian 系 | 131 MB |
| `spaceknife-<版本>.x86_64.rpm` | RHEL 9+ / Fedora 系 | 118 MB |
| `win-unpacked/`、`linux-unpacked/` | 免安装目录（冒烟测试、体验环境用） | 约 400 MB |

## 1. 构建

```bash
uv sync --locked --group native --group package
```

```bash
uv run python tools/build_package.py
```

在哪个系统上运行就出哪个系统的包（Windows 出 NSIS，Linux 出三种）。`--skip-backend` 复用已冻结的后端（只改了 Electron 时用），Linux 上 `--targets AppImage deb` 可以只出部分格式。

需要：uv（Python 3.12）、Rust 工具链（原生扩展必须打进包，没有它扫描会慢很多：构建前检查它能加载，冻结后的冒烟测试再查一次）、Node 与 pnpm（`apps/electron` 里先 `pnpm install`）。Linux 的 rpm 需要 `rpmbuild`；electron-builder 第一次运行会下载 Electron、NSIS、AppImage 工具与 fpm 到它的缓存目录。

脚本依次做：

1. **冻结后端**：PyInstaller onedir（[packaging/spaceknife-backend.spec](../packaging/spaceknife-backend.spec)）→ `dist/backend/spaceknife-backend/`。Windows 约 67 MB，Linux 约 141 MB（没有 strip，见第 6 节）。
2. **后端冒烟测试**（[packaging/smoke_backend.py](../packaging/smoke_backend.py)）：`version`（原生扩展必须能加载：`· native 0.1.0`，不是 `none`）；用原生遍历和用 2 个工作进程各扫一次临时小目录；`serve --token-stdin` 握手、`system.info`、stdin 关闭后正常退出。
3. **glibc 下限**（只在 Linux）：包里每个 ELF 需要的 glibc 符号版本不超过 2.34（RHEL 9）。
4. `electron-vite build`，然后 `electron-builder`（[apps/electron/electron-builder.yml](../apps/electron/electron-builder.yml)）。

Linux 包在 WSL（Ubuntu 22.04）里构建：把工作区同步到 WSL 自己的文件系统再构建，不在 `/mnt/c` 上跑（9P 很慢）。同步时不要删掉编译好的 `*.so`：原生包是可编辑安装，编译产物就在源码目录里，被删了以后包里会悄悄没有原生扩展（现在构建前的检查会拦下）。没有 root 时，`rpmbuild` 可以用 `apt-get download` 下载 Ubuntu 官方的 rpm 包，`dpkg-deb -x` 解到用户目录，再设 `PATH`、`LD_LIBRARY_PATH` 和 `RPM_CONFIGDIR`。

## 2. 冻结的后端

- 入口 [packaging/backend_entry.py](../packaging/backend_entry.py)：`spaceknife-backend serve …` 就是 `python -m spaceknife serve …`；`spaceknife-backend --helper …` 是扫描助手（冻结后没有 `-m`，`helper_command()` 在冻结时返回 `[自身, "--helper"]`）；入口先调用 `multiprocessing.freeze_support()`，spawn 方式的扫描工作进程会重新进入这个可执行文件。
- 以 `-X utf8` 冻结（spec 里的解释器选项），日志和 ready 行在所有平台都是 UTF-8。冻结的解释器只从自己的包里导入，不读 `PYTHON*` 环境变量，所以提权助手不需要 `-I`。
- 收进包里的数据：`locales/*.json`、`insights/builtin.toml`、`export/builtin/*.sktpl`；`spaceknife*` 的所有子模块都按隐式导入收进来（请求处理器、各平台后端、MFT 读取器是按需导入的）。排除开发工具（pytest、hypothesis、mypy；`pydantic.mypy` 会把 mypy 整个拉进来）。
- Windows 上带图标和版本信息（任务管理器里显示「SpaceKnife backend」）。
- Linux 上不带构建机的 `libgcc_s.so.1` 和 `libstdc++.so.6`：目标系统都有（glibc 的 `pthread_cancel` 需要 libgcc_s，apt / dnf 需要 libstdc++），manylinux 的 wheel 也这样假设；而 Ubuntu 22.04 的 libgcc_s（GCC 12）需要 `GLIBC_2.35`，打进去在 RHEL 9 上会加载失败。剩下的部分最高需要 `GLIBC_2.34`（原生扩展在 glibc 2.35 上编译，pthread 符号绑定到 2.34 版本），正好是 RHEL 9 的下限；Electron 自身最高 `GLIBC_2.25`。
- 安装目录本身是受保护路径（不能在应用里删除或移动它）。
- 命令行也能直接用：安装目录下 `resources/backend/spaceknife-backend scan …`、`run …`、`snapshot …`。

## 3. Electron

- 打包后主进程运行 `process.resourcesPath/backend/spaceknife-backend[.exe] serve --token-stdin …`（`bundledBackend()`，[src/main/backend.ts](../apps/electron/src/main/backend.ts)）；从源码运行时仍是仓库 uv 环境里的 `python -m spaceknife`。
- `appId` 是 `io.github.duskxi.spaceknife`（Windows 上也是 AppUserModelID：任务栏分组、固定的快捷方式）。
- Chromium 配置目录（本地存储、缓存）：`<appData>/SpaceKnife/electron`，和后端的文件分开（Linux 上后端的配置目录也是 `~/.config/SpaceKnife`）；设了 `SPACEKNIFE_HOME` 时是 `$SPACEKNIFE_HOME/electron`。
- 只保留界面的两种语言的 Chromium 语言包（`electronLanguages: en-US, zh-CN`）。
- 不带 `node_modules`：渲染进程用到的库都由 Vite 打进 `out/`，`package.json` 里全是 devDependencies。
- 窗口图标：Linux 用 `resources/icon.png`；Windows 打包后用 exe 自带的图标。

## 4. 安装包

**Windows（NSIS）**：引导式安装（不是一键安装），可选「仅当前用户」（默认，不需要管理员）或「所有用户」，可改安装目录；开始菜单与桌面快捷方式；安装向导跟随系统语言（中文 / 英文）；卸载时保留扫描、快照与设置。没有代码签名证书：第一次运行时 SmartScreen 会提示「未知发布者」。

静默安装与卸载（命令行部署、测试）：`SpaceKnife-<版本>-Setup-x64.exe /S /currentuser`（或 `/allusers`，需要管理员）；卸载用注册表卸载项里的 `QuietUninstallString`（`"Uninstall SpaceKnife.exe" /currentuser /S`），卸载程序会复制到临时目录再运行，命令本身立即返回。electron-builder 的安装程序总会把自己复制一份到 `%LOCALAPPDATA%\spaceknife-electron-updater`（给自动更新用，135 MB），卸载时也不删；SpaceKnife 没有自动更新，[build/installer.nsh](../apps/electron/build/installer.nsh) 在安装完和卸载时把它删掉。

**Linux**：

- deb / rpm：程序在 `/opt/SpaceKnife/`，`/usr/bin/spaceknife` 指向它；`spaceknife.desktop`（中英文名称与说明、关键字）和 hicolor 图标 16–512 px；依赖是 Electron 的常规依赖（GTK 3、NSS、libXss、libXtst、xdg-utils、at-spi2 等）。
- AppImage：需要 FUSE 2（Ubuntu 22.04 上是 `libfuse2`）；没有时用 `--appimage-extract-and-run` 运行。
- 窗口的 app_id / WM_CLASS 与 desktop 文件名一致（`desktopName: spaceknife.desktop`），任务栏能把窗口和启动器图标对上。

**数据位置**（`SPACEKNIFE_HOME` 会把这些全部放进同一个目录）：

| | Windows | Linux |
|---|---|---|
| 快照、标签、日志（数据） | `%LOCALAPPDATA%\SpaceKnife` | `~/.local/share/SpaceKnife` |
| 设置、文件类别、洞察规则 | `%LOCALAPPDATA%\SpaceKnife` | `~/.config/SpaceKnife` |
| 缓存 | `%LOCALAPPDATA%\SpaceKnife\Cache` | `~/.cache/SpaceKnife` |
| Chromium 配置 | `%APPDATA%\SpaceKnife\electron` | `~/.config/SpaceKnife/electron` |

## 5. 测试

```bash
uv run python packaging/smoke_app.py dist/electron/win-unpacked/SpaceKnife.exe
```

[packaging/smoke_app.py](../packaging/smoke_app.py) 用临时目录作为 `SPACEKNIFE_HOME`、最小化启动应用（不抢焦点），打开 DevTools 端口：等打包的后端就绪、启动页列出卷，再像用户一样关窗口，确认应用和后端都退出。参数也可以是 `linux-unpacked/spaceknife` 或 AppImage。

已验证（2026-10-18）：Windows 11 上安装包本身（静默「仅当前用户」安装 → 安装后的应用与后端冒烟 → 静默卸载；卸载后注册表卸载项、`HKCU\Software` 下的键、开始菜单与桌面快捷方式、安装目录都和安装前一致，用户数据不变）和 `win-unpacked`；WSL（Ubuntu 22.04，WSLg）上 `linux-unpacked` 与 AppImage；deb / rpm 的元数据、依赖、desktop 文件与图标。还没做：在干净的 Windows 10、RHEL 9、Ubuntu 22.04 上安装测试（见 TODO）；deb / rpm 只检查了内容，没有实际安装。

## 6. 已知限制

- 未签名（Windows 代码签名证书待定）。
- 还没有选定许可证：deb / rpm 里写的是 `unknown`；第三方许可清单只有 Electron 自带的 `LICENSES.chromium.html`。
- 项目主页暂定为 `https://github.com/DuskXi/SpaceKnife`（deb / rpm 必填；仓库还没推送）。
- 没有 strip：PyInstaller 的 strip 会破坏 auditwheel 改过的库（numpy 自带的 openblas 报「not page-aligned」，冒烟测试会发现）。只 strip 没被改过的库（libpython、zstandard、原生扩展）大约能省 40 MB 解压后体积。
- 「仅当前用户」安装在用户可写的目录里；需要管理员权限的 MFT 快扫会从这里启动提权助手。介意的话选「所有用户」安装（Program Files）。
- 没有自动更新。
