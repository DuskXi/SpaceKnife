<!-- The log panel's event list (LogPanel.vue): virtualised rows, the arrow keys move the highlight,
     Enter locates its path in the map, Ctrl+C copies it; follows the newest line while scrolled to
     the bottom (UI-W8-32). -->
<script setup lang="ts">
import { onMounted, onScopeDispose, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { getLang, t } from "@/lib/i18n";
import { LOG_ROW_H as ROW_H, logLevelNames, logLevelText, logSourceNames, messageParts, sourceText } from "@/lib/logPanel";
import { call, viewCall } from "@/state/actions";
import { get, type LogEntry, set, toast } from "@/state/store";

const props = defineProps<{ logs: LogEntry[] }>();

const box = ref<HTMLDivElement | null>(null);
const follow = ref(true);
const range = ref({ first: 0, count: 12 });
const active = ref(-1);

// stay on the newest line while following (UI-W8-32)
watch(
  [() => props.logs.length, follow],
  () => {
    const el = box.value;
    if (el && follow.value) el.scrollTop = el.scrollHeight;
  },
  { flush: "post" },
);

function onScroll(): void {
  const el = box.value;
  if (!el) return;
  const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < ROW_H;
  if (atBottom !== follow.value) follow.value = atBottom;
  const first = Math.floor(el.scrollTop / ROW_H);
  const count = Math.ceil(el.clientHeight / ROW_H) + 1;
  if (first !== range.value.first || count !== range.value.count) range.value = { first, count };
}

let ro: ResizeObserver | null = null;
onMounted(() => {
  const el = box.value;
  if (el && follow.value) el.scrollTop = el.scrollHeight;
  if (!el || typeof ResizeObserver === "undefined") return;
  ro = new ResizeObserver(onScroll);
  ro.observe(el);
});
onScopeDispose(() => ro?.disconnect());

function copyPath(path: string): void {
  window.sk.shell.copyText(path);
  toast(t("已复制"));
}

/** Locate an event's path in the focused pane of its scan's tab (UI-W8-27). */
async function locate(e: LogEntry): Promise<void> {
  const s = get();
  const tab = s.tabs.find((x) => x.scanId === e.scanId);
  if (!tab) {
    toast(t("这次扫描已经关闭"));
    return;
  }
  if (s.active !== tab.id) set({ active: tab.id });
  const viewId = tab.viewIds[tab.focusedPane] ?? tab.viewIds[0];
  try {
    const found = await call("tree.find", { scan_id: e.scanId, path: e.path });
    if (!found.node) {
      toast(t("该路径已不存在"));
      return;
    }
    await viewCall(viewId ?? null, "view.reveal", { key: found.node.id });
    if (viewId && get().views[viewId]?.filter?.active) toast(t("该项可能被当前过滤隐藏"));
  } catch {
    toast(t("无法定位该路径"));
  }
}

// keys pressed in the log belong to the log: they must never act on the treemap behind it
function onKey(e: KeyboardEvent): void {
  const ctrl = e.ctrlKey || e.metaKey;
  const own = () => {
    e.preventDefault();
    e.stopPropagation();
  };
  const logs = props.logs;
  const row = active.value >= 0 ? logs[active.value] : undefined;
  if (ctrl && e.key.toLowerCase() === "c") {
    own(); // Ctrl+C copies the log line's path, never the treemap selection (UI-W8-28)
    if (row?.path) copyPath(row.path);
    return;
  }
  if (e.key === "Enter") {
    own();
    if (row?.path) void locate(row);
    return;
  }
  if (!ctrl && !e.altKey && (e.key.length === 1 || e.key === "Delete" || e.key === "Backspace" || e.key === "Escape")) e.stopPropagation();
  const n = logs.length;
  if (!n) return;
  const a = active.value;
  let k: number;
  if (e.key === "ArrowDown") k = Math.min(n - 1, a < 0 ? n - 1 : a + 1);
  else if (e.key === "ArrowUp") k = Math.max(0, a < 0 ? n - 1 : a - 1);
  else if (e.key === "Home") k = 0;
  else if (e.key === "End") k = n - 1;
  else return;
  own();
  active.value = k;
  const el = box.value;
  if (el) {
    if (k * ROW_H < el.scrollTop) el.scrollTop = k * ROW_H;
    else if ((k + 1) * ROW_H > el.scrollTop + el.clientHeight) el.scrollTop = (k + 1) * ROW_H - el.clientHeight;
  }
}

function jumpToLatest(): void {
  follow.value = true;
  const el = box.value;
  if (el) el.scrollTop = el.scrollHeight;
}

/** The rows drawn: those in view and four on either side. */
function visible(): { entry: LogEntry; index: number }[] {
  const first = Math.max(0, range.value.first - 4);
  const last = Math.min(props.logs.length, range.value.first + range.value.count + 4);
  return props.logs.slice(first, last).map((entry, i) => ({ entry, index: first + i }));
}

function hhmmss(time: string): string {
  const d = new Date(time);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleTimeString(getLang(), { hour12: false });
}

const newest = (): string => {
  const last = props.logs[props.logs.length - 1];
  return last ? t("{level}：{message}", { level: logLevelText(last.level), message: last.message }) : "";
};
</script>

<template>
  <div class="log-wrap">
    <div
      ref="box"
      class="log-list"
      role="listbox"
      :aria-label="t('事件日志')"
      :aria-activedescendant="active >= 0 ? `log-row-${active}` : undefined"
      tabindex="0"
      @scroll="onScroll"
      @keydown="onKey"
    >
      <div :style="{ height: `${logs.length * ROW_H}px`, position: 'relative' }">
        <div
          v-for="{ entry: e, index } in visible()"
          :id="`log-row-${index}`"
          :key="`${e.seq}-${index}`"
          role="option"
          :aria-selected="index === active"
          :class="['log-row', { on: index === active }, e.level.toLowerCase()]"
          :style="{ top: `${index * ROW_H}px` }"
          :title="e.message"
          @mousedown="active = index"
        >
          <span class="log-time mono">{{ hhmmss(e.time) }}</span>
          <!-- a cell as wide as the column's longest name in the current language: the names sit
               stacked out of sight and size it; the row's own name fills it, cut with … when longer -->
          <span class="log-fit log-level"
            ><span v-for="(name, i) in logLevelNames()" :key="i" class="log-fit-size" aria-hidden="true">{{ name }}</span
            ><span class="log-fit-text">{{ logLevelText(e.level) }}</span></span
          >
          <span class="log-fit log-source"
            ><span v-for="(name, i) in logSourceNames()" :key="i" class="log-fit-size" aria-hidden="true">{{ name }}</span
            ><span class="log-fit-text">{{ sourceText(e.source) }}</span></span
          >
          <!-- paths in messages in monospace, long ones cut from the left (UI-W8-25) -->
          <span class="log-msg"
            ><template v-for="(part, i) in messageParts(e.message, e.path)" :key="i"
              ><span v-if="part.mono" class="mono">{{ part.text }}</span
              ><template v-else>{{ part.text }}</template></template
            ></span
          >
          <span v-if="e.path" class="log-actions">
            <button v-if="e.scanId" class="ghost tiny" @click="locate(e)">{{ t("在树图中定位") }}</button>
            <button class="icon-btn tiny" :aria-label="t('复制路径（Ctrl+C）')" :title="t('复制路径（Ctrl+C）')" @click="copyPath(e.path)">
              <Icon name="copy" :size="13" />
            </button>
          </span>
        </div>
      </div>
      <div v-if="logs.length === 0" class="log-empty">{{ t("暂无事件") }}</div>
    </div>
    <!-- new events only are announced, politely (UI-W8-36); the virtualised rows are not a live region -->
    <div class="sr-only" role="log" aria-live="polite">{{ newest() }}</div>
    <button v-if="!follow && logs.length > 0" class="log-latest" @click="jumpToLatest">
      <Icon name="chevronDown" :size="14" />
      {{ t("跳到最新") }}
    </button>
  </div>
</template>
