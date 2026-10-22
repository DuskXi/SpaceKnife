// The Markdown of a report's md block: a subset, with raw HTML off (anything in angle brackets is
// text), no images (nothing is fetched), and links shown as text with their address in the tooltip
// (a plugin opens pages only through a `url` action, which asks first).

import MarkdownIt from "markdown-it";

const md = new MarkdownIt({ html: false, linkify: false, typographer: false, breaks: false });
md.disable(["image"]);
md.renderer.rules.link_open = (tokens, idx) => {
  const href = String(tokens[idx]?.attrGet("href") ?? "");
  return `<span class="md-link" title="${md.utils.escapeHtml(href)}">`;
};
md.renderer.rules.link_close = () => "</span>";

/** HTML of a report's Markdown text (only elements markdown-it makes, attributes escaped). */
export function renderReportMd(text: string): string {
  return md.render(text);
}
