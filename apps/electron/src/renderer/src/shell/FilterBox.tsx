// Filter box (UI-MAIN-17, W3): live syntax colouring, completion panel, error bar, cheat-sheet row.

import { useEffect, useRef, useState } from "react";

import type { FilterDiagnostic, FilterParse } from "@/protocol/generated";
import { editFilter, focusPane } from "@/state/actions";
import { useApp } from "@/state/store";
import { Icon } from "@/ui/Icon";
import { classHexNow } from "@/lib/classes";
import { t } from "@/lib/i18n";

// every text is looked up when shown: the interface language can change at any time
const contextLabel = (context: string): string =>
  (({ keyword: t("关键字"), class: t("文件类别"), tag: t("标签"), attr: t("属性"), unit: t("单位"), prefix: t("条件开头") }) as Record<string, string>)[context] ?? context;
const BADGE: Record<string, string> = { keyword: ":", class: ":class:", tag: ":tag:", attr: ":attr:" };

/** File attribute names (also the condition capsules of 「匹配结果」). */
export const attrLabel = (a: string): string =>
  (({
    archive: t("归档"),
    system: t("系统"),
    readonly: t("只读"),
    hidden: t("隐藏"),
    compressed: t("压缩"),
    encrypted: t("加密"),
    offline: t("脱机"),
    temporary: t("临时"),
    notindexed: t("不索引"),
    sparse: t("稀疏"),
    ads: t("备用数据流"),
  }) as Record<string, string>)[a] ?? a;
const ATTR_LETTER: Record<string, string> = {
  archive: "a",
  system: "s",
  readonly: "r",
  hidden: "h",
  compressed: "c",
  encrypted: "e",
  offline: "o",
  temporary: "t",
  notindexed: "ni",
  sparse: "sp",
  ads: "ads",
};
const unitLabel = (unit: string): string =>
  (({
    b: t("字节"),
    kb: "KB",
    mb: "MB",
    gb: "GB",
    tb: "TB",
    pb: "PB",
    s: t("秒"),
    min: t("分钟"),
    h: t("小时"),
    d: t("天"),
    w: t("周"),
    month: t("月"),
    y: t("年"),
  }) as Record<string, string>)[unit] ?? "";
/** The message of a filter diagnostic code (the backend sends codes, the wording is here). */
export const errorText = (code: string): string =>
  (({
    incomplete: t("条件还没写完"),
    unterminated_quote: t("引号没有闭合"),
    text_after_quote: t("闭合引号后面还有内容"),
    bad_escape: t("引号内末尾的反斜杠"),
    backslash_in_mask: t("掩码里不能有反斜杠，需要时用引号"),
    bad_char: t("掩码里不能有 | < >，需要时用引号"),
    unknown_keyword: t("未知关键字"),
    unknown_class: t("未知类别"),
    unknown_tag: t("未知标签颜色"),
    unknown_attr: t("未知属性"),
    bad_list: t("列表格式不对"),
    unknown_prefix: t("未知前缀"),
    bad_operator: t("无法识别的运算符，可用"),
    bad_number: t("数字不合法"),
    missing_unit: t("缺少单位，可用"),
    unknown_unit: t("无法识别的单位，可用"),
    unit_mismatch: t("前缀与单位不同类，可用"),
    out_of_range: t("数值超出上限"),
    unsupported: t("此平台不记录"),
  }) as Record<string, string>)[code] ?? code;

/** One filter box per view (split panes each have their own, W8). */
export const filterInputId = (viewId: string | null) => `sk-filter-${viewId ?? "none"}`;

/** Focus the filter of the focused view (/ and Ctrl+F, 「聚焦过滤」). */
export function focusFilter(viewId: string | null): void {
  (document.getElementById(filterInputId(viewId)) as HTMLInputElement | null)?.focus();
}

export function FilterBox({ viewId, disabled, pane }: { viewId: string | null; disabled?: boolean; pane?: string }) {
  const input = useRef<HTMLInputElement>(null);
  const mirror = useRef<HTMLPreElement>(null);
  const text = useApp((s) => (viewId ? (s.views[viewId]?.filterText ?? "") : ""));
  const parse = useApp((s) => (viewId ? (s.views[viewId]?.parse ?? null) : null));
  const vocab = useApp((s) => s.vocab);
  const classes = vocab?.classes ?? [];
  const [focused, setFocused] = useState(false);
  const [active, setActive] = useState(0);
  const [closed, setClosed] = useState(false);
  const inputId = filterInputId(viewId);

  useEffect(() => {
    if (input.current && input.current.value !== text) input.current.value = text;
  }, [text, viewId]);

  const completion = parse?.completion ?? null;
  const candidates = completion?.candidates ?? [];
  const exact = candidates.length === 1 && candidates[0] === completion?.typed;
  const errors = (parse?.diagnostics ?? []).filter((d) => d.severity === "error");
  const panelOpen = focused && !closed && !!parse && (candidates.length > 0 ? !exact : errors.length > 0);
  const badge = completion ? BADGE[completion.context] : undefined;

  const onInput = (immediate = false) => {
    if (!viewId || !input.current) return;
    setClosed(false);
    setActive(0);
    void editFilter(viewId, input.current.value, input.current.selectionStart, immediate);
  };

  const accept = (k: number) => {
    if (!completion || !input.current) return;
    const cand = candidates[k];
    if (cand === undefined) return;
    const v = input.current.value;
    const next = v.slice(0, completion.start) + cand + v.slice(completion.end);
    input.current.value = next;
    const pos = completion.start + cand.length;
    input.current.setSelectionRange(pos, pos);
    onInput(true);
  };

  // only the keys handled here stay here: Ctrl+K, Ctrl+T, Ctrl+Tab… still reach the window
  // (single-key shortcuts are already ignored by App while a text field has the focus)
  const onKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (panelOpen && candidates.length) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        e.stopPropagation();
        setActive((a) => (a + (e.key === "ArrowDown" ? 1 : -1) + candidates.length) % candidates.length);
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        e.stopPropagation();
        accept(active);
        return;
      }
    }
    if (e.key === "Escape" || e.key === "Enter") e.stopPropagation();
    if (e.key === "Escape") {
      e.preventDefault();
      if (panelOpen) setClosed(true);
      else if (input.current?.value) {
        input.current.value = "";
        onInput(true);
      } else input.current?.blur();
    } else if (e.key === "Enter") onInput(true);
  };

  return (
    <div className={`filter${disabled ? " disabled" : ""}${pane ? " pane-filter" : ""}`}>
      <label htmlFor={inputId} className="sr-only">
        {pane ? t("过滤{pane}", { pane }) : t("过滤")}
      </label>
      <span className="filter-icon">
        <Icon name="filter" />
      </span>
      <pre ref={mirror} className="filter-mirror" aria-hidden="true">
        {renderColoured(text, parse)}
      </pre>
      <input
        id={inputId}
        onFocus={() => {
          setFocused(true);
          const st = useApp.getState();
          const tab = st.tabs.find((x) => viewId && x.viewIds.includes(viewId));
          if (tab && viewId && tab.viewIds.length > 1) focusPane(tab.id, tab.viewIds.indexOf(viewId));
        }}
        ref={input}
        className="filter-input"
        spellCheck={false}
        autoComplete="off"
        disabled={disabled || !viewId}
        role="combobox"
        aria-expanded={panelOpen}
        aria-controls={`${inputId}-panel`}
        aria-invalid={errors.length > 0}
        placeholder={disabled ? t("开始扫描后可以过滤") : pane ? t("过滤此窗格") : t("按名称、类型、大小过滤，例如 *.mp4; >1gb; :class:视频")}
        onChange={() => onInput()}
        onKeyDown={onKey}
        onKeyUp={(e) => ["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key) && onInput()}
        onClick={() => onInput()}
        onScroll={(e) => mirror.current && (mirror.current.scrollLeft = e.currentTarget.scrollLeft)}
        onBlur={() => setTimeout(() => setFocused(false), 120)}
      />
      {text ? (
        <button
          className="filter-clear"
          aria-label={pane ? t("清除{pane}过滤", { pane }) : t("清除过滤")}
          title={t("清除过滤（Esc）")}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => {
            if (input.current) input.current.value = "";
            onInput(true);
          }}
        >
          <Icon name="close" size={12} />
        </button>
      ) : (
        !pane && <kbd className="kbd filter-kbd">/</kbd>
      )}
      {panelOpen && (
        <div id={`${inputId}-panel`} className="filter-panel" role="listbox" onMouseDown={(e) => e.preventDefault()}>
          {candidates.length > 0 && completion && (
            <>
              <div className="panel-head">
                <span>{contextLabel(completion.context)}</span>
                {badge && <code className="badge">{badge}</code>}
                <span className="flex" />
                <span className="hint">
                  <kbd className="kbd">↑</kbd>
                  <kbd className="kbd">↓</kbd> {t("选择")} <kbd className="kbd">Tab</kbd> {t("补全")}
                </span>
              </div>
              <div className="panel-list">
                {candidates.slice(0, 50).map((c, k) => {
                  const cls = completion.context === "class" ? classes.find((x) => x.name === c) : undefined;
                  const detail =
                    completion.context === "class"
                      ? (cls?.extensions.slice(0, 6).join(" ") ?? "")
                      : completion.context === "attr"
                        ? ATTR_LETTER[c]
                          ? `${attrLabel(c)} · ${ATTR_LETTER[c]}`
                          : ""
                        : completion.context === "unit"
                          ? unitLabel(c)
                          : "";
                  return (
                    <div
                      key={c}
                      role="option"
                      aria-selected={k === active}
                      className={`panel-row${k === active ? " on" : ""}`}
                      onMouseEnter={() => setActive(k)}
                      onClick={() => accept(k)}
                    >
                      {cls && <span className="swatch" style={{ background: classHexNow(cls.id) }} />}
                      <span className="cand">{c}</span>
                      <span className="detail">{detail}</span>
                      {k === active && completion.context === "class" && <code className="preview">:class:{c}</code>}
                    </div>
                  );
                })}
              </div>
            </>
          )}
          {errors[0] && (
            <div className="panel-error" role="alert">
              <Icon name="warning" size={14} />
              <code>{errors[0].text || "∅"}</code>
              <span>{errors.length > 1 ? t("{error}（还有 {n} 处）", { error: diagnosticText(errors[0]), n: errors.length - 1 }) : diagnosticText(errors[0])}</span>
            </div>
          )}
          <div className="panel-cheats">
            <span>
              {t("大小")} <code className="t-compare">&gt;1gb</code>
            </span>
            <span>
              {t("时间")} <code className="t-compare">m&lt;7d</code>
            </span>
            <span>
              {t("标签")} <code className="t-keyword">:red</code>
            </span>
            <span>
              {t("属性")} <code className="t-keyword">:attr:</code>
              <code className="t-value">h</code>
            </span>
            <span>
              {t("取反")} <code className="t-negate">|</code>
            </span>
          </div>
        </div>
      )}
    </div>
  );
}

/** The W3 wording of a diagnostic: message plus the accepted spellings (UI-W3-18, UI-W4-32). */
export function diagnosticText(d: FilterDiagnostic): string {
  return errorText(d.code) + (d.expected.length > 0 ? ` ${d.expected.slice(0, 8).join("/")}` : "");
}

type Colourable = Pick<FilterParse, "text" | "tokens" | "diagnostics"> & { pending?: FilterParse["pending"] };

/** Token colouring with error underlines; offsets are UTF-16, like JS strings (UI-W3-17). */
export function renderColoured(text: string, parse: Colourable | null): React.ReactNode {
  if (!parse || parse.text !== text) return text;
  const cls = new Array<string>(text.length).fill("");
  for (const tok of parse.tokens) for (let k = tok.start; k < tok.end && k < text.length; k++) cls[k] = `t-${tok.kind}`;
  const err = new Array<boolean>(text.length).fill(false);
  for (const d of parse.diagnostics) {
    if (d.severity !== "error") continue;
    const end = Math.max(d.end, d.start + 1);
    for (let k = d.start; k < end && k < text.length; k++) err[k] = true;
  }
  if (parse.pending) for (let k = parse.pending.start; k < parse.pending.end && k < text.length; k++) cls[k] += " t-pending";
  const out: React.ReactNode[] = [];
  let k = 0;
  while (k < text.length) {
    let j = k;
    while (j < text.length && cls[j] === cls[k] && err[j] === err[k]) j++;
    out.push(
      <span key={k} className={`${cls[k]}${err[k] ? " t-error" : ""}`}>
        {text.slice(k, j)}
      </span>,
    );
    k = j;
  }
  return out;
}
