# Unreleased changes · 未发布的更新

**English** · [简体中文](#简体中文)

What is already done for the next version, **0.3.5**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

- **SpaceKnife is rewritten.** The features and the interface are those of 0.3.4; what changes is the program underneath and the name of the command-line program.
- **The backend is rewritten in Rust.** Scanning, the store, filters, layouts, live monitoring, export and file operations are now Rust; there is no frozen Python backend any more. In the Tauri app the server starts inside the app's own process; the Electron shell starts `spaceknife-cli serve` next to it, as 0.3.4 started its backend.
- **The interface is rewritten with Vue, in two shells.** The Windows and macOS portable copies, the new light Linux portable copy, the deb and the macOS app run it in Tauri 2 with the system's WebView (WebView2 on Windows, WKWebView on macOS, WebKitGTK on Linux) instead of a bundled Chromium, so the download is smaller and it needs less memory. The Windows installer, the rpm (RHEL 9 has no WebKitGTK 4.1), the AppImage and the Linux portable copy under 0.3.4's name run the same interface in a slimmed Electron shell.
- **Linux has two portable copies.** `SpaceKnife-<version>-linux-<arch>-portable.tar.gz`, under 0.3.4's name, is the Electron shell: it runs on any x64 or arm64 Linux with glibc 2.34+, RHEL 9 and its rebuilds included, and it is what 0.3.4's portable copies on Linux update to. The new `SpaceKnife-<version>-linux-<arch>-tauri.tar.gz` is the Tauri app, a much smaller download that needs the system's WebKitGTK 4.1 (Ubuntu 22.04+, Debian 12+, Fedora); 0.3.4 never offers it. Each copy updates to its own kind, and each has its own checksum list (`SHA256SUMS-portable-linux-<arch>.txt`, `SHA256SUMS-tauri-linux-<arch>.txt`).
- **The command-line program is `spaceknife-cli`** (in 0.3.x the program next to the app was `spaceknife-backend`), installed next to the app: `scan`, `run`, `snapshot list` / `show`, `serve`, `headless`, `version`.
- **Headless mode is `spaceknife-cli headless`** `[--listen <address>] [--yes] [--tls-cert FILE --tls-key FILE] [--ui DIR]`; the pages and the WebSocket share one address. 0.3.x's `--headless` and `--listen <address>` are still accepted, anywhere among the arguments, and on Windows `SpaceKnife --headless …` in a terminal still runs headless mode (`SpaceKnife.com` next to the app starts `spaceknife-cli` and waits for it; without these options it opens the window and waits for it to close, as in 0.3.x). In a portable copy headless mode keeps its data in the `data` folder, as in 0.3.x. The warning before it opens to the network now says all that the link allows: also writing files anywhere your account can write, and asking for administrator rights on that computer.
- **The release files keep 0.3.4's names** (`SpaceKnife-<version>-Setup-x64.exe`, `SpaceKnife-<version>-x86_64.AppImage`, `spaceknife_<version>_amd64.deb`, `spaceknife-<version>.x86_64.rpm`, `SpaceKnife-<version>-mac-<arch>.dmg` and the portable archives), so the update check of 0.3.4 finds 0.3.5 and installs it as it installs any update. A portable copy's **Seamless update** works from 0.3.4 too: the program keeps its name (`SpaceKnife.exe`, `spaceknife`, `SpaceKnife.app`) and every portable archive carries the swap helper 0.3.x runs. Before it replaces anything, the helper checks that the new version starts on this computer (the Tauri app's portable copies need the system's web view: the Microsoft Edge WebView2 Runtime on Windows, WebKitGTK 4.1 for the light Linux copy); when it doesn't, nothing is replaced, the update card says why and the copy goes on as it was. The Windows programs have the C runtime built in, so none of them needs the Visual C++ Redistributable.
- **The Windows installer is 0.3.4's** (the same uninstall entry, folder and shortcuts, for the current user or for all users), so it installs over 0.3.4. The deb puts `spaceknife` and `spaceknife-cli` in `/usr/bin` instead of `/opt/SpaceKnife`; the rpm keeps 0.3.4's `/opt/SpaceKnife`, linked from `/usr/bin`, and adds `/usr/bin/spaceknife-cli`.
- **Settings and data stay where they were**: settings, file classes, classified folders, remembered tags, the palette's recent entries, the update state and logs are read from the same folders; on macOS `XDG_DATA_HOME` / `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` are followed as 0.3.x followed them. The old logs 0.3.x rotated (`logs/spaceknife.<time>.log`) are deleted on the first start. A `classes.json` or `class_folders.json` that cannot be read is never written over: the next save renames it and leaves it beside the new one (`classes.unreadable-<time>.json`). The Tauri app keeps the window's own files (theme, WebView data) outside Electron's profile folder, and its first start takes the theme 0.3.4 had; the Electron shell keeps them where 0.3.4 had them.
- **Snapshots of 0.3.x still open.** Snapshots saved by 0.3.x (format 2) open and compare as they are, with no conversion, and show in the lists; 0.3.5 saves format 3. The auto-save retention (the newest 10, then one a week) counts 0.3.x's snapshots too; a snapshot file that cannot be read (damaged, or of a newer format) is never deleted.

### Fixes

- **Open in Terminal could run part of a folder's name** (Windows): for a folder whose name holds `&`, `^`, `%` or brackets (`x&calc`), 0.3.4 passed the name through the command processor, which ran the rest as a command. The terminal is now started directly, never through a shell.
- **Command-line output to NUL**: `scan` with its output sent to `NUL` (`> NUL` in the command prompt, `>/dev/null` in Git Bash) ended with an error and without its snapshot; now it finishes and saves the snapshot.
- **Singular English counts on the command line**: a scan with one unreadable folder says "1 folder couldn't be accessed", `snapshot list` says "1 snapshot in …" and `snapshot show` "· 1 item ·". Thanks to @swastiktawate ([#9](https://github.com/DuskXi/SpaceKnife/pull/9)).

---

## 简体中文

下一版 **0.3.5** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

- **SpaceKnife 整个重写了**：功能和界面与 0.3.4 相同，变的是底下的程序和命令行程序的名字。
- **后端用 Rust 重写**：扫描、存储、过滤、布局、实时监控、导出和文件操作都换成了 Rust，不再带冻结的 Python 后端。Tauri 版的服务端直接在应用进程里启动；Electron 外壳像 0.3.4 启动后端那样，在旁边启动 `spaceknife-cli serve`。
- **界面用 Vue 重写，有两个外壳**：Windows 和 macOS 的便携版、新增的 Linux 轻量便携版、deb 和 macOS 版用 Tauri 2，界面跑在系统自带的 WebView 里（Windows 上的 WebView2、macOS 上的 WKWebView、Linux 上的 WebKitGTK），不再打包一整个 Chromium，下载更小，占的内存也更少。Windows 安装包、rpm（RHEL 9 没有 WebKitGTK 4.1）、AppImage 和沿用 0.3.4 名字的 Linux 便携版用精简的 Electron 外壳，界面是同一份。
- **Linux 有两种便携版**：沿用 0.3.4 名字的 `SpaceKnife-<版本>-linux-<架构>-portable.tar.gz` 是 Electron 外壳，glibc 2.34+ 的 x64 或 arm64 Linux 都能用（包括 RHEL 9 及其衍生版），0.3.4 的 Linux 便携版就更新到它；新增的 `SpaceKnife-<版本>-linux-<架构>-tauri.tar.gz` 是 Tauri 版，下载小得多，要用系统的 WebKitGTK 4.1（Ubuntu 22.04+、Debian 12+、Fedora），0.3.4 不会更新到它。两种各自只更新到同一种，校验和也各有一份清单（`SHA256SUMS-portable-linux-<架构>.txt`、`SHA256SUMS-tauri-linux-<架构>.txt`）。
- **命令行程序是 `spaceknife-cli`**（0.3.x 时应用旁边的程序叫 `spaceknife-backend`），和应用装在一起：`scan`、`run`、`snapshot list` / `show`、`serve`、`headless`、`version`。
- **无头模式改成 `spaceknife-cli headless`** `[--listen <地址>] [--yes] [--tls-cert 证书 --tls-key 私钥] [--ui 目录]`，页面和 WebSocket 用同一个地址。0.3.x 的 `--headless`、`--listen <地址>` 写法仍然认，放在参数里的任何位置都行；Windows 上在终端里输入 `SpaceKnife --headless …` 也照样启动无头模式（应用旁边的 `SpaceKnife.com` 启动 `spaceknife-cli` 并等它结束；不带这些参数时它打开窗口，和 0.3.x 一样等窗口关掉）。便携版的无头模式和 0.3.x 一样把数据存在 `data` 文件夹里。在网络上开放之前的警告现在把链接能做的事都写全了：还能把文件写到你的账户能写的任何地方，还能在那台电脑上请求管理员权限。
- **发布文件沿用 0.3.4 的名字**（`SpaceKnife-<版本>-Setup-x64.exe`、`SpaceKnife-<版本>-x86_64.AppImage`、`spaceknife_<版本>_amd64.deb`、`spaceknife-<版本>.x86_64.rpm`、`SpaceKnife-<版本>-mac-<架构>.dmg` 和便携版），0.3.4 的更新检查能找到 0.3.5，像平常的更新一样装上。便携版的「无缝更新」从 0.3.4 也能用：程序名不变（`SpaceKnife.exe`、`spaceknife`、`SpaceKnife.app`），每个便携包都带着 0.3.x 要运行的替换助手。助手换文件之前先确认新版本能在这台电脑上启动（Tauri 版的便携版要用系统的 WebView：Windows 上的 Microsoft Edge WebView2 Runtime，Linux 轻量便携版要 WebKitGTK 4.1）；不能时什么都不换，更新卡片说明原因，旧的副本照常使用。Windows 上的程序都自带 C 运行库，不需要另装 Visual C++ 运行库。
- **Windows 安装程序和 0.3.4 的是同一个**（同一个卸载项、安装目录和快捷方式，可装给当前用户或所有用户），直接装在 0.3.4 上。deb 把 `spaceknife` 和 `spaceknife-cli` 装到 `/usr/bin`，不再放在 `/opt/SpaceKnife`；rpm 保留 0.3.4 的 `/opt/SpaceKnife` 和 `/usr/bin` 的链接，再加上 `/usr/bin/spaceknife-cli`。
- **设置和数据还在原来的位置**：设置、文件类别、归类的文件夹、记住的标签、命令面板的最近记录、更新记录和日志都从原来的文件夹读取；macOS 上的 `XDG_DATA_HOME` / `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` 和 0.3.x 一样照用。0.3.x 轮转下来的旧日志（`logs/spaceknife.<时间>.log`）在第一次启动时删掉。读不了的 `classes.json` / `class_folders.json` 不会被覆盖：下次保存时改名留在原处（`classes.unreadable-<时间>.json`）。Tauri 版窗口自己的文件（主题、WebView 的数据）不放在 Electron 的配置文件夹里，第一次启动时沿用 0.3.4 选的主题；Electron 外壳仍放在 0.3.4 的位置。
- **0.3.x 的快照照样能用**：0.3.x 存的快照（第 2 版）不用转换就能打开、对比，也出现在列表里；0.3.5 存的是第 3 版。自动保存的保留规则（最近 10 份，更早的每周留一份）也算上 0.3.x 的快照；读不了的快照文件（损坏的、更新版本的）从不删除。

### 修复

- **「在终端中打开」可能执行文件夹名的一部分**（Windows）：文件夹名里有 `&`、`^`、`%` 或括号（例如 `x&calc`）时，0.3.4 把名字交给命令提示符，后半段会被当成命令执行。现在直接启动终端，从不经过 shell。
- **命令行输出到 NUL**：`scan` 的输出送到 `NUL`（命令提示符里的 `> NUL`、Git Bash 里的 `>/dev/null`）时会报错退出、不存快照；现在照常完成并保存快照。
- **命令行的英文单数**：有一个文件夹读不了的扫描写「1 folder couldn't be accessed」，`snapshot list` 写「1 snapshot in …」，`snapshot show` 写「· 1 item ·」。感谢 @swastiktawate（[#9](https://github.com/DuskXi/SpaceKnife/pull/9)）。
