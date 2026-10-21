// The help tab: the documentation of the website, bundled with the app (help/docs.ts) and shown in
// the interface language. The table of contents on the left, the search in the toolbar (HelpSearch),
// the page with its own contents beside it when it is long, previous / next at its end.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { projectPage } from "../../../shared/links";
import { type DocLang, type DocPage, docLang, groupOf, NAV, neighbour, OVERVIEW, page, type SearchHit, search, siteUrl } from "@/help/docs";
import { type Heading, render } from "@/help/markdown";
import { t } from "@/lib/i18n";
import { HELP_SCROLL_ID, helpGo } from "@/state/help";
import { get, set, setting, toast, useApp } from "@/state/store";
import { Icon, Kbd } from "@/ui/Icon";
import { useReducedMotion } from "@/visual/scheduler";

/** The page's contents beside it from this many headings (## and ###) on. */
const TOC_FROM = 3;
/** A heading counts as reached while it is this far below the top of the page's scroll box. */
const SPY_OFFSET = 96;

export function HelpPage() {
  const lang = docLang(useApp((s) => s.lang));
  const loc = useApp((s) => s.help);
  const nonce = useApp((s) => s.helpNonce);
  const query = useApp((s) => s.helpQuery).trim();
  const reduced = useReducedMotion(useApp((s) => setting<string>(s, "appearance.reduce_motion", "system")));
  const doc = page(lang, loc.page) ?? page(lang, OVERVIEW);
  const hits = useMemo(() => (query ? search(lang, query) : null), [lang, query]);
  const scroller = useRef<HTMLDivElement>(null);
  /** how far the page (not the search results) is scrolled */
  const pageScroll = useRef(0);
  const last = useRef({ key: "", nonce: -1, search: false });

  // where the page opens: back in the history, where it was; at a heading a link named; else the
  // top. Closing the search returns to where the page was.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const key = `${lang}/${doc?.id ?? ""}`;
    const prev = last.current;
    last.current = { key, nonce, search: !!hits };
    if (hits) return void (el.scrollTop = 0);
    const samePage = prev.key === key;
    const smooth = samePage && !prev.search && !reduced;
    let top: number;
    if (samePage && prev.nonce === nonce) top = pageScroll.current;
    else if (loc.scroll != null) top = loc.scroll;
    else if (loc.anchor) top = reach(el, loc.anchor);
    else top = 0;
    el.scrollTo({ top, behavior: smooth ? "smooth" : "auto" });
    pageScroll.current = smooth ? top : el.scrollTop;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nonce, lang, doc?.id, !hits]);

  // leaving the tab keeps the position: the page comes back where it was
  useEffect(
    () => () => {
      const scroll = pageScroll.current;
      set((s) => ({ help: { ...s.help, scroll } }));
    },
    [],
  );

  if (!doc) return <div className="help muted">{t("帮助页面没有打包进这个版本")}</div>;
  return (
    <div className="help">
      <HelpNav lang={lang} current={doc.id} hits={hits} />
      <div className="help-main" id={HELP_SCROLL_ID} ref={scroller} onScroll={(e) => !hits && (pageScroll.current = e.currentTarget.scrollTop)}>
        {hits ? <Results hits={hits} query={query} lang={lang} /> : <Article doc={doc} lang={lang} scroller={scroller} />}
      </div>
    </div>
  );
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

/** Open a link in the browser: the project's pages and the website open, others are copied. */
export function openLink(url: string): void {
  if (projectPage(url)) window.sk.shell.openExternal(url);
  else {
    window.sk.shell.copyText(url);
    toast(t("链接已复制：{url}", { url }));
  }
}

// ------------------------------------------------------------------ table of contents

function HelpNav({ lang, current, hits }: { lang: DocLang; current: string; hits: SearchHit[] | null }) {
  const found = useMemo(() => new Set(hits?.map((h) => h.page.id)), [hits]);
  return (
    <nav className="settings-nav help-nav" aria-label={t("帮助目录")}>
      <div className="settings-nav-list">
        {NAV.map((g) => (
          <div key={g.id} className="settings-nav-group" role="group" aria-label={g.label[lang]}>
            <div className="help-nav-label">{g.label[lang]}</div>
            {g.items.map((id) => {
              const p = page(lang, id);
              if (!p) return null;
              const on = !hits && id === current;
              return (
                <button key={id} className={`settings-nav-item help-nav-item${on ? " on" : ""}${hits && !found.has(id) ? " faded" : ""}`} aria-current={on ? "page" : undefined} onClick={() => helpGo(id)}>
                  <span className="flex">{p.title}</span>
                </button>
              );
            })}
          </div>
        ))}
      </div>
      <button className="settings-nav-file" onClick={() => openLink(siteUrl(lang, current))}>
        <Icon name="reveal" size={14} />
        {t("在网站上查看")}
      </button>
    </nav>
  );
}

// ------------------------------------------------------------------ the page

function Article({ doc, lang, scroller }: { doc: DocPage; lang: DocLang; scroller: React.RefObject<HTMLDivElement | null> }) {
  const copyLabel = t("复制");
  const rendered = useMemo(() => render(doc.body, lang, doc.id, copyLabel), [doc, lang, copyLabel]);
  const toc = useMemo(() => rendered.headings.filter((h) => h.depth === 2 || h.depth === 3), [rendered]);
  const group = groupOf(doc.id);
  const prev = neighbour(doc.id, -1);
  const next = neighbour(doc.id, 1);

  const onClick = (e: React.MouseEvent) => {
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
  };

  return (
    <div className="help-layout">
      <article className="help-article" lang={lang === "zh" ? "zh-CN" : "en"}>
        <header className="help-head">
          <div className="help-kicker">{group?.label[lang]}</div>
          <h1>{doc.title}</h1>
          {doc.description && <p className="help-lede">{doc.description}</p>}
        </header>
        <div className="help-body" onClick={onClick} onAuxClick={(e) => (e.target as HTMLElement).closest("a") && e.preventDefault()} dangerouslySetInnerHTML={{ __html: rendered.html }} />
        <nav className="help-pager" aria-label={t("上一页与下一页")}>
          {prev ? <PagerLink id={prev} lang={lang} dir={-1} /> : <span />}
          {next ? <PagerLink id={next} lang={lang} dir={1} /> : <span />}
        </nav>
      </article>
      {toc.length >= TOC_FROM && <Contents doc={doc} toc={toc} scroller={scroller} />}
    </div>
  );
}

function PagerLink({ id, lang, dir }: { id: string; lang: DocLang; dir: 1 | -1 }) {
  const p = page(lang, id);
  if (!p) return <span />;
  return (
    <button className={`help-pager-link${dir > 0 ? " next" : ""}`} onClick={() => helpGo(id)}>
      <span className="muted small">{dir > 0 ? t("下一页") : t("上一页")}</span>
      <span className="help-pager-title">
        {dir < 0 && <Icon name="back" size={14} />}
        {p.title}
        {dir > 0 && <Icon name="forward" size={14} />}
      </span>
    </button>
  );
}

/** 「本页内容」: the page's ## and ### headings; the one being read is marked. */
function Contents({ doc, toc, scroller }: { doc: DocPage; toc: Heading[]; scroller: React.RefObject<HTMLDivElement | null> }) {
  const [current, setCurrent] = useState<string | null>(null);
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    let raf = 0;
    const spy = () => {
      raf = 0;
      const top = el.getBoundingClientRect().top + SPY_OFFSET;
      let at: string | null = null;
      for (const h of toc) {
        const node = el.querySelector<HTMLElement>(`[id="${CSS.escape(h.id)}"]`);
        if (node && node.getBoundingClientRect().top <= top) at = h.id;
        else if (node) break;
      }
      // at the very end the last heading may never reach the top: it is the one being read
      if (el.scrollTop + el.clientHeight >= el.scrollHeight - 2) at = toc[toc.length - 1]!.id;
      setCurrent(at);
    };
    const onScroll = () => void (raf ||= requestAnimationFrame(spy));
    spy();
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      el.removeEventListener("scroll", onScroll);
      cancelAnimationFrame(raf);
    };
  }, [toc, scroller]);
  return (
    <aside className="help-toc" aria-label={t("本页内容")}>
      <div className="help-toc-title">{t("本页内容")}</div>
      {toc.map((h) => (
        <button key={h.id} className={`help-toc-item d${h.depth}${current === h.id ? " on" : ""}`} aria-current={current === h.id ? "location" : undefined} onClick={() => helpGo(doc.id, h.id)}>
          {h.text}
        </button>
      ))}
    </aside>
  );
}

// ------------------------------------------------------------------ search

function Results({ hits, query, lang }: { hits: SearchHit[]; query: string; lang: DocLang }) {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  // the key cap sits inside the sentence: split the translated text around {key}
  const [before, after] = t("{n} 个页面 · 按 {key} 清空", { n: hits.length }).split("{key}");
  return (
    <div className="help-layout">
      <div className="help-article">
        <header className="help-head">
          <h1>{t("搜索结果")}</h1>
          <p className="help-lede">
            {before}
            <Kbd>Esc</Kbd>
            {after}
          </p>
        </header>
        {!hits.length ? (
          <div className="settings-empty muted">{t("没有找到与「{query}」相关的页面", { query })}</div>
        ) : (
          <div className="help-results">
            {hits.map((h) => (
              <section key={h.page.id} className="help-result" aria-label={h.page.title}>
                <button className="help-result-page" onClick={() => helpGo(h.page.id)}>
                  <span className="help-result-title">
                    <Mark text={h.page.title} words={words} />
                  </span>
                  <span className="muted small">{groupOf(h.page.id)?.label[lang]}</span>
                </button>
                {h.page.description && (
                  <p className="help-result-desc">
                    <Mark text={h.page.description} words={words} />
                  </p>
                )}
                {h.headings.slice(0, 6).map((x) => (
                  <button key={x.id} className="help-result-heading" onClick={() => helpGo(h.page.id, x.id)}>
                    <Icon name="chevronRight" size={12} />
                    <Mark text={x.text} words={words} />
                  </button>
                ))}
              </section>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function Mark({ text, words }: { text: string; words: string[] }) {
  const lower = text.toLowerCase();
  const marks: [number, number][] = [];
  for (const w of words) {
    for (let at = lower.indexOf(w); at >= 0 && w; at = lower.indexOf(w, at + w.length)) marks.push([at, at + w.length]);
  }
  if (!marks.length) return <>{text}</>;
  marks.sort((a, b) => a[0] - b[0]);
  const out: React.ReactNode[] = [];
  let pos = 0;
  for (const [a, b] of marks) {
    if (b <= pos) continue;
    const from = Math.max(a, pos);
    if (from > pos) out.push(text.slice(pos, from));
    out.push(<mark key={from}>{text.slice(from, b)}</mark>);
    pos = b;
  }
  out.push(text.slice(pos));
  return <>{out}</>;
}

/** Toolbar search box of the help tab: titles, descriptions and headings. Enter opens the best hit. */
export function HelpSearch() {
  const query = useApp((s) => s.helpQuery);
  return (
    <div className="filter settings-search">
      <span className="filter-icon">
        <Icon name="search" />
      </span>
      <input
        id="sk-help-search"
        className="filter-input plain"
        placeholder={t("搜索帮助")}
        aria-label={t("搜索帮助")}
        value={query}
        spellCheck={false}
        onChange={(e) => set({ helpQuery: e.target.value })}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Escape") {
            if (query) set({ helpQuery: "" });
            else (e.target as HTMLInputElement).blur();
          } else if (e.key === "Enter") {
            const q = query.trim();
            const best = q ? search(docLang(get().lang), q)[0] : undefined;
            if (!best) return;
            // a word only a heading has: that heading
            const inTitle = q.toLowerCase().split(/\s+/).every((w) => best.page.title.toLowerCase().includes(w));
            helpGo(best.page.id, inTitle ? null : (best.headings[0]?.id ?? null));
          }
        }}
      />
      <kbd className="kbd filter-kbd">/</kbd>
    </div>
  );
}

/** The title of the help page shown (the toolbar's breadcrumb). */
export function helpTitle(lang: string, id: string): string {
  return page(docLang(lang), id)?.title ?? page(docLang(lang), OVERVIEW)?.title ?? "";
}
