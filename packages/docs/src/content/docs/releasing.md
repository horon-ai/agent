---
title: Package layout
description: How the repository publishes the CLI, the TypeScript SDK and the Python SDK, and where each package lives.
section: Build
order: 4
---

The repository ships three independently versioned distribution sets from one
tag. Their sources live under `packages/`.

## Layout

```
packages/
  sdk/
    typescript/     @horon-ai/agent-sdk + six @horon-ai/agent-<os>-<arch> runtime packages
    python/         horon-agent-sdk (+ platform wheels)
  npm/
    horon/          @horon-ai/horon launcher + six @horon-ai/horon-<os>-<arch> packages
```

The TypeScript SDK and the CLI share the release tag version; the publish
workflow stamps the tag into its checkout, and CI checks that the committed version
and the six runtime pins equal the root `Cargo.toml` version.

## npm CLI

| Package | Contents |
| --- | --- |
| `@horon-ai/horon` | Launcher `bin/horon.js`; `bin` maps both `horon` and `hrn` to it. |
| `@horon-ai/horon-linux-x64` | `bin/horon-agent`, `bin/horon-tui` |
| `@horon-ai/horon-linux-arm64` | the same pair |
| `@horon-ai/horon-darwin-x64` | the same pair |
| `@horon-ai/horon-darwin-arm64` | the same pair |
| `@horon-ai/horon-win32-x64` | `.exe` pair |
| `@horon-ai/horon-win32-arm64` | `.exe` pair |

npm picks a platform package through `optionalDependencies`, so a global install
downloads only the one that matches the machine. Three properties keep the set
safe:

- Every platform pin is an exact version, never a range; a wrapper can never offer
  an install whose platform package does not exist at that version.
- No package declares an install script. `preinstall`, `install` and `postinstall`
  are absent from the generated manifests, and a platform package must not declare
  dependencies.
- Nothing is a Node library. The launcher reads `process.platform` and
  `process.arch`, resolves the matching package and starts `bin/horon-agent`
  through `spawnSync` with `stdio: "inherit"`. The child owns the terminal; the
  launcher ignores `SIGINT` and `SIGTERM` while it runs.

The launcher sets `HORON_AGENT_NO_AUTO_UPDATE=1` in the child's environment: the
package pins one release for both binaries, so an npm installation is updated with
npm. A missing platform package is a reinstall, not a broken install; the launcher
prints one `horon:` line and exits 1.

## TypeScript SDK

`@horon-ai/agent-sdk` plus one `@horon-ai/agent-<os>-<arch>` package per supported
platform, published at the same tag version behind the `NPM_PUBLISH` gate. It has
its own workflow file because npm trusts a top-level workflow file name for the
Trusted Publisher registration.

## Python SDK

`horon-agent-sdk` publishes a pure wheel, an sdist, and one platform wheel per
supported platform. Platform wheels carry the runtime; the pure wheel and sdist do
not, so `launch()` falls back to `horon-agent` on `PATH`.

## Release flow

1. Work lands on `dev`; `main` is what ships.
2. A release merges `dev` into `main` with a merge commit; the tag workflow tags
   that commit and publishes the images and packages.
3. Package publishing is gated behind `NPM_PUBLISH` and runs only from the tag
   workflow, never from a pull request.
