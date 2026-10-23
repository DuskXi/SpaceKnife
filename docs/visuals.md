# 可视化组件 —— 设计与接口约定

本文是「可视化」这一层的接口契约：后端的布局引擎、线上的布局格式和前端的绘制组件之间怎么分工，三种内置可视化（树图、火焰图、火焰-极坐标图）各自的几何与标签规则，以及将来插件要遵守的约定。线上格式的字段表见 [protocol.md](protocol.md) 第 4 节，界面需求见 [UI-SPEC.md](UI-SPEC.md) 的 V 节。改接口时先改本文；Qt 和 WinUI3 前端恢复后，照本文各写一遍。

命名：可视化 id `treemap` / `flame` / `flame_polar`，中文名「树图」「火焰图」「火焰-极坐标图」，英文名 "Treemap" / "Flame graph" / "Polar flame graph"。

## 1. 三层模型

```
 后端                                线上                       前端（每端一套）
 Visualization（id、引擎、描述）  ─►  布局（列契约 + geometry）  ─►  组件（按布局注册）
   treemap      TreemapEngine         drawlist.v1（冻结）            treemap
   flame        PartitionEngine ┐     flame.v1                       flame
   flame_polar  PartitionEngine ┘     polar.v1                       flamePolar
```

- **可视化 id**：后端的一个引擎加一份描述符（`layout/api.py` 的 `Visualization`），在注册表登记。用户切换的是它。
- **线上布局**：一份列契约和一个几何参数模型（`FrameInfo.geometry`）。几个可视化可以共用一种布局。
- **前端组件**：按布局注册，一种布局一个组件，只管把这种布局的帧画出来、做命中测试。

将来只有后端代码的插件，只要输出一种已有的布局，三端前端都不用改。宿主代码（视图、服务端、前端宿主）只经注册表拿到引擎或组件，不直接 import 任何一种可视化；三种内置可视化和插件走同一条 `register()`。

## 2. 后端接口（`spaceknife.layout`）

`ENGINE_API = 1`，接口有不兼容的变化时加一。

```python
class Engine(Protocol):
    def run(self, params: LayoutParams, root: RootSpec, sizes: SizeSource) -> DrawList: ...
    def levels(self, params: LayoutParams) -> int: ...  # 这个视口能画出的层数（≤ params.detail）


EngineFactory = Callable[[Basis], Engine]  # 每个 store 建一次，跨帧复用


@dataclass(frozen=True, slots=True)
class Visualization:
    id: str  # 小写单词，可用 . _ - 连接，≤ 48 个字符
    api: int  # 写它时的 ENGINE_API
    layout: str  # 线上布局，形如 name.vN
    capability: str  # 会话必须声明的能力；基线 drawlist.v1 为 ""
    title_id: str  # 名称的消息 id（中文原文），title 属性返回译文
    icon: str  # 设计图标键（camelCase，如 vizFlamePolar）
    order: int  # 在切换器里的位置，其次按 id
    engine: EngineFactory
    default_color: ColorModeName = "class"
    compare: bool = True  # 能否用于对比视图
    free_space: bool = True  # 卷根时是否画可用 / 未知空间
```

- **输入**：`LayoutParams`（CSS 像素的宽高、dpr、细节层级、最小元素尺寸、排序、字号缩放、元素比例等），加 `options`：可视化自己的参数，按名字取（`params.opt("origin", "bottom")`，类型和默认值不同就当没给）。火焰图的方向就走这里，设置项是 `treemap.flame_origin`。
- `RootSpec`：缩放根的节点，或聚合放大时的成员；卷根时带可用 / 未知空间的字节数。
- `SizeSource`：这一帧的面积口径。`of(ids)` 取一批节点的大小，`full()` 给全量数组（O(N)，只有树图用）。普通视图用 `StoreSizes(basis)`；隐藏模式用匹配部分、对比视图用变化量，都是 `ArraySizes`。
- `Basis`：同一次扫描共享的输入（尺寸缓存、类别、`children(node, n)` 带防撕读）。
- **输出**：`DrawList`，通用列加 `geometry`（几何列，按布局的列名）、`members`（聚合块的成员）、`layout`、`visual`、`meta`（`FlameGeometry` / `PolarGeometry` / None）和 `levels`。

**注册表**（`layout/registry.py`）：`register(v)` 遇到 id 不合规、id 重复、引擎 API 不同、布局名不合规时抛 `ValueError`；`get(id)`、`all_visuals()`（按 order、id 排序）、`available(caps)`、`is_available(v, caps)`、`unregister(id)`。内置可视化在 `layout/builtins.py` 里 import 一次就登记。

**视图**（`view/state.py`）：每个视图有 `visual` 和每种可视化各自的配色（`color_mode` 读写当前可视化那一项，没有时取 `Visualization.default_color`）。引擎由共享的 `Engines` 按 id 首次使用时建好；换 store 时整组换掉，正在算的帧不会混用两个 store。`showable(visual, caps)`：已登记、且会话声明了它的能力；没有 `visuals.v1` 的会话只能用树图。

**服务端**：`view.set{visual}` 遇到未知或当前会话不可用的可视化报 `INVALID_PARAMS`（「这种可视化在当前界面不可用」），什么都不改；配色 `branch` 也只给有 `visuals.v1` 的会话。`view.open` 和新视图的默认值按会话能力降级到树图和「类别」。visual 变了时帧的 reason 为 `visual`。`visual.list` 返回所有已登记的可视化，`available` 和 `reason` 按会话能力填写，`name` 是当前界面语言的译文（前端切换语言后要重新取）。

## 3. 行契约

所有布局都必须满足，插件也一样；`layout/contract.py` 的 `check_draw(draw, store, root, *, full=True, sizes=None)` 逐条检查，后端的契约测试按注册表参数化，每种已登记的可视化都要通过。

1. 父行在前：`parent[i] < i`；父行是上一层已展开的文件夹，`depth` 为父行加 1（顶层为 1）。
2. 同一帧里 `key` 不重复。
3. `kind` ≤ 3 的行是真实节点：`key == node`，文件 / 文件夹 / 等待扫描与节点表一致；还没列出子项的文件夹只能是等待扫描块。合成块用带标记的 key（`KEY_SMALL | 文件夹`、`KEY_MORE`、`KEY_FREE`、`KEY_UNKNOWN`）。
4. 一个节点最多属于一个聚合块，属于聚合块的节点自己不再是行；聚合块的 `count` 等于成员数。
5. 缩放根下的每个节点，要么是一行，要么是聚合块的成员，要么在某个已画行之下；被丢弃的极小块除外：树图里沿容器短边不超过 20 px，分区布局里不超过 2·gap + 2 px。真实行的 `size` 等于 `sizes.of(node)`，聚合块等于成员之和，可用 / 未知空间等于缩放根的值。分区布局的缩放根是文件，或画布连一层都放不下时，没有行，只靠 `FrameInfo.root` 画根条或中心圆。
6. 只有带标签位、且 `kind` ≤ 3 的行才有名字，名字就是节点名。
7. 分区布局（`flame.v1`、`polar.v1`）每帧最多 `MAX_ROWS = 50 000` 行。
8. 几何与布局相符：几何列是 `<i4`、每行一个值，`meta` 是这种布局的模型；块落在画布上、子块在父块之内（火焰图只看 x）。分区布局里同一层的行有相同的 `y,h`（或 `r0,r1`），按 `x`（或 `a0`）递增且不重叠；极坐标里 `0 ≤ a0 < a1 ≤ unit`，子扇区的 `r0` 不小于父扇区的 `r1`。

兄弟块共享边界值，严丝合缝；子块一定落在父块之内。

## 4. 布局一览

| 布局 | 几何列（`<i4`，物理像素） | `FrameInfo.geometry` | `FrameInfo.root` | kind 1 |
|---|---|---|---|---|
| `drawlist.v1`（树图，冻结） | `x y w h` | null | null（缩放根就是整个画布） | 框加标题栏，子块在框内 |
| `flame.v1` | `x y w h` | `FlameGeometry{kind:"flame", origin, row_h, rows, gap, root}` | 根条 | 已展开的实心条，子条在下一带 |
| `polar.v1` | `a0 a1 r0 r1` | `PolarGeometry{kind:"polar", cx, cy, radius, hole, ring_w, rings, gap, unit}` | 中心圆 | 已展开的实心扇区，子扇区在下一环 |

通用列（`key node parent kind depth cls label size count flags tag tone branch name_len`，对比视图另有 `delta dstat`）和每个字段的含义见 protocol.md 4.1–4.4。`FrameRoot` 是缩放根本身（它不是一行），字段与同名列相同。`flash[].row = −1` 表示缩放根，只在分区布局里出现。

## 5. 分区引擎（`layout/partition.py`）

火焰图和火焰-极坐标图共用一个引擎，只是投影不同。每行有一个归一化区间 `[u0, u1) ⊂ [0, 1)`（相对缩放根的整宽或整圈）和一个层号（缩放根的子项为 1）。投影（`Projection`）负责：`levels`、`columns`、`unit_px(level)`（这一层 [0, 1) 对应多少像素）、`cell(a, b, level)`（区间落到像素，画不下返回 None）、`expandable(cell, level)`、`label(cell, kind)`、`meta()`。

```
container(c, ids, extra, u0, u1, level, parent_row, page):
  s = sizes.of(ids)，去掉 s ≤ 0 的项
  span = u1 − u0
  page（聚合放大）: items = page_items(...)，basis = Σ 权重
  否则:            basis = max(Σs + Σextra, c 自身的大小)     # 文件夹自身的占用在子项之后留空
                   items = merge_items(...)，阈值 = min_element·dpr · basis / (unit_px(level)·span)
  累计字节，区间端点 = u0 + 累计/basis·span（最后一项正好到 u1）
  cell 为 None → 丢弃；文件夹 → 等待扫描 | 展开（有子项、层号 < levels、expandable、行数够）| 折叠
  展开的文件夹 → container(node, basis.children(node), [], a, b, level + 1, row)
```

- 卷根时可用 / 未知空间作为 `extra` 排进第 1 层（`Visualization.free_space` 为假时没有）。
- 小项聚合（G5）与树图同一套规则，只是用长度代替面积：沿这一层不到最小元素尺寸的项并入「N 个小项」「+N 项」。长度与字节成正比，所以一个容器只有一个字节阈值；极坐标里长度是环中线处的弧长，越往外环阈值越小，深层的小文件夹因此在外圈形成细刺。
- 等待扫描的文件夹从不展开。
- 行数严格不超过 `max_rows`：文件夹的子项先合并再发出文件夹行，还没排到的兄弟预留名额；放不下子项的文件夹画成折叠块，它的聚合块成员也一并去掉。
- 尺寸一律走 `sizes.of(ids)`，类别走 `basis.cls_of(ids)`，子项走 `basis.children(node, sizes.n)`：每帧没有 O(N) 的计算或分配。

## 6. 火焰图投影（`flame.v1`，`layout/flame.py`）

记号：lu = dpr·字号缩放，g = max(1, round(2·dpr))，m = min_element·dpr，W × H 为画布物理像素。常量与令牌 `visual.flame.*` 相同（`rowMin` 20、`rowMax` 48、`gap` 2，单位 CSS px）。

- **层数和带距**：row_min = round(20·lu)，row_max = round(48·lu)；n = max(0, min(detail, H // row_min − 1))；row_h = min(H // (n + 1), row_max)。条高 h = row_h − g。
- **第 k 带**（根条为第 0 带）：根在下（`origin = "bottom"`，默认，经典火焰图）时 y = H − k·row_h − (row_h − g)；根在上时 y = k·row_h。
- **x 方向**：x = round(a·(W + g))，w = round(b·(W + g)) − g − x。范围多算一个缝，所以最右边的条正好到 W，子条的右边缘与父条对齐。
- unit_px = W + g；可见：w ≥ round(2·dpr)；可展开：w ≥ m。
- 标签：`label_box(w/lu, h/lu, big=False)`（没有大号字）。
- 根条：`geometry.root = (0, y₀, W, row_h − g)`，按 `FrameInfo.root` 画。

## 7. 火焰-极坐标图投影（`polar.v1`，`layout/flame_polar.py`）

火焰图的极坐标投影：根在圆心，层号越大越往外。常量与令牌 `visual.flamePolar.*` 相同（`margin` 8、`holeRatio` 0.24、`holeMin` 36、`holeMaxRatio` 0.4、`ringMin` 14、`gap` 2、`segments` 32）。

- **圆心和半径**：cx = W // 2，cy = H // 2，R = min(W, H) // 2 − round(8·dpr)。
- **中心孔**：hole = min(max(round(0.24·R), round(36·lu)), round(0.4·R))。
- **环**：n = max(0, min(detail, (R − hole) // round(14·lu)))；ring_w = (R − hole) // n；radius = hole + n·ring_w。第 k 环的格子是 [hole + (k−1)·ring_w, hole + k·ring_w)。
- **半径的取整**：画出的 r0 = 格子下沿 + ⌈g/2⌉，r1 = 格子上沿 − ⌊g/2⌋。中心圆的半径为 hole − ⌊g/2⌋。
- **角度编码**：每圈 `unit` = 2^20 = 1 048 576 单位，0 为 12 点钟方向，屏幕坐标下顺时针递增；a0 = round(a·2^20)，a1 = round(b·2^20)。所有值都小于 2^24，在着色器的 f32 里也是精确的。a1 − a0 = 2^20 的满圈扇区没有径向边。
- unit_px(k) = 2π·r_mid(k)，r_mid = (r0 + r1)/2；可见：弧长 Δ·r_mid ≥ 2·dpr；可展开：弧长 ≥ m。
- **角向的缝**不烤进角度，由前端画。记 Δ = (a1 − a0)/unit·2π：两条径向边各沿垂直方向收进 `min(g/2, max(0, (Δ·r0 − 1)/2))` 像素，所以最细的刺在内半径处也至少留 1 px；满圈不收进。

### 7.1 `sectorDistance` 参考实现

前端的命中、描边和着色器都用同一个有符号距离（内部为负）。t0 < t1 ≤ t0 + 1 以圈为单位：

```ts
function sectorDistance(px, py, s /* cx cy t0 t1 r0 r1 */, gap) {
  const x = px - s.cx, y = py - s.cy, r = Math.hypot(x, y);
  const radial = Math.max(s.r0 - r, r - s.r1);
  const span = (s.t1 - s.t0) * 2 * Math.PI;
  if (span >= 2 * Math.PI) return radial;                        // 满圈：没有径向边
  const th0 = s.t0 * 2 * Math.PI, th1 = s.t1 * 2 * Math.PI;
  const d0 = x * Math.cos(th0) + y * Math.sin(th0);             // 到两条边线的有符号距离，扇区一侧为正
  const d1 = -(x * Math.cos(th1) + y * Math.sin(th1));
  const edge = span <= Math.PI ? -Math.min(d0, d1) : -Math.max(d0, d1);  // 半圈以内取交，超过取并
  const seam = Math.min(gap / 2, Math.max(0, (span * s.r0 - 1) / 2));
  return Math.max(radial, edge + seam);
}
```

着色器把每个扇区细分成 32 段三角形带，外半径按 1/cos(半段角) 放大再加 1 px（加上描边外扩和阴影模糊），盖住整段弧；片元着色器按这个距离做填充、（虚线）边框、条纹 / 点阵和阴影，抗锯齿 1 px。

### 7.2 标签

标签位由后端算好，前端只按位摆放、用同一公式算最大宽度。记 T = r1 − r0，L = Δ·r_mid − g：

- **沿半径**（T > L）：位为 `label_box(T/lu, (Δ·r0 − g)/lu)`；有名称位就给 名称 | 单行 | 径向（1 | 8 | 16），否则不画。前端从 r0 + 4·dpr 起沿角平分线写，最大宽度 T − 8·dpr；右半圈从内往外读、左对齐，左半圈转半圈、右对齐到内沿。
- **沿切线**：可用宽度 Wt = min(L, 2·√max(0, (r1 − round(2·dpr))² − r_mid²))，位为 `label_box(Wt/lu, T/lu, big=False)`；前端最大宽度 Wt − 12·dpr。锚点在 (r_mid, 角度中点)，落在下半圈时转半圈，保证字是正的（旋转角始终在 ±90° 内）；有大小位时两行沿半径方向各偏 ±7·字号缩放·dpr，名称在前。
- **中心圆**：缩放根的名称（粗体 13 px）和大小（等宽 11.5 px），最宽 1.6·hole；悬停某一行时换成它的名称、大小和「占 {pct}」（占缩放根的比例）；放不下的行从最后一行起丢掉。

## 8. 根条与中心圆

缩放根在分区布局里不是一行，按 `FrameInfo.root` 画成火焰图的根条或火焰-极坐标图的中心圆：

- **外观**：底色 `visual.center.bg`（即树图第 0 层标题栏色），文字 `visual.center.text` / `visual.center.sub`；根条上是粗体的最后一节面包屑，大小紧跟在后。带 DIM 位时暗化，带 64 位（扫描中）时和文件夹一样呼吸，有标签时显示标记。
- **命中**：`hitTest` 返回 `HIT_ROOT = −2`（火焰图：第 0 带、且在根条的 x 范围内；极坐标：r < hole）。
- **操作**：单击取消选中（详情面板此时显示缩放根）；双击 = `view.up`；右键先取消选中，再弹出缩放根的右键菜单（没有「只看这里（放大）」）；悬停显示缩放根的悬停卡。
- **闪烁**：分区布局里落不到任何行、但在缩放根之内的变化，闪根条或中心圆（`flash.row = −1`）。

## 9. 配色

颜色由前端按共享令牌从通用列和配色模式算出，三种可视化同一套规则：

| 配色 | 文件、聚合块 | 折叠的文件夹 | 已展开的文件夹 |
|---|---|---|---|
| 类别 / 扁平 / 时间 | 同树图（类别色、扁平色、修改时间档） | 树图：折叠色；分区布局：`visual.ring.{min(depth,4)}` | 树图：层级色框；分区布局：`visual.ring.{min(depth,4)}` |
| 分支 | `visual.branch.{槽位}.{min(depth,4)}`，槽位 0 用 `visual.branch.neutral` | 同左 | 树图：层级色框；分区布局：同左 |
| 对比视图 | 色阶优先于以上配色（没有变化的为折叠色） | 同左 | 树图：层级色框；分区布局：同左（`filled()` 全为真，色阶也按它们分档） |

- **分支槽位的分配**（`view/branch.py`）：按缩放根下第 1 层的 key 记住槽位，首次出现时分配：取在视野内用得最少、且与左右已分配的邻居都不同的槽位，平局取编号小的。缩放根一变就清空；最多记 4096 项，超出时淘汰最久未画的；节点 id 重映射时迁移，换 store 时清空。第 1 层的聚合块、可用 / 未知空间为 0（中性）。
- **往下传**：`branch[i] = branch[parent[i]]`，深浅档取 min(depth, 4)：第 1 档淡色，第 2 档最饱和，第 3–4 档逐级变暗。`branch` 列总是发送。
- 分支色板 8 色 × 4 档，每档配白字或深字对比度都 ≥ 4.5:1，避开闪烁黄和强调色；**待设计评审与色盲检查**。
- **每种可视化各自记住配色**：火焰图和火焰-极坐标图默认「分支」（设置 `appearance.partition_color_mode`），树图默认「类别」（`appearance.default_color_mode`）；切回某种可视化时恢复它上次的配色。
- 分区布局的祖先光晕是在祖先段上叠一层 `visual.haloOverlay`；悬停描边、选中环、选中投影、暗化（`dimOpacity` / 0.45 / 0.55）、文件夹掩码命中加粗、条纹（等待扫描 / 可用 / 未知空间）与树图相同。选中项在自己的投影之上再画一遍，层叠最高。

## 10. 能力与版本

| 能力 | 含义 |
|---|---|
| `visuals.v1` | 理解 `view.set{visual}`、`visual.list`、`FrameInfo.visual/layout/geometry/root`、配色 `branch` |
| `layout.flame.v1` | 能解码并画 `flame.v1` |
| `layout.polar.v1` | 能解码并画 `polar.v1` |

- `drawlist.v1` 是基线，不需要能力；后端只把会话协商过的布局发给它。
- 前端 hello 里的能力由前端注册表生成：基础四项加 `visuals.v1` 和每个已注册组件的能力。
- 给布局加可选列是兼容的（解码方按列名取列）；改变已有列或几何字段的含义，要升布局版本（如 `flame.v2`）并配新的能力名。`drawlist.v1` 已冻结。
- `PROTOCOL_VERSION` 为 1.1，握手只比较主版本号，1.0 的客户端照常连接、只看到树图。

## 11. 前端组件接口（`visual/types.ts`，`VISUAL_API = 1`）

```ts
interface VisualComponent<F> {
  layout: string; api: 1; capability: string | null;
  hoverPlacement: "beside" | "pointer";      // 悬停卡放在块旁，或跟随指针（大扇区的外接框可能是整个圆）
  geometry: VisualGeometry<F>;
  create(ctx: VisualContext): VisualInstance<F>;
}
interface VisualGeometry<F> {                 // 纯函数，物理像素
  hitTest(f, px, py): Hit;                    // 行号、HIT_ROOT = −2 或 HIT_NONE = −1
  bounds(f, hit): Box | null;
  anchor(f, hit, "label" | "tag" | "badge"): Pt | null;
  box(f, hit): { major, minor };              // 长边和短边：呼吸挑选、最小尺寸、排序
  path(f, hit, inset, out: PathSink): boolean;// 轮廓，正 inset 向内收
  filled(f, row): boolean;                    // 有自己的填充（对比色阶只算这些）
}
interface VisualInstance<F> {
  setFrame(f, s: PaintSettings): void;        // 新帧对象 = 新帧；同一帧对象 = 只改了绘制设置
  setHover(hit): void;
  draw(p: { now, alpha, zoom: { row, t } | null }): boolean;  // 不清屏
  dispose(): void;
  contextRestored(): void;
}
```

- **帧类型**：外壳代码只看到不含几何的 `VisualFrame`（通用列、`names`、`rowOf(key)`）；`RectFrame` 带 `x y w h`，`PolarFrame` 带 `a0 a1 r0 r1`。`decodeVisualFrame` 按二进制帧头的布局解码，不认识的布局返回 null。
- **锚点**：`label` 是标签中心；`tag` 是组件自己画标签标记的位置（树图和火焰图为标记左上角，极坐标为标记中心）；`badge` 是角标的中心，宿主画「减少动画」下的静态变化角标时以它为中心、再收进 `bounds`（树图的锚点是块的右下角，角标因此落在右下角）。
- **注册**（`visual/registry.ts`）：`registerVisual(c)` 遇到 API 版本不同、没有解码器的布局、重复的布局时抛错；`componentFor(layout)`、`registeredLayouts()`、`visualCapabilities()`。内置组件在 `visual/builtins.ts` 登记，连接之前 import。
- **宿主**（`visual/Host.tsx`）：一个视图一个 WebGL2 上下文和三层画布（GL、文字、呼吸），程序经 `ctx.shared(key, make)` 缓存，上下文丢失后重建并用最后一帧重画，卸载时 `WEBGL_lose_context`。每个显示帧宿主先清屏、设好混合，再依次调用实例的 `draw`：
  - 放大：旧实例 `draw({zoom: {row, t: 缓出}, alpha: 1 − 淡出})`，新实例 `draw({alpha: 淡入})`；返回时反过来，新实例从被点的行收回。
  - 切换可视化（reason 为 `visual`，或前后帧的 visual / layout 不同）：交叉淡入，时长 `motion.duration.swap` = 200 ms；「减少动画」时直接换。旧实例在动画结束后释放。
  - 每个组件自己定义「把这一行展开到满屏」：树图是矩形仿射；火焰图 sx = root.w/w、sy = 1，平移到根条所在的带，文字只移动锚点；火焰-极坐标图是极坐标仿射 [sθ, tθ, sr, tr] = [1 + (1/Δ − 1)t, −(t0/Δ)t, 1, −k·ring_w·t]（Δ、t0 以圈计，k 为环号），被点的扇区展开到满圈并收进中心圆。
  - 叠加层（闪烁、拖尾、呼吸）都用 `path()` 描边：闪烁 inset = dpr，拖尾 dpr/2，呼吸 lineWidth/2；呼吸挑 `box.minor ≥ 12·dpr` 的行，最多 300 个，缩放根带 64 位时也呼吸。
  - `visual/hosts.ts` 对外提供 `menuAnchor(row)`（客户端像素，Shift+F10 和菜单键用）和 `boundsCss(row)`。
- **帧通路**：`applyFrame` 先看 `FrameInfo.layout`，前端画不了就保留旧画面、提示一次「这个界面画不了这种可视化，已换回树图」并发 `view.set{visual: "treemap"}`；二进制帧按（stream, seq）配对，最多缓存 32 个，等待 10 秒超时，不认识的布局让等待者以 null 结束。

**一致性测试**（`visual/testing/conformance.ts` 的 `checkGeometry`，每个组件在它的样例场景上跑）：

- 短边 ≥ 4 px 的每一行（以及有 `info.root` 时的缩放根）：标签锚点不为 null；锚点处的 `hitTest` 是这一行或它的子孙（缩放根为 `HIT_ROOT`）；`bounds` 包含标签、标记和角标锚点；`path` 为真，轮廓上的点在 `bounds` 内（容差 1 px）。
- 画布外的点给 `HIT_NONE`；`HIT_NONE` 没有 bounds、锚点和轮廓；空帧不抛异常。
- 几何列的长度等于行数，组件的布局与帧相同。

此外极坐标的命中按格子算（r < hole 为缩放根；k = ⌊(r − hole)/ring_w⌋ + 1；在第 k 环里取最后一个 a0 ≤ u 的扇区，u < a1 时命中），测试与暴力查找逐点比对，覆盖 12 点钟回绕、缝和中心孔。前端测试的样例帧（`visual/testing/fixtures.ts`）照抄后端的投影公式，后端公式改了要同步改。

## 12. 性能与内存预算

- 分区布局每帧最多 `MAX_ROWS = 50 000` 行，每行 53 字节（通用列 37 字节加 4 个几何列）另加名字，一帧的列不超过约 2.7 MB。
- 分区引擎每帧没有 O(N) 的尺寸、类别或子项计算，只访问画出的行和被合并的兄弟；行缓冲每帧新建，不跨帧持有 store 的引用。树图仍用 `sizes()` 的全量缓存，行为与之前逐字节相同（金样测试守住）。
- 行数约为 Σ 每层像素长度 / 最小元素尺寸。实测（100 万节点合成树，内存中，取 3 次最好成绩，最小元素 10 px，2026-10-19，Windows）：

| 布局 | 1080p 层级 5 | 1080p 层级 15 | 4K 层级 5 | 4K 层级 15 |
|---|---|---|---|---|
| `flame.v1` | 0.8 ms（56 行） | 8.5 ms（365 行） | 0.8 ms（63 行） | 23.9 ms（781 行） |
| `polar.v1` | 0.8 ms（60 行） | 13.2 ms（485 行） | 0.9 ms（67 行） | 29.2 ms（908 行） |

  目标 ≤ 60 ms，验收线 < 120 ms（M2）。复测：`uv run python tools/bench_m2.py --visual flame,flame_polar --nodes 1000000 --budget 120`。
- 已知的遗留项：扫描推送时 `sizes()` 缓存每次都失效（树图的全量路径）；隐藏模式仍按全长数组构造尺寸；`_scanning_rows` 仍是 O(N)。

## 13. 插件

覆盖前后端的插件系统见 [plugins.md](plugins.md)，写插件见 [plugin-dev.md](plugin-dev.md)「可视化」「配色」；本节只说可视化这一类扩展。

**现在有**
- 后端 `ENGINE_API`、`Engine` 协议、`Visualization` 描述符、注册表、行契约检查；内置可视化和插件走同一条 `register()`（扩展注册表的 `visualization` 点）。服务端的能力从注册表推导，不再是固定的列表。
- 插件的可视化是**变体**：选一个内置引擎（`base = treemap | flame | flame_polar`），换一种面积口径（`size = alloc | size | files | items`），或者由插件的函数给每个文件一个权重（`size = "code"`，在插件进程里算，宿主加到文件夹上，按扫描版本缓存，每帧 120 ms 的预算，失败三次后这次会话不再用）。布局格式仍然只有我们维护的三种，`size_unit` 决定标签怎么写：`bytes` 写大小，`count` 写项数，`weight`（`size = "code"` 的默认，也可以用 `unit` 指定）写这一行占缩放根权重的比例（例如「12%」），悬停卡片另写这一项在磁盘上的真实大小。权重不是字节，所以不会按字节格式化。
- 配色模式也可以来自插件：声明式的规则由宿主算，代码的由插件给每个画出的行一个色板下标，帧里多一列 `pal`（`colors.v1`）。
- `plugin check` 和 `MockHost.visual()` 用 `check_draw(full=True)` 检查插件可视化的每一帧；`test_layout_contract.py` 照旧覆盖内置引擎。
- 前端 `VISUAL_API`、`VisualComponent`、注册表、一致性测试辅助不变（前端组件不对插件开放）。

**以后做**
- 插件自己的布局引擎（远程画行），以及 WASM 运行时。
- 新的布局格式（时间轴、关系图、表格）：三端各实现一个组件之后才能开放。
