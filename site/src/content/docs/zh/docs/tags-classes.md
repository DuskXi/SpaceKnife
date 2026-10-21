---
title: 标签与文件类别
description: "用键盘给文件和文件夹打四种颜色标签、用过滤找回它们，以及编辑决定图上颜色和 :class: 过滤的文件类别。"
---

标签是你自己打在文件和文件夹上的记号：「保留」「待确认」「以后删」。文件类别正好相反：SpaceKnife 按扩展名把每个文件归入一个类别，类别决定图上的颜色。两者都能用在[过滤](../filtering/)里：`:red` 找带标签的项，`:class:视频` 找视频。

## 标签

标签固定四种，每种都有颜色和形状，不靠颜色也能分清。

| 标签 | 形状 | 按键 | 过滤 |
|---|---|---|---|
| 红色标签 | ▲ | <kbd>Ctrl</kbd>+<kbd>1</kbd> | `:red` |
| 黄色标签 | ■ | <kbd>Ctrl</kbd>+<kbd>2</kbd> | `:yellow` |
| 绿色标签 | ● | <kbd>Ctrl</kbd>+<kbd>3</kbd> | `:green` |
| 蓝色标签 | ◆ | <kbd>Ctrl</kbd>+<kbd>4</kbd> | `:blue` |
| 清除标签 | | <kbd>Ctrl</kbd>+<kbd>0</kbd> | |

每一项最多一个标签，再打另一种颜色就替换掉原来的。

### 用键盘打标签

选中一个块，按 <kbd>Ctrl</kbd>+<kbd>1</kbd> 到 <kbd>Ctrl</kbd>+<kbd>4</kbd>；<kbd>Ctrl</kbd>+<kbd>0</kbd> 清除。

打开「树图有焦点时可省略 Ctrl」（默认打开）后，只要焦点不在输入框里，直接按 <kbd>1</kbd> 到 <kbd>4</kbd> 和 <kbd>0</kbd> 就行。开关在「设置 › 标签 › 快捷键」。macOS 上 Ctrl 也可以换成 ⌘。

整理磁盘的快办法：单击一个块，按一个数字，再单击下一个；全部过一遍后，按标签过滤，逐堆处理。

:::caution[清除文件夹的标签会清除里面的全部标签]
清除一个文件夹的标签时（<kbd>Ctrl</kbd>+<kbd>0</kbd>、菜单里的「清除标签」，或者点它已按下的标签按钮），它里面所有文件和文件夹的标签也一起清除。
:::

### 其他打标签的方式

- **详情面板**：「标签」区有四个按钮，提示「Ctrl+1–4 · Ctrl+0 清除」。当前标签对应的按钮是按下态；点另一个换标签，点已按下的那个清除。
- **右键菜单**：「加标签」打开子菜单，里面是四种标签和「清除标签」。键盘操作：<kbd>Shift</kbd>+<kbd>F10</kbd> 或菜单键打开选中块的菜单，<kbd>↓</kbd> 移到「加标签」，<kbd>→</kbd> 展开，<kbd>Enter</kbd> 选择。
- **命令面板**：「红色标签」「黄色标签」「绿色标签」「蓝色标签」「清除标签」作用于选中项。见[命令面板](../command-palette/)。

小项聚合块不能加标签，可用空间块和未知空间块也不能。

### 标签显示在哪里

- **图上**：块足够大时，文件块的右上角、文件夹的标题栏里会画出标签形状的标记。树图、火焰图和火焰-极坐标图都会画。
- **列表里**：「匹配结果」和详情面板「最大的子项」的行上显示标记。「匹配结果」还会显示继承自文件夹的标签，读作「红色标签（继承自文件夹）」。
- **右键菜单**：顶部名称旁边。

文件夹的标记只画在文件夹本身，不会在它里面的每一项上重复画。

### 在过滤里用标签

文件夹上的标签对它下面的所有内容都有效，所以给一个文件夹打标签，按标签过滤时它的全部内容都会算进去。

| 表达式 | 找出 |
|---|---|
| `:red` | 带红色标签的文件，或位于带红色标签的文件夹里的文件 |
| `:red;:yellow` 或 `:tag:r+y` | 红色或黄色 |
| `:all` | 任意标签 |
| `:tag:all-green` | 除绿色以外的任意标签 |
| `\|:all;>1gb` | 没有标签、大于 1 GB 的文件 |
| `\|:green;>1gb` | 大于 1 GB、没有绿色标签的文件 |
| `:blue;m>1y` | 一年没动过的蓝色标签项 |

例如：先给要保留的文件夹打绿色标签，再用 `|:green;>1gb` 看它们以外的大文件。标签的完整语法见[过滤](../filtering/#标签)。

### 显示名

「设置 › 标签 › 显示名」可以给每种标签起自己的名字，比如把绿色叫「保留」。名字跟在颜色后面，写成「绿色标签 · 保留」，出现在详情面板标签按钮的提示和右键菜单「加标签」的子菜单里。过滤仍然写颜色：`:green`。

### 保存标签

打开「记住标签」（「设置 › 标签 › 保存」，默认打开）后，标签按扫描位置保存。标签记的是相对扫描根的路径，所以重新扫描后、打开同一位置的快照时都会恢复。快照对比显示它所属扫描的标签。

- 扫描位置指你扫描的那一组文件夹。`C:\` 和 `C:\Users\lenny` 是两个不同的位置，标签各自独立。
- 关闭「记住标签」后，标签只在这次扫描打开期间有效。
- 「设置 › 标签 › 已保存的标签 › 清除所有已保存的标签」显示有多少个扫描位置保存了标签，确认后全部清除。
- 文件存放位置见[数据位置](../data-locations/)。

:::note[命令行]
`spaceknife-backend run … filter` 不读取已保存的标签，所以那里的标签条件不匹配任何文件。见[命令行](../command-line/)。
:::

## 文件类别

文件类别是一组有名字的扩展名，用在三个地方：

- 图的「文件类别」配色；
- `:class:` [过滤](../filtering/#文件类别)；
- 详情面板的「类别统计」页：按类别拆分选中的文件夹，每个类别显示文件数、大小、占比和比例条。

### 内置类别

| 类别 | 扩展名 |
|---|---|
| 视频 | mp4 mkv mov avi wmv webm m4v flv ts m2ts mts vob mpg mpeg mpe m1s qt rm rmvb 3gp f4v ogv mxf dv divx y4m m2t m2v h264 h265 hevc ivf asf wtv mk3d ogm lrv insv braw r3d bik bk2 usm |
| 图片 | jpg jpeg png gif bmp tif tiff heic heif webp avif jxl svg ico psd raw nef rw2 cr2 cr3 arw dng orf raf srw x3f pef rwl mrw dcr kdc erf mef mos nrw sr2 srf iiq 3fr jfif jpe jp2 j2k jxr hdp wdp apng qoi svgz exr hdr dds ktx ktx2 tga pcx pbm pgm ppm pnm icns cur ani psb xcf kra clip pdn sai afphoto afdesign ai eps emf wmf cdr sketch fig xd |
| 音频 | mp3 flac wav m4a aac ogg opus wma aif aiff alp au ac3 amr mp2 voc ape mid midi dsf 3gp m4b m4r caf mka wv tta dts mpc spx weba oga gsm ra aifc w64 rf64 dff xm s3m it sf2 wem bnk |
| 压缩包/镜像 | zip 7z 7zip rar tar gz tgz bz2 xz zst lz lzma cab iso img dmg wim esd vhd vhdx vmdk qcow2 tbz tbz2 txz tzst lz4 lzo br sz zpaq zipx arj lha lzh cpio xar sit sitx vdi qcow avhdx hdd hds vmem vmsn ova xva sparseimage nrg |
| 程序/系统 | exe dll sys msi msix appx com bat cmd ps1 sh so dylib ko efi drv ocx cpl ini log apk aab xapk ipa deb rpm snap appimage jar war ear vsix crx xpi pyd node wasm msp msu mui tlb winmd dmp mdmp etl evtx |
| 文档 | pdf doc docx xls xlsx ppt pptx odt ods odp rtf txt md csv epub pages numbers key docm dotx dotm xlsm xlsb pptm odg wps et dps hwp ofd xps oxps djvu chm mobi azw azw3 fb2 tex rst org tsv vsd vsdx one eml msg pst ost mbox |
| 游戏 | （没有扩展名：按文件夹识别，见下文） |
| 代码/开发 | c h cc cpp cxx hh hpp hxx cs java kt kts scala groovy gradle go rs swift m mm py pyi pyw pyx ipynb js mjs cjs jsx ts tsx mts cts vue svelte css scss sass less php rb pl pm lua r jl dart ex exs erl hs elm clj cljs fs fsx vb cmake pyc pyo class o lib a pdb ilk idb pch ipch tlog whl egg gem nupkg |
| 数据/模型 | safetensors gguf ggml ckpt pt pth onnx tflite mlmodel h5 hdf5 he5 nc npy npz pkl pickle joblib parquet arrow feather avro orc tfrecord jsonl ndjson json fits fts fit mat db sqlite sqlite3 db3 duckdb mdb accdb ldb vscdb realm ibd myd myi mdf ldf ndf dbf kdbx sql |
| 其他 | 不属于其他任何类别的扩展名，以及没有扩展名的文件 |

- 文件的扩展名是名字里最后一个点之后的部分，不区分大小写。开头的点不算：`.bashrc` 没有扩展名，归入「其他」。
- 一个扩展名可以属于多个类别（`3gp` 同时在视频和音频里）：列表里排在前面的类别决定颜色，过滤时每个包含它的类别都能匹配。
- `ts` 和 `mts` 例外：它们既可能是录像（MPEG 传输流、AVCHD），也可能是 TypeScript 源码。SpaceKnife 只看文件大小，不读内容：16 MB 以上，或 1 MB 以上且大小正好是整数个传输流包（`ts` 188 字节，`mts` 192 字节）的算视频，其余算代码。从视频类别里删掉 `ts`，就只按扩展名归类。
- 英文界面里这些类别显示为英文名（Video、Pictures、Audio、Archives、Programs、Documents、Games、Code、Data、Other）。两套名字在任何界面语言下都能用于过滤。

### 按文件夹归类

扩展名说的是文件是什么；有些文件夹说得更多。认得出的文件夹决定里面**所有**文件的类别，不论扩展名：

| 文件夹 | 算作 |
|---|---|
| 游戏：Steam 游戏库（`steamapps`）；Epic、GOG、Xbox、EA、Ubisoft、Riot、HoYoPlay / miHoYo、鹰角等启动器的游戏文件夹；Unity 游戏（`UnityPlayer.dll`，或 `名字.exe` 旁边有 `名字_Data`）、Unreal 游戏（`游戏/Content/Paks`）、Godot 游戏（`.pck` 旁边有 `.exe`）、暴雪游戏（`.build.info`）；家目录里的 `.minecraft`、`文档/My Games`、`文档/Paradox Interactive`、`Saved Games` | 游戏 |
| 项目：含 `.git`、`package.json`、`Cargo.toml`、`pyproject.toml`、`go.mod`、`CMakeLists.txt`、`*.sln`、`*.csproj` 等标记的文件夹 | 代码/开发 |
| 模型缓存：`.ollama`、`.cache/huggingface`、`.lmstudio` | 数据/模型 |
| 系统区：`System Volume Information`、MFT 快速扫描的「$NTFS 元数据」 | 程序/系统 |

另外，已安装的软件和应用数据里（卷顶层的 `Program Files`、`ProgramData`、`Windows`，`/usr`、`/opt`、`/Applications`，家目录的 `AppData`、`Library`、`.local`、`.config`），**没有类别的文件**算「程序/系统」；图片、视频这些认得出的文件仍按扩展名。

- 外层的文件夹优先：游戏里的 `package.json` 仍算游戏，项目里构建出的游戏仍算项目。
- 项目不会从已安装的软件、缓存和包仓库里（`AppData`、`node_modules`、`site-packages`、`.cargo` 等）开始，也不会是卷、家目录或家目录下的「下载」「文档」「桌面」等文件夹本身：下载文件夹里散落一个 `package.json` 不会让整个下载文件夹算项目，下载下来的仓库仍算。
- 「类别统计」里，文件夹归类带进来的文件在悬停提示里写明按扩展名本是什么，例如「其中按扩展名本是：视频 3.0 GB」；「详情」页写明选中的文件夹算作什么、为什么，例如「整个文件夹算作「游戏」 · Steam 游戏库」。
- 图的配色、`:class:` 过滤和「类别统计」都按这个结果：`:class:游戏` 找得到游戏里的视频，`:class:视频` 找不到它们（要找所有 mp4，用 `*.mp4`）。
- 从类别表里删掉「游戏」，游戏文件夹的规则就不起作用了。

**自己指定**：在图上右键一个文件夹，选「归类」：「自动识别」（默认）、「只按扩展名」（这个文件夹不用任何文件夹规则），或「算作「某类」」（里面的文件都算这一类）。你的选择优先于规则，嵌套时里层的选择管它自己的文件夹。所有选择列在「设置 › 文件类别 › 按文件夹归类」里，点 × 改回自动识别；「恢复默认类别」会一并清除，导出 / 导入设置时也一并带上。

### 类别怎样给图着色

「文件类别」配色按文件所属类别的颜色填充每个文件块。这是树图的默认配色；火焰图和火焰-极坐标图默认用「顶层分支」配色。按 <kbd>C</kbd> 在几种配色之间切换（工具栏上的「类别｜扁平｜时间｜分支」），也可以在命令面板里选「配色：文件类别」。默认配色在「设置 › 外观」里改。详见[可视化](../visualizations/)。

### 编辑类别

「设置 › 文件类别」按顺序列出所有类别。每次修改在离开输入框时立即生效：保存下来，各个视图重新着色，`:class:` 过滤重新求值。

- **颜色**：点色块（「改颜色」）选新颜色。
- **名称**：直接修改。行尾显示匹配这个类别的过滤写法，如 `:class:视频`。
- **扩展名**：用 `;`、逗号或空格分隔，例如 `pak; ucas`。开头的点会去掉，不区分大小写。
- **顺序**：按住左侧的把手拖动。同一扩展名在多个类别里时，排在前面的决定颜色；重复的扩展名会提示「已在前面的类别里：…」，并列出决定颜色的类别。
- **添加类别**：在「其他」前面加一行「新类别 N」。
- **删除**：点 × 并确认后删除，它的扩展名归入「其他」。
- **恢复默认类别**：改动过之后出现，确认「恢复为内置的 9 个类别？」后恢复。

编辑器会检查这些规则：

- 类别名不能为空，不能含 `;`、`:`、`"`、`\`，否则会破坏过滤里的 `:class:` 写法。
- 两个类别不能重名，类别的其他叫法（别名）也算在内。
- 「其他」不能删除、不能填扩展名（它收纳所有未归类的扩展名），而且总在最后。
- 扩展名里不能有 `/` 或 `\`。

给内置类别改名后，旧名字在过滤里照样有效：`:class:视频` 仍然找得到改了名的视频类别。SpaceSniffer 的类别名（`Audio/Music`、`Pictures/Images` 等）也一直有效。

:::tip[加一个自己的类别]
为自己磁盘上重要的文件建一个类别，比如「游戏资源」，扩展名填 `pak; pck; ucas; utoc; assets`。它在图上有自己的颜色，在「类别统计」里有自己的一行，`:class:游戏资源;>1gb` 就能找出其中的大文件。
:::

### 在不同电脑之间带走类别

- 「设置 › 高级 › 导出设置」把所有改过的设置和自定义的文件类别存成一个文件，「导入设置」再读回来。
- 「从 SpaceSniffer 导入」读取 `SpaceSnifferConfig.xml`，其中的文件类别也会导入，应用前先显示会改变什么。
- 「全部恢复默认」会把文件类别和所有设置一起恢复成内置的。
- 命令行也使用你的文件类别，脚本里的 `:class:` 过滤与软件里看到的一致。

## 按键

| 按键 | 作用 |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>1</kbd> … <kbd>Ctrl</kbd>+<kbd>4</kbd> | 给选中项打红、黄、绿、蓝标签 |
| <kbd>Ctrl</kbd>+<kbd>0</kbd> | 清除选中项及其里面全部内容的标签 |
| <kbd>1</kbd> … <kbd>4</kbd>、<kbd>0</kbd> | 同上，不按 Ctrl（设置打开、焦点不在输入框里时） |
| <kbd>Shift</kbd>+<kbd>F10</kbd> | 选中块的右键菜单（「加标签」） |
| <kbd>C</kbd> | 下一种配色 |
| <kbd>Ctrl</kbd>+<kbd>K</kbd> | 命令面板：标签命令、「配色：文件类别」 |

全部按键见[快捷键](../shortcuts/)。
