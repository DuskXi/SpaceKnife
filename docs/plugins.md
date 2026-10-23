# 插件系统 —— 设计与现状

状态：**已实现**：后端（第一阶段）、Electron 前端与 Web 面板（第二阶段）、命令行、作者文档和参考插件（第三阶段）。下面先是总览和三节「实现状态」，然后是动工前写下的经验与要求（第 1–8 节，保留作为依据），最后是第 9 节定下的决定。写插件的人只需要读 [plugin-dev.md](plugin-dev.md)。

## 总览：现在有什么

| 给谁 | 什么 | 在哪里 |
|---|---|---|
| 插件作者 | 作者文档：五分钟上手、每种贡献的字段表和例子、权限、预算、测试、打包、命令行、错误码 | [plugin-dev.md](plugin-dev.md)；`spaceknife plugin docs [--section 锚点]` |
| 插件作者 | 命令行 `spaceknife plugin new/check/build/call/render/dev/install/uninstall/list/enable/disable/schema/docs`（打包版：`spaceknife-backend plugin …`） | `packages/backend/src/spaceknife/cli_plugin.py` |
| 插件作者 | SDK：`Plugin` 与装饰器、`T`、`Context`、`TreeView`/`Rows`/`Node`、报告块 `r`、`layout`、结果模型 | `packages/sdk/src/spaceknife_sdk/`（`__init__.py` 的 `__all__`） |
| 插件作者 | 测试与工具：`testing.MockHost`/`make_tree`、`check`、`schema.export_plugin_schema`、`tsgen.generate_plugin`、`scaffold`（`plugin new` 的模板）、面板桥的副本 `web/` | 同上 |
| 插件作者 | 参考插件 | `examples/plugins/example.dupes`（代码）、`example.devpack`（纯声明式）、`example.panel`（Web 面板） |
| 程序自己 | 内置插件：导出模板和 CSV / JSON / HTML（`builtin.export`），洞察规则（`builtin.insights`），与普通插件同一套声明和注册 | `packages/backend/src/spaceknife/plugins/builtin/` |
| 宿主 | 扩展注册表与扩展点；发现、信任、安装；运行时（插件进程、进程内）；树镜像与查询；检查与构建；浏览器模式的开发服务器 | `packages/backend/src/spaceknife/extensions/`、`plugins/`、`devui.py` |
| 前端 | 插件数据、动作与键位、报告与放置位置、字段与清理复核、配色、插件页、Web 面板与桥 | `apps/electron/src/renderer/src/plugins/`、`src/main/plugin-files.ts`、`resources/plugin-sdk/` |

## 实现状态（第一阶段：后端）

已有：

- **协议 1.2**：能力 `plugins.v1`、`colors.v1`、`panels.v1`，服务端能力从注册表推导；插件管理方法 `plugin.list/describe/enable/disable/restart/install/uninstall/link`、`extensions.get`、`plugin.fields`、`insight.node`，事件 `extensions.changed`、`plugin.changed`、`plugin.toast`、`plugin.progress`；报告的 13 种块、12 种动作。
- **统一扩展注册表**（`spaceknife/extensions/`）：每类扩展一个点，带检查、按插件整体撤销、版本号；可视化、配色、导出格式与模板、洞察规则、文件类别、文件夹规则、节点集合、过滤预设、清理、扫描来源、钩子、命令、按键、设置、字段等 27 类贡献都走它。内置的颜色模式和可视化命令也从这里生成，命令与按键只在一处声明（`palette/keymap.py`）。
- **SDK**（`packages/sdk`，`spaceknife_sdk`）：清单模型与校验（带「是不是想写」的提示）、`Plugin` 装饰器、只读目录树（`TreeView`：列、查询、节点）、报告构造、篮子提议、上下文里的宿主服务、`testing.MockHost`（不起后端就能测插件）。
- **运行方式**：每个插件一个进程（复用助手的传输层：命名管道 / Unix 套接字，一次性令牌，核对对端进程号），Windows 用作业对象、Linux 用 rlimit 和 prctl 限内存与进程，审计钩子挡网络、写文件、起进程、ctypes；开发模式（`advanced.plugin_dev_mode` 或环境变量 `SPACEKNIFE_PLUGIN_DEV`）可在后端进程内跑。调用超时、取消后宽限 5 秒再结束进程、崩溃退避（1 / 5 / 30 秒，5 分钟内 3 次停用）、空闲 10 分钟停进程、最多 8 个进程、每个插件 4 个并发 32 个排队。
- **目录树**：宿主端查询 `host.query`（过滤表达式、范围、类型、排序、条数），以及按需导出的只读共享内存列（节点编号带版本号，树变了旧编号报 `STALE_TREE`）。
- **权限与信任**：`config_dir/plugins.json`，权限在清单里声明，首次启用时连同权限一起确认，只授予确认过的那些；按文件夹内容哈希锁定，内容一变就要重新确认。
- **设置**：每个插件一个命名空间 `plugin.<id>.<name>`，逐项校验与回退；插件不在时它的值原样留在设置文件里。
- **扫描来源的实时一面**：来源的扫描有自己的监控（`plugins/sources.py` 的 `SourceMonitor`）。重新扫描就是把来源重跑一遍、换上新树，视图按路径保留位置；来源写了 `watch = true`、插件有对应的 `watcher` 时，变化安静 1 秒后自动重跑（10 万项以内，更大的只提示需要重新扫描）。平台监视器改成了注册表（`monitor/base.py` 的 `register_watcher`，按平台、文件系统和优先级选，内置四种行为不变）。
- **对比原因**：对比侧栏和悬停卡的变化项，洞察规则没给原因的，由插件的 `change_annotator` 成批补上（每批 500 项、200 ms，按对比缓存）。
- **快照**：`ctx.snapshots.list()` 只读文件头；`ctx.snapshots.open()` 载入整份快照，每个插件同时最多 2 个，调用结束、`close()` 或进程停止时释放。插件打开的快照和树镜像计入内存看门狗（[memory.md](memory.md) 第 11 节）。
- **流和监控器**不受 600 秒的调用上限，直到退订或停止；关闭后端时不再启动插件进程。卸载插件把它的文件夹移进回收站（只限插件文件夹，数据目录本身仍受保护）。

1 000 001 个节点（`tools/bench_plugins.py`，同一台机器）：

| | Linux（WSL2） | Windows |
|---|---|---|
| 插件进程启动 / 常驻内存 | 1.5 秒 / 50 MB | 0.7 秒 / 50 MB |
| 一次调用往返 | 0.4 ms | 0.6 ms |
| 导出 7 列共享内存（35 MB） | 20 ms | 12 ms |
| 插件第一次映射 / 之后 | 22 ms / 2 ms | 16 ms / 2 ms |
| 宿主端查询（匹配 99.9 万，取前 1000 条带路径） | 155 ms | 107 ms |
| 代码可视化第一帧（插件算权重）/ 之后每帧 | 126 ms / 1.5 ms | 111 ms / 1.2 ms |

还很薄或未做：

- WASM 运行方式没有做原型；代码可视化只是「插件给每个文件一个权重，内置引擎来画」。
- 扫描来源列完才显示（不是边扫边画），重跑也是整个重跑；监控器只用于插件自己的来源，还没有按文件系统给普通扫描提供监控器的插件接口。
- 文件夹规则的叠加、代码洞察都只有最基本的部分；悬停字段只读缓存（没有的发一次请求，下次悬停才显示）。
- macOS 的内存上限只靠心跳检查。
- 每个插件的协议模式导出、`plugin build`、命令行和内置插件：第三阶段已做（见下面第三节）。

## 实现状态（第二阶段：Electron 前端）

浏览器模式（无头模式、演示服务器）用的是同一份渲染进程代码，下面说的两边都有，除非另外注明。文件在 `apps/electron/src/renderer/src/plugins/`（下称 `plugins/`）。

已有：

- **连接**：`Rpc` 公开 `callRaw`、`callFrames`（带二进制列帧的结果）和 `onRaw`；插件的帧（`plugin.<id>.<名>`）不进视图帧的配对器。hello 声明 `plugins.v1 colors.v1 panels.v1`。`plugins/client.ts` 是 `callPlugin`、`onPluginEvent`、`subscribePlugin` 和类型化的 `pluginClient<Api>(id)`；`plugins/store.ts` 在连接时、收到 `extensions.changed`（按 kinds 只取需要的）、语言变了时重新取 `extensions.get`、`palette.commands`、`plugin.list`。
- **动作与单一键位表**：`plugins/actions.ts` 执行 12 种动作，插件的动作只能调自己的方法、改自己的设置（加几项显示设置），`url` 和打开定位要 `host.open`、复制要 `clipboard`，删除只到删除篮。内置命令里通用的（配色、可视化、轮换、开关）由后端带上动作，其余是 `builtin`，`state/commands.ts` 按命令 id 处理。`plugins/keymap.ts` 用 `palette.commands` 的键建表，`App.tsx` 里手写的全局按键没有了：窗口自己的键只剩命令面板、切换标签页、帮助和设置页的搜索与历史、视图里的 Esc / Enter / 菜单键。旧行为由 `keymap.test.ts` 锁住。
- **放置位置**（`plugins/slots.tsx`）：标签页（`TabKind` 加 `plugin`）、详情面板的插件页（图标页签）、对话框、开始页卡片（以及不需要扫描的面板和历史报告的入口）、对比侧栏的一节、状态栏项。每处按 `when` 显示不可用的原因（`plugins/when.ts`）。
- **报告**（`plugins/report/`）：13 种块都用应用自己的部件画（Markdown 关闭原始 HTML、不显示图片、链接只显示地址；表格可排序、每页最多 50 行、可勾选、`more` 续读；圆环、堆叠条、仪表、折线）。按 `refresh` 重新取（选中项、扫描完成、帧每秒最多一次、对比、`interval:<秒>`、`state:<名>`、手动），只算最新，显示 `plugin.progress` 的进度。节点链接按路径重新解析后在视图里选中。
- **字段、洞察、清理**：详情页顶部是洞察卡片（`insight.node`；命令只显示、可复制），内置信息之后是「插件」字段；悬停卡只显示缓存里的字段。清理先进复核对话框（勾选、合计、原因），用户点了才 `basket.add {origin, reasons}`；删除篮每项显示「来自〈插件〉：〈原因〉」。
- **配色**：帧里的 `pal` 列解码进 `VisualFrame.pal`，三种可视化的画法按色板取色（中性色兜底），`folders = "palette"` 时火焰图和极坐标火焰图的文件夹条也上色；工具栏配色分段后多一个「插件」下拉（带图例），C 键轮换包括插件配色，不适用于当前布局的列出但不可选。按文件数的可视化变体（`size_unit = "count"`）标签和悬停卡显示项数。
- **插件页**（设置 › 插件，`pages/PluginsPage.tsx`）：列表、状态标签、开关；第一次启用（或文件改动后）先弹确认，列出权限（`net`/`exec` 的主机和程序、新增的权限醒目）、贡献摘要、键冲突、未签名提示和「这不是操作系统级沙箱」；从文件夹或 zip 安装（同 id 时确认替换），重启、卸载、错误和 stderr 尾部、内存；开发者模式下可以链接开发文件夹。插件自己的设置作为这一页的分组，用现有的设置控件。快捷键页显示命令来自哪个插件。
- **开始页的扫描来源**：按 `pick` 打开文件或文件夹对话框，或者输入；`scan.start {source, uri}`；「重新扫描全部」对来源的扫描是把来源重跑一遍。
- **Web 面板**：
  - Electron：`skplugin://<id>/` 协议（`main/plugin-files.ts`），文件来自开发链接、已安装或内置插件的 `web/`，`/_sk/` 是程序自带的 `resources/plugin-sdk/`（`bridge.js`、`base.css`、`sk.d.ts`、`guard.js`）。每个响应带面板的内容安全策略（没有网络、子框架、表单、worker，`sandbox allow-scripts`，`frame-ancestors` 只有应用自己），HTML 最前面插入 `guard.js`。`will-frame-navigate` 只放行面板留在自己插件的文件里，WebRTC 的 IP 处理策略为 `disable_non_proxied_udp`，面板不能申请任何权限（摄像头、通知……）。后端 ready 行加了 `plugins_dir`、`builtin_plugins_dir`、`plugins_trust`。
  - 浏览器模式：无头模式和演示服务器（`tools/demo/m3_web.py`）提供 `/plugins/<id>/web/…`，策略相同，`script-src` 限在这个插件的路径下；主页面的 `frame-src` 改为只允许本服务器。
  - 父页面一侧（`plugins/panel/`）：`sandbox="allow-scripts"` 的 iframe，只认这个 iframe 窗口的 hello，用 MessageChannel 交出一个端口；面板说出自己的地址，不在本插件文件里的就断开。桥只放行本插件的方法、白名单里的内置方法（各按权限；`basket.add` 强制带上面板的来源）、本插件的事件 / 状态 / 流，以及按权限的宿主操作；单条 1 MiB、同时 8 个调用、每秒 50 条；列结果以转移方式交出。`bridge.js` 有 mock 模式（`?mock=fixtures.json`）。
- **生成的类型**：`generate_plugin(schema)`（第三阶段起在 `spaceknife_sdk/tsgen.py`，`tools/gen_ts_protocol.py` 共用）按插件输出 `web/sk-plugin.d.ts`（`Methods`/`Events`/`States` 与 `Sk<…>`），由 `plugin build` 写出。
- **文案**：新的界面文字都在 `locales/en/plugins.json`；`tools/i18n_check.py --area electron` 通过。
- **测试**（vitest）：`plugins/keymap.test.ts`、`actions.test.ts`、`colors.test.ts`（含 `when`）、`report/report.test.ts`、`panel/bridge.test.ts`（用 `plugins/testing/mockBridge.ts`）、`protocol/rpc.test.ts` 的插件部分、`main/plugin-files.test.ts`、`main/plugin-sdk.test.ts`、`main/headless-server.test.ts` 的面板路由。

还很薄或未做：

- 对比侧栏和状态栏的插件报告只做了基本的；工具栏「更多」里的插件命令、开始页的快捷操作同样很薄。
- 详情面板的插件页没有按 `summaryInterval` 节流（跟随选中项、帧最多每秒一次）。
- Web 面板的 `bridge.js` mock 模式要在 SpaceKnife 之外的静态服务器上打开（SpaceKnife 自己的路由不给面板网络）；第三阶段起 `plugin build` 把桥复制到 `web/_sk/`，`plugin dev` 在浏览器模式里连真后端。
- WebRTC：Electron 和浏览器模式都靠 `guard.js` 删掉构造函数、拦下子框架，加上 CSP 的 `webrtc 'block'`（浏览器认识时生效）；浏览器模式没有 IP 处理策略这一层，启用对话框里写明了这项残余风险。
- 实测（Electron 44，Windows，用 DevTools 协议驱动打包前的界面，测试插件的面板放在详情面板里）：面板的源是 `null`；`RTCPeerConnection`、`webkitRTCPeerConnection`、`WebSocket` 都不存在；用 `DOMParser` 加 `adoptNode` 绕过 `createElement` 造出的子框架是另一个不透明源，读它的 `RTCPeerConnection` 报跨源错误，随后被移除；`fetch`、`localStorage`、`cookie`、父页面都拿不到；桥拒绝 `basket.execute`、`settings.update`、没授权的 `snapshot.list` 和 `reveal`、别的插件的方法（都是 -32014）。面板直接跳到别的插件的面板被 `will-frame-navigate` 拦下；跳到外部网址被父页面的 `frame-src` 拦下（停在错误页），再从错误页跳到别的插件同样被拦。浏览器模式的面板路由由 `headless-server.test.ts` 覆盖；手动走查用的浏览器自己拦掉了所有子框架，没能在那里看到面板的画面（其余功能在浏览器模式里都走过一遍）。
- Qt / WinUI3 前端没有做（它们不声明 `plugins.v1`，后端照旧工作）。

## 实现状态（第三阶段：开发体验与参考插件）

已有：

- **作者文档** [plugin-dev.md](plugin-dev.md)（≤ 1000 行，上手部分 ≤ 100 行）：每节有固定锚点，`plugin check` 的每个问题和 `plugin docs --section` 都用它们。文档里标了 `ex` 的例子由 `tests/test_plugin_docs.py` 拼成一个插件跑过 `check`；每种贡献的每个字段都必须出现在它那一节（同一个测试）；ruff 也格式化文档里的 Python 块。
- **命令行**（`cli_plugin.py`，`spaceknife plugin …`）：
  - `new`：`spaceknife_sdk/scaffold.py` 的 14 个部分（`method report command setting cleanup panel visual color export source set field insight hook`），生成后马上 `build`；任一部分单独、全部一起都能通过 `check --strict` 和自带的测试，代码过 ruff（`test_plugin_cli.py`）。
  - `check`（`plugins/check.py`）：清单；在没有权限的插件进程里 import 并读声明（`plugins/child.py --describe`）；文案、图标、颜色（对比度、重复）、import 白名单、代码用到的 API 与权限（语法树）、规则、模板、过滤预设（认得本插件的类别）、按键冲突、面板里的 WebRTC；契约：插件放进 `MockHost`（默认在插件进程里），可视化在三棵合成树上按行契约检查，配色、导出、报告（上限）、字段、清理、集合、钩子和带样例的扫描来源在一棵上各跑一次；面板的 `fixtures.json` 核对方法名；最后看生成的文件。每个问题有文件和行、错误码、改法和文档锚点；`--json` 给工具和 agent。
  - `build`（`plugins/build.py`）：`generated/schema.json`（`spaceknife_sdk/schema.py`，模型名带插件的 PascalCase 前缀）、`web/sk-plugin.d.ts`、`generated/pinyin.json`、`web/_sk/` 面板桥副本，列出缺英文的文案；`--zip` 打成 `dist/<id>-<version>.zip`。
  - `call` / `render`：在临时数据目录的 `MockHost` 里跑（默认插件进程），扫描 `--scan` 的文件夹、`--snapshot` 或合成树；`render` 支持报告、字段、洞察、清理候选、导出、节点集合、可视化和配色。
  - `dev`（`devui.py`）：后端加开发链接，监视文件夹，保存后重新描述、重启并广播 `extensions.changed`；仓库里构建过前端时同时提供浏览器模式（与 `tools/demo/m3_web.py` 共用同一个面板路由）。
  - `install/uninstall/list/enable/disable`：用真实数据目录里的插件宿主（不开端口）；`enable` 在终端列出权限并确认（`--yes` 给脚本）。`schema [--manifest | DIR]`、`docs`。
- **SDK 补全**：`MockHost.check()`（同 `plugin check`，在测试里跑）、`set_setting` 在下一次调用前送到插件进程；`layout`（代码可视化的权重帮手）；`check`；`schema`；`tsgen`；`docs`；`ExportInfo`（导出格式可选的第四个参数，带过滤式）；`MethodDecl.doc`（方法文档串的第一段，进 schema 和 `.d.ts`）；`ExportTemplateSpec.spacesniffer`。
- **内置插件**（`plugins/builtin/`，`plugins/builtins.py`）：随程序发布的插件在第一次用到时注册（不需要服务器：命令行导出也用它们），用同一个 SDK 加载、`describe`、经同一套适配器注册，属主是自己的 id；不能停用，不锁哈希，不起进程（只允许不需要会话的贡献：声明式的各类和导出格式；导出格式的函数在导出线程里直接调用，树是 `plugins/local.py` 的本地视图）。
  - `builtin.export`：五个模板是 `[[export_templates]]`（名字仍是模板的 `@name`，即 SpaceSniffer 的名字，大小写不敏感），三种格式是 `@plugin.export_format`，函数体调用原来的 `iter_csv/iter_json/iter_html`（内置插件可以用 `tree.store`）。`export.BUILTIN_INFO / BUILTIN_TEMPLATES / FORMATS`、`resolve`、`export.targets` 都从注册表读。金样：`test_export.py`、`test_cli_run.py` 一字未改并通过；`export.targets` 在两种语言下与迁移前的输出逐项相同（`tests/data/export_targets.golden.json`）。
  - `builtin.insights`：纯声明式，`insights/builtin.toml`。规则来源的顺序是内置插件 → 其他插件（按 id）→ 用户文件夹；`test_insights.py` 一字未改并通过，新测试验证插件的规则夹在中间。
  - 注册表排序：原生内置 → 内置插件 → 其他插件。
- **参考插件**（`examples/plugins/`，pytest 直接收集它们的 `tests/`）：`example.dupes`（方法、事件、共享状态、命令状态行、报告、清理、按键、设置、字段、`:set:dupes`，哈希在 `ctx.compute` 里）、`example.devpack`（规则、类别、文件夹规则、过滤预设与按键、导出模板、按扩展名的配色、按文件数的火焰图）、`example.panel`（按修改年份的条形图面板，`sk.call` / `sk.core`，`fixtures.json` 与方法的结果模型一致）。`tests/test_plugin_examples.py` 对它们和内置插件跑 `check --strict`，并确认示例只用公开的 SDK。
- **顺带修好的第一、二阶段问题**：
  - 插件进程的审计钩子不随新的扫描更新可读的根，进程启动后扫描的文件夹 `files.read` 读不了；现在随 `roots` 一起更新。
  - 扫描来源读不了用户选的文件（必须在已扫描的文件夹里）；现在来源和监控器调用带的 URI 在有 `files.read` 时可读。
  - 过滤预设引用同一插件声明的类别时解析失败。
  - Windows 上映射一个已经不存在的共享内存名会静默得到全零：镜像开头现在有宿主写的 16 字节头（`SKIMG1` 加段名），插件映射后先核对，对不上就再要一次，仍不对报 `UNKNOWN_SCAN`。
- **冻结版实测**（Windows，PyInstaller 6.22.3，2026-10-23，只冻结后端、没打安装包）：`spaceknife-backend plugin list` 列出两个内置插件；对三个示例 `plugin check --strict` 全部通过（读声明和契约都走 `--plugin-host` 进程）；`plugin new` → `check --strict` → `render` → `build --zip` → `install` → `enable --yes` → `uninstall` 一路通过；`run scan … export csv` 和 `export "Simple file list"` 经内置插件导出；`plugin docs --section` 读到打包进去的文档。第一次实测时一个 import 白名单里全部标准库模块的插件报 `No module named 'sqlite3'`：spec 现在按 `spaceknife_sdk/imports.py` 的白名单把这些模块列为隐式导入，重新冻结后这个插件能读声明、能调用。
- **面板桥只有一份内容**：`packages/sdk/src/spaceknife_sdk/web/` 是 `apps/electron/resources/plugin-sdk/` 的副本（`plugin build`、`plugin dev` 用），测试保证两者逐字节相同。

还很薄或未做：

- 文档的验证：原计划派一个只读 `plugin-dev.md` 的新 agent 写一个小插件、记下 token 与弯路；这一轮没有能派子 agent 的工具，改为照着第 0 节在干净的目录里走了一遍：五条命令都一次通过（WSL 与 Windows）。新 agent 的演练留到下一轮。
- `check` 的契约只跑一次、用固定的三棵合成树；扫描来源要有 `fixtures/` 样例才跑；监控器和对比原因不跑。
- `plugin dev` 的浏览器模式需要仓库里构建好的前端；打包版的程序里请用开发者模式链接文件夹。
- 内置插件只能有声明式贡献和导出格式；需要运行时的内置插件（报告、方法……）以后再说。
- 以后做：WASM 运行时、签名、插件目录、从 Git 安装、操作系统级的沙箱、插件之间的依赖、新的布局格式、Electron 主进程插件。

---

以下第 1–8 节是动工前写下的经验与要求，保留作为设计依据；实现与它们不同的地方以上面三节为准。

## 1. 目标

- **一个插件就是一个包**：可以只有后端部分，可以只有前端部分，也可以两者都有。
- **开发者不用操心通讯**：WebSocket、二进制帧、协议 schema 与 TypeScript 类型生成、能力协商、Electron 的 IPC 和 preload，都不用插件开发者管。
  - 例外：插件本身就是要改 Electron 的（托盘、全局快捷键、原生菜单、窗口行为），才需要碰 Electron。
- **跨平台**：Windows、Linux、macOS 用同一份插件。
- **前端一致**：Electron 现在一份，Qt 和 WinUI3 恢复后也要能用，画面保持一致（见 [PLAN.md](PLAN.md) 的一致性要求）。
- **默认安全，资源有界**：插件出错、卡住或有恶意，都不能拖垮扫描和界面，也不能偷偷删文件或往外发数据。

## 2. 可视化组件化留下的经验

### 2.1 插件交出数据，不交界面代码

可视化分三层，每层都可以单独替换：

- 后端引擎：`Engine` 加上 `Visualization` 描述符；
- 线上布局格式：`drawlist.v1`、`flame.v1`、`polar.v1`；
- 前端组件：`VisualComponent`。

只写后端引擎的插件，产出一种已有的布局格式，所有前端都不用改，画面自然一致。

推广开来：**前端能画的东西应当是少数几种通用的「形状」，由我们维护；插件往这些形状里填数据。** 布局格式（矩形、条带、极坐标）就是第一批这样的形状。设置页按元数据自动渲染（`settings.py` 里的 `S(...)`），是另一个现成的例子。

### 2.2 契约加一致性测试，才是真正的接口

- 后端的行契约 1–8 由 `layout/contract.py` 的 `check_draw` 检查；前端的几何一致性由 `visual/testing/conformance.ts` 检查。
- 两者都按注册表参数化：任何东西一注册，就会被同一套测试检查。

这套检查本身就是插件开发包的核心，应当原样发给插件开发者（第 7 节）。只给接口签名、不给契约检查，插件质量没法保证。

### 2.3 能力协商让新旧版本共存

`session.hello` 交换能力。旧前端永远只收到 `drawlist.v1`，不会卡在不认识的帧上。不支持的可视化或配色报 `INVALID_PARAMS`；打开视图时会自动降回树图。

缺口：服务端声明的能力目前是写死的（`spaceknife_protocol` 的 `ALL_CAPABILITIES`）。插件带来新东西时，能力必须**从注册表推导**。

### 2.4 通用效果由宿主统一提供

呼吸灯、变化闪烁、标签、过滤暗化、分支配色、选中与悬停，都由宿主按行算好、统一绘制（后端 `View._annotate`，前端 `visual/Host.tsx` 与 `visual/overlays.ts`）。新可视化不写一行就全有。

插件只负责自己独有的那部分，体验的一致性由宿主保证。这是开放程度和质量能同时成立的关键。

### 2.5 内置功能也走同一个 `register()`

三种内置可视化和将来的插件注册方式完全一样。接口一直有真实的使用者，不会变成摆设。

插件系统也应如此：至少把一两个内置功能改写成插件形式，作为参考实现。

### 2.6 先录金样再重构

拆分布局引擎前，先录下树图的金样（`test_layout_golden.py`），拆完逐字节比对。

把现有功能改造成插件形式时，照此办理：先锁住现有行为，再动结构。

### 2.7 一处声明，处处生效

这次实际碰到的麻烦，大多来自同一件事要在两处声明：

| 情况 | 现在的做法 | 问题 |
|---|---|---|
| 命令面板命令 | 后端 `palette/commands.py` 的 `C(...)` 声明 id 和按键；前端在 `state/commands.ts` 的 `runCommand` 和 `App.tsx` 的按键处理里再接一遍 | 按键要手动保持一致，代码里只能写注释提醒 |
| 协议 | pydantic 模型 → `spaceknife schema` → `tools/gen_ts_protocol.py` → `generated.ts` | 已经是生成的，但要记得跑；漏跑由 CI 的 `--check` 拦下 |
| 文案 | 代码里写中文原文，英文写在 `locales/en/*.json` | `tools/i18n_check.py` 兜底，但多个代理并行时共享的 locale 文件容易冲突 |
| 设置 | `S(...)` 一处声明，设置页自动渲染 | 这是好例子 |

插件系统的要求：**一项东西只声明一次，其余全部生成或自动接线。** 命令、按键、设置、文案、协议类型都一样。

### 2.8 最大的风险：进程内的插件代码

现在的引擎在后端进程里运行，在计算池中持有树的读锁（`STRUCTURE`），拿到的是节点列的实时数据。

现有护栏只能防失误，防不住恶意代码：

- 行数上限 `MAX_ROWS`；
- 契约检查；
- 不跨帧持有引用。

第三方代码进来之前，必须先有隔离（第 6 节）。

### 2.9 性能与内存的规矩

这些规矩照搬到插件，见 [memory.md](memory.md)：

- 计算池有界，只算最新一次请求；
- 每帧没有 O(N) 的分配；
- 输出有上限；
- 不跨帧持有数据引用。

可视化的预算是：100 万节点、4K、15 层时布局 ≤ 60 ms。每一类扩展点都要有类似的、写明的预算。

## 3. 现成可以复用的部件

| 部件 | 位置 | 插件系统里的用途 |
|---|---|---|
| 协议模型和方法、事件注册表 | `packages/protocol/src/spaceknife_protocol/`（`methods.py` 的 `MethodSpec`、`EventSpec`、`METHODS`、`EVENTS`） | 插件的方法和事件用同一套模型描述 |
| schema 与 TypeScript 生成 | `spaceknife schema`、`tools/gen_ts_protocol.py` | 扩展为按插件生成类型化的客户端 |
| 服务端路由 | `server/rpc.py` 的 `Router.method(spec)` | 插件方法按命名空间注册进来 |
| 订阅 | `server/topics.py` 的 `TopicRegistry`（`$/subscribe`） | 插件的数据流、共享状态 |
| 二进制列帧 | `spaceknife_protocol/binary.py` 的 `encode_frame` | 插件返回大数组时走二进制，不走 JSON |
| 能力协商 | `session.hello`、`spaceknife_protocol/__init__.py` | 插件提供和需要的能力 |
| Python 客户端 | `packages/client` | 测试插件的后端部分，以及命令行插件 |
| 前端调用与类型 | `apps/electron/src/renderer/src/protocol/rpc.ts`、`generated.ts` | 生成插件专用的类型化接口 |
| 宿主能力（Electron） | `apps/electron/src/preload/index.ts`、`apps/electron/src/shared/api.ts`（`window.sk`） | 统一成「宿主服务」，插件不直接碰 IPC |
| 浏览器模式 | `apps/electron/src/renderer/src/shim.ts`、`tools/demo/m3_web.py` | 不起 Electron 也能开发、测试插件界面 |
| 子进程通道 | `helper/transport.py`（提权助手：鉴权的本地通道） | 外部进程插件的传输层 |
| 设置元数据 | `settings.py` 的 `S(...)` 与设置页 | 插件设置一处声明、自动渲染 |
| 命令面板 | `palette/commands.py` 的 `C(...)` | 插件命令；顺带解决 2.7 的两处声明 |
| 国际化 | `i18n.tr` / 前端 `t()`、`locales/`、`tools/i18n_check.py` | 插件自带中英文案，同一套检查 |
| 设计令牌和图标 | `design/tokens`、`design/icons`、`tools/gen_design.py` | 插件图标和颜色取自令牌，风格统一 |
| 已有的注册表 | `layout/registry.py`；`monitor/base.py` 的 `Watcher` / `create_watcher`；`export/formats.py` 的 `FORMATS`；`insights/`（内置规则加用户 TOML） | 统一成一个扩展注册表 |
| 契约与一致性检查 | `layout/contract.py`、`visual/testing/conformance.ts` | 插件开发包的测试核心 |

## 4. 统一通讯层：插件开发者看到的样子

核心要求：**插件声明接口，宿主负责传输。** 下面的写法只是示意，名字以实现为准。

**后端部分**：用 pydantic 模型声明方法、事件和共享状态。

```python
plugin = Plugin("example.dupes", version="0.1.0")


class FindParams(BaseModel):
    min_size: int = 1 << 20


class FindResult(BaseModel):
    groups: list[list[int]]  # node ids


@plugin.method
async def find(ctx: Context, p: FindParams) -> FindResult:
    tree = ctx.tree  # read-only view of the scan, see 6
    ...


@plugin.event
class Progress(BaseModel):
    done: int
    total: int
```

**前端部分**：拿到按插件生成的类型化接口，不碰 WebSocket。

```ts
const dupes = usePlugin<DupesApi>("example.dupes");
const res = await dupes.find({ min_size: 1 << 20 });
dupes.on("progress", (p) => setDone(p.done / p.total));
```

**宿主自动完成的事**：

- 方法和事件加命名空间（`plugin.example.dupes.find`），注册进 `Router` 和 `EVENTS`；
- 生成 JSON schema 和 TypeScript 类型；
- 由注册表推导能力（`plugin.example.dupes.v0`）；
- 权限检查、限时、取消、错误包装（统一成 `RpcError`）、日志带插件 id。

**四种通讯形态，统一一套写法**：

- 请求/响应；
- 事件推送；
- 订阅流：插件返回 numpy 数组或字节时，宿主自动走二进制列帧；
- 共享状态：服务端说了算，前端订阅变化。

**插件代码与运行位置无关**：

- 后端部分可以在进程内、子进程里或 WASM 里运行；前端部分可以在 Electron 或浏览器模式里运行。插件代码都不变，由宿主的桥接层处理差异。
- 这也是测试的基础：宿主提供模拟传输，插件的后端部分在 pytest 里测，前端部分在 vitest 里测，都不用起 Electron。

**Electron 的事由宿主服务代办**：

- 打开或定位文件、剪贴板、通知、文件对话框、系统设置页，统一成一组宿主服务（在现有的 `window.sk` 基础上扩展）。插件按权限调用，不碰 `ipcRenderer` 和 preload。
- 只有确实要改 Electron 主进程的插件才写主进程部分，属于最高信任档（第 5 节）。

**一处声明**（2.7 节）：

- 插件的命令、按键、设置、文案都在清单或代码里声明一次；
- 命令面板、键位、设置页、i18n 检查自动接上；
- 内置功能也迁到这套机制上，顺带消除现在命令按键两处声明的问题。

## 5. 插件的组成与开放档次

### 清单（`plugin.toml`，示意）

- id、版本、依赖的宿主接口版本（类似 `ENGINE_API`、`VISUAL_API`）；
- 提供的能力、需要的能力；
- 权限（第 6 节）；
- 入口：
  - 后端：Python 模块、WASM 模块或可执行文件；
  - 前端：声明式界面描述或 Web 面板；
- 中英文案、图标（SVG，用设计令牌的颜色）、设置 schema。

### 开放档次

| 档 | 形式 | 适合做 | 隔离 |
|---|---|---|---|
| 声明式扩展包 | TOML/JSON、SVG、文案，不含代码 | 洞察规则、文件类别、配色色板、导出模板、命令别名、键位 | 不需要 |
| 计算插件 | WASM 模块 | 可视化引擎、分析器、导出格式 | 沙箱；只读访问树；限时、限内存 |
| 外部进程插件 | 子进程，用同一套协议与宿主通讯（像提权助手） | 新扫描来源（NAS、云盘、远程机器）、监控、文件操作 | 进程隔离；崩溃后可重启 |
| 进程内 Python | 普通 Python 包 | 开发与调试 | 无，只在开发者模式开放 |
| Electron 主进程 | 主进程脚本 | 托盘、全局快捷键、原生菜单 | 最高信任，单独确认 |

### 前端部分，分两档

1. **声明式**（所有前端都能实现，优先用）：
   - 设置项、命令、悬停卡和详情面板追加的字段；
   - 报告文档：文字、表格、图表描述，由内置组件绘制；
   - 新的可视化：选用一种现有的布局格式。
2. **Web 面板**：插件自带的 HTML/JS，跑在隔离的 iframe 里，通过宿主提供的消息通道调用第 4 节的同一套接口。
   - Electron 和浏览器模式可用。
   - Qt 和 WinUI3 上怎么办待定：内嵌 Web 视图，或者不支持（第 9 节）。

### 布局格式（「形状」）由我们维护

新增一种布局格式，意味着每个前端各写一个组件，所以不对插件开放。可以按需求增加通用的几种：

- 已有：矩形、条带、极坐标；
- 可能增加：时间轴、关系图、表格。

### 扩展点清单

- 可视化引擎、配色模式与色板；
- 扫描来源、文件监控；
- 导出格式；
- 洞察规则与洞察卡片（这是什么、能不能清理、怎么清理）；
- 文件类别与过滤词汇；
- 命令与键位；
- 文件操作：一律经过删除篮，由用户确认；
- 报告面板；
- 快照与对比的分析器。

## 6. 安全、信任与资源

**权限**（在清单里声明，第一次启用时给用户看）：

- 读目录树的元数据；
- 读文件内容；
- 联网；
- 写入与删除：只能提交到删除篮，由用户确认；
- 调用宿主服务（打开、定位文件等）；
- Electron 主进程。

**树的只读访问**：给插件的是只读视图（列的只读映射、子节点索引数组、名称），不暴露 `NodeStore` 本身，也不给锁。WASM 和子进程通过共享内存读取，不逐个节点地远程调用。

**资源**：

- 每次调用都有时间预算，只算最新一次请求；
- 内存上限：WASM 线性内存、子进程的限额；
- 输出由契约检查后才交给前端；
- 子进程崩溃只影响它自己，宿主可以重启它。

**信任**：

- 插件放在数据目录下的 `plugins/`，便携版同样适用；
- 可以单独启用、停用；
- 第一次启用时由用户确认，记下哈希，文件被改动后要重新确认；
- 以后再加签名。

**隐私**：没有联网权限的插件不能往外发任何东西。插件文案和示例同样遵守公开内容的规矩：不出现个人信息，不提第三方产品。

## 7. 开发体验

- **开发包**：一个 Python 包加一个 TypeScript 包，提供：
  - 第 4 节的声明式接口与类型；
  - 模拟宿主；
  - 契约检查（`contract.py`、`conformance.ts`）；
  - 插件模板。
- **命令**：
  - `spaceknife plugin new`：从模板新建；
  - `spaceknife plugin build`：生成类型与文案模板；
  - `spaceknife plugin check`：跑契约、i18n、权限声明检查；
  - `spaceknife plugin dev`：热重载，前端用浏览器模式。
- **参考实现**：把至少一个内置功能改写成插件，与普通插件同等对待，作为文档里的样例。候选：
  - 用 WASM 写的火焰图投影；
  - 一种导出格式；
  - 一组洞察规则。

## 8. 建议的落地顺序

1. **注册表与能力**
   - 服务端能力改为从注册表推导；
   - 把可视化、监控、导出格式、洞察规则的注册表统一成一个扩展注册表。每一类扩展各带接口版本号和契约检查。
2. **统一通讯层，先给内置功能用**
   - 先实现方法和事件的命名空间、类型化客户端的生成、一处声明的命令和按键；
   - 迁移一个现有功能来验证，并消除 2.7 节的两处声明。
3. **声明式扩展包**：价值最高、风险最低。用户的 TOML 洞察规则和导出模板直接迁进来。
4. **WASM 计算插件原型**
   - 把火焰图投影改写成参考插件；
   - 设计只读访问目录树的接口（子节点索引数组和尺寸列放在共享内存里）；
   - 验证 100 万节点时的开销是否在预算内。
5. **外部进程插件**：复用提权助手的传输层，首个用例是新的扫描来源。
6. **Web 面板**：等第 9 节的决定。

## 9. 定下的决定

| 问题 | 决定（已实现） |
|---|---|
| 开放给谁 | 先给高级用户，但按第三方可发布来做：清单、第一次启用时列出权限、按内容哈希锁定、逐个启用 / 停用。签名以后做。 |
| 代码插件用什么写 | 主档是 Python 插件进程，用宿主协议通讯，隔离、可重启、有资源上限；进程内 Python 只在开发者模式下开放（内置插件除外）。WASM 保留接口（运行时与语言无关），没有做原型。 |
| 前端 | 处处可用的声明式界面（设置、命令、键位、详情 / 悬停字段、报告文档、借用现有布局格式的可视化），加上 Electron 和浏览器模式里的沙箱 iframe Web 面板，经宿主消息通道调同一套接口。Qt 和 WinUI3 不声明 `plugins.v1`，照旧工作。 |
| 分发 | 数据目录下的 `plugins/`（便携版同样），从文件夹或 zip 安装；Git 和插件目录以后做。 |
| 一处声明 | 命令、按键、设置、文案（中英）、协议类型都只声明一次；能力从注册表推导；内置功能走同一个注册：导出和洞察规则是内置插件，命令和按键是单一键位表。 |
| 写操作 | 删除只能经删除篮由用户确认；没有 `net` 权限不能联网。 |
