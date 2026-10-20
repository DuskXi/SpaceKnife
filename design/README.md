# 设计 token 与图标

三个前端的视觉一致，靠的是同一份源文件生成各端产物。

| 源文件（手工编辑） | 内容 |
|---|---|
| `tokens/color.tokens.json` | A2「石墨 + 沉浸的对比度」外框色板（暗色） |
| `tokens/treemap.tokens.json` | 树图 `carbon` 主题：层级、类别色、可用/未知/等待扫描、选中/悬停/闪烁、快照对比色阶、几何常量 |
| `tokens/type.tokens.json` | 字体（Geist、Geist Mono、Noto Sans SC）、字号、字重、行高 |
| `tokens/size.tokens.json` | 圆角、间距、控件尺寸、窗口骨架、阴影、动效（含切换可视化的交叉淡入 `motion.duration.swap` 200 ms） |
| `tokens/visual.tokens.json` | 可视化：分支配色 `visual.branch.{1–8}.{1–4}` 与中性色（待设计评审与色盲检查）、分区布局的文件夹环形色 `visual.ring.1–4`、根条与中心圆 `visual.center.*`（别名到树图第 0 层标题栏）、祖先光晕 `visual.haloOverlay`、几何常量 `visual.flame.*` 与 `visual.flamePolar.*`（后端各有一份，测试核对两边相同）|
| `icons/icons.json` | 16×16 描边图标（50 个），含三种可视化的图标 `vizTreemap`、`vizFlame`、`vizFlamePolar`（生成的文件名为 `viz-treemap.svg` 等） |

格式是 DTCG 的子集：`$value`、`$type`（可写在父组上继承）、`$description`、`{路径}` 别名。支持的类型：color（`#RRGGBB` / `#RRGGBBAA`）、dimension（px）、duration（ms）、number、fontWeight、fontFamily、cubicBezier、shadow。

## 生成

```bash
uv run python tools/gen_design.py
```

| 生成物 | 使用方 |
|---|---|
| `build/tokens.css` | Electron：CSS 变量 `--sk-color-bg-window` 等 |
| `build/tokens.ts` | Electron：类型化常量（WebGL 树图调色板）和 `cssVar` 映射 |
| `build/qml/SpaceKnife/Tokens/` | Qt：QML 单例 `Tokens.colorBgWindow` 等 |
| `build/xaml/Tokens.xaml` | WinUI3：ResourceDictionary，键名 `SkColorBgWindow`、`SkColorBgWindowBrush` 等 |
| `../packages/client/src/spaceknife_client/tokens.py` | Python 前端：`TOKENS["color.bg.window"]` |
| `build/icons/*.svg`、`build/icons.ts` | 三端图标 |

生成物不要手改；CI 用 `--check` 检查它们是否与源文件一致。

## 待办

- 亮色主题：W7「外观」要求亮/暗切换，亮色色板需要先出设计稿给用户评审，再补进 `color.tokens.json`、`treemap.tokens.json` 和 `visual.tokens.json`（分支色、环形色）。
- 字体文件：M3 之前把 Geist、Geist Mono、Noto Sans SC 的字体文件（均为 OFL 许可）放进 `design/fonts/`，三端打包同一份。
