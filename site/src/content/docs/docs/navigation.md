---
title: Navigating the picture
description: Zoom in and out, walk the history, use the breadcrumb, the details panel, hover cards and the context menu, work with split view, tabs and the results list, and jump to any path, with every keyboard shortcut.
---

Each view keeps its own **zoom root** (the folder that fills the view), its own selection and its own back and forward history. Everything on this page works the same in the treemap, the flame graph and the polar flame graph (see [Visualizations](../visualizations/)), and almost everything has a key.

:::note[When keys work]
- Keys act on the **focused view**: in split view, the pane you clicked last.
- Single-key shortcuts (<kbd>V</kbd>, <kbd>C</kbd>, <kbd>F</kbd>, <kbd>U</kbd>, <kbd>1</kbd>–<kbd>4</kbd>, <kbd>0</kbd>, <kbd>/</kbd>, <kbd>Backspace</kbd>…) don't fire while you type in a text field such as the filter box, the path field or the command palette.
- A focused button or menu item keeps <kbd>Enter</kbd>, <kbd>Delete</kbd>, <kbd>Esc</kbd> and <kbd>Shift</kbd>+<kbd>F10</kbd> for itself; a focused drop-down list keeps <kbd>C</kbd> and <kbd>V</kbd>. Click the picture to get them back.
- On macOS you can press <kbd>⌘</kbd> wherever this page says <kbd>Ctrl</kbd> (<kbd>Ctrl</kbd> works too). <kbd>Alt</kbd> is <kbd>Option</kbd>.
:::

## Zooming in and out

**Zoom in** makes a folder the new zoom root: its contents fill the view and the step goes into the view's history.

| How | Notes |
|---|---|
| Double-click a folder | On its title bar or frame, a collapsed block, a bar or a sector |
| Select it, then <kbd>Enter</kbd> | Also the **Zoom in** command in the palette |
| Context menu › **Show only this (zoom in)** | Same as <kbd>Enter</kbd> |
| Double-click a file | Zooms into the folder that holds the file and selects the file |
| Double-click "*N* small items" or "+*N* more" | Lays out the merged items one by one (see [Small items](../visualizations/#small-items-and-n-more)) |

Free space and unknown space can't be zoomed into.

**Zoom out** with the history keys below, or double-click the flame graph's root bar or the polar flame graph's center disc to go up one level.

The zoom is animated: the block you double-clicked grows to fill the view, and going back shrinks it into its place again. Set the length in **Settings › Visualization › Zoom animation duration** (0 to 600 ms, 250 by default, 0 turns it off). With **Settings › Appearance › Reduce motion** on, zooming is instant.

## Back, forward, up and root

| Action | Keys | Toolbar button | What happens |
|---|---|---|---|
| Back | <kbd>Backspace</kbd>, <kbd>Alt</kbd>+<kbd>←</kbd> | **Back** | Returns to the previous zoom root and restores the selection you had there |
| Forward | <kbd>Shift</kbd>+<kbd>Backspace</kbd>, <kbd>Alt</kbd>+<kbd>→</kbd> | **Forward** | Undoes a Back |
| Up one level | <kbd>Alt</kbd>+<kbd>↑</kbd> | **Up one level** | Zooms out to the parent folder and selects the folder you just left |
| Root | <kbd>Ctrl</kbd>+<kbd>Home</kbd> | **Root** | Returns to the top of the scan; on a whole-volume scan the free and unknown space come back |

- Every tab and every split pane has its own history. Up and Root are recorded too, so Back undoes them.
- Up and Root are disabled at the top of the scan.
- Places that no longer exist (deleted, or gone after a rescan) are skipped when you go back or forward.
- Changing the visualization, the coloring or the detail level is not recorded: Back always means "the previous place".

:::tip[Up, then Enter]
Because Up selects the folder you came from, <kbd>Alt</kbd>+<kbd>↑</kbd> followed by <kbd>Enter</kbd> takes you back in. Use <kbd>Alt</kbd>+<kbd>↑</kbd> to look at a folder's neighbors, then <kbd>Backspace</kbd> or <kbd>Enter</kbd> to return.
:::

## Breadcrumb and path editing

The breadcrumb on the toolbar shows the path from the top of the scan to the zoom root.

- Click an earlier segment to go up to it. Each level is one step in the history, so Back retraces them one at a time.
- At the top of a whole-volume scan the breadcrumb shows the tab's name and **Whole volume**.
- Inside a merged block the last segment is "*N* small items" or "+*N* more".
- Click the last segment, or press <kbd>Ctrl</kbd>+<kbd>L</kbd>, to edit the path. The full path is selected; type or paste another one and press <kbd>Enter</kbd>. A folder becomes the zoom root; a file opens its folder with the file selected. <kbd>Esc</kbd> cancels. A path outside the scan says "The path doesn't exist or is outside this scan."

In split view each pane has its own breadcrumb, which you can click to go up. Path editing with <kbd>Ctrl</kbd>+<kbd>L</kbd> lives in the toolbar's breadcrumb, so in split view type the path in the command palette instead (see [Finding a path](#finding-a-path)).

## Selection and the details panel

**Click** a block to select it. Clicking a folder's title bar, frame or padding selects the folder. Click an empty spot or press <kbd>Esc</kbd> to clear the selection. Right-clicking selects too, before it opens the menu.

The selection is kept by identity: it stays when the view is redrawn, when you change the level or the visualization, and while the scan updates the picture. If the selected item is hidden inside a collapsed folder, the ring is drawn on its nearest visible ancestor.

A selected block gets a white ring and a soft shadow (turn the shadow off with **Selection shadow**), and its ancestors light up (**Highlight halo levels**, both in **Settings › Appearance**).

### The details panel

The panel on the right shows the selection, or the zoom root when nothing is selected. It has three tabs:

| Tab | Shows |
|---|---|
| **Details** | Name and path; the size, with the number of files and folders and the latest change for a folder, or the file class and modification time for a file (hover that line for the exact number of bytes); when the folder is [classed by folder](../tags-classes/#classes-by-folder), a line such as "Everything here counts as Games · Steam library"; **Share** of the parent folder and of the volume's used space; for a folder, **Largest items**: the five largest files anywhere below it; **Tags** |
| **Classes** | The selection's make-up by file class: a bar split in the class colors, then a row per class with its files, size, share and a bar, largest first. Click a row to filter by that class. **Code** and **Data** fold away while they hold less than 2% and less than 8 GB; **Show all classes** brings them back. For files a folder class took over, the tooltip says what their extensions make them, such as "By their extensions these are: Video 3.0 GB". Hover a row or a segment of the bar and the picture lights that class up: the rest dims, while the class's files, the small-items blocks mostly of it and the folders that count as it whole (a game install, say) keep their colors with a glowing edge, within the selected folder only. Move away and it's back; the filter stays as it is |
| **Largest files** | Up to 50 of the largest files below it |

Click a file in **Largest items** or **Largest files** to **locate** it: it is selected, and if it isn't drawn (too deep or too small) the view zooms in, with history, until it is.

At the bottom are the actions **Show in Explorer** (**Show in Finder** on macOS, **Show in file manager** on Linux), **Copy path** and **Add to delete basket**, each with its key. The **More actions** button opens the context menu, like <kbd>Shift</kbd>+<kbd>F10</kbd>. **Hide details panel** gives the picture the whole width; **Show sidebar** at the right edge brings it back.

Drag the panel's left edge to resize it (280 to 640 pixels, always leaving the picture at least 360), and the picture is laid out again as you drag. Double-click the edge for the default 320 pixels. The width is remembered. With the edge focused, <kbd>←</kbd> and <kbd>→</kbd> resize it too (64 pixels at a time with <kbd>Shift</kbd>).

The panel changes with what you do:

- While a filter is active it shows **Matches**, the [results list](#the-results-list).
- While a scan runs and nothing is selected it shows **Scan details**.
- When the delete basket is open it shows the basket.
- For a merged block, free space or unknown space it explains what the block is.

### Keys for the selection

| Keys | Action |
|---|---|
| <kbd>Enter</kbd> | Zoom in |
| <kbd>Esc</kbd> | Clear the selection (in a comparison with nothing selected: leave the comparison) |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | Copy the path |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> | Show in Explorer / Finder / file manager |
| <kbd>Ctrl</kbd>+<kbd>`</kbd> | Open a terminal in the folder (for a file: in its folder) |
| <kbd>Delete</kbd> | Add to the delete basket |
| <kbd>1</kbd>–<kbd>4</kbd> or <kbd>Ctrl</kbd>+<kbd>1</kbd>–<kbd>4</kbd> | Red, yellow, green or blue tag |
| <kbd>0</kbd> or <kbd>Ctrl</kbd>+<kbd>0</kbd> | Clear the tag of the item and everything below it |
| <kbd>Shift</kbd>+<kbd>F10</kbd>, menu key | Open the context menu |

<kbd>Delete</kbd> only puts the item in the delete basket; nothing leaves the disk until you act in the basket (see [Freeing up space](../cleanup/)). The keys without <kbd>Ctrl</kbd> for tags can be turned off with **Settings › Tags › Ctrl is optional when the treemap has focus**; more in [Tags and file classes](../tags-classes/).

## Hover cards

Rest the pointer on a block for a moment and a card appears beside it, placed so that it doesn't cover the block. It shows:

- the name and the folder it is in;
- the size and its share of the parent folder;
- for a folder, the number of files and folders; for a file, its file class and extension;
- the **Created**, **Modified** and **Accessed** times, with how long ago.

Once a card is open, moving to another block switches the card's content at once. Three switches in **Settings › General › Hover tips** control this: **Keep open when moving between blocks**, **Show dates and times** and **Show file age**.

- In the polar flame graph the card follows the pointer.
- No card is shown while a zoom animation runs.
- **Hover highlight** (**Settings › Appearance**) draws a thin outline around the block under the pointer (white in the dark theme, near-black in the light one); **Mouse trail** leaves fading outlines on the blocks the pointer passes.
- Hovering a row in the results list highlights its block in the picture the same way.

## Context menu

Right-click a block, a bar or a sector to open its menu; the block is selected first. From the keyboard, <kbd>Shift</kbd>+<kbd>F10</kbd> or the menu key opens the menu of the selection, placed beside it. In the flame graphs with nothing selected, it opens the zoom root's menu (the root bar or center disc); in the treemap it needs a selection.

The menu starts with a header (name, size, type, parent folder) and then:

| Item | Keys | Does |
|---|---|---|
| **Open** | | Opens the file with its default app, or the folder |
| **Show in Explorer** (Finder, file manager) | <kbd>Ctrl</kbd>+<kbd>E</kbd> | Shows it in the system's file manager |
| **Open terminal here** | <kbd>Ctrl</kbd>+<kbd>`</kbd> | A terminal in the folder, or in a file's folder |
| **Copy path** ▸ | | **As is** (<kbd>Ctrl</kbd>+<kbd>C</kbd>), **Quoted**, **POSIX path**, **file:// URI**; each shows a preview |
| **Add tag** ▸ | | **Red tag**, **Yellow tag**, **Green tag**, **Blue tag** (<kbd>Ctrl</kbd>+<kbd>1</kbd>–<kbd>4</kbd>), **Clear tag** (<kbd>Ctrl</kbd>+<kbd>0</kbd>) |
| **Show only this (zoom in)** | <kbd>Enter</kbd> | Zooms in |
| **Classify** ▸ (folders only) | | **Automatic**, **By extension only**, or **Count as** a class: every file in the folder counts as that class. See [Classes by folder](../tags-classes/#classes-by-folder) |
| **Rescan this folder** | | Reads the folder again with everything below it (for a file: its folder) |
| **Add to delete basket** | <kbd>Delete</kbd> | Puts it in the delete basket |
| **System menu…** | | Listed, not available yet |

Inside the menu:

| Keys | Action |
|---|---|
| <kbd>↑</kbd> / <kbd>↓</kbd> | Move |
| <kbd>→</kbd> or <kbd>Enter</kbd> on **Copy path**, **Add tag** or **Classify** | Open the submenu |
| <kbd>←</kbd> | Close the submenu |
| <kbd>Enter</kbd> | Run the highlighted item |
| <kbd>Esc</kbd> | Close the submenu, then the menu |

An item that can't be used stays in the menu: hover it, or choose it, to see why. A merged block, for example, has no single place to open or copy. The menu closes when you click elsewhere, resize the window or switch to another app. On the root bar and the center disc there is no **Show only this (zoom in)**, because that is already the zoom root.

## Split view

Split view shows two views of the same scan side by side. Turn it on with the **Split view** button on the toolbar or the **Split view** command in the palette; use either again to go back to one view (the focused pane stays).

- The new pane starts at the top of the scan with the focused pane's visualization, coloring, detail level and free and unknown space switches.
- From then on each pane has its own zoom, history, selection, filter, visualization and level.
- Each pane has a header with its own breadcrumb (click a segment to go up), its size, the number of matches while it filters, its own filter box, and a button to close it (**Close left pane**, **Close right pane**). Close one and the other takes the whole area.
- Click inside a pane to focus it; its header is highlighted. The toolbar, the keys and the details panel follow the focused pane.
- Split view is not available while comparing snapshots.

Ideas: a treemap and a flame graph of the same place; a whole drive on the left and one deep folder on the right; the same area with and without a filter.

## Tabs

Each scan opens in its own tab in the title bar. The tab shows the scan's state after its name ("· Scanning", "· Paused", "· Incomplete", "· Scan failed"), and its tooltip shows the file system and size.

| Keys | Action |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>T</kbd> | New tab, on the start page (**New scan**) |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> | Close the tab; middle-click a tab or its × does the same |
| <kbd>Ctrl</kbd>+<kbd>Tab</kbd>, <kbd>Ctrl</kbd>+<kbd>PageDown</kbd> | Next tab |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Tab</kbd>, <kbd>Ctrl</kbd>+<kbd>PageUp</kbd> | Previous tab |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>PageUp</kbd> / <kbd>PageDown</kbd> | Move the current tab one place left / right |
| <kbd>Ctrl</kbd>+<kbd>,</kbd> | Settings |

- Tab switching wraps around.
- Drag a tab sideways to change the order: it takes a neighbour's place once the pointer passes that tab's middle. <kbd>Esc</kbd> while dragging puts every tab back. Moving the tabs with the keys stops at the ends.
- Closing a tab that is still scanning stops that scan. Closing the last tab leaves an empty **New scan** tab.
- Settings opens as a tab of its own, and only once. In it, <kbd>/</kbd> or <kbd>Ctrl</kbd>+<kbd>F</kbd> jumps to the settings search, and <kbd>Backspace</kbd> / <kbd>Alt</kbd>+<kbd>←</kbd> and <kbd>Alt</kbd>+<kbd>→</kbd> move back and forward through the settings pages you visited.

## The results list

While a filter is active (<kbd>/</kbd> or <kbd>Ctrl</kbd>+<kbd>F</kbd>, see [Filters](../filtering/)), the details panel becomes **Matches**: the total size and count of the matches, their share of what was scanned, the filter's conditions, and a list.

The list follows the picture, and only covers what is below the zoom root:

- a matching file that is drawn on its own is one row;
- matches hidden in a collapsed folder or in a merged block are grouped into that block's row, with the number of items;
- matches under blocks too small to draw at all are one last row, "*N* more items".

So the rows add up to the total. If the list says **No matches here**, go up a level (<kbd>Alt</kbd>+<kbd>↑</kbd>) to search a wider area. Sort it **By size**, **By name** or **By item count** with the menu at the top.

| Mouse or key | Action |
|---|---|
| Hover a row | Highlights its block in the picture |
| Click | Locates the block: selects it, zooming in if needed |
| Double-click, <kbd>Enter</kbd> | Zooms in |
| Right-click | Opens the block's context menu |
| <kbd>↑</kbd> / <kbd>↓</kbd>, <kbd>Home</kbd> / <kbd>End</kbd>, <kbd>PageUp</kbd> / <kbd>PageDown</kbd> | Moves through the list, highlighting each block |
| <kbd>Space</kbd> | Locates the highlighted row |
| <kbd>Delete</kbd> | Adds the file or folder to the delete basket |

Keys with <kbd>Alt</kbd> or <kbd>Ctrl</kbd> still go to the view, so <kbd>Alt</kbd>+<kbd>↑</kbd> and <kbd>Ctrl</kbd>+<kbd>Home</kbd> work while the list has focus.

:::tip[Walk through the matches]
Filter, click the first row, then press <kbd>↓</kbd> repeatedly: each match lights up in the picture. Press <kbd>Space</kbd> to select one, <kbd>Delete</kbd> to collect it for cleanup, and carry on.
:::

## Finding a path

The command palette (<kbd>Ctrl</kbd>+<kbd>K</kbd>) finds any file or folder in the scan. See [Command palette](../command-palette/) for everything it does.

- **By name**: type part of a name. Exact names come first, then names that start with it, then names that contain it, then names with the letters in order; within each group the largest come first. <kbd>Enter</kbd> on a result zooms to it: a folder becomes the zoom root, a file opens its folder with the file selected.
- **By path**: type a path such as `C:\Users\lenny\Videos`, `/home/lenny/Videos` or one starting with `~`. The first row is **Go to** that path. Type a folder path ending in a separator and the palette lists what is inside; <kbd>Tab</kbd> completes the highlighted row, so you can walk down folder by folder. A path outside the scan offers **Scan this location**.
- **Like the selection**: select something, open the palette and leave the input empty. The paths group lists up to five other items whose names contain the selected one's name ("Matching the selection …"). Select one `node_modules` folder and you see others at once.

Places you jumped to show up under **Recent** next time.

## All keys on this page

| Keys | Action |
|---|---|
| Double-click, <kbd>Enter</kbd> | Zoom in |
| <kbd>Backspace</kbd>, <kbd>Alt</kbd>+<kbd>←</kbd> | Back |
| <kbd>Shift</kbd>+<kbd>Backspace</kbd>, <kbd>Alt</kbd>+<kbd>→</kbd> | Forward |
| <kbd>Alt</kbd>+<kbd>↑</kbd> | Up one level |
| <kbd>Ctrl</kbd>+<kbd>Home</kbd> | Root |
| <kbd>Ctrl</kbd>+<kbd>L</kbd> | Edit the path |
| <kbd>/</kbd>, <kbd>Ctrl</kbd>+<kbd>F</kbd> | Focus the filter |
| <kbd>Ctrl</kbd>+<kbd>K</kbd> | Command palette |
| <kbd>Esc</kbd> | Clear the selection |
| <kbd>Shift</kbd>+<kbd>F10</kbd>, menu key | Context menu |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> / <kbd>Ctrl</kbd>+<kbd>E</kbd> / <kbd>Ctrl</kbd>+<kbd>`</kbd> | Copy path / show in file manager / open terminal |
| <kbd>Delete</kbd> | Add to the delete basket |
| <kbd>1</kbd>–<kbd>4</kbd>, <kbd>0</kbd> (with or without <kbd>Ctrl</kbd>) | Tag, clear tag |
| <kbd>Ctrl</kbd>+<kbd>T</kbd> / <kbd>Ctrl</kbd>+<kbd>W</kbd> | New tab / close tab |
| <kbd>Ctrl</kbd>+<kbd>Tab</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Tab</kbd> | Next / previous tab |
| <kbd>Ctrl</kbd>+<kbd>PageDown</kbd> / <kbd>Ctrl</kbd>+<kbd>PageUp</kbd> | Next / previous tab |
| <kbd>Ctrl</kbd>+<kbd>,</kbd> | Settings |

The keys for the visualization itself (<kbd>V</kbd>, <kbd>C</kbd>, <kbd>F</kbd>, <kbd>U</kbd>, the detail level) are in [Visualizations](../visualizations/#keys-on-this-page); every key is in [Keyboard shortcuts](../shortcuts/).
