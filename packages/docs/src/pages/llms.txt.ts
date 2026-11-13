import type { APIRoute } from "astro";
import { getCollection } from "astro:content";
import { nav, SECTIONS } from "@/data/nav";

// llms.txt: a short, machine-readable map of this site for LLM crawlers.
export const GET: APIRoute = async ({ site }) => {
  const entries = await getCollection("docs");
  const bySlug = new Map(entries.map((entry) => [entry.id, entry.data]));

  const lines = [
    "# horon agent",
    "",
    "> Headless coding agent in one Rust binary for any OpenAI-compatible endpoint.",
    "",
    "horon agent runs a turn, calls tools and exits, or stays up as a daemon that",
    "SDKs and editors attach to over a Unix socket. The headless package has no",
    "terminal UI; the interactive client is a separate binary on the SDK.",
    "",
  ];

  for (const section of SECTIONS) {
    const items = nav.filter((item) => item.section === section && bySlug.has(item.slug));
    if (items.length === 0) continue;
    lines.push(`## ${section}`, "");
    for (const item of items) {
      const data = bySlug.get(item.slug)!;
      const href = new URL(`/${item.slug}/`, site);
      lines.push(`- [${item.title}](${href}): ${data.description}`);
    }
    lines.push("");
  }

  lines.push("## Source", "", "- [GitHub](https://github.com/horon-ai/agent)", "");

  return new Response(lines.join("\n"), {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
};
