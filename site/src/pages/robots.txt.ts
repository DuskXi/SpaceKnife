// Everyone may crawl, AI crawlers included. Note: crawlers read robots.txt only at the root of a
// host; on a project site (duskxi.github.io/SpaceKnife/) this copy takes effect with a custom domain.
import type { APIRoute } from "astro";

const AGENTS = ["GPTBot", "OAI-SearchBot", "ChatGPT-User", "ClaudeBot", "Claude-User", "Claude-SearchBot", "PerplexityBot", "Google-Extended", "Applebot-Extended", "CCBot", "Bingbot", "Googlebot"];

export const GET: APIRoute = ({ site }) => {
  const sitemap = new URL(`${import.meta.env.BASE_URL}sitemap-index.xml`, site).href;
  const lines = ["User-agent: *", "Allow: /", "", ...AGENTS.flatMap((a) => [`User-agent: ${a}`, "Allow: /", ""]), `Sitemap: ${sitemap}`, ""];
  return new Response(lines.join("\n"), { headers: { "Content-Type": "text/plain; charset=utf-8" } });
};
