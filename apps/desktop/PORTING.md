# Porting the interface from React to Vue

For 0.3.5 the interface of 0.3.x, `apps/electron/src/renderer/src` (React 19 + zustand, Electron),
was ported to `apps/desktop/app` (Nuxt 4 SPA + Vue 3 + Pinia, Tauri 2). The port is complete and the
React app is gone from the tree (it is in the history, tag `v0.3.4`); this file keeps the brief the
port was done by, since its commands, layout, conventions, host API and bundle notes still describe
the app.

Behaviour was ported as it was: same classes (the CSS in `app/styles/*.css` is unchanged and global),
same texts (the Chinese source text is the i18n key), same ids and `data-*` attributes, same keyboard
handling.

## Commands

```sh
cd apps/desktop
pnpm install
pnpm dev            # the interface in a browser at http://localhost:3000 (browser shim: no backend)
pnpm tauri dev      # the app window (Rust side in src-tauri); the backend comes with sk-server
pnpm typecheck      # vue-tsc over app/ (nuxt typecheck)
pnpm test           # vitest
pnpm generate       # static output in .output/public (what Tauri bundles)
pnpm bundle         # release bundles (scripts/bundle.mjs: tauri build + tauri.bundle.json, build paths remapped; spaceknife-cli, licences)
pnpm bundle:portable   # after bundle: target/portable/SpaceKnife-<v>-<os>-<arch>-portable.zip / .tar.gz
cargo test -p spaceknife-desktop   # from the repository root
```

## Layout

| Path | What |
| --- | --- |
| `app/app.vue` | Nuxt's root: loads `components/AppShell.vue` asynchronously, after `window.sk` exists |
| `app/plugins/01.host.client.ts` | puts `window.sk` in place before anything else (`lib/installHost.ts`) |
| `app/plugins/02.pinia.ts` | installs the one Pinia (`state/pinia.ts`) |
| `app/components/AppShell.vue` | port of `App.tsx` (layout, boot, global keys); placeholders mark what is missing |
| `app/components/{shell,pages,ui,visual}/*.vue` | the components (table below) |
| `app/composables/*.ts` | Vue versions of the React hooks |
| `app/lib`, `app/state`, `app/protocol`, `app/treemap`, `app/visual`, `app/help`, `app/locales` | framework-free modules, as they were (with their tests) |
| `app/shared/*.ts` | was `apps/electron/src/shared` (host API contract, theme, install, links) |
| `app/shim.ts` | `window.sk` in a plain browser (headless mode, demo, `pnpm dev`) |
| `src-tauri/` | the Tauri app (crate `spaceknife-desktop`, binary `spaceknife`) |

Aliases: `@/…` and `~/…` are `app/`, `@design/…` is `design/build`, `@site/…` is `site/src` (same as
before). `pages: false`: there is no router and no `app/pages/` directory; the page components live in
`app/components/pages/`. Nuxt's auto-imports and component auto-registration are **off**: import
`ref`, `computed`… from `vue` and components by path, as the framework-free modules do.

## Components to port

Target paths are under `app/components/`. "Pure exports" are non-component exports of the .tsx file
that other code imports: move them to a `.ts` module (suggested path given) so state code and tests
need no component; the four already moved are marked done.

| Electron (`src/renderer/src/…`) | Target `.vue` | Pure exports to move out first |
| --- | --- | --- |
| `App.tsx` | `AppShell.vue` (**done**, skeleton; fill the placeholders) | `onKey`, `keyboardMenu` → `state/shortcuts.ts` (**done**) |
| `main.tsx` | `app.vue` + `plugins/*` (**done**) | |
| `shell/TitleBar.tsx` | `shell/TitleBar.vue` (**done**, with RemoteChip / UpdateBadge) | |
| `ui/Icon.tsx` | `ui/Icon.vue` + `ui/Kbd.vue` (**done**) | `IconName` → `lib/icons.ts` (**done**) |
| `ui/ErrorBoundary.tsx` | `ui/ErrorBoundary.vue` (**done**, `onErrorCaptured`; wraps `AppShell` in `app.vue`) | |
| `ui/charts.tsx` | `ui/charts/{Ring,Meter,StackBar,Sparkline}.vue` (**done**) | `Tone`, `Segment` types, ring / sparkline geometry → `lib/charts.ts` (**done**) |
| `shell/Toolbar.tsx` | `shell/Toolbar.vue` (**done**; parts `Toolbar{Button,Crumbs,Detail,More,Visuals}.vue`) | `SPACE_SWITCHES`, `colorLabel`, `binName`, breadcrumb label / full path → `lib/toolbar.ts` (**done**) |
| `shell/StatusBar.tsx` | `shell/StatusBar.vue` (**done**) | `monitorText`, `LEVEL_CAP` → `lib/statusBar.ts` (**done**) |
| `shell/LogPanel.tsx` | `shell/LogPanel.vue` (**done**; `LogList.vue`, `LogStats.vue`) | source / level texts, message parts → `lib/logPanel.ts` (**done**) |
| `shell/Palette.tsx` | `shell/Palette.vue` (**done**; `PaletteDialog.vue`, `PaletteRow.vue`) | rows, first runnable row, hit parts → `lib/palette.ts` (**done**) |
| `shell/ContextMenu.tsx` | `shell/ContextMenu.vue` (**done**) | `closeMenu` → `state/menu.ts` (**done**) |
| `shell/OpToast.tsx` | `shell/OpToast.vue` (**done**) | |
| `shell/ExportDialog.tsx` | `shell/ExportDialog.vue` (**done**) | |
| `shell/FullDiskAccessDialog.tsx` | `shell/FullDiskAccessDialog.vue` (**done**) | |
| `shell/UpdateCard.tsx` | `shell/UpdateCard.vue` (**done**; `UpdatePrompt` = `shell/UpdatePrompt.vue`) | `UpdatePlacement` type → `state/updates.ts` (**done**); `choiceTitle`, `choiceHint` → `lib/updateCard.ts` |
| `shell/UpdateBadge.tsx` | `shell/UpdateBadge.vue` (**done**, in TitleBar.vue) | |
| `shell/BrowserLayer.tsx` | `shell/BrowserLayer.vue` + `shell/RemoteChip.vue` (**done**, chip in TitleBar.vue) | `isLoopback` → `lib/host.ts` (**done**) |
| `shell/FilterBox.tsx` | `shell/FilterBox.vue` (**done**) | `filterInputId`, `focusFilter` → `lib/filterInput.ts` (**done**); `attrLabel`, `errorText`, `diagnosticText` → `lib/filterText.ts` (**done**); `renderColoured` → `colourSegments` (`{ start, text, cls }[]`, rendered in the template) |
| `shell/Inspector.tsx` | `shell/Inspector.vue` (**done**; + `InspectorClassStats.vue`, `InspectorTopRow.vue`) | `revealLabel`, `notOnDisk` → `lib/reveal.ts` (**done**); `reveal` (async, store only) → `state/reveal.ts` (**done**) |
| `shell/Results.tsx` | `shell/Results.vue` (**done**, `Results.test.ts`) | paging, row text, condition capsules → `lib/results.ts` (**done**) |
| `shell/ScanProgress.tsx` | `shell/ScanBand.vue`, `shell/ScanDetails.vue` (one per exported component) (**done**) | `isRunning`, `fmtRemaining`, `scanPercent`, `fmtRate`, `levels`, `levelName`, `levelText`, `phaseText` → `lib/scanProgress.ts` (**done**); `curveSummary`, `activeTip` → `lib/scanProgress.ts` (**done**); hook `useScanIo` → `composables/useScanIo.ts` (**done**) |
| `shell/HoverCard.tsx` | `shell/HoverCard.vue` (**done**; delay logic in `composables/useHoverCard.ts`, shared with `CompareHoverCard.vue`) | `sameTarget`, `cardAvoid`, `hoverFacts`, card placement, `splitPlaceholders` → `lib/hoverCard.ts` (**done**) |
| `shell/Compare.tsx` | `shell/Compare*.vue` (one per exported component): `ComparePicker.vue` (`tab`, `compact?`), `CompareDirection.vue` (was `DirectionSegment`; `viewId`), `CompareExit.vue` (was `ExitCompare`; `tab`, `compact?`), `CompareSidebar.vue`, `CompareHoverCard.vue` (**done**; `CompareEndMenu.vue` is the picker's end) | `DIRECTIONS`, `directionLabel`, `fmtSpan` → `lib/compare.ts` (**done**) |
| `shell/BasketDrawer.tsx` | `shell/BasketDrawer.vue` (**done**) | sort, main volume, status text → `lib/basketDrawer.ts` (**done**) |
| `shell/ColumnResizer.tsx` | `shell/ColumnResizer.vue` (**done**) | |
| `visual/Host.tsx` | `visual/Host.vue` (**done**, `Host.test.ts`; `onHover` / `onContextMenu` are the emits `hover` / `menu`) (uses `composables/useMedia`, `visual/scheduler.ts` FrameLoop) | `pointerBox` → `visual/hit.ts` (**done**); `hitOfKey` and the hovered row (`hoverTarget`, `HoverLight`, `sameTarget`) → `visual/hover.ts` (**done**) |
| `pages/StartPage.tsx` | `pages/StartPage.vue` + `pages/StartRecents.vue` (**done**) | `MEDIA_ZH`, `LOAD_ZH`, `volumeName`, `mftCapable`, `fmtShareOf`, `keysOf` (+ `methodText`, `methodBadge`, `sameRoot`, `fmtWhen`, `recentRecords`) → `lib/volumes.ts` (**done**). Dropped folders / `.sks` come through `files.onDropPaths` (see "Drag and drop") |
| `pages/MachineCard.tsx` | `pages/MachineCard.vue` (**done**) | |
| `pages/MachinePage.tsx` | `pages/MachinePage.vue` + `pages/MachineDisk.vue`, `pages/MachineVolume.vue` (**done**) | `seconds`, `FULL`, `className`, `scanSummary` → `lib/machinePage.ts` (**done**) |
| `pages/ScanPage.tsx` | `pages/ScanPage.vue` (**done**; + `ScanPane.vue`, `ScanPaneHead.vue`, `ScanFilterFloater.vue`) | `openMenu`, `jumpUp`, crumb labels, `columnWidth` → `lib/scanPage.ts` (**done**) |
| `pages/SettingsPage.tsx` | `pages/SettingsPage.vue` + `SettingsGroup`, `SettingRow`, `SettingControl`, `SettingsKeys`, `SettingsFiles`, `SettingsFooter`, `SetRow`; toolbar box `SettingsSearch` = `pages/SettingsSearch.vue` (**done**) | `PAGES` (+ `GROUPS`, `pageName`, `groupBy`, `dependsOk`, `searchSettings`) → `lib/settingsPages.ts` (**done**) |
| `pages/AboutPage.tsx` | one per exported component: `pages/AboutHeader.vue`, `pages/AboutComponents.vue` (Tauri / WebView rows), `pages/AboutTimeline.vue`, `pages/AboutEntry.vue` (**done**) | |
| `pages/AdvancedTools.tsx` | `pages/AdvancedTools.vue`, `pages/ClearTags.vue`, `pages/ImportPreview.vue`, `pages/ConfirmButton.vue` (**done**) | hook `useMaintenance` → `composables/useMaintenance.ts` (**done**) |
| `pages/ClassesEditor.tsx` | `pages/ClassesEditor.vue` (**done**, reordering with pointer events) | row logic → `lib/classesEditor.ts` (**done**) |
| `pages/HelpPage.tsx` | `pages/HelpPage.vue` + `pages/HelpContents.vue`; toolbar box `HelpSearch` = `pages/HelpSearch.vue` (**done**) | `openLink`, `helpTitle` (+ `markWords`) → `help/links.ts` (**done**); `pages/HelpPage.test.ts` ported (**done**) |

Every component above is ported; no `TODO(port)` placeholder is left.

## State: `@/state/store`

The zustand store is now Pinia, with the same functions, so `state/actions.ts` and friends are
unchanged:

- `get(): AppState` — the current state, a plain object (zustand's `getState`). Replaces
  `useApp.getState()` in components.
- `set(patch | (s) => patch)` — shallow merge, as zustand's `setState`. **Always update immutably**
  (new arrays / objects) and **never assign to the Pinia store directly**: each top-level field is a
  `shallowRef`, so only replacing a field is seen, and `set()` keeps `get()` and the store in step.
- `useApp(pick): ComputedRef<T>` — the component read (zustand's `useApp(selector)`):
  `const tabs = useApp((s) => s.tabs)`, `tabs.value` in script, `tabs` in templates. Only the fields
  the selector reads are tracked. A selector may compute (`useApp((s) => activeTab(s))`).
- `subscribe((state, prev) => …)`, `appStore()` / `useAppStore` (the Pinia store, for devtools),
  `resetState()` (tests).
- All other exports (`activeTab`, `focusedViewId`, `patchView`, `toast`, `setting`, `startFormOf`,
  `readFlag`/`writeFlag`, …) are as before.

Nothing in the state is proxied deeply: frames (typed arrays, class instances) are stored raw.

## Composables (`app/composables/`)

| React | Vue |
| --- | --- |
| `useThrottled(value, ms)` (lib/throttle.ts) | `useThrottled(valueRefOrGetter, msRefOrGetter): Ref<T>` |
| `useCollapseLevel(ref, content)` (shell/toolbarFit.ts) | `useCollapseLevel(elRef, contentRefOrGetter): Ref<number>` (pure parts in `lib/toolbarFit.ts`) |
| `useReducedMotion(setting)` (visual/scheduler.ts) | `useReducedMotion(settingRefOrGetter): ComputedRef<boolean>` (`composables/useMedia.ts`) |
| `useDevicePixelRatio()` (visual/scheduler.ts) | `useDevicePixelRatio(): Ref<number>` (`composables/useMedia.ts`) |
| `useScanIo(scanId)` (shell/ScanProgress.tsx) | `useScanIo(scanIdRefOrGetter): ComputedRef<IoStatus \| null>` (`composables/useScanIo.ts`) |

## Host API: `window.sk`

Unchanged contract (`app/shared/api.ts`), now implemented by `app/lib/tauriHost.ts` (Tauri commands
in `src-tauri/src/commands.rs`, events `sk://backend`, `sk://maximized`) or `app/shim.ts` in a
browser. Differences from Electron:

- `versions` is `{ tauri, webview }` (was electron / chrome / node).
- `files.pathOf(file)` returns `""` in the app (a webview's `File` has no path); use
  `files.onDropPaths(cb)`, which gives the absolute paths of what was dropped on the window.
- `files.onDragState(cb)`: true while files are dragged over the window, false when they leave or
  are dropped (Tauri's drag enter / over / leave / drop); the start page's drop overlay follows it.
- `update.install` runs `src-tauri/src/install.rs` (Electron's installUpdate: NSIS silent / wizard,
  the portable swap through `--apply-update`, AppImage replace, dmg / deb / rpm opened).
- `backend()`: the server runs in this process (`src-tauri/src/backend.rs`, `sk_server::start` on a
  random loopback port with a fresh token); "failed" carries the reason, "exited" when it stops on
  its own. `SPACEKNIFE_READY_FILE=<path>` writes `{url, token, pid}` there once (tests, demos, the
  portable swap watching a new version); `main.rs` takes the variable out of the environment first,
  so nothing the app starts inherits it.

`lib/host.ts` (`inBrowser`, `serverName`) is as before.

## Conventions

- `<script setup lang="ts">`, typed `defineProps<…>()` / `defineEmits<…>()`. Props replace React props
  one to one; callbacks props (`onClose`) become emits (`close`) unless the parent passes a function
  that returns a value.
- Imports explicit (`import { ref } from "vue"`, `import Icon from "@/components/ui/Icon.vue"`).
- `className` → `class` (array / object syntax for conditionals), `htmlFor` → `for`,
  `onPointerDown` → `@pointerdown`, `e.stopPropagation()` → `.stop`, `dangerouslySetInnerHTML` →
  `v-html` (only for markup we produce: icons, rendered help Markdown).
- React state → `ref` / `shallowRef` (use `shallowRef` for large objects and frames); `useMemo` →
  `computed`; `useEffect` with deps → `watch` (`{ immediate: true }` when it also ran on mount) or
  `onMounted` + `onScopeDispose`; `useLayoutEffect` → `watch(…, { flush: "post" })` / `nextTick`;
  `useRef` for elements → template `ref`; `useRef` for mutable values → a plain `let`.
- `flushSync` has no counterpart: change state, then `await nextTick()` before measuring.
- Translations: `t()` and `byLang()` from `@/lib/i18n` are reactive (the language is a Vue ref): call
  them in templates or computeds and the text follows a language change; there is no need to re-key
  the tree. Same for `fmtSize` & co. (the unit base is a ref) and `palette()` / `themeName()` (the
  theme is a ref). The accent (`applyAccent`) is not: canvases repaint on `s.theme` / setting changes
  as before.
- Headless UI: Reka UI (`reka-ui`) is installed for dialogs, menus, popovers, tooltips, sliders, select
  where a component needs one (the React app used hand-written elements, not react-aria). Keep the
  existing markup and classes where the hand-written version works; reach for Reka only when it
  replaces focus trapping / positioning code.
- Teleport overlays (palette, dialogs, menus, toasts) to `body` only if the React version used a
  portal; otherwise keep them where `AppShell.vue` places them (after `.app`, which gets `inert`).
- Window dragging: elements that drag the window carry `data-tauri-drag-region` (the CSS
  `-webkit-app-region` has no effect in Tauri). A double click on them maximizes (Tauri's own: no
  dblclick handler).
- Tests: `*.test.ts` next to the file. Component tests start with
  `// @vitest-environment happy-dom` and mount with `@vue/test-utils`, setting `window.sk` to a fake
  (see `components/shell/TitleBar.test.ts`). `lib/theme.test.ts` also scans `.vue` files for base
  token colours read outside `lib/theme.ts`.

## Drag and drop

Tauri's drag-drop handler is on (window default), so drops of files from the system come through
`window.sk.files.onDropPaths`. On Windows that handler also swallows the page's own HTML5 drag and
drop: `ClassesEditor` reordering (dataTransfer) must use pointer events instead (as the title bar's
tab reordering does), or the window must be built with `disable_drag_drop_handler()` and the start
page loses dropped paths. Decided: the handler stays on. StartPage takes dropped paths from `onDropPaths` (a `.sks` file
opens as a snapshot, anything else goes in the path box, where the path check flags what isn't a
folder); ClassesEditor reorders with pointer events. The overlay 「松开以添加文件夹…」 follows
`files.onDragState` in the app (Tauri's enter / over / leave) and the page's own drag events in a
browser tab.

## Bundles

- `src-tauri/tauri.conf.json`: the app (productName SpaceKnife, identifier io.github.duskxi.spaceknife,
  frontendDist `../.output/public`), icons (made with `pnpm tauri icon ../../design/app-icon/spaceknife.svg`, the
  1024 px master); `tauri.{windows,linux,macos}.conf.json` the targets: NSIS
  (per-user), AppImage / deb / rpm, app / dmg.
- `src-tauri/tauri.bundle.json` (`pnpm bundle`): `scripts/prepare-bundle.mjs` builds `spaceknife-cli`
  (release) as `src-tauri/target-bundle/spaceknife-cli-<triple>` for `externalBin` (installed next to the app as
  `spaceknife-cli`) and writes `src-tauri/target-bundle/licenses` (LICENSE.txt, THIRD-PARTY-rust.txt from
  `cargo metadata`, THIRD-PARTY-ui.txt from `pnpm licenses list --prod`) as the `licenses` resource;
  the generated interface (`.output/public`) goes along as the `ui` resource, for
  `spaceknife-cli headless`.
  Kept out of the base config because Tauri's build script requires externalBin / resources to exist
  (plain `cargo build` and `pnpm tauri dev` would fail without them).
- Release file names: 0.3.4's, kept by every release (`SpaceKnife-<v>-Setup-x64.exe`,
  `spaceknife_<v>_amd64.deb`, `SpaceKnife-<v>-mac-<arch>.dmg`, the portable archives of
  `scripts/portable.mjs`; the rpm and the AppImage come from `apps/electron-shell`); the workflows
  rename the Tauri bundler's own names (`SpaceKnife_<v>_x64-setup.exe`, …) to them, and
  `crates/sk-core/src/update` picks by them (docs/packaging.md §9).
- Install kind (install.rs): dev (not bundled), portable (`data` next to the program), nsis
  (`uninstall.exe` next to it), dmg (macOS), deb / rpm (under /usr; deb when dpkg lists it),
  appimage (`APPIMAGE`); `SPACEKNIFE_INSTALL_KIND` overrides. No MSI bundle (the protocol has no kind
  for it).

## Headless mode and releases

- Headless / `--listen` mode is `spaceknife-cli headless` (crates/sk-server/src/cli/headless.rs): it
  serves the generated interface from `ui` next to the program (the portable archives carry it;
  `--ui <folder>` names another).
- The release workflows (`.github/workflows/portable.yml`, `macos.yml`, `release.yml`) build with
  `pnpm bundle --no-bundle` / `pnpm bundle` and `pnpm bundle:portable`; `tools/release.mjs`
  assembles the release by those file names. The packaged app is tested with
  `packaging/smoke-app.mjs`, `smoke-headless.mjs`, `update-e2e.mjs` and `nsis-e2e.mjs`, and every
  package is checked by `tools/package_checks.mjs` (no build-machine home folder in any file; glibc
  ≤ 2.34 in every ELF file of the Linux packages).
