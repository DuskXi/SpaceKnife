// Everyone may crawl, AI crawlers included (served at the root of spaceknife.kaltsit.dev, where
// crawlers look for it).
import type { APIRoute } from "astro";

const AGENTS = ["GPTBot", "OAI-SearchBot", "ChatGPT-User", "ClaudeBot", "Claude-User", "Claude-SearchBot", "PerplexityBot", "Google-Extended", "Applebot-Extended", "CCBot", "Bingbot", "Googlebot"];

export const GET: APIRoute = ({ site }) => {
  const sitemap = new URL(`${import.meta.env.BASE_URL}sitemap-index.xml`, site).href;
  const lines = ["User-agent: *", "Allow: /", "", ...AGENTS.flatMap((a) => [`User-agent: ${a}`, "Allow: /", ""]), `Sitemap: ${sitemap}`, ""];
  return new Response(lines.join("\n"), { headers: { "Content-Type": "text/plain; charset=utf-8" } });
};
