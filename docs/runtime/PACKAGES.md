# Rust packages

Language: **English** | [한국어](PACKAGES.ko.md)

A user-installed package is one Rust executable plus declarations. An extension
ships `extension.toml`; a plugin ships `plugin.toml` that bundles one
`extension.toml` with resource files. The host starts the installed executable as
a child process and speaks one newline-delimited JSON-RPC contract with it: the
package receives tool calls, executable commands and the session events it
declared. The authoring SDK and the worked example are not dependencies of the
distributed `horon-agent` binary; they exist so an author can build, package and
test a package without the engine's internal ABI.

## Status

On `dev`, the install command registers both package kinds. A running daemon
reconciles completed inbox folders and publishes catalog generations. The child
supervisor runs installed copies on Linux, macOS and Windows. A session captures
one snapshot per turn, so a running turn keeps the generation it started with
while the next turn and new sessions observe the newest one; a disable or removal
refuses new loads at once and runs the callbacks registered for the generation
that is being revoked. Tool, text, MCP and command surfaces are later steps; the
observation events an installed package and its bundled hook files subscribe to
are delivered now.

| Surface | State | Check |
| --- | --- | --- |
| Declaration schema and native target check | Implemented | `cargo test -p horon-agent-base --test package_manifest` |
| Installed copy and catalog | Implemented | `cargo test -p horon-agent-base --test package_catalog` |
| Wire contract, framing and deadlines | Implemented | `cargo test -p horon-agent-app-core --lib extensions::wire::tests` |
| Author SDK: framing, negotiation, cancellation | Implemented | `cargo test -p horon-agent-extension-sdk` |
| Worked example in both layouts | Implemented | `cargo test -p horon-package-example --test extension_layouts` |
| `horon package install` and the inbox reconciler | Implemented | `cargo test --locked --test package_cli` |
| Child supervision on Linux and macOS | Implemented | `cargo test --locked -p horon-agent-base --test package_spawn` |
| Child supervision on Windows | Implemented | `cargo test --locked -p horon-agent-base --test package_spawn_windows` on `.github/workflows/package-windows.yml` (x64 and arm64) |
| Daemon catalog control and automatic inbox reconciliation | Implemented | `cargo test --locked --test harness_roundtrip package_daemon` |
| Session snapshot and revocation | Implemented | `cargo test --locked -p horon-agent-app-core --test package_session` |
| Package observation events and bundled hook files | Implemented | `cargo test --locked -p horon-agent-app-core --lib extensions::observer::tests` |
| Package tools, text, MCP and commands | Pending | Follow-up changes on the same launch path |

## Two package kinds

Both kinds run one Rust binary and declare the same tool, command and event
names. A plugin adds resource files that the host supplies to a session.

| Kind | Root declaration | Carries |
| --- | --- | --- |
| Extension | `extension.toml` | the executable only |
| Plugin | `plugin.toml` | one bundled `extension.toml`, resource files, the executable |

## The author SDK

`horon-agent-extension-sdk` depends only on the peer contract crate, `serde`,
`serde_json`, `sha2` and `toml`. It does not depend on the agent runtime,
`app-core` or the terminal client, and it is not published. Add it while the
package lives beside the engine:

```toml
[dependencies]
horon-agent-extension-sdk = { path = "../agent/crates/extension-sdk" }
serde_json = "1"
```

The program is one handler and one call. `main` reads the package root the host
passes as its first argument, loads the declaration, checks that the code and the
declaration name the same surface, and serves stdio. The tool, executable command
and event observer of the worked example are in
[src/main.rs](../../examples/packages/example/src/main.rs):

```rust
use std::process::ExitCode;

fn main() -> ExitCode {
    let root = std::env::args_os().nth(1).expect("the host passes the package root");
    let declaration = Declaration::load(root).expect("the installed declaration");
    declaration.check(&Example).expect("the code matches the declaration");
    serve_stdio(Example, declaration).expect("the host closed the stream");
    ExitCode::SUCCESS
}
```

## The declaration and its digest

The declaration is the package's identity: id, version, display name, protocol,
the native targets that ship it, and the tool, command and event names. The
installer validates it once and the host compares the child's digest with the
installation record on every start. The digest is the SHA-256 of the raw
`extension.toml` bytes, lowercase hex, so a standalone extension and a plugin that
bundles identical bytes report the same value:

```toml
schema_version = 1
id = "hello"
version = "0.1.0"
name = "Hello package example"
protocol = "1.0"
events = ["turn.start", "turn.end"]

[targets]
aarch64-apple-darwin = "bin/aarch64-apple-darwin/horon-package-example"
```

A package whose code and declaration disagree never starts. The SDK refuses a
declared tool with no handler and a handler whose name is not declared, and it
applies the same rule to commands and observed events, so the failure names the
mismatch instead of answering a call the declaration never promised.

## Protocol

Everything is JSON-RPC 2.0, one UTF-8 object per line, in both directions.
stdout carries frames and nothing else; diagnostics go to stderr. The child
answers requests and reports observations, and the host never writes an answer:

| Method | Direction | Answer |
| --- | --- | --- |
| `initialize` | host to child | protocol range, capabilities, declaration digest |
| `tool/call` | host to child | the tool's JSON value |
| `command/invoke` | host to child | the executable command's JSON value |
| `observer/notify` | both | none; it carries no id |
| `cancel` | host to child | none; the cancelled call answers nothing |
| `shutdown` | host to child | one empty result, then the child exits |

`initialize` is the only request the child answers with state. The offer names
the package id and kind; the answer reports the version both sides share, the
capabilities both offered and the digest above:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":{"min":"1.0","max":"1.0"},"capabilities":["tool/call","command/invoke","observer/notify"],"package_id":"hello","kind":"extension"}}
```

Refusals use JSON-RPC codes so a host logs one line instead of guessing: `-32001`
when no protocol version is shared, `-32002` for a request before `initialize`
settles, `-32600` for a malformed frame, `-32602` for parameters that are not the
declared shape and `-32601` for a method or name the connection never negotiated.

## Cancellation

A `cancel` notification names one request id. The SDK reads frames on its own
thread, so the token flips while the handler runs; a long call polls
`Context::is_cancelled` and returns early. The server then sends nothing for that
id: the host already stopped waiting, and a late answer would be unregistered. A
cancel for a call that has not started keeps it from starting at all. A stream
that ends while a call is running cancels that call and starts no later one, so
a handler never waits for a cancellation the closed connection cannot deliver.

## Layouts

The worked example packages one binary twice, and both layouts carry the same
`extension.toml` bytes so one digest covers both:

```text
package/                     plugin root
  plugin.toml                names the bundled extension
  extension/extension.toml   byte-identical to the standalone declaration
  extension/bin/<target>/    the executable
  resources/                 skills, commands, mcp, rules, prompts
```

The standalone layout is the directory holding `extension.toml` and its own
`bin/<target>/`. Installation copies declared files only, checks every path
against the package root and rejects links.

## Targets

The six supported keys are the release targets. Declare one entry per target that
ships a binary; the installer refuses a missing artifact, a fat Mach-O, a wrong
CPU, a file without the executable bit and a Windows binary without its `.exe`
suffix.

| Target key | Platform |
| --- | --- |
| `aarch64-apple-darwin` | macOS on Apple silicon |
| `x86_64-apple-darwin` | macOS on Intel |
| `aarch64-unknown-linux-gnu` | Linux on arm64 |
| `x86_64-unknown-linux-gnu` | Linux on x86-64 |
| `aarch64-pc-windows-msvc` | Windows on arm64 |
| `x86_64-pc-windows-msvc` | Windows on x86-64 |

From the repository root, build the example for the target you distribute and
stage its declaration beside the executable:

```sh
cargo build --locked --release -p horon-package-example --target aarch64-apple-darwin
mkdir -p package/bin/aarch64-apple-darwin
cp examples/packages/example/layouts/extension.toml package/
cp target/aarch64-apple-darwin/release/horon-package-example package/bin/aarch64-apple-darwin/
```

To stage the plugin layout from the same build, put the standalone directory
under `extension/` and add the plugin declaration and resources:

```sh
mkdir -p plugin
cp -R package plugin/extension
cp examples/packages/example/layouts/plugin.toml plugin/
cp -R examples/packages/example/layouts/resources plugin/
```

The installer verifies every declared target, not only the host's, so a staged
package must hold a binary for each row of the table above. Build and stage all
six, or trim `[targets]` to the targets you ship before installing; the digest
is recomputed from whichever declaration you install.

## Signing and notarization

A package runs with the user's file and network permissions. On macOS, sign the
executable with Developer ID and the hardened runtime. Submit a disk image for
notarization and attach the ticket to that image after the status is `Accepted`.
See [Apple's notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).

```sh
codesign --force --options runtime --timestamp --sign "Developer ID Application: Example (TEAMID)" package/bin/aarch64-apple-darwin/horon-package-example
hdiutil create -srcfolder package -format UDZO package.dmg
xcrun notarytool submit package.dmg --keychain-profile horon --wait
xcrun stapler staple package.dmg
```

On Windows sign the executable with Authenticode before packaging
(`signtool sign /fd SHA256`). Linux packages are unsigned, and the catalog's
SHA-256 is the integrity check. The release steps for the agent itself are in the
[install guide](../release/INSTALL.md); a package is signed and shipped by its
author, not by that release.

## Installing and managing

`horon package install <dir>` and the inbox reconciler use the same catalog
installer. It validates, copies and registers the package. Installation is
consent to run the package with the user's permissions. The catalog records
SHA-256 values for integrity checks. A running daemon reconciles the inbox and
publishes new catalog generations. Applying them to open sessions is a later
step.

| Command | Effect |
| --- | --- |
| `horon package install <dir> [--project <root>] [--replace]` | validate, copy and register |
| `horon package list [--kind extension\|plugin]` | show source, version, scope, kind and state |
| `horon package verify <id>` | re-check the recorded bytes |
| `horon package disable <id>` | keep the copy and stop loading it |
| `horon package remove <id>` | revoke the record and keep user-held originals |

## Starting the installed executable

The launcher retains a lease on the installed copy and checks the recorded
executable immediately before starting it. The child starts from that copy,
with its absolute package root as the first argument. The launcher clears the
environment and supplies these values.

| Variable | Value |
| --- | --- |
| `HORON_PACKAGE_ROOT`, `HORON_PACKAGE_ID`, `HORON_PACKAGE_KIND` | Installed root, package ID, and package kind |
| `HORON_PACKAGE_STATE`, `HOME` | The package's owner-private state directory |
| `TMPDIR` (Unix) | The `tmp` directory under package state |
| `USERPROFILE`, `TEMP`, `TMP` (Windows) | The state directory, and `tmp` under it for the two temporary paths |
| `PATH`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TZ` (Unix); `PATH`, `PATHEXT`, `SYSTEMROOT`, `SYSTEMDRIVE`, `WINDIR`, `COMSPEC`, `NUMBER_OF_PROCESSORS`, `PROCESSOR_ARCHITECTURE`, `OS` (Windows) | Inherited when set |

Provider keys and other host variables are omitted. The child retains the file
and network permissions of the user's account.

One value owns the child's lifetime, and every way of letting go of it stops the
whole tree. On Linux and macOS the child starts in its own process group, and
the group is signalled while the leader is still unreaped, so a descendant that
outlived the leader is still reached. On Windows the child starts suspended in a
dedicated Job Object, is assigned to it, and only then resumes. The job is
configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and with neither breakaway
flag, so no package code runs before the job owns it, nothing in the package can
leave the job, and closing the handle terminates every member, whether it closes
by cancellation, by dropping the owner, or by the host exiting. Reporting an
exit means the tree was swept first. A host whose Job Object cannot take another
process, or a Windows without nested jobs, cannot own a child: the launch is
refused with a diagnostic rather than starting a process the host could not
stop. While the child starts, the launcher holds the installed executable open
read-only, which denies any concurrent write, rename or delete of that image on
Windows. The launch is not a sandbox: the child still runs with the user's
permissions.

## The example

`examples/packages/example` is the worked example: a tool (`greet`), an
executable command (`hello`) and an observer for `turn.start` and `turn.end`, with
the plugin resources and both layouts. Its test starts the built binary under each
layout over real pipes:

```sh
cargo test --locked -p horon-package-example --test extension_layouts
cargo run --locked -p horon-package-example -- package
```

## Limits

| Limit | Value |
| --- | --- |
| One frame, newline included | 1 MiB |
| Declaration file | 256 KiB |
| Queued frames in either direction | 128 |
| Pending requests the host waits for | 64 |
| Observer reports one call may queue | 128 |
