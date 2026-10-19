// The SpaceKnife website: a static Astro site published to GitHub Pages
// (https://spaceknife.kaltsit.dev/, a custom domain) by .github/workflows/pages.yml.
import sitemap from "@astrojs/sitemap";
import { defineConfig } from "astro/config";

export default defineConfig({
  site: "https://spaceknife.kaltsit.dev",
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
