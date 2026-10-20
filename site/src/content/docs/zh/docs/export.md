---
title: 导出与报告
description: 把整次扫描或过滤匹配的部分导出为文本模板、CSV、JSON 或自包含的 HTML 报告，以及怎样编写自己的模板。
---

导出把看到的内容写进文件：整次扫描或当前位置，全部内容或只有过滤匹配的部分。内置 5 个文本模板和 3 种数据格式，还可以自己写模板，在命令行里使用。

## 在软件里导出

1. 按 <kbd>Ctrl</kbd>+<kbd>K</kbd> 打开[命令面板](../command-palette/)，执行「导出报告…」。输入为空时它就在列表里；输入 `export`、`csv`、`json`、`html`、`report` 或拼音（如 `dcbg`）也能找到。
2. 在「导出报告」对话框里选模板或格式。列表分两组：「模板（与 SpaceSniffer 相同的模板语言）」和「数据格式」。
3. 选好范围和过滤选项（见下表），点「导出…」，选择保存位置。

写完后会提示文件大小和名称（例如「已导出 2.4 MB：report.html」），并在文件管理器里显示这个文件。

| 选项 | 何时出现 | 作用 |
|---|---|---|
| 当前位置 / 整个扫描 | 已经放大进入某个文件夹时 | 只导出放大进入的文件夹，或导出整次扫描 |
| 只导出过滤匹配的项 | 过滤生效时（默认勾选） | 不导出过滤不匹配的内容 |

:::note
对比视图还不能导出，请先退出对比。
:::

### 过滤对导出的影响

带过滤导出时，文件里只有匹配的文件和包含它们的文件夹，导出的根始终保留。文件夹的大小和计数只算导出的内容，所以数字与过滤后的视图一致。见[过滤](../filtering/)。

## 内置模板

| 对话框里的名称 | 命令行里的名称 | 内容 | 建议文件名 |
|---|---|---|---|
| 按文件夹分组 | `Grouped by folder` | 每个直接含有文件的文件夹一段：完整路径和占用，下面列出其中的文件（占用右对齐） | `File report.txt` |
| 第一层文件夹 | `First level folders` | 根下第一层的每个文件夹：完整路径和占用 | `First level.txt` |
| 文件清单 | `Simple file list` | 每个文件的完整路径，一行一个 | `File list.txt` |
| 汇总 | `Summary` | 总占用、文件夹数和文件数 | `Summary.txt` |
| 文件夹树 | `Folder tree` | 按层级缩进的文件夹和占用 | `Folder tree.txt` |

前四个与 SpaceSniffer 内置模板的文本相同。命令行里的模板名不区分大小写。「按文件夹分组」的输出像这样：

```text

/home/lenny/Videos [12.4 GB]
  [  6.1 GB] trip-2024.mp4
  [  4.0 GB] wedding.mov

/home/lenny/Videos/raw [2.3 GB]
  [  2.3 GB] clip-001.mts
```

## 数据格式

三种格式按同样的方式遍历：深度优先，大的在前，文件和文件夹不分先后；导出的根排在最前面。

### CSV 表格

每个文件和文件夹一行。UTF-8 编码、带 BOM、`\r\n` 行尾，表格软件打开时编码不会错。

| 列 | 内容 |
|---|---|
| `path` | 完整路径 |
| `name` | 名称 |
| `type` | `file`、`folder`、`link` 或 `other` |
| `depth` | 在导出根下的层级（根为 0） |
| `size` | 逻辑大小，字节（文件夹为其中文件的合计） |
| `disk_size` | 占用，字节（文件夹为其中文件的合计） |
| `files`、`folders` | 文件夹下的文件数、文件夹数（文件这两列为空） |
| `modified`、`created`、`accessed` | ISO 8601 本地时间，带时区偏移，如 `2024-03-05T14:07:09+08:00`；未知时为空 |
| `attributes` | 空格分隔，取自 `readonly hidden system archive compressed encrypted sparse temporary offline notindexed reparse cloud hardlink denied error mount pending` |

:::caution[表格软件与公式]
表格软件可能把以 `=`、`+`、`-`、`@` 开头的名字当成公式。SpaceKnife 原样写出名字；在意的话，用表格软件的「导入文本」方式打开。
:::

### JSON 数据

用 `*.jpg` 过滤后导出 `/home/lenny`：

```json
{"format": "spaceknife-export", "version": 1, "generator": "SpaceKnife …", "generated_at": "2026-10-18T12:30:00+08:00", "filter": "*.jpg",
"roots": [
{"path": "/home/lenny", "name": "lenny", "type": "folder", "size": 4000, "disk_size": 8192, "files": 2, "folders": 1, "modified": "2026-10-01T09:12:44+08:00", "created": null, "accessed": "2026-10-18T08:02:10+08:00", "attributes": [], "children": [
  {"name": "Photos", "type": "folder", "size": 4000, "disk_size": 8192, "files": 2, "folders": 0, "modified": "2026-09-30T21:40:02+08:00", "created": null, "accessed": "2026-10-17T19:55:31+08:00", "attributes": [], "children": [
    {"name": "a.jpg", "type": "file", "size": 1000, "disk_size": 4096, "modified": "2024-03-05T14:07:09+08:00", "created": null, "accessed": "2026-10-17T19:55:31+08:00", "attributes": []},
    {"name": "b.jpg", "type": "file", "size": 3000, "disk_size": 4096, "modified": "2024-03-05T14:09:51+08:00", "created": null, "accessed": "2026-10-17T19:55:31+08:00", "attributes": []}]}]}
]}
```

- 每个元素一行，按层级缩进。只有根带 `path`；文件夹带 `files`、`folders`、`children`。
- 字段含义与 CSV 的列相同。未知的时间为 `null`；应用了过滤时，`filter` 是过滤表达式。
- 键名在任何界面语言下都是英文，方便工具处理。

### HTML 报告

单个自包含文件：样式内联，没有脚本、外部字体或图片；跟随系统的深浅色，打印时为浅色。内容包括：

- 路径、生成时间，以及过滤表达式（如果有）；
- 占用空间、逻辑大小、文件数（带过滤时为匹配过滤的文件数）和文件夹数；
- 文件夹结构，可以折叠：默认 4 层，每层最大的 30 项，其余合并成一行；
- 最大的 50 个文件；
- 占用最多的 20 种扩展名。

报告的大小不随扫描规模增长，打开和分享都轻便。报告里的文字跟随界面语言。

### 通用规则

- 所有导出都是 UTF-8。先写到临时文件，写完再改名，导出失败不会留下半个文件。
- 目标文件夹必须已经存在。
- 有多个扫描根时，每个根依次导出到同一个文件里。
- 文件的多余硬链接按大小 0 计算，与图上一致。
- 不是合法 Unicode 的名字写成 `\udcXX` 转义；在 JSON 里这正是合法的字符串转义。

## 编写自己的模板

模板是一个文本文件，扩展名一般用 `.sktpl`。软件的导出对话框提供内置模板；自己写的模板在[命令行](#命令行导出)里使用。

### 模板文件

```text
@name=文件大小
@file=sizes.txt
@blocks=files
@sort=size
@order=desc
# 以 # 开头的是注释行
@header
<%pathfile%> 下的文件{&br}
@detail
{if{<%isfile%>}=={true}?{<%disksize%>  <%pathfile%>{&br}}}
@footer
合计：<%disksize%>{&br}
```

对一个小文件夹的输出：

```text
/home/lenny 下的文件
4.0 KB  /home/lenny/notes.txt
4.0 KB  /home/lenny/Photos/a.jpg
4.0 KB  /home/lenny/Photos/b.jpg
8.0 KB  /home/lenny/Photos/raw/c.nef
合计：20.0 KB
```

| 行 | 含义 | 默认 |
|---|---|---|
| `@name=` | 显示名 | 文件名 |
| `@file=` | 建议的输出文件名 | |
| `@blocks=files`、`folders` 或 `mixed` | 同一文件夹里文件在前、文件夹在前，或不区分 | `mixed` |
| `@sort=size`、`name` 或 `none` | 按占用、按名称（不区分大小写）排序，或保持列出顺序 | `size` |
| `@order=desc` 或 `asc` | 大的（或 Z）在前，或者相反 | `desc` |
| `@header` | 开头段：在导出根上求值一次 | 空 |
| `@detail` | 明细段：每个元素求值一次 | 空 |
| `@footer` | 结尾段：在导出根上求值一次 | 空 |

- 第一个字符是 `@` 的才是模板文件；否则整段文本就是明细段，排序用默认值。所以 SpaceSniffer 的明细文本可以原样使用。
- 各段顺序任意。一段从它的标记行开始，到下一个标记行或文件末尾为止。
- 每段最后一个换行不属于内容，所以一行的明细不会在每个元素后多出一个空行。需要换行时，在段末加一个空行或写 `{&br}`。
- 内容本身要写成 `@footer` 这样的一行时，前面加一个空组：`{}@footer`。
- 文件按 UTF-8 读取，可以带 BOM；换行统一按 `\n` 处理。不认识的选项和无法识别的行给出警告并忽略。

### 元素与顺序

元素是导出根下的每个文件和文件夹，再加上导出根本身（层级 0）。遍历是深度优先：先输出文件夹，再输出它的内容；每个文件夹的内容按 `@blocks`、`@sort`、`@order` 排序。开头段和结尾段在导出根上求值，所以那里的 `<%disksize%>` 就是总量。

### 语言

一段模板由三种成分组成，从左到右依次输出：

| 成分 | 写法 | 结果 |
|---|---|---|
| 文字 | 除 `{`、`}`、`<%` 以外的字符 | 原样输出，包括空格和换行 |
| 标签 | `<%名称%>` | 当前元素的一项属性；名称不区分大小写 |
| 组 | `{…}` | 以函数名开头时是函数调用，否则只输出其中的内容 |

- 花括号必须配对。要输出 `{`、`}`，写 `{&curlyop}`、`{&curlycl}`；要输出 `<%`，写 `{&angleop}%`。
- 以 `if`、`leftpad`、`rightpad`、`nest`、`html`、`csv`、`json` 开头（后面紧跟空白、`{` 或 `}`）的组是函数调用。要输出以这些词开头的文字，前面加一个空组：`{{}if you like}`。
- 函数的参数是紧随其后的若干个组，组之间可以有空白。参数里可以再调用函数，所以 `if` 可以嵌套。
- **脚本模式**：一段里出现 `{script}` 时，先删去这一段的所有换行和制表符再解析，这样可以把嵌套的 `if` 分行缩进来写。空格不会删除：最外层不要用空格缩进，否则每个元素都会输出这些空格。换行用 `{&br}` 显式写出。

### 标签

| 标签 | 文件 | 文件夹 |
|---|---|---|
| `file` | 名称（含扩展名） | 名称；扫描根取最后一级，盘符根或文件系统根取完整路径 |
| `filename` | 去掉扩展名的名称（`a.tar.gz` 得到 `a.tar`） | 名称 |
| `fileext` | 扩展名，不带点，保留原大小写；没有则为空 | 空 |
| `path` | 所在文件夹的完整路径 | 同左；导出根取它的上一级 |
| `pathfile` | 完整路径 | 完整路径 |
| `size`、`sizebytes` | 逻辑大小：格式化 / 字节数 | 其中文件的合计 |
| `disksize`、`disksizebytes` | 占用：格式化 / 字节数 | 其中文件的合计 |
| `filemodifydate`、`filemodifytime`、`filemodifyage` | 修改时间：日期 `2024-03-05`、时刻 `14:07:09`（本地时间）、距今整天数 | 文件夹自己的时间 |
| `filecreatedate`、`filecreatetime`、`filecreateage` | 创建时间，同上 | 同左 |
| `fileaccessdate`、`fileaccesstime`、`fileaccessage` | 访问时间，同上 | 同左 |
| `isfile`、`isfolder`、`iscontainer` | `true`、`false`、`false` | `false`、`true`、`true` |
| `count`、`localcount` | `0` | 下面全部 / 直接包含的文件与文件夹数（不含自身） |
| `filecount`、`localfilecount` | `0` | 下面全部 / 直接包含的文件数 |
| `foldercount`、`localfoldercount` | `0` | 下面全部 / 直接包含的文件夹数 |
| `containercount`、`localcontainercount` | `0` | 同 `foldercount`、`localfoldercount` |
| `containsfiles`、`containsfolders`、`containscontainers` | `false` | 直接包含文件 / 文件夹时为 `true` |
| `nestinglevel` | 层级：导出根为 `0`，其下第一层为 `1` | 同左 |

- 格式化的大小形如 `4.9 KB`。Windows、Linux 上 1 KB 按 1024 字节算，macOS 上按 1000。
- 时间未知时，日期、时刻和天数都为空。

### 函数

| 写法 | 结果 |
|---|---|
| `{&br}`、`{&tab}` | 换行（`\n`）、制表符 |
| `{&curlyop}`、`{&curlycl}`、`{&angleop}`、`{&anglecl}` | `{`、`}`、`<`、`>` |
| `{if{值1}=={值2}?{结果}}` | 比较成立时输出「结果」，否则不输出 |
| `{if{值1}=={值2}?{结果}:{否则}}` | 输出「结果」或「否则」。运算符：`==`、`!=`、`<`、`<=`、`>`、`>=` |
| `{leftpad{文字}{填充}{长度}}` | 在左侧重复「填充」，直到「文字」有「长度」个字符；文字已经够长时原样输出，从不截断 |
| `{rightpad{文字}{填充}{长度}}` | 同上，填在右侧 |
| `{nest{文字}}` | 「文字」重复 `nestinglevel` 次，用于按层级缩进 |
| `{html{文字}}` | 按 HTML 转义 |
| `{csv{文字}}` | 作为一个 CSV 字段，需要时加引号 |
| `{json{文字}}` | 作为 JSON 字符串，带引号 |
| `{script}` | 本段进入脚本模式，本身不输出 |

比较时两侧先去掉首尾空白。两侧都是十进制数时按数值比较（`10 > 9`，`1.50 == 1.5`），否则按文字比较且不区分大小写（`{JPG}=={jpg}` 成立）。`true`、`false` 就是普通文字。

### 出错时

写错的地方会在输出里留下标记，并给出带段名、行号和列号的提示：

| 标记 | 原因 |
|---|---|
| `*BAD TAG*` | 未知标签 |
| `*BAD IF*` | `if` 的写法不对（缺值、缺运算符、缺 `?` 或 `}`） |
| `*BAD PAD*` | `leftpad`、`rightpad` 的参数不对，填充为空，或长度不是 0 到 10000 之间的整数 |
| `*BAD CALL*` | `nest`、`html`、`csv`、`json` 的参数个数不对 |

没有闭合的 `<%` 原样输出，没有闭合的 `{` 当作在末尾闭合，多余的 `}` 和不认识的 `{&…}` 只给警告。命令行遇到有错误的模板会拒绝导出，并逐条列出错误；只有警告时照常导出，同时显示警告。

### 示例

带精确字节数的文件 CSV：

```text
@name=Files as CSV
@file=files.csv
@blocks=files
@header
path,bytes,modified{&br}
@detail
{if{<%isfile%>}=={true}?{{csv{<%pathfile%>}},<%disksizebytes%>,<%filemodifydate%>{&br}}}
```

JSON Lines，每个文件一个对象。对象的花括号用 `{&curlyop}`、`{&curlycl}` 写出：

```text
@name=JSON lines
@file=files.jsonl
@detail
{if{<%isfile%>}=={true}?{{&curlyop}"path": {json{<%pathfile%>}}, "bytes": <%disksizebytes%>{&curlycl}{&br}}}
```

前三层文件夹的 Markdown 大纲：

```text
@name=Folder outline
@file=outline.md
@blocks=folders
@detail
{script}
{if {<%isfolder%>} == {true}
  ? {{if {<%nestinglevel%>} <= {2}
      ? {{nest{  }}- <%file%> (<%disksize%>){&br}}}}}
```

输出：

```text
- lenny (144 KB)
  - Projects (136 KB)
    - old (124 KB)
```

一年多没改过的文件，带修改日期：

```text
{if{<%isfile%>}=={true}?{{if{<%filemodifyage%>}>{365}?{<%filemodifydate%>  <%pathfile%>{&br}}}}}
```

最后这个没有 `@` 行，所以整段文本就是明细段。

:::tip
模板配合过滤一起用：让过滤负责挑选（`m>1y;>100mb`），模板只负责排版，模板就能写得很短。
:::

## 命令行导出

`spaceknife-backend run`（随软件附带的程序，见[命令行](../command-line/)）可以不打开窗口，依次执行扫描、过滤和导出：

```shell
spaceknife-backend run scan "/home/lenny" filter "*.iso;>1gb" export csv "/home/lenny/isos.csv" autoclose
spaceknife-backend run scan "C:\Users\lenny" filter ":class:视频;>1gb" export "Grouped by folder" "C:\Users\lenny\videos.txt" autoclose
spaceknife-backend run load "/home/lenny/home.sks" export html "/home/lenny/report.html" export "/home/lenny/outline.sktpl" "/home/lenny/outline.md"
```

- `export "<模板>" "<文件>"` 里的模板依次按内置模板名、`csv` / `json` / `html`、模板文件路径查找。
- `filter` 之后的每个 `export` 都只导出匹配的部分；`save` 始终保存完整的树。
- 命令行导出整个视图，包括这次扫描的所有根。
- `:class:` 过滤使用你的文件类别；标签条件在这里不匹配任何文件，因为不读取已保存的标签。
- 目标文件夹必须已经存在，写入同样是原子的。
