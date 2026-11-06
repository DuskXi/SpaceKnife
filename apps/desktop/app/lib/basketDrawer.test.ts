import { describe, expect, it } from "vitest";

import type { BasketItem, BasketVolume } from "@/protocol/generated";
import { crumbLabel } from "@/lib/scanPage";

import { itemCount, mainVolume, parentOf, sortedItems, statusText } from "./basketDrawer";

const item = (name: string, path: string, alloc: number) => ({ id: name, name, path, alloc }) as BasketItem;

describe("basket drawer helpers", () => {
  it("sorts by size, name or place without touching the list", () => {
    const items = [item("b", "D:\\x\\b", 10), item("a", "C:\\y\\a", 30), item("c", "C:\\a\\c", 20)];
    expect(sortedItems(items, "size").map((i) => i.name)).toEqual(["a", "c", "b"]);
    expect(sortedItems(items, "name").map((i) => i.name)).toEqual(["a", "b", "c"]);
    expect(sortedItems(items, "path").map((i) => i.name)).toEqual(["c", "a", "b"]);
    expect(items.map((i) => i.name)).toEqual(["b", "a", "c"]);
  });

  it("shows the volume most of it frees space on", () => {
    const vol = (mount_point: string, freeable: number) => ({ mount_point, freeable }) as BasketVolume;
    expect(mainVolume([vol("C:", 1), vol("D:", 5), vol("E:", 5)])?.mount_point).toBe("D:");
    expect(mainVolume([])).toBeNull();
  });

  it("says counts, reasons and folders", () => {
    expect(itemCount(3)).toBe("3 项");
    expect(statusText("locked")).toBe("正被其他程序使用");
    expect(statusText("ready")).toBe("");
    expect(parentOf("C:\\Users\\me\\a.txt")).toBe("C:\\Users\\me");
    expect(parentOf("/home/me/dir/")).toBe("/home/me");
  });
});

describe("scan page helpers", () => {
  it("labels the crumbs of a pane header", () => {
    expect(crumbLabel({ label: "C:\\" }, 0)).toBe("C:");
    expect(crumbLabel({ label: "/" }, 0)).toBe("/");
    expect(crumbLabel({ label: "C:\\Users\\me" }, 2)).toBe("me");
    expect(crumbLabel({ label: "small:1200" }, 3)).toBe("1,200 个小项");
    expect(crumbLabel({ label: "more:5" }, 3)).toBe("+5 项");
  });
});
