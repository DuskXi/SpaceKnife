# 用户文档 —— 写作约定

用户文档（官网 `/docs/`、`/zh/docs/`，软件内的帮助页）都来自同一批 Markdown 文件。改文档就是改这些 `.md`：官网由 Starlight 构建（`site/`），软件把同样的文件打包进帮助页。

## 1. 文件

- 英文：`site/src/content/docs/docs/<id>.md`；中文：`site/src/content/docs/zh/docs/<id>.md`。两种语言都要有，标题层级和小节顺序一致，事实一致（不是逐字翻译，各自写得自然）。
- 目录：`site/src/docs-nav.json`（分组、顺序、分组名的中英文）。官网侧栏和软件帮助页都按它排。新增页面：在这里加 id，再写两个 `.md`。
- 页面标题来自 frontmatter：

  ```md
  ---
  title: 过滤
  description: 一句话说明这一页讲什么（官网的搜索、SEO 和软件内搜索都用它）。
  ---
  ```

## 2. 能用的写法

两边都要能显示，所以只用这些：

- 标准 Markdown：标题从 `##` 开始（`#` 是页面标题），列表、表格、引用、行内代码、带语言的代码块。
- 提示块（Starlight 的写法）：`:::note`、`:::tip`、`:::caution`、`:::danger`，可带标题 `:::tip[标题]`，以 `:::` 结束。
- 按键：`<kbd>Ctrl</kbd>+<kbd>K</kbd>`。这是唯一允许的 HTML。
- 页内链接：相对路径，带结尾斜杠，同一语言：`[过滤](../filtering/)`、`[大小条件](../filtering/#大小)`。外部链接写完整网址。
- 不用 MDX、组件、`import`、其他 HTML，也暂时不放图片。
- 生成的段落夹在 `<!-- generated:<name>:start -->` 与 `<!-- generated:<name>:end -->` 之间，由 `tools/gen_docs.py` 写入，不要手改（改源头：命令与按键在 `packages/backend/src/spaceknife/palette/commands.py`，设置在 `packages/backend/src/spaceknife/settings.py`）；`uv run python tools/gen_docs.py --check` 检查是否最新。

## 3. 内容

- 面向用户，讲怎么用、为什么这样设计；重点放在进阶用法和快捷键上，基础操作简明带过。
- 每一条说法都要对得上代码，不写没有的功能，不写计划（必要时明确标「计划中」）。
- 界面文字照抄：中文页用界面上的中文原文；英文页用英文界面的文字（`apps/electron/src/renderer/src/locales/en/*.json`、`packages/backend/src/spaceknife/locales/en/*.json`）。
- 按键写 Windows / Linux 的组合；macOS 上 Ctrl 对应 ⌘ 的地方单独说明（以代码为准）。
- 中文：简洁、直接、不用营销腔；英文：短句、第二人称、plain English。
- 不提第三方竞品，不做比较，不写「继任者」「替代」；示例路径用 `lenny`（`C:\Users\lenny\…`、`/home/lenny/…`）。

## 4. 检查

- 官网：`site/` 里 `pnpm build`（Node 22+），构建会检查链接到的页面是否存在。
- 软件内：帮助页的测试会把每一页渲染一遍。
