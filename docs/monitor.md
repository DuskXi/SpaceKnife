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
    path: str  # 绝对路径（renamed：新路径；dir：内容变了的目录；overflow：丢失事件的根或子树）
    old_path: str = ""  # renamed：旧路径（可能为空：只知道新名字）
    is_dir: bool | None = None
    size: int | None = None  # modified 且监视器拿得到时：新的逻辑大小 / 占用 / 修改时间（Unix 秒）
    alloc: int | None = None
    mtime: int | None = None


class Watcher(Protocol):
    name: str  # "rdcw" | "inotify" | "fsevents" | "poll"

    def start(self, sink: Callable[[list[Change]], None]) -> None: ...  # 在自己的线程里调用 sink
    # 已进入树的目录（浅层先）；逐目录监视与轮询要用，递归监视忽略。
    # mtimes：扫描记下的各目录 mtime（整秒）；since_ns：不晚于这些列目录的时刻（扫描开始），见下面的「补查」
    def add_dirs(
        self, dirs: Iterable[str], mtimes: Sequence[int] | None = None, since_ns: int = 0
    ) -> None: ...
    def stop(self) -> None: ...  # 1 秒内返回，之后不再调用 sink
    def status(self) -> WatchStatus: ...


@dataclass(frozen=True, slots=True)
class WatchStatus:
    state: Literal["starting", "running", "degraded", "failed", "stopped"]
    detail: str = ""  # 中文一句话，界面直接显示（如「inotify 监视额度不足，3 万个目录改为轮询」）
    watched_dirs: int = 0  # 有效监视的目录数（Windows 递归监视记为整棵树：-1）
    polled_dirs: int = 0  # 靠轮询对账的目录数
    overflows: int = 0
    events: int = 0


def create_watcher(
    root: str, *, poll_interval: float = 30.0, poll_rate: int = 2000, filesystem: str = ""
) -> Watcher:
    ...
    # 按注册表选实现（spaceknife/monitor/base.py）；poll_rate：轮询时每秒最多 stat 的目录数
```

- **监视器注册表**：`register_watcher(WatcherKind(name, factory, platforms, filesystems, priority))`。`create_watcher` 取适用于这个平台和这个文件系统（扫描根所在卷的 `fs_type`）的、优先级最高的一种；工厂抛 `OSError`（这台机器上用不了，比如 CoreServices 不可用）时换下一种，最后是轮询。内置四种照常注册：`rdcw`（Windows）、`inotify`（Linux）、`fsevents`（macOS）优先级 10，`poll` 到处可用、优先级最低，行为与改成注册表之前相同。以后可以按文件系统加监视器（插件提供的也走这里）。
- **插件扫描来源的扫描**没有平台监视器：它的 `handle.monitor` 是 `plugins/sources.py` 的 `SourceMonitor`。重新扫描（F5）就是把来源整个重跑一遍、换上新树，视图按路径保留缩放、选中和历史；来源声明了 `watch = true` 且插件有对应的 `watcher` 时，监控器的变化在安静 1 秒后同样重跑一遍（10 万项以内；更大的树只在状态栏提示「需要重新扫描」）。

- `sink` 收到的批次至少每 50 ms 一批（有事件时），路径是 `str`（Windows 按 Win32 路径；POSIX 按 `os.fsdecode`，不可解码的字节用 surrogateescape，和扫描一致）。
- 监视器自己不合并、不去重；但同一批里同一路径重复的 `modified` 可以只留最后一条。
- 任何时候丢了事件（缓冲区溢出、队列溢出、监视失效），都必须发一条 `overflow`（路径是受影响的最小已知目录），绝不静默丢失。
- **补查（逐目录监视与轮询）**：目录在父目录的列表里出现之后，隔一会儿（流水线按 tick 交出、监视器排队放）才加上监视；它自己的列目录若在加监视之前，这中间的改动不产生事件。所以 `add_dirs` 带上扫描记下的 mtime 时，目录加上 watch 后（轮询：取基线时）stat 一次，按 `poll.listed_changed` 判断：整秒数和记录不同，或者 mtime 不早于 `since_ns` 减去余量（同一秒里分不清在列目录之前还是之后，按变了算；余量照顾文件时间的精度：一般 50 ms，mtime 是整秒时按只记整秒甚至两秒的文件系统算，2.1 s），就发一条 `dir`，由流水线重新列出这个目录。只看目录的 mtime，所以抓得到这段时间里的新建、删除、改名，抓不到已有文件的原地改写（要等它下次变化或重新扫描）。已经在监视或轮询的目录再次交来（父目录 relist 后行号变了）不再核对；根目录在启动时就已监视，第一次交来时核对一次。

### 2.1 Windows：`ReadDirectoryChangesExW`（`monitor/win.py`）

- 每个扫描根一个目录句柄（`FILE_LIST_DIRECTORY`，`FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED`，共享读写删），`bWatchSubtree=TRUE`，`ReadDirectoryNotifyExtendedInformation`（Windows 10 1709+）。第一次读取被拒时依次退回：Ex 64 KiB → 旧接口 `ReadDirectoryChangesW`（`modified` 不带大小，状态说明「此卷不提供扩展变化信息」）。
- 关注：文件名、目录名、大小、最后写入时间、属性、创建。缓冲区 1 MiB（本地卷；网络卷 64 KiB）。读到 0 字节或 `ERROR_NOTIFY_ENUM_DIR` = 溢出：发 `overflow(root)`，继续监视。
- 线程里用重叠 I/O + 事件等待；每次读取完成先复制数据、立即挂下一次读取，再解码交给 sink。`stop` 用 `CancelIoEx` 打断，1 秒内返回。
- 动作映射：`ADDED` → created；`REMOVED` → deleted；文件的 `MODIFIED` → modified（`FileSize`、`AllocatedLength`、`LastModificationTime`；压缩/稀疏文件 alloc 为 None，云占位为 0，常驻文件为 0）；目录的 `MODIFIED` 忽略；`RENAMED_OLD_NAME` + `RENAMED_NEW_NAME` → renamed（旧名等不到新名 50 ms 则当作 deleted）。
- 根目录每秒检查一次（`DeletePending` 与最终路径）：被删除、移动或改名 → 先发 `overflow(root)` 再 `failed`。第一次读取都没挂上的失败（打不开、文件系统不支持通知）只报 `failed`，由流水线改用轮询。
- 8.3 短名的通知不展开：路径找不到节点时按「重新列出父目录」处理。

### 2.2 Linux：inotify（额度）+ 轮询对账（`monitor/linux.py`）

- 每个目录一个 watch，监视器线程看到新目录（`IN_CREATE`/`IN_MOVED_TO`）就立即加 watch 并列一遍，已有条目报 created（覆盖「mkdir 后立刻写入」和整棵树移入）。
- `add_dirs` 交来的目录按批排队（`poll.Backlog`：保存调用方的列表本身和每目录 4 B 的 mtime），线程每轮最多放 2048 个；每加上一个 watch 就做上面的补查（多一次 stat）。
- 额度：进程内所有扫描共享 `max_user_watches` 的 80%（读不到按 8192）；只有父目录有 watch 的目录才分到 watch（浅层先），其余用内嵌的轮询对账，状态 degraded「inotify 监视额度不足，N 个文件夹改为轮询」。
- 扫描列不出的文件夹（`ACCESS_DENIED`，普通用户扫 `/` 时有几十个，如 `/root`）根本不交过来（§5 第 1 步），所以不占额度、不轮询，也不让状态变成「部分轮询」。路径达到 PATH_MAX（4,096 字节）的文件夹 inotify 按路径加不上（ENAMETOOLONG），交给轮询；轮询像扫描一样分段打开它取 mtime（`worker.open_dir`），这时「部分轮询」是真的。
- `IN_Q_OVERFLOW`：发 `overflow(root)`，然后逐个校验 watch：过时的删掉、子树清理并重新遍历。根目录的 `IN_DELETE_SELF` / `IN_MOVE_SELF`：`overflow(root)` + `failed`。
- 文件的 `IN_CLOSE_WRITE` / `IN_ATTRIB` → modified，大小与占用（`st_blocks × 512`）由监视器自己 stat。
- fanotify（`FAN_MARK_FILESYSTEM`）需要 `CAP_SYS_ADMIN`，等 Linux 提权助手（见 TODO）。网络卷与 WSL 经 9P 挂的 Windows 盘：内核收不到对端的改动，流水线直接改用轮询。

### 2.3 macOS：FSEvents（`monitor/mac.py`）

- `FSEventStreamCreate`（`FileEvents | NoDefer | WatchRoot`，latency 0.05 s），专用线程跑 CFRunLoop；`MustScanSubDirs` / `UserDropped` / `KernelDropped` = overflow。
- 构造失败（找不到框架）或一启动就 `failed` 时退回轮询。**未在 macOS 实机验证。**

### 2.4 轮询（`monitor/poll.py`）

- 通用兜底：记录每个目录的 mtime，每 `poll_interval`（30 s）秒比对一轮，每秒最多 stat `poll_rate`（2000）个目录、分小批进行（不经 IO 限速器，靠这个速率保持轻量）。mtime 变了 → `dir`；目录消失 → `deleted`（只报最上层）。取基线时有扫描的记录就先补查。只能发现目录内容的增删改名，发现不了文件内容的大小变化。状态始终是 degraded（「未使用系统的文件变化通知……」）。路径达到 PATH_MAX 的文件夹 stat 会报 ENAMETOOLONG，改为分段打开后 `fstat`（以前一直「说不清」，永远发现不了变化）。

## 3. 提权助手：USN 补偿（Windows，`helper/usn.py`）

- `query_usn(volume)` → `{journal_id, first_usn, next_usn, lowest_valid_usn, max_usn, max_size, allocation_delta}`。
- `read_usn(volume, start_usn, journal_id, max_rows, duty)`：从检查点读到读取开始时的日志末尾（V2/V3 记录），二进制帧（流 `usn`，布局 `usn.v1`）：列 `frn` u64、`parent_frn` u64、`usn` i64、`reason` u32、`attrs` u32、`name_off` u32 / `name_len` u16 与 `names` blob，标量 `{journal_id, next_usn, rows, truncated}`。检查点有效区间 `[max(first_usn, lowest_valid_usn), next_usn]`，区间外或日志 id 变了 → `usn_gone`；日志未开启 → `usn_inactive`。
- 只用已经在运行的已提权助手（MFT 快扫会启动它），不为 USN 专门启动、更不弹 UAC；每 30 秒查一次检查点，助手因此不会空闲退出。检查点分「安全」与「较新」两个，安全的滞后一个周期：溢出时从安全检查点读，覆盖还在缓冲区里没送到的事件。
- FRN 到节点：比较低 48 位（记录号）。MFT 扫描的 `ino` 就是记录号；硬扫描在 Windows 上 `ino` 是完整的 FileId（原生列目录才有）。
- 没有可用的助手、树里没有文件 ID（纯 Python 列目录时）、或记录超过 100 万条：溢出 → 受影响子树标脏重扫。
- 检查点随快照保存、回答「快照之后变了什么」：随 M6 的快照对比做。

## 4. 节点表的增量修改（`spaceknife/store/mutate.py`）

不变量（名字索引、过滤的结构缓存都依赖它）：

1. **行只追加，id 不复用，已有行的名字永不改写。**
2. 每个目录的子项仍是一段连续 id（`child_start` / `child_count`）。通常在目录之后；被 relist 搬到新 id 的目录保留原来的子项区间，可能在它之前，所以读方的防护是「区间不从 0 开始、不越过 `n`」。
3. 已删除或被搬走的行打 `Flag.GONE`：`depth = 0`、`parent = -1`、`kind = OTHER`、大小与合计清零、`child_count = 0`；它们不在任何目录的子项区间里，树遍历到不了；过滤、命令面板搜索屏蔽 `GONE`。

操作：

- `update_stats(store, nodes, size, alloc, mtime)`：原地改文件的大小、占用、时间，差值沿祖先链上推（向量化）。id 不变。
- `relist(store, folder, names, entries, ext)`：按名字匹配旧子项。保留的复制到表尾新区间（目录连同子项区间与合计一起搬，孙辈的 `parent` 改指新行），新出现的建新行（新目录标 `PENDING`，交给增量扫描），消失的连同子树标 `GONE`，旧区间整体标 `GONE`；目录合计重算，差值上推。返回 `moved_from/moved_to`、`removed`（及其占用）、`added`、`pending`、`grown`（及占用差）。
- `can_relist(store, folder)`：已列出、未 GONE、且没有 `PENDING` 子项（扫描的队列按 id 引用它们）。
- `compact(store) -> (new_store, remap)`：`GONE` 行超过全表 25% 且多于 10 万行、扫描不在进行时，在计算池里按广度优先重排（重建名字堆），在结构锁下 `adopt`，名字索引重建，视图按 `remap` 换 id。

结构锁（`store/lock.py`）：计算池里的读操作（渲染、汇总、过滤、搜索）在 `STRUCTURE.read()` 下运行；`relist` / `update_stats` / 压缩在事件循环线程上 `async with STRUCTURE.write()`（异步等读者退出，写的过程同步、不 await）。扫描的追加不需要锁。写者只等它来时已经在跑的读者；这些读者没跑完时新读者照样开始（写者反正进不去），它们跑完后新读者才排到写者后面，所以一个很长的读者（在大盘上建清理方案之类）只耽误改动，不再让所有画面、汇总和别的扫描的视图一起等它，写者也不会被源源不断的短读者饿死。

视图（`view/state.py`）提供 `remap(new_ids, fallback)`：缩放根、选中项、前进/后退历史、小项聚合成员、待闪烁项都换成新 id；指向已删除节点的缩放退回到变化前记下的祖先链里最近的仍存在者。

## 5. 变化流水线（`spaceknife/server/monitors.py`）

1. **启动**：每个扫描（不含快照）一个 `ScanMonitor`，扫描一开始就启动；监视器在线程里启动，原生监视器一启动就失败 → 轮询。扫描进行中每 0.25 s、扫描结束时立即把新目录交给逐目录监视器（inotify、轮询；跳过挂载点和扫描列不出的文件夹：树里没有它们的内容要跟进，inotify 也加不上（EACCES），以前轮询它们只会在 mtime 变了时白白再列一次；它们自己的改名、删除由父目录的监视看到；文件夹等扫描列过之后才交，列之前不知道能不能读，以前提前交出去、后来发现列不出的会一直轮询），连同节点表里各目录的 mtime（Windows 除外：列目录给的目录时间可能来自父目录的索引项，比目录自己的旧；那里递归监视从扫描开始就覆盖整棵树）。`since_ns` 取扫描开始的时刻。
2. **合并**：安静窗口 `scan.coalesce_ms`（默认 150 ms），持续有事件时最长 1 s 也要处理一次。MFT 扫描读取期间事件全部留到整树就绪后再处理。
3. **归类**：文件的 `modified` → `update_stats`（硬扫描的树在 Windows 上按硬扫描的占用规则从大小推算：云占位 0、常驻 ≤ 512 B 为 0、其余按簇；占用未知的压缩/稀疏文件 → 重新列出所在目录）；其余 → 所在目录（`renamed` 两边、`dir` 本身）「要重新列出」。路径还没扫描到 → 交给扫描；目录有 `PENDING` 子项 → 延后到扫描读完；落在扫描列不出的文件夹里（递归监视器 FSEvents、`ReadDirectoryChangesW` 会报来）→ 不去打开它：macOS 的受保护位置一打开就弹授权，其余的本来就列不出。
4. **溢出**：有 USN 检查点 → 按日志补列被动过的目录；否则子树标脏。**事件风暴**（一个窗口里要列的目录 > 2000 或事件 > 20000）→ 公共祖先标脏。标脏的子树安静 2 秒后逐目录重新列出（新目录增量扫描）。
5. **应用**：列目录用扫描的同一套代码（原生或纯 Python，只列一层），扫描进行中经它的 IO 限速器；深的目录先 `relist`（祖先的 relist 会把它的新区间一起带走）；之后新目录增量扫描。
6. **推送**：变化项（新 id）与占用差交给视图（`ViewManager.flash`），视图把它们映射到当前帧的行（块本身、所在聚合块或最近的已绘祖先），随下一帧的 `FrameInfo.flash` 下发；前端闪两次后淡出，「减少动画」时画静态角标（UI-TM-43/44）。可用空间最多每秒重读一次（UI-MAIN-63）。
7. **状态**：`ScanStatus.monitor`（`MonitorStatus`：state、detail、watcher、监视/轮询目录数、事件数、已应用变化数、溢出、待处理、最近一次耗时）。状态栏「实时监控中 / 部分轮询 / 失败 / 已关闭」，点击开关 `scan.monitor`（UI-MAIN-49/52、UI-W1-12）。各平台一样：根目录一加上监视就是 running，扫描进行中也是；逐目录监视器还没给某个文件夹加上监视之前，那里的改动由补查兜住（§2）。
8. **F5 与继续扫描**：`scan.rescan(node)` 把一个文件夹连同子树重新读一遍（实时监控关闭时由不带监视器的被动监控执行）。以前列不出、现在能列的文件夹（权限改了：chmod 不改 mtime，也没有它自己的事件，只能靠这里）列出后去掉 `ACCESS_DENIED`，交给逐目录监视器，之后和别的文件夹一样监视；`scan.continue` 让已停止的硬扫描接着读它队列里剩下的文件夹。

## 6. 验收（来自 PLAN M5）

| 项 | 结果 |
|---|---|
| 并发写入压测：不丢事件，或可检测的溢出并补偿 | 4 个写线程在监视中的临时目录里持续新建、追加、删除、改名、建目录，结束后节点表与重新扫描逐项一致（Windows：rdcw；Linux：inotify）；溢出 → USN 补列或子树重扫，均有测试 |
| 事件到界面 | 合并窗口 150 ms 后应用（小目录一次 relist 约 1–5 ms），下一帧携带闪烁 |
| 长时间运行后的压缩 | `compact` 的等价性有测试；10 万次变化的长时间压测未做（按约定不在用户盘上做长时间 IO） |
