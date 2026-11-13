---
title: Install
description: Install horon agent from the release installer, npm, a container image, or a source checkout.
section: Start
order: 2
---

## Installer

**macOS · Linux**

```sh
curl -fsSL https://raw.githubusercontent.com/horon-ai/agent/main/scripts/install/install.sh | bash
```

**Windows (PowerShell)**

```powershell
irm https://raw.githubusercontent.com/horon-ai/agent/main/scripts/install/install.ps1 | iex
```

The installer downloads the release archive and `SHA256SUMS`, then hands them to
`horon install-release`, which verifies the checksum, stages an immutable version
directory and switches the `stable`/`current` links. A version directory that
already holds different bytes is refused, not overwritten.

Three launchers are linked: `horon` (used throughout these docs), `hrn` (shorter)
and `horon-agent` (the executable's own name).

## npm

The npm distribution is built and tested, but no release has been published yet:
the registry currently holds only the `0.0.0` placeholder packages that reserve
the seven names, and those carry no binaries. Use the installer or a source
checkout until `NPM_PUBLISH` is turned on.

When it is on, the install is:

```sh
npm i -g @horon-ai/horon
horon --version
```

The npm package is a small launcher plus one platform package carrying the
`horon-agent` and `horon-tui` binaries, chosen through `optionalDependencies`.
An npm installation is updated with npm, not with the agent's own updater: the
launcher sets `HORON_AGENT_NO_AUTO_UPDATE=1` for the binary it starts, because
the package pins one release for both binaries.

## Container

```sh
docker run --rm ghcr.io/horon-ai/agent:latest --version
```

## From source

Rust 1.99+ with edition 2024:

```sh
cargo build --release -p horon-agent
./target/release/horon-agent --version
```

## Platforms and data

Release assets cover Linux (x86_64, aarch64), macOS (aarch64, x86_64), Windows
(x86_64, aarch64) and FreeBSD (x86_64). Data lives in `~/.horon`, overridden with
`HORON_HOME`; releases up to v0.1.1 used `~/.horon-agent`.

## Linux servers and TLS

The release binary starts with only glibc, libm and libgcc_s present. All TLS goes
through rustls, so OpenSSL is not a dependency. Trusted roots are the union of the
webpki roots built into the binary and the roots the OS trusts, so a private CA
installed in the OS store is trusted too. No setting narrows the trust set or
disables verification; to use an internal CA, put it in the OS store.

## The optional terminal client

The release publishes the interactive client as its own archive
(`horon-tui-<suffix>.tar.gz`) beside the agent, verified against the same
`SHA256SUMS`. A copy that cannot be verified never blocks the agent: the installer
prints one line and continues with `horon-agent` alone.
