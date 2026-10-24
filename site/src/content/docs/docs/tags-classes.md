---
title: Tags and file classes
description: "Mark items with four color tags from the keyboard, find them with filters, and edit the file classes that color the picture and drive :class: filters."
---

Tags are marks you put on files and folders yourself: "keep", "ask about this", "delete later". File classes are the opposite. SpaceKnife sorts every file into a class by its extension, and the classes color the picture. Both work in [filters](../filtering/): `:red` finds tagged items, `:class:video` finds videos.

## Tags

There are four fixed tags. Each one has a color and a shape, so you can tell them apart without relying on color.

| Tag | Shape | Keys | Filter |
|---|---|---|---|
| Red tag | ▲ | <kbd>Ctrl</kbd>+<kbd>1</kbd> | `:red` |
| Yellow tag | ■ | <kbd>Ctrl</kbd>+<kbd>2</kbd> | `:yellow` |
| Green tag | ● | <kbd>Ctrl</kbd>+<kbd>3</kbd> | `:green` |
| Blue tag | ◆ | <kbd>Ctrl</kbd>+<kbd>4</kbd> | `:blue` |
| Clear tag | | <kbd>Ctrl</kbd>+<kbd>0</kbd> | |

An item has at most one tag. Giving it another color replaces the old one.

### Tagging from the keyboard

Select a block, then press <kbd>Ctrl</kbd>+<kbd>1</kbd> to <kbd>Ctrl</kbd>+<kbd>4</kbd>. <kbd>Ctrl</kbd>+<kbd>0</kbd> clears the tag.

With **Ctrl is optional when the treemap has focus** turned on (it is on by default), <kbd>1</kbd> to <kbd>4</kbd> and <kbd>0</kbd> work without Ctrl whenever the focus isn't in a text box. You find the switch in Settings › Tags › Shortcuts. On macOS, ⌘ works in place of Ctrl.

A quick way to sort through a disk: click a block, press a number, click the next one. When you are done, filter by tag to see each pile.

:::caution[Clearing a folder clears everything inside it]
Clearing the tag of a folder, with <kbd>Ctrl</kbd>+<kbd>0</kbd>, Clear tag in the menu or a click on its pressed tag button, also removes the tags of every file and folder inside it.
:::

### Other ways to tag

- **Details panel.** The Tags section has four buttons, one per tag, with the hint "Ctrl+1–4 · Ctrl+0 clears". The button of the item's current tag is pressed. Click another one to change the tag, or click the pressed one to clear it.
- **Right-click menu.** Add tag opens a submenu with the four tags and Clear tag. From the keyboard: <kbd>Shift</kbd>+<kbd>F10</kbd> or the menu key opens the menu of the selected block, <kbd>↓</kbd> to Add tag, <kbd>→</kbd> to open it, <kbd>Enter</kbd> to choose.
- **Command palette.** Red tag, Yellow tag, Green tag, Blue tag and Clear tag act on the selected item. See [Command palette](../command-palette/).

A small items block can't be tagged, and neither can the free space and unknown space blocks.

### Where tags show

- **In the picture.** A marker with the tag's shape sits in the top right corner of a file's block, or in a folder's title bar, when the block is large enough to hold it. The treemap, the flame graph and the polar flame graph all draw it.
- **In the lists.** Rows in the Matches panel and in the details panel's largest items show the marker. In the Matches panel, a tag that comes from a folder is shown too and is read as "Red tag (inherited from folder)".
- **In the right-click menu,** next to the item's name at the top.

A folder's marker is drawn only on the folder itself, not repeated on everything inside it.

### Tags in filters

A tag on a folder counts for everything below it, so tagging one folder is enough to include its whole content in a tag filter.

| Expression | Finds |
|---|---|
| `:red` | Files tagged red, or inside a folder tagged red |
| `:red;:yellow` or `:tag:r+y` | Red or yellow |
| `:all` | Any tag |
| `:tag:all-green` | Any tag except green |
| `\|:all;>1gb` | Untagged files over 1 GB |
| `\|:green;>1gb` | Files over 1 GB that are not tagged green |
| `:blue;m>1y` | Blue items untouched for a year |

For example, tag the folders you want to keep green, then filter `|:green;>1gb` to see the large files outside them. The full tag syntax is in [Filters](../filtering/#tags).

### Display names

Settings › Tags › Display names gives each tag a name of your own, such as "keep" for green. The name appears after the tag's color, as in "Green tag · keep", in the tooltips of the tag buttons in the details panel and in the right-click menu's Add tag submenu. Filters still use the color: `:green`.

### Saved tags

With **Remember tags** turned on (Settings › Tags › Saving, on by default), tags are saved per scan location. A tag is stored with its path relative to the scan root, so it comes back after a rescan and when you open a snapshot of the same location. Comparisons show the tags of their scan.

- A scan location is the set of folders you scanned. `C:\` and `C:\Users\lenny` are two different locations with separate tags.
- With Remember tags off, tags only last while the scan is open.
- Settings › Tags › Saved tags › **Clear all saved tags** shows how many scan locations have saved tags and removes all of them after you confirm.
- Where the files are kept is listed in [Data locations](../data-locations/).

:::note[Command line]
`spaceknife-backend run … filter` does not load saved tags, so tag conditions there match no file. See [Command line](../command-line/).
:::

## File classes

A file class is a named set of extensions. The classes are used in three places:

- the **File classes** coloring of the picture,
- `:class:` [filters](../filtering/#file-classes),
- the **Classes** page of the details panel, which breaks the selected folder down by class: number of files, size, share and a bar for each class. Hover a class and the picture shows where it is.

### The built-in classes

| Class | Extensions |
|---|---|
| Video | mp4 mkv mov avi wmv webm m4v flv ts m2ts mts vob mpg mpeg mpe m1s qt rm rmvb 3gp f4v ogv mxf dv divx y4m m2t m2v h264 h265 hevc ivf asf wtv mk3d ogm lrv insv braw r3d bik bk2 usm |
| Pictures | jpg jpeg png gif bmp tif tiff heic heif webp avif jxl svg ico psd raw nef rw2 cr2 cr3 arw dng orf raf srw x3f pef rwl mrw dcr kdc erf mef mos nrw sr2 srf iiq 3fr jfif jpe jp2 j2k jxr hdp wdp apng qoi svgz exr hdr dds ktx ktx2 tga pcx pbm pgm ppm pnm icns cur ani psb xcf kra clip pdn sai afphoto afdesign ai eps emf wmf cdr sketch fig xd |
| Audio | mp3 flac wav m4a aac ogg opus wma aif aiff alp au ac3 amr mp2 voc ape mid midi dsf 3gp m4b m4r caf mka wv tta dts mpc spx weba oga gsm ra aifc w64 rf64 dff xm s3m it sf2 wem bnk |
| Archives | zip 7z 7zip rar tar gz tgz bz2 xz zst lz lzma cab iso img dmg wim esd vhd vhdx vmdk qcow2 tbz tbz2 txz tzst lz4 lzo br sz zpaq zipx arj lha lzh cpio xar sit sitx vdi qcow avhdx hdd hds vmem vmsn ova xva sparseimage nrg |
| Programs | exe dll sys msi msix appx com bat cmd ps1 sh so dylib ko efi drv ocx cpl ini log apk aab xapk ipa deb rpm snap appimage jar war ear vsix crx xpi pyd node wasm msp msu mui tlb winmd dmp mdmp etl evtx |
| Documents | pdf doc docx xls xlsx ppt pptx odt ods odp rtf txt md csv epub pages numbers key docm dotx dotm xlsm xlsb pptm odg wps et dps hwp ofd xps oxps djvu chm mobi azw azw3 fb2 tex rst org tsv vsd vsdx one eml msg pst ost mbox |
| Games | (no extensions: recognised folders, see below) |
| Code | c h cc cpp cxx hh hpp hxx cs java kt kts scala groovy gradle go rs swift m mm py pyi pyw pyx ipynb js mjs cjs jsx ts tsx mts cts vue svelte css scss sass less php rb pl pm lua r jl dart ex exs erl hs elm clj cljs fs fsx vb cmake pyc pyo class o lib a pdb ilk idb pch ipch tlog whl egg gem nupkg |
| Data | safetensors gguf ggml ckpt pt pth onnx tflite mlmodel h5 hdf5 he5 nc npy npz pkl pickle joblib parquet arrow feather avro orc tfrecord jsonl ndjson json fits fts fit mat db sqlite sqlite3 db3 duckdb mdb accdb ldb vscdb realm ibd myd myi mdf ldf ndf dbf kdbx sql |
| Other | every extension in no other class, and files without an extension |

- A file's extension is the part of its name after the last dot, compared without case. A leading dot doesn't count: `.bashrc` has no extension, so it belongs to Other.
- An extension can be in several classes (`3gp` is in Video and Audio). The first class in the list gives the color, and every class that lists it matches it in filters.
- `ts` and `mts` are the exception: such a file can be a recording (an MPEG transport stream, AVCHD) or TypeScript source. SpaceKnife decides by the file's size and never reads its content: over 16 MB, or over 1 MB and exactly a whole number of transport-stream packets (188 bytes for `ts`, 192 for `mts`), it is Video; anything else is Code. Take `ts` out of Video and the extension alone decides again.
- The Chinese interface shows the same classes under Chinese names (视频, 图片, 音频, 压缩包/镜像, 程序/系统, 文档, 游戏, 代码/开发, 数据/模型, 其他). Both sets of names work in filters in either language.

### Classes by folder

The extension says what a file is; some folders say more. A folder SpaceKnife recognises decides the class of **every** file in it, whatever the extension:

| Folder | Counts as |
|---|---|
| Games: Steam libraries (`steamapps`); the game folders of Epic, GOG, Xbox, EA, Ubisoft, Riot, HoYoPlay / miHoYo, Hypergryph and other launchers; Unity games (`UnityPlayer.dll`, or `name.exe` beside `name_Data`), Unreal games (`game/Content/Paks`), Godot games (a `.pck` beside an `.exe`), Blizzard games (`.build.info`); `.minecraft`, `Documents/My Games`, `Documents/Paradox Interactive` and `Saved Games` in a home | Games |
| Projects: a folder holding `.git`, `package.json`, `Cargo.toml`, `pyproject.toml`, `go.mod`, `CMakeLists.txt`, `*.sln`, `*.csproj` or a similar marker | Code |
| Model caches: `.ollama`, `.cache/huggingface`, `.lmstudio` | Data |
| System areas: `System Volume Information`, the MFT scan's NTFS metadata folder | Programs |

Also, in installed software and app data (`Program Files`, `ProgramData` and `Windows` at the top of a volume, `/usr`, `/opt`, `/Applications`, and a home's `AppData`, `Library`, `.local`, `.config`), **files in no class** count as Programs; pictures, videos and other recognised files keep their extension's class.

- The outer folder wins: a game's own `package.json` stays the game's, and a game built inside a project stays the project's.
- A project never starts in installed software, a cache or a package store (`AppData`, `node_modules`, `site-packages`, `.cargo` …), and is never a volume, a home or a home's **Downloads**, **Documents**, **Desktop** and other standard folders themselves: a stray `package.json` in Downloads doesn't turn all of Downloads into a project, though a repository downloaded there still counts.
- On the **Classes** page, the tooltip of a class says what its folders' files would be by extension, such as "By their extensions these are: Video 3.0 GB"; the **Details** page says what the selected folder counts as and why, such as "Everything here counts as Games · Steam library".
- Colors, `:class:` filters and the **Classes** page all follow this: `:class:games` finds the videos inside games, `:class:video` doesn't (to find every mp4, use `*.mp4`).
- Take **Games** out of the class table and the game folder rules are off.

**Your own choice**: right-click a folder in the picture and pick **Classify**: **Automatic** (the default), **By extension only** (no folder rule for this folder), or **Count as** a class (every file in it counts as that class). Your choices win over the rules, and an inner choice decides its own folder. They are listed under **Settings › File classes › Classed by folder**, where × makes a folder automatic again; **Restore default classes** clears them too, and exporting or importing settings carries them along.

### How classes color the picture

The **File classes** coloring fills each file's block with the color of its class. It is the treemap's default coloring. The flame graph and the polar flame graph start with Top-level branch coloring instead. Press <kbd>C</kbd> to cycle through the colorings (Classes, Flat, Time, Branch on the toolbar), or pick Coloring: file classes in the command palette. The defaults are under Settings › Appearance. More in [Visualizations](../visualizations/).

### Editing classes

Settings › File classes lists the classes in order. Every change is applied as soon as you leave the field: it is saved, the pictures are recolored and `:class:` filters run again.

- **Color.** Click the color swatch to pick a new color. The light and the dark theme each have their own class colors, and you change the ones of the current theme. Tick **Also apply to the other theme** below the list to change both. Once you recolor a built-in class, a **Restore the default color** button appears next to its swatch. Colors picked before version 0.3.4, when there was only the dark theme, count for both themes.
- **Name.** Edit the name in place. The row shows the filter that matches the class, such as `:class:Video`.
- **Extensions.** Type them separated by `;`, commas or spaces, as in `pak; ucas`. Leading dots are dropped and case doesn't matter.
- **Order.** Drag a row by its handle. The order decides which color an extension gets when it is in several classes. A row warns with "Already in an earlier class: …" and lists those extensions with the class that colors them.
- **Add class** adds a "New class" row just above Other.
- **Delete.** The × button deletes a class after you confirm. Its extensions fall back to Other.
- **Restore default classes** appears once you have changed something and brings back the 9 built-in classes.

Rules the editor enforces:

- A name can't be empty and can't contain `;`, `:`, `"` or `\`, because those would break `:class:` in a filter.
- Two classes can't share a name, and that includes the other names a class answers to.
- Other can't be deleted, takes no extensions (it holds everything unclassified) and always stays last.
- An extension can't contain `/` or `\`.

When you rename a built-in class, its old name keeps working in filters. A filter that says `:class:Video` still finds your renamed video class. The SpaceSniffer class names (`Audio/Music`, `Pictures/Images` and so on) stay valid as well.

:::tip[Your own classes]
Add a class for the files that matter on your disk, for example "Game assets" with `pak; pck; ucas; utoc; assets`. It gets its own color in the picture, its own line in the Classes page, and `:class:Game assets;>1gb` finds the large ones.
:::

### Moving classes between machines

- Settings › Advanced › **Export settings** saves every changed setting and your custom file classes to one file; **Import settings** reads it back.
- **Import from SpaceSniffer** reads `SpaceSnifferConfig.xml`, file classes included, and shows what will change before applying it.
- **Restore all defaults** puts the file classes back to the built-in ones along with every setting.
- The command line uses your file classes too, so `:class:` filters in a script match what the app shows.

## Keys

| Keys | Action |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>1</kbd> … <kbd>Ctrl</kbd>+<kbd>4</kbd> | Red, yellow, green or blue tag on the selected item |
| <kbd>Ctrl</kbd>+<kbd>0</kbd> | Clear the tag of the selected item and everything inside it |
| <kbd>1</kbd> … <kbd>4</kbd>, <kbd>0</kbd> | The same without Ctrl, when the setting is on and the focus isn't in a text box |
| <kbd>Shift</kbd>+<kbd>F10</kbd> | Right-click menu of the selected block (Add tag) |
| <kbd>C</kbd> | Next coloring |
| <kbd>Ctrl</kbd>+<kbd>K</kbd> | Command palette: tag commands, Coloring: file classes |

All keys are listed in [Keyboard shortcuts](../shortcuts/).
