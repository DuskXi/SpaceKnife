# SpaceKnife 插件 API 参考

[plugin-dev.md](plugin-dev.md) 讲每种贡献怎么写；这一份是查阅用的：插件拿到和交回的每个对象有哪些字段、调用发生在哪、缓存什么时候作废、钩子收到什么、路径怎么写、能引用哪些图标和命令。带「生成」字样的表由 `tools/gen_plugin_api.py` 从代码生成，测试保证它们和代码一致。

- **命令里读**：`spaceknife plugin docs --api`；一节：`spaceknife plugin docs --section api-cache`（锚点都以 `api-` 开头，`--list` 列出三份文档的全部锚点，`--search 词` 找到提到它的行）。先读的是一页的[速查](plugin-quick.md)。
- **字段名**：Python 里是属性（`scan.items`），旧代码按 dict 读（`scan["items"]`、`scan.get("items")`）也行。

<a id="api-ctx"></a>
## 1. `Context` 补充

[plugin-dev.md 第 4 节](plugin-dev.md#ctx)列出了全部成员，这里补上它们的细节。

| 成员 | 说明 |
|---|---|
| `ctx.plugin` | `id` `version` `dir`（插件文件夹，只读）`storage`（可写的数据文件夹）`builtin` `dev`（开发链接） |
| `ctx.granted` | 用户授予的权限（`frozenset`），例如 `"files.read" in ctx.granted` 时才去读文件内容 |
| `ctx.slot` | 节点字段被问时在哪：`"inspector"`（详情面板，1 秒）或 `"hover"`（悬停卡，30 毫秒）；其他调用是 None |
| `ctx.node` | 调用针对的节点：字段和洞察的那一项，报告和清理的 `c.node`；没有时是 None |
| `ctx.where` | 这次调用从哪来：一个 [`ActionContext`](#api-models)（`scan_id` `view_id` `node` `selection` `zoom_node` `filter` `args` `compare_id`）。每种调用都有，宿主在一处按调用的来处填好（命令的方法收到的 `c` 就是它），这种调用没有的是 None 或空；每种调用填什么见[调用的上下文](#api-where) |
| `ctx.tree` | 这次调用所在的扫描（`ctx.where.scan_id`）；不针对哪个扫描的调用（设置、语言、删除篮、快照、`start` 钩子，配色的图例）是用户眼前的扫描；没有扫描时抛 `PluginError`「需要先完成扫描」。没有扫描的树的调用读它也抛 `PluginError`，写明原因：`scan_closed` 钩子（扫描已经关掉）、对比里的调用（节点是变化树的）、扫描来源列出时（树还在列）。带着节点下标的调用（字段、洞察、命令和它的 `check`、钩子、一帧的配色和布局、导出……）拿到的是宿主发出调用时的那棵树，那些下标属于它；调用途中树被整理了（[纪元](#api-epoch)变了），再读就抛 `HostError`（`STALE_TREE`），不会答成别的节点。后台任务、流和监控器的 `ctx.tree` 是第一次读时最新的树 |
| `ctx.scan(scan_id)` | 另一个打开着的扫描的树；`ctx.scans()` 列出它们（[`ScanInfo`](#api-models)，新的在前，`front` 标出眼前的那个） |
| `ctx.invalidate(*kinds)` | 丢掉宿主缓存的本插件的回答（[缓存](#api-cache)）；`kinds` 是 `node_field` `node_set` `visualization` `color_mode` `change_annotator`，不写就是全部。界面随后重新要字段、重新过滤、重画、重新取图例 |
| `ctx.fmt` | `size(n)`（用户的 1000 / 1024，和界面一样：`1.5 MB`）`count(n)`（`1,234`）`date(t)`（unix 秒 → `2026-10-24 21:30`，本地时间）`ago(t)`（`3 天前` / `3 days ago`）。导出、提示、文字单元格里用它；报告的表格列写单位就行（前端格式化） |
| `ctx.clipboard.write_text(text)` | 把文字放进剪贴板（要 `clipboard`）：只对发起这次调用的那个窗口有效，界面提示「〈插件〉复制了文字」；钩子、监控器这类没有用户发起的调用不复制 |
| `ctx.basket.propose(items, reason)` / `.withdraw(items)` | `items` 的每项是节点（下标、`Node`、`NodeRef`、绝对路径）或 `(节点, 原因)`；一次调用里放多少次都只提示一次（调用自己 `ctx.toast` 时不提示）。`withdraw` 只拿回本插件放进去的；用户拿出或删掉后插件收到 `basket` 钩子 |
| `ctx.deadline` | 这次调用的截止时间（`time.monotonic()` 的值）；`ctx.check()` 过了它就抛 `Cancelled` |
| `ctx.call(method, params)` | 白名单里的内置方法，返回 dict（[表](#api-core)） |
| `ctx.log` | 写进程序的日志面板，带「plugin:〈id〉」来源；`print` 只进插件进程的 stderr（插件页「最近的错误输出」） |

<a id="api-tree"></a>
## 2. `TreeView`、`Node` 与路径

**属性**

| 属性 | 说明 |
|---|---|
| `scan_id` | 扫描的 id |
| `n`、`len(tree)` | 节点数：下标 `0 … n-1` |
| `roots` | 根的下标（扫描的每个文件夹一个，通常是 `[1]`）；0 号节点是虚拟的顶层节点，不是文件夹 |
| `root_paths` | 根的路径，和 `roots` 一一对应 |
| `virtual` | 这棵树不在磁盘上（`on_disk = false` 的扫描来源） |
| `classes` | 类别表：`{下标: ClassInfo(index, id, name)}`，`cls` 列的值是这里的下标 |
| `epoch` | 树的纪元：宿主挪动了行（监控重新列出、压缩……）后加一，旧下标随之作废（`STALE_TREE`）；什么时候变见[下面](#api-epoch) |
| `version` | 树的版本：任何改动都加一。扫描进行中每批结果都会让它变（很频繁）；扫描完成后只在监控、重新扫描、删除改动树时变。宿主的缓存按它作废 |

**方法**：`node(id)` `nodes(ids)` → [`Node`](#api-models)；`name(id)` `names(ids)`；`path(id)` `paths(ids)`；`rel_path(id, under=None)` `rel_paths(ids, under=None)`；`root_of(id)`；`parent(id)`；`children(id)` → 下标数组（一个文件夹的子项是连续的一段下标：`child_start` 起的 `child_count` 个，`tree.cols("child_start", "child_count")` 整列读，按 numpy 排导出顺序时用得上）；`walk(id=None, chunk=…)` → 一块块下标，**逐层**（先 `id` 自己，再它的子项，再孙辈……同层按下标；不是深度优先，一个文件夹下面的项在结果里不连续）；`col(name)` `cols(*names)` → 只读的整列；`query(…)` → `Rows`（[限量](#api-query)）；`rule_folders(规则)` → 下标数组（[文件夹规则匹配到的](#api-query)）；`find(path)` → 下标或 None；`ref(id)` → [`NodeRef`](#api-models)；`file_ids(ids)` → `(设备, 文件 id, 链接数)`，系统现在说的（要 `files.read`，没有时抛 `PermissionError`；和 `ctx.files` 一样只看插件能读的地方，看不了的、扫描来源的树里的都是 None；只读元数据，不受磁盘负载上限节制）；`close()`（快照用完时）。

**层级**：0 号节点 `depth = 0`，根 `depth = 1`，根下面一层 `2`。`Node.parent` 对 0 号节点是 −1，对根是 0。根的 `name` 是它的完整路径。

**边界**：`query(under=X)` 是 X 下面的项，**不含 X**；不写 `under` 时含各个根（不含 0 号节点）。文件的 `tot_*` 列是 0；`bytes` 列对文件是占用、对文件夹是子树合计、对重复的硬链接是 0。

**文件夹的大小**：文件夹的 `size` `alloc`（`Node`、`bytes` 列）是它**里面**的合计，不含它自己那一项（目录块、扫描来源 `out.dir(size=, alloc=)` 给的）：那一项算进上一层的合计。例：`Videos` 里文件共 734,007,296 字节、目录块 4096 字节，`Videos` 的大小是 734,007,296，它的上级多算 4096（ncdu 把目录块算在文件夹自己身上，导入导出时要换算）。**硬链接**多出来的名字（`Flag.HARDLINK_DUP`）`bytes`、`Node.size`、`Node.alloc` 都是 0，大小算在按树的顺序第一个名字上。

<a id="api-epoch"></a>
**纪元什么时候变**：节点下标就是行号，宿主挪动了行，`epoch` 就加一（`version` 则是树有任何改动都加一）：

- 实时监控重新列出一个文件夹：里面新建、删除、改名了文件或文件夹（只是文件的大小、时间变了不算）。重新列出的文件夹里留下的项也换了下标，所以**监控着的扫描里每多一个文件，纪元就可能变一次**；
- 「重新扫描」一个文件夹；变化多了以后宿主压缩树；
- 扫描来源重新列出（列得久时边列边显示，每显示一批也算一次）；MFT 扫描换上建好的树。

纪元变了以后：还没送出的 `tree_changed` 文件夹跟着换成新下标；正在跑的调用再读树就抛 `HostError`（`STALE_TREE`）：`tree_changed` 钩子由宿主用新下标再调（[树变化](#api-live)），别的调用这次没有结果（字段、检查算没答，界面下次再问）。要跨调用记住节点，存路径或 `tree.ref(id)`。

<a id="api-query"></a>
**`query` 的限量**：不写 `limit` 时最多 10 万行（`QUERY_LIMIT`），写了最多 100 万（`QUERY_MAX`）；有 `sort` 时取排在前面的，没有时按下标。`rows.total` 是命中的总数，`rows.truncated` 说有没有被截掉；没写 `limit` 又被截掉时插件日志里有一条警告。`where` 按过滤框的规则只命中文件。逐个节点的调用（`tree.node(i)`、`tree.path(i)`）每次都跨进程，几千个以上时用 `query(columns=("path", …))`，或 `nodes(ids)` / `paths(ids)` 一次拿。

**文件夹规则匹配到了什么**：`tree.rule_folders("nm")` 是本插件 `[[folder_rules]]` 里 `name = "nm"` 那条规则决定了类别的文件夹（别的插件的写全名 `<id>.<name>`），按下标排，最多 10 万个：只是规则自己匹配上的文件夹，不含它们里面的；用户自己归了类的文件夹和它下面的不在其中。详情面板解释类别时也写出是哪个插件的哪条规则。

<a id="api-paths"></a>
**路径的写法**

| 在哪 | 写法 |
|---|---|
| `tree.path(id)`、`Node.path`、`rows.paths` | 系统自己的完整路径：Windows 用 `\`（`C:\Users\a\b.txt`），其他系统用 `/` |
| `tree.rel_path(id)`、`rel_paths` | 相对它的根（或 `under`），**永远用 `/`**，在每个系统上都一样：`a/b.txt`；根本身是 `""` |
| 扫描来源（`on_disk = false`）的树 | `path` 是 `<root_name>/a/b`（`out.root_name(...)` 给的名字开头），不是磁盘路径 |
| `out.file(path)`、`Change(path)` | 相对来源的根，用 `/`；`\` 也认，`..` 不行 |
| `tree.find(path)` | 绝对路径，或相对第一个根的路径 |
| `Proposal(node="…")`、`ctx.basket.propose(["…"])` | 绝对路径（系统自己的写法） |

**硬链接**：扫描只记下它见到的额外链接（`Flag.HARDLINK_DUP`，`bytes` 列为 0）。Windows 的普通扫描不给 `ino` 和 `nlink`，要判断两个文件是不是同一个时用 `tree.file_ids(ids)`。删除篮自己按（卷、文件 id）认同一个文件的几个名字：只放进一个名字时它不算可释放，全放进去时只算一次（扫描没记文件 id 时删除篮向系统问）。

<a id="api-places"></a>
## 3. 调用发生在哪

报告、清理、方法收到的 `ReportContext` / `ActionContext` 说明用户在哪；不同的位置填的字段不同：

| 位置 | `scan_id` | `view_id` | `node` | `selection` |
|---|---|---|---|---|
| 标签页里的报告（`slot = "tab"`） | 标签页对应的扫描 | 这个扫描的视图 | 视图里的选中项；没选时是缩放根（跟着用户变） | `[node]` |
| 详情面板的一页（`inspector`） | 视图的扫描 | 视图 | 同上 | `[node]` |
| 对话框（`dialog`） | 打开它的地方的 | 同左 | 同左 | 同左 |
| 状态栏（`status`） | 眼前的扫描 | 它的视图 | 选中项或缩放根 | `[node]` |
| 对比侧栏（`compare`） | 对比标签页的扫描 | 对比视图 | 选中的变化项或缩放根 | `[node]` |
| 开始页卡片（`start`）、从开始页打开的历史报告（`history`） | None | None | None | `[]` |
| 命令：右键菜单（`menus = ["node"]`） | 那个视图的扫描 | 那个视图 | 右键点的那一项 | `[node]` |
| 命令：详情面板（`menus = ["inspector"]`） | 那个视图的扫描 | 那个视图 | 详情面板的节点：选中项，没选时是缩放根 | `[node]` |
| 命令：工具栏（`toolbar`）、命令面板、按键 | 有焦点的视图的扫描 | 有焦点的视图 | 选中项；没选时是 None | `[node]` 或 `[]` |
| 命令：开始页（`start`） | 眼前的扫描或 None | — | None | `[]` |

聚合块（「N 个小项」「+N」）不是节点：选中它时报告的 `node` 是缩放根，命令的是 None。右键菜单和详情面板里的命令按上表的 `node` 问 `when` 和 `check`：不可用的变灰，写出原因。`compare_id` 只在对比时有值。`args` 是打开它的动作写的参数（[动作的参数](#api-action-args)）。有视图时还有 `zoom_node`（视图的缩放根，用户眼前的那一层）和 `filter`（视图的过滤式，没过滤是 `""`）：`ctx.tree.query(c.filter or None, under=c.zoom_node, …)` 就是视图里看到的那些文件。命令的 `check` 方法收到同样的 `ActionContext`，它的 `ctx.tree` 就是 `c.scan_id` 那个扫描（两个扫描都开着时也不会读到另一个；对比视图里没有，见[调用的上下文](#api-where)）。`check` 抛出异常时命令不可用，原因写成「检查出错：〈错误〉」（`PluginError` 是它的文字，别的异常是「类型: 异常自己的信息」，最多 160 字；同时进插件日志）；50 ms 内没答（插件正忙、进程正在启动、树刚被整理）算可用：动作运行时自己再查一遍。

导出格式收到的 `ExportInput`：`root` 是要导出的节点（导出对话框选的「当前位置」是缩放根、「选中项」是选中的节点、「整个扫描」是 0）；`match` 是过滤命中的布尔数组（下标是节点，None 表示没过滤）：过滤只命中文件，**文件夹总是 False**，装着命中文件的也是，要不要写出一个文件夹看它下面有没有为 True 的文件；`filter` 是那个过滤式；`selection` 是选中的节点；`options` 是对话框里这个格式的选项。「选中项」选了几项时，`root` 是它们共同的上级文件夹，`match` 只在这几项的子树里为真（没过滤时子树里的文件夹也为真，有过滤时再和命中相与）。

<a id="api-where"></a>
### 调用的上下文：`ctx.where` 与 `ctx.tree`

每种调用都带着它的上下文，宿主在一处（`plugins/where.py`）按调用从哪来填好：`ctx.where`（`ActionContext`）、`ctx.tree`、节点下标的[纪元](#api-epoch)、`ctx.lang`。两个扫描都开着时，每种调用读的都是它所为的那个扫描，不会落到眼前的另一个；没有扫描的树的调用，`ctx.tree` 抛 `PluginError` 说明原因，不会读别的树。纪元一列：「调用开始时」「取下标时」是固定的（那棵树整理了就是 `STALE_TREE`），「不固定」是第一次读时最新的树。

| 调用 | `ctx.tree` | 纪元 | `ctx.where` 里有的 |
|---|---|---|---|
| 命令的 `call`、报告、清理 | `c.scan_id` 的扫描 | 调用开始时 | 和参数 `c` 一样 |
| 面板的 `sk.call`、`sk.subscribe` 的流 | 面板的扫描（`sk.context.scan_id`） | 调用开始时（流不固定） | 面板的 `sk.context`（参数自己写了 `scan_id` `node` 等时以参数为准）；它们不进方法的参数 |
| 命令的 `check` | 视图的扫描（没有视图时是标签页的） | 取下标时 | 视图、选中的节点、缩放根、过滤 |
| 字段、洞察 | 那个节点的扫描 | 调用开始时 | `scan_id` `node` `selection`（字段另有 `ctx.slot`） |
| 节点集合、代码可视化的权重、自带布局 | 视图的扫描 | 取下标时 | `scan_id` |
| 代码配色 | 视图的扫描 | 这一帧的 | `scan_id` `view_id` `zoom_node` `filter` |
| 导出 | 导出的扫描 | 导出时 | `node`（就是 `x.root`）`selection` `filter` |
| `scan_done` / `tree_changed` 钩子 | 那个扫描 | 调用开始时 / `change.epoch` | `scan_id` |
| `scan_closed` 钩子 | 没有：扫描已经关掉 | — | `scan_id`（关掉的那个） |
| `start` `stop` `settings` `lang` `basket` `snapshots` 钩子、配色的图例 | 眼前的扫描 | 不固定 | 空（图例的 `ctx.lang` 是要的那种语言） |
| 后台任务 | 启动它的那次调用的扫描（`start="startup"` 的是眼前的扫描；那个扫描已经关掉时没有） | 不固定 | `scan_id` |
| 扫描来源列出 | 没有：这棵树正在列 | — | `scan_id`（要填的那个扫描） |
| 扫描来源的监控器 | 它监控的那个扫描 | 不固定 | `scan_id` |
| 变化注解 | 对比所在的扫描（条目只有路径，用 `tree.find(path)` 找） | 不固定 | `scan_id` `compare_id` |
| 对比里的调用：对比视图里的命令、`check`、自带布局，对比侧栏的报告 | 没有：节点是对比的变化树的，不是扫描的；对比所在的扫描用 `ctx.scan(ctx.where.scan_id)` 读 | — | `scan_id`（对比所在的扫描）`compare_id` `view_id` 和变化树的 `node` 等 |

对比视图里不问代码配色、节点集合（块是中性色，`:set:` 什么也不命中），也不提供代码权重的可视化。对比视图里命令的参数 `c.scan_id` 是对比的 id（和 `c.compare_id` 一样），以 `ctx.where` 为准。

<a id="api-action-args"></a>
### 动作的参数

动作写的 `args` 交到它打开或运行的东西手里；运行它的一方（面板的 `sk.host.command(id, args)`、测试的 `host.command(name, **args)`）再给的参数加在上面，同名的以运行方的为准：

| 动作 | `args` 到哪 |
|---|---|
| `{ call = "m", args = {…} }` | 方法收到的 `ActionContext.args` |
| `{ cleanup = "c", args = {…} }` | 清理收到的 `ReportContext.args`：一个清理按参数做几种事（「只清理这一条规则」） |
| `{ open = "report:r", args = {…} }` | 报告的 `c.args`；标签页和详情面板里的报告按新的参数重取 |
| `{ open = "panel:p", args = {…} }` | 面板的 `sk.context.args`；面板已经开着时换上新的，`sk.onContext` 收到 |
| `{ command = "c", args = {…} }` | 那个命令的动作（同上几行）；`report.export` 认 `format`（格式或模板）`scope`（`zoom` `all` `selection`）`options`（这个格式的选项，按名字）`use_filter` |

从右键菜单运行时，点的那一项就是 `node` 和 `selection`：清理、`call` 都作用在它上面，`report.export` 打开时「选中项」是它（报告表格里勾了几行时是那几行）。

<a id="api-settings"></a>
### 结构化设置

`control = "list"` 的设置是一列条目，每条是 `fields` 里那几栏的值；`ctx.settings[名]` 读到 `list[dict]`，`ctx.settings.set(名, [...])` 整个换掉。宿主逐条逐栏校验（栏的类型、范围、选项、`max_length`），最多 `max_items` 条；缺的栏补默认值，多出的栏、过长的文字、超出条数都整个拒绝并说明是第几条哪一栏。设置页按 `fields` 画出每条（增删、上下移，数字和文字离开输入框时保存）。`control = "textarea"` 是多行文字（默认最长 4096 字），`text` 默认 64 字，两者都可以写 `max_length`（≤ 16384）；默认值比它长时 `check` 报错并给出该写的数。导出格式的选项不能是列表。

```python
plugin.setting(
    "rules",
    T("清理规则", "Cleanup rules"),
    [{"name": "日志", "glob": "*.log", "days": 90}],
    max_items=20,
    fields=[
        {"name": "name", "title": T("名字", "Name"), "default": "", "max_length": 40},
        {"name": "glob", "title": T("通配符", "Pattern"), "default": "*"},
        {"name": "days", "title": T("天数", "Days"), "default": 30, "min": 1, "unit": T("天", "days")},
    ],
)
```

<a id="api-source"></a>
### 扫描来源：`Listing`、进度与失败

`out.file(path, size, alloc=None, mtime=0, ctime=0, atime=0, *, kind="file", attrs=(), ino=0, nlink=1, dev=0)` 和 `out.dir(path, mtime=0, *, size=0, alloc=0, attrs=())` 记下磁盘扫描也记的东西：

| 参数 | 说明 |
|---|---|
| `kind` | `"file"`；`"link"`（符号链接：显示，不进入）；`"other"`（设备、管道、套接字） |
| `attrs` | `hidden` `system` `readonly` `archive` `compressed` `sparse` `encrypted` `temporary` `notindexed` `offline`（同过滤的 `:attr:`），以及 `cloud`（不在本地的占位）`denied`（读不了的文件夹）`error`（读不了的项）；别的名字抛 `ValueError` |
| `ino` `nlink` `dev` | 文件 id、链接数、设备：`nlink > 1` 且 `ino` 不是 0 时，同一个（`dev`, `ino`）的几个名字只算一次，按树的顺序第一个算，其余标 `Flag.HARDLINK_DUP`（`bytes` 列为 0），和磁盘扫描一样 |
| `out.dir(size=, alloc=)` | 文件夹这一项本身占的（不是里面的）：像磁盘扫描的目录块一样算进上一层的合计 |

`size` 和 `alloc` 两列都照列出的保留。来源声明了 `size = "size"` 时，视图的面积、标签和配色按 `size` 量（压缩包解压后的大小），`alloc` 仍是列出的那个（详情面板两个都显示）；存成快照后又按 `alloc` 量。

- **进度**：来源里的 `ctx.progress(done, total, text)` 就是这次扫描的进度：`total > 0` 时状态栏有百分比，`text` 写在扫描条上（`T(...)` 按当时的语言）。
- **边列边显示**：列了 1 秒以上时，宿主把已经列出的建成树放上去，之后列出的项每翻一倍再建一次（小的来源每 5 秒一次），根的名字是 URI，直到来源结束时用 `out.root_name` 的名字；视图的缩放和选中按路径跟着走。每次建树和最后那次一样贵，所以合起来不超过多建一次。
- **失败**：来源抛出的异常（`PluginError(T(...))` 最好，说清楚在哪里坏了）成为扫描的失败原因：状态是 `failed`，视图里没有内容时画布中间写「没能读出内容」和这个原因；已经显示出来的部分留着。`MockHost.source` 抛 `RuntimeError`，消息就是它。
- **`ScanInfo`**：`source` 是来源的全名，`uri` 是它列的 URI，`on_disk` 是来源的 `on_disk`。

<a id="api-hooks"></a>
## 4. 钩子收到什么

| `event` | 函数签名 | 什么时候 |
|---|---|---|
| `start` | `async def f(ctx) -> None` | 插件进程启动、它的贡献注册好之后（第一次启用、`plugin dev` 重新加载也一样），`ctx.settings` 已有 |
| `stop` | `async def f(ctx) -> None` | 进程停止之前（2 秒内做完） |
| `scan_done` | `async def f(ctx, scan: ScanInfo) -> None` | 一个扫描完成（`scan.state` 是 `done`）；`ctx.tree` 是它 |
| `scan_closed` | `async def f(ctx, scan: ScanInfo) -> None` | 一个扫描被关掉：它的树已经读不到了，`ctx.tree` 抛 `PluginError`（不会读成眼前的另一个扫描） |
| `tree_changed` | `async def f(ctx, change: TreeChange) -> None` | 一个打开着的扫描的树变了（实时监控应用了文件变化、「重新扫描」、扫描来源重新列出）：每个扫描每秒最多一次，`ctx.tree` 是它（`change.epoch` 那一纪元的树）；见[下一节](#api-live) |
| `settings` | `async def f(ctx, changed: list[str]) -> None` | 本插件的设置变了：`changed` 是变了的局部名，如 `["min_size"]`；`ctx.settings` 已是新值。启用后任何时候改的设置（包括进程还在启动时）都在下一次调用或钩子之前到插件进程 |
| `lang` | `async def f(ctx) -> None` | 界面语言变了：`ctx.lang` 已是新的 |
| `basket` | `async def f(ctx) -> None` | 删除篮变了（用户拿出了项、删掉了它们……；一阵变化只来一次）：本插件放进去的可能已经不在了，`ctx.call("basket.list")` 看现在的 |
| `snapshots` | `async def f(ctx, change: SnapshotsChange) -> None` | 存了快照（扫描完成自动保存、「保存快照」）或删了最旧的自动快照；要 `snapshots.read`（没有时宿主不调它，`check` 报 `missing_permission`） |

`event` 写错时 `check` 从代码里找出来（`unknown_hook`，附最像的名字），导入时也报同样的错。钩子在后台跑，宿主不等结果，出错只记日志；每个钩子 30 秒（`@plugin.on(event, timeout=…)`，≤ 600），更久的事 `ctx.tasks.start` 一个[后台任务](#api-limits)；钩子里不能 `ctx.clipboard`，`ctx.toast` 和 `ctx.notify` 发给所有窗口。宿主找到了代码里的钩子却没有注册上它时（插件没启用好）记一行日志，不会悄悄不调。测试里 `await host.hook(event, …)` 会检查事件名和参数（`tree_changed` 的 `change` 不写时是「眼前扫描的根变了」），钩子抛的异常原样抛出。

**先后顺序**：宿主保证的只有这几条：

- 同一事件的几个钩子（一个插件写了两个 `scan_done`）按代码里的顺序一个接一个跑；
- 同一个扫描的 `tree_changed` 一批跑完才送下一批，不会同时跑两个；
- `settings` 钩子跑时插件进程已经拿到新设置，这个插件缓存的回答已经作废。

不同事件之间**没有顺序**：每次事件各是一次调用，宿主不等前一个（普通 `def` 的钩子在工作线程里，`limits.threads` 个可以同时跑）。例如扫描快完成时改了设置，`settings` 可能比 `scan_done` 晚到；`start` 还没跑完时别的钩子也可能来。所以钩子不要靠「先来的那个钩子记下的状态」：每次读现在的（`ctx.settings`、`ctx.scans()`、`ctx.tree`），共用的状态加锁。

<a id="api-live"></a>
### 树变化、快照与对比

- **`tree_changed`**：宿主把一个扫描大约一秒内的变化合成一次调用，插件的钩子还在跑时下一次等它跑完（不会排起队）。`change.folders` 是发生变化的文件夹（变了的文件所在的、变了的文件夹和装着它的）的节点下标，属于 `change.epoch`，钩子的 `ctx.tree` 就是这一纪元的树；最多 256 个，`more` 是大约还有几个；节点下标变了时还没送出的会跟着换成新的。和 `folders` 一一对应的还有 `paths`（路径）、`before` 和 `after`（文件夹连同里面的占用，字节，即 `Node.alloc`）：谁长了多少不用读树。
  - `before` 是这一批里第一个变化动到这个文件夹之前的占用（监控器在动它之前读下），`after` 是这一批最后一个变化之后的；这一批新出现的文件夹 `before` 是 0，`after` 连同它里面的东西（新建了放进文件的、整棵拷进或解压来的），上级文件夹也一样算全；删掉的文件夹连同里面的从上级减掉。
  - 同一个插件的各次调用首尾相接：一个文件夹这一次的 `before` 就是它上一次的 `after`，从变化之前一直接到树现在的样子，按增量累计不会漏也不会重。例外：`before` 是 None 的那次、送不进去被放弃的那批（见下一条），以及扫描还在进行时（扫描新读进来的东西也加在上级文件夹上，不另外送）。
  - `after` 是这一批变化刚完成时的；钩子跑着时又来的变化下一次再送，所以钩子读到的树可能已经比 `after` 新（钩子撞上 `STALE_TREE` 被再调时，等着的那批会一起送，见下一条）。
  - 不知道时 `before` 是 None、`after` 是树现在的：扫描来源重新列出、整棵树换掉，或一批变化牵涉的文件夹（连同上级）超过 3.2 万个。
  - 扫描来源重新列出时 `folders` 是根。文件夹的合计跟着变到根，更上层的文件夹没有列出，要的话读树（`tree.node(f).parent`）。
- **读树时树又变了**：监控着的扫描里每多一个文件，纪元就可能变一次（[纪元](#api-epoch)）。钩子读树时撞上了，读的那一步抛 `HostError`（`STALE_TREE`）：**不要捕获它**，宿主马上用新的下标再调这个钩子（最多再调 3 次），这期间又来的变化一起送（一次调用，`before` 是前一批之前的、`after` 是后一批之后的）；还不行就和下一批变化一起送；这样的失败不记警告。所以同样的文件夹可能来不止一次：按路径记账，重复来也无妨。连续 6 批都送不进去时（钩子读的是别的调用存下的树？）宿主放弃这批，记一条警告。
- **实时监控**：`ctx.scans()` 的 `ScanInfo.monitoring` 说扫描是否跟着文件变化：磁盘扫描从开始扫描起就挂着监控（设置「实时监控」关着时是 `off`；`starting` 只在监控器刚启动的那一下，扫描中已是 `running`）。Linux 上每个文件夹在扫描读过它之后才加上监视，加上时按扫描记下的修改时间补查一次，所以扫描刚完成时写进去的文件也会进树、送到 `tree_changed`（可能晚一两秒）；打开的快照是 `off`；扫描来源有 `watch` 的监控器时在列完之后开始。插件重启（`plugin dev` 存盘后重新加载）结束了监控器的调用时，宿主很快再调一次、把来源重跑一遍，状态仍是 `running`；插件停用时是 `failed`，重新启用后接着监控。
- **`snapshots`**：一阵保存和删除（≤ 0.5 秒）合成一次，`change.saved` / `change.removed` 是快照文件；`ctx.snapshots.list()` 读详情。窗口同时收到协议事件 `snapshots.changed`，网页面板用 `sk.onSnapshots(cb)` 听（要 `snapshots.read`）。
- **打开对比**：命令（或报告按钮）调用的方法返回 `CompareOpen(base=快照, target="now")`，界面就打开对比（W6 的对比模式）；两端可以是两份快照，或一份和 `"now"`（同一组文件夹的实时树）。界面用显示那组文件夹的标签页（`scan_id` 指定时用它的）；比 `"now"` 而没有这样的标签页时先扫描再对比；两份快照而没有时打开 `target` 那份、在它的标签页里和 `base` 比。网页面板：`sk.host.compare(base, target)`。都要 `snapshots.read`。

```python
@plugin.on("tree_changed")
def grew(ctx: Context, change: TreeChange) -> None:
    big = [
        path
        for path, before, after in zip(change.paths, change.before, change.after, strict=True)
        if before is not None and after - before > 1 << 30  # 这一批里长了 1 GiB 以上
    ]
    if big:
        ctx.notify(T("文件夹一下长了 1 GB 以上", "Folders grew by over 1 GB"), "\n".join(big[:3]))


@plugin.command("compare_last", T("和上一份快照比", "Compare with the last snapshot"), when=["done"])
def compare_last(ctx: Context, a: ActionContext) -> CompareOpen | None:
    snaps = ctx.snapshots.list()  # 新的在前
    return CompareOpen(base=snaps[0].path, target="now", scan_id=a.scan_id) if snaps else None
```

<a id="api-notify"></a>
### 通知

`ctx.notify(title, body="", *, action=None, button=None, args=None, tone="info") -> bool`（要 `notify`）：窗口右下角出一张卡片，写着插件名、`title` 和 `body`（多行可以），用户关掉之前一直在（窗口在前、指针不在卡片上时过 12 秒自己收起）；`action` 是本插件一条命令的名字（或全名），卡片上多一个按钮运行它（`button` 是按钮的字，不写时用命令的标题；`args` 加在命令的动作上，同 `sk.host.command(id, args)`）。窗口最小化或藏起来时，系统同时弹出通知，点它窗口回来并运行这条命令；没有系统通知的地方（Windows 的便携版、浏览器没给许可）任务栏闪一下。每个插件 5 秒最多一条：返回 False 表示这条被略过（宿主也这样限）。`title` ≤ 120 字、`body` ≤ 1000 字，`args` ≤ 4 KiB 的 JSON；`action` 不是本插件的命令时卡片没有按钮（日志写明）。通知同时进日志面板。来自用户发起的调用时只发给那个窗口，钩子和后台任务里发给所有窗口。测试：`host.notices`（`title body tone command button args`）。

<a id="api-errors"></a>
## 5. 异常

| 插件代码里 | 什么时候 | 用户和调用方看到 |
|---|---|---|
| `PluginError(T(...))` | 插件自己要告诉用户的话 | 原样显示（`PLUGIN_ERROR`） |
| `Cancelled` | `ctx.check()`：调用被取消或超时 | 不要吞掉，让它抛出去 |
| `PermissionError` | 少了权限：`ctx.files`、`ctx.net`、`ctx.clipboard` …；审计钩子拦下的联网、写文件、开进程。`ctx.files` 读范围外的文件是它的子类 `FileNotAllowed`；系统拒绝（用户自己也读不了）是普通的 `PermissionError` | `PERMISSION_DENIED`。**它是 `OSError` 的子类**：`except OSError:` 会把它一起吞掉，读文件时写 `except (FileNotFoundError, IsADirectoryError):` 这样具体的异常 |
| `spaceknife_sdk.errors.HostError` | 宿主拒绝了插件的请求：`code` 是协议错误码（如 `STALE_TREE` −32018：树在调用途中被整理了，见[纪元](#api-epoch)） | 不捕获时原样往外传；`tree_changed` 钩子里的 `STALE_TREE` 让宿主用新下标再调它 |
| 其他异常 | 插件的 bug | `plugin error: 〈类型〉: 〈异常自己的信息〉`（`PLUGIN_ERROR`；信息合成一行，最多 300 字），回溯进日志 |

测试和命令行里失败的调用抛 `spaceknife_sdk.RequestError`：`.code`（`spaceknife_sdk.ErrorCode`，如 `PLUGIN_ERROR` `PERMISSION_DENIED`）、`.message`、`.data`。

**错误码**：`-32014 PERMISSION_DENIED`（少了权限）、`-32015 PLUGIN_TIMEOUT`、`-32016 PLUGIN_UNAVAILABLE`（进程起不来、崩溃、卡死）、`-32017 PLUGIN_DISABLED`、`-32018 STALE_TREE`（下标过期）、`-32019 PLUGIN_ERROR`（`PluginError` 的文字原样给用户）、`-32020 PLUGIN_LIMIT`（并发、排队或输出超限）。

<a id="api-cache"></a>
## 6. 缓存

宿主替插件记住一些回答，免得每次悬停、每帧都去问插件：

| 什么 | 按什么记 | 什么时候作废 |
|---|---|---|
| 节点字段 | 字段、扫描、节点、树的 `version`、界面语言（详情面板和悬停卡共用一份） | 树变了；插件设置变了；`ctx.invalidate("node_field")`。**返回 None 也会被记住**（这一项不显示，直到作废） |
| 节点集合（`:set:`） | 集合、扫描、树的 `version` | 同上（`"node_set"`）；作废后用到它的过滤自动重算 |
| 代码可视化的权重 | 可视化、扫描、树的 `version`、它的 `options` 和本插件的设置 | 同上（`"visualization"`）；作废后视图重画 |
| 配色的图例（`@plugin.color_legend`） | 配色、语言 | 设置变了、语言变了、`ctx.invalidate("color_mode")`；新的算出来之前仍显示旧的，作废时宿主马上去取新的（设置变了时等插件进程拿到新设置之后才取） |
| 对比原因 | 对比、变化项 | 设置变了（`"change_annotator"`） |
| 代码洞察（`@plugin.insight`） | 不缓存：详情面板每显示一个节点就问一次（选中项变了、树变了、插件重新加载后再问），1 秒内没答或出错就没有这张卡 | — |
| 报告 | 不缓存：按 `refresh` 重新取 | — |

插件设置一变，宿主先把新设置交给插件进程，再把这个插件的全部缓存作废（插件的「缓存纪元」加一），然后调 `settings` 钩子：重新取的图例、字段和钩子都看到新值。插件自己的数据变了（它读的文件、它建的索引）、宿主看不出来的时候，调 `ctx.invalidate()`。字段在后台算完了也一样：`ctx.invalidate("node_field")` 之后界面会再要一次。

<a id="api-paging"></a>
## 7. 表格分页

一张表最多 2000 行；更多的行用 `more` 一页页给：

```python
@plugin.report(T("全部文件", "All files"))
async def all_files(ctx: Context, c: ReportContext) -> Report:
    start = int(c.cursor or 0)  # 第一页是 None
    rows = ctx.tree.query(kind="file", columns=("bytes", "path"), sort="-bytes", limit=start + 500)
    page = rows.records()[start:]
    more = r.more(str(start + 500)) if rows.total > start + 500 else None
    return Report(blocks=[r.table(COLS, rows=page, more=more)])
```

`r.more(cursor)` 填好要再调的报告（线上名 `plugin.<id>.<报告名>`，就是当前这个）；界面点「更多」时带着 `c.cursor = cursor` 再调一次，把返回的第一张表的行接在后面。`r.more(cursor, report="other")` 让另一个报告给下一页。`MockHost` 里：`await host.report("all_files", cursor="500")`。

<a id="api-limits"></a>
## 8. 时限、线程、后台任务与读盘

[plugin-dev.md 7.2 节](plugin-dev.md#permissions)是总表。能改的：`@plugin.method(timeout=…)`、`@plugin.command(…, timeout=…)`、`@plugin.report(…, timeout=…)`（默认 30 秒）、`@plugin.cleanup(…, timeout=…)`（默认 120 秒）、`@plugin.on(…, timeout=…)`（钩子，默认 30 秒），都 ≤ 600 秒；方法、命令和报告另外不超过清单的 `limits.timeout_s`（默认 60：写了 `timeout=300` 也只有 60 秒，要更久就把它也调高）。等宿主的调用槽（每个插件同时 4 个，`limits.threads` 更多时随它）的时间也算在时限里；监控器、流和后台任务不占调用槽。

**线程**：`async def` 的函数在插件进程的事件循环上跑；普通 `def` 的在工作线程上跑，同时 `limits.threads` 个（默认 2，1–8）。线程停不下来：被取消的调用（时限到了、用户走开了、悬停字段的 30 ms、配色的 20 ms 没赶上）宿主立即答「取消」、丢掉它后来的结果，**它的位置马上让给下一个调用**，函数在备用线程上跑完（最多 8 个备用线程；5 秒后还在跑时插件日志写出是哪个调用）。只有备用线程也用完、每个工作线程都被被取消的调用占了 30 秒，宿主才重启插件进程。所以长循环里调 `ctx.check()`（被取消后它抛 `Cancelled`）。`async def` 要在 `await` 处才停得下来：一直不 `await` 的长循环卡住整个事件循环，5 秒后日志写出是哪个调用，30 秒后宿主结束进程——把它写成普通 `def`，或放进 `await ctx.compute(...)`。

**后台任务**（`@plugin.background`）：没有时限（可以给 `timeout=`，≤ 1 天），不占调用槽和工作线程（普通 `def` 每个任务一条自己的线程，`async def` 在事件循环上）。宿主给每个任务一份记录（[`TaskInfo`](#api-models)），所有窗口都看得到（状态栏的任务列表，协议的 `plugin.tasks` 和 `plugin.task` 事件），用户可以取消：普通 `def` 在下一个 `ctx.check()` 停下，`async def` 在下一个 `await`。`ctx.progress(done, total, text)` 写进它的记录（每秒最多推 4 次；`total = 0` 表示说不出进度）。界面语言变了，任务在下一个 `ctx.check()` 换成新的（`ctx.lang`、`T(...)`）。`restart=True`：插件进程崩溃时宿主用同样的参数重新开始（`attempt` 加一，最多 3 次，进程的崩溃上限照样算）；用户停用、重启插件，或 `plugin dev` 重新加载时任务算取消，`start="startup"` 的随新进程再启动。有后台任务在跑时插件进程不算空闲。参数 ≤ 64 KiB 的 JSON；每个插件同时 8 个，结束的留最近 8 个。钩子里只做一会儿就完的事，长的交给 `ctx.tasks.start`（钩子返回后任务照样跑）。

**读盘**：`ctx.files` 的读（`open` 得到的文件、`read`、`hash`、`hash_many`）一段段向宿主要这块盘的许可，和扫描共用同一个磁盘负载上限（用户选的档位：这块盘正在扫描时是那次扫描的，否则是设置里的「磁盘负载」），算在 SpaceKnife 自己的负载里：一段最多 4 MiB 或 50 ms。许可只在读的时候拿着：一段读完、一个文件读到头（`open` 的文件读到末尾或关掉）、`hash_many` 的读线程没有文件可读了，许可马上还给宿主，插件的代码做别的事时不占着（没还的 2 秒后宿主收回）；`hash_many` 按节点下标给时，路径一批批地取。扫描正在读这块盘时，所有插件一次只读一段，扫描先走；扫描暂停时插件不等它。等许可时也会发现调用被取消。`open_path` 只给出路径：拿路径自己读盘的库（`zipfile.ZipFile(path)`、`ctx.exec` 的程序）不受这个限制，能传文件对象的就传 `ctx.files.open(n)`（可以 `seek`）。

单位的值：`size` 字节；`count` 个数；`pct` 0–1 的小数（0.25 显示成 25%）；`date` `ago` unix 秒；`duration` 秒；`bytes_per_s` 字节每秒。

长度：`Proposal.reason` ≤ 120 字；字段的值 ≤ 200 字；对比原因 ≤ 80 字；`ctx.toast` ≤ 512 字、每秒最多一条；`ctx.log` 一条 ≤ 4000 字；剪贴板 ≤ 1 MiB。

<a id="api-frames"></a>
## 9. 测试里的帧和图例

`await host.visual(name)`、`await host.color(name)` 返回宿主画出的一帧（已按行契约检查过）：

| 成员 | 说明 |
|---|---|
| `frame.draw.node` `key` `parent` `kind` `depth` `size` `count` `cls` | 每一行（一个画出的块）：节点、稳定的键、外层文件夹的行、种类（`RectKind`）、层级（1 是缩放根的子项）、面积依据、聚合块的项数、类别下标 |
| `frame.root` | 缩放根本身（不是一行）；`frame.root.size` 是它的大小（权重可视化里是它的权重：标签的「占权重的 x%」以它为分母） |
| `frame.pal` | 插件配色时每行的色板下标（`uint8`），否则 None |
| `frame.size_unit` | `bytes` `count` `weight`：标签怎么写 |
| `frame.draw.layout` | `drawlist.v1`（树图）`flame.v1` `polar.v1`，自带布局是 `shapes.v1` |
| `frame.draw.geometry` | 几何列：`shapes.v1` 是 `x y w h`（外框）`r`（圆角，半个短边就是圆）`poly`（多边形的点数，0 不是多边形），物理像素 |
| `frame.draw.points` | `shapes.v1` 里多边形的点，`x y` 交替，按行的顺序 |
| `frame.draw.label` | 每行的标签位（`uint8`）：0 不写标签；`1` 名字、`2` 第二行写大小、`4` 大字、`8` 矮块只写名字（字小，竖直居中）、`16` 极坐标里沿半径写、`32` `shapes.v1` 写在形状里的顶上（展开的文件夹）。宿主按块的大小定，`Shapes` 的 `label=` 只是要求，放不下时仍是 0 |
| `frame.draw.meta` | `shapes.v1` 是 `ShapesGeometry`：`root`（缩放根的形状，没有是 None）`nested` `message`（布局失败的原因）`stroke`（形状边里那条线，物理像素） |

**图例在哪**：工具栏的配色菜单里，每种配色下面一行色块和说明（声明的 `legend`、按 `bands` 生成的，或 `@plugin.color_legend` 给的）；没有说明的色板项不显示。

<a id="api-layout"></a>
### 自带布局：`LayoutInput` 与 `Shapes`

`@plugin.layout` 的函数收到 `LayoutInput`（字段见第 14 节），返回 `Shapes`（`out = v.shapes()`）。坐标都是物理像素，`v.dpr` 个物理像素一个 CSS 像素。

| `Shapes` 的方法 | 形状 |
|---|---|
| `out.rect(i, x, y, w, h, radius=0)` | 矩形，圆角 `radius`（半个短边：药丸形，宽高相等时是圆） |
| `out.circle(i, cx, cy, r)` | 圆 |
| `out.polygon(i, [(x, y), …])` | 凸多边形，3–12 个点，任一方向；不凸的不画 |
| `out.rects(ids, x, y, w, h, radius)` `out.circles(ids, cx, cy, r)` | 一次很多块（数组或数） |
| `out.clear(i)` | 这一块不画了（它里面的块也不画） |

- `i = -1`（`shapes.ROOT`）是缩放根：给了形状就画在最下面，上沿写着名字和大小（扫描的那个文件夹只写它自己的名字，不写整条路径）。悬停和火焰图的根条一样；单击它在详情面板里显示它，**双击**回到上一层（双击别的块是放大进入）。
- 块按顺序画，后画的盖住先画的；指针落在最后一个盖住它的形状上。里面一块都没画的文件夹画成折叠的。
- 线：每个形状的边里面有一条画布底色的细线（和火焰图块之间的缝一样），宽 `out.stroke` 个 CSS 像素：默认 1，0 不画，最多 4（超出时这一帧失败并写明）。深层的块和外层同色时（分支配色超过第 4 层、浅色主题里的文件夹）靠它看得出边。「边框对比」调它的深浅；条纹的块（可用、未知空间、等待扫描）用自己的边框；比 4 条线还窄的形状不画。
- 选中：一般的块像火焰图那样浮起（投影、自己再画一遍、选中环）；`nested` 布局里展开的文件夹只画选中环，它里面的块照样看得见（和树图选中文件夹一样）。
- 标签：每个方法都有 `label="auto" | "centre" | "top" | "none"`。`auto`：展开的文件夹在 `nested` 布局里画成矩形时名字写在顶部，画成圆或多边形时不写（里面是它的内容），其余写在中间；放得下两行写名字和大小，否则一行，放不下不写。
- 标签占的地方在 `v.label`（`LabelSpace`，物理像素，随 `dpr` 和「元素字号」，lu = `dpr` × `font_scale`）：`top` 是顶部标签从形状上沿往下占到的地方（19 lu），`lines` / `line` 是中间写两行 / 一行要的高度（34 / 18 lu），`font` 是字号。`nested` 布局里，一个顶部标签和外层形状（含缩放根）的顶部标签叠在一起时，宿主不写里面那个，所以圆堆积可以给每个展开的文件夹都要 `label="top"`；想让两个名字都在，里面的形状上沿要比外层的低 `v.label.top` 以上。
- `v.children(i)`：块 i 里的块（−1：缩放根上的）；`v.levels` 层数；`v.min_element` 最小元素（物理像素）；`v.options` 是 `options` 里列出的本插件设置的值。
- `shapes.pack(v, padding=2, margin=8)`：d3 的圆堆积（front-chain 加最小外接圆），确定的；根的圆上沿留出名字那一条（`v.label.top`）；`shapes.pack_siblings(radii)`、`shapes.enclose(circles)` 是它的两步。
- 失败：超时（5 s）或出错时这一帧没有行，画布中间写原因（异常只写类型，traceback 在插件日志里；`Shapes` 的块数不对等 SDK 自己的检查写全文）；下一个画面再问。坐标夹在 ±2^24 内，圆的直径取偶数，其余按整像素取整。

冰柱图（`fill = 1`，`nested = False`）：

```python
@plugin.layout("icicle", T("冰柱图", "Icicle"), fill=1.0, nested=False)
def icicle(v: LayoutInput) -> Shapes:
    out = v.shapes()
    band = min(v.height / (v.levels + 1), 48 * v.dpr)
    out.rect(-1, 0, 0, v.width, band - 2)  # 缩放根：顶上一条
    x, w = v.u0 * v.width, (v.u1 - v.u0) * v.width
    shown = np.flatnonzero(w >= 2 * v.dpr)  # 太窄的不画（它下面的也不画）
    out.rects(shown, x[shown], v.depth[shown] * band, w[shown] - 2, band - 2)
    return out
```

<a id="api-testing"></a>
## 10. `MockHost` 一览

| 方法 | 做什么 |
|---|---|
| `MockHost.load(path, grant=…, process=False)` | `path` 所在的插件；`grant=["tree.read"]` 只给部分权限；`process=True` 走真的插件进程 |
| `host.scan(tree)` / `await host.scan_dir(path)` | 用合成树，或真的扫描一个文件夹 |
| `host.find("a/x.bin")`、`host.store` | 相对根的路径 → 节点下标（找不到抛 `KeyError`）；存储 |
| `await host.call(name, **params)`、`call_frames` | 方法：JSON（和二进制列）；参数里的 `scan_id` `node` `selection` …… 和应用里一样成为 `ctx.where`，`ctx.tree` 默认是眼前的扫描 |
| `await host.report(name, node=, cursor=, **ctx)`、`host.text(report)` | 校验过、按上限截过的报告；纯文本 |
| `await host.fields(slot, node=)`、`await host.insights(node=)` | 字段（`"inspector"` 或 `"hover"`；本插件的字段抛了异常时这里抛 `RuntimeError`，程序里是不显示它）、洞察卡片 |
| `host.node("a/b")` | 相对根的路径 → [`Node`](#api-models)（`size alloc mtime …`，文件夹是子树合计），和 `ctx.tree.node` 给的一样 |
| `await host.legend(name)` | 本插件配色的图例文字，和配色菜单里的一样（`@plugin.color_legend` 的会等它答完） |
| `await host.cleanup(name, node=, args=)` | 复核对话框里的候选：`node path reason checked alloc`（`args` 成为 `c.args`） |
| `await host.node_set(name)`、`await host.filter(expr)` | 集合的掩码；过滤命中的相对路径 |
| `await host.visual(name)`、`await host.color(name)` | 一帧（[第 9 节](#api-frames)） |
| `await host.export(name, filter=, options=, selection=, root=)` | 导出的全文；`options` 像程序那样校验：没有的选项、不在可选值里的、超出范围的数抛 `RpcError` |
| `await host.source(name, uri)`、`host.status(scan_id)` | 用扫描来源扫描（失败抛 `RuntimeError`）；扫描的状态 |
| `await host.hook(event, **args)`、`await host.annotate(items)` | 钩子（检查事件名和参数，抛出钩子里的异常；缺权限的钩子抛 `PermissionError`；`ctx.tree` 和应用里一样，`scan_closed` 的没有）、对比原因（`ctx.tree` 是眼前的扫描） |
| `await host.changed(*nodes)` | 当前扫描在这些节点（下标或相对根的路径）上变了，像应用文件变化那样：`tree_changed` 钩子按宿主的方式（合并一秒、文件夹）跑完才返回；树本身不变 |
| `await host.task(name, params, key=, wait=True)`、`host.tasks()`、`await host.cancel_task(name, key=)` | 后台任务：启动并等它结束，返回 `TaskInfo`（`state` 是 `done` `failed` `cancelled`，`error` 是失败的原因）；`wait=False` 马上返回；本插件的任务（跑着的和最近结束的）；像取消按钮那样取消并等它停下 |
| `await host.command(name, node=, selection=, zoom_node=, filter=, **args)` | 按菜单那样对 `node` 运行命令，`args` 加在动作自己的上（同面板的 `sk.host.command(id, args)`）：`call` 返回方法的结果（`Toast`、`CompareOpen` 也是 dict），`cleanup` 返回候选（同 `host.cleanup`）；其余（`open` `command` `view` `filter` `setting` `scan_source` ……）只返回宿主规整过的动作 dict，不运行它：`{"kind": "open", "target": "report:<全名>", "args": {…}}`、`{"kind": "scan_source", "source": "<全名>", "uri": "$node"}`，名字补成全名、`args` 合并好，`$node` 这类占位**不替换**（界面运行动作时才换成节点的路径） |
| `await host.command_available(name, node=)` | 选中 `node` 时这个命令能不能跑：它的 `when` 和 `check` 给的原因，能跑时 None |
| `await host.visual_available(name)` | 本插件的可视化在当前扫描的视图里能不能选：它的 `when` 给的原因，能选时 None |
| `await host.settled()` | 等宿主后台的事做完：悬停字段的后补、钩子、设置和语言的变化 |
| `host.path("a/b")` | 节点在界面里的路径（系统的写法，Windows 是 `\`）：和插件写出的路径比较时用它 |
| `await host.export_text(name, …)`、`export_bytes` | 同 `export`，类型确定（文字格式 / 二进制格式） |
| `host.snapshot(when=None)` | 把当前扫描存成快照（同一秒两次也不重名；`when` 是文件名和快照里记的时间），给读历史的插件；和程序里一样触发 `snapshots` 钩子（`await host.settled()` 等它） |
| `host.events`、`host.state(name)`、`host.toasts`、`host.notices`、`host.clipboard`、`host.basket` | 副作用：事件、共享状态、提示、通知（dict：`title body tone command button args`）、复制的文字、删除篮 |
| `host.commands()`、`host.keys()`、`host.presets()` | 本插件的命令、按键（键 → 命令）、过滤预设 |
| `await host.set_setting(name, value)`、`await host.set_lang("en")` | 改设置（等 `settings` 钩子跑完）、改语言（触发 `lang` 钩子） |
| `await host.check(strict=True)` | 和 `plugin check` 一样的检查 |
| `host.call_sync(name, **params)` | 不写 async 测试时 |

`make_tree({...}, root=None)` 的值：大小、`(大小, 修改时间)`、`bytes`（真的写成文件，可以读和哈希）、`(bytes, 修改时间)`、dict（文件夹）；修改时间是 unix 秒或 `"YYYY-MM-DD"`。默认的根在 `~/.spaceknife-mock/` 下（`SPACEKNIFE_MOCK_TREES` 可改），不在临时文件夹里，免得程序自带的「临时文件夹」规则盖住测试要看的东西；`root=` 指定别的位置。

**测试插件自己的函数**：`from spaceknife_sdk.testing import plugin_module`，`mod = plugin_module(__file__)` 得到插件的 `plugin.py`（按宿主的方式导入，`lib/` 也在路径上；它是另一份副本，看不到宿主里那份的状态，`monkeypatch` 它也影响不到宿主）；不要写 `from plugin import x`，那可能导入别的插件的 `plugin.py`。

**返回值的形状**：`host.toasts` 是字符串的列表（提示的文字）；`host.notices` 是 dict 的列表；`host.basket` 是删除篮的条目（`path name kind scan_id alloc size files dirs status note origin reason`，`origin` 是提议它的清理的全名，用户自己放的是 None；没有节点下标，用 `host.path(...)` 比较 `path`）；`host.events` 是 `{事件名: [载荷…]}`；`host.presets()` 是 `{名字: 过滤式}`；`host.keys()` 是 `{键: 命令全名}`；`host.find` 找不到时抛 `KeyError`；`await host.cleanup(...)` 是 `Proposed(node, path, reason, checked, alloc)` 的列表；`host.status()` 是扫描的状态（`state items message progress monitor …`）。

**合成扫描**：`started_at` 和 `finished_at` 是 `host.scan()` 的时间；`ScanInfo.source` 是 `"scan"`（只有从快照文件打开的才是 `"snapshot"`）；`ScanInfo.snapshot` 对普通磁盘扫描是它自动保存的那个快照（不是「从哪个快照打开的」），快照打开的扫描是那个文件。

**命令行里看历史**：`plugin call` / `plugin render` 的 `--history FILE`（可以写多次）把快照放进临时后端的历史，`ctx.snapshots.list()` 就能看到它们；`--snapshot FILE` 是把一个快照当作当前的扫描。

<a id="api-shot"></a>
### `plugin shot` 的步骤

`--step`（可以写多次，按顺序）在页面就绪之后、截图之前像用户那样操作；一步做不成时后面的不做，照样截图并说停在哪一步：

| 步骤 | 做什么 |
|---|---|
| `click:<元素>` `rclick:<元素>` `dblclick:<元素>` `hover:<元素>` | 用鼠标点、右键（截得到右键菜单）、双击、悬停。`<元素>` 是 CSS 选择器，或 `text=<看得见的文字>`（最里层显示这段文字的元素） |
| `click:node=<a/b>` `rclick:node=…` `dblclick:node=…` `hover:node=…` | 对画布上画出的节点（相对根的路径；`node=` 是缩放根本身）：鼠标落在视图画它的地方（真的鼠标事件，和用户一样走视图自己的处理：单击选中、双击进入文件夹、在缩放根上双击回到上一层、右键出菜单、悬停出悬停卡）。节点没画出来（在没展开的文件夹里、太小、被盖住）时这一步失败并说明 |
| `click:@x,y` 等 | 在窗口的这个点（CSS 像素，就是截图的像素）点、右键、双击、悬停 |
| `set:<元素>=<值>` | 填输入框、多行文字、下拉框；勾选框写 `true` / `false`；之后离开输入框（触发校验） |
| `type:<文字>` | 往有焦点的输入框里打字（先 `click:` 它；`key:Control+A` 再打字就是替换），和键盘输入一样触发输入事件 |
| `key:<键>` | `Escape` `Enter` `Tab` `ArrowDown` `F5` `a`，组合键 `Control+K` `Shift+Alt+x` |
| `select:<a/b>` `do:<命令>` `open:<目标>` | 选中节点（相对根）、运行命令（同 `--do`）、打开（同 `--open`） |
| `wait:<秒>` `eval:<JS>` | 等一会儿；执行一段 JS 并打印结果 |
| `panel:<步骤>` | 在打开着的插件面板（它自己的框架）里做 `click` `rclick` `dblclick` `hover` `set` `type` `key` `eval` `wait`（元素只能用 CSS 选择器或 `text=`） |

```bash
spaceknife plugin shot . --setting min_size=1 --history old.skscan --step "rclick:text=a.log" --step "wait:0.3" -o menu.png
spaceknife plugin shot . --open panel:main --step "panel:set:#days=30" --step "panel:click:text=应用" --eval "document.title"
spaceknife plugin shot . --source list:fixtures/list.txt --step "select:docs/a.txt" --text
spaceknife plugin shot . --do visual:pack --step "dblclick:node=photos" --step "dblclick:node=" --step "rclick:node=code" -o menu.png
spaceknife plugin shot . --open settings --lang en --step "click:input[aria-label=Name]" --step "key:Control+A" --step "type:Old logs" --step "key:Tab"
```

设置页的输入框以设置的标题（列表设置里是栏的标题，按界面语言）为 `aria-label`。

`--eval` 在所有步骤之后、截图之前执行（读到的就是图里的状态）。脚本（`--eval`、`eval:`）抛出时打印 JS 的错误（`ReferenceError: foo is not defined`），照样截图。有一步失败或 `--eval` 出错时退出码是 3（截图仍在），选项写错是 2。

`--setting 名=值` 先改好本插件的设置：值按 JSON 读（`3`、`true`、`["a", "b"]`、`{"k": 1}`），读不成时是文字；`名=@文件.json` 从文件读（最不怕引号）。宿主不收的值（列表设置给了文字、超出范围……）在开始前就说哪一项、为什么。Windows PowerShell 5.1 不转义参数里的双引号（引号外有空格的参数整个套上双引号，其余照写），程序收到的 JSON 和 JS 会丢掉引号甚至断成两个参数；`spaceknife plugin …` 在 Windows 上按 PowerShell 5.1 的写法从命令行原文把它们读回来，所以 `--setting 'rules=[{"glob": "a b"}]'`、`--step 'eval:f("a b")'` 照写就行（`plugin call` 的参数也是，或写 `@params.json`）。经 `uv run` 时读不回来：uv 自己先按 C 运行库的规矩读了参数，再原样转给 Python；这时直接运行 `.venv\Scripts\spaceknife.exe`，或用 `@文件.json`，或把里面的双引号写成 `\"`。

`--history 文件` 把快照放进临时后端的历史（对比和 `ctx.snapshots.list()` 都看得到），`--source 名:URI` 用本插件的扫描来源扫描（代替 `--scan`）。浏览器里的界面没有系统的保存对话框，导出时在页面的路径框里填：`set:.path-dialog-input=<路径>`。

<a id="api-core"></a>
## 11. `ctx.call` 与 `sk.core` 能调的内置方法

参数和结果的完整字段见 [protocol.md](protocol.md) 和 `spaceknife schema`。删除、改设置、开始扫描、安装插件永远调不到。常用的几个：`scan.list` → `{scans: [ScanStatus]}`（`scan_id state roots items files dirs bytes_alloc …`，打开的顺序）；`view.list` → `{views: [ViewState]}`（`view_id scan_id zoom_node selected visual color_mode …`：面板（`sk.core`）里是这个窗口的视图，插件代码（`ctx.call`）里是空的，要视图所在的位置用调用带的 `view_id` 和 `node`）；`tree.summary` → 一个节点的合计和最大的子项；`snapshot.list` 同 `ctx.snapshots.list()`。

`filter.parse` → `FilterParse`（dict）：`diagnostics`（每项 `code` `severity`（`error` 时这个过滤式不生效，`warning` 只是提醒）`start` `end`（出错那一段，UTF-16 下标）`text`（那一段原文）`expected`（这里能写的）；**没有给人看的文字**，要显示时自己按 `code` 写）、`normal`（有错时是 None；`normal["canonical"]` 是唯一的规范写法，两个过滤式 `canonical` 相同就命中同样的文件；`masks` `classes` `folders` `sizes` `ages` `sets` 和对应的 `not_*` 是拆开的条件）、`conditions`（逐个条件：`kind` 是 `name` `folder` `class` `set` `tag` `attr` `size` `age`，`text` 是它的规范写法，`negated`）、`tokens`（上色用的 `kind start end`）、`pending`（光标处没写完的条件）、`completion`（光标处的补全）。只想知道写得对不对：`not any(d["severity"] == "error" for d in r["diagnostics"])`。

<!-- generated:plugin-core:start -->

| 方法 | 权限 | 参数 | 结果 |
|---|---|---|---|
| `basket.add` | `basket.propose` | `scan_id` `nodes` `view_id` `key` `origin` `reasons` | `BasketAddResult` |
| `basket.list` | — | — | `BasketState` |
| `classes.get` | — | — | `ClassesState` |
| `compare.info` | `tree.read` | `compare_id` | `CompareInfo` |
| `compare.node` | `tree.read` | `view_id` `key` `children` | `CompareNodeResult` |
| `compare.summary` | `tree.read` | `view_id` `limit` | `CompareSummary` |
| `export.targets` | — | — | `ExportTargets` |
| `extensions.get` | — | — | `ExtensionsState` |
| `filter.parse` | — | `text` `cursor` | `FilterParse` |
| `filter.vocab` | — | — | `FilterVocab` |
| `insight.node` | `tree.read` | `scan_id` `node` | `InsightNodeResult` |
| `insight.nodes` | `tree.read` | `scan_id` `nodes` | `InsightNodesResult` |
| `scan.list` | — | — | `ScanListResult` |
| `scan.status` | — | `scan_id` | `ScanStatus` |
| `snapshot.list` | `snapshots.read` | — | `SnapshotListResult` |
| `tree.children` | `tree.read` | `scan_id` `node` `sort` `offset` `limit` | `TreeChildrenResult` |
| `tree.find` | `tree.read` | `scan_id` `path` | `TreeFindResult` |
| `tree.node` | `tree.read` | `scan_id` `node` | `TreeNodeResult` |
| `tree.summary` | `tree.read` | `scan_id` `node` `top` | `TreeSummaryResult` |
| `view.list` | — | `scan_id` | `ViewListResult` |
| `visual.list` | — | — | `VisualListResult` |
| `volume.list` | — | — | `VolumeListResult` |

<!-- generated:plugin-core:end -->

<a id="api-commands"></a>
## 12. 程序命令的 id

命令动作 `{ command = "…" }`、按键 `[[keys]] command = "…"`、`builtin` 动作都用这些 id（生成）。插件的命令是 `<插件 id>.<名字>`；可视化和配色的切换命令是 `view.visual.<id>`、`view.color.<id>`（插件的也一样）。

<!-- generated:plugin-commands:start -->

| 命令 id | 名称 | Name |
|---|---|---|
| `app.about` | 关于与更新 | About & updates |
| `app.check_updates` | 检查更新 | Check for updates |
| `app.disks` | 本机磁盘 | Disks |
| `app.help` | 帮助 | Help |
| `app.log` | 日志面板 | Log panel |
| `app.settings` | 设置 | Settings |
| `app.show_update` | 查看更新 | Show update |
| `basket.add` | 加入删除篮 | Add to delete basket |
| `basket.open` | 打开删除篮 | Open delete basket |
| `filter.focus` | 聚焦过滤 | Focus filter |
| `item.copy_path` | 复制路径 | Copy path |
| `item.preview` | 预览 | Preview |
| `item.reveal` | 在资源管理器中显示（Windows） / 在访达中显示（macOS） / 在文件管理器中显示（Linux） | Show in Explorer（Windows） / Show in Finder（macOS） / Show in file manager（Linux） |
| `item.terminal` | 在此打开终端 | Open terminal here |
| `nav.back` | 后退 | Back |
| `nav.forward` | 前进 | Forward |
| `nav.home` | 根 | Root |
| `nav.up` | 上一级 | Up one level |
| `path.edit` | 编辑路径 | Edit path |
| `report.export` | 导出报告… | Export report… |
| `scan.continue` | 继续已停止的扫描 | Continue the stopped scan |
| `scan.new` | 新扫描 | New scan |
| `scan.pause` | 暂停扫描 | Pause scan |
| `scan.rescan_all` | 全盘重新扫描 | Rescan everything |
| `scan.resume` | 继续扫描 | Resume scan |
| `scan.stop` | 停止扫描 | Stop scan |
| `snapshot.compare` | 与快照对比… | Compare with snapshot… |
| `snapshot.open` | 打开快照文件… | Open snapshot file… |
| `snapshot.save` | 保存快照 | Save snapshot |
| `tab.close` | 关闭标签 | Close tab |
| `tab.move_left` | 标签左移 | Move tab left |
| `tab.move_right` | 标签右移 | Move tab right |
| `tab.new` | 新建标签 | New tab |
| `tag.blue` | 蓝色标签 | Blue tag |
| `tag.clear` | 清除标签 | Clear tag |
| `tag.green` | 绿色标签 | Green tag |
| `tag.red` | 红色标签 | Red tag |
| `tag.yellow` | 黄色标签 | Yellow tag |
| `view.color.branch` | 配色：顶层分支 | Coloring: top-level branch |
| `view.color.class` | 配色：文件类别 | Coloring: file classes |
| `view.color.flat` | 配色：扁平 | Coloring: flat |
| `view.color.time` | 配色：修改时间 | Coloring: date modified |
| `view.cycle_color` | 切换配色 | Cycle coloring |
| `view.cycle_visual` | 切换可视化 | Cycle visualization |
| `view.detail_down` | 降低细节层级 | Lower detail level |
| `view.detail_up` | 提高细节层级 | Raise detail level |
| `view.rescan` | 重新扫描当前视图 | Rescan current view |
| `view.split` | 分屏 | Split view |
| `view.toggle_free` | 显示/隐藏可用空间 | Show/hide free space |
| `view.toggle_trash` | 显示/隐藏回收站 | Show/hide the Recycle Bin |
| `view.toggle_unknown` | 显示/隐藏未知空间 | Show/hide unknown space |
| `view.visual.flame` | 可视化：火焰图 | Visualization: flame graph |
| `view.visual.flame_polar` | 可视化：火焰-极坐标图 | Visualization: polar flame graph |
| `view.visual.treemap` | 可视化：树图 | Visualization: treemap |
| `view.zoom_in` | 放大进入 | Zoom in |

<!-- generated:plugin-commands:end -->

<a id="api-icons"></a>
## 13. 图标名

`icon = "…"` 能用的程序图标（生成，来自 `design/icons/icons.json`）。别的名字 `plugin check` 报 `icon_name` 并给出最接近的几个；自己的图标放 `icons/<名字>.svg`（[plugin-dev.md 1.4 节](plugin-dev.md#icons)）。

<!-- generated:plugin-icons:start -->

`back` `basket` `bolt` `check` `chevronDown` `chevronRight` `clock` `close` `command` `copy` `diff` `diskGauge` `drive` `eye` `file` `filter` `folder` `forward` `gauge` `globe` `home` `info` `layers` `log` `logo` `minus` `more` `palette` `pause` `play` `plus` `pulse` `refresh` `reveal` `search` `settings` `shield` `sidebar` `snapshot` `split` `stop` `tag` `terminal` `thermometer` `up` `upload` `vizFlame` `vizFlamePolar` `vizTreemap` `warning` `windowClose` `windowMaximize` `windowMinimize`

<!-- generated:plugin-icons:end -->

<a id="api-models"></a>
## 14. 对象的字段

插件拿到和交回的每个对象（生成）。「str | Text」表示可以写 `T("中文", "English")`，界面按当前语言显示。

<!-- generated:plugin-models:start -->

### 上下文

#### `ScanInfo`

`ctx.scans()` 的一项（新的在前）；`scan_done` `scan_closed` 钩子的参数。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `scan_id` | str | 必需 | 扫描的 id：`ctx.scan(scan_id)` 读它的树 |
| `roots` | list[str] | `[]` | 根的路径（扫描来源的扫描：根的名字） |
| `items` | int | `0` | 文件夹和文件的个数，不算根 |
| `source` | str | `"scan"` | `scan`（磁盘扫描）、`snapshot`（打开的快照）或扫描来源的全名 |
| `uri` | str | `""` | 扫描来源列的 URI（用户选的文件或地址）；别的扫描是 `""` |
| `state` | (`scanning` `done` `cancelled` `failed`) \| str | `"done"` | 扫描中、完成、停止、失败 |
| `started_at` | float \| None | `None` | 开始时间，unix 秒 |
| `finished_at` | float \| None | `None` | 结束时间，unix 秒；还在扫描时是 None |
| `snapshot` | str \| None | `None` | 快照：打开的快照是那个文件；磁盘扫描是它完成时自动保存的那个（没有保存时 None） |
| `front` | bool | `False` | 用户眼前的那个扫描（`ctx.tree` 默认读它） |
| `on_disk` | bool | `True` | 节点是这台机器上的文件；扫描来源（`on_disk = false`）的是 False |
| `monitoring` | (`off` `starting` `running` `degraded` `failed`) \| str | `"off"` | 文件变化是否实时跟着（`tree_changed` 钩子）：`off`（实时监控关着、快照、没有监控器的扫描来源）、`starting`、`running`、`degraded`（部分靠轮询或重新扫描）、`failed` |

#### `TreeChange`

`tree_changed` 钩子的参数：一个打开着的扫描的树变了（每个扫描每秒最多一次）。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `scan_id` | str | 必需 | 变了的扫描（钩子里 `ctx.tree` 就是它） |
| `epoch` | int | `0` | `folders` 的节点下标属于这个纪元：钩子的 `ctx.tree` 就是这一纪元的树（[纪元](#api-epoch)） |
| `version` | int | `0` | 变化之后树的版本（`ctx.tree.version`） |
| `folders` | list[int] | `[]` | 发生变化的文件夹（变了的文件所在的、变了的文件夹和装着它的），最多 256 个；扫描来源重新列出时是根 |
| `paths` | list[str] | `[]` | `folders` 里每个文件夹的路径（顺序相同） |
| `before` | list[int \| None] | `[]` | 每个文件夹连同里面的占用（字节，即 `Node.alloc`）在这些变化之前是多少；不知道时是 None（整棵树换掉了，比如扫描来源重新列出） |
| `after` | list[int] | `[]` | 每个文件夹连同里面的占用在这些变化之后是多少（和 `ctx.tree` 里的一样） |
| `more` | int | `0` | 大约还有多少个文件夹变了而没有列出 |
| `changes` | int | `0` | 这一秒里变了、新增或消失的文件和文件夹数 |

#### `SnapshotsChange`

`snapshots` 钩子的参数（要 `snapshots.read`）：存了或删了快照。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `saved` | list[str] | `[]` | 写下的快照文件 |
| `removed` | list[str] | `[]` | 删掉的快照文件（自动快照超过保留个数时最旧的） |

#### `SnapshotInfo`

`ctx.snapshots.list()` 的一项（新的在前）。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `path` | str | 必需 | 快照文件；`ctx.snapshots.open(path)` 打开它 |
| `created_at` | datetime | 必需 | 保存的时间（UTC） |
| `roots` | list[str] | `[]` | 扫描的根 |
| `node_count` | int | `0` | 节点数（含虚拟的 0 号节点和根） |
| `bytes_alloc` | int | `0` | 占用合计（字节） |
| `files` | int | `0` | 文件数 |
| `file_bytes` | int | `0` | 快照文件本身的大小 |
| `method` | str \| None | `None` | 怎么读的：`hard`（逐个文件夹）`mft`（NTFS 快速扫描） |

#### `NodeRef`

带路径的节点引用：`tree.ref(id)`；报告、字段、动作里的节点。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `scan_id` | str | 必需 | 扫描 |
| `node` | int | 必需 | 节点下标 |
| `epoch` | int | `0` | 取下标时树的纪元：和 `tree.epoch` 不同时，宿主按 `path` 重新找 |
| `path` | str | `""` | 节点的路径（系统自己的写法）；空时只能按下标找 |

#### `Node`

`tree.node(id)`；字段、洞察函数的 `node` 参数。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `id` | int | 必需 | 节点下标（0 是虚拟的顶层节点，根从 1 起） |
| `name` | str | 必需 | 名字 |
| `path` | str | 必需 | 完整路径，系统自己的分隔符（Windows 是 `\`）；只要相对路径用 `tree.rel_path` |
| `kind` | Kind | 必需 | `Kind.FILE` `DIR` `LINK` `OTHER` |
| `size` | int | 必需 | 逻辑大小；文件夹是子树合计 |
| `alloc` | int | 必需 | 占用；文件夹是子树合计 |
| `files` | int | 必需 | 子树里的文件数（文件自己是 1） |
| `dirs` | int | 必需 | 子树里的文件夹数 |
| `mtime` | int | 必需 | 修改时间，unix 秒（0：不知道） |
| `ctime` | int | 必需 | 创建时间（Linux 是状态改变时间） |
| `atime` | int | 必需 | 访问时间 |
| `ext` | str | 必需 | 扩展名，小写，不带点 |
| `parent` | int | 必需 | 父节点下标（0 号节点是 −1） |
| `depth` | int | 必需 | 层级：0 号节点是 0，根是 1，根下面一层是 2 |
| `child_count` | int | 必需 | 直接子项的个数 |
| `flags` | Flag | 必需 | `Flag` 的组合：`HIDDEN` `HARDLINK_DUP` `PENDING` … |
| `cls` | str | 必需 | 文件类别的 id（`video`、插件的 `<id>.<名字>`）；没有类别是空 |

#### `ReportContext`

报告、清理的第二个参数：调用发生在哪。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `scan_id` | str \| None | `None` | 所在的扫描（`ctx.tree` 读它） |
| `view_id` | str \| None | `None` | 所在的视图（有的话） |
| `node` | int \| None | `None` | 针对的节点：详情面板里是选中项（没选时是缩放根），右键菜单里是点的那一项 |
| `selection` | list[int] | `[]` | 选中的节点（≤ 1000 个） |
| `compare_id` | str \| None | `None` | 对比视图里的对比 |
| `cursor` | str \| None | `None` | 表格续读时 `r.more(cursor)` 给的位置；第一页是 None |
| `args` | dict[str, Any] | `{}` | 打开它的动作带的参数（`{ open = "report:x", args = {…} }`） |
| `zoom_node` | int \| None | `None` | 视图的缩放根（用户眼前的那一层）；没有视图时 None |
| `filter` | str | `""` | 视图的过滤式；没过滤是 `""` |

#### `ActionContext`

`call` 动作调的方法收到的参数。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `scan_id` | str \| None | `None` | 所在的扫描 |
| `view_id` | str \| None | `None` | 所在的视图 |
| `node` | int \| None | `None` | 针对的节点（右键菜单：点的那一项；其他地方：选中项） |
| `selection` | list[int] | `[]` | 选中的节点（≤ 1000 个） |
| `compare_id` | str \| None | `None` | 对比 |
| `args` | dict[str, Any] | `{}` | 动作里写的 `args` |
| `zoom_node` | int \| None | `None` | 视图的缩放根（用户眼前的那一层）；没有视图时 None |
| `filter` | str | `""` | 视图的过滤式；没过滤是 `""` |

#### `TaskInfo`

后台任务：`ctx.tasks.start()` 的结果、`ctx.tasks.list()` 的一项、测试里 `host.task()` 的结果。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `id` | str | 必需 | 这次后端运行里唯一的 id |
| `plugin` | str | 必需 | 插件 id |
| `name` | str | 必需 | 任务的名字（`@plugin.background` 的第一个参数） |
| `key` | str | `""` | 同名任务里的哪一个（`ctx.tasks.start(…, key=)`：一个扫描、一个文件夹……） |
| `title` | str | 必需 | 任务的标题，界面语言 |
| `state` | (`running` `restarting` `done` `failed` `cancelled`) | 必需 | `running` `restarting`（插件崩溃后重新开始）`done` `failed` `cancelled` |
| `done` | float | `0.0` | 最近一次 `ctx.progress` 的 `done` |
| `total` | float | `0.0` | 最近一次 `ctx.progress` 的 `total`；0：说不出进度 |
| `text` | str | `""` | 最近一次 `ctx.progress` 的文字 |
| `started` | float | 必需 | 开始时间，unix 秒 |
| `ended` | float \| None | `None` | 结束时间，unix 秒；还在跑时 None |
| `attempt` | int | `1` | 第几次跑（崩溃后重新开始时加一） |
| `error` | str | `""` | 失败的原因（超时、插件的 `PluginError` 文字……） |

### 输入

#### `VisualInput`

代码可视化 `(v) -> 权重数组`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `tree` | TreeView | 必需 | 这一帧所在的树 |
| `options` | dict[str, Any] | `{}` | 声明的 `options`（本插件的设置名 → 视图里的值） |
| `ctx` | Context \| None | `None` | 调用的上下文：`v.settings` `v.cancelled` `v.check()` 也有 |

#### `ColorInput`

代码配色 `(c) -> 下标数组`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `tree` | TreeView | 必需 | 树 |
| `ids` | ndarray | 必需 | 这一帧画出的节点（int64 数组） |
| `palette` | int | `1` | 色板长度：返回的下标 < 它 |
| `ctx` | Context \| None | `None` | 调用的上下文 |

#### `LayoutInput`

自带布局的可视化 `(v) -> Shapes`：宿主挑出的块（父块在前）和画布。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `node` | ndarray | 必需 | 每块的节点（int64 数组）；「N 个小项」「+N 项」是它们所在的文件夹，可用和未知空间是 −1 |
| `parent` | ndarray | 必需 | 每块所在的块的下标；−1：直接在缩放根上 |
| `kind` | ndarray | 必需 | 每块是什么（`BlockKind`）：`FILE` `FOLDER`（它的块跟在后面）`COLLAPSED` `PENDING` `SMALL` `MORE` `FREE` `UNKNOWN` |
| `depth` | ndarray | 必需 | 层级：1 是缩放根的子项 |
| `size` | ndarray | 必需 | 面积代表的量：占用字节，或可视化 `size` 说的那种 |
| `count` | ndarray | 必需 | 「N 个小项」「+N 项」的成员数 |
| `u0` | ndarray | 必需 | 在缩放根 [0, 1) 里的区间起点：同级按顺序、按大小分（冰柱图的 x） |
| `u1` | ndarray | 必需 | 区间终点 |
| `width` | int | `0` | 画布宽，物理像素 |
| `height` | int | `0` | 画布高，物理像素 |
| `dpr` | float | `1.0` | 每个 CSS 像素几个物理像素 |
| `font_scale` | float | `1.0` | 「元素字号」 |
| `min_element` | float | `14.0` | 值得画的最小块，物理像素 |
| `levels` | int | `0` | 块最深到第几层 |
| `root` | int | `0` | 缩放根的节点 |
| `root_kind` | int | `1` | 缩放根是什么（`BlockKind`） |
| `root_size` | int | `0` | 缩放根的大小（同 `size`） |
| `options` | dict[str, Any] | `{}` | 声明的 `options`（本插件的设置名 → 当前值） |
| `ctx` | Context \| None | `None` | 调用的上下文：`ctx.check()`；有 `tree.read` 时 `v.tree` 读树 |
| `label` | LabelSpace | 算出 | 宿主的标签占多大地方（`LabelSpace`，物理像素），按 `dpr` × `font_scale` 算出 |

#### `LabelSpace`

`v.label`：宿主的标签占多大地方（物理像素，随 `dpr` 和「元素字号」），想让名字露出来就把它空出来。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `font` | float | 必需 | 名字的字号（11 lu；大标签 13，只写一行时 10.5） |
| `line` | float | 必需 | 形状中间写一行名字要的高度（18 lu；那里的宽度还要 ≥ 30 lu，形状 ≥ 15 lu 高） |
| `lines` | float | 必需 | 形状中间写名字和大小两行要的高度（34 lu；那里的宽度还要 ≥ 42 lu） |
| `top` | float | 必需 | 顶部标签从形状上沿往下占到哪里（4 + 15 lu；那一条的宽度还要 ≥ 40 lu，形状 ≥ 60 lu 高）：嵌套的文件夹比外层的低这么多，两个名字才都写 |

#### `ExportInput`

导出格式 `(x) -> Iterator[str]`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `tree` | TreeView | 必需 | 树 |
| `root` | int | 必需 | 要导出的子树：视图所在的位置、选中的项，或 0（全部） |
| `match` | ndarray \| None | `None` | 过滤后的布尔掩码（按节点下标）：命中的**文件**是 True；文件夹永远是 False（文件夹本身不命中，看它下面的文件）；只导出选中项时只留选中的子树；没过滤时是 None |
| `filter` | str | `""` | 过滤式；没过滤时是空 |
| `options` | dict[str, Any] | `{}` | 导出对话框里选的值（没给的取默认；`spaceknife run` 用默认） |
| `selection` | list[int] | `[]` | 选中的节点（「选中项」范围） |
| `ctx` | Context \| None | `None` | 调用的上下文 |

#### `ExportInfo`

导出格式旧写法 `(tree, root, match, info)` 的第四个参数。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `filter` | str | `""` | 过滤式 |
| `options` | dict[str, Any] | `{}` | 选项 |

#### `ChangeUnit`

对比原因 `(ctx, items)` 的一项。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `path` | str | 必需 | 变化项的路径 |
| `kind` | str | `"dir"` | `file` 或 `dir` |
| `status` | str | `"changed"` | `added` `removed` `changed` … |
| `delta` | int | `0` | 大小的变化（字节，可以是负的） |
| `base` | int | `0` | 旧快照里的大小 |
| `target` | int | `0` | 新快照里的大小 |

### 返回

#### `Report`

报告。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`report`) | `"report"` |  |
| `title` | str | `""` | 标题（标签页、对话框显示在外面） |
| `subtitle` | str | `""` | 标题下的一行 |
| `blocks` | list[MdBlock \| StatsBlock \| KvBlock \| TableBlock \| BarsBlock \| StackBlock \| RingBlock \| MeterBlock \| SparkBlock \| NodesBlock \| CalloutBlock \| ProgressBlock \| SectionBlock] | `[]` | 块（`r.*` 构造） |
| `actions` | list[LabeledAction] | `[]` | 报告底部的按钮 |
| `footnote` | str | `""` | 末尾的小字 |
| `empty` | str | `""` | 没有块时显示它 |

#### `Toast`

命令或方法返回它时显示一条提示。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`toast`) | `"toast"` |  |
| `text` | str | 必需 | 文字（≤ 512 字） |
| `tone` | (`info` `ok` `warn` `danger`) | `"info"` | 色调 |

#### `CompareOpen`

命令的方法返回它时打开对比（要 `snapshots.read`）。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`compare`) | `"compare"` |  |
| `base` | str | 必需 | 基准：快照文件，或 `now`（同一组文件夹的实时树）（≤ 32767 字） |
| `target` | str | `"now"` | 对比目标：同上（默认 `now`）（≤ 32767 字） |
| `scan_id` | str \| None | `None` | 在哪个扫描的标签页里对比；不写时用显示那组文件夹的标签页（≤ 64 字） |

#### `FieldValue`

节点字段（也可以只返回文字）。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`field`) | `"field"` |  |
| `value` | str \| int \| float \| bool \| None | 必需 | 值（文字 ≤ 200 字） |
| `unit` | (`size` `count` `pct` `date` `ago` `duration` `bytes_per_s` `text` `path`) | `"text"` | 怎么格式化：`size` 写成 1.2 MB，`count` 加千分位，`ago` 写成多久以前 … |
| `tone` | str \| ThemedColor \| None | `None` | 颜色（1.5 节的写法，或 `palette:<配色全名>#<下标>`） |
| `node` | NodeRef \| None | `None` | 点击时定位到它 |
| `action` | LabeledAction \| None | `None` | 旁边的一个按钮 |

#### `Insight`

代码洞察。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `title` | str | 必需 | 卡片标题：这是什么 |
| `reason` | str | 必需 | 为什么这么说 |
| `kind` | str | `"other"` | 卡片的图标：`app` `cache` `build` `vm` `chat` `game` `system`（同规则文件的 `kind`）；别的值（默认 `other`）显示成一般的信息图标 |
| `cleanup` | (`safe` `tool` `review` `keep`) | `"review"` | 能不能删：`safe`（随便删）`tool`（用工具清）`review`（先看看）`keep`（别删） |
| `app` | str | `""` | 相关的程序 |
| `note` | str | `""` | 补充说明 |
| `command` | str | `""` | 清理命令：只显示、可复制，从不运行 |
| `priority` | int | `0` | 详情面板里卡片按它从高到低排（规则的卡片用规则的 `priority`）（≥ -1000，≤ 1000） |
| `replaces` | list[str] | `[]` | Ids of insight rules whose card on this node it replaces ('*': any rule's)（≤ 32 个） |
| `regenerate` | str | `""` | How it comes back once deleted: 「npm install 会装回来」（≤ 300 字） |
| `node` | int \| NodeRef \| None | `None` | The folder the card is about when it is not the node asked (a file inside a project: the project): the card names it and 「加入删除篮」 puts it in; it must be the node or one of its folders |

#### `Proposal`

清理的一个候选。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `node` | int \| NodeRef \| str | 必需 | 节点下标、`NodeRef` 或绝对路径 |
| `reason` | str | `""` | 复核对话框里每一项的原因（≤ 120 字） |
| `checked` | bool | `True` | 默认勾选 |

#### `Change`

监控器产出的一项变化。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `path` | str | 必需 | 相对来源根的路径，用 `/`；空表示整个来源 |
| `kind` | (`created` `modified` `deleted` `overflow`) | `"modified"` | 变化的种类（`overflow`：太多了，整个重跑） |

### 报告的部件

#### `TableColumn`

`r.col(key, 标题, 单位)`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `key` | str | 必需 | 行里的键 |
| `label` | str | 必需 | 列标题 |
| `unit` | (`size` `count` `pct` `date` `ago` `duration` `bytes_per_s` `text` `path`) | `"text"` | 单位（格式化和排序方式） |
| `align` | (`start` `end` `center`) \| None | `None` | 对齐（数字默认靠右） |
| `grow` | float \| None | `None` | 宽度份数 |

#### `TableMore`

`r.more(cursor)`：表格的下一页。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `method` | str | 必需 | 再调哪个报告（线上名 `plugin.<id>.<报告名>`；`r.more` 自动填） |
| `cursor` | str | 必需 | 交给 `c.cursor` 的位置 |

#### `StatItem`

`r.stats` 的一项。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `label` | str | 必需 |  |
| `value` | str \| int \| float \| bool \| None | 必需 |  |
| `unit` | (`size` `count` `pct` `date` `ago` `duration` `bytes_per_s` `text` `path`) | `"count"` |  |
| `hint` | str | `""` | 悬停提示 |
| `tone` | str \| ThemedColor \| None | `None` |  |
| `delta` | float \| None | `None` | 变化量（对比报告里显示成 +/−） |

#### `BarItem`

`r.bars` 的一项（元组、dict 或它）。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `label` | str | 必需 |  |
| `value` | float | 必需 |  |
| `unit` | (`size` `count` `pct` `date` `ago` `duration` `bytes_per_s` `text` `path`) | `"size"` |  |
| `node` | NodeRef \| None | `None` | 点击时定位 |
| `tone` | str \| ThemedColor \| None | `None` |  |

#### `PartItem`

`r.stack` / `r.ring` 的一段。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `label` | str | 必需 |  |
| `value` | float | 必需 |  |
| `tone` | str \| ThemedColor \| None | `None` |  |

#### `LabeledAction`

`r.action(标签, icon, **动作)`：按钮。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `label` | str | 必需 |  |
| `icon` | str | `""` |  |
| `action` | CallAction \| OpenAction \| CommandAction \| ViewAction \| CycleAction \| ToggleAction \| FilterAction \| SettingAction \| CleanupAction \| NodeAction \| UrlAction \| ScanSourceAction \| BuiltinAction | 必需 | 动作，写法同命令的 `action` |

#### `ThemedColor`

分主题的颜色 `{ light, dark }`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `light` | str | 必需 | 浅色主题里的颜色 `#RRGGBB` |
| `dark` | str | 必需 | 深色主题里的颜色 |

### 报告的块

#### `MdBlock`

`r.md(text)`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`md`) | `"md"` |  |
| `text` | str | 必需 |  |

#### `StatsBlock`

`r.stats([...])`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`stats`) | `"stats"` |  |
| `items` | list[StatItem] | 必需 |  |

#### `KvBlock`

`r.kv([...])`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`kv`) | `"kv"` |  |
| `items` | list[KvItem] | 必需 |  |

#### `TableBlock`

`r.table(columns, rows=…)`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`table`) | `"table"` |  |
| `columns` | list[TableColumn] | 必需 |  |
| `rows` | list[TableRow] | 必需 |  |
| `sort` | str \| None | `None` | 按哪一列排（列的 key），`-key` 是从大到小 |
| `page_size` | int | `50` | ≥ 1，≤ 500 |
| `select` | bool | `False` |  |
| `more` | TableMore \| None | `None` |  |

#### `BarsBlock`

`r.bars([...])`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`bars`) | `"bars"` |  |
| `items` | list[BarItem] | 必需 |  |
| `max` | float \| None | `None` |  |

#### `StackBlock`

`r.stack(parts, unit)`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`stack`) | `"stack"` |  |
| `parts` | list[PartItem] | 必需 |  |
| `unit` | (`size` `count` `pct` `date` `ago` `duration` `bytes_per_s` `text` `path`) | `"size"` |  |

#### `RingBlock`

`r.ring(parts, unit, center)`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`ring`) | `"ring"` |  |
| `parts` | list[PartItem] | 必需 |  |
| `unit` | (`size` `count` `pct` `date` `ago` `duration` `bytes_per_s` `text` `path`) | `"size"` |  |
| `center` | str | `""` |  |

#### `MeterBlock`

`r.meter(value, max, label, unit, tone)`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`meter`) | `"meter"` |  |
| `value` | float | 必需 |  |
| `max` | float | 必需 |  |
| `label` | str | `""` |  |
| `unit` | (`size` `count` `pct` `date` `ago` `duration` `bytes_per_s` `text` `path`) | `"size"` |  |
| `tone` | str \| ThemedColor \| None | `None` |  |

#### `SparkBlock`

`r.spark(values, labels, unit)`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`spark`) | `"spark"` |  |
| `values` | list[float] | 必需 |  |
| `labels` | list[str] | `[]` |  |
| `unit` | (`size` `count` `pct` `date` `ago` `duration` `bytes_per_s` `text` `path`) | `"size"` |  |

#### `NodesBlock`

`r.nodes([...], actions)`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`nodes`) | `"nodes"` |  |
| `items` | list[NodeItem] | 必需 |  |
| `actions` | list[LabeledAction] | `[]` |  |

#### `CalloutBlock`

`r.callout(text, tone)`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`callout`) | `"callout"` |  |
| `tone` | (`info` `ok` `warn` `danger`) | `"info"` |  |
| `text` | str | 必需 |  |

#### `ProgressBlock`

`r.progress(done, total, text)`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`progress`) | `"progress"` |  |
| `done` | float | 必需 |  |
| `total` | float | 必需 |  |
| `text` | str | `""` |  |

#### `SectionBlock`

`r.section(title, blocks, collapsed=False)`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `type` | (`section`) | `"section"` |  |
| `title` | str | 必需 |  |
| `blocks` | list[MdBlock \| StatsBlock \| KvBlock \| TableBlock \| BarsBlock \| StackBlock \| RingBlock \| MeterBlock \| SparkBlock \| NodesBlock \| CalloutBlock \| ProgressBlock \| SectionBlock] | 必需 |  |
| `collapsed` | bool | `False` |  |

<!-- generated:plugin-models:end -->
