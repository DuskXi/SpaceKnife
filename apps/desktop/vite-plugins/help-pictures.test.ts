// Reading picture sizes from file headers, and finding the pictures a page names.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { imageSize, pictureRefs } from "./help-pictures";

const bytes = (...parts: (string | number[])[]) =>
  Uint8Array.from(parts.flatMap((p) => (typeof p === "string" ? [...p].map((c) => c.charCodeAt(0)) : p)));
const u32be = (n: number) => [(n >>> 24) & 255, (n >>> 16) & 255, (n >>> 8) & 255, n & 255];
const u32le = (n: number) => u32be(n).reverse();
const u16le = (n: number) => [n & 255, n >>> 8];
const u16be = (n: number) => [n >>> 8, n & 255];

describe("imageSize", () => {
  it("reads PNG", () => {
    const png = bytes([0x89], "PNG\r\n\x1a\n", u32be(13), "IHDR", u32be(1800), u32be(917), [8, 6, 0, 0, 0]);
    expect(imageSize(png)).toEqual({ width: 1800, height: 917 });
  });

  it("reads all three kinds of WebP", () => {
    const riff = (chunk: string, data: number[]) => bytes("RIFF", u32le(4 + 8 + data.length), "WEBP", chunk, u32le(data.length), data);
    // lossy: frame tag, start code, 14-bit width and height
    expect(imageSize(riff("VP8 ", [0, 0, 0, 0x9d, 0x01, 0x2a, ...u16le(1800), ...u16le(917)]))).toEqual({ width: 1800, height: 917 });
    // lossless: signature, then width − 1 and height − 1 in 14 bits each
    const bits = (640 - 1) | ((480 - 1) << 14);
    expect(imageSize(riff("VP8L", [0x2f, ...u32le(bits), 0, 0, 0, 0, 0]))).toEqual({ width: 640, height: 480 });
    // extended: flags, then width − 1 and height − 1 in 24 bits each
    expect(imageSize(riff("VP8X", [0, 0, 0, 0, 0x1f, 0x4e, 0, 0x0f, 0x27, 0]))).toEqual({ width: 20000, height: 10000 });
  });

  it("reads JPEG from its start-of-frame segment, skipping the others", () => {
    const app0 = [0xff, 0xe0, ...u16be(16), ...new Array<number>(14).fill(0)];
    const dht = [0xff, 0xc4, ...u16be(4), 0, 0]; // C4 is a Huffman table, not a frame
    const sof = [0xff, 0xc2, ...u16be(17), 8, ...u16be(768), ...u16be(1024), 3, ...new Array<number>(9).fill(0)];
    expect(imageSize(bytes([0xff, 0xd8], app0, dht, sof))).toEqual({ width: 1024, height: 768 });
  });

  it("reads GIF", () => {
    expect(imageSize(bytes("GIF89a", u16le(32), u16le(16), [0, 0, 0]))).toEqual({ width: 32, height: 16 });
  });

  it("reads the website's pictures", () => {
    const file = fileURLToPath(new URL("../../../site/src/assets/visualizations/polar.webp", import.meta.url));
    expect(imageSize(readFileSync(file))).toEqual({ width: 1800, height: 917 });
  });

  it("refuses anything else, or a header cut short", () => {
    expect(() => imageSize(bytes("<svg xmlns='http://www.w3.org/2000/svg'/>"))).toThrow();
    expect(() => imageSize(bytes([0x89], "PNG\r\n\x1a\n", u32be(13), "IHDR"))).toThrow();
    expect(() => imageSize(bytes("RIFF", u32le(30), "WEBP", "VP8 ", u32le(10), [0, 0, 0, 1, 2, 3]))).toThrow();
  });
});

describe("pictureRefs", () => {
  it("finds the pictures a page names, not ones shown as code", () => {
    const page = [
      "![Treemap](../../../assets/visualizations/treemap-en.webp)",
      "Text ![inline](<../a b.png>) and a [link](../filtering/).",
      "`![in code](x.png)`",
      "```md",
      "![in a block](y.png)",
      "```",
      "![web](https://example.com/z.png)",
    ].join("\n");
    expect(pictureRefs(page)).toEqual(["../../../assets/visualizations/treemap-en.webp", "../a b.png", "https://example.com/z.png"]);
  });
});
