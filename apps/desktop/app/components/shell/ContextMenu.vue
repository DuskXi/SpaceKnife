<!-- The unified context menu (UI-W5-01…24): an object header, open / show / terminal, copy path ▸,
     tags ▸, zoom, rescan, the delete basket and the system menu, the same in every front end. It
     lives in the store as `menu` (state/menu.ts closes it). -->
<script setup lang="ts">
import { computed, ref, shallowRef, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import Kbd from "@/components/ui/Kbd.vue";
import { classHexNow } from "@/lib/classes";
import { fmtInt, fmtSize } from "@/lib/format";
import { inBrowser } from "@/lib/host";
import { t } from "@/lib/i18n";
import type { IconName } from "@/lib/icons";
import { revealLabel } from "@/lib/reveal";
import { tagLabel } from "@/lib/tags";
import type { PathForms } from "@/protocol/generated";
import { basketAdd, basketAddBlock, call, rescanFolder, viewCall } from "@/state/actions";
import { closeMenu } from "@/state/menu";
import { toast, useApp } from "@/state/store";
import { Kind } from "@/visual/frame";
import { hitOfKey } from "@/visual/hover";
import { rowTitle } from "@/visual/rowTitle";
import { HIT_NONE, HIT_ROOT } from "@/visual/types";

const TAGS = [
  { id: "red", keys: "Ctrl+1" },
  { id: "yellow", keys: "Ctrl+2" },
  { id: "green", keys: "Ctrl+3" },
  { id: "blue", keys: "Ctrl+4" },
] as const;
const TAG_ROWS = [...TAGS, { id: null, keys: "Ctrl+0" } as const];
const TAG_INDEX = [null, "red", "yellow", "green", "blue"] as const;
type CopyForm = keyof Omit<PathForms, "on_disk">;
const COPY_FORMS: { form: CopyForm; keys?: string }[] = [{ form: "raw", keys: "Ctrl+C" }, { form: "quoted" }, { form: "posix" }, { form: "uri" }];
/** The label of a copy form, in the current language. */
const copyLabel = (form: CopyForm): string => ({ raw: t("原样"), quoted: t("带引号"), posix: t("POSIX 路径"), uri: "file:// URI" })[form];

/** What needs the server's own desktop (its file manager, a terminal there): not in a browser tab. */
const SERVER_DESKTOP = new Set(["open", "reveal", "terminal", "system"]);

interface Item {
  id: string;
  icon: IconName;
  label: string;
  keys?: string;
  sub?: Sub;
  danger?: boolean;
  disabled?: string; // why it is unavailable (tooltip)
  run?: () => void;
}

type Sub = "copy" | "tag" | "classify";

/** A path as the folder choices compare it (UI-W7-38): separators and case do not matter. */
const norm = (p: string) => p.replace(/[\\/]+$/, "").replace(/\\/g, "/").toLowerCase();
const samePath = (a: string, b: string) => norm(a) === norm(b);

const menu = useApp((s) => s.menu);
const view = useApp((s) => (s.menu ? s.views[s.menu.viewId] : undefined));
const vocab = useApp((s) => s.vocab);
const settings = useApp((s) => s.settings); // tag display names (W7 标签)
const scanning = useApp((s) => {
  const st = s.menu ? s.scans[s.menu.scanId] : undefined;
  return st?.state === "scanning" || st?.state === "queued";
});
const box = ref<HTMLDivElement | null>(null);
const pos = ref<{ left: number; top: number } | null>(null);
const forms = shallowRef<PathForms | null>(null);
const active = ref(0);
const sub = ref<Sub | null>(null);
const choice = ref<string | null | undefined>(undefined); // the folder's class choice, once read
const subActive = ref(0);

const frame = computed(() => view.value?.frame ?? null);
// a row, or the zoom root (the flame graphs' root bar and centre disc)
const row = computed(() => (menu.value && frame.value ? hitOfKey(frame.value, menu.value.key) : HIT_NONE));
const isRoot = computed(() => row.value === HIT_ROOT);
const kind = computed(() => {
  const f = frame.value;
  return !f ? -1 : isRoot.value ? f.info.root!.kind : row.value >= 0 ? f.kind[row.value]! : -1;
});
const group = computed(() => kind.value === Kind.SMALL || kind.value === Kind.MORE);
const folder = computed(() => kind.value === Kind.FOLDER || kind.value === Kind.COLLAPSED || kind.value === Kind.PENDING);
const node = computed(() => menu.value?.node ?? null);

// the path four ways (copy submenu) and whether it exists on disk at all (MFT pseudo folders)
watch(
  [menu, node, group],
  (_, __, onCleanup) => {
    forms.value = null;
    active.value = 0;
    sub.value = null;
    const m = menu.value;
    const n = node.value;
    if (!m || n == null || n <= 0 || group.value) return;
    let live = true;
    void call("path.forms", { scan_id: m.scanId, node: n })
      .then((f) => live && (forms.value = f))
      .catch(() => undefined);
    onCleanup(() => (live = false));
  },
  { immediate: true },
);

// place it: right of the block (8 px), downwards; flip when it does not fit (UI-W5-03)
watch(
  [menu, forms, row, box],
  () => {
    const m = menu.value;
    const el = box.value;
    if (!m || !el) {
      pos.value = null;
      return;
    }
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    const a = m.avoid;
    let left = a ? a.x + a.w + 8 : m.x;
    let top = m.y;
    if (left + w > window.innerWidth - 4) left = a ? Math.max(4, a.x - w - 8) : Math.max(4, m.x - w);
    if (top + h > window.innerHeight - 4) top = Math.max(4, window.innerHeight - h - 4);
    pos.value = { left, top };
    el.focus({ preventScroll: true }); // arrows and Enter work at once (UI-W5-02/22)
  },
  { flush: "post", immediate: true },
);

// any click elsewhere, a resize or the window losing focus closes it (UI-W5-23)
watch(
  menu,
  (m, _, onCleanup) => {
    if (!m) return;
    const down = (e: MouseEvent) => {
      if (box.value && !box.value.contains(e.target as Node)) closeMenu();
    };
    const close = () => closeMenu();
    window.addEventListener("mousedown", down, true);
    window.addEventListener("resize", close);
    window.addEventListener("blur", close);
    onCleanup(() => {
      window.removeEventListener("mousedown", down, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("blur", close);
    });
  },
  { immediate: true },
);

const items = computed<Item[]>(() => {
  const m = menu.value;
  const f = frame.value;
  const r = row.value;
  if (!m || r === HIT_NONE || !f) return [];
  const fm = forms.value;
  const gone = fm && !fm.on_disk ? t("NTFS 元数据与孤立文件只在 MFT 快速扫描里可见，没有对应的位置") : undefined;
  const noPath = group.value ? t("小项聚合块不是一个具体位置") : gone;
  const path = fm?.raw ?? "";
  const parentRow = r >= 0 ? f.parent[r]! : -1;
  const n = node.value;
  const folderNode = folder.value ? n : parentRow >= 0 ? f.node[parentRow]! : null;
  const all: Item[] = [
    { id: "open", icon: folder.value ? "folder" : "file", label: t("打开"), disabled: noPath, run: () => void window.sk.shell.openPath(path) },
    { id: "reveal", icon: "reveal", label: revealLabel(), keys: "Ctrl+E", disabled: noPath, run: () => window.sk.shell.showItemInFolder(path) },
    {
      id: "terminal",
      icon: "terminal",
      label: t("在此打开终端"),
      keys: "Ctrl+`",
      disabled: noPath,
      run: () => window.sk.shell.openTerminal(folder.value ? path : path.replace(/[\\/][^\\/]*$/, "")),
    },
    { id: "copy", icon: "copy", label: t("复制路径"), sub: "copy", disabled: noPath },
    { id: "tag", icon: "tag", label: t("加标签"), sub: "tag", disabled: group.value ? t("小项聚合块不能加标签") : undefined },
    { id: "zoom", icon: "layers", label: t("只看这里（放大）"), keys: "Enter", run: () => void viewCall(m.viewId, "view.zoom", { key: m.key }) },
    ...(folder.value ? [{ id: "classify", icon: "palette" as IconName, label: t("归类"), sub: "classify" as const, disabled: gone }] : []),
    {
      id: "rescan",
      icon: "refresh",
      label: t("重新扫描此文件夹"),
      disabled: folderNode == null || folderNode <= 0 ? t("没有可重新扫描的文件夹") : gone,
      run: () => folderNode != null && void rescanFolder(m.scanId, folderNode),
    },
    {
      id: "basket",
      icon: "basket",
      label: t("加入删除篮"),
      keys: "Delete",
      danger: true,
      disabled: gone,
      run: () => void (group.value ? basketAddBlock(m.scanId, m.viewId, m.key) : n != null && basketAdd(m.scanId, n)),
    },
    { id: "system", icon: "more", label: t("系统菜单…"), disabled: t("系统原生右键菜单还没有接入（见 TODO）") },
  ];
  // in a browser tab the files are on the server: nothing to open, show or run on this computer
  const here = inBrowser() ? all.filter((it) => !SERVER_DESKTOP.has(it.id)) : all;
  // the zoom root is what is zoomed into already
  return isRoot.value ? here.filter((it) => it.id !== "zoom") : here;
});

/** The header: the object's name, size, class, where it is and its tag. */
const head = computed(() => {
  const f = frame.value;
  const r = row.value;
  if (!f || r === HIT_NONE) return null;
  const root = isRoot.value ? f.info.root! : null;
  const size = root ? root.size : Number(f.size[r]!);
  const crumbs = f.info.breadcrumb;
  // a row too small for a label carries no name in the frame: its path's last part, once known
  const title = rowTitle(f, r, { scanning: scanning.value, formatInt: (n) => fmtInt(n) }).line1 || (forms.value?.raw.split(/[\\/]/).filter(Boolean).pop() ?? "");
  const cls = (root ? undefined : vocab.value?.classes[f.cls[r]!]?.name) ?? (folder.value ? t("文件夹") : t("文件"));
  const parentRow = root ? -1 : f.parent[r]!;
  const where = root ? (crumbs[crumbs.length - 2]?.label.split(/[\\/]/).filter(Boolean).pop() ?? "") : parentRow >= 0 ? (f.names[parentRow] ?? "") : "";
  const tag = TAG_INDEX[root ? root.tag : f.tag[r]!] ?? null;
  return { size, title, cls, where, tag };
});

// 「归类」 (UI-W7-38): automatic, by extension only, or one class for everything in the folder
const choices = computed<{ id: string | null; label: string }[]>(() => [
  { id: null, label: t("自动识别") },
  { id: "", label: t("只按扩展名") },
  ...(vocab.value?.classes ?? []).filter((c) => c.id !== "other").map((c) => ({ id: c.id, label: t("算作「{name}」", { name: c.name }) })),
]);

function activate(it: Item): void {
  if (it.disabled) return toast(it.disabled);
  if (it.sub) return openSub(it.sub);
  closeMenu();
  it.run?.();
}

function classify(id: string | null): void {
  const path = forms.value?.raw;
  closeMenu();
  if (!path) return;
  void call("classes.folder", { path, class_id: id })
    .then(() =>
      toast(
        id === null
          ? t("已改回自动识别")
          : id === ""
            ? t("这个文件夹只按扩展名归类")
            : t("这个文件夹的文件都算作「{name}」", { name: vocab.value?.classes.find((c) => c.id === id)?.name ?? id }),
      ),
    )
    .catch((e: unknown) => toast(e instanceof Error ? e.message : String(e)));
}

function openSub(s: Sub): void {
  sub.value = s;
  subActive.value = 0;
  const f = forms.value;
  if (s === "classify" && f) {
    choice.value = undefined;
    void call("classes.get", {})
      .then((st) => (choice.value = st.folders.find((x) => samePath(x.path, f.raw))?.class_id ?? null))
      .catch(() => (choice.value = null));
  }
}

function copy(form: CopyForm): void {
  const f = forms.value;
  if (!f) return;
  window.sk.shell.copyText(f[form]);
  toast(t("已复制"));
  closeMenu();
}

function setTag(color: string | null): void {
  const m = menu.value;
  closeMenu();
  if (!m) return;
  void viewCall(m.viewId, "view.select", { key: m.key }).then(() => viewCall(m.viewId, "view.tag", { color: color as "red" | null }));
}

function onHover(it: Item, i: number): void {
  active.value = i;
  if (it.sub && !it.disabled) openSub(it.sub);
  else sub.value = null;
}

function onKey(e: KeyboardEvent): void {
  const s = sub.value;
  const list = items.value;
  const n = s === "copy" ? COPY_FORMS.length : s === "tag" ? TAGS.length + 1 : s === "classify" ? choices.value.length : list.length;
  if (e.key === "Escape") return s ? void (sub.value = null) : closeMenu();
  if (e.key === "ArrowDown") return void (s ? (subActive.value = (subActive.value + 1) % n) : (active.value = (active.value + 1) % n));
  if (e.key === "ArrowUp") return void (s ? (subActive.value = (subActive.value + n - 1) % n) : (active.value = (active.value + n - 1) % n));
  if (e.key === "ArrowLeft" && s) return void (sub.value = null);
  if ((e.key === "ArrowRight" && !s && list[active.value]?.sub) || (e.key === "Enter" && !s)) return activate(list[active.value]!);
  if (e.key === "Enter" && s === "copy") return copy(COPY_FORMS[subActive.value]!.form);
  if (e.key === "Enter" && s === "tag") return setTag(subActive.value < TAGS.length ? TAGS[subActive.value]!.id : null);
  if (e.key === "Enter" && s === "classify") return classify(choices.value[subActive.value]!.id);
}
</script>

<template>
  <div
    v-if="menu && head"
    ref="box"
    class="ctxmenu"
    role="menu"
    :aria-label="t('{name} 的操作', { name: head.title })"
    tabindex="-1"
    :style="pos ? { left: `${pos.left}px`, top: `${pos.top}px` } : { left: '-9999px', top: '-9999px' }"
    @keydown="onKey"
    @contextmenu.prevent
  >
    <div class="cm-head" :title="head.title">
      <div class="cm-head-text">
        <div class="cm-title">{{ head.title }}</div>
        <div class="cm-sub">
          <span class="mono">{{ fmtSize(head.size) }}</span> · {{ head.cls }}{{ head.where ? ` · ${head.where}` : "" }}
        </div>
      </div>
      <span v-if="head.tag" :class="['tag-shape', head.tag]" :aria-label="t('标签：{tag}', { tag: head.tag })" />
    </div>
    <div v-for="(it, i) in items" :key="it.id">
      <div v-if="it.id === 'basket' || it.id === 'system'" class="cm-sep" role="separator" />
      <button
        role="menuitem"
        :class="['cm-item', { active: i === active && !sub, danger: it.danger }]"
        :aria-disabled="!!it.disabled"
        :aria-haspopup="it.sub ? 'menu' : undefined"
        :title="it.disabled"
        @mouseenter="onHover(it, i)"
        @click="activate(it)"
      >
        <span class="cm-icon">
          <Icon :name="it.icon" :size="16" />
        </span>
        <span class="flex">{{ it.label }}</span>
        <Icon v-if="it.sub" name="chevronRight" :size="14" />
        <Kbd v-else-if="it.keys">{{ it.keys }}</Kbd>
      </button>
      <div v-if="sub && it.sub === sub" :class="['cm-submenu', { wide: sub === 'copy' }]" role="menu" :aria-label="it.label">
        <template v-if="sub === 'classify'">
          <button
            v-for="(c, j) in choices"
            :key="c.id ?? 'auto'"
            role="menuitemradio"
            :aria-checked="choice !== undefined && choice === c.id"
            :class="['cm-item', { active: j === subActive }]"
            @mouseenter="subActive = j"
            @click="classify(c.id)"
          >
            <span class="cm-icon"><Icon v-if="choice !== undefined && choice === c.id" name="check" :size="14" /></span>
            <span v-if="c.id" class="swatch" :style="{ background: classHexNow(c.id) }" />
            <span class="flex">{{ c.label }}</span>
          </button>
        </template>
        <template v-else-if="sub === 'copy'">
          <button
            v-for="(f, j) in COPY_FORMS"
            :key="f.form"
            role="menuitem"
            :class="['cm-item two', { active: j === subActive }]"
            @mouseenter="subActive = j"
            @click="copy(f.form)"
          >
            <span class="flex">
              <span class="cm-line">{{ copyLabel(f.form) }}</span>
              <span class="cm-preview mono">{{ forms ? forms[f.form] : "…" }}</span>
            </span>
            <Kbd v-if="f.keys">{{ f.keys }}</Kbd>
          </button>
        </template>
        <template v-else>
          <button
            v-for="(tg, j) in TAG_ROWS"
            :key="tg.id ?? 'none'"
            role="menuitem"
            :class="['cm-item', { active: j === subActive }]"
            @mouseenter="subActive = j"
            @click="setTag(tg.id)"
          >
            <span class="cm-icon">
              <span v-if="tg.id" :class="['tag-shape', tg.id]" />
              <Icon v-else name="close" :size="14" />
            </span>
            <span class="flex">{{ tg.id ? tagLabel(settings, tg.id) : t("清除标签") }}</span>
            <Kbd>{{ tg.keys }}</Kbd>
          </button>
        </template>
      </div>
    </div>
  </div>
</template>
