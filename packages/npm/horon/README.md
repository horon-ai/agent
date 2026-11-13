# `@horon-ai/horon`

The Horon CLI: an autonomous agent runtime with a terminal client.

```sh
npm install -g @horon-ai/horon
horon --version
horon            # starts the terminal client on a TTY
```

The `horon` and `hrn` commands are the same launcher. `horon` with no arguments
opens the terminal client; every other subcommand runs the agent without one
(`horon --help` lists them). `HORON_NO_TUI=1` skips the client on a TTY and
prints the reason instead.

## How the install works

This package holds no binaries. It depends optionally on one package per
platform and runs the one that matches the machine:

| Package | Platform |
| --- | --- |
| `@horon-ai/horon-linux-x64` | Linux x86_64 |
| `@horon-ai/horon-linux-arm64` | Linux aarch64 |
| `@horon-ai/horon-darwin-x64` | macOS Intel |
| `@horon-ai/horon-darwin-arm64` | macOS Apple Silicon |
| `@horon-ai/horon-win32-x64` | Windows x86_64 |
| `@horon-ai/horon-win32-arm64` | Windows ARM64 |

Optional dependencies are how npm expresses a platform-specific package, so a
global install only downloads the matching one. If npm was run with
`--omit=optional`, or the platform is not in the table, the launcher exits 1 and
names the missing package; reinstall with `npm install -g --include=optional
@horon-ai/horon`.

The launcher sets `HORON_AGENT_NO_AUTO_UPDATE=1` for the binary it starts. The
npm package pins one release for both the agent and `horon-tui`, and the
binary's own updater would replace them with a different build.

## Not a Node library

Nothing here is importable API. The CLI is the Rust `horon-agent` and
`horon-tui` binaries; this package only finds and starts them.

Releases and the full documentation live at
[github.com/horon-ai/agent](https://github.com/horon-ai/agent).
