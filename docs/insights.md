# SpaceKnife 洞察规则（格式 1）

洞察规则库回答一个问题：**这个文件夹（或文件）是什么，能不能清理，怎么清理。** 它驱动两处界面：

- 详情栏的洞察卡（[UI-SPEC](UI-SPEC.md) UI-MAIN-41 / 42）：标题、说明、所属应用、清理方式与命令；
- 快照对比里的变化原因注释（UI-W6-30）：名称后的灰色短注，如 Cyberpunk 2077 后的「游戏更新」。

接口契约见 [manage.md](manage.md) 第 5 节。实现在 `packages/backend/src/spaceknife/insights/`：引擎 `rules.py`；内置规则 `builtin.toml` 是内置插件 `builtin.insights` 的规则文件（`packages/backend/src/spaceknife/plugins/builtin/builtin.insights/insights/builtin.toml`）。插件也能带规则文件（[plugin-dev.md](plugin-dev.md)「洞察规则」）。

## 1. 规则从哪里来

| 来源 | 位置 | 说明 |
|---|---|---|
| 内置规则 | 安装包内的 `spaceknife/plugins/builtin/builtin.insights/insights/builtin.toml` | 随版本更新，不要修改 |
| 插件的规则 | 各插件的 `insights/*.toml` | 按插件 id 排在内置规则之后、用户规则之前 |
| 用户规则 | 配置目录下的 `insights/*.toml`（`spaceknife.insights.user_rules_dir()`；设置了 `SPACEKNIFE_HOME` 时在 `$SPACEKNIFE_HOME/config/insights/`） | 按文件名顺序加载，后加载的优先 |

- **按 id 覆盖**：用户规则与内置规则 id 相同时，整条替换内置规则（不是逐字段合并）。
- **禁用**：写一条只有 `id` 和 `enabled = false` 的规则，就去掉了同 id 的内置规则（或前面文件里的规则）。
- **出错不影响其他规则**：无法读取的文件、TOML 语法错误、不支持的格式版本会跳过整个文件；字段有问题的规则只跳过这一条。每个问题都写一条警告日志（日志面板可见），并保留在 `RuleSet.problems` 里。
- 同一文件里 id 重复：以最后一条为准，并报告一次。

## 2. 文件格式

```toml
version = 1                      # 可省略；不是 1 的文件整个跳过

[[rule]]
id = "steam-game"                # 必填：字母、数字和 . _ -，以字母或数字开头
kind = "game"                    # 必填：app / cache / build / vm / chat / game / system
title = "Steam 游戏"             # 必填：洞察卡标题
reason = "通过 Steam 安装的游戏。在 Steam 中卸载……"   # 必填：是什么、能否清理、怎样清理
note = "游戏更新"                # 可选：名称后的短注（对比模式）；省略时等于 title
app = "Steam 游戏"               # 可选：所属应用（洞察卡副标题）
cleanup = "tool"                 # 可选：safe / tool / review / keep，默认 review
node = "dir"                     # 可选：file / dir / link / other / any，可写成列表，默认 any
paths = ["steamapps/common/*"]   # paths 与 names 至少有一个
platforms = ["windows", "linux", "macos"]   # 可选，默认全部
priority = 5                     # 可选：-1000 … 1000，默认 0
i18n.en.title = "Steam game"     # 可选：其他语言的文案
```

全部字段：

| 字段 | 类型 | 默认 | 含义 |
|---|---|---|---|
| `id` | 字符串 | 必填 | 规则标识，用于覆盖与禁用；也是界面「了解如何安全回收」帮助条目的键 |
| `kind` | 字符串 | 必填 | 分类：`app` 应用数据、`cache` 缓存、`build` 开发依赖与构建输出、`vm` 虚拟机与容器、`chat` 聊天数据、`game` 游戏、`system` 系统文件 |
| `title` | 字符串 | 必填 | 标题 |
| `reason` | 字符串 | 必填 | 说明：它是什么，能不能删，怎样安全地清理 |
| `note` | 字符串 | `title` | 名称后的灰色短注，越短越好（2–6 个字） |
| `app` | 字符串 | 空 | 所属应用 |
| `cleanup` | 字符串 | `review` | `safe` 可以直接删除（会自动重建或重新下载）；`tool` 用应用自己的功能或命令清理；`review` 含有个人数据或名字不确定，确认后再删；`keep` 系统或应用必需，不要手动删除 |
| `command` | 字符串 | 空 | 推荐的清理或查看命令 |
| `regenerate` | 字符串 | 空 | 删掉以后它怎么回来（「在项目里运行 npm install 会装回来」）：卡片单独写一行 |
| `paths` | 字符串或列表 | 空 | 路径通配符（第 3 节） |
| `names` | 字符串或列表 | 空 | 名称通配符：`names = ["x"]` 等于 `paths = ["**/x"]`，不能含路径分隔符 |
| `parents` | 字符串或列表 | 空 | 上级文件夹的名称通配符，满足其一才算匹配 |
| `exclude` | 字符串或列表 | 空 | 路径通配符；匹配到的项落在其中任意一个里就不算 |
| `contains` | 字符串或列表 | 空 | 名称通配符（不分大小写）：匹配到的文件夹里要有其中一个名字，如虚拟环境的 `pyvenv.cfg`（第 4.6 节） |
| `beside` | 字符串或列表 | 空 | 名称通配符：匹配到的项旁边（同一个文件夹里）要有其中一个名字，如 `node_modules` 旁边的 `package.json`（第 4.6 节） |
| `platforms` | 字符串或列表 | 全部 | `windows` / `linux` / `macos`，以及 `posix`（= linux + macos）、`all` |
| `node` | 字符串或列表 | `any` | 匹配哪类节点：`file` / `dir` / `link` / `other` / `any` |
| `min_size` | 整数或字符串 | 0 | 大小提示：已知大小且小于它时不匹配；整数是字节，字符串如 `"500 MB"`、`"2 GiB"`、`"10m"`（1 KB = 1024 B） |
| `priority` | 整数 | 0 | 排名第一关键字（第 4.4 节） |
| `subtree` | 布尔 | `true` | 是否同时覆盖文件夹里的所有内容（第 4.3 节） |
| `enabled` | 布尔 | `true` | `false` 表示删除同 id 的规则 |
| `i18n.<语言>.<字段>` | 字符串 | — | 其他语言的 `title` / `reason` / `note` / `app` / `regenerate`（第 5 节） |

未知字段会让这条规则作废（多半是拼写错误，例如把 `platforms` 写成 `platform`，静默忽略会让规则在所有平台生效）。

## 3. 通配符

- 分隔符写 `/` 或 `\` 都可以，含义相同。
- **相对模式在任意层级匹配**：`node_modules`、`.cache/pip`、`steamapps/common/*` 都可以出现在路径的任何位置（等于在前面加 `**/`）。
- **绝对模式从根开始**：`/var/cache/apt/archives`（POSIX）；`C:/Windows/WinSxS`，或用 `?:/Windows/WinSxS` 表示任意盘符。
- 模式要匹配**整段路径**（相对模式从某一层开始一直到末尾），不是包含关系。
- 不支持 `~`：主目录随用户、平台和快照来源而变，用相对模式（`.cache/pip`）即可。

| 写法 | 含义 |
|---|---|
| `*` | 一段名称里的任意字符（不跨越 `/`） |
| `?` | 一个字符（不是 `/`） |
| `**` | 整段写时匹配任意多层文件夹（含零层）：`a/**/b`；写在末尾 `a/**` 表示 a 里面的一切 |
| `[abc]` `[a-z]` `[!0-9]` | 字符集合 / 范围 / 取反（`[^…]` 同 `[!…]`） |
| `{a,b,c/d}` | 展开为多个模式，可以嵌套；展开后最多 256 个 |
| `[*]` `[?]` `[[]` `[{]` | 字面的 `*` `?` `[` `{` |

`names` 和 `parents` 用同样的语法，只是只能写一段名称。

## 4. 匹配语义

### 4.1 路径形式决定平台

- 盘符（`C:\…`）、UNC（`\\server\share\…`）和 `\\?\` 长路径属于 **Windows** 形式，只用适用于 Windows 的规则，且**不区分大小写**；`\\?\` 前缀会先去掉，`/` 与 `\` 等价。
- 其他路径属于 POSIX 形式，用宿主平台的规则：**Linux 区分大小写**，**macOS 不区分**（APFS 默认）。在 Windows 上打开 Linux 或 macOS 的快照时，POSIX 路径同时用 Linux 和 macOS 的规则。
- 末尾的分隔符会被忽略。

### 4.2 节点类型与大小

`match(path, kind)` 的 `kind` 是节点类型：`file` / `dir` / `link` / `other`（或节点库的 `Kind`）。规则的 `node` 不包含它就不匹配。`min_size` 只在调用方给出大小时生效，且只作用于该项本身。

### 4.3 文件夹里的内容（subtree）

`subtree = true`（默认）的文件夹规则也覆盖文件夹里的一切：`…\node_modules\react\index.js` 得到 `node-modules` 的洞察，`Insight.inside` 为真，`Insight.path` 是那个 `node_modules` 文件夹。`node = "file"` 的规则没有「里面」。

### 4.4 多条规则都匹配时

一条路径本身和它的每一级上级文件夹都可能匹配到规则。胜出的是：

1. `priority` 最高的；
2. 同优先级时，离该项最近（最深）的匹配；
3. 再相同时，模式最具体的（字面字符多、整段字面、绝对模式更具体）；
4. 再相同时，后加载的文件里的规则（用户规则胜过内置规则）。

胜出的规则如果在更上层也匹配，报告**最外层**的那一处：`a/node_modules/x/node_modules/y` 属于 `a/node_modules`。

内置规则的优先级分档：

| 优先级 | 用于 | 例子 |
|---|---|---|
| 10 | 系统文件与系统目录 | `hiberfil.sys`、WinSxS、Windows.old、交换文件 |
| 5 | 归某个应用整体所有的目录：里面的一切都属于它 | Docker / 虚拟机目录、游戏安装目录、聊天数据、SDK、编辑器扩展 |
| 1 | 开发依赖容器 | node_modules、虚拟环境、conda 环境、`.git` |
| 0 | 默认 | 各类缓存 |
| -5 | 宽泛的容器和通用文件类型 | `~/.cache`、`~/Library/Caches`、`*.vhdx`、`__pycache__` |
| -10 | 通用的构建目录名 | `build`、`dist`、`target` |

所以 Steam 游戏目录里的 `node_modules` 仍是「Steam 游戏」，虚拟环境里的 `build` 仍是「Python 虚拟环境」，`~/.cache/pip` 是「pip 缓存」而不是「用户缓存目录」。

### 4.5 结果

```python
@dataclass(frozen=True) class Insight:
    rule: str; title: str; reason: str; kind: str      # 契约字段（manage.md 第 5 节）
    note: str        # 短注（未写时等于 title）
    app: str         # 所属应用
    cleanup: str     # safe / tool / review / keep
    command: str     # 清理命令（只展示）
    path: str        # 匹配到的项：路径本身，或包含它的那个文件夹（本平台的分隔符）
    inside: bool     # 是否经由上级文件夹匹配
    regenerate: str  # 删掉以后它怎么回来（规则的 regenerate）
```

### 4.6 看邻居和内容（contains、beside）

`contains` 和 `beside` 要看树：匹配到的那一项（经由上级匹配时是那个上级文件夹）里有没有、旁边有没有这些名字。`match_node` / `match_nodes`（详情面板、对比原因、洞察配色都用它们）从节点所在的树里看；只有路径的 `match` / `match_many` 没有树可看，带这两个条件的规则对它们不匹配。树的根以上没有可看的东西：扫描根本身的 `beside` 不成立。

```toml
[[rule]]
id = "my-node-modules"
kind = "build"
title = "项目的依赖"
cleanup = "safe"
reason = "项目装的依赖包。"
regenerate = "在项目里运行 npm install 会装回来"
names = ["node_modules"]
beside = ["package.json"]          # 全局的 npm-global/node_modules 旁边没有它：不算
priority = 3
```

## 5. 多语言

基本字段（`title` / `reason` / `note` / `app`）是规则的默认文案；其他语言写在 `i18n.<语言代码>` 下：

```toml
i18n.en.title = "Steam game"
i18n.en.reason = "A game installed through Steam. …"
i18n.zh-tw.title = "Steam 遊戲"
```

- 查询时传 `lang`（`en`、`en-US`、`zh_TW` 均可，不区分大小写）：先找完整代码，再找主语言（`en-US` → `en`），都没有就用默认文案。
- 逐字段回退：某语言缺 `reason` 时用默认的 `reason`；缺 `note` 时用该语言的 `title`（也没有就用默认的 `note`）。
- 内置规则的默认文案是简体中文，并全部带 `i18n.en`（测试保证）。

## 6. 写规则的建议

- `reason` 说三件事：它是什么；直接删除会怎样；推荐的清理方式。语气中性、陈述事实，不吓唬人，也不鼓动删除。
- 名字通用的目录（`build`、`target`、`dist`）用负的优先级，并在 `reason` 里提醒确认；用 `exclude` 排除系统与应用安装目录。
- 归某个应用所有的整个目录（虚拟机、游戏、聊天数据）用 5，免得里面的 `node_modules`、`build` 被误判成可删的开发产物。
- 优先写较长的字面路径（`AppData/Local/Docker`），少用以通配符结尾的短模式：每个模式都按它最有辨识度的字面部分建索引（最后一段名称、上级名称、扩展名或名称前缀），完全没有字面部分的模式每次查询都要测试，会拖慢速度。

例：把自己的渲染输出目录标成「可清理的缓存」，并关掉内置的 `__pycache__` 规则：

```toml
# <配置目录>/insights/my.toml
[[rule]]
id = "my-renders"
kind = "cache"
title = "渲染输出"
note = "渲染缓存"
cleanup = "safe"
reason = "Blender 的渲染中间文件，可以随时重新渲染。"
paths = ["Videos/Renders/**/*.exr", "D:/Renders"]
min_size = "100 MB"

[[rule]]
id = "pycache"
enabled = false
```

## 7. 接口（`spaceknife.insights`）

```python
def load_rules(extra_dir: Path | None = None, *, host: str | None = None) -> RuleSet
    # 内置规则 + extra_dir/*.toml；成为下面模块级函数使用的规则集。服务启动时传 user_rules_dir()
def match(path: str, kind: str | Kind, *, size: int | None = None, lang: str | None = None) -> Insight | None
def match_many(paths, kinds="dir", *, sizes=None, lang=None) -> list[Insight | None]   # 兄弟项共享上级的计算
def match_node(store: NodeStore, node: int, *, lang=None) -> Insight | None             # 用节点的路径、类型和占用
def match_nodes(store: NodeStore, nodes, *, lang=None) -> list[Insight | None]          # 同一棵树的多个节点
def compile_rules(sources: Iterable[tuple[str, str]], *, host=None) -> RuleSet           # 从内存中的 TOML 文本构建
def user_rules_dir() -> Path
class RuleSet: rules, problems, get(id), match / match_many / match_node（同上）
```

- 未调用 `load_rules` 时，模块级函数第一次使用会加载内置规则。
- `RuleSet` 构建后不可变，可以在多个线程里同时查询；`load_rules` 用新规则集整体替换旧的。
- 性能：一次查询是每段路径几次字典查找，加上少数几个预编译正则；1 万次查询约 0.1 秒（测试上限 0.2 秒）。

## 8. 内置规则一览

下表由 `builtin.toml` 整理，规则的完整匹配条件与文案以该文件为准（测试检查每个 id 都出现在这里）。

### 虚拟机与容器（`vm`，12 条）

| id | 标题 | 平台 | 优先级 | 清理 |
|---|---|---|---|---|
| `docker-desktop` | Docker Desktop 虚拟磁盘 | Windows | 5 | 用应用 / 命令清理 |
| `docker-desktop-mac` | Docker Desktop 虚拟磁盘 | macOS | 5 | 用应用 / 命令清理 |
| `container-engine` | 容器引擎数据 | Linux | 5 | 用应用 / 命令清理 |
| `wsl-distro-disk` | WSL 发行版虚拟磁盘 | Windows | 0 | 用应用 / 命令清理 |
| `hyperv-vms` | Hyper-V 虚拟机 | Windows | 5 | 用应用 / 命令清理 |
| `virtualbox-vms` | VirtualBox 虚拟机 | 全部 | 5 | 用应用 / 命令清理 |
| `vmware-vms` | VMware 虚拟机 | 全部 | 5 | 用应用 / 命令清理 |
| `parallels-vms` | Parallels 虚拟机 | macOS | 5 | 用应用 / 命令清理 |
| `utm-vms` | UTM 虚拟机 | macOS | 5 | 用应用 / 命令清理 |
| `virtual-disk-file` | 虚拟磁盘文件 | 全部 | -5 | 确认后再删 |
| `android-avd` | Android 模拟器 | 全部 | 5 | 用应用 / 命令清理 |
| `ios-simulators` | iOS 模拟器 | macOS | 5 | 用应用 / 命令清理 |

### 开发：依赖与构建输出（`build`，14 条）

| id | 标题 | 平台 | 优先级 | 清理 |
|---|---|---|---|---|
| `node-modules` | node_modules 依赖目录 | 全部 | 1 | 可直接删 |
| `python-venv` | Python 虚拟环境 | 全部 | 1 | 可直接删 |
| `conda-env` | conda 环境 | 全部 | 1 | 用应用 / 命令清理 |
| `pycache` | Python 字节码缓存 | 全部 | -5 | 可直接删 |
| `dev-tool-caches` | 开发工具缓存 | 全部 | 0 | 可直接删 |
| `js-framework-build` | 前端框架构建输出 | 全部 | 0 | 可直接删 |
| `visual-studio-cache` | Visual Studio 解决方案缓存 | 全部 | 0 | 可直接删 |
| `cargo-target` | 构建输出（target） | 全部 | -10 | 确认后再删 |
| `build-output` | 构建输出 | 全部 | -10 | 确认后再删 |
| `git-repo` | Git 仓库数据 | 全部 | 1 | 不要手动删 |
| `git-objects` | Git 对象库 | 全部 | 1 | 不要手动删 |
| `xcode-derived-data` | Xcode DerivedData | macOS | 0 | 可直接删 |
| `xcode-archives` | Xcode 归档 | macOS | 0 | 确认后再删 |
| `unity-project-cache` | Unity 项目缓存 | 全部 | 0 | 可直接删 |

### 缓存（`cache`，41 条）

| id | 标题 | 平台 | 优先级 | 清理 |
|---|---|---|---|---|
| `conda-pkgs` | conda 包缓存 | 全部 | 0 | 用应用 / 命令清理 |
| `xcode-device-support` | Xcode 设备支持文件 | macOS | 0 | 可直接删 |
| `unreal-ddc` | Unreal 派生数据缓存 | 全部 | 0 | 可直接删 |
| `pip-cache` | pip 缓存 | 全部 | 0 | 可直接删 |
| `uv-cache` | uv 缓存 | 全部 | 0 | 用应用 / 命令清理 |
| `npm-cache` | npm 缓存 | 全部 | 0 | 可直接删 |
| `pnpm-store` | pnpm 全局仓库 | 全部 | 0 | 用应用 / 命令清理 |
| `yarn-cache` | Yarn 缓存 | 全部 | 0 | 可直接删 |
| `bun-deno-cache` | Bun / Deno 缓存 | 全部 | 0 | 可直接删 |
| `cargo-registry` | Cargo 依赖缓存 | 全部 | 0 | 可直接删 |
| `go-cache` | Go 模块与构建缓存 | 全部 | 0 | 用应用 / 命令清理 |
| `maven-repo` | Maven 本地仓库 | 全部 | 0 | 可直接删 |
| `gradle-cache` | Gradle 缓存 | 全部 | 0 | 可直接删 |
| `nuget-cache` | NuGet 包缓存 | 全部 | 0 | 可直接删 |
| `homebrew-cache` | Homebrew 下载缓存 | Linux / macOS | 0 | 可直接删 |
| `cocoapods-cache` | CocoaPods 缓存 | macOS | 0 | 可直接删 |
| `compiler-cache` | 编译器缓存 | 全部 | 0 | 可直接删 |
| `jetbrains-cache` | JetBrains IDE 缓存 | 全部 | 0 | 可直接删 |
| `vscode-cache` | VS Code 缓存 | 全部 | 0 | 可直接删 |
| `chrome-cache` | Chrome 缓存 | 全部 | 0 | 可直接删 |
| `edge-cache` | Edge 缓存 | 全部 | 0 | 可直接删 |
| `firefox-cache` | Firefox 缓存 | 全部 | 0 | 可直接删 |
| `safari-cache` | Safari 缓存 | macOS | 0 | 可直接删 |
| `wechat-cache` | 微信缓存 | 全部 | 5 | 可直接删 |
| `discord-cache` | Discord 缓存 | 全部 | 0 | 可直接删 |
| `steam-shadercache` | Steam 着色器缓存 | 全部 | 0 | 可直接删 |
| `steam-downloading` | Steam 未完成的下载 | 全部 | 0 | 确认后再删 |
| `epic-vaultcache` | Epic 资源下载缓存 | Windows | 0 | 可直接删 |
| `huggingface-cache` | Hugging Face 模型缓存 | 全部 | 0 | 用应用 / 命令清理 |
| `torch-cache` | PyTorch 模型缓存 | 全部 | 0 | 可直接删 |
| `unity-cache` | Unity 全局缓存 | 全部 | 0 | 可直接删 |
| `adobe-media-cache` | Adobe 媒体缓存 | 全部 | 0 | 可直接删 |
| `ios-firmware` | iOS 固件安装包 | 全部 | 0 | 可直接删 |
| `windows-temp` | 临时文件 | Windows | 0 | 可直接删 |
| `explorer-thumbcache` | 缩略图缓存 | Windows | 0 | 可直接删 |
| `driver-installers` | 显卡驱动安装包 | Windows | 0 | 可直接删 |
| `apt-cache` | APT 软件包缓存 | Linux | 0 | 可直接删 |
| `dnf-cache` | DNF / YUM 软件包缓存 | Linux | 0 | 可直接删 |
| `posix-temp` | 临时目录 | Linux / macOS | 0 | 确认后再删 |
| `xdg-cache` | 用户缓存目录 | Linux | -5 | 确认后再删 |
| `macos-caches` | 应用缓存 | macOS | -5 | 确认后再删 |

### 应用数据（`app`，14 条）

| id | 标题 | 平台 | 优先级 | 清理 |
|---|---|---|---|---|
| `uv-python` | uv 管理的 Python | 全部 | 5 | 用应用 / 命令清理 |
| `rustup-toolchains` | Rust 工具链 | 全部 | 5 | 用应用 / 命令清理 |
| `vscode-extensions` | VS Code 扩展 | 全部 | 5 | 用应用 / 命令清理 |
| `browser-profile` | 浏览器用户数据 | 全部 | 0 | 确认后再删 |
| `unreal-engine` | Unreal Engine | Windows / macOS | 5 | 用应用 / 命令清理 |
| `ollama-models` | Ollama 模型 | 全部 | 5 | 用应用 / 命令清理 |
| `lmstudio-models` | LM Studio 模型 | 全部 | 5 | 用应用 / 命令清理 |
| `android-sdk` | Android SDK | 全部 | 5 | 用应用 / 命令清理 |
| `unity-editors` | Unity 编辑器 | 全部 | 5 | 用应用 / 命令清理 |
| `ios-backups` | iPhone / iPad 备份 | 全部 | 5 | 确认后再删 |
| `photos-library` | 照片图库 | macOS | 5 | 不要手动删 |
| `outlook-ost` | Outlook 离线邮箱 | Windows | 0 | 确认后再删 |
| `snap-packages` | Snap 软件包 | Linux | 5 | 用应用 / 命令清理 |
| `flatpak` | Flatpak 应用与运行时 | Linux | 5 | 用应用 / 命令清理 |

### 聊天（`chat`，4 条）

| id | 标题 | 平台 | 优先级 | 清理 |
|---|---|---|---|---|
| `wechat-files` | 微信聊天数据 | 全部 | 5 | 用应用 / 命令清理 |
| `qq-files` | QQ 聊天数据 | 全部 | 5 | 用应用 / 命令清理 |
| `telegram-data` | Telegram 本地数据 | 全部 | 5 | 用应用 / 命令清理 |
| `telegram-downloads` | Telegram 下载的文件 | 全部 | 0 | 确认后再删 |

### 游戏（`game`，8 条）

| id | 标题 | 平台 | 优先级 | 清理 |
|---|---|---|---|---|
| `steam-library` | Steam 游戏库 | 全部 | 0 | 用应用 / 命令清理 |
| `steam-game` | Steam 游戏 | 全部 | 5 | 用应用 / 命令清理 |
| `steam-workshop` | Steam 创意工坊内容 | 全部 | 5 | 用应用 / 命令清理 |
| `steam-compatdata` | Proton 兼容层数据 | Linux | 5 | 确认后再删 |
| `epic-game` | Epic Games 游戏 | Windows / macOS | 5 | 用应用 / 命令清理 |
| `xbox-game` | Xbox 游戏 | Windows | 5 | 用应用 / 命令清理 |
| `gog-game` | GOG 游戏 | 全部 | 5 | 用应用 / 命令清理 |
| `launcher-game` | 游戏平台安装的游戏 | Windows | 5 | 用应用 / 命令清理 |

### 系统（`system`，17 条）

| id | 标题 | 平台 | 优先级 | 清理 |
|---|---|---|---|---|
| `windows-update-download` | Windows 更新下载缓存 | Windows | 10 | 用应用 / 命令清理 |
| `software-distribution` | Windows 更新数据 | Windows | 10 | 不要手动删 |
| `delivery-optimization` | 传递优化缓存 | Windows | 10 | 用应用 / 命令清理 |
| `windows-old` | 旧版 Windows 备份 | Windows | 10 | 用应用 / 命令清理 |
| `hiberfil` | 休眠文件 | Windows | 10 | 不要手动删 |
| `pagefile` | 虚拟内存分页文件 | Windows | 10 | 不要手动删 |
| `winsxs` | Windows 组件存储 | Windows | 10 | 不要手动删 |
| `windows-installer-cache` | Windows Installer 缓存 | Windows | 10 | 不要手动删 |
| `package-cache` | 安装程序包缓存 | Windows | 10 | 不要手动删 |
| `windowsapps` | Microsoft Store 应用 | Windows | 10 | 不要手动删 |
| `system-volume-information` | 系统还原点与卷影副本 | Windows | 10 | 用应用 / 命令清理 |
| `recycle-bin` | 回收站 | 全部 | 5 | 用应用 / 命令清理 |
| `crash-dumps` | 崩溃转储 | 全部 | 0 | 可直接删 |
| `linux-swapfile` | 交换文件 | Linux | 10 | 不要手动删 |
| `macos-swap` | 交换与休眠文件 | macOS | 10 | 不要手动删 |
| `macos-var-folders` | 系统临时与缓存目录 | macOS | 10 | 不要手动删 |
| `journal-logs` | systemd 日志 | Linux | 0 | 用应用 / 命令清理 |
