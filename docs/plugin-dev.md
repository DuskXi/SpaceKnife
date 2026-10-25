# SpaceKnife 插件开发

写插件先读一页的[速查](plugin-quick.md)（`spaceknife plugin docs`）；这一份是全文，按需只读一节。插件是一个文件夹：一份 `plugin.toml`，需要代码时再加一个 `plugin.py`，需要网页时再加 `web/`。通讯、协议、类型、主题、语言、权限确认都由 SpaceKnife 处理，插件只交数据和函数。

- **命令**：仓库里是 `uv run spaceknife plugin …`；打包版里是程序自带的 `spaceknife-backend plugin …`（安装目录的 `resources/backend/`）。下文写 `spaceknife plugin`。**只读一节**：`spaceknife plugin docs --section kind-report`；`--search 词` 找到提到它的行和所在的节；`--list` 列出三份文档的锚点。`plugin check` 的每个问题都带着对应一节的锚点。
- **API 参考**：[plugin-api.md](plugin-api.md)（`plugin docs --api`）列出每个对象的字段、调用发生在哪、缓存规则、钩子签名、路径写法、图标名和命令 id。**参考插件**见第 12 节（都有 `tests/`，CI 对它们跑 `plugin check --strict` 和测试）；设计与实现见 [plugins.md](plugins.md)。

<a id="quickstart"></a>
## 0. 起步

`plugin new` → `plugin check --strict` → `pytest` → `plugin shot`（真界面的截图）四步，见[速查第 1 节](plugin-quick.md#quick-start)；生成的骨架已经通过检查和它自带的测试，`--with` 选的每个部分都是一段能跑的例子。程序里边改边看：设置 › 高级 › 插件 › 开发者模式，在设置 › 插件「链接开发文件夹」，改完点「重启」；浏览器里：`spaceknife plugin dev <文件夹> --scan <要扫描的文件夹>`（第 10 节）。

<a id="layout"></a>
## 1. 插件长什么样

```
<数据目录>/plugins/example.dupes/   文件夹名就是插件 id（开发链接的文件夹可以叫别的）
  plugin.toml                       清单（必需）
  plugin.py                         代码入口（可选；也可以是包文件夹，见 entry）
  insights/*.toml                   洞察规则（按约定自动加载）
  templates/*.sktpl                 导出模板
  icons/*.svg                       图标（按约定自动加载，名字是文件名）
  locales/en.json                   额外的英文 {"中文": "English"}
  web/                              Web 面板：index.html、app.js…；_sk/ 与 sk-plugin.d.ts 由 build 生成
  generated/                        plugin build 生成：schema.json、pinyin.json
  lib/                              随插件带的纯 Python 依赖（加入 sys.path）
  fixtures/、tests/test_*.py        给 check 和测试用的样例（例如扫描来源的样例文件）、测试
```

- 数据目录：Windows `%LOCALAPPDATA%\SpaceKnife`，Linux `~/.local/share/SpaceKnife`，macOS `~/Library/Application Support/SpaceKnife`；便携版和设了 `SPACEKNIFE_HOME` 时是那个目录下的 `data/`。插件页显示实际位置。插件文件夹对插件只读。可写的是 `ctx.storage`（`<数据目录>/plugin-data/<id>/`，默认配额 256 MiB）。插件文件夹的上限：2000 个文件、64 MiB（`__pycache__`、`.git`、`dist`、`node_modules` 不算）。

<a id="manifest"></a>
### 1.1 清单 `plugin.toml`

| 字段 | 说明 |
|---|---|
| `id` | 必需。两段以上，小写字母、数字和 `-`，用点隔开，≤ 32 字符，例如 `example.dupes`。第一段不能是 `builtin sk spaceknife plugin` 或程序自己的命名空间（`view scan tree export …`） |
| `version` | 必需，semver：`0.1.0` |
| `api` | 插件接口版本，现在是 `1`（默认） |
| `name` | 必需，`{ zh = "…", en = "…" }` |
| `description` `author` `homepage` `license` | 插件页显示 |
| `icon` | 图标名（`gauge` `copy` `clock` …，[全部](plugin-api.md#api-icons)）或 `icons/` 里的文件 |
| `entry` | 代码入口：`plugin.py` 或一个包文件夹；不写就是纯声明式插件 |
| `permissions` | 要的权限（第 7 节），第一次启用时用户逐条看到 |
| `net_hosts` | 有 `net` 时必需：能连的主机，`["api.example.com", "*.cdn.example.com"]` |
| `exec` | 有 `exec` 时必需：能运行的程序名，`["git", "ffprobe"]` |
| `runtime` | `process`（默认，独立进程）或 `inprocess`（只在开发者模式下生效） |
| `activation` | `lazy`（默认，第一次用到才启动进程）或 `startup` |
| `requires` | `{ host = ">=0.2" }`：需要的 SpaceKnife 版本 |
| `limits` | `{ memory_mb = 1024, timeout_s = 60, threads = 2 }`：进程内存上限（64–4096）、方法和报告的时限上限、同时跑普通 `def` 调用的线程（1–8）；[运行规则](plugin-api.md#api-limits) |
| `main` | 保留（Electron 主进程插件），现在写了会报错 |

其余的表（`[[commands]]`、`[[settings]]` …）是贡献，见第 2、3 节。字段写错时 `plugin check` 会给出「是不是想写 …」。完整的 JSON Schema：`spaceknife plugin schema --manifest`。

<a id="code"></a>
### 1.2 两种写法，字段相同

同一项贡献可以写在 TOML 里，也可以在 Python 里声明，字段完全一样（同一个模型校验）。两处写同一项会报 `declared_twice`。

```toml
[[settings]]
name = "min_size"
title = { zh = "最小大小", en = "Minimum size" }
default = 1048576
```

等于 Python 里的 `plugin.setting("min_size", T("最小大小", "Minimum size"), 1048576)`。

带函数的贡献（报告、字段、清理……）用装饰器声明。被装饰的函数还是普通函数，插件内部可以直接调用。`plugin.py` 在 import 时只记录声明，不要在模块顶层做 I/O；宿主先在一个没有任何权限的进程里 import 它，读出声明（`describe`），之后才按需调用。

<a id="texts"></a>
### 1.3 文案

- 写在声明处，中英成对：TOML `{ zh = "重复文件", en = "Duplicate files" }`，Python `T("重复文件", "Duplicate files")`。只写中文也行，`check` 会提醒缺英文。
- 另外的英文可以放在 `locales/en.json`：`{"中文原文": "English"}`。
- 占位符：`T("{n} 组", "{n} groups").format(n=3)`；两种语言的占位符必须一样（`check` 检查）。运行时取当前语言的字符串：`ctx.t("你好，{name}", "Hello, {name}", name=x)`，`ctx.lang` 是 `zh-CN` 或 `en`。
- 界面上切换语言时，宿主重新取一遍插件的贡献；插件不用管。

<a id="icons"></a>
### 1.4 图标

`icon = "gauge"` 用程序自带的图标名（[一览](plugin-api.md#api-icons)，写错时 `check` 给出最接近的）；自己的图标放 `icons/<名字>.svg`，写 `icon = "icons/<名字>.svg"`。SVG 会被清洗：`viewBox="0 0 24 24"`，只允许形状元素和几何属性，颜色只能是 `currentColor` 或 `none`，≤ 16 KiB，不能有文字、脚本、外部引用。

<a id="colors"></a>
### 1.5 颜色

颜色可以写：`#RRGGBB`；每个主题一个 `{ light = "#…", dark = "#…" }`；语义名 `accent ok warn danger muted`；类别 `class:<类别 id>`；设计令牌路径（`treemap.class.video`）；报告和字段的色调还可以是 `palette:<配色全名>#<下标>`（那种配色色板里的一种颜色）。`check` 报出认不出的颜色、在深色或浅色背景上几乎看不见的颜色，以及色板里重复的颜色。

<a id="contributions"></a>
## 2. 贡献速查表

| 贡献 | TOML 表 | Python | 需要的权限 | 时限与上限 | 出现在 |
|---|---|---|---|---|---|
| [方法](#kind-method) | — | `@plugin.method` | 看它用什么 | 默认 30 s，≤ 600 s；JSON 8 MiB | 动作、报告、面板 |
| [事件 / 状态 / 流](#kind-event) | — | `@plugin.event` `@plugin.state` `@plugin.stream` | — | 事件 50 条/s，状态 20 次/s | 面板、命令状态行 |
| [命令](#kind-command) | `[[commands]]` | `@plugin.command` | 看动作 | 30 s（`timeout=`） | 命令面板、右键菜单、详情面板、工具栏 |
| [按键](#kind-keybinding) | `[[keys]]` | `plugin.keys` | — | — | 全局按键 |
| [设置](#kind-setting) | `[[settings]]` | `plugin.setting` | — | — | 设置 › 插件 |
| [报告](#kind-report) | `[[reports]]`（静态） | `@plugin.report` | 通常 `tree.read` | 30 s；2 MiB，每表 2000 行 | 标签页、详情面板、对话框、开始页、对比侧栏、状态栏 |
| [节点字段](#kind-node_field) | — | `@plugin.node_field` | `tree.read` | 详情 1 s，悬停 30 ms | 详情面板、悬停卡 |
| [洞察](#kind-insight) | — | `@plugin.insight` | `tree.read` | 1 s | 详情面板的洞察卡片 |
| [Web 面板](#kind-panel) | `[[panels]]` | `plugin.panel` | 看它调用什么 | 桥：单条 1 MiB，8 个并发 | 标签页、详情面板、对话框 |
| [可视化](#kind-visualization) | `[[visualizations]]` | `@plugin.visualization`、[`@plugin.layout`](#kind-layout) | 代码的要 `tree.read`（自带布局不要） | 每帧 120 ms；自带布局每个新画面 5 s | 工具栏、V 键 |
| [配色](#kind-color_mode) | `[[color_modes]]` | `@plugin.color_mode` | 代码的要 `tree.read` | 每帧 20 ms | 工具栏、C 键、图例 |
| [导出格式](#kind-export_format) | — | `@plugin.export_format` | `tree.read` | 30 s 没有输出就中止 | 导出对话框、命令行 |
| [导出模板](#kind-export_template) | `[[export_templates]]` | `plugin.export_template` | — | — | 导出对话框、命令行 |
| [洞察规则](#kind-insight_rules) | `insights/*.toml` | — | — | — | 洞察卡片、对比原因 |
| [文件类别](#kind-file_class) | `[[classes]]` `[[folder_rules]]` | `plugin.file_class` `plugin.folder_rule` | — | — | 类别配色、`:class:` 过滤 |
| [节点集合](#kind-node_set) | — | `@plugin.node_set` | `tree.read` | 5 s，按树的版本缓存 | 过滤框 `:set:<名>` |
| [过滤预设](#kind-filter_preset) | `[[filters]]` | `plugin.filter_preset` | — | — | 命令面板「过滤：…」 |
| [清理](#kind-cleanup) | — | `@plugin.cleanup` | `tree.read` | 120 s，10 万项 | 复核对话框 → 删除篮 |
| [扫描来源与监控](#kind-scan_source) | — | `@plugin.scan_source` `@plugin.watcher` | 读文件要 `files.read` | 60 s 没有进度提示卡住 | 开始页「新扫描」 |
| [对比原因与快照](#kind-change_annotator) | — | `@plugin.change_annotator`、`ctx.snapshots` | `snapshots.read` | 每批 200 ms | 对比侧栏、历史报告 |
| [钩子](#kind-hook) | — | `@plugin.on` | — | 30 s（`timeout=`），不等结果 | — |
| [后台任务](#kind-background) | — | `@plugin.background`、`ctx.tasks` | 看它用什么 | 不限（`timeout=`），每个插件同时 8 个 | 状态栏的任务列表 |
| [图标](#icons) | `[[icons]]`、`icons/*.svg` | — | — | 16 KiB | 处处 |

## 3. 每种贡献

下面每节：一张字段表（TOML 与 Python 相同，括号里是默认值），函数的签名，一个能直接用的最小例子，以及宿主替你做了什么。名字（`name`）是插件内的局部名：小写字母开头，字母、数字、下划线，≤ 24 字符；全名是 `<插件 id>.<名字>`。

### 3.1 通讯

<a id="kind-method"></a>
#### 方法

`@plugin.method` 声明一个请求 / 响应方法，线上名 `plugin.<id>.<名字>`。参数和结果都是 pydantic 模型（`Params`、`Payload` 的子类），宿主据此校验、生成 schema 和面板的 TypeScript 类型。

选项：`name`（默认是函数名）、`timeout`（30 秒，≤ 600）、`latest`（同一调用方的新请求取消没完成的旧请求，False）、`public`（面板和前端能不能直接调，True）。

签名：`async def f(ctx: Context, p: MyParams) -> MyResult`，没有参数时 `async def f(ctx) -> MyResult`。也可以是普通 `def`，宿主放到工作线程里跑，可以阻塞（被取消时立即答复，函数在备用线程上跑完，[运行规则](plugin-api.md#api-limits)）。返回 `Columns(...)` 时 numpy 列走二进制帧，不经过 JSON。宿主替你做了：路由、参数校验、超时与取消、并发上限、只算最新、错误映射。`raise PluginError(T("需要先选中一个文件夹", "Select a folder first"))` 原样显示给用户；其他异常只显示「插件出错：类型」，回溯进日志。

```python ex
class HelloParams(Params):
    name: str = "world"


class HelloResult(Payload):
    text: str


@plugin.method
async def hello(ctx: Context, p: HelloParams) -> HelloResult:
    """Greets someone (the first paragraph of the docstring becomes the method's doc)."""
    return HelloResult(text=ctx.t("你好，{name}", "Hello, {name}", name=p.name))
```

<a id="kind-event"></a>
#### 事件、共享状态与流

```python ex
@plugin.event
class Progress(Payload):
    done: int
    total: int


@plugin.state
class Status(Payload):
    runs: int = 0


@plugin.method
async def work(ctx: Context) -> Status:
    ctx.emit(Progress(done=1, total=1))  # 发给调用方（面板用 sk.on("progress", …) 收）
    return ctx.state(Status).update(runs=ctx.state(Status).get().runs + 1)  # 宿主保存最新值并推送


@plugin.stream
async def ticks(ctx: Context) -> AsyncIterator[Progress]:
    for i in range(3):  # 面板 sk.subscribe("ticks", null, cb)；Columns 走二进制帧
        yield Progress(done=i, total=3)
```

事件名、状态名默认是类名的蛇形（`Progress` → `progress`），也可以 `@plugin.event(name=…)`。`ctx.progress(done, total, text)` 是通用进度，报告和复核对话框直接显示，不用声明。流和监控器不受 600 秒的上限。

### 3.2 界面

<a id="kind-command"></a>
#### 命令

命令出现在命令面板里，可以绑键，可以放进右键菜单、详情面板的操作行和工具栏「更多」。

| 字段 | 说明 |
|---|---|
| `name` `title` | 必需 |
| `icon` | 图标名 |
| `keys` | 最多 4 个，`["Ctrl+Alt+D"]`；和程序的键冲突时程序的赢（`check` 提醒） |
| `aliases` | 命令面板里的别名 |
| `when` | 条件，都满足才可用，否则显示原因：`scan`（有扫描）`done`（扫描完成）`scanning` `view` `selection` `scan_view` `compare`，选中项是 `file` / `dir` / `ext:zip,7z`（这几种扩展名的文件），`on_disk`（扫描的是磁盘，不是来源），以及 `setting:<设置名>`（为真）、`cap:<能力>`、`plugin:<插件 id>`（已启用）、`source:<来源名>` |
| `menus` | `node`（节点右键菜单）`inspector`（详情面板操作行）`toolbar` `start`；菜单和操作行对它们所在的那个节点问 `when` 和 `check`，不可用的变灰并写出原因，运行时那个节点就是 `node` 和 `selection` |
| `action` | 做什么（下表）；用作装饰器时不写，调用被装饰的函数 |
| `status` | `{ state = "status", text = { zh = "{groups} 组，{bytes:size}", … } }`：命令面板里的状态行，取自共享状态（`:size` `:count` `:date` `:ago` 按界面的写法） |
| `typed_only` | 只在输入名字时出现（False） |
| `order` | 排序（0） |
| `check` | 一个方法名：`def f(ctx, c: ActionContext) -> str \| None`，收到和命令一样的上下文（选中项、缩放根、过滤），`ctx.tree` 是上下文的扫描；返回不可用的原因。抛异常时不可用（原因是那个错误），50 ms 内答不出算可用 |

动作，TOML 和 Python 都写成一个键：

| 动作 | 例子 | 说明 |
|---|---|---|
| `call` | `{ call = "hello", args = { name = "x" } }` | 调本插件的方法，参数是 `ActionContext`（扫描、视图、选中、缩放根 `zoom_node`、过滤 `filter`、`args`） |
| `open` | `{ open = "report:largest", args = { … } }`、`panel:years`、`settings` | 打开报告（`args` 成为它的 `c.args`）、面板（`sk.context.args`；已经开着的收到 `sk.onContext`）或本插件的设置 |
| `command` | `{ command = "view.rescan" }`；`{ command = "report.export", args = { format = "my.tool.md", scope = "selection", options = { … } } }` | 运行任意命令（`args` 交给它的动作）；导出对话框按 `format scope options use_filter` 打开，从右键菜单来时「选中项」是那一项 |
| `filter` | `{ filter = "*.log;>1mb" }` | 设置过滤 |
| `view` | `{ view = { visual = "flame" } }` | 改视图设置（`detail min_element sort_by_size show_free show_unknown show_trash visual color_mode filter_mode direction`） |
| `cycle` | `{ cycle = "color", step = 1 }` | 轮换可视化或配色 |
| `toggle` | `{ toggle = "show_free" }` | 开关 `show_free show_unknown show_trash sort_by_size` |
| `setting` | `{ setting = "min_size", value = 1 }`、`toggle = true` | 改本插件的设置 |
| `cleanup` | `{ cleanup = "extra", args = { rule = "logs" } }` | 运行一个清理（先复核），`args` 成为它的 `c.args` |
| `node` | `{ node = "reveal" }` | 对选中项：`reveal open terminal copy_path select zoom basket` |
| `url` | `{ url = "https://…" }` | 打开网页（用户先确认；要 `host.open`） |
| `scan_source` | `{ scan_source = "zip", uri = "$node" }` | 用本插件的扫描来源开一个扫描（`$node`：选中项的路径） |
| `builtin` | `{ builtin = "app.palette" }` | 前端的内置处理 |

```toml ex
[[commands]]
name = "open_largest"
title = { zh = "最大的文件", en = "Largest files" }
icon = "gauge"
when = ["done"]
menus = ["toolbar"]
action = { open = "report:largest" }
```

```python ex
@plugin.command("greet", T("问好", "Say hello"), icon="info", menus=["node"])
async def greet(ctx: Context, a: ActionContext) -> Toast:
    name = ctx.tree.name(a.node) if a.node is not None else "?"
    return Toast(text=T("你好，{name}", "Hello, {name}").format(name=name), tone="ok")
```

函数式命令返回 `Toast`（显示提示）、`Report`（在对话框里打开）、`CompareOpen`（打开两份快照或快照与现在的[对比](plugin-api.md#api-live)，要 `snapshots.read`）或 None。打开、复制、加入删除篮各要对应的权限（`host.open`、`clipboard`、`basket.propose`）。运行命令的一方可以再给参数：面板的 `sk.host.command(id, args)`、MockHost 的 `host.command(name, node=…, **args)`，加在动作自己的 `args` 上（[动作的参数](plugin-api.md#api-action-args)）。

<a id="kind-keybinding"></a>
#### 按键

| 字段 | 说明 |
|---|---|
| `key` | `Ctrl+Alt+D`、`Shift+F6`（修饰键顺序随意，宿主统一写法） |
| `command` | 本插件命令的名字，或任意命令的全名（`view.rescan`） |
| `when` | 同命令的 `when` |
| `override` | 明确要压过程序自己的键（False）；不写时程序的键赢 |

```toml ex
[[keys]]
key = "Ctrl+Alt+L"
command = "open_largest"
```

所有按键都在一张键位表里（设置 › 快捷键显示它们来自哪个插件），冲突在插件页和 `check` 里列出。

<a id="kind-setting"></a>
#### 设置

| 字段 | 说明 |
|---|---|
| `name` `title` `default` | 必需；`default` 是布尔、整数、小数、文字，或条目的列表（`list`） |
| `control` | `switch segmented select multi number slider color text textarea list`；默认按 `default` 和 `options` 推断：4 个以内的选项是 `segmented`，更多是 `select`（下拉），列表默认值是 `list`。`multi` 的值是选中的值按 `options` 的顺序用逗号连起来：`default = "name,size"`，读时 `.split(",")`；`textarea` 是多行文字 |
| `max_length` | `text` `textarea` 最长几个字（64；`textarea` 4096；≤ 16384）；默认值更长时 `check` 报错并给出要写的数 |
| `fields` `max_items` | `list`：每个条目的几栏（≤ 12，写法同设置，不能再是列表）和最多几条（50，≤ 500）；设置页里可以增删、上下移、逐栏校验，`ctx.settings[名]` 读到 `list[dict]`，缺的栏补默认值 |
| `description` | 说明 |
| `options` | `[[值, 标题], …]`：可选的值 |
| `min` `max` `step` `unit` | 数字的范围、步长和单位；`unit` 可以中英成对（`{ zh = "天", en = "days" }`），`B` 按大小显示和输入（1 MB） |
| `applies` | `now`（默认）`next_scan` `new_views` |
| `synonyms` `depends` | 设置搜索的同义词；另一个开关设置的名字：它关着时这一项变灰 |
| `group` `placeholder` | 分组标题（设置页写成「插件名 › 分组」）；空的文字框里的提示 |
| `default_from` | 只用于导出格式的选项：默认值取本插件这个设置的当前值 |

```toml ex
[[settings]]
name = "rules"
title = { zh = "清理规则", en = "Cleanup rules" }
default = [{ glob = "*.log", days = 90 }]
fields = [{ name = "glob", title = { zh = "通配符", en = "Pattern" }, default = "*" },
  { name = "days", title = { zh = "天数", en = "Days" }, default = 30, min = 1, unit = { zh = "天", en = "days" } }]
```

读：`ctx.settings["rules"]`（`ctx.settings.get(name, default)`）；写：`ctx.settings.set(name, value)`。设置键是 `plugin.<id>.<名字>`，出现在设置 › 插件，逐项校验，坏掉一项只回落那一项；插件停用后值留在设置文件里。改动后插件收到 `settings` 钩子（参数是变了的名字），宿主缓存的这个插件的回答（字段、节点集合、权重、图例）同时作废，见[缓存](plugin-api.md#api-cache)。

<a id="kind-report"></a>
#### 报告

报告是一份文档：一列块（第 5 节），宿主用程序自己的组件画，数字、日期、大小、主题和语言都和别处一样。

| 字段 | 说明 |
|---|---|
| `name` `title` | 必需；用作装饰器时 `name` 默认是函数名 |
| `icon` | 图标 |
| `slot` | 放在哪：`tab`（标签页，默认）`inspector`（详情面板的一页）`dialog` `start`（开始页卡片）`compare`（对比侧栏）`status`（状态栏）`history`（开始页的历史，配 `ctx.snapshots`） |
| `refresh` | 什么时候重新取：`open`（打开时，默认）`selection`（详情面板默认）`scan`（扫描完成）`frame`（视图变化，≤ 1 次/秒）`compare` `manual`（只在用户点「刷新」时）`interval:<秒>`（≥ 5）`state:<状态名>`（那个共享状态变了；报告自己改这个状态不会再触发一次，不会循环） |
| `when` | 同命令 |
| `order` | 排序 |
| `blocks` | 只在 TOML 里：静态报告，不要代码 |

签名：`async def f(ctx: Context, c: ReportContext) -> Report`。`c` 有 `scan_id view_id node selection compare_id cursor args`（`cursor` 是表格 `more` 续读的位置）。只算最新：用户切换选中时，还没算完的旧报告被取消。

```python ex
@plugin.report(T("最大的文件", "Largest files"), name="largest", icon="gauge", refresh=["scan"])
async def largest(ctx: Context, c: ReportContext) -> Report:
    rows = ctx.tree.query(kind="file", under=c.node, columns=("bytes", "path"), sort="-bytes", limit=50)
    total = int(rows["bytes"].sum())
    stats = r.stats([(T("文件", "Files"), len(rows), "count"), (T("合计", "Total"), total, "size")])
    cols = [r.col("bytes", T("占用", "Size"), "size"), r.col("path", T("位置", "Location"), "path")]
    clean = r.action(T("清理日志", "Clean up logs"), cleanup="logs")
    blocks = [stats, r.table(cols, rows=rows)]
    return Report(
        title=T("最大的文件", "Largest files"),
        empty=T("没有文件", "No files"),
        blocks=blocks,
        actions=[clean],
    )
```

```toml ex
[[reports]]
name = "about"
title = { zh = "说明", en = "About" }
slot = "dialog"
blocks = [{ type = "md", text = { zh = "这个插件找出**最大的文件**。", en = "This plugin finds the **largest files**." } }]
```

超过上限（200 块、每表 2000 行、4 层、2 MiB）的部分被截掉，末尾加一条提示；表格的其余部分用 `more` 分页。

<a id="kind-node_field"></a>
#### 节点字段

详情面板（`inspector`）和悬停卡（`hover`）里，内置信息之后的一行。

| 字段 | 说明 |
|---|---|
| `name` `title` | 必需 |
| `slots` | `["inspector"]`（默认），可加 `hover` |
| `when` `order` | 同上；`source:<来源名>` 只在那个来源的扫描里问，`on_disk` 只在磁盘扫描里问 |
| `kinds` `exts` `globs` | 只问这些节点：`["file"]`/`["dir"]`、扩展名 `["zip", "7z"]`、路径 glob；不写时每个节点都问 |

签名：`async def f(ctx, node: Node) -> FieldValue | str | None`；None 表示这一项不显示。`FieldValue(value=…, unit="size", tone=None, node=None, action=None)`，`value` 可以是 `T(...)`。悬停只显示缓存里的值（第一次悬停时宿主去算，下次显示；测试里 `await host.settled()`）；详情面板和悬停卡共用一份缓存，按语言分开；`ctx.slot` 说明在哪。后台算完后 `ctx.invalidate("node_field")` 让界面再要一次。

```python ex
@plugin.node_field(T("名字长度", "Name length"), slots=["inspector", "hover"])
def name_length(ctx: Context, node: Node) -> FieldValue | None:
    return FieldValue(value=len(node.name), unit="count")
```

<a id="kind-insight"></a>
#### 洞察（代码）

详情面板顶部的洞察卡片：这是什么、能不能删、怎么清理。规则能写的用[洞察规则](#kind-insight_rules)，规则写不出来的才用代码。选项：`name`（默认是函数名）；`globs`（只问路径匹配其中一个的节点，如 `["**/node_modules", "**/*.bak"]`；不写时每个节点都问，详情面板会慢）；`title`（启用确认里的名字）。`Insight(node=上级文件夹)` 让卡片说那个文件夹（「加入删除篮」放它），`regenerate` 写删掉后它怎么回来。

```python ex
@plugin.insight
async def backups(ctx: Context, node: Node) -> Insight | None:
    if not (node.is_file and node.name.endswith(".bak")):
        return None
    return Insight(
        title=T("备份文件", "A backup"), reason=T("编辑器的备份", "An editor's backup"), cleanup="review"
    )
```

`Insight` 的字段：`title reason kind cleanup app note command priority replaces`；`cleanup` 是 `safe tool review keep`；`command` 只显示、可复制，从不运行；卡片按 `priority`（规则的或插件给的，默认 0）从高到低排；`replaces=["cargo-target"]` 让这张卡代替同一节点上那些规则的卡（`["*"]`：任何规则的），不会再出现结论相反的两张。

<a id="kind-panel"></a>
#### Web 面板

声明一个 `web/` 里的网页，放进标签页、详情面板或对话框（第 6 节讲怎么写网页）。

| 字段 | 说明 |
|---|---|
| `name` `title` | 必需 |
| `short_title` | 标签页上的短标题（默认 `title`） |
| `icon` | 图标 |
| `entry` | `web/index.html`（默认），必须在 `web/` 里 |
| `params` | 给网页的参数（`sk.context.params`）：同一个网页放两处，各带一组；每次打开不同的用 `open` 动作的 `args`（`sk.context.args`） |
| `slot` | `tab`（默认）`inspector` `dialog` |
| `height` | 详情面板和对话框里的高度（320，40–800） |
| `keep` | 标签页转到后台时保留网页和它的状态（True） |
| `when` | 同命令 |

```toml ex
[[panels]]                    # 打开它：{ open = "panel:board" }
name = "board"
title = { zh = "面板", en = "Board" }
```

### 3.3 可视化与配色

<a id="kind-visualization"></a>
#### 可视化

插件不画图：要么选一个内置的画法（`base`），换一种面积的算法；要么[自带布局](#kind-layout)（`@plugin.layout`），自己决定每一块放在哪里、画成什么形状。两种都由程序来画，悬停、选中、标签、配色、缩放、呼吸、两种主题这些效果全都有。

| 字段 | 说明 |
|---|---|
| `name` `title` | 必需 |
| `icon` `order` | 工具栏里的图标和位置（50；插件的排在内置的之后，`visual.list` 报的是 100 + order） |
| `base` | `treemap` `flame`（默认）`flame_polar`，或 `shapes`（[自带布局](#kind-layout)，配 `@plugin.layout`） |
| `fill` `max_rows` `nested` | 只用于自带布局 |
| `size` | 面积按什么：`alloc`（占用，默认）`size`（逻辑大小）`files`（文件数）`items`（项数）`code`（函数给权重） |
| `unit` | 面积的单位：`bytes` `count` 或 `weight`（默认按 `size` 推断：`code` 是 `weight`），决定标签的写法：`weight` 的标签写占缩放根权重的比例（同一层加起来不超过 100%），悬停卡另写真实占用 |
| `options` | 本插件的设置名：传给函数的 `v.options` |
| `compare` | 对比时也可用（False） |
| `default_color` | 视图切到它时用的配色：`class flat time branch`，或本插件的配色名（其他插件的写全名）；不写跟着 `base` |
| `when` | 哪里能选它（`scan done scanning compare scan_view on_disk setting: cap: plugin: source:`，同命令的 `when`）：不满足时工具栏里的按钮变灰、悬停写原因，V 键跳过它 |

```toml ex
[[visualizations]]
name = "files_flame"
title = { zh = "按文件数的火焰图", en = "Flame graph by file count" }
base = "flame"
size = "files"
```

代码版：函数给每个节点一个权重（长度 `len(tree)` 的数组），宿主只取文件的权重、把它们加到文件夹上。权重是 ≥ 0 的整数，NaN 和负数当 0。`VisualInput` 有 `tree` `options`（`options` 里列出的本插件设置的当前值）`settings`（本插件全部设置）`ctx` 和 `cancelled`/`check()`。权重按树版本、选项和设置缓存。`spaceknife_sdk.layout` 有几个帮手：`files_only` `by_column` `count` `check`。

```python ex
@plugin.visualization("by_lines", T("按估计行数", "By estimated lines"), base="treemap")
def by_lines(v: VisualInput) -> np.ndarray:
    return layout.by_column(v.tree, "size") // 40
```

每帧 120 ms；连续失败三次后，这个可视化在这次会话里不再可用。

<a id="kind-layout"></a>
#### 自带布局的可视化

圆堆积、冰柱图、Voronoi 格子这类画法：程序挑出要画的块，插件的函数说每一块放在哪里、画成什么形状，程序来画。

```python ex
@plugin.layout("pack", T("圆堆积", "Circle packing"), fill=0.5)
def pack(v: LayoutInput) -> Shapes:
    return shapes.pack(v)  # SDK 带的圆堆积；自己排时 out = v.shapes()，每块 out.circle / rect / polygon
```

- **程序给什么**（`v: LayoutInput`）：缩放根下面值得画的块，父块在前，最多 `max_rows` 个。挑法和内置视图一样（合并小项、聚合块分页、等待扫描、可用和未知空间）；「太小」按面积估计：块占缩放根的份额 × 画布面积 × `fill`（块实际能分到的比例：圆堆积约 0.5，冰柱图 1）比最小元素的平方小就合并。每块有 `node` `parent` `kind` `depth` `size` 和区间 `u0`…`u1`（冰柱图的 x），画布是 `width` × `height` 物理像素。不要权限；要名字等再用 `v.tree`（要 `tree.read`）。
- **回答什么**（`Shapes`，坐标是物理像素）：每块一个圆角矩形、圆或 3–12 点的凸多边形，或者不画（它里面的块也不画）；下标 −1 是缩放根自己，给了形状就画成底（双击它回到上一层）。圆和多边形的标签居中，矩形的靠左上，放不下的不写；标签占多大地方在 `v.label`，形状边上的细线是 `out.stroke`。
- **宿主**：每个新画面调一次（≤ 5 s），按「这些块、这块画布、这些设置」缓存，所以同样的输入要给同样的答案；失败时画布中间写原因。

字段：`size`（`alloc` `size` `files` `items`）、`fill`（0.05–1，默认 0.75，见上）、`max_rows`（100–50 000，默认 5000）、`nested`（文件夹的块在它的形状里面：True，圆堆积；在别处：False，冰柱图），其余同上表。

方法、标签、失败时的样子和冰柱图的写法见 [API 参考](plugin-api.md#api-layout)；完整例子是 `examples/plugins/example.shapes`（圆堆积、六边形、冰柱图）。测试：`frame = await host.visual("pack")`（失败时抛出原因），`plugin render <文件夹> visual:pack` 打印形状的统计。

<a id="kind-color_mode"></a>
#### 配色

配色给每个画出的块一个色板下标（0 是中性色）。声明式的由宿主按规则算，不跑插件代码。

| 字段 | 说明 |
|---|---|
| `name` `title` `palette` | 必需；`palette` 1–16 种颜色（1.5 节），0 号是中性色 |
| `legend` | 每种颜色一条说明（与 `palette` 等长） |
| `by` | `ext`（按扩展名，配 `map`）`class`（按类别，配 `map`）`age`（按时间，配 `bands`、`field`）`size`（按大小，配 `bands`）`depth`（按层级）`name`（按名字的 glob，配 `rules`）`set`（在不在一个节点集合里，配 `set`）`insight`（洞察的类型，配 `map`）；不写时用函数 |
| `map` | `{ mp4 = 1, mkv = 1 }`：值 → 下标 |
| `field` | `by = "age"` 用哪个时间：`mtime`（默认）`ctime` `atime` |
| `bands` | 分界：`["30d", "1y"]` 或 `["1mb", "1gb"]`；k 个分界要 k + 2 种颜色 |
| `rules` | `[{ glob = "*.log", index = 1 }]` |
| `set` | `by = "set"` 的节点集合全名 |
| `folders` | 展开的文件夹怎么画：`ring`（默认，层级色）或 `palette` |
| `layouts` | 只用于哪些布局：`drawlist.v1`（树图）`flame.v1` `polar.v1`；空表示全部 |
| `bands_setting` | 本插件一个文字设置的名字（如 `"7d, 30d, 1y"`）：`bands` 跟着它变；没写 `legend` 时图例按分界生成（`< 7d`、`7d – 30d`…） |

```toml ex
[[color_modes]]
name = "media"
title = { zh = "媒体类型", en = "Media type" }
by = "ext"
palette = ["muted", "#3F7FBF", { light = "#B5651D", dark = "#E0A060" }]
legend = [{ zh = "其他", en = "Other" }, { zh = "视频", en = "Video" }, { zh = "图片", en = "Images" }]
map = { mp4 = 1, mkv = 1, jpg = 2, png = 2 }
```

代码版：`(c: ColorInput) -> np.ndarray[uint8]`，`c.ids` 是这一帧画出的节点，返回同样长的下标；`c` 还有 `tree` `palette`（色板长度）`settings` `ctx` `cancelled`。每帧 20 ms，超时这一帧全是中性色。旧写法 `(tree, ids)` 也能用。图例要跟着设置变时，用 `@plugin.color_legend("<配色名>")` 给出：`(ctx) -> list[文字 | (文字, 颜色)]`，每个下标一条，设置变了宿主重新要。

```python ex
@plugin.color_mode("parity", T("奇偶", "Odd and even"), palette=["muted", "ok", "warn"])
def parity(c: ColorInput) -> np.ndarray:
    return (1 + c.ids % 2).astype(np.uint8)


@plugin.color_legend("parity")
def parity_legend(ctx: Context) -> list[Text | str]:
    return [T("其他", "Other"), T("奇数", "Odd"), T("偶数", "Even")]
```

### 3.4 数据

<a id="kind-export_format"></a>
#### 导出格式

| 字段 | 说明 |
|---|---|
| `name` `title` `extension` | 必需；`extension` 如 `md`（写不写点都行，宿主存成不带点的） |
| `description` | 导出对话框里的说明 |
| `media_type` | `text/plain`（默认） |
| `binary` | 产出 `bytes` 而不是 `str`（False） |
| `bom` | 文件开头写 UTF-8 BOM（False；Excel 打开 CSV 需要） |
| `options` | 导出对话框里的字段，写法同[设置](#kind-setting)（≤ 12 个）；值在 `x.options` |

签名：`def f(x: ExportInput) -> Iterator[str]`。`x.root` 是要导出的子树（视图所在的位置、选中的项或 0 表示全部），`x.match` 是过滤后的掩码（按节点下标，没过滤时是 None；只导出选中项时它也只留选中的子树），`x.filter` 是过滤式，`x.options` 是对话框里选的值（没给的取默认），`x.selection` 是选中的节点，`x.settings` 和 `x.ctx` 同别处。宿主流式写文件、显示进度、出错时删掉临时文件；30 秒没有新的块就中止。用户在导出对话框和 `spaceknife run … export <全名> <文件>` 里都能选它（`run` 用选项的默认值）。旧写法 `(tree, root, match[, info])` 也能用。

```python ex
@plugin.export_format(
    "md",
    T("Markdown 表格", "Markdown table"),
    extension="md",
    media_type="text/markdown",
    options=[{"name": "limit", "title": T("最多几行", "At most"), "default": 100, "min": 1}],
)
def md(x: ExportInput) -> Iterator[str]:
    rows = x.tree.query(kind="file", under=x.root or None, columns=("bytes", "path"), sort="-bytes")
    yield "| Size | Path |\n|---:|---|\n"
    for node, size, path in list(zip(rows.ids.tolist(), rows["bytes"].tolist(), rows.paths, strict=True))[
        : int(x.options["limit"])
    ]:
        if x.match is None or x.match[node]:
            yield f"| {size} | {path} |\n"
```

<a id="kind-export_template"></a>
#### 导出模板

模板语言与 SpaceSniffer 兼容，语法见 [export-templates.md](export-templates.md)。

| 字段 | 说明 |
|---|---|
| `name` `file` `title` | 必需；`file` 在 `templates/` 里 |
| `description` | 说明 |
| `file_name` | 建议的输出文件名（默认取模板的 `@file`） |
| `spacesniffer` | 只用了 SpaceSniffer 的语法，在那边也能用（False） |

```toml ex
[[export_templates]]
name = "top"
file = "templates/top.sktpl"
title = { zh = "第一层文件夹的占用", en = "First-level folder sizes" }
```

```text ex-template
@name=First-level sizes
@file=Sizes.txt
@blocks=folders
@sort=size
@order=desc
@detail
{script}
{   if {<%nestinglevel%>} == {1}
  ? {<%disksize%>{&tab}<%pathfile%>{&br}}
}
```

<a id="kind-insight_rules"></a>
#### 洞察规则

`insights/` 下的每个 `*.toml` 都自动加载（也可以在 `[[insight_rules]]` 里用 `file` 列出：`file = "insights/x.toml"`）。格式见 [insights.md](insights.md)：规则能要求旁边或里面有某些名字（`beside = ["package.json"]`、`contains = ["pyvenv.cfg"]`），能写 `regenerate`（删掉后怎么回来）。顺序：程序自带的规则 → 各插件的（按插件 id）→ 用户自己的文件夹；后面的规则按 id 覆盖前面的，`enabled = false` 删除一条。规则也给对比侧栏的变化项提供原因。

```toml ex-rules
version = 1

[[rule]]
id = "docs-turbo"
kind = "cache"
title = "Turborepo 缓存"
app = "Turborepo"
cleanup = "safe"
reason = "Turborepo 的本地任务缓存，删掉后下次构建会重新生成。"
names = [".turbo"]
i18n.en.title = "Turborepo cache"
i18n.en.reason = "Turborepo's local task cache; the next build makes it again."
```

<a id="kind-file_class"></a>
#### 文件类别与文件夹规则

| `[[classes]]` 字段 | 说明 |
|---|---|
| `name` `title` | 必需；全名 `<插件 id>.<名字>` 用于 `:class:` 过滤 |
| `exts` | 扩展名（不带点，≤ 512 个） |
| `aliases` | 过滤里能用的别名 |
| `color` | 类别色（1.5 节） |

| `[[folder_rules]]` 字段 | 说明 |
|---|---|
| `glob` | 文件夹路径的 glob，`**/target/debug`；`{a,b}` 二选一：`**/{.venv,venv}` |
| `class` | 类别：本插件的名字、全名或内置类别 |
| `mode` | `swallow`（默认：里面的所有文件都算这个类别）或 `fallback`（只给没有类别的文件） |
| `name` | 规则名（默认由 glob 的字母数字生成：`**/.venv` 是 `rule_venv`，重名时加 `_2`） |
| `priority` | 先试优先级高的（0，−100–100），同级按声明顺序；第一条匹配的决定 |
| `exclude` | 不适用的路径 glob：`["**/steamapps/**"]` |
| `contains` `beside` | 还要求文件夹里有（`contains`）或旁边有（`beside`）这些名字之一，可以用 `*`：`glob = "**/target"`、`beside = ["Cargo.toml"]`；`contains = ["pyvenv.cfg", "CACHEDIR.TAG"]` |

`title` 两种语言都写：界面是英文时类别显示英文名，中文名留作过滤别名。`color` 写 `{ light, dark }` 时程序按自己的主题取。类别与文件夹的关系：落在文件夹规则（插件的、内置的项目文件夹，或用户选的）里的文件按文件夹的类别算，扩展名不再起作用；`fallback` 只给扩展名不属于任何类别的文件。

```toml ex
[[classes]]
name = "cad"
title = { zh = "CAD 图纸", en = "CAD drawings" }
exts = ["dwg", "dxf"]
color = "#3F7FBF"

[[folder_rules]]
glob = "**/drawings/archive"
class = "cad"
```

<a id="kind-node_set"></a>
#### 节点集合

过滤框里的一个词：`:set:<名字>`（或别名、标题）匹配函数给出的节点。

| 字段 | 说明 |
|---|---|
| `name` `title` | 必需 |
| `aliases` | 别名 |

签名：`async def f(ctx, tree: TreeView) -> np.ndarray`：长度 `tree.n` 的布尔掩码，或节点下标。宿主按树的版本缓存，5 秒时限。

```python ex
@plugin.node_set("big", T("大文件", "Big files"), aliases=["huge"])
async def big(ctx: Context, tree: TreeView) -> np.ndarray:
    return tree.query(kind="file", min_size=1 << 30, columns=()).ids
```

<a id="kind-filter_preset"></a>
#### 过滤预设

一个有名字的过滤式（语法见 [filter-syntax.md](filter-syntax.md)），成为命令「过滤：〈标题〉」，可以绑键。

```toml ex
[[filters]]
name = "recent_videos"
title = { zh = "最近的视频", en = "Recent videos" }
expr = ":class:视频;m<30d"
```

字段：`name` `title` `expr`。过滤式里可以用本插件的类别和节点集合。

### 3.5 操作、来源与历史

<a id="kind-cleanup"></a>
#### 清理

插件只提出候选；用户在复核对话框里看到每一项的路径、大小和原因，勾选后才进删除篮，真正删除仍然只能由用户在删除篮里做。

| 字段 | 说明 |
|---|---|
| `name` `title` | 必需 |
| `description` | 复核对话框里的说明 |
| `icon` | 图标（`basket`） |
| `when` | 同命令 |

签名：`async def f(ctx, c: ReportContext) -> list[Proposal]`。`Proposal(node=…, reason=…, checked=True)`，`node` 是节点下标、`NodeRef` 或绝对路径。

```python ex
@plugin.cleanup(T("清理日志文件", "Clean up log files"), name="logs")
async def logs(ctx: Context, c: ReportContext) -> list[Proposal]:
    rows = ctx.tree.query("*.log;m>30d", kind="file", columns=())
    why = T("一个月没改过的日志", "A log untouched for a month")
    return [Proposal(node=int(i), reason=why) for i in rows.ids]
```

报告里的按钮 `r.action(…, cleanup="logs")`、命令的 `{ cleanup = "logs" }` 都会打开复核。插件代码自己往删除篮里放东西用 `ctx.basket.propose(nodes, reason)`（要 `basket.propose`）。

<a id="kind-scan_source"></a>
#### 扫描来源与监控

扫描来源把一个 URI（文件、文件夹或一段文字）列成一棵树，出现在开始页的「新扫描」里。

| 字段 | 说明 |
|---|---|
| `name` `title` | 必需 |
| `icon` | 图标（`drive`） |
| `pick` | 用户怎么给 URI：`file` `folder` `text`（默认） |
| `extensions` | `pick = "file"` 时的扩展名 |
| `placeholder` | `text` 时输入框的提示 |
| `on_disk` | 列出的路径是真实的磁盘路径（False）；否则打开、显示位置等依赖真实路径的功能关闭 |
| `ops` | 允许删除（要 `on_disk`；False） |
| `watch` | 有对应的监控器（False） |
| `size` | 视图按哪一列量面积：`alloc`（默认）或 `size`（压缩包解压后的大小）；两列都照列出的保留 |

签名：`(ctx, uri: str, out: Listing) -> None`。`out.file(path, size=…, alloc=…, mtime=…)`、`out.dir(path)`、`out.root_name(text)`，路径相对来源的根，用 `/`，父文件夹自动补上；`out.file` 还认 `kind` `attrs` 和硬链接的 `ino` `nlink` `dev`，`out.dir` 认文件夹自身的 `size` `alloc`。`ctx.progress` 就是扫描的进度，列得久时已列出的先显示，抛出的异常是写在视图里的失败原因（[细节](plugin-api.md#api-source)）。用户选的 URI 本身在有 `files.read` 时可读：`ctx.files.open(uri)` 给出守磁盘负载上限的文件对象（`zipfile`、`tarfile` 都收），`open_path(uri)` 只给路径。

```python ex
@plugin.scan_source("zip", T("压缩包", "Archive"), pick="file", extensions=[".zip"], watch=True)
def zip_source(ctx: Context, uri: str, out: Listing) -> None:
    with ctx.files.open(uri) as fh, zipfile.ZipFile(fh) as z:
        for info in z.infolist():
            if not info.is_dir():
                out.file(info.filename, size=info.file_size, alloc=info.compress_size)
    out.root_name(PurePath(uri).name)


@plugin.watcher("zip")
async def watch_zip(ctx: Context, uri: str) -> AsyncIterator[list[Change]]:
    archive = Path(ctx.files.open_path(uri))
    seen = archive.stat().st_mtime_ns
    while True:
        await asyncio.sleep(1)
        if archive.stat().st_mtime_ns != seen:
            seen = archive.stat().st_mtime_ns
            yield [Change(path="", kind="modified")]
```

监控器 `@plugin.watcher(source, name=None)`（`source` 是来源的名字，`name` 默认是函数名）是一个异步生成器，产出一批批 `Change(path, kind)`（`created modified deleted overflow`）。变化安静 1 秒后宿主把来源重跑一遍（10 万项以内；更大的只提示需要重新扫描）。F5 也是整个重跑。`on_disk = false` 的来源扫描里，条目的路径是 `<root_name>/a/b`，详情面板不提供定位、复制路径以外的磁盘操作，删除篮拒绝它们。命令或报告里的 `{ scan_source = "zip" }` 动作（`uri` 默认 `$node`：选中项的磁盘路径）从普通扫描里直接打开一个来源扫描。`fixtures/<来源名>.*` 放一个样例，`check` 会用它跑一次。

<a id="kind-change_annotator"></a>
#### 对比原因与快照

对比两个快照时，洞察规则没给原因的变化项交给插件：

```python ex
@plugin.change_annotator
async def why(ctx: Context, items: list[ChangeUnit]) -> list[str | None]:
    return [ctx.t("变大了", "grew") if i.delta > 0 else None for i in items]
```

选项只有 `name`（默认是函数名）。`ChangeUnit` 有 `path kind status delta base target`；每批 500 项、200 ms，原因 ≤ 80 字，按对比缓存。

读历史用 `ctx.snapshots`（要 `snapshots.read`）：`list(root=None)` 只读文件头（时间、`bytes_alloc` 等合计、根），`open(path)` 载入整份快照，返回 `TreeView`，用完 `close()`（每个插件同时最多 2 个，调用结束自动释放）。配合 `slot = "history"` 或 `"compare"` 的报告：

```python ex
@plugin.report(T("增长趋势", "Growth"), name="growth", slot="history")
async def growth(ctx: Context, c: ReportContext) -> Report:
    snaps = ctx.snapshots.list()
    return Report(blocks=[r.spark([float(s["bytes_alloc"]) for s in snaps], unit="size")])
```

<a id="kind-hook"></a>
#### 钩子

`@plugin.on(event)`：`event` 是 `start` `stop`（进程启动、贡献注册好之后——第一次启用和 `plugin dev` 重新加载也一样 / 停止前，`stop` 有 2 秒）、`scan_done` `scan_closed`（收到 `ScanInfo`）、`tree_changed`（打开着的扫描的树变了：实时监控的文件变化、重新扫描；每个扫描每秒最多一次，收到 `TreeChange`：变化所在的文件夹、它们的路径和变化前后的大小、树的 `version`；读树时树又变了，宿主用新下标再调）、`settings`（收到变了的设置名）、`lang`（`ctx.lang` 已是新的）、`basket`（删除篮变了，一阵变化只来一次：用户拿出了本插件放进去的项、删了它们……）、`snapshots`（存了或删了快照，要 `snapshots.read`）；签名和细节见 [API 参考](plugin-api.md#api-hooks)（[树变化与快照](plugin-api.md#api-live)）。`name` 默认是函数名；写错的名字 `check` 从代码里就找出来（`unknown_hook`）。

```python ex
@plugin.on("scan_done")
async def scanned(ctx: Context, scan: ScanInfo) -> None:
    ctx.toast(T("扫描完成：{items} 项", "Scanned {items} items").format(items=scan.items), tone="ok")
```

宿主不等钩子的结果，出错记日志；`timeout`：秒（30，≤ 600），更久的事交给后台任务；`ctx.toast` 每秒最多一条。

<a id="kind-background"></a>
#### 后台任务

超过调用时限的事（给整盘的文件算哈希、一直盯着扫描）写成后台任务：宿主记着它，状态栏列出它的 `title` 和进度、可以取消。字段：`name` `title`（必需）、`start`（`manual` 默认：`ctx.tasks.start` 才启动；`startup`：插件进程启动就启动，进程也随插件启用而启动）、`restart`（插件进程崩溃后用同样的参数重新开始，最多 3 次；False）、`timeout`（秒，不写就不限）。

```python ex
@plugin.background("hash_big", T("给大文件算哈希", "Hashing big files"))
def hash_big(ctx: Context) -> None:
    ids = ctx.tree.query(">=1mb", kind="file", columns=()).ids
    ctx.files.hash_many(ids, progress=True)  # 进度进状态栏；取消时在下一个 MiB 停下
```

签名 `(ctx)` 或 `(ctx, p: 参数模型)`。`ctx.tasks.start(name, params, key="", replace=False)` 马上返回 `TaskInfo`，同名同 `key` 只跑一个（`replace=True` 取消旧的重来）；`ctx.tasks.cancel(name, key=None)`、`.list()`。普通 `def` 在自己的线程上跑，不占调用线程；`ctx.check()` 处停下，也在那里换成新的界面语言。测试：`await host.task(name, params)` 跑完返回 `TaskInfo`（`state` 是 `done` `failed` `cancelled`）。细节见[运行规则](plugin-api.md#api-limits)。

<a id="ctx"></a>
## 4. `Context` 与 `TreeView`

每个函数的第一个参数 `ctx` 是这一次调用的上下文：

| 成员 | 说明 | 权限 |
|---|---|---|
| `ctx.plugin` | `id version dir storage` | — |
| `ctx.lang`、`ctx.t(zh, en, **fields)`、`ctx.fmt` | 界面语言、即时翻译；`ctx.fmt.size(n)` `.count(n)` `.date(t)` `.ago(t)`：和界面一样写大小（用户的 1000 / 1024）、个数、时间 | — |
| `ctx.settings[name]`、`.get`、`.set(name, value)` | 本插件的设置 | — |
| `ctx.tree` | 这次调用所在的扫描（调用带的 `scan_id`，否则当前的扫描），带节点下标的调用拿到的是那一刻的树（[纪元](plugin-api.md#api-epoch)）；没有扫描时报「需要先完成扫描」 | `tree.read` |
| `ctx.scan(scan_id)`、`ctx.scans()` | 其他扫描 | `tree.read` |
| `ctx.node` | 调用针对的节点（`Node`）或 None | `tree.read` |
| `ctx.emit(event)`、`ctx.progress(done, total, text)`、`ctx.toast(text, tone)` | 事件；进度；提示（`info ok warn danger`） | — |
| `ctx.state(Model)` | `.get()` `.set(m)` `.update(**fields)` | — |
| `ctx.basket.propose(items, reason)`、`.withdraw(items)` | 放进删除篮（每项可以是 `(节点, 它自己的原因)`；一个命令里放多少次，用户只看到一条提示，命令自己 `toast` 时没有），返回放进去的项数；拿回本插件放进去的项 | `basket.propose` |
| `ctx.clipboard.write_text(text)` | 复制到发起调用的那个窗口的剪贴板 | `clipboard` |
| `ctx.notify(title, body, action=, button=, args=)` | 留在屏幕上的通知，带一个运行本插件命令的按钮；窗口最小化时也是系统通知（[通知](plugin-api.md#api-notify)） | `notify` |
| `ctx.granted`、`ctx.slot`、`ctx.invalidate(*kinds)` | 授予的权限；字段在哪（`inspector` `hover`）；丢掉宿主的缓存 | — |
| `ctx.files.open(n)`、`.read(n, limit)`、`.hash(n, "sha256", limit=None, progress=)`、`.hash_many(ns, limit=, progress=)`、`.open_path(path)` | 只读打开扫描范围内的文件（`n` 是节点、下标或路径），读盘守用户选的磁盘负载上限（和扫描共用，扫描时让路）；`hash_many` 几个线程一起读，跳过文件夹和云端占位（`Flag.CLOUD` `Flag.OFFLINE`，读它们会下载），读不了的给 None；`progress=True` 把读了多少字节报成 `ctx.progress`（或给 `(done, total)` 函数），每读 1 MiB 查一次取消；范围外是 `FileNotAllowed`，系统拒绝是别的 `PermissionError` | `files.read` |
| `ctx.net.fetch(url, method=, headers=, body=, timeout=)` | HTTP(S)，只能连 `net_hosts`；返回 `status headers body .json() .text()` | `net` |
| `ctx.exec(argv, timeout=, cwd=, input=)` | 运行 `exec` 里列出的程序；返回 `returncode stdout stderr` | `exec` |
| `ctx.snapshots.list(root)`、`.open(path)` | 3.5 节 | `snapshots.read` |
| `ctx.call(method, params)` | 白名单里的内置方法（第 7 节），返回 dict | 各方法自己的 |
| `await ctx.compute(fn, *args)` | 在工作线程里跑 CPU 密集或阻塞的代码 | — |
| `ctx.tasks.start(name, params, key=)`、`.cancel(name)`、`.list()` | 后台任务（3.5 节），返回 `TaskInfo` | — |
| `ctx.check()`、`ctx.cancelled`、`ctx.deadline` | 协作式取消：被取消或超时后 `check()` 抛 `Cancelled`（不要吞掉） | — |
| `ctx.log.debug/info/warning/error`、`ctx.storage` | 日志（程序的日志面板，带插件来源）；可写的 `Path`（配额 256 MiB） | — |

<a id="ctx-tree"></a>
### 4.1 读树

**首选 `query`**：宿主按过滤式、子树、种类、大小筛好，只送回命中的行和要的列。

```python
rows = ctx.tree.query(
    "*.mp4;>100mb",  # 过滤式（匹配文件，和过滤框一样），None 表示全部
    under=folder_id,  # 子树
    kind="file",  # file / dir / link
    min_size=1 << 20,  # 占用（文件夹按子树合计）
    columns=("bytes", "mtime", "path"),
    sort="-bytes",  # 列名，前面加 - 是降序；name / path 按字母
    limit=1000,
)
len(rows), rows.ids, rows["bytes"], rows.paths, rows.total  # total：截断前的条数
first = next(iter(rows))  # 逐行：row.id、row.bytes、row.path
```

`columns` 可以要：`bytes`（有效占用：文件的占用，文件夹的子树合计，重复的硬链接为 0）`logical`（同样口径的逻辑大小）`name` `path`，以及存储的列 `parent depth kind flags ext size alloc mtime ctime atime ino nlink child_start child_count tot_size tot_alloc tot_files tot_dirs cls`。

`under=X` 是 X 下面的项，不含 X 自己；不写时含各个根（根的 `name` 是完整路径）。命令和报告里 `under=c.zoom_node`、`query(c.filter or None, …)` 就是用户眼前的那部分。文件的 `tot_*` 列是 0（文件夹才有合计）。

**整列**：`tree.col("alloc")`、`tree.cols("kind", "size")`、`tree.children(id)`、`tree.walk(id)`（分块的子树先序）。第一次用到时宿主把这些列导出成只读的共享内存，插件直接映射，不逐个节点远程调用。

**单个节点**：`tree.node(id)` → `Node`（`id name path kind size alloc files dirs mtime ctime atime ext parent depth child_count flags cls`，文件夹的 `size alloc` 是子树合计）；`tree.nodes(ids)`、`tree.name(id)`、`tree.path(id)`（系统自己的分隔符）、`tree.rel_path(id)`（相对根，永远用 `/`）、`tree.find(path)`、`tree.ref(id)`；`tree.roots` 是根的下标（0 号节点是虚拟的顶层），`len(tree)` 是节点数。路径和层级的细节见 [API 参考](plugin-api.md#api-tree)。常量：`Kind.FILE/DIR/LINK/OTHER`、`Flag.HIDDEN` 等。

节点下标只在这棵树的这一「纪元」里有效：宿主挪动了行之后（监控着的扫描里每多一个文件都可能，见[纪元](plugin-api.md#api-epoch)）旧下标报 `STALE_TREE`。要跨调用保存，存路径，或用 `tree.ref(id)` 得到带路径的 `NodeRef`。

<a id="report-blocks"></a>
## 5. 报告文档

`Report(title=, subtitle=, blocks=[…], actions=[…], footnote=, empty=)`，块用 `r` 构造：

| 块 | 构造 | 显示 |
|---|---|---|
| `md` | `r.md(text)` | Markdown（不显示原始 HTML 和图片，链接只显示地址） |
| `stats` | `r.stats([(标签, 值, 单位[, 提示]), …])` | 一排关键数字 |
| `kv` | `r.kv([(标签, 值[, 单位[, 节点]]), …])` | 详情面板式的两列 |
| `table` | `r.table([r.col(key, 标题, 单位), …], rows=rows, sort=, page_size=50, select=False, more=None)` | 可排序、可勾选；`rows` 是 `query` 的结果或 dict 列表（可带 `node` 和 `actions`）；下一页 `more=r.more(cursor)`（[分页](plugin-api.md#api-paging)） |
| `bars` | `r.bars([(标签, 值[, 单位[, 节点]]), …])` | 横条 |
| `stack` / `ring` | `r.stack(parts, unit)`、`r.ring(parts, unit, center)`，`parts` 是 `(标签, 值[, 色调])` | 堆叠条、圆环 |
| `meter` | `r.meter(value, max, label, unit, tone)` | 仪表 |
| `spark` | `r.spark(values, labels, unit)` | 折线 |
| `nodes` | `r.nodes([id 或 (id, 标签, 大小, 原因)], actions)` | 带图标的节点列表，点击定位 |
| `callout` | `r.callout(text, tone)` | 提示条：`info ok warn danger` |
| `progress` | `r.progress(done, total, text)` | 进度条 |
| `section` | `r.section(title, blocks, collapsed=False)` | 可折叠的一节（≤ 4 层） |

单位：`size count pct date ago duration bytes_per_s text path`；前端按用户的设置格式化（1024 或 1000、日期写法、语言）。值：`pct` 是 0–1 的小数，`date` `ago` 是 unix 秒，`duration` 是秒，`size` 是字节。按钮：`r.action(标签, icon, **动作)`，动作同命令（`cleanup="logs"`、`open="report:x"`、`call="m"` …）。纯文本渲染：`spaceknife_sdk.report.render_text(report)`，`plugin render` 和测试用的就是它。

<a id="panels"></a>
## 6. Web 面板

面板是 `web/` 里的普通网页，没有构建步骤，跑在 `sandbox="allow-scripts"` 的 iframe 里：不透明源、没有网络（`fetch`、WebSocket、WebRTC 都没有）、没有存储和 cookie、不能开子框架和弹窗、只能停留在自己插件的文件里。和程序说话只有一条路：`./_sk/bridge.js`。

```html
<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <link rel="stylesheet" href="./_sk/base.css" />   <!-- 程序的颜色、字体、间距，跟随主题 -->
  </head>
  <body>
    <div id="out" class="card"></div>
    <script type="module" src="./app.js"></script>
  </body>
</html>
```

```js
// @ts-check
import sk from "./_sk/bridge.js";

await sk.ready;                                    // { plugin, lang, theme, context }
const hist = await sk.call("histogram", {});       // 本插件的方法（落在 sk.context 的扫描上）
const sum = await sk.core("tree.summary", { scan_id: sk.context.scan_id, node: 0 });   // 内置方法，参数照协议
sk.on("progress", (p) => …);  sk.state("status", (v) => …);  sk.subscribe("ticks", null, (x) => …);
sk.onContext((ctx) => …);  sk.onLang((lang) => …);  sk.onTheme((mode) => …);  sk.t("中文", "English", { n: 1 });
sk.format.size(1536000);  sk.format.count(1234);  sk.format.date(unixSeconds);   // 和界面一样的写法
sk.host.select(node); sk.host.zoom(node); sk.host.filter("*.log"); sk.host.toast("…");
sk.host.command("my.tool.clean", { rule: "logs" });  sk.host.command("view.visual.flame");   // 本插件的（args 加在它的动作上），和视图、导航、过滤、导出的
sk.context.args;                  // 打开它的 open 动作带的 args（再打开一次时 sk.onContext 收到新的）
sk.view;  sk.onView((v) => …);    // 面板所在视图画的是什么：visual layout color_mode zoom_node selected detail filter …
const { colors, legend } = await sk.colors(nodes);   // 视图给这些节点上的颜色（#rrggbb）和图例，要 tree.read
sk.host.basket([node], "原因");   // 要 basket.propose；sk.host.copy 要 clipboard，open / reveal / url 要 host.open；sk.host.compare(快照, "now") 和 sk.onSnapshots 要 snapshots.read
sk.resize(240);                   // 详情面板和对话框里要的高度
```

- **类型**：`spaceknife plugin build` 写出 `web/sk-plugin.d.ts`（本插件的方法、事件、状态）；JS 文件第一行写 `// @ts-check`，再写 `/** @type {typeof import("./sk-plugin").default} */ const typed = sk;`，编辑器就按它检查参数和返回值；`_sk/sk.d.ts` 是桥本身的类型。标签页里程序已经在网页上方显示了标题，网页里不用再写一遍。
- **视图**：`sk.view` 是面板所在视图（`sk.context.view_id`）现在画的东西：`visual`（可视化 id）`layout` `color_mode` `zoom_node` `zoom_key` `selected` `detail` `filter`（`{ text, active }`）`compare` `size_unit`；换可视化、配色、缩放、选中、过滤时 `sk.onView` 收到新的（马上先收到一次）。`sk.colors(nodes, { mode })` 给出视图按它的配色（或 `mode` 指定的）给这些节点上的颜色，和视图里的一样（主题、用户改过的类别颜色、插件配色的色板都算上）：文件按配色、文件夹用层级色；`legend` 是这些节点里出现的类别、时间档或插件配色的说明。`sk.host.command` 跑视图、导航的命令（`view.color.time` `view.visual.<id>` `view.zoom_in` `nav.up` …）时作用在面板所在的视图上，面板的标签页留在前面。
- **主题**：`base.css` 定义了 `--sk-*` 变量（`--sk-bg` `--sk-text` `--sk-muted` `--sk-accent` `--sk-border` `--sk-ok` `--sk-warn` `--sk-danger` `--sk-font` `--sk-mono` `--sk-radius` …），主题切换时自动更新；`document.documentElement.dataset.skTheme` 是 `dark` 或 `light`。
- **限制**：单条消息 1 MiB，同时 8 个调用，每秒 50 条；`basket.execute`、改设置、开始扫描等都调不到。
- **单独调试（mock 模式）**：`plugin build` 把桥复制到 `web/_sk/`；在 `web/` 里起一个静态服务器（`python -m http.server`），打开 `index.html?mock=fixtures.json`，调用按 `fixtures.json` 回答：`{ "lang": "zh-CN", "theme": "light", "context": {…}, "calls": { "<方法>": <结果> 或 [{ "params": {…}, "result": … }, …] }, "core": { "<内置方法>": <结果> }, "states": {…}, "events": { "<名>": [<载荷>…] } }`。
- **真界面**：`spaceknife plugin shot <文件夹> --open panel:<名> --text`（截图，连面板里的字和报错一起打印；`--step "panel:click:text=应用"` 先在面板里点，[步骤](plugin-api.md#api-shot)），或 `plugin dev`（第 10 节）。

`check` 静态扫描面板代码里的 `RTCPeerConnection`、`RTCDataChannel` 并报错，核对 `fixtures.json` 回答的是不是本插件的方法，`core` 的回答是不是真的长那样。

<a id="permissions"></a>
## 7. 权限、预算与错误码

| 权限 | 允许 |
|---|---|
| `tree.read` | 扫描的名字、路径、大小、时间（`ctx.tree`，以及要树的贡献） |
| `files.read` | 读扫描范围内文件的内容，和扫描来源里用户选的 URI |
| `snapshots.read` | 读已保存的快照 |
| `basket.propose` | 往删除篮里放东西（删除仍然要用户确认） |
| `net` | 连 `net_hosts` 里的主机 |
| `exec` | 运行 `exec` 里的程序 |
| `host.open` | 打开或定位文件、开终端、打开网页 |
| `clipboard` | 写剪贴板 |
| `notify` | 通知（`ctx.notify`），窗口最小化时显示为系统通知 |

- 第一次启用时用户看到全部权限（`net`、`exec` 连同主机和程序），只授予确认过的。文件夹按内容哈希锁定：任何文件变了，插件停在「已改动」，用户重新确认后才再启用（开发链接不锁）。
- 插件进程有审计钩子：没有权限时联网、写插件存储以外的地方、开子进程、`ctypes` 都会被拦下（`PermissionError`）。它不是操作系统级的沙箱，启用对话框也这么写。
- `ctx.call` 和面板的 `sk.core` 只能调这些内置方法：`tree.node tree.children tree.summary tree.find`（`tree.read`）、`scan.list scan.status volume.list view.list visual.list filter.parse filter.vocab classes.get export.targets basket.list extensions.get`、`snapshot.list`（`snapshots.read`）、`compare.info compare.summary compare.node insight.node insight.nodes`（`tree.read`）、`basket.add`（`basket.propose`）。删除、改设置、开始扫描、安装插件永远调不到。

<a id="imports"></a>
### 7.1 能 import 的模块

打包版只带它分析到的模块，所以插件只能用：SDK、`spaceknife_protocol`、numpy、pydantic、`lib/` 里的纯 Python 包，以及标准库的这些：`json re math statistics collections itertools functools dataclasses datetime time zoneinfo hashlib hmac base64 binascii struct zlib gzip bz2 lzma zipfile tarfile csv io pathlib fnmatch glob typing enum decimal fractions heapq bisect textwrap unicodedata uuid tomllib html xml urllib string difflib operator contextlib asyncio concurrent threading contextvars queue random secrets sqlite3 email os abc copy array codecs locale pprint warnings weakref types numbers shlex`。`ctypes cffi socket subprocess multiprocessing ssl select` 不行：联网用 `ctx.net`，运行程序用 `ctx.exec`。`check` 用语法树检查 import。插件文件夹本身在 `sys.path` 上（`from mypkg import x` 找插件里的 `mypkg/`）；自己开线程时用 `contextvars.copy_context().run(...)` 带上调用的语言，或者用 `ctx.compute`。

### 7.2 预算

| | 时限 | 上限 |
|---|---|---|
| 方法 | 默认 30 s，≤ 600 s | JSON 8 MiB，二进制 256 MiB |
| 报告 | 30 s（显示进度，可取消） | 2 MiB，200 块，每表 2000 行，4 层 |
| 代码可视化 / 配色 | 每帧 120 ms / 20 ms | 失败三次后这次会话不再用 |
| 字段 | 详情 1 s，悬停 30 ms | 16 个 / 4 个，每个 ≤ 200 字 |
| 节点集合 | 5 s | 每节点 1 bit |
| 清理 | 120 s | 10 万项 |
| 导出 | 30 s 没有新块就中止 | 每块 1 MiB |
| 后台任务 | 不限（`timeout=`） | 每个插件同时 8 个；结束的留 8 个给界面看 |
| 进程 | 启动 10 s；空闲 10 分钟停止（有后台任务时不停）；崩溃后 1 / 5 / 30 秒重启，5 分钟内 3 次就停用 | 内存 1 GiB；同时 4 个调用（`limits.threads` 更多时随它）、排队 32 个；普通 `def` 用 2 个线程（`limits.threads`）；最多 8 个进程 |

### 7.3 错误码

`-32014 PERMISSION_DENIED`（少了权限）、`-32015 PLUGIN_TIMEOUT`、`-32016 PLUGIN_UNAVAILABLE`（进程起不来、崩溃、卡死）、`-32017 PLUGIN_DISABLED`、`-32018 STALE_TREE`（下标过期）、`-32019 PLUGIN_ERROR`（`PluginError` 的文字原样给用户）、`-32020 PLUGIN_LIMIT`（并发、排队或输出超限）。插件代码里对应的异常见 [API 参考](plugin-api.md#api-errors)。

<a id="testing"></a>
## 8. 测试

`spaceknife_sdk.testing.MockHost` 用真正的宿主组件跑插件（扩展注册表、插件宿主、运行时、真实的 `NodeStore`），只是没有窗口和网络连接，数据在临时文件夹里。测试里的行为和程序里一样。

```python
from spaceknife_sdk.testing import MockHost, make_tree


async def test_largest() -> None:
    async with MockHost.load(__file__) as host:  # 测试文件所在的插件；process=True 走真的插件进程
        host.scan(make_tree({"a.iso": (200 << 20, "2020-01-01"), "b.txt": 10, "sub": {"c.log": b"hello"}}))
        report = await host.report("largest")
        assert "a.iso" in host.text(report)
        await host.check(strict=True)  # 和 plugin check 一样的检查
```

`make_tree` 的值和 `host` 的全部方法见 [API 参考](plugin-api.md#api-testing)：命令按菜单那样跑（`await host.command(name, node=…)`）、问能不能跑（`command_available`）、等后台做完（`settled()`）、系统写法的路径（`host.path("a/b")`）、`export_text` / `export_bytes`。失败的调用抛 `spaceknife_sdk.RequestError`（`.code` 是 `ErrorCode`）。要直接测插件自己的函数，用 `plugin_module(__file__)` 导入 `plugin.py`（另一份副本：看不到宿主里那份的状态）。默认在测试的进程里跑（快）；`plugin new` 生成的测试里另有一项 `process=True`，在真的插件进程里再查一遍——剪贴板、沙箱、卡住事件循环这类问题只在那里出现。`grant=["tree.read"]` 可以只给部分权限，测「缺权限时报 `PERMISSION_DENIED`」。测试需要 pytest 和 pytest-asyncio（SpaceKnife 的开发环境里都有）；`plugin new` 生成的 `pytest.ini` 打开了 asyncio 模式。

<a id="build"></a>
## 9. 构建、打包与安装

- `spaceknife plugin build`：写 `generated/schema.json`（本插件的协议 schema，模型名带插件前缀，如 `ExampleDupesFindResult`）、`web/sk-plugin.d.ts`、`generated/pinyin.json`（命令标题的拼音，命令面板按拼音找）、`web/_sk/`（桥的副本，mock 模式用），并列出缺英文的文案。`check` 报告这些文件缺了或过期了。`--zip`：`dist/<id>-<version>.zip`，里面一个 `<id>/` 文件夹。
- 安装：`spaceknife plugin install <文件夹或 zip>`，或插件页「从文件夹 / zip 安装」。装好的插件在 `<数据目录>/plugins/<id>/`，先是关着的；启用时确认权限。同 id 的要 `--replace`。卸载把文件夹移进回收站（`--data` 连数据一起）。zip 安装会检查：没有绝对路径、`..`、链接和设备文件，≤ 2000 个文件、64 MiB，压缩比 ≤ 100。签名、插件目录和从 Git 安装以后做。

<a id="cli"></a>
## 10. 命令行

| 命令 | 做什么 |
|---|---|
| `plugin new <id> [--with 部分,…] [--dir D] [--title 名字]` | 生成能跑、已构建、能通过 `check --strict` 和自带测试的骨架 |
| `plugin check [D] [--strict] [--json] [--no-contracts] [--inprocess]` | 全部检查；`--json` 给工具和 agent；退出码 1 表示有问题 |
| `plugin build [D] [--zip]` | 第 9 节 |
| `plugin call <D或id> <方法> ['{"a": 1}'] [--scan PATH] [--snapshot FILE] [--history FILE …] [--setting k=v …] [--lang en]` | 起一个临时后端，扫描 `PATH`（不给就用合成树或 `fixtures/tree/`），调用方法，打印 JSON 结果，事件、提示和插件日志打到 stderr |
| `plugin render <D或id> <report:名 \| field[:名] \| insight \| cleanup:名 \| export:名 \| set:名 \| visual:名 \| color:名 \| source:名> [--scan PATH] [--node a/b] [--format text\|json\|md] [--call 方法[={json}] …] [--option k=v …] [--filter 式] [--uri U] [-o FILE] [--setting k=v] [--lang en] [--history FILE]` | 不开界面看到结果；`--call` 先在同一个后端里调方法（先算好结果的字段、集合）；导出的 `--option` `--filter`、`--node`（只导出它）；`visual:` `color:` 的 `--format json` 给出每行的节点、大小和色板下标 |
| `plugin shot [D] [--open report:名\|panel:名\|export:格式\|settings] [--do 命令 …] [--select a/b] [--step 步骤 …] [--setting k=v …] [--history FILE …] [--source 名:URI] [--theme light\|dark] [--lang en] [--size 1400x900] [-o PNG] [--text] [--eval JS] [--scan PATH] [--ui DIR]` | 无头浏览器里真界面的截图（不开窗口）：自己的临时后端、数据目录和端口；`--do` 是本插件的命令名、`visual:名`、`color:名`、`export` 或任意命令 id；`--step` 截图前按顺序点按、填写、按键、在面板里操作（[步骤](plugin-api.md#api-shot)），`--eval` 在截图前执行；打印 PNG 的位置、页面和插件的报错，`--text` 还有页面（含面板）上的字 |
| `plugin dev [D] [--scan PATH] [--port N] [--ui DIR] [--home DIR]` | 后端加开发链接，保存就重新加载并通知前端；有构建好的前端时打印浏览器网址（打开就是 `--scan` 的扫描），前端比源码旧时提醒；停它用打印的 pid（`kill <pid>`、`Stop-Process -Id <pid>`），别用会误杀的 `pkill -f` |
| `plugin install <D\|zip> [--replace]`、`uninstall <id> [--data]`、`list [--json]`、`enable <id> [--yes]`、`disable <id>` | 安装、卸载（真实的数据目录）；列出、启用（列出权限并要求确认）、停用 |
| `plugin schema [--manifest \| D]` | 清单的 JSON Schema，或插件自己的协议 schema |
| `plugin docs [--section 锚点] [--search 词] [--list] [--guide] [--api]` | 速查（不带参数）；一节；提到某个词的行；全文 |

`call`、`render`、`check` 的契约检查默认在插件进程里跑（和程序一样），`--inprocess` 在命令自己的进程里跑（快，回溯直接打印）。`shot`、`dev` 的前端：`--ui`，或环境变量 `SPACEKNIFE_UI`（`pnpm -C apps/electron build` 的 `out/`）；浏览器：Chrome、Chromium 或 Edge，`SPACEKNIFE_BROWSER` 指定。`call` 遇到报告、导出这类名字时提示该用 `render` 的哪种写法。

<a id="errors"></a>
## 11. 检查与常见错误

`plugin check` 依次检查：清单；在一个没有权限的进程里 import 并读出声明；文案中英齐全、占位符一致；图标；颜色；import；代码用到的 API 和声明的权限；洞察规则、模板、过滤预设；按键冲突；然后在插件进程里把每个带代码的贡献在合成树上跑一次（可视化在三棵树上按行契约检查；配色的下标、导出、报告的上限、字段、清理、集合、钩子、`fixtures/` 里有样例的扫描来源并打印它列出了什么；命令按它的 `when` 和 `check` 在文件夹、文件和不选时各跑一次，要 `net` / `exec` 的插件不跑）；最后看生成的文件是否最新。

| 错误码 | 意思 | 改法 |
|---|---|---|
| `unknown_field` | 清单或某项贡献里有不认识的字段 | 看提示「是不是想写 …」；完整字段见本文对应一节 |
| `invalid` / `toml` / `no_manifest` | 值不合法（id 格式、semver、`when` 的词、动作……）/ TOML 语法错 / 没有清单 | 按消息改；`plugin schema --manifest` |
| `import` / `describe` | import 失败或读声明失败；消息带文件和行 | `plugin check` 的 `fix` 是异常的最后一行；`plugin render … --inprocess` 打印完整回溯 |
| `untyped` | 方法的第二个参数没标 `Params` 模型 | `async def f(ctx: Context, p: MyParams) -> MyResult` |
| `no_code` / `declared_twice` / `duplicate` | 要代码的贡献没有函数 / 同一项写了两处或两次 | 加装饰器或改成声明式（配色写 `by`）/ 留一处 |
| `missing_en` / `placeholders` | 缺英文 / 两种语言的占位符不同 | 1.3 节 |
| `icon` `bad_color` `low_contrast` `same_color` / `forbidden_import` `unknown_import` | 1.4、1.5 节 / 7.1 节 | 用 `ctx.net` / `ctx.exec`；依赖放进 `lib/` |
| `missing_permission` / `unused_permission` | 代码用了没声明的权限 / 声明了没用的 | 改 `permissions` |
| `rules` / `template` / `filter` | 规则文件、模板、过滤式有错 | 见 insights.md、export-templates.md、filter-syntax.md |
| `key_conflict` / `webrtc` | 和程序或本插件的键冲突 / 面板代码用了 WebRTC | 换键或 `override = true` / 走 `sk.call` |
| `register` | 宿主拒绝了某项贡献（重名、对不上） | 按消息改 |
| `visual` `color` `export` `report` `field` `cleanup` `node_set` `hook` `source` `command` | 跑这项贡献时出错或违约 | 用 `plugin render` / `call` 重现 |
| `not_built` / `stale_generated` | 生成的文件缺了或过期 | `spaceknife plugin build` |

运行时常见的：`PERMISSION_DENIED`（少了权限：清单里加上，重新启用）；`STALE_TREE`（存了节点下标又用：存 `tree.ref(id)`）；`PLUGIN_TIMEOUT`（把重活放进 `await ctx.compute(...)`，长循环里调 `ctx.check()`）；「需要先完成扫描」（命令加 `when = ["done"]`）。

<a id="examples"></a>
## 12. 参考插件

| 插件 | 位置 | 看点 |
|---|---|---|
| `example.dupes` | `examples/plugins/example.dupes/` | 代码插件（独立进程）：方法、事件、共享状态、报告、清理、命令和状态行、按键、设置、节点字段、`:set:dupes`、后台任务；右键菜单里的「只保留这一份」（按 `when` 和 `check` 变灰，带参数运行清理）；读文件内容算哈希（`files.read`），重活在 `ctx.compute` 里 |
| `example.devpack` | `examples/plugins/example.devpack/` | 纯声明式：洞察规则、文件类别、文件夹规则、过滤预设和按键、导出模板、按扩展名的配色、按文件数的火焰图 |
| `example.panel` | `examples/plugins/example.panel/` | Web 面板：按修改年份的条形图，`sk.call` 和 `sk.core`，主题，`fixtures.json` 的 mock 模式 |
| `example.shapes` | `examples/plugins/example.shapes/` | 自带布局的可视化：圆堆积（`shapes.pack`，可以画成六边形多边形）和自己写的冰柱图，自己的 SVG 图标，不要任何权限 |
| `builtin.export` | `packages/backend/src/spaceknife/plugins/builtin/builtin.export/` | 程序自带的五个导出模板（声明）和 CSV / JSON / HTML（`@plugin.export_format`） |
| `builtin.insights` | `packages/backend/src/spaceknife/plugins/builtin/builtin.insights/` | 程序自带的洞察规则，一个纯声明式插件 |
