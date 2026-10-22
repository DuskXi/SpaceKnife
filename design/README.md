# 设计 token 与图标

三个前端的视觉一致，靠的是同一份源文件生成各端产物。

| 源文件（手工编辑） | 内容 |
|---|---|
| `tokens/color.tokens.json` | A2「石墨 + 沉浸的对比度」外框色板（暗色） |
| `tokens/treemap.tokens.json` | 树图 `carbon` 主题：层级、类别色、扁平配色的默认色、可用/未知/等待扫描、选中/悬停/闪烁、快照对比色阶、几何常量 |
| `tokens/type.tokens.json` | 字体（Geist、Geist Mono、Noto Sans SC）、字号、字重、行高 |
| `tokens/size.tokens.json` | 圆角、间距、控件尺寸、窗口骨架、阴影、动效（含切换可视化的交叉淡入 `motion.duration.swap` 200 ms） |
| `tokens/visual.tokens.json` | 可视化：分支配色 `visual.branch.{1–8}.{1–4}` 与中性色（待设计评审与色盲检查）、分区布局的文件夹环形色 `visual.ring.1–4`、根条与中心圆 `visual.center.*`（别名到树图第 0 层标题栏）、祖先光晕 `visual.haloOverlay`、几何常量 `visual.flame.*` 与 `visual.flamePolar.*`（后端各有一份，测试核对两边相同）|
| `tokens/themes/light.tokens.json` | 浅色主题：上面各文件里颜色和投影的浅色值。A2 的浅色模式，不是已否决的 B「纸本」：中性色是冷灰石墨，表面保持暗色的次序和步距（最高一层是白色），可视化绕白底镜像，类别色只动亮度和彩度 |
| `icons/icons.json` | 16×16 描边图标（50 个），含三种可视化的图标 `vizTreemap`、`vizFlame`、`vizFlamePolar`（生成的文件名为 `viz-treemap.svg` 等） |

`tokens/*.tokens.json` 是暗色主题，也是所有主题的基础。`tokens/themes/light.tokens.json` 是浅色主题的覆盖文件（DTCG Resolver 的「集合 + 主题修饰」做法）：

- 路径必须在基础令牌里存在：令牌只能换令牌，组只能进组；
- 只能覆盖 color 和 shadow，`$type` 不能变，几何、字体、动效两个主题共用；
- 每个颜色令牌都写一遍（与暗色同值的也写），新加颜色令牌时同时给出浅色值，`tools/tests/test_gen_design.py` 检查；与暗色同值的颜色列在那里的 `SAME_AS_DARK`；
- 别名在合并之后解析，所以 `visual.center.*`、`treemap.flat.*`、`color.bg.selected` 这类别名会跟着各自的主题走；覆盖文件里的别名也指向浅色的值。

两个主题都要满足的对比度（外框文字、强调色和状态色、块上标签含大小行、标签形状、闪烁和选中环等，以及「层级对比」滑块拉满时各层不被夹成白或黑）也在 `tools/tests/test_gen_design.py` 里逐对检查；改色值后先跑它。

格式是 DTCG 的子集：`$value`、`$type`（可写在父组上继承）、`$description`、`{路径}` 别名。支持的类型：color（`#RRGGBB` / `#RRGGBBAA`）、dimension（px）、duration（ms）、number、fontWeight、fontFamily、cubicBezier、shadow。

## 生成

```bash
uv run python tools/gen_design.py
```

| 生成物 | 使用方 |
|---|---|
| `build/tokens.css` | Electron：CSS 变量 `--sk-color-bg-window` 等。`:root` 是浅色；`prefers-color-scheme: dark`（`data-theme="light"` 时除外）或 `data-theme="dark"` 时换成暗色，只写两个主题不同的变量；每块都带 `color-scheme` |
| `build/tokens.ts` | Electron：类型化常量 `tokens`（暗色）、`themes.dark` / `themes.light`（WebGL 调色板，渲染进程经 `lib/theme.ts` 的 `palette()` 取当前主题）和 `cssVar` 映射 |
| `build/qml/SpaceKnife/Tokens/` | Qt：QML 单例 `Tokens.colorBgWindow` 等，`Tokens.theme`（`"dark"` / `"light"`）决定两个主题不同的颜色；默认按系统的配色（Qt 6.5+ 的 `Qt.styleHints.colorScheme`，说不出时浅色），前端按「主题」设置改它 |
| `build/xaml/Tokens.xaml` | WinUI3：ResourceDictionary，键名 `SkColorBgWindow`、`SkColorBgWindowBrush` 等；两个主题不同的放在 `ThemeDictionaries` 里：暗色的键是 `Default`（WinUI 找不到 `Dark` 和高对比度主题时都退回它），浅色的键是 `Light` |
| `../packages/client/src/spaceknife_client/tokens.py` | Python 前端：`TOKENS["color.bg.window"]`（暗色）、`THEMES["light"][…]`、`token(path, theme)`；调用方传屏幕上的主题（不传是暗色） |
| `build/icons/*.svg`、`build/icons.ts` | 三端图标 |

生成物不要手改；CI 用 `--check` 检查它们是否与源文件一致。

## 待办

- 浅色色板的用户评审：`uv run python tools/preview_design.py <out.html>` 并排画出两个主题，配合实机截图。
- 字体文件：M3 之前把 Geist、Geist Mono、Noto Sans SC 的字体文件（均为 OFL 许可）放进 `design/fonts/`，三端打包同一份。
