# Unreleased changes · 未发布的更新

**English** · [简体中文](#简体中文)

What is already done for the next version, **0.3.5**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

- **SpaceKnife is rewritten.** The features and the interface are those of 0.3.4; what changes is the program underneath, the names of the release files and the name of the command-line program.
- **The backend is rewritten in Rust and runs inside the app.** Scanning, the store, filters, layouts, live monitoring, export and file operations are now Rust, and the server starts inside the app's own process: no separate frozen Python backend, no second process.
- **The window is rewritten with Tauri 2 and Vue.** The interface uses the system's WebView (WebView2 on Windows, WKWebView on macOS, WebKitGTK on Linux) instead of a bundled Chromium, so the download is smaller and it needs less memory.
- **The command-line program is `spaceknife-cli`** (in 0.3.x the program next to the app was `spaceknife-backend`), installed next to the app: `scan`, `run`, `snapshot list` / `show`, `serve`, `headless`, `version`.
- **Headless mode is `spaceknife-cli headless`** `[--listen <address>] [--yes] [--tls-cert FILE --tls-key FILE] [--ui DIR]`; the pages and the WebSocket share one address. 0.3.x's `--headless` and `--listen <address>` are still accepted, anywhere among the arguments, and on Windows `SpaceKnife --headless …` in a terminal still runs headless mode (`SpaceKnife.com` next to the app starts `spaceknife-cli` and waits for it). In a portable copy headless mode keeps its data in the `data` folder, as in 0.3.x. The warning before it opens to the network now says all that the link allows: also writing files anywhere your account can write, and asking for administrator rights on that computer.
- **New release file names** (Tauri's): `SpaceKnife_<version>_x64-setup.exe`, `SpaceKnife_<version>_amd64.AppImage`, `SpaceKnife_<version>_amd64.deb`, `SpaceKnife-<version>-1.x86_64.rpm`, `SpaceKnife_<version>_aarch64.dmg` / `SpaceKnife_<version>_x64.dmg`; the portable archives keep their names. The update check of 0.3.x does not find the files with new names; it does find the portable archives, but their **Seamless update** stops without changing anything. Users of 0.3.x download 0.3.5 once by hand.
- **The Windows installer installs for the current user only**; deb and rpm put `spaceknife` and `spaceknife-cli` in `/usr/bin` instead of `/opt/SpaceKnife`.
- **Settings and data stay where they were**: settings, file classes, classified folders, remembered tags, the palette's recent entries, the update state and logs are read from the same folders; on macOS `XDG_DATA_HOME` / `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` are followed as 0.3.x followed them. The old logs 0.3.x rotated (`logs/spaceknife.<time>.log`) are deleted on the first start. A `classes.json` or `class_folders.json` that cannot be read is never written over: the next save renames it and leaves it beside the new one (`classes.unreadable-<time>.json`). The window's own files (theme, WebView data) moved out of Electron's profile folder.
- **Snapshots of 0.3.x still open.** Snapshots saved by 0.3.x (format 2) open and compare as they are, with no conversion, and show in the lists; 0.3.5 saves format 3. The auto-save retention (the newest 10, then one a week) counts 0.3.x's snapshots too; a snapshot file that cannot be read (damaged, or of a newer format) is never deleted.

### Fixes

- **Command-line output to NUL**: `scan` with its output sent to `NUL` (`> NUL` in the command prompt, `>/dev/null` in Git Bash) ended with an error and without its snapshot; now it finishes and saves the snapshot.
- **Singular English counts on the command line**: a scan with one unreadable folder says "1 folder couldn't be accessed", `snapshot list` says "1 snapshot in …" and `snapshot show` "· 1 item ·". Thanks to @swastiktawate ([#9](https://github.com/DuskXi/SpaceKnife/pull/9)).

---

## 简体中文

下一版 **0.3.5** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

- **SpaceKnife 整个重写了**：功能和界面与 0.3.4 相同，变的是底下的程序、发布文件的名字和命令行程序的名字。
- **后端用 Rust 重写，跑在应用里面**：扫描、存储、过滤、布局、实时监控、导出和文件操作都换成了 Rust，服务端直接在应用进程里启动，不再另外带一个冻结的 Python 后端，也不再启动第二个进程。
- **窗口用 Tauri 2 + Vue 重写**：界面用系统自带的 WebView（Windows 上的 WebView2、macOS 上的 WKWebView、Linux 上的 WebKitGTK），不再打包一整个 Chromium，下载更小，占的内存也更少。
- **命令行程序是 `spaceknife-cli`**（0.3.x 时应用旁边的程序叫 `spaceknife-backend`），和应用装在一起：`scan`、`run`、`snapshot list` / `show`、`serve`、`headless`、`version`。
- **无头模式改成 `spaceknife-cli headless`** `[--listen <地址>] [--yes] [--tls-cert 证书 --tls-key 私钥] [--ui 目录]`，页面和 WebSocket 用同一个地址。0.3.x 的 `--headless`、`--listen <地址>` 写法仍然认，放在参数里的任何位置都行；Windows 上在终端里输入 `SpaceKnife --headless …` 也照样启动无头模式（应用旁边的 `SpaceKnife.com` 启动 `spaceknife-cli` 并等它结束）。便携版的无头模式和 0.3.x 一样把数据存在 `data` 文件夹里。在网络上开放之前的警告现在把链接能做的事都写全了：还能把文件写到你的账户能写的任何地方，还能在那台电脑上请求管理员权限。
- **发布文件换成 Tauri 的命名**：`SpaceKnife_<版本>_x64-setup.exe`、`SpaceKnife_<版本>_amd64.AppImage`、`SpaceKnife_<版本>_amd64.deb`、`SpaceKnife-<版本>-1.x86_64.rpm`、`SpaceKnife_<版本>_aarch64.dmg` / `SpaceKnife_<版本>_x64.dmg`；便携版的文件名不变。0.3.x 的更新检查找不到换了名字的文件；便携版它找得到，但「无缝更新」会中途停下、什么也不改。0.3.x 的用户要手动下载一次 0.3.5。
- **Windows 安装程序只装到当前用户**；deb / rpm 把 `spaceknife` 和 `spaceknife-cli` 装到 `/usr/bin`，不再放在 `/opt/SpaceKnife`。
- **设置和数据还在原来的位置**：设置、文件类别、归类的文件夹、记住的标签、命令面板的最近记录、更新记录和日志都从原来的文件夹读取；macOS 上的 `XDG_DATA_HOME` / `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` 和 0.3.x 一样照用。0.3.x 轮转下来的旧日志（`logs/spaceknife.<时间>.log`）在第一次启动时删掉。读不了的 `classes.json` / `class_folders.json` 不会被覆盖：下次保存时改名留在原处（`classes.unreadable-<时间>.json`）。窗口自己的文件（主题、WebView 的数据）不再放在 Electron 的配置文件夹里。
- **0.3.x 的快照照样能用**：0.3.x 存的快照（第 2 版）不用转换就能打开、对比，也出现在列表里；0.3.5 存的是第 3 版。自动保存的保留规则（最近 10 份，更早的每周留一份）也算上 0.3.x 的快照；读不了的快照文件（损坏的、更新版本的）从不删除。

### 修复

- **命令行输出到 NUL**：`scan` 的输出送到 `NUL`（命令提示符里的 `> NUL`、Git Bash 里的 `>/dev/null`）时会报错退出、不存快照；现在照常完成并保存快照。
- **命令行的英文单数**：有一个文件夹读不了的扫描写「1 folder couldn't be accessed」，`snapshot list` 写「1 snapshot in …」，`snapshot show` 写「· 1 item ·」。感谢 @swastiktawate（[#9](https://github.com/DuskXi/SpaceKnife/pull/9)）。
