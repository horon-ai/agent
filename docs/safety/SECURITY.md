# Security

Language: **English** | [한국어](SECURITY.ko.md)

Workspace trust gates, local crash bundles, and dependency security classification.

## Workspace trust

A project repository can name MCP servers for the agent to run. So horon agent does not read a workspace's project-local configuration without consent for its contents. This document explains the gate's scope, the digest rules, the headless fail-closed behavior, and the overrides in the current tree.

### What is gated

Three files in the workspace are gated (`crates/base/src/workspace_trust.rs:22` `PROJECT_CONFIG_SURFACES`).

- `.horon/mcp.json`
- `.mcp.json`
- `.claude/mcp.json`

User-level configuration is not gated. `~/.horon/mcp.json`, `~/.claude.json`, and `~/.claude/mcp.json` are the user's own configuration, not something a repository decides.

There is a single enforcement point. When `McpConfig` reads project-local configuration, `load_authorized_project_locals` calls `authorize_project_config` and parses only the bytes of the authorized snapshot (`crates/base/src/mcp/protocol.rs:649`, `workspace_trust.rs:258`). If all three files are absent there is nothing to gate, so the result is `Absent`; if the decision is denial, it logs one warning line and does not load project configuration.

### Digest rules

The snapshot reads the three surfaces in a fixed order and computes SHA-256 (`workspace_trust.rs:162` `snapshot`).

1. It starts with the domain separator `horon-agent/workspace-trust/v1`.
2. For each surface it adds the path, the state, and a content fingerprint.
3. The state is one of `absent`, `present`, `unreadable`. `present` includes the byte length together with that file's SHA-256.

Consent is recorded as the pair of the normalized workspace path and this digest. The record file is `workspace-trust.json` in the agent directory (`workspace_trust.rs:408` `trust_file_path`), and nothing is written into the workspace. An entry carries version 1, the digest, the decision, the recording time, and the per-surface SHA-256 and length.

- If any of the three files changes even one byte, the digest differs and that workspace is asked for trust again. Creating or deleting a file has the same effect.
- The denial reason names the changed surface as precisely as it can (`workspace_trust.rs:304` `untrusted_reason`).
- The record is read and rewritten as a whole file, so if two processes write at the same moment one decision can be lost. What is lost is one decision; an allowance that did not exist never appears (`workspace_trust.rs:222` `record`).
- The digest is not a mechanism that makes a repository trustworthy. It binds consent to the contents the user saw.

### Headless fail-closed

A headless path has no client to ask. So an unknown state is denial, not allowance.

- With no record for the current digest the result is `Skipped`. It logs a warning, does not load project-local configuration, and records no decision. The next run asks again.
- With a record but a different digest the result is `Skipped`. The warning names the changed surface.
- If a surface cannot be read as a regular file, the result is `Skipped` regardless of overrides (`workspace_trust.rs:258`).
- If the recorded decision is denial, the result is `Skipped`.
- If all three files are absent the result is `Absent` and the gate does not engage.

Denial is closure, not failure. The session keeps running and only the MCP servers the project named are dropped.

### Overrides

| Means | Scope | Recording |
| --- | --- | --- |
| `--trust-workspace` | This run | Records trust for the current digest. |
| `HORON_AGENT_WORKSPACE_TRUST=trust` | This process | Records nothing. |
| `HORON_AGENT_WORKSPACE_TRUST=deny` | This process | Records nothing, and denies even when a stored trust exists. |
| Explicit commands such as `mcp test` and `mcp list` | That command | Records nothing. The command itself is the consent. |

The environment variable is case-insensitive and recognizes only `trust` and `deny`. Any other value is ignored and the stored decision applies (`workspace_trust.rs:363` `env_decision`). An environment denial is applied before `--trust-workspace`, so if you pass both, the denial wins.

## Crash bundles

When a process panics, horon agent leaves a local diagnostic bundle. The bundle is written to disk only and sent nowhere. This document explains the location, contents, redaction, and the no-transmission guarantee in the current tree.

### Location

The bundle is written under `crashes/` below the agent directory (`crates/base/src/crash.rs:121` `crash_root`). The agent directory defaults to `~/.horon` and can be changed with `HORON_HOME`.

One bundle is one directory.

```
<agent dir>/crashes/<UTC time>-p<pid>/
  bundle.json
  report.txt
```

The time format is `YYYYMMDDTHHMMSS.mmmZ` with the PID appended (`crash.rs:158` `bundle_stamp`). If the same name already exists, a new directory gets a suffix such as `-1` or `-2` (`crash.rs:166` `create_bundle_dir`).

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

`operations` is a process-wide ring buffer (`crash.rs:25` `OPERATION_RING_CAPACITY`). Each entry carries an increasing `seq`, the tool name, and the serialized input JSON string. When the feature is off, dispatches are not recorded at all.

`report.txt` lays out the same contents for humans. Its first line is `horon-agent crash bundle (local only; nothing was transmitted)` (`crash.rs:205` `render_report`).

### Redaction

Every string that goes into a bundle passes through `redact_secrets`, the existing secret-removal pass (`crates/base/src/message.rs:51`).

- The same pass applies to the panic message, location, thread name, tool names, and tool input JSON.
- Fields are truncated at 2048 characters with `[truncated]` appended. Tool names are truncated at 128 characters (`crash.rs:241` `truncate_chars`).
- Tool input is treated as text an attacker can influence. It is written into the bundle as inert data only and is never read back to be executed or interpreted.

Redaction is a defense that removes known secret shapes, not a complete guarantee. Bundles are better treated as sensitive material.

### Nothing is transmitted

The bundle is written to a local directory with `std::fs::write` only. There are no network calls, uploads, or telemetry sends. The first line of `report.txt` records that fact too.

### Turning it on and off

`[features] crash_bundles` defaults to `true` (`crates/config-types/src/feature.rs:54`). Setting it to `false` means the panic hook writes no bundle and tool dispatches are not recorded either.

The hook is installed once at startup (`src/cli/startup.rs:52`). Later calls only update the enabled flag (`crash.rs:67` `install_panic_hook`). So re-reading configuration and turning the feature on or off still leaves the already installed hook following that flag.

### Limits

- Writes are best effort. If creating the directory or writing a file fails, panic handling itself still continues.
- Stack traces, full environment variables, and session transcripts are not included. Only the panic location and recent tool calls are.
- Bundles stay local, so the user has to clean them up.

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
removed on 2026-10-29 (`ls crates/`).

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
2. `lru` through `ratatui` (record from before the headless transition. The current tree has no `ratatui`)
3. `bincode` through `syntect`
4. `paste` / `rand` through multiple transitive dependencies

### Difference from the current lock file

This section is not a re-measurement of the table above; it only counts whether the same crates are in today's
`Cargo.lock`. It was checked with `grep -c '^name = "<crate>"' Cargo.lock`, and it does not mean an advisory is resolved.

| Crate | In `Cargo.lock` | Related record |
|---|---|---|
| `ratatui` | No | `lru` row, TUI paths |
| `syntect` | No | `bincode` row |
| `bincode` | No | `RUSTSEC-2025-0141` |
| `lru` | No | `RUSTSEC-2026-0002` |
| `quick-xml` | No | `RUSTSEC-2026-0194`, `RUSTSEC-2026-0195` |
| `paste` | Yes | `RUSTSEC-2024-0436` |
| `rand` | Yes | `RUSTSEC-2026-0097` |
| `azure_core` | Yes | `RUSTSEC-2026-0097` |
| `tungstenite` | Yes | `RUSTSEC-2026-0097` |
| `tract-nnef` | Yes | `RUSTSEC-2026-0097`, the `RUSTSEC-2026-0217` item below |
| `lopdf` | Yes | `RUSTSEC-2026-0187` |
| `rustls-webpki` | Yes | `RUSTSEC-2026-0049`, `0098`, `0099`, `0104` |
| `crossterm` | No | It left together with the deletion of the terminal image crate (`horon-agent-terminal-image`) |

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
  `tokio` 1.53.3, `chacha20` 0.10.2). The five remaining unmaintained advisories (`paste`
  RUSTSEC-2024-0436, `pin-utils` RUSTSEC-2026-0441, `rustybuzz` RUSTSEC-2026-0206, `ttf-parser`
  RUSTSEC-2026-0192, `unsafe-libyaml` RUSTSEC-2024-0449) are transitive dependencies this
  repository cannot drop and that have no upstream replacement, so they are ignored in
  `scripts/ci/check_security.sh`. That script runs `cargo audit --deny warnings`, so a new warning
  (unmaintained, unsound, yanked) fails CI.
- `scripts/ci/check_security.sh` ignores the vulnerability advisories explicitly classified
  above (`rustls-webpki`) so CI stays actionable
  (`audit_ignores` in `scripts/ci/check_security.sh`). New
  vulnerabilities still fail CI by default.
- 2026-11-13: three advisories published the same day failed the preflight on a push that changed no
  dependency — `RUSTSEC-2026-0524` and `RUSTSEC-2026-0523` (`quinn-proto` 0.11.18) and
  `RUSTSEC-2026-0538` (`quinn-udp` 0.5.14), all reached through `reqwest`'s QUIC stack. They were
  fixed in `Cargo.lock` rather than ignored (`quinn-proto` 0.11.19, `quinn-udp` 0.5.16), so no
  `audit_ignores` entry was added. `main` carried the same versions, so the failure was not
  specific to `dev`.
- Before changing dependency versions, run:
  - `cargo check`
  - `cargo test -j 1`
  - `scripts/ci/check_security.sh`

2026-10-29: `crates/provider-openai-runtime` and the 8317 default base were removed later.

2026-11-04: the native plugin model path was removed too, so model requests now go directly to an OpenAI-compatible endpoint from `[providers.<name>]`.