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

模块布局：`src/lib.rs` 只做注册；`src/mft/` 与 `src/hardscan/` 各自独立（feature `mft` / `hardscan`，默认都开）。Python 包 `python/spaceknife_native/`，类型桩在 `_native.pyi`。

### 2.1 MFT（`src/mft/`）

```python
def mft_volume_info(volume: str) -> dict[str, int]: ...
    # Windows：打开 \\.\X:（需要管理员），几何信息取自引导扇区（不用 FSCTL）：bytes_per_sector,
    # cluster_size, record_size, mft_lcn, mft_size, mft_valid_size, mft_fragments, volume_size。
def scan_mft(volume: str, duty: float = 1.0) -> tuple[MftTable, dict[str, int]]: ...
    # 主路径：按 $MFT 的 data runs 每次读 32 MiB 窗口，读线程与 rayon 解析重叠（有界通道，最多领先一个窗口）；
    # duty ∈ (0, 1]：读线程按这个占空比歇一歇（IO 档位换算见 4.3）。峰值内存约等于结果表本身
    # （C: 845 万项时助手峰值 1.6 GB；整块读入再解析要 10.6 GB）。
def read_mft(volume: str) -> bytes: ...          # 整个 $MFT 读进内存（旧接口，测试与对比用）
def parse_mft(data, record_size: int, cluster_size: int) -> MftTable: ...
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

解析规则：只保留 in-use 记录；应用 fixup 并校验，坏记录跳过并计数（`bad_records`）；扩展记录按头部的 base reference（含序列号）合并，所以非驻留的 `$ATTRIBUTE_LIST` 也能处理；同一父目录下同一记录的 DOS 8.3 名与 Win32 名并存时只保留 Win32 名；`$FILE_NAME` 的父引用取自该名字本身（序列号不校验）。流式解析时，带 `$ATTRIBUTE_LIST` 的基础记录推迟到所有窗口读完再输出；没有列表却在后面出现扩展记录的基础记录，结束时按记录号补读重解析——所以行基本按记录号排列，有扩展记录的文件排在最后。

后端把 `MftTable` 变成 `NodeStore`（`spaceknife/scan/mft.py` 的 `table_to_store`）：

- 按父子关系做 BFS，保证每个目录的子项连续，一次 `NodeStore.from_columns`（新建的列直接接管，不再复制）+ `reaggregate`。
- 记录 5 是根；元文件（记录 0–15 与 `$Extend` 树）放进伪目录 `$NTFS 元数据`；父项缺失、不是目录或成环的名字放进 `$孤立文件`（成环时每个环断开一个成员）。伪目录只在非空时创建，带 `Flag.VIRTUAL`；它们下面的项没有真实路径（`tree.node` / `tree.summary` 的 `on_disk = false`，界面禁用「在资源管理器中显示」和「加入删除篮」）。
- 占用规则同硬扫描：目录与链接不占空间，同一文件的第二个及以后的名字标 `HARDLINK_DUP`；`ino` 是记录号。
- 备用数据流只在 `include_ads`（设置「扫描 NTFS 备用数据流」）时计入；元文件夹里的流总是计入占用（`$UsnJrnl:$J`、`$Secure:$SDS` 等）。例外是 `$BadClus:$Bad`：它和卷一样长、只映射坏簇，却不带稀疏标志，allocated size 等于整卷大小，所以从不计入（坏簇本身也不计）。
- `consume=True`：调用方把表交出来，列在拷进节点顺序后立即释放（C: 级的表约 1 GB，建树峰值相应降低）。
- 与硬扫描的有意差异：云占位文件保留 MFT 里真实的 alloc（硬扫描记 0）；MFT 能看到硬扫描被拒绝访问的目录（如 System Volume Information）；元文件放在伪目录里。

测试钩子（不属于公开接口）：`_mft_image_info(path)`、`_read_mft_image(path)`、`_scan_mft_image(path, duty=1.0, window=0)`、`_parse_mft_stream(data, record_size, cluster_size, window_records)`，读带引导扇区的卷镜像或按小窗口驱动流式解析器。

### 2.2 并行硬扫描（`src/hardscan/`）

```python
def scan_slice(tasks: list[tuple[tuple[int, int, int], str, int]], opts: SliceOpts | SliceOptions) -> dict: ...
```

与 `spaceknife.scan.worker.scan_slice` **同一输入同一输出**（返回 dict 的键与 `SliceResult` 字段一一对应：`SliceResult(**scan_slice(...))`）。

- `opts`：dict 或带同名属性的对象（`worker.SliceOptions` 可直接传）。未知键报 `ValueError`。
  - `roots`：每个根一项 `{dev, cluster_size, resident_max, cross_mounts}`，语义同 `worker.RootOptions`。
  - `budget_s`（默认 0.03；≤ 0 表示列一次就停）、`max_entries`（默认 100000）。
  - `threads`：调用内部的列目录线程数，0 表示 min(逻辑 CPU, 16)。
- Windows：`NtQueryDirectoryFile` + `FileIdExtdDirectoryInformation`（一次系统调用拿到名字、大小、占用、时间、属性、文件 ID、重解析标记），NT 路径形式打开。
- POSIX：`openat` + `getdents64`（Linux）/ `readdir`（macOS）+ `fstatat`，按 fd 相对路径遍历；不跨设备（除非 `cross_mounts`）；不跟随符号链接，调用内新发现的子目录用 `O_NOFOLLOW` 打开。
- 名字编码同 worker.py（Windows surrogatepass，POSIX 原始字节等价的 surrogateescape）；非 ASCII 扩展名用 Python 自己的 `str.lower` 小写，避免 Unicode 版本差异。
- 有意差异（未截断的切片与 worker 完全一致）：(1) 被预算截断的切片是「已列目录的先序子树」而不是 worker 的 DFS 前缀，引用仍然有效；每个线程按目录检查预算，`max_entries` 最多超出每线程一个目录。(2) Windows 上名字以点或空格结尾的目录能正常列出（worker 打不开）；空的 FAT 根是空列表而不是错误。
- 扫描器的接法（`scan/scanner.py`）：有扩展时 `native_slicer()` 每次调用只用 1 个列目录线程（`threads=1`），由后端的线程池并发多次调用，并发数由 IO 限速器控制（`native_threads()` = min(CPU, 16) 为上限）；每批 256 个目录任务、预算 0.05 s。没有扩展时退回 spawn 进程池。
- 归并（`NodeStore.ingest_slice`）：一次把整个切片并进节点表——深度用指针跳跃求、合计用分段求和，切片内由深到浅累加，再沿已有祖先各上传一次；与逐列表的 `ingest_listing` 结果逐列一致（真实目录切片与随机切片的等价性测试）。每个列表的归并开销从 30–78 µs 降到 1–3 µs。一个 38 万项的测试目录（热缓存）端到端由 8–10 万项/秒提到 12–30 万项/秒（同一代码多次运行的波动主要来自系统），此时 16 个列目录线程都在忙、归并只占 16–22%：瓶颈已在系统的目录枚举。遍历本身：原生在 Program Files 上是进程池的 3.4–3.9 倍、`/usr` 上 11.7 倍。

## 3. 只读扫描助手（`spaceknife/helper/`）

接口：`from spaceknife.helper import HelperClient, SharedHelper, HelperError, HelperUnavailable, HelperDenied, is_elevated`。

- 启动：后端生成 32 字节随机令牌和管道名 `\\.\pipe\spaceknife-helper-<随机>`，令牌写进只有当前用户和 `BUILTIN\Administrators` 能读的临时文件（DACL 受保护；POSIX 为 0600），助手读完立即删除。命令行 `python -I -m spaceknife.helper --pipe <名> --token-file <文件> --server-pid <pid> [--idle-timeout 秒]`（不经过 typer/rich，管理员进程里加载的代码尽量少）。
  - 后端已经是管理员（「已提权」= `BUILTIN\Administrators` 组已启用）时直接以子进程启动，**不弹 UAC**；否则 `ShellExecuteExW(lpVerb="runas", SW_HIDE)` 弹一次 UAC。用户拒绝 → `HelperDenied`（「用户拒绝了管理员授权」），扫描回退硬扫描。
  - 鉴权：助手先校验后端 pid 再发令牌；后端校验令牌和对端 pid（Windows 允许是所启动进程的直接子进程——venv 启动器；句柄一直持有，pid 不会被复用）。
  - 一个后端只有一个助手（`SharedHelper`）：已提权的助手也服务不需要提权的请求；空闲 10 分钟退出，下次快扫再启动（非管理员后端会再弹一次 UAC）。
- 协议：帧 = 4 字节小端长度 + 内容，内容首字节是类型：`J` JSON（pydantic 严格模型）/ `B` 二进制帧（复用 `spaceknife_protocol.binary`，流名 `mft`，布局 `mft.v1`，名字 blob `names`）。消息：`hello {token, pid, elevated}`、`progress {phase: scan|send, elapsed}`（扫描时每秒一次心跳）、`begin {rows, frames}`、`response {result, extras}`、`error {code, message}`。
- 白名单操作：`ping`、`volume_info(volume)`、`scan_mft(volume, batch_rows, duty)`（读并解析 $MFT，分批发回各列）、`shutdown`；仅非提权助手开放的测试操作 `scan_mft_image(path, duty)`（读 NTFS 卷镜像）。参数严格校验：卷只能是 `X:` 形式的本地 NTFS 卷。M5 加 `read_usn(volume, from)`。
- 错误码：`needs_admin`、`forbidden`、`invalid_request`、`not_supported`、`not_ntfs`、`io_error`、`unavailable`、`bad_frame`、`internal`（操作失败，助手仍可用）；连接层 `HelperUnavailable` 的 code：`exited`、`timeout`、`auth`、`protocol`、`closed`、`not_supported`、`denied`。
- 测试：自动化测试**绝不**触发 UAC。助手用普通权限运行（这台开发机本身已提权，测试用 Safer NORMALUSER 令牌启动助手），`scan_mft` 在非提权时返回 `needs_admin`；UAC 路径只用替身测试参数构造和取消映射。Linux/macOS 上暂不实现提权（`elevate=True` 返回 `not_supported`），unix socket 通道已有测试。
- 吞吐：900 万行（约 850 MB）经管道传输与拼装约 0.5 s（1.7 GB/s）；客户端直接拼进按 `begin` 预分配的表。

## 4. IO 动态限速（`spaceknife/io/`）与扫描调度

### 4.1 采样与指标

- 每块物理磁盘一个 `IoGovernor`（`IoRegistry` 按卷 → 磁盘映射），应用启动即采样（W2-40）：
  - Windows：`IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` 找物理磁盘，PDH / 原始磁盘计数器取忙碌度与队列深度，不需要管理员。
  - Linux：`/sys/dev/block/<maj>:<min>` 找到整盘，`/sys/block/<disk>/stat` 的 io_ticks 与 time_in_queue。
  - macOS：IOKit 块存储统计（拿不到时为开环）。
- 负载 `rho = min(busy, 平均队列深度 / Q_eff)`，`Q_eff` 先验 HDD 1 / SATA SSD 8 / NVMe 32（介质来自 `scan.media`）。对 NVMe 来说，任务管理器的「活动时间」只要有一个请求在途就算忙，几个并发读就会显示 100%，而盘远未饱和；`rho` 按队列深度折算，更接近「还能让出多少给别的程序」。
- 自身占用 = 扫描期间的 `rho` 减去基线（其他程序）；基线在扫描前测，扫描中定期短暂停发派（探测，间隔 8 s 起按需翻倍到 32 s）重测。

### 4.2 控制

- 每 0.5 s 一次：AIMD 调节允许的并发 + 占空比闸门（派发前 `await governor.permit()`）；超过上限快速减半，低于上限每 0.5 s 最多加一个；最低保持 5% 占空比（「最低进度」）。
- 「整盘繁忙时让路」打开时，整盘超过上限就降到最低进度；「空闲」档在其他程序读盘时（> 10%）暂缓。
- 档位：极速（不限）· 均衡 70% · 安静 35% · 空闲（系统低 I/O 优先级 + 让路）· 自定义 N%。扫描中可随时改档（`scan.set_load`），下一个控制周期生效。
- 系统 I/O 优先级：Windows 进程后台模式（`PROCESS_MODE_BACKGROUND_BEGIN`）；Linux `ioprio_set(IOPRIO_CLASS_IDLE)`；macOS `setiopolicy_np(IOPOL_THROTTLE)`。
- 上报（2 Hz，`io` 主题 / `io.load` 事件 / `io.status`）：`{disk, level, limit, self, total, concurrency, auto, yielding, history[60]}`，W2 曲线与状态栏同源。

### 4.3 扫描调度（`server/scans.py`）

- 扫描方式 `auto`：Windows 上「整个本地 NTFS 卷」且允许提权（设置 `scan.elevate`，或已有管理员权限 / 活着的已提权助手，不需要弹窗）时用 MFT，其他情况硬扫描。`mft` 强制尝试 MFT（失败回退硬扫描并说明原因），`hard` 强制硬扫描。
- MFT 读取是一次短时的顺序突发，用占空比限速：`duty` = 极速 1.0 · 空闲 0.2 · 其他档位 上限/100（最低 0.05）。
- 硬扫描中途「以管理员身份启用 MFT 快速扫描」（`scan.use_mft`，W2-24/25）：硬扫描继续，助手就绪后整树替换；被拒绝则继续硬扫描。
- 暂停/继续（`scan.pause` / `scan.resume`）：停止派发新批次（在途的完成即止），1 秒内自身读盘归零；继续时从队列断点接着派发。MFT 读取阶段暂停按钮不可用（突发很短）。
- 快照记录扫描方式（`stats.method`，`SnapshotInfo.method`）；M4 之前的快照没有这个字段，均为硬扫描。

## 5. 验收（来自 PLAN M4）

| 项 | 目标 | 结果 |
|---|---|---|
| C: 级卷 MFT 扫描 | < 10 s | 安静档（占空比 0.35）：C: 837 万项 22.1 s，F: 107 万项 9.3 s；按约定只做了短时低强度的实测，满速（占空比 1.0）未测 |
| 内存 | — | C: 助手峰值 1.6 GB（流式），后端建树峰值见 [memory.md](memory.md) §6 |
| 限速 | 稳态 5 s 均值 ≤ 上限，单点超出 ≤ 5 个百分点，改档 3 s 内收敛 | 控制器与采样的单元测试覆盖收敛与让路逻辑；按约定没有在用户磁盘上做长时间压测，实盘只做了短时检查（38 万项的测试目录，安静档，自身 2% / 整盘 9%，限速未触发） |
| 原生与纯 Python 一致 | 逐列一致 | MFT（含流式与随机树）、硬扫描（三平台中 Windows 与 Linux）均有逐列比对测试；没有 Rust 工具链时测试照常通过 |
