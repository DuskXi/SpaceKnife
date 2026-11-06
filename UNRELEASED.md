# Unreleased changes · 未发布的更新

**English** · [简体中文](#简体中文)


What is already done for the next version, **0.4.0**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

- **SpaceKnife is rewritten.** The features and the interface are those of 0.3.4; what changes is the program underneath, the names of the release files and the name of the command-line program.
- **The backend is rewritten in Rust and runs inside the app.** Scanning, the store, filters, layouts, live monitoring, export and file operations are now Rust, and the server starts inside the app's own process: no separate frozen Python backend, no second process.
- **The window is rewritten with Tauri 2 and Vue.** The interface uses the system's WebView (WebView2 on Windows, WKWebView on macOS, WebKitGTK on Linux) instead of a bundled Chromium, so the download is smaller and it needs less memory.
- **The command-line program is `spaceknife-cli`**, installed next to the app (replacing `spaceknife-backend`): `scan`, `run`, `snapshot list` / `show`, `serve`, `headless`, `version`.
- **Headless mode is `spaceknife-cli headless`** `[--listen <address>] [--yes] [--tls-cert FILE --tls-key FILE] [--ui DIR]`; the pages and the WebSocket share one address. `--headless` and `--listen <address>` are still accepted, by `spaceknife-cli`.
- **New release file names** (Tauri's): `SpaceKnife_<version>_x64-setup.exe`, `SpaceKnife_<version>_amd64.AppImage`, `SpaceKnife_<version>_amd64.deb`, `SpaceKnife-<version>-1.x86_64.rpm`, `SpaceKnife_<version>_aarch64.dmg` / `SpaceKnife_<version>_x64.dmg`; the portable archives keep their names. The update check of 0.3.x does not know these names: users of 0.3.x download 0.4.0 once by hand.
- **The Windows installer installs for the current user only**; deb and rpm put `spaceknife` and `spaceknife-cli` in `/usr/bin` instead of `/opt/SpaceKnife`.
- **Settings and data stay where they were**: settings, file classes, classified folders, tags, recent palette entries and logs are read from the same folders. The window's own files (theme, WebView data) moved out of Electron's profile folder.
- **Snapshots have a new format (3).** Snapshots saved by 0.3.x are not opened by 0.4.0 and do not show in the lists; the files stay on disk.

---

## 简体中文

下一版 **0.4.0** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

- **SpaceKnife 整个重写了**：功能和界面与 0.3.4 相同，变的是底下的程序、发布文件的名字和命令行程序的名字。
- **后端用 Rust 重写，跑在应用里面**：扫描、存储、过滤、布局、实时监控、导出和文件操作都换成了 Rust，服务端直接在应用进程里启动，不再另外带一个冻结的 Python 后端，也不再启动第二个进程。
- **窗口用 Tauri 2 + Vue 重写**：界面用系统自带的 WebView（Windows 上的 WebView2、macOS 上的 WKWebView、Linux 上的 WebKitGTK），不再打包一整个 Chromium，下载更小，占的内存也更少。
- **命令行程序改名 `spaceknife-cli`**，和应用装在一起（替代 `spaceknife-backend`）：`scan`、`run`、`snapshot list` / `show`、`serve`、`headless`、`version`。
- **无头模式改成 `spaceknife-cli headless`** `[--listen <地址>] [--yes] [--tls-cert 证书 --tls-key 私钥] [--ui 目录]`，页面和 WebSocket 用同一个地址。`--headless`、`--listen <地址>` 的写法仍然认，但要交给 `spaceknife-cli`。
- **发布文件换成 Tauri 的命名**：`SpaceKnife_<版本>_x64-setup.exe`、`SpaceKnife_<版本>_amd64.AppImage`、`SpaceKnife_<版本>_amd64.deb`、`SpaceKnife-<版本>-1.x86_64.rpm`、`SpaceKnife_<版本>_aarch64.dmg` / `SpaceKnife_<版本>_x64.dmg`；便携版的文件名不变。0.3.x 的更新检查认不出这些新名字：0.3.x 的用户要手动下载一次 0.4.0。
- **Windows 安装程序只装到当前用户**；deb / rpm 把 `spaceknife` 和 `spaceknife-cli` 装到 `/usr/bin`，不再放在 `/opt/SpaceKnife`。
- **设置和数据还在原来的位置**：设置、文件类别、归类的文件夹、标签、命令面板的最近记录和日志都从原来的文件夹读取。窗口自己的文件（主题、WebView 的数据）不再放在 Electron 的配置文件夹里。
- **快照换了新格式（第 3 版）**：0.3.x 存的快照 0.4.0 打不开，也不会出现在列表里；旧文件仍留在磁盘上。
=======
What is already done for the next version, **0.3.5**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

(nothing yet)

### Fixes

- **Singular English counts on the command line**: a scan with one unreadable folder says "1 folder couldn't be accessed", `snapshot list` says "1 snapshot in …" and `snapshot show` "· 1 item ·". Thanks to @swastiktawate ([#9](https://github.com/DuskXi/SpaceKnife/pull/9)).

---

## 简体中文

下一版 **0.3.5** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

（暂无）

### 修复

- **命令行的英文单数**：有一个文件夹读不了的扫描写「1 folder couldn't be accessed」，`snapshot list` 写「1 snapshot in …」，`snapshot show` 写「· 1 item ·」。感谢 @swastiktawate（[#9](https://github.com/DuskXi/SpaceKnife/pull/9)）。

