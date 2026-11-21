# horon agent documentation

Language: **English** | [한국어](README.ko.md)

This folder describes how horon agent behaves today and labels proposed designs separately from current behavior. Every document ships as an English `X.md` and a Korean `X.ko.md` in the same folder; English is the default, and the Korean files are the `.ko.md` siblings. Paths, commands, and identifiers keep their original spelling. File names are uppercase and each topic gets its own subfolder.

The documents are embedded in the binary at build time (`crates/app-core/build.rs` reads the subfolders and skips `.ko.md`) and the `horon_agent_docs` tool reads them. Moving or deleting a document updates the paths left in code, scripts, and tests in the same change. The site at [agent.horon.ai](https://agent.horon.ai) shows them under its Reference tab: `scripts/ci/sync_docs_site.py` writes `packages/docs-site/reference/` from this folder, and the guides in the rest of `packages/docs-site` are written by hand, so a behavior change that a guide also describes is edited in both places.

## Two modes, one agent

horon agent runs in two modes that use the same engine, daemon, sessions, tools, and configuration. In headless mode a program drives the session: `run`, the daemon (`serve`) behind the SDKs and `api-bridge`, `acp` for editors, and `rpc`. In TUI mode a person drives it through `horon-tui`. Neither mode is a reduced form of the other; they differ only in who is on the other end. The modes ship as two binaries: `horon-agent` (the agent binary, which every installation needs) and `horon-tui` (the TUI binary, a separate optional asset of the same release). [`release/INSTALL.md`](release/INSTALL.md) and [`runtime/TUI.md`](runtime/TUI.md) describe how each is installed.

## Fixed requirements

Every change keeps the three rules below. The source of truth is `AGENTS.md` at the repository root.

| Requirement | What it means | Current state |
| --- | --- | --- |
| No TUI in `horon-agent` | The agent binary (`serve`, `run`, `api-bridge`, `acp`, `rpc`) never opens a screen, and its dependency graph holds no `ratatui`, `crossterm`, `horon-tui`, or `horon-agent-tui*` crate. TUI mode lives in the separate `horon-tui` package on `horon-agent-sdk`, and bare `horon` on a terminal replaces itself with that sibling binary. | Met. `scripts/ci/check_headless_graph.py` walks the resolved graph |
| ACP v1 and v2 support | `horon acp` handles both `protocolVersion` 1 and 2 and answers with the highest version both sides support. Adding v2 does not break v1. | Met. v2 negotiation lives in `src/cli/acp/v2/negotiate.rs`, v1 in `src/cli/acp/util.rs` |
| No performance regression | A change must be as fast as or faster than the baseline recorded on the same machine, build profile, and workload. A slower change is not merged. | The method and recorded numbers exist (`performance/BENCHMARK.md`, `performance/RESULTS.md`). The recorded numbers are the baseline |

## Folder layout

| Folder | Document | Contents |
| --- | --- | --- |
| `docs/` | [`README.md`](README.md) | This index and the update rules |
| `diagram/` | [`README.md`](diagram/README.md) | Eight hand-written SVG diagrams with the code paths each one draws: system overview, run modes, turn lifecycle, provider wires, ACP negotiation, session storage, tool loading, branch and release flow |
| `architecture/` | [`SERVER.md`](architecture/SERVER.md) | Server, session, and socket structure |
| | [`CRATES.md`](architecture/CRATES.md) | Crate ownership boundaries, platform adapters, storage durability, and the TLS trust set |
| `protocol/` | [`ACP.md`](protocol/ACP.md) | ACP v1 and v2 adapters, editor connections, and the stdio contract |
| | [`HARNESS_API.md`](protocol/HARNESS_API.md) | Harness API credential setup and the request and event surface |
| | [`SESSION_STORE.md`](protocol/SESSION_STORE.md) | Session event storage format, delivery contract, writer pool, durable runtime journal |
| | [`COLLABORATION.md`](protocol/COLLABORATION.md) | Proposed live session invitations and participant permissions |
| | [`RPC.md`](protocol/RPC.md) | stdio JSONL RPC mode: commands, frames, and the omp conformance table |
| | [`SHARING.md`](protocol/SHARING.md) | Proposed encrypted session sharing and revocation |
| `runtime/` | [`TOOLS.md`](runtime/TOOLS.md) | Tool loading, effect and approval tiers, edit safety devices, parent `AGENTS.md`, `skill://` and `memory://` reads and MCP config sources, auxiliary calls |
| | [`EDIT_DIAGNOSTICS.md`](runtime/EDIT_DIAGNOSTICS.md) | Proposed post-edit language diagnostics |
| | [`TOOL_SCHEDULING.md`](runtime/TOOL_SCHEDULING.md) | Tool scheduling and loop prevention |
| | [`TURNS.md`](runtime/TURNS.md) | Turn recovery, mid-run message insertion, the `[prewalk]` plan-to-execution model handoff, goal continuation and its token budget, runtime commands, `--resume` |
| | [`IRC.md`](runtime/IRC.md) | Inter-agent IRC mailbox bus, the `irc` tool, configuration, performance gate |
| | [`TUI.md`](runtime/TUI.md) | The `horon-tui` terminal client: the handoff from bare `horon`, user files (`tui.toml`, themes, key bindings, command files, status line), keys, the multi-line editor, the transcript and its scrollback, turn state, prompts, the slash popup, the model and session pickers, commands, the settings screen, installing the optional asset, limits |
| | [`PROMPT.md`](runtime/PROMPT.md) | System prompt composition, skill sources and invocation, token estimates |
| | [`HOOKS.md`](runtime/HOOKS.md) | Lifecycle hooks |
| | [`PACKAGES.md`](runtime/PACKAGES.md) | User-installed Rust packages: the author SDK, the declaration and its digest, both layouts, targets, signing and installation |
| | [`SWARM_TASK_GRAPH.md`](runtime/SWARM_TASK_GRAPH.md) | Swarm task DAG design (a feature that is off by default, and the design rationale of the `horon-agent-plan` crate) |
| `provider/` | [`MODEL.md`](provider/MODEL.md) | Direct calls to the endpoint of the wire a profile's `type` names (Chat Completions, Responses, Anthropic Messages, Gemini generateContent), provider profiles and API keys, model roles and their fallback order, the one-shot `[prewalk]` handoff, model usage metadata |
| | [`LOCAL_HELPERS.md`](provider/LOCAL_HELPERS.md) | Proposed offline local model helper |
| | [`INTEGRATIONS.md`](provider/INTEGRATIONS.md) | Remote MCP, user-defined web search |
| | [`DIAGNOSTICS.md`](provider/DIAGNOSTICS.md) | Proposed bounded provider-stream diagnostic |
| | [`SAMPLING.md`](provider/SAMPLING.md) | Proposed typed model sampling |
| `safety/` | [`SAFETY.md`](safety/SAFETY.md) | Safety system, unattended approval and automatic switching |
| | [`SECURITY.md`](safety/SECURITY.md) | Workspace trust, crash bundles, dependency security classification |
| `performance/` | [`BENCHMARK.md`](performance/BENCHMARK.md) | The goal of beating jcode and the measurement standard, per-OS benchmarks (Linux, macOS, Windows), the structural gate, A/B measurement, skill and MCP config loading A/B |
| | [`EVAL.md`](performance/EVAL.md) | Proposed fixture-backed instruction-following evaluation |
| | [`RESULTS.md`](performance/RESULTS.md) | Measured results and their limits |
| | [`MEMORY.md`](performance/MEMORY.md) | Memory regression budgets, idle session derived-cache release, incident response procedure |
| `release/` | [`INSTALL.md`](release/INSTALL.md) | Release asset verification and `install-release`, installing from npm, running on a Linux server, the TLS trust set |
| | [`NPM.md`](release/NPM.md) | The npm packages of the CLI, installing and running them, versioning, the publish workflow, the one-time trusted publisher setup, provenance |
| | [`WINDOWS.md`](release/WINDOWS.md) | Windows build and signing |

## Update rules

| When this changes | Do this |
| --- | --- |
| Current behavior changes | Edit the document that describes that behavior, and the diagram in `diagram/` that draws it, in the same change. |
| You want a new document | First consider adding a section to an existing document in the right folder. A new file gets an uppercase name and a row in this table. |
| A document moves or is deleted | Fix this table and the paths in code, scripts, and tests in the same change. |
| The same content appears in two documents | Make one the source of truth and have the other point at it. |
| You want to record a measurement, acceptance note, or plan | Do not create a document; write it in the PR body or an issue. Performance numbers go only in `performance/RESULTS.md`. |
| You edit any document | Keep the pair in step: every `X.md` has a Korean `X.ko.md` beside it, and `python3 scripts/ci/check_docs_pairs.py` verifies that both exist, that headings, table rows, code fences, and links match, and that relative links resolve. |
| You change a file under `docs/` | Run `python3 scripts/ci/sync_docs_site.py` and commit the changed pages under `packages/docs-site/reference/` and `packages/docs-site/ko/reference/`, together with any regenerated images under `packages/docs-site/images/reference/`. CI checks all three directories. |