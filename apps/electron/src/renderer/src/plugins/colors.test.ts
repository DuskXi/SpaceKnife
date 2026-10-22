// Plugins' colour modes (colors.v1): the choices after the built-in four, the C key's order, palette
// entries resolved here (semantic names, tokens, class colours, hex, per-theme), the frame's pal
// column and the painters reading it.

import { tokens } from "@design/tokens";
import { describe, expect, it } from "vitest";

import type { ColorModeInfo, ExtensionsState, FrameInfo } from "@/protocol/generated";
import type { BinaryFrame } from "@/protocol/rpc";
import { fillOf, rgba } from "@/treemap/paint";
import { decodeVisualFrame } from "@/visual/frame";

import { colorChoices, legendOf, nextColorMode, palettePaint, resolveColor, toneColor } from "./colors";
import { whenReason, type WhenEnv } from "./when";

const mode = (over: Partial<ColorModeInfo> = {}): ColorModeInfo => ({
  id: "t.decl.media",
  plugin: "t.decl",
  title: "媒体类型",
  palette: ["muted", "#3F7FBF", "class:video", { light: "#111111", dark: "#EEEEEE" }],
  legend: ["其他", "视频", "图片", ""],
  folders: "ring",
  layouts: [],
  ...over,
});
const ext = (modes: ColorModeInfo[]) => ({ color_modes: modes }) as unknown as ExtensionsState;
const classHex = (id: string) => (id === "video" ? "#AA0000" : "#000000");

describe("choices", () => {
  it("lists the built-in modes, then the plugins', with why one does not fit the layout", () => {
    const list = colorChoices(ext([mode(), mode({ id: "t.decl.flat2", layouts: ["flame.v1"] })]), "drawlist.v1");
    expect(list.map((c) => c.id)).toEqual(["class", "flat", "time", "branch", "t.decl.media", "t.decl.flat2"]);
    expect(list[4]!.reason).toBeNull();
    expect(list[5]!.reason).toBe("这种配色不适用于当前可视化");
    // the layout not known yet: offered
    expect(colorChoices(ext([mode({ layouts: ["flame.v1"] })]), null)[4]!.reason).toBeNull();
    // no plugins.v1: the built-in four
    expect(colorChoices(null, "drawlist.v1")).toHaveLength(4);
  });

  it("C walks the usable ones and wraps; Shift+C goes back", () => {
    const list = colorChoices(ext([mode(), mode({ id: "t.decl.no", layouts: ["polar.v1"] })]), "drawlist.v1");
    expect(nextColorMode(list, "branch")).toBe("t.decl.media");
    expect(nextColorMode(list, "t.decl.media")).toBe("class"); // the unfitting one is skipped
    expect(nextColorMode(list, "class", -1)).toBe("t.decl.media");
    expect(nextColorMode(list, "gone.mode")).toBe("class");
  });
});

describe("palette entries", () => {
  it("resolves semantic names, tokens, class colours, hex and per-theme pairs (the app is dark)", () => {
    expect(resolveColor("accent", classHex)).toBe(tokens.color.accent.default);
    expect(resolveColor("danger", classHex)).toBe(tokens.color.status.danger);
    expect(resolveColor("treemap.class.video", classHex)).toBe(tokens.treemap.class.video);
    expect(resolveColor("class:video", classHex)).toBe("#AA0000");
    expect(resolveColor("#123456", classHex)).toBe("#123456");
    expect(resolveColor({ light: "#111111", dark: "#EEEEEE" }, classHex)).toBe("#EEEEEE");
  });

  it("falls back to the neutral colour for anything it can't read", () => {
    const neutral = resolveColor("muted", classHex);
    expect(resolveColor("no.such.token", classHex)).toBe(neutral);
    expect(resolveColor("red", classHex)).toBe(neutral);
    expect(resolveColor("url(javascript:x)", classHex)).toBe(neutral);
    expect(resolveColor(null, classHex)).toBe(neutral);
    expect(toneColor(null, classHex)).toBeNull();
  });

  it("builds the painters' palette and the legend; built-in modes have none", () => {
    const p = palettePaint("t.decl.media", ext([mode()]), classHex)!;
    expect(p.colors).toEqual([resolveColor("muted", classHex), "#3F7FBF", "#AA0000", "#EEEEEE"]);
    expect(p.folders).toBe(false);
    expect(palettePaint("class", ext([mode()]), classHex)).toBeNull();
    // a mode that went away: neutral until the view changes it
    expect(palettePaint("gone.mode", ext([]), classHex)!.colors).toHaveLength(1);
    expect(legendOf(mode(), classHex).map((x) => x.text)).toEqual(["其他", "视频", "图片"]);
  });
});

describe("the pal column", () => {
  const frame = (columns: Record<string, unknown>): BinaryFrame => ({ header: { layout: "drawlist.v1", count: 2 } as never, columns: columns as never, blobs: {} });
  const base = {
    key: Uint32Array.of(1, 2),
    node: Int32Array.of(1, 2),
    parent: Int32Array.of(-1, 0),
    kind: Uint8Array.of(0, 0),
    depth: Uint8Array.of(1, 2),
    cls: Uint8Array.of(0, 0),
    label: Uint8Array.of(0, 0),
    size: BigInt64Array.of(1n, 2n),
    count: Uint32Array.of(1, 1),
    flags: Uint16Array.of(0, 0),
    tag: Uint8Array.of(0, 0),
    tone: Uint8Array.of(0, 0),
    name_len: Uint32Array.of(0, 0),
    x: Int32Array.of(0, 0),
    y: Int32Array.of(0, 0),
    w: Int32Array.of(1, 1),
    h: Int32Array.of(1, 1),
  };
  const info = { palette: "t.decl.media" } as FrameInfo;

  it("is decoded when the frame has it, null otherwise", () => {
    expect(decodeVisualFrame(frame({ ...base, pal: Uint8Array.of(1, 2) }), info)!.pal).toEqual(Uint8Array.of(1, 2));
    expect(decodeVisualFrame(frame(base), info)!.pal).toBeNull();
  });

  it("paints each row in its palette entry; the neutral one without pal or out of range", () => {
    const p = palettePaint("t.decl.media", ext([mode()]), classHex)!;
    const f = { kind: Uint8Array.of(0, 0, 0), cls: Uint8Array.of(0, 0, 0), tone: Uint8Array.of(0, 0, 0), delta: null, pal: Uint8Array.of(1, 9, 2) };
    expect(fillOf(f, 0, "t.decl.media", [], undefined, undefined, p)).toEqual(rgba("#3F7FBF"));
    expect(fillOf(f, 1, "t.decl.media", [], undefined, undefined, p)).toEqual(rgba(p.colors[0]!));
    expect(fillOf(f, 2, "t.decl.media", [], undefined, undefined, p)).toEqual(rgba("#AA0000"));
    expect(fillOf({ ...f, pal: null }, 0, "t.decl.media", [], undefined, undefined, p)).toEqual(rgba(p.colors[0]!));
    // the built-in modes as before
    expect(fillOf(f, 0, "flat", [], undefined, undefined, null)).toEqual(rgba(tokens.treemap.class.other));
  });
});

describe("when", () => {
  const env = (over: Partial<WhenEnv> = {}): WhenEnv => ({
    scan: { state: "done" },
    view: true,
    selection: true,
    compare: false,
    setting: (k) => ({ "plugin.a.b.on": true })[k],
    capabilities: ["layout.flame.v1"],
    plugins: ["a.b"],
    owner: "a.b",
    sourceId: null,
    ...over,
  });

  it("holds when every condition does; names the first that fails", () => {
    expect(whenReason([], env())).toBeNull();
    expect(whenReason(["scan", "done", "view", "selection", "scan_view", "setting:on", "cap:layout.flame.v1", "plugin:a.b"], env())).toBeNull();
    expect(whenReason(["done"], env({ scan: { state: "scanning" } }))).toBe("扫描完成后可用");
    expect(whenReason(["scan"], env({ scan: null }))).toBe("需要先扫描");
    expect(whenReason(["selection"], env({ selection: false }))).toBe("没有选中项");
    expect(whenReason(["compare"], env())).toBe("只在快照对比中可用");
    expect(whenReason(["setting:off"], env())).toBe("需要先在设置里打开这一项");
    expect(whenReason(["plugin:c.d"], env())).toBe("需要启用插件「c.d」");
  });

  it("reads a plugin's local names as its own, and never holds for a name it doesn't know", () => {
    expect(whenReason(["source:zip"], env({ sourceId: "a.b.zip" }))).toBeNull();
    expect(whenReason(["source:zip"], env({ sourceId: "c.d.zip" }))).toBe("只用于这个插件的扫描来源");
    expect(whenReason(["moon_phase"], env())).toBe("这个版本不认识的条件：moon_phase");
  });
});
