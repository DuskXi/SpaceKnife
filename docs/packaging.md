# 打包与发布

0.3.5 起，同一份界面（`apps/desktop` 的 Nuxt 构建）有两个外壳：默认是 **Tauri 外壳**（`apps/desktop/src-tauri`，界面跑在系统已有的网页视图里，后端在应用进程里运行）；**Electron 外壳**（`apps/electron-shell`，后端是子进程 `spaceknife-cli serve`）只用在 Tauri 版跑不了的地方：RHEL 9 的 rpm（RHEL 9 没有 WebKitGTK 4.1）、AppImage，以及沿用 0.3.4 文件名的 Linux 便携版（0.3.4 在 RHEL 9 上的便携版也要能更新上来）。后端是 Rust（`crates/sk-server`），不再有冻结的 Python 后端；命令行程序 `spaceknife-cli` 装在应用旁边。macOS 的包是 ad-hoc 签名、未公证（Developer ID 签名与公证等 Apple 开发者账号，见 [TODO.md](../TODO.md)）。0.3.x 的打包方式见 git 历史与 [release-notes/0.3.4.md](../release-notes/0.3.4.md)。

| 产物 | 平台 | 外壳 |
|---|---|---|
| `SpaceKnife-<版本>-Setup-x64.exe` | Windows 10+ x64 的安装包：NSIS，fork 的 Tauri 模板，接管 0.3.4 electron-builder 的安装（同一个卸载项 GUID、目录和快捷方式，第 5 节），未签名；Windows on ARM 上模拟运行 | Tauri |
| `SpaceKnife-<版本>-win-x64-portable.zip`、`SpaceKnife-<版本>-win-arm64-portable.zip` | Windows 便携版 | Tauri |
| `spaceknife_<版本>_amd64.deb` | Ubuntu 22.04+ / Debian 系，包名 `spaceknife` | Tauri |
| `spaceknife-<版本>.x86_64.rpm` | RHEL 9+ / Fedora 系，包名 `spaceknife` | Electron |
| `SpaceKnife-<版本>-x86_64.AppImage`（和 `.AppImage.zsync`） | Linux x64，免安装 | Electron |
| `SpaceKnife-<版本>-linux-x64-portable.tar.gz`、`SpaceKnife-<版本>-linux-arm64-portable.tar.gz` | Linux 便携版，0.3.4 的名字：0.3.4 的 Linux 便携版都更新到它，glibc 2.34+ 就能用（RHEL 9 也行） | Electron |
| `SpaceKnife-<版本>-linux-x64-tauri.tar.gz`、`SpaceKnife-<版本>-linux-arm64-tauri.tar.gz` | Linux 轻量便携版，要系统的 WebKitGTK 4.1；0.3.5 新增，0.3.4 永远拿不到它 | Tauri |
| `SpaceKnife-<版本>-mac-arm64.dmg`、`SpaceKnife-<版本>-mac-x64.dmg` | macOS 14+，Apple Silicon / Intel | Tauri |
| `SpaceKnife-<版本>-mac-arm64-portable.zip`、`SpaceKnife-<版本>-mac-x64-portable.zip` | macOS 便携版：`SpaceKnife.app` 旁边一个 `data` 文件夹 | Tauri |

每个文件的大小写在那个版本的发布说明里：发布说明写 `{{size:文件名}}`，Release 工作流按真正发布的文件填进去（第 9 节）。macOS 的最低版本是 14（`tauri.conf.json` 的 `bundle.macOS.minimumSystemVersion`，与 0.3.x 相同）。

文件名是 0.3.4 的命名，以后每个版本都沿用（第 9 节「发布时的约定」）：更新检查按这些名字找文件，唯一的来源是 `crates/sk-core/src/update/mod.rs` 的 `asset_templates`，`tools/release.mjs` 的 `expected` 按它发布。唯一的新名字是 0.3.4 没有的那种安装方式，Tauri 版的 Linux 便携包 `SpaceKnife-<版本>-linux-<架构>-tauri.tar.gz`（`expected` 里标着 `since: "0.3.5"`）：它不以 `-linux-<架构>-portable.tar.gz` 结尾，0.3.4 的模式（`{v}` 换成 `[0-9A-Za-z.+~_-]+`、整串匹配、区分大小写）怎么也匹配不到它，`tools/compat034.mjs` 用 0.3.4 自己的 `pick_asset` 核对；它的校验清单 `SHA256SUMS-tauri-linux-<架构>.txt` 里的词（tauri）不在 0.3.4 的文件名里，0.3.4 照旧先读 `SHA256SUMS-portable-linux-<架构>.txt`。两种 Linux 便携版各自只更新到同一种（外壳经 `SPACEKNIFE_SHELL` 告诉后端，`asset_templates` 按外壳给名字）。Tauri 打包器自己的名字（`SpaceKnife_<版本>_amd64.deb`、`SpaceKnife_<版本>_x64-setup.exe` 之类）由工作流改成上面的名字：deb 由 `tools/deb.mjs` 重新打包成包名 `spaceknife`，安装包由 `tools/release.mjs windows-installer` 改名，dmg 在 `macos.yml` 里改名。

## 1. 构建

需要：rustup（仓库根目录的 `rust-toolchain.toml` 固定 Rust 的版本（edition 2024）和 clippy、rustfmt，cargo 第一次在仓库里运行时装上；CI 的每个工作流用 `rustup toolchain install` 装同一个）、Node 24 与 pnpm 10。Linux 上还要 Tauri 的系统库：`libwebkit2gtk-4.1-dev`、`libgtk-3-dev`、`libayatana-appindicator3-dev`、`librsvg2-dev`、`libssl-dev`、`patchelf`。

**Tauri 外壳**：

```bash
cd apps/desktop
pnpm install --frozen-lockfile
pnpm bundle            # tauri build：Windows 上 NSIS，Linux 上 deb（还能出 AppImage、rpm，不发布），macOS 上 app + dmg
pnpm bundle:portable   # 在 bundle 之后：target/portable/ 下的便携包
```

在哪个系统上运行就出哪个系统的包（`src-tauri/tauri.{windows,linux,macos}.conf.json` 定目标），一律为构建机自己的架构。Linux 上发布的只有 deb（CI 用 `pnpm bundle --bundles deb`），rpm 和 AppImage 由 Electron 外壳出。Windows 和 Linux 的安装包只发 x64；arm64 只发便携包。安装包在 `target/release/bundle/<格式>/`，便携包在 `target/portable/`。

`pnpm bundle`（[scripts/bundle.mjs](../apps/desktop/scripts/bundle.mjs)）是带路径重映射（下文「不带构建机路径」）的 `tauri build --config src-tauri/tauri.bundle.json`（Windows 上再加 `--config src-tauri/tauri.bundle.windows.json`：fork 的 NSIS 模板、`SpaceKnife.com`、WebView2 的安装方式和最低版本，第 5 节；macOS 上加 `tauri.bundle.macos.json`），后面的参数照传（CI 只出程序时用 `pnpm bundle --no-bundle`）。它的 `beforeBuildCommand` 先 `pnpm generate`（界面的静态文件 `.output/public`，嵌进应用），再运行 [scripts/prepare-bundle.mjs](../apps/desktop/scripts/prepare-bundle.mjs)：以 release 编译 `spaceknife-cli`，放成 `src-tauri/target-bundle/spaceknife-cli-<目标三元组>[.exe]` 作为 Tauri 的 `externalBin`（装在应用旁边，名字是 `spaceknife-cli`）；写出许可证 `src-tauri/target-bundle/licenses/`；Windows 上编 `SpaceKnife.com`（`packaging/console-launcher`）；macOS 上复制并签名替换助手。这些不放在基础配置 `tauri.conf.json` 里：Tauri 的构建脚本要求 externalBin 和资源已经存在，否则普通的 `cargo build` 和 `pnpm tauri dev` 都会失败。Windows 的打包配置 `tauri.bundle.windows.json` 设了 `useLocalToolsDir`：打包器用的 NSIS 3.13、nsis_tauri_utils 和 WebView2 引导程序都在 `target/.tauri`，不碰 `%LOCALAPPDATA%\tauri`。这些工具由 `node tools/nsis_fork.mjs tools <目录>` 事先准备：下载失败时重试（最多 5 次，间隔加倍），按 `windows/upstream.json` 里钉住的 SHA-1 核对，每次都从核对过的压缩包重新解出 NSIS；CI 用 actions/cache 按版本和 SHA-1 缓存下载。打包器发现 `NSIS.exe` 的 SHA-1 不符时会把那个文件夹整个删掉重下，所以不要手改里面的文件。

`pnpm bundle:portable`（[scripts/portable.mjs](../apps/desktop/scripts/portable.mjs)）把 `target/release` 里的程序（Windows 上叫 `SpaceKnife.exe`，Linux 上是 `spaceknife`）与 `spaceknife-cli`、替换助手 `resources/backend/spaceknife-backend[.exe]`（`spaceknife-cli` 的一份副本，Linux 上可执行）、许可证、无头模式用的界面（`ui`）和一个空的 `data` 文件夹（里面一份中英文说明）放进一个文件夹，Windows 上打成 zip（系统自带的 bsdtar），Linux 上打成 `-tauri.tar.gz`；macOS 上是 `pnpm bundle` 做出的 `SpaceKnife.app`（可执行文件 `Contents/MacOS/SpaceKnife`；`spaceknife-cli` 在包里；替换助手是包里的资源 `Contents/Resources/backend/spaceknife-backend`，用应用的身份签名）加旁边的 `data`，用 `ditto` 打成 zip（保留包内的链接、属性和签名）。程序名和替换助手是给 0.3.x 便携版的：它的「无缝更新」按确切的名字找新版本里的 `SpaceKnife.exe` / `spaceknife` / `SpaceKnife.app`，再运行 `resources/backend/spaceknife-backend --apply-update <计划 v1>`。

**Electron 外壳**：`pnpm -C apps/electron-shell package [electron-builder 参数]`（[scripts/package.mjs](../apps/electron-shell/scripts/package.mjs)）先用 `apps/desktop/scripts/prepare-bundle.mjs` 编 `spaceknife-cli`、写许可证，放进 `target/electron-shell/stage`；界面没有构建时先 `pnpm generate`；然后 `electron-vite build` 和 `electron-builder`，产物在 `target/electron-shell/dist`。配置是 [electron-builder.yml](../apps/electron-shell/electron-builder.yml)：与 0.3.4 同一个 appId、产品名、可执行文件名和 rpm 包名。发布的只有 Linux 的 rpm 和 AppImage（CI 在 Linux x64 上运行 `pnpm run package`，electron-builder 按 `linux.target` 出这两种）；`win` / `nsis` 那一段只给在 Windows 上本地试 Electron 外壳用，CI 不构建、不发布——Windows 的安装包是 Tauri 外壳的（第 5 节）。`publish: null`：没有发布目标，也就不写更新信息（`latest.yml`、`app-update.yml`），更新检查是 SpaceKnife 自己的；不写的话，环境里有 `GH_TOKEN`（CI 为 `gh release download` 设的）时 electron-builder 会去找 GitHub 仓库发布，找不到（「Cannot detect repository by .git/config」）就出错退出（`src/main/builder.test.ts` 用 electron-builder 自己的代码核对）。

Linux 便携版：`pnpm -C apps/electron-shell package:portable`（[scripts/portable.mjs](../apps/electron-shell/scripts/portable.mjs)，在 `package` 之后；ARM 上先 `node scripts/package.mjs --linux dir --arm64`）把 electron-builder 解开的应用（`dist/linux-unpacked` 或 `linux-arm64-unpacked`：Electron 程序 `spaceknife`、旁边的 `spaceknife-cli`、`resources/ui`、`resources/licenses`）加上替换助手 `resources/backend/spaceknife-backend`（`spaceknife-cli` 的副本，可执行）和空的 `data` 文件夹，打成 `target/electron-shell/portable/SpaceKnife-<版本>-linux-<x64|arm64>-portable.tar.gz`（成员属主按数字记为 root）。这是 0.3.4 的 Linux 便携版要的样子：顶层一个文件夹，里面有确切名字的 `spaceknife` 和 `resources/backend/spaceknife-backend`。

CI：「Portable build」（[portable.yml](../.github/workflows/portable.yml)）在 Windows 与 Linux 的 x64 / arm64 运行器上构建 Tauri 的便携包（Linux x64 上同时出 deb）并跑冒烟测试；「macOS build」（[macos.yml](../.github/workflows/macos.yml)）在 arm64 与 Intel 运行器上出 dmg 与便携 zip；「Windows installer build」（[windows-installer.yml](../.github/workflows/windows-installer.yml)）出 Windows 安装包；「Electron shell build」（[electron.yml](../.github/workflows/electron.yml)）出 Electron 外壳的 rpm、AppImage 和 Linux 便携版；「Release」（[release.yml](../.github/workflows/release.yml)）从 tag 调用这四个工作流并建发布（第 9 节）。

**许可证**：SpaceKnife 是 MIT（仓库根目录 `LICENSE`）。装好的 `licenses/`（Electron 外壳是 `resources/licenses/`）里有 `LICENSE.txt`、`THIRD-PARTY-rust.txt`（`spaceknife` 与 `spaceknife-cli` 链接的 crate，来自 `cargo metadata`）和 `THIRD-PARTY-ui.txt`（界面的 npm 生产依赖，来自 `pnpm licenses list --prod`，加上 Geist / Noto Sans SC 字体的 OFL）；相同的许可证文本只印一次。由 `prepare-bundle.mjs` 生成。

**不带构建机路径**：Rust 的 panic 信息和调试路径里会带上构建机的绝对路径（也就带上了用户名）。所以发布用的构建（`pnpm bundle`，`prepare-bundle.mjs` 编的 `spaceknife-cli` 用同一组参数）给 rustc 加上 `--remap-path-prefix`：家目录、`CARGO_HOME`、`RUSTUP_HOME` 和仓库分别写成 `~`、`/cargo`、`/rustup`、`/spaceknife`（Cargo 的 trim-paths 还没稳定）；Windows 上再加 `-Clink-arg=/PDBALTPATH:%_PDB%`，程序里只留 PDB 的文件名（`tools/package_checks.mjs` 的 `remappedEnv`）。直接运行 `pnpm tauri build` 或 `cargo build --release` 不做这一步，只适合本机自用。做好的包由 `node tools/package_checks.mjs paths <文件或文件夹>…` 检查：任何文件里都不能出现构建机的家目录（UTF-8 和 UTF-16LE、两种斜杠、JSON / JS 转义、ASCII 字母不分大小写，后面不能紧跟别的名字字符），zip、tar、gzip、zstd 和 ar（deb）里的成员也看，成员里的压缩包再往里看；tar 成员的属主也不能是构建机的用户。读不了的格式（xz 等）算失败，不算通过。

**glibc 下限**：Linux 的包里每个 ELF 文件都只能要 glibc 2.34 及以下（RHEL 9；Ubuntu 22.04 是 2.35）：`node tools/package_checks.mjs glibc <文件或文件夹>…`，和上面读同样的成员。判断是保守的：文件里出现的任何 `GLIBC_x.y` 都算；一个 ELF 文件都没有也算失败（多半是给错了文件夹）。

**C 运行库静态链接**：安装包和便携版都不带 Visual C++ 运行库（Redistributable），所以 Windows 上发布的每个程序——`SpaceKnife.exe`、`spaceknife-cli.exe`、`SpaceKnife.com`、`resources/backend/spaceknife-backend.exe`——都把整个 C 运行库链接进程序（`+crt-static`）：仓库根目录的 `.cargo/config.toml` 给所有 MSVC 目标的构建（测试、开发构建也一样）加 `-C target-feature=+crt-static`；发布构建设了 `CARGO_ENCODED_RUSTFLAGS`，Cargo 就不再读那张表，所以 `remappedEnv` 自己也加上这个参数（`tools/package_checks.mjs` 的 `STATIC_CRT`）。Tauri 的构建脚本默认只静态链接 VC 运行库、UCRT 仍从系统取（`staticVCRuntime`），开了 `+crt-static` 时 `src-tauri/build.rs` 把它关掉，否则它会把 UCRT 又换回 `api-ms-win-crt-*.dll`。检查：`node tools/package_checks.mjs crt [--allow-ucrt 文件名 …] <文件或文件夹>…` 读每个 PE 文件（exe、dll、com，和上面读同样的成员）的导入表和延迟加载表，导入 `VCRUNTIME*`、`MSVCP*`、`api-ms-win-crt-*`、`ucrtbase`（以及更早的 `MSVCR*`、`CONCRT*`、`VCOMP*`）的算失败；`--allow-ucrt` 点名的别人预编译的文件可以从系统取 UCRT（Windows 10 起自带），VC 运行库仍然不行。一个 PE 文件都没有也算失败。

三项检查在 CI 里跑：「Portable build」检查每个便携包（glibc 只在 Linux 上，C 运行库只在 Windows 上）和 deb；「Windows installer build」用 7-Zip 解开安装包（程序文件在以暂存变量命名的 `$_<n>_` 文件夹里），查路径和 C 运行库；「Electron shell build」检查它的 rpm（经 rpm2cpio）、AppImage（`--appimage-extract`）和 Linux 便携版；「macOS build」检查便携 zip 和 dmg 里的应用（只读挂载）。测试在 `tools/tests/package_checks.test.mjs`。rpm 包头里会记下构建机的主机名（Build Host）：没有 root 时用 `apt-get download` 取 Ubuntu 官方的 rpm 工具、`dpkg-deb -x` 解到用户目录，可以在 `RPM_CONFIGDIR/macros.d/` 里放一个定义 `%_buildhost localhost` 的宏文件。

## 2. 后端与命令行

- Tauri 外壳里后端就在应用进程里：`src-tauri/src/backend.rs` 调用 `sk_server::start`，在随机的回环端口上提供协议（每次启动一个新 token），界面连上它（[protocol.md](protocol.md) §1）。Electron 外壳的后端是子进程 `spaceknife-cli serve --token-stdin`（第 4 节）。
- 提权的扫描助手（`sk_core::helper`）是应用旁边的 `spaceknife-cli helper …`，从来不是窗口程序；`<程序> --apply-update <计划>` 是便携版的「无缝更新」（`sk_core::update::swap`，第 8 节），在 `src-tauri/src/main.rs` 里、加载任何东西之前分派。
- 命令行：`spaceknife-cli`（`crates/sk-server/src/bin/spaceknife-cli.rs`，`cli/mod.rs`）装在应用旁边：`scan`、`run`、`snapshot list | show`、`serve`（给界面用的 WebSocket 后端，`--ready-file` 等）、`headless`、`version`；0.3.4 应用自己的写法 `--headless` / `--listen <地址>` 仍然接受，没有命令打头时可以出现在任何位置，其余参数照 0.3.4 跳过（`cli/headless.rs` 的 `is_app_form`）。输出格式与 0.3.4 的 `scan` 逐字逐句相同（`crates/sk-server/src/cli/rich.rs`），外部基准（Disk-Scan-Benchmark）照旧解析；「最大的文件」用部分选择（第 k 大的值，并列时编号小的在前），不对全部文件排序。
- 安装目录本身是受保护路径（不能在应用里删除或移动它）。
- **就绪文件**（`SPACEKNIFE_READY_FILE`：端到端测试、演示，以及替换助手确认新版本已启动，第 8 节）：后端开始监听时把地址和 token 写进这个文件（只有当前用户能读；先写一个新建的临时文件——不能是已有的文件或链接，Unix 上另加 O_NOFOLLOW——再改名替换，不跟随链接；Unix 上别的用户的文件夹拒绝写，`cli/serve.rs` 的 `write_ready_file`）。两个外壳启动时就把这个变量从自己的环境里拿掉，只交给第一次启动的后端（Tauri：`main.rs` 在任何线程之前读出并删掉，`backend.rs` 写一次就忘，`crates/sk-server/tests/environment.rs` 只放行这一处改环境；Electron：`index.ts` 一开始 `takeReadyFile(process.env)`，只给第一个后端加 `--ready-file`）：之后启动的后端、更新的助手和安装程序、终端都不会继承一个可能已被别人重建的路径。

## 3. Tauri 外壳

- 应用在 `apps/desktop`：Nuxt 4 + Vue 3 的界面（`app/`），Tauri 2 的外壳（`src-tauri/`，crate `spaceknife-desktop`，可执行文件在 Windows 和 macOS 上叫 `SpaceKnife`，和 0.3.4 一样——0.3.4 的「无缝更新」按这个名字找新版本；Linux 上叫 `spaceknife`；`tauri.{windows,macos}.conf.json` 的 `mainBinaryName`）。见 [apps/desktop/PORTING.md](../apps/desktop/PORTING.md)「Commands」「Bundles」。`identifier` 是 `io.github.duskxi.spaceknife`。
- 界面跑在系统的 WebView 里（Windows WebView2、macOS WKWebView、Linux WebKitGTK 4.1），不再自带 Chromium。Windows 上要 WebView2 Runtime 89.0.774.44 或更新（wry 用 ICoreWebView2Controller2 设 WebView 的背景色，没有它窗口建不起来；`startup_check.rs` 的 `MIN_WEBVIEW2`，`tauri.bundle.windows.json` 的 `minimumWebview2Version`，`tests/config.rs` 核对两者相同）。
- 外壳自己的文件（`theme.json`、WebView 的配置目录）：设了 `SPACEKNIFE_HOME`（演示、测试）或是便携版时在 `<SPACEKNIFE_HOME 或便携版的 data>/desktop`，否则在 Tauri 的应用本地数据目录（按 identifier 命名），和后端的文件分开（`src-tauri/src/window.rs` 的 `ui_dir`）。自己的 `theme.json` 还没有时（从 0.3.4 升级后的第一次启动），读一次 0.3.4 的 Electron 版留下的 `theme.json`（同样的格式）：设了 `SPACEKNIFE_HOME` 或是便携版时是 `<那个目录>/electron/theme.json`，否则是系统配置目录（`%APPDATA%`、`~/Library/Application Support`、`$XDG_CONFIG_HOME` 或 `~/.config`）下的 `SpaceKnife/electron/theme.json`（`theme.rs` 的 `electron_theme_file`、`ThemeKeeper::restore`）；旧文件不动，自己的文件在页面发来第一个设置时写出。
- 便携版的 `data`：打包后的应用启动时，可执行文件旁边（macOS 上是 `SpaceKnife.app` 旁边）有 `data` 文件夹，就像设了 `SPACEKNIFE_HOME` 那样用它（`src-tauri/src/portable.rs`，`lib.rs` 的 `prepare_environment` 调 `sk_core::paths::set_home`；不改进程的环境变量，程序启动的子进程不会继承它）。Tauri 的进程不设 `SPACEKNIFE_SHELL`：后端没有它时就是 Tauri 外壳（`sk_core::update::shell`）。
- macOS 的 `Info.plist` 带着「文件与文件夹」授权提示里显示的理由（和 0.3.4 的 `electron-builder.yml` `mac.extendInfo` 逐字相同）：`src-tauri/Info.plist`，由 `tauri.macos.conf.json` 的 `bundle.macOS.infoPlist` 合并进包里；`tests/config.rs` 守着它和最低版本，「macOS build」在做好的包里再查一遍。
- 拖动窗口的元素带 `data-tauri-drag-region`。窗口只显示界面自己的页面（`src-tauri/src/navigation.rs`：`tauri://localhost`、`http(s)://tauri.localhost`，开发时加上 dev server），不开新窗口，项目的网页交给浏览器；CSP 在 `tauri.conf.json`，内联脚本的哈希由 Tauri 加上。
- Windows 上 release 构建的 `SpaceKnife.exe` 是 GUI 子系统；终端里用的无头模式是控制台程序 `spaceknife-cli headless`。0.3.x 的 `SpaceKnife.com`（`packaging/console-launcher`，自成一个小工作区，`prepare-bundle.mjs` 在 Windows 上以重映射的路径构建，`portable.mjs` 放进便携版，安装包经 `tauri.bundle.windows.json` 的 `resources` 放到安装目录）照旧放在程序旁边：命令提示符和 PowerShell 先找 `.com`，所以终端里的 `SpaceKnife --headless …`、`--listen …`、`--help`（这些选项在任何位置）到了它这里，它以原样的参数运行旁边的 `spaceknife-cli.exe` 并等它结束（Y/N 在终端里问，Ctrl+C 交给 `spaceknife-cli`，退出码照传）；别的参数打开窗口，并和 0.3.4 的 `SpaceKnife.com` 一样等窗口关掉、照传它的退出码：在终端里不给窗口程序标准句柄、设 `SPACEKNIFE_CONSOLE=1`，窗口程序自己附到这个控制台（`src-tauri/src/console.rs`），启动器自己忽略 Ctrl+C；重定向时照原样给句柄，Ctrl+C 由启动器结束窗口程序（退出码 130）。和 0.3.4 不同的一处是修正：Tauri 外壳收到 Ctrl+C 时像点了窗口的关闭按钮那样退出（扫描停下、后端写完要保存的东西，退出码 130）。
- 0.3.4 的无头启动方式（`SpaceKnife --headless`、`--listen <地址>`、`--help`，参数在任意位置）不开窗口，在本进程里运行无头模式（`main.rs`，`src-tauri/src/cli.rs`）：读参数的是 `spaceknife-cli` 读 0.3.4 写法的同一段代码（`sk_server::cli::headless` 的 `wants_headless`、`cli(…, Form::App)`），所以和 `spaceknife-cli --headless …`、`SpaceKnife.com`、Electron 外壳逐个情况一致。
- 「无缝更新」换上新版本之前，替换助手问新版本的程序能不能在这台机器上启动：`<新程序> --startup-check`（`main.rs` 最先处理，`src-tauri/src/startup_check.rs`）。到这一步系统的加载器已经给了程序它链接的所有库（Linux 上的 WebKitGTK 4.1 和 GTK 等），剩下要问的是 WebView 的运行时：Windows 上用 WebView2 加载器的版本查询（`GetAvailableCoreWebView2BrowserVersionString`），没装或低于 89.0.774.44 都算不能启动。它在 stdout 回一行 JSON（`{"ok":true,"webview":…}`，或 `{"ok":false,"error":…}` 并以 1 退出），不开窗口和 WebView，不读写应用的任何设置和文件。

## 4. Electron 外壳

- 只有主进程和 preload（`src/main`、`src/preload`，electron-vite 构建到 `out/`），没有自己的界面。窗口从 `app://spaceknife` 提供 `apps/desktop/.output/public`（打包后是 `resources/ui`），页面带 Content-Security-Policy 头，内联脚本的 sha256 按页面计算（`src/main/csp.ts`），指令与 Tauri 的相同。来源和 userData（`<appData>/SpaceKnife/electron`，设了 `SPACEKNIFE_HOME` 或是便携版时是 `<那个目录>/electron`）与 0.3.4 相同，页面的存储和 `theme.json` 都还在。
- preload 只暴露 `window.skElectron`（版本信息、白名单内的 IPC 通道、拖放文件的路径）；界面的 `app/lib/electronHost.ts` 在它上面实现 `window.sk`，`app/lib/installHost.ts` 依次选：已有的 `window.sk`、Electron、Tauri、浏览器 shim。
- 后端是子进程 `spaceknife-cli serve --token-stdin --log-format plain`（与 0.3.4 的 Python 后端同一个契约：令牌走 stdin，stdout 一行 ready，stdin 关闭即退出，`src/main/backend.ts`）；版本、安装方式、系统语言和 `SPACEKNIFE_SHELL=electron` 经环境变量传；就绪文件只给第一个后端（第 2 节）。
- Windows 和 Linux 上没有菜单栏（`Menu.setApplicationMenu(null)`，也就没有默认的重载和开发者工具快捷键）；页面申请的权限一律拒绝；导航只限 `app://spaceknife`，新窗口一律不开。
- 0.3.4 的无头启动方式以原样的参数交给 `spaceknife-cli`（只有 `--help` / `-h` 时前面加 `--headless`，和 `SpaceKnife.com` 的分法一样），以继承终端的方式运行（`src/main/cli.ts`）。`--startup-check`（`src/main/cli.ts` 的 `STARTUP_CHECK`）在任何窗口、配置目录和后端之前回答。
- 包和便携版里 `spaceknife-cli` 在主程序旁边，界面在 `resources/ui`，`spaceknife-cli headless` 默认就在那里找界面。便携版的「无缝更新」由 Electron 外壳做（`index.ts` 的 `replacePortable`，助手优先用新版本里的 `spaceknife-cli`），数据在 `data` 里（Chromium 的配置在 `data/electron`，和 0.3.4 一样）。

## 5. Windows 安装包

发布的安装包是 Tauri 版的 NSIS 包，用 fork 的模板：`apps/desktop/src-tauri/windows/installer.nsi`（`bundle.windows.nsis.template`，打包器仍按自己的数据渲染它）和 SpaceKnife 自己的逻辑 `spaceknife.nsh`（`installerHooks`），由 `tauri.bundle.windows.json` 接上；`files-0.3.4.nsh` 是真实的 0.3.4 安装包装进去的文件清单。它接管 0.3.4 electron-builder 的安装：

- **同一套键**：electron-builder 按 appId 算出的 GUID（`3cb3f1fc-eef4-5813-9008-c29b5510458c`），`Software\{GUID}` 的 `InstallLocation`、`KeepShortcuts`、`ShortcutName` 和 `Uninstall\{GUID}` 的卸载项（`UninstallString` 是 `"<目录>\uninstall.exe" /currentuser`，`QuietUninstallString` 再加 ` /S`），「应用」里只有一条。
- **同一个目录**：当前用户 `FOLDERID_UserProgramFiles\SpaceKnife`（通常 `%LOCALAPPDATA%\Programs\SpaceKnife`），所有用户 `C:\Program Files\SpaceKnife`。已有安装（0.3.4 或 fork 的）时沿用它的目录，不问；新装时 `/D=` 给的目录两种范围都认（`SkChooseFolder`）；目录页上选的（键入、浏览或 `/D=`）路径里没有「SpaceKnife」时补上 `\SpaceKnife`，和 0.3.4 的 electron-builder（`instFilesPre`）一样（`SkAppendName`）；静默安装的 `/D=` 照原样用，0.3.4 也是。绝不装进数据目录 `%LOCALAPPDATA%\SpaceKnife`、外壳的 `%LOCALAPPDATA%\io.github.duskxi.spaceknife`、`%APPDATA%\SpaceKnife` 及其上下级，也不装成盘根、`%LOCALAPPDATA%`、Programs、Program Files、Windows 这样的系统文件夹（`SkUnsafeDir`）：目录页上提示换一个，静默安装时退出，已有安装在那里时改装到默认目录。
- **同样的快捷方式**：开始菜单和桌面的 `SpaceKnife.lnk`，同一个文件只改目标和 AppUserModelID（`io.github.duskxi.spaceknife`，`SkMigrateShortcut`；改了目标的再查一次指向哪里才写 AUMID）；用户删掉的不重建。应用启动时也设同一个 AUMID（`window.rs` 的 `set_app_user_model_id`），任务栏的固定图标和运行中的窗口归为一组。
- **两套参数都认**：0.3.4 的 `--updated /S --force-run /currentuser|/allusers`（0.3.5 的「无缝更新」也用这一套，`install.rs` 的 `nsis_update_args`）和 Tauri 的 `/S /P /NS /UPDATE /R /ARGS`；带 `--updated` 时给应用加 `--updated`。manifest 是 asInvoker（0.3.4 和 0.3.5 都用 CreateProcess 启动安装程序，要求管理员的 manifest 会让它直接失败）；为所有用户安装又没有管理员权限时，自己以 `runas` 启动一份提权副本（`--sk-elevated`，参数和 `/D=` 照传）并等它，应用由外层重新启动（只有 `TokenElevationType=2` 时才借 explorer 的令牌，其余情况直接启动并继承环境，`SkStartApp`）；拒绝 UAC 时退出码 1223，0.3.4 重新启动。
- **和 0.3.4 的安装程序互斥**（electron-builder 同名的 mutex，最多等 60 秒）。降级到 0.3.4 时，0.3.4 的安装程序运行 fork 的卸载程序（`/S /KEEP_APP_DATA /currentuser --keep-shortcuts --updated _?=…`），它认这套参数，删自己的文件和键，数据和快捷方式留着。

安装的顺序（`installer.nsi` 的 `Section WebView2` 和 `Section Install`），先做的几步失败时什么都没改：

1. **WebView2**：缺少时用内嵌的引导程序安装（要联网），更新时也装（0.3.4 的 Electron 版不需要它）；已装但低于 89.0.774.44 时先请 WebView2 自己的更新程序（EdgeUpdate）升级，升级后再读一次版本（`SkWebView2Minimum`）。装不上或仍然太旧就中止：静默更新时重新启动 0.3.4（`.onInstFailed` → `SkRestartPrevious`），交互运行时可以重试。
2. 剩余空间至少是估计大小加 64 MB。
3. 新文件解到同盘的暂存文件夹 `<上级>\<目录名>.sk-staging`，逐个核对（`SkMakeStage`、`SkCheckStaged`）；建不了（权限，或那里有别的构建留下的文件）就解到插件目录，之后复制而不是改名。暂存文件夹只按这次暂存的名字清空，里面有别的文件就原样留着。
4. 关掉从这个目录和旧安装的目录运行的 SpaceKnife，和 0.3.4 的安装程序一样按目录认进程（`SkCloseIn`）：程序在目录下面、文件名里有「spaceknife」（SpaceKnife.exe、SpaceKnife.com、spaceknife-cli.exe、0.3.4 的后端）或是 0.3.4 的 `resources\elevate.exe`；安装程序自己（也可能放在那个目录里，如 0.3.4 装在数据目录时的 `updates`）和它的另一份副本、用户放在目录里的别的程序都不算。先礼后兵：给它们可见的顶层窗口发 WM_CLOSE（和不带 /F 的 taskkill 一样），有窗口的等 5 秒、正在退出的等 2 秒，再 TerminateProcess（每次先在同一个句柄上核对程序路径），最多三轮，每轮等 3 秒让 Electron 的子进程跟着退出；交互运行时先问一句。只有真的结束不了（如安装程序没提权而它提了权）才中止，什么都没改；每一步写进日志。别的程序占着目录里的文件不归它关：0.3.4 的卸载程序有一个文件被占用就全部放回；fork 原地更新时 `SkCheckReplaceable` 先确认要替换的文件没有被不许删除的句柄占着（等 5 秒），否则中止，什么都没改。
5. **移除 0.3.4**：用它自己的卸载程序（从一份副本运行 `/S /KEEP_APP_DATA <范围> --keep-shortcuts --updated _?=<目录>`：数据、快捷方式和固定都留着；失败时重试 3 次，它的 `--updated` 模式有一个文件被占用就全部放回，0.3.4 仍完好）。它的每次运行最后都 `RMDir /r` 整个目录，所以在这几种情况下不用它，只按 `files-0.3.4.nsh` 删 0.3.4 自己的文件、删空了的文件夹（`SkOldUninstallerUse`、`SkRemove034Files`）：目录不安全（数据目录、系统文件夹）、路径里没有「SpaceKnife」（静默 `/D=` 装进 `D:\Tools` 这样和别的程序共用的文件夹）、卸载程序不在了。之后删掉 0.3.4 的卸载项，但 `Software\{GUID}` 的 `InstallLocation` 重新写回它的目录（0.3.4 的卸载程序自己会删两个键），直到这一版放好之后由新的键值覆盖：放置失败时再运行安装程序仍能找到原来的目录、装回那里，保留下来的快捷方式也能改指新程序。从这里起 0.3.4 回不来了，剩下的都是同盘改名。
6. 旧的 fork 安装（以后的版本更新 0.3.5 时）：按它的清单删它装的文件和空了的文件夹（`SkRemoveListed`，见下）；目录改了的（旧目录不安全）运行它自己的卸载程序（`--updated`）。原版 Tauri 的测试构建用它自己的卸载程序移除。
7. 暂存的文件放到位（`SkPlaceAll`）：目录不在或空着时整个暂存文件夹改名成它，否则逐个文件改名（失败几次后复制）。整个改名成功以后再不退回逐个放置，缺了哪个文件（如被杀毒软件拿走）就报告、已在的留着；逐个放置时只在暂存里确有替换文件时才删已装的那个。
8. 写卸载程序 `uninstall.exe` 和**文件清单** `installed-files.txt`（`SkWriteManifest`：这次装进去的每个文件，再是它们所在的、`$INSTDIR` 下面的文件夹，最深的在前；UTF-16 LE），写键，处理快捷方式。

**删除只按名字**：安装程序和卸载程序都不整个删除任何文件夹（`tools/tests/nsis_installer.test.mjs` 核对）。fork 的安装按 `installed-files.txt` 删（`SkRemoveListed`：清单上的文件，再是清单上空了的文件夹；带盘符、以 `\` 开头、有 `.` 或 `..` 段、通配符、`/` 的行一律跳过），0.3.4 的按 `files-0.3.4.nsh`，暂存文件夹按这次暂存的名字；目录里别的东西（用户放的文件、和别的程序共用的文件夹里的东西）都留着。唯一的例外是卸载程序删 SpaceKnife 自己的缓存和数据文件夹（`un.SkRemoveData`，见下）。

**卸载程序** `uninstall.exe`：先关掉运行中的 SpaceKnife，按自己的文件列表和 `installed-files.txt` 删文件、删空了的文件夹和 `$INSTDIR`（空时）。第二页是 0.3.4 的「你的数据」（文案照搬）：默认不勾，只删程序、界面缓存（`%LOCALAPPDATA%\io.github.duskxi.spaceknife`、`%APPDATA%\io.github.duskxi.spaceknife`、0.3.4 的 `%APPDATA%\SpaceKnife\electron` 和 `%APPDATA%\spaceknife-electron`、`%LOCALAPPDATA%\spaceknife-electron-updater`）、后端的 `Cache` 和下载的更新；勾上（或静默卸载带 `--delete-app-data`）才删快照、设置和日志（`%LOCALAPPDATA%\SpaceKnife`、`%APPDATA%\SpaceKnife`；程序装在里面时绝不删）。带 `--updated` / `/UPDATE` 时（更新、降级）什么数据都不删，快捷方式、跳转列表和固定都留着；只带 `--keep-shortcuts` 时只留快捷方式、跳转列表和固定。

- fork 照着锁定的 tauri-cli 2.12.4 / tauri-bundler 2.10.4 的原版模板写：`windows/upstream.json` 记着原版 6 个文件的 SHA-256，`node tools/nsis_fork.mjs check`（CI 的 Rust 作业、界面作业和安装包工作流里跑）核对 package.json / pnpm-lock 锁的 CLI 版本、crates.io 上 tauri-cli 对 tauri-bundler 的精确依赖、crate 里那 6 个文件和装好的 CLI 内嵌的文件；升级 tauri-cli 时先把原版的改动合进 fork，再更新 `upstream.json`。
- 安装程序和卸载程序把每一步写进 `%TEMP%\SpaceKnife-install.log`（UTF-16，静默运行时也写，`SkLogLine`）。
- `node tools/release.mjs windows-installer <版本> <bundle 目录> <输出目录>` 把 Tauri 的 `SpaceKnife_<版本>_x64-setup.exe` 改成 0.3.4 的名字 `SpaceKnife-<版本>-Setup-x64.exe`，并写 `SHA256SUMS-installers-win-x64.txt`。
- 没有代码签名证书：第一次运行时 SmartScreen 会提示「未知发布者」。

## 6. Linux 的包

- **deb**（Tauri 外壳）：`spaceknife` 和 `spaceknife-cli` 在 `/usr/bin`，其余在 `/usr/lib/SpaceKnife`；desktop 文件沿用 0.3.4 的 `spaceknife.desktop`（收藏的启动器不失效），hicolor 图标由 Tauri 生成；依赖 WebKitGTK 4.1、GTK 3 等。包名 `spaceknife`，是对 0.3.4 那个包的普通升级（dpkg 删掉 0.3.4 的 `/opt/SpaceKnife`）。0.3.4 的 postinst 把 `/usr/bin/spaceknife` 做成 `/opt/SpaceKnife/spaceknife` 的 alternatives 链接；它的 postrm 在升级时（新文件解包之后）`update-alternatives --remove`，update-alternatives 不删新包放在那里的普通文件；新包的 postinst 若还看到那条登记就再去掉一次（`tools/deb.mjs`）。`install.rs` 用 `/var/lib/dpkg/info/` 的文件列表区分 deb 与 rpm。
- **rpm**（Electron 外壳，RHEL 9 没有 WebKitGTK 4.1）：包名 `spaceknife`，布局同 0.3.4：程序在 `/opt/SpaceKnife`，`/usr/bin/spaceknife`（alternatives）和 `/usr/bin/spaceknife-cli` 两个链接；界面在 `/opt/SpaceKnife/resources/ui`。安装、卸载脚本在 `apps/electron-shell/build/linux/`：卸载脚本只在真正卸载时（`$1` 为 0）删链接；0.3.4 的 `%postun` 不看 `$1`，而 rpm 先跑新包的 `%post` 再跑旧包的 `%postun`，从 0.3.4 升级时会删掉刚登记的 `/usr/bin/spaceknife`，所以新包带 `rpm-posttrans.sh` 作 `%posttrans`（整个事务之后）把缺的链接补回来，以后每个 rpm 都要带。rpm 只删包自己列着的东西：fpm 默认只列文件，0.3.4 的 rpm 一个文件夹也不拥有，卸载后 `/opt/SpaceKnife` 和里面的文件夹空着留下，从 0.3.4 升级后它 Python 后端的文件夹（`resources/backend` 下）也空着留下；所以 `electron-builder.yml` 给 fpm 加 `--directories /opt/SpaceKnife`，包拥有 `/opt/SpaceKnife` 和其中每个文件夹（`%dir`），`%posttrans` 删掉 0.3.4 留下的那些空文件夹（只删空的）。
- **AppImage**（Electron 外壳）：需要 FUSE 2（Ubuntu 22.04 上是 `libfuse2`）；没有时用 `--appimage-extract-and-run` 运行。和 0.3.4 一样，运行时的 `.upd_info` 段里写着更新信息（`gh-releases-zsync|DuskXi|SpaceKnife|latest-all|SpaceKnife-*-x86_64.AppImage.zsync`），发布里有 `.AppImage.zsync`，给 AppImageUpdate 之类的工具用（`node tools/appimage.mjs finish <AppImage>`，`electron.yml` 在算校验和之前运行）；SpaceKnife 自己的更新检查不用它们。

## 7. 便携版与 macOS

**便携版**（Windows zip、Linux tar.gz、macOS zip）：

- 一个文件夹：程序（`SpaceKnife.exe`；Linux 上 `spaceknife`）、`spaceknife-cli[.exe]`（Windows 上还有 `SpaceKnife.com`）、`resources/backend/spaceknife-backend[.exe]`、`ui/`（Electron 外壳是 `resources/ui`）、`licenses/` 和一个空的 `data` 文件夹；macOS 上是 `SpaceKnife.app` 加旁边的 `data`。快照、设置、日志、缓存和 WebView 配置都存在 `data` 里，不写进用户目录。手动设的 `SPACEKNIFE_HOME` 优先；删掉或重命名 `data`，就改用正常的用户目录。无头模式（`spaceknife-cli headless` 和 0.3.4 的 `--headless`）也这样找 `data`（`cli/headless.rs` 的 `portable_home`）；`spaceknife-cli` 的其他命令仍用用户目录，除非设了 `SPACEKNIFE_HOME`。
- 解压到用户可写的位置（解压到 Program Files 之类的位置会写不进去）。Windows 的便携版要 WebView2 Runtime 89.0.774.44 或更新。
- Linux 的 tar.gz 不需要 FUSE。Linux 有两种：沿用 0.3.4 名字的 `-portable.tar.gz` 是 Electron 外壳（第 4 节），glibc 2.34+ 就能用，0.3.4 的 Linux 便携版都更新到它；`-tauri.tar.gz` 是 Tauri 版，下载小得多，要系统的 WebKitGTK 4.1。两种都带替换助手、都回答 `--startup-check`，各自只更新到同一种。
- 有 x64 和 arm64 两份（`win-arm64`、`linux-arm64`）。更新时按处理器的架构挑包（`asset_templates`），架构在运行时读（`machine`：Windows 用 `IsWow64Process2`，macOS 看 `hw.optional.arm64`，Linux 用 `uname`），所以在 ARM 上模拟运行的 x64 副本会换成 arm64 版：Windows on ARM 上先找 arm64 包，发布里没有时仍给 x64；ARM Linux 只取 arm64 包。arm64 版目前只在 GitHub 的 ARM 运行器上测过。没有 32 位 Windows 版。
- macOS 的 zip 保留包里的符号链接、扩展属性和签名（ditto 格式）。App 转移（Gatekeeper 路径随机化）：带隔离属性、在解压出来的位置直接打开的应用，会被系统从 `/private/var/folders/…/AppTranslocation/…` 下的只读副本运行，旁边没有 `data`；`portable.rs` 认出这种路径后，用 `/sbin/mount` 的挂载表（只读 nullfs 挂载）找回原来的位置，`data` 照样用原位置旁边那个，「无缝更新」替换的也是原位置的文件夹（真机上还没验证过：CI 下载的文件不带隔离属性）。

**macOS（dmg）**：

- 打开 dmg，把 SpaceKnife 拖进「应用程序」。ad-hoc 签名、未公证：第一次打开会被 Gatekeeper 拦下，用户要去「系统设置 › 隐私与安全性」点「仍要打开」并输入密码，之后正常打开；每次更新后再来一次。终端里 `xattr -dr com.apple.quarantine /Applications/SpaceKnife.app` 也行。README 里有给用户的步骤。
- 「完全磁盘访问权限」按应用的签名记：ad-hoc 签名每个版本都不同，每次更新后要在设置里关掉再打开一次（便携版也一样，等 Developer ID 签名之后才会记住）。不给的话，扫描到「桌面」「文稿」「下载」、云盘、照片图库、其他应用的数据、外接盘和网络盘时会逐个弹出授权提示，邮件、信息等受保护目录读不到，计入未知空间。应用在扫描前检查（`crates/sk-core/src/privacy.rs`：读用户的 TCC 数据库，不弹窗），没有时弹出说明框：打开系统设置的对应页面 / 跳过这些位置 / 照常逐个询问；设置「扫描范围 › 受保护的位置」记住选择。扫描在应用进程里做，授权和提示都记在 SpaceKnife 这个应用名下。
- 扫描 `/` 时不进入 `/System/Volumes/Data`：同一 APFS 卷组的数据卷与系统卷报告同一个设备号，它的文件已经通过 firmlink 在树里（`crates/sk-core/src/volume.rs` 的 `volume_group_twins`）。云盘里「仅在云端」的文件（`SF_DATALESS`）按占用 0 计，这样的文件夹不进入，进程也设为不物化 dataless 文件，扫描不会触发下载。磁盘负载在进程内读 IOKit 的块设备统计（与 `iostat` 同源）。
- 文件条目的属性用 `getattrlistbulk` 批量取（文件夹、挂载点、firmlink 仍逐个 `fstatat`）。读到的第一批文件与 `fstatat` 对照，有差异就整条退回 readdir + `fstatat`；`SPACEKNIFE_NO_BULK=1` 关掉。文件夹也能从它取（`SPACEKNIFE_BULK_DIRS=1`），但并不更快。`spaceknife-cli scan --profile` 打印耗时分解和它的状态。列目录的起步宽度是可用 CPU 数的两倍（4–16），读盘时加宽到冷宽度（[native.md](native.md) 2.2「列目录线程」）；`SPACEKNIFE_SCAN_THREADS` 可以固定。2026-10-20 在 GitHub 的 macOS 运行器上对比过（扫 /System/Library /usr，单位是项/秒；热缓存 6 轮，`sudo purge` 后 3 轮）：

  | | 3 核 arm64 | 4 核 Intel |
  |---|---|---|
  | 单线程切片各用自己的线程池（e07daf3） | 5.5 万 → 13.9 万 | 2.1 万 → 6.1 万 |
  | 线程数两倍 | +8%（热）/ +15%（purge 后） | +12% / +5% |
  | 文件夹也用 getattrlistbulk | −19% / −2% | 0% / −4% |

## 8. 更新

**更新检查**（`crates/sk-core/src/update/`，服务端 `crates/sk-server/src/updates.rs`，协议 `update.*`，界面在 `app/components/shell/UpdateBadge.vue` / `UpdateCard.vue`）：

- 后端每天最多一次读取 `https://api.github.com/repos/DuskXi/SpaceKnife/releases`（启动约 1 分钟后第一次；带 ETag，没变化时 GitHub 回 304；失败 6 小时后再试），不带令牌，只发 `User-Agent: SpaceKnife/<版本>`。设置「关于与更新 › 更新」可以关掉，或改成「只要正式版」（默认包括 GitHub 上标为预发布的版本）。命令面板「检查更新」随时检查；「关于与更新」页显示版本、组件版本和最近 30 个发布的时间线（`update.releases`）。
- 和正在运行的版本比较：只差修订号时只在标题栏右侧显示徽章；次版本号不同时左上角挂一张卡片；主版本号不同时在窗口内弹对话框。关掉卡片或对话框后这个版本不再自动弹出，徽章保留。「跳过 0.4.x」/「跳过 1.x」（`update.mute`）记在 `update.json` 的 `muted` 里（最多 20 个）。
- 外壳把运行版本和安装方式交给后端（`ServeOptions` 的 `app_version`、`install_kind`：nsis / portable / appimage / deb / rpm / dmg / dev，由 `src-tauri/src/install.rs` 的 `install_kind` 判断，`SPACEKNIFE_INSTALL_KIND` 可以覆盖；`spaceknife-cli serve` 对应 `--app-version`、`--install-kind`）。下载的更新放在数据目录的 `updates` 里，外壳只启动这个文件夹里、按本次安装方式的发布文件命名的文件（`install.rs` 的 `install_refusal`）。演示和测试可以用 `SPACEKNIFE_UPDATE_FEED` 指向本机（只接受回环地址）的一份发布列表。
- 下载（「自动下载更新」开启时检查之后自动开始，否则点「一键下载」）按安装方式和外壳挑发布文件（`SPACEKNIFE_SHELL`），用同一个发布里的 `SHA256SUMS*.txt` 校验，不符就删掉；发布里没有列出这个文件的 SHA-256 时不下载。每次读网络都有 15 秒的超时，取消在一个超时之内生效。之后更新卡片给出两个大按钮（`app/shared/install.ts` 的 `installStep`）：「无缝更新」由 SpaceKnife 自己换上新版本再启动；经典方式交给安装程序、磁盘映像或安装包，或者在文件夹中显示下载的文件让用户自己替换。两者都只在用户点击后由外壳执行，从不自己开始，以后也不会加自动安装的选项。有扫描在进行时，会关闭 SpaceKnife 的按钮第一次点只提示，第二次点才执行。
- 新版本带 `--updated` 启动时，界面提示一次「已更新到 SpaceKnife <版本>」。启动时后端清理 `updates`：旧安装包、`staging`、`previous`、`update-<版本>.json` 和日志都会删掉；删不掉的（刚更新完时旧版本最后几个进程可能还占着）过 5 秒、15 秒……再试，最多到 5 分钟后，仍不行就留到下次启动。

**各种安装方式怎样换上新版本**：

- **NSIS**：「无缝更新」先停掉后端，再以 0.3.4 的参数 `--updated /S --force-run` 加范围（程序在 Program Files 下时 `/allusers`，在 `%LOCALAPPDATA%` 下时 `/currentuser`，别的目录不加；`install.rs` 的 `nsis_update_args`）运行新安装程序（静默、就地更新、装好后带 `--updated` 启动新版本），然后退出；安装程序做的事见第 5 节。「运行安装程序」不带参数运行新安装程序（显示向导），SpaceKnife 随即退出。
- **AppImage**：「无缝更新」用新文件替换 `$APPIMAGE`，然后启动它；「自己替换」在文件夹中显示新的 AppImage。
- **dmg / deb / rpm**：只有经典方式「打开磁盘映像」/「打开安装包」，交给系统打开，SpaceKnife 随即退出。
- **便携版**：「自己替换」在文件夹中显示下载的压缩包，用户退出后自己解压覆盖，保留 `data`。「无缝更新」换文件的是**新版本自己的程序**（`<程序> --apply-update <计划>`，`sk_core::update::swap`），见下。

**便携版的「无缝更新」**：

1. 外壳把压缩包解到 `updates/staging-<版本>`，确认里面唯一的顶层文件夹带着程序（macOS 上是 `SpaceKnife.app`），写出计划 `updates/update-<版本>.json`（版本 1：`v`、`pid`、`app`、`new`、`old`、`exe`、`log`、`keep: ["data"]`）。计划是和每个旧版本的约定：旧应用把它交给更新的版本，所以以后的版本要一直能读 `v: 1`。0.3.x 的计划交给的是新版本的 `resources/backend/spaceknife-backend`（macOS 上在 `SpaceKnife.app/Contents/Resources/backend/`），所以每个便携包都带着它（`spaceknife-cli` 的副本，同样把 `--apply-update` 当第一个参数）。
2. 以 detached 方式启动新版本的助手（Tauri 版是 `<staging>/<程序名>`，Electron 外壳是新版本里的 `spaceknife-cli`），等它在 stdout 回一行 `{"ok": true}` 后退出。30 秒没回、回了别的或者它先退出了，应用就照旧运行并提示原因（`updateStartFailed`）。
3. 助手先检查计划：路径都是绝对路径、`exe` 在 `app` 里、新版本的同一位置有程序、`old` 是空的。再问新版本的程序能不能在这里启动：以应用启动时的同一套环境运行 `<新程序> --startup-check`（第 3 节），不开窗口、不开控制台，最多等 20 秒。Linux 上加 `LD_BIND_NOW=1`，系统里的库太旧时在这里失败；Windows 上新程序继承 `SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX`，缺 DLL 或崩溃时系统不弹框。回 `{"ok": true}` 并以 0 退出才算能启动。不能用的计划、不能启动的新版本都什么都不动：日志写一行 `not started: …`，回 `{"ok": false, "error": …}`，应用重新启动自己的后端、照常运行，更新卡片显示「无法启动更新：…」。没有 WebView2 或低于 89.0.774.44 的 Windows、没有 libwebkit2gtk-4.1 的 Linux（如 RHEL 9）、缺运行库的程序都是这样被拦下的。
4. 等应用主进程退出（最多 60 秒）；把程序目录里除 `data` 以外的条目移到 `updates/previous-<旧版本>`（文件还被占用时在 30 秒内反复重试，日志写「busy: …」）；再把新条目移进来（助手自己就在新条目里运行，Windows 不让改名一个里面有程序在运行的文件夹，所以整体改不了名的逐个移动，连这也不行的就复制）。移文件时任何一步失败就把旧条目移回原处、启动旧版本，日志写「kept the old version」。
5. 日志写「updated」，记下 `previous-<旧版本>` 里每个条目的相对路径和大小，带 `--updated` 启动新版本，并等它说它已经在运行：助手把 `SPACEKNIFE_READY_FILE` 只交给新版本（`launch_env` 从别的启动里去掉它），指向一个助手新建的、名字是 128 位随机数、只有当前用户能打开的临时文件夹（Unix 上 0700）里的文件（助手自己的环境里已经有这个变量时就用那一个，比如测试）；新版本的后端开始监听时写进去（两个外壳都这样做，只写一次，第 2 节），助手看到之后删掉那个文件夹，不再写日志（新版本启动时会清理 `updates`，日志也在其中）。最多等 90 秒（`READY_TIMEOUT`）。
6. 新版本在那之前以非 0 退出（崩溃），或者到时还没写（这时把它结束掉），就撤回这次更新：日志写 `the new version did not start: …`；`previous-<旧版本>` 里的条目必须和第 5 步记下的完全一样——同样的相对路径、同样的大小（不比较内容；新版本一运行就会清理它们，少了任何一个、大小变了就不撤回，日志写明，新版本留在原处）；再把新条目整个移回解压的地方（移不动的文件夹逐个移，不复制；有一个移不动就全部放回，什么都不变），旧条目移回程序目录，启动旧版本，日志写 `took the new version out again` 和 `kept the old version`。新版本自己以 0 退出不算失败。
7. 每一步都写进 `updates/update-<版本>.log`，每行带 UTC 时间。助手启动应用时去掉指向这份副本自己 `data` 的 `SPACEKNIFE_HOME`（旧应用给自己的进程设了它），新版本自己找到 `data`、仍认得自己是便携版。

## 9. 发布

**发布时的约定**：更新检查靠文件名找文件。已安装的 0.3.4 先按版本号挑出最新的发布，挑中之后才按 0.3.4 的名字找文件，找不到就什么也不下载，所以**以后的每一个版本**都必须沿用这套名字、便携包里都带着替换助手，否则还没升级的 0.3.4 会被永远卡住。发布文件和校验和文件（与 v0.3.4 的发布逐个相同）：

- `SpaceKnife-<版本>-Setup-x64.exe`、`SpaceKnife-<版本>-x86_64.AppImage`、`spaceknife_<版本>_amd64.deb`、`spaceknife-<版本>.x86_64.rpm`（这四个列在 `SHA256SUMS.txt`）；`SpaceKnife-<版本>-x86_64.AppImage.zsync`（不在任何校验清单里）；`SpaceKnife-<版本>-win-x64-portable.zip`、`SpaceKnife-<版本>-win-arm64-portable.zip`、`SpaceKnife-<版本>-linux-x64-portable.tar.gz`、`SpaceKnife-<版本>-linux-arm64-portable.tar.gz`（各自的 `SHA256SUMS-portable-<目标>.txt`；Linux 的两个是 Electron 外壳的）；0.3.5 起还有 Tauri 版的 `SpaceKnife-<版本>-linux-x64-tauri.tar.gz`、`SpaceKnife-<版本>-linux-arm64-tauri.tar.gz`（`SHA256SUMS-tauri-linux-<架构>.txt`，0.3.4 不会拿到）；`SpaceKnife-<版本>-mac-<架构>.dmg`、`SpaceKnife-<版本>-mac-<架构>-portable.zip`（`SHA256SUMS-mac-<架构>.txt`）。deb 和 rpm 名字里预发布段的 `-` 写成 `~`。
- `tools/release.mjs assemble` 检查并合并：各构建作业的 `SHA256SUMS-installers-*.txt`（Tauri 的 Windows 安装包和 deb、Electron 外壳的 rpm 和 AppImage）合成 `SHA256SUMS.txt`，只写那四个安装包。
- tag 是 `v<版本>`（语义化版本，可带预发布段）。不要改名：`crates/sk-core/src/update/mod.rs` 的 `asset_templates`、`tools/release.mjs` 的 `expected` 和 `tools/compat034.mjs` 的 `CONTRACT` 都按 0.3.4 写死，测试逐个比对。

**累积更新**：还没发布的更改随做随记在仓库根目录的 [UNRELEASED.md](../UNRELEASED.md)（中英双语，「新功能」「修复」两节，和发布说明同一个结构）。两份 README 的顶部导航里固定链接到它。发布时，它就是发布说明的正文，再补上下载表和已知限制；发布之后，在 dev 上把它清空：只留标题、语言切换、开头那句说明（版本号改成下一版），以及每节下面的「（暂无）」/「(nothing yet)」。

**发布**（从 0.3.4 起由 CI 发布，文件不经过任何人的电脑）：

1. dev 上改版本号（根目录 `Cargo.toml` 的工作区版本、`apps/desktop/package.json`、`apps/electron-shell/package.json`、`apps/desktop/src-tauri/tauri.conf.json`、`app/shim.ts` 的网页版版本、问题模板的占位符），把 `UNRELEASED.md` 压成 `release-notes/<版本>.md`：先英文后中文，下载表的大小写成 `{{size:文件名}}`，构建运行写成 `{{build_runs}}`，也可以用 `{{version}}`；工作流填进去，写错的占位符或不属于这个发布的文件名会让发布失败（`tools/tests/release.test.mjs` 也按 `expected` 核对仓库里的发布说明）。
2. PR dev → main，等「CI passed」再合并。
3. 在 main 的合并提交上本地签名打 tag（`git tag -s v<版本> <提交> -m "SpaceKnife <版本>"`）并推送。CI 不持有签名的私钥。
4. tag 推上去就触发 **Release** 工作流（`.github/workflows/release.yml`）：检查 tag 是带签名的附注 tag 且 GitHub 认可签名、文件和 main 完全相同、应用的版本号就是 tag 的版本、`release-notes/<版本>.md` 存在、同名的发布还没有；用 `Portable build`（带 deb）、`macOS build`、`Windows installer build` 和 `Electron shell build` 从 tag 构建全部文件，四者都给每个文件生成构建来源证明（`actions/attest-build-provenance`，可以用 `gh attestation verify <文件> --repo DuskXi/SpaceKnife` 核对），并各自测从真实的 0.3.4 文件更新上来（第 11 节）；`node tools/release.mjs assemble` 核对每份校验和文件、确认正好是这个发布该有的文件、合并 `SHA256SUMS.txt`、填好发布说明（汇总结果也作为运行的产物 `release-<版本>` 留 14 天）；`node tools/compat034.mjs --dir release` 用 0.3.4 自己的更新代码读汇总好的文件；建成预发布的草稿（0.x 和带预发布段的版本都是预发布）。
5. 在 GitHub 上看一眼草稿，点发布。**Release check** 工作流（`release-check.yml`）在发布时用 `cargo run -p sk-core --example release_check -- <版本>` 按已安装的 SpaceKnife 的方式读这个发布（每种安装方式都按确切的名字找到文件，`--download` 时下载并核对校验和），再用 `tools/compat034.mjs --github` 按已安装的 0.3.4 的方式读一遍。

手动运行 Release（Actions › Release › Run workflow）给已经在 GitHub 上的 tag 发布：可以填同一份文件树的四个构建运行号，直接用它们的文件，不再构建（工作流核对四个运行成功、构建的提交和 tag 的文件完全相同；这时没有构建来源证明）。`dry_run` 只核对和汇总，不建发布；`publish` 不留草稿，直接发布并立刻读回检查。两者在本机也能跑：`node tools/release.mjs assemble` 接 `gh run download` 下载的产物；`cargo run -p sk-core --example release_check -- <版本> [--sums DIR] [--download]` 读真实的发布列表。

## 10. 数据位置

`SPACEKNIFE_HOME` 会把这些全部放进同一个目录；后端的位置和 0.3.x 相同（`crates/sk-core/src/paths.rs`）：

| | Windows | Linux | macOS |
|---|---|---|---|
| 快照、标签、日志（数据） | `%LOCALAPPDATA%\SpaceKnife` | `~/.local/share/SpaceKnife` | `~/Library/Application Support/SpaceKnife` |
| 设置、文件类别、洞察规则 | `%LOCALAPPDATA%\SpaceKnife` | `~/.config/SpaceKnife` | `~/Library/Application Support/SpaceKnife` |
| 缓存 | `%LOCALAPPDATA%\SpaceKnife\Cache` | `~/.cache/SpaceKnife` | `~/Library/Caches/SpaceKnife` |
| 窗口外壳（主题、WebView 配置） | `%LOCALAPPDATA%\io.github.duskxi.spaceknife` | `~/.local/share/io.github.duskxi.spaceknife` | `~/Library/Application Support/io.github.duskxi.spaceknife` |

最后一行是 Tauri 外壳的；设了 `SPACEKNIFE_HOME` 时，Tauri 外壳的文件在 `<SPACEKNIFE_HOME>/desktop`。Electron 外壳（rpm、AppImage、沿用 0.3.4 名字的 Linux 便携版）照旧用 0.3.x 的 Electron 配置目录（`~/.config/SpaceKnife/electron`，设了 `SPACEKNIFE_HOME` 或是便携版时是 `<那个目录>/electron`）；Tauri 外壳只在第一次启动时从那里读一次主题。

## 11. 测试

```bash
node packaging/smoke-app.mjs <打包好的应用>
```

[packaging/smoke-app.mjs](../packaging/smoke-app.mjs) 最小化启动打包好的应用（`--portable`：便携版，用它旁边的 `data`；否则用临时目录作 `SPACEKNIFE_HOME`），从就绪文件读后端的地址和 token，`session.hello`、`volume.list` 列出卷、`update.status` 的安装方式（`--kind`），再像系统那样关掉应用，确认后端的地址随之关闭；便携版还要在 `data` 里看到应用写的东西。两个外壳都能用。窗口本身不检查：Linux 和 macOS 上的系统 WebView 没有 DevTools 端口。Linux 上用 `xvfb-run -a`。

```bash
node packaging/smoke-headless.mjs <spaceknife-cli> [--ui <界面文件夹>]
```

[packaging/smoke-headless.mjs](../packaging/smoke-headless.mjs) 测无头模式：`spaceknife-cli headless --listen 127.0.0.1:0` 启动，读打印出的地址，检查页面、`/sk/info` 只认 token、WebSocket 拒绝别的 Origin（403）、用页面自己的 Origin 能和后端握手，再停止，确认地址已关闭。

```bash
node packaging/update-e2e.mjs target/portable/SpaceKnife-<v>-<目标>-portable.zip           # Windows、macOS
xvfb-run -a node packaging/update-e2e.mjs target/portable/SpaceKnife-<v>-linux-<架构>-tauri.tar.gz
xvfb-run -a node packaging/update-e2e.mjs target/electron-shell/portable/SpaceKnife-<v>-linux-<架构>-portable.tar.gz
```

[packaging/update-e2e.mjs](../packaging/update-e2e.mjs) 是便携版「无缝更新」的端到端测试（`Portable build`、`macOS build` 和 `Electron shell build` 里跑）：在 127.0.0.1 上提供一份发布列表，把这次构建改名、加一个标记文件当作下一个修订版，附上校验清单（Linux 上还有另一个外壳的便携包，这份副本不能拿它）；经应用后端的 WebSocket 确认找到了这个版本和它的文件、下载并校验通过，然后做「无缝更新」按钮做的事（解包、写计划、启动新版本的 `--apply-update`），关掉应用；新版本换好文件后带 `--updated` 重新启动：程序文件换成了新的、`data` 原样保留、旧文件从 updates 文件夹清掉。按钮本身和解包、写计划的代码由 `install.rs` 的单元测试覆盖。

[packaging/nsis-e2e.mjs](../packaging/nsis-e2e.mjs)（只在 GitHub Actions 上跑，`Windows installer build`）：静默安装 Windows 安装包（`--previous` 时先装旧版再按 0.3.x 的参数升级），以 `--kind nsis` 跑一遍 smoke-app，再按 `install.rs` 的参数原地更新一轮（清单里列着的旧界面文件删掉、用户放进目录和 `ui` 里的文件留着、没有暂存目录、应用带 `--updated` 并拿到安装程序的环境重新启动），最后静默卸载，确认程序、清单和卸载项已删除、数据目录还在；失败时打印安装日志。

[packaging/nsis-logic.mjs](../packaging/nsis-logic.mjs) 不安装任何东西：用 Tauri 打包器的 NSIS 编译并运行 `packaging/nsis-logic/logic-test.nsi`，逐个检查 `spaceknife.nsh` 的逻辑：0.3.4 和 Tauri 的参数、提权副本的参数、目录保护、目录名补 `\SpaceKnife`、`/D=` 与默认目录的选择、暂存（整目录改名、整个改名后缺文件不退回、逐个文件改名、暂存不全时什么都不改、文件被占用、上级目录建不了暂存目录、残留的暂存目录只按名字清空、不是暂存目录的绝不删）、文件清单的写和按清单删（清单外的、`..`、盘符、通配符一律不删）、0.3.4 的文件清单、什么时候不用 0.3.4 的卸载程序、WebView2 的最低版本（读版本和升级换成测试替身）、沙箱里的 `.lnk` 改目标和 AUMID，以及启动应用时的环境。只在 Windows 上跑，只写临时文件夹，不写注册表；在自己的桌面上要经 [packaging/hidden-desktop.ps1](../packaging/hidden-desktop.ps1) 运行（启动的用例会开 cmd.exe）。`tools/tests/nsis_installer.test.mjs` 从源码核对它够不着的模板部分（各段的顺序、没有 `RMDir /r`）。

[packaging/installer-experiments.ps1](../packaging/installer-experiments.ps1)（`Installer experiments` 工作流：`exp/**` 分支或手动，Windows Server 2022 和 2025）只在一次性的机器上运行：装好真实的 0.3.4 安装程序后，像 0.3.4 的「无缝更新」那样启动候选安装程序，覆盖当前用户、所有用户、0.3.4 在运行、文件被别的程序占着、没有 WebView2、降级和重装（0.3.5 开着时重装）、两种范围并存、自定义目录、0.3.4 装在数据目录、控制台启动器、Defender 扫描等场景，每条断言写进一份 JSON，失败的断言写明看到了什么。一个场景出错时记为一条失败的断言（错误信息和行号），重置 SpaceKnife 后接着跑下一个，JSON 每个场景之后和最后都写。GitHub 的运行器上脚本本身就是提权运行的（UAC 开着，但它启动的每个进程都已提权），要未提权管理员的场景记为 skip，要在脚本里说明的 Hyper-V 虚拟机上跑。

```bash
node packaging/update-from-034.mjs portable|nsis|appimage|deb|rpm|dmg <0.3.4 的文件> <候选文件>
```

[packaging/update-from-034.mjs](../packaging/update-from-034.mjs) 是从真实的 0.3.4 更新上来的端到端测试（工作流先 `gh release download v0.3.4`，用 0.3.4 发布自己的校验和与 `gh attestation verify` 核对）：在 127.0.0.1 上提供 v0.3.4 和候选版本两个预发布，打开 0.3.4（带 DevTools 端口，最小化），经 0.3.4 自己的后端改一个设置，等它自动检查、下载并校验，像用户一样点徽章和「无缝更新」（便携版、安装包、AppImage）；deb / rpm / dmg 的 0.3.4 只把文件交给系统，测试代做那一步。之后检查：0.3.4 退出、新版本以新进程起来（它的就绪文件），版本和安装方式对，0.3.4 里改的设置和 data / config / cache 里的标记文件逐字节都在，updates 里没有残留；便携版还查交换日志以「updated」结束、文件夹里除了 `data` 正好是候选版本的文件、逐字节相同、程序的架构（`--expect-machine`）；安装包查只有一个卸载项、目录和快捷方式不变、卸载后数据还在；deb / rpm 查只有一个包、`/usr/bin/spaceknife`、一个 desktop 文件、alternatives，像用户那样先退出应用再卸载、卸载干净（rpm 还查每次事务之后和应用运行时 `/opt/SpaceKnife` 里的一切都是包自己的，文件夹也算；卸载后留下什么就列出什么）；dmg 查 bundle id、版本、可执行文件名和签名。便携版之外的几种只在 GitHub Actions 上跑。失败或超时时先打印能看的东西，再结束它启动的进程和还在它的文件夹里运行的进程；每条腿都有总期限（`SK_E2E_DEADLINE_S`，默认 1200 秒）。本机在 Windows 上跑便携版那一种时，用 [packaging/hidden-desktop.ps1](../packaging/hidden-desktop.ps1) 把它放到一个不显示的桌面上。

[tools/compat034.mjs](../tools/compat034.mjs)（Release 和 Release check 里跑）不开窗口、不安装，只用 0.3.4 的代码读发布：每种安装方式选中的文件名、读到的校验值、0.3.4 窗口的 `installRefusal` 是否接受、便携包的布局。

其余测试：`cargo test --workspace`、`apps/desktop` 和 `apps/electron-shell` 里的 `pnpm test`、`node --test "tools/tests/*.test.mjs"`。

## 12. 已知限制

- 未签名（Windows 代码签名证书待定）；macOS 只有 ad-hoc 签名、没有公证（等 Apple 开发者账号），也还没进 Homebrew（官方 cask 要求通过 Gatekeeper）。
- 「仅当前用户」安装在用户可写的目录里；需要管理员权限的 MFT 快扫会从这里启动提权助手。想让它待在只有管理员能改的地方，就为所有用户安装（Program Files）。
- 不会自动安装（有意如此）：有新版本时提示并在后台下载，安装总要用户在更新卡片里点「无缝更新」或经典方式。
- 安装包的运行时行为（提权与等待、Restart Manager 关进程、0.3.4 卸载程序的实际退出码、快捷方式迁移、WebView2 的升级）只在 CI 的一次性运行器和安装实验里跑过；本机只跑不安装任何东西的逻辑测试。
