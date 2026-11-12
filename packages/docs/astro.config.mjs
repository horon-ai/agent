import sitemap from "@astrojs/sitemap";
import { defineConfig } from "astro/config";

// The public documentation site for horon-agent (agent.horon.ai). Output is a
// fully static directory that Cloudflare Pages serves; see wrangler.toml.
export default defineConfig({
  site: "https://agent.horon.ai",
  // Cloudflare Pages serves each page at `<path>/` and 308s the bare path, so
  // the slashed form is canonical here too and dev matches production.
  trailingSlash: "always",
  integrations: [sitemap()],
  markdown: {
    shikiConfig: {
      themes: { light: "github-light", dark: "github-dark" },
    },
  },
});
