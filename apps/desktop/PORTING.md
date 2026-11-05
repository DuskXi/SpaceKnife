# Porting the interface from React to Vue

`apps/electron/src/renderer/src` (React 19 + zustand, Electron) is being ported to `apps/desktop/app`
(Nuxt 4 SPA + Vue 3 + Pinia, Tauri 2). Phase 1 (done) moved every framework-free module, the store,
the host layer and the shell skeleton. This file is the brief for porting the remaining components;
several people can work on it in parallel, one component (or one row group below) each.

The Electron app stays in the repository as the reference until the port is complete. Port behaviour
as it is: same classes (the CSS in `app/styles/*.css` is unchanged and global), same texts (the
Chinese source text is the i18n key), same ids and `data-*` attributes, same keyboard handling.

## Commands

```sh
cd apps/desktop
pnpm install
pnpm dev            # the interface in a browser at http://localhost:3000 (browser shim: no backend)
pnpm tauri dev      # the app window (Rust side in src-tauri); the backend comes with sk-server
pnpm typecheck      # vue-tsc over app/ (nuxt typecheck)
pnpm test           # vitest
pnpm generate       # static output in .output/public (what Tauri bundles)
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
| `shell/TitleBar.tsx` | `shell/TitleBar.vue` (**done**, minus RemoteChip / UpdateBadge) | |
| `ui/Icon.tsx` | `ui/Icon.vue` + `ui/Kbd.vue` (**done**) | `IconName` → `lib/icons.ts` (**done**) |
| `ui/ErrorBoundary.tsx` | `ui/ErrorBoundary.vue` (`onErrorCaptured`) | |
| `ui/charts.tsx` | `ui/charts/*.vue` (one per chart) | `Tone`, `Segment` types → `lib/charts.ts` |
| `shell/Toolbar.tsx` | `shell/Toolbar.vue` (uses `composables/useCollapseLevel`) | |
| `shell/StatusBar.tsx` | `shell/StatusBar.vue` | `monitorText` → `lib/statusBar.ts` |
| `shell/LogPanel.tsx` | `shell/LogPanel.vue` | |
| `shell/Palette.tsx` | `shell/Palette.vue` | |
| `shell/ContextMenu.tsx` | `shell/ContextMenu.vue` | `closeMenu` → `state/menu.ts` |
| `shell/OpToast.tsx` | `shell/OpToast.vue` | |
| `shell/ExportDialog.tsx` | `shell/ExportDialog.vue` | |
| `shell/FullDiskAccessDialog.tsx` | `shell/FullDiskAccessDialog.vue` | |
| `shell/UpdateCard.tsx` | `shell/UpdateCard.vue` (`UpdatePrompt` = `shell/UpdatePrompt.vue`) | `UpdatePlacement` type → `state/updates.ts` |
| `shell/UpdateBadge.tsx` | `shell/UpdateBadge.vue` (then add it to TitleBar.vue) | |
| `shell/BrowserLayer.tsx` | `shell/BrowserLayer.vue` + `shell/RemoteChip.vue` (then add the chip to TitleBar.vue) | `isLoopback` → `lib/host.ts` |
| `shell/FilterBox.tsx` | `shell/FilterBox.vue` | `filterInputId`, `focusFilter` → `lib/filterInput.ts` (**done**); `attrLabel`, `errorText`, `diagnosticText` → `lib/filterText.ts`; `renderColoured` returns React nodes: make it return segments `{ text, cls }[]` and render them in the template |
| `shell/Inspector.tsx` | `shell/Inspector.vue` | `revealLabel`, `notOnDisk` → `lib/reveal.ts` (**done**); `reveal` (async, store only) → `state/reveal.ts` |
| `shell/Results.tsx` | `shell/Results.vue` | |
| `shell/ScanProgress.tsx` | `shell/ScanBand.vue`, `shell/ScanDetails.vue` (one per exported component) | `isRunning`, `fmtRemaining`, `scanPercent`, `fmtRate`, `levels`, `levelName`, `levelText`, `phaseText` → `lib/scanProgress.ts` (**done**); `curveSummary`, `activeTip` → `lib/scanProgress.ts`; hook `useScanIo` → `composables/useScanIo.ts` |
| `shell/HoverCard.tsx` | `shell/HoverCard.vue` | `sameTarget`, `cardAvoid`, `hoverFacts` → `lib/hoverCard.ts` |
| `shell/Compare.tsx` | `shell/Compare*.vue` (one per exported component) | `DIRECTIONS`, `directionLabel`, `fmtSpan` → `lib/compare.ts` |
| `shell/BasketDrawer.tsx` | `shell/BasketDrawer.vue` | |
| `shell/ColumnResizer.tsx` | `shell/ColumnResizer.vue` | |
| `visual/Host.tsx` | `visual/Host.vue` (uses `composables/useMedia`, `visual/scheduler.ts` FrameLoop) | `pointerBox`, `hitOfKey` → `visual/hit.ts` |
| `pages/StartPage.tsx` | `pages/StartPage.vue` | `MEDIA_ZH`, `LOAD_ZH`, `volumeName`, `mftCapable`, `fmtShareOf`, `keysOf` → `lib/volumes.ts`. Dropping folders: see "Drag and drop" |
| `pages/MachineCard.tsx` | `pages/MachineCard.vue` | |
| `pages/MachinePage.tsx` | `pages/MachinePage.vue` | |
| `pages/ScanPage.tsx` | `pages/ScanPage.vue` | |
| `pages/SettingsPage.tsx` | `pages/SettingsPage.vue` | `PAGES` → `lib/settingsPages.ts` |
| `pages/AboutPage.tsx` | `pages/AboutPage.vue` (`window.sk.versions` is now `{ tauri, webview }`) | |
| `pages/AdvancedTools.tsx` | `pages/AdvancedTools.vue` | |
| `pages/ClassesEditor.tsx` | `pages/ClassesEditor.vue` (see "Drag and drop") | |
| `pages/HelpPage.tsx` | `pages/HelpPage.vue` | `openLink`, `helpTitle` → `help/links.ts`; port `help/HelpPage.test.ts` (dropped in phase 1, it mounted the React page) as `pages/HelpPage.test.ts` with @vue/test-utils |

When a component is done, replace its placeholder / TODO(port) comment in `AppShell.vue`,
`TitleBar.vue` or its parent.

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
| `useScanIo(scanId)` (shell/ScanProgress.tsx) | to port: `composables/useScanIo.ts` |

## Host API: `window.sk`

Unchanged contract (`app/shared/api.ts`), now implemented by `app/lib/tauriHost.ts` (Tauri commands
in `src-tauri/src/commands.rs`, events `sk://backend`, `sk://maximized`) or `app/shim.ts` in a
browser. Differences from Electron:

- `versions` is `{ tauri, webview }` (was electron / chrome / node).
- `files.pathOf(file)` returns `""` in the app (a webview's `File` has no path); use
  `files.onDropPaths(cb)`, which gives the absolute paths of what was dropped on the window.
- `update.install` answers "no install step" until the updater is ported (src-tauri TODO(update)).
- `backend()` stays `{ state: "starting" }` until crate sk-server is wired in
  (`src-tauri/src/backend.rs`, TODO(sk-server)).

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
page loses dropped paths. Decide when porting StartPage / ClassesEditor.

## Not ported yet (outside the components)

- The update installer (`apps/electron/src/main/update.ts`, `installUpdate`) — `src-tauri` TODO(update).
- Headless / `--listen` mode (`main/headless*.ts`): will be served by the Rust server; `src-tauri/src/files.rs`
  keeps the static-file resolution it needs.
- The third-party licence file of the bundle (`rollup-plugin-license` in electron.vite.config.ts).
- macOS bundle icon (`icon.icns`) in `src-tauri/icons`.
