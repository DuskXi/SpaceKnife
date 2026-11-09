<!-- The help tab: the documentation of the website, bundled with the app (help/docs.ts) and shown in
     the interface language. The table of contents on the left, the search in the toolbar
     (HelpSearch.vue), the page with its own contents beside it when it is long (HelpContents.vue),
     previous / next at its end. -->
<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

import HelpContents from "@/components/pages/HelpContents.vue";
import Icon from "@/components/ui/Icon.vue";
import Kbd from "@/components/ui/Kbd.vue";
import { useReducedMotion } from "@/composables/useMedia";
import { docLang, groupOf, NAV, neighbour, OVERVIEW, page, search, siteUrl } from "@/help/docs";
import { markWords, openLink } from "@/help/links";
import { render } from "@/help/markdown";
import { t } from "@/lib/i18n";
import { HELP_SCROLL_ID, helpGo } from "@/state/help";
import { set, setting, toast, useApp } from "@/state/store";

/** The page's contents beside it from this many headings (## and ###) on. */
const TOC_FROM = 3;

const lang = useApp((s) => docLang(s.lang));
const loc = useApp((s) => s.help);
const nonce = useApp((s) => s.helpNonce);
const query = useApp((s) => s.helpQuery.trim());
const reduced = useReducedMotion(useApp((s) => setting<string>(s, "appearance.reduce_motion", "system")));
const doc = computed(() => page(lang.value, loc.value.page) ?? page(lang.value, OVERVIEW));
const hits = computed(() => (query.value ? search(lang.value, query.value) : null));
const scroller = ref<HTMLDivElement | null>(null);
/** how far the page (not the search results) is scrolled */
let pageScroll = 0;
let last = { key: "", nonce: -1, search: false };

// where the page opens: back in the history, where it was; at a heading a link named; else the
// top. Closing the search returns to where the page was.
function place(): void {
    const el = scroller.value;
    if (!el) return;
    const key = `${lang.value}/${doc.value?.id ?? ""}`;
    const prev = last;
    last = { key, nonce: nonce.value, search: !!hits.value };
    if (hits.value) return void (el.scrollTop = 0);
    const samePage = prev.key === key;
    const smooth = samePage && !prev.search && !reduced.value;
    let top: number;
    if (samePage && prev.nonce === nonce.value) top = pageScroll;
    else if (loc.value.scroll != null) top = loc.value.scroll;
    else if (loc.value.anchor) top = reach(el, loc.value.anchor);
    else top = 0;
    el.scrollTo({ top, behavior: smooth ? "smooth" : "auto" });
    pageScroll = smooth ? top : el.scrollTop;
}
onMounted(place);
watch([nonce, lang, () => doc.value?.id, () => !hits.value], place, { flush: "post" });

// leaving the tab keeps the position: the page comes back where it was
onBeforeUnmount(() => {
  const scroll = pageScroll;
  set((s) => ({ help: { ...s.help, scroll } }));
});

function onScroll(e: Event): void {
  if (!hits.value) pageScroll = (e.currentTarget as HTMLElement).scrollTop;
}

/** Where a heading of the page shown is (the top when there is none); it is marked a moment. */
function reach(el: HTMLElement, id: string): number {
  const target = el.querySelector<HTMLElement>(`[id="${CSS.escape(id)}"]`);
  if (!target) return 0;
  target.classList.remove("help-reached");
  void target.offsetWidth; // restart the mark when the same heading is reached again
  target.classList.add("help-reached");
  setTimeout(() => target.classList.remove("help-reached"), 1600);
  return Math.max(0, target.getBoundingClientRect().top - el.getBoundingClientRect().top + el.scrollTop - 16);
}

// ------------------------------------------------------------------ table of contents

const found = computed(() => new Set(hits.value?.map((h) => h.page.id)));

// ------------------------------------------------------------------ the page

const rendered = computed(() => (doc.value ? render(doc.value.body, lang.value, doc.value.id, t("复制")) : null));
const toc = computed(() => rendered.value?.headings.filter((h) => h.depth === 2 || h.depth === 3) ?? []);
const group = computed(() => (doc.value ? groupOf(doc.value.id) : undefined));
const pager = computed(() => {
  const id = doc.value?.id ?? OVERVIEW;
  return ([-1, 1] as const).map((dir) => {
    const to = neighbour(id, dir);
    return { dir, id: to, page: to ? page(lang.value, to) : undefined };
  });
});

function onBodyClick(e: MouseEvent): void {
  const el = e.target as HTMLElement;
  const copy = el.closest<HTMLElement>("[data-copy]");
  if (copy) {
    const code = copy.parentElement?.querySelector("pre")?.textContent ?? "";
    window.sk.shell.copyText(code.replace(/\n$/, ""));
    toast(t("已复制"));
    return;
  }
  const a = el.closest<HTMLAnchorElement>("a");
  if (!a) return;
  e.preventDefault(); // nothing navigates the window
  if (a.dataset.doc) helpGo(a.dataset.doc, a.dataset.anchor ?? null);
  else if (a.dataset.external !== undefined) openLink(a.href);
}
function onAuxClick(e: MouseEvent): void {
  if ((e.target as HTMLElement).closest("a")) e.preventDefault();
}

// ------------------------------------------------------------------ search

const words = computed(() => query.value.toLowerCase().split(/\s+/).filter(Boolean));
// the key cap sits inside the sentence: split the translated text around {key}
const resultsLine = computed(() => t("{n} 个页面 · 按 {key} 清空", { n: hits.value?.length ?? 0 }).split("{key}"));
</script>

<template>
  <div v-if="!doc" class="help muted">{{ t("帮助页面没有打包进这个版本") }}</div>
  <div v-else class="help">
    <nav class="settings-nav help-nav" :aria-label="t('帮助目录')">
      <div class="settings-nav-list">
        <div v-for="g in NAV" :key="g.id" class="settings-nav-group" role="group" :aria-label="g.label[lang]">
          <div class="help-nav-label">{{ g.label[lang] }}</div>
          <template v-for="id in g.items" :key="id">
            <button
              v-if="page(lang, id)"
              :class="['settings-nav-item help-nav-item', { on: !hits && id === doc.id, faded: hits && !found.has(id) }]"
              :aria-current="!hits && id === doc.id ? 'page' : undefined"
              @click="helpGo(id)"
            >
              <span class="flex">{{ page(lang, id)!.title }}</span>
            </button>
          </template>
        </div>
      </div>
      <button class="settings-nav-file" @click="openLink(siteUrl(lang, doc.id))">
        <Icon name="reveal" :size="14" />
        {{ t("在网站上查看") }}
      </button>
    </nav>
    <div :id="HELP_SCROLL_ID" ref="scroller" class="help-main" @scroll="onScroll">
      <div v-if="hits" class="help-layout">
        <div class="help-article">
          <header class="help-head">
            <h1>{{ t("搜索结果") }}</h1>
            <p class="help-lede">{{ resultsLine[0] }}<Kbd>Esc</Kbd>{{ resultsLine[1] }}</p>
          </header>
          <div v-if="!hits.length" class="settings-empty muted">{{ t("没有找到与「{query}」相关的页面", { query }) }}</div>
          <div v-else class="help-results">
            <section v-for="h in hits" :key="h.page.id" class="help-result" :aria-label="h.page.title">
              <button class="help-result-page" @click="helpGo(h.page.id)">
                <span class="help-result-title">
                  <template v-for="(m, i) in markWords(h.page.title, words)" :key="i"><mark v-if="m.mark">{{ m.text }}</mark><template v-else>{{ m.text }}</template></template>
                </span>
                <span class="muted small">{{ groupOf(h.page.id)?.label[lang] }}</span>
              </button>
              <p v-if="h.page.description" class="help-result-desc">
                <template v-for="(m, i) in markWords(h.page.description, words)" :key="i"><mark v-if="m.mark">{{ m.text }}</mark><template v-else>{{ m.text }}</template></template>
              </p>
              <button v-for="x in h.headings.slice(0, 6)" :key="x.id" class="help-result-heading" @click="helpGo(h.page.id, x.id)">
                <Icon name="chevronRight" :size="12" />
                <template v-for="(m, i) in markWords(x.text, words)" :key="i"><mark v-if="m.mark">{{ m.text }}</mark><template v-else>{{ m.text }}</template></template>
              </button>
            </section>
          </div>
        </div>
      </div>
      <div v-else class="help-layout">
        <article class="help-article" :lang="lang === 'zh' ? 'zh-CN' : 'en'">
          <header class="help-head">
            <div class="help-kicker">{{ group?.label[lang] }}</div>
            <h1>{{ doc.title }}</h1>
            <p v-if="doc.description" class="help-lede">{{ doc.description }}</p>
          </header>
          <!-- the page rendered by help/markdown.ts (our markup) -->
          <div class="help-body" @click="onBodyClick" @auxclick="onAuxClick" v-html="rendered?.html" />
          <nav class="help-pager" :aria-label="t('上一页与下一页')">
            <template v-for="n in pager" :key="n.dir">
              <button v-if="n.id && n.page" :class="['help-pager-link', { next: n.dir > 0 }]" @click="helpGo(n.id)">
                <span class="muted small">{{ n.dir > 0 ? t("下一页") : t("上一页") }}</span>
                <span class="help-pager-title">
                  <Icon v-if="n.dir < 0" name="back" :size="14" />
                  {{ n.page.title }}
                  <Icon v-if="n.dir > 0" name="forward" :size="14" />
                </span>
              </button>
              <span v-else />
            </template>
          </nav>
        </article>
        <HelpContents v-if="toc.length >= TOC_FROM" :doc-id="doc.id" :toc="toc" :scroller="scroller" />
      </div>
    </div>
  </div>
</template>
