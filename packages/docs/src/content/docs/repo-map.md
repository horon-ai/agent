---
title: Repository map
description: Where each part of horon-agent lives in the repository and which document describes it.
section: Reference
order: 1
---

## Top level

| Path | What it holds |
| --- | --- |
| `src/` | The CLI: `serve`, `run`, `repl`, `api-bridge`, `acp`, and their dispatch |
| `crates/` | The workspace: `base`, `app-core`, `session-store`, `harness-api`, `agent-sdk`, `tui` (the `horon-tui` terminal client), provider and platform crates |
| `docs/` | Behaviour documentation, compiled into the binary and served by `horon_agent_docs` |
| `packages/sdk/typescript` | `@horon-ai/agent-sdk` |
| `packages/sdk/python` | `horon-agent-sdk` |
| `packages/npm/horon` | `@horon-ai/horon` launcher |
| `packages/docs` | This documentation site |
| `scripts/` | CI gates, release and install scripts, benchmarks |
| `tests/` | Workspace-level integration tests |

## Documents in the repository

| Topic | Document |
| --- | --- |
| Server, sessions, sockets | `docs/architecture/SERVER.md` |
| Crate ownership, platform adapters, TLS trust | `docs/architecture/CRATES.md` |
| ACP v1 and v2 | `docs/protocol/ACP.md` |
| Harness API credential setup and surface | `docs/protocol/HARNESS_API.md` |
| Session event storage | `docs/protocol/SESSION_STORE.md` |
| Tool loading, effect tiers, edit safety | `docs/runtime/TOOLS.md` |
| Tool scheduling and loop prevention | `docs/runtime/TOOL_SCHEDULING.md` |
| Turn recovery, mid-run insert, `--resume` | `docs/runtime/TURNS.md` |
| Inter-agent IRC mailbox | `docs/runtime/IRC.md` |
| Terminal client (`horon-tui`) | `docs/runtime/TUI.md` |
| System prompt composition | `docs/runtime/PROMPT.md` |
| Provider profiles and model roles | `docs/provider/MODEL.md` |
| Safety, approvals, unattended runs | `docs/safety/SAFETY.md` |
| Workspace trust, crash bundles | `docs/safety/SECURITY.md` |
| Benchmarks and the measurement standard | `docs/performance/BENCHMARK.md` |
| Measured results | `docs/performance/RESULTS.md` |
| Release assets and installation | `docs/release/INSTALL.md` |
| npm packages | `docs/release/NPM.md` |
| Windows build and signing | `docs/release/WINDOWS.md` |

Each document ships as English `X.md` with a Korean `X.ko.md` beside it. The
English file is canonical; `.ko.md` files are not bundled into the binary.

## Checks

| Area | Command |
| --- | --- |
| Build | `cargo build -p horon-agent` |
| Format | `cargo fmt --all -- --check` |
| Lints | `cargo clippy --locked --all-targets --all-features -- -D warnings` |
| Tests | `cargo test --workspace` |
| Agent graph (no terminal crates) | `python3 scripts/ci/check_headless_graph.py` |
| ACP adapter | `cargo test -p horon-agent --lib acp` |
| Docs pairs | `python3 scripts/ci/check_docs_pairs.py` |

Performance measurement runs only in GitHub Actions; the comparison suite builds
the baseline and the candidate on one runner, so a before/after difference is not
a compiler difference.
