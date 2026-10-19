// schema.org structured data (JSON-LD) for search engines and AI assistants, built from the same
// text and release data as the page.

import type { Content } from "../i18n/content";
import { REPO_URL, type Release } from "./release";

/** shots: absolute URLs of the screenshots; image: the social card. */
export function structuredData(c: Content, release: Release, pageUrl: string, shots: string[], image: string): Record<string, unknown>[] {
  const author = { "@type": "Person", name: "DuskXi", url: "https://github.com/DuskXi" };
  const downloads = Object.values(release.assets)
    .filter((a) => a && a.kind !== "sums")
    .map((a) => a!.url);
  const app = {
    "@context": "https://schema.org",
    "@type": "SoftwareApplication",
    "@id": `${pageUrl}#software`,
    name: "SpaceKnife",
    alternateName: c.lang === "zh" ? "SpaceKnife 硬盘占用分析工具" : "SpaceKnife disk usage analyzer",
    description: c.description,
    url: pageUrl,
    inLanguage: c.htmlLang,
    applicationCategory: c.appCategory,
    applicationSubCategory: c.lang === "zh" ? "磁盘空间分析" : "Disk space analyzer",
    operatingSystem: "Windows 10, Windows 11, Linux (Ubuntu 22.04+, Debian 12+, RHEL 9+, Fedora)",
    softwareVersion: release.version,
    datePublished: release.date.slice(0, 10),
    releaseNotes: release.url,
    downloadUrl: downloads,
    installUrl: `${pageUrl}#download`,
    fileSize: release.assets.windows ? `${Math.round(release.assets.windows.size / 2 ** 20)}MB` : undefined,
    license: "https://opensource.org/licenses/MIT",
    isAccessibleForFree: true,
    offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
    image,
    screenshot: shots,
    featureList: c.featureList,
    author,
    publisher: author,
    sameAs: [REPO_URL],
    keywords: c.lang === "zh" ? "硬盘占用分析, 磁盘空间, 树图, 开源, Windows, Linux" : "disk usage analyzer, disk space, treemap, open source, Windows, Linux",
  };
  const source = {
    "@context": "https://schema.org",
    "@type": "SoftwareSourceCode",
    name: "SpaceKnife",
    codeRepository: REPO_URL,
    programmingLanguage: ["Python", "Rust", "TypeScript"],
    runtimePlatform: ["Windows", "Linux"],
    license: "https://opensource.org/licenses/MIT",
    author,
    targetProduct: { "@id": `${pageUrl}#software` },
  };
  const faq = {
    "@context": "https://schema.org",
    "@type": "FAQPage",
    inLanguage: c.htmlLang,
    mainEntity: c.faqs.map((f) => ({ "@type": "Question", name: f.q, acceptedAnswer: { "@type": "Answer", text: f.a } })),
  };
  const site = {
    "@context": "https://schema.org",
    "@type": "WebSite",
    name: "SpaceKnife",
    url: pageUrl,
    inLanguage: c.htmlLang,
    description: c.description,
    publisher: author,
  };
  return [app, source, faq, site];
}
