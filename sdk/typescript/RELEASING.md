# Releasing `@horon-ai/agent-sdk`

A release publishes seven npm packages at one version: `@horon-ai/agent-sdk` and
the six platform runtime packages generated from it. All seven come from one
release tag, in one workflow, and every check that decides whether they may be
published runs before anything is written to the registry.

## Package ownership

The package is published as `@horon-ai/agent-sdk`. The `@horon-ai` npm organization
owns the scope, and every package name under it keeps the product word out of the
name after the slash: `@horon-ai/agent-sdk`, not `@horon-ai/horon-agent-sdk`.
Publish only under the `@horon-ai` scope; the shorter `@horon-agent` scope is not
available and must not appear in package metadata or documentation.

The six runtime packages follow the same pattern and are named after the
`libc`/`os`/`cpu` triple they hold:

```
@horon-ai/agent-linux-x64    @horon-ai/agent-darwin-x64    @horon-ai/agent-win32-x64
@horon-ai/agent-linux-arm64  @horon-ai/agent-darwin-arm64  @horon-ai/agent-win32-arm64
```

If the package name changes in the future, update `name` in
`sdk/typescript/package.json`, the install examples in this README and the SDK
README, the repository references, and the website's `/sdk` page together.

## The one workflow

`.github/workflows/publish-typescript-sdk.yml` owns the release. It has three
entry points and only one of them can write to npm.

| Entry point | When | What it does |
|---|---|---|
| `workflow_run` on **Release Tag** | A release was tagged on `main` | Publishes the seven packages at the tag's version |
| `workflow_dispatch` | An operator names a published release tag | Dry run by default; `dry_run: false` publishes when the switch is on |
| `pull_request` | A change to `sdk/typescript`, `scripts/sdk`, or the workflow | Generates fixture runtimes and dry-runs the whole flow |

A dispatch only works for a tag created after this workflow reached `main`. The
run checks out the tag, and a tree older than `scripts/sdk/sdk_release.py`
(`v0.1.6` and earlier) has no helper to call, so it stops at the stamp step
before anything is packed. Dispatch the next release instead.

The SDK is released at the same version as the CLI, because it bundles that
CLI's runtime: a tag `v0.1.6` publishes `@horon-ai/agent-sdk@0.1.6` together
with the six `0.1.6` runtime packages. There is no separate SDK version line to
keep in step, and no dispatch input can substitute a different run.

Each run does the same six things:

1. Resolve the version from the tag (`scripts/sdk/sdk_release.py tag-version`),
   which refuses anything that is not `vMAJOR.MINOR.PATCH`. The `plan` job also
   decides the mode through the same helper, so the rule the unit tests prove is
   the rule a release runs.
2. Install the SDK in `sdk/typescript` (`bun install --frozen-lockfile`) against
   the committed manifest. A dry run then also validates it with `bun run
   check`; a publish run does not build anything here, because it is already
   running on a commit whose CI on `main` passed and `prepack` rebuilds `dist/`
   from a clean slate when the tarball is packed.
3. Download the release's six `horon-agent-*.tar.gz` assets. The pull-request
   path generates stand-ins with the same archive names and members instead.
4. Stamp the version into `sdk/typescript/package.json` and its six
   `optionalDependencies` pins, then generate the runtime packages with
   `scripts/sdk/prepare_runtime_packages.sh`.
5. Check that all seven prepared packages carry that exact version
   (`scripts/sdk/sdk_release.py check-packages`). A version that disagrees
   anywhere stops the run before publishing.
6. Pack each package once and publish the tarball, six platforms first and the
   SDK last, so a published SDK never offers an install whose runtime is not on
   the registry yet.

Packing the SDK runs `prepack`, which rebuilds `dist/` from a clean slate and
needs the Bun-installed dev dependencies. `files` limits the SDK tarball to
`dist`, `README.md`, and `LICENSE`; `bash scripts/sdk/test_package.sh` checks
the packed tarball as a consumer sees it.

## Turning the real publish on

Until three things are true the workflow is a dry run, on every path:

- The repository variable `NPM_PUBLISH` is `true`. Without it, even
  `workflow_dispatch` with `dry_run: false` stays a dry run.
- The npm packages exist. npm has no way to claim a new name without publishing
  it, so the owner publishes the seven names once as `0.0.0` placeholders.
- Each of the seven packages names this workflow as a Trusted Publisher
  (npm → package → Settings → Trusted Publisher → GitHub Actions), with
  repository `horon-ai/agent`, workflow `publish-typescript-sdk.yml` and
  environment empty.

Trusted publishing is why this is a separate workflow file rather than a job
inside `publish-npm.yml`: npm trusts a *top-level workflow file name*, so
sharing one file between the CLI and the SDK would mean both trust the same
entry, and an npm `workflow_call` target cannot be named at all. No npm token
is stored: the publish job asks GitHub for short-lived OIDC credentials
(`id-token: write`) and npm exchanges them for a one-time publish grant, which
is also what signs provenance.

`scripts/sdk/publish_preflight.sh` checks the local half of this (the gates, the
tarball contents, and whether the seven names exist on the registry). It cannot
see the repository variable or the Trusted Publisher configuration.

## The owner-only first release

These steps happen once and are not part of a normal release. They need npm
organization ownership and repository settings, so an agent must not perform
them.

1. Publish `0.0.0` placeholders for the seven names, under the `@horon-ai`
   scope, so each package page exists and can carry a Trusted Publisher.
2. For each of the seven packages, add `horon-ai/agent` /
   `publish-typescript-sdk.yml` as a Trusted Publisher.
3. Set the `NPM_PUBLISH` repository variable to `true`.
4. Tag the release the usual way (Release Tag does this from a green `main`) and
   let the workflow run. It publishes the seven packages at the tag version.

Remove `NPM_PUBLISH` (or set it to anything but `true`) to stop publishing
without editing the workflow.

## Verifying a published release

```bash
cd "$(mktemp -d)"
npm init -y >/dev/null
npm install @horon-ai/agent-sdk
node --input-type=module -e '
  import { HoronAgentClient } from "@horon-ai/agent-sdk";
  const client = await HoronAgentClient.launch({ workingDir: process.cwd() });
  const session = await client.createSession();
  console.log((await client.run(session.session_id, "say hello")).text);
  await client.close();
'
```

A publish run finishes by waiting for all seven `name@version` pairs to appear
on the registry, then printing the SDK's `optionalDependencies`, so a green run
is itself the evidence that the set landed together. The consumer command above
is the same shape as `scripts/sdk/test_package.sh`, run against the registry
rather than a local tarball.

## Versioning

The SDK version is the release version; a tag decides it and nothing else does.
Which number the tag carries is decided by `scripts/release/next_version.py`
from the changes in the release, not by the SDK's own protocol surface.

The committed `sdk/typescript/package.json` must still agree with the root
`Cargo.toml`, and `scripts/sdk/sdk_release.py check-committed` runs in CI next
to the npm CLI package gate. The workflow stamps the tag version over it in the
runner checkout only; the stamp preserves field order, so stamping an unchanged
version rewrites the tracked file byte for byte.

Two mechanical guards make a protocol change hard to land halfway, and both run
in CI: a Rust test fails if a variant or field is missing from
`sdk/typescript/src/protocol.ts`, and a Node test fails if the tag sets diverge.
A protocol major bump is a breaking release, so it is also a major one.

## Platform support

macOS and Linux are exercised end to end. Windows compiles and is wired up (the
bridge uses a named pipe, and the SDK derives the same pipe name, pinned by
tests on both sides) but has no live coverage yet. Do not describe it as
supported until something actually runs there.
