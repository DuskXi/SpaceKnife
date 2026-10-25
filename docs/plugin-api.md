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
| `ctx.tree` | 调用带的扫描（`c.scan_id`），否则用户眼前的扫描；都没有时抛 `PluginError`「需要先完成扫描」 |
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
| `epoch` | 树的纪元：宿主整理树（压缩、监控重新列出）后加一，旧下标随之作废（`STALE_TREE`） |
| `version` | 树的版本：任何改动都加一。扫描进行中每批结果都会让它变（很频繁）；扫描完成后只在监控、重新扫描、删除改动树时变。宿主的缓存按它作废 |

**方法**：`node(id)` `nodes(ids)` → [`Node`](#api-models)；`name(id)` `names(ids)`；`path(id)` `paths(ids)`；`rel_path(id, under=None)` `rel_paths(ids, under=None)`；`root_of(id)`；`parent(id)`；`children(id)` → 下标数组；`walk(id=None, chunk=…)` → 一块块下标（子树先序）；`col(name)` `cols(*names)` → 只读的整列；`query(…)` → `Rows`；`find(path)` → 下标或 None；`ref(id)` → [`NodeRef`](#api-models)；`file_ids(ids)` → `(设备, 文件 id, 链接数)`（要 `files.read`）；`close()`（快照用完时）。

**层级**：0 号节点 `depth = 0`，根 `depth = 1`，根下面一层 `2`。`Node.parent` 对 0 号节点是 −1，对根是 0。根的 `name` 是它的完整路径。

**边界**：`query(under=X)` 是 X 下面的项，**不含 X**；不写 `under` 时含各个根（不含 0 号节点）。文件的 `tot_*` 列是 0；`bytes` 列对文件是占用、对文件夹是子树合计、对重复的硬链接是 0。

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

**硬链接**：扫描只记下它见到的额外链接（`Flag.HARDLINK_DUP`，`bytes` 列为 0）。Windows 的普通扫描不给 `ino` 和 `nlink`，要判断两个文件是不是同一个时用 `tree.file_ids(ids)`。

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
| 命令：详情面板（`inspector`）、工具栏（`toolbar`）、命令面板、按键 | 有焦点的视图的扫描 | 有焦点的视图 | 选中项；没选时是 None | `[node]` 或 `[]` |
| 命令：开始页（`start`） | 眼前的扫描或 None | — | None | `[]` |

聚合块（「N 个小项」「+N」）不是节点：选中它时报告的 `node` 是缩放根，命令的是 None。`compare_id` 只在对比时有值。`args` 是打开它的动作写的参数。有视图时还有 `zoom_node`（视图的缩放根，用户眼前的那一层）和 `filter`（视图的过滤式，没过滤是 `""`）：`ctx.tree.query(c.filter or None, under=c.zoom_node, …)` 就是视图里看到的那些文件。命令的 `check` 方法收到同样的 `ActionContext`。

导出格式收到的 `ExportInput`：`root` 是要导出的节点（导出对话框选的「当前位置」是缩放根、「选中项」是选中的节点、「整个扫描」是 0）；`match` 是过滤命中的布尔数组（下标是节点，None 表示没过滤）；`filter` 是那个过滤式；`selection` 是选中的节点；`options` 是对话框里这个格式的选项。

<a id="api-hooks"></a>
## 4. 钩子收到什么

| `event` | 函数签名 | 什么时候 |
|---|---|---|
| `start` | `async def f(ctx) -> None` | 插件进程启动、读完声明之后 |
| `stop` | `async def f(ctx) -> None` | 进程停止之前（2 秒内做完） |
| `scan_done` | `async def f(ctx, scan: ScanInfo) -> None` | 一个扫描完成（`scan.state` 是 `done`） |
| `scan_closed` | `async def f(ctx, scan: ScanInfo) -> None` | 一个扫描被关掉（它的树已经读不到了） |
| `settings` | `async def f(ctx, changed: list[str]) -> None` | 本插件的设置变了：`changed` 是变了的局部名，如 `["min_size"]`；`ctx.settings` 已是新值 |
| `lang` | `async def f(ctx) -> None` | 界面语言变了：`ctx.lang` 已是新的 |
| `basket` | `async def f(ctx) -> None` | 删除篮变了（用户拿出了项、删掉了它们……）：本插件放进去的可能已经不在了，`ctx.call("basket.list")` 看现在的 |

钩子在后台跑，宿主不等结果，出错只记日志；钩子里不能 `ctx.clipboard`，`ctx.toast` 发给所有窗口。测试里 `await host.hook(event, …)` 会检查事件名和参数，钩子抛的异常原样抛出。

<a id="api-errors"></a>
## 5. 异常

| 插件代码里 | 什么时候 | 用户和调用方看到 |
|---|---|---|
| `PluginError(T(...))` | 插件自己要告诉用户的话 | 原样显示（`PLUGIN_ERROR`） |
| `Cancelled` | `ctx.check()`：调用被取消或超时 | 不要吞掉，让它抛出去 |
| `PermissionError` | 少了权限：`ctx.files`、`ctx.net`、`ctx.clipboard` …；审计钩子拦下的联网、写文件、开进程。`ctx.files` 读范围外的文件是它的子类 `FileNotAllowed`；系统拒绝（用户自己也读不了）是普通的 `PermissionError` | `PERMISSION_DENIED`。**它是 `OSError` 的子类**：`except OSError:` 会把它一起吞掉，读文件时写 `except (FileNotFoundError, IsADirectoryError):` 这样具体的异常 |
| `spaceknife_sdk.errors.HostError` | 宿主拒绝了插件的请求：`code` 是协议错误码（如 `STALE_TREE` −32018） | 不捕获时原样往外传 |
| 其他异常 | 插件的 bug | 「插件出错：〈类型〉」，回溯进日志 |

测试和命令行里失败的调用抛 `spaceknife_sdk.RequestError`：`.code`（`spaceknife_sdk.ErrorCode`，如 `PLUGIN_ERROR` `PERMISSION_DENIED`）、`.message`、`.data`。

<a id="api-cache"></a>
## 6. 缓存

宿主替插件记住一些回答，免得每次悬停、每帧都去问插件：

| 什么 | 按什么记 | 什么时候作废 |
|---|---|---|
| 节点字段 | 字段、扫描、节点、树的 `version`、界面语言（详情面板和悬停卡共用一份） | 树变了；插件设置变了；`ctx.invalidate("node_field")`。**返回 None 也会被记住**（这一项不显示，直到作废） |
| 节点集合（`:set:`） | 集合、扫描、树的 `version` | 同上（`"node_set"`）；作废后用到它的过滤自动重算 |
| 代码可视化的权重 | 可视化、扫描、树的 `version`、它的 `options` 和本插件的设置 | 同上（`"visualization"`）；作废后视图重画 |
| 配色的图例（`@plugin.color_legend`） | 配色、语言 | 设置变了、语言变了、`ctx.invalidate("color_mode")`；新的算出来之前仍显示旧的，作废时宿主马上去取新的 |
| 对比原因 | 对比、变化项 | 设置变了（`"change_annotator"`） |
| 报告 | 不缓存：按 `refresh` 重新取 | — |

插件设置一变，宿主就把这个插件的全部缓存作废（插件的「缓存纪元」加一），然后调 `settings` 钩子。插件自己的数据变了（它读的文件、它建的索引）、宿主看不出来的时候，调 `ctx.invalidate()`。字段在后台算完了也一样：`ctx.invalidate("node_field")` 之后界面会再要一次。

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
## 8. 时限与长度

[plugin-dev.md 7.2 节](plugin-dev.md#permissions)是总表。能改的：`@plugin.method(timeout=…)`、`@plugin.report(…, timeout=…)`（默认 30 秒）、`@plugin.cleanup(…, timeout=…)`（默认 120 秒），都 ≤ 600 秒；清单的 `limits.timeout_s` 是没写时的默认。等宿主的调用槽（每个插件同时 4 个）的时间也算在时限里；监控器和流不占调用槽。

被取消的调用（时限到了、用户走开了）：普通 `def` 的函数在工作线程里，宿主立即答「取消」并丢掉它后来的结果；`async def` 的函数要在 `await` 处才停得下来——一直不 `await` 的长循环会卡住整个插件进程，5 秒后日志写出是哪个调用，30 秒后宿主结束进程。所有工作线程都卡在被取消的调用里时，插件进程也会重启。

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
| `frame.draw.meta` | `shapes.v1` 是 `ShapesGeometry`：`root`（缩放根的形状，没有是 None）`nested` `message`（布局失败的原因） |

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

- `i = -1`（`shapes.ROOT`）是缩放根：给了形状就画在最下面，写着名字和大小，悬停、点它（回到上一层）和火焰图的根条一样。
- 块按顺序画，后画的盖住先画的；指针落在最后一个盖住它的形状上。里面一块都没画的文件夹画成折叠的。
- 标签：每个方法都有 `label="auto" | "centre" | "top" | "none"`。`auto`：展开的文件夹在 `nested` 布局里画成矩形时名字写在顶部，画成圆或多边形时不写（里面是它的内容），其余写在中间；放得下两行写名字和大小，否则一行，放不下不写。
- `v.children(i)`：块 i 里的块（−1：缩放根上的）；`v.levels` 层数；`v.min_element` 最小元素（物理像素）；`v.options` 是 `options` 里列出的本插件设置的值。
- `shapes.pack(v, padding=2, margin=8)`：d3 的圆堆积（front-chain 加最小外接圆），确定的；`shapes.pack_siblings(radii)`、`shapes.enclose(circles)` 是它的两步。
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
| `await host.call(name, **params)`、`call_frames` | 方法：JSON（和二进制列） |
| `await host.report(name, node=, cursor=, **ctx)`、`host.text(report)` | 校验过、按上限截过的报告；纯文本 |
| `await host.fields(slot, node=)`、`await host.insights(node=)` | 字段（`"inspector"` 或 `"hover"`）、洞察卡片 |
| `await host.cleanup(name)` | 复核对话框里的候选：`node path reason checked alloc` |
| `await host.node_set(name)`、`await host.filter(expr)` | 集合的掩码；过滤命中的相对路径 |
| `await host.visual(name)`、`await host.color(name)` | 一帧（[第 9 节](#api-frames)） |
| `await host.export(name, filter=, options=, selection=, root=)` | 导出的全文 |
| `await host.source(name, uri)`、`host.status(scan_id)` | 用扫描来源扫描（失败抛 `RuntimeError`）；扫描的状态 |
| `await host.hook(event, **args)`、`await host.annotate(items)` | 钩子（检查事件名和参数，抛出钩子里的异常）、对比原因 |
| `await host.command(name, node=, selection=, zoom_node=, filter=, **args)` | 按菜单那样运行命令：调用它的方法并返回结果（不是 `call` 动作的命令返回动作本身） |
| `await host.command_available(name, node=)` | 选中 `node` 时这个命令能不能跑：它的 `when` 和 `check` 给的原因，能跑时 None |
| `await host.visual_available(name)` | 本插件的可视化在当前扫描的视图里能不能选：它的 `when` 给的原因，能选时 None |
| `await host.settled()` | 等宿主后台的事做完：悬停字段的后补、钩子、设置和语言的变化 |
| `host.path("a/b")` | 节点在界面里的路径（系统的写法，Windows 是 `\`）：和插件写出的路径比较时用它 |
| `await host.export_text(name, …)`、`export_bytes` | 同 `export`，类型确定（文字格式 / 二进制格式） |
| `host.snapshot(when=None)` | 把当前扫描存成快照（同一秒两次也不重名；`when` 是文件名和快照里记的时间），给读历史的插件 |
| `host.events`、`host.state(name)`、`host.toasts`、`host.clipboard`、`host.basket` | 副作用：事件、共享状态、提示、复制的文字、删除篮 |
| `host.commands()`、`host.keys()`、`host.presets()` | 本插件的命令、按键（键 → 命令）、过滤预设 |
| `await host.set_setting(name, value)`、`await host.set_lang("en")` | 改设置（等 `settings` 钩子跑完）、改语言（触发 `lang` 钩子） |
| `await host.check(strict=True)` | 和 `plugin check` 一样的检查 |
| `host.call_sync(name, **params)` | 不写 async 测试时 |

`make_tree({...}, root=None)` 的值：大小、`(大小, 修改时间)`、`bytes`（真的写成文件，可以读和哈希）、`(bytes, 修改时间)`、dict（文件夹）；修改时间是 unix 秒或 `"YYYY-MM-DD"`。默认的根在 `~/.spaceknife-mock/` 下（`SPACEKNIFE_MOCK_TREES` 可改），不在临时文件夹里，免得程序自带的「临时文件夹」规则盖住测试要看的东西；`root=` 指定别的位置。

**测试插件自己的函数**：`from spaceknife_sdk.testing import plugin_module`，`mod = plugin_module(__file__)` 得到插件的 `plugin.py`（按宿主的方式导入，`lib/` 也在路径上；它是另一份副本，看不到宿主里那份的状态，`monkeypatch` 它也影响不到宿主）；不要写 `from plugin import x`，那可能导入别的插件的 `plugin.py`。

**合成扫描**：`started_at` 和 `finished_at` 是 `host.scan()` 的时间；`ScanInfo.snapshot` 对普通磁盘扫描是它自动保存的那个快照（不是「从哪个快照打开的」），快照打开的扫描是那个文件。

**命令行里看历史**：`plugin call` / `plugin render` 的 `--history FILE`（可以写多次）把快照放进临时后端的历史，`ctx.snapshots.list()` 就能看到它们；`--snapshot FILE` 是把一个快照当作当前的扫描。

<a id="api-core"></a>
## 11. `ctx.call` 与 `sk.core` 能调的内置方法

参数和结果的完整字段见 [protocol.md](protocol.md) 和 `spaceknife schema`。删除、改设置、开始扫描、安装插件永远调不到。常用的几个：`scan.list` → `{scans: [ScanStatus]}`（`scan_id state roots items files dirs bytes_alloc …`，打开的顺序）；`view.list` → `{views: [ViewState]}`（`view_id scan_id zoom_node selected visual color_mode …`：面板（`sk.core`）里是这个窗口的视图，插件代码（`ctx.call`）里是空的，要视图所在的位置用调用带的 `view_id` 和 `node`）；`tree.summary` → 一个节点的合计和最大的子项；`snapshot.list` 同 `ctx.snapshots.list()`。

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
| `state` | (`scanning` `done` `cancelled` `failed`) \| str | `"done"` | 扫描中、完成、停止、失败 |
| `started_at` | float \| None | `None` | 开始时间，unix 秒 |
| `finished_at` | float \| None | `None` | 结束时间，unix 秒；还在扫描时是 None |
| `snapshot` | str \| None | `None` | 快照：打开的快照是那个文件；磁盘扫描是它完成时自动保存的那个（没有保存时 None） |
| `front` | bool | `False` | 用户眼前的那个扫描（`ctx.tree` 默认读它） |
| `on_disk` | bool | `True` | 节点是这台机器上的文件；扫描来源（`on_disk = false`）的是 False |

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
| `selection` | list[int] | `[]` | 选中的节点（≤ 1000 个）（≤ 1000 个） |
| `compare_id` | str \| None | `None` | 对比视图里的对比 |
| `cursor` | str \| None | `None` | 表格续读时 `r.more(cursor)` 给的位置；第一页是 None |
| `args` | dict[str, Any] | `{}` | 打开它的动作带的参数（`{ open = "report:x", args = {…} }`） |
| `zoom_node` | int \| None | `None` | The view's zoom root (where the user is) |
| `filter` | str | `""` | The view's filter expression ('' = not filtered) |

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
| `zoom_node` | int \| None | `None` | The view's zoom root (where the user is) |
| `filter` | str | `""` | The view's filter expression ('' = not filtered) |

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

#### `ExportInput`

导出格式 `(x) -> Iterator[str]`。

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `tree` | TreeView | 必需 | 树 |
| `root` | int | 必需 | 要导出的子树：视图所在的位置、选中的项，或 0（全部） |
| `match` | ndarray \| None | `None` | 过滤后的布尔掩码（按节点下标）；没过滤时是 None |
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
| `kind` | str | `"other"` | 类型（`cache` `build` `log` …，洞察配色按它上色） |
| `cleanup` | (`safe` `tool` `review` `keep`) | `"review"` | 能不能删：`safe`（随便删）`tool`（用工具清）`review`（先看看）`keep`（别删） |
| `app` | str | `""` | 相关的程序 |
| `note` | str | `""` | 补充说明 |
| `command` | str | `""` | 清理命令：只显示、可复制，从不运行 |
| `priority` | int | `0` | 详情面板里卡片按它从高到低排（规则的卡片用规则的 `priority`）（≥ -1000，≤ 1000） |
| `replaces` | list[str] | `[]` | Ids of insight rules whose card on this node it replaces ('*': any rule's)（≤ 32 个） |

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
| `sort` | str \| None | `None` | Column key, '-key' for descending |
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
