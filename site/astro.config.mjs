// The SpaceKnife website: a static Astro site published to GitHub Pages
// (https://spaceknife.kaltsit.dev/, a custom domain) by .github/workflows/pages.yml.
// The landing page is src/pages/; the documentation (/docs/, /zh/docs/) is Starlight, written in
// Markdown under src/content/docs/ (the app shows the same files on its help page).
import { readFileSync } from "node:fs";

import sitemap from "@astrojs/sitemap";
import starlight from "@astrojs/starlight";
import { defineConfig } from "astro/config";

const nav = JSON.parse(readFileSync(new URL("./src/docs-nav.json", import.meta.url), "utf8"));
const slug = (id) => (id === "index" ? "docs" : `docs/${id}`);

export default defineConfig({
  site: "https://spaceknife.kaltsit.dev",
  trailingSlash: "always",
  build: { format: "directory" },
  integrations: [
    starlight({
      title: { en: "SpaceKnife Docs", "zh-CN": "SpaceKnife 文档" },
      description: "How to use SpaceKnife, the disk usage analyzer with a live treemap.",
      defaultLocale: "root",
      locales: {
        root: { label: "English", lang: "en" },
        zh: { label: "简体中文", lang: "zh-CN" },
      },
      logo: { src: "./public/favicon.svg", alt: "SpaceKnife" },
      favicon: "/favicon.svg",
      social: [{ icon: "github", label: "GitHub", href: "https://github.com/DuskXi/SpaceKnife" }],
      editLink: { baseUrl: "https://github.com/DuskXi/SpaceKnife/edit/dev/site/" },
      sidebar: nav.groups.map((g) => ({
        label: g.label.en,
        translations: { "zh-CN": g.label.zh },
        items: g.items.map((id) => ({ slug: slug(id) })),
      })),
      customCss: [
        "@fontsource-variable/geist",
        "@fontsource-variable/geist-mono",
        "@fontsource-variable/noto-sans-sc",
        "./src/styles/docs.css",
      ],
      components: {
        // the site is dark only: so are the docs (no theme switch)
        ThemeProvider: "./src/components/docs/ThemeProvider.astro",
        ThemeSelect: "./src/components/docs/ThemeSelect.astro",
      },
      disable404Route: true, // src/pages/404.astro
    }),
    sitemap({
      i18n: { defaultLocale: "en", locales: { en: "en", zh: "zh-CN" } },
    }),
  ],
  vite: {
    // the screenshots (docs/images) and icons (design/build) live outside the site folder
    server: { fs: { allow: [".."] } },
  },
});
