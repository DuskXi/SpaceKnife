# Unreleased changes · 未发布的更新

**English** · [简体中文](#简体中文)

What is already done for the next version, **0.3.7**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

- **Hide a folder right from the picture** ([#26](https://github.com/DuskXi/SpaceKnife/issues/26)). Right-click a folder's block, bar or sector and choose **Hide this folder from the picture**: it goes on the list in Settings › Visualization › Hidden folders and leaves the picture at once, with no path to type. For a folder on the list the item reads **Stop hiding this folder** (it is drawn while Hide folders is off, or while you are zoomed into it). The command palette has both for the selected folder. They aren't offered for the scan's root, the MFT quick scan's `$NTFS metadata` and `$Orphaned files`, or in snapshot comparisons: the item is grayed out and says why.

### Fixes

- **SpaceKnife closes by itself when the installer or another program asks it to (Windows).** Installing a new version while SpaceKnife was open, the installer's request to close it went unanswered, and SpaceKnife was ended 5 seconds later instead of closing. This version's installer also closes a running 0.3.5 or 0.3.6 properly. Closing the window yourself (its close button, Alt+F4, the taskbar) always worked.

---

## 简体中文

下一版 **0.3.7** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

- **在画面里直接隐藏文件夹**（[#26](https://github.com/DuskXi/SpaceKnife/issues/26)）。右键单击文件夹的块、条或扇区，选「在画面中隐藏这个文件夹」：它加进「设置 › 可视化 › 隐藏的文件夹」，立即离开画面，不用再输入路径。已经在列表里的文件夹，这一项是「不再隐藏这个文件夹」（「隐藏文件夹」关着时，或放大在它里面时，画面里能看到它）。命令面板对选中的文件夹也有这两条命令。扫描的根、MFT 快速扫描的 `$NTFS 元数据` 和 `$孤立文件`、快照对比里不能用：这一项是灰色的，并说明原因。

### 修复

- **安装程序或别的程序请求关闭时，SpaceKnife 会自己关闭（Windows）。** 以前开着 SpaceKnife 安装新版本时，安装程序的关闭请求得不到回应，SpaceKnife 要等 5 秒后被强制结束，而不是自己关掉。这一版的安装程序也能正常关闭正在运行的 0.3.5 或 0.3.6。自己关窗口（关闭按钮、Alt+F4、任务栏）一直都正常。
