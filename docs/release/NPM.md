# Installing `horon` from npm

Language: **English** | [한국어](NPM.ko.md)

`@horon-ai/horon` is the npm distribution of the CLI: a small launcher plus one package per platform that carries the `horon-agent` and `horon-tui` binaries. The launcher starts the binary of the machine it runs on, and npm pins the installed version like it does for any other dependency. Choosing a release tag and the installers that fetch the release archives are described in [`INSTALL.md`](INSTALL.md).

## Package layout

The seven names are one wrapper and six platform packages. The wrapper holds no binaries.

| Package | Contents |
| --- | --- |
| `@horon-ai/horon` | Launcher `bin/horon.js` and README. `bin` maps both `horon` and `hrn` to that file. |
| `@horon-ai/horon-linux-x64` | `bin/horon-agent` and `bin/horon-tui` for Linux x86_64. |
| `@horon-ai/horon-linux-arm64` | The same pair for Linux aarch64. |
| `@horon-ai/horon-darwin-x64` | The same pair for macOS Intel. |
| `@horon-ai/horon-darwin-arm64` | The same pair for macOS Apple Silicon. |
| `@horon-ai/horon-win32-x64` | `bin/horon-agent.exe` and `bin/horon-tui.exe` for Windows x86_64. |
| `@horon-ai/horon-win32-arm64` | The same pair for Windows ARM64. |

npm picks a platform package through `optionalDependencies`, so a global install downloads only the one that matches the machine. Three properties make the set safe to publish:

- Every platform pin in the wrapper is an exact version, never a range. `scripts/npm/release_npm_version.py check-packages` fails the run when a pin differs from the version being published, so a wrapper can never offer an install whose platform package does not exist at that version.
- No package declares an install script. `preinstall`, `install` and `postinstall` are absent from the generated manifests and from `npm/horon/package.json`, and the same check rejects a platform package that declares dependencies.
- Nothing is a Node library. `npm/horon/bin/horon.js` reads `process.platform` and `process.arch`, resolves the matching package's directory, and starts `bin/horon-agent` with the forwarded arguments and environment, through `spawnSync` with `stdio: "inherit"`. The child owns the terminal; the launcher ignores `SIGINT` and `SIGTERM` while it runs so an interrupt reaches the CLI.

The launcher sets `HORON_AGENT_NO_AUTO_UPDATE=1` in the child's environment. The npm package pins one release for both binaries, and the agent's own updater would replace them with a different build, so an npm installation is updated with npm.

## Installing and running

```bash
npm i -g @horon-ai/horon
horon --version
npx @horon-ai/horon --help
```

`npm i -g` installs the wrapper and the matching platform package. `npx` runs the same launcher from a temporary install without a global one. After a global install, `horon` with no arguments starts the agent and then hands the terminal over to `horon-tui` from the same platform package, and every other subcommand goes to the headless agent; `hrn` is the same launcher under a shorter name. The hand-off needs both stdin and stdout to be a terminal, so a pipe or a set `HORON_NO_TUI` keeps the headless path.

A missing platform package is a reinstall, not a broken install. The launcher prints one `horon:` line and exits 1.

| Situation | What the launcher prints |
| --- | --- |
| npm ran with `--omit=optional`, or the package was installed incompletely | `horon: the platform package @horon-ai/horon-darwin-arm64 is not installed. reinstall with optional dependencies enabled: npm install -g --include=optional @horon-ai/horon` (the package name is the machine's own) |
| The platform is not one of the six | `horon: <platform>-<arch> is not a supported platform.` followed by the six supported keys and the same reinstall hint |

## How versions work

The npm version of a release is the release tag without its `v`. `scripts/npm/release_npm_version.py tag-version` accepts only `vMAJOR.MINOR.PATCH`, so a tag with a prerelease or build suffix is refused before anything is downloaded; the `dispatch` command applies the same rule to a manual run.

`npm/horon/package.json` keeps `0.0.0` in git and is never bumped. `scripts/npm/prepare_cli_packages.sh <out-dir> <assets-dir> <version>` copies the manifest and the launcher into a staging directory and writes the release version into that copy, together with the six pins. The tracked file therefore always describes the placeholder state, while every published package carries the version of the archives it holds.

## The publish workflow

`.github/workflows/publish-npm.yml` is the only path to the registry. Three entry points reach it, and one of them can publish.

| Entry point | When it runs | What it does |
| --- | --- | --- |
| `workflow_run` | A "Release Tag" run on `main` finishes | Finds the tag pointing at the released commit, requires a public release, and publishes at the tag version |
| `workflow_dispatch` | An operator names a release tag, or a `package_only` Release run and its version | Dry run by default; `dry_run=false` publishes when the switch is on |
| `pull_request` | A change to `npm/`, `scripts/npm/` or the workflow file | Packages fixture archives and runs the same dry-run loop |

The workflow requires npm 11.5.1 or newer, because provenance signing and trusted publishing need it. The planning job holds `contents: read` and `actions: read`, the second for reading a finished run's jobs; only the publish job gets `id-token: write`, which is the short-lived credential npm exchanges for a one-time publish grant. A concurrency group with `cancel-in-progress: false` queues a second run instead of cancelling it, so the later run re-reads the registry and skips what already landed.

A run is a dry run unless every condition holds: the event is not a pull request, the planning job resolved `mode=publish`, and the repository variable `NPM_PUBLISH` is exactly `true`. The automatic path additionally requires a successful "Release Tag" run on `main` of `horon-ai/agent`, a tag that points at that run's head commit, and a tag whose GitHub release is published rather than a draft.

Both loops publish the six platform packages first and the wrapper last, the order `scripts/npm/release_npm_version.py publish-order` prints, because the wrapper resolves its optional dependencies by name. Before each publish the real path asks `npm view <name>@<version> version`; a version that already answers is skipped, so a rerun finishes a partial set instead of failing on it. The run ends by polling every `name@version` for up to ten minutes and then printing the wrapper's `optionalDependencies`, which makes a green run the evidence that the seven landed together.

A dry run packs each package with `npm pack` and then runs `npm publish <tarball> --access public --provenance --dry-run`, comparing the wrapper's version list before and after so a dry run that wrote to the registry fails the job. The pull-request path uses the fixture version `0.0.1` for this: npm 11.5.1 refuses a dry-run publish of a version that is already on the registry, every name the first-time setup reserved carries a `0.0.0` placeholder, and a prerelease version would need an explicit `--tag`.

To restart a release that stopped midway, dispatch the same tag again. The publish job is idempotent because the platform packages and the wrapper that already exist are skipped:

```bash
gh workflow run publish-npm.yml -f release_tag=vX.Y.Z -f dry_run=false
```

## One-time setup before the first release

These steps happen once, need npm organization ownership and repository settings, and are not part of a normal release.

1. Each of the seven names must exist on the registry before it can carry a Trusted Publisher, and npm has no way to claim a name without publishing it. Check which names are already taken with the read-only `npm view @horon-ai/horon-linux-x64 version`; a name that answers needs no placeholder. For the names that do not exist yet, `scripts/npm/prepare_placeholders.sh <out-dir>` writes a folder per name holding a manifest at `0.0.0`, a description, the repository, and a one-line README, with no install script, no `bin` and no payload, and prints the commands to run. Publish those folders by hand, with two-factor authentication; the script itself never publishes.

   ```bash
   bash scripts/npm/prepare_placeholders.sh /tmp/horon-placeholders
   npm publish /tmp/horon-placeholders/horon-linux-x64 --access public
   ```

2. Register a Trusted Publisher for each of the seven packages: npm → package → Settings → Trusted Publisher → GitHub Actions, with repository `horon-ai/agent`, workflow `publish-npm.yml`, and an empty environment. Also allow `npm publish` from that publisher, because package settings created after 2026-09-03 default to staged publish only; the staged approval flow is the alternative when the publish permission is left off.
3. Register the publisher just before the first release. The setting expires unless an OIDC publish succeeds within two days of being created.
4. Set the repository variable `NPM_PUBLISH` to `true`. Until then every path, including a dispatch with `dry_run=false`, stays a dry run.
5. After the first successful publish, switch Publishing access to "Require two-factor authentication and disallow tokens". No npm token is stored anywhere in the workflow; the publish job authenticates with GitHub OIDC only.

## Checking provenance

The publish job passes `--provenance`, so npm records a signed statement that binds each tarball to the workflow run and commit that produced it. Read it back from the registry:

```bash
npm view @horon-ai/horon --json | jq '.dist.attestations'
```

The same field on each platform package names its own attestation.

## The TypeScript SDK

The TypeScript SDK publishes its own seven packages, `@horon-ai/agent-sdk` plus the six `@horon-ai/agent-<os>-<arch>` runtime packages, at the same tag version and behind the same `Release Tag` trigger and `NPM_PUBLISH` gate. It lives in `.github/workflows/publish-typescript-sdk.yml`, a separate file because npm trusts a top-level workflow file name: the SDK's seven packages are registered against `publish-typescript-sdk.yml` as their Trusted Publisher. Its procedure is in `sdk/typescript/RELEASING.md`; do not duplicate it here.

## Known limits

- `workflow_run` always runs the copy of the workflow file on the default branch, and `workflow_dispatch` needs the file there too, so both paths work only after this workflow reaches `main`. On a pull request the fixture path exercises packaging without publishing.
- A `package_only` Release run can only feed a dry run: its artifacts are not a published release, and the dispatch validation rejects `dry_run=false` with `run_id`.
- A registry that lags behind a publish is handled by the verification loop, which waits up to ten minutes; a package that still does not answer fails the run and the tag can be dispatched again.
