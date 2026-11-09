// The filter's words (W3; components/shell/FilterBox.vue, the palette, 「匹配结果」): completion
// contexts, attribute and unit names, diagnostic messages, and the syntax colouring as segments.
// Every text is looked up when shown: the interface language can change at any time.

import type { FilterDiagnostic, FilterParse } from "@/protocol/generated";
import { t } from "@/lib/i18n";

/** The name of a completion context (the panel's header). */
export const contextLabel = (context: string): string =>
  (({ keyword: t("关键字"), class: t("文件类别"), tag: t("标签"), attr: t("属性"), unit: t("单位"), prefix: t("条件开头") }) as Record<string, string>)[context] ?? context;

/** The prefix a completion context writes (the panel's badge). */
export const CONTEXT_BADGE: Record<string, string> = { keyword: ":", class: ":class:", tag: ":tag:", attr: ":attr:" };

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

/** The short spelling of each attribute (`:attr:h`). */
export const ATTR_LETTER: Record<string, string> = {
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

/** A size or time unit's name; "" for one not known. */
export const unitLabel = (unit: string): string =>
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

/** The W3 wording of a diagnostic: message plus the accepted spellings (UI-W3-18, UI-W4-32). */
export function diagnosticText(d: FilterDiagnostic): string {
  return errorText(d.code) + (d.expected.length > 0 ? ` ${d.expected.slice(0, 8).join("/")}` : "");
}

export type Colourable = Pick<FilterParse, "text" | "tokens" | "diagnostics"> & { pending?: FilterParse["pending"] };

/** A run of the coloured text: its characters and their classes (`t-<token kind>`, ` t-pending`,
 * ` t-error`); `cls` null for plain text (no parse, or a parse of another text). */
export interface ColouredSegment {
  start: number;
  text: string;
  cls: string | null;
}

/** Token colouring with error underlines; offsets are UTF-16, like JS strings (UI-W3-17). Rendered
 * as `<span :class="cls">` per segment, or the bare text when `cls` is null. */
export function colourSegments(text: string, parse: Colourable | null): ColouredSegment[] {
  if (!parse || parse.text !== text) return text ? [{ start: 0, text, cls: null }] : [];
  const cls = new Array<string>(text.length).fill("");
  for (const tok of parse.tokens) for (let k = tok.start; k < tok.end && k < text.length; k++) cls[k] = `t-${tok.kind}`;
  const err = new Array<boolean>(text.length).fill(false);
  for (const d of parse.diagnostics) {
    if (d.severity !== "error") continue;
    const end = Math.max(d.end, d.start + 1);
    for (let k = d.start; k < end && k < text.length; k++) err[k] = true;
  }
  if (parse.pending) for (let k = parse.pending.start; k < parse.pending.end && k < text.length; k++) cls[k] += " t-pending";
  const out: ColouredSegment[] = [];
  let k = 0;
  while (k < text.length) {
    let j = k;
    while (j < text.length && cls[j] === cls[k] && err[j] === err[k]) j++;
    out.push({ start: k, text: text.slice(k, j), cls: `${cls[k]}${err[k] ? " t-error" : ""}` });
    k = j;
  }
  return out;
}
