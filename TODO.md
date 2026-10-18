# TODO

离最终发布还没做完的事。里程碑定义见 [docs/PLAN.md](docs/PLAN.md) 第 8 节，界面需求见 [docs/UI-SPEC.md](docs/UI-SPEC.md)。完成一项就勾掉，挂起的写明原因和恢复条件。

## 挂起

- **M7 原生 UI**（2026-10-18 起挂起，暂不做）
  - spaceknife-client 共享视图层
  - Qt（PySide6）与 WinUI3（win32more）各自实现全部界面（UI-SPEC 484 条需求），两者逻辑与视觉一致
  - 三端 golden 截图对比
  - 受影响的后续项：M8 里 Qt / WinUI3 的 PyInstaller 打包和 Windows App Runtime 安装一并挂起
  - 恢复条件：用户决定恢复
- **macOS 打包、签名与公证**（M8 的一部分，暂不做）
  - 原因：Apple 开发者证书需要付费
  - macOS 的原生遍历代码（`readdir`、`st_birthtime`）也还没编译和运行过
  - 恢复条件：有了开发者证书和 macOS 测试机

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
- [ ] W2 的「正在扫描」闪烁（UI-W2-04 / UI-TM-42：扫描中正在填充的文件夹）

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
- [ ] 干净机器安装测试：Windows 10、RHEL 9、Ubuntu 22.04（装 / 运行 / 扫描 / 卸载）；Windows 上安装包本身（安装到用户机器前先问用户）
- [ ] Windows 代码签名：没有证书时先发未签名包（SmartScreen 会提示）
- [ ] 选定许可证（deb / rpm 里现在是 `unknown`），附第三方许可清单（Python 依赖、字体）
- [ ] 确认项目主页（现在暂定 `https://github.com/DuskXi/SpaceKnife`，deb / rpm 必填）
- [ ] 包体积：只 strip 没被 auditwheel 改过的库（libpython、zstandard、原生扩展），Linux 后端约省 40 MB 解压后体积
- [ ] 自动更新（electron-updater，需要发布渠道）

## 其他

- [ ] 亮色主题（待设计评审）
