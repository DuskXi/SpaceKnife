import type { APIRoute } from "astro";

import { llmsFullTxt } from "../data/llms";
import { latestRelease } from "../data/release";

export const GET: APIRoute = async () =>
  new Response(llmsFullTxt(await latestRelease()), { headers: { "Content-Type": "text/plain; charset=utf-8" } });
