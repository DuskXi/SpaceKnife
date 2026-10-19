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
- [ ] Linux / macOS 的提权助手（现在 `elevate=True` 返回 `not_supported`）
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
- [ ] 自动更新（electron-updater，需要发布渠道）
- [x] 便携版（小众软件用户请求）：Windows x64 zip、Linux x64 tar.gz，程序旁的 `data` 文件夹存放全部数据（GitHub Actions「Portable build」，解压后以便携模式冒烟）
- [ ] Windows / Linux ARM64（依赖都有对应 wheel，Electron 也有 arm64 版；缺测试设备）。32 位 Windows 做不了：Electron 44 已不再提供 ia32

## 其他

- [x] 官网（`site/`，Astro → GitHub Pages）：中英双语、按系统给出下载按钮、SEO（canonical / hreflang / Open Graph / JSON-LD / sitemap）、给 AI 助手的 `llms.txt` 与 `llms-full.txt`
- [x] 自定义域名 `spaceknife.kaltsit.dev`（Cloudflare 上的 CNAME → `duskxi.github.io`，仅 DNS；GitHub Pages 自定义域名 + 强制 HTTPS）：站点在根路径，`robots.txt` 与 `llms.txt` 在域名根目录
- [ ] 在 Google Search Console / Bing Webmaster Tools 提交 sitemap（需要账号所有者操作）；在 GitHub 账号设置里验证 `kaltsit.dev`（防止子域名被接管）
- [x] 英文界面排版：悬停卡的时间标签、对比图例、对比模式工具栏在 1440 px 下不再截断

- [ ] MFT 快扫：卷根目录下若真有名为「$NTFS 元数据」/「$孤立文件」的文件，会和伪文件夹同名并排显示（合计正确，只是名字重复）；给伪文件夹加后缀避让（Hypothesis 找到的边界情况）

- [ ] 亮色主题（待设计评审）：包括可视化的分支色（`visual.branch.*`）和环形色（`visual.ring.*`）

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
- [ ] 在 Linux（WSL）上跑全量测试：本次只用现成的 Linux 环境跑了布局与可视化相关的 269 项（树图金样逐字节一致），没跑带原生扩展的全量
- [ ] 发布说明：旧版本后端读到 `branch`、`flame`、`flame_polar` 或新的设置键时，会判整份设置文件不合法，退回上一次的有效设置（`settings.py` 的校验）；按单项回落的改造另行排期
- [ ] 性能遗留项：扫描推送时 `sizes()` 缓存每次都失效（树图的全量路径）；隐藏模式仍按全长数组构造尺寸；`_scanning_rows` 仍是 O(N)
- [ ] `min_element` 有三个默认值：10（设置）、14（`ViewSettings` / `LayoutParams`）、15（令牌），各端阈值口径要统一
- [ ] `settings.py` 里 `treemap.fps` 的说明文字提到了第三方产品名，改成不提名的写法
- [ ] 极坐标里的角标（对比的「新增 / 已删除」、减少动画时的静态变化角标）以锚点为中心画成轴对齐的框，再收进扇区的外接框；在 3 点和 9 点方向附近，宽角标可能越过扇区内沿几个像素。改为沿角平分线摆放并按扇区形状判断放不放得下
- [ ] P2：扫描帧之间按 key 补间；火焰图与火焰-极坐标图之间的变形动画；沿弧线弯曲的文字
- [ ] 插件（以后做）：发现与加载（entry point `spaceknife.visuals`、插件目录、启用 / 停用、签名与信任）；服务端按注册表声明布局能力（现在是固定的 `ALL_CAPABILITIES`）；隔离（子进程加只读映射的列视图，或 WASM；时间预算，只算最新一次）——加载第三方代码之前必须先做；插件自带的参数 schema、词表和图标；运行时的 `visual.changed` 事件。前端组件插件原则上不做（Qt 和 WinUI3 无法保持一致）
