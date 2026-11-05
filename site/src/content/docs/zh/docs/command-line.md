---
title: 命令行
description: 在终端里用后端的命令行扫描、批量执行与导出，以及改变 SpaceKnife 行为的环境变量。
---

替应用做扫描的后端有自己的命令行。可以不开窗口直接扫描，在脚本或计划任务里批量扫描和导出，也可以查看已保存的快照。命令行保存的快照和应用用的是同一个文件夹，所以它们会出现在启动页的「最近」里，也能拿来对比。

## 程序在哪里

安装包不会把命令行加进 PATH。它是应用 `resources/backend` 文件夹里的 `spaceknife-backend`：

| 安装方式 | 命令行 |
|---|---|
| Windows 安装包，仅当前用户 | `%LOCALAPPDATA%\Programs\SpaceKnife\resources\backend\spaceknife-backend.exe`（默认安装目录） |
| Windows 安装包，所有用户 | `C:\Program Files\SpaceKnife\resources\backend\spaceknife-backend.exe`（默认安装目录） |
| Windows 便携版 | `SpaceKnife.exe` 旁边的 `resources\backend\spaceknife-backend.exe` |
| deb / rpm | `/opt/SpaceKnife/resources/backend/spaceknife-backend` |
| Linux 便携版 | `spaceknife` 旁边的 `resources/backend/spaceknife-backend` |
| AppImage | 在镜像里面：`./SpaceKnife-<版本>-x86_64.AppImage --appimage-extract` 解到 `squashfs-root/`，再用 `squashfs-root/resources/backend/spaceknife-backend` |
| macOS | `/Applications/SpaceKnife.app/Contents/Resources/backend/spaceknife-backend` |
| 从源码运行 | 仓库里 `uv run spaceknife` |

本页的例子都写作 `spaceknife-backend`。可以把所在文件夹加进 PATH，或者设个别名：

```bash
alias skb=/opt/SpaceKnife/resources/backend/spaceknife-backend
```

:::note
deb / rpm 安装后，`spaceknife`（`/usr/bin/spaceknife`）启动的是应用，不是命令行。

应用自己的可执行文件也有选项：`--headless` 和 `--listen` 让 SpaceKnife 不开窗口运行，把界面提供给浏览器，本机或别的设备都能用。见[无头模式](../headless/)。
:::

:::caution[便携版]
便携版把数据存在旁边的 `data` 文件夹里，但只有应用知道这件事。要让命令行使用同一份快照和设置，把 `SPACEKNIFE_HOME` 设为这个 `data` 文件夹（见[环境变量](#环境变量)）。
:::

输出的语言跟随「设置 › 常规 › 界面语言」（命令行会读取设置文件）；`--help` 的说明是英文。

## 命令一览

| 命令 | 作用 |
|---|---|
| `scan <路径>…` | 扫描文件夹或磁盘，列出空间去了哪里；保存快照 |
| `run <命令>…` | 不开窗口，按顺序执行 `scan`、`load`、`filter`、`save`、`export` |
| `snapshot list` | 列出已保存的快照，最新的在前 |
| `snapshot show <文件>` | 不重新扫描，查看快照里空间的分布 |
| `version` | 显示后端、协议和原生扩展的版本 |
| `serve` | 运行应用连接的后端服务 |
| `schema` | 导出协议的 JSON Schema（开发用） |

`spaceknife-backend <命令> --help` 列出该命令的全部选项。

## scan

```bash
spaceknife-backend scan "D:\Photos"
spaceknife-backend scan /home/lenny --top 20 --depth 3
```

扫描和应用里一样并行进行，过程中显示一行实时进度。结束后输出：

- 「扫描结果」：状态、用时、速度，项目、文件和文件夹数，占用与逻辑大小。扫描整个卷时还有可用空间、未知空间，以及有多少文件夹无法访问（计入未知空间）。
- 每个根的「…里最大的项目」：展开 `--depth` 层，每层 `--top` 行，其余的合成一行「…其余 N 项」。
- 「最大的文件」：占用、修改时间和完整路径。

| 选项 | 默认 | 作用 |
|---|---|---|
| `--top N` | 15 | 摘要每层的行数，也是「最大的文件」的个数 |
| `--depth N` | 2 | 摘要展开的层数 |
| `--disk-limit 档位` | 不限制 | 和应用一样限制磁盘负载：`max`、`balanced`（70%）、`quiet`（35%）、`idle`，或 1–100 的百分比 |
| `--workers N` | 与应用相同 | 列目录线程数（没有原生扩展时是工作进程数）。`0` 表示在本进程内用 Python 遍历器扫描：单线程，不限速 |
| `--cross-mounts` / `--no-cross-mounts` | 关 | 进入挂载在扫描根下面的其他文件系统 |
| `--save 文件` | — | 另外把快照保存到这个文件 |
| `--no-snapshot` | — | 不自动保存快照 |
| `--profile` | — | 另外输出「耗时分解」（见下文） |
| `--mft` | 关 | Windows：读取卷的 $MFT（和应用里一样的 MFT 快速扫描），不再逐个文件夹列目录。只能扫一个完整的本地 NTFS 卷，例如 `C:\`；会请求一次管理员权限 |

要点：

- 一条命令里写多个路径，就是一次有多个根的扫描，和在启动页同时选几个位置一样。
- 扫描完成后保存到应用的快照文件夹。命令行在那里为每个位置保留最近 10 个快照，更早的每周留一个。
- 不加 `--mft` 时命令行总是逐目录硬扫描。加了 `--mft`，MFT 快速扫描读不了的路径（只是一个文件夹而不是整个卷；ReFS、FAT、exFAT、Btrfs 卷；网络盘；一次写了多个路径；不是 Windows）照样逐目录硬扫描，开始前先输出「不支持 MFT 快速扫描：……」并写明原因；拒绝了管理员授权时也一样改用硬扫描并说明原因。这时「扫描结果」里写「方式 硬扫描（……）」；读了 $MFT 的写「方式 MFT 快速扫描」，`--profile` 改为列出它的几个阶段（启动只读助手、读取并解析 $MFT、建树），而不是列目录线程。
- `--disk-limit idle` 也会使用系统的低 I/O 优先级，和应用里的「空闲」档一样。限速的原理见[扫描速度与磁盘负载](../performance/)。
- 退出码：`0` 扫描已执行；`2` 无法开始扫描（例如路径不存在）；`130` 按了 <kbd>Ctrl</kbd>+<kbd>C</kbd>。

### 用法示例

一边工作一边温和地扫描：

```bash
spaceknife-backend scan "D:\" --disk-limit quiet
```

为快照对比攒历史记录：用任务计划程序、cron 或 launchd 每晚扫描一次。应用会在「最近」里列出这些快照，任意两个都可以对比（见[快照与对比](../snapshots-compare/)）。

```bash
spaceknife-backend scan /home/lenny --disk-limit idle
```

读 $MFT 扫整个系统盘，比逐个文件夹快得多（Windows，需要管理员权限）：

```bash
spaceknife-backend scan "C:\" --mft
```

只测速度，不留下快照：

```bash
spaceknife-backend scan "C:\Program Files" --no-snapshot --profile
```

在 Linux 上，你的账户读不了的文件夹会计入未知空间。要看进去，用 `sudo` 运行命令行，并加上 `--no-snapshot`，免得把快照存进 root 的目录：

```bash
sudo /opt/SpaceKnife/resources/backend/spaceknife-backend scan / --no-snapshot
```

### 耗时分解

`--profile` 多输出一块面板，说明扫描被什么拖住了：

| 行 | 含义 |
|---|---|
| 「原生遍历器 · 列目录并发 16」 | 用哪种遍历器列目录（没有原生扩展时是「Python 遍历器」），允许多少个列目录线程 |
| 「列目录线程平均忙碌 …」 | 列目录线程花在系统调用上的时间占比。接近 100% 说明瓶颈在磁盘或文件系统 |
| 「主线程合并结果 …」 | 把结果并进目录树所花的时间占比。接近 100% 说明瓶颈在合并，加线程没用 |
| 「共 … 批」 | 一共列了多少批目录 |
| 「等磁盘负载上限放行 …」 | 只在用了 `--disk-limit` 时出现：等限速放行下一批的时间 |
| 「批量列目录（getattrlistbulk）：…」 | 只在 macOS 上出现：是否批量取文件属性（`on`、`off: 原因` 或 `untested`） |

应用在每次硬扫描后把同样的数字写进日志文件，怎么解读见[扫描速度与磁盘负载](../performance/#日志里的耗时分解)。

## run：批量执行

```text
spaceknife-backend run <命令> [<命令> …]
```

`run` 按顺序执行一串命令，不打开窗口。`scan` 和 `load` 各打开一个**视图**；`filter`、`save`、`export` 作用于最后打开的那个视图。整串命令先检查一遍：有未知命令、缺参数，或者 `filter` / `save` / `export` 前面没有 `scan` / `load`，就一条也不执行。

| 命令 | 作用 |
|---|---|
| `help` | 显示内置帮助（`run` 后面什么都不写也一样） |
| `scan "<路径>"` | 扫描文件夹或磁盘，打开一个视图。多个路径用 `;` 分隔，属于同一个视图（多个扫描根）。路径本身就是一个存在的文件夹时不拆分，所以文件夹名里可以有 `;` |
| `load "<快照>"` | 把 SpaceKnife 快照（`.sks`）打开为一个视图；一个 `load` 只打开一个快照 |
| `filter "<表达式>"` | 过滤视图，语法和过滤框相同（见[过滤](../filtering/)）。有错误时列出每处错误的位置并停止；只有警告时显示警告并继续 |
| `save "<文件>"` | 把视图的完整结果保存为快照。不受过滤影响，保存的是整棵树 |
| `export "<模板>" "<文件>"` | 把视图导出到文件，只含过滤后留下的内容。`<模板>` 可以是内置模板名、`csv`、`json`、`html`，或模板文件的路径 |
| `autoclose` | 结束时不显示视图摘要 |

内置模板（名称不区分大小写；括号里是界面上的显示名）：

| 名称 | 内容 |
|---|---|
| `Grouped by folder`（按文件夹分组） | 每个直接含有文件的文件夹一段：完整路径和占用，下面列出其中的文件 |
| `First level folders`（第一层文件夹） | 根下第一层的每个文件夹：完整路径和占用 |
| `Simple file list`（文件清单） | 每个文件的完整路径，一行一个 |
| `Summary`（汇总） | 总占用、文件夹数和文件数 |
| `Folder tree`（文件夹树） | 按层级缩进的文件夹和占用 |

执行时：

- `run` 里的扫描最多用 4 个列目录线程，不限速，也不自动保存快照；要保存就写 `save`。
- 进度和消息输出到标准错误。没有 `autoclose` 时，结束后在标准输出列出每个视图的摘要：根、最大的 10 项及占比、合计。
- `export` 一次写成（先写临时文件再改名），读的一方不会看到写了一半的文件；目标文件夹必须已经存在。模板文件必须是 UTF-8、不超过 1 MB；模板有错误时不导出，逐条列出错误所在的行和列。导出格式和模板语言见[导出与报告](../export/)。
- 退出码：`0` 全部执行完；`2` 命令行有误或某条命令失败（停在那条命令）；`130` 被中断。

### 例子

下面的例子分别按 Windows 的命令提示符和 Linux / macOS 的 POSIX shell（bash、zsh）书写。

```bat
spaceknife-backend run scan "D:\Photos" filter "*.jpg;>5mb" export "Simple file list" big.txt autoclose
spaceknife-backend run scan "C:\" save "D:\snapshots\c.sks" autoclose
spaceknife-backend run load "D:\snapshots\c.sks" filter "\node_modules" export html node_modules.html
spaceknife-backend run scan "C:\;D:\" export Summary totals.txt autoclose
```

```bash
spaceknife-backend run scan /home/lenny filter '>1gb' export csv big-files.csv autoclose
spaceknife-backend run load ~/c.sks filter '\node_modules' export html node_modules.html
```

Windows 的第一个例子找出 `D:\Photos` 里大于 5 MB 的 JPEG，把路径写进 `big.txt`；第二个把整个 C: 盘扫成一个快照，之后可以在应用里打开或对比；第三个不重新扫描，直接打开这个快照，把所有 `node_modules` 文件夹导出成 HTML 报告。

### 参数与分隔符

参数之间用空白分隔。以开分隔符（默认是 `"`）开头的参数一直到闭分隔符为止，**没有转义字符**。所以 `"C:\"` 就是 `C:\`，闭引号前的反斜杠不是转义。为此 SpaceKnife 在 Windows 上直接读取原始命令行；在 Linux、macOS 上参数已经由 shell 拆分、去掉了 shell 的引号，同样的规则作用在 shell 传进来的参数上。

两个元命令改变后面内容的读法，每个都必须是一个不带分隔符的词：

| 元命令 | 作用 |
|---|---|
| `@delim=X` | 之后的参数改用 `X` 作分隔符，不再用 `"` |
| `@delim=XY` | 之后的参数以 `X` 开头、以 `Y` 结尾，例如 `@delim={}` |
| `@parser=OS` | 从头重新解析整行，直接使用系统拆分好的参数；之后不再接受 `@delim=` |

参数本身含有 `"` 时就需要换分隔符，例如按字面匹配名为 `a;b.jpg` 的文件：

```bat
spaceknife-backend run @delim={} scan {D:\Photos} filter {"a;b.jpg";>1mb} export {Grouped by folder} {D:\out\files.txt}
```

```bash
spaceknife-backend run @delim={} scan /home/lenny/Photos filter '{"a;b.jpg";>1mb}' export 'Grouped by folder' files.txt
```

在 POSIX shell 里给过滤表达式加单引号，shell 才不会处理其中的 `;`、`>` 和 `\`。不写 `@delim={}` 的话，参数 `"a;b.jpg";>1mb` 以默认分隔符开头，到第二个 `"` 就结束了，剩下的部分会被当成未知命令。

:::caution[PowerShell]
`{ }` 和 `@` 在 PowerShell 里另有含义。用到 `@delim=` 的命令行请在命令提示符里运行。
:::

## snapshot

```bash
spaceknife-backend snapshot list
spaceknife-backend snapshot show "D:\snapshots\c.sks" --top 10 --depth 3
```

`snapshot list` 列出快照文件夹里的所有快照：时间、扫描对象、占用、项目数和文件名，最后是快照文件夹本身的位置。`snapshot show` 显示快照的保存时间和其中最大的项目；`--top`、`--depth` 与 `scan` 相同。

## version

```bash
spaceknife-backend version
```

输出后端版本、协议版本、协议 Schema 的哈希和原生扩展的版本。显示 `native none` 表示原生扩展没有加载：扫描会改用 Python 遍历器，慢好几倍。报告问题时请附上这一行。

## serve

应用自己会用 `serve` 启动后端；只有接入自己的客户端或调试时才需要手动运行。开始监听后，它在标准输出打印一行 JSON（`{"event": "ready", "url": "ws://127.0.0.1:…/v1", …}`），日志输出到标准错误。

| 选项 | 默认 | 作用 |
|---|---|---|
| `--host` | `127.0.0.1` | 监听的网络接口。除非确实需要，保持回环地址 |
| `--port` | `0` | TCP 端口；`0` 自动选一个空闲端口，写在就绪行里 |
| `--token-stdin` | 关 | 从标准输入的第一行读取会话令牌，标准输入关闭时退出。不加时自动生成令牌，写在就绪行里 |
| `--allow-origin` | — | 额外允许的 WebSocket Origin，例如开发服务器的地址；可以写多次 |
| `--hello-timeout` | 5 | 客户端必须在这么多秒内完成握手 |
| `--log-level` | `INFO` | `TRACE`、`DEBUG`、`INFO`、`WARNING` 或 `ERROR` |
| `--log-format` | `rich` | 标准错误上的日志格式：`rich`、`plain` 或 `json` |
| `--ready-file` | — | 另外把就绪信息（含令牌）写进这个文件（只有你能读；退出时删除） |

消息格式见[协议文档](https://github.com/DuskXi/SpaceKnife/blob/main/docs/protocol.md)。

## schema

`schema --output 文件` 导出协议的 JSON Schema（不加 `--output` 时输出到标准输出）。`schema --check 文件` 在文件与当前 Schema 不一致时以 `1` 退出，项目的 CI 用它做检查。

## 环境变量

在启动应用或命令行之前设置。应用会把自己的环境变量传给后端。

| 变量 | 作用 |
|---|---|
| `SPACEKNIFE_HOME` | 把 SpaceKnife 写的所有东西放进一个文件夹：`data`（快照、标签、日志）、`config`（设置、文件类别）、`cache`，应用还有 `electron`（界面的浏览器数据）。适合另开一套互不影响的环境、做测试，以及便携版。见[数据位置与便携模式](../data-locations/) |
| `SPACEKNIFE_SCAN_THREADS` | 列目录线程数，1–64。默认每个 CPU 一个，在 2–16 之间；macOS 上是 CPU 数的两倍，在 4–16 之间。手动并发数也不会超过它 |
| `SPACEKNIFE_LANGUAGE` | `en` 或 `zh-CN`：命令行的输出固定用这种语言，不看设置。适合脚本和问题报告 |
| `SPACEKNIFE_NO_NATIVE=1` | 不使用原生扩展：改用 Python 遍历器列目录，慢好几倍。用来对比速度，或排查问题时排除原生代码 |
| `SPACEKNIFE_NO_BULK=1` | macOS：列文件改用 `readdir` 加 `fstatat`，不用 `getattrlistbulk`，用来对比速度 |
| `SPACEKNIFE_MEMTRACE=1` | 内存看门狗报警时，日志文件里还会列出占用内存最多的代码行。会让后端变慢，只在排查内存警告时使用 |
