# M4：原生加速、快扫与 IO 限速 —— 设计与接口约定

本文是 M4 各部分（Rust 扩展、提权扫描助手、IO 限速、扫描调度）之间的接口契约，内容与 M4 完成时的实现一致。改动接口时先改本文。

> **现状（0.3.5 起）**：后端已整体改写为 Rust，原来的 PyO3 扩展 `spaceknife_native`（`packages/native`）现在是普通的 Rust 库 `crates/sk-native`（`sk_native`：`hardscan`、`mft`、`fold`、`columns`、`cpus`；没有 Python、没有 GIL），由 `crates/sk-core` 和 `crates/sk-store` 通过它自己的接口调用，它不依赖工作区里的任何其他 crate（性能层：遍历、`$MFT` 解析与建树、切片归并的逐项工作都在这里；后端的策略——伪目录的名字、store 的内存、锁和发布顺序、ACCESS_DENIED 与待扫列表——留在后端）。下文 §2 的 Python 签名（`def scan_slice(...)`、`MftTable` 的 numpy 列、`py.allow_threads`、「纯 Python 回退」）记录的是 M4 时的接口；行为、选项名、环境变量和测量结论在 Rust 版里保持不变，对应的 Rust 接口见 [crates/sk-native/README.md](../crates/sk-native/README.md)。对应关系：
>
> | 本文写的（Python 时期） | 现在 |
> |---|---|
> | `packages/native/src/mft/`、`src/hardscan/` | `crates/sk-native/src/mft/`、`crates/sk-native/src/hardscan/` |
> | `src/mft/build.rs`（`mft_build`） | `crates/sk-native/src/mft/build.rs`（`build_tree`：建树本身，写进 store 借出的列） |
> | `spaceknife/scan/mft.py` 的 `table_to_store` | `crates/sk-core/src/scan/mft.rs`（`table_to_store`：伪目录的名字、给建树一个 `StoreBuilder`） |
> | `src/fold.rs`（`SliceFold`） | `crates/sk-native/src/fold.rs`（`SliceFold`、`write_rows`：核对与写行） |
> | `NodeStore.ingest_slices` | `crates/sk-store/src/store.rs`（`ingest_slice`：借出新行、按自己的顺序发布） |
> | `spaceknife/scan/scanner.py`、`worker.py` | `crates/sk-core/src/scan/`（`scanner.rs`、`gate.rs`、`winopts.rs`） |
> | `spaceknife/helper/` | `crates/sk-core/src/helper/` |
> | `spaceknife/io/` | `crates/sk-core/src/io/` |
> | `server/scans.py` | `crates/sk-server/src/scans.rs` |
> | `tests/test_*.py` | `crates/sk-native/tests/`、`crates/sk-store/tests/`、`crates/sk-core/tests/` |
>
> 纯 Python 的遍历器、numpy 建树与归并已随 Python 后端一起去掉，没有回退开关（`SPACEKNIFE_PY_FOLD` 等）。

## 1. 总体

```
 后端（普通权限，sk-server / sk-core，在应用进程里）
   scans ──► Scanner ──► 扫描策略
                           ├─ 硬扫描：sk_native::hardscan::scan_slice（列目录线程）
                           └─ MFT 快扫：SharedHelper ──命名管道──► 只读扫描助手（管理员，`<程序> helper …`）
                                                                   └─ scan_mft：边读边解析（sk_native::mft）
   IoRegistry → IoGovernor（每块物理磁盘一个）：采样 → 控制并发与派发节奏 → 2 Hz 上报（W2 曲线、状态栏）
```

- （M4 时）原生扩展是可选的，所有原生路径都有纯 Python 回退，结果逐列一致。现在 `sk-native` 是工作区里的普通 crate，总是编译进去，没有回退。
- 内部用 rayon 或自有线程并行。
- 整个程序不提权；需要管理员权限的只有只读扫描助手，它只做白名单内的只读操作，用一次性令牌鉴权。
- 构建与测试：`cargo build`、`cargo test -p sk-native`（工作区根目录）。

## 2. Rust 库 `sk_native`（crates/sk-native；M4 时是 PyO3 扩展 `spaceknife_native`）

模块布局：`src/lib.rs` 只做导出；`src/mft/` 与 `src/hardscan/` 各自独立（feature `mft` / `hardscan`，默认都开），`src/cpus.rs` 是本进程可用的 CPU 数。节点表的归并（2.3）现在在 `crates/sk-store` 里。下面的 Python 签名是 M4 时的接口（见文首「现状」）。

### 2.1 MFT（`src/mft/`）

```python
def mft_volume_info(volume: str) -> dict[str, int]:
    ...
    # Windows：打开 \\.\X:（需要管理员），几何信息取自引导扇区（不用 FSCTL）：bytes_per_sector,
    # cluster_size, record_size, mft_lcn, mft_size, mft_valid_size, mft_fragments, volume_size。


def scan_mft(volume: str, duty: float = 1.0, progress=None, stats=None) -> tuple[MftTable, dict[str, int]]:
    ...
    # 主路径（src/mft/scan.rs、io.rs）：$MFT 按 data runs 切成 8 MiB 的窗口，同时有几个窗口在读；
    # Windows 上是一个重叠 IO 的无缓冲卷句柄加完成端口，直接读进对齐的窗口缓冲区（不经弹跳缓冲区）。
    # 读完的窗口立即交给 rayon 解析（一个窗口一个任务），解析完按记录号顺序并入输出列。
    # 解析里的 panic（程序错误）变成这次扫描的错误（"the $MFT scan failed: …"），进程照常运行：
    # rayon 对派出去的任务里的 panic 默认直接中止整个进程，现在每个窗口的任务自己接住它，
    # 扫描线程上的 panic 也在 stream_scan 里接住（引擎在展开时等在途的读完）。测试钩子
    # ScanOptions::before_parse（doc(hidden)）在解析每个窗口前调用。
    # 同时在读的窗口数（深度）在扫描中自己找：从 2 开始，每个阶段至少 150 ms 忙碌时间、16 个窗口，
    # 翻倍能让吞吐多一成以上就翻倍，否则退回上一档不再变，最多 128 MiB 在途（scan::Depth）。
    # duty ∈ (0, 1]：读按突发进行，每次突发至少占盘 50 ms，之后等在途读完、歇 忙碌时间×(1−duty)/duty，
    # 盘的忙碌占比就是 duty（IO 档位换算见 4.3）。progress(done, total) 每解析完一个窗口调用一次
    # （读线程上，短暂拿 GIL；出错忽略）。stats：传入 dict 时写进各阶段耗时和计数（基准用）。
    # 峰值内存约为结果表加正在读的窗口和结束时搬进 bytes 的名字（C: 886 万项时助手峰值 +1.62 GB，
    # 旧的流式读法 +2.82 GB，整块读入再解析要 10.6 GB）。


def read_mft(volume: str) -> bytes: ...  # 整个 $MFT 读进内存（旧接口，测试与对比用）
def parse_mft(data, record_size: int, cluster_size: int) -> MftTable:
    ...
    # 纯计算、跨平台；data 可以是任何 C 连续的 uint8 缓冲区。测试用合成 MFT 镜像驱动。
```

`MftTable` 是一个 dict，值都是 numpy 数组（小端）；每个「名字」一行（硬链接有几个名字就有几行）：

| 键 | dtype | 含义 |
|---|---|---|
| `record` | u64 | 文件记录号（低 48 位）|
| `parent` | u64 | 父目录记录号（低 48 位）|
| `name_off`, `name_len` | u32, u16 | 在 `names`（UTF-8 字节，孤立代理按 surrogatepass 编码）里的位置 |
| `kind` | u8 | 0 文件 · 1 目录 · 2 名称代理类重解析点（符号链接 / 联接 / 挂载点），以及 tag 不在 MFT 里（非驻留 `$REPARSE_POINT`）的情况；其他重解析点（dedup、WOF、云文件）仍是文件或目录，只带 `Flag.REPARSE` |
| `flags` | u32 | 同 `spaceknife.store.nodes.Flag`；有命名 `$DATA` 时带 `Flag.ADS` |
| `size` | i64 | 未命名 `$DATA` 的逻辑大小（目录为 0）|
| `alloc` | i64 | 实际占用：非驻留取 allocated size，压缩/稀疏取 compressed size；驻留为 0；目录为 0 |
| `ads_size`, `ads_alloc` | i64 | 命名 `$DATA`（备用数据流）的合计，规则同上 |
| `mtime`, `ctime`, `atime` | i64 | Unix 秒（取 `$STANDARD_INFORMATION`；FILETIME 为 0 时为 0）|
| `nlink` | u16 | 硬链接名数（Win32/POSIX 命名空间的名字个数）|
| `names` | bytes | 名字 blob |

解析规则：只保留 in-use 记录；应用 fixup 并校验，坏记录跳过并计数（`bad_records`）；扩展记录按头部的 base reference（含序列号）合并，所以非驻留的 `$ATTRIBUTE_LIST` 也能处理；同一父目录下同一记录的 DOS 8.3 名与 Win32 名并存时只保留 Win32 名；`$FILE_NAME` 的父引用取自该名字本身（序列号不校验）。流式解析时，带 `$ATTRIBUTE_LIST` 的基础记录推迟到所有窗口读完再输出（只留解析结果，不留原始记录；结束时并行并入它们的扩展记录）；没有列表却在后面出现扩展记录的基础记录，结束时按记录号补读重解析——所以行基本按记录号排列，有扩展记录的文件排在最后。窗口可以乱序解析完，但总按记录号顺序并入，所以行和行序与逐窗口读解析完全相同，与窗口大小、深度、完成顺序都无关（有测试；D:、F:、K: 的真实 $MFT 镜像上新旧两版逐字节相同）。

流式扫描的输出列按「已读记录的行密度 × 记录总数」预留（多留 2%，不够时同样按投影再扩），每个窗口的表和窗口缓冲区循环使用；名字按窗口分块搬入，最后一次拷进 `bytes`，不再在最后整表拼接（旧做法那一刻是两张表）。

后端把 `MftTable` 变成 `NodeStore`（`spaceknife/scan/mft.py` 的 `table_to_store`）。有原生扩展时由 `mft_build`（`src/mft/build.rs`）完成，所有步骤在 Rust 里并行：首行与重复名字、按记录找目录行、每行的父目录行、断环、广度优先顺序、子项区间、深度、各列（直接写进节点表可原样接管的 numpy 数组）、`\0name` 名字图像（一次排好，`from_columns(separated=True)`，不再经过 `_separate`）、四个子树合计（按层从深到浅，每层的目录并行求和，等同 `reaggregate`）。这些并行步骤跑在建树自己的 rayon 线程池上（线程数是本进程可用的 CPU 数，`cpu_count`，见 2.2「列目录线程」；解析用的全局池也按它建），建完池和线程一起退出，不在后端常驻（见 [memory.md](memory.md) §6）。Python 只给去重后的扩展名编号（`ext_numbers`，与 numpy 建树同一顺序、同一 `str.lower`），没有逐行的 Python 工作。没有原生扩展时用 `_table_to_store_numpy`。两种建树的结果逐列相同——节点顺序、名字图像、扩展名表、store 版本号都相同（`tests/test_mft_build.py` 用样例、环、随机表对照；C:、K:、F:、D: 的真实表上也逐列相同）。建树放在后端而不在助手里：建树要用后端的状态（扫描语言下伪目录的名字、根路径、「扫描 NTFS 备用数据流」「硬链接」设置、映射模式的内存策略），放进助手就要扩大提权进程里跑的代码和白名单协议；而助手发来的表本来就是解析器的列，后端用 `consume=True` 边建边释放表的列，建树在表之外只多 57 B/行（C: 886 万行 +482 MB，numpy 建树 +1014 MB）。

- 按父子关系做 BFS，保证每个目录的子项连续，一次 `NodeStore.from_columns`（新建的列直接接管，不再复制）+ 子树合计。
- 记录 5 是根；元文件（记录 0–15 与 `$Extend` 树）放进伪目录 `$NTFS 元数据`；父项缺失、不是目录或成环的名字放进 `$孤立文件`（成环时每个环断开一个成员）。伪目录只在非空时创建，带 `Flag.VIRTUAL`；它们下面的项没有真实路径（`tree.node` / `tree.summary` 的 `on_disk = false`，界面禁用「在资源管理器中显示」和「加入删除篮」）。
- 占用规则同硬扫描：目录与链接不占空间，同一文件的第二个及以后的名字标 `HARDLINK_DUP`；`ino` 是记录号。
- 备用数据流只在 `include_ads`（设置「扫描 NTFS 备用数据流」）时计入；元文件夹里的流总是计入占用（`$UsnJrnl:$J`、`$Secure:$SDS` 等）。例外是 `$BadClus:$Bad`：它和卷一样长、只映射坏簇，却不带稀疏标志，allocated size 等于整卷大小，所以从不计入（坏簇本身也不计）。
- `consume=True`：调用方把表交出来，列在拷进节点顺序后立即释放（C: 级的表约 1 GB，建树峰值相应降低）。
- 环：不经根可达的父链环里，每个环断开最小的那一行放进 `$孤立文件`；环长超过 4096 时（numpy 建树的标签传播到此为止）整个环的成员都断开。两种建树相同。
- 与硬扫描的有意差异：云占位文件保留 MFT 里真实的 alloc（硬扫描记 0）；MFT 能看到硬扫描被拒绝访问的目录（如 System Volume Information）；元文件放在伪目录里。

测试钩子（不属于公开接口）：`_mft_image_info(path)`、`_read_mft_image(path)`、`_scan_mft_image(path, duty=1.0, window=0, progress=None, stats=None)`、`_parse_mft_stream(data, record_size, cluster_size, window_records, concurrent=False, seed=0)`，读带引导扇区的卷镜像或按小窗口驱动流式解析器（`concurrent`：窗口按 `seed` 打乱的顺序并行解析，再按记录号并入，与顺序喂入逐字节相同）。镜像文件在 Windows 上也走重叠 IO 引擎（不提权也能测到它），其他系统用读线程。基准用的环境变量 `SPACEKNIFE_MFT_WINDOW` / `_DEPTH`（固定深度）/ `_SPARE` / `_PIECE` 改窗口大小和深度，不属于接口。

### 2.2 并行硬扫描（`src/hardscan/`）

```python
def scan_slice(
    tasks: list[tuple[tuple[int, int, int], str, int]], opts: SliceOpts | SliceOptions
) -> dict: ...
```

与 `spaceknife.scan.worker.scan_slice` **同一输入同一输出**（返回 dict 的键与 `SliceResult` 字段一一对应：`SliceResult(**scan_slice(...))`）。

- `opts`：dict 或带同名属性的对象（`worker.SliceOptions` 可直接传）。未知键报 `ValueError`。
  - `roots`：每个根一项 `{dev, cluster_size, resident_max, cross_mounts, skip, protect, path, verify_names, listed_alloc}`，语义同 `worker.RootOptions`。`listed_alloc`（Windows；`"NTFS"`、`"ReFS"`、`"Btrfs"` 或 `""`）让文件的占用取列目录项的 AllocationSize，即文件系统自己的计数（不是整簇的值是放在文件记录或目录里的数据，记 0；只有一分钟内写过的稀疏、压缩文件和 NTFS 上 WOF 压缩的文件才单独问文件，`win.rs` 的 `listed_alloc`、`on_disk`、`ask_alloc`），`""` 按大小估算（`cluster_size`、`resident_max`，稀疏和压缩文件按路径问 `GetCompressedFileSizeW`）；扫描器对本地 NTFS、ReFS、WinBtrfs 卷打开它，`SPACEKNIFE_WIN_LISTED_ALLOC=0` 回到估算。`path` 是扫描根本身（按用户给的写法），`verify_names`（Windows）让每个任务都按文件系统存的名字核对（见下「只差大小写的同名项」；Btrfs 上原生遍历器自己会打开）。模块属性 `ROOT_KEYS` 列出这个构建认识的根键，扫描器只在认识时才传 `path` / `verify_names`（旧构建拒绝未知键）。
  - `budget_s`（默认 0.03；≤ 0 表示列一次就停）、`max_entries`（默认 100000）。
  - `threads`（默认 0）：0 表示放到进程共用的列目录线程上，同时列几个文件夹由 Rust 决定（宽度，见下「列目录线程」，扫描器的调用都这样）；N 表示这次调用自己的 N 个线程（1：调用线程自己的那一个）。
  - `gate`（默认 None）：一个 `ListingGate`，几次调用共用的「在列的文件夹数」上限和暂停（后端的磁盘负载上限，每块盘一个），见下「列目录线程」。
  - `keep_access_time`（默认 True，Windows 和 Linux 上起作用，macOS 忽略）：列目录时不更新文件夹的最后访问时间，见下。
  - `relative_open`（默认 True，只在 Windows 上起作用）：调用内新发现的子目录按名字相对父目录的句柄打开，见下。
  - `skip_final`、`dot_check`（默认 False）、`keep_where_updated`（默认 True）（只在 Windows 上起作用；0.3.4）：省掉收尾的目录查询、相对打开按 "." 项核对、访问时间更新已关的卷上不保留访问时间，见下。模块属性 `OPT_KEYS` 列出这个构建认识的选项键；扫描器（`native_slicer`）在构建认识时把 `worker.SliceOptions` 的这三项传下去，它们在每次创建选项时从环境变量读：`SPACEKNIFE_WIN_SKIP_FINAL=1`、`SPACEKNIFE_WIN_DOT_CHECK=1` 打开前两项，`SPACEKNIFE_WIN_KEEP_WHERE_UPDATED=0` 关掉第三项（回退开关；`0`、`false`、`no`、`off` 算关，其他非空值算开）。
  - 测试钩子（不属于公开接口，只给测试用）：`_fold_strict`（Windows：只差大小写的子目录按转成大写的名字打开；在区分大小写的 NTFS 文件夹里这样打开到的是大写的那一个，和 WinBtrfs 的查找一样）、`_extd_self_ids`（Windows：FileIdExtd 列表里每一项的 ID 都换成被列目录自己的，和 WinBtrfs 一样）、`_pinned_cap`（预算之后列出的项的上限，见下「预算」）、`_verify_final`（Windows：`skip_final` 要省掉的每一次查询都照样发，记下它的结果，返回了项也不关掉，见下「省掉收尾的目录查询」）。
  - `_open_stats(reset=False)`（测试与测量用，不属于公开接口）：进程内累计的打开方式计数，POSIX 上为空。键：`relative`、`by_path`（真正用来列目录的句柄；`relative` 只算普通子目录按 ID 核对过的相对打开，带标记的和固定文件夹下的按名字打开记在 `strict`、`pinned` 里）、`relative_failed`、`id_mismatch`、`id_unknown`、`keep_refused`、`ids_distrusted`、`path_after_failed`、`relisted_by_path`；只差大小写的同名项：`strict`、`strict_by_id`、`strict_failed`、`mismatch_renamed`、`pinned`、`pinned_cap_hit`、`task_name_mismatch`、`task_name_unknown`、`btrfs_size_mismatch`、`by_id_incomplete`（含义见下）；`final_skipped`、`final_verified`、`final_wrong`（省掉的收尾查询；自检或核验模式下照发后确实没有更多项的、还返回了项的）、`dot_matched`、`dot_unmatched`（相对打开由 "." 项核对通过的、"." 项定不了而改查句柄的）、`keep_off`（根所在的文件系统按系统设置不更新访问时间、这次调用不保留访问时间的根数）。
- Windows：`NtQueryDirectoryFile` + `FileIdExtdDirectoryInformation`（一次系统调用拿到名字、大小、占用、时间、属性、文件 ID、重解析标记），NT 路径形式打开。
- Windows 省掉收尾的目录查询（`skip_final`，默认关，0.3.4；`SPACEKNIFE_WIN_SKIP_FINAL=1` 打开）：一个文件夹的最后一次 `NtQueryDirectoryFile` 只返回 `STATUS_NO_MORE_FILES`，却同样要穿过卷上整个过滤驱动栈（第一轮剖析：wof 对每次目录查询都自己再同步查一遍，bindflt 查映射表；热缓存 C:、16 个线程时每个文件夹 12–14 µs）。
  - 规则：文档写明每次调用返回「缓冲区放得下的完整项」与「剩下的项」两者中较少的那么多。所以上一次查询之后，缓冲区剩下的空间（下一项从 8 字节对齐处开始）还放得下一个名字最长的项（这一信息类的名字偏移加 255 个 UTF-16 单元：NTFS、ReFS 的名字上限；Linux 名字 255 字节，在 WinBtrfs 上最多 255 个单元）时，就不会再有项，下一次查询省掉（`final_skipped`）；放不下时照样发。
  - 只在本地 NTFS、ReFS、Btrfs 上这样做（`case::fs`，根第一次按路径打开时认出；WinBtrfs 的 dirctrl.c 同样填到放不下为止）。FAT、exFAT 和网络盘照旧一直查到 `STATUS_NO_MORE_FILES`：重定向器和别的文件系统按规矩也可以提前返回填不满的缓冲区。
  - 重解析点的列表不省：列表开头的 "." 项带重解析属性（云文件的占位文件夹、ProjFS 的虚拟化目录等，它们的列表可能由过滤驱动按别的规则填）时，这个列表照旧查到底。
  - 自检：每次调用里每个根头 64 个本可以省的列表、之后每 64 个里的一个，照样发出那次查询（记 `final_verified` / `final_wrong`）；只要有一次还返回了项，这个根在这次调用里就不再省（列表本身是完整的，查询已经发了）。残余风险：自检之间没被抽到、又恰好不守规则的列表会少列它最后的那些项；所以默认关，只作为可以手动打开的选项（每个文件夹省的是 CPU，热缓存 5–13%，墙钟在噪声之内或只在主线程归并是瓶颈时可见，见本节末尾）。
  - 核验模式（测试钩子 `_verify_final`）：省掉的那次照样发，没有更多项记 `final_verified`，还返回了项记 `final_wrong` 并照常列出。整盘核验的结果见本节末尾的表；第一轮的原型核验过 1,133,141 个文件夹（C:\Users，卷上有 CldFlt、bindflt、KLIF、Sandboxie；Program Files，含 WindowsApps；ProgramData；W:；K:；I:\usr；64 KiB 和 4 KiB 缓冲），0 违规。
  - 有意差异：只有收尾那次查询才会报的错误（两次查询之间文件夹被删、被拒绝访问）不再出现，列出的是此前读到的内容。
  - 用时（2026-10-23，热缓存，同一构建只关这一项，按轮交替各 5 次，后台 7–11 个核在忙）：W:\work（ReFS，136 万项）关掉时内核 CPU 是开着的 1.16 倍 [1.11–1.39]、墙钟 1.12 倍 [0.97–1.22]；C:\Windows（NTFS，39 万项）CPU 1.05 倍 [0.97–1.17]，墙钟在噪声内。
- Windows 保留最后访问时间（`keep_access_time`，0.3.4）：先带 `FILE_WRITE_ATTRIBUTES` 打开，再用 `NtSetInformationFile(FileBasicInformation)` 把 `LastAccessTime` 设成 -1（即 `SetFileTime` 的「这个句柄以后不更新访问时间」），列这个目录就不再改写它的最后访问时间。
  - NTFS 开着访问时间更新时（`fsutil behavior query disablelastaccess` 为 0，或 2「系统管理，已启用」），以前每列一个文件夹都要写一次元数据（NTFS 只在上次更新超过一小时时才写）：一次 C: 扫描（870 万项）写 1.5–3.3 GB，打开后 30–110 MB。不产生 USN 记录、不改 ChangeTime、不触发监控关心的变化通知（实测）。ReFS 默认不更新访问时间（`RefsDisableLastAccessUpdate` = 1），这时它不起作用，也无害。
  - 拿不到写属性权限（标准用户进系统文件夹、只读 ACL、只读卷）→ 这个文件夹照旧打开，它的访问时间照旧会更新；一个根上被拒超过 64 次且多于成功次数，就不再先试。
  - 管理员也拿不到的地方（2026-10-22 实测）：`C:\Windows\WinSxS` 及其下所有文件夹、`System32\DriverStore\FileRepository` 等只给 TrustedInstaller 写权限，管理员打开 `FILE_WRITE_ATTRIBUTES` 也是拒绝访问（`SeRestorePrivilege` 有但默认禁用）。整个 C:（106 万个文件夹）里约 6.3 万次被拒；扫描后 MFT 里有 16.8 万个文件夹（16%）的访问时间是扫描期间的（同期文件只有 2,290 个），一次硬扫描仍写 250–690 MB（热缓存 250–270 MB，冷缓存 320–690 MB；WinSxS 单独扫一次约 180 MB，相隔两分钟再扫，访问时间又改一次、又写这么多），上面「30–110 MB」今天没有复现。在本进程里启用 `SeRestorePrivilege` 后，带 `FILE_OPEN_FOR_BACKUP_INTENT` 打开这些文件夹就能拿到写属性权限（实测），但这个特权让带备份语义的打开绕过写权限检查，所以不用（2026-10-22 决定）：这些文件夹照常打开，结果正确、速度优先，WinSxS 这类文件夹只占少数。
  - 用时：访问时间刚更新过时与旧方式相同（热缓存 C: 18.5–18.7 s，旧方式 18.9–23.4 s）。要写回大量访问时间时旧方式明显更慢（同一台机器上，旧方式写 3.3 GB 的那次用了 49 s，写 1.5 GB 时 19–23 s），但「两种方式都处在要写回的状态」的对比没有单独测过，所以不写提速。
  - 一致性（2026-10-22）：真实扫描器把整个 C:（106 万个文件夹）扫三遍（旧方式 / 保留访问时间 / 旧方式），逐个文件夹比对子项的名字、类型、大小、占用、修改时间：没有一个文件夹只在中间那一遍不同（两遍旧方式之间因文件变动本身就有 91 个不同）；W:\work（ReFS，11 万个文件夹）、I:\usr（WinBtrfs，3.4 万个文件夹）也都是 0。
  - 测试（`tests/test_hardscan.py`）：开、关、单线程三种结果与 worker 完全一致；扫描前把每个文件夹的访问时间设成 2001 年，默认选项扫完一个都不变（手动对照：关掉时 41 个文件夹全被改写）；用监控的过滤条件（`monitor.win.NOTIFY_FILTER`）监视扫描的目录，扫描本身不产生任何变化通知（这个过滤条件不含访问时间，旧方式在小目录上也没有触发；测试防的是打开方式引入属性或大小类的变化）。
  - 本来就不更新访问时间的卷上不做（`keep_where_updated`，默认开，0.3.4；`SPACEKNIFE_WIN_KEEP_WHERE_UPDATED=0` 关掉）：保留访问时间要多付带 `FILE_WRITE_ATTRIBUTES` 的打开、一次 `NtSetInformationFile`（第一轮剖析占热缓存 C: 扫描 CPU 的 11.8%，其中 NTFS 本身只占 1.3%，其余在过滤驱动）和被拒后的第二次打开；卷根本不更新访问时间时这些都是白做。根第一次按路径打开、认出文件系统时，按系统设置判断（`HKLM\SYSTEM\CurrentControlSet\Control\FileSystem`，每个进程读一次）：NTFS 的 `NtfsDisableLastAccessUpdate` 去掉最高位（Windows 管理这个值时置的标志）后是 1（用户管理，关）或 3（系统管理，关），ReFS 的 `RefsDisableLastAccessUpdate` 是 1，这个根就不再保留访问时间（`keep_off` 按调用计数）。只信这几个明说「关」的值：没有这个值、值是 0 / 2（开）或别的，以及其他文件系统，照旧保留。每次调用的第一次按路径打开在认出文件系统之前，照旧带写属性。
    - 访问时间的行为不变，只有一个窗口：设置要到下次启动才生效（fsutil 的说明）。改成「关」还没重启时，卷仍在更新访问时间，这段时间里这些卷上的扫描会更新文件夹的访问时间（和 0.3.4 之前一样），重启后恢复；改成「开」还没重启时照旧保留，无害。
    - 本机：`NtfsDisableLastAccessUpdate` = 0x80000002（系统管理，开），C:、D:、F:、K: 照旧保留；`RefsDisableLastAccessUpdate` = 1，W:、G: 不再保留。W:\work 热缓存（同一构建只关这一项，按轮交替各 5 次）：关掉时墙钟 1.20 倍 [0.80–1.24]、CPU 1.04 倍 [0.94–1.26]（后台 7–9 个核在忙，噪声大）。
    - 测试：注册表值的判定每一格（Rust `win::tests`），这台机器临时文件夹（NTFS）上根的保留与否与设置一致。
- Windows 相对父目录打开（`relative_open`，0.3.4；NTFS、ReFS 和 Btrfs 卷上，网络盘照旧按完整路径，见下「WinBtrfs」）：调用内新发现的子目录，用 `NtOpenFile`（`RootDirectory` = 父目录还开着的句柄，`ObjectName` = 子目录这一级的名字）打开，访问权限、共享方式和选项与按完整路径打开完全相同；文件系统只在父目录里查一个名字，不再从卷根逐级解析整条路径。任务（Python 交来的目录）仍按完整路径打开。
  - 为什么不会漏扫：它仍是按名字打开，过滤驱动拿到的是父目录的文件对象加一级名字，过滤管理器由此拼出的完整名字与按路径打开相同；按规则取名字（`FltGetFileNameInformation`）的过滤驱动看不出区别。按文件 ID 打开（下面「试过但去掉的」）恰恰没有名字，过滤驱动才把一些目录只显示了一部分。直接读 `FileObject->FileName`、不看 `RelatedFileObject` 的过滤驱动会看到不同的名字，这是违反过滤驱动规则的写法，所以列出来是否相同最终以整盘比对为准（见下）。
  - 打开后核对：句柄的文件 ID（128 位用 `FileIdInformation`，对应 `FileIdExtdDirectoryInformation`；64 位用 `FileInternalInformation`，对应 `FileIdBothDirectoryInformation`）必须等于父目录列表给这个名字的 ID，这排除了打开成别的目录（例如列出之后被替换）。核对只能排除「打开了另一个目录」，排除不了「同一个目录显示得不一样」。
  - 先用 "." 项核对（`dot_check`，默认关，0.3.4；`SPACEKNIFE_WIN_DOT_CHECK=1` 打开；两边的测量都没看出收益，它却要先通过没核对过的句柄列目录、父目录句柄也要多留一会儿，所以不默认开）：普通子目录相对打开以后先列，它自己列表里的 "." 项就是这个目录本身；"." 的文件 ID 在同一个信息类、同样的宽度下等于父目录列表给的 ID，就算核对通过（`dot_matched`），不再查询句柄（每次相对打开省一次 `NtQueryInformationFile`，第一轮测得 16 线程时 2.5–3.3 µs）。"." 缺失、宽度不同（这期间根换了信息类）或 ID 不同时，照原来的方式查句柄的 ID（`dot_unmatched`）：相同就保留已列的内容；不同或读不出就丢掉已列的内容，按上面的规则（按完整路径，或名字到达了另一项时按 ID）重新打开、重列，结果与不开这一项时相同。核对不过之前父目录的句柄留到这一项列完。依据：第一轮原型 NTFS 4 轮每轮 105 万次相对打开、ReFS 19.4 万次，"." 的 ID 与句柄的 ID 全部一致；这一轮热缓存 C:\Windows 每次 7.3 万次、W:\work 10.6 万次相对打开全部由 "." 核对通过，`dot_unmatched` 0。用时在这台机器的噪声之内（只关这一项：C:\Windows CPU 0.92 倍 [0.86–1.07]，W:\work 1.04 倍 [0.89–1.17]；第一轮绑 P 核时测得内核 CPU −3%）。测试（Rust `win::tests`）：列表给的 ID 时由 "." 通过、给错 ID 时按路径重列且列出的是这个文件夹、信息类不给 ID 时改查句柄（128 位和 64 位两种）；名字到达了另一项时先列出的内容丢掉、按列出的 ID 重开重列（开与关两种）。
  - 一律按完整路径打开的情况（同一列表里没有别的项与它只差大小写时；有的话见下「只差大小写的同名项」）：列表里没有 ID（`FileBothDirectoryInformation`，或 ID 为 0、全 1）；重解析点目录（云盘文件夹、ProjFS、DFS 链接等：列表里的 ID 是重解析点本身的，它的子目录仍可相对它的句柄打开）；NT 路径是要 Win32 先规范化的形式；NT 路径或内核展开后的名字离长度上限不到 8 个单位（内核会把 `\??\C:` 换成 `\Device\HarddiskVolumeN`、把 SUBST 盘符换成目标路径；每个根第一次按路径打开时用 `NtQueryObject` 量出长多少，不论相对打开开没开）。这样相对打开不会比按路径打开走得更深。名字里有路径不能有的字符的目录（`\`、`/`、`:`、`* ? " < > |`、控制字符；WinBtrfs 会这样显示 Linux 文件名，例如 abrt 的 `ccpp-2025-11-23-18:28:47`、systemd 的 `\x2d`）按路径会被当成两级或流名、通配符，以前因此打开了别的目录或报错；现在只按文件 ID 打开，见下。
  - 回退：相对打开失败、ID 不符、ID 读不出，都交给按完整路径打开（它也给出错误信息）；只有 ID 不符、而句柄上文件系统存的名字与列出的名字也不同时例外：这个名字打开的是另一项（$UpCase 与系统不同的卷上没被认出来的同名项；8.3 短名：ntfs-3g 能写下一个名字正好是另一个文件夹短名的项，按这个名字打开会经短名到达那个文件夹），改按列表给它的文件 ID 打开（`mismatch_renamed`）。存的名字用规范化的名字（`FileNormalizedNameInformation`，`GetFinalPathNameByHandleW` 默认用的那种：NTFS 把经过的短名换成长名；文件系统不支持时用 `FileNameInformation`）：经短名打开时 `FileNameInformation` 报的是短名，也就是列出的名字，分不出来（2026-10-22 实测，WinBtrfs 也支持规范化的名字，结果相同）。通过相对句柄列目录失败，按路径重新打开再列一遍；列表给了 ID 时，按路径打开的句柄也要是这个 ID，不是就报原来的错误。失败或不符累计超过 64 次且多于核对通过的次数，这个根（本次调用内）就不再相对打开；ID 读不出时立刻停。保留访问时间被拒只按最后真正用来列目录的句柄计数。
  - WinBtrfs（2026-10-22 实测）：它的 `FileIdExtdDirectoryInformation` 给每个子项填的都是被列目录自己的 ID（`I:\usr` 下的 bin、games、include 都是 `I:\usr` 的 ID），`FileIdBothDirectoryInformation` 和句柄上查到的 ID 才对。第一版核对因此在 I: 上 9.8 万次全部不符、全部退回按路径（结果仍对，但白打开一次）。一次列表里出现两个子目录同一 ID（目录没有硬链接，正常的文件系统不会这样），这次列表的子目录的 ID 就都不用，这个根改用 `FileIdBothDirectoryInformation`；FileIdBoth 的 ID 也不对就停用相对打开。Btrfs 根现在不必等到识别出来：第一次按路径打开时查到文件系统是 Btrfs，就从第一次列表起用 FileIdBoth。之前按 ID 打开在 WinBtrfs 上「打开的是锚目录本身、无限循环」，根因也是这个：拿到的子目录 ID 其实是父目录的。
  - 用在哪些卷上（0.3.4 改）：每个根第一次按路径打开时查一次卷的文件系统名（`FileFsAttributeInformation`）和设备特征（`FileFsDeviceInformation`）。NTFS、ReFS、Btrfs 用相对打开；FAT、exFAT 和网络盘照旧按完整路径。网络盘按设备特征 `FILE_REMOTE_DEVICE` 认：SMB 共享报告的是服务器那边的文件系统名（例如「NTFS」），以前因此在网络盘上也开着相对打开，与「网络盘按完整路径」不符；网络盘也不能按文件 ID 打开。WinBtrfs 上相对打开配合 FileIdBoth 结果正确，用时相同或略快（I: 上缓存命中时 0.94–1.05 s 对 1.03–1.10 s，在误差之内）；先前开着相对打开的那几遍（约 11.8 万次相对打开）里，句柄上的 ID 除识别出来之前的那些和约 100 个只差大小写的同名项外都与 FileIdBoth 的一致；那几遍里同名项 ID 不符、按路径重开，列出的是胜出的那一个（见下「只差大小写的同名项」；这个构建在整盘上的核对还没有做）。以前 WinBtrfs 上不用，是因为它的 FileIdExtd ID 要靠识别兜底、收益又在误差之内；现在它的 ID 核对是下面「只差大小写的同名项」的安全网，所以也用。注意：WinBtrfs 用自己的内存缓存树节点，每 30 s 整个释放一次（见下「冷缓存」），它的用时在约 0.1 s / 0 MB 与约 2 s / 205 MB（H:，4.5 万项）之间跳，取决于与上次扫描之间有没有经过一次释放，与打开方式无关（先前把这种跳动误当成相对打开的代价）。
  - 句柄：父目录的句柄由它还没开始的子目录共同持有（`Arc`），最后一个子目录开始时关闭；单线程按深度优先走，同时开着的句柄约等于深度。没有子目录要打开时列完就关。
  - 实测（2026-10-22，同一个构建里 `relative_open` 开 / 关交替，C: 870 万项 NTFS、W: 321 万项 ReFS、I: 149 万项 WinBtrfs；冷缓存怎么测见下「冷缓存」）：
    - 打开方式：C: 每次约 80 万次相对、25 万次按路径（按路径的是每次调用的任务，即预算用完后交回 Python 的目录），ID 不符 0；W: 27 万 / 5 万，不符 0；I: 改用 FileIdBoth 后约 11.8 万 / 1.7 万，每次调用开头那几个列表在识别出来之前仍有约 1,800 次不符（退回按路径）。
    - 热缓存：C: 12.6 / 12.6 s（关：13.1 / 13.2 s），CPU 143 s（关 151 s），约快 4%；W: 3.7 / 4.0 s（关：5.0 / 5.9 s），约快 25–30%；I: WinBtrfs 自己的缓存命中时 0.94–1.05 s（关 1.03–1.10 s），没命中时两者都约 5.8 s、读 1.2 GB，命不命中与开关无关。
    - 冷缓存：开关之间的差别小于每次之间的波动（C: 开 26.4 / 24.6 s，关 24.4 s，每次读约 5.6–5.8 GiB）。W: 上那几次（开 40.0 / 28.9 s，旧版 36.9 / 26.7 s）有的只是半冷（见下），不能拿来比较开关。
  - 冷缓存（2026-10-22，与重启后的扫描逐盘对照；判断冷不冷看读盘的字节数和次数，不看用时：同样的读盘量，用时能差 15–40%）：清缓存用 `SetSystemFileCacheSize(-1, -1)` 清空系统缓存工作集，再 `MemoryFlushModifiedList`、`MemoryPurgeStandbyList`（RAMMap 的「Empty Standby List」）；读盘量是物理盘的 `IOCTL_DISK_PERFORMANCE` 计数（在所有缓存之下，含同一块盘上其他程序的读写）。
    - NTFS：清缓存后与重启后读得一样多（C: 5,701 / 5,782 MiB、105.8 / 107.0 万次读，K: 1,087 / 1,090 MiB，F: 561 / 561 MiB，D: 31 / 31 MiB）。C: 冷缓存 20–26 s，约 106 万次读、平均 5.5 KiB，在 C: 上约等于每个文件夹一次（K: 0.64 次、F: 0.88 次，取决于 MFT 的布局）：读的是文件夹的 MFT 记录和目录索引，文件的大小和时间就在父目录的索引项里，文件的 MFT 记录和内容都不读（例外：压缩和稀疏文件要打开查实际占用，`GetCompressedFileSizeW`）。C: 冷缓存时列目录线程约 63% 是内核 CPU、31% 等读盘（同时在途约 5 个读，每个约 80 µs），不是盘的极限；热缓存 11–14 s，不读盘，几乎全是内核 CPU（单线程 103 s，16 个线程 12.4 s）。exFAT 只在一个 9 个文件夹的盘上看过（222 / 219 次读），说明不了什么。
    - ReFS：清缓存不一定清得掉。扫过 ReFS 卷之后一段时间内，有 1.2–2.2 GiB 的 ReFS 元数据留在系统缓存工作集里修剪不掉，这时的「冷」扫描少读的正好约是这么多（W: 剩 1,237 MiB 时读 6,833 MiB / 25.3 s、剩 2,083 MiB 时读 5,943 MiB，重启后 8,008 MiB / 38.7 s）。剩下的不一定是被扫的那个卷的（G: 剩 2,175 MiB 时仍读了 1,617 MiB 里的 1,517，剩的大多是刚扫过的 W: 的）。清完工作集接近 0 的那次与重启一样（W: 8,010 / 8,008 MiB）。所以 ReFS 的冷缓存只算清完后系统缓存工作集接近 0、读盘量与重启后相同的那几次。
    - WinBtrfs：清缓存不起作用。它把树节点缓存在自己的内存里，由刷新线程每 `FlushInterval`（默认 30 s）整个释放一次（`flushthread.c` 的 `do_flush` 调 `free_trees`），所以一次扫描冷不冷只看上次扫描之后过没过一个刷新周期，与清缓存无关（I: 重启后 1,213 MB / 5.5 s，紧接着清缓存再扫 0 MB / 1.7 s）。
  - 一致性（2026-10-22）：每一遍都与参照逐个文件夹比对子项的名字、类型、大小、修改时间。C: 的参照是扫描之后直接读的 MFT（盘上实际有的，不经过过滤驱动），W:、I: 是 Python 遍历器（FindFirstFileEx）。C:（105.7 万个文件夹）只在扫描里有的项事后都已删除，只在 MFT 里有的都是扫描之后新建的；另外 22 个文件夹列不出（拒绝访问，共 145 项）、235 项类型不同（云占位文件和应用执行别名，MFT 快扫把这类重解析点算作链接，有意差异）、两三百个正在写入的文件大小或时间不同（目录项里的大小要等文件关闭才更新）。W:（32.4 万个文件夹）、I:（13.4 万个文件夹）名字完全一致。开与关之间、冷与热之间也只有期间文件变动造成的差异（例如诊断服务在 12:35 把 4 个 `Triggers\*.dat` 移走、12:45 带原时间戳放回）。比对脚本按扫描器的调度方式运行真实扫描（`spaceknife scan` 用的 `_run_scan`），结果存成快照再逐个文件夹比较。
  - 测试（`tests/test_hardscan.py`，Windows）：开、关两种结果与 worker 逐列一致，且每个目录只打开一次、没有回退；比按路径打开的长度上限更深的目录，两种方式报同样的错误、列出同样的内容；超出 Win32 上限（32,767 个单位）的文件夹，worker 也报同样的错误（`os.scandir` 对它抛 `ValueError`，worker 换成错误 206 的说明）。Rust 单元测试（`cargo test --lib`，CI 的「Native unit tests」三个系统都跑）：故意给错 ID 时相对打开被拒（128 位和 64 位两种）；名字到达了另一个文件夹（存的名字不同、8.3 短名）时按列表的 ID 打开；一次列表里重复的 ID 使根改用下一种信息类；哪些名字可以相对打开。
- 只差大小写的同名项（0.3.4，`src/hardscan/case.rs`；2026-10-22 发现）：Windows 按名字打开一律不区分大小写。文件夹里的名字区分大小写地存着时（WinBtrfs 上 Linux 建的树；NTFS 上 ntfs-3g 写下的 POSIX 名字；区分大小写的 NTFS 文件夹），按名字打开可能到达另一项：WinBtrfs 不区分大小写的查找取第一个大写后相同的子项（`create.c` 的 `find_file_in_dir`）。I:（WinBtrfs，149 万项）上有 102 对文件夹只差大小写（例如 flatpak 运行时的 `share\terminfo` 里的 `a` 与 `A`、`lib\perl5\5.42` 里的 `pod` 与 `Pod`）：以前两个遍历器都按名字打开，`a` 列出的是 `A` 的 1 项（按 ID 打开它有 335 项），`pod` 列出的是 `Pod` 的 15 项（它自己有 223 项），胜出的那一个算了两遍，输掉的那一个的内容一项也没算，也不报错。
  - 规则：一个文件夹只在列它的句柄被证明就是列表里那一项时，才按这个名字列出：句柄的文件 ID 等于列表给的 ID；列表没有可用的 ID 时，句柄上文件系统存的名字（规范化的名字的最后一级，见上「回退」）与列出的名字完全相同。都证明不了时，在文件系统能做到的地方按文件 ID 打开它，否则就是列目录错误；可疑的文件夹不再退回不加核对的按路径打开。
  - 识别（每次列表）：每个名字按 Windows 自己的大写规则（`RtlUpcaseUnicodeChar` 逐个 UTF-16 单元建的 64K 表，ASCII 直接算；不用 Rust 或 Unicode 的大小写规则，ı、ß、ς、开尔文符号、代理对都不变）算哈希，排序后同一哈希的两项以上（文件也算：同名的文件可能抢走文件夹的名字），其中的子目录标 TWIN；与另一项 WTF-8 字节完全相同的（WinBtrfs 把无效 UTF-8 显示成 U+FFFD）再标 DUP；名字里有路径不能有的字符（`case::unnameable`：`\`、`/`、`:`、`* ? " < > |`、控制字符，也就是 WinBtrfs 的 `check_file_name_valid` 拒绝的那些）的子目录标 BADNAME。WinBtrfs 按 ID 打开时会按名字重新解析一次（`open_fileref_by_inode` → `find_file_in_dir` → `check_file_name_valid(…, posix = false)`），所以名字里有 `:`、`* ? " < > |` 或控制字符的文件夹按 ID 也打不开，报 BADNAME（以前报「系统找不到指定的路径」）；只有 `\` 的能按 ID 列出（2026-10-22，I: 上 9 个 `\x2d` 一类的 systemd 目录列出，18 个带 `:` 或 `"` 的报 BADNAME，`strict_failed` 18）。哈希碰撞只多一次核对。每个名字读出时都算哈希，排序和标记只在列表里至少有一个子目录、至少两项时做；评审时的原型实测约 9 ns 一个名字（热缓存 C: 扫描的约 0.05%），这个构建的开销还没有在整盘上测过。重复 ID 时只清掉 ID，不清标记。存放哈希的每线程缓冲区列完一个超过 65,536 项的文件夹后就释放（扫描线程一直留着，否则每个线程会一直占着最大那个文件夹的 32 字节一项）。
  - 打开带标记（「strict」）的子目录（`case::first_open` / `after_open`，每一格都有单元测试；`after_open` 的普通子目录那一列就是上面「回退」的规则，相对打开走的也是它）：BADNAME 只按 ID（按名字会被当成路径），没有可信 ID 就报错；其余先按名字打开（相对打开开着时相对父目录，否则按完整路径；列出的那一项不是重解析点时带 `FILE_OPEN_REPARSE_POINT`，名字到达的是什么就打开什么、不跟随：胜出的同名项是符号链接或交接点时打开的是链接本身，ID 和存的名字都不同，于是按 ID，而不会列出它指向的、可能正好同名的文件夹；WinBtrfs 上 Linux 的符号链接就是这样的重解析点，没有目录属性的那种按名字打开报 NOT_A_DIRECTORY，同样按 ID），然后：句柄 ID 等于列表 ID → 列出；列表没有可信 ID → 存的名字相同且不是 DUP 才列出，否则报 CASE 或 DUP；ID 不同但存的名字相同、又不是 DUP（列出之后被替换了）→ 列出这个句柄；ID 不同且存的名字不同，或是 DUP → 按 ID；ID 读不出 → 看存的名字；打开失败（同名的文件抢走名字时是 NOT_A_DIRECTORY、拒绝访问等）→ 有可信 ID 就按 ID，否则报这次打开的错误。没有标记的子目录照旧（见上「回退」）。strict 的结果不计入根的核对通过 / 不符 / 失败次数，预料之中的不符不会让这个根不再信任 ID 或停用相对打开。
  - 按文件 ID 打开：`NtOpenFile`，`RootDirectory` = 父目录的句柄，`FILE_OPEN_BY_FILE_ID`，访问权限、共享方式、选项和保留访问时间与其他打开相同。ID 按列表给的宽度（8 或 16 字节），从不扩宽或截短（补零的 64 位 ID 在 ReFS 和 WinBtrfs 上打不开）；只有 Btrfs 的 64 位 ID（FileIdBoth：子卷 << 40 | inode）用 16 字节的 `{inode, 子卷}`，8 字节的形式在父目录的子卷里找 inode，输掉的那一个是子卷根时就找错。只在句柄 ID 等于列表 ID、且不等于父目录自己的 ID 时接受（后一条挡住 WinBtrfs FileIdExtd 那种「打开的是父目录本身」）。打不开或核对不过：按名字打开失败过就报那次的错误，否则报 CASE（DUP 的报 DUP）。
  - 「固定」（pinned）的文件夹：按 ID 打开的文件夹和它下面的一切没有路径能到达，只相对本次调用里打开的句柄、按一级名字打开（不管相对打开开没开，所以内核展开长度总要量出来），核对同上；名字过长到按路径打开会被拒时报错误 206，所以固定的子树同样受路径长度上限约束，加上「不等于父目录的 ID」，不会循环。通过固定句柄列目录失败就是错误，不再按路径重列；压缩和稀疏文件不按路径查实际占用，用按簇取整的大小。
  - 预算（`walk.rs` 的 `past_budget`，表的每一格都有单元测试）：预算用完后，普通目录照旧交回 frontier；带标记的和固定的文件夹在预算大于 0、且预算之后列出的项（`Ctx::extra`，这些文件夹的项都算，预算之后也只列它们）少于上限时照样列。到了上限：只有 TWIN 标记的交回 frontier（作为任务时 Btrfs 上有任务核对），带 DUP 或 BADNAME 的和固定的报 CAP，从不进 frontier（`flatten` 再兜一次）。预算为 0（监控只重列一个文件夹）时：TWIN 交回 frontier，DUP、BADNAME 报各自的错误（作为任务的路径分不出它们），固定的不会出现（预算为 0 时带标记的不打开）。上限默认 max(2 × max_entries, 10,000)，扫描器的选项下是 20 万：一次调用多出的时间和内存、取消或暂停时的等待最多约两个切片；每一层都有同名项的一串文件夹也受它约束（以前带标记的不计入，这样一串会在一次调用里全部列完）。
  - 任务核对：Btrfs 根（或 `verify_names`）上，任务（frontier、监控重列、F5 重扫、继续扫描、扫描新文件夹）打开后读它存的名字，必须正好是扫描根自己存的名字（每次调用读一次）后面接上任务路径在扫描根以下的各级，逐个 UTF-16 单元相同（`case::task_is`）。根本身那部分不与输入的根比较，用的是它自己存的名字，所以换了大小写的根、SUBST 盘、挂载文件夹照常。不同就是错误 CASE（`task_name_mismatch`）：输掉的那一个、它下面的任何路径，以及经过胜出的符号链接到了别处的路径（即使那里最后几级名字恰好相同），作为任务时得到的都是明确的错误，而不是别的文件夹的内容。根的名字读不出时只比较最后几级，存的名字级数比任务在根以下的级数少也算不同（经链接到了更浅的地方）。任务的名字读不出时照常列（`task_name_unknown`）。WinBtrfs 的 `FileNameInformation` 有个怪处（2026-10-22 实测，`FileNormalizedNameInformation` 也一样）：缓冲区正好 64 KiB 或更小可以，大几个字节（65,540、65,544 字节）反而报 STATUS_BUFFER_OVERFLOW，评审时的探测脚本用 4 + 65,536 字节因此读不出名字；两个遍历器都用正好 64 KiB（容得下 NT 路径能有的任何名字）。每个任务多一次查询，只在 Btrfs 根上（约 1.3–1.7 µs），每次调用每个根再多打开一次根。监控重列（预算 0）：结果的第一个列表是任务本身就接受，只有任务自己报错才作废（子目录的 DUP / BADNAME 错误是它们自己的，见下「扫描之后」）；输掉的那一个重列时任务核对失败，于是保持扫描时的样子（旧，但不错）。
  - 完整性核对（Btrfs）：文件夹在父目录列表里的大小（i_size）= 2 × 它所有子项名字的 UTF-8 字节数之和（每个名字一个 DIR_ITEM、一个 DIR_INDEX；评审逐个核对了约 1 万个文件夹，按路径列出时只有输掉的那几个不符，按 ID 列出的全部相符）。每个调用内发现的子目录、名字里没有 U+FFFD 时都核对：按 ID 打开的不符就是错误 INCOMPLETE（按 ID 打开没有名字，过滤驱动曾经把这样打开的文件夹只列出一部分，见下「试过但去掉的」；I: 上装着 KLIF、bfs、SbieDrv、UCPD、FileInfo），按名字打开的只计数（`btrfs_size_mismatch`）。
  - 错误说明（两个遍历器完全相同，`worker.CASE_ERROR` 等；日志面板里由 `events.denied_reason` 换成中文）：CASE `cannot be opened by its name: another entry in this folder has the same name in a different case`；DUP `… has the same name as Windows shows it`；BADNAME `cannot be opened by its name: it contains a character Windows names cannot hold`；CAP `not read: too many entries in one pass in folders that can only be opened by file id`；INCOMPLETE `listed incompletely when opened by its file id`。
  - Python 遍历器（`worker.py`）不能按 ID 打开：根上 `verify_names`（扫描器在 Windows 的 Btrfs 卷上打开）时，列表里有子目录、至少两项时按 `str.upper()` 分组（Windows 大写规则的超集：973 个被 Rtl 改变的单元里 upper(c) == upper(Rtl(c)) 一个不差，`test_windows_upper_groups_every_pair_windows_folds` 钉住；多出来的组如 ß / SS 只多一次核对），组里的子目录标 strict，完全相同的名字里第一项之后的子目录标 dup，名字里有路径不能有的字符的（`worker.unnameable`，与 `case::unnameable` 相同）标 bad。轮到它们时（不是在父目录列表时：只重列父目录的结果不会因此带 CASE 的子目录错误，交回 frontier 的任务下一片还会再核对）：bad、dup 报 BADNAME、DUP；strict 和任务（根本身除外）用 `CreateFileW`（备份语义、不跟随重解析点）读存的名字（`FileNormalizedNameInfo`，不支持时 `FileNameInfo`），按上面的规则与根自己存的名字（每片读一次）比较，不同就报 CASE，读不出就照常列。预算用完时栈里剩下的 dup / bad 报错，不进 frontier。
  - 扫描之后：按路径找节点（`PathIndex.locate`、`NodeStore.find`，监控事件、对比视图的 `live_node`、文件操作后重读父目录）先按原样大小写找，找不到再按 normcase 找、且只有一项匹配时才算；以前 `PathIndex` 留下最后一个、`find` 取第一个，`A` 里的变化会落到 `a` 上。删除和移到回收站拒绝按路径会打开另一项的路径（`ambiguous_name`，见 docs/manage.md 2.3）。删除篮不收没有路径单独指向的项：它或扫描根以下某一级上级的名字里有分隔符，或与同一文件夹里另一项的名字完全相同（DUP：扫描按 ID 列出各自的内容，按路径打开的却只是其中一个）。删除篮的每一项记下所在的扫描根，扫描关闭之后执行也照样核对。监控重列（文件变化、F5、扫描新文件夹）同样只用路径：重列结果里子目录的错误（DUP、BADNAME）记到新出现的那几行上，标成无法读取，不再按路径扫描它们；名字没有路径单独指向的文件夹（`PathIndex.unnamed`）和它下面的一切不按路径重列，保持扫描时按 ID 列出的样子；`mutate.relist` 把同名的几项按列表顺序一一对应，不再让两行共用一个子范围。资源管理器里打开、复制路径等仍会到达胜出的那一个（以后用节点标志处理）。
  - FAT / exFAT 上不会有只差大小写的同名项。FastFAT 的 FileIdBoth 列表给 ID，但不能按 ID 打开（STATUS_NOT_IMPLEMENTED），网络重定向器和 9P 也不能：那里真有这种项时只能是错误。NTFS 也不是完全不受影响（v1 的说法不对）：不带区分大小写标志的文件夹里，ntfs-3g 写下的 POSIX 名字和 8.3 短名会撞上；相对打开的 ID 不符 + 规范化后存的名字不同（`mismatch_renamed`）覆盖它们（短名那种只有规范化的名字分得出，见上「回退」；`win::tests` 用一个真实的短名测）。剩下的风险：NTFS / ReFS 上按 ID 列出的文件夹没有完整性核对（只出现在 POSIX 名字和 8.3 短名上）；NTFS 上交回 frontier 的输掉的那一个没有任务核对（要 POSIX 同名项再加预算 0 或碰上上限）；相对打开关着、又没有 ID 的普通子目录按路径打开，不核对（FAT、exFAT、网络盘）；大写表是当前系统的，自带 $UpCase 的旧 NTFS 卷靠上面的「存的名字不同」兜住；Python 遍历器只核对同名组里的和任务，8.3 短名撞上的照旧按路径。
  - 计数（`_open_stats`）：`strict`（按 strict 规则打开的子目录）、`strict_by_id`、`strict_failed`、`mismatch_renamed`、`pinned`（通过固定句柄列出的文件夹）、`pinned_cap_hit`（到了上限没有读的文件夹：固定的，带 DUP 或 BADNAME 的）、`task_name_mismatch`、`task_name_unknown`、`btrfs_size_mismatch`、`by_id_incomplete`。`relative` 和 `id_unknown` 只算普通子目录的相对打开（strict 和固定的按名字打开不算在里面，它们读不出 ID 时也不停用相对打开）。I: 上（2026-10-22）`share\terminfo` 原生扫描：`a` 335 项、`A` 1 项，`strict` 16、`strict_by_id` 8、`pinned` 8、`relative` 26，没有错误；`perl5\5.42`：`pod` 223 项、`Pod` 15 项。
  - 测试：Rust（三个系统都跑）`case::tests`：大写哈希（ÿ/Ÿ、ⱥ/Ⱥ 相撞，UTF-8 长度不同所以按 WTF-8 比较会漏掉；ı/I、ß/SS、ς/σ、开尔文符号/K、U+10400/U+10428 不撞；孤立代理）、`mark_twins`（DUP、文件对文件夹、文件对文件不标、三项一组、没有子目录不看、超过 64 KiB 名字跨两个缓冲区）、ID 的宽度与 Btrfs 的 `{inode, 子卷}`、全 1 按宽度判断、任务路径与根自己存的名字（经链接到了别处、级数更少）、`first_open` / `after_open` 每一格（含普通子目录那一列）；`walk::tests` 预算表每一格（上限之内、到了上限、预算为 0）；Windows `win::tests`：大写表与 `RtlUpcaseUnicodeChar` 逐个单元相同，名字 `X` 配 `y` 的 ID 时相对打开到的是 `x`、存的名字是 `x`，于是按 ID 打开到 `y`，名字是另一个文件夹的 8.3 短名时同样按 ID（规范化的名字），父目录自己的 ID、文件的 ID、已删除文件夹的 ID 都是错误，相对打开关着时固定文件夹的子目录照样相对打开，存的名字是存的大小写，Btrfs 大小核对，每线程缓冲区的释放。Python（`tests/test_hardscan.py`，Windows，`fsutil file setCaseSensitiveInfo` 建区分大小写的文件夹，真建得出 `probe` 和 `PROBE` 两个才算，否则跳过：fsutil 失败时退出码也是 0）：`a`/`A` 内容不同、`a` 里有子树和嵌套的同名项、文件夹 `b` 旁边是文件 `B`，用 `_fold_strict` + `_extd_self_ids` 时原生结果与 worker 按路径列出的真实树逐列相同，worker 打开 `verify_names` 时也相同（它的分组和真实的 `final_name` 都走一遍）；按扫描器调度也相同，而且 `a`、`A`、`b` 和 `a`、`b` 下面的路径从不作为任务交来，也没有 CAP；第 0 级列表 ID 共用时（`_extd_self_ids`）标记保留，`a` 报 CASE、`b` 报打开自己的错误，`A` 照常；预算 0 时只有一个列表、同名项进 frontier、没有错误；`_pinned_cap=2` 时固定文件夹下报 CAP、不进 frontier；每层都有同名项的 8 层文件夹在 `_pinned_cap=0` 时只列一个、同名项进 frontier；胜出的同名项是指向另一个 `a` 的目录符号链接时，`a` 列出的是它自己的文件（相对打开开、关两种）；`verify_names` 时大小写不同的任务两个遍历器都报 CASE，原样大小写的照常，根换了大小写照常；`str.upper()` 超集；扫描器在 Windows 的 Btrfs 卷上给原生遍历器的每一片都带上根的路径和 `verify_names`（三个系统都跑，平台和文件系统是假的）。`tests/test_case_twins.py`（三个系统；真实的 `a`/`A` 删除在 Windows 能建区分大小写的文件夹时和 Linux 上跑，macOS 的 APFS 默认不区分大小写，跳过）：worker 在轮到时报错而不进 frontier、预算 0 重列时同名项进 frontier、dup / bad 是那几个子目录自己的错误；经链接到了别处的任务报 CASE；`_list_one`（只接受这一个文件夹的列表）；两个遍历器的错误文字逐字相同（解析 case.rs）、日志面板的中文说明；按路径查找、`PathIndex.unnamed`、监控把 `A` 里的文件变化记到 `A`、对比视图找到对的那一个；监控重列把新出现的 DUP / BADNAME 子目录标成无法读取、F5 不按路径重列同名的；`mutate.relist` 按顺序对应同名的两项；删除与回收站的拒绝（存的名字在别处、路上又有只差大小写的一对时也拒绝：胜出的可能是指向别处的符号链接）、扫描之后只改了大小写的照常执行、路上没有这样一对时别处的名字照常执行、真实的 `a`/`A` 各删各的、名字带分隔符或与另一项完全相同的不进删除篮、删除篮记下扫描根；Windows 上名字以点或空格结尾的拒绝删除、移到回收站和加入删除篮。`tests/test_ops_server.py`：扫描关闭之后执行删除篮，删除和回收站都按记下的扫描根核对，核对不过时什么都不动。`SPACEKNIFE_TEST_CASE_ROOT`（Windows，选跑）：与一个独立的 ctypes 参照遍历器比较，它用 FileIdBoth 列出、每个子目录都相对父目录按 ID 打开（Btrfs 上 16 字节），从不按名字，逐个文件夹比较名字、类型、大小，原生多出参照没有的文件夹（参照不进的重解析点除外）也算不同，同时核对每个文件夹的 i_size，以及按路径能到达的文件夹（一路上没有名字与别的项撞上）按 ID 与按路径列出是否相同（过滤驱动的金丝雀）；按路径到达别的项的文件夹（参照用句柄 ID 认出来）作为任务交给原生遍历器，Btrfs 上报 CASE，胜出的那一个照常列出（识别出 Btrfs → 任务核对这条链）；不写死任何项数。`test_native_scan_of_a_real_tree_matches_the_python_walker` 不比较 worker 报 CASE / DUP / BADNAME 的路径和它们下面的一切（它们由上一条比较）。
- 试过但去掉的：按文件 ID 打开子目录（`FILE_OPEN_BY_FILE_ID`，名字是父目录列表里的 8 / 16 字节文件 ID，相对于同一卷上的一个「锚」目录句柄；45f501d 加入，下一个提交去掉）。
  - 结果不对：锚是别的目录时，有的目录按 ID 打开后只列出一部分（`AppData\Local` 291 项只列出 15 项），不报任何错误。打开的确实是同一个目录（`FileIdInformation` 核对过），差别来自卷上的文件系统过滤驱动（这台机器上有 KLIF、Sandboxie、CldFlt、bindflt、bfs 等）。真实扫描器扫 C: 时因此整棵子树没被列出：870 万项只扫到 585 万，345,085 个文件夹与旧方式不同。之前只在 Program Files 和 WinSxS 上比对过（两者一致），没有发现。
  - WinBtrfs 上按 ID 往下走成了 `usr\bin\bin\bin…`：它的 `FileIdExtdDirectoryInformation` 给每个子项的 ID 都是被列目录自己的（见上「相对父目录打开」），按这个 ID 打开的就是父目录本身（当时以为是「收到 128 位 ID 时打开锚目录」）；按 ID 打开不受 NT 路径长度上限约束，不会停，几十秒吃掉约 49 GB 内存。
  - 也不更快：在结果正确（条目一样多）的对比里，按扫描器的方式调度，C:\Windows 从 2.0–2.3 s 变成 2.6–3.0 s，ReFS 的 W:\work 也更慢。之前测到的「快一倍」来自少扫了三分之一。
  - 教训：改打开或列目录的方式，要在真实的整盘上、按扫描器的调度方式，逐个文件夹比对内容（前后各扫一次旧方式，排除期间的文件变动）；只比项数、只比小目录都不够。`test_native_scan_of_a_real_tree_matches_the_python_walker`（设 `SPACEKNIFE_TEST_SCAN_ROOT` 后运行，几分钟）在真实目录上与 Python 遍历器这样比对；`test_scanner_driven_native_scans_build_the_same_tree` 在测试树上按扫描器的方式（多任务、几种预算）检查同样的性质。
- POSIX：`openat` + `getdents64`（Linux）/ `readdir`（macOS）+ `fstatat`，按 fd 相对路径遍历；不跨设备（除非 `cross_mounts`）；不跟随符号链接，调用内新发现的子目录用 `O_NOFOLLOW` 打开。
  - 路径达到 PATH_MAX 的文件夹（0.3.4）：`open()` 对 4,096 字节（macOS 1,024）及以上的路径报 ENAMETOOLONG，但文件夹在、能列。以前两个遍历器都把它记成列目录错误（「File name too long」），下面整个漏掉（独立参照遍历器逐项比对时发现，2026-10-22）。现在只在 ENAMETOOLONG 时退回 `open_long`（`worker.open_dir`）：按不超过 1,000 字节、在 `/` 处切开的若干段，每段 `openat` 相对上一段打开的文件夹；中间的段用 `O_PATH`（Linux；只要求搜索权限，与按完整路径解析相同，其他系统用 `O_RDONLY`），最后一段用原来的标志（`O_NOFOLLOW` 只管最后一个名字，与一次打开相同），中间的符号链接照样跟随。正常路径一次 `open()`，不多花。Windows 不同：NT 路径本身最长 32,767 个单位，见上。测试：`test_posix_folders_past_path_max_are_listed`（调用内发现的和作为任务交来的长路径文件夹，两个遍历器逐列一致）、Rust `paths_past_path_max_open_piece_by_piece`（双斜杠、中间的符号链接、错误码与一次打开相同）。
- Linux 保留最后访问时间（`keep_access_time`，0.3.4）：目录打开后用 `fcntl(F_SETFL, O_NOATIME)` 补上 `O_NOATIME`，通过它列目录就不更新访问时间。内核只允许目录的属主和有 `CAP_FOWNER` 的进程这样做，别人被拒（EPERM），那个目录照旧列、访问时间照旧更新；打开后再设，不用事先知道属主，也不会多打开一次。`relatime`（大多数发行版的默认）下，一天里第一次扫描会改写所有一天没访问过的目录的 inode，Btrfs 上还要写时复制元数据、拆开快照之间共享的元数据。WSL 的根分区上 4.7 万个目录里 3.3 万个属于当前用户（家目录、uv 缓存），扫描家目录时几乎全部受益；系统目录属于 root，标准用户没有办法。测试（`test_listing_keeps_folder_access_times`）把测试目录的访问时间设成 2001 年再扫：默认选项一个都不变，关掉时都被改写（文件系统根本不更新访问时间时跳过）。macOS 没有对应的开关。
- 硬链接只计一次（`LinkSet`，0.3.4）：扫描器把计过的 `(dev, ino)` 记在原生的 `LinkSet` 里，`mark(dev, ino, nlink, kind, flags)` 按切片顺序给后来者标 `HARDLINK_DUP`，与原来 `Scanner._fold` 里 Python 集合加逐项循环的结果逐项相同（随机数据与真实扫描两种等价性测试）；没有原生扩展时仍用 Python 集合。原来的循环每个多链接文件约 0.5 µs：WSL 根分区（60 万项，其中 16 万个文件有多个链接，uv 的缓存与各个虚拟环境共享）热缓存扫描中位数 0.37 s → 0.29 s。计在哪个名字上仍取决于先合并进来的是哪个，原生与 Python 遍历器顺序不同，所以真实目录比对（`test_native_scan_of_a_real_tree_matches_the_python_walker`）不比较这一项和由它决定的合计。
- Linux 上的瓶颈（2026-10-22，WSL2 的 ext4 根分区，60 万项，16 个 vCPU）：热缓存约 0.3–0.4 s，列目录线程只忙 8–17%，主线程归并占 73–95%；线程数 4 / 8 / 16 / 32 用时没有差别（0.32–0.41 s），所以更快的 `fstatat`（例如只要所需字段的 `statx`）在热缓存下改变不了总用时。冷缓存（`drop_caches` 加清宿主机缓存）3.4–5.9 s，读 327 MB，列目录线程忙 83–91%，是读盘。原生与 Python 遍历器冷热各两遍逐个文件夹一致。
- 名字编码同 worker.py（Windows surrogatepass，POSIX 原始字节等价的 surrogateescape）；非 ASCII 扩展名用 Python 自己的 `str.lower` 小写，避免 Unicode 版本差异。
- 有意差异（未截断的切片与 worker 完全一致）：(1) 被预算截断的切片是「已列目录的先序子树」而不是 worker 的 DFS 前缀，引用仍然有效；每个线程按目录检查预算，`max_entries` 最多超出每线程一个目录。(2) Windows 上空的 FAT 根是空列表而不是错误。（名字以点或空格结尾的目录以前也在这里：Win32 会去掉结尾的点和空格，worker 按普通路径打开 `odd.` 列出的是旁边的 `odd`；0.3.4 起 worker 对这样的路径也加 `\\?\` 前缀，两边相同。）(3) Windows 上 NT 路径 32,763–32,767 个单位的目录，worker 报「系统找不到指定的路径」（`os.scandir` 在路径后加 `\*.*` 再交给 `FindFirstFileW`，超出上限）；原生列得出，32,767 时报错误 206。再长的两边都报错误 206。 (4) Windows 上只差大小写、按名字打不到的文件夹（输掉的那一个）、名字完全相同的（DUP）和名字里有路径不能有的字符的（BADNAME）：原生按文件 ID 打开并列出，worker 不能按 ID 打开，报 CASE、DUP、BADNAME 错误（见上「只差大小写的同名项」）。(5) Windows 本地 NTFS、ReFS、Btrfs 上，只有收尾那次目录查询才会报的错误（两次查询之间文件夹被删、被拒绝访问），原生不再报，列出此前读到的内容（见上「省掉收尾的目录查询」）。
- 扫描器的接法（`scan/scanner.py`）：有扩展时 `native_slicer()` 以 `threads=0` 调用，每次调用的文件夹都放到进程共用的列目录线程上列；后端从 `native_threads()` 个线程发起调用（= 起步宽度，见下；现在的 Rust 后端里调用数跟着遍历器想要的 `calls` 走，见下「列目录线程」），每批 256 个目录任务、预算 0.05 s，在途的批次数由 IO 限速器的许可控制；有限速器时每次调用带上这块盘的闸门（`gate`）。构建里没有 `gate` 这个键时（旧构建）照旧每次调用 1 个线程（`threads=1`）。没有扩展时退回 spawn 进程池。
- 列目录线程（`hardscan/pace/`、`slots.rs`、`walk.rs`）：
  - 谁定、定什么：Rust。一个进程一组共用的列目录线程（rayon 池），所有 `threads=0` 的调用都在上面列，互相分担。要定的是同时在列的文件夹数，分两条道（lane），每条道一个名额上限（`Slots`，宽度），不是线程数：
    - IO 道：预计要等读盘的文件夹（冷）。在列的时间大多在等盘，在途的要多，盘的队列才够深；
    - CPU 道：预计不读盘的文件夹（热：目录和文件记录都在缓存里）。在列的时间几乎全是 CPU（多半是内核），在途的约等于 CPU 数；文件系统的锁争得厉害时更少才快（ReFS 热缓存，32 个 CPU：8 个线程 2.8 s，16 个 4.6 s，64 个 6.1 s）。

    冷热不是整次扫描的一个开关：扫到一半停下的树（先扫过的那一半在缓存里）、只有一部分子树被别的程序读过的树，同一时刻两种文件夹都有，所以每个文件夹各自判断、各进各的道（下面「每个文件夹的先验」）。一个文件夹拿到所在道的一个名额、闸门的一个名额（后端的磁盘负载上限，见下）、本次调用的份额才开始列，拿不到的留在本次调用这条道的待列栈里（`Parked`，每条道一个栈），列目录的线程从不为名额等待（闸门只放两个的 HDD 扫描不会占住别的盘的线程）。一个文件夹列完，本次调用同一条道栈顶的下一个文件夹直接接过它的名额，由同一个线程接着列（深度优先；不建任务、不唤醒线程、不还名额再拿）；多出来的空名额给栈里其余的文件夹，成为别的空闲线程的任务。一次调用同时在列的文件夹最多是两条道的宽度之和在共用线程上的调用之间平分（向上取整），因为同一次调用里并排列兄弟文件夹，文件系统更费劲：W:\work（ReFS）热缓存时内核时间多约 14%，墙钟差不多。一次调用的文件夹全在等名额时，由调用它的线程等（有名额还回来就醒，最长 10 ms 再看一次），预算到了或闸门暂停时把能等的交回去。
  - 每次列目录都是一个样本（`walk.rs` 的 `list_one`）：墙钟用时（打开加枚举）、这段时间本线程用的 CPU 时间（Windows x86-64 是 `QueryThreadCycleTime`，与 `rdtsc` 同一单位：本机忙循环 0.9965、睡眠 0.0002，每次约 115 ns；Linux、macOS 是 `CLOCK_THREAD_CPUTIME_ID`；拿不到时只看墙钟）、项数、完成时这条道在列几个。等待 = 墙钟 − CPU：热的文件夹几乎不等（抢占和锁除外），冷的至少等一次读盘（NVMe 约 80 µs，SATA 约 150 µs，机械盘几毫秒，网络 1 ms 起）。特征 x = ln(1 + 等待的微秒数)：两个峰相隔一个数量级以上。直接看墙钟分不清：C: 冷缓存时列目录线程 63% 是内核 CPU、只有 31% 在等盘，冷的墙钟只比热的长 2–3 倍，而文件夹大小让墙钟差上百倍。
  - 冷的比例（模型，`pace/model.rs`）：x 上的两簇在线混合模型——热、冷各一个正态分布，充分统计量按样本衰减（簇心约 512 个样本的记忆），每次列目录做一步在线 EM。簇心有先验，各算 16 个样本：热 ln(1 + 2 µs)，冷由设备提示给（下表）；冷簇心至少比热簇心高 1.0（2.7 倍），簇宽 0.35–2.5；后验低于 0.05 或高于 0.95 的样本在统计量里算 0 或 1（成千上万个同一类的样本不会把另一个簇心拖进它们的尾巴）。结果不是一个阈值上的是或否：每个样本得到「冷」的后验概率；全局冷比例 π 是最近样本后验的指数平均（快的约 64 个样本，慢的约 512 个，快减慢是趋势），随时间画出来就是冷比例曲线。
    - 读盘计数只用来否决：Windows 是扫描根所在卷的读次数（`IOCTL_DISK_PERFORMANCE`，含别的程序的读），POSIX 是本进程的块读（`ru_inblock`）。至少每 2 ms、16 个文件夹看一次每个文件夹读了几次 r（平滑过）。冷的文件夹平均至少读 0.25 次（C: 1.0、K: 0.64、F: 0.88，见上），所以冷比例不超过 r / 0.25。只看用时的冷比例比这高时，否决系数 κ（乘在「冷」的几率上，1e-8–1）按两者之比收紧；低于它时按比值的平方放开（至少翻倍）：后一半冷的树一开始读盘，几毫秒内就不再否决。r / 0.25 不到 0.02 时盘是「静」的：每个样本都算热。被否决的样本不进任何一个簇（它等的不是盘：当成热的教给热簇，热簇会变宽，等盘真的被读时把冷的读也吞掉——模拟器里扫了一半的树就这样只列到 27%）。别的程序的读只会让 r 变大、否决变弱，不会把热的判成冷。网络共享没有可信的计数，不否决。挡住的是不读盘也要等的情况：ReFS 的锁、CPU 不够时的抢占、APFS 在内核里等。
  - 每个文件夹的先验（`walk.rs` 的 `Node::prior`，交回前沿的任务带着它：`Task::prior`）：没列过的文件夹拿父文件夹的后验与全局冷比例的混合（0.8 / 0.2，限在 0.1–0.9：子树的状态是个倾向，定论由它自己的样本下）作先验——子树往往一起冷、一起热；列完由它自己的样本更新成后验，再传给它的子文件夹。扫描根和不带先验的任务用全局冷比例（还没有样本时 0.5）。先验 ≥ 0.3 进 IO 道，否则进 CPU 道：两种放错的代价不一样——冷的放进 CPU 道，要在它不多的名额里占着一个等盘；热的放进 IO 道，只是很快列完——所以分不清的（W:（ReFS）冷缓存时后验在 0.5 上下）去 IO 道。放错道的文件夹（预计热其实冷）只是在 CPU 道里等一次盘，它的后验马上是冷的，子文件夹都去 IO 道；反过来预计冷其实热的，在 IO 道里很快列完。这样扫了一半的树、冷热按子树分布不均的树，各个子树按各自的样本走，不被一个全局平均盖掉。
  - 每条道跟着自己的「速率—在列个数」曲线（`pace/lane.rs`，类似 BBR）：
    - 一轮：这条道至少 max(32, 3 × 宽度) 次完成、至少 3 ms；下面的噪声超过 10% 时按比例拉长，最多 24 ms（真盘上的树比模拟器的乱：W:（ReFS）冷缓存时 3 ms 一轮的速率能差十倍）；慢设备 250 ms 后至少 4 次也算一轮。一轮量出：这条道的速率（每秒的文件夹数加项数 / 64，和以前一样，大文件夹不算慢窗口）、同一段时间两条道合起来的速率（总速率）、平均在列个数（每次完成时这条道在列几个，取平均）、每单位的平均用时。宽度刚改过时，先跳过改的时候在列的那么多次完成再开始一轮：改之前开始的列目录不算。平均在列不到限额的 0.8 的轮是「需求不足」（这条道没有那么多文件夹可列，或者闸门、调用数不放），不用来判断宽度该不该变。
    - 判断用这条道自己的速率，要超过它在同一宽度上一轮和一轮之间的噪声的两倍（噪声是相邻两轮速率差的平滑值，约一个标准差：只超过一倍时，平台上每六次探测就有一次凭运气变宽，模拟器里 IO 道这样从 34 一路「宽」到 64）。总速率只能否决：一步之后总速率掉得比它自己的噪声多，这一步就不算数——两条道共用 CPU 和盘，一条道变宽可能赚的正是另一条道亏的。总速率不能单独作判据：另一条道的速率随它的需求起伏（热的子树一来一大批），冷热混合的树里一轮和一轮能差 40%。同一时刻只有一条道在做实验（起步或探测），两条都想做时 IO 道先。
    - 起步（每条道各自，实时的，不靠上一次扫描）：IO 道从提示的起步宽度开始，每个满轮翻倍，直到连续 2 个满轮的速率都不比最好的一轮高出这一步在直线上该有的四分之一（翻倍是 25%），最多翻到 0.3.5 的冷宽度 max(2 × 起步, 32)，再宽只靠探测（扫描最初几轮树正在展开，需求的增长会被当成宽度的收益：W: 冷缓存时起步一路翻到 64，此后在 44–64 之间随噪声游走，比 0.3.5 慢 13–29%）；然后取 clamp(1.25 × BDP, 速率最好时的宽度, 现在的宽度)，BDP 还没有（不到 4 轮）时取速率最好时的宽度，但已经到了「家」的宽度（IO 道：0.3.5 的冷宽度；CPU 道：它的起步宽度）就不再落到它以下。BDP 是最近 16 轮里最大的速率乘最小的每单位用时：按 Little 定律，就是拐点上的在列个数。起步从不往窄走：一轮和一轮的速率能差十倍时，一两轮说明不了拐点在哪（W: 冷缓存时 IO 道翻到 32 的第一轮碰上一个 80 ms 的慢轮，以前「第一步就慢 5% 就退回起步宽度」的规则把它退回 16，再也没回到 32，整次扫描慢 1.5 倍）；往窄是探测的事，要过两倍噪声那一关。IO 道最窄是起步宽度的一半（32 个 CPU 时 8，4 个 CPU 时 2）：等盘的列目录少几个不省什么。CPU 道从下面起步：起步宽度大于 8 时从 max(8, 起步 / 2) 开始，每个满轮 ×1.5（门槛 12.5%），最宽是 CPU 数（macOS 两倍）——文件系统锁争得厉害时拐点远在 CPU 数以下（ReFS 热缓存 32 个 CPU 时是 8），从下往上几轮就找到，从起步宽度往下探要很多轮；8 个 CPU 及以下从起步宽度开始。模拟器里 NVMe 冷缓存 16 → 32 → 64 → 落到 32，约 12 ms，不再是 0.12 s 一个窗口。
    - 巡航与探测：每 4 个满轮探测一次：宽一档（×1.25）一轮，回原宽度一轮，窄一档（×0.8）一轮，再回原宽度一轮。宽的一轮比它前后几轮的均值快 5%（IO 道）/ 4%（CPU 道）加两倍噪声就留在宽的；否则窄的一轮比均值快 5%（IO 道：等盘的列目录只占一个线程，不占 CPU，一样快不值得变窄）/ 不慢于 98%（CPU 道：一样快就少用，锁争得少）各加两倍噪声，就留在窄的；都不是就不动。前后取均值，抵消树越列越快或越慢。留下了的探测，马上朝同一方向再探一次；一轮什么都没改的探测之后，下一次等两倍的轮数（最多 64 轮），留下一次、或者巡航中一轮比上一轮差出三倍噪声（树或缓存变了）时回到 4 轮。CPU 道往窄的探测就是以前热缓存往窄的探测（ReFS 那样的锁），只是几毫秒一轮，不再是 0.12 s 起的窗口加 0.25–4 s 的退避。
    - 太吵时回到起点：轮已经拉到最长，噪声还在 25% 以上时，探测分不出好坏（只会凭运气走），就不再探测，每 4 轮朝「家」走一档（×1.25 / ×0.8），直到噪声降下来。家是这条道起步时落定的宽度（上面：不低于起步宽度，到过 0.3.5 的冷宽度就不低于它；CPU 预算的上限更低时按上限）。W: 冷缓存时两条道就是这样随噪声游走的，所以落定的宽度不能由一个倒霉的轮定（上面）。家不直接用 0.3.5 的冷宽度：4 个 CPU 时拐点约 10，往 32 走就慢了（模拟器里 83%），而那里的 CPU 占比把等 CPU 的时间也算作等待，挡不住。
    - BDP 护栏：宽度超过 2 × 增益 × BDP（增益：IO 道 1.5，CPU 道 1.0）时直接降到 1.25 × 增益 × BDP：已经远过拐点，多出来的只是排队。
    - 一条道连续 1 s 需求不足（例如前一半热、后一半冷的树，IO 道在前一半一直空着），它下一个满轮从当前宽度重新起步。
  - 两条道共用 CPU（`pace/ctl.rs`）：每条道量出它的列目录有多大比例在用 CPU（本线程 CPU 时间 / 墙钟，平滑过）。NTFS 冷缓存的列目录在 NVMe 上约三分之二是 CPU，机械盘上几乎为零。IO 道在列的个数乘这个比例就是它占着的 CPU 数：CPU 道最多拿剩下的（CPU 道最宽减去它），两条道加起来不把 CPU 挤过头；IO 道最多让 1.25 × CPU 道的拐点个 CPU 忙（拐点是 CPU 道一次探测确认过的宽度——宽一档没有更快，探测结束在它上面——在 IO 道几乎不占 CPU 时巡航，1 s 内有效；没有时按 CPU 数。起步落定的宽度、BDP 护栏降到的宽度、回家走到的宽度都不算拐点：W: 冷缓存时 CPU 道起步碰上一个慢轮退回 8、或者需求少被 BDP 护栏降到 4，被当成拐点，IO 道的上限成了 1.25 × 8 / 0.56 = 18，而那里 IO 道 32 比 18 快 1.5 倍）——ReFS 那样的锁对冷的列目录一样起作用，冷的那一半不该在 32 宽上和锁争。这个上限跟着平滑过的 CPU 占比一点一点地动，IO 道的宽度超出它八分之一以上时才降下来，否则只挡住以后的加宽（不然每次变一个就重开一轮）。被压在自己宽度以下的道（IO 道占着 CPU 时的 CPU 道）不走任何一步，BDP 护栏也不动它：那时它的探测什么也改变不了，宽度只会凭运气漂下去；W: 冷缓存时 CPU 道就这样漂到最窄的 4，IO 道一安静，这个 4 就被当成拐点，把 IO 道压到 8 宽，比 0.3.5 慢 1.4 倍。
  - 全局冷比例和它的趋势只用来平滑过渡：子文件夹的先验里混 0.2 的全局比例；不带先验的任务用它；扫描器的调用数按它分配（下面）。
  - 设备提示（`RootOptions::device`，`DeviceHint`）：扫描开始时就知道的事实，不用学：介质来自后端（`sk_core::media`：Windows `IOCTL_STORAGE_QUERY_PROPERTY` 的寻道惩罚与总线类型，Linux `/sys/class/block/<dev>` 的 rotational 与设备路径，macOS `diskutil` 的 SolidState 与 BusProtocol），换成 sk-native 自己的 `Medium` 按普通数据传下来（sk-native 不依赖别的 crate，`tests/layering.rs`）。它定 IO 道的起步宽度、上限，冷簇心的先验，以及读盘计数能不能否决：

    | 介质（`Medium`） | IO 道起步 | IO 道最宽 | 冷簇心先验 | 计数否决 |
    |---|---|---|---|---|
    | `Nvme` | 16 | 64（`SPACEKNIFE_WALK_MAX`） | 80 µs | 是 |
    | `Ssd`（SATA 和其他 SSD）、`Virtual` | 8 | 64 | 150 µs | 是 |
    | `Hdd`（含 USB 机械盘） | 冷宽度，固定（下面） | 同左 | 8 ms | 是 |
    | `Flash`（U 盘、SD 卡） | 4 | 16 | 500 µs | 是 |
    | `Network` | 8 | 64 | 1.5 ms | 否 |
    | `Unknown` | 8 | 64 | 200 µs | 是 |

    CPU 道的起步宽度：本进程可用的 CPU 数（`cpu_count`：Windows 按进程亲和掩码，线程跨处理器组时把所在各组的活动处理器加起来；Linux 是 `sched_getaffinity` 与 cgroup v2 的 `cpu.max` 配额（向上取整，沿 cgroup 路径到挂载点取最小）里小的那个，且不超过 Rust 标准库的计数，它也认 cgroup v1；macOS 是逻辑 CPU 数），取 2–16；macOS 是它的两倍、4–16（APFS 列目录常在内核里等，线程多一些 CPU 才忙得起来，见 [packaging.md](packaging.md)）。最窄 max(2, 起步 / 4)。一组道一次只跟一种介质：空闲 1 s 以上之后的第一个调用的第一个根定下提示，换了介质时两条道和冷簇心从这个提示重新开始，同一种介质时簇心留着（它们是设备和 CPU 的性质，不是这次缓存的状态）。两次扫描在不同介质的盘上同时进行时共用一组道（和以前共用一个宽度一样；每块盘的闸门照样各自封顶）。
  - 机械盘的钩子（`Medium::Hdd`）：IO 道固定在以前的冷宽度 max(2 × 起步, 32)，不跟曲线：盘自己的 NCQ 按位置重排，队列越深吞吐越高、每翻一倍只多十几个百分点，25% 的门槛会停得太早；固定下来就不比 0.3.5 差（0.3.5 要过一个窗口才到冷宽度）。机械盘专用的调度（浅并发、按物理位置排序：NTFS 上按文件夹的 MFT 记录号排，接近 LBA 的顺序）以后放在这个分支里做。本机没有机械盘，只在模拟器里看过。
  - 扫描器的调用跟着需求（`scan/scanner.rs`）：遍历器给出它想要的调用数 `calls` = ⌈CPU 道宽度 / k⌉ + ⌈IO 道宽度 × IO 道的占用比例 / 2⌉，2–64（k：Windows 2，和 0.3.5 热缓存时 8 个调用对 16 的宽度一样；其他系统 1；IO 道的占用比例是最近样本里 IO 道列目录时间的份额）。扫描器每次分派按 min(`calls`, 限速器的并发) 派批次，调用线程不够时现开（最多 64 个；开了的不退，没活时等在任务队列上）。Windows 上最多 8 个调用的固定上限去掉了：C: 冷缓存 0.3.5 的 8 个调用对 64 的宽度，调用本身成了瓶颈（「listing threads busy 91% of 8」；部分冷的 C: 上 `--workers 16` 18.1 / 31.9 / 17.7 s 对 8 个调用 23.1 / 23.5 / 25.2 s）。`--workers N` 和设置里的手动并发照旧固定 N 个调用。
  - 磁盘负载上限（`ListingGate`，见 4.2）：总量照旧由限速器的闸门封顶：两条道在列的加起来不超过它的并发，暂停时不再开始新的。闸门不放时两条道都是需求不足，不会因此加宽或变窄。暂停时能等的文件夹不再开始，原样作为任务交回（预算之内暂停的调用也立刻交回）；必须在本次调用里列的（按文件 ID 打开的文件夹下面的、名字与别的项相同的，见上「预算」）照样列，但不超过上限。限速器的最大并发（`listing_max`）是两条道最宽之和。
  - 线程池：线程数至少是两条道的宽度之和，向上取到 8 的倍数（宽度几毫秒变一次，池不跟着每次重建）；宽度之和 2 s 都低于池的线程数减 8 时（或扫描结束）换回小的池。还在旧池上的调用在旧池上做完，旧池的线程随后退出。
  - 测量和日志：扫描的耗时分解多出调用数的范围和时间平均、两条道的宽度范围和时间平均、各道列了多少文件夹、冷比例随时间的曲线（每秒一个点，太长时两两合并）、读盘否决起作用的样本比例；`listing_threads()` 给出两条道的宽度和 `calls`，`pace_report(reset)` 给出这些曲线。`SPACEKNIFE_WALK_TRACE=<文件>` 把每一轮的决定追加进文件（道、阶段、宽度、速率、平均在列、用时、π、两个簇心、κ）。
  - 环境变量（测量用，不属于接口）：`SPACEKNIFE_SCAN_THREADS=N`（1–64）不分道，宽度和扫描器的调用数都固定为 N；`SPACEKNIFE_WALK_ADAPT=0` 不分道、固定在起步宽度；`SPACEKNIFE_WALK_LANES=0` 不分道，所有文件夹都走一条按曲线走的道（量分道本身值多少）；`SPACEKNIFE_WALK_MAX`（默认 64，1–256；POSIX 另外不超过 RLIMIT_NOFILE 软上限减 128 后的一半，两条道合起来）是 IO 道的最宽；`SPACEKNIFE_WALK_TRACE=<文件>`。
  - 模拟器（`pace/sim.rs`，只在测试里编译）：用模拟的设备、CPU 和目录树驱动真正的控制器代码（模型和两条道，与遍历器用的是同一份），模拟时间、固定种子，几秒跑完。设备是 NVMe、SATA SSD、网络和机械盘（各自的读延迟随队列深度的曲线和吞吐上限），缓存状态按子树：全冷、全热、扫了一半（前一半热）、不均匀（随机 30% 的子树热）、边读边热（读一个冷文件夹时它的子文件夹的记录有一部分一起进了缓存），CPU 有每项的开销和锁争用（不争、像 NTFS 那样轻、像 ReFS 那样重）。「粗糙的卷」照 W:（ReFS，NVMe）冷缓存的追查做：元数据读得慢（每次 400 µs，抖动大）、15% 的子树里列一个冷文件夹要读 10 倍的次数（元数据散在卷上）、卷时不时把所有读压住几十 ms（像一把锁，平均每 150 ms 一次、每次约 30 ms，第一次来得早：追查过的每次 W: 冷扫描在最初 100 ms 里都有一轮慢了十倍），再加 ReFS 那样的锁争用；一轮和一轮的速率差十倍，最好的固定宽度是 32（W: 上快的几次 IO 道也在 32–40）。0.3.5 的规则（v1，下面）也在模拟器里跑，作为基线。每个场景检查（第一次扫描，没有任何先验）：总速率不低于这个场景最好的固定宽度的 90%；不比 v1 慢（模拟器自己的噪声留 3%）；达到最好固定宽度平均速率的 80% 不晚于那个固定宽度自己（加它用时的 5%）。粗糙的两个场景各跑 12 个种子：每个种子都要过 90% 那一条，后两条按 12 次的合计算（一棵树上 v1 或者第一次停顿的早晚可能碰巧占便宜）。每个场景 8 千到 15 万个文件夹，32 个 CPU（另有三个 4 个 CPU 的场景），在调试构建里一共约 2 s（每个场景各宽度并行地跑）。结果（`cargo test -p sk-native --lib sim -- --nocapture`；百分比是总速率相对最好的固定宽度，后面是第一次扫描从开头到速率达到最好固定宽度平均速率 80% 的时间）：

    | 场景 | 最好的固定宽度 | 新算法 | v1 |
    |---|---|---|---|
    | NVMe 全冷（NTFS 式 CPU） | 32：0.368 s | 0.371 s，99%，10 ms | 0.408 s，90%，124 ms |
    | NVMe 全热 | 32：0.130 s | 0.139 s，94%，13 ms | 0.167 s，78%，从未 |
    | NVMe 扫了一半（前一半热） | 32：0.249 s | 0.256 s，97%，10 ms | 0.319 s，78%，10 ms |
    | NVMe 不均匀（30% 子树热） | 32：0.298 s | 0.302 s，99%，11 ms | 0.334 s，89%，65 ms |
    | NVMe 边读边热 | 32：0.289 s | 0.290 s，100%，10 ms | 0.326 s，89%，124 ms |
    | NVMe 全冷，15 万个文件夹 | 32：1.846 s | 1.847 s，100%，37 ms | 1.882 s，98%，135 ms |
    | SATA SSD 全冷（不争锁的 CPU） | 24：0.630 s | 0.635 s，99%，13 ms | 0.636 s，99%，13 ms |
    | SATA SSD 不均匀 | 96：0.442 s | 0.446 s，99%，10 ms | 0.454 s，97%，10 ms |
    | ReFS 式锁，全热 | 8：0.192 s | 0.198 s，97%，10 ms | 0.314 s，61%，从未 |
    | ReFS 式锁，扫了一半 | 8：0.584 s | 0.568 s，103%，12 ms | 0.794 s，74%，12 ms |
    | 机械盘全冷 | 64：31.482 s | 31.697 s，99%，630 ms | 31.681 s，99%，630 ms |
    | 机械盘扫了一半 | 32：15.736 s | 15.835 s，99%，315 ms | 15.746 s，100%，315 ms |
    | 网络共享全冷 | 96：1.962 s | 1.968 s，100%，39 ms | 2.180 s，90%，39 ms |
    | 网络共享不均匀 | 32：1.373 s | 1.384 s，99%，27 ms | 1.572 s，87%，41 ms |
    | 4 个 CPU，NVMe 全冷 | 8：1.019 s | 1.012 s，101%，20 ms | 1.748 s，58%，从未 |
    | 4 个 CPU，NVMe 全热 | 8：0.383 s | 0.387 s，99%，10 ms | 0.383 s，100%，10 ms |
    | 4 个 CPU，NVMe 不均匀 | 8：0.823 s | 0.820 s，100%，16 ms | 1.408 s，58%，61 ms |
    | 粗糙的卷，全冷（12 个种子，合计） | 32：37.36 s | 37.92 s，每个种子 97–100%（中位 99%） | 40.41 s，86–99%（中位 92.5%） |
    | 粗糙的卷，不均匀（40% 子树热；12 个种子） | 32：25.71 s | 26.78 s，92–99%（中位 96%） | 27.25 s，88–98%（中位 94.5%） |

    粗糙的两个场景照 W: 的失败做出来，先在 13f9db3 的控制器上跑过：全冷最差的种子只列到 57%（IO 道平均 17：起步翻到 32 的那一轮碰上第一次停顿，「第一步就慢就退回」把它退回 16，太吵时回家又一直留在那里），不均匀的 12 个种子里 3 个低于 90%（最低 67%），合计比 v1 慢 5%；换成 24 个种子，全冷 1 个、不均匀 6 个低于 90%，最低 55%（W: 上坏的那几次比 0.3.5 慢 1.5–2.2 倍）。改了以后（起步不往窄走、落定不低于到过的家、拐点要探测确认）24 个种子都在 91% 以上，IO 道平均 32–38，合计比 v1 快 5–6%（全冷）和 2–4%（不均匀）；达到 80% 的时间 12 个种子合计 1.15 s 和 0.97 s（上限 3.52 s 和 2.25 s）。别的 17 个场景里变了的只有 ReFS 式锁（扫了一半 114% → 103%，全热 98% → 97%：CPU 道起步不再一轮就退回 8，靠探测走到 9 左右）和 SATA、网络（IO 道落定在 32 而不是 18–20，速率差在 1% 以内）。

    换三个种子（树和缓存状态都变；`sim::seeds`，不是检查项）再跑一遍：新算法在每个场景都在最好固定宽度的 92–115% 之间，最差的是 NVMe 扫了一半的一个种子（92%，v1 78%）；比 v1 慢的只有机械盘、4 个 CPU 全热、SATA 全冷和粗糙的卷不均匀的个别种子，都在 2.4% 以内（13f9db3 在粗糙的两个场景上有 1.52 和 1.64 倍的种子）。`SK_SIM=<场景> SK_SIM_SEEDS=<n> cargo test -p sk-native --lib sim::sweep -- --ignored --nocapture` 把一个场景换 n 个种子各跑一遍。模拟器的模型是简化的：设备是按通道排队、读延迟对数正态抖动的队列，CPU 是带锁争用的处理器共享，每个文件夹的工作与项数成正比；真实的盘、缓存管理器和调度器复杂得多，所以它只证明控制器在这些曲线上的行为，最终以真机的测量为准（下面）。调用数、预算、闸门和扫描器的归并不在模拟器里（它假设调用从不成为瓶颈：扫描器的调用跟着走）。

  - 两条道的实测（2026-11-09，i9-14900K，32 个逻辑 CPU；C:（NVMe，NTFS，1,003 万项）、K:（NTFS，191 万项）、W:（NVMe，ReFS，322 万项）；后台一直有别的程序的训练和推理任务，占着一部分 CPU 和约 40 GB 内存，所以同一臂几次之间也差得多；与 0.3.5（cc36750：v1 加 Windows 上 8 个调用）比。`tools/scan_pace_bench.py`：每次扫描一个新进程、不存快照，一次只跑一个扫描，两臂每轮交替先后，比值按轮配对（几秒以上的扫描用扫描自己的用时，更短的用进程墙钟）。冷缓存：每次扫描前温和地清缓存——裁掉系统文件缓存的工作集（`SetSystemFileCacheSize(-1, -1, 0)`）、清空备用列表（`NtSetSystemInformation` 的 `MemoryPurgeStandbyList`），不动任何进程的工作集、不卸载或锁定卷；清之前查有没有别的磁盘基准或扫描器在跑、空闲内存是否至少 8 GB）：
    - 13f9db3（W: 冷缓存的修正之前；C:\Program Files、C:\Windows 和 C: 冷缓存是在 IO 道最窄还是 2 的前一版上测的，两版只差这一条），热缓存，不限速，每臂先热一次不算再 5 次：C:\Program Files（55 万项）0.45 → 0.43 s，配对 1.00 [0.90–1.02]；C:\Windows（36 万项）1.55 → 1.53 s，1.01 [0.93–1.08]；W:\work（ReFS，136 万项）0.47 → 0.39 s，0.84 [0.81–0.96]。热缓存时几乎所有文件夹都在 CPU 道（IO 道 0–2%）。
    - 13f9db3，冷缓存，整个 C:：不限速 0.3.5 29.5 / 29.5 / 28.2 s，新 29.1 / 28.7 / 27.0 s，配对 0.99 / 0.97 / 0.96；均衡档 30.8 / 29.9 / 32.3 s 对 29.4 / 29.1 / 30.4 s，0.95 / 0.97 / 0.94。85–97% 的文件夹在 IO 道，调用平均 17–25 个、最多 36–41 个（0.3.5 固定 8 个）。
    - 0.3.4 / 0.3.5 / 新（bae38a5，即 13f9db3 的代码）三版对比，冷缓存整个 C:，每版 4 轮交替、温和清缓存，中位数：命令行（`scan --no-snapshot`，每版各用一个 `SPACEKNIFE_HOME`）均衡档 27.6 / 27.2 / 26.5 s，不限速 24.7 / 26.1 / 24.0 s；走桌面应用的路径（`serve --token-stdin`，像桌面应用那样 `session.hello`，`scan.start` 硬扫描加均衡档，`view.open` 1600×860 并收帧，3 轮）28.3 / 28.5 / 29.2 s，三版都有 11–14% 的时间被限速挡着。用户报告的那次 39 s 的应用内扫描（8 个调用忙 91%，被挡 2%）两条路径都没有复现；冷缓存时 0.3.5 不比 0.3.4 慢。
    - 13f9db3，冷缓存，W:（ReFS）不限速：0.3.5 27.4 / 15.2 / 17.2 s，新 15.9 / 33.3 / 15.3 s，配对 0.58 / 2.19 / 0.89。W: 清过缓存后一次比一次快（ReFS 自己留着一部分元数据），两臂都是；新的有两种结果：好的一次 IO 道平均 28–33、调用 16–29 个，比 0.3.5 快；坏的一次 IO 道平均 15.6、CPU 道 5.8、调用 10.5 个，慢一倍。追查（`SPACEKNIFE_WALK_TRACE`，三次冷扫描：14.8 s、9.6 s、13.5 s，IO 道平均 17、32、16.7）看到两条路，都是一轮定终身：一次是 IO 道翻到 32 的第一轮碰上一个 80 ms 的慢轮（1,383/s，前一轮 31,211/s），「第一步就慢 5% 就退回」把它退回 16，噪声又大于 25%，「太吵时回家」把它一直留在 16；另一次是 CPU 道 12 宽的第一轮慢了，退回 8，这个 8 被当成拐点，IO 道的上限成了 1.25 × 8 / 0.56 = 18，起步落定在 18、家也是 18。快的那次 IO 道起步直接到了 32。改法见上面「起步」「太吵时回到起点」「两条道共用 CPU」，模拟器里「粗糙的卷」两个场景照这两条路做出来，13f9db3 在上面不过，修正后过。
    - 第一版（db78c1a：还没有起步上限、按噪声拉长的轮、太吵时回家、受压的道不动、IO 道最窄 8，分道门槛 0.5）的完整冷协议，每块 3 轮：C: 冷不限速配对 1.19 / 0.95 / 0.77，均衡档 1.18 / 0.63 / 0.60（0.3.5 自己 36.6–69.0 s）；C: 不均匀（清缓存后随机读 30% 的第二层文件夹，均衡档）0.96 / 1.01 / 0.91；C: 扫了一半（清缓存后先读 C:\Users，再扫整个 C:，均衡档）0.96 / 1.00 / 1.06，冷比例曲线从 0.6 左右降到 0（先扫的系统目录冷、后扫的 C:\Users 热），IO 道跟着从 40 多退到 2–10；K: 冷不限速 0.90 / 1.05 / 0.97，均衡档 0.79 / 1.09 / 0.99；W: 冷不限速 1.13 / 2.87 / 1.11、均衡档 1.17 / 2.29 / 1.29——W: 的追查（`SPACEKNIFE_WALK_TRACE`）引出了最终版的那几条规则。热缓存整个 C:（均衡档，每臂先热一次再 3 次）0.3.5 30.6 / 34.1 / 49.5 s，第一版 25.4 / 25.1 / 33.0 s，配对 0.83 / 0.74 / 0.67。
    - 内存：扫描进程的峰值工作集两臂一样（整个 C: 1.13–1.17 GB，W: 约 420 MB，K: 约 280 MB，C:\Program Files 约 90 MB）：两条道和样本不多占内存（每个文件夹多一个 f32 先验和一个字节的道）。
    - 修正后（起步不往窄走、落定不低于到过的家、拐点要探测确认），同样的协议，与 0.3.5 比：
      - W:（ReFS）冷缓存不限速，5 轮：0.3.5 中位 24.3 s [19.1–26.0]，新 21.7 s [17.5–23.4]，配对 0.72 / 0.99 / 1.14 / 0.89 / 0.71（中位 0.89）。每一次 IO 道平均都在 29–32（13f9db3 坏的那几次 15.6–18），调用平均 16.5–28.9 个。
      - W: 冷缓存均衡档，两组各 5 轮：0.3.5 中位 27.8 s [9.5–33.4] 和 20.9 s [12.9–33.9]，新 31.5 s [22.8–37.6] 和 23.4 s [19.0–27.3]；配对 1.13 / 1.13 / 1.10 / 1.37 / 2.40 和 0.90 / 0.69 / 1.16 / 1.49 / 1.47，10 对的中位 1.15。同一段时间与 13f9db3 比 4 轮：13f9db3 中位 23.4 s [17.2–34.8]，新 14.2 s [13.0–18.2]，配对 0.64 / 0.86 / 0.75 / 0.37。均衡档时限速器把两条道在列的合起来压在平均 18–28 个（「Disk-load limit's concurrency」），IO 道多宽决定不了速度；0.3.5 有几次（9.5、12.9、17.2 s）比两臂任何一次不限速的都快。这些配对定不了结论：后来的追查查出 W: 这块盘自己的读延迟在两档之间跳，两臂碰上哪一档全凭时间（下面）。
      - 整个 C: 冷缓存不限速，3 轮：0.3.5 中位 29.0 s [28.5–29.9]，新 27.8 s [26.0–28.9]，配对 0.91 / 0.97 / 0.96。
      - W:\work（ReFS，136 万项）热缓存，先热一次再 5 轮：0.52 → 0.42 s，配对 0.65 / 0.88 / 0.82 / 0.78 / 0.80（中位 0.80）。
      - 峰值工作集：W: 413–433 MB（0.3.5 415–438 MB），整个 C: 1.14–1.15 GB（1.15–1.17 GB），W:\work 206–219 MB（193–204 MB）。
    - W: 冷缓存均衡档的追查（上面 10 对中位 1.15 从哪来）：
      - 每次扫描另外记下 W: 所在的物理盘（W: 独占这块盘）每 0.25 s 的计数（忙、在途 IO、读次数和字节、读延迟），清缓存前后系统文件缓存的工作集，每个进程每秒的读字节和 CPU，盘的 NVMe 健康日志（温度、热节流计时）；新的构建加上限速器每个窗口的记录（`SPACEKNIFE_IO_TRACE`，4.2），0.3.5 另编一个只加了这条记录的构建（限速器的代码两版相同）。
      - 噪声：16 次冷扫描（两臂各 8）每次都读盘 380,692–382,055 次、5.95–5.97 GB，冷得一样；清缓存后文件缓存的工作集每次剩 2.07–2.09 GB；扫描前 3 s 别的程序对这块盘几乎不读（0–2 次/s）；另一个会话的训练任务一直占着 12–16 个 CPU、读 45–61 MB/s（都不在这块盘上）；盘温 43–44 °C 不变，热节流计时不动。变的是盘自己的读延迟：按扫描平均 0.45–1.15 ms，两臂在同一段时间里一起变（12:19 以前每次 1.0–1.15 ms，以后 0.45–0.68 ms；一次 0.3.5 的扫描在第 18 s 从 1.1 ms 跳到 0.55 ms）。均衡档时限速器把在途 IO 压在差不多的深度，用时就跟着延迟走：同一个构建、同样的读盘次数，13.2–23.4 s（0.3.5 14.3–28.2 s）。不扫描、不清缓存时，每 10 s 对 W: 上一个大文件做 500 次 4 KiB 无缓存随机读，5 分钟里中位延迟在 100–250 µs 和 1.25–1.39 ms 两档之间跳，盘上没有别的读、温度不动：是这块盘自己的状态（4 TB 的 NVMe 盘，像是映射表缓存或后台整理），不是扫描的代码。以前「W: 清过缓存后一次比一次快」的印象也是它：读盘次数没有变少。
      - 控制：每次记下读盘次数和平均读延迟；配对只比同一档延迟的，或者按延迟归一（用时 × 0.5 ms / 平均读延迟）。
      - 结果（09459e2 对 0.3.5，均衡档 7 对、不限速 1 对）：均衡档原始比值 0.83 / 0.80 / 0.87 / 0.55 / 0.90 / 0.93 / 0.78（中位 0.83；0.55 那对 0.3.5 在慢档、新的在快档），按延迟归一 0.84 / 0.80 / 0.98 / 0.98 / 0.95 / 0.96 / 0.82（中位 0.95），每一对都是新的快；不限速同在 0.5 ms 档 9.9 → 7.6 s（0.77）。峰值工作集两臂一样（425–437 MB）。
      - 机制（限速器每个窗口的记录）：两版的限速器一样。负载 ρ = min(忙, 在途 IO / Q_eff)，NVMe 的 Q_eff 先验 32；冷扫描时盘一直忙，ρ 就是在途 IO / 32，均衡档的设定点让在途 IO 停在 13–20 个。并发从 4 起，约 3 s 到 20、5 s 到 25，之后跟着学到的「每个在列文件夹的负载」走：0.3.5 每个在列文件夹平均带 0.62 个在途 IO（冷热混在一条道），限速器给它 29–31 个名额；新的带 0.82 个（冷的都在 IO 道），给 24–26 个。两版的 ρ 都在 0.48–0.61，没有一次让路，每次扫描 3–8 个探测窗口。并发多并没有让负载量度比有用的速率涨得快：每个在途 IO 每秒完成的读两版一样（同一档延迟下），新的只是在同一个上限下维持的在途 IO 多一些，所以快。
      - 两条道在限速下：闸门一直卡着（几乎每个窗口都有文件夹在等名额），两条道的轮大多是需求不足（每次扫描 IO 道约 700 轮、CPU 道约 650 轮）；IO 道每 4 个巡航轮往宽探一次，探测轮被闸门卡成需求不足，当场退回（每次约 155 次），宽度不动；「需求不足满 1 s 重新起步」一次也没有触发（满轮和需求不足的轮交替，计时一直被打断）。没有来回摆，代码不用改。
      - 加了这两条记录的构建回归（与 0.3.5 各 1 轮）：W: 冷缓存不限速 23.4 → 19.3 s（0.82，两臂都在 1.1–1.2 ms 的慢档），整个 C: 冷缓存不限速 28.8 → 27.0 s（0.94），W:\work 热缓存 3 轮 0.47 → 0.36 s（0.74 / 0.88 / 0.73）；峰值工作集 W: 428–429 MB、C: 1.16 GB（0.3.5 1.17 GB）、W:\work 215–221 MB（191–204 MB）。
      - 一共用了 20 次冷扫描（追查 16 次、回归 4 次，另加不清缓存的延迟探测）。
    - 其他盘（09459e2 对 0.3.5，不限速；热缓存每臂先热一次再 5 轮，冷缓存 3 轮、温和清缓存，两个 WinBtrfs 盘不清缓存、两次扫描隔 40 s：WinBtrfs 每 30 s 整个丢掉自己的缓存）：
      - 热缓存：I:（Btrfs，150 万项）0.88 → 0.64 s（配对中位 0.72）；F:（NTFS，SATA SSD，107 万项）1.50 / 1.50 s；G:（ReFS，NVMe，125 万项）0.91 / 0.96 s。
      - 冷缓存：F: 2.1–2.2 → 2.0 s（约 0.95；新的多读约 5% 的盘）；G: 7.0 / 5.4 / 6.4 对 4.3 / 5.5 / 6.7 s（G: 与 W: 同型号，读延迟也在 0.7–1.15 ms 之间跳，按延迟归一新的快约 20%，只有 3 对）；I: 4.5 / 4.7 / 5.4 对 4.5 / 4.6 / 5.4 s。
      - H:（Btrfs）和 D:（NTFS）只有约 4.5 万项（热 30–40 ms，冷 0.13–0.33 s），分不出。峰值工作集两臂一样。
    - 冷缓存整个 C: 的上限（不限速，每种 2 轮交替，温和清缓存，一共 12 次）：
      - 固定宽度（`SPACEKNIFE_SCAN_THREADS`）16 → 28.0 / 29.9 s，32 → 25.6 / 26.9 s，64 → 29.2 / 27.1 s；`SPACEKNIFE_WALK_MAX=128` 29.4 / 27.7 s；新（默认）27.2 / 28.1 s；0.3.5 29.5 / 29.3 s。
      - 每次读盘约 121–123 万次（6.5 GB），盘上平均只有 2.3–3.3 个在途 IO、读延迟 60–80 µs，8 个调用和 64 个调用一样：冷的 NTFS 元数据读在文件系统里排成了队，盘远没跑满，宽度怎么调都在 26–30 s。
      - 新的比 0.3.5 快约 6%，比最好的固定宽度（32）慢约 5%（每种只有 2 轮，还说不准）。整盘 NTFS 要大幅加快只有 MFT 快速扫描。
    - 协议和命令（在提升了权限的终端里跑，清缓存要 SeIncreaseQuotaPrivilege 和 SeProfileSingleProcessPrivilege，脚本自己打开；同一个命令里的几个路径依次跑，每次扫描一行 JSON 追加进 `--out`；每次清缓存别的程序也要重新读它们的文件，所以只在没有别的测量、内存充足时跑，次数尽量少）：

      ```
      python tools/scan_pace_bench.py --old OLD.exe --new NEW.exe --out cold.jsonl --state cold --rounds 3 C:\
      python tools/scan_pace_bench.py --old OLD.exe --new NEW.exe --out cold.jsonl --state cold --rounds 3 --limit balanced C:\
      python tools/scan_pace_bench.py --old OLD.exe --new NEW.exe --out cold.jsonl --state uneven --share 0.3 --seed 1 --rounds 3 --limit balanced C:\
      python tools/scan_pace_bench.py --old OLD.exe --new NEW.exe --out cold.jsonl --state half --warm C:\Users --rounds 3 --limit balanced C:\
      python tools/scan_pace_bench.py --old OLD.exe --new NEW.exe --out cold.jsonl --state cold --rounds 3 K:\ W:\
      python tools/scan_pace_bench.py --old OLD.exe --new NEW.exe --out cold.jsonl --state cold --rounds 3 --limit balanced K:\ W:\
      python tools/scan_pace_bench.py --old OLD.exe --new NEW.exe --out warm.jsonl --state warm --rounds 5 "C:\Program Files" C:\Windows W:\work K:\
      ```
  - 没验证的（两条道）：macOS（`CLOCK_THREAD_CPUTIME_ID` 和 `ru_inblock` 的路径只按代码推理，本机没有 Mac，等 CI 编译和测试）；Linux 只在 WSL 里跑了 sk-native、sk-core 的测试，真盘上的扫描速度没测；机械盘、U 盘、SD 卡、网络共享只在模拟器里看过（机械盘的 IO 道固定在 0.3.5 的冷宽度）；Windows on ARM 没有每线程的 CPU 时钟，等待按墙钟算，热的大文件夹可能被当成冷的，只靠读盘计数否决；同时在不同介质的盘上扫描时两条道跟先开始的那个介质；多于 64 个逻辑 CPU、跨处理器组；限速器在冷缓存下和两条道一起的长期行为（均衡档只测了几轮）；W: 那块盘读延迟分两档的原因（盘内部的什么）没查，走桌面应用路径的 W: 均衡档没测；模拟器里没有限速器，「粗糙的卷」是照三次追查做的模型，不是 W: 本身。
  - 0.3.5 的规则（v1，模拟器里的基线，`pace/v1.rs`）：只有一个宽度，起步宽度如上，冷宽度 max(2 × 起步, 32)，最宽 64，最窄 max(2, 起步 / 4)；每次调用结束时看一眼，窗口至少 0.12 s 且至少列了 32 个文件夹（或满 1 s）才决定一次。看四个数：`u`，整个进程的 CPU 时间除以墙钟和可用 CPU 数；`s`，宽度的名额有多大比例在列目录；`io`，每列一个文件夹读了几次盘（计数同上）；速率，每秒列的文件夹数加项数 / 64。规则：
    - 读盘（`io` ≥ 0.2）且名额忙（`s` > 0.5）：直接到冷宽度（不管 CPU）；此后 CPU 有余（`u` < 0.6）时加倍探测，下一个窗口快不到 15%（或者不再读盘）就退回，下次探测的等待从 0.25 s 起翻倍到 4 s。
    - 起步宽度以上，不再读盘（`io` < 0.05），或 CPU 忙（`u` > 0.9）而读得少：回到起步宽度。
    - 起步宽度及以下、不读盘、名额忙：探测一半的宽度一个窗口，再回原宽度一个窗口，新宽度的速率比前后两个窗口的均值快 15% 以上才留下；起步宽度以下时一次往下、一次往上交替。往下探测只在 `u` < 0.8 时做。
    - 数不出读盘次数（没有计数器的卷、网络共享）：从不超过起步宽度。窗口 2 s 几乎没列东西：从起步宽度重新开始。扫描开头 0.5 s 不探测。扫描器从起步宽度那么多个线程发起调用（Windows 最多 8 个），整次扫描不变。
  - v1 的实测（2026-10-25，i9-14900K，32 个逻辑 CPU，后台其他进程一直占着一部分核；与 dev a7be4ce 比；按 `spaceknife scan` 的方式调度、不限速、不存快照，每次扫描一个新进程，两臂按轮交替，中位数 [最小–最大]，比值按轮配对；只读，没有清缓存）：
    - 热缓存，32 个 CPU（每臂 5 次）：C:\Program Files（NTFS，35 万项）0.355 → 0.327 s，墙钟 0.85 [0.79–1.17]、CPU 1.15 [0.92–1.44]；K:\（NTFS，191 万项）2.57 → 2.12 s，0.84 [0.79–0.88]、CPU 0.83 [0.81–0.93]（往下探测留在 8，有时 4）；W:\work（ReFS，136 万项）0.453 → 0.432 s，0.89 [0.85–1.04]、CPU 1.05 [0.98–1.14]；I:\usr（WinBtrfs，38 万项）0.238 → 0.188 s，0.75 [0.74–0.88]、CPU 1.00 [0.86–1.16]。
    - 热缓存，进程亲和 4 个 E 核（新的数出 4 个 CPU，起步宽度 4；dev 照旧按 32 个 CPU 开 16 个列目录线程，每臂 4 次）：C:\Program Files 墙钟 0.99 [0.96–1.04]、CPU 0.93 [0.93–1.00]；K:\ 7.62 s 对 9.00 s，0.85 [0.66–1.03]、CPU 0.88 [0.86–0.93]；W:\work 1.04 [1.02–1.09]、CPU 0.96 [0.92–0.97]；I:\usr 1.04 [1.01–1.14]、CPU 0.95 [0.93–0.97]。在 4 个核上 dev 多开的线程用多 4–12% 的 CPU 换来至多 4% 的墙钟。
    - 自然冷（WinBtrfs I:\usr，每次距上次扫描 ≥ 36 s，WinBtrfs 每 30 s 整个丢掉自己的缓存；每臂 3 次）：32 个 CPU 时 0.66 s 对 0.69 s（配对 0.93，dev 第一次 1.76 s 不算），没有收益也没有损失；亲和 4 个核、dev 用 4 个列目录线程（相当于一台 4 核机器上的 dev）时 2.16–2.43 s 对 4.33–5.65 s，配对 0.43 / 0.43 / 0.50，CPU 相同；dev 在 4 个核上照旧开 16 个线程时 2.27–2.48 s 对 2.13–2.36 s（1.02–1.10：宽度要过一个窗口才到冷宽度）。
    - 一次调用同时列几个文件夹：W:\work 热缓存、宽度固定 16（`SPACEKNIFE_WALK_ADAPT=0`），每次调用都可以占满宽度时 CPU 6.85 s（内核时间比 dev 多 0.8 s），最多一个时 6.20 s（dev 6.22 s），墙钟 0.51 / 0.53 / 0.51 s（各 8 次）；所以按调用数分宽度。
    - 磁盘负载上限（`spaceknife scan --disk-limit`，自然冷的 I:\usr）：安静档下在列的文件夹数从没超过限速器的并发（4，后来 8），用时与 dev 相同（1.45 / 1.40 s）；极速档时并发 32、在列最多 30。
    - 正确性：C:\Program Files、K:\、W:\work、I:\usr 各扫 dev、新、dev 三遍，逐个文件夹比较每一项的名字、类型、大小、占用、修改时间（共 41.5 万个文件夹）：0 处不同（dev 两遍之间也 0 处）。
  - 正确性：未截断的切片与 worker 逐列相同，与宽度、闸门无关（`tests/test_hardscan.py`：子进程里固定宽度 1 / 3 / 64、只用起步宽度、自适应、最宽 8，各与 worker 逐列比；闸门 1 / 2 / 5 在共用线程和 7 个线程的私有池上；Rust `walk::tests` 宽度 3、闸门 1 / 2 与单线程逐个文件夹相同，峰值不超过上限）。被截断的切片列到哪些文件夹本来就允许不同（`test_windows_case_twins_never_wait_in_the_frontier` 在共用线程上检查任何顺序下都成立的那些：上限只报在按 ID 打开的文件夹下面，它们下面的从不作为任务交回，同名项列了或作为任务交回、不会丢）。
  - 没验证的：NTFS、ext4、APFS 的冷缓存（本机不清缓存，留给基准的运行器）；Linux 和 macOS 上的扫描速度（Linux 只跑了 cargo test）；机械硬盘；多于 64 个逻辑 CPU、跨处理器组的 Windows；cgroup v2 配额只用测试里构造的目录树验证过。
- 归并（`NodeStore.ingest_slice`）：一次把整个切片并进节点表——深度用指针跳跃求、合计用分段求和，切片内由深到浅累加，再沿已有祖先各上传一次；与逐列表的 `ingest_listing` 结果逐列一致（真实目录切片与随机切片的等价性测试）。每个列表的归并开销从 30–78 µs 降到 1–3 µs。一个 38 万项的测试目录（热缓存）端到端由 8–10 万项/秒提到 12–30 万项/秒（同一代码多次运行的波动主要来自系统），此时 16 个列目录线程都在忙、归并只占 16–22%：瓶颈已在系统的目录枚举。遍历本身：原生在 Program Files 上是进程池的 3.4–3.9 倍、`/usr` 上 11.7 倍。有原生扩展时这一步在 Rust 里做，见 2.3。
- IO 饱和第一步的实测与核对（2026-10-23，0.3.4：`skip_final`、`dot_check`、`keep_where_updated` 和 2.3 的原生归并；与 dev 565d4db 比）。i9-14900K，32 个逻辑 CPU，默认线程放置；按 `spaceknife scan` 的方式调度（`bench\run_scan.py`）；各臂按轮交替（每轮顺序反过来），中位数 [最小–最大]，比值按轮配对；后台其他进程在运行期间占 5–15 个核（中位约 7–10 个，主要是用户的训练任务和别的 agent），所以几个百分点的差别看不出来。
  - 热缓存（每臂 5 次；读了盘的那一次不算）：

    | 场景 | 565d4db | 这一版 | 墙钟比 | CPU 比 |
    |---|---|---|---|---|
    | C:\Windows（NTFS 系统盘，39 万项） | 1.97 s [1.92–2.19] | 2.04 s [1.96–2.30] | 1.02 [0.93–1.06] | 0.87 [0.81–0.89] |
    | W:\work（ReFS，136 万项；列目录线程只忙 51–68%，主线程归并是瓶颈时） | 0.66 s [0.63–0.71] | 0.47 s [0.46–0.51] | 0.72 [0.70–0.73] | 0.85 [0.73–0.90] |
    | W:\work（同上，列目录线程忙 85–94% 时，评审复测 12 对） | 1.21 s [1.11–1.25] | 1.18 s [1.14–1.31] | 0.97–0.98 | 0.88–0.89 |
    | I:\usr（WinBtrfs，38 万项） | 0.34 s [0.31–0.35] | 0.29 s [0.28–0.30] | 0.84 [0.80–0.94] | 0.92 [0.72–0.96] |
    | F:\（NTFS，SATA SSD，107 万项） | 1.44 s [1.38–1.45] | 1.37 s [1.28–1.51] | 0.94 [0.92–1.05] | 0.93 [0.83–0.99] |

  - 冷缓存（每次运行前 `purge_file_cache.ps1`，每臂 3 次）：C:\Users（NTFS 系统盘，749 万项）16.85 s [15.19–17.13] → 13.87 s [13.77–14.35]，按轮 0.85 [0.81–0.91]，CPU 0.89 [0.86–0.91]；F:\（每次都读 561 MB）3.17 s [3.13–3.23] → 3.00 s [2.96–3.05]，0.95 [0.93–0.96]。
  - 每一项单独的作用（热缓存，同一构建只关这一项，与全开比，每臂 5 次）：`skip_final` 关掉时 CPU 是 1.05 [0.97–1.17]（C:\Windows）、1.09（W:，内核 CPU 1.16 [1.11–1.39]）、1.10（I:）、1.13 [1.08–1.21]（F:）倍；`dot_check` 在噪声之内（0.92–1.11）；`keep_where_updated`（只在 W: 上起作用）墙钟 1.20 [0.80–1.24]、CPU 1.04；原生归并（`SPACEKNIFE_PY_FOLD=1` 与它比）W: 墙钟 1.15 [0.98–1.34]，C:\Windows 1.02，CPU 都在噪声之内。W: 上快的那一截主要来自主线程归并（不省收尾查询的默认设置下也是 0.76 倍，见下）：W: 热缓存每秒约 300 万项，主线程归并已经是瓶颈之一。同一棵树过了一个多小时复测时，两个构建每个文件夹的内核开销都翻了一倍（565d4db 内核 CPU 4–5.6 s → 9.2–10 s，原因没查清），瓶颈回到列目录，墙钟只剩 0.97–0.98 倍、CPU 约 0.88 倍；W:\ 整盘同时段约 0.93 倍，G:\（ReFS）0.96 倍。所以「快约四分之一」只在主线程归并是瓶颈时成立。
  - 上面的表和冷缓存数字是全开（`skip_final`、`dot_check` 也开）时测的；评审之后这两项默认关掉。默认设置（`keep_where_updated` 和原生归并）与 565d4db 比（2026-10-23 晚，`bench\seq_ab.ps1`，两臂按轮交替、顺序每轮反过来，热缓存每臂 5 次、先各热一次，冷缓存每臂 3 次）：

    | 场景 | 565d4db | 默认设置 | 墙钟比 | CPU 比 | 列目录线程忙 |
    |---|---|---|---|---|---|
    | W:\work（ReFS，136 万项，热） | 0.65 s [0.61–0.66] | 0.50 s [0.49–0.53] | 0.76 [0.75–0.87] | 0.90 [0.86–1.02] | 55–58% → 63–69% |
    | C:\Windows（NTFS，39 万项，热） | 1.82 s [1.69–1.98] | 1.84 s [1.77–1.88] | 0.99 [0.93–1.09] | 0.99 [0.88–1.13] | 84–85% → 89–91% |
    | I:\usr（WinBtrfs，38 万项，热） | 0.31 s [0.29–0.33] | 0.29 s [0.24–0.30] | 0.94 [0.83–1.00] | 1.00 [0.96–1.08] | 45–53% → 51–66% |
    | C:\Users（NTFS，767 万项，冷：每次先 `purge_file_cache.ps1`） | 15.81 s [15.56–16.63] | 14.87 s [14.72–15.71] | 0.94 [0.93–0.96] | 0.97 [0.95–0.98] | 88–90% → 93–94% |

    W:\work 这次又是主线程归并是瓶颈的状态（565d4db 内核 CPU 4.6–5.4 s），所以和上表全开时一样快约四分之一；列目录是瓶颈时（上面评审复测的状态）默认设置也只剩 CPU 上的差别。
  - 整盘 A-B-A（565d4db、这一版、565d4db，每次扫描都存快照）：逐个文件夹按名字比对三棵树，每一项比较扫描写的全部列（kind、flags、size、alloc、mtime、ctime、atime、ino、nlink），只把「两次 565d4db 相同、这一版不同」的算作这一版的差异：W:（ReFS 321.7 万项，32.5 万个文件夹）、I:（WinBtrfs 150.5 万项）、F:（107.5 万项）、K:（191.3 万项）都是 0；C:（883.3 万项，105.9 万个文件夹）2 处，都是期间的文件变动：Wallpaper Engine 一个随机名字的临时文件只在第二次扫描时存在，NZXT CAM 的 `scope_v2.json` 第二次是 12,033 字节、前后两次是 12,032 字节（两次 565d4db 之间本身就有 4,790 项访问时间、126 项修改时间、29 个名字不同）。`bench\compare.py` 对每一对的结论相同（W:、I:、F:、K: 两侧独有的名字都是 0）。Linux（WSL 根分区 66 万项）同样比对：只有 HARDLINK_DUP 落在硬链接的哪个名字上不同（取决于切片落地的先后，上面「硬链接只计一次」；两次 565d4db 之间本身就有 38,662 处），另一处是另一个程序的临时文件。
  - `skip_final` 的核验模式（`_verify_final`）整盘跑了一遍：C: 1,058,647 个文件夹、D: 4,600、F: 147,880、G:（ReFS）48,607、H:（WinBtrfs）4,794、I:（WinBtrfs）134,475、K: 303,277、W:（ReFS）324,650，共 2,026,930 个文件夹，省掉的那次查询发出去都只返回 `STATUS_NO_MORE_FILES`（`final_wrong` 0）。这几次扫描里相对打开全部由 "." 项核对通过（`dot_unmatched` 0），WinBtrfs 上也是。

### 2.3 原生归并（M4 时是 `SliceFold`，`src/fold.rs`；现在是 `crates/sk-native/src/fold.rs` 的 `SliceFold`，由 `crates/sk-store/src/store.rs` 的 `ingest_slice` 调用）

> **现状**：三步的分工与下文相同，只是「后端」换成了 sk-store：第一步（`SliceFold::plan`，通过 `fold::Tree` 读 store 的行）和第二步（`SliceFold::write`，写进 store 借出的 `RowsOut`：新行的各列、`name_off` 和名字堆里留出的一段）在 sk-native；扩列、留名字空间（`name_room`）、借出新行（`lend_rows`）和第三步发布（去掉 PENDING、版本号、节点数、先起点后数量、合计沿父链上传）在 sk-store。`ingest_listing` 和监控重列的新行用同一个写行函数（`fold::write_rows`）。测试：`crates/sk-native/tests/fold.rs`（随机切片与逐个列表归并的结果逐列相同、跳过的列表、坏切片、借出的行都写满且不越界）、`crates/sk-store/tests/ingest_slice.rs`（与 `ingest_listing` 比，含小名字块和变宽的名字偏移、一部分列表被跳过的切片、两边的 kind/flag 值一致）。

`NodeStore.ingest_slices(parts)` 把同时落地的几个切片当成一个归并（就是以前 `worker.merge_slices` 再 `ingest_slice` 的结果，但不再拼出合并后的副本）；`ingest_slice` 是只有一个切片的特例，监控的重列也走它。有原生扩展时由 `spaceknife_native.SliceFold` 分三步做，没有时（或设 `SPACEKNIFE_PY_FOLD=1`，回退开关）用原来的 numpy 归并（`NodeStore._ingest_slice_py`），两者逐列相同。

- **第一步，核对**（`SliceFold(parts, n, kind, flags, child_count, depth)`，持 GIL，只看列表、不看每一项）：哪些列表属于已离开树的文件夹（GONE，或在它里面）而跳过；对留下的列表按 `ingest_slice` 的顺序做同样的检查、报同样的 `ValueError`（引用了后面的列表、列表里没有这一项、节点不在表里、不是目录、目录已经列过），出错时什么都不写；算出每个列表的起点、所列文件夹的深度，以及要加的行数和名字字节数。
- **第二步，写行**（`write(cols, chunk, pos, base)`，**放开 GIL**）：后端先按行数扩列（`_grow`、`_fit_offsets`）、在名字堆里留出一段（`NameHeap.run`），分配都在 Python 这边；Rust 把每一项的列（parent、depth、kind、flags、各个时间、大小、ext、清零的合计和子项区间）、`\0name` 和名字偏移（uint32，名字过 4 GiB 后 int64）一遍写进去，挂载点不进 PENDING，本次列到的子目录去掉 PENDING，同时按列表求和（HARDLINK_DUP 的大小和占用不算）、由深到浅累加到上层列表。这些行在节点数之外，读者看不到；列目录线程在这段时间里可以拿到 GIL，交出它们的结果。
- **第三步，发布**（`publish(store)`，从头到尾持 GIL）：顺序与 `ingest_slice` 相同——列到的已有节点去掉 PENDING，`version` 加一，节点数更新，所有列表先写子项起点、再写数量（中间一道 release 屏障，读者先读数量），然后写新文件夹自己的合计，最后每个列到的已有节点把子树合计沿父链加到节点 0。整数按 numpy 的规则回绕（tot_files / tot_dirs 是 int32）。
- 以前每个切片要几十次 numpy 调用和每项约 100 B 的临时数组（多个切片同时落地时 `merge_slices` 再复制约 85 B/项），而且全程持有 GIL，和列目录线程抢；Linux 热缓存时主线程归并是瓶颈（73–95%）。
- 实测（2026-10-23，WSL2 根分区 77 万项热缓存，16 个 vCPU，扫描器的调度方式，三臂按轮交替各 6 次，中位数 [最小–最大]）：dev 565d4db 0.34 s [0.31–0.43]，这一版 0.26 s [0.22–0.31]，这一版设 `SPACEKNIFE_PY_FOLD=1` 0.35 s [0.33–0.37]；运行时 WSL 的 1 分钟负载约 3.0（含本测量）。单独重放同一次扫描的 24 个切片（不含列目录）：numpy 92–102 ms，原生 50–56 ms（各 5 次；两者都含列扩容的复制和新页的缺页）。Windows 见 2.2 末尾的表。
- 测试：`tests/test_store_fold_native.py`——随机树（每轮 1–4 个切片一起归并，随机 GONE、挂载点、硬链接、超大数值、多字节名字，列用扫描器的类型或宽松的类型，名字偏移窄或宽）上原生与 numpy 归并逐列、名字堆、起点、版本号、节点数全部相同（250 个例子），再和整棵树重算的合计比；空切片、全部跳过、空文件夹；坏切片两边报同样的错且什么都不改；30 万项的大切片跨过几个名字块和几次扩列；放开 GIL 期间另一个线程反复读子项区间、父节点和名字，始终是完整的列表。`tests/test_store_ingest_slice.py`（与逐列表的 `ingest_listing` 比）、`tests/test_scanner.py`（几个切片一起归并与逐个归并的树相同）现在走原生归并。

## 3. 只读扫描助手（`crates/sk-core/src/helper/`；M4 时是 `spaceknife/helper/`）

接口：`sk_core::helper` 的 `HelperClient`、`SharedHelper`、`HelperError`（`HelperErrorKind` 区分不可用 / 被拒，相当于以前的 `HelperUnavailable` / `HelperDenied`；M4 时：`from spaceknife.helper import HelperClient, SharedHelper, HelperError, HelperUnavailable, HelperDenied, is_elevated`）。

- 启动：后端生成 32 字节随机令牌和管道名 `\\.\pipe\spaceknife-helper-<随机>`，令牌写进只有当前用户和 `BUILTIN\Administrators` 能读的临时文件（DACL 受保护；POSIX 为 0600），助手读完立即删除。命令行 `<spaceknife-cli> helper --pipe <名> --token-file <文件> --server-pid <pid> [--idle-timeout 秒]`：助手是应用旁边的命令行程序 `spaceknife-cli`（每种安装方式都把它放在应用的可执行文件旁边；后端本身就是 `spaceknife-cli serve` 时就是它自己，`helper/launch.rs` 的 `helper_command_for`），带 `helper` 子命令，在加载任何别的东西之前分派（`HELPER_SUBCOMMAND`），管理员进程里运行的代码尽量少。提权的从来不是窗口程序：它不加载 WebView2、WebKitGTK、GTK 和它们的库；找不到 `spaceknife-cli` 时助手不可用（`unavailable`），不弹任何提示。助手读令牌文件时只经同一个打开的句柄（POSIX：同一个打开的文件夹）读取、核对和删除：经联接、挂载点或符号链接才到得了这个文件（例如文件夹被换成了联接）时只读不删（后端连接后会删掉它），删掉的只可能是读过的那个文件。（M4 时是 `python -I -m spaceknife.helper …`。）
  - 后端已经是管理员（「已提权」= `BUILTIN\Administrators` 组已启用）时直接以子进程启动，**不弹 UAC**；否则 `ShellExecuteExW(lpVerb="runas", SW_HIDE)` 弹一次 UAC。用户拒绝 → `HelperDenied`（「用户拒绝了管理员授权」），扫描回退硬扫描。
  - Linux（只用于「本机磁盘」读磁盘自己的记录，用户点按钮之后）：`/usr/bin/pkexec <spaceknife-cli 的绝对路径> helper …`，新会话启动（`start_new_session`，pkexec 不会退回到后端终端上要密码），由桌面的 polkit 代理弹出密码框；pkexec 通过后 exec 成助手，pid 不变，鉴权照旧。等待连接最长 180 秒（含输入密码）。退出码 126 → `HelperDenied`；127 且 stderr 有 “No authentication agent” → `no_agent`（SSH、无图形桌面），其余 127 → `not_authorized`。找不到 pkexec、AppImage（FUSE 挂载 root 读不到）、路径不是绝对路径 → `not_supported`，不启动 pkexec。助手的 stderr 在线程里读，最后 20 行用来说明原因。没有随安装包附带 polkit 策略文件，密码框显示 polkit 的通用说明（要以超级用户运行哪个程序）。
  - 鉴权：助手先校验后端 pid 再发令牌；后端校验令牌和对端 pid（Windows 允许是所启动进程的直接子进程——venv 启动器；句柄一直持有，pid 不会被复用）。
  - 一个后端只有一个助手（`SharedHelper`）：已提权的助手也服务不需要提权的请求；空闲 10 分钟退出，下次快扫再启动（非管理员后端会再弹一次 UAC）。
- 协议：帧 = 4 字节小端长度 + 内容，内容首字节是类型：`J` JSON（严格校验的消息，`helper/messages.rs`）/ `B` 二进制帧（复用 `sk_protocol::binary`，流名 `mft`，布局 `mft.v1`，名字 blob `names`）。消息：`hello {token, pid, elevated}`、`progress {phase: scan|send, elapsed}`（扫描时每秒一次心跳）、`begin {rows, frames}`、`response {result, extras}`、`error {code, message}`。
- 白名单操作：`ping`、`volume_info(volume)`、`scan_mft(volume, batch_rows, duty)`（读并解析 $MFT，分批发回各列）、`shutdown`；仅非提权助手开放的测试操作 `scan_mft_image(path, duty)`（读 NTFS 卷镜像）。参数严格校验：卷只能是 `X:` 形式的本地 NTFS 卷。M5 加了 `query_usn(volume)` 与 `read_usn(volume, start_usn, journal_id, max_rows, duty)`（USN 日志，二进制帧流 `usn`、布局 `usn.v1`，见 [monitor.md](monitor.md) §3）。0.3.3 加了 `disk_health(disk)`（只在 Linux 的提权助手里可用）：`disk` 只能是整块盘的内核名（`nvme0n1`、`sda`），助手只打开只读的 `/dev/<disk>` 块设备，只发固定的只读命令——NVMe 管理命令 Get Log Page 02h（`NVME_IOCTL_ADMIN_CMD`），或经 `SG_IO` 的 ATA PASS-THROUGH(16)：IDENTIFY DEVICE、READ LOG EXT 设备统计日志 04h 的第 01h、05h 页——把原始页面以 hex 发回，由后端解析（`crates/sk-core/src/disks/passthrough.rs`、`health.rs`）。设备不应答（USB 桥接不转发、盘没有这份日志）→ `not_supported`，附原因。
- 错误码：`needs_admin`、`forbidden`、`invalid_request`、`not_supported`、`not_ntfs`、`io_error`、`unavailable`、`bad_frame`、`internal`、`usn_inactive`（日志未开启）、`usn_gone`（检查点失效）（操作失败，助手仍可用）；连接层 `HelperUnavailable` 的 code：`exited`、`timeout`、`auth`、`protocol`、`closed`、`not_supported`、`denied`、`no_agent`、`not_authorized`（后两个是 pkexec 的）。
- 测试：自动化测试**绝不**触发 UAC。助手用普通权限运行（这台开发机本身已提权，测试用 Safer NORMALUSER 令牌启动助手），`scan_mft` 在非提权时返回 `needs_admin`；UAC 路径只用替身测试参数构造和取消映射。测试也绝不调用真正的 pkexec（找 pkexec 的路径表被清空）：命令行、退出码与 stderr 的解释用替身测试；Linux 上用一个直接 exec 参数的「pkexec」脚本跑通启动、连接、鉴权和拒绝 / 无代理。`disk_health` 的命令结构在假内核上测试（读结构里的指针、把应答写进缓冲区）；在 WSL 里以 root 对虚拟盘实测过 SG_IO（内核接受结构，返回 ILLEGAL REQUEST）。macOS 不实现提权（`elevate=True` 返回 `not_supported`）。
- 吞吐：900 万行（约 850 MB）经管道传输与拼装约 0.5 s（1.7 GB/s）；客户端直接拼进按 `begin` 预分配的表。

## 4. IO 动态限速（`crates/sk-core/src/io/`；M4 时是 `spaceknife/io/`）与扫描调度

### 4.1 采样与指标

- 每块物理磁盘一个 `IoGovernor`（`IoRegistry` 按卷 → 磁盘映射），应用启动即采样（W2-40）：
  - Windows：`IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` 找物理磁盘，PDH / 原始磁盘计数器取忙碌度与队列深度，不需要管理员。
  - Linux：`/sys/dev/block/<maj>:<min>` 找到整盘，`/sys/block/<disk>/stat` 的 io_ticks 与 time_in_queue。
  - macOS：IOKit 块存储统计（拿不到时为开环）。
- 负载 `rho = min(busy, 平均队列深度 / Q_eff)`，`Q_eff` 先验 HDD 1 / SATA SSD 8 / NVMe 32（介质来自 `scan.media`）。对 NVMe 来说，任务管理器的「活动时间」只要有一个请求在途就算忙，几个并发读就会显示 100%，而盘远未饱和；`rho` 按队列深度折算，更接近「还能让出多少给别的程序」。限速只看 `rho`；原始忙碌度作为「活动时间」（`IoStatus.active_pct`、每秒曲线点 `LoadPoint.active_pct`）显示在状态栏、扫描详情和负载曲线上，只作参考（用户 2026-10-18 决定）。
- 自身占用 = 扫描期间的 `rho` 减去基线（其他程序）；基线在扫描前测，扫描中定期短暂停发派（探测，间隔 8 s 起按需翻倍到 32 s）重测。

### 4.2 控制

- 每 0.5 s 一次：AIMD 调节允许的并发 + 占空比闸门（派发前 `await governor.permit()`）；超过上限快速减半，低于上限每 0.5 s 最多加一个；最低保持 5% 占空比（「最低进度」）。
- 原生遍历器的一个批次在几个线程上同时列文件夹（宽度，见 2.2「列目录线程」），所以限速器给它一个闸门（`IoGovernor.listing_gate()`，原生的 `ListingGate`）：在列的文件夹数不超过并发，暂停时不再开始新的。有闸门时并发按「在列的文件夹数」算：窗口里的平均与峰值取闸门的（列目录的时间之和 / 窗口长度、最多同时在列几个、有没有文件夹在等闸门），和以前每批一个列目录线程时同一个单位；批次照旧按许可派发、最多并发那么多。并发的上限是遍历器的最宽宽度（64），所以「极速」档的冷扫描能到冷宽度，「均衡」等档位由测得的负载决定能到多少（以前上限是列目录线程数 16，抗饱和又按在途批次数封顶，宽度过不了批次数加一）。状态里的「并发」在自动时显示它与遍历器当前宽度中小的那个，「进行中」是在列的文件夹数。
- 「整盘繁忙时让路」打开时，整盘超过上限就降到最低进度；「空闲」档在其他程序读盘时（> 10%）暂缓。
- 档位：极速（不限）· 均衡 70% · 安静 35% · 空闲（系统低 I/O 优先级 + 让路）· 自定义 N%。扫描中可随时改档（`scan.set_load`），下一个控制周期生效。
- 系统 I/O 优先级：Windows 进程后台模式（`PROCESS_MODE_BACKGROUND_BEGIN`）；Linux `ioprio_set(IOPRIO_CLASS_IDLE)`；macOS `setiopolicy_np(IOPOL_THROTTLE)`。
- `SPACEKNIFE_IO_TRACE=<文件>`（测量用，不属于接口）：每块盘每个采样窗口（扫描时 0.25 s）一行：墙钟、盘、忙、在途 IO、读次数/s、读延迟、ρ、别人的负载、自己的负载、每个在列文件夹的负载（`g_ref`、`gain`）、平均和最多在列、有没有被挡、探测、让路、控制前后的强度 u、并发、Q_eff、上限。`SPACEKNIFE_WALK_TRACE` 的第一行带 `wall=`（时间 0 的墙钟），两份记录可以对齐。
- 上报（2 Hz，`io` 主题 / `io.load` 事件 / `io.status`）：`{disk, level, limit, self, total, concurrency, auto, yielding, history[60]}`，W2 曲线与状态栏同源。

### 4.3 扫描调度（`crates/sk-server/src/scans.rs`；M4 时是 `server/scans.py`）

- 扫描方式 `auto`：Windows 上「整个本地 NTFS 卷」且允许提权（设置 `scan.elevate`，或已有管理员权限 / 活着的已提权助手，不需要弹窗）时用 MFT，其他情况硬扫描。`mft` 强制尝试 MFT（失败回退硬扫描并说明原因），`hard` 强制硬扫描。
- MFT 读取是一次短时的顺序突发，用占空比限速：`duty` = 极速 1.0 · 空闲 0.2 · 其他档位 上限/100（最低 0.05）。读在突发里进行（每次至少占盘 50 ms），突发之间歇够，盘被本次扫描占用的时间比例就是 `duty`（K: 实测 0.7 档 0.70、0.35 档 0.35；旧读法窗口之间有空档，只有 0.54、0.26）。
- 硬扫描中途「以管理员身份启用 MFT 快速扫描」（`scan.use_mft`，W2-24/25）：硬扫描继续，助手就绪后整树替换；被拒绝则继续硬扫描。
- 暂停/继续（`scan.pause` / `scan.resume`）：停止派发新批次；原生遍历器在途的批次经闸门立刻不再开始新的文件夹（能等的交回队列，见 2.2「列目录线程」），Python 遍历器在途的批次在预算内做完；1 秒内自身读盘归零；继续时从队列断点接着派发。MFT 读取阶段暂停按钮不可用（突发很短）。
- 快照记录扫描方式（`stats.method`，`SnapshotInfo.method`）；M4 之前的快照没有这个字段，均为硬扫描。

## 5. 验收（来自 PLAN M4）

| 项 | 目标 | 结果 |
|---|---|---|
| C: 级卷 MFT 扫描 | < 10 s | 2026-10-23（i9-14900K，NVMe，后台约 6 核负载）：助手读解析 C: 886 万项满速 1.86 s（旧读法 4.78 s），均衡档 2.76 s（旧 7.15 s）；后端建树 0.36 s（numpy 建树 10.6 s，同一张表按轮交替各 3 次）；应用里从开始到建好树（均衡档，含启动助手和传表）4.7 s [4.2–5.1]（旧 19.3 s [17.3–21.2]）。更早：安静档 C: 837 万项 22.1 s |
| 内存 | — | C: 助手峰值 +1.62 GB（旧读法 +2.82 GB），后端建树在表之外 +0.48 GB（旧 +1.01 GB），见 [memory.md](memory.md) §6 |
| 限速 | 稳态 5 s 均值 ≤ 上限，单点超出 ≤ 5 个百分点，改档 3 s 内收敛 | 控制器与采样的单元测试覆盖收敛与让路逻辑；按约定没有在用户磁盘上做长时间压测，实盘只做了短时检查（38 万项的测试目录，安静档，自身 2% / 整盘 9%，限速未触发） |
| 原生与纯 Python 一致 | 逐列一致 | MFT（含流式与随机树）、硬扫描（三平台中 Windows 与 Linux）均有逐列比对测试；没有 Rust 工具链时测试照常通过 |
