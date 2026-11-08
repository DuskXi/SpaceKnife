# 打包与发布（M8）

Tauri 版（0.3.5 起）的安装包：Windows NSIS；Linux AppImage / deb / rpm；macOS app + dmg（Apple Silicon 与 Intel 各一个，ad-hoc 签名、未公证：Developer ID 签名与公证等 Apple 开发者账号下来再接上，见 [TODO.md](../TODO.md)）。后端（Rust，`crates/sk-server`）在应用进程里运行，不再有单独冻结的 Python 后端；命令行程序 `spaceknife-cli` 装在应用旁边。0.3.x 的 Electron 版及其打包方式见 git 历史与 [release-notes/0.3.4.md](../release-notes/0.3.4.md)。

| 产物 | 平台 | 大小 |
|---|---|---|
| `SpaceKnife_<版本>_x64-setup.exe` | Windows 10+ x64，NSIS（仅当前用户），未签名；Windows on ARM 上模拟运行 | — |
| `SpaceKnife_<版本>_amd64.AppImage` | Linux x64，免安装 | — |
| `SpaceKnife_<版本>_amd64.deb` | Ubuntu 22.04+ / Debian 系 | — |
| `SpaceKnife-<版本>-1.x86_64.rpm` | RHEL 9+ / Fedora 系 | — |
| `SpaceKnife_<版本>_aarch64.dmg` | macOS，Apple Silicon，ad-hoc 签名、未公证 | — |
| `SpaceKnife_<版本>_x64.dmg` | macOS，Intel，ad-hoc 签名、未公证 | — |
| `SpaceKnife-<版本>-win-x64-portable.zip`、`SpaceKnife-<版本>-win-arm64-portable.zip` | Windows 便携版 | — |
| `SpaceKnife-<版本>-linux-x64-portable.tar.gz`、`SpaceKnife-<版本>-linux-arm64-portable.tar.gz` | Linux 便携版 | — |
| `SpaceKnife-<版本>-mac-arm64-portable.zip`、`SpaceKnife-<版本>-mac-x64-portable.zip` | macOS 便携版：`SpaceKnife.app` 旁边一个 `data` 文件夹 | — |

macOS 的最低版本是 14（`tauri.conf.json` 的 `bundle.macOS.minimumSystemVersion`，与 0.3.x 相同）。

<!-- TODO: confirm — 0.3.5 的各文件大小在第一次 CI 构建后补上 -->

文件名是 Tauri 打包器的命名（便携包是 `apps/desktop/scripts/portable.mjs` 的），更新检查按这些名字找文件：唯一的来源是 `crates/sk-core/src/update/mod.rs` 的 `asset_templates`。

## 1. 构建

需要：Rust 工具链（stable，edition 2024）、Node 24 与 pnpm 10。Linux 上还要 Tauri 的系统库：`libwebkit2gtk-4.1-dev`、`libgtk-3-dev`、`libayatana-appindicator3-dev`、`librsvg2-dev`、`libssl-dev`、`patchelf`。

```bash
cd apps/desktop
pnpm install --frozen-lockfile
pnpm bundle            # tauri build：NSIS / AppImage + deb + rpm / app + dmg
pnpm bundle:portable   # 在 bundle 之后：target/portable/ 下的便携包
```

在哪个系统上运行就出哪个系统的包（`src-tauri/tauri.{windows,linux,macos}.conf.json` 定目标：Windows 出 NSIS，Linux 出三种，macOS 出 app 和 dmg），一律为构建机自己的架构。Windows 和 Linux 的安装包只发 x64；arm64 只发便携包。安装包在 `target/release/bundle/<格式>/`，便携包在 `target/portable/`。

`pnpm bundle`（[scripts/bundle.mjs](../apps/desktop/scripts/bundle.mjs)）是带路径重映射（见下文「不带构建机路径」）的 `tauri build --config src-tauri/tauri.bundle.json`，后面的参数照传（CI 只出程序时用 `pnpm bundle --no-bundle`）；它的 `beforeBuildCommand` 先 `pnpm generate`（界面的静态文件，`.output/public`，嵌进应用），再运行 [apps/desktop/scripts/prepare-bundle.mjs](../apps/desktop/scripts/prepare-bundle.mjs)：

- 以 release 编译 `spaceknife-cli`（`crates/sk-server`），放成 `src-tauri/target-bundle/spaceknife-cli-<目标三元组>[.exe]`，作为 Tauri 的 `externalBin` 装在应用旁边（名字是 `spaceknife-cli`）；
- 写出许可证 `src-tauri/target-bundle/licenses/`（见下）。

这两样不放在基础配置 `tauri.conf.json` 里：Tauri 的构建脚本要求 externalBin 和资源已经存在，否则普通的 `cargo build` 和 `pnpm tauri dev` 都会失败。

`pnpm bundle:portable`（[scripts/portable.mjs](../apps/desktop/scripts/portable.mjs)）把 `target/release` 里的 `spaceknife` 与 `spaceknife-cli`、许可证、无头模式用的界面（`ui`，即 `apps/desktop/.output/public`）和一个空的 `data` 文件夹（里面一份中英文说明）放进一个文件夹，Windows 上打成 zip（系统自带的 bsdtar），Linux 上打成 tar.gz；macOS 上是 `pnpm bundle` 做出的 `SpaceKnife.app`（`spaceknife-cli` 在包里）加旁边的 `data`，用 `ditto` 打成 zip（保留包内的链接、属性和签名）。

CI：「Portable build」（[.github/workflows/portable.yml](../.github/workflows/portable.yml)）在 Windows 与 Linux 的 x64 / arm64 运行器上构建便携包（x64 上同时出安装包）并跑冒烟测试；「macOS build」（[.github/workflows/macos.yml](../.github/workflows/macos.yml)）在 arm64 与 Intel 运行器上出 dmg 与便携 zip；「Release」（[.github/workflows/release.yml](../.github/workflows/release.yml)）从 tag 调用这两个工作流并建发布（见第 4 节「发布」）。

**许可证**：SpaceKnife 是 MIT（仓库根目录 `LICENSE`）。安装后的 `licenses/` 资源里有 `LICENSE.txt`、`THIRD-PARTY-rust.txt`（`spaceknife` 与 `spaceknife-cli` 链接的 crate，来自 `cargo metadata`）和 `THIRD-PARTY-ui.txt`（界面的 npm 生产依赖，来自 `pnpm licenses list --prod`，加上 Geist / Noto Sans SC 字体的 OFL）；相同的许可证文本只印一次。由 `prepare-bundle.mjs` 生成。

**不带构建机路径**：Rust 的 panic 信息和调试路径里会带上构建机的绝对路径（也就带上了用户名）。所以发布用的构建（`pnpm bundle`，即 [scripts/bundle.mjs](../apps/desktop/scripts/bundle.mjs)；`prepare-bundle.mjs` 编的 `spaceknife-cli` 用同一组参数）给 rustc 加上 `--remap-path-prefix`：家目录、`CARGO_HOME`、`RUSTUP_HOME` 和仓库分别写成 `~`、`/cargo`、`/rustup`、`/spaceknife`（Cargo 的 trim-paths 还没稳定）；Windows 上再加 `-Clink-arg=/PDBALTPATH:%_PDB%`，程序里只留 PDB 的文件名（`tools/package_checks.mjs` 的 `remappedEnv`）。直接运行 `pnpm tauri build` 或 `cargo build --release` 不做这一步，只适合本机自用。

做好的包由 `node tools/package_checks.mjs paths <文件或文件夹>…` 检查：任何文件里都不能出现构建机的家目录（UTF-8 和 UTF-16LE、两种斜杠、JSON / JS 转义、ASCII 字母不分大小写，后面不能紧跟别的名字字符），zip、tar、gzip、zstd 和 ar（deb）里的成员也看，成员里的压缩包再往里看；tar 成员的属主也不能是构建机的用户（Linux 便携包用 `--owner=0 --group=0 --numeric-owner` 打包）。读不了的格式（xz 等）算失败，不算通过。

**glibc 下限**：Linux 的包里每个 ELF 文件都只能要 glibc 2.34 及以下（RHEL 9；Ubuntu 22.04 是 2.35）：`node tools/package_checks.mjs glibc <文件或文件夹>…`，和上面读同样的成员。判断是保守的：文件里出现的任何 `GLIBC_x.y` 都算；一个 ELF 文件都没有也算失败（多半是给错了文件夹）。

两项检查在 CI 里跑：「Portable build」检查每个便携包（glibc 只在 Linux 上），带安装包时再检查安装包里的东西（NSIS 用 7-Zip 解开、deb 直接读、rpm 经 rpm2cpio、AppImage 用它自己的 `--appimage-extract` 解开）；「macOS build」检查便携 zip 和 dmg 里的应用（只读挂载）。测试在 `tools/tests/package_checks.test.mjs`。<!-- TODO: confirm — Tauri 的 AppImage 带着构建机（Ubuntu 22.04）的系统库（WebKitGTK、GTK 的加载器缓存等），第一次带安装包的构建要看它能不能过 glibc 下限和路径检查 -->

rpm 包头里会记下构建机的主机名（Build Host）：没有 root 时用 `apt-get download` 取 Ubuntu 官方的 rpm 工具、`dpkg-deb -x` 解到用户目录，可以在 `RPM_CONFIGDIR/macros.d/` 里放一个定义 `%_buildhost localhost` 的宏文件。

## 2. 后端与命令行

- 后端就在应用进程里：`src-tauri/src/backend.rs` 调用 `sk_server::start`，在随机的回环端口上提供协议（每次启动一个新 token），界面连上它（[protocol.md](protocol.md) §1）。没有子进程，也没有 stdin 交接 token。
- 同一个程序的另外两种用途在 `src-tauri/src/main.rs` 里、加载任何东西之前分派：`spaceknife helper …` 是提权的扫描助手（`sk_core::helper`），`spaceknife --apply-update <计划>` 是便携版的「无缝更新」（`sk_core::update::swap`）。
- 命令行：`spaceknife-cli`（`crates/sk-server/src/bin/spaceknife-cli.rs`，`cli/mod.rs`）装在应用旁边：`scan`、`run`、`snapshot list | show`、`serve`（给界面用的 WebSocket 后端，`--ready-file` 等）、`headless`、`version`；`--headless` / `--listen` 作为 `headless` 的别名仍然接受。
- 安装目录本身是受保护路径（不能在应用里删除或移动它）。

### 命令行扫描的启动

`spaceknife-cli scan <路径> --no-snapshot …`（基准和脚本都这样用）的用时包括进程的启动和退出。Rust 版没有解释器启动和模块导入的开销；输出格式与 Python 版的 `scan` 逐字逐句相同（`crates/sk-server/src/cli/rich.rs`），外部基准（Disk-Scan-Benchmark）照旧解析。

（Python 后端时期为缩短启动做过的工作——不收 setuptools、按需导入、`os._exit` 跳过收尾——以及当时的测量见 git 历史中本文的旧版本。）

- 「最大的文件」用部分选择（第 k 大的值，并列时编号小的在前），不对全部文件排序。

## 3. Tauri 外壳

- 应用在 `apps/desktop`：Nuxt 4 + Vue 3 的界面（`app/`），Tauri 2 的外壳（`src-tauri/`，crate `spaceknife-desktop`，可执行文件 `spaceknife`）。见 [apps/desktop/PORTING.md](../apps/desktop/PORTING.md)「Commands」「Bundles」。
- `identifier` 是 `io.github.duskxi.spaceknife`。
- 界面跑在系统的 WebView 里（Windows WebView2、macOS WKWebView、Linux WebKitGTK），不再自带 Chromium。
- 外壳自己的文件（`theme.json`、WebView 的配置目录）：设了 `SPACEKNIFE_HOME`（便携版、演示、测试）时在 `<SPACEKNIFE_HOME>/desktop`，否则在 Tauri 的应用本地数据目录（按 identifier 命名），和后端的文件分开（`src-tauri/src/window.rs` 的 `ui_dir`）。
- 拖动窗口的元素带 `data-tauri-drag-region`。
- Windows 上 release 构建的 `SpaceKnife.exe` 是 GUI 子系统；终端里用的无头模式是控制台程序 `spaceknife-cli headless`，不再需要 0.3.x 的 `SpaceKnife.com` 启动器。

## 4. 安装包

**Windows（NSIS）**：Tauri 的 NSIS 安装程序，「仅当前用户」（`installMode: currentUser`，不需要管理员）；安装向导有中文和英文。没有代码签名证书：第一次运行时 SmartScreen 会提示「未知发布者」。卸载程序是安装目录里的 `uninstall.exe`（`install.rs` 据此认出 NSIS 安装）。<!-- TODO: confirm — 静默安装 / 卸载的参数、卸载时是否保留 / 删除扫描、快照与设置（Electron 版的 installer.nsh 清理逻辑没有移植），待 Tauri 的安装程序定下来再写 -->

**Linux**：

- deb / rpm：程序在 `/usr/bin/spaceknife`，`spaceknife-cli` 在旁边；desktop 文件与 hicolor 图标由 Tauri 生成；依赖是 WebKitGTK 4.1、GTK 3 等。`install.rs` 用 `/var/lib/dpkg/info/` 的文件列表区分 deb 与 rpm。
- AppImage：需要 FUSE 2（Ubuntu 22.04 上是 `libfuse2`）；没有时用 `--appimage-extract-and-run` 运行。发布里不再有 `.AppImage.zsync`（Electron 版写过 zsync 更新信息；Tauri 的 AppImage 由 SpaceKnife 自己的更新检查替换）。

**便携版**（Windows zip、Linux tar.gz、macOS zip）：

- 一个文件夹：`spaceknife[.exe]`、`spaceknife-cli[.exe]`、`licenses/` 和一个空的 `data` 文件夹（macOS 上是 `SpaceKnife.app` 加旁边的 `data`）。打包后的应用启动时，如果可执行文件旁边（macOS 上是 `SpaceKnife.app` 旁边）有 `data` 文件夹，就把它当作 `SPACEKNIFE_HOME`（`src-tauri/src/portable.rs`，`lib.rs` 的 `prepare_environment`）：快照、设置、日志、缓存和 WebView 配置都存在里面，不写进用户目录。手动设的 `SPACEKNIFE_HOME` 优先；删掉或重命名 `data`，就改用正常的用户目录。
- 解压到用户可写的位置（解压到 Program Files 之类的位置会写不进去）。
- Linux 的 tar.gz 不需要 FUSE。
- 有 x64 和 arm64 两份（`win-arm64`、`linux-arm64`）。便携版更新时按处理器的架构挑包（`crates/sk-core/src/update/mod.rs` 的 `asset_templates`）：Windows on ARM 上先找 arm64 包，发布里没有时仍给 x64（Windows 能模拟运行）；ARM Linux 只取 arm64 包。arm64 版目前只在 GitHub 的 ARM 运行器上测过，还没在真机上用过。
- 没有 32 位 Windows 版。
- macOS 的 zip 保留包里的符号链接、扩展属性和签名（ditto 格式）。ad-hoc 签名、未公证，第一次打开和 dmg 一样要在「隐私与安全性」里放行；更新由应用自己下载，不带隔离属性，换上的新版本不再问。
- macOS 的 App 转移（Gatekeeper 路径随机化）：带隔离属性、在解压出来的位置直接打开的应用，会被系统从 `/private/var/folders/…/AppTranslocation/…` 下的只读副本运行，旁边没有 `data`。`portable.rs` 认出这种路径后，用 `/sbin/mount` 的挂载表（只读 nullfs 挂载，`<原路径> on <挂载点> (nullfs, …)`）找回原来的位置，`data` 照样用原位置旁边那个；「无缝更新」替换的也是原位置的文件夹。真机上的 App 转移还没验证过（CI 下载的文件不带隔离属性）。
- 「完全磁盘访问权限」按应用的签名记：ad-hoc 签名每个版本都不同，每次更新后要重新打开一次开关（dmg 也一样，等 Developer ID 签名之后才会记住）。

**macOS（dmg）**：

- 打开 dmg，把 SpaceKnife 拖进「应用程序」。
- ad-hoc 签名、未公证：第一次打开会被 Gatekeeper 拦下，用户要去「系统设置 › 隐私与安全性」点「仍要打开」并输入密码，之后正常打开；每次更新后再来一次。终端里 `xattr -dr com.apple.quarantine /Applications/SpaceKnife.app` 也行。README 里有给用户的步骤。
- 「完全磁盘访问权限」：不给的话，扫描到「桌面」「文稿」「下载」、云盘、照片图库、其他应用的数据、外接盘和网络盘时会逐个弹出授权提示，邮件、信息等受保护目录读不到，计入未知空间。给了就都能扫到。应用在扫描前检查（`crates/sk-core/src/privacy.rs`：读用户的 TCC 数据库，不弹窗），没有时弹出说明框：打开系统设置的对应页面 / 跳过这些位置（不弹窗，显示为无法访问）/ 照常逐个询问；设置「扫描范围 › 受保护的位置」记住选择。ad-hoc 签名的授权绑定在具体的构建上，更新后要在设置里关掉再打开一次。
- 扫描 `/` 时不进入 `/System/Volumes/Data`：同一 APFS 卷组的数据卷与系统卷报告同一个设备号，它的文件已经通过 firmlink 在树里（`crates/sk-core/src/volume.rs` 的 `volume_group_twins`）。云盘里「仅在云端」的文件（`SF_DATALESS`）按占用 0 计，这样的文件夹不进入，进程也设为不物化 dataless 文件，扫描不会触发下载。
- 磁盘负载在进程内读 IOKit 的块设备统计（与 `iostat` 同源）。
- 文件条目的属性用 `getattrlistbulk` 批量取（文件夹、挂载点、firmlink 仍逐个 `fstatat`）。读到的第一批文件与 `fstatat` 对照，有差异就整条退回 readdir + `fstatat`；`SPACEKNIFE_NO_BULK=1` 关掉，用来对比速度。文件夹也能从它取（`SPACEKNIFE_BULK_DIRS=1`，同样单独对照），但并不更快：请求文件夹属性让每次调用都变慢，抵掉了省下的 `fstatat`。`spaceknife-cli scan --profile` 打印耗时分解和它的状态。
- 列目录的起步宽度是可用 CPU 数的两倍（4–16），读盘时加宽到冷宽度，由控制器决定（[native.md](native.md) 2.2「列目录线程」）；`SPACEKNIFE_SCAN_THREADS` 可以固定。
- 2026-10-20 在 GitHub 的 macOS 运行器上对比过各设置（当时的「Scan benchmark」工作流，后来删掉了，云端不再跑基准）：各设置轮流跑，每轮换一个开头；热缓存 6 轮，`sudo purge` 后 3 轮（purge 清不掉 APFS 元数据的缓存，两组差不多）。扫 /System/Library /usr（3 核 arm64 45 万项，4 核 Intel 70 万项），单位是项/秒：

  | | 3 核 arm64 | 4 核 Intel |
  |---|---|---|
  | 单线程切片各用自己的线程池（e07daf3） | 5.5 万 → 13.9 万 | 2.1 万 → 6.1 万 |
  | 线程数两倍 | +8%（热）/ +15%（purge 后） | +12% / +5% |
  | 文件夹也用 getattrlistbulk | −19% / −2% | 0% / −4% |

  修好线程池之前，所有单线程切片排在同一个单线程池上，列目录实际只有一个线程；耗时分解里的「列目录线程忙碌」把排队也算了进去，看起来像内核侧的瓶颈。现在忙碌度只算列目录本身。
- 修好之后（Python 后端时期），Intel 运行器上主线程把结果合并进树占 48–62%。同一次等待里落下的切片合并成一次写入，祖先的合计一次加完。Windows 上用录下的 470 个切片（C:\Program Files，35.9 万项）重放合并：140 ms → 99 ms（逐片）→ 61–77 ms（合并后）；实际扫描中，主线程合并占比 45–58% → 39–43%，这时列目录线程成了瓶颈。Rust 版在 `crates/sk-core/src/scan/scanner.rs` 里同样成批合并切片。
- 扫描在应用进程里做，授权和提示都记在 SpaceKnife 这个应用名下。

**更新检查**（`crates/sk-core/src/update/`，服务端 `crates/sk-server/src/updates.rs`，协议 `update.*`，界面在 `app/components/shell/UpdateBadge.vue` / `UpdateCard.vue`）：

- 后端每天最多一次读取 `https://api.github.com/repos/DuskXi/SpaceKnife/releases`（启动约 1 分钟后第一次；带 ETag，没变化时 GitHub 回 304；失败 6 小时后再试），不带令牌，只发 `User-Agent: SpaceKnife/<版本>`。设置「关于与更新 › 更新」可以关掉，或改成「只要正式版」（默认包括 GitHub 上标为预发布的版本）。命令面板「检查更新」随时检查；「关于与更新」页（`app/components/pages/AboutHeader.vue` 等）显示版本、组件版本和最近 30 个发布的时间线（`update.releases`）。
- 和正在运行的版本比较：只差修订号时只在标题栏右侧显示徽章；次版本号不同时左上角挂一张卡片（发布说明的概要、前往发布页、一键下载）；主版本号不同时在窗口内弹对话框。关掉卡片或对话框后这个版本不再自动弹出，徽章保留。次版本和主版本的卡片里还有「跳过 0.4.x」/「跳过 1.x」（`update.mute`）：整个系列（次版本是 major.minor，主版本是 major）的版本都不弹卡片、不弹对话框、不显示徽章，也不自动下载；记在 `update.json` 的 `muted` 里（最多 20 个，运行的版本已经到了的系列在下一次改动时去掉），「关于与更新」页仍显示这个版本并能「恢复提示」。
- 外壳启动进程内的后端时把运行版本和安装方式交给它（`ServeOptions` 的 `app_version`、`install_kind`：nsis / portable / appimage / deb / rpm / dmg / dev，由 `src-tauri/src/install.rs` 的 `install_kind` 判断，`SPACEKNIFE_INSTALL_KIND` 可以覆盖；`spaceknife-cli serve` 对应 `--app-version`、`--install-kind`）。下载的更新放在数据目录的 `updates` 里，外壳只认这个文件夹里的更新文件。演示和测试可以用 `SPACEKNIFE_UPDATE_FEED` 指向本机（只接受回环地址）的一份发布列表。
- 下载（「自动下载更新」开启时检查之后自动开始，否则点「一键下载」）按安装方式挑发布文件，存进 `updates` 文件夹，并用同一个发布里的 `SHA256SUMS*.txt` 校验，不符就删掉。之后更新卡片给出两个大按钮（`app/shared/install.ts` 的 `installStep` 决定每种安装方式各是什么）：「无缝更新」由 SpaceKnife 自己换上新版本再启动；经典方式交给安装程序、磁盘映像或安装包，或者在文件夹中显示下载的文件让用户自己替换。两者都只在用户点击后由外壳执行（`src-tauri/src/install.rs`），从不自己开始，以后也不会加自动安装的选项：
  - **NSIS**：
    - 「无缝更新」：外壳先停掉后端，再以 `/S /UPDATE /R /ARGS --updated` 运行新安装程序（静默、就地更新、装好后带 `--updated` 启动新版本），然后退出。
    - 「运行安装程序」：不带参数运行新安装程序（显示安装向导），SpaceKnife 随即退出。
  - **便携版**（Windows zip、Linux tar.gz、macOS zip）：
    - 「无缝更新」：换文件的是**新版本自己的可执行文件**（`spaceknife --apply-update <计划>`，`sk_core::update::swap`）。
      - 外壳把压缩包解到 `updates/staging-<版本>`，确认里面唯一的顶层文件夹带着可执行文件（macOS 上是 `SpaceKnife.app`）。
      - 写出计划 `updates/update-<版本>.json`（版本 1：`v`、`pid`、`app`、`new`、`old`、`exe`、`log`、`keep: ["data"]`）。计划是和每个旧版本的约定：旧应用把它交给更新的版本，所以以后的版本要一直能读 `v: 1`。0.3.x 的计划交给的是新版本的 `resources/backend/spaceknife-backend`，0.3.5 的包里没有它（见下面「发布时的约定」）。
      - 以 detached 方式启动 `<staging>/spaceknife[.exe] --apply-update <计划>`（macOS 上是 `SpaceKnife.app/Contents/MacOS/SpaceKnife`），等它在 stdout 回一行 `{"ok": true}` 后退出。30 秒没回、回了别的或者它先退出了，应用就照旧运行并提示原因。
      - 助手先检查计划：路径都是绝对路径、`exe` 在 `app` 里、新版本的同一位置有可执行文件、`old` 是空的。不能用的计划什么都不动：日志写一行 `not started: …`，回 `{"ok": false, "error": …}`，应用保持打开并提示原因。
      - 等应用主进程退出（最多 60 秒）。
      - 把程序目录里除 `data` 以外的条目移到 `updates/previous-<旧版本>`。文件还被占用时在 30 秒内反复重试，日志写「busy: …」。
      - 再把新条目移进来。助手自己就在新条目里运行，而 Windows 不让改名一个里面有程序在运行的文件夹，所以整体改不了名的逐个移动，连这也不行的就复制。
      - 带 `--updated` 启动程序，日志末行写「updated」。任何一步失败就把旧条目移回原处、启动旧版本，日志写「kept the old version」。
      - 每一步都写进 `updates/update-<版本>.log`，每行带 UTC 时间。
    - 「自己替换」：在文件夹中显示下载的压缩包，SpaceKnife 继续运行；用户退出后自己解压覆盖，保留 `data`。
  - **AppImage**：
    - 「无缝更新」：新文件替换 `$APPIMAGE`，然后启动它。
    - 「自己替换」：在文件夹中显示新的 AppImage，由用户退出后替换旧文件。
  - **dmg / deb / rpm**：没有无缝的办法，只有经典方式「打开磁盘映像」/「打开安装包」：交给系统打开（用户把新版拖进「应用程序」，或由软件安装器安装），SpaceKnife 随即退出。
  - 有扫描在进行时，会关闭 SpaceKnife 的按钮第一次点只提示「还有扫描在进行，更新会中断它（已扫描的部分不会保存）」，按钮改写「再点一次继续」，第二次点才执行；「自己替换」不关闭应用，不问。
  - 新版本带 `--updated` 启动时，界面提示一次「已更新到 SpaceKnife <版本>」。下一次启动后端清理 `updates`，旧安装包、`staging`、`previous`、`update-<版本>.json` 和日志都会删掉。
  - 外壳只启动 `updates` 文件夹里、按本次安装方式的发布文件命名的文件（`install.rs` 的 `install_refusal`）。
- **发布时的约定**：更新检查靠文件名找文件。发布文件必须沿用下面的名字，并附上对应的校验和文件：
  - `SpaceKnife_<版本>_x64-setup.exe`、`SpaceKnife_<版本>_amd64.AppImage`、`SpaceKnife_<版本>_amd64.deb`、`SpaceKnife-<版本>-1.x86_64.rpm`、`SpaceKnife_<版本>_aarch64.dmg`、`SpaceKnife_<版本>_x64.dmg`、`SpaceKnife-<版本>-win-x64-portable.zip`、`SpaceKnife-<版本>-win-arm64-portable.zip`、`SpaceKnife-<版本>-linux-x64-portable.tar.gz`、`SpaceKnife-<版本>-linux-arm64-portable.tar.gz`、`SpaceKnife-<版本>-mac-arm64-portable.zip`、`SpaceKnife-<版本>-mac-x64-portable.zip`；
  - `SHA256SUMS.txt`（安装包）、`SHA256SUMS-portable-<目标>.txt`（目标是 `win-x64`、`win-arm64`、`linux-x64`、`linux-arm64`）、`SHA256SUMS-mac-<架构>.txt`（dmg 和 macOS 便携 zip）。（`tools/release.mjs assemble` 检查并合并：两份 `SHA256SUMS-installers-<目标>.txt` 合成 `SHA256SUMS.txt`）
  - tag 是 `v<版本>`（语义化版本，可带预发布段）。改名之前先改 `crates/sk-core/src/update/mod.rs` 的 `asset_templates` 和外壳的 `install_refusal`。
  - 0.3.5 改了安装包的文件名：0.3.x（Electron 版）的更新检查按旧名字找文件，找不到 0.3.5 的安装包，0.3.x 的用户要手动下载 0.3.5。<!-- TODO: confirm -->
- **累积更新**：还没发布的更改随做随记在仓库根目录的 [UNRELEASED.md](../UNRELEASED.md)（中英双语，「新功能」「修复」两节，和发布说明同一个结构）。两份 README 的顶部导航里固定链接到它。
  - 发布时，它就是发布说明的正文：再补上下载表和已知限制。
  - 发布之后，在 dev 上把它清空：只留标题、语言切换、开头那句说明（版本号改成下一版），以及每节下面的「（暂无）」/「(nothing yet)」。
- **发布**（从 0.3.4 起由 CI 发布，文件不经过任何人的电脑）：
  1. dev 上改版本号（根目录 `Cargo.toml` 的工作区版本、`apps/desktop/package.json`、`apps/desktop/src-tauri/tauri.conf.json`、`app/shim.ts` 的网页版版本、问题模板的占位符），把 `UNRELEASED.md` 压成 `release-notes/<版本>.md`：先英文后中文，下载表的大小写成 `{{size:文件名}}`，构建运行写成 `{{build_runs}}`，也可以用 `{{version}}`；工作流填进去，写错的占位符或不属于这个发布的文件名会让发布失败。
  2. PR dev → main，等「CI passed」再合并。
  3. 在 main 的合并提交上本地签名打 tag（`git tag -s v<版本> <提交> -m "SpaceKnife <版本>"`）并推送。CI 不持有签名的私钥。
  4. tag 推上去就触发 **Release** 工作流（`.github/workflows/release.yml`）：
     - 检查 tag 是带签名的附注 tag 且 GitHub 认可签名、文件和 main 完全相同、应用的版本号就是 tag 的版本、`release-notes/<版本>.md` 存在、同名的发布还没有；
     - 用 `Portable build`（带安装包）和 `macOS build` 从 tag 构建全部文件，两者都给每个文件生成构建来源证明（`actions/attest-build-provenance`，可以用 `gh attestation verify <文件> --repo DuskXi/SpaceKnife` 核对）；
     - `node tools/release.mjs assemble` 核对每份校验和文件、确认正好是这个发布该有的文件（上面的名单），把两份安装包的校验和合并成 `SHA256SUMS.txt`，填好发布说明；汇总结果也作为运行的产物 `release-<版本>` 留 14 天；
     - 建成预发布的草稿（0.x 和带预发布段的版本都是预发布）。
  5. 在 GitHub 上看一眼草稿，点发布。**Release check** 工作流（`release-check.yml`）在发布时用 `cargo run -p sk-core --example release_check -- <版本>` 按已安装的 SpaceKnife 的方式读这个发布：它是自己渠道里最新的，每种安装方式都按确切的名字找到文件，每个文件下载下来和应用会读到的校验和相同（`--download`）。
  - 手动运行 Release（Actions › Release › Run workflow）给已经在 GitHub 上的 tag 发布。可以填同一份文件树的 `Portable build`（带安装包）和 `macOS build` 运行号，直接用它们的文件，不再构建（工作流核对两个运行成功、构建的提交和 tag 的文件完全相同；这时没有构建来源证明，发布说明链接到那两个运行）。`dry_run` 只核对和汇总，不建发布；`publish` 不留草稿，直接发布并立刻读回检查。
  - 两者在本机也能跑：`node tools/release.mjs assemble` 接 `gh run download` 下载的产物；`cargo run -p sk-core --example release_check -- <版本> [--sums DIR] [--download]` 读真实的发布列表。`tools/release.mjs` 的测试在 `tools/tests/`（`node --test "tools/tests/*.test.mjs"`）。

**数据位置**（`SPACEKNIFE_HOME` 会把这些全部放进同一个目录；后端的位置和 0.3.x 相同，`crates/sk-core/src/paths.rs`）：

| | Windows | Linux | macOS |
|---|---|---|---|
| 快照、标签、日志（数据） | `%LOCALAPPDATA%\SpaceKnife` | `~/.local/share/SpaceKnife` | `~/Library/Application Support/SpaceKnife` |
| 设置、文件类别、洞察规则 | `%LOCALAPPDATA%\SpaceKnife` | `~/.config/SpaceKnife` | `~/Library/Application Support/SpaceKnife` |
| 缓存 | `%LOCALAPPDATA%\SpaceKnife\Cache` | `~/.cache/SpaceKnife` | `~/Library/Caches/SpaceKnife` |
| 窗口外壳（主题、WebView 配置） | `%LOCALAPPDATA%\io.github.duskxi.spaceknife` | `~/.local/share/io.github.duskxi.spaceknife` | `~/Library/Application Support/io.github.duskxi.spaceknife` |

设了 `SPACEKNIFE_HOME` 时，窗口外壳的文件在 `<SPACEKNIFE_HOME>/desktop`。0.3.x 的 Electron 配置目录（`%APPDATA%\SpaceKnife\electron` 等）0.3.5 不再使用。

## 5. 测试

```bash
node packaging/smoke-app.mjs <打包好的应用>
```

[packaging/smoke-app.mjs](../packaging/smoke-app.mjs) 最小化启动打包好的应用（`--portable`：便携版，用它旁边的 `data`；否则用临时目录作 `SPACEKNIFE_HOME`），从 `SPACEKNIFE_READY_FILE` 读进程内后端的地址和 token，`session.hello`、`volume.list` 列出卷、`update.status` 的安装方式（`--kind`，便携版默认 `portable`），再像系统那样关掉应用（WM_CLOSE / SIGTERM，不响应就强制结束），确认后端的地址随之关闭；便携版还要在 `data` 里看到应用写的东西。窗口本身不检查：Linux 和 macOS 上的系统 WebView 没有 DevTools 端口。Linux 上用 `xvfb-run -a`。

```bash
node packaging/smoke-headless.mjs <spaceknife-cli> [--ui <界面文件夹>]
```

[packaging/smoke-headless.mjs](../packaging/smoke-headless.mjs) 测无头模式：`spaceknife-cli headless --listen 127.0.0.1:0` 启动，读打印出的地址，检查页面、`/sk/info` 只认 token、WebSocket 拒绝别的 Origin（403）、用页面自己的 Origin 能和后端握手，再像终端一样停止（Windows 上 Node 发不了 Ctrl+C，直接结束进程），确认地址已关闭。

```bash
node packaging/update-e2e.mjs target/portable/SpaceKnife-<v>-<目标>-portable.<zip|tar.gz>
```

[packaging/update-e2e.mjs](../packaging/update-e2e.mjs) 是便携版「无缝更新」的端到端测试（`Portable build` 和 `macOS build` 工作流里跑）。它在 127.0.0.1 上提供一份发布列表（`SPACEKNIFE_UPDATE_FEED`），把这次构建改名、加一个标记文件当作下一个修订版，附上 `SHA256SUMS`；通过应用后端的 WebSocket 确认找到了这个版本和它的文件、下载并校验通过，然后做窗口里「无缝更新」按钮做的事（解包到 updates 文件夹、写交换计划、启动新版本的 `--apply-update`，它回答 `{"ok": true}`），关掉应用；新版本换好文件后带 `--updated` 重新启动应用：程序文件换成了新的、`data` 原样保留、旧文件从 updates 文件夹清掉。按钮本身和解包、写计划的代码由 `install.rs` 的单元测试覆盖。

[packaging/nsis-e2e.mjs](../packaging/nsis-e2e.mjs)（只在 GitHub Actions 上跑，带安装包的 `Portable build`）：静默安装 Windows 安装包（`--previous` 时先装旧版再升级），以 `--kind nsis` 跑一遍 smoke-app，再静默卸载，确认程序已删除。

其余测试：`cargo test --workspace`、`apps/desktop` 里的 `pnpm test`、`node --test "tools/tests/*.test.mjs"`。

## 6. 已知限制

- 未签名（Windows 代码签名证书待定）；macOS 只有 ad-hoc 签名、没有公证（等 Apple 开发者账号），也还没进 Homebrew（官方 cask 要求通过 Gatekeeper）。
- 项目主页：`https://github.com/DuskXi/SpaceKnife`。
- 「仅当前用户」安装在用户可写的目录里；需要管理员权限的 MFT 快扫会从这里启动提权助手。
- 0.3.x（Electron 版）不能自动更新到 0.3.5：它的更新检查按旧的文件名找，找不到 Tauri 命名的安装包；要手动下载一次。<!-- TODO: confirm — 是否在 0.3.5 的发布里另放旧名字的文件来接住 0.3.x，由维护者决定 -->
- 不会自动安装（有意如此）：有新版本时提示并在后台下载（见第 4 节「更新检查」），安装总要用户在更新卡片里点「无缝更新」或经典方式，以后也不会加自动安装的选项。
