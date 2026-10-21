# Unreleased changes · 未发布的更新

**English** · [简体中文](#简体中文)

What is already done for the next version, **0.3.4**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

- **Disks** keeps what a complete scan of a whole volume showed (its file classes, largest folders, counts and scan time) per volume, so the volume still shows them after you leave the page, close the scan's tab, reload the window or restart SpaceKnife, until the next complete scan. Only the same volume gets them back (drive letter or mount point and volume serial), so another drive on the same letter never does.
- **Disks** has a big **File classes** bar at the top: the classes of every volume with a known mix added up, with each class's size and share, and which volumes it covers.
- The help page **Visualizations** explains how the treemap, the flame graph and the polar flame graph each organize the picture, with a table that sets them side by side. The polar flame graph reads like a stacked pie chart with no center area: each ring is one folder level, the angle is the share. A picture of each shows on that page, in the app's help as on the website, and on the website's home page.

### Fixes

- **Closing a tab while its snapshot loads no longer keeps the scan in memory.** A snapshot that finished loading, or a scan that started, after its tab was closed stayed in the backend with no tab until SpaceKnife quit (about 1.1 GB for a snapshot of 8.6 million items); a started scan even kept running. Now it is freed at once.
- **The toolbar fits narrow windows.** In a window narrower than about 1,400 px the toolbar's right end was cut off: at the smallest window size the split, command palette and delete basket buttons were out of reach, and the path could disappear. Now the filter box shrinks first, then the chips show only their swatch, and the coloring, the chips and last the visualization switch move into a new **More** (…) menu, so every control stays reachable at any window size, in a comparison too.

---

## 简体中文

下一版 **0.3.4** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

- 「本机磁盘」按卷保留整卷完整扫描的结果（文件类别、最大的文件夹、文件数和扫描时间）：离开这一页、关掉扫描的标签页、重新载入窗口或重启 SpaceKnife 之后，卷下面照样显示，直到下一次完整扫描。只有同一个卷才会拿回它（盘符或挂载点和卷序列号都相同），换一个盘插在同一个盘符上不会。
- 「本机磁盘」顶部加了一条大的「文件类别」条：所有已知类别构成的卷加在一起，写出每类多大、占多少，以及算进了哪些卷。
- 帮助页「三种可视化」讲清楚树图、火焰图和火焰-极坐标图各自怎样组织信息，加了一张对照表。火焰-极坐标图可以看成一种多层堆叠、没有中心区域的特殊饼图：每一环是一层文件夹，角度就是比例。这一页给三种可视化各配了一张图，软件的帮助页和官网都显示，官网首页也有。

### 修复

- **快照加载途中关掉标签，扫描不再一直占着内存**：标签关掉以后才加载完的快照，或者才开始的扫描，会一直留在后端、没有标签显示，直到退出 SpaceKnife（一个 860 万项的快照约 1.1 GB）；已经开始的扫描甚至会继续跑。现在会立即释放。
- **窄窗口下工具栏不再被截断**：窗口窄于约 1400 像素时，工具栏右端会被截掉，最小窗口下分屏、命令面板和删除篮按钮都点不到，路径也可能消失。现在先压缩过滤框，再把芯片缩成色块，然后依次把配色、芯片、最后是可视化切换收进新的「更多」（…）菜单，任何窗口大小下每个控件都点得到，快照对比时也一样。
