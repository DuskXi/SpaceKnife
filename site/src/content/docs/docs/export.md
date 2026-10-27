---
title: Export and reports
description: Export a scan, or only what a filter matches, as a text template, CSV, JSON or a self-contained HTML report, and write your own templates.
---

An export writes what you see to a file: the whole scan or the current location, everything or only the filter matches. There are five built-in text templates and three data formats, and you can write your own templates for the command line.

## Exporting from the app

1. Open the [command palette](../command-palette/) with <kbd>Ctrl</kbd>+<kbd>K</kbd> and run **Export report…**. With an empty input it is in the list straight away; typing `export`, `csv`, `json`, `html` or `report` also finds it.
2. In the **Export report** dialog, pick a template or a format. The list has two groups: Templates (the same template language as SpaceSniffer) and Data formats.
3. Choose the scope and the filter option (see below), then click **Export…** and choose where to save.

When the file is written, a message shows its size and name (for example "Exported 2.4 MB: report.html") and the file is shown in your file manager.

| Option | Shown when | Effect |
|---|---|---|
| Current location / Whole scan | You have zoomed in | Export the zoomed-in folder only, or the whole scan |
| Export only the filter matches | A filter is active (on by default) | Leave out everything the filter doesn't match |

:::note
Comparison views can't be exported yet. Leave the comparison first.
:::

### What a filter does to an export

With a filter applied, an export contains the matching files and the folders that hold them. The export's root is always included. Folder sizes and counts add up only what is exported, so the numbers agree with the filtered view. See [Filters](../filtering/).

## Built-in templates

| In the dialog | Name on the command line | Writes | Suggested file name |
|---|---|---|---|
| Grouped by folder | `Grouped by folder` | One section per folder that directly contains files: its full path and size on disk, followed by its files with right-aligned sizes | `File report.txt` |
| First-level folders | `First level folders` | Each folder on the first level below the root: full path and size on disk | `First level.txt` |
| File list | `Simple file list` | The full path of each file, one per line | `File list.txt` |
| Summary | `Summary` | Total size on disk, number of folders and number of files | `Summary.txt` |
| Folder tree | `Folder tree` | Folders and their size on disk, indented by level | `Folder tree.txt` |

The first four use the same template text as SpaceSniffer's built-in templates. Template names are not case-sensitive on the command line. Grouped by folder looks like this:

```text

/home/lenny/Videos [12.4 GB]
  [  6.1 GB] trip-2024.mp4
  [  4.0 GB] wedding.mov

/home/lenny/Videos/raw [2.3 GB]
  [  2.3 GB] clip-001.mts
```

## Data formats

The three formats walk the tree the same way: depth first, largest first, files and folders mixed. The export's root comes first.

### CSV table

One row per file and folder, UTF-8 with a byte order mark and `\r\n` line ends, so spreadsheet programs open it with the right encoding.

| Column | Contents |
|---|---|
| `path` | Full path |
| `name` | Name |
| `type` | `file`, `folder`, `link` or `other` |
| `depth` | Level below the export's root (the root is 0) |
| `size` | Logical size in bytes (a folder: the total of its files) |
| `disk_size` | Size on disk in bytes (a folder: the total of its files) |
| `files`, `folders` | Number of files and folders below a folder (empty for files) |
| `modified`, `created`, `accessed` | Local time in ISO 8601 with the offset, like `2024-03-05T14:07:09+08:00`; empty when unknown |
| `attributes` | Space-separated, from `readonly hidden system archive compressed encrypted sparse temporary offline notindexed reparse cloud hardlink denied error mount pending` |

:::caution[Spreadsheets and formulas]
A spreadsheet program may treat a name that starts with `=`, `+`, `-` or `@` as a formula. SpaceKnife writes the names unchanged. If that matters, open the file with the program's text import.
:::

### JSON data

An export of `/home/lenny` filtered with `*.jpg`:

```json
{"format": "spaceknife-export", "version": 1, "generator": "SpaceKnife …", "generated_at": "2026-10-18T12:30:00+08:00", "filter": "*.jpg",
"roots": [
{"path": "/home/lenny", "name": "lenny", "type": "folder", "size": 4000, "disk_size": 8192, "files": 2, "folders": 1, "modified": "2026-10-01T09:12:44+08:00", "created": null, "accessed": "2026-10-18T08:02:10+08:00", "attributes": [], "children": [
  {"name": "Photos", "type": "folder", "size": 4000, "disk_size": 8192, "files": 2, "folders": 0, "modified": "2026-09-30T21:40:02+08:00", "created": null, "accessed": "2026-10-17T19:55:31+08:00", "attributes": [], "children": [
    {"name": "a.jpg", "type": "file", "size": 1000, "disk_size": 4096, "modified": "2024-03-05T14:07:09+08:00", "created": null, "accessed": "2026-10-17T19:55:31+08:00", "attributes": []},
    {"name": "b.jpg", "type": "file", "size": 3000, "disk_size": 4096, "modified": "2024-03-05T14:09:51+08:00", "created": null, "accessed": "2026-10-17T19:55:31+08:00", "attributes": []}]}]}
]}
```

- One element per line, indented by level. Only the roots have a `path`. Folders have `files`, `folders` and `children`.
- The fields mean the same as the CSV columns. Unknown times are `null`, and `filter` holds the filter expression when one was applied.
- The keys are in English in every interface language, so tools can rely on them.

### HTML report

A single self-contained file: inline styles, no scripts, no external fonts or images. It follows the system's light or dark mode and prints in light colors. It contains:

- the path, the time it was generated and the filter, if any;
- size on disk, logical size, number of files (or of files matching the filter) and of folders;
- the folder structure, collapsible, 4 levels deep with the 30 largest items per level and the rest merged into one row;
- the 50 largest files;
- the 20 extensions that take the most space.

The report's size does not grow with the size of the scan, so it stays easy to open and share. Its text is in the interface language.

### Common rules

- Every export is UTF-8. The file is written to a temporary name first and renamed when complete, so a failed export never leaves half a file.
- The folder you export to must exist.
- A scan with several roots exports each root in turn into the same file.
- Extra hard links of a file count with size 0, the same as in the picture.
- Names that are not valid Unicode are written as `\udcXX` escapes, which in JSON is the string's own escape.

## Writing your own template

A template is a text file, usually with the extension `.sktpl`. The app's dialog offers the built-in templates; your own templates are used from the [command line](#from-the-command-line).

### Template files

```text
@name=File sizes
@file=sizes.txt
@blocks=files
@sort=size
@order=desc
# comment lines start with #
@header
Files under <%pathfile%>{&br}
@detail
{if{<%isfile%>}=={true}?{<%disksize%>  <%pathfile%>{&br}}}
@footer
Total: <%disksize%>{&br}
```

On a small folder it prints:

```text
Files under /home/lenny
4.0 KB  /home/lenny/notes.txt
4.0 KB  /home/lenny/Photos/a.jpg
4.0 KB  /home/lenny/Photos/b.jpg
8.0 KB  /home/lenny/Photos/raw/c.nef
Total: 20.0 KB
```

| Line | Meaning | Default |
|---|---|---|
| `@name=` | Display name | The file name |
| `@file=` | Suggested output file name | |
| `@blocks=files`, `folders` or `mixed` | Within a folder: files first, folders first, or no distinction | `mixed` |
| `@sort=size`, `name` or `none` | Sort by size on disk, by name (ignoring case), or keep the listing order | `size` |
| `@order=desc` or `asc` | Largest (or Z) first, or the reverse | `desc` |
| `@header` | Starts the header: evaluated once, on the export's root | empty |
| `@detail` | Starts the detail: evaluated for every element | empty |
| `@footer` | Starts the footer: evaluated once, on the export's root | empty |

- A file is a template file when its first character is `@`. Otherwise the whole text is the detail section with the default sorting, so a SpaceSniffer detail text works as it is.
- Sections can come in any order. A section runs from its marker line to the next marker or the end of the file.
- The last line break of each section is not part of it, so a one-line detail doesn't print an extra line per element. Add an empty line or `{&br}` when you want the line break.
- To output a line that reads `@footer` itself, start it with an empty group: `{}@footer`.
- Files are read as UTF-8, with or without a byte order mark. Line ends are normalized to `\n`. Unknown options and unreadable lines give a warning and are ignored.

### Elements and order

The elements are every file and folder below the export's root, plus the root itself (level 0). The walk is depth first: a folder comes before its content, and each folder's content is ordered by `@blocks`, `@sort` and `@order`. The header and footer are evaluated on the root, so `<%disksize%>` there is the total.

### The language

A section is made of three things, output from left to right:

| Part | Written as | Result |
|---|---|---|
| Text | Anything except `{`, `}` and `<%` | Output as it is, spaces and line breaks included |
| Tag | `<%name%>` | A property of the current element; names ignore case |
| Group | `{…}` | A function call when it starts with a function name; otherwise just its content |

- Braces must pair. Output `{` and `}` with `{&curlyop}` and `{&curlycl}`, and `<%` with `{&angleop}%`.
- A group that starts with `if`, `leftpad`, `rightpad`, `nest`, `html`, `csv` or `json` (followed by a space, `{` or `}`) is a call. To output text that starts with one of those words, put an empty group first: `{{}if you like}`.
- Function arguments are the groups that follow, with optional spaces between them. Arguments can contain calls, so `if`s nest.
- **Script mode.** When a section contains `{script}`, all line breaks and tabs are removed from it before it is read, so you can lay out nested `if`s over several lines. Spaces are kept: don't indent the outermost level with spaces, or every element prints them. Write line breaks explicitly with `{&br}`.

### Tags

| Tag | For a file | For a folder |
|---|---|---|
| `file` | Name with extension | Name; for the scan root its last part, for a drive or file-system root the full path |
| `filename` | Name without the extension (`a.tar.gz` gives `a.tar`) | Name |
| `fileext` | Extension without the dot, original case; empty if none | Empty |
| `path` | Full path of the containing folder | The same; for the export's root, its parent |
| `pathfile` | Full path | Full path |
| `size`, `sizebytes` | Logical size, formatted or in bytes | Total of the files below |
| `disksize`, `disksizebytes` | Size on disk, formatted or in bytes | Total of the files below |
| `filemodifydate`, `filemodifytime`, `filemodifyage` | Date modified as `2024-03-05`, time as `14:07:09` (local), or whole days ago | The folder's own time |
| `filecreatedate`, `filecreatetime`, `filecreateage` | The same for the date created | The same |
| `fileaccessdate`, `fileaccesstime`, `fileaccessage` | The same for the date accessed | The same |
| `isfile`, `isfolder`, `iscontainer` | `true`, `false`, `false` | `false`, `true`, `true` |
| `count`, `localcount` | `0` | Files and folders below / directly inside (not counting itself) |
| `filecount`, `localfilecount` | `0` | Files below / directly inside |
| `foldercount`, `localfoldercount` | `0` | Folders below / directly inside |
| `containercount`, `localcontainercount` | `0` | Same as `foldercount`, `localfoldercount` |
| `containsfiles`, `containsfolders`, `containscontainers` | `false` | `true` when it directly contains files / folders |
| `nestinglevel` | Level: the export's root is `0`, its children `1` | The same |

- Formatted sizes look like `4.9 KB`. Windows and Linux count 1 KB as 1024 bytes, macOS as 1000.
- An unknown time gives an empty date, time and age.

### Functions

| Written as | Result |
|---|---|
| `{&br}`, `{&tab}` | Line break (`\n`), tab |
| `{&curlyop}`, `{&curlycl}`, `{&angleop}`, `{&anglecl}` | `{`, `}`, `<`, `>` |
| `{if{a}=={b}?{then}}` | `then` when the comparison holds, otherwise nothing |
| `{if{a}=={b}?{then}:{else}}` | `then` or `else`. Operators: `==`, `!=`, `<`, `<=`, `>`, `>=` |
| `{leftpad{text}{pad}{length}}` | Repeats `pad` on the left until `text` is `length` characters long; never cuts `text` |
| `{rightpad{text}{pad}{length}}` | The same on the right |
| `{nest{text}}` | `text` repeated `nestinglevel` times, for indentation |
| `{html{text}}` | `text` escaped for HTML |
| `{csv{text}}` | `text` as one CSV field, quoted when needed |
| `{json{text}}` | `text` as a JSON string, quotes included |
| `{script}` | Turns on script mode for the section; outputs nothing |

Comparisons trim spaces first. When both sides are decimal numbers they compare as numbers (`10 > 9`, `1.50 == 1.5`); otherwise as text, ignoring case (`{JPG}=={jpg}` holds). `true` and `false` are plain text.

### When something is wrong

Mistakes leave a marker in the output and produce a message with the section, line and column:

| Marker | Cause |
|---|---|
| `*BAD TAG*` | Unknown tag |
| `*BAD IF*` | An `if` written wrongly (missing value, operator, `?` or `}`) |
| `*BAD PAD*` | `leftpad` or `rightpad` with the wrong arguments, an empty pad, or a length that isn't a whole number from 0 to 10000 |
| `*BAD CALL*` | `nest`, `html`, `csv` or `json` with the wrong number of arguments |

An unclosed `<%` is output as it is, an unclosed `{` is treated as closed at the end, and a stray `}` or unknown `{&…}` only gives a warning. The command line refuses to export with a template that has errors and lists each one; with warnings only, it exports and shows them.

### Examples

A CSV of files with exact sizes:

```text
@name=Files as CSV
@file=files.csv
@blocks=files
@header
path,bytes,modified{&br}
@detail
{if{<%isfile%>}=={true}?{{csv{<%pathfile%>}},<%disksizebytes%>,<%filemodifydate%>{&br}}}
```

JSON Lines, one object per file. The braces of the object are written with `{&curlyop}` and `{&curlycl}`:

```text
@name=JSON lines
@file=files.jsonl
@detail
{if{<%isfile%>}=={true}?{{&curlyop}"path": {json{<%pathfile%>}}, "bytes": <%disksizebytes%>{&curlycl}{&br}}}
```

A Markdown outline of the top three levels of folders:

```text
@name=Folder outline
@file=outline.md
@blocks=folders
@detail
{script}
{if {<%isfolder%>} == {true}
  ? {{if {<%nestinglevel%>} <= {2}
      ? {{nest{  }}- <%file%> (<%disksize%>){&br}}}}}
```

The output:

```text
- lenny (144 KB)
  - Projects (136 KB)
    - old (124 KB)
```

Files not modified for more than a year, with their date:

```text
{if{<%isfile%>}=={true}?{{if{<%filemodifyage%>}>{365}?{<%filemodifydate%>  <%pathfile%>{&br}}}}}
```

That last one has no `@` lines, so the whole text is the detail section.

:::tip
Combine templates with filters. A filter can do the selecting (`m>1y;>100mb`) and the template only the formatting, which keeps templates short.
:::

## From the command line

`spaceknife-backend run` (the program that comes with the app, see [Command line](../command-line/)) runs scan, filter and export steps without opening a window:

```shell
spaceknife-backend run scan "/home/lenny" filter "*.iso;>1gb" export csv "/home/lenny/isos.csv" autoclose
spaceknife-backend run scan "C:\Users\lenny" filter ":class:video;>1gb" export "Grouped by folder" "C:\Users\lenny\videos.txt" autoclose
spaceknife-backend run load "/home/lenny/home.sks" export html "/home/lenny/report.html" export "/home/lenny/outline.sktpl" "/home/lenny/outline.md"
```

- `export "<template>" "<file>"` looks the template up as a built-in template name first, then as `csv`, `json` or `html`, then as the path of a template file.
- Every `export` after a `filter` exports only the matches. `save` always writes the complete tree.
- The command line exports the whole view, every root of the scan.
- `:class:` filters use your file classes. Tag conditions match nothing there, because saved tags are not loaded.
- The output folder must exist, and the file is written atomically, as in the app.
