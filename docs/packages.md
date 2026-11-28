# Packages (extensions and plugins)

A package adds tools, commands and event observers through a Rust executable that runs as a separate child process under your user account. Nothing is loaded into the agent process.

| Kind | Root file | Carries |
| --- | --- | --- |
| Extension | `extension.toml` | The executable |
| Plugin | `plugin.toml` | One bundled `extension.toml`, resource files, the executable |

## Manage packages

| Command | Effect |
| --- | --- |
| `horon package install <dir> [--project <root>] [--replace]` | Validate, copy, register; installing is consent to run it with your permissions |
| `horon package list [--kind extension\|plugin]` | Source, version, scope, kind, state |
| `horon package verify <id>` | Re-check the recorded SHA-256 |
| `horon package disable <id>` | Keep the copy, stop loading it |
| `horon package remove <id>` | Remove the record |

A folder dropped into `<home>/packages/inbox/<id>/` is installed by a running daemon the same way.

## What happens at run time

| Case | Result |
| --- | --- |
| A package is installed while a session is open | The session sees it from its next turn |
| A package tool is called | Approved like any tool, then the child process starts (or is reused) after the `extension.toml` digest is checked |
| The digest no longer matches | The child is not started |
| A hosted shared worker | Package children are refused ([hosting.md](hosting.md)) |
| A `core` session | Package tools are listed behind `toolbox` like other non-eager tools |

## Writing one

Use the author SDK crate `horon-agent-extension-sdk`; `tests/fixtures/package-example` in the source repository is a complete package. The protocol, targets, signing and limits are in the developer reference at https://agent.horon.ai/reference/runtime/packages.
