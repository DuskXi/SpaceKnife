<!-- macOS without Full Disk Access (spaceknife.privacy): before a scan, grant it once in System
     Settings, leave the consent-guarded places out, or let macOS ask for each of them. -->
<script setup lang="ts">
import { ref, watch } from "vue";

import { t } from "@/lib/i18n";
import { answerProtectedPlaces, type ProtectedChoice } from "@/state/actions";
import { useApp } from "@/state/store";

const open = useApp((s) => s.fdaPrompt);
const remember = ref(false);
// the settings button takes the focus when the dialog opens (React's autoFocus)
const primary = ref<HTMLButtonElement | null>(null);
watch(open, (now) => now && primary.value?.focus(), { flush: "post", immediate: true });

function answer(choice: ProtectedChoice): void {
  answerProtectedPlaces(choice, remember.value);
  remember.value = false;
}
</script>

<template>
  <div v-if="open" class="modal-back" role="presentation" @mousedown="answer('cancel')">
    <div class="modal fda-dialog" role="dialog" aria-modal="true" aria-labelledby="fda-title" @mousedown.stop @keydown.esc="answer('cancel')">
      <h3 id="fda-title">{{ t("让 SpaceKnife 一次读到全部文件") }}</h3>
      <p class="muted">{{ t("macOS 会为桌面、文稿、下载、iCloud 云盘和其他云盘、照片图库以及其他应用的数据分别询问权限，扫描时会一个接一个弹窗。") }}</p>
      <p class="muted">{{ t("开启「完全磁盘访问权限」可以一次授权全部：在系统设置的列表里打开 SpaceKnife 的开关（没有就点 + 添加），再按提示重新打开 SpaceKnife。") }}</p>
      <!-- the grant follows the app's signature, which changes with every update while builds are
           ad-hoc signed: the switch stays on and grants nothing -->
      <p class="muted">{{ t("开关已经打开却不起作用（常见于更新之后）：在列表里选中 SpaceKnife，点 − 删掉，再点 + 重新添加；或者把开关关掉再打开。然后重新打开 SpaceKnife。") }}</p>
      <label class="export-check">
        <input v-model="remember" type="checkbox" />
        {{ t("记住「跳过」或「逐个询问」的选择（可在设置 › 扫描范围里修改）") }}
      </label>
      <div class="modal-buttons">
        <button class="ghost" @click="answer('allow')">{{ t("照常扫描，逐个询问") }}</button>
        <button class="ghost" @click="answer('skip')">{{ t("跳过受保护的位置") }}</button>
        <button ref="primary" class="primary" @click="answer('settings')">{{ t("打开系统设置") }}</button>
      </div>
    </div>
  </div>
</template>
