// The pictures of the help pages, for the app's help (src/renderer/src/help/pictures.ts). The pages
// are the website's Markdown (site/src/content/docs); a picture is a file in site/src/assets that a
// page names with a relative path, as the website resolves it.
//
// The virtual module "virtual:help-pictures" maps each picture the pages name, by its path below
// site/src ("assets/visualizations/treemap.webp"), to its bundled address and its size in pixels:
// only those files go into the app, and the help can give every <img> its width and height, so a
// page doesn't move while its pictures load (the help jumps to headings and restores scroll
// positions right after it renders). A page naming a picture that doesn't exist fails the build.

import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import type { Plugin } from "vite";

export interface Size {
  width: number;
  height: number;
}

/** Width and height from an image file's header: PNG, WebP (lossy, lossless, extended), JPEG, GIF. */
export function imageSize(data: Uint8Array): Size {
  const b = Buffer.from(data.buffer, data.byteOffset, data.byteLength);
  const ascii = (at: number, n: number) => b.toString("latin1", at, at + n);
  if (b.length >= 24 && b.readUInt32BE(0) === 0x89504e47 && ascii(12, 4) === "IHDR") {
    return { width: b.readUInt32BE(16), height: b.readUInt32BE(20) };
  }
  if (b.length >= 30 && ascii(0, 4) === "RIFF" && ascii(8, 4) === "WEBP") {
    const chunk = ascii(12, 4);
    if (chunk === "VP8X") return { width: 1 + b.readUIntLE(24, 3), height: 1 + b.readUIntLE(27, 3) };
    if (chunk === "VP8L" && b[20] === 0x2f) {
      const bits = b.readUInt32LE(21);
      return { width: 1 + (bits & 0x3fff), height: 1 + ((bits >>> 14) & 0x3fff) };
    }
    if (chunk === "VP8 " && b[23] === 0x9d && b[24] === 0x01 && b[25] === 0x2a) {
      return { width: b.readUInt16LE(26) & 0x3fff, height: b.readUInt16LE(28) & 0x3fff };
    }
  }
  if (b.length >= 10 && (ascii(0, 6) === "GIF87a" || ascii(0, 6) === "GIF89a")) {
    return { width: b.readUInt16LE(6), height: b.readUInt16LE(8) };
  }
  if (b.length >= 4 && b[0] === 0xff && b[1] === 0xd8) {
    // segments up to a start-of-frame marker (C0–CF, except DHT C4, JPG C8 and DAC CC)
    let at = 2;
    while (at + 9 < b.length) {
      if (b[at] !== 0xff) break;
      const marker = b[at + 1]!;
      if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
        return { width: b.readUInt16BE(at + 7), height: b.readUInt16BE(at + 5) };
      }
      at += 2 + b.readUInt16BE(at + 2);
    }
  }
  throw new Error("not a PNG, WebP, JPEG or GIF file, or its header is cut short");
}

/** The pictures a page's Markdown names (`![alt](src)`), outside code blocks and inline code. */
export function pictureRefs(markdown: string): string[] {
  const prose = markdown.replace(/^(```|~~~)[^\n]*\n[\s\S]*?^\1[^\n]*$/gm, "").replace(/`[^`\n]*`/g, "");
  return [...prose.matchAll(/!\[[^\]]*\]\(\s*(?:<([^>\n]+)>|([^)\s]+))/g)].map((m) => (m[1] ?? m[2])!);
}

const ID = "virtual:help-pictures";

/** `site` is the website's folder (the one with src/content/docs and src/assets). */
export function helpPictures(site: string): Plugin {
  const pages = resolve(site, "src", "content", "docs");
  const assets = resolve(site, "src", "assets");
  const src = resolve(site, "src");
  return {
    name: "sk-help-pictures",
    resolveId: (id) => (id === ID ? `\0${ID}` : null),
    load(id) {
      if (id !== `\0${ID}`) return null;
      const found = new Map<string, { file: string; size: Size }>();
      for (const page of markdownFiles(pages)) {
        this.addWatchFile(page);
        for (const ref of pictureRefs(readFileSync(page, "utf8"))) {
          if (/^[a-z][a-z0-9+.-]*:|^\/\//i.test(ref)) continue; // a web address: not bundled, the help shows its description
          const file = resolve(dirname(page), decodeURI(ref));
          const where = `${relative(site, page)}: ${ref}`;
          if (!file.startsWith(assets + sep)) this.error(`${where} is not in site/src/assets`);
          let data: Buffer;
          try {
            data = readFileSync(file);
          } catch {
            return this.error(`${where}: no such file`);
          }
          this.addWatchFile(file);
          try {
            found.set(relative(src, file).split(sep).join("/"), { file, size: imageSize(data) });
          } catch (e) {
            this.error(`${where}: ${(e as Error).message}`);
          }
        }
      }
      const entries = [...found].sort(([a], [b]) => a.localeCompare(b));
      return [
        ...entries.map(([, p], i) => `import url${i} from ${JSON.stringify(`${p.file.split(sep).join("/")}?url`)};`),
        "export default {",
        ...entries.map(([key, p], i) => `  ${JSON.stringify(key)}: { url: url${i}, width: ${p.size.width}, height: ${p.size.height} },`),
        "};",
      ].join("\n");
    },
  };
}

function markdownFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? markdownFiles(join(dir, e.name)) : e.name.endsWith(".md") ? [join(dir, e.name)] : [],
  );
}
