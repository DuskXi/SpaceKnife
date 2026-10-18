# M6：管理与分析 —— 设计与接口约定

本文是 M6 各部分（删除篮与文件操作、快照对比、模板导出与命令行、洞察规则库、文件类别与设置导入导出、协议与界面）之间的接口契约，按实际实现更新。改接口时先改本文。界面需求见 [UI-SPEC.md](UI-SPEC.md) 的 W5 / W6 / W7。

## 1. 总体

```
 前端（W5 右键菜单 / 删除篮抽屉，W6 对比模式，W7 设置）
   │ WebSocket（path.forms / basket.* / ops.* / compare.* / export.* / classes.* / settings.* …）
 后端
   ops/        删除篮（后端唯一持有）· 受保护路径 · 可释放空间 · 占用检测 · 回收站 / 永久删除 / 撤销 · 路径格式
   diff/       两棵树按路径合并：两端大小、变化量、状态，子树增长与减少，变化单元排行
   export/     SpaceSniffer 模板语言 + CSV / JSON / HTML；命令行 spaceknife run（cli_run.py）
   insights/   已知应用目录的规则库（TOML）：变化原因注释、洞察
   classes.py  可编辑的文件类别表（W7）；settings_io.py 设置导出导入（含 SpaceSniffer 配置）
   server/     ops_service.py · compares.py · *_handlers.py：把上面这些接到协议上
```

- 文件操作只在后端执行；前端只发「要做什么」，后端逐项给结果（错误码 + 中文原因）。执行后由文件变化钩子（M5）更新树；该扫描没有实时监控时，后端对受影响的父目录做定点重读。
- 所有面向用户的文字用中文，每个模块集中在一个 `TEXT` / `MESSAGES` 字典里（M7.5 国际化时抽取）。
- 不在自动化测试里碰用户的真实文件：只在临时目录里建、删、移入回收站，测试结束前还原自己放进回收站的条目；从不清空回收站。

## 2. 删除篮与文件操作（`spaceknife/ops/`、`server/ops_service.py`）

### 2.1 路径格式（`ops/paths.py`）

```python
@dataclass(frozen=True) class PathForms: raw: str; quoted: str; posix: str; uri: str
def path_forms(path: str, *, system: "windows" | "linux" | "darwin" | None = None) -> PathForms
def protected_reason(path: str) -> str | None
```

| 格式 | Windows | POSIX |
|---|---|---|
| raw | `C:\a b` | `/home/a b` |
| quoted | `"C:\a b"` | `'/home/a b'`（`'` 写成 `'\''`，UI-W5-11） |
| posix | WSL 形式 `/mnt/c/a b`；UNC `//host/share/x`（UI-W5-12） | 原样 |
| uri | `file:///C:/a%20b`；UNC `file://host/share/x` | `file:///home/a%20b` |

- `\\?\` 前缀会去掉；不成对的代理项按 WTF-8 编码；POSIX 上无法解码的字节原样百分号编码。
- `protected_reason`：按平台内置清单（卷根、系统目录、Program Files、用户主目录本身、AppData 根、回收站、SpaceKnife 自己的目录、正在运行的程序目录…），还会解析父目录里的链接、检查挂载点。返回中文原因。

### 2.2 删除篮（`ops/basket.py`，每个方法持 RLock）

```python
class Basket:
    def add(self, store, scan_id, nodes, members=None) -> AddResult   # members：一个文件夹的直接子项组成小项组
    def remove(ids) / clear() / items() / get(id) / contains(path) / freeable() -> {卷: 字节}
    def targets(ids=None) -> list[Target]          # 执行时逐项处理的路径（小项组展开为成员）
    def lock_targets() / set_lockers(find_lockers 的结果)
    def apply_results(results) -> list[BasketItem] # 执行后离开删除篮的项（撤销时 reinstate）
    def reinstate(items) -> AddResult
    def refresh(store_of) -> list[str]             # 树变化后按路径重新定位、重算；返回新变成 missing 的 id
AddResult(added: list[id], merged: list[Merged(path, into)], absorbed: list[id], refused: list[Refused(path, reason)])
```

- 去重：已在篮里的路径或其祖先在篮里 → merged；新加的是篮里条目的祖先 → 吸收（absorbed）。拒绝：节点 0、GONE 行、MFT 伪目录（VIRTUAL）里的项、受保护路径。
- 可释放空间：文件为 `alloc`，云占位为 0；硬链接按（卷，ino）合并，只有全部名字都在篮里时计一次，否则注明「另有 N 个硬链接」。Windows 硬扫描没有 ino（=0），硬链接只在 MFT 扫描里可知。

### 2.3 占用检测与回收站（`ops/locks.py`、`ops/trash.py`）

```python
def find_lockers(paths) -> dict[path, list[程序名]]        # Windows Restart Manager；Linux /proc/*/fd；macOS lsof
def trash_status(path) -> TrashStatus(available, reason, location, used, capacity, same_volume)
def move_to_trash(paths, *, progress=None, cancel=None, sizes=None, check_locks=True) -> list[OpResult]
def restore(refs) -> list[OpResult]                      # 不覆盖已有项（exists）
def delete_permanently(paths, *, progress=None, cancel=None, check_locks=True) -> list[OpResult]
OpResult(path, ok, code: ok|missing|denied|locked|protected|no_trash|exists|cancelled|error, message, trash_ref)
```

- **移到回收站从不变成永久删除**：Windows 用 IFileOperation（FOFX_RECYCLEONDELETE），进度回调拒绝任何不进回收站的删除；卷没有回收站、超过回收站容量、路径太长时逐项返回 `no_trash`，原样不动。Linux 按 freedesktop Trash 规范（`renameat2(RENAME_NOREPLACE)`，跨设备返回 `no_trash`，从不复制）。
- 执行前逐项复核：仍存在、仍不受保护、未被占用。

### 2.4 服务层（`server/ops_service.py`）

- 删除篮每个后端一份，所有会话共享，变化推 `basket.changed`；状态里每个卷带 `trash_status`（回收站说明、当前占用）。
- `basket.execute {mode}` → 任务 id；`ops.progress` 推给发起的会话（`mode`、进度、结束时逐项结果、`freed`、`can_undo`）。
- 永久删除先等 `DELETE_DELAY_S`（5 秒）：期间 `ops.undo` / `ops.cancel` 直接作废，什么都没碰。移到回收站完成后 `ops.undo` 用 `restore` 放回原处并把条目放回删除篮。后端保留最近 8 个任务的撤销信息。
- 加入后在后台检查占用（Restart Manager 等），树变化（监控事件）后 1 秒静默再 `refresh`。
- 对比视图里的节点按路径映射到「现在」的树再加入；已删除的项、目标为历史快照时拒绝（「只能操作「现在」还在磁盘上的项」）。

## 3. 快照对比（`spaceknife/diff/`、`server/compares.py`）

```python
def compare(base, target, *, progress=None, cancel=None, fold_case=None) -> DiffTree
class DiffTree:
    store: NodeStore                 # 并集树，广度优先；大小列为目标端（已删除行为基准端）
    base_id / target_id: i4          # 两端的行号，-1 表示不存在
    base_alloc / target_alloc / base_size / target_size: i8   # 文件夹为子树合计
    delta: i8 = target_alloc - base_alloc
    status: u1                        # 0 不变 · 1 新增 · 2 已删除 · 3 变化
    grow / shrink: i8 ≥ 0             # 子树内增长 / 减少（文件为自身）；grow - shrink == delta
    fold_case: bool
    def area() -> grow + shrink       # 对比模式的面积
    def changes(direction, limit, *, scope=0, amount=None, dominant=0.5, minor=0.05) -> list[Change]
Change(node, path, kind, status, amount, delta, base_alloc, target_alloc, base_size, target_size, rest, members)
```

- 匹配：从根开始逐层按名字合并（Windows 规则：`ntpath.normcase`，精确拼写优先）；文件与文件夹同名算「删除 + 新增」，FILE↔LINK 算变化，改名算「删除 + 新增」，只改大小写（Windows 规则）算不变。只改 mtime 算不变。GONE 行不参与。
- **变化单元**：从范围（scope）往下，占父文件夹变化 ≥ 5% 的子项为「显著」；显著子项合计 ≥ 50% 的文件夹被拆开，显著子项各自参与排行，其余部分成为一个「其余」单元（`rest=True`，`members` 为其余子项数）；其他文件夹整体是一个单元，文件各自是单元。单元恰好划分总变化，互不重叠。`amount` 可传入按过滤结果计算的值。
- 规模：800 万 + 750 万行约 5 秒（Windows）/ 3.3 秒（WSL），峰值内存约 2 倍单棵树。

### 3.1 服务层：变化树

- `compare.open {scan_id, base, target}`（`"now"` 或快照文件）在后台加载快照、在计算池里对比（持结构读锁），然后**只保留有变化的行**（及其祖先和根）构成「变化树」：一个小的 `NodeStore`，`alloc` / `size` 为 |变化量|，文件夹为子树合计。完整并集树随即释放。
- 变化树像扫描一样被视图打开（`view.open {scan_id: compare_id}`）：缩放、过滤（大小条件指变化量）、选中、结果列表、帧编码都复用；帧多两列 `delta`（i8，净变化）和 `dstat`（u1）。`view.set {direction: all|grow|shrink}` 按方向重新布局。
- `compare.summary {view_id}`：跟随视图的缩放根和过滤，给出增长 / 减少 / 净变化、相隔时间、日均与写满预测、两张排行（用 `DiffTree.changes` 的规则）。`compare.node {view_id, key}`：悬停卡（两端大小、子项变化、新增项数）。
- 目标为「现在」时跟随文件变化：树静默 3 秒后重算（最多每 10 秒一次），视图按路径迁移到新的变化树。
- 快照的扫描位置必须与当前标签一致，否则失败并给出原因，当前结果保持不变。

## 4. 模板导出与命令行（`spaceknife/export/`、`spaceknife/cli_run.py`）

语言参考见 [export-templates.md](export-templates.md)。

```python
def parse_template(source, name="") -> ExportTemplate      # 从不抛异常；issues 带行列
def render(store, root, template, *, match=None, now=None) -> str   # root=0：每个扫描根依次导出
def render_chunks(...) -> Iterator[str]                    # 流式
BUILTIN_TEMPLATES / BUILTIN_INFO                           # Grouped by folder、First level folders、Simple file list、Summary、Folder tree
FORMATS: csv / json / html；export_csv / export_json / export_html / iter_*
def resolve(name, *, cwd=None) -> ExportTemplate | ExportFormat     # 内置名 → 格式 → 模板文件
def export_chunks(target, store, root, *, match=None, filter_text="") / write_export(path, chunks, *, bom=False)
```

- SpaceSniffer 的名称标签是 `file`（含扩展名）和 `filename`（不含），没有 `name`。
- 命令行 `spaceknife run <命令序列>`（cli.py 注册，实现在 cli_run.py）：`scan`、`load`（.sks）、`filter`、`save`（.sks）、`export`、`autoclose`、`help`，元命令 `@delim=`、`@parser=OS`。只做硬扫描，不提权、不受负载上限约束；`:class:` 使用用户编辑过的类别表，标签条件不可用。
- 界面：命令面板「导出报告…」→ 选模板或格式、范围（当前位置 / 整个扫描）、是否只导出过滤结果 → `export.run`。

## 5. 洞察规则库（`spaceknife/insights/`）

规则格式见 [insights.md](insights.md)。

```python
Insight(rule, title, reason, kind, note="", app="", cleanup="review", command="", path="", inside=False)
def match(path, kind, *, size=None, lang=None) -> Insight | None
def match_node(store, node, *, lang=None) -> Insight | None
def load_rules(extra_dir=None, *, host=None) -> RuleSet       # 内置 110 条 + 用户目录 config/insights/*.toml
```

- 规则用 TOML；W6 的「变化原因注释」取 `note`（如「游戏更新」）。

## 6. 文件类别与设置导入导出（W7）

- `classes.py`：用户可编辑的类别表存在 `config/classes.json`，启动时成为「当前类别表」（`classes.active()`）；过滤引擎、布局着色、解析词汇都读它。`classes.build(edits)` 校验：类别名唯一且不含 `; : " \`；扩展名用 `;`、空格或逗号分隔，去掉开头的点、不分大小写；「其他」不能删、不能填扩展名、始终最后；内置类别改名后旧名字留作别名（`:class:视频` 仍可用）。类别可带颜色（`#RRGGBB`，空为主题色）。
- `settings_io.py`：导出 `{"format": "spaceknife-settings", "version": 1, "values": 非默认值, "classes": 自定义类别表或 null}`；导入先给出计划（将改变的项、新类别表、不导入的项及原因），用户确认后才应用。也读 SpaceSniffer 的 `SpaceSnifferConfig.xml`（每个设置一个元素；颜色 `0x00BBGGRR`；模板为 Base64 的 UTF-16LE）：能对应的设置照搬或换算（字号按 7pt=100%，边框对比按 30=10），配色、刷新间隔、缩放时长、DynaZoom、元素比例不导入；文件组按 SpaceSniffer 的名字和扩展名成为类别，内置类别保留 id，SpaceSniffer 默认的三原色改用主题色，用户自选的颜色保留；SpaceSniffer 没有的「文档」类别保留。

## 7. 协议

| 方法 / 事件 | 说明 |
|---|---|
| `path.forms {scan_id, node}` | 四种格式 + `on_disk`（对比视图里同样可用） |
| `basket.list / add / remove / clear / execute`，事件 `basket.changed` | `add {scan_id, nodes}` 或 `{view_id, key}`（聚合块的成员） |
| `ops.cancel / undo`，事件 `ops.progress` | 见 2.4 |
| `compare.sources / open / info / close / summary / node`，事件 `compare.progress` | 见 3.1 |
| `export.targets / check / run` | 见 4 |
| `classes.get / update / reset`，事件 `classes.changed` | 见 6 |
| `settings.export / import` | 见 6 |
| `system.maintenance / helper_stop / cache_clear`、`tags.clear_saved` | 「高级」页：扫描助手状态、缓存与快照占用、清除已保存的标签 |
