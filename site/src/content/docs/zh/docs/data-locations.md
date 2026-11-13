---
title: 数据位置与便携模式
description: SpaceKnife 在各平台把快照、设置、日志和缓存存在哪里，怎样用 SPACEKNIFE_HOME 整体搬走，便携模式怎么工作。
---

扫描只读取磁盘。SpaceKnife 自己保存的东西都在几个用户目录里；便携版则全部在程序旁边的 `data` 文件夹里。

## 存在哪里

| | Windows | Linux | macOS |
|---|---|---|---|
| 数据：快照、标签、日志 | `%LOCALAPPDATA%\SpaceKnife` | `~/.local/share/SpaceKnife` | `~/Library/Application Support/SpaceKnife` |
| 配置：设置、文件类别 | `%LOCALAPPDATA%\SpaceKnife` | `~/.config/SpaceKnife` | `~/Library/Application Support/SpaceKnife` |
| 缓存 | `%LOCALAPPDATA%\SpaceKnife\Cache` | `~/.cache/SpaceKnife` | `~/Library/Caches/SpaceKnife` |
| 窗口文件（窗口的主题、本地存储与缓存） | `%LOCALAPPDATA%\io.github.duskxi.spaceknife` | `~/.local/share/io.github.duskxi.spaceknife`；Electron 版：`~/.config/SpaceKnife/electron` | `~/Library/Application Support/io.github.duskxi.spaceknife`（主题）；`~/Library/WebKit/io.github.duskxi.spaceknife`（网页视图的存储与缓存） |

数据、配置和缓存用的就是 0.3.x 的这些文件夹，所以升级后一切都还在原处。Linux 上，以及和 0.3.x 一样在 macOS 上，设置了 `XDG_DATA_HOME`、`XDG_CONFIG_HOME`、`XDG_CACHE_HOME`（绝对路径）时，这些目录跟着它们走。

窗口文件属于绘制窗口的外壳。Tauri 外壳（Windows、macOS、deb 和小巧的 Linux 便携版）的文件夹以应用的标识 `io.github.duskxi.spaceknife` 命名；系统的网页视图（WebView2、WebKitGTK、WebKit）也可能另外保存自己的缓存。Electron 外壳（rpm、AppImage 和自带引擎的 Linux 便携版）和 0.3.x 一样放在 `~/.config/SpaceKnife/electron`。0.3.4 的窗口文件在 `%APPDATA%\SpaceKnife\electron`（Windows）、`~/.config/SpaceKnife/electron`（Linux）和 `~/Library/Application Support/SpaceKnife/electron`（macOS）：Electron 版接着用这个文件夹；Tauri 版第一次启动时从那里读一次主题，之后不再碰它。

不用自己去找，「设置 › 高级」里都有：

- 「文件与诊断」：「设置文件」和「日志文件夹」，各带一个「打开」。
- 「扫描助手与缓存」：「快照」（数量、总大小和文件夹，带「打开」）和「缓存」（大小和文件夹，带「清除缓存…」；正在使用的缓存会保留）。
- 设置列表底部的「打开设置文件」，以及启动页「最近」的「…」菜单里的「打开快照文件夹」。

## 里面有什么

| 文件或文件夹 | 位于 | 内容 |
|---|---|---|
| `snapshots/` | 数据 | 每个扫描位置一个文件夹，每份快照一个 `.sks` 文件 |
| `tags/` | 数据 | 保存的标签，每个扫描位置一个文件 |
| `logs/spaceknife.log` | 数据 | 日志文件，以及较早的 `spaceknife.log.1` 到 `.5` |
| `update.json` | 数据 | 上一次更新检查、最近的版本和它们的发布说明，以及关掉过更新卡片的版本 |
| `updates/` | 数据 | 下载好的新版本（直到它运行起来）；便携版替换时，还有新旧两份程序文件。下次启动时清空（[不留残余](../updates-privacy/#不留残余)） |
| `palette_recent.json` | 数据 | 命令面板最近的路径跳转和过滤 |
| `volume_summaries.json` | 数据 | 「本机磁盘」保留的每个卷上一次完整扫描的结果（文件类别、最大的文件夹） |
| `settings.json` | 配置 | 你的设置 |
| `classes.json` | 配置 | 你的文件类别 |
| `class_folders.json` | 配置 | 你归入文件类别的文件夹 |
| `columns/` | 缓存 | 超大扫描时移出内存的扫描数据 |

SpaceKnife 自己的这些文件夹和它的安装目录都是受保护的：不能在 SpaceKnife 里删除或移动它们。

### 快照

自动保存的快照是 `snapshots/<位置>-<编号>/<日期>-<时间>.sks`，例如 `snapshots/Videos-3f2a9c41d0be/20261020-081530.sks`。日期和时间是 UTC。每个位置的文件夹名和 0.3.x 的相同，所以升级后它的标签和快照仍在一起。

0.3.4 及更早版本存的快照照样能打开、对比；0.3.5 起存的是更新的格式，0.3.4 及更早的版本打不开。下面的清理从不删除 SpaceKnife 读不了的快照文件（损坏的，或者更新版本存的）。

每个位置保留最近 10 份（「设置 › 高级 › 保留最近快照数」，1–100），更早的每周保留最新的一份；其余的在每次自动保存后删除。

:::tip[永久保留某一份快照]
文件名不是日期时间的文件，清理时从不删除：在快照所在的文件夹里把它改个名（比如 `清理前.sks`）就能留住。改名后的文件仍然占「最近 10 份」中的一个名额。也可以把它复制到别处，以后用「打开快照文件…」打开。
:::

### 设置

`settings.json` 里是你的设置。SpaceKnife 运行时也可以直接手改：它每秒检查一次这个文件，改动随即生效。文件不合法时，设置页顶部会提示，并继续使用上一次的有效设置。要把设置搬到另一台电脑，用「设置 › 高级 › 导出设置」和「导入设置」：导入时先列出会改变的项，确认后才生效。见[设置](../settings/)。

### 日志

`logs/spaceknife.log` 写到 10 MB 后换一个新文件，旧文件保留最近 5 个：`spaceknife.log.1`（最新的）到 `spaceknife.log.5`。0.3.x 留下的旧日志（`spaceknife.<时间>.log`）在第一次启动 0.3.5 或更新的版本时删掉。「设置 › 高级 › 日志文件级别」可选「信息」或「调试」。日志面板（<kbd>Ctrl</kbd>+<kbd>J</kbd>）显示本次会话的事件。

报告问题时，「设置 › 高级 › 复制诊断信息」把版本、平台、能力清单以及设置文件和日志文件夹的路径复制到剪贴板。它不会发送任何东西。

### 缓存

打开「设置 › 高级 › 大扫描存进映射文件」（默认关）时约 200 万项以上的扫描，以及扫描数据超过「设置 › 高级 › 扫描数据内存上限」（默认是物理内存的 25%）之后的部分，存进缓存里的内存映射文件，不占私有内存。这些文件不再需要时会自己删除，崩溃后残留的会在下次启动时清掉。「清除缓存…」删除没有在用的部分。

## SPACEKNIFE_HOME

把环境变量 `SPACEKNIFE_HOME` 设成一个文件夹，SpaceKnife 就把所有东西都存在那里，不再用用户目录：

| 文件夹 | 内容 |
|---|---|
| `SPACEKNIFE_HOME/data` | 快照、标签、日志、更新状态 |
| `SPACEKNIFE_HOME/config` | 设置、文件类别 |
| `SPACEKNIFE_HOME/cache` | 缓存 |
| `SPACEKNIFE_HOME/desktop` | 窗口文件（Tauri 版；macOS 上网页视图的存储与缓存仍在 `~/Library/WebKit/io.github.duskxi.spaceknife`） |
| `SPACEKNIFE_HOME/electron` | 窗口文件（Electron 版） |

适合另开一套配置（试设置而不碰自己的那套）、把快照放到另一块盘上，或者做演示和测试。请用绝对路径。

Windows，在 PowerShell 里只对这一次启动生效：

```powershell
$env:SPACEKNIFE_HOME = "D:\SpaceKnife-data"
& "<安装目录>\SpaceKnife.exe"
```

Windows，对之后启动的所有程序生效（要撤销，在 Windows 的环境变量设置里删掉它）：

```powershell
setx SPACEKNIFE_HOME "D:\SpaceKnife-data"
```

Linux：

```bash
SPACEKNIFE_HOME=/home/lenny/spaceknife-data spaceknife
```

macOS：

```bash
SPACEKNIFE_HOME=/Users/lenny/spaceknife-data /Applications/SpaceKnife.app/Contents/MacOS/SpaceKnife
```

[命令行](../command-line/)也认这个变量：设了 `SPACEKNIFE_HOME` 后，`spaceknife-cli snapshot list` 列出的就是那个文件夹里的快照。

## 便携模式

便携包（`SpaceKnife-<版本>-win-<架构>-portable.zip`、`SpaceKnife-<版本>-mac-<架构>-portable.zip`，Linux 上是 `SpaceKnife-<版本>-linux-<架构>-tauri.tar.gz` 和 `SpaceKnife-<版本>-linux-<架构>-portable.tar.gz`）在程序旁边带一个空的 `data` 文件夹，里面只有一份简短说明。SpaceKnife 每次启动都会找这个文件夹：可执行文件旁边（macOS 上是 `SpaceKnife.app` 旁边）有 `data` 文件夹，就把它当作 `SPACEKNIFE_HOME`。快照、设置、日志、缓存和窗口文件都存在里面（`data/data`、`data/config`、`data/cache`，以及 `data/desktop` 或 `data/electron`），不写进你的用户目录。唯一的例外是 macOS：WKWebView 不接受指定的文件夹，网页视图自己的数据（页面存储与缓存）总在 `~/Library/WebKit/io.github.duskxi.spaceknife`，便携版和设了 `SPACEKNIFE_HOME` 时也一样。

- 手动设置的 `SPACEKNIFE_HOME` 优先于 `data` 文件夹。
- 删掉或重命名 `data`，这份程序就改用正常的用户目录；放回去又是便携的。
- 解压到自己能写入的位置：比如 Program Files 下面就不行。
- 软件内的更新知道这是便携版：它下载便携包，「无缝更新」替换程序文件、留下 `data`；「自己替换」把压缩包显示出来，由你解压覆盖（见[升级](../install/#升级)）。
- macOS 上，解压后直接打开的应用可能被系统放到一个隐藏的只读副本里运行（App 转移）。SpaceKnife 会找到原来的文件夹，数据照样存在那里的 `data` 里。
- 从 dmg 装进「应用程序」的应用（旁边没有 `data` 文件夹）和 AppImage 没有便携模式。
- 从 0.3.4 更新上来的副本里，可能还有 0.3.4 的窗口用过的 `data/electron` 文件夹。Tauri 版从里面读一次主题，之后不再碰它；可以删掉。

:::caution[命令行不认便携模式]
只有应用和无头模式会找 `data` 文件夹，便携版里的 `scan`、`run`、`snapshot` 不会。要在终端里操作便携版的快照，把 `SPACEKNIFE_HOME` 指向它的 `data` 文件夹，见下面的例子。
:::

```powershell
$env:SPACEKNIFE_HOME = "C:\Users\lenny\Apps\SpaceKnife-portable\data"
& "C:\Users\lenny\Apps\SpaceKnife-portable\spaceknife-cli.exe" snapshot list
```

### 搬移数据

要把设置带进便携版（或从便携版带出来），在一边导出、另一边导入（「设置 › 高级 › 导出设置」/「导入设置」）。快照就是普通文件：复制需要的 `.sks` 文件，用「打开快照文件…」打开；或者退出 SpaceKnife，把整个 `snapshots` 文件夹复制到另一份的数据文件夹里（便携版里是 `data/data/snapshots`）。

## 删除全部数据

想从头开始，或卸载后清理干净：退出 SpaceKnife，删除[第一张表](#存在哪里)里的文件夹，或者你用的 `SPACEKNIFE_HOME`、`data` 文件夹。Windows 上是 `%LOCALAPPDATA%\SpaceKnife` 和 `%LOCALAPPDATA%\io.github.duskxi.spaceknife`，装过 0.3.x 的还有 `%APPDATA%\SpaceKnife`；卸载时勾上「同时删除快照、设置和日志」，Windows 的卸载程序会把它们都删掉（[卸载](../install/#卸载)）。macOS 上还要删 `~/Library/WebKit/io.github.duskxi.spaceknife`。「设置 › 高级 › 全部恢复默认」只重置设置和文件类别。
