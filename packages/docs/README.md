# Documentation site (agent.horon.ai)

The Astro site in this folder is published to Cloudflare Pages as the project
`horon-agent-docs`. `src/content/docs/*.md` holds the pages, `src/data/nav.ts`
holds the sidebar, and `wrangler.toml` names the Pages project and its output
directory.

## Build

```bash
bun install --frozen-lockfile
bun run check   # astro check, then a production build into dist/
```

`bun run build` alone skips the type check. The build writes `dist/` and
generates `sitemap-index.xml` from `site` in `astro.config.mjs`, so a change to
the public URL belongs there and in `public/robots.txt`.

## Deploy

```bash
bun run deploy  # astro build && wrangler pages deploy dist --project-name horon-agent-docs
```

The command needs Cloudflare credentials. `wrangler login` once per machine
stores an OAuth token; an expired token keeps `wrangler whoami` reporting
"You are not authenticated". CI instead sets `CLOUDFLARE_API_TOKEN` and
`CLOUDFLARE_ACCOUNT_ID`. Never commit either value.

Production deployments use the branch `dev`; any other branch becomes a preview
deployment. Add `--commit-dirty=true` only when deploying an uncommitted build,
because the deployment records the commit it claims to describe.

## Custom domain

`agent.horon.ai` is attached to the Pages project as a custom domain. The domain
verification needs a DNS record in the `horon.ai` zone:

| Type | Name | Content | Proxy |
| --- | --- | --- | --- |
| `CNAME` | `agent` | `horon-agent-docs.pages.dev` | proxied |

Attaching a custom domain to the Pages project does not create that record by
itself through the API. Creating or editing DNS records needs the
`Zone.DNS:Edit` permission, which `wrangler login` does not request; use a scoped
API token or the Cloudflare dashboard for that one step, and do not touch the
other records in the zone.

After the record exists, the Pages project moves the domain from `pending` to
`active` and issues the certificate. Check with:

```bash
curl -sS -o /dev/null -w '%{http_code}\n' https://agent.horon.ai/
curl -sS -o /dev/null -w '%{http_code}\n' https://horon-agent-docs.pages.dev/llms.txt
```

## Verify a deployment

`dist/` is a static directory, so Cloudflare Pages answers an unknown path with
`404.html`, which this site ships as `src/pages/404.astro`. Confirm the pages
that are not a route: `llms.txt`, `sitemap-index.xml`, `robots.txt` and a deep
link such as `/getting-started/`, alongside `/` and a deliberately wrong path
that must return 404 rather than the index page.
