# Unreleased changes · 未发布的更新

**English** · [简体中文](#简体中文)

What is already done for the next version, **0.3.7**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

(nothing yet)

### Fixes

- **SpaceKnife closes by itself when the installer or another program asks it to (Windows).** Installing a new version while SpaceKnife was open, the installer's request to close it went unanswered, and SpaceKnife was ended 5 seconds later instead of closing. This version's installer also closes a running 0.3.5 or 0.3.6 properly. Closing the window yourself (its close button, Alt+F4, the taskbar) always worked.

---

## 简体中文

下一版 **0.3.7** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

（暂无）

### 修复

- **安装程序或别的程序请求关闭时，SpaceKnife 会自己关闭（Windows）。** 以前开着 SpaceKnife 安装新版本时，安装程序的关闭请求得不到回应，SpaceKnife 要等 5 秒后被强制结束，而不是自己关掉。这一版的安装程序也能正常关闭正在运行的 0.3.5 或 0.3.6。自己关窗口（关闭按钮、Alt+F4、任务栏）一直都正常。
