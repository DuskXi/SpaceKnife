# TODO

离最终发布还没做完的事。里程碑定义见 [docs/PLAN.md](docs/PLAN.md) 第 8 节，界面需求见 [docs/UI-SPEC.md](docs/UI-SPEC.md)。完成一项就勾掉，挂起的写明原因和恢复条件。

## 挂起

- **M7 原生 UI**（2026-10-18 起挂起，暂不做）
  - spaceknife-client 共享视图层
  - Qt（PySide6）与 WinUI3（win32more）各自实现全部界面（UI-SPEC 512 条需求），两者逻辑与视觉一致
  - 两端各实现三种可视化组件：`drawlist.v1`、`flame.v1`、`polar.v1`（几何、标签、缝、命中和配色按 [docs/visuals.md](docs/visuals.md)），并通过同一套一致性测试
  - 三端 golden 截图对比（含火焰图和火焰-极坐标图）
  - 受影响的后续项：M8 里 Qt / WinUI3 的 PyInstaller 打包和 Windows App Runtime 安装一并挂起
  - 恢复条件：用户决定恢复

## macOS（2026-10-19 恢复）

- [x] dmg 打包：arm64 与 x64 各在对应架构的 Mac 上构建（GitHub Actions「macOS build」，手动触发），ad-hoc 签名、未公证；README 写明第一次打开的放行步骤
- [x] 实机试用（Intel Mac，2026-10-19）：放行流程可用；发现并修好了数据卷重复计数（进度卡 99%）、扫描期间被删文件夹导致的扫描失败、磁盘负载测不到、云盘触发下载、逐个弹权限，并用 getattrlistbulk 提速
- [ ] 再次实机验证（Intel Mac）：上面这些修复、FSEvents 监控（数据卷路径经 firmlink 映射）、回收站（`NSFileManager`）、标题栏与菜单
- [ ] Developer ID 签名与公证（等 Apple 开发者账号审核）：Hardened Runtime、权限声明、notarytool、CI 里用 Secrets 自动签名
- [ ] Homebrew cask（官方仓库要求通过 Gatekeeper，等公证）
- [ ] Mac App Store（可选）：沙盒下用户授权整盘，访问权要用隐式安全范围书签传给后端子进程，先做概念验证
- [x] 应用内引导开启「完全磁盘访问权限」（扫描前的说明框直接打开系统设置的对应页面；设置「扫描范围 › 受保护的位置」）

## M4 收尾

- [ ] 满速 MFT 扫描实测：验收「C: 级卷 < 10 s」。目前只有安静档的数字（C: 837 万项 22 s）；要在用户同意后做一次短时满速测试
- [ ] 限速精度实测：稳态 5 s 均值 ≤ 上限、单点超出 ≤ 5 个百分点、改档 3 s 内收敛
- [x] 负载口径（用户 2026-10-18 决定）：限速继续按「忙碌度 / 队列深度」折算的占用计算；任务管理器式的「活动时间」显示在旁边作参考（状态栏、扫描详情、负载曲线的点线）
- [x] 硬扫描结果归并向量化（`NodeStore.ingest_slice`）：每个列表 30–78 µs → 1–3 µs，瓶颈转到系统目录枚举
- [x] Linux 的提权助手（0.3.3）：pkexec + polkit，目前只给「本机磁盘」读磁盘自己的记录（`disk_health`），点按钮才请求；macOS 不需要（NVMe 的记录不提权就能读），`elevate=True` 仍返回 `not_supported`
- [ ] W2 扫描摘要卡（扫描中选中某块时，详情面板顶部的进度 + 档位 + 负载）
- [ ] 日志面板「扫描统计」页复用负载曲线与并发

## M5 文件变化钩子

- [x] Windows：ReadDirectoryChangesExW + USN 日志补偿（助手 `query_usn` / `read_usn`）
- [x] Linux：inotify（额度 + 轮询对账）；macOS：FSEvents（未实机验证）
- [ ] Linux fanotify（需要 Linux 提权助手）
- [x] 增量流水线：定点重扫、合并进节点表（relist、压缩、视图 id 重映射）
- [x] 树图元素闪烁（变化提示；减少动画时静态角标）
- [ ] 「快照后变化」
- [x] 已停止扫描的「继续扫描」（从停下的位置补扫）
- [x] F5「重新扫描当前视图」
- [x] 验收：并发写入压测无事件丢失，或溢出可检测并补扫（Windows 与 Linux）
- [ ] 长时间运行（10 万次变化）后的压缩与内存：未在用户盘上做长时间压测
- [x] W2 的「正在扫描」标示（UI-W2-04 / UI-TM-42）：视图根以下 3 层里还没扫完的文件夹画强调色呼吸描边

## M6 管理与分析

- [x] W5 统一右键菜单（键盘 Shift+F10 / 菜单键、结果列表行同样可用）、复制路径四种格式、删除篮（去重与吸收、可释放空间、硬链接与云占位、占用检测）、移到回收站与撤销、5 秒延迟的永久删除；MFT 伪目录里的项在后端拒绝
- [ ] 系统原生 Shell 菜单（右键菜单里的「系统菜单…」，Windows IContextMenu / Linux 文件管理器）
- [x] W6 快照对比：变化树（只保留有变化的项）、面积 = 变化量、增长 / 减少色阶与图例、「新增 / 已删除」角标、净增长与写满预测、增长 / 减少排行（变化单元）与原因注释、悬停卡、快照时间轴、交换两端、「现在」随文件变化更新
- [ ] W6 对比报告导出（「更多对比选项 › 导出对比报告」）；对比模式下分屏
- [x] W7 完整设置：文件类别编辑（拖动排序、取色、改名、扩展名、恢复默认）、标签显示名与清除已保存的标签、高级页（设置导出 / 导入、从 SpaceSniffer 导入、全部恢复默认、扫描助手、缓存与快照）、字号 / 层级对比 / 边框对比 / 元素比例 / 帧率 / DynaSmooth / 鼠标拖尾
- [ ] DynaZoom（缩放过程中实时重排）：需要后端按中间矩形逐帧布局，设置项先锁定
- [ ] 改键（`keys.preset` 与自定义键位，含 SpaceSniffer 经典键位）：设置页现在只列出当前键位
- [x] 模板导出（SpaceSniffer 模板语言、5 个内置模板、CSV / JSON / HTML）与命令面板「导出报告…」
- [ ] 自定义导出模板的保存与编辑（`export.check` 已有诊断；SpaceSniffer 配置里的自定义模板现在只列为「不导入」）
- [x] 命令行 `spaceknife run`（scan / load / filter / save / export / autoclose / help，@delim= / @parser=OS）
- [x] 洞察规则库（110 条内置规则，用户 TOML 规则），用于 W6 变化原因注释
- [ ] 洞察卡片（详情面板 UI-MAIN-41/42：这是什么、能不能清理、清理命令）
- [ ] 「预览」（Space，命令面板 item.preview）
- [ ] 验收：UI-SPEC W5、W6、W7 逐条勾选（上面未勾的项除外）

## M7.5 国际化（i18n）

- [x] 文案资源化：界面、日志与事件、设置项、命令面板、错误信息、导出报告、命令行（后端 + Electron，见 [docs/i18n.md](docs/i18n.md)）
- [x] 简体中文与英文两套资源（英文约 1,550 条，按区域分文件，术语表统一）
- [x] 首次启动按系统语言自动选择（Windows / Linux / macOS；Electron 把系统界面语言传给后端），设置里可切换、即时生效
- [x] 数字、大小单位、日期：两种语言同一格式；相对时间、时长与常见计数有英文单复数
- [x] 过滤语法：关键字只有一套 ASCII 写法；内置类别名两种语言都能用（英文界面显示 Video / Pictures …，中文名作别名）
- [x] 检查脚本 tools/i18n_check.py（含在测试里）：没有遗漏的硬编码文案、每条都有英文
- [ ] 英文复数仍有少数「1 items」式的说法（计数文案多，按需逐条加单数）

## M8 打包与发布（Windows 与 Linux）

- [x] 应用图标：用户选定「D 斜切」（`design/app-icon/spaceknife.svg`）；`tools/render_app_icons.py --app` 生成 Windows `.ico`（16–256，10 个尺寸）、Linux hicolor PNG（16–512）、窗口图标；NSIS 安装 / 卸载图标、`.desktop`；标题栏标识改成同一个图形
- [x] Electron 安装包：Windows NSIS；Linux deb / rpm / AppImage（`tools/build_package.py`，见 [docs/packaging.md](docs/packaging.md)）
- [x] 冻结后端：PyInstaller onedir，扫描助手与工作进程走同一个可执行文件；Linux 包的 glibc 下限 2.34 有检查
- [x] 冒烟测试：Windows 11 上 `win-unpacked`，WSL（Ubuntu 22.04）上 `linux-unpacked` 与 AppImage（`packaging/smoke_app.py`）
- [x] Windows 安装包本身：静默「仅当前用户」安装、冒烟、卸载，卸载后与安装前一致（Windows 11）
- [ ] 干净机器安装测试：Windows 10、RHEL 9、Ubuntu 22.04（装 / 运行 / 扫描 / 卸载；deb / rpm 还没实际安装过）
- [ ] Windows 代码签名：没有证书时先发未签名包（SmartScreen 会提示）
- [x] 许可证 MIT；安装包带第三方许可清单（Python、Rust、界面 JavaScript 与字体；`tools/third_party_licenses.py`）
- [x] 项目主页 `https://github.com/DuskXi/SpaceKnife`；发布包里不带构建机路径（原生扩展单独做发布构建，构建时检查）
- [ ] 包体积：只 strip 没被 auditwheel 改过的库（libpython、zstandard、原生扩展），Linux 后端约省 40 MB 解压后体积
- [x] 更新提示（2026-10-20）：每天最多一次检查 GitHub 发布；修订号只显示徽章，次版本号左上角挂卡片，主版本号弹对话框；一键下载对应安装方式的文件并按 SHA256SUMS 校验，Windows 安装包一键安装（docs/packaging.md「更新检查」）
- [ ] 设计图标集里加一个「下载」图标（现在用旋转 180° 的向上箭头）
- [x] **无缝更新与零残留卸载**（2026-10-21 用户决定，0.3.3 在 dev 上做完；端到端测试 `packaging/update_e2e.py` 在「Portable build」里跑）
  - **更新方式**：后台下载并按 SHA-256 校验，下载好后更新卡片给出两个大按钮：「无缝更新」（静默安装或换文件后重新打开，不弹安装向导）和经典方式（安装向导、磁盘映像、安装包，便携版和 AppImage 是自己替换）。用户点了才安装，扫描进行中要再点一次确认；从不自己安装，也不做全自动安装的选项。次版本 / 主版本还能「跳过 0.4.x」/「跳过 1.x」，整个系列不再提示（「关于与更新」里恢复）。
  - **下载位置**：更新文件放进数据目录下的 `updates`，不再放「下载」文件夹（现在每次更新都会在「下载」里留下一个 130 多 MB 的安装包）。装完即删，启动时清掉过期的。
  - **Windows 安装版**：运行新安装包时带 `/S`，安装范围和当前一致（`/currentuser` 或 `/allusers`；所有用户安装的会弹一次管理员确认），装完重新打开。
  - **便携版**：下好后解压到旁边，退出时替换程序文件，保留 `data`，再重新打开。
  - **AppImage**：退出时直接替换文件。
  - **deb / rpm**：不变，仍交给系统的软件安装器（以后可以做软件源）。
  - [ ] **macOS**：等公证之后再做（现在仍是打开磁盘映像，由用户拖进「应用程序」）。
  - **卸载**：
    - 总是删除：`%APPDATA%\SpaceKnife\electron`（界面缓存）、旧版本留下的 `%APPDATA%\spaceknife-electron`、`updates` 文件夹。
    - 快照、设置和日志：卸载向导里加一个「同时删除快照、设置和日志」勾选框，默认不勾。
    - 更新时旧卸载程序也会运行（带 `--updated`），这时绝不能删数据。
  - **风险**：没有代码签名时，「未签名程序静默运行下载来的安装包」可能被个别杀毒软件当成可疑行为。完整性靠发布里的 SHA-256 保证。
  - **设置里的「关于与更新」页**（2026-10-21 用户要求）：没有新版本时也能打开，看版本信息和时间线。
    - **版本卡片**：当前版本、安装方式（安装版 / 便携版 / AppImage…）、提示哪些版本、上次检查时间和下次检查时间。
    - **状态**：已是最新 / 有新版本 / 已下载，重启即可更新。
    - **按钮**：「检查更新」、更新卡片里的「无缝更新」和经典方式、「前往发布页」；被跳过的系列在这里「恢复提示」。
    - **组件版本**：SpaceKnife、Electron、Chromium、Node、Python、协议版本和系统，附「复制版本信息」，方便报告问题。
    - **时间线**：所有发布从新到旧排列。
      - 每一条显示版本号、日期、是否预发布，并标出「当前」和「新」。
      - 展开可以看发布说明：用帮助页的 Markdown 渲染，链接在浏览器里打开。
      - 离线时显示缓存的内容。
    - **后端**：更新记录现在只保存每个渠道最新的一版（`update.trim`），要改成保存最近约 30 个发布的历史，并限制每条发布说明的大小。
    - 「自动检查更新」和「提示哪些版本」两项设置移到这一页，「常规」里留一个入口。
- [x] 便携版（小众软件用户请求）：Windows x64 zip、Linux x64 tar.gz，程序旁的 `data` 文件夹存放全部数据（GitHub Actions「Portable build」，解压后以便携模式冒烟）
- [ ] Windows / Linux ARM64（依赖都有对应 wheel，Electron 也有 arm64 版；缺测试设备）。32 位 Windows 做不了：Electron 44 已不再提供 ia32

## 索引扫描调研：Btrfs / ReFS / XFS（2026-10-21 记下）

**现状**
- 只有两条扫描路径：
  - NTFS：经提权助手读原始 `$MFT`，用 USN 回放修正。
  - 其他文件系统：一律并行硬扫描。Windows 用 `NtQueryDirectoryFile` / `FileIdExtdDirectoryInfo`；Linux 用 `open` + `getdents64` + 每项一次 `fstatat`；macOS 用 `getattrlistbulk`。
- [docs/PLAN.md](docs/PLAN.md) 3.1 节规划了 Linux 上 Btrfs `TREE_SEARCH` 和 XFS `bulkstat`，作为需要提权、可选的索引扫描，还没实现。
- Windows 上的 ReFS 卷（包括 Windows 11 的开发人员驱动器 Dev Drive），以及 WinBtrfs 驱动挂载的 Btrfs 卷，现在都走硬扫描。
- `scan/volume.py` 已能识别 `fs_type`，可以按卷选择扫描后端。

**要调研的点**
- [ ] **Btrfs（Linux）**
  - [ ] 用 `BTRFS_IOC_TREE_SEARCH_V2` 读 FS tree：
    - `INODE_ITEM`：大小、实际占用、时间；
    - `INODE_REF` / `DIR_INDEX`：名字和父目录；
    - `EXTENT_DATA`：实际占用、压缩、inline extent。
    - 需要 `CAP_SYS_ADMIN`，所以走提权助手，只读。
  - [ ] 子卷：每个子卷一棵 FS tree。只扫挂载的那个子卷，还是带上嵌套子卷？跨子卷的边界怎样对应「不跨挂载点」？
  - [ ] 快照和 reflink 共享的 extent：要像 APFS clone 那样给出「独占大小」，口径与 `btrfs filesystem du` 对齐。
  - [ ] 增量：按 transid 过滤（类似 `btrfs subvolume find-new`），能否用来做增量重扫和监控补偿。
  - [ ] 速度：在大量小文件的卷上，和并行硬扫描比较耗时与 IO 量。
- [ ] **XFS（Linux，RHEL 默认文件系统）**
  - [ ] `XFS_IOC_BULKSTAT`（v5 接口）能批量读 inode 属性，但没有名字和父目录。名字有两种来源：
    - 照常读目录；
    - parent pointers（`XFS_IOC_GETPARENTS`）：需要较新的内核，且建文件系统时开了 `parent=1`，待核实，RHEL 9 很可能没有。
  - [ ] 测「bulkstat + 读目录」的组合是否明显快于 `getdents64` + `statx`；不快就不做。
  - [ ] 同样需要 `CAP_SYS_ADMIN`，走提权助手。
- [ ] **ReFS（Windows）**
  - [ ] 没有公开的、类似 `$MFT` 的原始格式，不打算解析磁盘结构。
  - [ ] 调研系统接口：
    - `FSCTL_ENUM_USN_DATA` 能否像在 NTFS 上那样枚举全卷（ReFS 的 USN 日志支持情况待核实）；
    - `FSCTL_GET_REFS_VOLUME_DATA`；
    - 128 位文件 ID 的处理。
  - [ ] 块克隆（block cloning）共享的空间要算「独占大小」，Dev Drive 上很常见。
  - [ ] 监控：ReFS 上 USN 日志能否和 NTFS 一样用来补偿。
- [ ] **ext4（Linux）**
  - [ ] 没有批量读 inode 的 ioctl。唯一的办法是提权后直接读块设备（libext2fs，或自己解析 inode 表和目录块），风险和工作量都大，先评估值不值得。
- [ ] **Btrfs（Windows，WinBtrfs 驱动）**
  - [ ] Linux 的 ioctl 在这里不可用。调研 WinBtrfs 有没有自己的 FSCTL 可以批量枚举（它的 shell 扩展用到的接口），是否支持 `FSCTL_ENUM_USN_DATA`。
  - [ ] 都不行就继续走硬扫描。用户量小，优先级最低。
- [ ] **共同要做的**
  - [ ] 作为可插拔扫描后端接入：按 `fs_type` 选择；索引扫描失败或没有权限时自动回退硬扫描，界面上说明用的是哪种。
  - [ ] 提权助手协议加只读命令。和 Linux 上读磁盘寿命记录一样，只在用户点击后请求授权。
  - [ ] 正确性对照：总量与 `du` 和系统报告的差、硬链接只计一次、稀疏和压缩文件、快照和克隆的共享空间。
  - [ ] 基准测试要设上限（时间、读取量），不在用户的盘上做持续的重 IO。
  - [ ] 结论写回 docs/PLAN.md 3.1 节：做哪些、不做哪些、为什么。

## 重复文件检测（2026-10-22 记下）

**目标**：找出内容完全相同的文件，按能腾出的空间排序，选好保留哪一份后放进删除篮。只读，不改文件。

- [ ] **分几步筛，尽量少读盘**
  - [ ] 按大小分组：节点表里已有大小，不读盘。去掉只有一个文件的大小和 0 字节文件。
  - [ ] 同一组里先读每个文件头尾各一小块（比如 64 KiB），算快速哈希再分组。
  - [ ] 剩下的再算整个文件的哈希（BLAKE3 或 xxh3-128），放在 Rust 扩展里多线程算、不占 GIL；要不要再逐字节比一遍，待定。
- [ ] **不算重复的**
  - [ ] 同一个文件的多个硬链接（`ino` / `nlink`，`HARDLINK_DUP`）。
  - [ ] 云盘占位文件：读内容会触发下载，跳过并说明。
  - [ ] 系统和程序目录默认不列，或者单独提示风险（例如 `C:\Windows\WinSxS` 本来就靠硬链接）。
- [ ] **共享空间**：NTFS / ReFS 块克隆、APFS clone、Btrfs reflink 的文件内容相同，但不占两份空间。「能腾出多少」要分开算，和索引扫描调研里「独占大小」的口径一致。
- [ ] **读盘要守规矩**
  - [ ] 遵守磁盘负载上限（`IoGovernor`），可以暂停和取消，显示进度和剩余量。
  - [ ] 结果按路径、大小、修改时间、文件 ID 缓存，下次只重算变过的文件；实时监控发现变化时，受影响的组失效。
  - [ ] 基准测试设上限（时间、读取量），不在用户盘上做持续的重 IO。
- [ ] **内存**：候选只记节点 id（int32），哈希只给有同大小伙伴的文件算；按每个候选文件 32–48 B 估算，写进 docs/memory.md。
- [ ] **界面**
  - [ ] 结果按组列出：每组的份数、单份大小、能腾出的空间（份数减一乘大小）。
  - [ ] 可视化里高亮同组的其他文件；悬停卡写「另有 N 份相同内容」。
  - [ ] 选择保留哪一份：最旧、最新、指定的文件夹优先，或者手动挑。
  - [ ] 加入删除篮，复用它的去重、占用检测和回收站。
  - [ ] 命令面板和右键菜单加入口（整个扫描，或者选中的文件夹）。
- [ ] **以后再说**：在同一卷上把重复文件换成硬链接（风险高：改一份会改到全部）。
- [ ] **调研**：TreeSize、WizTree、dupeGuru、czkawka 怎么做筛选和保留规则，取舍写进 docs/PLAN.md。

## 硬扫描的准确性与开销（2026-10-22 调查）

起因：试过「按文件 ID 打开子目录」，结果在 C: 上悄悄漏扫了三分之一，在 WinBtrfs 上无限循环，后来去掉了（经过见 [docs/native.md](docs/native.md) 2.2「试过但去掉的」）。同时对照了小众软件论坛上各款空间分析软件在 WinSxS 上结果不一致的讨论。

- [x] **非 ID、只保留访问时间的硬扫描：冷热缓存实测与整盘核对**（2026-10-22）：C:（NTFS 876 万项）、W:（ReFS 321 万项）、I:（WinBtrfs 149 万项）各冷 2 次、热 2 次，逐个文件夹与参照（C: 读 MFT，W:/I: Python 遍历器）比对，没有漏扫和扫错，差异都能归到期间的文件变动或已知的有意差异（docs/native.md 2.2）。
- [x] **子目录相对父目录句柄、按名字打开**（`relative_open`，默认开；代替按 ID 打开）：每个目录打开后核对文件 ID；重解析点、没有 ID、名字带 `\`、接近长度上限的都按完整路径；WinBtrfs 的 FileIdExtd ID 不可信时改用 FileIdBoth。热缓存 W: 快约 25–30%、C: 约 4%，冷缓存看不出差别；三块盘逐个文件夹比对一致（docs/native.md 2.2）。
- [x] **保留访问时间在受保护的系统文件夹里不起作用**：`C:\Windows\WinSxS`、`DriverStore\FileRepository` 等只给 TrustedInstaller 写权限，管理员打开 `FILE_WRITE_ATTRIBUTES` 也被拒（系统盘上约 16% 的文件夹），硬扫描整个 C: 仍写 0.25–0.7 GB。不启用 `SeRestorePrivilege`（2026-10-22 用户决定）：这些文件夹照常列，关键是结果不能错、扫描要快，WinSxS 这类文件夹不占多数。
- [x] Rust 单元测试（`cargo test --lib`，含 Windows 上相对打开的 ID 核对测试）在 CI 的 Python 任务里三个系统都跑（「Native unit tests」）。
- [ ] WinBtrfs 上的硬扫描用时在约 1 s 和约 5.8 s（每次读 1.2 GB）之间跳，和开不开相对打开无关，看起来是 WinBtrfs 自己的缓存；Python 遍历器（FindFirstFileEx）有时不读盘。查清楚是不是 `FileIdExtdDirectoryInformation` 让它多读了 inode。
- [ ] **Windows 硬扫描的硬链接只计一次**（PLAN.md 的正确性要求，现在没做到）
  - 现状：每行都是 `ino=0, nlink=1`，每个名字都算一次。本机 WinSxS 的情况：
    - 每个名字都算：27.21 GiB；
    - 同一文件只算一次：25.15 GiB；
    - 其中与 System32 等共享：8.00 GiB（33,920 个文件）。
  - 后果：整盘合计会多算 10 GiB 以上，可能超过卷的已用空间（违反 UI-W7-18 的验收）。删除篮在 Windows 硬扫描下也判断不了硬链接（docs/manage.md）。
  - [ ] 原生遍历器把 `FileIdExtdDirectoryInformation` 已经给出的文件 ID 填进 `ino`，不多花系统调用。
    - 注意 WinBtrfs（2026-10-22 实测）：它的 `FileIdExtdDirectoryInformation` 给每个子项填的都是被列目录自己的 ID（`I:\usr` 下 bin、games、include 都是 `I:\usr` 的 `…0775`），`FileIdBothDirectoryInformation` 和句柄上查到的 ID 才是对的。照搬就会把整个文件夹当成同一个文件的硬链接。相对打开已经按「一次列表里两个子目录同一 ID」识别它并改用 FileIdBoth（`win.rs` `shares_ids`），硬链接要用同一份判断；文件之间的重复 ID 本来就可能是硬链接，只能从子目录判断。
  - [ ] 没有 nlink，要自己找重复：扫描结束时按 `ino` 排序（870 万项约 70 MB、1 秒量级），或者边扫边找；然后修正 `HARDLINK_DUP` 和各级合计。内存按 docs/memory.md 估算。
  - [ ] 三种扫描统一归属规则。现在：
    - MFT 快扫：算在 MFT 记录里第一个名字上；
    - POSIX 硬扫描：算在先合并进来的那个上，并行时每次可能不同。
    - 改为都按 UI-SPEC 待定 15 的建议，算在路径字典序最小的那一处，三种扫描结果一致。
  - [ ] 测试要覆盖：跨文件夹的硬链接、同一文件夹里的多个名字、扫描中途新增的链接；整盘合计不超过已用空间。
- [ ] **文件夹的独占 / 共享占用**
  - 像 DISM `/AnalyzeComponentStore` 那样分开显示。本机 WinSxS：独占 17.15 GiB，与别处共享 8.00 GiB。
  - 范围：硬链接，加上块共享（ReFS 块克隆、Btrfs reflink、APFS clone、NTFS 重复数据删除）。口径和上面「索引扫描调研」「重复文件检测」里的独占大小一致。
  - [ ] 先做设计调研：各文件系统用什么接口拿到共享 / 私有的大小、要不要提权、每个文件多花多少；每个文件夹的独占部分怎样高效算（每组共享的文件求最近公共祖先）；详情面板、树图提示和删除篮怎么显示。结论写进 docs/。
  - 依赖上一项（硬链接只计一次）。
- [x] **Linux 硬扫描不改目录的访问时间**（0.3.4：打开后 `fcntl(F_SETFL, O_NOATIME)`，被拒照旧；测试见 docs/native.md 2.2）
  - 做法：目录属于当前用户（父目录列表里有 `st_uid`）或有 `CAP_FOWNER`（提权助手）时，用 `O_NOATIME` 打开；被拒就照旧打开。
  - 为什么要做：`relatime` 下每个目录每天最多写一次，但一天里的第一次扫描会改写自己所有目录的 inode。Btrfs 上更新访问时间还要写时复制元数据，并拆开快照之间共享的元数据。
  - 验证方法同 Windows：扫描前后访问时间不变，三遍逐个文件夹比对一致。
  - macOS 没有对应的开关。
- [ ] **Linux 硬扫描的 CPU 大头是每项一次 `fstatat`**（2026-10-22 在 WSL 上重测：热缓存时列目录线程只忙 8–17%，瓶颈是主线程归并，`statx` 改变不了总用时，先不做；硬链接去重已挪进原生 `LinkSet`，热缓存快约 22%。剩下的主线程开销是 `ingest_slice`，约 0.25 µs/项；2026-10-23 归并移进 Rust 后 WSL 根分区热缓存 0.34 → 0.26 s，见 docs/native.md 2.3，主线程归并仍占约 50–60%，其中含等 GIL）
  - WSL 实测 /usr（22 万项）：约 1.2 µs/项，约占 CPU 的三分之二。
  - 按完整路径打开和相对父目录 `openat` 没有可测出的差别，所以 Linux 不做 Windows 那种按 ID 打开。
  - 可以试：`statx` 只要用到的字段，并加 `AT_STATX_DONT_SYNC`（主要对 NFS / SMB 挂载有用）。
  - 真正的大提速要靠上面「索引扫描调研」里的提权读元数据。
- [ ] **重新测线程数和 CPU 的关系**（Linux/WSL 2026-10-22：4–32 个线程用时相同，瓶颈不在列目录；Windows 还没重测）
  - 之前「超过 8 个线程只多花 CPU、不提速」的数据，来自有 bug 的按 ID 打开版本（少扫了三分之一），作废。
  - 用正确的版本、按扫描器的调度方式，重测 8 / 16 / 32 个线程的用时和 CPU。
  - 再决定动态线程算法要不要盯「每 CPU 秒列出的项数」；默认上限仍按核数。
- [ ] **WinBtrfs 上名字含 `\` 的 Linux 文件打不开**
  - 现状：`I:\usr` 里有 8 个，例如 systemd 的 `app-chromium\x2dbrowser-.scope.d`。Windows 上 `\` 是分隔符，按路径打不开，现在记为错误。
  - 可以试：相对父目录句柄、按单个名字打开（`RootDirectory` + 名字）。先查清 WinBtrfs 怎么把这类名字映射给 Windows。
- [ ] **改打开或列目录的方式之前，先在真实的整盘上比对**
  - 做法：设 `SPACEKNIFE_TEST_SCAN_ROOT=<盘或目录>`，跑 `test_native_scan_of_a_real_tree_matches_the_python_walker`。Python 遍历器在前后各扫一次，排除扫描期间的文件变动。
  - 教训：按 ID 打开那次只比了项数和两个小目录，漏掉了 C: 的三分之一。
  - 考虑在发布检查里固定跑一次（C: 加一块非 NTFS 的盘）。

## 硬扫描的 IO 饱和（2026-10-23，第一步）

IO 饱和调查的最终报告（仓库外）建议分步落地：先做第一轮已验证、与载体池设计无关的 CPU 和内存项，再用载体池替换每个 Python 线程一个的 rayon 小池，然后才是控制器。原则：一套可移植的策略，不做只对这台机器成立的东西（绑 P 核、按卷写死的线程数）。

- [x] 第一步（0.3.4，每项一个回退开关，整盘 A-B-A 比对见 docs/native.md 2.2 末尾）：
  - [x] Windows 省掉收尾的目录查询（`skip_final`，只在本地 NTFS、ReFS、WinBtrfs，缓冲区放不下另一项时照发；重解析点的列表不省；每次调用里每个根自检头 64 个、之后 64 个抽 1 个，发现多出项就对这个根停用；核验模式 `_verify_final`）。评审后默认关，`SPACEKNIFE_WIN_SKIP_FINAL=1` 打开：省的是 CPU，靠的是文件系统填缓冲区的方式
  - [x] 相对打开先按 "." 项的文件 ID 核对（`dot_check`），不符或缺失时照原来的方式查句柄。测不出收益，评审后默认关，`SPACEKNIFE_WIN_DOT_CHECK=1` 打开
  - [x] 系统设置不更新访问时间的卷上不保留访问时间（`keep_where_updated`，只信 ReFS 的 1、NTFS 的 1 / 3）
  - [x] 归并移进 Rust、逐项那一遍放开 GIL（`SliceFold`，docs/native.md 2.3；`SPACEKNIFE_PY_FOLD=1` 回退）
  - [x] 快照边压缩边写，zstd 最多 4 个线程，name_off 分块换基（docs/memory.md §4）
  - [x] Linux glibc：mmap 阈值 4 MiB、arena 最多 4 个，扫完和空闲时 malloc_trim（docs/memory.md §8；`SPACEKNIFE_GLIBC_DEFAULTS=1` 回退）
  - [x] 预派发切片（在途切片 = 1.5 × 列目录线程）：默认线程放置下热缓存 C:\Windows 按轮交替 8 次，墙钟 0.97 倍 [0.90–1.05]、CPU 0.98 倍，没有确定的收益（第一轮的 −4–5% 是绑 P 核时测的；归并移进 Rust 之后列目录线程已忙 91%），**不做**。
- [ ] 第一步里还没做的一项（等用户决定）：NodeStore 的列放进预留地址空间、按需提交（第一轮：880 万行模拟 append 0.46 → 0.14 s，扫描结束时提交等于实际用量）：生命周期、提交失败回退、映射模式切换要设计，还差在 W: 上端到端测。
- [ ] 第二步：载体池替换 rayon 小池（宽度先固定为今天的值），之前先查清同宽度下 W: 冷缓存慢约 30% 的原因，补上 POSIX 的 fd 预算和资源错误重排。并发提高以后在大并发下重测 glibc 的 `M_ARENA_MAX=4`（docs/memory.md §8）。
- [ ] K:、W:（TOPMORE NVMe）冷缓存每臂至少 8 轮，按每次运行的读延迟分层或配对，确认第一步在这两块盘上冷缓存没有变慢（评审：按中位数慢 33%，但盘的延迟状态在两次运行之间差 5 倍）。
- [ ] 第三、四步：governor 的上限交给 Rust；控制器先作为默认关闭的选项（报告 §2.4 的几处先修，离线回放后再实机验证）。
- [ ] 不做：父目录拒绝写属性就让子目录直接普通打开（预测错的文件夹会被更新访问时间，改变了访问时间行为）；绑 P 核及类似的机器专用设置。

## 其他

- [x] 官网（`site/`，Astro → GitHub Pages）：中英双语、按系统给出下载按钮、SEO（canonical / hreflang / Open Graph / JSON-LD / sitemap）、给 AI 助手的 `llms.txt` 与 `llms-full.txt`
- [x] 自定义域名 `spaceknife.kaltsit.dev`（Cloudflare 上的 CNAME → `duskxi.github.io`，仅 DNS；GitHub Pages 自定义域名 + 强制 HTTPS）：站点在根路径，`robots.txt` 与 `llms.txt` 在域名根目录
- [ ] 在 Google Search Console / Bing Webmaster Tools 提交 sitemap（需要账号所有者操作）；在 GitHub 账号设置里验证 `kaltsit.dev`（防止子域名被接管）
- [x] 英文界面排版：悬停卡的时间标签、对比图例、对比模式工具栏在 1440 px 下不再截断

- [ ] MFT 快扫：卷根目录下若真有名为「$NTFS 元数据」/「$孤立文件」的文件，会和伪文件夹同名并排显示（合计正确，只是名字重复）；给伪文件夹加后缀避让（Hypothesis 找到的边界情况）

- [ ] 浅色主题（0.3.4 暂定稿，起因是 AppImage 目录审核 AppImage/appimage.github.io#10482）：A2 的浅色模式，默认跟随系统；外框、树图、火焰图、火焰-极坐标图、分支色（`visual.branch.*`）和环形色（`visual.ring.*`）都有浅色值（`design/tokens/themes/light.tokens.json`），类别颜色按主题保存。实现已完成，色值经用户评审之前不算完成，也不合进发布
  - [ ] 用户评审浅色色板：`uv run python tools/preview_design.py <out.html>` 的深浅对照和实机截图。要问清：当初否决 B「纸本」的理由（暖色和印刷感，还是「浅底加白色表面」本身；若是后者，这一稿同样踩中）；青柠、橙色预设在浅色里变深（#4F6200 军绿、#954104 锈褐）能不能接受；黄色标签在外框里是暗金 #957700、画布上是深色底座上的亮黄 #FFD23F，同一屏两种黄，要不要改成外框也用深色底座加亮色；命令面板的 22% 石墨遮罩把黄、橙块压成芥末黄、焦褐，要不要换成不压暗的遮罩；警告的浅底现在垫在淡黄 `status.warningBase` 上（帮助页「注意」框 #FEF4CA），标题和边线仍是暗金 #845D00
  - [ ] Linux 实测「跟随系统」：GNOME 默认（应为浅色）、GNOME 深色、没有 xdg-desktop-portal 的桌面（如 Xfce）各跑一次 AppImage；实测之前 UNRELEASED 不写 Linux 的具体行为
  - [ ] macOS 实测「跟随系统」：系统设置 › 外观的浅色、深色、自动；红绿灯按钮和系统菜单跟随应用的选择
  - [ ] 帮助页的三张配图按主题出两套（`vite-plugins/help-pictures.ts` 按 `themeName()` 选图源）；在此之前浅色里仍是深色截图。`tools/demo/screenshots.cjs` 用 `SHOT_THEME=light` 截浅色
  - [ ] 分支色板在绿色盲下有几对槽位接近（如 4 / 5、8 / 1），两套主题相同（V 待定 1）
- [ ] 深色主题原有的对比度不足（`tools/tests/test_gen_design.py` 的 `DARK_SHORT`）：`treemap.time.2` 的名称 4.43、大小行 3.71；分支第 3 档 1、3、5、7 槽的大小行 4.26–4.41；火焰图和火焰-极坐标图点亮的文件夹条（大小行已改用名称的颜色），分支第 3 档 1、3、5、7 槽点亮后的名称 4.31–4.44。另有点阵阈值：代码用亮度 0.18，规格写 0.2。非文字：深色里「其他」和「游戏」的类别色在面板上不到 3:1；本机磁盘「外接盘」的深蓝段在深色卡片上约 2:1
- [ ] 0.3.3 里明确选过「深色」的用户升级后第一次启动：还没有 `theme.json`，窗口先按系统出一次，设置到了才翻成深色（只此一次，之后 `theme.json` 记着）。要消掉得让 main 照 platformdirs 找到后端的 `settings.json` 读 `appearance.theme`（SPACEKNIFE_HOME、Windows 的 LocalAppData、macOS / Linux 的 XDG_CONFIG_HOME 各一套），先不做
- [ ] 扁平配色按主题保存（等 UI-W7-31 ⑤ 的改色实现）：每个元素、每个主题一个设置项 `appearance.flat_<元素>_<主题>`，空为 `treemap.flat.*` 的默认色
- [ ] 导出报告的浅色色板（`export/formats.py`）对齐浅色令牌：它的三级灰在底色上只有 4.62:1

## 本机统计页面（2026-10-20 记下；2026-10-21 做成「本机磁盘」页，0.3.3）

一页看全这台机器上的所有盘：没扫描过的盘和扫描过的盘都列出来，并且分清每块盘的类型、接口和速率。入口：开始页右栏最上面的「本机磁盘」摘要卡片和命令面板「本机磁盘」；不自动弹出、不自动刷新。页面是仪表盘：汇总数字、容量构成、发现清单、每块盘的环形计量和链路计量、卷的快照增长折线（UI-SPEC W9）。文档：`disks.md`。

- [x] 盘的范围：没扫描过的盘列出系统能给的数据（容量、已用、可用、文件系统、卷标）；扫描过的盘再加上扫描得出的数据，比如最近一次扫描或快照的时间、文件数、类别分布、最大的文件夹
- [x] 介质类型：SSD / HDD / U 盘 / SD 卡 / 网络盘 / 虚拟盘
- [x] 接口和速率：SATA（1.5 / 3 / 6 Gb/s）、PCIe / NVMe（代数和通道数，例如 Gen4 x4）、USB（5 / 10 / 20 Gb/s 等）、SAS 等；尽量同时给出当前协商的速率和设备支持的最高速率
- [x] 物理盘与分区 / 卷的对应关系（一块盘上有几个卷，一个卷跨几块盘）
- [x] 现状（做之前）：`VolumeInfo.media` 已经能分出 `nvme_ssd` / `sata_ssd` / `hdd` / `usb_*` 等（启动页卡片和限速在用），但还没有接口代数、速率，也没有物理盘与卷的对应
- [x] 数据来源都不需要管理员权限（Windows 上 ATA IDENTIFY、NVMe Identify 也能用不带读权限的句柄经 `IOCTL_STORAGE_QUERY_PROPERTY` 取到；PCIe 链路来自 CfgMgr 的 `DEVPKEY_PciDevice_*Link*`；USB 速率来自集线器的连接查询）。调研时列的候选：可能的来源：Windows 用 `IOCTL_STORAGE_QUERY_PROPERTY`、`MSFT_PhysicalDisk`（BusType / MediaType）、PCI 设备属性（当前链路速度和宽度）；Linux 用 `/sys/block/*/queue/rotational`、`/sys/class/nvme`、`/sys/bus/pci/devices/*/current_link_speed` 和 `current_link_width`、`/sys/class/ata_link/*/sata_spd`；macOS 用 IOKit 或 `system_profiler`
- [x] 协议：`machine.disks`（`spaceknife_protocol.machine`），前端只负责显示
- [x] 界面：单独的标签页（`pages/MachinePage.tsx`），每块盘一张卡片，盘上的卷按盘上顺序；链路比盘能跑的慢时用一行说明（不弹窗）
- [ ] macOS 只用记录的数据形状测过（`system_profiler` 的键名随系统版本变化，比如 macOS 15 的 `SPUSBHostDataType`）：在真机上核对 NVMe / SATA / USB 链路
- [x] 磁盘自己的记录（0.3.3）：Windows 不提权读 NVMe 健康日志 / ATA 设备统计日志；macOS 经 IOKit 的 NVMe SMART 接口读 NVMe 盘（不提权）；Linux 点「用管理员权限读取…」后由提权助手读（NVMe 管理命令、SG_IO 的 ATA 直通）
- [ ] macOS 的 NVMe SMART 在 Intel Mac 上实测（`disks/mac_smart.py` 还没在真机上跑过）：数值和 smartctl / DriveDx 对一下；另一个程序（Stats、DriveDx）占着接口时安静地读不到；连读约 20 次看有没有泄漏；正式签名（Developer ID、hardened runtime、不加 disable-library-validation）的包里也读得到。有机会在 Apple 芯片的 Mac 上跑一次
- [ ] macOS 的 SATA 盘（iMac 2019 的机械盘 / Fusion Drive）：`ATASMARTLib` 的接口还没调研（要不要 root、读得到什么）
- [ ] Linux 的 deb / rpm 在真机桌面上走一遍：polkit 密码框、冻结后端以 root 跑 `--helper`；可以随包附带一个 polkit 策略文件（`org.freedesktop.policykit.exec.path` 指向 `/opt/SpaceKnife/resources/backend/spaceknife-backend/spaceknife-backend`），让密码框写清楚要做什么（现在是 pkexec 的通用说明）
- [ ] Linux 上 SATA 盘只有当前速率（设备支持的最高速率要 IDENTIFY，需要 root）：提权助手读健康记录时已经拿到 IDENTIFY，可以顺带补上
- [ ] Windows 把 USB 3.2 Gen 2×2 报告成 10 Gb/s：`IOCTL_USB_GET_NODE_CONNECTION_SUPERSPEEDPLUS_INFORMATION` 能分出 20 Gb/s

## 可视化组件（2026-10-19）

契约见 [docs/visuals.md](docs/visuals.md)，界面需求见 [docs/UI-SPEC.md](docs/UI-SPEC.md) 的 V 节。

- [x] 可替换的可视化：后端引擎注册表与行契约检查（`layout/registry.py`、`layout/contract.py`），前端组件注册表与一致性测试（`visual/registry.ts`、`visual/testing/conformance.ts`）；树图金样逐字节不变
- [x] 火焰图（`flame.v1`，默认根在下，设置「火焰图方向」）和火焰-极坐标图（`polar.v1`）：共用分区引擎，每帧最多 5 万行；分支配色，每种可视化记住自己的配色；工具栏、V / Shift+V、命令面板、设置
- [x] 协议 1.1：能力 `visuals.v1`、`layout.flame.v1`、`layout.polar.v1`，`visual.list`；旧前端只收到树图
- [ ] 端到端检查（在 sandbox 或隐藏桌面里跑，不在用户桌面弹窗）：扫描中切换（持续推送、中心圆和外环呼吸、交叉淡入、缩放根 / 选中 / 历史保持）；监控中的闪烁（沿弧线或条的轮廓、减少动画时的静态角标、落不到行的变化闪中心圆）；聚合放大后切换、隐藏模式、结果列表重新分组；Shift+F10 和右键菜单不遮住目标；极坐标里悬停卡跟随指针；对比视图三种都能切换且色阶作用在文件夹段；分屏各用各的、新分屏复制焦点窗格、重连后保持；只带基础能力的客户端只收到 `drawlist.v1`；不认识的布局回退到树图且不卡住；缩放动画（火焰图横向拉满、火焰-极坐标图展开并收进中心圆，返回时反向）；「新视图默认可视化」和火焰图方向两个设置生效
- [ ] 与旧树图对比手动检查：缩放动画、闪烁、呼吸、悬停卡、右键菜单、分屏（树图的宿主拆分后行为应一致；缩放返回时旧画面的文字现在随画面一起淡出）
- [ ] 无障碍与布局手动检查：Tab 焦点顺序、屏幕阅读器读出「可视化（V）」和各段名称
- [ ] 窄窗口时工具栏的收起顺序：过滤框先压缩，然后是芯片、配色，可视化切换最后（工具栏右侧现在没有收起逻辑，需要 CSS 改动）
- [ ] 截图回归：`tools/demo/screenshots.py` 加火焰图和火焰-极坐标图
- [ ] 分支色板（8 色 × 4 档）：设计评审与色盲检查，或换成用户给的色相
- [x] 在 Linux（WSL）上跑全量测试（2026-10-22，带原生扩展：1797 项通过、237 项按平台跳过；真实目录对照 `SPACEKNIFE_TEST_SCAN_ROOT=/` 也通过）
- [ ] 发布说明：旧版本后端读到 `branch`、`flame`、`flame_polar` 或新的设置键时，会判整份设置文件不合法，退回上一次的有效设置（`settings.py` 的校验）；按单项回落的改造另行排期
- [ ] 性能遗留项：隐藏模式仍按全长数组构造尺寸；`_scanning_rows` 仍是 O(N)（树图已改为只算画到的节点的尺寸，不再每次推送重算整列，2026-10-22）
- [ ] `min_element` 有三个默认值：10（设置）、14（`ViewSettings` / `LayoutParams`）、15（令牌），各端阈值口径要统一
- [x] `settings.py` 里 `treemap.fps` 的说明文字提到了第三方产品名，改成不提名的写法（2026-10-20：「动画重绘的频率」）
- [ ] 极坐标里的角标（对比的「新增 / 已删除」、减少动画时的静态变化角标）以锚点为中心画成轴对齐的框，再收进扇区的外接框；在 3 点和 9 点方向附近，宽角标可能越过扇区内沿几个像素。改为沿角平分线摆放并按扇区形状判断放不放得下
- [ ] P2：扫描帧之间按 key 补间；火焰图与火焰-极坐标图之间的变形动画；沿弧线弯曲的文字
- [x] 插件系统（前后端一体）：设计与现状见 [docs/plugins.md](docs/plugins.md)，写插件见 [docs/plugin-dev.md](docs/plugin-dev.md)。统一通讯层（插件声明方法 / 事件 / 状态 / 流，宿主负责传输、类型生成、能力与权限）；能力从注册表推导；插件代码默认在独立进程里（审计钩子、资源上限、只读的树镜像和宿主端查询）；一处声明命令、按键、设置与文案；声明式界面和沙箱 Web 面板；`spaceknife plugin` 命令行；内置导出和洞察规则改成了内置插件；三个参考插件。
  - [x] 试写第一轮（六个插件）找出的 23 处缺口：宿主、SDK、命令行和文档里补上（docs/plugins.md「试写第一轮之后的改进」），新增 API 参考 [docs/plugin-api.md](docs/plugin-api.md)
  - [ ] 试写的插件（不在仓库里，worktree 的 plugins-trial1/）跟上：按新的 `check` 换掉不存在的图标名、去掉 `_current`、重新 `plugin build`；archivesrc 的坏包测试改成 `pytest.raises`，growthpanel 打开历史以外快照的测试改成期望被拒
  - [ ] 派一个只读 `docs/plugin-dev.md` 的新 agent 写一个小插件（如「按扩展名统计」的报告加一条命令），记下 token 用量和走的弯路，回头补文档
  - [ ] 还很薄的部分（docs/plugins.md 三节「还很薄或未做」）：对比侧栏和状态栏的插件报告、工具栏「更多」；扫描来源边扫边画；监控器给普通扫描用；文件夹规则叠加；悬停字段只读缓存；macOS 内存上限只靠心跳；`check` 的契约只跑固定的合成树
  - [ ] 以后做：WASM 运行时、签名与发布者、插件目录与从 Git 安装、操作系统级沙箱（Windows 低完整性、Linux Landlock + seccomp、macOS sandbox profile）、零拷贝的树视图、按需申请的权限、插件之间的依赖、新的布局格式、Electron 主进程插件、新的 `OpMode`（移动、压缩、硬链接去重）
  - [x] Windows：子进程映射一个不存在的共享内存名时会静默得到一块全零的内存；现在镜像开头有宿主写的头，映射时核对（2026-10-23）
