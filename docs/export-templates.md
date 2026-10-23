# SpaceKnife 导出模板与命令行

本文是导出模板语言、CSV / JSON / HTML 导出格式和 `spaceknife run` 命令行的规范。参考实现在 `packages/backend/src/spaceknife/export/`（`template.py` 模板语言与遍历，`formats.py` 三种格式，`builtin/` 内置模板）和 `packages/backend/src/spaceknife/cli_run.py`（命令行）。接口约定见 [manage.md](manage.md) 第 4 节。

## 0. 依据与原则

**规格来源。** SpaceSniffer 2.0.5.18 没有公开的模板文档。本文依据仓库根目录 `SpaceSniffer.exe` 里的内容整理，**没有运行它**：

- 程序里的 ANSI 字符串：导出配置的字段名、内置模板的各个片段、标签名与函数名、错误标记（`*BAD TAG*` 等）、命令行帮助；
- 导出窗口（VCL 窗体资源 TFRMEXPORT）右键菜单里列出的全部标签和函数写法；
- 代码里对这些字符串的引用顺序（x86-64 RIP 相对寻址），用来还原内置模板由哪些片段按什么顺序拼成。

提取结果没有入库，方法可重做：按文件偏移取可打印字符串，再扫描 `.text` 段里指向这些字符串的位移）。字符串只给出了“有什么”，没有给出每个标签的输出格式，这部分按名称、内置模板的用法和合理性确定，第 12.4 节逐条列出哪些是推断。

**合理兼容。** 与 [filter-syntax.md](filter-syntax.md) 相同：SpaceSniffer 合理的行为照搬，不合理或无法确认的行为采用更合理的设计，并在第 12 节逐条列出。SpaceSniffer 的模板文本和命令行在 SpaceKnife 中照常可用。

## 1. 速览

```text
{if{<%isfile%>}=={true}?{<%pathfile%>{&br}}}
```

每个文件输出一行完整路径（内置模板 Simple file list）。

```text
@blocks=files
@detail
{script}
{   if {<%isfile%>} == {true}
  ? {  [{leftpad{<%disksize%>}{ }{8}}] <%file%>{&br}}
  : {   if {<%containsfiles%>} == {true}
      ? {{&br}<%pathfile%> [<%disksize%>]{&br}}
    }
}
```

按文件夹分组：每个直接含有文件的文件夹先输出空行、完整路径和占用，再逐行列出其中的文件，占用右对齐到 8 个字符（内置模板 Grouped by folder）。

```shell
spaceknife run scan "D:\Photos" filter "*.jpg;>5mb" export "Simple file list" big.txt autoclose
```

扫描、过滤、导出，不打开窗口。

## 2. 模板文件

一个模板有三段和三个排序选项，与 SpaceSniffer 的导出配置一一对应：

| SpaceSniffer 字段 | SpaceKnife | 含义 |
|---|---|---|
| headerFormat | `@header` 段 | 开头，在导出根上求值一次 |
| detailFormat | `@detail` 段 | 每个元素求值一次（第 6 节的遍历顺序） |
| footerFormat | `@footer` 段 | 结尾，在导出根上求值一次 |
| blockSorting | `@blocks=files｜folders｜mixed` | 同一文件夹里文件在前、文件夹在前，或不区分（默认 `mixed`） |
| fineSorting | `@sort=size｜name｜none` | 按占用、按名称（不区分大小写）或按列出顺序（默认 `size`） |
| fineSortingDirection | `@order=desc｜asc` | 从大到小 / 从 Z 到 A，或相反（默认 `desc`） |
| description | `@name=` | 显示名（缺省为文件名） |
| preferredFileName / Extension | `@file=` | 建议的输出文件名，如 `File report.txt` |

写法：

- **第一行以 `@` 开头**的文本是模板文件：先是 `@键=值` 选项行（空行和以 `#` 开头的注释行可以穿插），然后是各段，每段由单独一行 `@header`、`@detail` 或 `@footer` 开始（不区分大小写，两侧可以有空白），到下一个段标记或文件末尾为止。段的顺序任意，缺少的段为空。
- **否则整段文本就是 detail**，排序用默认值。所以 SpaceSniffer 的 detail 文本可以原样当模板用。
- 段内的每一行都是内容，只有恰好是段标记的一行例外；内容本身要写成 `@footer` 这样的一行时，前面加一个空组：`{}@footer`。
- 换行统一为 `\n`（`\r\n`、`\r` 都按 `\n` 读）。**每段最后一个换行不属于内容**（它是段与段标记之间的分隔）：只有一行的 detail 不会在每个元素后多出换行；需要结尾换行时，在段末加一个空行或写 `{&br}`。
- 文件用 UTF-8 编码（可带 BOM）。不认识的选项、无效的值、无法识别的行给出警告并忽略。

## 3. 语言

一段模板由三种成分组成，从左到右依次输出：

| 成分 | 写法 | 结果 |
|---|---|---|
| 文字 | 除 `{`、`}`、`<%` 以外的字符 | 原样输出（含空格和换行，脚本模式除外） |
| 标签 | `<%名称%>` | 当前元素的一项属性（第 4 节）；名称不区分大小写，两侧空白忽略 |
| 组 | `{…}` | 以关键字开头时是函数（第 5 节），否则是普通分组：输出其中内容 |

规则：

- **组的识别。** `{` 之后跳过空白，若紧接着是关键字（`if` `leftpad` `rightpad` `nest` `html` `csv` `json`），且关键字后面是空白、`{` 或 `}`，这个组就是函数调用；`{&br}` 这类转义和 `{script}` 也在这里识别。其他组都是普通分组，内容原样求值（`{x{y}z}` 输出 `xyz`）。要输出以关键字开头的文字，在前面加空组：`{{}if you like}`。
- **参数。** 函数的参数是紧随其后的若干个组，参数之间可以有空白。参数本身也是组：`{if{a}=={b}?{if{…}…}}` 里第二个 `if` 是嵌套调用（SpaceSniffer 的内置模板就这样写）。
- **花括号必须配对。** 要输出 `{` `}` 用 `{&curlyop}` `{&curlycl}`；要输出 `<%` 用 `{&angleop}%`。
- **脚本模式。** 一段里出现 `{script}`，这一段在解析前先删去所有换行（`\r` `\n`）和制表符，`{script}` 本身不输出。这样可以分行缩进地写嵌套的 `if`，换行用 `{&br}` 显式输出。注意空格**不**删除：段的最外层不要用空格缩进，否则每个元素都会输出这些空格（与 SpaceSniffer 相同）。

## 4. 标签

元素是导出根下的每个文件和文件夹，导出根本身也是一个元素（层级 0）。“文件”指一切不是文件夹的条目（普通文件、符号链接、设备等，与过滤语法的候选范围一致）。

| 标签 | 文件 | 文件夹 |
|---|---|---|
| `file` | 名称（含扩展名） | 名称；导出根是扫描根时取最后一级（`D:\Photos` → `Photos`），卷根取完整路径（`C:\`、`/`） |
| `filename` | 去掉扩展名的名称（`a.tar.gz` → `a.tar`；`.bashrc` → `.bashrc`） | 名称 |
| `fileext` | 扩展名，不带点，保留原大小写（`TXT`）；没有则为空 | 空 |
| `path` | 所在文件夹的完整路径 | 同左；导出根取其上一级（卷根为空） |
| `pathfile` | 完整路径（本平台分隔符） | 完整路径 |
| `size` / `sizebytes` | 逻辑大小：格式化（`4.9 KB`）/ 字节数 | 子树中文件的合计 |
| `disksize` / `disksizebytes` | 占用空间：格式化 / 字节数 | 子树中文件的合计 |
| `filecreatedate` / `filecreatetime` / `filecreateage` | 创建时间：`2024-03-05` / `14:07:09`（本地时间）/ 距今整天数 | 同左（文件夹自己的时间） |
| `filemodify…` / `fileaccess…` | 修改时间、访问时间，同上 | 同左 |
| `isfile` / `isfolder` / `iscontainer` | `true` / `false` / `false` | `false` / `true` / `true` |
| `count` / `localcount` | `0` | 子树中 / 直接包含的文件与文件夹总数（不含自身） |
| `filecount` / `localfilecount` | `0` | 子树中 / 直接包含的文件数 |
| `foldercount` / `localfoldercount` | `0` | 子树中 / 直接包含的文件夹数 |
| `containercount` / `localcontainercount` | `0` | 同 `foldercount` / `localfoldercount` |
| `containsfiles` / `containsfolders` / `containscontainers` | `false` | 直接包含文件 / 文件夹 / 文件夹时为 `true` |
| `nestinglevel` | 层级：导出根为 `0`，其下第一层为 `1` | 同左 |

- 格式化大小与界面一致：Windows、Linux 以 1024 为进，macOS 以 1000 为进，单位写作 KB/MB/GB。
- 时间未知（如 Linux 不记录创建时间）时，日期、时刻、天数都为空。
- 硬链接的其余链接（同一文件已在别处计入）大小为 0，与树图的计算一致。
- 未知标签输出 `*BAD TAG*`，并给出诊断。

## 5. 函数与转义

| 写法 | 结果 |
|---|---|
| `{&br}` `{&tab}` | 换行（`\n`）、制表符 |
| `{&curlyop}` `{&curlycl}` `{&angleop}` `{&anglecl}` | `{` `}` `<` `>` |
| `{if{值1}=={值2}?{结果}}`<br>`{if{值1}=={值2}?{结果}:{否则}}` | 比较成立时输出“结果”，否则输出“否则”（没有就不输出）。运算符：`==` `!=` `<` `<=` `>` `>=` |
| `{leftpad{文字}{填充}{长度}}`<br>`{rightpad{文字}{填充}{长度}}` | 在左侧 / 右侧重复“填充”直到“长度”个字符（多字符的填充在最后截断）；文字已够长时原样输出，从不截断 |
| `{nest{文字}}` | “文字”重复 `nestinglevel` 次（用于按层级缩进） |
| `{html{文字}}` `{csv{文字}}` `{json{文字}}` | 按 HTML、CSV 字段（RFC 4180）、JSON 字符串转义（SpaceKnife 新增） |
| `{script}` | 本段进入脚本模式（第 3 节），不输出 |

- **比较。** 两侧先去掉首尾空白；都是十进制数时按数值比较（`10 > 9`，`1.50 == 1.5`），否则按文字比较且不区分大小写（`{JPG}=={jpg}` 成立）。`true`/`false` 就是文字。
- **填充出错**（填充为空、长度不是非负整数、长度超过 10000）时输出 `*BAD PAD*`。参数在每个元素上才确定，这是运行时的结果，不是语法错误。
- 写法不对的 `if` 输出 `*BAD IF*`，写法不对的 `leftpad`/`rightpad` 输出 `*BAD PAD*`，其余函数输出 `*BAD CALL*`；它们都给出诊断，并跳到这个调用的 `}` 之后继续。

## 6. 遍历

- **深度优先、先序**：先输出文件夹，再依次输出它的内容；每个文件夹的内容按 `@blocks`、`@sort`、`@order` 排序，相同的保持列出顺序。导出根是第一个元素。
- header 和 footer 在导出根上求值一次（所以 `<%disksize%>` 是总占用），detail 对每个元素求值。
- 导出整个视图且视图有多个扫描根（`scan "C:\;D:\"`）时，每个根各导出一遍（各自的 header、detail、footer），依次写入同一个文件。
- 输出分块生成（约 64K 字符一块），遍历只为每个元素保留十几个字节的列，百万级的树也不会拼出一个巨大的字符串。

## 7. 过滤

导出时可以带上过滤（[filter-syntax.md](filter-syntax.md) 的匹配结果，每个节点一个布尔值）：

- 只导出匹配的文件，以及其下有匹配文件的文件夹；导出根始终保留。
- 文件夹的大小、计数、`contains…` 都只算可见的元素：导出的内容与过滤后的视图一致。
- 命令行里 `filter` 之后的 `export` 都带上这个过滤；`save` 不受影响，始终保存完整的树。

## 8. 诊断

`parse_template` 从不抛出异常；每个问题给出严重程度、错误码、中文说明、所在段和源文件中的行列（脚本模式删掉的字符也计入，行列指向原文）。

| 错误码 | 严重程度 | 含义 | 输出 |
|---|---|---|---|
| `unknown_tag` | 错误 | 未知标签 | `*BAD TAG*` |
| `unclosed_tag` | 错误 | `<%` 没有对应的 `%>` | 原样输出 |
| `unclosed_group` | 错误 | `{` 没有对应的 `}` | 当作在末尾闭合 |
| `bad_if` | 错误 | `if` 的写法不对（缺值、缺运算符、缺 `?`、缺 `}`） | `*BAD IF*` |
| `bad_call` | 错误 | `leftpad` `rightpad` `nest` `html` `csv` `json` 的参数个数不对 | `*BAD PAD*` / `*BAD CALL*` |
| `stray_close` | 警告 | 多余的 `}` | 原样输出 |
| `unknown_entity` | 警告 | `{&…}` 不是已知转义 | 原样输出 |
| `bad_option` `unknown_option` `bad_line` `duplicate_section` | 警告 | 模板文件的选项与段 | 忽略 / 以最后一次为准 |

命令行在模板有**错误**时拒绝导出（列出每条诊断，不写文件），只有警告时照常导出并显示警告。程序接口 `render` 照常输出（带上述标记），由调用方决定是否先检查 `ExportTemplate.ok`。

## 9. 内置模板

| 名称 | 显示名 | 来源 | 内容 |
|---|---|---|---|
| Grouped by folder | 按文件夹分组 | SpaceSniffer | 第 1 节第二例；文件在前，按占用从大到小 |
| First level folders | 第一层文件夹 | SpaceSniffer | 层级为 1 的文件夹：空行、完整路径、`[占用]`；文件夹在前 |
| Simple file list | 文件清单 | SpaceSniffer | 第 1 节第一例；文件在前 |
| Summary | 汇总 | SpaceSniffer | 只有 footer：`Total size`、`Folders`、`Files` 三行 |
| Folder tree | 文件夹树 | SpaceKnife | 按层级缩进（每级 4 个空格）的文件夹名和占用 |

模板文本在内置插件 `builtin.export` 里：`packages/backend/src/spaceknife/plugins/builtin/builtin.export/templates/*.sktpl`，可以复制一份改成自己的模板（或者做成插件的 `[[export_templates]]`，见 [plugin-dev.md](plugin-dev.md)）。SpaceSniffer 的四个模板与原版逐字一致（片段的拼接顺序由代码引用还原），只是写成了 SpaceKnife 的模板文件格式。名称查找不区分大小写。

## 10. CSV、JSON、HTML

三种格式与模板走同一套遍历（按占用从大到小、文件与文件夹不分先后，带过滤时只含可见的元素），文件一律 UTF-8。

**CSV**（RFC 4180，`\r\n` 行尾，带 UTF-8 BOM：没有 BOM 时 Excel 按 ANSI 代码页读）。每个元素一行：

| 列 | 内容 |
|---|---|
| `path` `name` | 完整路径、名称 |
| `type` | `file` `folder` `link` `other` |
| `depth` | 层级（导出根为 0） |
| `size` `disk_size` | 逻辑大小、占用（字节；文件夹为子树合计） |
| `files` `folders` | 子树中的文件数、文件夹数（只有文件夹有值） |
| `modified` `created` `accessed` | ISO 8601 本地时间带时区，如 `2024-03-05T14:07:09+08:00`；未知为空 |
| `attributes` | 空格分隔：`readonly hidden system archive compressed encrypted sparse temporary offline notindexed reparse cloud hardlink denied error mount pending` 中适用的项 |

注意：用 Excel 打开时，以 `=` `+` `-` `@` 开头的名字可能被当作公式；SpaceKnife 不改动数据，需要时请用“导入文本”方式打开。

**JSON**：

```json
{"format": "spaceknife-export", "version": 1, "generator": "SpaceKnife 0.1.0",
 "generated_at": "2026-10-18T12:30:00+08:00", "filter": "*.jpg",
 "roots": [
  {"path": "D:\\Photos", "name": "Photos", "type": "folder", "size": 1, "disk_size": 4096,
   "files": 1, "folders": 0, "modified": "…", "created": "…", "accessed": "…", "attributes": [],
   "children": [{"name": "a.jpg", "type": "file", "size": 1, "disk_size": 4096, "…": "…"}]}
 ]}
```

每个元素一行、按层级缩进；只有根带 `path`，文件夹带 `files` `folders` `children`；字段含义同 CSV，未知时间为 `null`。

**HTML 报告**：单个自包含文件（内联 CSS，没有脚本、外部字体或图片；跟随系统深浅色，打印时为浅色）。内容：路径与生成时间（及过滤表达式）、占用 / 逻辑大小 / 文件数 / 文件夹数、文件夹结构（可折叠，默认 4 层、每层最大的 30 项，其余合并为一行）、最大的 50 个文件、占用最多的 20 种扩展名。报告的大小与树的规模无关。

名字里含有无法表示为 Unicode 的字符时（Windows 上不成对的代理项、POSIX 上不是 UTF-8 的字节），写文件时记作 `\udcXX`；在 JSON 里这正是合法的字符串转义。

## 11. 命令行：`spaceknife run`

```shell
spaceknife run <命令> [<命令> …]
```

按顺序执行与 SpaceSniffer 相同的命令，不打开任何窗口。整个序列先检查一遍（未知命令、缺少参数、`filter`/`save`/`export` 前面没有 `scan`/`load`），有错就一条也不执行，退出码 2。

| 命令 | 作用 |
|---|---|
| `help` | 显示帮助 |
| `scan "<路径>"` | 扫描并打开一个视图。多个路径用 `;` 分隔，属于同一个视图（多个扫描根）；路径本身就是一个存在的文件夹时不拆分（文件夹名可以含 `;`） |
| `load "<快照>"` | 打开 SpaceKnife 快照（`.sks`）为一个视图；一个 `load` 只打开一个快照 |
| `filter "<表达式>"` | 对前一个视图应用过滤；表达式有错时列出每处错误并停止，只有警告时照常继续 |
| `save "<文件>"` | 把前一个视图的完整结果保存为 SpaceKnife 快照（不受过滤影响） |
| `export "<模板>" "<文件>"` | 把前一个视图（过滤后）导出到文件。`<模板>` 依次按内置模板名、`csv` / `json` / `html`、模板文件路径查找。写入是原子的（先写临时文件再改名），目标文件夹必须已存在 |
| `autoclose` | 结束时不在终端显示视图摘要 |

元命令（必须是一个不带分隔符的词）：

- `@delim=X`：之后的参数改用 `X` 作分隔符（默认 `"`）；`@delim=XY`：`X` 开头、`Y` 结尾，例如 `@delim={}` 后可写 `filter {"a;b.txt"}`。可以多次切换。
- `@parser=OS`：改用系统拆分好的参数，从头重新解析；之后的 `@parser=OS` 不起作用，`@delim=` 报错。

**参数解析。** SpaceSniffer 自己解析原始命令行：参数以空白分隔，以开分隔符开头的参数一直到闭分隔符为止，**没有转义字符**，所以 `scan "C:\"` 里的 `\"` 不会像 C 运行库那样被当成转义的引号。SpaceKnife 在 Windows 上同样读取原始命令行（`GetCommandLineW` 中 `run` 之后的部分，且只在它与系统拆分的第一个词一致时采用）；在 Linux、macOS 上参数已由 shell 拆分，同样的规则作用在参数上：以开分隔符开头的参数会与后续参数（以一个空格相连）合并到闭分隔符为止。

**执行。** 扫描在本进程内进行（有原生扩展时用 4 个线程，否则单线程），进度和消息输出到标准错误；没有 `autoclose` 时，结束后在标准输出列出每个视图（根、最大的 10 项及占比、合计），作为无界面时的“视图”。退出码：0 成功，2 命令行有误或某条命令失败（停在那条命令），130 被中断。

```shell
spaceknife run scan "C:\" save "C:\snap\c.sks" autoclose
spaceknife run load c.sks filter "\node_modules" export html report.html
spaceknife run @delim={} scan {D:\Photos} filter {"a;b.jpg";>1mb} export {Grouped by folder} {D:\out\files.txt}
```

## 12. 与 SpaceSniffer 2.0.5.18 的关系

### 12.1 照搬

标签全集与名称；`{if}` 的六种比较和有无 `:否则` 两种写法；`leftpad` / `rightpad` / `nest` / `{script}`；六个 `{&…}` 转义；`*BAD TAG*`、`*BAD PAD*` 标记；`true` / `false`；header / detail / footer 三段与三项排序；四个内置模板的文本、名称、建议文件名和排序；命令 `help` `scan` `load` `filter` `save` `export` `autoclose`；`;` 分隔多个扫描路径；元命令 `@delim=X` / `@delim=XY` / `@parser=OS` 及“元命令不能带分隔符”“`@parser=OS` 只有第一次有效、之后不再接受元命令”；内部解析器没有转义字符。

### 12.2 扩展（SpaceSniffer 没有）

| 编号 | 内容 |
|---|---|
| X1 | `{html{}}` `{csv{}}` `{json{}}` 转义函数：写 HTML、CSV、JSON 模板时名字不会破坏格式 |
| X2 | CSV、JSON、HTML 报告三种内置格式 |
| X3 | 内置模板 Folder tree |
| X4 | 文本模板文件：`@` 选项、段标记、`#` 注释；`export` 可以直接给模板文件路径 |
| X5 | 带行列的诊断；命令行在模板有错时拒绝导出 |

### 12.3 有意的差异

| 编号 | SpaceKnife | SpaceSniffer | 理由 |
|---|---|---|---|
| D1 | `{&br}` 和模板里的换行在所有平台都输出 `\n` | `\r\n` | 三个平台输出一致；现在的编辑器和工具都能读 `\n` |
| D2 | 脚本模式删去所有 `\r` `\n` 和制表符 | 删去 `\r\n` 和制表符 | 用 `\n` 行尾保存的模板效果相同 |
| D3 | 内置模板名不区分大小写 | 区分 | 不会让合法的 SpaceSniffer 命令失效，只是更宽容 |
| D4 | 输出一律 UTF-8（CSV 带 BOM） | 未公开 | 任何文件名都能写出；跨平台 |
| D5 | 日期 `YYYY-MM-DD`、时刻 `HH:MM:SS`、“距今”为整天数 | 按 Windows 区域设置格式 | 与区域无关、可排序、可在 `{if}` 里按数值比较 |
| D6 | 大小格式与 SpaceKnife 界面一致（第 4 节） | SpaceSniffer 自己的格式 | 同一个程序里只有一种写法 |
| D7 | 没有“驱动器 / 可用空间 / 未知空间”这类伪元素，`isfolder` 与 `iscontainer` 相同；非文件夹条目都算文件 | 树里有这些伪元素 | SpaceKnife 的树只含真实条目；可用空间等在界面单独显示 |
| D8 | 写法不对的 `if` 等输出 `*BAD IF*` / `*BAD CALL*` 并给诊断 | 未知 | 错误要看得见 |
| D9 | `leftpad` 的长度必须是不超过 10000 的非负整数 | 未知 | 写错的长度不能让每个元素分配巨量内存 |
| D10 | 一个 `scan` 的多个路径是同一个视图的多个根，导出时每个根各一遍 | 未明确 | 与 SpaceKnife 的多根扫描一致；`filter`/`export` 作用的对象明确 |
| D11 | `save` 写 SpaceKnife 快照 `.sks`；`load` 不能读 `.sns` | `.sns`（未公开的二进制格式） | 格式未公开，无法可靠读写 |
| D12 | 一个 `load` 只打开一个快照 | 可用 `;` 分隔多个 | 多个快照是多棵树，不能合成一个视图；写多个 `load` 即可 |
| D13 | `autoclose` 只决定是否显示终端摘要，执行完总会退出 | 关闭窗口 | 无界面执行 |
| D14 | 每段最后一个换行不属于内容 | 配置存在 XML 里，没有这个问题 | 文本文件总以换行结尾，不应让每个元素多出一行 |
| D15 | 模板保存为文本文件 | 存在 SpaceSniffer 的 XML 配置里 | 便于版本管理和分享；SpaceSniffer XML 的导入留待以后（字段已在第 2 节对应好） |
| D16 | 只实现帮助里列出的命令 | 另有未写进帮助的 `log`、`fpmasking` | 前者是 SpaceSniffer 自己的日志，后者是浮点异常屏蔽，与 SpaceKnife 无关 |

### 12.4 推断（无法从程序文本确认）

| 编号 | 内容 | 依据 |
|---|---|---|
| I1 | `{nest{文字}}` 把文字重复 `nestinglevel` 次 | 参数名 `text`、与 `nestinglevel` 同在“脚本”菜单 |
| I2 | 导出根的 `nestinglevel` 为 0，其下第一层为 1 | 内置模板 First level folders 用 `nestinglevel == 1` 选第一层文件夹 |
| I3 | `containsfiles` 指**直接**包含文件 | Grouped by folder 用它决定是否输出文件夹标题，标题下只列直接包含的文件 |
| I4 | header / footer 在导出根上求值；Summary 的三行是 footer | Summary 的片段在代码中的位置晚于其他模板 detail 的位置；`<%disksize%>` 只有在根上才是总量 |
| I5 | `fileext` 不带点；文件夹没有扩展名 | 惯例；`filename` 与 `fileext` 拼起来需要加点 |
| I6 | 比较时数值按数值、文字不区分大小写 | 使 `{if{<%nestinglevel%>}>{2}…}` 这类写法按直觉工作；Windows 文件名本身不区分大小写 |
| I7 | `count` 是子树中文件与文件夹的总数 | 与 `filecount`、`foldercount` 并列 |
| I8 | `*VOID*`（没有当前元素时的输出）在 SpaceKnife 中不会出现 | header / footer 也有当前元素（导出根） |

## 13. 测试

| 测试 | 内容 |
|---|---|
| `packages/backend/tests/test_export.py` | 每个标签在文件、文件夹、导出根上的值；`if` 的六种比较、分支、嵌套、写法错误与恢复；填充（多字符、不截断、运行时错误）；`nest`；三个转义函数与六个 `{&…}`；普通分组、空组、不配对的花括号；脚本模式及其诊断行列；模板文件的选项、段、注释、BOM、`\r\n`、末尾换行规则与诊断；三种排序选项；五个内置模板的完整输出；过滤（可见性、大小与计数、比树短的掩码）；硬链接与已删除行；多个根；分块输出；CSV（列、引号转义、过滤）、JSON（结构、流式）、HTML（自包含、转义、标签配对、截断行、过滤与多根）；按名查找与模板文件；原子写入与非 Unicode 名字 |
| `packages/backend/tests/test_cli_run.py` | 内部解析器（反斜杠加引号、`@delim=` 的一字符和两字符形式、带分隔符的元命令是普通参数、未闭合）、`@parser=OS` 从头重解析、已拆分参数上的同样规则、按参数个数分组与顺序检查、原始命令行中 `run` 之后部分的定位；在临时目录上端到端执行 scan → filter → 多种 export → save → load → export，模板文件与终端摘要，同一视图的多个根与含 `;` 的文件夹名，执行前检查与各种失败（缺路径、过滤错误、未知模板、模板有错、目标文件夹不存在、`.sns`），过滤警告，帮助，以及按 `cli.py` 的方式注册成 typer 命令后的调用 |
