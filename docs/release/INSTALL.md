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

## How the install scripts use it

`scripts/install/install.sh` and `scripts/install/install.ps1` fetch the release assets and `SHA256SUMS` into a temporary directory and then call this command. `scripts/install/install_from_source.sh` calls the same command with `--source-binary` to install a local build. The shell and PowerShell installers therefore never produce different layouts, and a failure leaves the previous installation alone.

The Windows install locations and signing procedure are in [`WINDOWS.md`](WINDOWS.md).

## Installing from npm (not published yet)

The npm distribution is built and tested, but nothing has been published: the registry holds only the `0.0.0` placeholder packages that reserve the seven names, and those carry no binaries, so `npm i -g @horon-ai/horon` installs a launcher that cannot find a runtime. Use the installers above, the container image, or a source checkout until publishing is enabled.

When it is, `@horon-ai/horon` installs the same two binaries without a release download: one small launcher and one platform package pinned to the same version, chosen through `optionalDependencies`, so a global install fetches only the one that matches the machine.

```bash
npm i -g @horon-ai/horon
npx @horon-ai/horon --help
```

An npm installation is updated with npm, not with the agent's own updater: the launcher sets `HORON_AGENT_NO_AUTO_UPDATE=1` for the binary it starts, because the package pins one release for both binaries. Package layout, the error a missing platform package prints, the version rule, and the publish procedure are in [`NPM.md`](NPM.md).

## The optional terminal client

The release also publishes the interactive client as its own archive beside the agent: `horon-tui-<suffix>.tar.gz`, with the same platform suffix the agent asset uses (`horon-tui-linux-x86_64.tar.gz`, `horon-tui-macos-aarch64.tar.gz`, `horon-tui-windows-x86_64.tar.gz`, and so on). `release_asset.rs` derives the name from the agent asset, and the suffix is what ties the two together.

`scripts/install/install.sh` and `scripts/install/install.ps1` fetch the archive into the same temporary directory as the agent asset and check it against `SHA256SUMS` before handing the directory to `install-release`. The staged client is placed in `builds/versions/<version>/` beside `horon-agent`, and the `LICENSE` each archive carries is installed there too, so one version directory holds both binaries with the licence that covers them and activation swaps them together. Installing the client is documented in [`../runtime/TUI.md`](../runtime/TUI.md).

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

No TTY, display or `XDG_RUNTIME_DIR` is required. The socket directory is taken from `HORON_AGENT_RUNTIME_DIR`, then `XDG_RUNTIME_DIR`, then the platform's per-user runtime directory, and when none is set the binary uses `horon-agent-<user discriminator>` under the temporary directory (`crates/harness-api/src/sockets.rs:16-31`). The discriminator is read from `UID`, then `USER`, keeping only alphanumerics, `-` and `_` and cutting to 64 characters. If nothing is left, it is `user`.

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
| Stay resident as a daemon | `horon serve` |
| Open the SDK socket | `horon api-bridge` |

The TypeScript and Python SDKs' `launch()` starts its own instance on Linux. There is no need to run `serve` first (`packages/sdk/typescript/README.md`).

### Release assets

| Asset | Build |
| --- | --- |
| `horon-agent-linux-x86_64` | Built against a glibc 2.17 baseline in a CentOS 7 / manylinux2014 container (`scripts/release/build_linux_compat.sh`). |
| `horon-agent-linux-aarch64` | Built on `ubuntu-24.04-arm` (`.github/workflows/release.yml`). |

The archive holds the wrapper, `<artifact>.bin` and the `LICENSE`; the OpenSSL libraries are no longer included. `scripts/install/install.sh` still accepts `libssl.so*` members so it can install older archives.

### CI guard

`scripts/ci/check_linux_bare_runtime.sh <binary>` runs the binary in `debian:trixie-slim`. It deletes the OpenSSL libraries and the CA store inside the container and checks that `ldd` lists only the allowed glibc family and libgcc entries. It then runs `--version` and one `run` turn against a mock provider. The Linux entry of the CI `build` job runs this script after the release-profile build. That step runs only on GitHub-hosted runners (with `CI_SELF_HOSTED` set, only the macOS entry remains). The same command can run on a developer machine that has Docker.

## Release numbers and automatic publishing

`[package].version` in the root `Cargo.toml` is the single source of the version. It sets the release line (MAJOR.MINOR) and the lowest patch on that line, and tags number the later patches. No commit bumps the version.

| Step | What happens |
| --- | --- |
| main push | `ci.yml` checks that commit. |
| CI success | `release-tag.yml` picks the next tag with `scripts/release/next_version.py`. It adds one patch to the highest tag on the same MAJOR.MINOR, or uses the `Cargo.toml` version when the line has no tag. Commits already tagged and commits older than the latest release are skipped. |
| Tag creation | The same run creates the tag on that commit and calls `release.yml` through `workflow_call`. The remaining checks (CI success for the exact commit, draft, platform builds, `SHA256SUMS`, publishing, install verification) are the same as for a directly pushed tag. |
| Image publishing | The published release's Linux assets are verified against `SHA256SUMS`, packaged as a container image and pushed. |

`prepare_source.py` allows a build only when the tag shares MAJOR.MINOR with `Cargo.toml` and its patch is at least that value. A new MINOR or MAJOR needs only the root version in `Cargo.toml` and `Cargo.lock` bumped and merged to main. With the repository variable `RELEASE_AUTO` set to `false` no tag is created. A manual run of `release-tag.yml` reports the tag it would create and creates nothing.

The update check only updates when the release version is higher than the current version. An installation built from 0.98.x therefore does not treat a 0.1.x release as newer. Such an installation is reinstalled once with `scripts/install/install.sh`.

## Container image

Each release pushes `linux/amd64` and `linux/arm64` images to `ghcr.io/horon-ai/agent` tagged `<version>`, `<MAJOR.MINOR>` and `latest`. GitHub Packages supports neither the cargo nor the PyPI registry, and the SDKs will ship through npm and PyPI rather than GitHub Packages, so the only thing published to GitHub Packages is the container that starts the headless daemon. The image does not recompile anything and uses the release assets as they are (`scripts/release/Containerfile`).

```bash
docker run --rm ghcr.io/horon-ai/agent:latest --version
```

## Launcher aliases (`horon`, `hrn`)

`horon` and `hrn` are created in the same directory as the `horon-agent` launcher. The aliases are installed from v0.1.3 on; docs and examples use `horon`, and `hrn` is the shorter form. All three point at the same channel executable, as symbolic links on Unix and as copies on Windows (`crates/build-support/src/paths.rs` `update_launcher_symlink`). They change with the launcher on every update. On Termux, `install.sh` wires the aliases as wrappers that clear `LD_PRELOAD` as well.

- A different program with the same name is never overwritten. On Unix only a symbolic link pointing at `horon-agent` counts as ours, on Windows only a copy whose bytes match the previous launcher; when another file is present the install finishes without aliases.
- `uninstall.sh` and `uninstall.ps1` remove only aliases by the same rule.
- Help and `--version` print `horon-agent` whatever name the command was run as (`#[command(name = "horon-agent")]`).
- The data directory is `~/.horon` (`HORON_HOME`) and in-project settings live in `.horon/`. Releases up to v0.1.1 used `~/.horon-agent`, and the current code does not read that path.