<!-- The delete basket drawer (UI-W5-25…49; shell/BasketDrawer.tsx): what goes, what it frees, then
     the recycle bin or a permanent deletion (two-step), with undo. The basket itself lives in the
     backend. -->
<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";

import Icon from "@/components/ui/Icon.vue";
import {
  BASKET_SORTS,
  type BasketSort,
  type Confirmed,
  basketSortLabel,
  changedSince,
  confirmation,
  confirmedOf,
  itemCount,
  mainVolume,
  parentOf,
  sortedItems,
  statusText,
  stillConfirmed,
} from "@/lib/basketDrawer";
import { fmtInt, fmtSize, plural, sizeParts } from "@/lib/format";
import { inBrowser, serverName } from "@/lib/host";
import { byLang, t } from "@/lib/i18n";
import { basketClear, basketExecute, basketRemove } from "@/state/actions";
import { set, toast, useApp } from "@/state/store";

const basket = useApp((s) => s.basket);
const busy = useApp((s) => !!s.op && (s.op.phase === "running" || s.op.phase === "waiting"));
const sort = ref<BasketSort>("size");
const confirm = ref(false);
// what the confirmation dialog asks about: the basket as it was when it opened (another session
// can add to the shared basket meanwhile; that is not part of what the user confirms)
const confirmed = ref<Confirmed>([]);
const asked = computed(() => stillConfirmed(confirmed.value, basket.value?.items ?? []));
const askedSize = computed(() => asked.value.reduce((n, it) => n + it.alloc, 0));
const cancelBtn = ref<HTMLButtonElement | null>(null);
const items = computed(() => sortedItems(basket.value?.items ?? [], sort.value));
const total = computed(() => basket.value?.freeable ?? 0);
const vol = computed(() => mainVolume(basket.value?.volumes ?? []));
const blocked = computed(() => items.value.filter((it) => it.status !== "ready"));
const big = computed(() => sizeParts(total.value));
const noTrash = computed(() => (basket.value?.volumes ?? []).filter((v) => !v.trash && v.freeable > 0));
const remote = inBrowser();

const trashNote = computed(() =>
  noTrash.value.length
    ? t("{volumes}。这些项只能永久删除", {
        volumes: noTrash.value.map((v) => t("{volume}：{note}", { volume: v.mount_point, note: v.trash_note || t("没有回收站") })).join(byLang("；", "; ")),
      })
    : vol.value?.trash_note || t("同一个卷上的回收站，随时可以还原"),
);

const cap = computed(() => {
  const v = vol.value;
  if (!v) return null;
  const used = Math.max(0, v.total - v.free - v.freeable);
  const pct = (n: number) => `${v.total ? (n / v.total) * 100 : 0}%`;
  return { v, used: pct(used), queued: pct(v.freeable), free: pct(v.free) };
});

watch(confirm, async (open) => {
  if (!open) return;
  await nextTick();
  cancelBtn.value?.focus(); // autoFocus
});

function askDelete(): void {
  confirmed.value = confirmedOf(items.value);
  confirm.value = true;
}

// a confirmed item changed meanwhile (a group took in more, one member left as another came):
// show the confirmed items as they are now and ask again
function askAgain(): void {
  confirmed.value = confirmedOf(stillConfirmed(confirmed.value, basket.value?.items ?? []));
  confirm.value = true;
  toast(t("删除篮在确认时有了变化，请再确认一次"));
}

async function deleteNow(): Promise<void> {
  const now = basket.value?.items ?? [];
  if (changedSince(confirmed.value, now)) return askAgain();
  const sent = confirmation(confirmed.value, now);
  confirm.value = false;
  // the backend checks the same (the change may not have reached this window yet)
  if ((await basketExecute("delete", sent)) === "changed") askAgain();
}

// what the drawer shows when the button is clicked
async function trashNow(): Promise<void> {
  if ((await basketExecute("trash", confirmedOf(items.value).slice())) === "changed") toast(t("删除篮刚刚有了变化，请看一眼再执行"));
}
</script>

<template>
  <aside class="basket-drawer" :aria-label="t('删除篮')">
    <div class="bk-summary">
      <div class="bk-title">
        <Icon name="basket" />
        <strong>{{ t("删除篮") }}</strong>
        <span class="muted">· {{ itemCount(items.length) }}</span>
        <span class="flex" />
        <button class="icon-btn" :title="t('清空删除篮（不删除任何文件）')" :aria-label="t('清空删除篮')" :disabled="!items.length || busy" @click="basketClear()">
          <Icon name="more" />
        </button>
        <button class="icon-btn" :title="t('关闭删除篮')" :aria-label="t('关闭删除篮')" @click="set({ basketOpen: false })">
          <Icon name="close" />
        </button>
      </div>
      <div class="bk-free">
        <div class="muted small"><span class="bk-legend" /> {{ t("可释放") }}</div>
        <div class="bk-big">
          <span class="mono">{{ big[0] }}</span> <span class="bk-unit">{{ big[1] }}</span>
        </div>
      </div>
      <div v-if="cap" class="bk-cap">
        <div class="bk-bar" aria-hidden="true">
          <span class="used" :style="{ width: cap.used }" />
          <span class="queued" :style="{ width: cap.queued }" />
          <span class="free" :style="{ width: cap.free }" />
        </div>
        <div class="row-between small">
          <span>
            {{ t("{volume} 可用 {size}", { volume: cap.v.mount_point, size: fmtSize(cap.v.free) }) }} › <strong>{{ fmtSize(cap.v.free + cap.v.freeable) }}</strong>
          </span>
          <span class="muted">{{ t("共 {size}", { size: fmtSize(cap.v.total) }) }}</span>
        </div>
      </div>
    </div>
    <div class="bk-listhead">
      <span>{{ t("待删除") }}</span>
      <select v-model="sort" class="bk-sort" :aria-label="t('排序')">
        <option v-for="k in BASKET_SORTS" :key="k" :value="k">{{ basketSortLabel(k) }}</option>
      </select>
    </div>
    <div class="bk-list" role="list">
      <div v-if="items.length === 0" class="muted bk-empty">{{ t("在树图上右键「加入删除篮」，或选中后按 Delete") }}</div>
      <div v-for="it in items" :key="it.id" :class="['bk-row', { blocked: it.status !== 'ready' }]" role="listitem">
        <div class="bk-row-main">
          <span :class="['bk-icon', it.kind]">
            <Icon :name="it.kind === 'dir' ? 'folder' : it.kind === 'group' ? 'layers' : 'file'" :size="15" />
          </span>
          <span class="bk-text">
            <span class="bk-name" :title="it.path"
              >{{ it.kind === "group" ? t("{n} 个小项", { n: fmtInt(it.members) }) : it.name }}<span v-if="it.tag" :class="['tag-mini', it.tag]"
            /></span>
            <span class="bk-path mono">{{ parentOf(it.path, it.kind === "group" ? undefined : it.name) }}</span>
          </span>
          <span class="bk-size">
            <span class="mono">{{ fmtSize(it.alloc) }}</span>
            <span class="muted small">{{ Math.round((total ? it.alloc / total : 0) * 100) }}%</span>
          </span>
          <button class="icon-btn small" :title="t('从删除篮移除')" :aria-label="t('从删除篮移除 {name}', { name: it.name })" @click="basketRemove([it.id])">
            <Icon name="close" :size="14" />
          </button>
        </div>
        <div v-if="it.status !== 'ready'" class="bk-warn"><Icon name="warning" :size="13" /> {{ it.note || statusText(it.status) }}</div>
      </div>
    </div>
    <div v-if="items.length > 0" class="bk-total">
      <span>{{ plural(items.length, t("合计 {n} 项", { n: fmtInt(items.length) }), "Total: 1 item") }}</span>
      <span class="mono">{{ fmtSize(total) }}</span>
    </div>
    <div v-if="items.length > 0" class="bk-note">
      <Icon name="info" :size="14" />
      <div>
        <div>{{ t("移到回收站后，空间要等清空回收站才会释放") }}</div>
        <div class="muted small">{{ trashNote }}</div>
      </div>
    </div>
    <span class="flex" />
    <div v-if="remote && items.length > 0" class="bk-remote" role="note">
      <Icon name="globe" :size="14" />
      <div>{{ t("这些文件在 {host} 上：移到回收站和永久删除都作用于那台电脑，撤销的 5 秒也照常", { host: serverName() }) }}</div>
    </div>
    <div class="bk-actions">
      <div v-if="blocked.length > 0" class="bk-estimate warn-text">
        <Icon name="warning" :size="14" />
        {{ plural(blocked.length, t("{n} 项现在不能删除，执行时会跳过", { n: fmtInt(blocked.length) }), "1 item can't be deleted now and will be skipped.") }}
      </div>
      <div class="bk-buttons">
        <button class="primary flex" :disabled="!items.length || busy" @click="trashNow()"><Icon name="basket" :size="14" /> {{ t("移到回收站") }}</button>
        <button class="danger-btn" :disabled="!items.length || busy" @click="askDelete()">{{ t("永久删除…") }}</button>
      </div>
    </div>
    <div v-if="confirm" class="modal-back" role="presentation" @mousedown="confirm = false">
      <div class="modal" role="alertdialog" aria-modal="true" aria-labelledby="bk-confirm" @mousedown.stop>
        <h3 id="bk-confirm">
          {{ plural(asked.length, t("永久删除 {n} 项（{size}）？", { n: fmtInt(asked.length), size: fmtSize(askedSize) }), "Delete 1 item ({size}) permanently?", { size: fmtSize(askedSize) }) }}
        </h3>
        <p class="muted">{{ t("不进回收站，无法从回收站还原。执行前有 5 秒可以撤销。") }}</p>
        <p v-if="remote" class="remote-line">
          <Icon name="globe" :size="13" />
          {{ t("删除的是 {host} 上的文件", { host: serverName() }) }}
        </p>
        <div class="modal-buttons">
          <button ref="cancelBtn" class="ghost" @click="confirm = false">{{ t("取消") }}</button>
          <button class="danger-btn" @click="deleteNow">{{ t("永久删除") }}</button>
        </div>
      </div>
    </div>
  </aside>
</template>
