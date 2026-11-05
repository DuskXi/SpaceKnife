<!-- Start page (W1, UI-W1-01…51): pick volumes or folders, set this scan's options, start; open a
     recent scan or a snapshot file. The form lives in the store per tab (switching tabs keeps it).
     Folders and snapshot files dropped on the window come with their paths through
     window.sk.files.onDropPaths (Tauri's drag-drop event); in a browser tab the page's own drop only
     explains that the paths are the server's. -->
<script setup lang="ts">
import { computed, onScopeDispose, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import Kbd from "@/components/ui/Kbd.vue";
import StartRecents from "@/components/pages/StartRecents.vue";
import { fmtSize, fmtSizeAs } from "@/lib/format";
import { inBrowser, serverName } from "@/lib/host";
import { byLang, t } from "@/lib/i18n";
import { baseName, IS_WIN, LOAD_ZH, LOADS, MEDIA_ZH, methodBadge, methodText, METHODS, mftCapable, sameRoot, splitPaths, volumeName } from "@/lib/volumes";
import { call, openSnapshot, refreshLists, refreshVolumes, startScan } from "@/state/actions";
import { patchStartForm, setting, startFormOf, type Tab, toast, useApp } from "@/state/store";

const props = defineProps<{ tab: Tab }>();

const volumes = useApp((s) => s.volumes);
const conn = useApp((s) => s.conn);
const form = useApp((s) => startFormOf(s, props.tab.id));
const elevated = useApp((s) => s.volumesElevated);
const mayElevate = useApp((s) => setting(s, "scan.elevate", true));
const refreshing = ref(false);
const starting = ref(false);
const dragging = ref(false);
const pathInput = ref<HTMLInputElement | null>(null);
const volumeGroup = ref<HTMLDivElement | null>(null);
const patch = (p: Parameters<typeof patchStartForm>[1]) => patchStartForm(props.tab.id, p);

// lists on connect; volumes every few seconds so inserted / removed drives show up (UI-W1-21)
watch(
  conn,
  (c, _, onCleanup) => {
    if (c !== "ready") return;
    void refreshLists();
    const timer = setInterval(() => void refreshVolumes(), 5000);
    onCleanup(() => clearInterval(timer));
  },
  { immediate: true },
);

const paths = computed(() => splitPaths(form.value.path));
// a path that is exactly a volume root selects that card (UI-W1-24)
const rootMatch = computed(() => (paths.value.length === 1 ? volumes.value.find((v) => sameRoot(v.mount_point, paths.value[0]!)) : undefined));
const selected = computed(() => (paths.value.length === 0 ? volumes.value.find((v) => v.mount_point === form.value.picked && !v.reason) : rootMatch.value));

// validate typed paths shortly after typing stops (UI-W1-25)
watch(
  () => [form.value.path, rootMatch.value?.mount_point] as const,
  (_, __, onCleanup) => {
    if (!paths.value.length || rootMatch.value) {
      if (form.value.check) patch({ check: null });
      return;
    }
    const asked = paths.value;
    const timer = setTimeout(() => {
      void call("scan.check_paths", { paths: asked })
        .then((check) => patch({ check }))
        .catch(() => {});
    }, 300);
    onCleanup(() => clearTimeout(timer));
  },
  { immediate: true },
);

const check = computed(() => (form.value.check && paths.value.length && !rootMatch.value ? form.value.check : null));
const errors = computed(() => check.value?.items.filter((i) => i.state !== "ok") ?? []);
const merged = computed(() => check.value?.items.filter((i) => i.merged_into) ?? []);
const roots = computed(() => {
  if (selected.value) return [selected.value.mount_point];
  if (!paths.value.length) return [];
  const c = check.value;
  return c && c.items.length === paths.value.length && !errors.value.length ? c.roots : [];
});
const ready = computed(() => conn.value === "ready" && roots.value.length > 0);
const targetName = computed(() =>
  selected.value ? volumeName(selected.value) : roots.value.length > 1 ? t("{n} 个位置", { n: roots.value.length }) : roots.value[0] ? (baseName(roots.value[0]) ?? "") : "",
);
const capable = computed(() => mftCapable(selected.value));
// the MFT fast scan reads a whole local NTFS volume; it may need the administrator prompt (UI-W1-34)
const viaMft = computed(() => form.value.method !== "hard" && capable.value && (elevated.value || mayElevate.value));
const needsAdmin = computed(() => viaMft.value && !elevated.value);
const mergedText = computed(() =>
  t("{items}，将合并扫描", {
    items: merged.value.map((m) => t("{path} 已包含在 {parent} 中", { path: m.path, parent: m.merged_into ?? "" })).join(byLang("；", "; ")),
  }),
);

function start(): void {
  if (!ready.value || starting.value) return;
  const mft = capable.value;
  starting.value = true;
  void startScan({ roots: roots.value, method: mft ? form.value.method : "hard", load_level: form.value.load, ads: mft && form.value.ads }, props.tab.id).finally(
    () => (starting.value = false),
  );
}

function onKey(e: KeyboardEvent): void {
  if (e.key !== "Enter") return;
  const el = e.target as HTMLElement;
  // Enter starts the scan unless another button has the focus (UI-W1-35)
  if (el.tagName === "BUTTON" && !el.closest(".volumes, .start-options")) return;
  if (el.tagName === "INPUT" && el !== pathInput.value) return;
  e.preventDefault();
  start();
}

function onPathKey(e: KeyboardEvent): void {
  e.stopPropagation(); // typing is not a shortcut
  if (e.key === "Enter" && !e.isComposing) {
    e.preventDefault();
    start(); // Enter in the path box starts the scan (UI-W1-28)
  }
}

async function browse(): Promise<void> {
  const picked = await window.sk.dialog.openFolders();
  if (picked.length) patch({ path: [...paths.value, ...picked].join("; "), picked: null });
}

function refresh(): void {
  refreshing.value = true;
  void refreshVolumes().finally(() => (refreshing.value = false));
}

// folders or a snapshot file dropped on the window (UI-W1-27, UI-W1-44): the app gets their paths
// from Tauri's drag-drop event; a path that isn't a folder shows in the path check's errors
onScopeDispose(
  window.sk.files.onDropPaths((dropped) => {
    dragging.value = false;
    const snap = dropped.find((p) => /\.sks$/i.test(p));
    if (snap) return void openSnapshot(snap, props.tab.id);
    if (dropped.length) patch({ path: dropped.join("; "), picked: null });
  }),
);

// the page's own drop: a browser tab gets no paths, and what it gets is this computer's, not the server's
function onDragOver(e: DragEvent): void {
  e.preventDefault();
  if (!dragging.value) dragging.value = true;
}
function onDragLeave(e: DragEvent): void {
  if (!(e.currentTarget as HTMLElement).contains(e.relatedTarget as Node | null)) dragging.value = false;
}
function onDrop(e: DragEvent): void {
  e.preventDefault();
  dragging.value = false;
  if (!inBrowser()) return; // the app: the paths come through onDropPaths
  if (e.dataTransfer?.files.length) toast(t("拖进来的是这台电脑上的文件：请输入 {host} 上的路径", { host: serverName() }), 4000);
}

// ------------------------------------------------------------------ volumes (radio group)

const usable = computed(() => volumes.value.filter((v) => !v.reason));
function pick(mp: string): void {
  patch({ picked: mp, path: "", check: null });
}
function onVolumeKey(e: KeyboardEvent): void {
  const step = ({ ArrowRight: 1, ArrowLeft: -1, ArrowDown: 2, ArrowUp: -2 } as Record<string, number>)[e.key];
  const list = usable.value;
  if (!step || !list.length) return;
  e.preventDefault();
  const at = Math.max(0, list.findIndex((v) => v.mount_point === selected.value?.mount_point));
  const next = list[Math.max(0, Math.min(list.length - 1, at + step))]!;
  pick(next.mount_point);
  volumeGroup.value?.querySelector<HTMLElement>(`[data-mp="${CSS.escape(next.mount_point)}"]`)?.focus();
}
const pct = (used: number, total: number) => (total ? Math.round((used / total) * 100) : 0);
const volumeMeta = (v: (typeof volumes.value)[number]) => [v.fs_type || "—", MEDIA_ZH[v.media] ?? v.media, v.total ? fmtSize(v.total, true) : "—"].join(" · ");
const pathPlaceholder = computed(() => t("例如 {paths}", { paths: IS_WIN() ? "C:\\Users\\Public\\Videos; D:\\Downloads" : "~/Videos; ~/Downloads" }));
</script>

<template>
  <div :class="['start', { dragging }]" @keydown="onKey" @dragover="onDragOver" @dragleave="onDragLeave" @drop="onDrop">
    <div class="start-cols">
      <section class="start-main" :aria-label="t('选择要扫描的位置')">
        <div class="start-scroll">
          <div class="start-title">
            <h1>{{ t("选择要扫描的位置") }}</h1>
            <button class="ghost small" :disabled="refreshing" @click="refresh">
              <Icon name="refresh" :size="14" />
              {{ refreshing ? t("正在刷新…") : t("刷新") }}
            </button>
          </div>
          <div v-if="!volumes.length" class="volumes-empty muted">{{ t("正在读取卷…") }}</div>
          <div v-else ref="volumeGroup" class="volumes" role="radiogroup" :aria-label="t('卷')" @keydown="onVolumeKey">
            <button
              v-for="v in volumes"
              :key="v.mount_point"
              :data-mp="v.mount_point"
              role="radio"
              :aria-checked="v.mount_point === selected?.mount_point"
              :aria-disabled="!!v.reason"
              :tabindex="v.mount_point === selected?.mount_point || (!selected && v === volumes[0]) ? 0 : -1"
              :class="['volume', { on: v.mount_point === selected?.mount_point, unavailable: !!v.reason }]"
              :title="v.remote_path || v.mount_point"
              @click="!v.reason && pick(v.mount_point)"
            >
              <span class="vol-top">
                <span class="vol-icon"><Icon name="drive" :size="18" /></span>
                <span class="vol-text">
                  <span class="vol-name">
                    {{ volumeName(v) }}
                    <span v-if="v.remote_path" class="vol-remote mono">{{ v.remote_path }}</span>
                  </span>
                  <span class="vol-meta">{{ volumeMeta(v) }}</span>
                </span>
                <span :class="['vol-radio', { on: v.mount_point === selected?.mount_point }]">
                  <Icon v-if="v.mount_point === selected?.mount_point" name="check" :size="12" :stroke-width="3" />
                </span>
              </span>
              <span class="vol-bar">
                <span :style="{ width: `${v.total ? (v.used / v.total) * 100 : 0}%` }" />
              </span>
              <span class="vol-figures mono">
                <span>{{ v.total ? t("已用 {size} · {pct}%", { size: fmtSizeAs(v.used, v.total), pct: pct(v.used, v.total) }) : t("已用 —") }}</span>
                <span>{{ v.total ? t("可用 {size}", { size: fmtSizeAs(v.free, v.total) }) : t("可用 —") }}</span>
              </span>
              <span :class="['vol-badge', { warn: v.reason || v.note }]" :title="v.note || undefined">
                <Icon :name="methodBadge(v, form.method, elevated).icon" :size="13" />
                {{ methodBadge(v, form.method, elevated).text }}
              </span>
            </button>
          </div>
          <div class="path-area">
            <div class="path-label">
              <span>{{ t("或输入路径（多个路径用 ; 分隔）") }}</span>
              <span v-if="inBrowser()" class="muted small"><Icon name="globe" :size="13" /> {{ t("{host} 上的路径", { host: serverName() }) }}</span>
              <span v-else class="muted small"><Icon name="upload" :size="13" /> {{ t("也可以把文件夹直接拖进窗口") }}</span>
            </div>
            <div class="path-row">
              <span class="path-icon"><Icon name="folder" /></span>
              <input
                ref="pathInput"
                class="path-input"
                :placeholder="pathPlaceholder"
                spellcheck="false"
                :aria-label="t('扫描路径')"
                :aria-invalid="errors.length > 0"
                :value="form.path"
                @input="patch({ path: ($event.target as HTMLInputElement).value })"
                @keydown="onPathKey"
              />
              <button v-if="!inBrowser()" class="ghost" @click="browse">{{ t("浏览…") }}</button>
            </div>
            <div v-for="e in errors" :key="e.path" class="path-error" role="alert">
              <Icon name="warning" :size="13" />
              <span class="mono">{{ e.path }}</span>{{ byLang("：", ": ") }}{{ e.message }}
            </div>
            <div v-if="merged.length > 0 && !errors.length" class="path-note">
              <Icon name="info" :size="13" />
              {{ mergedText }}
            </div>
          </div>
          <div class="start-options">
            <h2>{{ t("扫描选项") }}</h2>
            <!-- the method and the NTFS streams only mean something for a whole local NTFS volume on
                 Windows: anywhere else (Linux, macOS, ReFS / Btrfs / exFAT drives, folders) a hard scan is
                 the only way -->
            <div class="set-card">
              <div v-if="capable" class="set-row">
                <div class="set-text">
                  <div class="set-name">{{ t("扫描方式") }}</div>
                  <div class="set-desc">{{ methodText(form.method)[1] }}</div>
                </div>
                <div class="segmented small" role="radiogroup" :aria-label="t('扫描方式')">
                  <button v-for="m in METHODS" :key="m" role="radio" :aria-checked="form.method === m" :class="form.method === m ? 'on' : ''" @click="patch({ method: m })">
                    {{ methodText(m)[0] }}
                  </button>
                </div>
              </div>
              <div class="set-row">
                <div class="set-text">
                  <div class="set-name">{{ t("磁盘负载上限") }}</div>
                  <div class="set-desc">{{ LOAD_ZH[form.load][1] }}</div>
                </div>
                <div class="segmented small" role="radiogroup" :aria-label="t('磁盘负载上限')">
                  <button v-for="l in LOADS" :key="l" role="radio" :aria-checked="form.load === l" :class="form.load === l ? 'on' : ''" @click="patch({ load: l })">
                    {{ LOAD_ZH[l][0] }}
                  </button>
                </div>
              </div>
              <div v-if="capable" class="set-row">
                <div class="set-text">
                  <div class="set-name">{{ t("扫描 NTFS 备用数据流") }}</div>
                  <div class="set-desc">{{ t("统计附加在文件上的隐藏数据流（MFT 快速扫描时计入）") }}</div>
                </div>
                <button role="switch" :aria-checked="form.ads" :aria-label="t('扫描 NTFS 备用数据流')" :class="['switch', { on: form.ads }]" @click="patch({ ads: !form.ads })">
                  <span />
                </button>
              </div>
            </div>
          </div>
        </div>
        <div class="start-bar">
          <div class="start-summary">
            <div>
              <template v-if="targetName">
                <strong>{{ targetName }}</strong> · {{ capable ? methodText(form.method)[0] : t("并行硬扫描")
                }}{{ capable && form.method === "auto" ? (viaMft ? t("（MFT 快速扫描）") : t("（并行硬扫描）")) : ""
                }}{{ " · " }}{{ t("磁盘负载上限 {level}", { level: LOAD_ZH[form.load][0].replace(/ (\d+%)/, " ≤$1") })
                }}{{ form.ads && viaMft ? ` · ${t("备用数据流")}` : "" }}
              </template>
              <span v-else class="muted">{{ t("选择一个卷，或输入要扫描的文件夹") }}</span>
            </div>
            <div v-if="selected?.note" class="warn-text small">{{ selected.note }}</div>
            <div v-if="needsAdmin" class="muted small"><Icon name="shield" :size="13" /> {{ t("需要管理员权限：仅启动只读扫描助手，主程序不提权") }}</div>
          </div>
          <button class="primary big" :disabled="!ready || starting" :aria-busy="starting" @click="start">
            <Icon name="play" />
            {{ starting ? t("正在开始…") : t("开始扫描") }}
            <Kbd>Enter</Kbd>
          </button>
        </div>
      </section>
      <StartRecents :tab="tab" />
    </div>
    <div v-if="dragging" class="drop-hint" aria-hidden="true">
      <Icon name="upload" :size="28" />
      {{ t("松开以添加文件夹，或打开快照文件") }}
    </div>
  </div>
</template>
