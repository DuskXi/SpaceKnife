# 打包与发布（M8）

Electron 版（UI v1）的安装包：Windows NSIS；Linux AppImage / deb / rpm；macOS dmg（Apple Silicon 与 Intel 各一个，ad-hoc 签名、未公证：Developer ID 签名与公证等 Apple 开发者账号下来再接上，见 [TODO.md](../TODO.md)）。

| 产物（`dist/electron/`） | 平台 | 大小（0.3.0） |
|---|---|---|
| `SpaceKnife-<版本>-Setup-x64.exe` | Windows 10+ x64，NSIS，未签名 | 135 MB |
| `SpaceKnife-<版本>-x86_64.AppImage` | Linux x64，免安装 | 164 MB |
| `spaceknife_<版本>_amd64.deb` | Ubuntu 22.04+ / Debian 系 | 131 MB |
| `spaceknife-<版本>.x86_64.rpm` | RHEL 9+ / Fedora 系 | 118 MB |
| `SpaceKnife-<版本>-mac-arm64.dmg` | macOS 14+，Apple Silicon，ad-hoc 签名、未公证 | 141 MB |
| `SpaceKnife-<版本>-mac-x64.dmg` | macOS 14+，Intel，ad-hoc 签名、未公证 | 148 MB |
| `SpaceKnife-<版本>-win-x64-portable.zip` | Windows 10+ x64 便携版（`--portable`） | 174 MB |
| `SpaceKnife-<版本>-linux-x64-portable.tar.gz` | Linux x64 便携版（`--portable`） | 161 MB |
| `win-unpacked/`、`linux-unpacked/`、`mac[-arm64]/SpaceKnife.app` | 免安装目录（冒烟测试、体验环境用） | 约 400 MB |

## 1. 构建

```bash
uv sync --locked --group native --group package
```

```bash
uv run python tools/build_package.py
```

在哪个系统上运行就出哪个系统的包（Windows 出 NSIS，Linux 出三种，macOS 按本机架构出 dmg：PyInstaller 冻结的是正在运行的解释器，交叉不了架构）。`--portable` 另外把免安装目录打成便携包（见第 4 节），`--no-installers` 只出免安装目录（和便携包）。便携包由「Portable build」工作流（[.github/workflows/portable.yml](../.github/workflows/portable.yml)）在 `windows-latest` 与 `ubuntu-22.04` 上构建，解压后用 `smoke_app.py --portable` 跑冒烟测试。两个打包工作流都可以手动触发，dev 分支上改动打包相关文件时也会自动构建。macOS 的两个 dmg 由 GitHub Actions 的「macOS build」工作流（[.github/workflows/macos.yml](../.github/workflows/macos.yml)，手动触发）分别在 `macos-15`（arm64）和 `macos-15-intel`（x64）上构建，dmg 与 SHA256 作为运行产物上传，再由维护者附到 Release 上。`--skip-backend` 复用已冻结的后端（只改了 Electron 时用），Linux 上 `--targets AppImage deb` 可以只出部分格式。

需要：uv（Python 3.12）、Rust 工具链（原生扩展必须打进包，没有它扫描会慢很多：构建前检查它能加载，冻结后的冒烟测试再查一次）、Node 与 pnpm（`apps/electron` 里先 `pnpm install`）。Linux 的 rpm 需要 `rpmbuild`；electron-builder 第一次运行会下载 Electron、NSIS、AppImage 工具与 fpm 到它的缓存目录。

脚本依次做：

0. **原生扩展的发布构建**：maturin 单独编译一份 wheel（`build/native-release`），用 `--remap-path-prefix` 把 Cargo、rustup 与仓库目录换成 `/cargo`、`/rustup`、`/spaceknife`，Windows 上 PDB 只记文件名（`/PDBALTPATH:%_PDB%`）：Rust 的 panic 信息和调试路径里会带上构建机的绝对路径（也就带上了用户名），Cargo 的 `trim-paths` 还没稳定。PyInstaller 用这一份，不用开发环境里的。
1. **冻结后端**：PyInstaller onedir（[packaging/spaceknife-backend.spec](../packaging/spaceknife-backend.spec)）→ `dist/backend/spaceknife-backend/`。Windows 约 67 MB，Linux 约 141 MB（没有 strip，见第 6 节）。
2. **后端冒烟测试**（[packaging/smoke_backend.py](../packaging/smoke_backend.py)）：`version`（原生扩展必须能加载：`· native 0.1.0`，不是 `none`）；用原生遍历和用 2 个工作进程各扫一次临时小目录；`serve --token-stdin` 握手、`system.info`、stdin 关闭后正常退出。
3. **glibc 下限**（只在 Linux）：包里每个 ELF 需要的 glibc 符号版本不超过 2.34（RHEL 9）。
4. `electron-vite build`；**许可证**（[tools/third_party_licenses.py](../tools/third_party_licenses.py) → `dist/licenses`）；然后 `electron-builder`（[apps/electron/electron-builder.yml](../apps/electron/electron-builder.yml)）。
5. **不带构建机路径**：冻结的后端和打包好的应用目录里，任何文件都不能出现构建机的家目录（两种斜杠、转义、UTF-8 / UTF-16、不分大小写），否则构建失败；zip 文件（`base_library.zip`）和后端可执行文件里压缩的 Python 模块（PYZ）也逐项解开检查。
6. **ad-hoc 签名**（只在 macOS）：`codesign --verify --deep --strict` 整个 `SpaceKnife.app` 通过，且是 ad-hoc 签名。Apple Silicon 不运行没有签名的代码；签名坏了的话，Gatekeeper 会说应用「已损坏」，而不是给出「仍要打开」。

**许可证**：SpaceKnife 是 MIT（仓库根目录 `LICENSE`）。安装后的 `resources/licenses/` 里有 `LICENSE.txt` 和三份第三方清单：`THIRD-PARTY-backend.txt`（Python 运行时与打进后端的包，含各包自带的许可证文件）、`THIRD-PARTY-native.txt`（原生扩展链接的 Rust crate）、`THIRD-PARTY-ui.txt`（打进界面的 JavaScript，由 `rollup-plugin-license` 生成，加上 Geist / Noto Sans SC 字体的 OFL）。Electron 自己的 `LICENSE.electron.txt` 与 `LICENSES.chromium.html` 在可执行文件旁边。

Linux 包在 WSL（Ubuntu 22.04）里构建：把工作区同步到 WSL 自己的文件系统再构建，不在 `/mnt/c` 上跑（9P 很慢）。同步时不要删掉编译好的 `*.so`：原生包是可编辑安装，编译产物就在源码目录里，被删了以后包里会悄悄没有原生扩展（现在构建前的检查会拦下）。没有 root 时，`rpmbuild` 可以用 `apt-get download` 下载 Ubuntu 官方的 rpm 包，`dpkg-deb -x` 解到用户目录，再设 `PATH`、`LD_LIBRARY_PATH` 和 `RPM_CONFIGDIR`。rpm 包头里会记下构建机的主机名（Build Host）：在 `RPM_CONFIGDIR/macros.d/` 里放一个定义 `%_buildhost localhost` 的宏文件。

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

**便携版**（Windows zip、Linux tar.gz）：

- 就是免安装目录，外加一个空的 `data` 文件夹（里面一份中英文说明）。打包后的 Windows / Linux 应用启动时，如果可执行文件旁边有 `data` 文件夹，就把它当作 `SPACEKNIFE_HOME`（[src/main/portable.ts](../apps/electron/src/main/portable.ts)）：快照、设置、日志、缓存和 Chromium 配置都存在里面，不写进用户目录。手动设的 `SPACEKNIFE_HOME` 优先；删掉或重命名 `data`，就改用正常的用户目录。
- 解压到用户可写的位置（解压到 Program Files 之类的位置会写不进去）。
- Linux 的 tar.gz 不需要 FUSE。和 AppImage 一样，`chrome-sandbox` 没有 setuid：在限制非特权用户命名空间的发行版上（Ubuntu 23.10 起的 AppArmor 规则）可能启动不了，需要加 `--no-sandbox`；目标系统 Ubuntu 22.04 和 RHEL 9 不受影响。
- 没有 32 位 Windows 版：Electron 44 已不再提供 win32-ia32。

**macOS（dmg）**：

- 打开 dmg，把 SpaceKnife 拖进「应用程序」。最低 macOS 14（`LSMinimumSystemVersion`；numpy 用的是需要 macOS 14 的 Accelerate 版本）。
- ad-hoc 签名、未公证：第一次打开会被 Gatekeeper 拦下，用户要去「系统设置 › 隐私与安全性」点「仍要打开」并输入密码，之后正常打开；每次更新后再来一次。macOS 14 上也可以在「应用程序」里右键 › 打开。终端里 `xattr -dr com.apple.quarantine /Applications/SpaceKnife.app` 也行。README 里有给用户的步骤。
- 关闭了 Hardened Runtime：它会强制库校验，ad-hoc 签名的应用会拒绝加载 Electron 框架原有的签名。等有 Developer ID 再开（公证需要它）。
- 「完全磁盘访问权限」：不给的话，扫描到「桌面」「文稿」「下载」、云盘、照片图库、其他应用的数据、外接盘和网络盘时会逐个弹出授权提示（提示文案是 `electron-builder.yml` 里的 `NS*UsageDescription`），邮件、信息等受保护目录读不到，计入未知空间。给了就都能扫到。应用在扫描前检查（`spaceknife.privacy`：读用户的 TCC 数据库，不弹窗），没有时弹出说明框：打开系统设置的对应页面 / 跳过这些位置（不弹窗，显示为无法访问）/ 照常逐个询问；设置「扫描范围 › 受保护的位置」记住选择。ad-hoc 签名的授权绑定在具体的构建上，更新后要在设置里关掉再打开一次。
- 扫描 `/` 时不进入 `/System/Volumes/Data`：同一 APFS 卷组的数据卷与系统卷报告同一个设备号，它的文件已经通过 firmlink 在树里（`scan.volume.volume_group_twins`）。云盘里「仅在云端」的文件（`SF_DATALESS`）按占用 0 计，这样的文件夹不进入，进程也设为不物化 dataless 文件，扫描不会触发下载。
- 磁盘负载在进程内读 IOKit 的块设备统计（与 `iostat` 同源，`ioreg` 为后备）。
- 目录条目（文件和文件夹）的属性用 `getattrlistbulk` 批量取，挂载点和 firmlink 仍逐个 `fstatat`。每个进程读到的第一批文件、第一批文件夹各自与 `fstatat` 对照：文件有差异就整条退回 readdir + `fstatat`，文件夹有差异只让文件夹退回。`SPACEKNIFE_NO_BULK=1` 全关，`=dirs` 只关文件夹，用来对比速度；`spaceknife-backend scan --profile` 打印耗时分解和它的状态。
- 列目录线程默认是 CPU 数的两倍（4–16），`SPACEKNIFE_SCAN_THREADS` 可以改。
- 「Scan benchmark」workflow 在 GitHub 的 macOS 运行器上对比各设置（各设置轮流、每轮换一个开头，热缓存跑 6 轮）。2026-10-19 扫 /System/Library /usr（45 万到 70 万项）的结果：
  - 3 核 arm64：两倍线程和文件夹批量取各快约 8%；在 300 万项上，8 线程比 3 线程快 27%。
  - 4 核 Intel：与改动前持平。
  - 两台运行器上，多线程只比单线程的 `find`、`du` 快约 20%；列目录线程 92–96% 的时间在系统调用里，瓶颈在内核侧。
  - `sudo purge` 清不掉 APFS 元数据的缓存，测不出冷缓存。
- 扫描是后端子进程做的，授权和提示都记在 SpaceKnife 这个应用名下（macOS 按负责的应用归属）。

**数据位置**（`SPACEKNIFE_HOME` 会把这些全部放进同一个目录）：

| | Windows | Linux | macOS |
|---|---|---|---|
| 快照、标签、日志（数据） | `%LOCALAPPDATA%\SpaceKnife` | `~/.local/share/SpaceKnife` | `~/Library/Application Support/SpaceKnife` |
| 设置、文件类别、洞察规则 | `%LOCALAPPDATA%\SpaceKnife` | `~/.config/SpaceKnife` | `~/Library/Application Support/SpaceKnife` |
| 缓存 | `%LOCALAPPDATA%\SpaceKnife\Cache` | `~/.cache/SpaceKnife` | `~/Library/Caches/SpaceKnife` |
| Chromium 配置 | `%APPDATA%\SpaceKnife\electron` | `~/.config/SpaceKnife/electron` | `~/Library/Application Support/SpaceKnife/electron` |

## 5. 测试

```bash
uv run python packaging/smoke_app.py dist/electron/win-unpacked/SpaceKnife.exe
```

[packaging/smoke_app.py](../packaging/smoke_app.py) 用临时目录作为 `SPACEKNIFE_HOME`、最小化启动应用（不抢焦点），打开 DevTools 端口：等打包的后端就绪、启动页列出卷，再像用户一样关窗口，确认应用和后端都退出。参数也可以是 `linux-unpacked/spaceknife` 或 AppImage。

已验证（2026-10-18）：Windows 11 上安装包本身（静默「仅当前用户」安装 → 安装后的应用与后端冒烟 → 静默卸载；卸载后注册表卸载项、`HKCU\Software` 下的键、开始菜单与桌面快捷方式、安装目录都和安装前一致，用户数据不变）和 `win-unpacked`；WSL（Ubuntu 22.04，WSLg）上 `linux-unpacked` 与 AppImage；deb / rpm 的元数据、依赖、desktop 文件与图标。还没做：在干净的 Windows 10、RHEL 9、Ubuntu 22.04 上安装测试（见 TODO）；deb / rpm 只检查了内容，没有实际安装。

## 6. 已知限制

- 未签名（Windows 代码签名证书待定）；macOS 只有 ad-hoc 签名、没有公证（等 Apple 开发者账号），也还没进 Homebrew（官方 cask 要求通过 Gatekeeper）。
- 项目主页：`https://github.com/DuskXi/SpaceKnife`。
- 没有 strip：PyInstaller 的 strip 会破坏 auditwheel 改过的库（numpy 自带的 openblas 报「not page-aligned」，冒烟测试会发现）。只 strip 没被改过的库（libpython、zstandard、原生扩展）大约能省 40 MB 解压后体积。
- 「仅当前用户」安装在用户可写的目录里；需要管理员权限的 MFT 快扫会从这里启动提权助手。介意的话选「所有用户」安装（Program Files）。
- 没有自动更新。
