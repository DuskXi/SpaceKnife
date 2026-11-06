<!-- The command palette's dialog (W4, UI-W4-01…49). The backend ranks everything (palette.query /
     palette.paths / palette.preview); this renders the groups, keeps one row highlighted, and runs
     the chosen row. Requests are cancelled as the input changes; old rows stay until new ones arrive. -->
<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, shallowRef, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import Kbd from "@/components/ui/Kbd.vue";
import { fmtInt } from "@/lib/format";
import { t } from "@/lib/i18n";
import { firstRunnable, type PaletteGroup, type PaletteLine, paletteLines, withSep } from "@/lib/palette";
import type { PalettePaths, PalettePreview, PaletteResult } from "@/protocol/generated";
import { call, cancellable, editFilter, startScan, viewCall } from "@/state/actions";
import { paletteContext, runCommand } from "@/state/commands";
import { activeTab, basketCount, focusedViewId, set, useApp } from "@/state/store";

import PaletteRow from "./PaletteRow.vue";

const tab = useApp((s) => activeTab(s));
const viewId = useApp((s) => focusedViewId(s));
const basket = useApp(basketCount);
// live values (UI-W4-24): a scan state change refreshes the lists; while a scan runs, every 2 s
const scanState = useApp((s) => (tab.value.scanId ? (s.scans[tab.value.scanId]?.state ?? "") : ""));
const tick = ref(0);
watch(
  scanState,
  (st, _, onCleanup) => {
    if (st !== "scanning" && st !== "queued") return;
    const timer = setInterval(() => tick.value++, 2000);
    onCleanup(() => clearInterval(timer));
  },
  { immediate: true },
);
const scanTab = computed(() => tab.value.kind === "scan" && !!tab.value.scanId);
const returnTo = document.activeElement as HTMLElement | null;
const text = ref("");
const res = shallowRef<PaletteResult | null>(null);
const paths = shallowRef<PalettePaths | null>(null);
const searching = ref(false);
const previews = shallowRef<Record<string, PalettePreview>>({});
const active = ref(-1);
const reload = ref(0);
const input = ref<HTMLInputElement | null>(null);
let composing = false;
let shownText: string | null = null;
let pendingEnter = false; // Enter pressed before the answer for the current input arrived

const close = () => set({ paletteOpen: false });
onMounted(() => input.value?.focus());
onUnmounted(() => returnTo?.focus?.());

// palette.query, then palette.paths and filter previews; superseded requests are cancelled
watch(
  [text, () => tab.value.kind, () => tab.value.scanId, viewId, basket, reload, scanState, tick],
  (_, __, onCleanup) => {
    const q0 = text.value;
    const scanId = tab.value.scanId;
    const view = viewId.value;
    const context = paletteContext();
    const refresh = shownText === q0 && paths.value !== null;
    const jobs: { cancel: () => void }[] = [];
    let alive = true;
    const q = cancellable("palette.query", { text: q0, context });
    jobs.push(q);
    void q.promise
      .then((r) => {
        if (!alive || !r) return;
        res.value = r;
        if (shownText !== r.text) active.value = -1; // new input: highlight the first row again
        shownText = r.text;
        if (pendingEnter && r.text === q0) {
          pendingEnter = false;
          setTimeout(() => run(firstRunnable(r, scanTab.value)), 0);
        }
        if (r.paths && scanId) {
          if (!refresh) searching.value = true; // a live refresh keeps the shown rows without 「搜索中…」 (UI-W4-44)
          const p = cancellable("palette.paths", { scan_id: scanId, text: q0, view_id: view, limit: q0.trim() ? 50 : 5 });
          jobs.push(p);
          void p.promise
            .then((found) => {
              if (alive && found) {
                paths.value = found;
                searching.value = false;
              }
            })
            .catch(() => alive && (searching.value = false));
        } else {
          paths.value = null;
          searching.value = false;
        }
        if (scanId) {
          const exprs = [...r.direct.filter((d) => d.kind === "filter" && d.valid).map((d) => d.text), ...r.recent.filter((x) => x.kind === "filter").map((x) => x.text)];
          for (const expr of exprs) {
            const job = cancellable("palette.preview", { scan_id: scanId, text: expr });
            jobs.push(job);
            void job.promise.then((pv) => alive && pv && (previews.value = { ...previews.value, [expr]: pv })).catch(() => {});
          }
        }
      })
      .catch(() => {});
    onCleanup(() => {
      alive = false;
      for (const j of jobs) j.cancel();
    });
  },
  { immediate: true },
);

const lines = computed(() => paletteLines(res.value, paths.value, scanTab.value));
const current = computed(() => {
  const n = lines.value.length;
  if (active.value >= 0 && active.value < n) return active.value;
  const first = lines.value.findIndex((l) => l.enabled);
  return first >= 0 ? first : 0;
});

watch(current, (k) => document.getElementById(`pal-${k}`)?.scrollIntoView({ block: "nearest" }), { flush: "post" });

// ------------------------------------------------------------------ running rows

async function jump(node: number, path: string): Promise<void> {
  const view = viewId.value;
  if (!view) return;
  await viewCall(view, "view.zoom", { key: node });
  void call("palette.remember", { kind: "jump", text: path, scan_id: tab.value.scanId ?? null }).catch(() => {});
}

function run(line: PaletteLine | null | undefined): void {
  if (!line || !line.enabled) return;
  const row = line.row;
  const view = viewId.value;
  close();
  setTimeout(() => {
    switch (row.kind) {
      case "filter":
        if (view) {
          void editFilter(view, row.d.text, null, true);
          void call("palette.remember", { kind: "filter", text: row.d.text, scan_id: null }).catch(() => {});
        }
        return;
      case "goto":
        if (row.d.state === "in_scan" && row.d.node != null) void jump(row.d.node, row.d.text);
        else if (row.d.state === "outside") void startScan({ roots: [row.d.text] });
        return;
      case "recent":
        if (row.r.kind === "jump" && row.r.node != null) void jump(row.r.node, row.r.text);
        else if (row.r.kind === "filter" && view) {
          void editFilter(view, row.r.text, null, true);
          void call("palette.remember", { kind: "filter", text: row.r.text, scan_id: null }).catch(() => {});
        }
        return;
      case "command":
        void runCommand(row.c.id);
        return;
      case "path":
        void jump(row.p.node, row.p.complete.replace(/[\\/]+$/, "") || row.p.complete);
    }
  }, 0);
}

function complete(line: PaletteLine | undefined): void {
  if (!line) return;
  const row = line.row;
  if (row.kind === "filter") {
    const q = text.value;
    void call("filter.parse", { text: q, cursor: q.length }).then((p) => {
      const c = p.completion;
      if (c && c.candidates[0] !== undefined) text.value = q.slice(0, c.start) + c.candidates[0] + q.slice(c.end);
    });
    return;
  }
  let next: string | null = null;
  if (row.kind === "goto") next = row.d.complete ?? null;
  else if (row.kind === "path") next = row.p.complete;
  else if (row.kind === "command") next = row.c.name;
  else if (row.kind === "recent") next = row.r.kind === "jump" && row.r.is_dir ? withSep(row.r.text) : row.r.text;
  if (next != null) text.value = next;
}

function onKey(e: KeyboardEvent): void {
  if (composing || e.isComposing || e.keyCode === 229) return; // the IME owns these keys
  const n = lines.value.length;
  const cur = current.value;
  const move = (to: number) => {
    e.preventDefault();
    if (n) active.value = ((to % n) + n) % n;
  };
  if (e.key === "ArrowDown") move(cur + 1);
  else if (e.key === "ArrowUp") move(cur - 1);
  else if (e.key === "PageDown") move(Math.min(n - 1, cur + 10));
  else if (e.key === "PageUp") move(Math.max(0, cur - 10));
  else if (e.key === "Enter") {
    e.preventDefault();
    // the rows on screen may still answer an older input: run once the current one is answered (UI-W4-42)
    if (res.value?.text !== text.value) pendingEnter = true;
    else run(lines.value[cur]);
  } else if (e.key === "Tab") {
    e.preventDefault();
    complete(lines.value[cur]);
  } else if (e.key === "Escape") {
    e.preventDefault();
    close();
  } else return;
  e.stopPropagation();
}

function clearRecent(): void {
  void call("palette.clear", { scan_id: tab.value.scanId ?? null }).then(() => reload.value++);
  input.value?.focus();
}

// keep the caret in the input: headers, hints and padding must not take the focus (UI-W4-38)
function keepFocus(e: MouseEvent): void {
  if (e.target !== input.value && !(e.target as HTMLElement).closest("button")) e.preventDefault();
}

// ------------------------------------------------------------------ rendering

const GROUPS: PaletteGroup[] = ["direct", "recent", "command", "path"];
const groupTitle = (g: PaletteGroup): string => ({ direct: "", recent: t("最近"), command: t("命令"), path: t("路径") })[g];
/** The groups shown, each with its lines and their places in the whole list (the ids `pal-<k>`). */
const shownGroups = computed(() => {
  const all = lines.value;
  const pathVisible = !!res.value?.paths && (searching.value || (paths.value?.items.length ?? 0) > 0);
  return GROUPS.map((id) => ({ id, members: all.map((line, k) => ({ line, k })).filter((m) => m.line.group === id) })).filter((g) =>
    g.id === "path" ? pathVisible : g.members.length > 0,
  );
});
const noResults = computed(() => !!res.value && lines.value.length === 0 && !searching.value);

function onText(e: Event): void {
  text.value = (e.target as HTMLInputElement).value;
}

const setComposing = (on: boolean) => (composing = on);
</script>

<template>
  <div class="pal-layer">
    <div class="scrim" @mousedown="close" />
    <div class="palette" role="dialog" aria-modal="true" :aria-label="t('命令面板')" @mousedown="keepFocus">
      <div class="pal-input">
        <Icon name="search" :size="20" />
        <input
          ref="input"
          :value="text"
          :placeholder="t('输入命令、路径或过滤条件…')"
          spellcheck="false"
          role="combobox"
          :aria-label="t('命令面板输入')"
          aria-expanded="true"
          aria-controls="sk-palette-list"
          aria-autocomplete="list"
          :aria-activedescendant="lines.length ? `pal-${current}` : undefined"
          @input="onText"
          @keydown="onKey"
          @compositionstart="setComposing(true)"
          @compositionend="setComposing(false)"
        />
        <span v-if="scanTab" class="pal-scope" :title="t('命令面板作用于这个扫描')">
          <Icon name="drive" :size="14" />
          {{ tab.title }}
        </span>
      </div>
      <div id="sk-palette-list" class="pal-list" role="listbox" :aria-label="t('命令、路径与最近')">
        <div v-for="g in shownGroups" :key="g.id" role="group" :aria-label="groupTitle(g.id) || t('直达')" class="pal-group">
          <div v-if="groupTitle(g.id)" class="pal-head">
            <span aria-hidden="true">{{ groupTitle(g.id) }}</span>
            <span class="pal-head-extra" :role="g.id === 'path' ? 'status' : undefined">
              <button v-if="g.id === 'recent'" class="pal-clear" :aria-label="t('清除最近记录')" @mousedown.prevent @click="clearRecent">{{ t("清除") }}</button>
              <template v-else-if="g.id === 'path'">
                <span v-if="searching">{{ t("搜索中…") }}</span>
                <span v-else-if="paths?.partial" class="warn">{{ t("扫描未完成，结果可能不全") }}</span>
                <span v-else-if="res?.seed">{{ t("匹配选中项「{seed}」", { seed: res.seed }) }}</span>
                <span v-else-if="paths?.folder">{{ t("{n} 项", { n: fmtInt(paths.total) }) }}</span>
                <span v-else-if="paths && paths.total > paths.items.length">{{ t("显示 {shown} / {total}", { shown: paths.items.length, total: fmtInt(paths.total) }) }}</span>
              </template>
            </span>
          </div>
          <PaletteRow
            v-for="{ line, k } in g.members"
            :id="`pal-${k}`"
            :key="k"
            :line="line"
            :on="k === current"
            :previews="previews"
            @hover="active = k"
            @run="run(line)"
          />
        </div>
        <div v-if="noResults" class="pal-empty">{{ t("没有匹配的命令或路径") }}</div>
      </div>
      <div class="pal-hints">
        <span><Kbd>↑</Kbd><Kbd>↓</Kbd> {{ t("选择") }}</span>
        <span><Kbd>Enter</Kbd> {{ t("执行") }}</span>
        <span><Kbd>Tab</Kbd> {{ t("补全") }}</span>
        <span><Kbd>Esc</Kbd> {{ t("关闭") }}</span>
      </div>
      <div class="sr-only" role="status" aria-live="polite">{{ res ? t("{n} 个结果", { n: lines.length }) : "" }}</div>
    </div>
  </div>
</template>
