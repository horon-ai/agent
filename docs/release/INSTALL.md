# Release installation

Language: **English** | [한국어](INSTALL.ko.md)

The installers download the release assets and then hand them to the agent binary's `install-release` instead of unpacking the archive themselves. Verification, extraction, the immutable version directory and channel activation all run through one path (`src/cli/install_release.rs`, `crates/app-core/src/update.rs` `install_release_from_directory`).

```bash
horon install-release --assets <directory> --agent-version <version>
```

| Flag | Meaning |
| --- | --- |
| `--assets <DIR>` | Real directory holding the release assets. It must contain `SHA256SUMS` and the platform asset. |
| `--agent-version <VERSION>` | Release version the assets belong to. It names the immutable version directory. |
| `--source-binary <PATH>` | Install this locally built executable instead of a downloaded asset. |

## Verification

`crates/app-core/src/update/release_asset.rs` checks the assets during staging.

- The asset directory must be a real directory, not a symbolic link, and `SHA256SUMS` is always read. A missing file or one that is not a regular file fails the install.
- Without `--source-binary` the platform asset's SHA-256 is compared against `SHA256SUMS`. A mismatched value or a malformed hash installs nothing.
- With `--source-binary` the locally compiled executable is trusted as the source and is not compared against the published bytes. `SHA256SUMS` is still required.
- Version strings allow only alphanumerics and `.`, `-`, `+`. Names containing a path separator or `..` are rejected.
- An archive yields only regular files and directories. Symbolic links, hard links, absolute paths, `..` escapes and duplicate paths are rejected, and going over the 512 MiB per-file or 2 GiB total budget fails. The archive must hold exactly one top-level platform executable, and every published archive also carries a top-level `LICENSE`, which is installed beside the executable as documentation. Any other member is refused.
- When a directory for the same version already exists, the installed executable's SHA-256 is compared first. Identical bytes reuse that version; different bytes are refused with `the installed agent version already contains different bytes` and the existing files are left untouched.

Once verification passes, `stable`, `current`, the launcher links and the update metadata are swapped together. A failure midway rolls back, so the previous installation stays in place (`crates/app-core/src/update/activation.rs`). The command downloads nothing from the network. Preparing the assets is the caller's job.

## Archive format

The FreeBSD x64 archive uses POSIX ustar so FreeBSD `tar` and `install-release` read the same two top-level regular files, `horon-agent-freebsd-x86_64` and `LICENSE`. Before upload, `scripts/release/agent_archive.sh pack <dist-dir> <binary>` checks the format, names, executable permission and bytes. The release job then installs the archive, compares the installed executable with the build output and runs the launcher.

`scripts/release/agent_archive.sh install-verify <dist-dir> <binary> [version] [pairs]` installs the archive into a fresh home and reinstalls into that home. Both installs must preserve the source bytes and the launcher's version output. The final optional argument is the number of install samples, each one cold then warm, so the default is a single pair. A manual FreeBSD smoke run with `official_asset` set also verifies that release's published checksum, re-packs its own executable and checks five installation pairs. The recorded run uses `v0.1.14`, whose FreeBSD archive is the GNU-sparse case this packaging replaced, so the repair is proved on published bytes rather than on a fresh build. The workflow saves its logs and member listing in `freebsd-archive-evidence-<tag>`.

## Platform assets

`release.yml` builds every asset from the tagged commit and attaches it to a draft release. `Finalize release` publishes only when the complete set exists; a missing platform leaves the release in draft.

| Platform | Agent asset | TUI asset |
| --- | --- | --- |
| Linux x86_64, glibc 2.17 baseline | `horon-agent-linux-x86_64.tar.gz` | `horon-tui-linux-x86_64.tar.gz` |
| Linux aarch64 | `horon-agent-linux-aarch64.tar.gz` | `horon-tui-linux-aarch64.tar.gz` |
| macOS Apple Silicon | `horon-agent-macos-aarch64.tar.gz` | `horon-tui-macos-aarch64.tar.gz` |
| macOS Intel | `horon-agent-macos-x86_64.tar.gz` | `horon-tui-macos-x86_64.tar.gz` |
| Windows x86_64 | `horon-agent-windows-x86_64.exe` and `.tar.gz` | `horon-tui-windows-x86_64.exe` and `.tar.gz` |
| Windows ARM64 | `horon-agent-windows-aarch64.exe` and `.tar.gz` | `horon-tui-windows-aarch64.exe` and `.tar.gz` |
| FreeBSD x86_64 | `horon-agent-freebsd-x86_64.tar.gz` | none |

FreeBSD is the one platform with no interactive client: the release publishes the agent only, and the installer never looks for `horon-tui` there.

## Package support

A user-installed Rust package (an extension or a plugin) runs as a child process of the agent, so a platform is supported only where the package was installed, called and removed. The `Package compat` workflow (`.github/workflows/package-compat.yml`) builds the agent binary and the worked example `examples/packages/example` on a native runner for each target below, then runs `cargo test --locked --test package_end_to_end` against them. The report covers both kinds: the install, a tool call from a later turn, a plugin resource call, a cancelled call, a replacement, removal with new calls refused, every descendant process reaped, a refused wrong target and a refused corrupted copy.

Only a target whose native run passed every step says `Yes` here. A target whose run fails stays in the table as `No` until it passes: the six rows are the complete statement of support, and the checker refuses a table that omits one. A `No` row is not a passing state and does not make the workflow green; a red run is the warning to fix that target, and the row changes to `Yes` in a follow-up once the run passes.

A log from a cross-compiled build is not execution evidence: a report whose `cross_compiled` is not false, or whose native run misses a step, is refused instead of counted. The final job of the workflow downloads every report, requires a report for all six targets and the steps of both kinds, and prints the target by kind by step matrix. It stays red while any target fails, whatever this table says.

| Target | Platform | Native execution verified |
| --- | --- | --- |
| `aarch64-apple-darwin` | macOS on Apple silicon | Yes |
| `x86_64-apple-darwin` | macOS on Intel | Yes |
| `aarch64-unknown-linux-gnu` | Linux on arm64 | Yes |
| `x86_64-unknown-linux-gnu` | Linux on x86-64 | Yes |
| `aarch64-pc-windows-msvc` | Windows on arm64 | Yes |
| `x86_64-pc-windows-msvc` | Windows on x86-64 | Yes |

`scripts/ci/package_compat.py docs-claims` reads this table inside the `Package support matrix` (`support-matrix`) job after the six native targets run and fails when a row says `Yes` for a target that has no passing report, so a claim here can only follow a green native run. On pull requests and pushes that edit only prose in `docs/release/INSTALL.md` or `docs/release/INSTALL.ko.md` without changing the advertised target table, `Check if install claims changed` verifies the table is unchanged and skips rebuilding the six native targets as well as `support-matrix`, while `Documentation Checks` still validates table consistency (`scripts/ci/test_docs_install_claims.py`). A pull request is compared with its base and a push with the previous tip; a new branch, a push whose previous tip cannot be diffed, and a manual dispatch always run the matrix. FreeBSD and the other platforms have no runner in that workflow, so they are not listed and this page claims no package support there. The package format and the six declaration targets are described in [`../runtime/PACKAGES.md`](../runtime/PACKAGES.md).

## How the install scripts use it

`scripts/install/install.sh` and `scripts/install/install.ps1` fetch the release assets and `SHA256SUMS` into a temporary directory and then call this command. `scripts/install/install_from_source.sh` calls the same command with `--source-binary` to install a local build. The shell and PowerShell installers therefore never produce different layouts, and a failure leaves the previous installation alone.

The PowerShell installer hides progress while downloading release files. A caller that pipes the script into `iex` keeps its own `$ProgressPreference` setting.

The Windows install locations and signing procedure are in [`WINDOWS.md`](WINDOWS.md).

## Release download cache

`scripts/install/install.sh` keeps the last verified copy of each asset it installs, so a reinstall does not download it a second time. `install.ps1` does not use this cache.

| Item | Location |
| --- | --- |
| Cache root | `$XDG_CACHE_HOME`, otherwise `~/.cache` on Linux and FreeBSD and `~/Library/Caches` on macOS |
| Entries | `<cache root>/horon-agent/release/<asset name>`, one file per asset name |

- `SHA256SUMS` is fetched from `https://github.com/horon-ai/agent/releases/download/<tag>/` on every run and is never stored. An entry is reused only when this run's list gives the same digest for that exact asset name.
- An entry is only a source. Its bytes are copied into the temporary asset directory and the copy is hashed against this run's list, so the file handed to `install-release` is the file that was verified.
- The cache is used only when the directory that holds the cache root, the cache root itself and the directories created under it are all real directories owned by the current user with neither the group nor the world write bit set. A shared location such as `/tmp`, a symbolic link, an unreadable mode or an owner other than the current user disables the cache for that run instead of being written to.
- After `install-release` succeeds, the installer moves each verified download to a temporary name in the cache and sets the entry's final name with a rename. A move on the same filesystem reuses the downloaded file. Across filesystems the move copies the bytes before the rename. Concurrent installs see only complete entries.
- A damaged entry, an entry this release does not list, or a cache that cannot be written falls back to the download path. The cache never fails an install.
- `HORON_AGENT_NO_DOWNLOAD_CACHE=1` disables the cache for one run. `rm -rf <cache root>/horon-agent` removes it; the immutable version store, the launcher and uninstall do not depend on it.

## Installing from npm (not published yet)

The npm distribution is built and tested, but nothing has been published: the registry holds only the `0.0.0` placeholder packages that reserve the seven names, and those carry no binaries, so `npm i -g @horon-ai/horon` installs a launcher that cannot find a runtime. Use the installers above, the container image, or a source checkout until publishing is enabled.

When it is, `@horon-ai/horon` installs the same two binaries without a release download: one small launcher and one platform package pinned to the same version, chosen through `optionalDependencies`, so a global install fetches only the one that matches the machine.

```bash
npm i -g @horon-ai/horon
npx @horon-ai/horon --help
```

An npm installation is updated with npm, not with the agent's own updater: the launcher sets `HORON_AGENT_NO_AUTO_UPDATE=1` for the binary it starts, because the package pins one release for both binaries. Package layout, the error a missing platform package prints, the version rule, and the publish procedure are in [`NPM.md`](NPM.md).

## The TUI binary

The release also publishes the TUI binary `horon-tui`, the client that TUI mode runs in, as its own archive beside the agent: `horon-tui-<suffix>.tar.gz`, with the same platform suffix the agent asset uses (`horon-tui-linux-x86_64.tar.gz`, `horon-tui-macos-aarch64.tar.gz`, `horon-tui-windows-x86_64.tar.gz`, and so on). Every platform except FreeBSD publishes one; FreeBSD is the agent alone. `release_asset.rs` derives the name from the agent asset, and the suffix is what ties the two together. Both binaries are built by the same `release.yml` run from the same tagged commit and are staged into the same version directory, so there is no separate client release or channel. The version string differs: `horon-agent --version` derives its version from the release tag (`crates/build-meta`), while `horon-tui --version` prints the `crates/tui` package version (`crates/tui/src/lib.rs`).

`scripts/install/install.sh` and `scripts/install/install.ps1` fetch the archive into the same temporary directory as the agent asset and check it against `SHA256SUMS` before handing the directory to `install-release`. `install.sh` starts that transfer right after `SHA256SUMS` and lets it run beside the agent download, and it requests the archive only when the manifest names it; both transfers are settled before `install-release` is called. The staged client is placed in `builds/versions/<version>/` beside `horon-agent`, and the `LICENSE` each archive carries is installed there too, so one version directory holds both binaries with the licence that covers them and activation swaps them together. Installing the client is documented in [`../runtime/TUI.md`](../runtime/TUI.md).

The client is optional at every step, and a copy that cannot be verified never blocks the agent:

| Situation | Result |
| --- | --- |
| The release publishes no archive for the platform | One line, then `horon-agent` alone |
| `SHA256SUMS` has no entry for the archive | One line, then `horon-agent` alone |
| The digest does not match | One line, then `horon-agent` alone |
| `--source-binary`, or FreeBSD | No released client is staged; FreeBSD publishes `horon-agent` only |
| The version directory already exists | The client is added only when the file is absent; the version is immutable otherwise |

`scripts/install/uninstall.sh` and `scripts/install/uninstall.ps1` list the `horon-tui` launcher in the install directory with the other targets and remove it, including one left behind by an older install.

## Running on a Linux server

The release binary starts with only glibc, libm and libgcc_s present. OpenSSL and other packages are not needed. All TLS goes through rustls, and OpenSSL is absent from the dependency graph (`cargo tree -p horon-agent -i openssl-sys --target all` finds nothing).

### Certificates

The roots trusted by model HTTPS calls are the union of the webpki roots built into the binary and the roots the OS trusts. `serve`, `run` and `api-bridge` therefore work without `ca-certificates`, and a private CA installed in the OS store is trusted too.

- macOS and Windows use the OS verifier through `rustls-platform-verifier`. They do not read every certificate in the keychain or certificate store at process startup.
- Linux and the other Unix systems read the OS store (`/etc/ssl/certs` and similar) once per process with `rustls-native-certs` and merge it with the webpki roots. An image without a store still connects on the webpki roots alone.
- The configuration is built once per process and shared by every HTTPS client. ALPN offers `h2` and `http/1.1`.

No setting narrows the trust set or disables verification. To use an internal CA, put it in the OS store. The implementation is described under "TLS trust set" in [`../architecture/CRATES.md`](../architecture/CRATES.md).

### Preparation

No TTY, display or `XDG_RUNTIME_DIR` is required. The socket directory is taken from `HORON_AGENT_RUNTIME_DIR`, then `XDG_RUNTIME_DIR`, then the platform's per-user runtime directory, and when none is set the binary uses `horon-agent-<user discriminator>` under the temporary directory (`crates/harness-api/src/sockets.rs:16-36`). The discriminator is read from `UID`, then `USER`, keeping only alphanumerics, `-` and `_` and cutting to 64 characters. If nothing is left, it is `user`.

Put the provider profile in `config.toml`. The location is `$HORON_HOME/config.toml` when `HORON_HOME` is set and `~/.horon/config.toml` otherwise. Keys and file locations are in [`../provider/MODEL.md`](../provider/MODEL.md).

```toml
[provider]
default_provider = "my-gw"

[providers.my-gw]
type = "openai-compatible"
base_url = "https://llm.example.com/v1"
api_key_env = "MY_GW_API_KEY"
default_model = "my-model"
```

| Task | Command |
| --- | --- |
| Ask once and exit | `horon --no-update run "..."` |
| Limit turn processing to 30 seconds | `horon --no-update run --max-time 30 "..."` |
| Print shell completions | `horon completions bash` (also `zsh` and `fish`) |
| Estimate tokens in a UTF-8 file offline | `horon --model gpt-4o tokens --file input.txt` |
| Read a version's release notes without updating | `horon changelog 0.1.5` |
| Stay resident as a daemon | `horon serve` |
| Open the SDK socket | `horon api-bridge` |

The TypeScript and Python SDKs' `launch()` starts its own instance on Linux. There is no need to run `serve` first (`packages/sdk/typescript/README.md`).

### Release assets

| Asset | Build |
| --- | --- |
| `horon-agent-linux-x86_64` | Built against a glibc 2.17 baseline in a CentOS 7 / manylinux2014 container (`scripts/release/build_linux_compat.sh`). |
| `horon-agent-linux-aarch64` | Built on `ubuntu-24.04-arm` (`.github/workflows/release.yml`). |

The archive holds the wrapper, `<artifact>.bin` and the `LICENSE`; the OpenSSL libraries are no longer included. `scripts/install/install.sh` still accepts `libssl.so*` members so it can install older archives.

The shell installer verifies the checksum and checks every member name before preparing the bootstrap executable. On Linux the verified archive is expanded once into the temporary directory and both the listing check and the extraction read that image, but only when the filesystem holds twice the uncompressed length; otherwise the compressed archive is read again for each pass. An extraction out of that image that fails is retried once from the archive itself. When the bootstrap needs more than one file, GNU tar streams the selected files in one extraction pass. Other tar implementations stream one selected file at a time.

### CI guard

`scripts/ci/check_linux_bare_runtime.sh <binary>` runs the binary in `debian:trixie-slim`. It deletes the OpenSSL libraries and the CA store inside the container and checks that `ldd` lists only the allowed glibc family and libgcc entries. It then runs `--version` and one `run` turn against a mock provider. The Linux entry of the CI `build` job runs this script after the release-profile build. The same command can run on a developer machine that has Docker.

On pull requests, `.github/workflows/ci.yml` scopes crate compilation (`scripts/ci/workspace_impact.py`), TUI PTY smoke (`tui_smoke`), OS runners (running `ubuntu-latest` and the Windows cross-checks for portable Rust edits, adding `macos-latest` and `windows-build-test` when platform code, shared Linux-macOS `#[path]` sources, manifests, lockfiles or `ci:all-os` are present), and attach/detach soak length (`HORON_AGENT_SOAK_CYCLES=20`), while documentation-only pull requests (the generated `packages/docs-site/` pages count as documentation) run only `Documentation Checks`, the embedded `horon_agent_docs` corpus test and, for site pages, `Documentation Site`, and workflow-only pull requests skip Rust builds altogether while satisfying `CI Required` and `Structural performance required`. Pushes to `dev` that change more than documentation, and every push to `main`, run the full 3-OS suite, the 200-cycle soak, and every mandatory release job verified by `scripts/release/check_ci.py`. On `dev` a newer push waits for the running CI instead of cancelling it, so the next push is compared with that run's head.

A release pull request (base `main`) whose head is the head of the last successful `dev` push run of the same workflow, and whose merge commit differs from that head only in documentation, runs only the documentation gates: that push run already ran every gate on the same code. The merge commit is compared rather than `main`'s history, so a change `main` received by cherry-pick does not count. The push to `main` after the merge still runs everything.

## Release numbers and automatic publishing

`[package].version` in the root `Cargo.toml` is the single source of the version. It sets the release line (MAJOR.MINOR) and the lowest patch on that line, and tags number the later patches. No commit bumps the version.

| Step | What happens |
| --- | --- |
| main push | `ci.yml` checks that commit. |
| CI success | `release-tag.yml` picks the next tag with `scripts/release/next_version.py`. The patch is one above the highest tag on the same MAJOR.MINOR line and never below the `Cargo.toml` patch; a line with no tag starts at the `Cargo.toml` version. Commits already tagged and commits older than the latest release are skipped. |
| Tag creation | The same run creates the tag on that commit and calls `release.yml` through `workflow_call`. The remaining checks (CI success for the exact commit, draft, platform builds, `SHA256SUMS`, publishing, install verification) are the same as for a directly pushed tag. |
| Release notes | The draft body is rendered from the commit subjects since the previous tag, grouped by conventional-commit prefix and ended with the compare link (`scripts/release/release_notes.sh`). |
| Image publishing | The published release's Linux assets are verified against `SHA256SUMS`, packaged as a container image and pushed. |

`prepare_source.py` allows a build only when the tag shares MAJOR.MINOR with `Cargo.toml` and its patch is at least that value. A new MINOR or MAJOR needs only the root version in `Cargo.toml` and `Cargo.lock` bumped and merged to main. With the repository variable `RELEASE_AUTO` set to `false` no tag is created. A manual run of `release-tag.yml` reports the tag it would create and creates nothing.

### Rerunning a release

A published tag is never moved. To rebuild a release that stopped, dispatch the reviewed workflow from `dev` with the existing tag; the checkout is detached at that tag's commit and `prepare_source.py` refuses a mismatched tree, while `check_ci.py` still requires the successful push CI run for that exact commit.

```bash
gh workflow run release.yml --ref dev -f tag=vX.Y.Z
```

Every asset is uploaded with `--clobber`, so a platform that failed is replaced and the ones that succeeded stay. The release remains a draft until the full asset set exists. The container image has its own path that touches neither the tag nor the release: `gh workflow run release-tag.yml --ref dev -f republish_container=vX.Y.Z` verifies the tag, its exact-commit CI and the published assets, then rebuilds and pushes the image.

The update check only updates when the release version is higher than the current version. An installation built from 0.98.x therefore does not treat a 0.1.x release as newer. Such an installation is reinstalled once with `scripts/install/install.sh`.

## Container image

Each release pushes `linux/amd64` and `linux/arm64` images to `ghcr.io/horon-ai/agent` tagged `<version>`, `<MAJOR.MINOR>` and `latest`. GitHub Packages supports neither the cargo nor the PyPI registry, and the SDKs will ship through npm and PyPI rather than GitHub Packages, so the only thing published to GitHub Packages is the container that starts the daemon in headless mode. The image does not recompile anything and uses the release assets as they are (`scripts/release/Containerfile`).

```bash
docker run --rm ghcr.io/horon-ai/agent:latest --version
```

## Launcher aliases (`horon`, `hrn`)

`horon` and `hrn` are created in the same directory as the `horon-agent` launcher. The aliases are installed from v0.1.3 on; docs and examples use `horon`, and `hrn` is the shorter form. All three point at the same channel executable, as symbolic links on Unix and as copies on Windows (`crates/build-support/src/paths.rs` `update_launcher_symlink`). They change with the launcher on every update. On Termux, `install.sh` wires the aliases as wrappers that clear `LD_PRELOAD` as well.

- A different program with the same name is never overwritten. On Unix only a symbolic link pointing at `horon-agent` counts as ours, on Windows only a copy whose bytes match the previous launcher; when another file is present the install finishes without aliases.
- `uninstall.sh` and `uninstall.ps1` remove only aliases by the same rule.
- Help and `--version` print `horon-agent` whatever name the command was run as (`#[command(name = "horon-agent")]`).
- `--isolated-profile <name>` selects an isolated local profile: `$HORON_HOME/profiles/<name>` holds its configuration, credentials and sessions, and its daemon and API sockets live in their own runtime directory. Names are 1-32 lowercase letters, digits or hyphens. `--isolated-profile` cannot be combined with `--socket` or `--ssh`, and a launch without it keeps the existing `HORON_HOME` and socket variables. `horon --isolated-profile work --show-profile` prints the paths and the alias `alias horon-work='horon --isolated-profile work'`; Horon never edits shell files. The terminal shows `profile: <name>` at start.
- The data directory is `~/.horon` (`HORON_HOME`) and in-project settings live in `.horon/`. Releases up to v0.1.1 used `~/.horon-agent`, and the current code does not read that path.
