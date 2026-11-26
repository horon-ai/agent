# Security

Language: **English** | [한국어](SECURITY.ko.md)

Workspace trust gates, hosted MCP and extension isolation, local crash bundles, and dependency security classification.

## Workspace trust

A project repository can name MCP servers for the agent to run. So horon agent does not read a workspace's project-local configuration without consent for its contents. This document explains the gate's scope, the digest rules, the fail-closed behavior when no client is attached, and the overrides in the current tree.

### What is gated

Three files in the workspace are gated (`crates/base/src/workspace_trust.rs:22` `PROJECT_CONFIG_SURFACES`).

- `.horon/mcp.json`
- `.mcp.json`
- `.claude/mcp.json`

User-level configuration is not gated. `~/.horon/mcp.json`, `~/.claude.json`, and `~/.claude/mcp.json` are the user's own configuration, not something a repository decides. Those Claude Code files are read only when `[mcp] claude_user` is on, `~/.codex/config.toml` only when `[mcp] codex_user` is on, and the per-project entries in `~/.claude.json` are matched to the session's project directory, so one repository's entry never reaches another (`crates/base/src/mcp/sources.rs:1-14`).

There is a single enforcement point. When `McpConfig` reads project-local configuration, `load_authorized_project_locals` calls `authorize_project_config` and parses only the bytes of the authorized snapshot (`crates/base/src/mcp/protocol.rs:706`, `workspace_trust.rs:267`). If all three files are absent there is nothing to gate, so the result is `Absent`; if the decision is denial, it logs one warning line and does not load project configuration.

### Digest rules

The snapshot reads the three surfaces in a fixed order and computes SHA-256 (`workspace_trust.rs:162` `snapshot`).

1. It starts with the domain separator `horon-agent/workspace-trust/v1`.
2. For each surface it adds the path, the state, and a content fingerprint.
3. The state is one of `absent`, `present`, `unreadable`. `present` includes the byte length together with that file's SHA-256.

Consent is recorded as the pair of the normalized workspace path and this digest. The record file is `workspace-trust.json` in the agent directory (`workspace_trust.rs:439` `trust_file_path`), and nothing is written into the workspace. An entry carries version 1, the digest, the decision, the recording time, and the per-surface SHA-256 and length.

- If any of the three files changes even one byte, the digest differs and that workspace is asked for trust again. Creating or deleting a file has the same effect.
- The denial reason names the changed surface as precisely as it can (`workspace_trust.rs:313` `untrusted_reason`).
- The record is read and rewritten as a whole file, so if two processes write at the same moment one decision can be lost. What is lost is one decision; an allowance that did not exist never appears (`workspace_trust.rs:231` `record`).
- The digest is not a mechanism that makes a repository trustworthy. It binds consent to the contents the user saw.

### Fail closed without a client

When no client is attached there is nobody to ask. So an unknown state is denial, not allowance.

- With no record for the current digest the result is `Skipped`. It logs a warning, does not load project-local configuration, and records no decision. The next run asks again.
- With a record but a different digest the result is `Skipped`. The warning names the changed surface.
- If a surface cannot be read as a regular file, the result is `Skipped` regardless of overrides (`workspace_trust.rs:267`).
- If the recorded decision is denial, the result is `Skipped`.
- If all three files are absent the result is `Absent` and the gate does not engage.

Denial is closure, not failure. The session keeps running and only the MCP servers the project named are dropped.

When `[tools] bash_direnv` is enabled, local shells also require an explicit Horon trust decision before loading a direnv file inside the session workspace. A matching stored decision, `--trust-workspace`, or `HORON_AGENT_WORKSPACE_TRUST=trust` allows the direnv check. `HORON_AGENT_WORKSPACE_TRUST=deny` takes precedence, and direnv must independently approve the file before execution. Horon keeps the direnv output and environment values out of diagnostics.

### Overrides

| Means | Scope | Recording |
| --- | --- | --- |
| `--trust-workspace` | This run | Records trust for the current digest. |
| `HORON_AGENT_WORKSPACE_TRUST=trust` | This process | Records nothing. |
| `HORON_AGENT_WORKSPACE_TRUST=deny` | This process | Records nothing, and denies even when a stored trust exists. |
| Explicit commands such as `mcp test` and `mcp list` | That command | Records nothing. The command itself is the consent. |

The environment variable is case-insensitive and recognizes only `trust` and `deny`. Any other value is ignored and the stored decision applies (`workspace_trust.rs:372` `env_decision`). An environment denial is applied before `--trust-workspace`, so if you pass both, the denial wins.

## Hosted MCP and extension isolation

A hosted deployment gives one customer one process, either a container or a Fly
Machine with that customer's volume mounted as its home. The daemon runs under
one of the modes in [hosting](../architecture/HOSTING.md). The mode decides which
MCP servers and child processes it may run for a customer, and one module makes
that decision (`crates/base/src/mcp/hosted.rs`).

| Mode | MCP stdio child | Package or extension child | Editor-supplied stdio server | Shared-pool stdio connect | Remote MCP connection |
| --- | --- | --- | --- | --- | --- |
| No hosting (the ordinary process) | allowed | allowed | allowed | allowed | allowed |
| `per-user-container` | allowed | allowed | allowed | allowed | allowed |
| `per-user-container` with `[hosting] allow_shared_children = false` | refused | refused | refused | refused | allowed |
| `shared-worker` | refused | refused | refused | refused | refused |

- A shared worker refuses all five, and `horon mcp list` is refused with `HOSTED_POLICY` (exit `1`). A child process and the remote credential store would belong to one customer while the process serves every customer, so there is no per-user environment, process tree or token to prove.
- A per-user container or Fly Machine keeps its own servers: the home volume, the OAuth store under it, and the process environment a `${VAR}` expansion reads are that customer's own. `allow_shared_children = false` closes its child processes and leaves remote connections alone, because a remote connection starts none.
- `[hosting] allowed_mcp_servers` narrows the MCP servers a hosted process may connect: absent leaves all of them reachable, an empty list reaches none, and the match is exact against the name a source or an editor registered. It never widens a mode, and a process without hosting is unchanged, so a key left over on a reused volume does not take the operator's own servers away.
- Package children pass one gate (`crates/base/src/packages/child_gate.rs`, called from `PackageExecutable::start_with_arguments`), so a package's MCP stdio server, its extension child, its declared commands and its observers are all covered by the same decision.

A refusal is explicit and recorded once. The caller gets a typed error that names
the policy and the operation, and the daemon writes one audit event:

**`HOSTED_POLICY`** carries `policy` (`shared-worker`, `per-user-container`, or
`unresolved`), `operation` (`mcp-stdio`, `session-mcp-stdio`,
`pooled-mcp-stdio`, `mcp-remote`, `package-child`), `subject` (the server or
package name, `-` when the gate knows none), `reason` (`shared-worker`,
`children-disabled`, `not-allowed`, `unresolved-policy`) and `tenant` (the audit
label, `-` outside hosting). It never carries a command argument, an environment
value, a header, a URL or a token.

A refused package MCP server is also queued for the session's next turn, so the
model can tell the user why the tools are missing instead of the refusal ending
as a log line. An editor whose server list is refused receives the request error
itself, because the whole list is refused rather than silently reduced.

Limits: this gate covers MCP servers and package children only. `bash`, `eval`
and hook commands are outside it; a hosted deployment keeps those behind the
per-user boundary from [hosting](../architecture/HOSTING.md) (a container or a
Fly Machine, a dedicated `HORON_HOME`, a separate runtime directory, and no TCP
gateway), not behind this gate.

## Python eval

A Python cell takes the same approval tier as `bash`. The tool declares process, write and network effects, so `[tools] approval_mode = "yolo"` and `"off"` start a cell without asking, while `"write"` and `"all"` ask first. A scoped grant answers a cell the mode would otherwise ask about, the same way it answers a shell command. `[tools.bash_patterns]` reads a `bash` command, so it does not inspect Python code, and `[tools] restricted_paths` matches the paths a call names, which a cell does not. What a cell can do is bounded by the OS permissions of the agent user, not by those rules: a cell that runs under an auto-approving mode runs with the user's filesystem, network and process access, including every credential file the user can read. The kernel inherits only selected environment variables. `[tools] eval = false` removes the tool entirely, `[tools.load_mode] eval = "off"` removes it from one profile, and a build with no Python 3.10 or later on `PATH` never registers it.

`tool.<name>` sends requests to the current session's tool registry through the kernel's child-process pipes. The agent verifies the active cell and request number, applies the session allow-list and load modes, and runs the named tool's own approvals and hooks. These tool rules govern bridge requests. Python's direct filesystem and process access still follows the OS permissions of the agent user.

## Crash bundles

When a process panics, horon agent leaves a local diagnostic bundle. The bundle is written to disk only and sent nowhere. This document explains the location, contents, redaction, and the no-transmission guarantee in the current tree.

### Location

The bundle is written under `crashes/` below the agent directory (`crates/base/src/crash.rs:180` `crash_root`). The agent directory defaults to `~/.horon` and can be changed with `HORON_HOME`.

One bundle is one directory.

```text
<agent dir>/crashes/<UTC time>-p<pid>/
  bundle.json
  report.txt
```

The time format is `YYYYMMDDTHHMMSS.mmmZ` with the PID appended (`crash.rs:217` `bundle_stamp`). If the same name already exists, a new directory gets a suffix such as `-1` or `-2` (`crash.rs:225` `create_bundle_dir`).

### Contents

`bundle.json` is a schema 1 document.

| Field | Contents |
| --- | --- |
| `schema` | Bundle format version. Currently 1. |
| `captured_at` | The UTC time the bundle was created, plus the PID. |
| `pid` | Process ID. |
| `panic.message` | The panic payload string. |
| `panic.location` | File, line, column. |
| `panic.thread` | The name or ID of the thread that panicked. |
| `operations` | Up to the last 64 tool dispatches, oldest first. |

`operations` is a process-wide ring buffer (`crash.rs:26` `OPERATION_RING_CAPACITY`). Each entry carries an increasing `seq`, the tool name, and the input text after redaction and truncation (`crash.rs:61` `OperationRecord`). When the feature is off, dispatches are not recorded at all.

`report.txt` lays out the same contents for humans. Its first line is `horon-agent crash bundle (local only; nothing was transmitted)` (`crash.rs:264` `render_report`).

### Redaction

Every string that goes into a bundle passes through `redact_secrets`, the existing secret-removal pass (`crates/base/src/message.rs:45`).

- The same pass applies to the panic message, location, thread name, tool names, and tool input JSON.
- Fields are truncated at 2048 characters with `[truncated]` appended. Tool names are truncated at 128 characters (`crash.rs:318` `truncate_chars`).
- Tool input is treated as text an attacker can influence. It is written into the bundle as inert data only and is never read back to be executed or interpreted.

Redaction is a defense that removes known secret shapes, not a complete guarantee. Bundles are better treated as sensitive material.

### Nothing is transmitted

The bundle is written to a local directory with `std::fs::write` only. There are no network calls, uploads, or telemetry sends. The first line of `report.txt` records that fact too.

### Turning it on and off

`[features] crash_bundles` defaults to `true` (`crates/config-types/src/feature.rs:54`). Setting it to `false` means the panic hook writes no bundle and tool dispatches are not recorded either.

The hook is installed once at startup (`src/cli/startup.rs:143`). Later calls only update the enabled flag (`crash.rs:86` `install_panic_hook`). So re-reading configuration and turning the feature on or off still leaves the already installed hook following that flag.

### Limits

- Writes are best effort. If creating the directory or writing a file fails, panic handling itself still continues.
- Stack traces, full environment variables, and session transcripts are not included. Only the panic location and recent tool calls are.
- Bundles stay local, so the user has to clean them up.

## API exposure and credentials

The harness API is local by default: a Unix socket or named pipe that only the
owning OS user can open, or one stdio connection. `--api-bind` adds a TCP
listener, and that listener is the only harness surface another machine can
reach.

- Every TCP connection presents `Authorization: Bearer <token>` before `hello`.
  A failure closes the connection without writing a frame and without dialling
  the daemon, so the listener never processes a request from an unauthenticated
  peer. Pre-auth input is limited to 1 KiB and 10 seconds, so a silent or
  slow-drip peer cannot hold a connection or memory.
- Tokens are 32 random bytes stored only as `sha256:<hex>` digests in
  `$HORON_HOME/harness-api/credentials.json`, a `0600` file in a `0700`
  directory. The plaintext is printed once by
  `horon-agent api-credentials issue` or `rotate`. The bridge re-reads the file
  for every connection, so `revoke` and `rotate` apply immediately, and a store
  that is a symlink, owned by another user, or not owner-only is refused rather
  than trusted.
- Built-in TLS is not implemented in this build. A non-loopback bind therefore
  refuses to start unless the operator acknowledges a TLS-terminating proxy with
  `--api-insecure-behind-proxy` or `insecure_behind_proxy = true`, and at least
  one credential exists. Without that proxy the bearer token would travel in
  cleartext; client certificates at the proxy are the supported mutual
  authentication path.
- Credentials are per `HORON_HOME`. A per-user container's home volume is that
  tenant's, so one tenant's tokens cannot authenticate another tenant's listener.
- The gateway (`[gateway]`, the WebSocket pairing surface) and the harness API
  listener are separate surfaces with separate registries; neither surface's
  token is accepted by the other.
- Exposure does not authenticate a person, only a credential. On a hosting
  provider the private network (`6PN` on Fly.io) keeps other organizations out
  but treats every process on it as trusted, and a public port is public. The
  token is required on both paths, and no forwarded header (`Fly-Client-IP` or
  any other) is trusted for authentication or authorization.
- A bind address is parsed as a literal IPv4 or IPv6 address. `[::]`,
  `0.0.0.0` and a private `fdaa::/16` address are all non-loopback and all
  require the proxy acknowledgement; only `127.0.0.0/8` and `::1` are loopback.

## Dependency security classification

> **Status:** dated record
> **Measured:** 2026-05-14
> **How to read:** this document is a `cargo audit` snapshot taken on 2026-05-14 and is not the current state. The dependency paths and classifications in the table below are what that day's tree looked like. To check current values, run `scripts/ci/check_security.sh` yourself.

This file records horon agent's 2026-05-14 `cargo audit` results and the resolution paths intended at the time.
It is not an allowlist. It is a classification record kept so that security advisories stay visible and actionable.

The record was written against the tree before two removals. First the TUI crate left, then the vendor
provider runtime crate left. So rows naming `ratatui` and `syntect` paths and rows that went through the provider
runtime are read as historical records. The provider crates that remain are `provider-core`,
`provider-env`, `provider-metadata`, and `provider-openai`; `provider-openai-runtime` was
removed on 2026-10-29 (`ls crates/`). The TUI has since come back as the separate package `horon-tui`
(`crates/tui`): `ratatui` and `crossterm` are in `Cargo.lock` again, reached only from the TUI binary and
never from `horon-agent` (`scripts/ci/check_headless_graph.py`). `syntect` did not come back.

### Current advisories

| Advisory | Crate | Dependency path | Area affected in horon agent | Classification | Planned action |
|---|---|---|---|---|---|
| `RUSTSEC-2025-0141` | `bincode` | `syntect -> bincode` | TUI Markdown/code highlighting | Unmaintained transitive dependency. Not directly exposed to provider/auth flows. | Track the `syntect` upgrade, and replace `syntect` if upstream does not move off `bincode` soon. |
| `RUSTSEC-2024-0436` | `paste` | `ratatui -> paste`, `tokenizers -> paste`, `tract-* -> paste` | TUI rendering, tokenizers, embedding/model support | The transitive dependency is widespread. Not confined to one module. | Prefer upstream dependency upgrades over local workarounds. Re-evaluate after raising `ratatui`, `tokenizers`, and `tract-*`. |
| `RUSTSEC-2026-0002` | `lru` | `ratatui -> lru` | TUI rendering/cache internals | Unsoundness warning in a UI dependency. Not auth/provider logic, but it is loaded into the process. | Raise `ratatui` / `ratatui-image` together when a compatible version exists. |
| `RUSTSEC-2026-0097` | `rand` | `azure_core`, `tungstenite`, `tract-*`, `ratatui-image`, and others | Azure auth, websockets, embeddings, UI transitive paths | Unsoundness warning involving a custom logger that uses `rand::rng()`. HoronAgent deliberately does not use that pattern, but the crate is widespread in the graph. | Prioritize upstream upgrades to a dependency stack compatible with `rand` 0.9. |
| `RUSTSEC-2026-0098` | `rustls-webpki` | `rustls` dependency stack | TLS certificate verification wherever rustls is used | Accepts name constraints for URI names incorrectly. A transitive issue through the TLS library. | Raise the rustls/webpki stack when a compatible release ships. |
| `RUSTSEC-2026-0099` | `rustls-webpki` | `rustls` dependency stack | TLS certificate verification wherever rustls is used | Accepts name constraints on wildcard certificates. A transitive issue through the TLS library. | Raise the rustls/webpki stack when a compatible release ships. |
| `RUSTSEC-2026-0104` | `rustls-webpki` | `rustls` dependency stack | TLS certificate revocation list parsing | A reachable panic in CRL parsing. A transitive issue through the TLS library. | Raise the rustls/webpki stack when a compatible release ships. |
| `RUSTSEC-2026-0049` | `rustls-webpki` | `rustls` dependency stack (`aws-smithy` rustls 0.21) | TLS certificate revocation list handling | Because of faulty matching logic it does not treat the CRL named by a Distribution Point as authoritative. A transitive issue through an older rustls stack, and the fix requires rustls-webpki >=0.103.10, which needs a major upgrade of the `aws-sdk` stack. | Raise the rustls/webpki stack when a compatible release ships. |
| `RUSTSEC-2026-0187` | `lopdf` | `horon-agent-pdf -> pdf-extract 0.8.2 -> lopdf 0.34` | PDF text extraction (`/pdf`, image/PDF reading) | The stack overflows when parsing deeply nested PDF objects. Only reachable when extracting text from a PDF the user opens (which may be malicious), and not an auth/provider/network path. Because `pdf-extract 0.8.2` pins `lopdf 0.34`, it cannot move to the fixed `>=0.42` without an upstream `pdf-extract` release. | Raise it when `pdf-extract` ships a release that depends on `lopdf >=0.42`, and drop the ignore then. |

### Priority

1. The `rustls-webpki` TLS advisories through the rustls stack
2. `lru` through `ratatui` (record from before the TUI moved out of the agent binary. Today `ratatui` is reached only from `horon-tui`, never from `horon-agent`)
3. `bincode` through `syntect`
4. `paste` / `rand` through multiple transitive dependencies

### Difference from the current lock file

This section is not a re-measurement of the table above; it only counts whether the same crates are in today's
`Cargo.lock`. It was checked with `grep -c '^name = "<crate>"' Cargo.lock`, and it does not mean an advisory is resolved.

| Crate | In `Cargo.lock` | Related record |
|---|---|---|
| `ratatui` | Yes, through `horon-tui` only (`crates/tui`); `scripts/ci/check_headless_graph.py` keeps it out of `horon-agent` | `lru` row, TUI paths |
| `syntect` | No | `bincode` row |
| `bincode` | No | `RUSTSEC-2025-0141` |
| `lru` | Yes, through `ratatui-core` | `RUSTSEC-2026-0002` |
| `quick-xml` | No | `RUSTSEC-2026-0194`, `RUSTSEC-2026-0195` |
| `paste` | No | `RUSTSEC-2024-0436` |
| `rand` | Yes | `RUSTSEC-2026-0097` |
| `azure_core` | No | `RUSTSEC-2026-0097` |
| `tungstenite` | Yes | `RUSTSEC-2026-0097` |
| `tract-nnef` | Yes | `RUSTSEC-2026-0097`, the `RUSTSEC-2026-0217` item below |
| `lopdf` | Yes | `RUSTSEC-2026-0187` |
| `chrono` | No | `RUSTSEC-2026-0543` |
| `rustls-webpki` | Yes | `RUSTSEC-2026-0049`, `0098`, `0099`, `0104` |
| `crossterm` | Yes, through `horon-tui` only | It left with the deletion of the terminal image crate (`horon-agent-terminal-image`) and came back with `crates/tui` |

### Notes

- None of the advisories above came from the provider auth refactor.
- Provider/auth hardening work has to continue separately from these dependency upgrades.
- `RUSTSEC-2026-0217` (`tract-nnef` 0.21.10, an integer overflow in the NNEF tensor parser) was
  resolved on 2026-07-30 by moving `horon-agent-embedding` to `tract` 0.23.
  The inline `0.21.16` fix was unreachable, because `tract-data 0.21.16` pins
  `half ^2.5`. The 0.23 line removes that pin. What made this one important
  is that the parser runs on models downloaded at runtime rather than models built
  into the binary, so passing it over with an ignore was clearly not safe.
- `RUSTSEC-2024-0320` (`yaml-rust`) was removed from the dependency graph on 2026-03-05 by
  reducing the `syntect` feature to built-in syntax and theme dumps instead of YAML loading.
- `RUSTSEC-2026-0194` / `RUSTSEC-2026-0195` (`quick-xml` 0.39.2) are reachable only through
  `wayland-scanner`, a build-time proc-macro in the winit stack of the desktop
  crate. It parses trusted, vendored Wayland protocol XML during compilation and never touches
  untrusted input at runtime. The resolution is upstream's to make.
  `wayland-scanner` has to move to `quick-xml >= 0.41`. It was classified on 2026-07-04 and
  ignored in `scripts/ci/check_security.sh`.
- On 2026-11-09, while bringing CI warnings to zero, the items `cargo audit` only reported as
  warnings were cleaned up. The unsoundness advisories for `anyhow` (RUSTSEC-2026-0190),
  `event-listener` (RUSTSEC-2026-0221) and `tokio` (RUSTSEC-2026-0386), and the `chacha20`
  yanked warning, were fixed by bumping `Cargo.lock` (`anyhow` 1.0.104, `event-listener` 5.4.2,
  `tokio` 1.53.3, `chacha20` 0.10.2). At the time the audit excluded five unmaintained
  dependencies: `paste`, `pin-utils`, `rustybuzz`, `ttf-parser`, and `unsafe-libyaml`.
- The current lockfile drops all five. `futures` 0.3.34 and `hyper` 1.12 remove
  `pin-utils`. `yaml_serde` reads skill frontmatter, and `resvg`/`usvg` 0.48 and
  `lopdf` 0.45 replace the old YAML and font parser dependencies. The workspace pins
  the upstream PDF, tokenizer and allocator revisions in its Cargo manifests until
  compatible published versions carry those changes. The preflight runs
  `cargo audit --deny warnings` with no advisory exclusions.
- 2026-11-13: three advisories published the same day failed the preflight on a push that changed no
  dependency — `RUSTSEC-2026-0524` and `RUSTSEC-2026-0523` (`quinn-proto` 0.11.18) and
  `RUSTSEC-2026-0538` (`quinn-udp` 0.5.14), all reached through `reqwest`'s QUIC stack. They were
  fixed in `Cargo.lock` rather than ignored (`quinn-proto` 0.11.19, `quinn-udp` 0.5.16), so no
  `audit_ignores` entry was added. `main` carried the same versions, so the failure was not
  specific to `dev`.
- 2026-11-17: `RUSTSEC-2026-0543` (`chrono` 0.4.42, unmaintained, all versions, no patched version,
  issued that day) failed the preflight on a pull request that changed no dependency: the same `dev`
  commit passed at 09:38 and failed at 14:14, because the advisory database gained the entry in
  between. The migration in #377 removed the dependency and the temporary audit exception.
- Before changing dependency versions, run:
  - `cargo check`
  - `cargo test -j 1`
  - `scripts/ci/check_security.sh`

2026-10-29: `crates/provider-openai-runtime` and the 8317 default base were removed later.

2026-11-04: the native plugin model path was removed too, so model requests now go directly to the configured `[providers.<name>]` endpoint over the wire its `type` selects (`openai-compatible`, `openai-responses`, `anthropic-compatible`, or `gemini-compatible`).

An extension declares a Rust executable and a plugin adds resources to that
extension. `horon package install <directory>` validates and registers either
kind in the user catalog. The explicit `--project <root>` option limits the
installed package to one canonical project root. Installation is consent.
The daemon reconciles completed folders under the user-owned
`$HORON_HOME/packages/inbox/<id>/` through the same installer. It leaves the
source in place and skips IDs already recorded, including removed packages.
The CLI sends mutations to the daemon when it is running. If a reply is lost
after sending the request, the CLI returns an error. An open session captures
each updated catalog generation at its next turn boundary.

SHA-256 is an internal integrity check. The versioned JSON-RPC handshake
compares the declaration digest the child reports before any call is dispatched.
Tool calls and executable commands use the existing tool and hosting policy
gates (`crates/base/src/packages/child_gate.rs`). Child processes run with the user's OS permissions. The host checks
cannot restrict direct file or network access inside the child, so install only
trusted packages.
