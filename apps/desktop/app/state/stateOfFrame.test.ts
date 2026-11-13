// The 「隐藏文件夹」 chip follows a running scan: each frame brings how many listed folders the scan
// holds, between the calls that answer with the view's whole state.

import { describe, expect, it } from "vitest";

import type { ViewState } from "@/protocol/generated";

import { stateOfFrame } from "./actions";

const state = { view_id: "view-1", hidden_folders: 0, hide_folders: true } as ViewState;

describe("stateOfFrame", () => {
  it("takes the frame's count of hidden folders", () => {
    expect(stateOfFrame(state, { hidden_folders: 2 }).hidden_folders).toBe(2);
  });
  it("keeps the same state when nothing changed, or the server sends no count", () => {
    expect(stateOfFrame(state, { hidden_folders: 0 })).toBe(state);
    expect(stateOfFrame(state, {} as { hidden_folders: number })).toBe(state);
  });
});
