# M4：原生加速、快扫与 IO 限速 —— 设计与接口约定

本文是 M4 各部分（Rust 扩展、提权扫描助手、IO 限速、扫描调度）之间的接口契约，内容与 M4 完成时的实现一致。改动接口时先改本文。

## 1. 总体

```
 后端（普通权限，asyncio）
   ScanManager ──► Scanner ──► 扫描策略
                                 ├─ 硬扫描：有原生扩展时 spaceknife_native.scan_slice（线程池，释放 GIL）
                                 │          否则 spawn 进程池 × worker.scan_slice（纯 Python）
                                 └─ MFT 快扫：SharedHelper ──命名管道──► 只读扫描助手（管理员）
                                                                         └─ scan_mft：边读边解析（Rust；回退 numpy）
   IoRegistry → IoGovernor（每块物理磁盘一个）：采样 → 控制并发与派发节奏 → 2 Hz 上报（W2 曲线、状态栏）
```

- 原生扩展是**可选**的：`uv sync --group native` 才编译（需要 Rust 工具链）。后端通过 `spaceknife.native` 探测，所有原生路径都有纯 Python 回退，结果逐列一致（有测试比对）。
- 原生代码在耗时部分释放 GIL（`py.allow_threads`），内部用 rayon 或自有线程并行。
- 整个程序不提权；需要管理员权限的只有只读扫描助手，它只做白名单内的只读操作，用一次性令牌鉴权。
- 构建注意：不要在私有 venv 里用 `maturin develop`（它会把 `_native.pyd/.so` 写进共享的 `packages/native/python/spaceknife_native/`）。要单独的构建就 `uvx maturin build --release -m packages/native/Cargo.toml -o <目录>` 再 `uv pip install --reinstall --no-deps`。

## 2. Rust 扩展 `spaceknife_native`（packages/native）

模块布局：`src/lib.rs` 只做注册；`src/mft/` 与 `src/hardscan/` 各自独立（feature `mft` / `hardscan`，默认都开），`src/fold.rs` 是节点表的归并（2.3，不分 feature）。Python 包 `python/spaceknife_native/`，类型桩在 `_native.pyi`。

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
  - 路径达到 PATH_MAX 的文件夹（0.3.4）：`open()` 对 4,096 字节（macOS 1,024）及以上的路径报 ENAMETOOLONG，但文件夹在、能列。以前两个遍历器都把它记成列目录错误（「File name too long」），下面整个漏掉（独立参照遍历器逐项比对时发现，2026-10-22）。现在只在 ENAMETOOLONG 时退回 `open_long`（`worker._open_dir`）：按不超过 1,000 字节、在 `/` 处切开的若干段，每段 `openat` 相对上一段打开的文件夹；中间的段用 `O_PATH`（Linux；只要求搜索权限，与按完整路径解析相同，其他系统用 `O_RDONLY`），最后一段用原来的标志（`O_NOFOLLOW` 只管最后一个名字，与一次打开相同），中间的符号链接照样跟随。正常路径一次 `open()`，不多花。Windows 不同：NT 路径本身最长 32,767 个单位，见上。测试：`test_posix_folders_past_path_max_are_listed`（调用内发现的和作为任务交来的长路径文件夹，两个遍历器逐列一致）、Rust `paths_past_path_max_open_piece_by_piece`（双斜杠、中间的符号链接、错误码与一次打开相同）。
- Linux 保留最后访问时间（`keep_access_time`，0.3.4）：目录打开后用 `fcntl(F_SETFL, O_NOATIME)` 补上 `O_NOATIME`，通过它列目录就不更新访问时间。内核只允许目录的属主和有 `CAP_FOWNER` 的进程这样做，别人被拒（EPERM），那个目录照旧列、访问时间照旧更新；打开后再设，不用事先知道属主，也不会多打开一次。`relatime`（大多数发行版的默认）下，一天里第一次扫描会改写所有一天没访问过的目录的 inode，Btrfs 上还要写时复制元数据、拆开快照之间共享的元数据。WSL 的根分区上 4.7 万个目录里 3.3 万个属于当前用户（家目录、uv 缓存），扫描家目录时几乎全部受益；系统目录属于 root，标准用户没有办法。测试（`test_listing_keeps_folder_access_times`）把测试目录的访问时间设成 2001 年再扫：默认选项一个都不变，关掉时都被改写（文件系统根本不更新访问时间时跳过）。macOS 没有对应的开关。
- 硬链接只计一次（`LinkSet`，0.3.4）：扫描器把计过的 `(dev, ino)` 记在原生的 `LinkSet` 里，`mark(dev, ino, nlink, kind, flags)` 按切片顺序给后来者标 `HARDLINK_DUP`，与原来 `Scanner._fold` 里 Python 集合加逐项循环的结果逐项相同（随机数据与真实扫描两种等价性测试）；没有原生扩展时仍用 Python 集合。原来的循环每个多链接文件约 0.5 µs：WSL 根分区（60 万项，其中 16 万个文件有多个链接，uv 的缓存与各个虚拟环境共享）热缓存扫描中位数 0.37 s → 0.29 s。计在哪个名字上仍取决于先合并进来的是哪个，原生与 Python 遍历器顺序不同，所以真实目录比对（`test_native_scan_of_a_real_tree_matches_the_python_walker`）不比较这一项和由它决定的合计。
- Linux 上的瓶颈（2026-10-22，WSL2 的 ext4 根分区，60 万项，16 个 vCPU）：热缓存约 0.3–0.4 s，列目录线程只忙 8–17%，主线程归并占 73–95%；线程数 4 / 8 / 16 / 32 用时没有差别（0.32–0.41 s），所以更快的 `fstatat`（例如只要所需字段的 `statx`）在热缓存下改变不了总用时。冷缓存（`drop_caches` 加清宿主机缓存）3.4–5.9 s，读 327 MB，列目录线程忙 83–91%，是读盘。原生与 Python 遍历器冷热各两遍逐个文件夹一致。
- 名字编码同 worker.py（Windows surrogatepass，POSIX 原始字节等价的 surrogateescape）；非 ASCII 扩展名用 Python 自己的 `str.lower` 小写，避免 Unicode 版本差异。
- 有意差异（未截断的切片与 worker 完全一致）：(1) 被预算截断的切片是「已列目录的先序子树」而不是 worker 的 DFS 前缀，引用仍然有效；每个线程按目录检查预算，`max_entries` 最多超出每线程一个目录。(2) Windows 上空的 FAT 根是空列表而不是错误。（名字以点或空格结尾的目录以前也在这里：Win32 会去掉结尾的点和空格，worker 按普通路径打开 `odd.` 列出的是旁边的 `odd`；0.3.4 起 worker 对这样的路径也加 `\\?\` 前缀，两边相同。）(3) Windows 上 NT 路径 32,763–32,767 个单位的目录，worker 报「系统找不到指定的路径」（`os.scandir` 在路径后加 `\*.*` 再交给 `FindFirstFileW`，超出上限）；原生列得出，32,767 时报错误 206。再长的两边都报错误 206。 (4) Windows 上只差大小写、按名字打不到的文件夹（输掉的那一个）、名字完全相同的（DUP）和名字里有路径不能有的字符的（BADNAME）：原生按文件 ID 打开并列出，worker 不能按 ID 打开，报 CASE、DUP、BADNAME 错误（见上「只差大小写的同名项」）。(5) Windows 本地 NTFS、ReFS、Btrfs 上，只有收尾那次目录查询才会报的错误（两次查询之间文件夹被删、被拒绝访问），原生不再报，列出此前读到的内容（见上「省掉收尾的目录查询」）。
- 扫描器的接法（`scan/scanner.py`）：有扩展时 `native_slicer()` 以 `threads=0` 调用，每次调用的文件夹都放到进程共用的列目录线程上列；后端从 `native_threads()` 个线程发起调用（= 起步宽度，见下），每批 256 个目录任务、预算 0.05 s，在途的批次数由 IO 限速器的许可控制；有限速器时每次调用带上这块盘的闸门（`gate`）。构建里没有 `gate` 这个键时（旧构建）照旧每次调用 1 个线程（`threads=1`）。没有扩展时退回 spawn 进程池。
- 列目录线程（`hardscan/pace.rs`、`slots.rs`、`walk.rs`）：
  - 谁定、定什么：Rust。一个进程一组共用的列目录线程（rayon 池），所有 `threads=0` 的调用都在上面列，互相分担。宽度是「同时在列的文件夹数」的上限（`Slots`），不是线程数：一个文件夹拿到宽度和闸门各一个名额才开始列，拿不到的留在本次调用的待列栈里（`Parked`），列目录的线程从不为名额等待（闸门只放两个的 HDD 扫描不会占住别的盘的线程）。一个文件夹列完，本次调用栈顶的下一个文件夹直接接过它的名额，由同一个线程接着列（深度优先；不建任务、不唤醒线程、不还名额再拿）；多出来的空名额给栈里其余的文件夹，成为别的空闲线程的任务。一次调用同时在列的文件夹最多是宽度在共用线程上的调用之间平分（向上取整：开扫时只有一个调用，全给它；热缓存时 16 个调用对 16 的宽度，一个一个地列，和以前一样；冷宽度时两个），因为同一次调用里并排列兄弟文件夹，文件系统更费劲：W:\work（ReFS）热缓存时内核时间多约 14%，墙钟差不多。一次调用的文件夹全在等名额时，由调用它的线程等（有名额还回来就醒，最长 10 ms 再看一次），预算到了或闸门暂停时把能等的交回去。线程池的线程数至少是宽度：宽度超过时换一个更大的池（还在旧池上的调用在旧池上做完，旧池的线程随后退出），宽度回到起步宽度 2 s 后（或扫描结束）换回起步宽度那么多。
  - 起步宽度：本进程可用的 CPU 数（`cpu_count`：Windows 按进程亲和掩码，线程跨处理器组时把所在各组的活动处理器加起来；Linux 是 `sched_getaffinity` 与 cgroup v2 的 `cpu.max` 配额（向上取整，沿 cgroup 路径到挂载点取最小）里小的那个，且不超过 Rust 标准库的计数，它也认 cgroup v1；macOS 是逻辑 CPU 数），取 2–16；macOS 是它的两倍、4–16（APFS 列目录常在内核里等，线程多一些 CPU 才忙得起来，见 [packaging.md](packaging.md)）。冷宽度 max(2 × 起步, 32)，最宽 64，最窄 max(2, 起步 / 4)。以前列目录线程数按 `os.cpu_count()` 定，不认亲和与配额：限制到 4 个核的进程照样开 16 个列目录线程（加上每个调用自己的线程池，共约 42 个线程）；后端的计算池、MFT 的线程、快照压缩的线程也一样，现在都按 `spaceknife.cpus.usable_cpus()`（有扩展时就是 `cpu_count`；没有时只认 Linux 的亲和）。
  - 控制器（`pace.rs` 的 `decide`）：每次调用结束时看一眼，窗口至少 0.12 s 且至少列了 32 个文件夹（或满 1 s）才决定一次。看四个数：`u`，整个进程的 CPU 时间除以墙钟和可用 CPU 数；`s`，宽度的名额有多大比例在列目录（低：扫描器给的活不够，或者闸门放得少）；`io`，每列一个文件夹读了几次盘（Windows 是扫描根所在卷的 `IOCTL_DISK_PERFORMANCE` 读次数，含别的程序对这个卷的读；POSIX 是本进程的 `getrusage` `ru_inblock`，按 4 KiB 一次）；速率，每秒列的文件夹数加项数 / 64（大文件夹不算慢窗口）。规则：
    - 读盘（`io` ≥ 0.2）且名额忙（`s` > 0.5）：直接到冷宽度（不管 CPU）；此后 CPU 有余（`u` < 0.6）时加倍探测，下一个窗口快不到 15%（或者不再读盘）就退回，下次探测的等待从 0.25 s 起翻倍到 4 s。
    - 起步宽度以上，不再读盘（`io` < 0.05），或 CPU 忙（`u` > 0.9）而读得少：回到起步宽度。
    - 起步宽度及以下、不读盘、名额忙：探测一半的宽度一个窗口，再回原宽度一个窗口，新宽度的速率比前后两个窗口的均值快 15% 以上才留下（前后取均值，抵消树越列越快或越慢）；起步宽度以下时一次往下、一次往上（回起步宽度方向）交替。往下探测只在 `u` < 0.8 时做：CPU 几乎全忙时列目录的线程并不互相等，少一半线程就慢一半（4 个核、热缓存：每次都只有 0.55–0.65 的速率）。速率说了算，不只看 CPU：在文件系统的锁上等的线程，可能占着 CPU 也可能不占。
    - 数不出读盘次数（没有计数器的卷、网络共享）：从不超过起步宽度。窗口 2 s 几乎没列东西（扫描结束、暂停）：从起步宽度重新开始。扫描开头 0.5 s 不探测：失败的探测要在差的宽度上过一个窗口，1 秒内的扫描付不起（C:\Program Files 0.4 s 的热扫描里一次往下探测要 0.06 s）。
  - 磁盘负载上限（`ListingGate`，见 4.2）：每块盘一个闸门，上限 = 限速器的并发，暂停 = 限速器的暂停；在列的文件夹数从不超过它（`window()` 给出窗口里的峰值）。暂停时能等的文件夹不再开始，原样作为任务交回（预算之内暂停的调用也立刻交回）；必须在本次调用里列的（按文件 ID 打开的文件夹下面的、名字与别的项相同的，见上「预算」）照样列，但不超过上限，和以前在途的批次做完同样多的事。
  - 环境变量（测量用，不属于接口）：`SPACEKNIFE_SCAN_THREADS=N`（1–64）把宽度和扫描器的调用线程都固定为 N；`SPACEKNIFE_WALK_ADAPT=0` 固定在起步宽度；`SPACEKNIFE_WALK_MAX`（默认 64，1–256；POSIX 另外不超过 RLIMIT_NOFILE 软上限减 128 后的一半）；`SPACEKNIFE_WALK_TRACE=<文件>` 把每次决定追加写进文件。`listing_threads(reset=False)` 返回 `cpus`、`start`、`least`、`cold`、`max`、`width`、`threads`（池现在的线程数）、`mode`（`adaptive` / `fixed` / `start`）和上次 reset 以来最窄、最宽的宽度 `low` / `high`（扫描的耗时分解写出它：「listing width 8–16」）。
  - 实测（2026-10-25，i9-14900K，32 个逻辑 CPU，后台其他进程一直占着一部分核；与 dev a7be4ce 比；按 `spaceknife scan` 的方式调度、不限速、不存快照，每次扫描一个新进程，两臂按轮交替，中位数 [最小–最大]，比值按轮配对；只读，没有清缓存）：
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

### 2.3 原生归并（`SliceFold`，`src/fold.rs`）

`NodeStore.ingest_slices(parts)` 把同时落地的几个切片当成一个归并（就是以前 `worker.merge_slices` 再 `ingest_slice` 的结果，但不再拼出合并后的副本）；`ingest_slice` 是只有一个切片的特例，监控的重列也走它。有原生扩展时由 `spaceknife_native.SliceFold` 分三步做，没有时（或设 `SPACEKNIFE_PY_FOLD=1`，回退开关）用原来的 numpy 归并（`NodeStore._ingest_slice_py`），两者逐列相同。

- **第一步，核对**（`SliceFold(parts, n, kind, flags, child_count, depth)`，持 GIL，只看列表、不看每一项）：哪些列表属于已离开树的文件夹（GONE，或在它里面）而跳过；对留下的列表按 `ingest_slice` 的顺序做同样的检查、报同样的 `ValueError`（引用了后面的列表、列表里没有这一项、节点不在表里、不是目录、目录已经列过），出错时什么都不写；算出每个列表的起点、所列文件夹的深度，以及要加的行数和名字字节数。
- **第二步，写行**（`write(cols, chunk, pos, base)`，**放开 GIL**）：后端先按行数扩列（`_grow`、`_fit_offsets`）、在名字堆里留出一段（`NameHeap.run`），分配都在 Python 这边；Rust 把每一项的列（parent、depth、kind、flags、各个时间、大小、ext、清零的合计和子项区间）、`\0name` 和名字偏移（uint32，名字过 4 GiB 后 int64）一遍写进去，挂载点不进 PENDING，本次列到的子目录去掉 PENDING，同时按列表求和（HARDLINK_DUP 的大小和占用不算）、由深到浅累加到上层列表。这些行在节点数之外，读者看不到；列目录线程在这段时间里可以拿到 GIL，交出它们的结果。
- **第三步，发布**（`publish(store)`，从头到尾持 GIL）：顺序与 `ingest_slice` 相同——列到的已有节点去掉 PENDING，`version` 加一，节点数更新，所有列表先写子项起点、再写数量（中间一道 release 屏障，读者先读数量），然后写新文件夹自己的合计，最后每个列到的已有节点把子树合计沿父链加到节点 0。整数按 numpy 的规则回绕（tot_files / tot_dirs 是 int32）。
- 以前每个切片要几十次 numpy 调用和每项约 100 B 的临时数组（多个切片同时落地时 `merge_slices` 再复制约 85 B/项），而且全程持有 GIL，和列目录线程抢；Linux 热缓存时主线程归并是瓶颈（73–95%）。
- 实测（2026-10-23，WSL2 根分区 77 万项热缓存，16 个 vCPU，扫描器的调度方式，三臂按轮交替各 6 次，中位数 [最小–最大]）：dev 565d4db 0.34 s [0.31–0.43]，这一版 0.26 s [0.22–0.31]，这一版设 `SPACEKNIFE_PY_FOLD=1` 0.35 s [0.33–0.37]；运行时 WSL 的 1 分钟负载约 3.0（含本测量）。单独重放同一次扫描的 24 个切片（不含列目录）：numpy 92–102 ms，原生 50–56 ms（各 5 次；两者都含列扩容的复制和新页的缺页）。Windows 见 2.2 末尾的表。
- 测试：`tests/test_store_fold_native.py`——随机树（每轮 1–4 个切片一起归并，随机 GONE、挂载点、硬链接、超大数值、多字节名字，列用扫描器的类型或宽松的类型，名字偏移窄或宽）上原生与 numpy 归并逐列、名字堆、起点、版本号、节点数全部相同（250 个例子），再和整棵树重算的合计比；空切片、全部跳过、空文件夹；坏切片两边报同样的错且什么都不改；30 万项的大切片跨过几个名字块和几次扩列；放开 GIL 期间另一个线程反复读子项区间、父节点和名字，始终是完整的列表。`tests/test_store_ingest_slice.py`（与逐列表的 `ingest_listing` 比）、`tests/test_scanner.py`（几个切片一起归并与逐个归并的树相同）现在走原生归并。

## 3. 只读扫描助手（`spaceknife/helper/`）

接口：`from spaceknife.helper import HelperClient, SharedHelper, HelperError, HelperUnavailable, HelperDenied, is_elevated`。

- 启动：后端生成 32 字节随机令牌和管道名 `\\.\pipe\spaceknife-helper-<随机>`，令牌写进只有当前用户和 `BUILTIN\Administrators` 能读的临时文件（DACL 受保护；POSIX 为 0600），助手读完立即删除。命令行 `python -I -m spaceknife.helper --pipe <名> --token-file <文件> --server-pid <pid> [--idle-timeout 秒]`（不经过 typer/rich，管理员进程里加载的代码尽量少）。
  - 后端已经是管理员（「已提权」= `BUILTIN\Administrators` 组已启用）时直接以子进程启动，**不弹 UAC**；否则 `ShellExecuteExW(lpVerb="runas", SW_HIDE)` 弹一次 UAC。用户拒绝 → `HelperDenied`（「用户拒绝了管理员授权」），扫描回退硬扫描。
  - Linux（只用于「本机磁盘」读磁盘自己的记录，用户点按钮之后）：`/usr/bin/pkexec <解释器或冻结后端的绝对路径> …`，新会话启动（`start_new_session`，pkexec 不会退回到后端终端上要密码），由桌面的 polkit 代理弹出密码框；pkexec 通过后 exec 成助手，pid 不变，鉴权照旧。等待连接最长 180 秒（含输入密码）。退出码 126 → `HelperDenied`；127 且 stderr 有 “No authentication agent” → `no_agent`（SSH、无图形桌面），其余 127 → `not_authorized`。找不到 pkexec、AppImage（FUSE 挂载 root 读不到）、路径不是绝对路径 → `not_supported`，不启动 pkexec。助手的 stderr 在线程里读，最后 20 行用来说明原因。没有随安装包附带 polkit 策略文件，密码框显示 polkit 的通用说明（要以超级用户运行哪个程序）。
  - 鉴权：助手先校验后端 pid 再发令牌；后端校验令牌和对端 pid（Windows 允许是所启动进程的直接子进程——venv 启动器；句柄一直持有，pid 不会被复用）。
  - 一个后端只有一个助手（`SharedHelper`）：已提权的助手也服务不需要提权的请求；空闲 10 分钟退出，下次快扫再启动（非管理员后端会再弹一次 UAC）。
- 协议：帧 = 4 字节小端长度 + 内容，内容首字节是类型：`J` JSON（pydantic 严格模型）/ `B` 二进制帧（复用 `spaceknife_protocol.binary`，流名 `mft`，布局 `mft.v1`，名字 blob `names`）。消息：`hello {token, pid, elevated}`、`progress {phase: scan|send, elapsed}`（扫描时每秒一次心跳）、`begin {rows, frames}`、`response {result, extras}`、`error {code, message}`。
- 白名单操作：`ping`、`volume_info(volume)`、`scan_mft(volume, batch_rows, duty)`（读并解析 $MFT，分批发回各列）、`shutdown`；仅非提权助手开放的测试操作 `scan_mft_image(path, duty)`（读 NTFS 卷镜像）。参数严格校验：卷只能是 `X:` 形式的本地 NTFS 卷。M5 加了 `query_usn(volume)` 与 `read_usn(volume, start_usn, journal_id, max_rows, duty)`（USN 日志，二进制帧流 `usn`、布局 `usn.v1`，见 [monitor.md](monitor.md) §3）。0.3.3 加了 `disk_health(disk)`（只在 Linux 的提权助手里可用）：`disk` 只能是整块盘的内核名（`nvme0n1`、`sda`），助手只打开只读的 `/dev/<disk>` 块设备，只发固定的只读命令——NVMe 管理命令 Get Log Page 02h（`NVME_IOCTL_ADMIN_CMD`），或经 `SG_IO` 的 ATA PASS-THROUGH(16)：IDENTIFY DEVICE、READ LOG EXT 设备统计日志 04h 的第 01h、05h 页——把原始页面以 hex 发回，由后端解析（`disks/passthrough.py`、`disks/health.py`）。设备不应答（USB 桥接不转发、盘没有这份日志）→ `not_supported`，附原因。
- 错误码：`needs_admin`、`forbidden`、`invalid_request`、`not_supported`、`not_ntfs`、`io_error`、`unavailable`、`bad_frame`、`internal`、`usn_inactive`（日志未开启）、`usn_gone`（检查点失效）（操作失败，助手仍可用）；连接层 `HelperUnavailable` 的 code：`exited`、`timeout`、`auth`、`protocol`、`closed`、`not_supported`、`denied`、`no_agent`、`not_authorized`（后两个是 pkexec 的）。
- 测试：自动化测试**绝不**触发 UAC。助手用普通权限运行（这台开发机本身已提权，测试用 Safer NORMALUSER 令牌启动助手），`scan_mft` 在非提权时返回 `needs_admin`；UAC 路径只用替身测试参数构造和取消映射。测试也绝不调用真正的 pkexec（找 pkexec 的路径表被清空）：命令行、退出码与 stderr 的解释用替身测试；Linux 上用一个直接 exec 参数的「pkexec」脚本跑通启动、连接、鉴权和拒绝 / 无代理。`disk_health` 的命令结构在假内核上测试（读结构里的指针、把应答写进缓冲区）；在 WSL 里以 root 对虚拟盘实测过 SG_IO（内核接受结构，返回 ILLEGAL REQUEST）。macOS 不实现提权（`elevate=True` 返回 `not_supported`）。
- 吞吐：900 万行（约 850 MB）经管道传输与拼装约 0.5 s（1.7 GB/s）；客户端直接拼进按 `begin` 预分配的表。

## 4. IO 动态限速（`spaceknife/io/`）与扫描调度

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
- 上报（2 Hz，`io` 主题 / `io.load` 事件 / `io.status`）：`{disk, level, limit, self, total, concurrency, auto, yielding, history[60]}`，W2 曲线与状态栏同源。

### 4.3 扫描调度（`server/scans.py`）

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
