// The SpaceKnife website: a static Astro site published to GitHub Pages
// (https://duskxi.github.io/SpaceKnife/) by .github/workflows/pages.yml.
import sitemap from "@astrojs/sitemap";
import { defineConfig } from "astro/config";

export default defineConfig({
  site: "https://duskxi.github.io",
  base: "/SpaceKnife",
  trailingSlash: "always",
  build: { format: "directory" },
  integrations: [
    sitemap({
      i18n: { defaultLocale: "en", locales: { en: "en", zh: "zh-CN" } },
    }),
  ],
  vite: {
    // the screenshots (docs/images) and icons (design/build) live outside the site folder
    server: { fs: { allow: [".."] } },
  },
});
