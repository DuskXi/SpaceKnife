# M5：文件变化钩子 —— 设计与接口约定

本文是 M5 各部分（平台监视器、提权助手的 USN 读取、节点表增量修改、变化流水线、协议与界面）之间的接口契约。改接口时先改本文。

## 1. 总体

```
 平台监视器（线程，每个扫描根一个）           后端（asyncio）
   Windows  ReadDirectoryChangesExW ─┐
   Linux    inotify（预算）+ 轮询对账 ─┼─► Change 批 ─► Monitor：合并（安静窗口）─► 应用到节点表 ─► 推送
   macOS    FSEvents（失败时轮询）   ─┤                  │  事件风暴 → 子树标脏 → 安静后限速重扫
   通用     轮询对账（兜底）          ─┘                  └─ 溢出 → USN 补偿（助手）或标脏重扫
```

- 监视器只负责「哪里变了」，不负责「变成了什么」：结构变化（新增、删除、改名）一律由后端重新列出所在目录得到准确结果；只有「文件大小或时间变了」可以直接用事件自带的数值原地更新（Windows 扩展信息自带大小、占用和时间）。
- 不用 watchdog / watchfiles 做主后端（实测丢事件或吞掉溢出信号）。
- 扫描一开始就启动监视（UI-W2-09），只对已扫描的部分应用变化；还没扫描到的目录本来就会被扫到。快照打开的标签不监视（UI-W1-46）。
- 设置：`scan.monitor`（开关，默认开）、`scan.coalesce_ms`（安静窗口，默认 150 ms）、`scan.load_incremental`（标脏重扫的磁盘负载档位，默认安静）、`treemap.flash_changes`（扫描或事件时闪烁）。

## 2. 监视器接口（`spaceknife/monitor/`）

```python
@dataclass(frozen=True, slots=True)
class Change:
    kind: Literal["modified", "created", "deleted", "renamed", "dir", "overflow"]
    path: str              # 绝对路径（renamed：新路径；dir：内容变了的目录；overflow：丢失事件的根或子树）
    old_path: str = ""     # renamed：旧路径（可能为空：只知道新名字）
    is_dir: bool | None = None
    size: int | None = None    # modified 且监视器拿得到时：新的逻辑大小 / 占用 / 修改时间（Unix 秒）
    alloc: int | None = None
    mtime: int | None = None

class Watcher(Protocol):
    name: str                                   # "rdcw" | "inotify" | "fsevents" | "poll"
    def start(self, sink: Callable[[list[Change]], None]) -> None: ...   # 在自己的线程里调用 sink
    def add_dirs(self, dirs: Iterable[str]) -> None: ...  # 已进入树的目录（浅层先）；逐目录监视与轮询要用，递归监视忽略
    def stop(self) -> None: ...                 # 1 秒内返回，之后不再调用 sink
    def status(self) -> WatchStatus: ...

@dataclass(frozen=True, slots=True)
class WatchStatus:
    state: Literal["starting", "running", "degraded", "failed", "stopped"]
    detail: str = ""        # 中文一句话，界面直接显示（如「inotify 监视额度不足，3 万个目录改为轮询」）
    watched_dirs: int = 0   # 有效监视的目录数（Windows 递归监视记为整棵树：-1）
    polled_dirs: int = 0    # 靠轮询对账的目录数
    overflows: int = 0
    events: int = 0

def create_watcher(root: str, *, poll_interval: float = 30.0, poll_rate: int = 2000) -> Watcher: ...
    # 按平台选实现（spaceknife/monitor/base.py）；poll_rate：轮询时每秒最多 stat 的目录数
```

- `sink` 收到的批次至少每 50 ms 一批（有事件时），路径是 `str`（Windows 按 Win32 路径；POSIX 按 `os.fsdecode`，不可解码的字节用 surrogateescape，和扫描一致）。
- 监视器自己不合并、不去重；但同一批里同一路径重复的 `modified` 可以只留最后一条。
- 任何时候丢了事件（缓冲区溢出、队列溢出、监视失效），都必须发一条 `overflow`（路径是受影响的最小已知目录），绝不静默丢失。

### 2.1 Windows：`ReadDirectoryChangesExW`

- 每个扫描根一个目录句柄（`FILE_LIST_DIRECTORY`，`FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED`，共享读写删），`bWatchSubtree=TRUE`，`ReadDirectoryNotifyExtendedInformation`（Windows 10 1709+；更老的系统用 `ReadDirectoryChangesW`，`modified` 不带大小）。
- 关注：文件名、目录名、大小、最后写入时间、属性、创建。缓冲区 1 MiB（本地卷；网络卷 64 KiB）。读到 0 字节或 `ERROR_NOTIFY_ENUM_DIR` = 溢出。
- 线程里用重叠 I/O + 事件等待；`stop` 用 `CancelIoEx` 打断。
- 动作映射：`ADDED` → created；`REMOVED` → deleted；`MODIFIED` → modified（带 `FileSize`、`AllocatedLength`、`LastModificationTime`；目录的 MODIFIED 忽略）；`RENAMED_OLD_NAME` + `RENAMED_NEW_NAME` → renamed。

### 2.2 Linux：inotify（预算）+ 轮询对账

- 每个目录一个 watch（`IN_CREATE | IN_DELETE | IN_MOVED_FROM | IN_MOVED_TO | IN_CLOSE_WRITE | IN_ATTRIB | IN_DELETE_SELF | IN_MOVE_SELF | IN_ONLYDIR | IN_DONT_FOLLOW`），新目录出现时补加 watch。
- 预算：`/proc/sys/fs/inotify/max_user_watches` 减去已用（取不到时按 8192），最多用其中 80%；按广度优先（浅层先）分配。超出预算的子树，以及 `IN_Q_OVERFLOW` 之后，用轮询对账：按目录 mtime 发现增删（每 `poll_interval` 秒一轮，经 IO 限速器）。
- fanotify（`FAN_MARK_FILESYSTEM`）需要 `CAP_SYS_ADMIN`，等 Linux 提权助手（见 TODO）。

### 2.3 macOS：FSEvents

- `FSEventStreamCreate`（`kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagNoDefer | kFSEventStreamCreateFlagWatchRoot`，latency 0.05 s），专用线程跑 CFRunLoop；`MustScanSubDirs` / `UserDropped` / `KernelDropped` 标志 = overflow。
- 创建失败（ctypes 找不到框架等）时退回轮询。**未在 macOS 实机验证。**

### 2.4 轮询（`poll`）

- 通用兜底：记录每个已扫描目录的 mtime，每 `poll_interval` 秒比对一次（经 IO 限速器、分批），mtime 变了 → `dir`。只能发现目录内容的增删改名，发现不了文件内容大小变化（文件 mtime 不传到目录）。

## 3. 提权助手：USN 补偿（Windows）

- 新操作 `read_usn(volume, start_usn, journal_id)`：`FSCTL_QUERY_USN_JOURNAL` 取日志 id 和当前位置；从 `start_usn` 读 `FSCTL_READ_USN_JOURNAL`（V2/V3 记录），返回 `{journal_id, next_usn, rows: [(frn, parent_frn, reason, name)]}`（分帧，同 scan_mft 的二进制帧）。日志 id 变了或 `start_usn` 已被截断 → 错误码 `usn_gone`。
- 后端在扫描开始时记下 `(journal_id, next_usn)` 作为检查点（随快照保存，M6 的「快照后变化」用）。溢出时读检查点之后的记录，取出所有 `parent_frn` → 对应目录重新列出；之后把检查点推进到 `next_usn`。
- FRN 到节点：MFT 扫描的 `ino` 就是记录号；硬扫描在 Windows 上 `ino` 是 FileId（低 48 位 = 记录号）。后端按需建 `ino → 节点` 的排序索引。
- 没有已提权的助手（不弹 UAC 去要权限）时：溢出 → 受影响子树标脏重扫。

## 4. 节点表的增量修改（`spaceknife/store/mutate.py`）

不变量（名字索引、过滤结构缓存都依赖它）：

1. **行只追加，id 不复用，已有行的名字永不改写。**
2. 每个目录的子项仍是一段连续 id（`child_start` / `child_count`），子项永远在目录之后。
3. 已删除或被搬走的行打 `Flag.GONE`：`depth = 0`、`parent = -1`、`kind = OTHER`、大小与合计清零、`child_count = 0`；它们不在任何目录的子项区间里，树遍历到不了；对全表做向量化扫描的地方（过滤、名字搜索、结果列表、命令面板）必须屏蔽 `GONE`。

操作：

- `update_stats(store, nodes, size, alloc, mtime, …) -> Delta`：原地改文件的大小、占用、时间；差值沿祖先链上推（同一批按层向量化）。id 不变。
- `relist(store, dir, listing) -> Relist`：目录的新内容（和扫描 worker 一样的列）。按名字匹配旧子项：保留的子项复制到新行（目录连同自己的子项区间与合计一起搬，孙辈的 `parent` 改指新行），新出现的子项建新行（新目录标 `PENDING`，交给增量扫描），消失的子项连同整棵子树标 `GONE`。新区间追加在表尾，旧区间整体标 `GONE`。目录合计按新子项重算，差值沿祖先链上推。返回 `remap`（旧 id → 新 id）、`removed`、`added`、`pending`（要扫描的新目录）和每个变化项的占用差（闪烁角标用）。
- `compact(store) -> remap`：`GONE` 行超过全表 25% 且多于 10 万行时，在计算池里按广度优先重排整张表（重建名字堆），之后名字索引重建、视图按 remap 换 id。
- 发布顺序沿用 M4：先写行，再改 `_n`，再写 `child_start`，最后写 `child_count`；读方的保护（`child_range`、`subtree_ids`）不变。

视图（`view/state.py`）提供 `remap(fn)`：缩放根、选中项、前进/后退历史、小项聚合的成员数组都换成新 id；指向已删除节点的条目退回到最近的仍存在的祖先。

## 5. 变化流水线（`spaceknife/monitor/manager.py`）

1. **合并**：每个扫描一个队列，安静窗口 `scan.coalesce_ms`（默认 150 ms），持续有事件时最长 1 s 也要处理一次。
2. **归类**：`modified` 且带数值、路径对应已扫描的文件 → `update_stats`；其余 → 所在目录（`renamed` 两边的目录、`dir` 本身）记为「要重新列出」。路径还没扫描到 → 忽略（扫描会读到最新状态）。
3. **事件风暴**：一个窗口里要重新列出的目录超过 2000 个、或事件超过 20000 条 → 改为把它们的最近公共祖先标脏；溢出同理。标脏的子树在安静 2 秒后按 `scan.load_incremental` 档位经 IO 限速器重扫（重扫结果整体替换该子树，和 relist 一样走 remap）。
4. **应用**：重新列出用扫描 worker 的同一套列目录代码（原生或纯 Python），只列这一层；结果交给 `relist`。新目录（`pending`）排进增量扫描（同一个扫描器，限速档位同上）。
5. **推送**（不超过 10 Hz）：`scan.changes` 事件 = 变化的节点（新 id）、占用差、变化种类；视图据此生成闪烁列表（只含当前可见的块，或其最近的可见祖先），随下一帧绘制列表下发（UI-TM-43）；状态栏的可用空间与详情栏同步刷新（UI-MAIN-63）。
6. **状态**：`monitor.status` / `monitor` 主题：每个扫描的 `WatchStatus`，加上待处理队列长度与最近一次应用耗时。状态栏「实时监控中」/「实时监控 · 降级」/「实时监控已关闭」（UI-MAIN-49、UI-W1-12）。

## 6. 验收（来自 PLAN M5）

- 并发写入压测：多线程在监视的目录里持续新建、追加、改名、删除文件（短时、低强度、只在临时目录），结束后节点表与重新硬扫描的结果逐项一致；溢出时有 `overflow` 记录并补偿到一致。
- 事件到界面：单个文件变化在 `coalesce_ms + 100 ms` 内反映到树图并闪烁。
- 长时间运行（模拟 10 万次变化）后，`GONE` 行压缩生效，内存回到扫描结束时的水平附近。
