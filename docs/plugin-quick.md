# SpaceKnife 插件速查

先读这一页（`spaceknife plugin docs` 打印的就是它）。要细节时只读一节：`spaceknife plugin docs --section <锚点>`，或搜一个词：`spaceknife plugin docs --search <词>`。全文：[plugin-dev.md](plugin-dev.md)（`--guide`）、[plugin-api.md](plugin-api.md)（`--api`）。

命令在仓库里写 `uv run spaceknife plugin …`，打包版是 `spaceknife-backend plugin …`。

<a id="quick-start"></a>
## 1. 四步起步

```sh
spaceknife plugin new my.tool --with report,command,setting   # 骨架：已能通过 check 和自带测试
spaceknife plugin check my.tool --strict                       # 每个问题带 fix: 和 see: 锚点
uv run pytest my.tool/tests                                    # 合成树上跑；含一项插件进程里的检查
spaceknife plugin shot my.tool --open report:largest           # 真界面的截图（无头浏览器，不弹窗）
```

`--with` 的部分（每部分一段带注释的代码和它的测试）：`method report command setting cleanup panel visual visual-code color color-code export source set field insight hook`。`plugin new` 结束时按选的部分列出下一步的命令。

<a id="quick-tasks"></a>
## 2. 我要……

| 我要 | 写什么（`plugin = Plugin()` 之后） | `--with` | 一节 |
|---|---|---|---|
| 一张表、一组数字 | `@plugin.report(T("…","…"))` → `Report(blocks=[r.table(...)])` | `report` | `kind-report` |
| 一个按钮 / 右键菜单项 | `@plugin.command("name", T(..), menus=["node"], when=["file"])` | `command` | `kind-command` |
| 打开报告、面板、导出对话框 | `[[commands]] action = { open = "report:x" }`；`{ command = "report.export", args = { format = "<全名>" } }` | `command` | `kind-command` |
| 一项设置 | `[[settings]]`（`select`/`multi` 见 `kind-setting`） | `setting` | `kind-setting` |
| 详情面板里多一行 | `@plugin.node_field(T(..), exts=["zip"])` → `FieldValue(value=…, unit="size")` | `field` | `kind-node_field` |
| 洞察卡片（这是什么、能不能删） | 规则文件 `insights/*.toml`；写不出来时 `@plugin.insight(globs=[...])` → `Insight(..., replaces=["规则 id"])` | `insight` | `kind-insight` |
| 让清理候选进删除篮 | `@plugin.cleanup(T(..))` → `[Proposal(node=…, reason=T(..))]`；命令里 `ctx.basket.propose([(node, 原因), …])` | `cleanup` | `kind-cleanup` |
| 按类别上色、`:class:` 过滤 | `[[classes]]`、`[[folder_rules]] glob/contains/beside` | — | `kind-file_class` |
| 过滤框里的 `:set:名` | `@plugin.node_set(..)` → 节点下标数组或布尔掩码 | `set` | `kind-node_set` |
| 换一种面积 / 配色 | `[[visualizations]] size="files"`；代码版 `size="code"`；`[[color_modes]] by="age"`；代码版 `@plugin.color_mode` | `visual` `visual-code` `color` `color-code` | `kind-visualization` `kind-color_mode` |
| 一种导出格式 | `@plugin.export_format("md", T(..), extension="md", options=[...])` → 逐块 `yield str` | `export` | `kind-export_format` |
| 把压缩包等当文件夹扫描 | `@plugin.scan_source(..)` → `out.file(path, size)` | `source` | `kind-scan_source` |
| 一个网页面板 | `[[panels]]` + `web/index.html`、`web/app.js`（`sk.call` / `sk.core`） | `panel` | `panels` |
| 扫描完、设置变了、删除篮变了时做事 | `@plugin.on("scan_done" / "settings" / "lang" / "basket")` | `hook` | `kind-hook`、`api-hooks` |

<a id="quick-api"></a>
## 3. 最常用的 API

```python
from spaceknife_sdk import ActionContext, Context, FieldValue, Insight, Node, Plugin, Report, ReportContext, T, r

rows = ctx.tree.query("*.log;>1mb", under=c.zoom_node, kind="file", columns=("bytes", "path"), sort="-bytes", limit=100)
rows.ids, rows["bytes"], rows.paths, rows.names, rows.total, rows.records()   # total: 截断前的条数
ctx.tree.node(i).name / .path / .is_file / .size / .flags;  ctx.tree.children(i);  ctx.tree.col("alloc");  len(ctx.tree)
ctx.tree.rel_path(i)          # 相对根，永远 "/"；tree.path(i) 是系统的写法（Windows "\"）
ctx.settings["min_size"];  ctx.lang;  ctx.t("{n} 个", "{n} items", n=3)
ctx.fmt.size(n) / .count(n) / .date(t) / .ago(t)                               # 和界面一样的写法
ctx.toast(T("好了", "Done"), tone="ok");  ctx.progress(i, n);  ctx.check()      # check(): 取消时抛 Cancelled
ctx.basket.propose([(i, T("旧日志", "Old log"))]);  ctx.basket.withdraw([i])
ctx.files.read(i, limit=4096);  ctx.files.hash_many(ids, limit=1 << 20)       # 要 files.read；跳过云端占位
ctx.clipboard.write_text(T("复制的文字", "Copied text"))                        # 要 clipboard，只在用户发起的调用里
r.table([r.col("bytes", T("占用", "Size"), "size"), r.col("path", T("位置", "Location"), "path")], rows=rows)
r.stats([(T("文件", "Files"), n, "count")]);  r.callout(T(..), "warn");  r.md("…")
```

- 文案：`T("中文", "English")` 写在哪里都行（模块顶层定义的列标题也跟着界面语言变）；占位符可以写 `{n:size}` `{n:count}` `{t:date}` `{t:ago}`；复数：`T("{n} 组", "{n} groups").plural(n, "1 group")`。
- 命令和报告收到用户在哪：`c.node`（选中项）、`c.selection`、`c.zoom_node`（缩放根）、`c.filter`（过滤式，没过滤是 `""`）、`c.args`（动作里写的参数）。
- `when`：`scan done scanning view selection scan_view compare file dir on_disk`、`ext:zip,7z`、`setting:名`、`source:名`、`plugin:id`、`cap:能力`。
- 重活：普通 `def` 在工作线程里跑；`async def` 里不要做长循环（会卡住插件进程，取消不了），要做就 `await ctx.compute(fn, …)`。长循环里调 `ctx.check()`。

<a id="quick-test"></a>
## 4. 测试

```python
from spaceknife_sdk import RequestError
from spaceknife_sdk.testing import MockHost, make_tree

TREE = make_tree({"big.iso": 4 << 20, "logs": {"app.log": (1200, "2020-01-01")}, "data.bin": b"real bytes"})


async def test_it() -> None:
    async with MockHost.load(__file__) as host:  # process=True：真的插件进程（慢一点）
        host.scan(TREE)
        rep = await host.report("largest")
        assert "big.iso" in host.text(rep)
        log = host.find("logs/app.log")  # 相对根的路径 → 节点下标
        assert await host.command_available("greet", node=log) is None  # 它的 when 和 check
        await host.command("greet", node=log, zoom_node=None, filter="")  # 和菜单一样运行
        await host.fields("hover", node=log)
        await host.settled()
        await host.fields("hover", node=log)
        await host.set_setting("min_size", 1 << 30)
        await host.set_lang("en")
        assert host.path("logs") == host.store.path(host.find("logs"))  # 系统写法的路径
        await host.check(strict=True)  # 和 plugin check 一样
```

还有：`host.call` `cleanup` `insights` `node_set` `filter` `visual` `color` `export_text` `export_bytes` `source` `hook` `snapshot(when=)` `toasts` `clipboard` `basket` `events` `state`（[全表](plugin-api.md#api-testing)）。失败的调用抛 `RequestError`（`.code` 是 `ErrorCode`）。

<a id="quick-see"></a>
## 5. 在真界面里看

```sh
spaceknife plugin shot my.tool --open report:largest --text           # 截图 + 页面上的字 + 报错
spaceknife plugin shot my.tool --do visual:by_name --theme light --size 1200x800 -o v.png
spaceknife plugin shot my.tool --select logs/app.log --open panel:main --lang en
spaceknife plugin shot my.tool --open export:md       # 导出对话框选好这个格式；--scan 某个文件夹换掉合成树
spaceknife plugin dev my.tool --scan ~/Downloads      # 一直开着：保存就重新加载，打印浏览器网址
```

`shot` 和 `dev` 用 `--ui`（或环境变量 `SPACEKNIFE_UI`）指定构建好的前端（`pnpm -C apps/electron build` 的 `out/`）；前端比源码旧时会提醒。`shot` 需要 Chrome、Chromium 或 Edge（`SPACEKNIFE_BROWSER` 指定），每次用自己的临时数据目录和端口，结束就关掉。不开界面的看法：`plugin render my.tool report:largest --scan DIR`（还有 `field insight cleanup:名 export:名 set:名 visual:名 color:名 source:名`；`--call 方法` 先调方法，`--format json`）。

<a id="quick-pitfalls"></a>
## 6. 常踩的坑

| 现象 | 原因与改法 |
|---|---|
| `check` 报 `missing_en` / `placeholders` | 每段文案都写两种语言；占位符两边一样 |
| `check` 报 `icon_name` | 用程序的图标名（`--section api-icons`）或 `icons/<名>.svg` |
| `missing_permission` / `unused_permission` | `permissions` 里加上或删掉；`ctx.call("basket.add")` 也算 `basket.propose` |
| 字段第一次悬停是空的 | 悬停只给缓存里的；测试里 `await host.settled()` 再问 |
| 插件被结束（`did not stop a cancelled call`） | `async def` 里跑了长时间不 `await` 的代码；改成 `def` 或 `await ctx.compute(...)` |
| 测试里路径对不上（Windows 上 `\`） | 比较 `host.path("a/b")` 或 `tree.rel_path`，不要自己拼 |
| 命令到处出现 | 加 `when`（`file` `dir` `ext:…` `on_disk`）和 `menus` |
| 面板里 `sk.core` 报 `invalid params` | 参数照协议写（`tree.summary` 要 `node`）；`fixtures.json` 的 `core` 回答要和真的一样，`check` 会核对 |
| 别的插件或内置规则给同一节点相反的卡片 | `Insight(replaces=["规则 id"])` |
| 读文件触发云盘下载 | 跳过带 `Flag.CLOUD`、`Flag.OFFLINE` 的文件；`ctx.files.hash_many` 自己会跳过 |
| `STALE_TREE` | 存了节点下标又用：存 `tree.ref(i)` |
| 「需要先完成扫描」 | 命令加 `when = ["done"]` |

<a id="quick-more"></a>
## 7. 接着看

- 每种贡献的全部字段：`--section kind-<种类>`（`kind-report` `kind-command` `kind-setting` `kind-node_field` `kind-insight` `kind-cleanup` `kind-panel` `kind-visualization` `kind-color_mode` `kind-export_format` `kind-scan_source` `kind-hook` …，`--list` 列全）。
- 对象的每个字段、缓存、时限、`MockHost` 全表：`--section api-models` `api-cache` `api-limits` `api-testing`。
- 参考插件：`examples/plugins/`（`example.dupes` 代码插件、`example.devpack` 纯声明、`example.panel` 网页面板）。
