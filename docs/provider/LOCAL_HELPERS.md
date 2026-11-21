# Local model helpers

Language: **English** | [한국어](LOCAL_HELPERS.ko.md)

Status: proposed design for #557. Named provider roles and the configurable legacy embedding backend already exist. Neither should start downloading models as a side effect of this proposal. [Provider models](MODEL.md)

## Decision

| Option | Consequence |
| --- | --- |
| Bundle a runtime and weights | Adds install size and startup work for every user. |
| Run a small typed helper through an optional external backend | Reuses one invocation contract while keeping runtime and model files out of the base installation. Selected. |
| Give each caller its own loader | Duplicates model verification, device selection and cache rules. |

## Contract

Start with one `suggest_title` operation. Its versioned request accepts at most 4 KiB of redacted UTF-8 text from the calling feature and returns one title capped at 80 characters plus a backend receipt. A mock backend is sufficient for the first implementation. Memory vectors and predictions can adopt the interface after each defines its own data contract, privacy rules and fallback. The helper never sees provider credentials or full session state.

The backend declaration pins a model identity, immutable revision, license reference, file sizes and SHA-256 digests. An explicit user install verifies the approved manifest, limits total bytes, streams files into a temporary directory, verifies all hashes and atomically activates a complete revision. A failed operation leaves the prior revision available. If caching is not enabled, an explicitly supplied local verified model directory is required. Caching is opt-in, per user and keyed by model revision and digest. No startup, first use, retry or fallback initiates a download. Review license, dependency footprint and cold start before approving any actual model. Do not distribute weights merely because a development fixture can read them.

The user chooses `auto`, `cpu` or an advertised accelerator and a supported precision. `auto` picks an available supported pair. An unsupported explicit choice yields `unsupported_device` or `unsupported_precision` and uses the caller's existing local fallback, such as deterministic title truncation. Return the reason and chosen path to the caller. Preserve the original feature result on missing cache, malformed output or timeout. Use one invocation at a time per backend, a 2-second default deadline, and no network or implicit remote inference. Release child resources on timeout, cancellation or result.

## Implementation boundaries

1. In `crates/config-types/src/lib.rs`, add default-off config and validate explicit device, precision and cache settings. Keep ordinary startup free of helper initialization and I/O.
2. Under `crates/base/src/`, add the typed adapter and one mock-backed title consumer. Verify cancellation, fallback and model metadata without an inference dependency.
3. Review one independently licensed model and engine for artifact size, memory and latency in Actions before a separate verified installation implementation. Keep TUI as a client of existing typed surfaces.

## Verification

Run `cargo test --locked -p horon-agent-config-types --lib`. In Actions use an isolated home, mock child and forbidden network. Start once with default configuration and a directory that would catch any writes. Expect zero downloads, child launches and cache writes. Enable the fixture and send `Plan release` on a mock CPU. Expect `Release plan`. Request the unsupported mock accelerator. Expect reason `unsupported_device`, the deterministic fallback title `Plan release`, and zero model downloads. A bad digest must never become active. A timed-out or cancelled child must leave no live process.
