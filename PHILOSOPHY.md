# horon agent philosophy

Language: **English** | [한국어](PHILOSOPHY.ko.md)

This document says what horon agent is for, which principles the code already follows, which ideas it took from omp2, and which it left out. It states a direction and a test for new work. It does not change any requirement: `AGENTS.md` is the contract and wins when the two disagree. Every claim names the file, issue, or run where it can be checked; a sentence without one is a decision, not a measurement.

## What horon is

A headless agent engine: the agent loop (model calls, tools, sessions, MCP) behind three surfaces, a one-shot command (`run`), a harness API on a Unix socket for SDKs (`api-bridge`), and ACP on stdio for editors (`acp`). One engine, two binaries: `horon-agent` never opens a terminal UI, and `horon-tui` is a separate package that drives the daemon through `horon-agent-sdk`. Server code can use the same engine through the Rust, TypeScript, and Python SDKs (`crates/agent-sdk`, `sdk/typescript`, `sdk/python`).

## Principles the code already follows

| # | Principle | What it means in practice | Where to check it |
|---|---|---|---|
| 1 | Beat jcode in every cell, never regress | jcode is the closest competitor; horon aims to be faster and smaller on every OS and every cell of the comparison benchmark. The floor is never slower than the recorded baseline. A tie inside the min–max overlap is not a win | `AGENTS.md` (Fixed requirement 3, Performance target), `docs/performance/BENCHMARK.md`, `docs/performance/RESULTS.md` |
| 2 | Measure in CI, only there | Performance is measured by the `compare` suite in GitHub Actions; the same job builds the `dev` baseline and the candidate. Every reported number carries its window and sample count | `AGENTS.md` (Performance target), `.github/workflows/benchmark.yml` |
| 3 | Durability is not traded for speed | Originals (event log, blob, local state, runtime journal) are flushed; derived caches are rebuilt. A cell jcode keeps for that reason is reported as a structural difference, not hidden | `AGENTS.md` (Performance target, `[storage] sync`), `docs/protocol/SESSION_STORE.md` |
| 4 | ACP v1 and v2 both | `initialize` answers with the highest version both sides support; adding v2 never breaks v1 | `AGENTS.md` (Fixed requirement 2), `docs/protocol/ACP.md` |
| 5 | Headless and TUI, one engine | The headless package has no terminal stack in its dependency graph; the TUI is a separate package on the SDK | `AGENTS.md` (Fixed requirement 1), `scripts/ci/check_headless_graph.py` |
| 6 | SDK-first | The harness API is the contract between the engine and any client, including the TUI and server code | `docs/protocol/HARNESS_API.md`, `crates/agent-sdk` |
| 7 | Tools cost tokens | The `core` profile sends 7 of its 17 tools every request and reaches the rest through `toolbox`; schema bytes are a budget, not free | `AGENTS.md` (What this build is), `docs/runtime/TOOLS.md` |
| 8 | One model path | OpenAI-compatible `/chat/completions` only; no second provider runtime, router process, or plugin host. External tools arrive over MCP | `AGENTS.md` (What this build is) |
| 9 | Clean cutover | Surface that stopped earning its place is deleted with every caller migrated, not kept behind an alias. The home directory moved to `~/.horon` with no compatibility path | commits `738712a2` (#7), `d6a0f81b` (#9), `a3a70db7` (#88) |
| 10 | Zero CI warnings | A run with warnings is not finished; the cause is fixed, not silenced | `AGENTS.md` (CI warnings), `scripts/ci/run_warnings.py` |

Clean cutover applies to internal leftovers only. ACP v1, harness API v1, and reading sessions stored by v0.1.x are promises to users and stay through v0.1.x (issue #153).

## Test for new work

A proposal fits when the answer to all five is yes. Section "Design principles" in `AGENTS.md` carries the short form.

1. It reduces per-request tokens or per-turn CPU, fsync, or memory, or it makes a measurement more honest.
2. CI `compare` or the structural gate can measure it.
3. It does not grow the headless dependency graph or the binary.
4. It does not break ACP v1/v2, the SDKs, or stored sessions.
5. It does not bring back removed surface under another name.

## Adopted from omp2

omp2 is a much larger product (about 1.17 million lines of Rust and 1,361 locked packages when the plan was written, `.hyper/plans/philosophy-adoption.md`). Each item below was weighed against the test above; the issue is where the work is tracked.

| Idea | What horon does | Issue |
|---|---|---|
| Tool schema tax | Record body bytes, `tools` bytes, and tool count per request in the mock server and hold a ceiling on `core`'s `tools` bytes in the structural gate | #147 |
| Typed verdict | Every tool result carries a verdict (ok, partial, truncated, rejected, failed, interrupted) beside its text; the model still sees text. Staged through the store, harness API, ACP, and the three SDKs, additive at each step | #150 |
| Lenient argument repair with a record | A repaired call keeps its original arguments and the change list, counted per tool and emitted as an SDK event, with no new flush | #151 |
| Degradation receipts | A reduced tool schema or a dropped request parameter writes one receipt (kind, target) and emits an SDK event; no degradation means identical bytes | #152 |
| Honest ledger | Losing cells stay in the results with a reason; compatibility decisions and the ratchets that guard them are written down | #148, #153 |
| Typed errors at the boundary | The public Rust SDK returns an error enum, with `thiserror` as a direct dependency of `horon-agent-sdk` | #156 |
| Measured tool placement | Todo and session search become on by default, and so does swarm, outside SDK-driven sessions (SDK sessions also keep session search off), each placed in whichever tier the #147 measurement shows is cheaper | #154, #155 |
| Written-down principles | This document and a short section in `AGENTS.md` | #149 |

## Left out, and why

| Idea | Reason |
|---|---|
| Per-tool `family@rev` versions and `lift()` | No planned tool-schema change needs it, and #150 keeps the model-facing bytes of existing calls unchanged |
| Forced tool-call steps | An extra request per turn works against principle 1 |
| Streaming dry-run of arguments | Adds CPU per turn and `compare` cannot show a gain |
| A `report_issue` tool | One more tool slot is paid on every request, and a headless run has no reader; repair records (#151) cover the same need |
| KDL model catalog | A parser and about 123 data files in omp2 would grow install size; model names stay in config |
| In-process shell, `dyn` device bus, embedded Python extensions | Rebuilds a plugin host that horon removed (principle 8) |
| Wholesale `anyhow` or `SmallVec` replacement | About 2,000 call sites for no measured gain; memory work targets allocations the profile names |
| Clean cutover of external contracts | Released versions exist; ACP v1, harness API v1, and stored sessions are kept |

## Changing this document

Edit `PHILOSOPHY.md` and `PHILOSOPHY.ko.md` in the same commit with the same headings, tables, and links (`python3 scripts/ci/check_docs_pairs.py` checks it). Neither file is compiled into the binary; only `README.md` and `docs/` are (`crates/app-core/build.rs`), so this document adds no install size.
