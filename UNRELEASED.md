# Unreleased changes · 未发布的更新

**English** · [简体中文](#简体中文)

What is already done for the next version, **0.3.4**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

- **Disks** keeps what a complete scan of a whole volume showed (its file classes, largest folders, counts and scan time) per volume, so the volume still shows them after you leave the page, close the scan's tab, reload the window or restart SpaceKnife, until the next complete scan. Only the same volume gets them back (drive letter or mount point and volume serial), so another drive on the same letter never does.
- **Disks** has a big **File classes** bar at the top: the classes of every volume with a known mix added up, with each class's size and share, and which volumes it covers.
- The help page **Visualizations** explains how the treemap, the flame graph and the polar flame graph each organize the picture, with a table that sets them side by side. The polar flame graph reads like a stacked pie chart with no center area: each ring is one folder level, the angle is the share. The website shows a picture of each, on that page and on the home page.

### Fixes

(nothing yet)

---

## 简体中文

下一版 **0.3.4** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

- 「本机磁盘」按卷保留整卷完整扫描的结果（文件类别、最大的文件夹、文件数和扫描时间）：离开这一页、关掉扫描的标签页、重新载入窗口或重启 SpaceKnife 之后，卷下面照样显示，直到下一次完整扫描。只有同一个卷才会拿回它（盘符或挂载点和卷序列号都相同），换一个盘插在同一个盘符上不会。
- 「本机磁盘」顶部加了一条大的「文件类别」条：所有已知类别构成的卷加在一起，写出每类多大、占多少，以及算进了哪些卷。
- 帮助页「三种可视化」讲清楚树图、火焰图和火焰-极坐标图各自怎样组织信息，加了一张对照表。火焰-极坐标图可以看成一种多层堆叠、没有中心区域的特殊饼图：每一环是一层文件夹，角度就是比例。官网在这一页和首页上为三种可视化各配了一张图。

### 修复

（暂无）
