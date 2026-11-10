# Crate ownership and modularization boundaries

Language: **English** | [한국어](CRATES.ko.md)

> This document mentions "TUI". The `horon-agent` package has no terminal UI and no TUI crate in its dependency graph. An interactive terminal client, when one ships, is the separate `horon-tui` package and binary on `horon-agent-sdk`. The name is here to explain the boundary criteria.
This document defines the target structure that keeps `horon-agent` modular without turning shared crates into a dumping ground. It carries the practical decision rules. Use it when deciding whether to move a type, helper, or behavior out of the root crate.

## Goals

The main goal is to shrink the root crate's recompile scope so ordinary development builds and selfdev builds run faster. A clean structure matters because it supports that compile-time goal.

- Move stable DTOs and protocol-safe state into small crates, so changing root behavior does not recompile those contracts and changing a contract recompiles only the dependents that use it.
- Keep lightweight crates lightweight so they compile quickly and do not pull the big runtime, TUI, and provider dependency graphs into unrelated builds.
- Keep root-only behavior, storage, process, TUI, server, and provider runtime logic in the root crate until the whole dependency boundary can move without spreading dependencies.
- Avoid circular dependencies and hidden coupling through broad `horon-agent-core` re-exports.
- Keep serde compatibility and root re-exports during migration unless every call site is being changed on purpose.
- Success is measured by compile impact: fewer root edits, fewer root-owned DTOs, smaller dependency spread, and faster `cargo check --profile selfdev` plus reload after common changes.

## Platform adapters

OS-specific system calls live only in the five `crates/platform*` crates. The core crates (`base`, `session-store`, `provider-core`, `app-core`) call only free functions from `horon_agent_platform` and do not know which OS they compile on.

```text
crates/base · session-store · provider-core · app-core
   │  horon_agent_platform::{sync_file, sync_dir, tls_client_config, spawn_detached, ...}
   ▼
crates/platform           picks Current by cfg and exposes the fs·ipc·paths·power·process·signal·term·watch modules
   ▼
crates/platform-macos | platform-linux | platform-windows   PlatformAdapter implementations, one compiled per build
   ▼
crates/platform-api       PlatformAdapter trait, shared types, log sink
```

| Crate | Package | Owns |
| --- | --- | --- |
| `crates/platform-api` | `horon-agent-platform-api` | The `PlatformAdapter` trait and shared types (`SyncMode`, `SleepAssertion`, `SleepInhibitor`, `FileStamp`, `FileIds`, `StdinState`, `ReadyChild`), plus the `log` sink. The trait returns `rustls::ClientConfig`, so its only dependency is `rustls`. |
| `crates/platform` | `horon-agent-platform` | Sets `pub type Current` by `cfg` and exposes it as free functions. Builds the TLS config once per process and hands out the `Arc`. |
| `crates/platform-macos` | `horon-agent-platform-macos` | macOS implementation |
| `crates/platform-linux` | `horon-agent-platform-linux` | Linux and the rest of Unix. FreeBSD is also this crate's job. |
| `crates/platform-windows` | `horon-agent-platform-windows` | Windows implementation |

The selection rule is fixed at compile time. `target_os = "macos"` means macOS, `windows` means Windows, `all(unix, not(target_os = "macos"))` means Linux. `crates/platform` declares that choice as target-specific dependencies, so the final binary contains one adapter. The macOS and Windows crates become empty libraries under `#![cfg(...)]` off their target, while the Linux crate is POSIX code that compiles on every Unix target and is selected only on Unix. macOS and Linux share POSIX code (`unix.rs`, `unix_fs.rs`, `unix_term.rs`, `signal.rs`, `ipc.rs`) through `#[path]` and implement only the differing parts themselves.

Trait methods are associated functions and hold no state. Callers call them on the compiled `Current`, so there is no `dyn` dispatch on this path.

The `crates/platform` modules carry these boundaries.

| Module | What it does |
| --- | --- |
| `fs` | Private directory creation, safe file opening and validation, cache stamps, backup rotation |
| `ipc` | Unix sockets and Windows named pipes under the same type names |
| `paths` | Per-OS config and runtime directory rules |
| `power` | Sleep-prevention policy while working, and OS assertions |
| `process` | Process group signals, child placement, ready pipes, diagnostics |
| `signal` | Signal interpretation, process identity, shutdown requests |
| `term` | Standard input state and terminal detection |
| `watch` | File system change notifications |

Adapters send logs to the `horon_agent_platform_api::log` sink. `horon-agent-logging` depends on the application foundation and the foundation depends on the adapters, so an adapter calling the logging crate directly would be a dependency cycle. Logging installs its own function as the sink when it initializes, so messages land in the same log file at the same level.

### Keep OS `cfg` inside the adapters

Do not add new `#[cfg(unix)]`, `#[cfg(windows)]`, or `#[cfg(target_os = ...)]` branches to core crates for OS default behavior. Call a function in `crates/platform`, and if the function is missing, widen the `PlatformAdapter` trait.

`scripts/ci/check_platform_cfg.py` enforces this rule.

- It counts lines that mention `unix`, `windows`, `target_os`, or `target_family` among `#[cfg(`, `#[cfg_attr(`, `#![cfg(`, `#![cfg_attr(`, and `cfg!(`. Test modules and commented-out lines count too.
- Lines left outside the five platform crates are recorded per file with a count and reason in `scripts/ci/platform_cfg_allowlist.json`. A new line is reported as `path:line: text` and fails the gate.
- A dropped count also fails until the budget is lowered with `--update`. It is a ratchet, so the gate cannot loosen itself.
- `--update` rewrites counts only and never invents a reason. A newly added file gets the reason `TODO: classify`, and the gate keeps failing until the reason is written.
- `--report <path>` writes the remaining lines as `path<TAB>count<TAB>reason`.

The lines that stay on the allowlist are product code where the feature itself exists on only one OS. Windows console handling, macOS notifications, and OpenSSH ControlMaster are surfaces that exist only on that OS, not system calls that move into an adapter. When one stays, the reason says why it is not an adapter primitive.

What remains on the list today looks like this. Linux-only Claude Code adoption has `crates/base/src/claude_live.rs` target the process with pidfd and send it signals. Resource metrics have `crates/app-core/src/perf.rs` read Linux `/proc`, macOS `sysctlbyname` and `getloadavg`, and Windows `GlobalMemoryStatusEx` respectively, while `crates/app-core/src/overnight.rs` uses Linux `statvfs` plus process and session hooks. Windows console VT handling (`crates/core/src/console.rs`) and glibc allocator diagnostics (`crates/base/src/process_memory.rs`, and `crates/platform-linux/src/allocator.rs` after it moved into the adapter) sit in the same place. Adapters carry common behavior; these lines are measurement and diagnostic means that exist on one OS, so they stay as product `cfg` with the reason recorded in `scripts/ci/platform_cfg_allowlist.json`.

### TLS trust set

The roots an HTTPS client trusts are the union of the built-in webpki roots and the OS roots, and on no OS is the set narrower than that union.

| OS | Implementation |
| --- | --- |
| macOS | Passes `webpki-root-certs`'s `TLS_SERVER_ROOT_CERTS` to `Verifier::new_with_extra_roots` from `rustls-platform-verifier` 0.7. It does not read the keychain certificates all at once at start. |
| Windows | Uses the same verifier and the same root set. |
| Linux and the rest of Unix | Reads the OS store once per process with `rustls-native-certs` and adds it to the `RootCertStore` alongside the `webpki-roots` roots. Unparsable certificates are skipped, so one bad file cannot lose the remaining roots. Without a store it works from the webpki roots alone. |

- ALPN is `h2`, then `http/1.1`.
- The crypto provider is explicitly aws-lc-rs.
- `horon_agent_platform::tls_client_config()` builds it once behind a `LazyLock` and returns the same `Arc`. Many clients still prepare the OS verifier once per process, and `tls_config_builds()` exposes that count.
- The failing build path uses only the webpki roots, a subset of the fixed union, so it never widens what is accepted.
- Every reqwest declaration uses `rustls-tls-manual-roots` and each client is built with `.use_preconfigured_tls(tls_client_config().as_ref().clone())`. A client that bypasses the adapter fails HTTPS, so the TLS setup path cannot fork in two.
- Tests use `tls_client_config_with_extra_roots` to check that a root the OS trusts and webpki does not still works. They never touch the real trust store.

### Storage durability is core policy; only the flush call goes to the OS

Core, which is independent of the OS, decides what to confirm; the adapter chooses only which system call confirms it. `SyncMode` names the guarantee level, not a specific call.

| Mode | Meaning | macOS | Linux | Windows |
| --- | --- | --- | --- | --- |
| `Full` (default) | The strongest guarantee that survives power loss | `F_FULLFSYNC` (std `sync_all`), flushing the drive write cache too | `fdatasync` (std `sync_data`) | `FlushFileBuffers` (std `sync_all`) |
| `Fast` | The cheapest flush that survives an OS crash | `fsync(2)` | No cheaper call, so the same as `Full` | No cheaper call, so the same as `Full` |

Originals are confirmed with `sync_file` and `sync_dir`.

- Event log appends, origin markers (`event_origin.rs`), archives
- The runtime journal (`durable_runtime/store.rs`) and durable input
- Blobs
- Local state that cannot be rebuilt from the originals: `local-head.json`, `local-head-explicit.json`, `local-explicit-source.json`, `local-runtime.json`

The dirty marker (`.catalog.dirty/<id>` in `writer.rs`) is a regeneration hint, not an original. `writer.rs::dirty` creates the
empty file before the original append but does not flush it. What prevents stale derivatives is the `Stamp` comparison below, and
losing the marker to a power cut still leaves the longer log and the stale stamp, so the next read rebuilds from the original. The
reasoning and the pinning tests are in [`../protocol/SESSION_STORE.md`](../protocol/SESSION_STORE.md).

A batch commit issues one log flush. Appends inside `BlockingWriter::batched` write all original bytes and then defer the flush, and
`flush_pending_log` at the end of the batch issues that single flush. Selection state such as `local-head.json` is written only after that
flush succeeds. If the flush fails, `batched` returns an error and leaves no confirmed selection state, so resending the same event puts it
in the log only once. Blob flushes are still per blob.

Derivatives are written to a temporary file and only renamed. `cache_io::publish_derived` and `event_store::publish_local_derived` are that
path and do not fsync.

- Derivative targets: `checkpoint.json`, `sparse-index.jsonl`, `summary.json`, catalog rows (`.catalog.index.jsonl`), `facade-cache.json`, dirty marker removal

If a rename result is missing after a crash, the next load rebuilds from the original. The condition for trusting a derivative is `Stamp`:
device, inode, length, mtime, and ctime must match the original event log, and the stored value is checked once more with sha256
(`cache_io::decode`). On a mismatch the cache is discarded and rebuilt from the original. Catalog rows for a session with a
`.catalog.dirty` marker are distrusted for the same reason.

`[storage] sync` selects the mode and `HORON_AGENT_STORAGE_SYNC` overrides it for one process. An unknown value falls back to `full` with a
warning once per process. The default is `full`; do not change the Mac default to `fast`.

#### Commit I/O belongs to a fixed-size writer pool

Thread ownership for session-lifetime commit I/O lives in `crates/base/src/session/event_pool.rs`, and
`crates/session-store` still exposes only the `BlockingWriter` blocking boundary. The pool creates
`clamp(available_parallelism, 2, 4)` workers once per process and pins each session to one worker by its
`registry_key` hash. `BlockingWriter` is `Send` but not `Sync`, so the worker's session map owns the value
and `.writer.lock` is released when that entry disappears.

The contract is the same as the earlier structure. One session's jobs run in submission order, `call()` waits
on a response rendezvous, and dropping the last facade drains earlier jobs, receives the `Close` response, and
releases the lock. A job panic makes `catch_unwind` remove only that session's entry, so other sessions on the
same worker keep committing. Re-entry from a worker thread back into its own writer is an error rather than a
deadlock. The cost is head-of-line blocking: if a large session's save holds its worker for a long time, saves
of other sessions pinned to that worker are delayed by as much as that, while sessions on other workers are
unaffected. Threads per session and the RSS slope are the measurements that price this trade.

## Ownership rules

### Type crates own stable data contracts

A `*-types` crate holds:

- Pure data structures shared by several crates or protocol layers.
- Serialization shapes and small pure helper methods tied to the data contract.
- No file system, network, process, TUI, provider client, global state, or storage access.
- Dependencies limited to serde, chrono, and other type crates where needed.

Examples: `horon-agent-session-types`, `horon-agent-background-types`, `horon-agent-todo-policy`.

### Domain behavior modules own root runtime behavior

A root module keeps ownership of behavior when it needs:

- `crate::storage`, `crate::config`, `crate::logging`, `crate::server`, or process execution.
- Provider HTTP clients and authentication managers.
- The Tokio runtime, background jobs, channels, global caches, file locks, PID registries.

Terminal rendering and crossterm/ratatui state are not root concerns: the interactive client is the separate `horon-tui` package and reaches the daemon through `horon-agent-sdk`.

If a type's inherent methods need these APIs, keep the type in the root or move the behavior and its dependencies together into a domain crate. Do not move only the struct and leave a root-side inherent impl that violates the rules.

### `horon-agent-core` is for genuinely shared primitives

`horon-agent-core` holds:

- Primitives between domains that do not yet warrant a domain crate.
- Very small, lightly dependent helpers used by several crates.
- Temporary DTO storage only when it is too early for a new domain type crate.

`horon-agent-core` must not pile up extracted DTOs forever. When the pile grows, split it into domain crates that fit the purpose.

### Compile-speed decision rule

Prefer splits that reduce root crate churn or dependency spread. Do not split just to make files look tidy if a new crate adds dependencies, widens rebuild spread, or forces frequent cross-crate edits. A good split carries at least one of these compile benefits.

- A common root behavior edit no longer touches a stable type definition.
- A type-only change can be checked by compiling the small type crate and its affected dependents.
- Heavy dependencies do not enter a DTO crate.
- Several sub-crates can use a small contract without depending on the root crate.

### Re-export policy

During migration, follow this order.

1. Move the type to the target crate.
2. Leave the old root path as `pub use ...` so call sites keep working.
3. Verify with narrow tests and a reload.
4. Delete the now-unneeded root re-export only after sub-crates can depend on the domain crate directly.

## Move checklist

Use this checklist every time you move a type or pure helper. When the move is not simple, copy it into the PR or commit description.

1. Classify the target.
   - [ ] Is it a stable data contract or pure helper rather than root runtime behavior?
   - [ ] Does it have inherent methods?
   - [ ] Do those methods need root-only APIs such as storage, network clients, TUI state, process management, or globals?
   - [ ] If the behavior must move too, can the whole dependency boundary move without widening spread?
2. Check compatibility.
   - [ ] Is the serde representation unchanged?
   - [ ] Are defaults, skips, renames, and enum discriminants preserved?
   - [ ] Is field visibility still appropriate everywhere?
   - [ ] Can the root keep a compatibility re-export?
3. Check the crate's state.
   - [ ] Does the target crate already have the needed dependency policy?
   - [ ] Are new dependencies limited to libraries that fit a type crate, usually `serde`, `serde_json`, `chrono`, or a sibling type crate?
   - [ ] Does the target crate still have no circular dependency?
   - [ ] Did `cargo metadata` and `cargo check` avoid pulling root, TUI, provider, storage, server, or process dependencies into the type crate?
4. Verify.
   - [ ] Is there a narrow test filter that covers the moved type?
   - [ ] Did `cargo check --profile selfdev -p <type-crate> -p horon-agent --bin horon-agent` pass?
   - [ ] Did the related narrow root tests pass?
   - [ ] Did `cargo fmt` pass?
   - [ ] Did the reload pass on a cleanly committed HEAD?

## Dependency boundary guard

Run this guard after adding or changing a type crate dependency.

```sh
python3 scripts/ci/check_dependency_boundaries.py
```

The guard blocks `horon-agent-*-types` crates from depending directly on root- or runtime-heavy internal crates such as `horon-agent`, `horon-agent-core`, provider crates, TUI crates, protocol and runtime crates, and desktop and mobile crates. Type crates may depend on external lightweight libraries and on other type crates. When a new internal dependency is needed, first decide whether it should itself be a type crate.

## Growth ratchets

Two counts that only a reviewed decision may raise are frozen by one gate.

```sh
python3 scripts/ci/check_growth_ratchets.py
python3 scripts/ci/check_growth_ratchets.py --list
```

| Ratchet | Counts | Ceiling file |
| --- | --- | --- |
| Lockfile packages | `[[package]]` entries in `Cargo.lock`, so one crate at two versions counts twice | `scripts/ci/lockfile_packages.ratchet.json` |
| Internal legacy markers | Rust lines matching `legacy`, `deprecated`, `compat_alias` or `back.?compat`, outside the excluded subtrees | `scripts/ci/internal_legacy_markers.ratchet.json` |

A ratchet file is any `scripts/ci/*.json` file that ends in `.ratchet.json` or has a top-level object declaring `counter` and `ceiling`, so a later ratchet (the `Result<_, String>` count, model-name branches) adds one file and no workflow or `check_all.sh` edit. A `.ratchet.json` that lost its keys fails instead of being skipped. `counter.kind` is `cargo_lock_packages` or `regex_lines`; the latter takes `pattern`, `roots`, `extensions` and `exclude`. The per-gate budgets beside them (`panic_budget.json`, `code_size_budget.json`) declare neither key and stay owned by their own scripts.

The legacy-marker ratchet excludes the subtrees that implement an external contract, each with a reason in the file: `crates/harness-api-server` (harness API v1 bridge and translate layer), `crates/harness-api` (the published v1 surface and its schema snapshots) and `crates/session-store` (the stored-session migration and replay readers for sessions written by v0.1.x). An exclude whose path does not exist fails, so a stale entry cannot widen the hole. Everything else is internal leftover: it may shrink, and it may not grow.

Growth fails; a drop only prints `Consider running ... --update`. A cleanup in an unrelated change must not have to edit this file, and the gate still cannot loosen itself: the old ceiling fails again the moment anything grows back. A new dependency legitimately raises the package ceiling, and that re-pin belongs in the reviewed commit that adds the dependency. `--update` rewrites only `ceiling`, never a description or a reason.

## Test policy

Prefer narrow filters for verification. Broad filters often pick up unrelated stateful tests, timing-sensitive tests, and benchmark tests.

Risks observed in broad filters during modularization:

- `usage` selects app display tests in addition to the pure usage tests.
- `session::` goes beyond session persistence into live-connection server tests and picker behavior.
- `ambient` goes beyond the ambient module's persistence and runtime tests into helper integration tests that handle config and schedule state.

Write the exact filter next to the domain crate or module. A broad filter is still useful for periodic full checks, but it must not block a DTO-only extraction once the exact tests and the compile check pass.

Current narrow verification table after the DTO splits:

| Area | Fast compile check | Narrow root test used during the split | Notes |
| --- | --- | --- | --- |
| Usage DTO | `cargo check --profile selfdev -p horon-agent-usage-types -p horon-agent --bin horon-agent` | Prefer the unit tests in `crates/usage-types` and the exact test names that consume the DTO. A bare `usage` filter also selects display-layer tests, so it is not a required gate. | The DTO crate owns the report and local counter contracts. Runtime lookup, caches, and display stay in the root. |
| Gateway DTO | `cargo check --profile selfdev -p horon-agent-gateway-types -p horon-agent --bin horon-agent` | Narrow the gateway persistence and auth tests to exact test names where possible. | Pairing and token HTTP and WebSocket behavior stay in the root. |
| Ambient DTO | `cargo check --profile selfdev -p horon-agent-ambient-types -p horon-agent --bin horon-agent` | Target only the scheduler and the type consumers. | The ambient DTO crate owns usage records only. Queue, runtime, and prompt behavior stay in the root. |
| Ambient behavior module | `cargo check --profile selfdev -p horon-agent-app-core -p horon-agent --bin horon-agent` | `cargo test --profile selfdev -p horon-agent-app-core ambient::ambient_tests --lib`; `cargo test --profile selfdev -p horon-agent-app-core ambient::scheduler::tests --lib`; `cargo test --profile selfdev -p horon-agent-app-core ambient::runner::runner_tests --lib` | Do not use bare `ambient` as a required gate for a module-only refactor. It also picks up cross-module config state tests. |
| Memory activity DTO | `cargo check --profile selfdev -p horon-agent-memory-types -p horon-agent-core -p horon-agent --bin horon-agent` | `cargo test --profile selfdev -p horon-agent-base runtime_memory_log --lib` | `memory::activity` matches no test today, so use the consumer test. |
| Goal, todo, catchup core DTO | `cargo check --profile selfdev -p horon-agent-core -p horon-agent --bin horon-agent` | When behavior changes, use the exact goal, todo, and catchup filters. | They are small and stable enough to stay in `horon-agent-core` for now. Revisit when change frequency rises. |

## Compile baseline observations

> The measurements below were recorded on 2026-04-30 and are not the current state. The `src/usage.rs` and
> `crates/core/src/usage_types.rs` in the table are the paths of that time and are not in the tree now. Today
> the root `src/` holds only `cli/`, `lib.rs`, and `main.rs`, and `crates/core/src/` has no domain DTO files.

These were measured on 2026-04-30 with `scripts/dev/cargo.sh check --profile selfdev -p horon-agent --bin horon-agent`, right after the
compile-speed boundary document commit. It is a rough benchmark that touches mtime, not a statistical study. It is still enough to set
priorities.

| Scenario | Observed time | Reading |
| --- | ---: | --- |
| No-change check right after a docs-only commit | ~65.8s | The first check can be dominated by environment and cache state. Treat it as a warm-up and noise baseline, not a clean no-change steady state. |
| Touching the root behavior module `src/usage.rs` | ~6.25s | With dependencies already built, a root-only behavior edit is relatively cheap. |
| Touching `crates/core/src/usage_types.rs` (the path at the time; gone now) | ~65.35s | Editing `horon-agent-core` invalidates a wide set of dependents. Do not put frequently changing domain DTOs in `horon-agent-core`. |

Implication: the compile-speed goal is not simply "move things out of root". Moving stable, rarely changing contracts out of the root is good. But piling frequently changing domain DTOs into `horon-agent-core` can backfire because `horon-agent-core` spreads widely. Use purpose-specific leaf crates such as `horon-agent-usage-types`, `horon-agent-gateway-types`, and `horon-agent-ambient-types` for domain DTOs likely to change.

## `horon-agent-core` spread audit

At this point the root crate is the only Cargo target that depends directly on `horon-agent-core`. But the root re-exports many
`horon-agent-core` modules and the root is the expensive target to recompile. In the baseline above, touching `horon-agent-core` invalidated
a wide dependent set. So `horon-agent-core` must be treated as a high-spread crate even if few direct dependents appear in Cargo.toml today.

Root re-exports and usage paths observed:

- `catchup` types live in `crates/app-core/src/catchup.rs`, `goal` in
  `crates/base/src/goal.rs`, and `todo` in `crates/task-types`
- `env`, `id`, `stdin_detect`, `util`, and `panic_util` live in `crates/core/src/`

This audit targeted the root modules before the headless transition. Those modules are no longer in the root `src/`, which now holds only
`cli/`, `lib.rs`, and `main.rs`.

Compile-speed priorities set by this audit:

1. Move domain DTOs that are lumped into `horon-agent-core` and likely to change into purpose-specific leaf crates.
2. Keep stable general utilities in `horon-agent-core`.
3. Do not add new domain DTOs to `horon-agent-core` unless they are very stable or temporary storage.

| Module | Current contents | Long-term home | Notes |
| --- | --- | --- | --- |
| `ambient_usage_types` | Ambient scheduler usage records and rate limit DTOs | Moved to `horon-agent-ambient-types` | The move is done, and the compatibility re-export from then is gone from the root `src/lib.rs`. |
| `catchup_types` | State catchup stores and rendered brief DTOs | `horon-agent-catchup-types` or stay in core | Small and rarely changing. Split only when catchup grows. |
| `copilot_usage_types` | Local Copilot usage counters | Moved to `horon-agent-usage-types` | The move is done, and the compatibility re-export from then is gone from the root `src/lib.rs`. |
| `gateway_types` | Durable records of paired devices and pairing codes | Moved to `horon-agent-gateway-types` | Pairing and token behavior stay in the root. |
| `goal_types` | Goal state, milestones, status, updates | `horon-agent-goal-types` or `horon-agent-task-types` | The domain is bigger. Worth splitting when goals and tool work grow. |
| `memory_types` | Memory activity DTOs | Moved to `horon-agent-memory-types` | Memory carries enough domain weight to have its own type crate. |
| `todo_types` | Todo item DTOs | `horon-agent-task-types`, `horon-agent-todo-types`, or core | Very small. Can merge into the goal and catchup work-state crate. |
| `usage_types` | Provider usage report DTOs | Moved to `horon-agent-usage-types` | Runtime lookup, caches, and display stay in the root. |
| `env` | Environment variable helpers | Stay in core | A general utility that needs no domain crate. |
| `id` | ID helpers | Stay in core | A general utility. |
| `panic_util` | Panic formatting helpers | Stay in core | A general runtime utility. |
| `stdin_detect` | Stdin detection helpers | Stay in core | A general platform and runtime utility. |
| `util` | Other utilities | Audit later | Must not become the place everything collects. |

## Target domain type crates

Domain type splits that are done or clearly worthwhile:

1. `horon-agent-usage-types`
   - `usage_types`
   - `copilot_usage_types`
   - pure account usage DTOs once they are separated from root formatting and runtime helpers

2. `horon-agent-gateway-types`
   - `gateway_types`
   - `GatewayConfig` if config should own it, once that is decided
   - protocol-safe DTOs for the mobile gateway once a mobile crate needs them

3. `horon-agent-ambient-types`
   - `ambient_usage_types`
   - ambient state, request, and result DTOs, but only after the root-only `AmbientState::load/save/record_cycle` methods are split into root free functions or a persistence layer

4. `horon-agent-memory-types`
   - `memory_types`
   - memory protocol and activity DTOs shared by the server, TUI, and tools

5. An optional work-state crate
   - `goal_types`
   - `todo_types`
   - `catchup_types` if the product model wants them together

## Large module refactor targets

These are not simple DTO moves. Refactor the behavior boundary first.

### Session module

The responsibilities of `src/session.rs` before the headless transition are now split between `crates/session-store` and
`crates/session-types`. What remains as split targets on top of that:

- Metadata and the session model
- Persistence and journal replay
- Start stubs and remote start snapshots
- Memory profiling and cache attribution
- Rendering lives in the existing `session/render.rs`
- Crash recovery lives in the existing `session/crash.rs`

### Ambient module

The responsibilities of `src/ambient.rs` before the headless transition now live in `crates/app-core/src/ambient/`. What remains as split
targets on top of that:

- Visible cycle context input and output
- State persistence
- Directive persistence
- Schedule queue and locks
- Prompt composition
- Administrator and runtime coordination

Do not move `AmbientState` into a DTO until the load/save/record behavior is separated from the struct.

### Usage module

The responsibilities of `src/usage.rs` before the headless transition now live in `crates/base/src/usage.rs` and
`crates/base/src/usage/`, and `horon-agent-usage-types` owns the DTOs.

- API lookup providers
- Provider response parsing
- Local cache and sync
- Display formatting
- Account selection and guidance
- Public report DTOs go in `horon-agent-usage-types`

### Gateway module

The responsibilities of `src/gateway.rs` before the headless transition now live in `crates/base/src/gateway.rs` and
`crates/base/src/gateway/`, and `horon-agent-gateway-types` owns the DTOs.

- Registry persistence
- Pairing and token authentication
- HTTP route handling
- WebSocket authentication and upgrade
- WebSocket relay
- Public gateway DTOs go in `horon-agent-gateway-types`

## Harness boundary invariants

These boundaries hold when you move crates or widen the tool surface. Each item names the file that actually owns the boundary. The paths listed here are only those that exist at this change, and the `## Compile baseline observations` table above stays as recorded at measurement time.

### Turn state lives outside the loop and does not know the agent

- `turn_state` does not reference `Agent`. It touches neither sessions, client events, nor disk, and holds only the turn counter and the stop reason: `TurnState`, `TurnOutcome`, and `TurnStateError` in `crates/agent-runtime/src/turn_state.rs`.
- This crate depends only on `thiserror` and tokio's `sync` feature. If storage, provider, or protocol types enter it, the boundary is broken, so add only what can be expressed in primitive types when extending turn state.
- Both turn loops (`crates/app-core/src/agent/turn_loops.rs`, `crates/app-core/src/agent/turn_streaming_mpsc.rs`) record transitions on this type instead of local variables.

### The auto-continue budget belongs to turn state and is capped

- The auto-continue budget lives in `TurnState` and returns to 0 on each new turn
  (`crates/agent-runtime/src/turn_state_tests.rs:20-26`).
- The cap is 2 (`crates/agent-runtime/src/turn_recovery.rs:31`).
- The auto-continue decision for an unexpected stop uses the same function in both turn loops (sequential and streaming)
  (`crates/app-core/src/agent/turn_loops.rs:1392-1396`,
  `crates/app-core/src/agent/interrupts.rs:679-683`).

### Queue consumption commits memory and disk together

- `SoftInterruptQueue` does not touch disk. The queue type is owned by `crates/agent-runtime/src/lib.rs` and the disk snapshot by `crates/app-core/src/agent/interrupts.rs`. Because of that split, no file system dependency enters the queue crate.
- The queue is a newtype around one `Arc<Mutex<QueueInner>>`, and `QueueInner` holds `pending` and `in_flight` together. A clone shares the same session's queue, and transitions modify both vectors inside one lock, so no intermediate state is observable: `transfer_to_in_flight`, `commit`, `restore`, and `pending_snapshot` in `crates/agent-runtime/src/lib.rs`.
- The turn loop builds one `Option<InFlightGuard>` per turn and holds it for the turn's lifetime, and injection only moves entries from `pending` to `in_flight`. It creates no new guard, so a helper returning does not trigger a restore: `InFlightGuard` in `crates/app-core/src/agent/interrupts.rs` and guard ownership in `crates/app-core/src/agent/turn_loops.rs` and `crates/app-core/src/agent/turn_streaming_mpsc.rs`.
- The commit order on normal exit is disk first. `soft_interrupt_store::overwrite` overwrites the pending snapshot and `queue.commit()` clears `in_flight` only when that call returns `Ok`. If the disk write fails, `in_flight` stays, so no message is lost, and injecting once more on the next restore is better than losing it: `commit_soft_interrupt_turn` in `crates/app-core/src/agent/interrupts.rs`, `overwrite` in `crates/base/src/soft_interrupt_store.rs`.
- A turn that ends in failure, cancellation, or panic moves `in_flight` back in front of `pending` and leaves the disk copy alone. At the same time it removes only the messages that turn attached to the transcript, so a retry injects the same message exactly once: `restore_soft_interrupt_turn`, `roll_back_soft_interrupt_injections`, and `settle_soft_interrupt_turn` in `crates/app-core/src/agent/interrupts.rs`.
- Restore reads with `soft_interrupt_store::load` rather than a destructive `take`. A crash before the commit must leave the disk copy in place: `load` in `crates/base/src/soft_interrupt_store.rs` and `restore_persisted_soft_interrupts` in `crates/app-core/src/agent/interrupts.rs`.

### Inactive tools appear in none of the three lists

- A tool that is registered but not yet active appears in none of `definitions*`, `tool_names`, or `closest_tool_names`. All three surfaces see the same inactive set: `inactive_lazy_tools`, `register_inactive`, and `advertised_tool_names` in `crates/app-core/src/tool/mod.rs`, and `closest_tool_names` in `crates/app-core/src/tool/name_suggestions.rs`, which builds the suggestion list.
- Activation is never called while holding the registry lock. `execute` drops every guard in `resolve_registered_tool`, calls the activator, then reads the map again. Calling it under the lock would deadlock when the activator registers a tool.
- The activator is shared from a clone of the `Registry` and from a `WeakRegistry` upgrade. Building a new one per clone splits lazy activation inside one session.

### Midstream rules and foreign rule import are off by default

- `[[stream_rules]]` entries are enforced only when `[features] stream_rules` is on, and the default is
  off (`crates/base/src/config/default_file.rs:342-345`,
  `crates/config-types/src/lib.rs:1352-1375`).
- Importing external agent rule files is also off by default
  (`FeatureConfig` in `crates/base/src/config.rs`).

### Path policy only denies and cannot stop the shell

- Path policy is deny-only and cannot stop `bash`. The policy can refuse but cannot force an allow, and it is checked only before `read`, `write`, `edit`, `replace`, `apply_patch`, `ls`, and `agentgrep` run: `crates/app-core/src/tool/path_policy.rs`.
- The policy store is not a `Registry` field. It registers and unregisters by session key, owner token, and `Drop` cleanup. That keeps ownership from overlapping the tool registry's clone semantics.
- `bash` reaches any path through the shell and is outside this hook, and a symbolic link bypasses it too. Neither limit is a missing implementation; both are outside what this policy covers.

### Session extension state grows only inside the recording log

- Session extension state lives inside the session recording log and grows only without breaking `KnownEventKind`'s existing 8 kinds and `AuxiliaryRecordedPayload`'s existing 4 variants: `crates/session-store/src/schema.rs`.
- Step 1 puts it inside the `Legacy { schema_version, value }` envelope of the existing `auxiliary_recorded` kind. Old builds decode this variant normally and ignore it in projection.
- Step 2 writes the new kind only after every supported reader can read it. That change updates the `ALL` array length, `as_str` and `parse`, `typed_payload_ok`, the `Payload` and `Projection` fields, and the session event format table in the same commit. Writing a new variant first and shipping readers later leaves old builds unable to open that session.
- Do not push extension state into `ProjectedMessage`. That mixes it into the model conversation history, and tool validation and compaction boundary math then count the entry as conversation. Keep separate fields and per-kind accessors.

### Wire version constants are written with their owning crate

- Wire version constants belong in one contract, with each constant's owning crate named beside it. Four constants are spread across two crates today and the unified type is not in the code yet.
- `REPLICATION_PROTOCOL_VERSION` has the same name and the same value but two owners. The storage-side original event exchange contract is `crates/session-store/src/replication.rs`, and the bridge-side second definition of the same name is `crates/harness-api/src/replication.rs`.
- The harness API frame's major and minor are `API_VERSION_MAJOR` and `API_VERSION_MINOR`, both owned by `crates/harness-api/src/lib.rs`. The frame's `v` carries only the major today, so a minor bump is invisible to clients. When unifying, carry major and minor as a pair and split the two same-named replication constants into `STORE_REPLICATION_VERSION` and `REPLICATION_VERSION`, with a test asserting that their values match.
- MCP stays out of this contract. MCP's `protocol_version` is chosen by the peer server and we cannot raise or lower it.

### The IRC bus crate uses only synchronization primitives

`horon-agent-irc-core` (crate path `crates/irc-core/`) owns the mailbox bus that runs only inside a process. It touches no server state, socket, config, or disk, and uses only synchronization primitives such as `Mutex` and `oneshot` plus `Arc` sharing. Dependencies are limited to tokio's `sync`, `time`, `macros`, and `rt` features and `futures` (`crates/irc-core/Cargo.toml`). Because it builds no runtime, unit tests and throughput examples run without a server, and the server-side connection is owned by `crates/app-core/src/server/irc/`.

This placement is an exception to the ownership rule that type crates take no global state or runtime dependency. The bus is not a data
contract but a small engine with behavior, and putting that behavior in the server crate would make it impossible to verify without a
server. Two rules keep the exception narrow.

- The bus body (`crates/irc-core/src/bus.rs`) reads no globals. The daemon installs it once from `install` in `crates/app-core/src/server/irc/mod.rs` after the listener binds, and removes it when the daemon run ends.
- Global access is exposed only through the three functions in `crates/irc-core/src/global.rs`, and session removal cleanup is handled by `crates/app-core/src/server/session_forget.rs` calling the bus's `forget`.

If a new feature needs server session state, sockets, or disk, widen `crates/app-core/src/server/irc/` instead of adding it here. Pure queue, table, and ordering rules stay in this crate.

## "Good enough" definition

The structure is good enough when these hold.

- Every type crate has a clear domain and a minimal dependency set.
- `horon-agent-core` holds only genuine primitives or documented temporary storage modules.
- Root modules no longer mix large DTO piles, persistence, runtime coordination, and rendering in one file.
- Every domain has a narrow verification command.
- The selfdev build and reload work cleanly after every structural change.