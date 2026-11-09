<!-- The update card (updates.v1, state/updates.ts): the newest release, the start of its notes, and
     getting it: the download (by itself with 「自动下载更新」, else one click), then two big buttons
     (shared/install.ts): 「无缝更新」 (SpaceKnife puts the new version in place itself and starts again)
     and the classic way (the installer with its pages, the disk image or package opened, or the file
     shown in its folder). Nothing is ever installed without one of those clicks. 「跳过 0.4.x」 keeps a
     whole series quiet (update.mute). The same card opens under the title bar's badge, hangs at the
     top left by itself for a minor release and fills the dialog of a major one (UpdatePrompt.vue). It
     never takes the focus, except as that dialog. -->
<script setup lang="ts">
import { computed, onMounted, ref } from "vue";

import Icon from "@/components/ui/Icon.vue";
import { fmtSize } from "@/lib/format";
import { inBrowser, serverName } from "@/lib/host";
import { t } from "@/lib/i18n";
import { revealLabel } from "@/lib/reveal";
import { choiceHint, choiceTitle } from "@/lib/updateCard";
import type { UpdateInfo, UpdateStatus } from "@/protocol/generated";
import { type InstallHow, type InstallStep, stepQuits } from "@/shared/install";
import { cancelUpdate, closeUpdate, downloadUpdate, installUpdate, muteUpdates } from "@/state/actions";
import { useApp } from "@/state/store";
import { downloading, downloadOf, downloadPct, installChoices, notesFor, releaseDate, seriesName, summarizeNotes, type UpdatePlacement } from "@/state/updates";

const props = defineProps<{ status: UpdateStatus & { latest: UpdateInfo }; placement: UpdatePlacement }>();

const lang = useApp((s) => s.lang);
const latest = computed(() => props.status.latest);
// the notes are English, then 简体中文: the start of the part in the interface language
const notes = computed(() => summarizeNotes(notesFor(latest.value.notes, lang.value)));
const date = computed(() => releaseDate(latest.value.published_at));
const dialog = computed(() => props.placement === "dialog");
const inline = computed(() => props.placement === "inline");
const self = ref<HTMLElement | null>(null);
const titleId = computed(() => `update-title-${props.placement}`);
onMounted(() => {
  if (dialog.value) self.value?.focus(); // the dialog itself, not a button: Enter meant for the map does nothing here
});

function onKeyDown(e: KeyboardEvent): void {
  if (e.key !== "Escape" || props.placement === "anchored") return; // under the badge: the badge closes it and takes the focus back
  e.stopPropagation(); // not also the map's Escape (clear the selection)
  closeUpdate();
}

// --- the download and the buttons for the next step

// installing quits SpaceKnife: a scan still running asks once more first
const scanning = useApp((s) => Object.values(s.scans).some((x) => x.state === "scanning" || x.state === "queued"));
const confirming = ref<InstallHow | null>(null);
const browser = inBrowser();
const d = computed(() => downloadOf(props.status));
const pct = computed(() => downloadPct(d.value));
const series = computed(() => props.status.series);
const openPage = () => window.sk.shell.openExternal(latest.value.url);
const busy = computed(() => downloading(d.value));
/** the downloaded file, once it is there */
const readyPath = computed(() => (d.value.state === "done" && d.value.path ? d.value.path : null));
const reveal = (path: string) => window.sk.shell.showItemInFolder(path);
const choices = computed(() => installChoices(props.status.install_kind));
const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

// closing SpaceKnife stops a running scan: the first click says so, the second goes on
function go(path: string, how: InstallHow, step: InstallStep): void {
  if (scanning.value && stepQuits(step) && confirming.value !== how) confirming.value = how;
  else void installUpdate(path, how);
}
</script>

<template>
  <section
    ref="self"
    :class="['update-card', placement]"
    role="dialog"
    :aria-modal="dialog || undefined"
    :aria-labelledby="titleId"
    :tabindex="dialog ? -1 : undefined"
    @keydown="onKeyDown"
  >
    <header class="uc-head">
      <span class="uc-icon">
        <Icon name="up" :size="dialog ? 20 : 18" />
      </span>
      <div class="uc-title">
        <span v-if="dialog" class="uc-kicker">{{ t("大版本更新") }}</span>
        <h3 :id="titleId">{{ t("SpaceKnife {version} 可用", { version: latest.version }) }}</h3>
        <div class="uc-sub">
          <span v-if="date">{{ t("{date} 发布", { date }) }}</span>
          <span v-if="date" class="uc-dot" aria-hidden="true" />
          <span>{{ t("你在用 {current}", { current: status.current }) }}</span>
          <span v-if="latest.prerelease" class="uc-tag">{{ t("预览版") }}</span>
        </div>
      </div>
      <button
        v-if="placement !== 'inline'"
        class="icon-btn small"
        :aria-label="t('关闭')"
        :title="placement === 'anchored' ? t('关闭') : t('关闭（这个版本不再自动提示）')"
        @click="closeUpdate"
      >
        <Icon name="close" :size="14" />
      </button>
    </header>
    <div v-if="notes.lines.length > 0" class="uc-notes">
      <p v-for="(line, i) in notes.lines" :key="i" :class="['uc-note', line.kind, { deep: line.depth }]">{{ line.text }}</p>
      <button class="link small uc-more" @click="openPage">
        {{ t("查看完整说明") }}
        <Icon name="reveal" :size="12" />
      </button>
    </div>
    <div v-if="status.muted && status.series" class="uc-muted">
      <Icon name="info" :size="14" />
      <span>{{ t("已跳过 {series} 的版本提示：不弹出、不显示徽章、不自动下载", { series: seriesName(status.series) }) }}</span>
      <button class="link small" @click="muteUpdates(status.series!, false)">{{ t("恢复提示") }}</button>
    </div>

    <!-- what the backend's download is at: the body, then the buttons for the next step -->
    <div v-if="browser" class="uc-body">
      <!-- the update is for the computer SpaceKnife runs on: nothing to download or install from a tab -->
      <div class="uc-line">
        <Icon name="info" :size="14" />
        {{ t("请在 {host} 上更新 SpaceKnife：浏览器里不能下载或安装更新", { host: serverName() }) }}
      </div>
    </div>
    <div v-else-if="busy" class="uc-body" aria-live="polite">
      <div class="uc-progress">
        <span>{{
          d.state === "verifying"
            ? t("正在校验文件…")
            : d.total > 0
              ? t("正在下载 {received} / {total}", { received: fmtSize(d.received), total: fmtSize(d.total) })
              : t("正在下载 {received}", { received: fmtSize(d.received), total: fmtSize(d.total) })
        }}</span>
        <span v-if="pct !== null" class="mono">{{ pct }}%</span>
      </div>
      <div
        :class="['uc-bar', { indeterminate: pct === null }]"
        role="progressbar"
        :aria-label="t('下载进度')"
        :aria-valuemin="0"
        :aria-valuemax="100"
        :aria-valuenow="pct ?? undefined"
      >
        <span :style="pct === null ? undefined : { width: `${pct}%` }" />
      </div>
    </div>
    <template v-else-if="readyPath">
      <div class="uc-body">
        <div class="uc-line">
          <Icon name="check" :size="14" />
          <span class="uc-file mono" :title="readyPath">{{ fileName(readyPath) }}</span>
        </div>
        <div v-if="d.verified" class="uc-line ok">
          <Icon name="shield" :size="14" />
          {{ t("SHA-256 校验通过") }}
        </div>
        <div v-else class="uc-line warn">
          <Icon name="warning" :size="14" />
          {{ t("这个版本没有发布校验和，文件未经校验") }}
        </div>
        <div v-if="choices.length > 0" class="uc-line">
          <Icon name="info" :size="14" />
          {{ t("选一种方式安装：只有你点了才会开始，扫描数据和设置都会保留") }}
        </div>
        <div v-if="confirming" class="uc-line warn">
          <Icon name="warning" :size="14" />
          {{ t("还有扫描在进行，更新会中断它（已扫描的部分不会保存）") }}
        </div>
      </div>
      <div v-if="choices.length > 0" class="uc-choices">
        <button v-for="{ how, step } in choices" :key="how" :class="['uc-choice', how, { confirm: confirming === how }]" @click="go(readyPath, how, step)">
          <Icon :name="how === 'seamless' ? 'bolt' : step === 'reveal' ? 'folder' : 'play'" :size="18" />
          <span class="uc-choice-text">
            <strong>{{ choiceTitle(status.install_kind, step) }}</strong>
            <span>{{ confirming === how ? t("再点一次继续") : choiceHint(status.install_kind, step) }}</span>
          </span>
        </button>
      </div>
    </template>
    <div v-else class="uc-body">
      <div v-if="d.state === 'failed'" class="uc-line error" role="alert">
        <Icon name="warning" :size="14" />
        {{ d.error || t("下载失败") }}
      </div>
      <div v-else-if="latest.asset" class="uc-line">
        <Icon name="file" :size="14" />
        <span class="uc-file mono">{{ latest.asset.name }}</span>
        <span class="uc-size">{{ fmtSize(latest.asset.size) }}</span>
      </div>
      <div v-else class="uc-line">
        <Icon name="info" :size="14" />
        {{ t("这种安装方式请到发布页下载") }}
      </div>
      <div v-if="d.state === 'cancelled'" class="uc-line">{{ t("已取消下载") }}</div>
    </div>

    <div class="uc-actions">
      <button v-if="!browser && busy" class="ghost" @click="cancelUpdate()">{{ t("取消") }}</button>
      <template v-else>
        <span v-if="!inline" class="uc-later">
          <button class="text-btn" @click="closeUpdate">{{ t("以后再说") }}</button>
          <button
            v-if="series && !status.muted"
            class="text-btn"
            :title="t('{series} 的版本不再弹出提示、不显示徽章，也不自动下载；可以在「关于与更新」里恢复', { series: seriesName(series) })"
            @click="muteUpdates(series, true)"
          >
            {{ t("跳过 {series}", { series: seriesName(series) }) }}
          </button>
        </span>
        <template v-if="!browser && readyPath">
          <button v-if="!choices.some((c) => c.step === 'reveal')" :class="choices.length > 0 ? 'ghost' : 'primary'" @click="reveal(readyPath)">
            <Icon name="folder" :size="14" />
            {{ revealLabel() }}
          </button>
        </template>
        <template v-else>
          <button class="ghost" @click="openPage">
            <Icon name="reveal" :size="14" />
            {{ t("前往下载页") }}
          </button>
          <button
            v-if="!browser"
            class="primary uc-download"
            :disabled="!latest.asset"
            :title="latest.asset ? `${latest.asset.name} · ${fmtSize(latest.asset.size)}` : t('这种安装方式请到发布页下载')"
            @click="downloadUpdate(latest.version)"
          >
            <Icon name="up" :size="14" />
            {{ d.state === "failed" ? t("重试") : t("一键下载") }}
          </button>
        </template>
      </template>
    </div>
  </section>
</template>
