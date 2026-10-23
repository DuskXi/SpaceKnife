---
title: 快捷键
description: 按场景列出 SpaceKnife 的全部按键和鼠标操作，附 macOS 上的差别和命令面板里每条命令的按键。
---

SpaceKnife 的大部分操作都能只用键盘完成。这一页按场景列出每一个键，最后附上命令面板里全部命令的按键表。

:::tip[记不住就按 Ctrl+K]
命令面板列出每条命令和它的按键，输入几个字母、拼音或拼音首字母就能找到并执行，例如输入 `qhps` 找到「切换配色」。见[命令面板](../command-palette/)。
:::

下面写的是 Windows 和 Linux 的组合；macOS 见[macOS 上的差别](../shortcuts/#macos-上的差别)。

## 按键什么时候生效

- **单键不会在输入框里触发。** <kbd>/</kbd>、<kbd>F</kbd>、<kbd>U</kbd>、<kbd>R</kbd>、<kbd>C</kbd>、<kbd>V</kbd>、数字键、<kbd>Enter</kbd>、<kbd>Delete</kbd>、<kbd>Backspace</kbd>、<kbd>Esc</kbd> 只在焦点不在文本框里时作用于视图。在过滤框里输入 `f` 不会关掉可用空间，<kbd>Ctrl</kbd>+<kbd>C</kbd> 在输入框里也是普通的复制。
- **全局键在过滤框里也有效。** <kbd>Ctrl</kbd>+<kbd>K</kbd>、<kbd>Ctrl</kbd>+<kbd>J</kbd>、<kbd>Ctrl</kbd>+<kbd>T</kbd>、<kbd>Ctrl</kbd>+<kbd>W</kbd>、<kbd>Ctrl</kbd>+<kbd>,</kbd>、<kbd>Ctrl</kbd>+<kbd>Tab</kbd>，以及 0.3.2 起的 <kbd>F5</kbd>、<kbd>Shift</kbd>+<kbd>F5</kbd>、<kbd>Ctrl</kbd>+<kbd>S</kbd> 不用先离开过滤框。有些输入框把按键全部留给输入，全局键在里面不起作用：启动页的路径框、编辑路径的输入框（<kbd>Ctrl</kbd>+<kbd>L</kbd>），以及设置里的搜索框、数字框和标签名称框。
- **视图键作用于焦点窗格。** 分屏时，带强调边框的那一侧是焦点窗格；点一下另一侧就把焦点移过去。
- **控件自己的键优先。** 焦点在按钮、单选、菜单项这类控件上时，<kbd>Enter</kbd>、<kbd>Delete</kbd>、<kbd>Esc</kbd> 和 <kbd>Shift</kbd>+<kbd>F10</kbd> 交给控件（例如按钮上的 <kbd>Enter</kbd> 就是按下按钮）；焦点在下拉框上时，<kbd>C</kbd> 和 <kbd>V</kbd> 用来按首字母选项。
- **对话框打开时，背后的界面不响应按键。** 命令面板打开时只有 <kbd>Ctrl</kbd>+<kbd>K</kbd>（关闭面板）和 0.3.2 起的 <kbd>F1</kbd>（打开命令面板的帮助页）还在窗口层面起作用；大版本更新的对话框打开时，所有快捷键都不生效。

## 全局

| 按键 | 作用 |
| --- | --- |
| <kbd>Ctrl</kbd>+<kbd>K</kbd> | 打开或关闭命令面板 |
| <kbd>Ctrl</kbd>+<kbd>,</kbd> | 打开设置；设置已经打开时切换过去 |
| <kbd>Ctrl</kbd>+<kbd>J</kbd> | 显示或隐藏日志面板 |
| <kbd>F1</kbd> | 0.3.2 起：打开帮助，直接翻到讲当前界面的那一页；标题栏的「帮助」按钮也一样 |
| <kbd>Ctrl</kbd>+<kbd>T</kbd> | 新建标签（一个「新扫描」启动页） |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> | 关闭当前标签；关闭正在扫描的标签会停止这次扫描，关掉最后一个标签时会留下一个新的启动页 |
| <kbd>Ctrl</kbd>+<kbd>Tab</kbd>、<kbd>Ctrl</kbd>+<kbd>PageDown</kbd> | 下一个标签（到最后一个后回到第一个） |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Tab</kbd>、<kbd>Ctrl</kbd>+<kbd>PageUp</kbd> | 上一个标签 |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>PageUp</kbd> / <kbd>PageDown</kbd> | 把当前标签左移 / 右移一位 |

## 导航

在扫描视图里使用。扫描还在进行时也可以放大和导航。

| 按键 | 作用 |
| --- | --- |
| <kbd>Enter</kbd> | 放大进入选中的块。选中的是文件时，放大到它所在的文件夹并保持选中；选中的是「N 个小项」聚合块时，进入这个聚合块 |
| <kbd>Backspace</kbd>、<kbd>Alt</kbd>+<kbd>←</kbd> | 后退到上一个位置 |
| <kbd>Shift</kbd>+<kbd>Backspace</kbd>、<kbd>Alt</kbd>+<kbd>→</kbd> | 前进 |
| <kbd>Alt</kbd>+<kbd>↑</kbd> | 上一级 |
| <kbd>Ctrl</kbd>+<kbd>Home</kbd> | 回到根 |
| <kbd>Ctrl</kbd>+<kbd>L</kbd> | 编辑路径：工具栏的路径变成输入框，输入本次扫描里的路径后按 <kbd>Enter</kbd> 跳过去，<kbd>Esc</kbd> 取消。点路径的最后一段也一样。分屏和快照对比时不可用；分屏时请在命令面板里输入路径 |
| <kbd>/</kbd>、<kbd>Ctrl</kbd>+<kbd>F</kbd> | 把焦点放进过滤框 |

后退和前进记录的是「放大到哪里、选中了什么」，和浏览器的历史一样。想跳到本次扫描范围以外的路径，用命令面板：输入路径后它会提供「扫描此位置」。更多导航方式见[浏览与导航](../navigation/)。

## 画面

| 按键 | 作用 |
| --- | --- |
| <kbd>C</kbd> | 切换配色：文件类别 → 扁平 → 修改时间 → 顶层分支 → 文件类别。在快照对比里改为切换显示「全部 → 增长 → 减少」 |
| <kbd>V</kbd> | 下一种可视化：树图、火焰图、火焰-极坐标图。快照对比里只在支持对比的可视化之间切换 |
| <kbd>Shift</kbd>+<kbd>V</kbd> | 上一种可视化 |
| <kbd>F</kbd> | 显示或隐藏可用空间块（快照对比里不起作用） |
| <kbd>U</kbd> | 显示或隐藏未知空间块（快照对比里不起作用） |
| <kbd>R</kbd> | 显示或隐藏回收站（扫描里有回收站时才有；快照对比里不起作用） |
| <kbd>Ctrl</kbd>+<kbd>=</kbd>、<kbd>Ctrl</kbd>+<kbd>+</kbd> | 提高细节层级，最多 15 |
| <kbd>Ctrl</kbd>+<kbd>-</kbd> | 降低细节层级，最少 1 |

<kbd>Ctrl</kbd>+<kbd>+</kbd> 和 <kbd>Ctrl</kbd>+<kbd>-</kbd> 用数字小键盘上的加减号也可以。切换配色、可视化和细节层级只改当前视图；新视图从哪种开始，在[设置](../settings/)的「外观」和「可视化」里定。几种可视化的区别见[可视化](../visualizations/)。

## 选中与标签

| 按键 | 作用 |
| --- | --- |
| <kbd>Esc</kbd> | 取消选中。没有选中项时再按一次：在快照对比里退出对比，过滤时清除过滤 |
| <kbd>Delete</kbd> | 把选中的文件或文件夹加入删除篮（「N 个小项」聚合块请用右键菜单加入） |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | 复制选中项的完整路径 |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> | 在资源管理器中显示（macOS 是访达，Linux 是文件管理器） |
| <kbd>Ctrl</kbd>+<kbd>&#96;</kbd> | 在此打开终端：选中文件夹时在这个文件夹里，选中文件时在它所在的文件夹里 |
| <kbd>Shift</kbd>+<kbd>F10</kbd>、菜单键 | 打开选中块的右键菜单，菜单出现在块旁边。没有选中项时，打开火焰图根条或火焰-极坐标图中心圆盘的菜单。详情面板的「更多操作」按钮也一样 |
| <kbd>Ctrl</kbd>+<kbd>1</kbd> … <kbd>Ctrl</kbd>+<kbd>4</kbd> | 加红 ▲、黄 ■、绿 ●、蓝 ◆ 标签 |
| <kbd>Ctrl</kbd>+<kbd>0</kbd> | 清除标签 |
| <kbd>1</kbd> … <kbd>4</kbd>、<kbd>0</kbd> | 同上，不用按 <kbd>Ctrl</kbd> |

不按 <kbd>Ctrl</kbd> 的数字键由设置「标签 › 快捷键 › 树图有焦点时可省略 Ctrl」控制，默认打开；关掉后只有 <kbd>Ctrl</kbd>+数字有效。标签的用法和过滤见[标签与文件类别](../tags-classes/)，删除篮见[清理](../cleanup/)。

:::note
只在 MFT 快速扫描里出现的 NTFS 元数据和孤立文件，在磁盘上没有对应的位置：<kbd>Ctrl</kbd>+<kbd>E</kbd> 和 <kbd>Ctrl</kbd>+<kbd>&#96;</kbd> 对它们只给出提示。
:::

## 右键菜单

右键点一个块，或者用 <kbd>Shift</kbd>+<kbd>F10</kbd> 打开。菜单打开后焦点就在菜单里。

| 按键 | 作用 |
| --- | --- |
| <kbd>↑</kbd>、<kbd>↓</kbd> | 上下移动 |
| <kbd>Enter</kbd> | 执行当前项；在「复制路径」「加标签」「归类」上是打开子菜单 |
| <kbd>→</kbd> | 打开子菜单 |
| <kbd>←</kbd> | 从子菜单回到上一层 |
| <kbd>Esc</kbd> | 关闭子菜单；不在子菜单里时关闭菜单 |

菜单项依次是：打开、在资源管理器中显示、在此打开终端、复制路径 ▸、加标签 ▸、只看这里（放大）、归类 ▸（仅文件夹）、重新扫描此文件夹、加入删除篮、系统菜单…（还没有接入）。「复制路径」子菜单有四种写法：原样（<kbd>Ctrl</kbd>+<kbd>C</kbd> 复制的就是这一种）、带引号、POSIX 路径、`file://` URI，每一种都先预览出来。在菜单外点一下、改变窗口大小或切到别的窗口，菜单就关闭。

## 匹配结果列表

过滤生效后，右侧栏显示「匹配结果」。点一下列表，或者用 <kbd>Tab</kbd> 把焦点移进去：

| 按键 | 作用 |
| --- | --- |
| <kbd>↑</kbd>、<kbd>↓</kbd> | 上下移动，画面里同时高亮对应的块 |
| <kbd>Home</kbd>、<kbd>End</kbd> | 第一项、最后一项 |
| <kbd>PageUp</kbd>、<kbd>PageDown</kbd> | 翻一屏 |
| <kbd>Space</kbd> | 在画面里定位这一项 |
| <kbd>Enter</kbd> | 放大进入 |
| <kbd>Delete</kbd> | 加入删除篮（文件和文件夹） |

鼠标单击定位、双击放大、右键打开同一个右键菜单。带 <kbd>Ctrl</kbd> 或 <kbd>Alt</kbd> 的键照常交给视图，例如在列表里按 <kbd>Alt</kbd>+<kbd>↑</kbd> 回到上一级、在更大的范围里找。

## 过滤框

| 按键 | 作用 |
| --- | --- |
| <kbd>/</kbd>、<kbd>Ctrl</kbd>+<kbd>F</kbd> | 进入过滤框 |
| <kbd>Enter</kbd> | 立即应用（不按也会在输入时自动应用） |
| <kbd>↑</kbd>、<kbd>↓</kbd> | 在补全列表里选择 |
| <kbd>Tab</kbd> | 补全当前选中的候选 |
| <kbd>Esc</kbd> | 依次：关闭补全列表 → 清空过滤 → 离开过滤框 |

过滤语法见[过滤](../filtering/)。

## 日志面板

<kbd>Ctrl</kbd>+<kbd>J</kbd> 打开或收起。焦点在日志列表里时，下表的键（按住 <kbd>Ctrl</kbd> 或 <kbd>Alt</kbd> 时也一样），以及字母、数字、<kbd>Delete</kbd>、<kbd>Backspace</kbd>、<kbd>Esc</kbd>，都只作用于日志：

| 按键 | 作用 |
| --- | --- |
| <kbd>↑</kbd>、<kbd>↓</kbd>、<kbd>Home</kbd>、<kbd>End</kbd> | 选择一行 |
| <kbd>Enter</kbd> | 在画面里定位这一行涉及的路径（会切到那次扫描的标签） |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | 复制这一行的路径 |

其他快捷键，例如 <kbd>Alt</kbd>+<kbd>←</kbd>、<kbd>Ctrl</kbd>+<kbd>E</kbd>、<kbd>Shift</kbd>+<kbd>F10</kbd>，照常作用于日志背后的视图。

## 命令面板

| 按键 | 作用 |
| --- | --- |
| <kbd>Ctrl</kbd>+<kbd>K</kbd> | 打开；再按一次关闭 |
| <kbd>↑</kbd>、<kbd>↓</kbd> | 上下移动（到头后从另一端继续） |
| <kbd>PageUp</kbd>、<kbd>PageDown</kbd> | 一次移动 10 行 |
| <kbd>Enter</kbd> | 执行高亮的一行 |
| <kbd>Tab</kbd> | 补全：把命令名、路径或过滤条件填进输入框，继续编辑 |
| <kbd>Esc</kbd> | 关闭，焦点回到打开前的位置 |

用中文输入法组字时，<kbd>Enter</kbd>、方向键和 <kbd>Esc</kbd> 交给输入法。详见[命令面板](../command-palette/)。

## 启动页与扫描

| 按键 | 作用 |
| --- | --- |
| <kbd>Enter</kbd> | 开始扫描（焦点在其他按钮上时，按的是那个按钮）。在路径框里按 <kbd>Enter</kbd> 也一样 |
| <kbd>←</kbd>、<kbd>→</kbd>、<kbd>↑</kbd>、<kbd>↓</kbd> | 在卷列表里换卷（左右换一个，上下换一行） |
| <kbd>←</kbd>、<kbd>→</kbd>（或 <kbd>↑</kbd>、<kbd>↓</kbd>） | 焦点在扫描详情的「磁盘负载上限」上时，换到相邻的档位 |
| <kbd>F5</kbd> | 0.3.2 起：重新扫描当前视图。放大进入的文件夹连同下面的一切重新读一遍，就地更新；在多个扫描位置的最顶层时，所有位置都重读一遍 |
| <kbd>Shift</kbd>+<kbd>F5</kbd> | 0.3.2 起：全盘重新扫描，用同样的位置重新开始一次扫描 |
| <kbd>Ctrl</kbd>+<kbd>S</kbd> | 0.3.2 起：把当前扫描保存为快照 |

这三个键和命令面板里的同名命令一样：命令在面板里是灰的（扫描还没完成、当前标签不是扫描结果、对比时重新扫描当前视图），按键就不执行，窗口底部提示原因。按住不放只执行一次。

## 设置标签页

| 按键 | 作用 |
| --- | --- |
| <kbd>/</kbd>、<kbd>Ctrl</kbd>+<kbd>F</kbd> | 进入「搜索设置」 |
| <kbd>Esc</kbd>（在搜索框里） | 清空搜索；已经是空的就离开搜索框 |
| <kbd>Backspace</kbd>、<kbd>Alt</kbd>+<kbd>←</kbd> | 回到上一个看过的设置页 |
| <kbd>Alt</kbd>+<kbd>→</kbd> | 前进 |

全局键照常可用。设置页的用法见[设置参考](../settings/)。

## 帮助标签页

0.3.2 起，帮助和设置一样只有一个标签页，按 <kbd>F1</kbd> 打开。

| 按键 | 作用 |
| --- | --- |
| <kbd>/</kbd>、<kbd>Ctrl</kbd>+<kbd>F</kbd> | 进入「搜索帮助」 |
| <kbd>Backspace</kbd>、<kbd>Alt</kbd>+<kbd>←</kbd> | 回到上一个看过的位置 |
| <kbd>Alt</kbd>+<kbd>→</kbd> | 前进 |

## 对话框

<kbd>Esc</kbd> 关闭导出报告、更新提示和 macOS 的「完全磁盘访问权限」提示（等于选择取消）。导入设置的预览要点「取消」或点对话框外面。

## 鼠标

| 操作 | 作用 |
| --- | --- |
| 单击块 | 选中，详情面板显示它；点空白处取消选中。点火焰图的根条或火焰-极坐标图的中心圆盘，详情面板显示当前缩放根 |
| 双击块 | 放大进入；双击文件会放大到它所在的文件夹并选中它 |
| 双击根条或中心圆盘 | 上一级（火焰图、火焰-极坐标图） |
| 右键块 | 选中它并打开右键菜单 |
| 悬停 | 显示提示卡；在[设置](../settings/)「常规 › 悬停提示」里调整 |
| 单击路径的某一段 | 跳到那一级；单击最后一段开始编辑路径 |
| 中键单击标签 | 关闭这个标签 |
| 双击标题栏空白处 | 最大化或还原窗口 |
| 往启动页拖入文件夹 | 加进扫描路径；拖入 `.sks` 快照文件则直接打开它 |

## macOS 上的差别

- **⌘ 和 Control 都可以。** 凡是写 <kbd>Ctrl</kbd> 的地方，在 macOS 上按 <kbd>⌘</kbd> 或 <kbd>Control</kbd> 都行。命令面板和设置里的键位表用 ⌘、⌥、⇧ 显示，例如 <kbd>⌘</kbd><kbd>K</kbd>、<kbd>⌥</kbd><kbd>←</kbd>。
- **Alt 就是 Option（⌥）。**
- **Mac 键盘上的 delete 是 Backspace。** 按 <kbd>delete</kbd> 是后退；加入删除篮要按 <kbd>fn</kbd>+<kbd>delete</kbd>（向前删除）。
- **没有 Home、PageUp、PageDown 键时用 fn。** <kbd>fn</kbd>+<kbd>←</kbd> 是 Home，<kbd>fn</kbd>+<kbd>↑</kbd> / <kbd>fn</kbd>+<kbd>↓</kbd> 是 PageUp / PageDown，所以「回到根」是 <kbd>⌘</kbd>+<kbd>fn</kbd>+<kbd>←</kbd>。
- **切换标签用 Control+Tab。** <kbd>⌘</kbd>+<kbd>Tab</kbd> 是系统的应用切换；也可以用 <kbd>⌘</kbd>+<kbd>fn</kbd>+<kbd>↑</kbd> / <kbd>↓</kbd>。
- **打开终端可以用 Control+&#96;。** 系统默认把 <kbd>⌘</kbd>+<kbd>&#96;</kbd> 用于在同一应用的窗口之间切换。
- **功能键**：功能键被设成媒体键时，<kbd>Shift</kbd>+<kbd>F10</kbd>（以及 0.3.2 起的 <kbd>F1</kbd>、<kbd>F5</kbd>、<kbd>Shift</kbd>+<kbd>F5</kbd>）要再加 <kbd>fn</kbd>。
- **右键菜单**：Mac 键盘没有菜单键，用 <kbd>Shift</kbd>+<kbd>F10</kbd>，或者按住 Control 单击、双指轻点。

## 暂时不能用的键

:::caution
提示卡上写的 <kbd>Space</kbd> 预览还没有实现，按了没有反应。

0.3.1 里 <kbd>F5</kbd>、<kbd>Shift</kbd>+<kbd>F5</kbd>、<kbd>Ctrl</kbd>+<kbd>S</kbd> 也还没有反应，请从命令面板运行这几个命令；重新扫描某个文件夹也可以用右键菜单的「重新扫描此文件夹」。
:::

键位目前不能修改：「设置 › 快捷键 › 键位预设」显示为灰色，下面的键位表列出当前的全部按键，可以按命令名或按键搜索。

## 命令面板里的全部命令

下表与命令面板、「设置 › 快捷键」里的键位表来自同一份命令清单。「也能用这些词找到」是命令的英文别名；此外命令的中文名、全拼和拼音首字母都能找到它。

<!-- generated:shortcuts:start -->

### 空输入时列出的命令

打开面板、还没有输入时，「命令」组按这个顺序列出这几条：

| 命令 | 按键 | 也能用这些词找到 | 备注 |
| --- | --- | --- | --- |
| 重新扫描当前视图 | <kbd>F5</kbd> | `rescan` · `refresh` | — |
| 全盘重新扫描 | <kbd>Shift</kbd>+<kbd>F5</kbd> | `rescan all` · `full rescan` | — |
| 切换配色 | <kbd>C</kbd> | `color` · `colour` · `theme` | — |
| 显示/隐藏可用空间 | <kbd>F</kbd> | `free space` | — |
| 打开删除篮 | — | `basket` · `trash` · `delete` | — |
| 保存快照 | <kbd>Ctrl</kbd>+<kbd>S</kbd> | `save snapshot` | — |
| 与快照对比… | — | `compare` · `diff` | — |
| 导出报告… | — | `export` · `report` · `csv` · `json` · `html` | — |
| 设置 | <kbd>Ctrl</kbd>+<kbd>,</kbd> | `settings` · `preferences` · `options` | — |

### 输入后才列出的命令

其余命令要输入名称、拼音或下表的词才会出现：

| 命令 | 按键 | 也能用这些词找到 | 备注 |
| --- | --- | --- | --- |
| 查找重复文件… | — | `duplicates` · `dupes` · `dedupe` · `same files` | — |
| 重复文件面板 | — | `duplicates panel` · `dupes panel` | — |
| 重复文件清理方案… | — | `cleanup` · `dedupe plan` · `hard link` | — |
| 重复文件清理记录 | — | `cleanup history` · `undo cleanup` · `recovery area` | — |
| 后退 | <kbd>Backspace</kbd> · <kbd>Alt</kbd>+<kbd>←</kbd> | `back` | — |
| 前进 | <kbd>Alt</kbd>+<kbd>→</kbd> | `forward` | — |
| 上一级 | <kbd>Alt</kbd>+<kbd>↑</kbd> | `up` · `parent` | — |
| 根 | <kbd>Ctrl</kbd>+<kbd>Home</kbd> | `home` · `root` | — |
| 提高细节层级 | <kbd>Ctrl</kbd>+<kbd>=</kbd> | `more detail` · `deeper` | — |
| 降低细节层级 | <kbd>Ctrl</kbd>+<kbd>-</kbd> | `less detail` | — |
| 切换可视化 | <kbd>V</kbd> | `visualization` · `chart` · `treemap` · `flame` · `icicle` · `flame_polar` · `radial` · `rings` | — |
| 聚焦过滤 | <kbd>/</kbd> · <kbd>Ctrl</kbd>+<kbd>F</kbd> | `filter` · `find` | — |
| 编辑路径 | <kbd>Ctrl</kbd>+<kbd>L</kbd> | `path` · `go to` · `address` | — |
| 放大进入 | <kbd>Enter</kbd> | `zoom` · `enter` | — |
| 加入删除篮 | <kbd>Delete</kbd> | `delete` · `remove` · `trash` | — |
| 预览 | <kbd>Space</kbd> | `preview` · `quick look` | 计划中 |
| 复制路径 | <kbd>Ctrl</kbd>+<kbd>C</kbd> | `copy path` | — |
| 在资源管理器中显示（Windows） / 在访达中显示（macOS） / 在文件管理器中显示（Linux） | <kbd>Ctrl</kbd>+<kbd>E</kbd> | `reveal` · `explorer` · `finder` · `show in folder` | — |
| 在此打开终端 | <kbd>Ctrl</kbd>+<kbd>&#96;</kbd> | `terminal` · `shell` · `console` | — |
| 新建标签 | <kbd>Ctrl</kbd>+<kbd>T</kbd> | `new tab` | — |
| 关闭标签 | <kbd>Ctrl</kbd>+<kbd>W</kbd> | `close tab` | — |
| 标签左移 | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>PageUp</kbd> | `move tab left` · `reorder tabs` | — |
| 标签右移 | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>PageDown</kbd> | `move tab right` · `reorder tabs` | — |
| 红色标签 | <kbd>1</kbd> · <kbd>Ctrl</kbd>+<kbd>1</kbd> | `red tag` | — |
| 黄色标签 | <kbd>2</kbd> · <kbd>Ctrl</kbd>+<kbd>2</kbd> | `yellow tag` | — |
| 绿色标签 | <kbd>3</kbd> · <kbd>Ctrl</kbd>+<kbd>3</kbd> | `green tag` | — |
| 蓝色标签 | <kbd>4</kbd> · <kbd>Ctrl</kbd>+<kbd>4</kbd> | `blue tag` | — |
| 清除标签 | <kbd>0</kbd> · <kbd>Ctrl</kbd>+<kbd>0</kbd> | `clear tag` · `untag` | — |
| 显示/隐藏未知空间 | <kbd>U</kbd> | `unknown space` | — |
| 显示/隐藏回收站 | <kbd>R</kbd> | `trash` · `recycle bin` | — |
| 分屏 | — | `split` · `side by side` | — |
| 暂停扫描 | — | `pause` | — |
| 继续扫描 | — | `resume` · `continue` | — |
| 停止扫描 | — | `stop` · `cancel` | — |
| 继续已停止的扫描 | — | `continue` · `go on` | — |
| 新扫描 | — | `new scan` · `scan` | — |
| 打开快照文件… | — | `open snapshot` · `load` | — |
| 日志面板 | <kbd>Ctrl</kbd>+<kbd>J</kbd> | `log` · `logs` · `console` | — |
| 帮助 | <kbd>F1</kbd> | `help` · `docs` · `documentation` · `manual` · `guide` | — |
| 检查更新 | — | `check for updates` · `update` · `upgrade` · `new version` | — |
| 查看更新 | — | `update` · `release notes` · `what's new` · `download update` | — |
| 本机磁盘 | — | `disks` · `drives` · `hardware` · `ssd` · `nvme` · `sata` · `usb` · `pcie` | — |
| 关于与更新 | — | `about` · `version` · `release notes` · `changelog` · `timeline` | — |

### 只按名称列出的命令

这几条直接选定一种配色或可视化，只在输入时出现，不进设置里的键位表：

| 命令 | 按键 | 也能用这些词找到 | 备注 |
| --- | --- | --- | --- |
| 配色：文件类别 | — | `color class` | — |
| 配色：扁平 | — | `color flat` | — |
| 配色：修改时间 | — | `color time` · `age` | — |
| 配色：顶层分支 | — | `color branch` · `top level` | — |
| 配色：重复 | — | `color duplicates` · `dupes` | — |
| 可视化：树图 | — | `visualization treemap` · `treemap` | — |
| 可视化：火焰图 | — | `visualization flame` · `flame graph` · `icicle` | — |
| 可视化：火焰-极坐标图 | — | `visualization polar` · `polar flame graph` · `radial` · `rings` | — |

<!-- generated:shortcuts:end -->
