# Unreleased changes · 未发布的更新

**English** · [简体中文](#简体中文)

What is already done for the next version, **0.3.3**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

- **Restart and update.** A new version is downloaded and checked in the background (**Settings › About & updates › Download updates automatically**, on by default). Then one click on **Restart and update** puts it in place and opens SpaceKnife again: the Windows installer runs silently, a portable copy swaps its program files (its `data` folder stays, and the old files come back if something can't be replaced), an AppImage replaces its file. The window says "Updated to SpaceKnife …" once. Disk images and deb / rpm packages still open in the system's installer.
- **Nothing left behind by updates.** Update files go into the `updates` folder in SpaceKnife's data folder instead of your Downloads folder, and each start removes those of the version you run or older.
- **Clean uninstall on Windows.** The uninstaller removes the window's caches, the cache and downloaded updates. Its new **Your data** page asks about the snapshots, settings and logs: kept unless you tick **Also delete the snapshots, settings and logs**. A silent uninstall keeps them, or removes them with `--delete-app-data`.
- **About & updates**, a new settings page (also in the command palette, and linked from **Settings › General**): your version and how it was installed, the last and next update check, the update card when there is one, the versions of the components with **Copy version info**, and a timeline of the last 30 releases with their notes in the interface language.

### Fixes

- **Deleted space no longer shows as unknown.** What was deleted while a scan was monitored (in Explorer, by emptying the Recycle Bin, by another program) stayed counted as used in the open view, so the freed space grew the unknown-space block while the free-space block kept its old size. The view now follows the volume's free space, read again right after each change.

---

## 简体中文

下一版 **0.3.3** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

- **重启并更新**：新版本在后台下载并校验（「设置 › 关于与更新 › 自动下载更新」，默认打开），之后点一下「重启并更新」就换上新版本并重新打开 SpaceKnife：Windows 安装版静默安装；便携版替换程序文件（`data` 文件夹保留，有文件替换不了时放回旧文件）；AppImage 替换文件本身。窗口会提示一次「已更新到 SpaceKnife …」。磁盘映像和 deb / rpm 包仍然交给系统的安装器打开。
- **更新不留残余**：更新文件放进 SpaceKnife 数据文件夹里的 `updates`，不再放进「下载」文件夹；每次启动都会删掉正在运行的版本及更旧版本的更新文件。
- **Windows 上干净卸载**：卸载程序会删除界面缓存、缓存和下载的更新。新的「你的数据」一页询问快照、设置和日志：默认保留，勾上「同时删除快照、设置和日志」才删除。静默卸载默认保留，加 `--delete-app-data` 则删除。
- **「关于与更新」**设置页（命令面板里也有，「设置 › 常规」里有入口）：你的版本和安装方式、上次与下次检查更新的时间、有新版本时的更新卡片、各组件的版本和「复制版本信息」，以及最近 30 个版本的发布时间线，发布说明按界面语言显示。

### 修复

- **删除的空间不再显示成未知空间**：扫描在监控时被删掉的东西（在资源管理器里删、清空回收站、别的程序删除），在已打开的视图里仍按原来的已用空间计算，释放出来的空间就变成了「未知空间」，可用空间块却还是原来的大小。现在视图跟着卷的可用空间走，每次变化后都会重新读取。
