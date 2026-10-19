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
   {"event": "ready", "url": "ws://127.0.0.1:53817/v1", "host": "127.0.0.1", "port": 53817, "pid": 1234, "protocol": "1.0", "subprotocol": "spaceknife.v1"}
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
 "params": {"token": "…", "protocol": "1.0",
            "client": {"name": "spaceknife-electron", "version": "0.1.0", "kind": "electron"},
            "capabilities": ["binary-frames.v1", "subscriptions", "cancel", "log-stream"]}}
```

成功时返回协议版本、服务端信息、`session_id`、双方能力的交集、schema 指纹和各项限制。失败时先回一个错误响应，再按下表关闭连接：

| 情况 | 错误码 | 关闭码 |
|---|---|---|
| 第一条消息不是 `session.hello` | -32002 NOT_HANDSHAKEN | 4401 |
| token 不对 | -32001 UNAUTHORIZED | 4401 |
| 主版本号不同 | -32003 PROTOCOL_MISMATCH | 4426 |
| 参数不合法 | -32602 INVALID_PARAMS | 4400 |
| 超时未收到 hello | — | 4408 |

次版本号新增的功能只通过 capabilities 协商启用，不靠新增字段本身。

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
| `view.open` | 在一次扫描上打开一个视图（标签页或分屏窗格），参数含视口（CSS 像素宽高与设备像素比）和可选的初始设置；返回视图状态并发送第一帧 |
| `view.zoom`、`view.back`、`view.forward`、`view.up`、`view.home` | 放大进入某块（按上一帧的 `key`，或直接给节点 id）、后退、前进、上一级（刚离开的块被选中）、回到根 |
| `view.select` | 选中一块或取消选中 |
| `view.set` | 改细节层级（1–15）、最小元素尺寸、按大小排序、可用/未知空间开关、配色模式（类别 / 扁平 / 时间）、过滤显示方式（暗化 / 隐藏）、视口 |
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

### 当前事件与主题

| 事件 | 说明 |
|---|---|
| `log.record` | `log` 主题：先回放最近若干条（`backlog`），之后实时推送不低于 `min_level` 的后端日志（W8 日志面板） |
| `$/progress` | 长任务进度（LSP 风格） |
| `scan.progress` | `scan` 主题（参数 `{scan_id}`）：订阅时立即推送一次当前状态，之后最多每秒 4 次，结束时再推送一次最终状态 |
| `view.frame` | 不由请求触发的新帧（扫描进行中最多每秒 4 次，扫描结束时一次，设置改变几何参数时一次），随后到达对应的二进制帧 |
| `settings.changed` | 任何一端修改设置、或设置文件被外部编辑后，推送给所有会话的完整设置文档 |

## 4. 二进制帧（数据面）

```
[u32 小端：帧头长度 H][H 字节 UTF-8 JSON 帧头][补 0 到 8 字节对齐][数据区：各列小端、8 字节对齐]
```

帧头（`BinaryFrameHeader`）：`stream`、`request_id`、`seq`、`final`、`layout`（带版本，如 `drawlist.v1`）、`count`（行数）、`columns: {name: {offset, dtype}}`，以及可选的 `blobs: {name: {offset, length}}`。各列都有 `count` 行，排在数据区前面；blob 是排在列之后的原始字节串（8 字节对齐），含义由 layout 规定。偏移量相对数据区起点；dtype 只允许 `<u1 <i1 <u2 <i2 <u4 <i4 <u8 <i8 <f4 <f8`。Python 端解码为只读、零拷贝的 numpy 视图（blob 为 uint8 数组）；JS 端可以在同一块内存上直接建 TypedArray。

### 4.1 绘制列表 `drawlist.v1`

改变视图的方法返回 `ViewUpdate`（JSON：视图状态、`changed`、帧信息 `frame`、过滤信息 `filter`）；画面有变化时，再发一个二进制帧：`stream` 为 `view:<view_id>`，`seq` 等于 `ViewUpdate.frame.seq`，`request_id` 是该请求的 id。响应先于二进制帧到达（响应优先级更高），客户端按（视图，`seq`）配对，参考实现见 `spaceknife_client.views.ViewClient`。

帧信息 `FrameInfo`：画布的物理像素宽高与设备像素比、行数、选中行（选中项被折叠或聚合时是它最近的可见块）、面包屑（`small:N` / `more:N` 表示聚合块）、缩放根下的已扫描合计、过滤统计（匹配文件数、占用与逻辑大小、是否沿用上一次的结果、诊断）、布局耗时。

每一行是一个矩形，父块总在子块之前；坐标是整数物理像素：

| 列 | 类型 | 含义 |
|---|---|---|
| `key` | `<u4` | 稳定 id：节点 id；高 2 位非零时是合成块（「N 个小项」「+N 项」「可用空间」「未知空间」） |
| `node` | `<i4` | 节点 id（聚合块为所在文件夹，可用/未知空间为 -1） |
| `parent` | `<i4` | 所在展开文件夹的行号，顶层为 -1 |
| `kind` | `<u1` | 0 文件 · 1 展开的文件夹（框 + 标题栏）· 2 折叠的文件夹 · 3 等待扫描 · 4 N 个小项 · 5 +N 项 · 6 可用空间 · 7 未知空间 |
| `depth` | `<u1` | 嵌套层级，缩放根的子项为 1（层级色取 min(depth, 4)） |
| `x` `y` `w` `h` | `<i4` | 矩形 |
| `cls` | `<u1` | 文件类别序号（`filter.vocab` 的顺序），用于文件和小项块 |
| `label` | `<u1` | 1 名称 · 2 第二行大小 · 4 大号字 · 8 矮块（只显示名称，垂直居中） |
| `size` | `<i8` | 显示的字节数（面积口径；隐藏模式下是匹配部分） |
| `count` | `<u4` | 「N 个小项」的文件数，「+N 项」的条目数 |
| `flags` | `<u2` | 1 匹配 / 含匹配 · 2 暗化 · 4 命中文件夹掩码（粗体）· 8 自身有标签 · 16 选中 · 32 选中项的祖先（标题栏光晕）· 64 扫描进行中、子树还有没列出的文件夹（只标视图根以下 3 层的文件夹块） |
| `tag` | `<u1` | 自身标签：0 无，1 红，2 黄，3 绿，4 蓝 |
| `tone` | `<u1` | 修改时间档位（时间配色）：0 一天内 … 4 一年以上或未知 |
| `name_len` | `<u4` | 本行名称在 `names` blob 中的字节数（0 表示不画文字） |

blob `names`：各行名称的 UTF-8 编码，按行顺序首尾相接。颜色由前端按共享的令牌规则从 `kind`、`depth`、`cls`、`tone`、`flags` 和配色模式算出。

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
