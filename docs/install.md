# Install and update

## Choose a method

| Situation | Command |
| --- | --- |
| macOS, Linux, FreeBSD | `curl -fsSL https://raw.githubusercontent.com/horon-ai/agent/main/scripts/install/install.sh \| bash` |
| Windows (PowerShell) | `irm https://raw.githubusercontent.com/horon-ai/agent/main/scripts/install/install.ps1 \| iex` |
| You already use npm | `npm i -g @horon-ai/horon` (a launcher plus one platform package with both binaries) |
| Container | `docker run --rm ghcr.io/horon-ai/agent:latest --version` (headless daemon image) |
| From source | `cargo build --release -p horon-agent` (Rust 1.99+, edition 2024); `cargo build --release -p horon-tui` for the client |
| A downloaded release or a local build | `horon install-release --assets <dir> --agent-version <version>`; add `--source-binary <path>` to install a local build the same way |

Release assets cover Linux (x86_64, aarch64), macOS (aarch64, x86_64), Windows (x86_64, aarch64) and FreeBSD (x86_64). FreeBSD has no `horon-tui`, so it runs headless only.

## What the installer does

1. Downloads the release archive and `SHA256SUMS`.
2. `horon install-release` verifies the checksum, stages an immutable version directory under `<home>/builds/versions/<version>/`, and switches the `stable` and `current` links.
3. Links three launchers: `horon`, `hrn`, `horon-agent`. A different program with the same name is never overwritten.
4. Fetches `horon-tui` from the same release when one exists. A client that cannot be verified prints one line and the agent installs alone.

| Case | Result |
| --- | --- |
| The version directory already holds the same bytes | Reused |
| It holds different bytes | Refused, never overwritten |
| The archive has links, absolute paths, `..`, duplicates, or exceeds 512 MiB per file / 2 GiB total | Refused |
| Same release reinstalled with `install.sh` | Uses the cached archive when this run's `SHA256SUMS` lists the same digest; `HORON_AGENT_NO_DOWNLOAD_CACHE=1` disables the cache |

## Update

| Installed with | Update with |
| --- | --- |
| Installer or `install-release` | `horon update` |
| npm | `npm i -g @horon-ai/horon`; the launcher sets `HORON_AGENT_NO_AUTO_UPDATE=1` so the agent's own updater stays off |
| Container | Pull a newer image tag |

After an upgrade, run `horon server reload` so live sessions move to the new binary instead of being dropped. `horon changelog [VERSION]` shows release notes without updating. `horon serve` checks for updates only when asked; `--no-update` skips it everywhere.

## Linux servers

The release binary needs only glibc, libm and libgcc_s. TLS uses rustls with the union of the built-in webpki roots and the OS trust store, so a private CA added to the OS store is trusted. Verification cannot be turned off.

## Uninstall

`uninstall.sh` honors `HORON_HOME`, stops only daemons of that installation, and removes only launcher links that point at it.

## Data location

Data lives in `~/.horon`, or `HORON_HOME`. Releases up to v0.1.1 used `~/.horon-agent`.
