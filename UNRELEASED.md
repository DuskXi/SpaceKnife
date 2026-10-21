# Unreleased changes · 未发布的更新

**English** · [简体中文](#简体中文)

What is already done for the next version, **0.3.2**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

- **Help inside the app**: the [documentation](https://spaceknife.kaltsit.dev/docs/) now ships with SpaceKnife and works offline.
  - <kbd>F1</kbd> opens the page about where you are: the start page opens "Your first scan", the filter box opens "Filters", and the command palette opens its own page.
  - You can also open it with the Help button in the title bar or the **Help** command in the palette.
  - The help has its own search (<kbd>/</kbd>). **View on the website** opens the same page in your browser.
- **<kbd>F5</kbd>, <kbd>Shift</kbd>+<kbd>F5</kbd> and <kbd>Ctrl</kbd>+<kbd>S</kbd>** now work for **Rescan current view**, **Rescan everything** and **Save snapshot**. They also work while you type in the filter box.
  - Where the palette would show the command grayed out, the key does nothing and a note at the bottom of the window says why.
  - Held down, a key runs once.
- **AppImage updates**: the AppImage carries update information, and each release has a `.zsync` file next to it, so AppImageUpdate and AppImageLauncher can update it in place.

### Fixes

- **Rescan current view in a comparison**: it used to read an unrelated folder of the scan. It is now unavailable there and says why.
- **Rescan current view at the top of a scan of several locations**: it used to do nothing. It now reads every location again.
- **Corrected setting descriptions**: theme, animation frame rate, low I/O priority when idle, and live monitoring on Linux.
- **Default for incremental rescans** is grayed out: nothing uses it yet.

---

## 简体中文

下一版 **0.3.2** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

- **软件内帮助**：[文档](https://spaceknife.kaltsit.dev/zh/docs/)随软件一起打包，离线也能看。
  - <kbd>F1</kbd> 打开讲当前界面的那一页：启动页打开「第一次扫描」，过滤框打开「过滤」，命令面板打开它自己的那一页。
  - 也可以从标题栏的帮助按钮或命令面板的「帮助」打开。
  - 帮助有自己的搜索框（<kbd>/</kbd>）；「在网站上查看」在浏览器里打开官网的同一页。
- **<kbd>F5</kbd>、<kbd>Shift</kbd>+<kbd>F5</kbd>、<kbd>Ctrl</kbd>+<kbd>S</kbd>** 开始生效，分别对应「重新扫描当前视图」「全盘重新扫描」「保存快照」。在过滤框里打字时也能用。
  - 在命令面板里显示为灰色的命令，按键也不执行，窗口底部会提示原因。
  - 按住不放只执行一次。
- **AppImage 增量更新**：AppImage 里写入了更新信息，每次发布都附带 `.zsync` 文件，AppImageUpdate、AppImageLauncher 之类的工具可以直接就地更新。

### 修复

- **对比视图里的「重新扫描当前视图」**：以前会去重扫扫描结果里另一个不相干的文件夹；现在在对比视图里不可用，并提示原因。
- **多个扫描位置的最顶层**：「重新扫描当前视图」以前什么都不做，现在会把每个位置都重读一遍。
- **设置说明更正**：主题、动画帧率、空闲时使用低 I/O 优先级、Linux 上的实时监控。
- **「增量重扫默认」**：还没有接入，现在显示为灰色。
