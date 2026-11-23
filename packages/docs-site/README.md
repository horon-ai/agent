# Documentation site (agent.horon.ai)

The [Mintlify](https://mintlify.com) project in this folder is published at
`https://agent.horon.ai`. `docs.json` holds the theme, the navigation and the
site settings, each `*.mdx` file is one page, `logo/` and `favicon.svg` are the
brand assets, and `index.mdx` is the home page. The package is managed with bun;
the Mintlify CLI (`mint`) is a dev dependency, so no global install is needed.

## Generated Reference pages

The `Reference` tab (English) and its Korean counterpart under `ko/` are not
written here. `scripts/ci/sync_docs_site.py` generates `reference/`, `ko/reference/`
and `images/reference/` from `docs/`, which stays the source of truth. Edit
`docs/`, run the script, and commit the result; never edit those folders by hand.
Three more sources feed the same tab: `PHILOSOPHY.md` (with its Korean pair) and
the READMEs of `packages/sdk/typescript`, `packages/sdk/python` and
`packages/npm/horon`, which are English only.

```bash
python3 scripts/ci/sync_docs_site.py          # rewrite the generated files
python3 scripts/ci/sync_docs_site.py --check  # exit 1 when they are stale (runs in CI)
```

The other pages (`index`, `getting-started`, `install`, `cli`, ...) are hand-written
and are English only. `docs.json` lists every generated page; the script's
`--check` fails when a page is missing from the navigation or listed without a
source.

## Work on the site

```bash
bun install --frozen-lockfile
bun run dev           # mint dev: preview at http://localhost:3000
bun run check         # mint validate (strict build) and mint broken-links
```

`mint dev` copies static assets (logos, favicon) at start-up, so restart it after
changing one. A page that is not listed in `docs.json` `navigation` is hidden from
the sidebar but still reachable by URL; add the page path (no leading slash, no
extension) to a `pages` array when you add a file. When a page moves, add a
`redirects` entry in `docs.json` and run `bunx mint broken-links --check-redirects`.

`bun run check` also runs in CI (`Documentation Site` job). It does not deploy.

## How it is published

Mintlify builds and deploys from the GitHub repository, not from CI, and only
from the branch `deploy/docs`. Its GitHub App rebuilds the site on every push to
that branch and builds a preview for every pull request that targets it, with no
path filter, so nothing but `.github/workflows/docs-site-deploy.yml` moves the
branch:

- A push to `main` that touches `packages/docs-site/`, a successful
  `Benchmark docs` run on `main` (its `[skip ci]` commits rewrite the Reference
  pages), or a manual dispatch starts the workflow.
- The workflow compares `packages/docs-site/` on `main` with the copy on
  `deploy/docs`. When they match it stops; otherwise it runs the same
  validation as the `Documentation Site` CI job and fast-forwards `deploy/docs`
  to `main` (never a force-push).
- A push to `dev`, a pull request, or a `main` push that leaves the folder
  unchanged deploys nothing. A documentation refresh of `main` is merged with
  `[skip ci]` and starts no workflow; run
  `gh workflow run docs-site-deploy.yml --ref main` after it.

The Publishes page (<https://app.mintlify.com/koo-d/koo-d/publishes/overview>)
shows the build status.

The Mintlify side is already set up:

- Organization `KooD` (slug `koo-d`), project name `Horon`. The default address,
  `https://koo-d.mintlify.site`, stays reachable beside the custom domain.
- Git Settings: GitHub repository `horon-ai/agent`, branch `deploy/docs`,
  **docs.json is in a subdirectory** set to `/packages/docs-site`.
- The Mintlify GitHub App is installed on `horon-ai`, restricted to the `agent`
  repository.

A site published before the repository was connected was hosted by Mintlify; the
hosted starter content (`Introduction`, `Quickstart`) was deleted when the
repository was connected.

## Custom domain

`agent.horon.ai` is added to the Mintlify project (Settings, Domain setup) and
shows `Connected`. The records live in the `horon.ai` zone on Cloudflare:

| Type | Name | Content | Proxy |
| --- | --- | --- | --- |
| `TXT` | `_acme-challenge.agent` | value shown on the Domain setup page | DNS only |
| `TXT` | `_cf-custom-hostname.agent` | value shown on the Domain setup page | DNS only |
| `CNAME` | `agent` | `cname.mintlify.builders` | DNS only |

The `CNAME` is DNS only (gray cloud), the setting Mintlify's documentation gives
for a switch on Cloudflare without downtime; Mintlify issues the certificate
through Let's Encrypt. This setup was not tried with the record proxied (orange
cloud). If it is proxied, Mintlify requires SSL/TLS **Full (strict)** and
**Always Use HTTPS** off, and TXT verification only completes once the `CNAME`
points at Mintlify. After a DNS change, use **Re-verify domain** on the Domain
setup page.

The site used to be served by the Cloudflare Pages project `horon-agent-docs`
(an Astro build). Nothing points at it any more; its `agent.horon.ai` custom
domain binding and the project can be deleted in Cloudflare (Workers & Pages).

`docs.json` sets `seo.metatags.canonical` to `https://agent.horon.ai`.

## Verify a deployment

```bash
dig +short CNAME agent.horon.ai                     # cname.mintlify.builders.
curl -sS -o /dev/null -w '%{http_code}\n' https://agent.horon.ai/
curl -sS -o /dev/null -w '%{http_code}\n' https://agent.horon.ai/getting-started
curl -sS https://agent.horon.ai/llms.txt | head -5  # generated by Mintlify
curl -sS -o /dev/null -w '%{http_code}\n' https://agent.horon.ai/no-such-page   # 404
```

`sitemap.xml`, `robots.txt`, `llms.txt`, `llms-full.txt` and the search MCP server
(`/mcp`) are generated by Mintlify, so none of them lives in this folder.

<!-- dry-run check -->
