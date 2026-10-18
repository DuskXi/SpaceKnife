# SpaceKnife 开发计划

> 状态：v1，2026-10-18。本文件与 [UI-SPEC.md](UI-SPEC.md) 是实施依据。

SpaceKnife 是跨平台的新一代 SpaceSniffer：保留它的嵌套树图、实时扫描、文件变化钩子、过滤语法、标签、导出模板等精华，去掉只支持 Windows、没有 MFT 快扫、界面过时、扫描慢等问题。目标平台 Windows 10+ / Linux / macOS。

---

## 1. 已拍板的决定

| 决定 | 结论 | 拍板时间 |
|---|---|---|
| 原生扩展 | 允许在 Python 3.12 后端里内嵌 Rust 扩展（PyO3 + maturin，abi3-py312，由 uv 管理），每条原生路径都保留纯 Python 回退 | 2026-10-15 |
| 平台下限 | Windows 10+、RHEL 9+ / Ubuntu 22.04+（glibc 2.34）、macOS 14+；不支持 RHEL 8 / Ubuntu 20.04 | 2026-10-15 |
| 视觉一致的口径 | 共享设计 token、同一套字体和图标、同样的布局；树图由同一份绘制列表渲染并做截图比对（像素接近）；标准控件高度相似即可，不要求逐像素 | 2026-10-15 |
| 提权方式 | UI 和后端以普通权限运行；MFT/USN 快扫通过 UAC 单独拉起一个只读、带令牌鉴权的扫描助手进程；整个程序不提权 | 2026-10-15 |
| 视觉基线 | **A2「石墨 + 沉浸的对比度」**：A「石墨」的布局、外框和详情栏，加上 C「沉浸」的近黑底色、高饱和类别色和 2px 块间隙（树图主题 `carbon`）。B「纸本」否决；C 的悬浮式布局不采用 | 2026-10-18 |
| UI 范围 | 线框 W1–W8、树图元素规范、主界面里出现的**全部功能和状态都保留，必须全部实现**，逐条列在 [UI-SPEC.md](UI-SPEC.md) | 2026-10-18 |

默认取舍（用户未反对，可随时改）：后端承担逻辑和布局，前端只负责显示；节点用列式存储，不为每个文件建 Python 对象；先做 Electron v1，再做 Qt 和 WinUI3；硬链接只计一次；删除默认进回收站；过滤语法合理兼容 SpaceSniffer（见 filter-syntax.md）；键位提供「现代」和「SpaceSniffer 经典」两套预设。需求里写的「QtSlide」按 PySide6 理解。

---

## 2. 总体架构

```
 Electron (React + WebGL2)      Qt (PySide6 / QML)      WinUI3 (win32more)
        │                              └──── spaceknife-client（共享 Python 视图层，不引入 GUI 库）
        └────────────── WebSocket v1：JSON 控制帧 + 二进制数据帧 ─────────────────┐
 后端 Python 3.12 / asyncio（rich + loguru，pydantic 包装所有消息与配置）             │
   会话与视图状态机（焦点、历史、细节层级、过滤、配色、标签、分屏视图）
   树图布局引擎（squarified，按视口计算，下发绘制列表） · 过滤引擎 · 快照与对比 · 导出模板
   删除篮与文件操作 · 洞察规则库 · 日志
   列式节点仓库（numpy 列存 + 名称 blob）
   扫描编排 + IO 限速器 ──► 扫描后端（插件化）      文件监控（分层）
                         └─ spaceknife-native（Rust/PyO3，可选加速）
 └─ 提权只读扫描助手（只在需要 MFT / USN / fanotify 时拉起）
```

**原则**：
- 后端持有树、视图状态并计算布局，三个前端只负责光栅化、动画插值、命中测试和发送操作意图。三端的逻辑和树图画面一致靠这一点保证。
- 节点不建 Python 对象：650 万节点用列式存储约 0.5 GB，建成对象要几个 GB。
- 多进程一律用显式 spawn，线程起来之后不再 fork；进程间只传紧凑的列式批数据；缓存和快照写入做成原子操作；后端检测到 stdin EOF 就退出，防止留下孤儿进程；提权助手只读并强制鉴权。
- Python 3.12 下不要用线程池做逐文件 stat（实测因 GIL 争用慢 3.5–35 倍）；并行靠原生线程（释放 GIL）或 spawn 多进程。

---

## 3. 后端模块

### 3.1 扫描（可插拔后端，按卷自动选择）

| 场景 | 方案 | 实测参考 |
|---|---|---|
| Windows NTFS + 提权助手 | 读原始 $MFT，Rust 并行解析，USN 回放修正 | C: 650 万文件端到端约 7 s，与系统报告差 0.12% |
| NTFS 非管理员 / ReFS / exFAT / FAT / 网络盘 | 并行硬扫描（NtQueryDirectoryFile / FileIdExtdDirectoryInfo） | Rust 11.6 万–71.6 万条/s |
| Linux | openat + getdents64 + statx 并行；btrfs TREE_SEARCH / XFS bulkstat 作为提权可选的索引扫描 | ext4 无索引接口，只能硬扫 |
| macOS | getattrlistbulk 批量枚举；需要完全磁盘访问权限 | 待真机验证 |
| 回退 | 纯 Python：spawn 进程池 + fd 相对遍历；NumPy 向量化 MFT 解析 | MFT 约 1.4M 条/s |

正确性：硬链接按 FileId / (st_dev, st_ino) 只计一次并显示所有链接；压缩/稀疏文件取实际占用；云占位符占用为 0；APFS clone 提供「独占大小」；默认不跨挂载点；NTFS 元文件放进伪目录；可用空间与未知空间（系统报告已用 − 已扫描）作为树图元素；可选扫描 NTFS 备用数据流（ADS）。

### 3.2 IO 动态限速（「最多占用硬盘 X%」）
- 语义（2026-10-18 用户确认，见 UI-SPEC 全局决议 G3）：上限按 SpaceKnife 自身造成的磁盘读取占用计（对应需求「最多允许造成 X%」，用令牌桶按自身实际消耗的磁盘时间扣费实现，不需要管理员权限）；另设「整盘繁忙时让路」开关，默认开启，其他程序已让整盘超过 X% 时降到最低进度（默认 5% 占空比）但不停止。界面同时显示 SpaceKnife 占用和整盘占用两路数据。
- 指标：`rho = min(busy, 平均队列深度 / 设备有效并行度 Q_eff)`（NVMe 上 busy% 会饱和，不能直接用）；Q_eff 先验 HDD 1 / SATA SSD 8 / NVMe 32，按设备在线学习。
- 控制：AIMD 调节并发上限 + 「磁盘时间令牌桶」闸门；HDD 自动低并发并按 inode/FileId 排序。
- 采样：Windows `IOCTL_DISK_PERFORMANCE`（不用 psutil）；Linux `/sys/block/*/stat`；macOS IOKit 统计。
- 档位（五档，三处界面一致）：极速（不限）/ 均衡 70%（手动扫描默认）/ 安静 35%（增量重扫默认）/ 空闲（系统低 I/O 优先级，开环）/ 自定义。
- 状态以 2 Hz 上报给前端（W2 的负载曲线、并发数、自动调节）。

### 3.3 文件变化钩子
- Windows：`ReadDirectoryChangesExW` 扩展信息（事件自带大小）+ USN 日志精确补偿溢出；快照里保存 USN 检查点，回答「快照之后变了什么」。
- Linux：有权限时用 fanotify（FAN_MARK_FILESYSTEM），否则用带 watch 预算的 inotify，最后兜底轮询对账。
- macOS：FSEvents，并利用 sinceWhen 历史回放。
- 不用 watchdog / watchfiles 做主后端（实测丢事件或吞掉溢出信号）。
- 公共流水线：事件合并（150 ms 安静窗口）→ 大小增量沿祖先链上推 → 以不超过 10 Hz 推送差异 → 前端元素闪烁；事件风暴时转为子树标脏，安静后在限速器下重扫。

### 3.4 布局、视图与过滤
- 树图布局在后端按（视图根、视口尺寸、dpr、细节层级、最小元素像素）计算，返回带稳定 id 的矩形和标签列表；缩放动画开始时预取目标布局。
- 视图状态机：每个视图有焦点、前进/后退历史、细节层级、过滤、配色模式、可用/未知空间开关；一次扫描可开多个视图（W8 分屏）。
- 过滤引擎：语法见 [filter-syntax.md](filter-syntax.md)（SKF 1：合理兼容 SpaceSniffer，覆盖通配符、大小、时间、标签、属性、类别、文件夹掩码、取反、组合，满足幂等、完整、全覆盖、无歧义、规范唯一），解析成 pydantic 条件树和规范形式，在列式数据上向量化求值，返回匹配集与按子树汇总的统计。

### 3.5 其他后端能力
快照（自有压缩列式格式，含 USN/FSEvents 检查点；每次扫描完成自动保存，每个扫描根保留最近 10 份另加每周 1 份）与对比（W6）；模板导出（复刻 SpaceSniffer 的 `<%tag%>` + `{if}` 脚本语言，外加 CSV / JSON / HTML 报告 / 树图 PNG）；命令行（scan / load / filter / save / export / autoclose / help，含 `@delim` 与 `@parser=OS`）；删除篮与文件操作（回收站、永久删除、占用检测、撤销）；洞察规则库（YAML，例如 Docker 虚拟磁盘、node_modules、.venv、微信/QQ 数据目录）；日志（loguru，带级别与来源，推送到日志面板）。

---

## 4. 协议
- 连接：`ws://127.0.0.1:{随机端口}/v1`，子协议 `spaceknife.v1`；启动时生成随机 token，经 stdin 交给后端；握手 `session.hello` 校验 token、协商版本与能力；Origin 白名单；关闭 permessage-deflate。
- 控制帧：JSON-RPC 2.0 风格（请求/响应/事件/订阅/取消/进度），全部 pydantic 校验。
- 数据帧：pydantic 描述的帧头 + 小端列式二进制数据（绘制列表、子项列表、扫描批次），JS 端零拷贝读取。实测 10 万节点走 pydantic JSON 约 240 ms，走二进制约 1 ms。
- JSON 里不出现超过 2^53 的整数，节点一律用后端内部 u32 id。
- pydantic 模型是唯一事实来源：生成 JSON Schema → TypeScript 类型；Python 前端直接复用模型；CI 检查生成物与源码一致。

---

## 5. 前端

| 前端 | 技术 | 树图渲染 |
|---|---|---|
| Electron（v1） | Electron 44+ · electron-vite · React 19 · 无样式组件原语（React Aria）· 设计 token 生成的 CSS 变量 + 手写 CSS（M3 决定不用 Tailwind：token 管线已经产出 CSS 变量，三端共用同一套 token，组件样式按 A2 稿逐个落地，比工具类更易与 QML / XAML 对齐） | WebGL2 instanced 画矩形，Canvas2D 叠加层画标签 |
| Qt（v2） | PySide6-Essentials 6.12.x（最后一个支持 Win10 的版本）· QML | 自定义 QSGGeometryNode，numpy 一次写入；动画交给 Qt 引擎，每帧路径上不跑 Python |
| WinUI3（v2） | win32more 0.8.x（锁定精确版本）· XAML | Win2D CanvasControl + D2D SpriteBatch 一次提交；热路径单独封装并做版本守卫 |

- Qt 与 WinUI3 共用 `spaceknife-client`：WS 客户端跑在核心线程，状态与视图模型不引入任何 GUI 库，通过 UiPort 适配（Qt Signal / DispatcherQueue）回到 UI 线程。
- 视觉统一：一份设计 token（DTCG JSON）经 `tools/gen_design.py` 生成 CSS / TS / QML / XAML / Python 产物（M0 实测后改用自研生成器替代 Style Dictionary：不引入额外的 Node 依赖，输出确定、可在 CI 用 `--check` 校验，QML/XAML 的写法也能完全掌控）；三端打包同一套字体（Geist、Geist Mono、Noto Sans SC）和同一套 SVG 图标；golden 截图对比做回归测试。
- 视觉基线 A2 的色板、字体、尺寸、树图元素语义见 [UI-SPEC.md](UI-SPEC.md) 的「主界面与共享外框」和「树图元素与交互」两节。

---

## 6. UI 范围（全部必须实现）

[UI-SPEC.md](UI-SPEC.md) 按界面逐条列出 **484 条**需求（ID 前缀 `UI-`），每条带验收标准；开头的「全局决议」G1–G14 统一了跨界面的口径。**每一条都必须在三个前端里实现**，并在交付清单里逐条勾选。

| 界面 | 规格章节 | 主要内容 | 首次交付里程碑 |
|---|---|---|---|
| 主界面与共享外框 | 00 | 标题栏标签页、工具栏、状态栏、详情栏、悬停信息卡 | M3 |
| 树图元素与交互 | T | 嵌套布局、标题栏、可用/未知空间、小项聚合、选中/悬停/闪烁、标签、过滤暗化、缩放动画 | M3 |
| W1 启动页 | W1 | 卷卡片与扫描方式、路径输入、扫描选项、最近记录、快照拖放 | M3 |
| W2 扫描中 | W2 | 进度条带、边扫边填、提权提速入口、磁盘负载控制与曲线 | M3（负载控制完整功能 M4） |
| W3 过滤 | W3 | 语法着色、自动补全、匹配统计、暗化/隐藏、结果列表、语法错误 | M3 |
| W4 命令面板 | W4 | 最近/命令/路径三组、模糊匹配、快捷键速查 | M3 |
| W5 右键菜单与删除篮 | W5 | 右键菜单、复制路径四种格式、原生 Shell 菜单、删除篮、撤销 | M6 |
| W6 快照对比 | W6 | 对比选择器、变化量树图、增长/减少列表、写满预测、快照时间轴 | M6 |
| W7 设置 | W7 | 扫描与性能等全部设置页 | M3（基础）/ M6（完整） |
| W8 多视图与日志 | W8 | 标签页、分屏、窗格头、日志面板 | M3 |

---

## 7. 仓库结构

```
pyproject.toml                 # uv workspace 根
packages/
  protocol/   spaceknife-protocol   pydantic 消息模型（三端共用的事实来源）
  backend/    spaceknife-backend    服务、扫描编排、存储、布局、过滤、监控、导出、CLI
  native/     spaceknife-native     Rust/PyO3：MFT、并行遍历、（后续）布局与过滤加速
  client/     spaceknife-client     Python 前端共享视图层（无 GUI 依赖）
  ui-qt/      spaceknife-qt         PySide6 / QML
  ui-winui/   spaceknife-winui      win32more / WinUI3
apps/electron/                      electron-vite + React + WebGL2
design/tokens/                      DTCG 设计 token → tools/gen_design.py
design/icons/                       共享 SVG 图标
i18n/                               zh-CN、en
docs/                               PLAN.md、UI-SPEC.md、protocol.md
tools/                              代码生成、基准测试
```

---

## 8. 里程碑

| 里程碑 | 内容 | 完成标准 |
|---|---|---|
| **M0 骨架** | uv workspace、rich + loguru、protocol 包、WS 服务与握手鉴权、CLI 骨架、CI（lint / 类型 / 测试）、设计 token 管线、图标与字体资源 | `spaceknife serve` 能被客户端握手连接；token 生成 CSS/TS/QML/XAML/py 产物 |
| **M1 扫描核心** | 列式节点仓库、纯 Python 并行硬扫描（spawn）、可用/未知空间、扫描进度流、快照保存/加载与扫描后自动保存（保留策略）、硬链接与占用口径 | 扫描结果与 `du`/资源管理器对账；快照往返无损 |
| **M2 布局与视图** | 后端布局引擎（含小项聚合规则 G5）、视图状态机（历史、细节层级、多视图）、过滤语言全语法、绘制列表二进制帧、标签与配色模式；**用项目根目录的 SpaceSniffer.exe 实测过滤组合语义，做成兼容性测试集（G6）** | 兼容性测试集与过滤语法测试集 100% 通过；4K 视口布局 < 120 ms |
| **M3 Electron v1** | 主界面与共享外框、树图（含缩放动画、悬停信息卡、选中、标签）、W1、W2（基础）、W3、W4、W7（基础）、W8 | UI-SPEC 中标为 M3 的需求全部勾选；缩放动画 60 fps |
| **M4 原生加速与快扫** | Rust 扩展：MFT 解析、并行硬扫描（三平台）；提权扫描助手；IO 限速器与 OS I/O 优先级；W2 负载控制完整功能 | C: 级卷 MFT 扫描 < 10 s；限速目标误差 ±10% |
| **M5 文件变化钩子** | RDCW-Ex + USN、fanotify/inotify、FSEvents；增量流水线；元素闪烁；快照后变化 | 并发写入压测无事件丢失（或可检测的溢出并补偿） |
| **M6 管理与分析功能** | W5 右键菜单（含原生 Shell 菜单）与删除篮、W6 快照对比、W7 完整设置、模板导出、命令行完整参数、洞察规则库 | 对应 UI-SPEC 需求全部勾选 |
| **M7 原生 UI** | spaceknife-client 共享视图层；Qt 与 WinUI3 实现全部界面；三端 golden 截图对比 | 三端逐界面勾选 UI-SPEC 全部需求；树图截图差异在阈值内 |
| **M7.5 国际化（i18n）** | 界面文案、日志与事件、设置说明、命令面板、错误信息全部抽成资源；先支持简体中文与英文；首次启动按系统语言自动选择（Windows `GetUserDefaultUILanguage`、Linux `LC_ALL`/`LC_MESSAGES`/`LANG`、macOS `AppleLanguages`），设置里可切换并即时生效；数字、大小单位、日期按语言格式化；过滤语法的关键字与类别名支持中英两套 | 两种语言下 UI-SPEC 需求逐条可用，无遗漏的硬编码文案（有检查脚本） |
| **M8 打包与发布** | Electron（NSIS / DMG / deb / rpm / AppImage）、PyInstaller onedir（Qt、WinUI3）、Windows App Runtime 安装、签名与公证、三平台验证 | 三平台安装包在干净机器上通过冒烟测试 |

---

### 进度

| 里程碑 | 状态 | 说明 |
|---|---|---|
| M0 骨架 | 完成（2026-10-18） | 字体文件推迟到 M3（需要下载 OFL 字体包）；亮色主题待设计评审 |
| M1 扫描核心 | 完成（2026-10-18） | 并行硬扫描（spawn 进程池，`C:\Program Files` 36 万项约 2 秒）、列式节点仓库、卷信息、快照与自动保存、协议 18 个方法；Windows 与 Linux（WSL）全量测试通过。Windows 硬扫描的占用按簇推算、硬链接按每个链接计入，精确值待 M4 的 MFT 快扫；NTFS 备用数据流扫描也推迟到 M4 |
| M2 布局与视图 | 完成（2026-10-18） | 过滤语言 SKF 1（[filter-syntax.md](filter-syntax.md)，按「合理兼容」原则定稿：SpaceSniffer 实测 170 条，163 条一致，其余为文档第 9 节列出的扩展与有意差异）；向量化过滤求值；squarified 嵌套布局（像素对齐、细节层级、G5 小项聚合与「+N 项」、可用/未知空间、等待扫描块、标签级别）；视图状态机（历史、细节层级、配色、过滤暗化/隐藏、标签持久化、选中与祖先光晕）；协议 `filter.*` 与 13 个 `view.*` 方法、`drawlist.v1` 二进制帧（新增 blob 区）。性能（Windows，300 万节点合成树）：4K 布局常规 7 ms、最坏（层级 15）84 ms；过滤 44–90 ms，新通配符首次 160–560 ms（后续由 Rust 扩展加速）。Windows 636 项、Linux（WSL）446 项测试通过。修复：Windows 上 `--token-stdin` 阻塞读 stdin 会卡住进程池的 CreateProcess |
| M3 Electron v1 | 完成（2026-10-18） | 主界面与外框、WebGL2 树图（缩放动画、悬停卡、选中、标签）、W1 启动页（介质识别、路径校验与合并、拖入、最近记录）、W2 扫描进度带与扫描详情（滑动速率、按上次项数估算进度与剩余时间）、W3 过滤与「匹配结果」（按可见块分组、分页、定位）、W4 命令面板（后端统一排序与命中区间、拼音首字母、路径分档搜索，130 万项多数查询 < 100 ms）、W7 设置（元数据驱动、即时保存、外部编辑热加载、按页恢复与撤销）、W8 分屏窗格与日志面板（结构化事件、定位与复制、扫描统计）。协议 46 个方法；样式改用 token 生成的 CSS 变量（不用 Tailwind）。依赖 M4–M6 的项（MFT、负载限速、暂停、快照对比、删除篮、右键菜单）在界面上禁用并注明。并行审查修复约 30 处问题（名称索引线程安全、不可编码文件名、进程池损坏恢复、小项聚合放大死路等）。测试：后端 Windows 773 项、Linux（WSL）586 项，前端 vitest 105 项 |
| M4 原生加速与快扫 | 完成（2026-10-18） | 接口与实测见 [native.md](native.md)。Rust 扩展：流式 MFT 读取与解析（32 MiB 窗口，读与解析重叠，占空比限速；C: 837 万项在安静档 22 秒，助手峰值 1.6 GB，整块读入要 10.6 GB）、并行硬扫描遍历（Windows `NtQueryDirectoryFile`、Linux `getdents64`，与纯 Python 逐字段一致；macOS 代码未编译验证）；只读扫描助手（命名管道、一次性令牌、受保护 DACL；后端已有管理员权限时不弹 UAC，拒绝授权回退硬扫描）；每块物理磁盘一个 IO 限速器（忙碌度与队列深度折算的负载、AIMD + 占空比闸门、基线探测、五档 + 自定义、系统低 I/O 优先级）；扫描方式自动选择（整个本地 NTFS 卷用 MFT）、硬扫描中途切换 MFT；W2 完整：暂停/继续、扫描中改档、并发与自动调节、近 60 秒负载曲线、状态栏实时负载。另：MFT 快扫可计入备用数据流、快照记录扫描方式、MFT 伪目录的项禁用「在资源管理器中显示/加入删除篮」、MFT 建树峰值下降且看门狗计入建树。修复：`$BadClus:$Bad` 被算成整卷大小（C: 显示 7.2 TB）；两次内存暴涨（扫描中读到半写入的子项区间、无界并发计算），见 [memory.md](memory.md)。未做或未验证：满速 MFT 扫描与限速精度没有在用户磁盘上压测（按约定只做短时低强度实测）；硬扫描端到端受 Python 归并限制（约 25 万项/秒，向量化归并待做）；Linux/macOS 的提权助手未实现；已停止扫描的「继续扫描」随 M5。测试：后端 Windows 974 项、Linux（WSL）782 项，前端 vitest 105 项 |
| M5 文件变化钩子 | 完成（2026-10-18） | 契约与实测见 [monitor.md](monitor.md)。监视器：Windows ReadDirectoryChangesExW（递归，事件自带大小，溢出 / 根目录被删或移走都会报告）、Linux inotify（进程内共享额度，超出与队列溢出后轮询对账）、macOS FSEvents（未实机验证）、通用轮询；网络卷与 WSL 的 9P 盘用轮询。提权助手 query_usn / read_usn：溢出时按 USN 日志补列被动过的目录（只用已在运行的助手）。节点表增量修改（store/mutate.py：update_stats、relist、compact，GONE 行与结构锁）；变化流水线（合并窗口、深目录先 relist、新目录增量扫描、事件风暴与溢出时子树标脏安静后重扫、扫描中延后有待扫描子目录的目录、MFT 读取期间暂存事件）；视图 id 重映射与树图闪烁（减少动画时静态角标）；状态栏实时监控状态与开关；F5 重新扫描当前视图；已停止扫描的「继续扫描」。WSL：9P 挂载的 Windows 盘标为不推荐、隐藏基础设施挂载；ext4 root 保留块不再算作已用；虚拟盘识别。验收：4 个写线程并发压测后节点表与重新扫描逐项一致（Windows、Linux）。未做：10 万次变化的长时间压测、Linux fanotify、扫描中「正在扫描」闪烁。测试：后端 Windows 1063 项、Linux（WSL）865 项，前端 vitest 105 项 |
| M6 管理与分析 | 完成（2026-10-18） | 契约见 [manage.md](manage.md)。W5：统一右键菜单（键盘与结果列表同样可用）、复制路径四种格式（POSIX 用单引号、Windows 的 POSIX 形式为 WSL 路径）、后端持有的删除篮（去重与吸收、硬链接与云占位、占用检测）、移到回收站（从不变成永久删除，撤销即还原）与 5 秒延迟的永久删除。W6：快照对比引擎（800 万 + 750 万行约 5 秒），只保留变化的「变化树」复用视图管线，面积 = 变化量、色阶与图例、变化单元排行与原因注释、悬停卡、时间轴、「现在」随文件变化重算。W7：文件类别编辑、标签显示名、设置导出 / 导入（含 SpaceSniffer 配置，导入前预览）、高级页工具、字号 / 对比度 / 元素比例 / 帧率 / 拖尾。模板导出（SpaceSniffer 模板语言 + CSV / JSON / HTML）、`spaceknife run` 命令行、洞察规则库（110 条）。协议 79 个方法。未完成项（原生 Shell 菜单、DynaZoom、改键、对比报告导出、自定义导出模板、洞察卡片、预览）见 [TODO.md](../TODO.md)。测试：后端 Windows 1431 项、Linux（WSL）1245 项，前端 vitest 105 项 |
| M7 原生 UI | 挂起（2026-10-18） | 用户决定暂不做；Qt / WinUI3 的打包与 Windows App Runtime 一并挂起。待办见 [TODO.md](../TODO.md) |
| M7.5 国际化 | 完成（2026-10-18） | 见 [i18n.md](i18n.md)。gettext 式：中文原文即消息 id，英文词表按区域分文件（后端 3 份、前端 4 份，约 1,550 条，术语表统一）；「界面语言」跟随系统 / 简体中文 / English，即时切换（Electron 把系统界面语言传给后端；后端按设置翻译设置项、命令面板、错误、事件、导出报告与命令行；前端整树重渲染、树图标签重画）；过滤关键字一套 ASCII，内置类别名中英两套；检查脚本 tools/i18n_check.py 进测试。测试：后端 Windows 1456 项、Linux（WSL）1258 项，前端 vitest 108 项 |
| M8 打包与发布 | 范围调整（2026-10-18） | 先做 Windows 与 Linux 的 Electron 安装包；macOS 打包、签名与公证暂不做（Apple 开发者证书需要付费） |

## 9. 测试与验收
- 每条 `UI-` 需求按其验收标准测试，三端各自勾选；交付时附需求追踪表。
- 扫描正确性：与 `GetDiskFreeSpaceEx` / `statfs` 合计、`FSCTL_QUERY_FILE_LAYOUT` 结果、`du` 交叉校验。
- 性能基准：MFT 扫描、硬扫描吞吐（冷/热缓存、SSD/HDD）、布局耗时、绘制帧时间、WS 吞吐。
- 视觉回归：同一份绘制列表在三端渲染后做截图比对。
- 过滤语法、导出模板语言、命令行解析各有独立测试集。

---

## 10. 风险
- win32more 是单人维护的 Alpha 项目，快路径依赖其私有 API：锁定版本、加回归基准、失效时自动回落公开 API。
- Qt 6.12 是最后一个支持 Win10 的版本：长期停在 6.12.x。
- 不同平台的文字渲染引擎不同：标签省略号位置可能差几个像素，按「像素接近」验收。
- macOS 与真实 HDD 上的扫描、限速、监控都还没实机验证，需要补测。
- 应用内删除有数据风险：受保护路径拦截、占用检测、默认进回收站、永久删除二次确认。

---

## 11. 待定事项

跨界面的口径已在 [UI-SPEC.md](UI-SPEC.md) 第 1 节「全局决议」G1–G14 定下：外框取 A2 视觉、功能取并集；负载上限五档；扫描后自动存快照；小项聚合规则；过滤组合语义以实测为准；分屏时工具栏让给窗格头；查找同名位置做成正式功能；写满预测保留并标注估算；删除篮跳过被占用项；详情栏三页签保留；Ctrl+F 随键位预设；大小单位跟随平台。各节里其余「待定事项」按其「建议默认」执行。

磁盘负载上限的语义（G3）已于 2026-10-18 确认：按 SpaceKnife 自身造成的占用计，另带「整盘繁忙时让路」开关（默认开启）。目前没有需要用户确认的待定事项。
