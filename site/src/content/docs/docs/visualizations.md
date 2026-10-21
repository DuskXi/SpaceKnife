---
title: Visualizations
description: The treemap, the flame graph and the polar flame graph, how to switch between them, the four colorings, detail levels and what every special block means.
---

SpaceKnife can draw a scan three ways. All three show the same data and share the view's state: the same zoom, selection, filter and history. Size becomes area in the treemap, length in the flame graph and angle in the polar flame graph. Pick the one that answers your question best, and switch at any time with <kbd>V</kbd>.

## The three visualizations

### Treemap

The treemap fills the whole view with the zoom root's contents. Every block's area is proportional to the space it takes on disk.

- An open folder is a frame with a title bar (name and size). Its contents are laid out inside it, and so on down.
- Files are solid blocks in their file class color (see [Coloring](#coloring)).
- By default larger blocks go to the top left of their folder. Turn off **Sort by size** in **Settings › Visualization** to order by name instead; positions then stay steadier while a scan runs.
- Deeper frames get a slightly lighter background so you can tell the levels apart. **Level contrast** and **Border contrast** in **Settings › Appearance** make the difference stronger or softer.
- A folder on the last [detail level](#detail-levels), or one too small to hold its contents, is drawn as a solid collapsed block that keeps its name and size. Zoom in to open it.
- Blocks big enough for text show their name, larger ones also their size. Long names end in "…". Hover a block that is too small for text to see what it is.

**Block proportions** (**Settings › Visualization**) makes treemap blocks taller or wider; 0 keeps them as square as possible. It only affects the treemap.

### Flame graph

The flame graph stacks one band per level. The zoom root is the **root bar**, which spans the full width. Its children sit in the next band, each one inside its parent's span, with a length proportional to its size.

- By default the root bar is at the bottom and the levels grow upward. To flip it, set **Settings › Visualization › Flame graph direction** to **Root at top**. Open flame graphs change at once.
- The root bar shows the zoom root's name in bold, followed by its size.
- Bars are 20 to 48 px tall, scaled with **Font size**. The number of bands is the detail level or what fits in the view's height, whichever is smaller.
- Folders, open or collapsed, are solid bars, so you can read a deep path from the root bar outward in one line.

The flame graph is good for deep, narrow trees: a long chain of nested folders stays readable, and widths in one band are easy to compare.

### Polar flame graph

The polar flame graph is the flame graph wrapped into a circle. The zoom root is the **center disc** and level *k* is the *k*-th ring outward.

- Items start at 12 o'clock and run clockwise, largest first (with **Sort by size** on). Each sector's angle is proportional to its size, and a child sector stays inside its parent's angle.
- The center disc shows the zoom root's name and size. While you hover a sector, the disc shows that sector's name, its size and its share of the zoom root ("{pct} of total").
- Rings are at least 14 px wide, scaled with **Font size**. The number of rings is the detail level or what fits in the radius, whichever is smaller.
- Outer rings are longer, so they have room for smaller items: small folders deep down show up as thin spikes on the outside.
- Labels follow the sector's shape. A thick, narrow sector writes its name along the radius; the others write along the arc, with the size as a second line when it fits. Text is always upright.
- The hover card follows the pointer here, because a large sector can cover most of the disc.

### The root bar and the center disc

In both flame graphs the zoom root is drawn too, as the root bar or the center disc. It answers the mouse like this:

| Action | Result |
|---|---|
| Click | Clears the selection; the details panel shows the zoom root |
| Double-click | Goes up one level |
| Right-click | Opens the zoom root's context menu (without **Show only this (zoom in)**) |
| Hover | Shows the zoom root's hover card |
| <kbd>Shift</kbd>+<kbd>F10</kbd> with nothing selected | Opens the zoom root's context menu |

The treemap has no such element: its zoom root is the whole view.

## Switching

| How | What it does |
|---|---|
| Toolbar, **Visualization** group (icons, left of the level control) | Click a visualization |
| <kbd>V</kbd> | Next visualization, wrapping around |
| <kbd>Shift</kbd>+<kbd>V</kbd> | Previous visualization |
| Command palette, **Cycle visualization** | Same as <kbd>V</kbd>; its status line says which one is current |
| Command palette, type `visualization` | **Visualization: treemap**, **Visualization: flame graph**, **Visualization: polar flame graph** pick one directly |

What switching keeps and what it doesn't:

- **Kept**: the zoom root (also a zoom into a "small items" block), back and forward history, the selection, the filter and its dim or hide mode, the detail level and the free and unknown space switches.
- **Changed**: the coloring becomes the one the target visualization last used (see [Coloring](#coloring)).
- **Closed**: the hover card and the context menu.
- **Not recorded in history**: <kbd>Backspace</kbd> never takes you back to the previous visualization.

The two pictures cross-fade for 200 ms; with **Reduce motion** on they swap at once. You can switch while a scan runs, while it is paused, on an opened snapshot and in a comparison. The scan itself is not affected.

A visualization this window can't show stays on the toolbar, disabled, and its tooltip says why. <kbd>V</kbd> skips it.

Every tab and every split pane has its own visualization. New views start with **Settings › Appearance › Visualization › Default visualization for new views**.

:::tip[Two views of one scan]
Turn on split view and set one pane to the treemap and the other to the flame graph. The treemap shows where the big files are; the flame graph shows how deep they sit. See [Split view](../navigation/#split-view).
:::

## Coloring

The toolbar's **Coloring** group has four modes. <kbd>C</kbd> steps through them in this order: Classes → Flat → Time → Branch → Classes. Coloring changes colors only; the layout stays where it is.

| Mode | Toolbar | Command palette | Color shows |
|---|---|---|---|
| File classes | **Classes** | **Coloring: file classes** | The file class, found by the extension: Video, Pictures, Audio, Archives, Programs, Documents, Code, Data, and Other for the rest. Edit the classes in **Settings › File classes**. |
| Flat | **Flat** | **Coloring: flat** | Nothing: every file has the same neutral color, so only size and structure remain |
| Date modified | **Time** | **Coloring: date modified** | How recently a file changed, in five bands: under a day, a week, a month, a year, and older |
| Top-level branch | **Branch** | **Coloring: top-level branch** | Which top-level item a block belongs to |

The four **Coloring: …** commands only appear when you type in the palette; **Cycle coloring** (<kbd>C</kbd>) is always there.

More about the modes:

- **Branch**: each item directly under the zoom root gets its own hue (there are eight), and everything below it keeps that hue, shading darker with depth. Merged blocks on the top level and the free and unknown space stay neutral gray. A new branch gets the hue used least on screen that also differs from its neighbors, and a branch keeps its hue while a scan reorders the blocks. The hues are handed out again when the zoom root changes.
- **Time** and **Classes** on merged blocks: a "small items" block takes the time band that holds most of its bytes, and the file class that holds at least half of its bytes (otherwise **Other**).
- **Folders**: in the treemap, open folder frames always use their level colors, and collapsed folders and "+N more" blocks use a neutral collapsed color (in **Branch** they take the branch hue). In the flame graphs, folder bars use four gray tones by depth, again except in **Branch**.

Each visualization remembers its own coloring in each view. The treemap starts with **Classes**, the flame graph and the polar flame graph with **Branch**. Switch a treemap to **Time**, move to the flame graph and back, and the treemap is still on **Time**. The defaults for new views are in **Settings › Appearance**: **Default treemap coloring** and **Default coloring for flame graphs and polar flame graphs**.

## Detail levels

The detail level says how many levels below the zoom root are drawn. At level *N* the folders of levels 1 to *N* − 1 are open and the folders on level *N* are collapsed; nothing deeper is drawn.

| Keys | Toolbar | Command palette |
|---|---|---|
| <kbd>Ctrl</kbd>+<kbd>=</kbd> (or <kbd>Ctrl</kbd>+<kbd>+</kbd>) | **+** on the **Level** control | **Raise detail level** |
| <kbd>Ctrl</kbd>+<kbd>-</kbd> | **−** on the **Level** control | **Lower detail level** |

- The range is 1 to 15. New views start at **Settings › Visualization › Initial detail level** (5 by default).
- Changing the level only redraws: the zoom root and the selection stay.
- Each view has its own level.
- The flame graphs can only draw as many bands or rings as fit. When that is fewer than you asked for, the level control adds "Levels that fit: *n*". Make the window taller, or zoom in, to see more.

:::tip
On a whole drive a high level makes a busy picture. Keep the level moderate and zoom into the part you care about instead: the level counts from the zoom root, so every zoom shows that many levels again.
:::

## Small items and "+N more"

A block smaller than **Settings › Visualization › Minimum block size** (3 to 20 px, 10 by default) is not drawn on its own. In the treemap "smaller" means area; in the flame graph it means length along the band, and in the polar flame graph the arc length in the middle of the ring.

What is too small is merged:

- **"N small items"**: two or more files of one folder that are each too small. The block has a dotted texture and takes the color of the file class that holds at least half of its bytes (otherwise **Other**).
- **"+N more"**: whatever is still too small at that level (small folders, a single small file, or a "small items" block that is itself too small), merged into one block in a neutral color, placed last.

Working with merged blocks:

- Double-click one, or select it and press <kbd>Enter</kbd>, to lay out its members one by one, largest first. What still doesn't fit becomes a new "+N more". The breadcrumb ends in "*N* small items" or "+*N* more", and <kbd>Backspace</kbd> takes you back.
- Select one and the details panel explains what it is.
- In its context menu, **Open**, **Show in Explorer**, the terminal and **Copy path** are unavailable (a merged block is not one place on disk), and it can't be tagged. **Add to delete basket** adds the whole block with its members. The <kbd>Delete</kbd> key does not add merged blocks; use the menu.

Raise **Minimum block size** for a calmer picture with fewer, larger blocks; lower it to see more individual files.

## Free and unknown space

When the zoom root is a whole volume you scanned (for example `C:\` or `/`), two more blocks appear on the first level and are sorted with the rest:

- **Free space**: the volume's free space as the system reports it. Green stripes with a solid outline.
- **Unknown space**: the used space the system reports minus everything the scan counted. That is usually places the scan could not read and space the system reserves. Gray stripes with a dashed outline. It is not drawn when the difference is zero or less. While the scan runs it is called **Unknown space · not scanned yet** and shrinks as the scan goes on.

| Keys | Toolbar | Command palette |
|---|---|---|
| <kbd>F</kbd> | **Free space** chip | **Show/hide free space** |
| <kbd>U</kbd> | **Unknown space** chip | **Show/hide unknown space** |

- Turning a block off redraws the rest to fill the view, so you see the scanned files at a larger scale.
- New views follow **Show free space in new views** and **Show unknown space in new views** in **Settings › Visualization**.
- They disappear once you zoom into a folder, and are not drawn in comparisons or while a filter hides non-matches. You can't zoom into them or tag them.
- In the flame graphs they are segments of the first band.

A large unknown space is worth a look: scanning with more rights usually makes it smaller. See [Troubleshooting](../troubleshooting/).

## The Recycle Bin

What you deleted into the Recycle Bin (the Trash on macOS) still takes up space until the bin is emptied. SpaceKnife marks it so you don't mistake it for files you use:

- Everything in the bin is shaded and cross-hatched inside a coral dashed outline.
- The bin itself carries a badge: **Recycle Bin · emptying frees 12.3 GB** (**Trash · …** on macOS). It also shows as the root bar or center disc when you zoom into the bin in a flame graph.
- The hover card says **In the Recycle Bin: the space comes back when the bin is emptied**.
- With **Moving** (the default), the dashes march and a band of light sweeps across the bin now and then. **Reduce motion** holds them still.

The bins it knows: `$Recycle.Bin` at the top of each Windows drive; `~/.Trash` and `.Trashes` at the top of a volume on macOS; `~/.local/share/Trash` and `.Trash-<user id>` at the top of a drive on Linux (the freedesktop.org Trash, used by GNOME, KDE and most file managers; `rm` deletes without one). A scan of the whole drive includes the bin; a scan of one folder does only when the bin is inside it.

While SpaceKnife monitors the scan, moving files to the bin moves their blocks into it, and emptying the bin turns that space into free space.

Choose **Moving**, **Still** or **Off** in **Settings › Appearance › Treemap visuals › Recycle Bin effect**.

To leave the bin out of the picture, press <kbd>R</kbd> or click the **Recycle Bin** chip next to **Unknown space** (**Trash** on macOS). The chip is there only when the scan holds a bin, and not in comparisons. With it off, the bin takes no room and the rest grows to fill the view. Its bytes are not counted as unknown space, and a view zoomed into the bin keeps showing it. New views follow **Settings › Visualization › Geometry › Show the Recycle Bin in new views**.

## While a scan runs: the breathing outline

The picture fills in while the scan runs, several times a second. You can select, hover, zoom, go back, change the level and switch the visualization the whole time.

- A folder that has been found but not read yet is a striped block that says **Waiting to scan…**. It opens once its contents are read.
- Folders still being read, up to three levels below the zoom root (and the zoom root itself), **breathe**: an accent-colored outline glows and fades slowly. Inner levels breathe a little later than outer ones, so the glow ripples inward.
- With **Reduce motion** on, the outline stays still.

The breathing outline and the change flashes share one switch: **Settings › Visualization › Flash changed blocks**.

## Change flashes

After the scan, live monitoring keeps the picture up to date (see [Live monitoring](../monitoring/)). When a file is added, deleted or changes size, its block flashes: a yellow outline with a glow, two pulses, then it fades.

- When the changed item is not drawn on its own (too small, merged, or inside a collapsed folder), the block that holds it flashes. In the flame graphs, a change inside the zoom root that falls on no bar or sector flashes the root bar or the center disc.
- With **Reduce motion** on, there is no flashing. The changed block shows a static badge with the change, for example "+212 MB", for about ten seconds, where it fits.
- Turn flashing off with **Settings › Visualization › Flash changed blocks**.

## In a comparison

When you compare a scan with a snapshot (see [Snapshots and comparison](../snapshots-compare/)), all three visualizations show changes instead of sizes:

- Area, length or angle is the amount of change. A folder's size line shows its net change, such as "+14.8 GB" or "−5.7 GB".
- Orange means growth, in four tiers, and blue means decrease, in three. The tiers are 1, 3 and 8 GB, and step down to smaller units when all changes are small; the side panel's legend shows the current bounds.
- Items that are new since the base get a **New** badge when the block is big enough. Deleted items appear as decrease, sized by what they were in the base.
- The **Coloring** group becomes **Changes shown** with **All**, **Growth** and **Decrease**; <kbd>C</kbd> steps through these instead of the colorings.
- There are no free or unknown space blocks, so <kbd>F</kbd> and <kbd>U</kbd> do nothing. Split view is not available.
- <kbd>Esc</kbd> first clears the selection; pressed again with nothing selected, it leaves the comparison.

## Settings that shape the picture

The defaults, the initial detail level and the two "in new views" switches only affect views opened afterwards; everything else applies at once to the open views. See the [Settings reference](../settings/) for the full list.

| Setting | Where | Effect |
|---|---|---|
| Default visualization for new views | Appearance › Visualization | Treemap, flame graph or polar flame graph |
| Default treemap coloring | Appearance › Coloring | Classes by default |
| Default coloring for flame graphs and polar flame graphs | Appearance › Visualization | Branch by default |
| Reduce motion | Appearance › Animation | Instant zoom and switching, badges instead of flashes, still outlines |
| Level contrast, Border contrast | Appearance › Treemap visuals | How much folder levels and block edges stand out |
| Highlight halo levels | Appearance › Treemap visuals | How many ancestors light up on hover or selection (0 to 10) |
| Hover highlight, Selection shadow, Mouse trail | Appearance › Treemap visuals | Outline under the pointer, shadow under the selection, fading trail |
| Font size | Visualization › Geometry | 80 to 140 %; also scales bar and ring sizes |
| Minimum block size | Visualization › Geometry | When small items are merged |
| Block proportions | Visualization › Geometry | Treemap only: taller or wider blocks |
| Initial detail level | Visualization › Geometry | Level of new views |
| Show free space / unknown space in new views | Visualization › Geometry | Start values of <kbd>F</kbd> and <kbd>U</kbd> |
| Show the Recycle Bin in new views | Visualization › Geometry | Start value of <kbd>R</kbd> |
| Sort by size | Visualization › Geometry | Largest first, or by name |
| Flame graph direction | Visualization › Flame graph | Root at bottom or at top |
| Zoom animation duration | Visualization › Animation | 0 to 600 ms, 250 by default; 0 turns it off |
| Flash changed blocks | Visualization › Change highlights | Change flashes and the breathing outline |

## Keys on this page

| Keys | Action |
|---|---|
| <kbd>V</kbd> / <kbd>Shift</kbd>+<kbd>V</kbd> | Next / previous visualization |
| <kbd>C</kbd> | Next coloring (in a comparison: next **Changes shown** option) |
| <kbd>Ctrl</kbd>+<kbd>=</kbd> / <kbd>Ctrl</kbd>+<kbd>-</kbd> | Raise / lower the detail level |
| <kbd>F</kbd> | Show or hide free space |
| <kbd>U</kbd> | Show or hide unknown space |
| <kbd>R</kbd> | Show or hide the Recycle Bin (when the scan holds one) |
| Double-click, <kbd>Enter</kbd> | Zoom in, also into merged blocks |

Single-letter keys don't work while you type in a text field, and a focused drop-down list keeps <kbd>C</kbd> and <kbd>V</kbd> for itself. On macOS you can press <kbd>⌘</kbd> wherever this page says <kbd>Ctrl</kbd>. All keys are listed in [Keyboard shortcuts](../shortcuts/); zooming and the rest of the navigation are in [Navigating the picture](../navigation/).
