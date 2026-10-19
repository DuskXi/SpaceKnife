import type { APIRoute } from "astro";

import { llmsTxt } from "../data/llms";
import { latestRelease } from "../data/release";

export const GET: APIRoute = async () =>
  new Response(llmsTxt(await latestRelease()), { headers: { "Content-Type": "text/plain; charset=utf-8" } });
