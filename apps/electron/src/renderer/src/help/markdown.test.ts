// The Markdown subset of the help pages (docs/docs-writing.md) and how links resolve.

import { describe, expect, it } from "vitest";

import { outline, render } from "./markdown";
import { resolveLink, siteUrl } from "./site";

const html = (src: string, lang: "en" | "zh" = "en", page = "filtering") => render(src, lang, page).html;

describe("markdown subset", () => {
  it("renders headings with the website's ids (github-slugger, Chinese kept, repeats numbered)", () => {
    const r = render("## Size conditions\n\n### 大小\n\n## Size conditions\n\n## `size:` and <kbd>Ctrl</kbd>+<kbd>K</kbd>", "en", "filtering");
    expect(r.headings).toEqual([
      { depth: 2, id: "size-conditions", text: "Size conditions" },
      { depth: 3, id: "大小", text: "大小" },
      { depth: 2, id: "size-conditions-1", text: "Size conditions" },
      { depth: 2, id: "size-and-ctrlk", text: "size: and Ctrl+K" },
    ]);
    expect(r.html).toContain('<h2 id="size-conditions">Size conditions</h2>');
    expect(r.html).toContain('<h3 id="大小">大小</h3>');
    expect(outline("## 过滤（进阶）\n\n```\n## not a heading\n```\n")).toEqual([{ depth: 2, id: "过滤进阶", text: "过滤（进阶）" }]);
  });

  it("turns <kbd> into key caps and keeps every other tag as text", () => {
    expect(html("Press <kbd>Ctrl</kbd>+<kbd>K</kbd>.")).toBe('<p>Press <kbd class="kbd">Ctrl</kbd>+<kbd class="kbd">K</kbd>.</p>\n');
    expect(html("<kbd>Shift</kbd> then <b>bold</b>")).toBe('<p><kbd class="kbd">Shift</kbd> then &lt;b&gt;bold&lt;/b&gt;</p>\n');
    expect(html("an <kbd>open cap")).toBe("<p>an &lt;kbd&gt;open cap</p>\n");
    expect(html("a stray </kbd> close")).toBe("<p>a stray &lt;/kbd&gt; close</p>\n");
    expect(html("`<kbd>F1</kbd>` in code")).toBe("<p><code>&lt;kbd&gt;F1&lt;/kbd&gt;</code> in code</p>\n");
    expect(html("<script>alert(1)</script>")).not.toContain("<script");
    expect(html('<div onclick="x()">hi</div>')).not.toContain("<div");
  });

  it("drops HTML comments: the generated-section markers, on their own lines or inline", () => {
    const src = "Before\n\n<!-- generated:keys:start -->\n| Key | Command |\n| --- | --- |\n| <kbd>F1</kbd> | Help |\n<!-- generated:keys:end -->\n\nAfter <!-- inline --> text\n\n<!--\nmany\n\nlines\n-->\nEnd";
    const out = html(src);
    expect(out).not.toContain("generated");
    expect(out).not.toContain("&lt;!--");
    expect(out).not.toContain("many");
    expect(out).toContain("<p>After  text</p>");
    expect(out).toContain("<p>End</p>");
    expect(out).toContain('<td><kbd class="kbd">F1</kbd></td>');
    // a comment written as code is shown
    expect(html("`<!-- generated:x:start -->`")).toContain("<code>&lt;!-- generated:x:start --&gt;</code>");
  });

  it("renders tables in a scroll box and code blocks with a copy button", () => {
    const table = html("| A | B |\n| :-- | --: |\n| 1 | 2 |");
    expect(table).toContain('<div class="help-table"><table>');
    expect(table).toContain('<th style="text-align:left">A</th>');
    expect(table).toContain('<td style="text-align:right">2</td>');
    expect(table).toContain("</table></div>");
    const code = render("```sh\nspaceknife scan C:\\Users\\lenny <path>\n```", "en", "command-line", "Copy code").html;
    expect(code).toContain('<pre><code class="language-sh">spaceknife scan C:\\Users\\lenny &lt;path&gt;\n</code></pre>');
    expect(code).toContain('<button type="button" class="help-copy" data-copy aria-label="Copy code" title="Copy code">');
  });

  it("renders the four asides, with the default title in the page's language or the one given", () => {
    const note = html(":::note\nPlain **text**.\n:::");
    expect(note).toContain('<aside class="help-aside note" aria-label="Note">');
    expect(note).toContain("<span>Note</span>");
    expect(note).toContain("<p>Plain <strong>text</strong>.</p>");
    expect(note.trim().endsWith("</aside>")).toBe(true);
    expect(html(":::tip\nx\n:::", "zh")).toContain("<span>提示</span>");
    expect(html(":::caution\nx\n:::", "zh")).toContain('aria-label="警告"');
    expect(html(":::danger\nx\n:::", "zh")).toContain("<span>危险</span>");
    const titled = html(":::tip[Use the `/` key]\n- one\n- two\n:::");
    expect(titled).toContain("<span>Use the <code>/</code> key</span>");
    expect(titled).toContain("<li>one</li>");
    // nested: the outer fence has more colons
    const nested = html("::::note\nouter\n:::tip\ninner\n:::\nafter\n::::\nout");
    expect(nested.match(/<aside/g)).toHaveLength(2);
    expect(nested).toMatch(/inner<\/p>\n<\/div>\n<\/aside>\n<p>after<\/p>/);
    expect(nested.trim().endsWith("<p>out</p>")).toBe(true);
    // not one of the four: stays text, as on the website
    expect(html(":::warning\nx\n:::")).toContain("<p>:::warning");
  });

  it("marks links: help pages (relative, with anchors) stay inside, the rest opens in the browser", () => {
    const r = render("[A](../shortcuts/) [B](../filtering/#大小) [C](#top) [D](https://example.com/x) https://spaceknife.kaltsit.dev/ [E](javascript:alert(1))", "zh", "filtering");
    expect(r.html).toContain(`<a href="${siteUrl("zh", "shortcuts")}" data-doc="shortcuts">A</a>`);
    expect(r.html).toContain('data-doc="filtering" data-anchor="大小">B</a>');
    expect(r.html).toContain('data-doc="filtering" data-anchor="top">C</a>');
    expect(r.html).toContain('<a href="https://example.com/x" data-external="" title="https://example.com/x">D</a>');
    expect(r.html).toContain('data-external="" title="https://spaceknife.kaltsit.dev/">https://spaceknife.kaltsit.dev/</a>');
    expect(r.html).not.toContain(`href="javascript`); // not a link: shown as written
    expect(r.links.map((l) => l.target.kind)).toEqual(["doc", "doc", "doc", "external", "external"]);
  });
});

describe("resolveLink", () => {
  it("resolves like the website does, from the page's own address", () => {
    expect(resolveLink("../filtering/", "en", "shortcuts")).toEqual({ kind: "doc", id: "filtering", anchor: null, url: "https://spaceknife.kaltsit.dev/docs/filtering/" });
    expect(resolveLink("../filtering/#%E5%A4%A7%E5%B0%8F", "zh", "index")).toMatchObject({ kind: "external" }); // /zh/filtering/: not a page
    expect(resolveLink("filtering/#大小", "zh", "index")).toEqual({ kind: "doc", id: "filtering", anchor: "大小", url: "https://spaceknife.kaltsit.dev/zh/docs/filtering/#%E5%A4%A7%E5%B0%8F" });
    expect(resolveLink("../", "en", "filtering")).toMatchObject({ kind: "doc", id: "index" });
    expect(resolveLink("/docs/install/", "en", "index")).toMatchObject({ kind: "doc", id: "install" });
    // the other language, or a page without its trailing slash: the website
    expect(resolveLink("/zh/docs/install/", "en", "index")).toMatchObject({ kind: "external" });
    expect(resolveLink("../install", "en", "filtering")).toMatchObject({ kind: "external" });
    expect(resolveLink("mailto:someone@example.com", "en", "index")).toEqual({ kind: "invalid" });
  });
});
