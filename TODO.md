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
- [ ] 负载口径待用户决定：现在按「忙碌度 / 队列深度」折算；任务管理器的「活动时间」在 NVMe 上几个并发读就到 100%。建议保留现口径，旁边加显示活动时间
- [ ] 硬扫描结果归并向量化（`NodeStore.ingest_slice`）：现在端到端被 Python 归并卡在约 25 万项/秒
- [ ] Linux / macOS 的提权助手（现在 `elevate=True` 返回 `not_supported`）
- [ ] W2 扫描摘要卡（扫描中选中某块时，详情面板顶部的进度 + 档位 + 负载）
- [ ] 日志面板「扫描统计」页复用负载曲线与并发

## M5 文件变化钩子

- [ ] Windows：ReadDirectoryChangesW + USN 日志（助手加 `read_usn(volume, from)`）
- [ ] Linux：fanotify / inotify；macOS：FSEvents
- [ ] 增量流水线：定点重扫、合并进节点表
- [ ] 树图元素闪烁（变化提示）
- [ ] 「快照后变化」
- [ ] 已停止扫描的「继续扫描」（从停下的位置补扫）
- [ ] 验收：并发写入压测无事件丢失，或溢出可检测并补扫

## M6 管理与分析

- [ ] W5 右键菜单（含系统原生 Shell 菜单）与删除篮：真正删除 / 移到回收站、危险操作确认；MFT 伪目录（`$NTFS 元数据`、`$孤立文件`）里的项在后端也要拒绝删除
- [ ] W6 快照对比
- [ ] W7 完整设置（「全部恢复默认」等）
- [ ] 模板导出
- [ ] 命令行完整参数
- [ ] 洞察规则库
- [ ] 验收：UI-SPEC 中 W5、W6、W7（完整）需求全部勾选

## M8 打包与发布（Windows 与 Linux）

- [ ] Electron 安装包：Windows NSIS；Linux deb / rpm / AppImage
- [ ] Windows 代码签名：没有证书时先发未签名包（SmartScreen 会提示）
- [ ] 干净机器冒烟测试：Windows 10+、RHEL 9、Ubuntu 22.04

## 其他

- [ ] 亮色主题（待设计评审）
