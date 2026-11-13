# Unreleased changes · 未发布的更新

**English** · [简体中文](#简体中文)

What is already done for the next version, **0.3.6**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

- **Every coloring explains its colors.** Hover Classes, Flat, Time or Branch in the toolbar to see what each color means: the file classes, the time bands (under 1 day, 1 day – 1 week, 1 week – 1 month, 1 month – 1 year, over 1 year), or the top-level branches in view. Right-click one to open its settings: File classes for Classes, Appearance for the others.
- **The hover card says what a block's color stands for.** For "N small items" in Classes it names the class that holds at least half of their space ("Mostly Pictures"), or says the classes are mixed; in Time it gives how long ago the file was last modified; in Branch the top-level item it belongs to.
- **Hide the folders you already know** ([#26](https://github.com/DuskXi/SpaceKnife/issues/26)). List them in Settings › Visualization › Hidden folders, for example your music, movies or games. In a scan that holds any of them, Hide folders (H) appears in the toolbar (in the command palette: **Show/hide the folders listed in settings**): they take no room in the picture and the rest grows to fill it, until you turn it off. Zoomed into one, it shows as usual, and a folder's hover card says how much of it is in hidden folders. Nothing is hidden until you add a folder, and the scan itself still counts everything.

### Fixes

- **Changing the theme no longer freezes the window (Windows).** In 0.3.5 the window stopped responding the first time you switched between Light, Dark and Match system in a way that changed its colors, and it froze again at every start after that. If 0.3.5 is stuck for you, end it in Task Manager and get 0.3.6 from the [releases page](https://github.com/DuskXi/SpaceKnife/releases) (the update button can't be reached while the window is frozen): run the installer over it, or for a portable copy unpack the new zip and copy the old copy's `data` folder into it. 0.3.6 starts normally and keeps the theme you chose.
- **A whole-volume scan and the Disks page no longer hang while files change.** At the end of a scan of a whole drive, or when the Disks page showed an open scan of one, SpaceKnife could stop for good if the drive's free space changed in that moment (a busy system drive does so all the time): the scan stayed at "scanning" and the window stopped answering. Two rarer cases of the same kind are gone too, in the command palette and in the Delete basket while files change.
- **Waiting for administrator rights no longer stops SpaceKnife.** While Windows asked whether to allow the scan helper (or Linux asked for a password), the drive list and soon everything else waited with it.
- **Show in Explorer (Finder, file manager), Open and Open terminal here no longer freeze the window** when the item is on a network drive that went away or a disk that has to spin up first.
- **The start page checks a typed folder once.** It checked it again about three times a second for as long as the path stayed in the box, keeping network drives and sleeping disks awake.
- **In Scan details, the slider of a Custom disk load limit stays where you drag it** while a scan runs; progress updates used to put it back.

---

## 简体中文

下一版 **0.3.6** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

- **每种配色都有图例。** 鼠标停在工具栏的「类别」「扁平」「时间」「分支」上，就会看到每种颜色代表什么：各个文件类别、各个时间段（1 天内、1 天 – 1 周、1 周 – 1 个月、1 个月 – 1 年、1 年以上），或当前位置的各个顶层分支。右键直接打开相关设置：「类别」打开「文件类别」，其余打开「外观」。
- **悬停卡写出色块颜色的含义。** 类别配色下，「N 个小项」会写出占其中一半及以上空间的类别（「大部分是「图片」」），或说明是多种类别混合；时间配色下写出距上次修改多久；分支配色下写出属于哪个顶层分支。
- **隐藏你已经了解的文件夹**（[#26](https://github.com/DuskXi/SpaceKnife/issues/26)）。在「设置 › 可视化 › 隐藏的文件夹」里列出它们，比如音乐、影片、游戏。扫描里有其中的文件夹时，工具栏会出现「隐藏文件夹」（H；命令面板里是「显示/隐藏设置里列出的文件夹」）：它们不占画面，其余的块放大填满，关掉开关就回来。放大进其中一个时它照常显示，文件夹的悬停卡会写出其中有多少在隐藏的文件夹里。不添加就什么都不隐藏，扫描本身照样统计全部内容。

### 修复

- **切换主题不再让窗口卡死（Windows）。** 0.3.5 在浅色、深色和「跟随系统」之间切换时，只要窗口颜色真的变了，第一次就会停止响应，之后每次启动也都会卡住。如果你的 0.3.5 已经卡住：在任务管理器里结束它，再从[发布页](https://github.com/DuskXi/SpaceKnife/releases)下载 0.3.6（窗口卡住时点不到更新按钮）：安装版直接用安装包覆盖安装；便携版解压新的 zip，把旧副本的 `data` 文件夹复制进去。0.3.6 能正常启动，并保留你选的主题。
- **整卷扫描和「本机磁盘」页不再在文件变化时卡死。** 扫描整个磁盘结束时，或「本机磁盘」页显示一个打开着的整卷扫描时，如果那一刻磁盘的可用空间变了（繁忙的系统盘随时都在变），SpaceKnife 可能从此停住：扫描一直显示「扫描中」，窗口不再响应。同类但更少见的两处也修好了：文件变化时的命令面板和删除篮。
- **等待管理员授权时不再卡住整个程序。** Windows 询问是否允许扫描助手（或 Linux 要求输入密码）期间，磁盘列表会跟着等待，很快其他功能也会停住。
- **「在资源管理器中显示」（访达、文件管理器）、「打开」和「在此打开终端」不再让窗口卡住**，即使对象在已经断开的网络盘上，或在需要先唤醒的硬盘上。
- **起始页对输入的路径只检查一次。** 以前只要路径还留在输入框里，就会每秒检查约三次，让网络盘和休眠的硬盘一直醒着。
- **扫描进行中，「扫描详情」里「磁盘负载上限」选「自定义」时，滑块会停在你拖到的位置**，不再被进度更新拉回原处。
