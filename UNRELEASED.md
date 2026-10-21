# Unreleased changes · 未发布的更新

**English** · [简体中文](#简体中文)

What is already done for the next version, **0.3.4**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

- **Disks** keeps what a complete scan of a whole volume showed (its file classes, largest folders, counts and scan time) per volume, so the volume still shows them after you leave the page, close the scan's tab, reload the window or restart SpaceKnife, until the next complete scan. Only the same volume gets them back (drive letter or mount point and volume serial), so another drive on the same letter never does.
- **Disks** has a big **File classes** bar at the top: the classes of every volume with a known mix added up, with each class's size and share, and which volumes it covers.
- The help page **Visualizations** explains how the treemap, the flame graph and the polar flame graph each organize the picture, with a table that sets them side by side. The polar flame graph reads like a stacked pie chart with no center area: each ring is one folder level, the angle is the share. A picture of each shows on that page, in the app's help as on the website, and on the website's home page.
- **Large scans take far less private memory.** A scan of more than about two million items now keeps its data in memory-mapped files in the cache folder (**Settings › Advanced › Memory › Keep large scans in mapped files**, on by default). The system keeps the parts in use in RAM and can drop the rest at any time. On a drive of 3.2 million items the backend now settles at about 250 MB instead of about 600 MB, and peaks at about 400 MB while scanning instead of about 750 MB; scans are as fast as before. The cost: after a scan its data is written to those files once, about 200 bytes per item. Turn the switch off to keep scans in memory (only the memory limit below it then moves them).

### Fixes

- **Closing a tab while its snapshot loads no longer keeps the scan in memory.** A snapshot that finished loading, or a scan that started, after its tab was closed stayed in the backend with no tab until SpaceKnife quit (about 1.1 GB for a snapshot of 8.6 million items); a started scan even kept running. Now it is freed at once.
- **The toolbar fits narrow windows.** In a window narrower than about 1,400 px the toolbar's right end was cut off: at the smallest window size the split, command palette and delete basket buttons were out of reach, and the path could disappear. Now the filter box shrinks first, then the chips show only their swatch, and the coloring, the chips and last the visualization switch move into a new **More** (…) menu, so every control stays reachable at any window size, in a comparison too.
- **MFT fast scans need far less memory at their peak.** Building the tree of a large volume used to take about 325 bytes per item on top of the table read from the $MFT, for a moment: on a drive of 8.6 million items the backend reached about 3.6 GB (SpaceKnife as a whole 3.8 GB). It now takes about 85 bytes per item, so the same drive should peak at about 2.2 GB, with the same result and speed.
- **Memory no longer jumps every few seconds while files change after a scan.** With live monitoring on, every batch of changes made the details panel work out the whole drive's summary again, often several at once, each needing about 100 bytes per item for a moment (about 0.85 GB on a drive of 8.6 million items); and the first change after a scan doubled the memory of the scan data for good. Now the summary is worked out in small pieces, once for everyone who asks for the same one, and refreshes less often on large scans (at most every 10 seconds); a change adds only a little room. On a drive of 3.2 million items with files changing for a minute, the backend's peak went from about 1.3 GB to about 660 MB (300 MB with the mapped files above), and it settles at about 600 MB instead of 835 MB.

---

## 简体中文

下一版 **0.3.4** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

- 「本机磁盘」按卷保留整卷完整扫描的结果（文件类别、最大的文件夹、文件数和扫描时间）：离开这一页、关掉扫描的标签页、重新载入窗口或重启 SpaceKnife 之后，卷下面照样显示，直到下一次完整扫描。只有同一个卷才会拿回它（盘符或挂载点和卷序列号都相同），换一个盘插在同一个盘符上不会。
- 「本机磁盘」顶部加了一条大的「文件类别」条：所有已知类别构成的卷加在一起，写出每类多大、占多少，以及算进了哪些卷。
- 帮助页「三种可视化」讲清楚树图、火焰图和火焰-极坐标图各自怎样组织信息，加了一张对照表。火焰-极坐标图可以看成一种多层堆叠、没有中心区域的特殊饼图：每一环是一层文件夹，角度就是比例。这一页给三种可视化各配了一张图，软件的帮助页和官网都显示，官网首页也有。
- **大扫描占的私有内存少得多**：约 200 万项以上的扫描，数据存进缓存文件夹里的内存映射文件（「设置 › 高级 › 内存 › 大扫描存进映射文件」，默认开）。常用的部分由系统留在内存里，其余的随时可以丢掉。320 万项的盘，后端扫完稳定在约 250 MB（原来约 600 MB），扫描中峰值约 400 MB（原来约 750 MB），扫描速度不变。代价是扫完后把数据写进这些文件一次，每项约 200 字节。关掉这个开关，扫描就留在内存里（只按下面的内存上限切换）。

### 修复

- **快照加载途中关掉标签，扫描不再一直占着内存**：标签关掉以后才加载完的快照，或者才开始的扫描，会一直留在后端、没有标签显示，直到退出 SpaceKnife（一个 860 万项的快照约 1.1 GB）；已经开始的扫描甚至会继续跑。现在会立即释放。
- **窄窗口下工具栏不再被截断**：窗口窄于约 1400 像素时，工具栏右端会被截掉，最小窗口下分屏、命令面板和删除篮按钮都点不到，路径也可能消失。现在先压缩过滤框，再把芯片缩成色块，然后依次把配色、芯片、最后是可视化切换收进新的「更多」（…）菜单，任何窗口大小下每个控件都点得到，快照对比时也一样。
- **MFT 快速扫描的内存峰值大幅降低**：大盘建树的那一刻，以前要在读到的 $MFT 表之外再占每项约 325 字节：860 万项的盘后端会冲到约 3.6 GB（整个 SpaceKnife 约 3.8 GB）。现在每项约 85 字节，同一块盘的峰值应在约 2.2 GB，结果和速度都不变。
- **扫完后文件持续变化时，内存不再每隔几秒冒高**：开着实时监控时，每批文件变化都让详情面板把整盘汇总重算一遍，还经常几份同时算，每份临时要每项约 100 字节（860 万项的盘约 0.85 GB）；扫完后的第一次文件变化还会让扫描数据的内存一直翻倍。现在汇总分小块计算，同一份只算一次，大扫描时刷新得慢一些（最长每 10 秒一次）；文件变化只多留一点余量。320 万项的盘、文件连续变化一分钟：后端峰值从约 1.3 GB 降到约 660 MB（加上上面的映射文件约 300 MB），之后稳定在约 600 MB，原来是 835 MB。
