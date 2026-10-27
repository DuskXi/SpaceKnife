// The documentation (Starlight): src/content/docs/docs/*.md in English, src/content/docs/zh/docs/*.md
// in Chinese. See docs/docs-writing.md (repository root) for how the pages are written.
import { defineCollection } from "astro:content";
import { docsLoader } from "@astrojs/starlight/loaders";
import { docsSchema } from "@astrojs/starlight/schema";

export const collections = {
  docs: defineCollection({ loader: docsLoader(), schema: docsSchema() }),
};
