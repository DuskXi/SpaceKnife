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
- 和正在运行的版本比较：只差修订号时只在标题栏右侧显示徽章；次版本号不同时左上角挂一张卡片（发布说明的概要、前往发布页、一键下载）；主版本号不同时在窗口内弹对话框。关掉卡片或对话框后这个版本不再自动弹出，徽章保留。
- Electron 主进程把运行版本和安装方式告诉后端：`SPACEKNIFE_APP_VERSION`、`SPACEKNIFE_INSTALL_KIND`（nsis / portable / appimage / deb / rpm / dmg / dev）。后端在就绪行里报回 `updates_dir`（数据目录的 `updates`），主进程只认这个文件夹里的更新文件。演示和测试可以用 `SPACEKNIFE_UPDATE_FEED` 指向本机（只接受回环地址）的一份发布列表。
- 下载（「自动下载更新」开启时检查之后自动开始，否则点「一键下载」）按安装方式挑发布文件，存进 `updates` 文件夹，并用同一个发布里的 `SHA256SUMS*.txt` 校验，不符就删掉。之后按安装方式（`main/update.ts` 的 `restartMode`）：
  - **NSIS**（「重启并更新」）：主进程先停掉后端，再以 `--updated /S --force-run` 加 `/allusers` 或 `/currentuser`（按安装目录在 Program Files 还是 `%LOCALAPPDATA%` 下）运行新安装程序，然后退出。安装程序从注册表读出原来的安装目录，所有用户安装时自己请求提权，杀掉残留的 SpaceKnife 进程，以 `/S /KEEP_APP_DATA --updated` 运行旧版本的卸载程序，装好后（`--force-run`）启动新版本。
  - **便携版**（「重启并更新」）：主进程把压缩包解到 `updates/staging-<版本>`（Windows 用 tar.exe，没有时用 Expand-Archive；Linux 用 tar），确认里面唯一的顶层文件夹带着可执行文件，再写出 `update-<版本>.ps1`（带 BOM 的 UTF-8：Windows PowerShell 5.1 把不带 BOM 的脚本按 ANSI 代码页读，中文路径会乱）或 `.sh`，启动它后退出。Windows 上不能直接以 detached 方式启动 powershell.exe（没有控制台时脚本根本不执行），不 detached 又会随应用一起被 libuv 的作业对象结束；所以由一个不 detached、隐藏的 PowerShell 用 `Start-Process -WindowStyle Hidden` 启动真正执行的那个（它不在作业对象里，应用退出后继续运行），应用等这个启动器结束才退出（`main/update.ts` 的 `swapLauncher`）。`Start-Process -ArgumentList` 的数组不会给带空格的元素加引号，所以参数写成一整串、脚本路径放在双引号里。两种脚本都先检查新版本里有没有可执行文件，没有就什么都不动、留下旧版本。脚本等旧进程结束，把程序目录里除 `data` 以外的条目移到 `updates/previous-<旧版本>`（文件被占用时重试 60 次），再把新条目移进来；任何一步失败就把旧条目移回原处。最后带 `--updated` 启动程序，过程写进 `update-<版本>.log`。
  - **AppImage**（「重启并更新」）：复制成 `$APPIMAGE.new`、设为 755，再改名覆盖 `$APPIMAGE`（同一文件系统内的原子替换），然后启动它。
  - **dmg / deb / rpm**（「安装」）：交给系统打开（用户把新版拖进「应用程序」，或由软件安装器安装），SpaceKnife 随即退出。
  - 新版本带 `--updated` 启动时，预加载脚本拿到 `justUpdated`，界面提示一次「已更新到 SpaceKnife <版本>」。下一次启动后端清理 `updates`，旧安装包、`staging`、`previous` 都会删掉。
  - 主进程只启动 `updates` 文件夹里、按本次安装方式的发布文件命名的文件（`installRefusal`）。
- **发布时的约定**：更新检查靠文件名找文件。发布文件必须沿用下面的名字，并附上对应的校验和文件：
  - `SpaceKnife-<版本>-Setup-x64.exe`、`SpaceKnife-<版本>-win-x64-portable.zip`、`SpaceKnife-<版本>-linux-x64-portable.tar.gz`、`SpaceKnife-<版本>-x86_64.AppImage`、`spaceknife_<版本>_amd64.deb`、`spaceknife-<版本>.x86_64.rpm`、`SpaceKnife-<版本>-mac-arm64.dmg`、`SpaceKnife-<版本>-mac-x64.dmg`；
  - `SHA256SUMS.txt`（安装包）、`SHA256SUMS-portable-<目标>.txt`、`SHA256SUMS-mac-<架构>.txt`；
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

[packaging/update_e2e.py](../packaging/update_e2e.py) 是「重启并更新」和卸载清理的端到端测试，只在 GitHub Actions 上跑（`Portable build` 工作流；它会在当前用户的目录里安装、更新、卸载，所以在别处拒绝运行）。它在 127.0.0.1 上提供一份发布列表（`SPACEKNIFE_UPDATE_FEED`），把这次构建当作下一个修订版，通过 DevTools 端口等徽章变成「可以更新」，再像用户一样点徽章和「重启并更新」：

- 便携版（Windows、Linux）：程序文件换成新的（新包里多一个标记文件），`data` 原样保留，应用带 `--updated` 回来，新后端清掉 `previous-<旧版本>`。
- Windows 安装版：先装最新发布的安装包，用应用的那组参数（`--updated /S --force-run /currentuser`）升级到这次构建；再从应用里「重启并更新」；然后检查静默卸载（保留快照、设置和日志，删掉界面配置、缓存、下载的更新和 electron-updater 的缓存）、勾选「同时删除快照、设置和日志」的卸载（通过 Win32 消息操作卸载程序的页面，确认默认不勾、页面写出数据位置），以及 `--delete-app-data`。

已验证（2026-10-18）：Windows 11 上安装包本身（静默「仅当前用户」安装 → 安装后的应用与后端冒烟 → 静默卸载；卸载后注册表卸载项、`HKCU\Software` 下的键、开始菜单与桌面快捷方式、安装目录都和安装前一致，用户数据不变）和 `win-unpacked`；WSL（Ubuntu 22.04，WSLg）上 `linux-unpacked` 与 AppImage；deb / rpm 的元数据、依赖、desktop 文件与图标。还没做：在干净的 Windows 10、RHEL 9、Ubuntu 22.04 上安装测试（见 TODO）；deb / rpm 只检查了内容，没有实际安装。

## 6. 已知限制

- 未签名（Windows 代码签名证书待定）；macOS 只有 ad-hoc 签名、没有公证（等 Apple 开发者账号），也还没进 Homebrew（官方 cask 要求通过 Gatekeeper）。
- 项目主页：`https://github.com/DuskXi/SpaceKnife`。
- 没有 strip：PyInstaller 的 strip 会破坏 auditwheel 改过的库（numpy 自带的 openblas 报「not page-aligned」，冒烟测试会发现）。只 strip 没被改过的库（libpython、zstandard、原生扩展）大约能省 40 MB 解压后体积。
- 「仅当前用户」安装在用户可写的目录里；需要管理员权限的 MFT 快扫会从这里启动提权助手。介意的话选「所有用户」安装（Program Files）。
- 没有静默的自动更新：有新版本时提示并一键下载（见第 4 节「更新检查」），安装仍要用户点一下。
