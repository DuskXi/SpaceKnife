// The help tab's links and words (pages/HelpPage.vue, the toolbar): open a link outside the app,
// the title of the page shown, and the search words marked in a result.

import { docLang, OVERVIEW, page } from "@/help/docs";
import { t } from "@/lib/i18n";
import { projectPage } from "@/shared/links";
import { toast } from "@/state/store";

/** Open a link in the browser: the project's pages and the website open, others are copied. */
export function openLink(url: string): void {
  if (projectPage(url)) window.sk.shell.openExternal(url);
  else {
    window.sk.shell.copyText(url);
    toast(t("链接已复制：{url}", { url }));
  }
}

/** The title of the help page shown (the toolbar's breadcrumb). */
export function helpTitle(lang: string, id: string): string {
  return page(docLang(lang), id)?.title ?? page(docLang(lang), OVERVIEW)?.title ?? "";
}

/** A text cut where the search words are found: `mark` pieces are the matches. */
export function markWords(text: string, words: string[]): { text: string; mark: boolean }[] {
  const lower = text.toLowerCase();
  const marks: [number, number][] = [];
  for (const w of words) {
    for (let at = lower.indexOf(w); at >= 0 && w; at = lower.indexOf(w, at + w.length)) marks.push([at, at + w.length]);
  }
  if (!marks.length) return [{ text, mark: false }];
  marks.sort((a, b) => a[0] - b[0]);
  const out: { text: string; mark: boolean }[] = [];
  let pos = 0;
  for (const [a, b] of marks) {
    if (b <= pos) continue;
    const from = Math.max(a, pos);
    if (from > pos) out.push({ text: text.slice(pos, from), mark: false });
    out.push({ text: text.slice(from, b), mark: true });
    pos = b;
  }
  if (pos < text.length) out.push({ text: text.slice(pos), mark: false });
  return out;
}
