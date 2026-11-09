# M6：管理与分析 —— 设计与接口约定

本文是 M6 各部分（删除篮与文件操作、快照对比、模板导出与命令行、洞察规则库、文件类别与设置导入导出、协议与界面）之间的接口契约，按实际实现更新。改接口时先改本文。界面需求见 [UI-SPEC.md](UI-SPEC.md) 的 W5 / W6 / W7。

## 1. 总体

```
 前端（W5 右键菜单 / 删除篮抽屉，W6 对比模式，W7 设置）
   │ WebSocket（path.forms / basket.* / ops.* / compare.* / export.* / classes.* / settings.* …）
 后端（crates/sk-core/src/，服务层在 crates/sk-server/src/）
   ops/        删除篮（后端唯一持有）· 受保护路径 · 可释放空间 · 占用检测 · 回收站 / 永久删除 / 撤销 · 路径格式
   diff/       两棵树按路径合并：两端大小、变化量、状态，子树增长与减少，变化单元排行
   export/     SpaceSniffer 模板语言 + CSV / JSON / HTML；命令行 spaceknife-cli run（sk-server 的 cli/run.rs）
   insights/   已知应用目录的规则库（TOML）：变化原因注释、洞察
   classes.rs  可编辑的文件类别表（W7）；settings/io.rs 设置导出导入（含 SpaceSniffer 配置）
   sk-server   ops_service.rs · compares.rs · handlers/*.rs：把上面这些接到协议上
```

下面各节的签名沿用最初 Python 实现的写法记录；Rust 实现里的函数名、字段名与之相同。

- 文件操作只在后端执行；前端只发「要做什么」，后端逐项给结果（错误码 + 中文原因）。执行后由文件变化钩子（M5）更新树；该扫描没有实时监控时，后端对受影响的父目录做定点重读。
- 所有面向用户的文字用中文，每个模块集中在一个 `TEXT` / `MESSAGES` 字典里（M7.5 国际化时抽取）。
- 不在自动化测试里碰用户的真实文件：只在临时目录里建、删、移入回收站，测试结束前还原自己放进回收站的条目；从不清空回收站。

## 2. 删除篮与文件操作（`crates/sk-core/src/ops/`、`crates/sk-server/src/ops_service.rs`）

### 2.1 路径格式（`ops/paths.rs`）

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

### 2.2 删除篮（`ops/basket.rs`）

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

### 2.3 占用检测与回收站（`ops/locks.rs`、`ops/trash.rs`）

```python
def find_lockers(paths) -> dict[path, list[程序名]]        # Windows Restart Manager；Linux /proc/*/fd；macOS lsof
def trash_status(path) -> TrashStatus(available, reason, location, used, capacity, same_volume)
def move_to_trash(paths, *, progress=None, cancel=None, sizes=None, longest=None, bytes=None, check_locks=True, roots=None) -> list[OpResult]
def restore(refs) -> list[OpResult]                      # 不覆盖已有项（exists）
def delete_permanently(paths, *, progress=None, cancel=None, check_locks=True, roots=None) -> list[OpResult]   # 多线程，见 2.3.1
OpResult(path, ok, code: ok|missing|denied|locked|protected|no_trash|exists|cancelled|ambiguous_name|error, message, trash_ref)
```

- **移到回收站从不变成永久删除**：卷没有回收站、超过回收站容量、路径太长时逐项返回 `no_trash`，原样不动。Windows 上（0.3.5 起）回收站是本用户自己的文件夹（`$Recycle.Bin` 和 `$Recycle.Bin\<SID>` 都是真文件夹、不是重解析点，`<SID>` 的属主是本用户，即 shell 建的那个；`$Recycle.Bin` 的属主是本用户或系统——SYSTEM、Administrators、TrustedInstaller：别的用户在数据卷上先建了它时，它可以在检查之后、改名之前把本用户的文件夹换成链接，这样的卷交给 shell）、这一项也放得下（回收站已有的加上它不超过容量）时由后端自己放进去，与 shell 的做法相同：先以一个没人用的名字写下 `$I` 记录（Windows 10 的版本 2：大小、删除时间、原路径，8.3 短名展开），再把这一项改名成旁边的 `$R`——只改名，不覆盖已有的、不跨卷复制，改名失败就删掉刚写的 `$I`，原样不动。一项属于它所在文件夹的卷（链接——联接点、符号链接——也是：改名移动的是链接本身，指向别的卷也一样；按链接自己的路径问卷会得到目标的卷）。改名不可能销毁任何东西；文件夹里有句柄开着时改名整个失败（ERROR_ACCESS_DENIED，不管对方是否允许删除），这时才问是谁占着（`locked`），说不清时交给 shell；项本身不在了才报 `missing`（找不到的是回收站文件夹时交给 shell）。放不下的（shell 会按用户的设置删掉回收站里最旧的项腾地方，这一步留给它）、回收站文件夹不是自己的（FAT 没有属主）或还没建的、不是链接的重解析点或数据不在本地的项（OneDrive 等云文件的占位符：离开同步文件夹时由它的过滤驱动决定怎么办，可能先下载；脱机、访问时才取回的文件），仍用 IFileOperation（FOFX_RECYCLEONDELETE），与 0.3.4 相同，进度回调拒绝任何不进回收站的删除。`$I` 里的大小用扫描给的逻辑大小（`bytes`），没有时用量出来的。回收站已有多少（`trash_status` 的 `used`、上面的容量判断）按各项的 `$I` 记录相加：只列一次回收站文件夹、每个记录读一次（之后按路径、长度、时间记住），不再用会走遍回收站里每个文件夹的 `SHQueryRecycleBin`。Linux 按 freedesktop Trash 规范（`renameat2(RENAME_NOREPLACE)`，跨设备返回 `no_trash`，从不复制）：每个设备的回收站一次执行只找一次、目录只建一次（同一设备的另一个挂载点——bind mount——上的项不在找到的那个顶层目录下，为它另找）；移进去的文件夹把扫描给的大小写进 `$trash/directorysizes`（规范 1.0：大小、`.trashinfo` 的 mtime、百分号编码的名字，先写临时文件再改名），算回收站占用时文件夹按这一行算（`.trashinfo` 的 mtime 没变时），没有或已变的才走一遍再写回，不在回收站里的行去掉。macOS 用 `NSFileManager trashItemAtURL`，逐项。
- 执行前逐项复核：仍存在、仍不受保护、未被占用。Windows 移到回收站时：交给 shell 的项在交给它之前查；自己改名的，文件夹里开着的句柄由改名整个失败查出（见上），改名照样会成功的才先查——文件照旧问 Restart Manager（别的程序开着它但允许删除时 NTFS 照样改名），文件夹与正在运行的程序的路径比较（运行中的程序的映像映射着、没有句柄开着，文件夹照样改名；只比较进程的映像路径，不走文件夹、不读盘；别的程序从文件夹里加载的 DLL 看不到）。各项的检查按线程数同时做（2.3.1），大小写的检查同一次执行里每个文件夹只列一次（一个小项聚合块的成员都在同一个文件夹里）。Windows 查占用时把要问的文件分成几份、同时开几个 Restart Manager 会话问「有没有人占着」（它的时间跟一个会话里登记的文件数走：2 万个文件一个会话 2.5 s，四个 0.5 s），只有问出有人占着的那一份再按路径二分找是哪几项。
- 只差大小写的同名项（0.3.4，docs/native.md 2.2）：`roots` 给出每个路径所在的扫描根（删除篮加入时就为每一项记下，`ops_service` 执行时传入，扫描已经关闭也照样核对）。Windows 上逐项按路径打开（不要任何权限、不跟随重解析点）读文件系统存的名字（`FileNormalizedNameInfo`，不支持时 `FileNameInfo`），与扫描根自己存的名字接上路径在根以下的各级比较（`worker.task_is`，同一次执行里每个根只读一次）。完全相同就照常执行。不同时，从扫描根往下逐级列出路径经过的文件夹：某一级里既有路径的这个名字、又有一个只差大小写的别的名字，就说明按这个路径可能打开另一项（WinBtrfs 上 `a` 的路径打开的是 `A`，`A` 是符号链接时还会跟着它到别处；NTFS 上不带区分大小写标志的文件夹里 ntfs-3g 写下的 POSIX 名字同理），返回 `ambiguous_name`（「另一项与它只差大小写，按名字会打开另一项；没有改动任何东西」），原样不动；哪一级列不出也同样拒绝。路上没有这样的一对时照常执行：扫描之后只改了大小写（例如 `Photos` 改成 `photos`）、网络盘上存的名字是最先打开它的那个程序用的大小写、存的名字在别处（硬链接的另一个名字、重定向），都不误拦。旧快照、对比视图、Python 遍历器扫出的树、只差大小写的文件都由这一条兜住。Windows 上路径里有名字以点或空格结尾的也拒绝（`ambiguous_name`，「它或上级文件夹的名字以点或空格结尾，Windows 按路径找到的不是它；没有改动任何东西」）：Win32 会去掉结尾的点和空格，`odd.` 的路径删的是旁边的 `odd`，而扫描按 NT 路径（worker 按 `\\?\` 路径）把两个都照原样列出。删除篮加入时拒绝没有路径单独指向的项：名字（或扫描根以下某一级上级的名字）里有路径分隔符（Windows 上 `\` 和 `/`，WinBtrfs 照原样显示 Linux 的文件名；POSIX 上 `\` 是普通字符）、与同一文件夹里另一项的名字完全相同（WinBtrfs 把无效的 UTF-8 显示成 U+FFFD：扫描按 ID 列出各自的内容，按路径删的却是其中一个），以及 Windows 上以点或空格结尾。
- 永久删除文件夹（`ops/remove.rs`）：里面的项从不靠「列出的名字接到文件夹路径后面再按路径找」。列出的名字为空、是 `.` / `..`、含 NUL 或路径分隔符（Windows 上还有 `\`、`/` 和流分隔符 `:`）的不拼接（「其中有名字里带路径分隔符的项，按路径找不到它，未删除」）：WinBtrfs 照原样列出 Linux 上的 `..\x`，拼出来就在选中的文件夹外面。POSIX 上一律相对父目录的描述符操作（`fstatat`、带 `O_NOFOLLOW` 的 `openat`、`unlinkat`），打开的文件夹必须是 `fstatat` 看到的那一个，且与被删文件夹在同一个挂载上（Linux 比 `statx` 的 mount id，同一文件系统的 bind mount 也认得出；没有时查 `/proc/self/mountinfo`）。描述符只有被删的文件夹和最深的 32 级一直开着（每级一个的话，几千级深的树会半路用光：Linux 默认 1024 个，macOS 的程序 256 个）；往下走时关掉的上级，往回走时用下一级的 `..` 打开，不行再从最近一个开着的级别按名字逐级打开，每一级都必须仍是原来那个文件夹（设备号和 inode），否则这一级和它剩下的内容都不动（`moved_inside`）。Windows 上每一项按完整的 `\\?\` 路径、不跟随重解析点打开，在句柄上核对同一个卷、打开后的名字等于父文件夹的名字加列出的名字（途中被换成链接就对不上），再通过这个句柄删除；名字只差大小写时（SMB 重定向器、WinFsp、Dokan 报告的是最先打开它的程序用的大小写）不按名字算数，必须是父文件夹按列出的名字列着的那一项：同一个卷、文件 ID（`FileIdBothDirectoryInformation`，通过核对过是这个文件夹的句柄列出）等于句柄的文件索引，区分大小写的文件夹里只差大小写的另一项、移进来的别的文件夹都对不上；文件夹通过核对过是同一个的句柄列出。被换成链接或别的项时不进入、不删（「删除过程中其中一项被换成了别的项或链接，未进入、未删除」）；链接只删链接本身；另一个卷的挂载点（Windows 的文件夹挂载点、Linux 的 bind mount）不进入、不移除（`mount_inside`）。每个文件少一次调用（0.3.5）：Windows 上属性随身份（卷、文件索引）一次取到，只有重解析点再取它的标记；POSIX 上列目录时 `d_type` 说不是文件夹的项直接 `unlinkat`（不带 `AT_REMOVEDIR`：不会删文件夹、不会跟随链接；这期间变成了文件夹的，EISDIR / EPERM 之后再照常看一遍）。POSIX 上从父文件夹按名字删一个文件夹之前，核对这个名字还是它（设备号和 inode），别的文件夹占了这个名字时不删。多线程见 2.3.1。
- 删除篮按路径的键比较（Windows 上不分大小写），但同一个文件夹里两个名字比较起来相同（大小写双胞胎、显示成同样 U+FFFD 的名字）时，这一级的名字按原样写进键：两项各是各的，下面的项只归各自那一项。执行结果按目标（而不是显示的路径）对回删除篮里的项。
- 回收站引用留在后端，名字不是合法 Unicode 时逐单元记下（`ref_of`）；还原按 `$I` 记录 / `.trashinfo` 里原样的路径（孤立代理项、非法 UTF-8 字节都不变），名字不会变成 U+FFFD。

#### 2.3.1 并发与读盘（0.3.5，`ops/par.rs`、`ops/remove.rs`）

用户报告：删除篮移到回收站、永久删除都慢（普通盘上，性能差的 U 盘上更慢），而且一直在读盘。先量后改（`ops::probe`，见下），查到的原因：

| 原因 | 量到的 |
|---|---|
| Windows 上 shell 的回收引擎（IFileOperation）把交给它的每个文件夹、还有回收站本身都走一遍 | 单个文件每项 7–14 ms（越往后越慢），文件夹里每个文件约 1 ms；一个 2 万个文件的文件夹进程读 2.6 万次、其他 I/O 30 万次；批大小从 64 改到 256 几乎不变 |
| `SHQueryRecycleBin`（回收站占用，删除篮抽屉每次刷新都要）走遍回收站里每个文件夹 | 回收站里有一个 3.3 万个文件的文件夹时，每问一次其他 I/O 6 万次 |
| 执行前查占用：Restart Manager 一个会话里每个文件约 0.2 ms，它会把每个文件都打开一遍 | 2 万个文件 3.9 s；删除篮每加一项又把整个篮子查一遍 |
| 永久删除一个线程一项一项、一个文件一个文件地删 | 每个文件 5 次句柄调用（Windows），NVMe 上本可以同时做 |
| Linux 回收站占用每次走遍 `files/`（最多 10 万项），移进去的每一项都重新找一次回收站、建一次目录 | |

**线程数**（`par::workers_for`）：按卷的介质（`crate::media`，扫描用的同一个探测）——NVMe 8，SATA SSD、USB SSD、虚拟盘、网络位置 4，机械盘、USB 机械盘、U 盘、SD 卡、不知道的 2，再不超过 CPU 数；几个卷时取最少的（Windows 按盘符分卷，其他系统按项所在文件夹的设备号：`/media/lenny/NVME` 和 `/media/lenny/STICK` 是两个卷）。删除不走扫描的磁盘负载调节器（用户发起的前台操作；调节器只管后台扫描）。测试用 `par::seams::WORKERS` 定死。

**永久删除**（`remove::remove_trees`）：每一项、每个文件夹（有线程闲着时，找到的子文件夹交给它）、名字多的文件夹借出去的一半名字（剩下多于 64 个、有线程闲着时）都是任务。每个文件夹记着里面还剩几件事（自己的名字、没删掉的子文件夹、借出去的名字），减到 0 时由做完最后一件事的线程删掉它（Windows：先通过它的句柄删、马上关掉句柄），然后它的上级少一件事：总是先删里面的、再删文件夹，一个文件夹的句柄在上级被删之前已经关了（不支持 POSIX 语义的 FAT/exFAT 上删除在关句柄时才生效）。每一步都是一个线程时的那一步：上面那些检查在每一项打开时照做。POSIX 上只有上面 4 层（`remove::PAR_DEPTH`）由几个线程做，更深的文件夹由遇到它的线程按一个线程的办法删完（开着的描述符：每个线程 `32 / 线程数`（至少 4）级，加上上面 4 层的，macOS 的 256 个也够）；测试设了开着的级数时整项按一个线程的办法删（那些测试讲的就是一个线程往下走的路）。互相包含的项（删除篮里不会有，调用允许）里面的先删完、外面的才开始。取消在每一项开始前、每进一个文件夹前问；说了停以后哪个线程都不再开始新的事，也不再删文件夹（正在做的那一项做完），没做完的项报「已取消」（删了一部分时「已取消，其中一部分已删除」）。进度在每一项完成时报，结果仍按输入的顺序。

**移到回收站**：见上一条；一项一项地改名（8 个线程同时往同一个文件夹改名每次 6.3 ms，一个一个 0.12 ms），改完后按原来所在的文件夹、回收站文件夹各通知 Explorer 一次（`SHCNE_UPDATEDIR`；逐项通知每项 0.7 ms），再刷新回收站图标。改名之前查改名查不出的占用（2.3 复核那一条：文件问 Restart Manager，文件夹比较运行中的程序的路径）：下表里 2000 个小项多 0.09 s（0.50 → 0.58 s，进程读不变，其他 I/O 多 2.3 万次，是 Restart Manager 打开这些文件），2 万个文件的树多约 0.01 s（不读盘），同样交替各 3 次。

**怎么量**（只在基准里编进去）：`sk-core` 的 `probe` 特性打开 `ops::probe`——各处文件操作数自己的调用（stat、打开、查询、列目录、删除、改名、写、shell 操作、Restart Manager 会话和登记的文件），记各阶段时间；`SPACEKNIFE_OPS_DEVICE=<微秒>[,<队列深度>]` 让每个数到的调用先等一个模拟设备（一次只服务 `队列深度` 个，每个占多少微秒：一次只做一件事的 U 盘）。不打开时这些都是空的内联函数，发布版里没有。`cargo run --release -p sk-core --features probe --example ops_bench -- <自己建的临时文件夹> <形状> <trash|delete>` 在那里建测试树（`tree:N` 一个文件夹 N 个小文件分在嵌套的文件夹里加 4 个 16 MB，`flat:N` 一个文件夹直接装 N 个，`group:N` 一个文件夹的 N 个文件作小项聚合块，`items:K:N` K 个文件夹项）、扫描、放进删除篮、照后端的方式执行，输出一行 JSON：时间、各阶段、数到的调用、进程的 I/O 次数；移到回收站之后只把这次放进去的（`$I` 记的原路径在那个临时文件夹下面的）`$I`/`$R` 从回收站拿掉。

**实测**（2026-11-09，开发机：Windows 11，C: 在 NVMe 上，热缓存；「之前」是 dev 3150111 的 ops 代码配同一个基准程序，前后两个版本交替各跑 3 次取中位。「进程 I/O」是本进程 `GetProcessIoCounters` 的读次数 / 读写以外的 I/O 次数（打开、查询、列目录……shell 引擎在本进程里做的也算），与设备无关；「物理读」是 C: 所在物理盘在执行期间的读次数（`IOCTL_DISK_PERFORMANCE`，整盘的，别的进程同时读的也在内，热缓存下大多不到盘上））：

| 测试树（自己建在临时文件夹里） | 操作 | 之前 | 之后 | 进程读 / 其他 I/O：之前 → 之后 | 物理读（中位） |
|---|---|---|---|---|---|
| 一个文件夹，2 万个小文件分在 200 个文件夹里，另有 4 × 16 MB | 移到回收站 | 4.37 s | 0.20 s | 26 609 / 316 276 → 10 / 62 450 | 37 → 1 |
| 同上 | 永久删除 | 2.00 s | 0.59 s | 0 / 159 411 → 0 / 139 574 | 51 → 25 |
| 一个文件夹，10 万个小文件 | 移到回收站 | 13.3 s | 0.89 s | 26 609 / 1 470 603 → 10 / 308 611 | 404 → 6 |
| 同上 | 永久删除 | 8.11 s | 3.75 s | 0 / 730 853 → 0 / 630 136 | 320 → 106 |
| 一个文件夹里 2000 个小文件作小项聚合块 | 移到回收站 | 26.7 s | 0.59 s | 40 127 / 779 348 → 10 / 65 380 | 1567 → 23 |
| 同上 | 永久删除 | 0.45 s | 0.21 s | 0 / 62 008 → 0 / 62 426 | 1 → 1 |

第一轮（同样交替 3 次，`之后` 是还没减少每个文件调用次数、仍用 `SHQueryRecycleBin` 的版本）另有 20 个文件夹项共 2 万个文件：移到回收站 31.0 s → 0.20 s（进程读 507 378 → 1），永久删除 4.37 s → 1.21 s。剩下的「其他 I/O」几乎都是改名进回收站本身引起的：同一台机器上把 2 万 / 10 万个文件的文件夹改名到别的文件夹只有 43 次、5–100 ms，改名进 `$Recycle.Bin` 却是每个文件约 3 次、0.15 / 0.9–2.2 s——系统对进回收站的项还要看一遍（过滤驱动之类），不是我们的调用；shell 的引擎同样要付这一份，还要加上它自己的走遍。

模拟的慢设备（`SPACEKNIFE_OPS_DEVICE`，只加在我们自己数到的调用上；「之前」是 3150111 加上同样的计数）：5000 个文件的树永久删除，每个调用 0.3 ms、一次只做一个（USB 2 的 U 盘那样）8.77 s → 6.46 s（调用 25 523 → 20 470，少的是每个文件的一次查询和执行前查占用时列的目录）；一次做 4 个（UAS 的 USB SSD 那样）9.06 s → 1.71 s。一次只做一个的设备上线程不帮忙，快的部分全来自少做的事；回收站在这样的设备上（USB 机械盘、USB SSD 是「固定磁盘」，有回收站）快得最多：之前 shell 要把整个文件夹和回收站走一遍，之后是写一个 `$I`、改一次名。

Linux（WSL 2 的 ext4，热缓存，同样交替 3 次）：永久删除 2 万个文件的树 0.163 s → 0.087 s、10 万个 0.737 s → 0.382 s（POSIX 上每个文件从 `fstatat` + `unlinkat` 两次调用变成一次：10 万个文件的树之后只有 2203 次 `fstatat`，都是文件夹和执行前的检查）；2000 个小项移到回收站 0.054 s → 0.044 s；把一个 2 万个文件的文件夹移进回收站之后算回收站占用 0.018 s → 0.000 s（`directorysizes`；之前每次都走遍回收站，最多 10 万项）。真实的慢盘、U 盘、Mac 上没有测（见下）。

**没在真机上验证的**：USB 3（UAS）与 USB 2（BOT）的 U 盘、USB 机械盘上的实际时间（这里只有模拟的设备和与设备无关的调用 / I/O 次数）；FAT32 / exFAT 的 U 盘上永久删除只问一次 POSIX 语义就记住不支持（每个文件少一次调用）；macOS 上多线程永久删除（描述符上限 256）和 `NSFileManager` 回收；Explorer 的回收站窗口对我们放进去的项「还原」（这里用 `Shell.Application` 的回收站文件夹核对过：它看得到这两项、名字和原位置都对，`restore` 还原正常）；OneDrive「按需文件」的仅在线文件和文件夹移到回收站（这里交给 shell，与 0.3.4 相同，没有 OneDrive 可试）；Linux 上同一文件系统的两个 bind mount 里的项一起移到回收站（各进各自顶层目录的 `.Trash-$uid`，需要 root）。

还可以再做（这次没做）：没有实时监控的扫描执行完后对父文件夹整个重读（`after`，只该重读一层，需要监控那边给一个只列一层的接口）；实时监控时删除过程中被清空的文件夹会被反复列（`monitor::pipeline` 的合并），删除期间可以暂停它对被删路径的处理。

### 2.4 服务层（`crates/sk-server/src/ops_service.rs`）

- 删除篮每个后端一份，所有会话共享，变化推 `basket.changed`；状态里每个卷带 `trash_status`（回收站说明、当前占用）。
- `basket.execute {mode, ids}` → 任务 id；`ops.progress` 推给发起的会话（`mode`、进度、结束时逐项结果、`freed`、`can_undo`）。界面发它让用户确认的那些项：`confirm` 是每一项的 `id` 和确认时看到的 `rev`（basket-confirm.v1；删除篮所有会话共享，对话框打开之后别的会话加进来的不算在内）。`rev` 在一项覆盖的范围变大时变（小项聚合块加了成员，即使同时少了一个、成员数不变），重新加入的项也是新的 `rev`；查锁、树变化后的重新核对不改它。执行时在登记任务的同一把锁下逐项核对，有一项离开了删除篮或 `rev` 不同，就什么都不执行，返回 BASKET_CHANGED（-32014），界面按现在的内容再问一次；界面发出之前自己也比一次。旧客户端照样可以发 `ids`（只执行这些）或都不发（全部），与 0.3.4 相同。同一时间只有一个操作：忙检查与登记在同一把锁下，两个同时的请求只有一个开始。
- 永久删除先等 `DELETE_DELAY`（5 秒）：期间 `ops.undo` / `ops.cancel` 直接作废，什么都没碰。移到回收站完成后（或中途停下时已经移进去的部分）`ops.undo` 用 `restore` 放回原处并把条目放回删除篮。后端保留最近 8 个任务的撤销信息。
- 加入后在后台检查新加入的那几项的占用（Restart Manager 等；原来每加一次就把整个删除篮的文件问一遍、读一遍，检查进行中再加的项还会漏掉，现在排队由同一个检查接着问），树变化（监控事件）后 1 秒静默再 `refresh`。
- 对比视图里的节点按路径映射到「现在」的树再加入；已删除的项、目标为历史快照时拒绝（「只能操作「现在」还在磁盘上的项」）。

## 3. 快照对比（`crates/sk-core/src/diff/`、`crates/sk-server/src/compares.rs`）

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

## 4. 模板导出与命令行（`crates/sk-core/src/export/`、`crates/sk-server/src/cli/run.rs`）

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
- 命令行 `spaceknife-cli run <命令序列>`（实现在 `crates/sk-server/src/cli/run.rs`）：`scan`、`load`（.sks）、`filter`、`save`（.sks）、`export`、`autoclose`、`help`，元命令 `@delim=`、`@parser=OS`。只做硬扫描，不提权、不受负载上限约束；`:class:` 使用用户编辑过的类别表，标签条件不可用。
- 界面：命令面板「导出报告…」→ 选模板或格式、范围（当前位置 / 整个扫描）、是否只导出过滤结果 → `export.run`。

## 5. 洞察规则库（`crates/sk-core/src/insights/`）

规则格式见 [insights.md](insights.md)。

```python
Insight(rule, title, reason, kind, note="", app="", cleanup="review", command="", path="", inside=False)
def match(path, kind, *, size=None, lang=None) -> Insight | None
def match_node(store, node, *, lang=None) -> Insight | None
def load_rules(extra_dir=None, *, host=None) -> RuleSet       # 内置 110 条 + 用户目录 config/insights/*.toml
```

- 规则用 TOML；W6 的「变化原因注释」取 `note`（如「游戏更新」）。

## 6. 文件类别与设置导入导出（W7）

- `classes.rs`：用户可编辑的类别表存在 `config/classes.json`，启动时成为「当前类别表」（`classes.active()`）；过滤引擎、布局着色、解析词汇都读它。`classes.build(edits)` 校验：类别名唯一且不含 `; : " \`；扩展名用 `;`、空格或逗号分隔，去掉开头的点、不分大小写；「其他」不能删、不能填扩展名、始终最后；内置类别改名后旧名字留作别名（`:class:视频` 仍可用）。类别可带颜色（`#RRGGBB`，空为主题色），深色、浅色主题各一个：`color` 和 `color_light`；为空时内置类别用当前主题的颜色，自建类别用另一主题的颜色。`classes.json` 是第 2 版；读到第 1 版（或没有版本号）时，`color` 同时当作 `color_light`：那时只有深色主题，用户选的颜色两套主题都用。
- `settings/io.rs`：导出 `{"format": "spaceknife-settings", "version": 1, "values": 非默认值, "classes": 自定义类别表或 null}`；导入先给出计划（将改变的项、新类别表、不导入的项及原因），用户确认后才应用。也读 SpaceSniffer 的 `SpaceSnifferConfig.xml`（每个设置一个元素；颜色 `0x00BBGGRR`；模板为 Base64 的 UTF-16LE）：能对应的设置照搬或换算（字号按 7pt=100%，边框对比按 30=10），配色、刷新间隔、缩放时长、DynaZoom、元素比例不导入；文件组按 SpaceSniffer 的名字和扩展名成为类别，内置类别保留 id，SpaceSniffer 默认的三原色改用主题色，用户自选的颜色保留（两个主题都用它）；SpaceSniffer 没有的「文档」类别保留。

## 7. 协议

| 方法 / 事件 | 说明 |
|---|---|
| `path.forms {scan_id, node}` | 四种格式 + `on_disk`（对比视图里同样可用） |
| `basket.list / add / remove / clear / execute`，事件 `basket.changed` | `add {scan_id, nodes}` 或 `{view_id, key}`（聚合块的成员），带 `generation`（读到这些 id 的画面、结果页或摘要的编号，node-generation.v1）：树在这之后重新编号过就拒绝（UNKNOWN_NODE，「树刚刚重新编号过，请在新的画面里再选一次」）；按块加入时以画面自己的编号为准，客户端不说也核对 |
| `ops.cancel / undo`，事件 `ops.progress` | 见 2.4 |
| `compare.sources / open / info / close / summary / node`，事件 `compare.progress` | 见 3.1 |
| `export.targets / check / run` | 见 4 |
| `classes.get / update / reset`，事件 `classes.changed` | 见 6 |
| `settings.export / import` | 见 6 |
| `system.maintenance / helper_stop / cache_clear`、`tags.clear_saved` | 「高级」页：扫描助手状态、缓存与快照占用、清除已保存的标签 |
