<!-- One row of the command palette (Palette.vue): a filter, a path to jump to or scan, a recent entry,
     a command or a path found in the scan. Matched characters in bold accent. -->
<script setup lang="ts">
import { computed } from "vue";

import Icon from "@/components/ui/Icon.vue";
import Kbd from "@/components/ui/Kbd.vue";
import { colourSegments, diagnosticText } from "@/lib/filterText";
import { fmtInt, fmtKeys, fmtSize, middlePath } from "@/lib/format";
import { t } from "@/lib/i18n";
import type { IconName } from "@/lib/icons";
import { hitParts, type PaletteLine } from "@/lib/palette";
import type { PalettePreview } from "@/protocol/generated";

const props = defineProps<{ id: string; line: PaletteLine; on: boolean; previews: Record<string, PalettePreview> }>();
defineEmits<{ hover: []; run: [] }>();

const stats = (expr: string): string => {
  const pv = props.previews[expr];
  return pv ? (pv.valid ? t("{n} 项 · {size}", { n: fmtInt(pv.files), size: fmtSize(pv.alloc) }) : "") : t("搜索中…");
};

/** The icon, the accessible label and whether it is an invalid filter. */
const meta = computed<{ icon: IconName; label: string; error: boolean }>(() => {
  const row = props.line.row;
  switch (row.kind) {
    case "filter": {
      const e = row.d.diagnostics[0];
      return {
        icon: "filter",
        error: !row.d.valid && !!e,
        label: e ? t("过滤 {text}，错误：{error}", { text: row.d.text, error: diagnosticText(e) }) : t("过滤 {text}", { text: row.d.text }),
      };
    }
    case "goto":
      return { icon: row.d.is_dir ? "folder" : "file", error: false, label: t("跳转到 {path}", { path: row.d.text }) };
    case "recent": {
      const r = row.r;
      return {
        icon: r.kind === "filter" ? "filter" : r.is_dir ? "folder" : "file",
        error: false,
        label: r.kind === "filter" ? t("过滤 {text}", { text: r.text }) : t("跳转到 {path}", { path: r.text }),
      };
    }
    case "command": {
      const c = row.c;
      return { icon: (c.icon as IconName) || "command", error: false, label: [c.name, c.status, c.keys[0] ? fmtKeys(c.keys[0]) : ""].filter(Boolean).join(t("，")) };
    }
    case "path": {
      const p = row.p;
      return { icon: p.is_dir ? "folder" : "file", error: false, label: [p.name, p.parent, fmtSize(p.alloc)].join(t("，")) };
    }
  }
  return { icon: "command", error: false, label: "" };
});
</script>

<template>
  <div
    :id="id"
    :class="['pal-row', { on, disabled: !line.enabled, error: meta.error }]"
    role="option"
    :aria-selected="on"
    :aria-disabled="!line.enabled"
    :aria-label="meta.label"
    @mousemove="$emit('hover')"
    @mousedown.prevent
    @click="$emit('run')"
  >
    <span class="pal-icon">
      <Icon :name="meta.icon" :size="16" />
    </span>
    <template v-if="line.row.kind === 'filter'">
      <span class="pal-main">
        <span class="pal-verb">{{ t("过滤") }}</span>
        <code class="pal-expr"><template v-for="s in colourSegments(line.row.d.text, line.row.d)" :key="s.start"><span v-if="s.cls !== null" :class="s.cls">{{ s.text }}</span><template v-else>{{ s.text }}</template></template></code>
      </span>
      <span v-if="meta.error" class="pal-sub"><span class="danger">{{ diagnosticText(line.row.d.diagnostics[0]!) }}</span></span>
      <span class="pal-right"><span v-if="!meta.error" class="mono">{{ stats(line.row.d.text) }}</span></span>
    </template>
    <template v-else-if="line.row.kind === 'goto'">
      <span class="pal-main">
        <span class="pal-verb">{{ t("跳转到") }}</span>
        <span class="mono pal-path">{{ line.row.d.text }}</span>
      </span>
      <span v-if="line.row.d.state === 'outside'" class="pal-sub"><span>{{ t("不在当前扫描范围内") }}</span></span>
      <span v-else-if="line.row.d.state !== 'in_scan'" class="pal-sub"><span>{{ t("路径不存在") }}</span></span>
      <span class="pal-right">
        <span v-if="line.row.d.state === 'in_scan'" class="mono">{{ fmtSize(line.row.d.alloc ?? 0) }}</span>
        <span v-else-if="line.row.d.state === 'outside'" class="pal-action">{{ t("扫描此位置") }}</span>
      </span>
    </template>
    <template v-else-if="line.row.kind === 'recent' && line.row.r.kind === 'filter'">
      <span class="pal-main">
        <span class="pal-verb">{{ t("过滤") }}</span>
        <code class="pal-expr"><template v-for="s in colourSegments(line.row.r.text, { text: line.row.r.text, tokens: line.row.r.tokens, diagnostics: [] })" :key="s.start"><span v-if="s.cls !== null" :class="s.cls">{{ s.text }}</span><template v-else>{{ s.text }}</template></template></code>
      </span>
      <span class="pal-right"><span class="mono">{{ stats(line.row.r.text) }}</span></span>
    </template>
    <template v-else-if="line.row.kind === 'recent'">
      <span class="pal-main">
        <span class="pal-verb">{{ t("跳转到") }}</span>
        <span class="mono pal-path"><template v-for="(h, i) in hitParts(line.row.r.text, line.row.r.hits)" :key="i"><b v-if="h.hit" class="pal-hit">{{ h.text }}</b><template v-else>{{ h.text }}</template></template></span>
      </span>
      <span v-if="!line.row.r.exists" class="pal-sub"><span>{{ t("已不存在") }}</span></span>
      <span class="pal-right"><span v-if="line.row.r.exists" class="mono">{{ fmtSize(line.row.r.alloc ?? 0) }}</span></span>
    </template>
    <template v-else-if="line.row.kind === 'command'">
      <span class="pal-main"><template v-for="(h, i) in hitParts(line.row.c.name, line.row.c.hits)" :key="i"><b v-if="h.hit" class="pal-hit">{{ h.text }}</b><template v-else>{{ h.text }}</template></template></span>
      <span v-if="line.row.c.status" class="pal-sub"><span>{{ line.row.c.status }}</span></span>
      <span class="pal-right"><Kbd v-if="line.row.c.keys[0]">{{ fmtKeys(line.row.c.keys[0]) }}</Kbd></span>
    </template>
    <template v-else>
      <span class="pal-main"><template v-for="(h, i) in hitParts(line.row.p.name, line.row.p.hits)" :key="i"><b v-if="h.hit" class="pal-hit">{{ h.text }}</b><template v-else>{{ h.text }}</template></template></span>
      <span class="pal-sub">
        <span class="mono" :title="line.row.p.parent">{{ middlePath(line.row.p.parent) }}</span>
      </span>
      <span class="pal-right"><span class="mono">{{ fmtSize(line.row.p.alloc) }}</span></span>
    </template>
  </div>
</template>
