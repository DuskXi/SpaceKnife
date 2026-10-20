---
title: Filters
description: The complete filter language (names, folders, size, dates, file classes, tags, attributes, negation, quoting) with worked examples, the dim and hide modes, the Matches panel and the keys.
---

The filter box picks out files by name, folder, size, age, file class, tag and attribute. Matching files stay lit in the picture, everything else dims, and the side panel lists the matches, largest first. Press <kbd>/</kbd> or <kbd>Ctrl</kbd>+<kbd>F</kbd> to jump into the box and start typing. The view follows as you type.

```text
:class:video;>1gb;m>1y
```

That expression finds videos larger than 1 GB that nobody has modified for more than a year.

## Quick reference

| Expression | Finds |
|---|---|
| `*.mp4;*.mkv` | Files named `*.mp4` or `*.mkv` |
| `>1gb;<10gb` | Size on disk between 1 GB and 10 GB |
| `s>=1mb` | Logical size (file length) of at least 1 MB |
| `m<10min` | Modified in the last 10 minutes |
| `c>1y` | Created more than a year ago |
| `\temp` | Anything inside a folder called `temp`, at any depth |
| `\|*.exe;\|*.dll` | Every file except `.exe` and `.dll` files |
| `:attr:h-s` | Hidden files that are not system files |
| `:red;:yellow` or `:tag:r+y` | Files tagged red or yellow |
| `:class:video` | Files in the Video class |
| `"a;b.txt"` | The file literally named `a;b.txt` |

## How an expression is read

An expression is a list of conditions separated by `;`. Spaces around a condition are ignored, empty pieces are ignored, and an empty box means no filter. Keywords, prefixes, units, tag colors and attribute names are not case-sensitive, and neither are file names.

Each condition is classified by its first character. Leading spaces and a leading `|` (negation) are skipped first, then the first rule that fits wins:

1. Starts with `\`: a [folder mask](#folders).
2. Starts with `:`: a keyword: [class](#file-classes), [tag](#tags) or [attribute](#attributes).
3. Starts with `"`: a [quoted name](#quotes-and-literal-names).
4. Contains `<` or `>`: a [size](#size) or [date](#dates) comparison.
5. Anything else: a [name mask](#names).

:::caution[A number on its own is a name]
`500mb` and `=1mb` are name masks, because a file can have that name. They show up in the Conditions block as a name. A comparison always needs `<` or `>`: write `>500mb`.
:::

### Combining conditions

Conditions of the same kind are alternatives. Conditions of different kinds must all hold. Size and date bounds always add up, so two bounds make a range. Negated conditions exclude. The order you write them in, and repeating one, never changes the result.

| Kind | Written as | When there are several |
|---|---|---|
| What the file is | `*.mp4`, `:class:video` | Any one of them (names and classes form one group) |
| Where it is | `\Downloads` | Any one of them |
| Tags | `:red`, `:tag:r+y` | Any one of them |
| Size and dates | `>1gb`, `m<7d` | All of them |
| Attributes | `:attr:h` | All of them |
| Negated conditions | `\|*.tmp`, `\|\node_modules` | None of them may hold |

So `*.mp4;*.mkv;\Downloads;>1gb;|\Temp` reads: (an mp4 or an mkv) and inside a folder called Downloads and larger than 1 GB and not inside a folder called Temp.

### Only files match

Folders never match on their own. A folder *contains matches* when some file below it matches, and the match counts and sizes add up through the folders. Files here means every entry that is not a folder: regular files, symbolic links and other special files. When a file has several hard links, the extra links count as matching files but add no size, the same way the picture counts them.

## Conditions

### Names

A name mask is compared with the whole file name, extension included.

- `?` matches exactly one character, `*` matches any number of characters, including none.
- `mp4` only matches a file named exactly `mp4`. To match names that contain it, write `*mp4*`.
- `*.*` needs a dot in the name, so it does not match `README`.
- Matching ignores case on every platform.
- A mask without quotes cannot contain `\`, `|`, `<` or `>`. Put the name in [quotes](#quotes-and-literal-names) when you need them.

### Folders

`\mask` matches a file when any folder on its path matches the mask, from the scan root down to the folder that holds the file. `\temp` finds `C:\Windows\Temp\data\abc.txt`.

- The leading `\` is only the marker. It does not mean the root, and spaces after it are ignored.
- A folder mask is one folder name, not a path: `\temp\x` is an error.
- The scan root takes part under its own name. When you scan `D:\Photos`, `\Photos` matches every file in the scan. A drive or file-system root (`C:\`, `/`) has an empty name, so only `\*` matches it. Folders above the scan root never take part, so the result does not depend on where you started the scan.
- Folders that match a folder mask get bold names in the treemap and the flame graph.
- Wildcards work as in names: `\*cache*` matches any folder whose name contains `cache`.

### Size

Write `[prefix] operator number unit`, for example `>1gb`, `s<=4kb` or `>= 1.5 GB`. Spaces between the parts are allowed.

| Prefix | Compares |
|---|---|
| none, `d`, `disk`, `disksize`, `clustersize` | Size on disk (the space allocated to the file) |
| `s`, `size`, `filesize`, `logicalsize` | Logical size (the file's length) |

| Unit | Bytes |
|---|---|
| `b` | 1 |
| `kb`, `kib` | 1024 |
| `mb`, `mib` | 1024² |
| `gb`, `gib` | 1024³ |
| `tb`, `tib` | 1024⁴ |
| `pb`, `pib` | 1024⁵ |

- The operators mean exactly what they say: `>` is greater than, `>=` greater than or equal, `<` less than, `<=` less than or equal.
- Filter units are always multiples of 1024, whatever unit base the interface uses to display sizes.
- Decimals work: `>1.5gb` means more than 1536 MB, measured exactly in bytes.
- The unit is required. `>500` is an error, because without a unit it could be a size or a time. Size units always end in `b`, so `k` or `g` alone are errors too: write `kb`, `gb`.
- The largest value is 2⁶² bytes.

:::note[Tiny files on NTFS]
On NTFS, very small files are stored inside the file table itself and take 0 bytes on disk. A size condition without a prefix compares size on disk, so these files never match `>0b`. Use `s` to compare their length instead: `s<1kb`.
:::

### Dates

Write `[prefix] operator number unit`, for example `m<7d` or `c>1y`. A date condition compares the file's **age**, the time from that timestamp until now. `<` means within that time (newer), `>` means longer ago than that (older).

| Prefix | Timestamp |
|---|---|
| none, `m`, `modify`, `modified` | Date modified |
| `c`, `create`, `created` | Date created |
| `a`, `access`, `accessed` | Date accessed |

| Unit | Length |
|---|---|
| `s`, `sec`, `secs`, `second`, `seconds` | 1 second |
| `m`, `min`, `mins`, `minute`, `minutes` | 1 minute |
| `h`, `hr`, `hrs`, `hour`, `hours` | 1 hour |
| `d`, `day`, `days` | 1 day |
| `w`, `wk`, `wks`, `week`, `weeks` | 7 days |
| `mon`, `month`, `months` | 30 days |
| `y`, `yr`, `yrs`, `year`, `years` | 365 days |

- `m` is the "modified" prefix in front of the operator and "minutes" after the number, so `m<10m` means modified in the last 10 minutes.
- Months and years have a fixed length. They do not follow the calendar.
- The prefix and the unit must agree. `s<5d` is an error, because `s` is the logical-size prefix. `m>1mb` is an error for the same reason.
- A file whose timestamp is unknown never matches a date condition, not even a negated one. Linux, for example, does not provide creation times.
- The largest value is 1000 years.
- The filter is evaluated again whenever the scanned data changes: while a scan runs, and when [monitoring](../monitoring/) reports changes. With monitoring on, `m<10min` follows fresh writes.

### File classes

`:class:name` matches files of a file class. The name runs to the end of the condition, may contain spaces and `/`, and ignores case. Every name of a class works: its display name in either interface language, its aliases and its id.

| Class | Also accepted |
|---|---|
| Video | `video`, `视频`, `Videos`, `Movies`, `Video/Movies` |
| Pictures | `image`, `图片`, `Image`, `Images`, `Picture`, `Pictures/Images`, `Photos` |
| Audio | `audio`, `音频`, `Music`, `Audio/Music` |
| Archives | `archive`, `压缩包/镜像`, `压缩包`, `镜像`, `Archive`, `Disk images`, `Archives/Disk images` |
| Programs | `exec`, `程序/系统`, `程序`, `系统`, `Program`, `Executable`, `Executables`, `System files`, `Executables/System files` |
| Documents | `doc`, `文档`, `Document`, `Docs` |
| Other | `other`, `其他`, `Others`, `Unclassified` |

- A class matches by extension: the part of the name after the last dot. A leading dot does not count, so `.bashrc` has no extension.
- `:class:other` matches every file whose extension is in no class, including files without an extension.
- An unknown class name is an error, and the completion list offers the class names.
- Classes and their extensions are yours to edit. See [Tags and file classes](../tags-classes/).

### Tags

`:red`, `:yellow`, `:green` and `:blue` match files with that tag. `:all` matches any tag.

- The long form is `:tag:` followed by a list. It starts with one item, `+` adds a color and `-` removes one. Colors can be shortened to `r`, `y`, `g` and `b`. `:tag:red+yellow` is red or yellow, `:tag:all-red` is any tag except red.
- Several tag conditions are alternatives: `:red;:yellow` is the same as `:tag:r+y`.
- A tag on a folder counts for everything inside it. Tagging a folder red makes `:red` match all its files.
- `|:all` matches files without any tag.
- The tag display names you set in Settings do not change the syntax. A red tag is always `:red`.

How to tag items is described in [Tags and file classes](../tags-classes/).

### Attributes

`:attr:` (or `:attrs:`) is followed by a list of attributes. `+` or `,` means "and also has", and `-` in front of an attribute means "does not have". `-` can also separate items on its own, so `:attr:a+h-s` and `:attr:a,h,-s` are the same condition: archive, hidden, not system.

| Attribute | Short | Attribute | Short |
|---|---|---|---|
| `archive` | `a` | `offline` | `o` |
| `system` | `s` | `temporary` | `t` |
| `readonly` | `r` | `notindexed` | `ni` |
| `hidden` | `h` | `sparse` | `sp` |
| `compressed` | `c` | `alternatedatastream` | `ads` |
| `encrypted` | `e` | | |

- Several `:attr:` conditions must all hold.
- `:attr:-a` means "has no archive attribute". `|:attr:a` negates the whole condition.
- Which attributes a platform records is listed under [Platform differences](#platform-differences).

### Negation

`|` in front of any condition negates it. Spaces after the `|` are ignored.

- A negated name, class, folder, tag or attribute condition **excludes** the files it matches: `|*.tmp`, `|\node_modules`, `|:class:video`, `|:green`, `|:attr:h`.
- A negated size or date condition becomes the **opposite range**: `|>1mb` is the same as `<=1mb`, and `|<5d` is the same as `>=5d`. Files with an unknown timestamp still do not match.

### Quotes and literal names

Put a name in double quotes when it contains characters the filter would otherwise read as syntax.

- Inside quotes, `;`, spaces at either end, `|`, `<` and `>` are ordinary characters.
- `\` escapes the next character: `\*` and `\?` are a literal star and question mark, `\"` is a quote, `\\` is a backslash.
- `*` and `?` without a backslash are still wildcards inside quotes.
- Quotes only count at the start of a condition, after an optional `|` or `\`: `|"~$*"` and `\"my;folder"` work.

With quotes, any file name can be matched literally, including Linux names that contain `;`, `*` or `\`: `"a;b.txt"`, `" leading space.txt"`, `"report\*.pdf"`.

## Worked examples

| Expression | Finds |
|---|---|
| `*.iso;*.img;>1gb` | Disk images over 1 GB, by name |
| `:class:archive;>1gb` | Archives and disk images over 1 GB, by class |
| `:class:video;>1gb;m>1y` | Videos over 1 GB not modified for a year |
| `s>=4gb` | Files at least 4 GB long |
| `>1gb;<10gb` | Files taking between 1 GB and 10 GB on disk |
| `c<7d;>100mb` | Files over 100 MB created this week |
| `a>1y;>100mb` | Files over 100 MB nobody opened for a year (access time) |
| `m<10min` | What changed in the last 10 minutes |
| `\node_modules` | Everything inside any `node_modules` folder |
| `\node_modules;m>90d` | `node_modules` contents untouched for 90 days |
| `\|\node_modules;\|\.git;>50mb` | Files over 50 MB outside `node_modules` and `.git` |
| `\*cache*;>10mb` | Files over 10 MB in folders whose name contains `cache` |
| `*.log;*.tmp;m>30d` | Logs and temporary files older than 30 days |
| `\|:class:video;\|:class:image;\|:class:audio;>500mb` | Large files that are not media |
| `:class:other;>100mb` | Large files whose extension is in no class |
| `\DCIM;\|*.jpg` | Everything in `DCIM` folders that is not a JPEG |
| `\|.*` | Everything except dot files |
| `:attr:h-s` | Hidden files that are not system files |
| `:attr:c` | NTFS-compressed files |
| `:red;:yellow` | Files tagged red or yellow, directly or through a folder |
| `\|:green;>1gb` | Files over 1 GB not tagged green |
| `\|:all;>1gb` | Untagged files over 1 GB |
| `*.mp4;*.mkv;\Downloads;>1gb;\|\Temp` | Large mp4 and mkv files in Downloads, but not in Temp |
| `"a;b.txt"` | The file literally named `a;b.txt` |

:::tip[Same meaning, different spelling]
SpaceKnife reduces every expression to its meaning before it filters. `>1mb;>2mb` is the same filter as `>2mb`, `MODIFIED<7200min` is `<5d`, and `:CLASS:music` is `:class:audio`. The Conditions block in the Matches panel shows that meaning, so you can check what you wrote.
:::

## While you type

- The box colors each part of the expression (names, comparisons, keywords, negation) and underlines errors with a wavy line.
- A completion list opens where there is something to choose: after `:`, after `:class:`, `:tag:` and `:attr:`, after `+`, `,` or `-` inside those lists, after a number (units) and at the start of a condition (prefixes). Classes show their color and a few extensions; attributes show their name and letter.
- The bottom of the list is a cheat sheet: Size `>1gb`, Time `m<7d`, Tags `:red`, Attributes `:attr:h`, Negate `|`.
- A condition you are still writing, where the cursor is, is left out instead of being reported as an error. The Conditions block shows it as a dashed capsule with "Choosing…". The same text away from the cursor is an error.
- The filter applies after a short pause. <kbd>Enter</kbd> applies it at once.
- While the expression has an error, the previous valid result stays on screen and the match bar says "Invalid expression, showing the previous results". The completion list shows the first error, followed by "(2 more)" or similar when there are others. Each condition is checked on its own, so one mistake does not hide errors in the others.

### Error messages

| Message | Cause | Example |
|---|---|---|
| Incomplete condition | A part is missing | `\|`, `\`, `:`, `>`, `:class:`, `""` |
| Unclosed quote | The closing `"` is missing | `"abc` |
| Text after the closing quote | Something follows the closing `"` | `"abc"x` |
| Trailing backslash inside quotes | A `\` with nothing to escape | `"abc\` |
| A mask can't contain a backslash; quote it if needed | `\` inside a name or folder mask | `\temp\x` |
| A mask can't contain \| < >; quote it if needed | `\|`, `<` or `>` inside a mask | `a\|b` |
| Unknown keyword | Not a known `:` keyword | `:foo` |
| Unknown class | No class by that name | `:class:nosuch` |
| Unknown tag color | Not red, yellow, green, blue or all | `:tag:purple` |
| Unknown attribute | Not in the attribute table | `:attr:z` |
| Malformed list | A `:tag:` or `:attr:` list is broken | `:attr:h++r`, `:tag:+red` |
| Unknown prefix | Not a size or date prefix | `x>1mb` |
| Unknown operator, use | Only `<`, `>`, `<=`, `>=` exist | `=>1mb`, `<>1mb` |
| Invalid number | Not a plain decimal number | `>abc`, `>.5mb`, `>1,5mb`, `>-1mb` |
| Missing unit, use | The number has no unit | `>500` |
| Unknown unit, use | Not a size or time unit | `>500xb`, `d<16k` |
| Unit doesn't match the prefix, use | Size prefix with a time unit, or the reverse | `s<5d` |
| Value too large | Over 2⁶² bytes or 1000 years | `>99999999pb` |
| Not recorded on this platform | A warning: valid, but this platform doesn't record it | `c<5d` on Linux |

Messages that end in "use" are followed by the spellings that would work there.

## Reading the results

### The match bar

While a filter is active, a bar at the top right of the picture shows the match count and size, such as "36 matches · 96.4 GB", and the share of the total, plus a switch for non-matching items:

- **Dim non-matches** (the default) keeps the layout. Blocks that don't match are dimmed, and folders that contain matches stay normal.
- **Hide non-matches** removes everything that doesn't match and lays the picture out again using only the matching sizes. Free space and unknown space are left out too.

Each view remembers its choice. The numbers count the matches below the current location, so zooming in narrows them.

### The Matches panel

While a filter is active, the side panel becomes **Matches**. Clearing the filter brings the details panel back.

- The summary shows the total size and number of matching files, and a bar for their share of what was scanned.
- **Conditions** shows the expression as capsules, one per kind: Name, Classes, Folder, Tags, Size, Logical size, Date modified, Date created, Date accessed, Attributes, and the Excluded kinds for negated conditions. Values within a capsule are joined with "or" or "and" as the rules above combine them.
- The list follows the picture. A matching file that has its own block is one row. Matches inside a collapsed folder or a small-items block are one row for that block, showing how many matching files it holds. The sort menu offers By size, By name and By item count, and each view remembers its sort.
- Rows show tag markers, including tags inherited from a folder.
- The list only covers the current location. When it says "No matches here", go up a level with <kbd>Alt</kbd>+<kbd>↑</kbd> to search a wider area.

Click a row to select its block in the picture. When the block is not drawn at the current location or detail level, the view first navigates to where it is, and Back returns you. Double-click or press <kbd>Enter</kbd> to zoom into it. Right-click for the same menu as in the picture. Hovering a row highlights its block.

## Keys

| Keys | Where | Action |
|---|---|---|
| <kbd>/</kbd> or <kbd>Ctrl</kbd>+<kbd>F</kbd> | Scan tab, outside text boxes | Focus the filter box of the focused pane |
| <kbd>↑</kbd> <kbd>↓</kbd> | Filter box, completion list open | Move through the suggestions |
| <kbd>Tab</kbd> | Filter box, completion list open | Insert the highlighted suggestion and apply |
| <kbd>Enter</kbd> | Filter box | Apply now instead of after the pause |
| <kbd>Esc</kbd> | Filter box | Close the completion list if it is open; otherwise clear the filter; in an empty box, leave it |
| <kbd>↑</kbd> <kbd>↓</kbd> | Matches list | Move between rows (the block is highlighted) |
| <kbd>Home</kbd> <kbd>End</kbd> | Matches list | First or last row |
| <kbd>PgUp</kbd> <kbd>PgDn</kbd> | Matches list | One page up or down |
| <kbd>Space</kbd> | Matches list | Locate the row's block |
| <kbd>Enter</kbd> | Matches list | Zoom into the row |
| <kbd>Delete</kbd> | Matches list | Add the row's file or folder to the delete basket |
| <kbd>Alt</kbd>+<kbd>↑</kbd> | Scan tab | Up one level, to see matches in a wider area |
| <kbd>Ctrl</kbd>+<kbd>K</kbd> | Anywhere | Command palette, where you can preview filters |

On macOS, ⌘ works in place of Ctrl for these keys. All keys are listed in [Keyboard shortcuts](../shortcuts/).

## Filters in the command palette

Type a filter into the [command palette](../command-palette/) (<kbd>Ctrl</kbd>+<kbd>K</kbd>). Anything containing `*`, `?`, `;`, `|`, `<`, `>`, `:` or `"`, or starting with a single `\`, is read as a filter (a text starting with `\\` is a network path). The "Filter …" row shows how many files and how much space it would match before you apply it, and <kbd>Enter</kbd> applies it to the focused view.

Filters you run from the palette are kept in its **Recent** group, newest first, shared by all scans. Each one shows its match count and size in the current scan, so you can reuse them like saved searches.

## Split view

In a [split view](../navigation/), each pane has its own filter box ("Filter this pane") and its own match bar. The side panel shows the matches of the focused pane. Use it to look at two filters of the same scan side by side, for example `:class:video` on the left and `:class:image` on the right.

## Platform differences

- Name matching ignores case on every platform.
- Windows records every attribute in the table. `ads` is accepted but gets the "Not recorded on this platform" warning.
- Linux and macOS only record `hidden`, which means a name starting with a dot. The other attributes get the warning and count as "not set" on every file, so `:attr:r` matches nothing and `:attr:-r` matches everything.
- Linux does not provide creation times. `c` conditions get the warning and match no file.

## Filters from SpaceSniffer

Filters written for SpaceSniffer work in SpaceKnife, and so do the examples in its help (`d<16k` needs `kb`, see below).

These rules give every expression one clear meaning:

- `>` and `<` are strict. Use `>=` and `<=` when the boundary should match.
- Negated size and date conditions are the opposite range: `|>1mb` is `<=1mb`.
- Several bounds on the same side intersect: `>1mb;>2mb` is `>2mb`.
- Spaces after `|` and `\` are ignored. Quote a name that starts with a space.
- Every prefix spelling in the tables works, including `created` and `accessed`.
- Folder masks look at the scan root's own name and below, never at the path above it.
- Spellings that could never match anything are errors instead of empty results: `\temp\x`, `:foo`, an unquoted mask with `|`, `<` or `>`.
- `k` alone is not a unit. Write `d<16kb` instead of `d<16k`.

## Related

- [Tags and file classes](../tags-classes/): tag items, and edit the classes behind `:class:`.
- [Export and reports](../export/): export only the files a filter matches.
- [Command line](../command-line/): run `filter` in a script, without a window.
