import { describe, expect, it } from "vitest";

import { GlRenderer } from "./gl";
import { STRIDE } from "./paint";

/** A WebGL2 stand-in that records buffer uploads: constants are 1, every call succeeds. */
function fakeCanvas(): { canvas: HTMLCanvasElement; calls: { name: string; args: unknown[] }[] } {
  const calls: { name: string; args: unknown[] }[] = [];
  const gl = new Proxy(
    {},
    {
      get: (_t, key) => {
        if (typeof key !== "string") return undefined;
        if (key === key.toUpperCase()) return 1;
        return (...args: unknown[]) => {
          calls.push({ name: key, args });
          return key.endsWith("Parameter") ? true : {};
        };
      },
    },
  );
  const canvas = { getContext: () => gl } as unknown as HTMLCanvasElement;
  return { canvas, calls };
}

const uploads = (calls: { name: string; args: unknown[] }[]) => calls.filter((c) => c.name === "bufferData" || c.name === "bufferSubData");

describe("GlRenderer.upload", () => {
  it("keeps one buffer per picture and updates it in place", () => {
    const { canvas, calls } = fakeCanvas();
    const r = new GlRenderer(canvas);
    const data = new Float32Array(1000 * STRIDE);
    calls.length = 0;
    const first = r.upload(data, 100);
    expect(uploads(calls).map((c) => c.name)).toEqual(["bufferData", "bufferSubData"]);
    expect(first.capacity).toBe(100 * STRIDE * 4);

    calls.length = 0;
    const again = r.upload(data, 80, first); // a hover repaint: same size or smaller
    expect(again).toBe(first);
    expect(uploads(calls).map((c) => c.name)).toEqual(["bufferSubData"]);
    expect(calls.some((c) => c.name === "createBuffer" || c.name === "createVertexArray")).toBe(false);

    calls.length = 0;
    const grown = r.upload(data, 900, first); // more rows: the buffer grows once, at least doubling
    expect(grown).toBe(first);
    expect(uploads(calls).map((c) => c.name)).toEqual(["bufferData", "bufferSubData"]);
    expect(grown.capacity).toBe(900 * STRIDE * 4);
    expect(grown.count).toBe(900);
  });

  it("asks for no depth or stencil buffer", () => {
    let attrs: WebGLContextAttributes | undefined;
    const { canvas } = fakeCanvas();
    const get = canvas.getContext;
    (canvas as unknown as { getContext: (k: string, a: WebGLContextAttributes) => unknown }).getContext = (k, a) => {
      attrs = a;
      return (get as unknown as (k: string) => unknown)(k);
    };
    new GlRenderer(canvas);
    expect(attrs).toMatchObject({ depth: false, stencil: false });
  });
});
