# SpaceKnife 协议 v1

后端与三个前端（Electron、Qt、WinUI3）之间只通过这套 WebSocket 协议通信。消息模型的唯一事实来源是 `packages/protocol`（pydantic v2）；机器可读的 JSON Schema 在 `packages/protocol/schema/protocol.schema.json`，由 `spaceknife schema` 生成，CI 会检查它与源码是否一致。

## 1. 启动与连接

后端由前端进程以子进程方式启动：

```bash
python -X utf8 -m spaceknife serve --token-stdin
```

1. 启动方在 stdin 第一行写入本次启动的随机 token（不放在 argv 或环境变量里，避免被同机其他用户看到）。
2. 后端开始监听后，在 **stdout** 输出唯一一行 JSON，之后 stdout 不再输出任何内容；日志一律写 stderr：

   ```json
   {"event": "ready", "url": "ws://127.0.0.1:53817/v1", "host": "127.0.0.1", "port": 53817, "pid": 1234, "protocol": "1.1", "subprotocol": "spaceknife.v1"}
   ```

   不带 `--token-stdin` 时，后端自己生成 token，并放在 ready 行的 `token` 字段里（开发调试用）。
3. 后端在以下情况优雅退出：stdin 读到 EOF（父进程关闭管道或已退出）、收到 `app.shutdown`、收到 SIGTERM/SIGINT；在 Linux 上父进程死亡时内核会发 SIGTERM（PR_SET_PDEATHSIG），在 macOS 上会轮询 `getppid()`。

连接要求：

| 项 | 要求 |
|---|---|
| 地址 | 只绑定 `127.0.0.1`，端口随机 |
| 路径 | `/v1`，其他路径返回 HTTP 404 |
| 子协议 | 必须协商 `spaceknife.v1`，否则 HTTP 400 |
| Origin | 白名单：`app://spaceknife`、无 Origin（Python 客户端）、以及 `--allow-origin` 指定的开发地址；其他返回 HTTP 403 |
| 压缩 | 关闭（permessage-deflate 会让二进制吞吐下降约 8 倍） |
| 入站消息上限 | 1 MiB |

## 2. 握手

连接后的第一条消息必须是 `session.hello` 请求，并且要在 5 秒内发出：

```json
{"jsonrpc": "2.0", "id": 0, "method": "session.hello",
 "params": {"token": "…", "protocol": "1.1",
            "client": {"name": "spaceknife-electron", "version": "0.1.0", "kind": "electron"},
            "capabilities": ["binary-frames.v1", "subscriptions", "cancel", "log-stream",
                             "visuals.v1", "layout.flame.v1", "layout.polar.v1", "updates.v1"]}}
```

成功时返回协议版本、服务端信息、`session_id`、双方能力的交集、schema 指纹和各项限制。失败时先回一个错误响应，再按下表关闭连接：

| 情况 | 错误码 | 关闭码 |
|---|---|---|
| 第一条消息不是 `session.hello` | -32002 NOT_HANDSHAKEN | 4401 |
| token 不对 | -32001 UNAUTHORIZED | 4401 |
| 主版本号不同 | -32003 PROTOCOL_MISMATCH | 4426 |
| 参数不合法 | -32602 INVALID_PARAMS | 4400 |
| 超时未收到 hello | — | 4408 |

次版本号新增的功能只通过 capabilities 协商启用，不靠新增字段本身。当前版本是 1.1；握手只比较主版本号，1.0 的客户端照常连接，只是拿不到它没声明的能力。

| 能力 | 含义 |
|---|---|
| `binary-frames.v1` | 二进制帧（§4） |
| `subscriptions` | `$/subscribe` / `$/unsubscribe` 与主题事件 |
| `cancel` | `$/cancel` |
| `log-stream` | `log` 主题 |
| `visuals.v1` | 理解视图设置的 `visual` 字段、`visual.list`、`FrameInfo` 的 `visual` / `layout` / `geometry` / `root`、配色模式 `branch` 和 `branch` 列 |
| `layout.flame.v1` | 能解码并画 `flame.v1` 帧（火焰图） |
| `layout.polar.v1` | 能解码并画 `polar.v1` 帧（火焰-极坐标图） |
| `updates.v1` | 接收 `update.changed`（更新检查结果与下载进度，见 §3「更新检查」） |

`drawlist.v1`（树图）是基线，不需要能力。后端只给会话发它协商过的布局：没有对应能力的会话里，新视图和 `view.open` 的可视化退回树图，`view.set` 选不可用的可视化返回 INVALID_PARAMS；配色 `branch` 也只对带 `visuals.v1` 的会话开放。Electron 的能力列表由前端已注册的可视化组件生成。

## 3. 文本帧（控制面）

采用 JSON-RPC 2.0 结构，每帧一条消息：

| 类型 | 形状 |
|---|---|
| 请求 | `{"jsonrpc":"2.0","id":1,"method":"…","params":{…}}` |
| 成功响应 | `{"jsonrpc":"2.0","id":1,"result":…}` |
| 错误响应 | `{"jsonrpc":"2.0","id":1,"error":{"code":…,"message":"…","data":…}}` |
| 通知 / 事件 | `{"jsonrpc":"2.0","method":"…","params":{…}}` |

- 请求 id 是非负整数（≤ 2^53−1）或长度 1–64 的字符串；同一连接上同时进行的请求 id 不能重复。
- JSON 中不出现超过 2^53 的整数；节点一律用后端内部的 u32 id。
- 服务端校验入站参数时拒绝未知字段（`extra="forbid"`）；客户端解析结果与事件时忽略未知字段（兼容更新的服务端）。
- 取消：客户端发 `$/cancel {"id": …}` 通知，被取消的请求回 -32800 REQUEST_CANCELLED。
- 订阅：`$/subscribe {"topic", "params", "rate_hz"}` 返回 `{"sub"}`，之后以事件推送，事件 params 里带 `sub`；`$/unsubscribe {"sub"}` 结束订阅。
- 出站顺序：响应优先，其次是二进制大块数据，最后是事件；客户端积压过多时丢弃事件。

### 当前方法

| 方法 | 说明 |
|---|---|
| `session.hello` | 鉴权与协商（必须第一个） |
| `system.info` | 服务端版本、进程号、平台、会话数 |
| `system.ping` | 回显，用于连通性检查 |
| `app.shutdown` | 请求后端优雅退出（先回响应再关闭） |
| `$/subscribe`、`$/unsubscribe` | 订阅管理 |
| `volume.list` | 可扫描的卷：挂载点、文件系统、容量、已用、可用、簇大小、网络盘/可移动盘；介质（NVMe SSD / SATA SSD / 机械盘 / USB / 网络，免提权探测）、是否系统卷、网络路径、不可用原因（无介质、未连接、无法读取）（W1） |
| `scan.check_paths` | 校验输入的扫描路径：是否存在、是否文件夹、能否访问、所在卷；互相包含的路径合并到上层，返回最终要扫描的根（UI-W1-25） |
| `scan.start` | 开始扫描一个或多个根（`roots`；`cross_mounts`、`workers`、`auto_snapshot` 省略时取设置），立即返回扫描状态 |
| `scan.status`、`scan.list` | 扫描状态：项数、文件/文件夹数、占用与逻辑大小、近几秒的项速率与字节速率、百分比（整卷按已用空间，文件夹按同一目标上次扫描的项数）、剩余时间估计、无法访问的目录、可用/未知空间、快照路径 |
| `scan.cancel`、`scan.close` | 停止扫描（保留已扫部分）；关闭并释放内存 |
| `tree.node`、`tree.children`、`tree.find` | 节点详情与完整路径；按大小或名称排序、分页的子项；按绝对路径查找 |
| `snapshot.list`、`snapshot.save`、`snapshot.load` | 自动保存的快照列表；保存；以「已完成的扫描」打开快照 |
| `filter.parse` | 解析过滤式（[语法](filter-syntax.md)）：语法着色 token、诊断（UTF-16 区间 + 错误码 + 可用值）、光标处未完成的条件、补全上下文、规范化条件树与规范形式 |
| `filter.vocab` | 过滤用的文件类别表，以及本平台不记录的属性和时间 |
| `view.open` | 在一次扫描上打开一个视图（标签页或分屏窗格），参数含视口（CSS 像素宽高与设备像素比）和可选的初始设置（可视化不可用时退回树图）；返回视图状态并发送第一帧 |
| `view.zoom`、`view.back`、`view.forward`、`view.up`、`view.home` | 放大进入某块（按上一帧的 `key`，或直接给节点 id）、后退、前进、上一级（刚离开的块被选中）、回到根 |
| `view.select` | 选中一块或取消选中 |
| `view.set` | 改细节层级（1–15）、最小元素尺寸、按大小排序、可用/未知空间开关、可视化（`visual`：`visual.list` 里的 id，需 `visuals.v1`；未知或本会话不可用时返回 INVALID_PARAMS）、配色模式（类别 / 扁平 / 时间 / 分支，作用于当前可视化，每种可视化各自记住）、过滤显示方式（暗化 / 隐藏）、视口。只换可视化时缩放根、历史、选中、过滤、细节层级都保持，不记入后退历史，帧的 `reason` 为 `visual` |
| `visual.list` | 本服务端提供的可视化：id、布局、按会话语言的名称、图标、顺序、能否画对比视图、是否画可用/未知空间、默认配色，以及本会话能否使用（不能时附原因） |
| `view.filter` | 设置视图的过滤式（[语法](filter-syntax.md)）；有错误时保留上一次有效的过滤并返回诊断；被更新的输入取代的请求直接跳过 |
| `view.tag` | 给选中项打红 / 黄 / 绿 / 蓝标签，或清除它及其下所有标签（Ctrl+0）；标签按扫描根持久保存 |
| `view.reveal` | 定位一个块或节点（匹配结果单击、详情面板的子项）：已画出的块直接选中；在小项聚合块里的就选中聚合块；被细节层级折叠或在当前根之外的，视图导航（记入后退历史）到能把它画出来的最高祖先，仍太小时退到它的父文件夹 |
| `view.results` | 匹配结果列表的一页（`offset` / `limit` ≤ 500）。行按当前帧里的可见块分组：单独画出的匹配文件一行；折叠文件夹、小项聚合块、+N 块里的匹配各合并成一行；画不出的零字节等项归入最近的已画祖先（在缩放根下则为一行 `rest`）。各行项数之和 = 匹配项数，字节之和 = 匹配大小。`sort`（size / name / count）按视图记住；返回 `seq` 标明分组依据的帧，`max_alloc` 用于相对大小条 |
| `settings.get` / `settings.update` / `settings.reset` | 设置（W7）：值 + 元数据（名称、说明、控件、选项、范围、生效时机、是否可改及原因）一次下发，三端按元数据渲染同一页面；`update` 带版本号，冲突返回 −32013；按页恢复默认 |
| `palette.commands` | 全部命令及默认快捷键（设置 › 快捷键页） |
| `palette.query` | 命令面板（W4）的主列表：直达项（输入是路径 → 是否在扫描内、大小、Tab 补全；输入含过滤语法 → 着色 token 与错误）、最近（路径跳转按扫描目标、过滤全局共用，带当前大小）、命令（ID、名称、命中区间、状态或禁用原因、快捷键、图标）。空输入时命令为固定 9 条；输入后对名称、全拼、首字母和英文别名做模糊匹配并排序。`paths` 为真时由前端再请求 `palette.paths` |
| `palette.paths` | 路径组：空输入时以选中项名称为种子（包含匹配、排除自身）；输入是文件夹路径时列出其子项；否则按「整名 > 前缀 > 名称中间 > 散字（3 个字符起）」分档、档内按大小降序。返回前 `limit` 条、总命中数、命中区间（UTF-16）、扫描未完成标记 |
| `palette.preview` | 一个过滤式在整个扫描上的匹配项数与大小（命令面板的过滤直达项与最近过滤项），按规范形与数据版本缓存 |
| `palette.remember` / `palette.clear` | 记录在面板里执行过的路径跳转（按扫描目标）与过滤（全局）；清除某扫描目标的跳转和全部过滤 |
| `view.render`、`view.list`、`view.close` | 重发一帧；列出本会话的视图；关闭视图（会话断开或扫描关闭时自动关闭） |
| `update.status` | 更新检查的现状（`UpdateStatus`）：当前版本、安装方式、是否自动检查、通道、上次成功检查的时间与失败原因、落后程度 `level`、最新版本（标题、发布页、说明、本安装方式对应的文件）、卡片是否已关闭、下载状态 |
| `update.check` | 立即检查 GitHub 发布列表（自动检查关闭时也查），检查进行中则等它结束；返回新的 `UpdateStatus` |
| `update.dismiss` | `{version}` 的卡片 / 对话框已被关闭，此版本不再弹出（徽章保留） |
| `update.download` / `update.cancel` | 把 `{version}` 对应本安装方式的文件下载到下载文件夹并校验 SHA-256（立即返回，进度见 `update.changed`）；不是当前可更新的版本返回 INVALID_PARAMS，没有对应文件返回 INVALID_REQUEST。停止下载并删除未完成的文件 |

### 当前事件与主题

| 事件 | 说明 |
|---|---|
| `log.record` | `log` 主题：先回放最近若干条（`backlog`），之后实时推送不低于 `min_level` 的后端日志（W8 日志面板） |
| `$/progress` | 长任务进度（LSP 风格） |
| `scan.progress` | `scan` 主题（参数 `{scan_id}`）：订阅时立即推送一次当前状态，之后最多每秒 4 次，结束时再推送一次最终状态 |
| `view.frame` | 不由请求触发的新帧（扫描进行中最多每秒 4 次，扫描结束时一次，设置改变几何参数时一次），随后到达对应的二进制帧 |
| `settings.changed` | 任何一端修改设置、或设置文件被外部编辑后，推送给所有会话的完整设置文档 |
| `update.changed` | `{status}`：检查开始与结束、下载进度（最多每秒 4 次）、卡片关闭、相关设置或界面语言改变时，推送给所有声明了 `updates.v1` 的会话 |

### 更新检查（`updates.v1`）

后端唯一的联网请求是 `GET https://api.github.com/repos/DuskXi/SpaceKnife/releases`（User-Agent `SpaceKnife/<版本>`，不带 token，不带其他数据，代理沿用系统设置），以及用户点击后从 github.com 下载文件。结果缓存在数据目录的 `update.json`（上次检查时间、ETag、精简后的发布列表、已关闭卡片的版本）；再次检查带 `If-None-Match`，列表没变时 GitHub 回 304，不占查询次数。

- **频率**：设置「自动检查更新」开启时，启动约 60 秒后、且上次成功检查已超过 24 小时才查，之后每 24 小时一次；失败 6 小时后重试。打开该设置或切换通道时立即检查。
- **通道**：`preview`（默认）包括 GitHub 上标为 Pre-release 或版本号带预发布段（`0.4.0-beta.1`）的版本，`stable` 只要正式版；草稿和不是语义化版本号的 tag 一律忽略，取版本号最大的一个。
- **落后程度** `level` 按第一个不同的版本段：`patch`（0.3.0 → 0.3.1，只在右上角显示徽章）、`minor`（0.3.x → 0.4.0，另在左上角挂出带说明的卡片）、`major`（0.x → 1.0.0，另弹出窗口内对话框）；同一版本的预发布 → 正式版算 `patch`。徽章对所有级别都显示，`dismissed` 只影响卡片 / 对话框。
- **文件**：按启动方传入的安装方式（`SPACEKNIFE_INSTALL_KIND`：`nsis` / `portable` / `appimage` / `deb` / `rpm` / `dmg`，不设为 `dev`）和平台、架构选发布里的文件（命名见 tools/build_package.py）；`dev`、`unknown` 或没有对应文件时 `latest.asset` 为空，只给发布页。SHA-256 取自发布里的 `SHA256SUMS*.txt`（下载时才取），不符时删除文件并报错；目标文件已存在且校验相符时直接复用。下载目录取 `SPACEKNIFE_DOWNLOAD_DIR`（Electron 传系统下载文件夹），否则 `~/Downloads`，都没有时用数据目录下的 `downloads`；AppImage 下载后设为可执行。
- **当前版本**：`SPACEKNIFE_APP_VERSION`（Electron 传 `app.getVersion()`），否则取第一个声明 `updates.v1` 的会话在 `session.hello` 里报的客户端版本。测试与演示可以用 `SPACEKNIFE_UPDATE_FEED` 指向本机（只接受回环地址）的一份发布列表。

## 4. 二进制帧（数据面）

```
[u32 小端：帧头长度 H][H 字节 UTF-8 JSON 帧头][补 0 到 8 字节对齐][数据区：各列小端、8 字节对齐]
```

帧头（`BinaryFrameHeader`）：`stream`、`request_id`、`seq`、`final`、`layout`（带版本，如 `drawlist.v1`）、`count`（行数）、`columns: {name: {offset, dtype}}`，以及可选的 `blobs: {name: {offset, length}}`。各列都有 `count` 行，排在数据区前面；blob 是排在列之后的原始字节串（8 字节对齐），含义由 layout 规定。偏移量相对数据区起点；dtype 只允许 `<u1 <i1 <u2 <i2 <u4 <i4 <u8 <i8 <f4 <f8`。Python 端解码为只读、零拷贝的 numpy 视图（blob 为 uint8 数组）；JS 端可以在同一块内存上直接建 TypedArray。

### 4.1 视图帧与通用列

改变视图的方法返回 `ViewUpdate`（JSON：视图状态、`changed`、帧信息 `frame`、过滤信息 `filter`）；画面有变化时，再发一个二进制帧：`stream` 为 `view:<view_id>`，`layout` 等于 `FrameInfo.layout`，`seq` 等于 `ViewUpdate.frame.seq`，`request_id` 是该请求的 id。响应先于二进制帧到达（响应优先级更高），客户端按（视图，`seq`）配对，参考实现见 `spaceknife_client.views.ViewClient`。遇到不认识的 `layout` 时不要等它的二进制帧。

帧信息 `FrameInfo`：画布的物理像素宽高与设备像素比、行数、选中行（选中项被折叠或聚合时是它最近的可见块）、面包屑（`small:N` / `more:N` 表示聚合块；最后一节就是缩放根）、缩放根下的已扫描合计、过滤统计（匹配文件数、占用与逻辑大小、是否沿用上一次的结果、诊断）、布局耗时、文件变化的闪烁项，以及：

| 字段 | 含义 |
|---|---|
| `visual` | 画这一帧的可视化（`treemap` / `flame` / `flame_polar` / …），缺省为 `treemap` |
| `layout` | 随后那个二进制帧的布局，缺省为 `drawlist.v1` |
| `geometry` | 布局的几何参数，按 `kind` 区分：`drawlist.v1` 为 null，`flame.v1` 为 `FlameGeometry`（4.3），`polar.v1` 为 `PolarGeometry`（4.4） |
| `root` | `FrameRoot`：缩放根本身，画成根条或中心圆，只在 `flame.v1` 和 `polar.v1` 里有。字段 `key node kind size count flags tag tone branch delta dstat` 与同名列的含义相同；它不是一行 |
| `flash[].row` | 变化的块或它最近的已画祖先所在的行；`flame.v1` 和 `polar.v1` 里落不到任何行的变化记为 −1，表示缩放根 |

所有布局的行都有同样的通用列，父块总在子块之前；各布局另加自己的几何列（4.2–4.4）：

| 列 | 类型 | 含义 |
|---|---|---|
| `key` | `<u4` | 稳定 id：节点 id；高 2 位非零时是合成块（「N 个小项」「+N 项」「可用空间」「未知空间」） |
| `node` | `<i4` | 节点 id（聚合块为所在文件夹，可用/未知空间为 -1） |
| `parent` | `<i4` | 所在展开文件夹的行号，顶层为 -1 |
| `kind` | `<u1` | 0 文件 · 1 展开的文件夹（画法由布局规定）· 2 折叠的文件夹 · 3 等待扫描 · 4 N 个小项 · 5 +N 项 · 6 可用空间 · 7 未知空间 |
| `depth` | `<u1` | 嵌套层级，缩放根的子项为 1（层级色取 min(depth, 4)） |
| `cls` | `<u1` | 文件类别序号（`filter.vocab` 的顺序），用于文件和小项块 |
| `label` | `<u1` | 1 名称 · 2 第二行大小 · 4 大号字 · 8 矮块（只显示名称，垂直居中）· 16 径向（只用于 `polar.v1`，文字沿半径方向写） |
| `size` | `<i8` | 显示的字节数（面积或长度的口径；隐藏模式下是匹配部分） |
| `count` | `<u4` | 「N 个小项」的文件数，「+N 项」的条目数 |
| `flags` | `<u2` | 1 匹配 / 含匹配 · 2 暗化 · 4 命中文件夹掩码（粗体）· 8 自身有标签 · 16 选中 · 32 选中项的祖先（光晕）· 64 扫描进行中、子树还有没列出的文件夹（只标缩放根以下 3 层的文件夹块；`FrameInfo.root.flags` 也可能带这一位） |
| `tag` | `<u1` | 自身标签：0 无，1 红，2 黄，3 绿，4 蓝 |
| `tone` | `<u1` | 修改时间档位（时间配色）：0 一天内 … 4 一年以上或未知 |
| `branch` | `<u1` | 顶层分支的色板槽位（分支配色）：0 中性，1–8 为槽位，深浅档取 min(depth, 4)。后端按缩放根下第 1 层的 key 记住槽位，子孙沿用祖先的槽位；第 1 层的聚合块、可用空间、未知空间为 0。这一列总是发送 |
| `name_len` | `<u4` | 本行名称在 `names` blob 中的字节数（0 表示不画文字） |
| `delta` | `<i8` | 只在对比视图里有：占用的净变化 |
| `dstat` | `<u1` | 只在对比视图里有：0 未变 · 1 新增 · 2 已删除 · 3 有变化 |

blob `names`：各行名称的 UTF-8 编码，按行顺序首尾相接。颜色由前端按共享的令牌规则从 `kind`、`depth`、`cls`、`tone`、`branch`、`flags` 和配色模式算出。

行契约（所有布局都必须满足）：

1. 父行在前：`parent[i] < i`。
2. 同一帧里 `key` 不重复。
3. `kind` ≤ 3 的行是真实节点。
4. 一个节点最多属于一个聚合块。
5. 缩放根下的每个节点，要么是一行，要么是聚合块的成员，要么在某个已画行之下；被丢弃的极小块除外。
6. 只有带标签位、且 `kind` ≤ 3 的行才有名称。
7. `flame.v1` 和 `polar.v1` 每帧最多 50 000 行。
8. 同一层（同一带或同一环）的行按 `x` 或 `a0` 递增。

兄弟块之间共享边界值，严丝合缝；子块一定落在父块之内。

版本规则：给布局加可选列是兼容的（解码方按列名取列，多出来的列忽略）；改变已有列或几何字段的含义，要升布局的版本（如 `flame.v2`）并配新的能力名。`drawlist.v1` 已冻结。

### 4.2 树图 `drawlist.v1`

| 列 | 类型 | 含义 |
|---|---|---|
| `x` `y` `w` `h` | `<i4` | 矩形，整数物理像素 |

kind 1 是框加标题栏，子块画在框里。`FrameInfo.geometry` 和 `FrameInfo.root` 为 null，缩放根就是整个画布。

### 4.3 火焰图 `flame.v1`

几何列与 `drawlist.v1` 相同（`x y w h`，`<i4`，整数物理像素），但每一层是一条横带：kind 1 是已展开的实心条，子条在下一带，横向落在父条的范围内。

`FlameGeometry`：

| 字段 | 含义 |
|---|---|
| `kind` | 固定为 `"flame"` |
| `origin` | 根条所在的一侧：`bottom` 向上生长（设置 `treemap.flame_origin` 的默认值），`top` 向下展开 |
| `row_h` | 带距（物理像素），条高为 `row_h − gap` |
| `rows` | 根条以外画出的层数 |
| `gap` | 带之间、相邻条之间的缝 |
| `root` | 根条的矩形 `{x, y, w, h}`，按 `FrameInfo.root` 画 |

第 k 带（根条为第 0 带）：`origin` 为 `top` 时 `y = k·row_h`，为 `bottom` 时 `y = height − k·row_h − (row_h − gap)`。条间的缝已经从 `w` 里扣掉，最右边的条正好到画布右边；命中测试时右侧的缝算作左边那条（`[x, x + w + gap)`）。

### 4.4 火焰-极坐标图 `polar.v1`

| 列 | 类型 | 含义 |
|---|---|---|
| `a0` `a1` | `<i4` | 扇区的起止角：每圈 2^20 = 1 048 576 单位（`PolarGeometry.unit`），0 为 12 点钟方向，屏幕坐标下顺时针递增；0 ≤ a0 < a1 ≤ 2^20 |
| `r0` `r1` | `<i4` | 内外半径：从圆心量起的物理像素，环间的缝已经扣掉 |

所有值都小于 2^24，在着色器的 f32 里也是精确的。kind 1 是已展开的实心扇区，子扇区在下一环，角度落在父扇区之内；同一环里相邻扇区共享边界（`a1` 等于下一个的 `a0`）。`a1 − a0 = 2^20` 的满圈扇区没有径向边。文件夹自身占用的那部分在它的子扇区之后留成空白。

`PolarGeometry`：

| 字段 | 含义 |
|---|---|
| `kind` | 固定为 `"polar"` |
| `cx` `cy` | 圆心（物理像素） |
| `radius` | 最外一环格子的外沿，等于 `hole + rings·ring_w` |
| `hole` | 中心圆的半径；中心圆按 `FrameInfo.root` 画 |
| `ring_w` | 环距：第 k 环的格子是 `[hole + (k−1)·ring_w, hole + k·ring_w)` |
| `rings` | 中心圆以外画出的环数 |
| `gap` | 环之间、相邻扇区之间的缝（下文记作 g） |
| `unit` | 每圈的角度单位，固定为 1 048 576 |

半径的取整：画出的 `r0 = 格子下沿 + ⌈g/2⌉`，`r1 = 格子上沿 − ⌊g/2⌋`。

角向的缝不烤进角度，由前端画。记 Δ = (a1 − a0)/unit·2π（弧度）：两条径向边各沿垂直方向收进 `min(g/2, max(0, (Δ·r0 − 1)/2))` 像素，所以最细的扇区在内半径处也至少留 1 像素；满圈扇区不收进。

标签位由后端按下面的规则算好，前端只按位摆放。记 lu = dpr·字号缩放，r_mid = (r0 + r1)/2，T = r1 − r0，L = Δ·r_mid − g：

- T > L 时沿半径写：`label` 为 1 | 8 | 16（名称、单行、径向），放不下时为 0。前端的最大文字宽度为 `T − 8·dpr`；右半圈从内往外读，左半圈转半圈后右对齐。
- 否则沿切线写：可用宽度 `Wt = min(L, 2·√max(0, (r1 − round(2·dpr))² − r_mid²))`，前端的最大文字宽度为 `Wt − 12·dpr`；有 2 位时第二行写大小。锚点在 (r_mid, 角度中点)，落在下半圈时转半圈，保证字是正的。

层数、带高、环宽、中心孔和可见/可展开阈值的投影公式见 [visuals.md](visuals.md)。

### 日志事件

`log.record` 里带 `extra.event` 的记录是给用户看的事件（来源：scan / perm / monitor / delete / snapshot / export / system），`extra.scan_id` 与 `extra.path` 用于「在树图中定位」。日志面板只显示这些事件和警告以上级别；完整日志写入数据目录下的 `logs/spaceknife.log`（按 10 MB 轮转，保留 5 个）。

## 5. 错误码

| 码 | 名称 |
|---|---|
| -32700 | PARSE_ERROR |
| -32600 | INVALID_REQUEST |
| -32601 | METHOD_NOT_FOUND |
| -32602 | INVALID_PARAMS |
| -32603 | INTERNAL_ERROR |
| -32001 | UNAUTHORIZED |
| -32002 | NOT_HANDSHAKEN |
| -32003 | PROTOCOL_MISMATCH |
| -32004 | UNKNOWN_TOPIC |
| -32005 | UNKNOWN_SUBSCRIPTION |
| -32006 | SHUTTING_DOWN |
| -32007 | UNKNOWN_SCAN |
| -32008 | SCAN_FAILED |
| -32009 | SNAPSHOT_ERROR |
| -32010 | UNKNOWN_NODE |
| -32011 | SCAN_BUSY |
| -32012 | UNKNOWN_VIEW |
| -32013 | SETTINGS_CONFLICT（设置已被其他窗口修改，带上新版本号重试） |
| -32800 | REQUEST_CANCELLED |
