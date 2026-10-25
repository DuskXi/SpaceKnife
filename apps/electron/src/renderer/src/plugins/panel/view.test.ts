// A web panel and its view: what sk.view says, sk.colors' colours and legend (the view's paint
// functions with the theme on screen), the colors operation's permission, and the view pushes.

import { describe, expect, it } from "vitest";

import bridgeJs from "../../../../../resources/plugin-sdk/bridge.js?raw";

import type { ColorModeInfo, ExtensionsState, FilterVocab, ViewColors } from "@/protocol/generated";
import { palette } from "@/lib/theme";
import { rgba } from "@/treemap/paint";
import { branchFill, ringFill } from "@/visual/colors";

import { FakePort, mockHost, settle } from "../testing/mockBridge";
import { PanelBridge, PERMISSION_DENIED } from "./bridge";
import { panelColors } from "./colors";
import { panelView } from "./PanelFrame";

const hex = (c: readonly number[]) => "#" + c.slice(0, 3).map((v) => Math.round(v * 255).toString(16).padStart(2, "0")).join("");
const vocab = { classes: [{ id: "video", name: "视频", name_en: "", color: "#AA0000", color_light: "#AA0000", aliases: [], extensions: [] }, { id: "doc", name: "文档", name_en: "", color: "", color_light: "", aliases: [], extensions: [] }] } as unknown as FilterVocab; // i18n-ignore
const answer = (over: Partial<ViewColors> = {}): ViewColors => ({ mode: "class", palette: null, kind: [0, 0, 2, -1], cls: [0, 1, 255, 255], tone: [0, 4, 4, 4], branch: [1, 2, 1, 0], depth: [1, 2, 1, 0], pal: null, ...over });
const classHex = (id: string) => (id === "video" ? "#AA0000" : "#334455");
const timeHex = (k: number) => ["#000001", "#000002", "#000003", "#000004", "#000005"][k]!;

describe("sk.colors", () => {
  it("paints files by the class mode, folders on the level tones, and leaves out what is not a node", () => {
    const r = panelColors(answer(), vocab, null, classHex, timeHex);
    expect(r.colors[0]).toBe("#aa0000"); // the user's colour of class 0
    expect(r.colors[2]).toBe(hex(ringFill(1))); // a folder
    expect(r.colors[3]).toBe("");
    expect(r.legend).toEqual([
      { label: "视频", color: "#AA0000" }, // i18n-ignore
      { label: "文档", color: expect.stringMatching(/^#/) }, // i18n-ignore
    ]);
  });

  it("follows the branch and time modes", () => {
    const b = panelColors(answer({ mode: "branch" }), vocab, null, classHex, timeHex);
    expect(b.colors[1]).toBe(hex(branchFill(2, 2)));
    expect(b.legend).toEqual([]);
    const t = panelColors(answer({ mode: "time" }), vocab, null, classHex, timeHex);
    expect(t.colors[0]).toBe(hex(rgba((palette().treemap.time as Record<string, string>)["0"]!)));
    expect(t.legend.map((x) => x.color)).toEqual(["#000001", "#000005"]);
  });

  it("paints a plugin's colour mode from its palette, with its legend", () => {
    const info = { id: "p.ages", plugin: "p", title: "Ages", palette: ["muted", "#112233", "#445566"], legend: ["", "new", "old"], legend_colors: [], folders: "ring", layouts: [] } as unknown as ColorModeInfo;
    const ext = { color_modes: [info] } as unknown as ExtensionsState;
    const r = panelColors(answer({ mode: "p.ages", palette: "p.ages", pal: [1, 2, 0, 0] }), vocab, ext, classHex, timeHex);
    expect(r.colors.slice(0, 2)).toEqual(["#112233", "#445566"]);
    expect(r.legend).toEqual([
      { label: "new", color: "#112233" },
      { label: "old", color: "#445566" },
    ]);
  });
});

describe("sk.view", () => {
  it("says what the view shows, null without one", () => {
    const state = { scan_id: "s1", visual: "example.shapes.pack", color_mode: "class", zoom_node: 4, zoom_key: 4, selected: 9, detail: 5, compare: false };
    const views = { v1: { state, info: { layout: "shapes.v1", size_unit: "bytes" }, filter: { text: ">1mb", active: true } } } as never;
    expect(panelView({ views }, "v1")).toEqual({
      view_id: "v1",
      scan_id: "s1",
      visual: "example.shapes.pack",
      layout: "shapes.v1",
      color_mode: "class",
      zoom_node: 4,
      zoom_key: 4,
      selected: 9,
      detail: 5,
      filter: { text: ">1mb", active: true },
      compare: false,
      size_unit: "bytes",
    });
    expect(panelView({ views }, null)).toBeNull();
    expect(panelView({ views }, "v2")).toBeNull();
  });
});

describe("the bridge", () => {
  const BASE = "skplugin://example.panel/";
  const connected = (permissions: string[]) => {
    const port = new FakePort();
    const host = mockHost({ permissions });
    const bridge = new PanelBridge(host, port, { base: BASE });
    port.panel({ type: "ready", href: `${BASE}index.html` });
    return { port, host, bridge };
  };

  it("asks for the view's colours with tree.read only", async () => {
    const without = connected([]);
    without.port.panel({ type: "host", id: 1, op: "colors", args: { nodes: [1] } });
    await settle();
    expect(without.port.of("error")).toMatchObject([{ id: 1, code: PERMISSION_DENIED }]);
    const reader = connected(["tree.read"]);
    reader.port.panel({ type: "host", id: 2, op: "colors", args: { nodes: [1] } });
    await settle();
    expect(reader.host.hostOps).toEqual([{ op: "colors", args: { nodes: [1] } }]);
  });

  it("runs the app's view commands for the panel's view, never the basket's", async () => {
    const c = connected([]);
    c.port.panel({ type: "host", id: 1, op: "command", args: { command: "view.color.time" } });
    c.port.panel({ type: "host", id: 2, op: "command", args: { command: "view.visual.example.shapes.pack" } });
    c.port.panel({ type: "host", id: 3, op: "command", args: { command: "basket.empty" } });
    await settle();
    expect(c.host.hostOps.map((x) => x.args.command)).toEqual(["view.color.time", "view.visual.example.shapes.pack"]);
  });

  it("pushes the view once the panel is ready, and the panel SDK reads it", () => {
    const c = connected([]);
    c.bridge.push("view", { view: { view_id: "v1" } });
    expect(c.port.of("view")).toEqual([{ type: "view", view: { view_id: "v1" } }]);
    expect(bridgeJs).toContain('case "view":');
    expect(bridgeJs).toContain("onView(cb)");
    expect(bridgeJs).toContain('host("colors")');
  });
});
