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
| `SpaceKnife-<版本>-win-arm64-portable.zip`、`SpaceKnife-<版本>-linux-arm64-portable.tar.gz` | Windows 11 ARM 版、ARM Linux 的便携版（在 arm64 机器上 `--portable --no-installers`） | — |
| `SpaceKnife-<版本>-mac-arm64-portable.zip`、`SpaceKnife-<版本>-mac-x64-portable.zip` | macOS 14+ 便携版：`SpaceKnife.app` 旁边一个 `data` 文件夹（`--portable`，ditto 打包） | — |
| `win[-arm64]-unpacked/`、`linux[-arm64]-unpacked/`、`mac[-arm64]/SpaceKnife.app` | 免安装目录（冒烟测试、体验环境用） | 约 400 MB |

## 1. 构建

```bash
uv sync --locked --group native --group package
```

```bash
uv run python tools/build_package.py
```

在哪个系统上运行就出哪个系统的包（Windows 出 NSIS，Linux 出三种，macOS 按本机架构出 dmg：PyInstaller 冻结的是正在运行的解释器，交叉不了架构）。`--portable` 另外把免安装目录打成便携包（见第 4 节），`--no-installers` 只出免安装目录（和便携包）。一律为构建机自己的架构打包（`tools/build_package.py` 的 `build_arch`，看运行它的 Python）：x64，或在 ARM 机器上 arm64；Windows 和 Linux 的安装包只出 x64，arm64 只出免安装目录和便携包（要安装包时直接报错）。便携包由「Portable build」工作流（[.github/workflows/portable.yml](../.github/workflows/portable.yml)）在 `windows-latest`、`windows-11-arm`、`ubuntu-22.04` 与 `ubuntu-22.04-arm` 上构建（x64 和 arm64 各一份；Windows on ARM 的运行器没有 Rust，工作流先装 rustup，ARM Linux 上先补 Electron 要的库），解压后用 `smoke_app.py --portable` 跑冒烟测试，再测一遍便携版的「无缝更新」；NSIS 安装包的升级与卸载测试只在 x64 上跑。两个打包工作流都可以手动触发，dev 分支上改动打包相关文件时也会自动构建。macOS 的两个 dmg 和两个便携 zip 由 GitHub Actions 的「macOS build」工作流（[.github/workflows/macos.yml](../.github/workflows/macos.yml)，手动触发）分别在 `macos-15`（arm64）和 `macos-15-intel`（x64）上构建；便携 zip 用 ditto 解开后校验签名、以便携模式跑冒烟测试、再测一遍「无缝更新」，dmg、zip 与 SHA256 作为运行产物上传，再由维护者附到 Release 上。`--skip-backend` 复用已冻结的后端（只改了 Electron 时用），Linux 上 `--targets AppImage deb` 可以只出部分格式。

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
- 收进包里的数据：`locales/*.json`、`insights/builtin.toml`、`export/builtin/*.sktpl`；`spaceknife*` 的所有子模块都按隐式导入收进来（请求处理器、各平台后端、MFT 读取器是按需导入的）。排除开发工具（pytest、hypothesis、mypy；`pydantic.mypy` 会把 mypy 整个拉进来）和 setuptools（见下面「命令行扫描的启动」）。
- Windows 上带图标和版本信息（任务管理器里显示「SpaceKnife backend」）。
- Linux 上不带构建机的 `libgcc_s.so.1` 和 `libstdc++.so.6`：目标系统都有（glibc 的 `pthread_cancel` 需要 libgcc_s，apt / dnf 需要 libstdc++），manylinux 的 wheel 也这样假设；而 Ubuntu 22.04 的 libgcc_s（GCC 12）需要 `GLIBC_2.35`，打进去在 RHEL 9 上会加载失败。剩下的部分最高需要 `GLIBC_2.34`（原生扩展在 glibc 2.35 上编译，pthread 符号绑定到 2.34 版本），正好是 RHEL 9 的下限；Electron 自身最高 `GLIBC_2.25`。
- 安装目录本身是受保护路径（不能在应用里删除或移动它）。
- 命令行也能直接用：安装目录下 `resources/backend/spaceknife-backend scan …`、`run …`、`snapshot …`。

### 命令行扫描的启动

`spaceknife-backend scan <路径> --no-snapshot …`（基准和脚本都这样用）的用时包括进程的启动和退出；目录小的时候它们比扫描本身还长。为此：

- 冻结后端不收 setuptools（spec 的 `excludes`）。它是构建工具，运行时没有代码导入它；但只要收进来，PyInstaller 就会带上运行时钩子 `pyi_rth_setuptools`，每次启动都导入 setuptools 和它自带的 jaraco、packaging 等包（冻结版加 `-X importtime` 在 Windows 上量到 130–230 ms）。可以用 PyInstaller 的 `CArchiveReader` 看可执行文件的目录：不该有 `pyi_rth_setuptools`。
- `scan` 只导入它用得到的模块：`spaceknife.io` 的名字按需加载子模块（取 `io.priority` 不会带出调速器和协议的 pydantic 模型），`--disk-limit` 时才导入 `IoRegistry`；快照（文件头是 pydantic 模型）只在保存、列出、打开快照时导入；导出模板、`rich.progress`、`LogHub` 也在用到时才导入。界面语言：没有设置文件、文件读不了或不是 JSON、或者文件里的语言就是 `system` 时，`SettingsStore` 的结果一定是 `system`，直接用它；设了别的语言才用 `SettingsStore` 校验整个文件（`cli_scan.stored_language`，测试逐种情况与 `SettingsStore` 对比）。[test_cli_startup.py](../packages/backend/tests/test_cli_startup.py) 在新的解释器里跑一次 `scan --no-snapshot`，检查 pydantic、协议、设置、调速器、快照、导出、服务器和 `rich.progress` 都没有被导入：加了新的模块级导入时它会报出来。asyncio 去不掉：loguru 自己就导入它。
- 输出不是终端（重定向）时不建进度条：rich 这时本来什么都不画，只在停止时打一个空行；这个空行照样打，输出与原来逐字节一致。
- 输出写完（自动快照也已保存）后，`cli.main` 先 flush 标准输出和标准错误，再 `os._exit`，跳过解释器的收尾（等空闲的列目录线程、逐个释放节点表、清理模块）。只用于 `scan`，退出码不变；Python 遍历器的工作进程还没退完时照常退出。
- 「最大的文件」用 `store.subtree.largest` 的部分选择（第 k 大的值，并列时编号小的在前），不对全部文件排序；行和顺序与整体排序相同（130 万项时 35 ms → 7 ms）。
- 实测（i9-14900K，冻结版，热缓存，扫空文件夹，整个进程的墙钟，每臂 7 轮的中位数）：Windows 全部 CPU 408 → 219 ms、4 个 E 核 701 → 377 ms；WSL（Linux）全部 CPU 236 → 120 ms、4 CPU 228 → 120 ms。其中退出（最后一个字节到进程结束）Windows 28 → 3 ms，Linux 26 → 1 ms。Windows 上扫真实的文件夹（全部 CPU，每臂 5 轮）：34.8 万项的 NTFS 程序文件夹 906 → 666 ms，136 万项的 ReFS 文件夹 964 → 743 ms，130 万项的 NTFS 文件夹 2.82 → 2.62 s；打印 21–45 → 12–23 ms，退出 39–47 → 6–10 ms。

## 3. Electron

- 打包后主进程运行 `process.resourcesPath/backend/spaceknife-backend[.exe] serve --token-stdin …`（`bundledBackend()`，[src/main/backend.ts](../apps/electron/src/main/backend.ts)）；从源码运行时仍是仓库 uv 环境里的 `python -m spaceknife`。
- `appId` 是 `io.github.duskxi.spaceknife`（Windows 上也是 AppUserModelID：任务栏分组、固定的快捷方式）。
- Chromium 配置目录（本地存储、缓存）：`<appData>/SpaceKnife/electron`，和后端的文件分开（Linux 上后端的配置目录也是 `~/.config/SpaceKnife`）；设了 `SPACEKNIFE_HOME` 时是 `$SPACEKNIFE_HOME/electron`。
- 只保留界面的两种语言的 Chromium 语言包（`electronLanguages: en-US, zh-CN`）。
- 不带 `node_modules`：渲染进程用到的库都由 Vite 打进 `out/`，`package.json` 里全是 devDependencies。
- 窗口图标：Linux 用 `resources/icon.png`；Windows 打包后用 exe 自带的图标。
- Windows 上 `SpaceKnife.exe` 旁边还有 `SpaceKnife.com`（[packaging/console-launcher](../packaging/console-launcher/src/main.rs)，无依赖的 Rust 小程序，`tools/build_package.py` 用和原生扩展相同的路径重映射编译到 `dist/console/`，electron-builder 的 `win.extraFiles` 复制过去）：控制台子系统的启动器，带同样的参数启动 `SpaceKnife.exe` 并等待它结束，让终端等得到无头模式（`SpaceKnife --headless`）的 Y/N 提问和 Ctrl+C。在终端里它不给子进程标准句柄（Electron 于是挂接到这个控制台，并设 `SPACEKNIFE_CONSOLE=1` 让主进程从 `CONIN$` 读回答）；输出被重定向时则原样传递句柄，Ctrl+C 由它结束子进程。

## 4. 安装包

**Windows（NSIS）**：引导式安装（不是一键安装），可选「仅当前用户」（默认，不需要管理员）或「所有用户」，可改安装目录；开始菜单与桌面快捷方式；安装向导跟随系统语言（中文 / 英文）；卸载时默认保留扫描、快照与设置（卸载程序第二页「你的数据」可以勾选一起删除）。没有代码签名证书：第一次运行时 SmartScreen 会提示「未知发布者」。

静默安装与卸载（命令行部署、测试）：`SpaceKnife-<版本>-Setup-x64.exe /S /currentuser`（或 `/allusers`，需要管理员）；卸载用注册表卸载项里的 `QuietUninstallString`（`"Uninstall SpaceKnife.exe" /currentuser /S`），卸载程序会复制到临时目录再运行，命令本身立即返回。electron-builder 的安装程序总会把自己复制一份到 `%LOCALAPPDATA%\spaceknife-electron-updater`（给 electron-updater 用，135 MB），卸载时也不删；SpaceKnife 不用 electron-updater（它自己的更新检查见下文），[build/installer.nsh](../apps/electron/build/installer.nsh) 在安装完和卸载时把它删掉。

卸载时的清理（installer.nsh 的 `customUnInstall`，按卸载的那个用户的目录，所有用户安装也一样）：

- 总是删除：界面配置 `%APPDATA%\SpaceKnife\electron`（以及 0.3.0 以前的 `%APPDATA%\spaceknife-electron`）、后端缓存 `%LOCALAPPDATA%\SpaceKnife\Cache`、下载的更新 `%LOCALAPPDATA%\SpaceKnife\updates`；`%APPDATA%\SpaceKnife` 空了就一起删掉。
- 快照、设置和日志（`%LOCALAPPDATA%\SpaceKnife` 的其余部分）只在勾选「同时删除快照、设置和日志」时删除，默认不勾；静默卸载加 `--delete-app-data` 也会删除。`SPACEKNIFE_HOME` 指定的目录从不碰。
- 升级时（新安装程序带 `--updated` 运行旧版本的卸载程序）什么都不删，第二页也不显示。
- installer.nsh 要带 UTF-8 BOM，否则 NSIS 读错其中的中文。它被包含在 MUI2.nsh 之前，所以卸载页面的函数定义写在 `customUnWelcomePage` 宏里。

**Linux**：

- deb / rpm：程序在 `/opt/SpaceKnife/`，`/usr/bin/spaceknife` 指向它；`spaceknife.desktop`（中英文名称与说明、关键字）和 hicolor 图标 16–512 px；依赖是 Electron 的常规依赖（GTK 3、NSS、libXss、libXtst、xdg-utils、at-spi2 等）。
- AppImage：需要 FUSE 2（Ubuntu 22.04 上是 `libfuse2`）；没有时用 `--appimage-extract-and-run` 运行。
- AppImage 的更新信息：electron-builder 用的运行时留了空的 `.upd_info` 段，构建时写进 `gh-releases-zsync|DuskXi|SpaceKnife|latest-all|SpaceKnife-*-x86_64.AppImage.zsync`（`latest-all`：最新的正式版或预发布版；`latest` 不算预发布），并用 `zsyncmake` 在旁边生成 `.zsync`（`tools/build_package.py` 的 `embed_update_info` / `make_zsync`；本机没装 `zsync` 包时只写更新信息、不生成 .zsync）。这样 AppImageUpdate、AppImageLauncher 之类的工具能增量更新。`./SpaceKnife-*.AppImage --appimage-updateinformation` 打印写进去的信息。
- 窗口的 app_id / WM_CLASS 与 desktop 文件名一致（`desktopName: spaceknife.desktop`），任务栏能把窗口和启动器图标对上。

**便携版**（Windows zip、Linux tar.gz、macOS zip）：

- 就是免安装目录（macOS 上是 `SpaceKnife.app`），外加一个空的 `data` 文件夹（里面一份中英文说明）。打包后的应用启动时，如果可执行文件旁边（macOS 上是 `SpaceKnife.app` 旁边）有 `data` 文件夹，就把它当作 `SPACEKNIFE_HOME`（[src/main/portable.ts](../apps/electron/src/main/portable.ts)）：快照、设置、日志、缓存和 Chromium 配置都存在里面，不写进用户目录。手动设的 `SPACEKNIFE_HOME` 优先；删掉或重命名 `data`，就改用正常的用户目录。
- 解压到用户可写的位置（解压到 Program Files 之类的位置会写不进去）。
- Linux 的 tar.gz 不需要 FUSE。和 AppImage 一样，`chrome-sandbox` 没有 setuid：在限制非特权用户命名空间的发行版上（Ubuntu 23.10 起的 AppArmor 规则）可能启动不了，需要加 `--no-sandbox`；目标系统 Ubuntu 22.04 和 RHEL 9 不受影响。
- 有 x64 和 arm64 两份（`win-arm64`、`linux-arm64`）。便携版更新时按处理器的架构挑包（`spaceknife.update.asset_templates`；Windows 上 Python 读的是处理器的架构，模拟运行的 x64 进程也一样）：Windows on ARM 上的 x64 便携版下一次更新就换成 arm64 版，发布里没有 arm64 包时仍给 x64（Windows 能模拟运行）；ARM Linux 只取 arm64 包。主进程两种架构的包都接受（`portableVersion`、`installRefusal`）。arm64 版目前只在 GitHub 的 ARM 运行器上测过，还没在真机上用过。
- 没有 32 位 Windows 版：Electron 44 已不再提供 win32-ia32。
- macOS 的 zip 由 `ditto -c -k --sequesterRsrc --keepParent` 打包（苹果自己的格式：包里的符号链接、扩展属性和签名都在；Python 的 zipfile 会丢），应用更新时也用 `ditto -x -k` 解开（`main/index.ts` 的 `unpack`）。ad-hoc 签名、未公证，第一次打开和 dmg 一样要在「隐私与安全性」里放行；更新由应用自己下载，不带隔离属性，换上的新版本不再问。
- macOS 的 App 转移（Gatekeeper 路径随机化）：带隔离属性、在解压出来的位置直接打开的应用，会被系统从 `/private/var/folders/…/AppTranslocation/…` 下的只读副本运行，旁边没有 `data`。`portable.ts` 认出这种路径后，用 `/sbin/mount` 的挂载表（只读 nullfs 挂载，`<原路径> on <挂载点> (nullfs, …)`）找回原来的位置，`data` 照样用原位置旁边那个；「无缝更新」替换的也是原位置的文件夹，换好后启动原位置的 `SpaceKnife.app`（替换计划里的可执行文件可以在文件夹的子目录里，`update_swap.Plan.check`）。真机上的 App 转移还没验证过（CI 下载的文件不带隔离属性）。
- 「完全磁盘访问权限」按应用的签名记：ad-hoc 签名每个版本都不同，每次更新后要重新打开一次开关（dmg 也一样，等 Developer ID 签名之后才会记住）。

**macOS（dmg）**：

- 打开 dmg，把 SpaceKnife 拖进「应用程序」。最低 macOS 14（`LSMinimumSystemVersion`；numpy 用的是需要 macOS 14 的 Accelerate 版本）。
- ad-hoc 签名、未公证：第一次打开会被 Gatekeeper 拦下，用户要去「系统设置 › 隐私与安全性」点「仍要打开」并输入密码，之后正常打开；每次更新后再来一次。macOS 14 上也可以在「应用程序」里右键 › 打开。终端里 `xattr -dr com.apple.quarantine /Applications/SpaceKnife.app` 也行。README 里有给用户的步骤。
- 关闭了 Hardened Runtime：它会强制库校验，ad-hoc 签名的应用会拒绝加载 Electron 框架原有的签名。等有 Developer ID 再开（公证需要它）。
- 「完全磁盘访问权限」：不给的话，扫描到「桌面」「文稿」「下载」、云盘、照片图库、其他应用的数据、外接盘和网络盘时会逐个弹出授权提示（提示文案是 `electron-builder.yml` 里的 `NS*UsageDescription`），邮件、信息等受保护目录读不到，计入未知空间。给了就都能扫到。应用在扫描前检查（`spaceknife.privacy`：读用户的 TCC 数据库，不弹窗），没有时弹出说明框：打开系统设置的对应页面 / 跳过这些位置（不弹窗，显示为无法访问）/ 照常逐个询问；设置「扫描范围 › 受保护的位置」记住选择。ad-hoc 签名的授权绑定在具体的构建上，更新后要在设置里关掉再打开一次。
- 扫描 `/` 时不进入 `/System/Volumes/Data`：同一 APFS 卷组的数据卷与系统卷报告同一个设备号，它的文件已经通过 firmlink 在树里（`scan.volume.volume_group_twins`）。云盘里「仅在云端」的文件（`SF_DATALESS`）按占用 0 计，这样的文件夹不进入，进程也设为不物化 dataless 文件，扫描不会触发下载。
- 磁盘负载在进程内读 IOKit 的块设备统计（与 `iostat` 同源，`ioreg` 为后备）。
- 文件条目的属性用 `getattrlistbulk` 批量取（文件夹、挂载点、firmlink 仍逐个 `fstatat`）。每个进程读到的第一批文件与 `fstatat` 对照，有差异就整条退回 readdir + `fstatat`；`SPACEKNIFE_NO_BULK=1` 关掉，用来对比速度。文件夹也能从它取（`SPACEKNIFE_BULK_DIRS=1`，同样单独对照），但并不更快：请求文件夹属性让每次调用都变慢，抵掉了省下的 `fstatat`。`spaceknife-backend scan --profile` 打印耗时分解和它的状态。
- 列目录线程默认是 CPU 数的两倍（4–16），`SPACEKNIFE_SCAN_THREADS` 可以改。
- 「Scan benchmark」workflow 在 GitHub 的 macOS 运行器上对比各设置：各设置轮流跑，每轮换一个开头；热缓存 6 轮，`sudo purge` 后 3 轮（purge 清不掉 APFS 元数据的缓存，两组差不多）。2026-10-20 扫 /System/Library /usr（3 核 arm64 45 万项，4 核 Intel 70 万项），单位是项/秒：

  | | 3 核 arm64 | 4 核 Intel |
  |---|---|---|
  | 单线程切片各用自己的线程池（e07daf3） | 5.5 万 → 13.9 万 | 2.1 万 → 6.1 万 |
  | 线程数两倍 | +8%（热）/ +15%（purge 后） | +12% / +5% |
  | 文件夹也用 getattrlistbulk | −19% / −2% | 0% / −4% |

  修好线程池之前，所有单线程切片排在同一个单线程池上，列目录实际只有一个线程；耗时分解里的「列目录线程忙碌」把排队也算了进去，看起来像内核侧的瓶颈。现在忙碌度只算列目录本身。
- 修好之后，Intel 运行器上主线程把结果合并进树占 48–62%。同一次等待里落下的切片合并成一次写入（`worker.merge_slices`），祖先的合计一次加完。Windows 上用录下的 470 个切片（C:\Program Files，35.9 万项）重放合并：140 ms → 99 ms（逐片）→ 61–77 ms（合并后）；实际扫描中，主线程合并占比 45–58% → 39–43%，这时列目录线程成了瓶颈。
- 扫描是后端子进程做的，授权和提示都记在 SpaceKnife 这个应用名下（macOS 按负责的应用归属）。

**更新检查**（`spaceknife.update`，协议 `update.*`，界面在 `shell/UpdateBadge.tsx` / `UpdateCard.tsx`）：

- 后端每天最多一次读取 `https://api.github.com/repos/DuskXi/SpaceKnife/releases`（启动约 1 分钟后第一次；带 ETag，没变化时 GitHub 回 304；失败 6 小时后再试），不带令牌，只发 `User-Agent: SpaceKnife/<版本>`。设置「关于与更新 › 更新」可以关掉，或改成「只要正式版」（默认包括 GitHub 上标为预发布的版本）。命令面板「检查更新」随时检查；「关于与更新」页（`pages/AboutPage.tsx`）显示版本、组件版本和最近 30 个发布的时间线（`update.releases`）。
- 和正在运行的版本比较：只差修订号时只在标题栏右侧显示徽章；次版本号不同时左上角挂一张卡片（发布说明的概要、前往发布页、一键下载）；主版本号不同时在窗口内弹对话框。关掉卡片或对话框后这个版本不再自动弹出，徽章保留。次版本和主版本的卡片里还有「跳过 0.4.x」/「跳过 1.x」（`update.mute`）：整个系列（次版本是 major.minor，主版本是 major）的版本都不弹卡片、不弹对话框、不显示徽章，也不自动下载；记在 `update.json` 的 `muted` 里（最多 20 个，运行的版本已经到了的系列在下一次改动时去掉），「关于与更新」页仍显示这个版本并能「恢复提示」。
- Electron 主进程把运行版本和安装方式告诉后端：`SPACEKNIFE_APP_VERSION`、`SPACEKNIFE_INSTALL_KIND`（nsis / portable / appimage / deb / rpm / dmg / dev）。后端在就绪行里报回 `updates_dir`（数据目录的 `updates`），主进程只认这个文件夹里的更新文件。演示和测试可以用 `SPACEKNIFE_UPDATE_FEED` 指向本机（只接受回环地址）的一份发布列表。
- 下载（「自动下载更新」开启时检查之后自动开始，否则点「一键下载」）按安装方式挑发布文件，存进 `updates` 文件夹，并用同一个发布里的 `SHA256SUMS*.txt` 校验，不符就删掉。之后更新卡片给出两个大按钮（`shared/install.ts` 的 `installStep` 决定每种安装方式各是什么）：「无缝更新」由 SpaceKnife 自己换上新版本再启动；经典方式交给安装程序、磁盘映像或安装包，或者在文件夹中显示下载的文件让用户自己替换。两者都只在用户点击后由主进程执行（`main/index.ts` 的 `installUpdate`），从不自己开始，以后也不会加自动安装的选项：
  - **NSIS**：
    - 「无缝更新」：主进程先停掉后端，再以 `--updated /S --force-run` 加 `/allusers` 或 `/currentuser`（按安装目录在 Program Files 还是 `%LOCALAPPDATA%` 下）运行新安装程序，然后退出。安装程序从注册表读出原来的安装目录，所有用户安装时自己请求提权，杀掉残留的 SpaceKnife 进程，以 `/S /KEEP_APP_DATA --updated` 运行旧版本的卸载程序，装好后（`--force-run`）启动新版本。
    - 「运行安装程序」：不带参数运行新安装程序（显示安装向导），SpaceKnife 随即退出。它照样就地升级：electron-builder 以 `--updated` 运行旧版本的卸载程序，数据保留。
  - **便携版**（Windows zip、Linux tar.gz、macOS zip）：
    - 「无缝更新」：换文件的是**新版本自己的后端可执行文件**，不再是 PowerShell / sh 脚本（脚本已经去掉）。
      - 主进程把压缩包解到 `updates/staging-<版本>`（Windows 用 tar.exe，没有时用 Expand-Archive；Linux 用 tar；macOS 用 ditto），确认里面唯一的顶层文件夹带着可执行文件（macOS 上是 `SpaceKnife.app`）和后端（`resources/backend/spaceknife-backend[.exe]`，macOS 上在 `SpaceKnife.app/Contents/Resources/backend/` 里）。
      - 写出计划 `updates/update-<版本>.json`（版本 1：`v`、`pid`、`app`、`new`、`old`、`exe`、`log`、`keep: ["data"]`）。计划是和每个旧版本的约定：旧应用把它交给更新版本的后端，所以以后的后端要一直能读 `v: 1`。
      - 以 detached 方式启动 `<staging>/resources/backend/spaceknife-backend[.exe] --apply-update <计划>`，等它在 stdout 回一行 `{"ok": true}` 后退出。30 秒没回、回了别的或者它先退出了，应用就照旧运行并提示原因。
      - 助手（`spaceknife.update_swap`，只用标准库；`packaging/backend_entry.py` 在加载别的东西之前就分派过去）先检查计划：路径都是绝对路径、`exe` 在 `app` 里（macOS 上在 `app/SpaceKnife.app/Contents/MacOS/` 里）、新版本的同一位置有可执行文件、`old` 是空的。不能用的计划（比如新版本不完整）什么都不动：日志写一行 `not started: …`，回 `{"ok": false, "error": …}`，应用保持打开并提示原因。
      - 等应用主进程退出（最多 60 秒，日志「waiting for SpaceKnife (<pid>) to quit」；超时写「SpaceKnife did not quit: kept the old version」）。
      - 把程序目录里除 `data` 以外的条目移到 `updates/previous-<旧版本>`。文件还被占用时（正在关闭的应用自己的子进程、杀毒软件）在 30 秒内反复重试，日志写「busy: …」。
      - 再把新条目移进来。助手自己就在新条目里运行，而 Windows 不让改名一个里面有程序在运行的文件夹，所以整体改不了名的逐个移动，连这也不行的就复制。
      - 带 `--updated` 启动程序，日志末行写「updated」。任何一步失败就把旧条目移回原处、启动旧版本，日志写「kept the old version」。
      - 每一步都写进 `updates/update-<版本>.log`，每行带 UTC 时间。
    - 「自己替换」：在文件夹中显示下载的压缩包，SpaceKnife 继续运行；用户退出后自己解压覆盖，保留 `data`。
  - **AppImage**：
    - 「无缝更新」：复制成 `$APPIMAGE.new`、设为 755，再改名覆盖 `$APPIMAGE`（同一文件系统内的原子替换），然后启动它。
    - 「自己替换」：在文件夹中显示新的 AppImage，由用户退出后替换旧文件。
  - **dmg / deb / rpm**：没有无缝的办法，只有经典方式「打开磁盘映像」/「打开安装包」：交给系统打开（用户把新版拖进「应用程序」，或由软件安装器安装），SpaceKnife 随即退出。
  - 有扫描在进行时，会关闭 SpaceKnife 的按钮第一次点只提示「还有扫描在进行，更新会中断它（已扫描的部分不会保存）」，按钮改写「再点一次继续」，第二次点才执行；「自己替换」不关闭应用，不问。
  - 新版本带 `--updated` 启动时，预加载脚本拿到 `justUpdated`，界面提示一次「已更新到 SpaceKnife <版本>」。下一次启动后端清理 `updates`，旧安装包、`staging`、`previous`、`update-<版本>.json` 和日志都会删掉。
  - 主进程只启动 `updates` 文件夹里、按本次安装方式的发布文件命名的文件（`installRefusal`）。
- **发布时的约定**：更新检查靠文件名找文件。发布文件必须沿用下面的名字，并附上对应的校验和文件：
  - `SpaceKnife-<版本>-Setup-x64.exe`、`SpaceKnife-<版本>-win-x64-portable.zip`、`SpaceKnife-<版本>-win-arm64-portable.zip`、`SpaceKnife-<版本>-linux-x64-portable.tar.gz`、`SpaceKnife-<版本>-linux-arm64-portable.tar.gz`、`SpaceKnife-<版本>-x86_64.AppImage`、`spaceknife_<版本>_amd64.deb`、`spaceknife-<版本>.x86_64.rpm`、`SpaceKnife-<版本>-mac-arm64.dmg`、`SpaceKnife-<版本>-mac-x64.dmg`、`SpaceKnife-<版本>-mac-arm64-portable.zip`、`SpaceKnife-<版本>-mac-x64-portable.zip`；
  - `SHA256SUMS.txt`（安装包）、`SHA256SUMS-portable-<目标>.txt`（目标是 `win-x64`、`win-arm64`、`linux-x64`、`linux-arm64`）、`SHA256SUMS-mac-<架构>.txt`（dmg 和 macOS 便携 zip）；
  - `SpaceKnife-<版本>-x86_64.AppImage.zsync`（AppImage 里的更新信息指向它）。
  - tag 是 `v<版本>`（语义化版本，可带预发布段）。改名之前先改 `spaceknife.update.pick_asset` 和主进程的 `installRefusal`。
- **累积更新**：还没发布的更改随做随记在仓库根目录的 [UNRELEASED.md](../UNRELEASED.md)（中英双语，「新功能」「修复」两节，和发布说明同一个结构）。两份 README 的顶部导航里固定链接到它。
  - 发布时，它就是发布说明的正文：再补上下载表和已知限制。
  - 发布之后，在 dev 上把它清空：只留标题、语言切换、开头那句说明（版本号改成下一版），以及每节下面的「（暂无）」/「(nothing yet)」。

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

```bash
uv run python packaging/smoke_headless.py dist/electron/win-unpacked/SpaceKnife.com
```

[packaging/smoke_headless.py](../packaging/smoke_headless.py) 测无头模式：`--headless --listen 127.0.0.1:0` 启动，读打印出的地址，检查页面、`/sk/info` 只认 token、WebSocket 拒绝别的 Origin、用页面自己的 Origin 能和后端握手，再像终端一样停止（SIGINT；Windows 上对 `SpaceKnife.com` 发 Ctrl+Break）。Linux 上不开 xvfb，验证没有显示器也能启动。`Portable build` 工作流在便携版冒烟测试之后跑它。

[packaging/update_e2e.py](../packaging/update_e2e.py) 是「无缝更新」和卸载清理的端到端测试，只在 GitHub Actions 上跑（`Portable build` 工作流；它会在当前用户的目录里安装、更新、卸载，所以在别处拒绝运行）。它在 127.0.0.1 上提供一份发布列表（`SPACEKNIFE_UPDATE_FEED`），把这次构建当作下一个修订版，通过 DevTools 端口等徽章变成「可以更新」，再像用户一样点徽章和「无缝更新」（`.uc-choice.seamless`）：

- 便携版（Windows、Linux）：换文件的是新版本后端的 `--apply-update`（没有脚本）。测试等 `updates/update-<新版本>.log` 的最后一行说出结局（赶在新后端清掉 `updates` 之前读），它必须以「updated」结尾；然后等应用带 `--updated` 回来，检查程序文件换成了新的（新包里多一个标记文件）、`data` 原样保留，新后端清掉了 `previous-<旧版本>`。
- Windows 安装版：先装最新发布的安装包，用应用的那组参数（`--updated /S --force-run /currentuser`）升级到这次构建；再从应用里点「无缝更新」（静默运行下一版的安装程序）；然后检查静默卸载（保留快照、设置和日志，删掉界面配置、缓存、下载的更新和 electron-updater 的缓存）、勾选「同时删除快照、设置和日志」的卸载（通过 Win32 消息操作卸载程序的页面，确认默认不勾、页面写出数据位置），以及 `--delete-app-data`。

已验证（2026-10-18）：Windows 11 上安装包本身（静默「仅当前用户」安装 → 安装后的应用与后端冒烟 → 静默卸载；卸载后注册表卸载项、`HKCU\Software` 下的键、开始菜单与桌面快捷方式、安装目录都和安装前一致，用户数据不变）和 `win-unpacked`；WSL（Ubuntu 22.04，WSLg）上 `linux-unpacked` 与 AppImage；deb / rpm 的元数据、依赖、desktop 文件与图标。还没做：在干净的 Windows 10、RHEL 9、Ubuntu 22.04 上安装测试（见 TODO）；deb / rpm 只检查了内容，没有实际安装。

## 6. 已知限制

- 未签名（Windows 代码签名证书待定）；macOS 只有 ad-hoc 签名、没有公证（等 Apple 开发者账号），也还没进 Homebrew（官方 cask 要求通过 Gatekeeper）。
- 项目主页：`https://github.com/DuskXi/SpaceKnife`。
- 没有 strip：PyInstaller 的 strip 会破坏 auditwheel 改过的库（numpy 自带的 openblas 报「not page-aligned」，冒烟测试会发现）。只 strip 没被改过的库（libpython、zstandard、原生扩展）大约能省 40 MB 解压后体积。
- 「仅当前用户」安装在用户可写的目录里；需要管理员权限的 MFT 快扫会从这里启动提权助手。介意的话选「所有用户」安装（Program Files）。
- 不会自动安装（有意如此）：有新版本时提示并在后台下载（见第 4 节「更新检查」），安装总要用户在更新卡片里点「无缝更新」或经典方式，以后也不会加自动安装的选项。
