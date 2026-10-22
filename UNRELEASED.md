# Unreleased changes · 未发布的更新

**English** · [简体中文](#简体中文)

What is already done for the next version, **0.3.4**, and not in a release yet. When that version is published, this list becomes its release notes and the file starts over.

### What's new

- **Disks** keeps what a complete scan of a whole volume showed (its file classes, largest folders, counts and scan time) per volume, so the volume still shows them after you leave the page, close the scan's tab, reload the window or restart SpaceKnife, until the next complete scan. Only the same volume gets them back (drive letter or mount point and volume serial), so another drive on the same letter never does.
- **Disks** has a big **File classes** bar at the top: the classes of every volume with a known mix added up, with each class's size and share, and which volumes it covers.
- The help page **Visualizations** explains how the treemap, the flame graph and the polar flame graph each organize the picture, with a table that sets them side by side. The polar flame graph reads like a stacked pie chart with no center area: each ring is one folder level, the angle is the share. A picture of each shows on that page, in the app's help as on the website, and on the website's home page.
- **New switch: Keep large scans in mapped files** (**Settings › Advanced › Memory**, off by default). When on, a scan of more than about two million items keeps its data in memory-mapped files in the cache folder instead of private memory; the system keeps the parts in use in RAM and can drop the rest at any time. On a drive of 3.2 million items the backend then settles at about 250 MB instead of about 600 MB, and peaks at about 400 MB while scanning instead of about 750 MB, at the same speed. The cost, and why it is off by default: after a scan its data is written to those files once, about 200 bytes per item (about 1.8 GB for a drive of 8.6 million items).

### Fixes

- **Hard scans on Windows write far less to the drive.** Listing a folder used to update its last access time, so on a drive that keeps access times, one hard scan of a drive of 8.7 million items wrote 1.5–3.3 GB and left every folder looking as if it had just been opened. Now folders are opened so that their access times stay as they were. Where that is not allowed, a folder is opened as before: on a read-only drive, for a standard user in system folders, and even for an administrator in folders only Windows itself may change, such as `C:\Windows\WinSxS` (about one folder in six on a system drive, so a hard scan of the whole system drive still writes about 0.25–0.7 GB).
- **Hard scans on Windows are faster.** A folder found during a scan is now opened from its parent folder, which is still open, instead of from the root of the drive again. With the folders in memory, a ReFS drive of 3.2 million items takes about 3.8 s instead of 5.2 s, and an NTFS system drive of 8.8 million items 12.6 s instead of 13.1 s. Every folder opened this way is checked to be the one its parent listed (the same file ID); anything else is opened from the root as before. What a scan finds is the same: on an NTFS, a ReFS and a WinBtrfs drive, scans with and without it were compared folder by folder, on the NTFS drive also with what the disk itself holds.
- **Closing a tab while its snapshot loads no longer keeps the scan in memory.** A snapshot that finished loading, or a scan that started, after its tab was closed stayed in the backend with no tab until SpaceKnife quit (about 1.1 GB for a snapshot of 8.6 million items); a started scan even kept running. Now it is freed at once.
- **The toolbar fits narrow windows.** In a window narrower than about 1,400 px the toolbar's right end was cut off: at the smallest window size the split, command palette and delete basket buttons were out of reach, and the path could disappear. Now the filter box shrinks first, then the chips show only their swatch, and the coloring, the chips and last the visualization switch move into a new **More** (…) menu, so every control stays reachable at any window size, in a comparison too.
- **MFT fast scans need far less memory at their peak.** Building the tree of a large volume used to take about 325 bytes per item on top of the table read from the $MFT, for a moment: on a drive of 8.6 million items the backend reached about 3.6 GB (SpaceKnife as a whole 3.8 GB). It now takes about 85 bytes per item, so the same drive should peak at about 2.2 GB, with the same result and speed.
- **Memory no longer jumps every few seconds while files change after a scan.** With live monitoring on, every batch of changes made the details panel work out the whole drive's summary again, often several at once, each needing about 100 bytes per item for a moment (about 0.85 GB on a drive of 8.6 million items); and the first change after a scan doubled the memory of the scan data for good. Now the summary is worked out in small pieces, once for everyone who asks for the same one, and refreshes less often on large scans (at most every 10 seconds); a change adds only a little room. On a drive of 3.2 million items with files changing for a minute, the backend's peak went from about 1.3 GB to about 660 MB (about 300 MB with **Keep large scans in mapped files** on), and it settles at about 600 MB instead of 835 MB.
- **Scans moved to memory-mapped files write half as much to disk.** Once the scan data passed **Scan data memory limit**, each mapped file was first filled with zeros on Windows, and copied to a new file every time it grew. Now the files grow in place and only the data itself is written: on a drive of 3.2 million items about 0.67 GB instead of about 1.3 GB.
- **Live monitoring no longer makes memory jump by gigabytes after a while.** Once many files had changed, the backend renumbered the whole tree, and for a moment needed about 760 bytes per item on top of it: about 6.3 GB more on a drive of 8.7 million items. It now needs about 1 GB, the size of the renumbered copy itself, at the same speed.
- **Opening a snapshot needs half the memory.** Each column was first unpacked into a temporary copy, so a snapshot of 8.7 million items briefly needed 1.8 GB for the 0.95 GB it keeps. Now it is unpacked straight into place and needs just the 0.95 GB.
- **The backend keeps less memory all the time.**
  - About 50 MB less: the command palette's pinyin search no longer loads a Chinese dictionary when SpaceKnife starts.
  - 8 bytes less per item while a scan is open (about 66 MB on a drive of 8.6 million items): the treemap and the palette work out sizes only for what they show, instead of an array for every item that was worked out again after every file change. The treemap also redraws faster after a change.
- **A hard scan of a drive scanned before sets its memory aside once,** from the number of items the last complete scan found, instead of growing it step by step and copying everything each time.
- **The window gives memory back when it is hidden.** A minimized or covered window releases its drawing surfaces and draws again when it shows. Moving the pointer over the treemap now reuses the same buffers, so it no longer allocates new memory every time something lights up.

---

## 简体中文

下一版 **0.3.4** 已经做完、但还没发布的更改。这一版发布时，这里的内容会成为它的发布说明，然后这个文件清空，重新开始记录。

### 新功能

- 「本机磁盘」按卷保留整卷完整扫描的结果（文件类别、最大的文件夹、文件数和扫描时间）：离开这一页、关掉扫描的标签页、重新载入窗口或重启 SpaceKnife 之后，卷下面照样显示，直到下一次完整扫描。只有同一个卷才会拿回它（盘符或挂载点和卷序列号都相同），换一个盘插在同一个盘符上不会。
- 「本机磁盘」顶部加了一条大的「文件类别」条：所有已知类别构成的卷加在一起，写出每类多大、占多少，以及算进了哪些卷。
- 帮助页「三种可视化」讲清楚树图、火焰图和火焰-极坐标图各自怎样组织信息，加了一张对照表。火焰-极坐标图可以看成一种多层堆叠、没有中心区域的特殊饼图：每一环是一层文件夹，角度就是比例。这一页给三种可视化各配了一张图，软件的帮助页和官网都显示，官网首页也有。
- **新开关「大扫描存进映射文件」**（「设置 › 高级 › 内存」，默认关）：打开后，约 200 万项以上的扫描把数据存进缓存文件夹里的内存映射文件，不占私有内存；常用的部分由系统留在内存里，其余的随时可以丢掉。320 万项的盘，后端扫完稳定在约 250 MB（不开约 600 MB），扫描中峰值约 400 MB（不开约 750 MB），扫描速度不变。代价是扫完后把数据写进这些文件一次，每项约 200 字节（860 万项的盘约 1.8 GB），所以默认关。

### 修复

- **Windows 上的硬扫描写盘大大减少**：以前每列一个文件夹都会更新它的「最后访问时间」，盘开着访问时间更新时，硬扫描一次 870 万项的盘要往盘里写 1.5–3.3 GB，扫完所有文件夹看起来都像刚被打开过。现在打开文件夹时让访问时间保持原样。不允许这样打开的地方照旧按原来的方式打开：只读的盘、标准用户进系统文件夹，以及只有 Windows 自己能改的文件夹（例如 `C:\Windows\WinSxS`，管理员也一样；系统盘上约六分之一的文件夹），所以硬扫描整个系统盘仍会写约 0.25–0.7 GB。
- **Windows 上的硬扫描更快**：扫描中发现的文件夹，现在从还开着的父文件夹打开，不再每次从盘的根目录逐级找下去。文件夹都在内存里时，321 万项的 ReFS 盘从约 5.2 秒降到约 3.8 秒，880 万项的 NTFS 系统盘从 13.1 秒降到 12.6 秒。这样打开的每个文件夹都要核对是父文件夹列出的那一个（文件 ID 相同），对不上的照旧从根目录打开。扫描结果不变：在 NTFS、ReFS 和 WinBtrfs 盘上，开与不开逐个文件夹比对过，NTFS 盘还和盘上实际记录的内容比对过。
- **快照加载途中关掉标签，扫描不再一直占着内存**：标签关掉以后才加载完的快照，或者才开始的扫描，会一直留在后端、没有标签显示，直到退出 SpaceKnife（一个 860 万项的快照约 1.1 GB）；已经开始的扫描甚至会继续跑。现在会立即释放。
- **窄窗口下工具栏不再被截断**：窗口窄于约 1400 像素时，工具栏右端会被截掉，最小窗口下分屏、命令面板和删除篮按钮都点不到，路径也可能消失。现在先压缩过滤框，再把芯片缩成色块，然后依次把配色、芯片、最后是可视化切换收进新的「更多」（…）菜单，任何窗口大小下每个控件都点得到，快照对比时也一样。
- **MFT 快速扫描的内存峰值大幅降低**：大盘建树的那一刻，以前要在读到的 $MFT 表之外再占每项约 325 字节：860 万项的盘后端会冲到约 3.6 GB（整个 SpaceKnife 约 3.8 GB）。现在每项约 85 字节，同一块盘的峰值应在约 2.2 GB，结果和速度都不变。
- **扫完后文件持续变化时，内存不再每隔几秒冒高**：开着实时监控时，每批文件变化都让详情面板把整盘汇总重算一遍，还经常几份同时算，每份临时要每项约 100 字节（860 万项的盘约 0.85 GB）；扫完后的第一次文件变化还会让扫描数据的内存一直翻倍。现在汇总分小块计算，同一份只算一次，大扫描时刷新得慢一些（最长每 10 秒一次）；文件变化只多留一点余量。320 万项的盘、文件连续变化一分钟：后端峰值从约 1.3 GB 降到约 660 MB（打开「大扫描存进映射文件」约 300 MB），之后稳定在约 600 MB，原来是 835 MB。
- **存进内存映射文件的扫描，写盘量减半**：扫描数据超过「扫描数据内存上限」后，Windows 上每个映射文件都先整个写一遍零，每次变大还要复制到新文件。现在文件原地变大，只写数据本身：320 万项的盘约 0.67 GB，原来约 1.3 GB。
- **实时监控开久了，内存不再一下子冒高好几 GB**：文件改动累计多了以后，后端会把整棵树重新编号一次，这一刻每项要多占约 760 字节，870 万项的盘多出约 6.3 GB。现在只多出重新编号后那份副本本身（约 1 GB），速度不变。
- **打开快照只要一半的内存**：以前每一列都先解压成一份临时副本，870 万项的快照打开时要临时占 1.8 GB，最后只留 0.95 GB。现在直接解压到位，只要 0.95 GB。
- **后端平时占的内存更少**：
  - 少约 50 MB：命令面板的拼音搜索不再在启动时载入中文词典。
  - 打开扫描时每项少 8 字节（860 万项的盘约 66 MB）：树图和命令面板只算要显示的那些项的大小，不再为每一项建一整列、每次文件变化后再整列重算。树图在文件变化后的重画也更快。
- **硬扫描扫过的盘时，一次留好内存**：按上次完整扫描的项数一次分配，不再一边扫一边扩容、每次都把已有的数据整份复制一遍。
- **窗口隐藏时归还内存**：最小化或被完全挡住的窗口会交还绘图画布，再显示时重新画出来。鼠标在树图上移动时复用同一块缓冲，不再每次高亮都重新分配。
