# Session store

Language: **English** | [한국어](SESSION_STORE.ko.md)

This document covers the on-disk format and transfer contract of the session event log, and the durable runtime state journal layered on top of it.

## Session event store: disk format and transfer contract

This is the reference document for what horon-agent stores per session, how it hashes and verifies those bytes, how two instances replicate, and how legacy sessions are migrated. The authoritative types live in `crates/session-store/src/schema.rs`; this document explains those types and names the module that owns each rule.

There is exactly one store root: `$HORON_HOME/sessions` (normally `~/.horon/sessions`). Every path below is relative to that root. Every file is owner-only (directories `0700`, files `0600`), every accessor re-validates the session id, and accessors under the root reject symbolic links, non-directory components and components owned by another user. So even a well-formed id cannot point outside the store (`schema.rs::SessionPaths`). Windows has neither permission bits nor `O_NOFOLLOW`, so owner-only is carried by the inherited ACL on the home directory and files are checked after opening. Details are in the architecture notes of [`release/WINDOWS.md`](../release/WINDOWS.md).

### Layout

```
sessions/
  <id>/                      canonical session directory
    events.jsonl             append-only canonical event log (the original)
    blobs/<sha256>.json      externalized payload bytes, named by their own hash
    local-head.json          local original: the selected head, written only for an explicit selection
    local-head-explicit.json local original: whether the head selection is explicit (absent = not explicit)
    local-explicit-source.json local original: path and SHA-256 of the explicit snapshot the session came from
    local-runtime.json       local original: resume binding (working_dir, migration_epoch, provider_session_id, status)
    checkpoint.json          derived cache: head, source stamp, metadata, compaction, tail
    sparse-index.jsonl       derived cache: every 128th event plus message offsets
    summary.json             derived cache: one catalog row for this session
    facade-cache.json        derived cache: session facade payload and its hash
    .writer.lock             session-lifetime exclusive writer lock (never unlinked)
    .blob-staging/<sha256>/  resumable inbound blob chunks
    .record-transfer/        resumable inbound whole-record staging
  <id>.json                  legacy snapshot (preserved, never rewritten)
  <id>.journal.jsonl         legacy journal tail (preserved, never rewritten)
  <id>.bak                   legacy recoverable snapshot (preserved)
  .catalog.index.jsonl       derived cache: one row per session, for listing
  .catalog.lock              catalog write lock
  .catalog.dirty/<id>        rebuild hint: empty file, one per session being rebuilt
                             (created before the originals, never flushed)
  .origin-id                 this HOME's writer identity, created once at 0600
  edit-stats/                per-session edit counters (not a session)
```

The originals of session content are `events.jsonl` and `blobs/`. Four local state files that the event log cannot reproduce are originals as well. `local-head.json` is written only when the selection is explicit; `local-head-explicit.json` records whether there is such a selection; `local-explicit-source.json` records the path and hash of the explicit snapshot the session came from; and `local-runtime.json` holds the `working_dir`, `migration_epoch`, `provider_session_id` used for resuming plus a display `status`. `local-runtime.json` is rewritten only when one of those fields changes; a batch that moved only `last_pid` and `last_active_at` writes no file. The single original of `status`, `last_pid` and `last_active_at` is `local-runtime.json`. The event log's auxiliary record (`AuxiliaryRecorded`) does not carry these values, and the loader restores them from the binding. A save that changed only state, such as the close at the end of a run, therefore commits the binding alone and appends nothing to the log (the binding-only path in `event_save.rs`).

The runtime journal holds a second copy of that binding. Every `run.started` and `run.stopped` row may carry `binding`, the same fields plus a `rev`, and the highest `rev` wins on read. The older of the two originals is therefore not overwritten: `local-runtime.json` gains the `rev` of the row that already states it, and a load takes the higher of the file's and the journal's. A file without `rev` is what a build before this change wrote, so it wins outright - that build never recorded a version and may well have written last. When the file is missing, zero bytes, or unparsable, the journal answers; when neither states a binding, the loader keeps its defaults. The next binding takes one above the highest `rev` either copy holds, so a compaction that dropped the journal's copy never hands out a version the file already used. A publish whose value a journal row confirms writes the file by rename alone, because a crash can only lose the file and the journal answers the same value; every other publish, including a change no row states and a file that already exists without a `rev`, flushes the file and its directory. A one-shot `run` writes its start row before the turn's first commit, so the first binding of a new session is already answered and that commit renames; a row that does not land leaves the commit flushing as before.

The downgrade limit is deliberate: a build before this change rewrites the journal without `binding` when it repairs a tail or compacts, and writes `local-runtime.json` without `rev`. Neither is corruption. The next read of that session falls back to whichever copy still states a binding, or to the defaults when both lost it, and the following publish flushes to recover a version. Only a crash between a confirmed row and the file can leave a mirror one version behind the journal, which the higher-`rev` rule resolves in the journal's favour.

When `local-head-explicit.json` is absent, every reader (`event_store.rs`, `event_cache.rs`, `archive.rs`, `history_page.rs`, replication `replication.rs`) reads it as "no explicit selection". So `false` is not written for a session that has neither `local-head.json` nor the flag (`event_store.rs::select`). A new session that has never had an explicit selection saves one file flush on its first commit, and after a crash or power loss "file absent" and "`false`" read as the same state, so nothing is lost. A session that has been explicitly selected once keeps the flag, so clearing the selection still records `false` as before.

Originals are committed with `sync_file` and `sync_dir`. `checkpoint.json`, `sparse-index.jsonl`, `summary.json`, `facade-cache.json` and `.catalog.index.jsonl` are derived. Derived files are written to a temporary file and only renamed, never flushed, so a crash costs only regeneration. A derived file that fails its stamp or sha256 check is rebuilt under the writer lock. The boundary between original and derived, the regeneration conditions and `[storage] sync` are described in "Storage durability is a core policy and only the flush call is left to the OS" in [`../architecture/CRATES.md`](../architecture/CRATES.md). `doctor` reports original corruption and does not repair it.

A newly created directory survives a power cut only if its entry is committed in the parent directory. The session writer flushes the parent only when it created the `sessions/` root while opening it (the `ensure_dir` of `writer.rs`). Entries in a root that already existed were committed when they were created, so the parent is not flushed again on every session open.

A `.catalog.dirty/<id>` marker is an empty file with no content. Readers check presence, never bytes (`cache_io.rs::dirty_present`). The marker hints that this session's derived files must be rebuilt soon; it is not an original. That is why the writer creates the marker before the originals without fsyncing it.

- The marker is created before the first original append of the batch. When creation fails, the append returns an error and no bytes enter the event log (`IoPoint::DirtyCreated`).
- Neither the marker file nor its directory entry is flushed. This decision removed one directory fsync per commit (about 5 for a one-shot question).
- A marker created once inside a batch is reused as it is.

**Why dropping the fsync is safe.** Without a marker there is no path that trusts a stale derived file. Every derived file also stores the `Stamp` of the original event log (device, inode, length, mtime, ctime) and compares it against the current log on every read. `checkpoint.rs::load` checks the stamps of the log, index and summary for a checkpoint, and `catalog.rs::list` checks the row's stamp and `summary.json` for a listing row. An original append grows the log, so the stamp necessarily changes, and a mismatched derived file is rebuilt from the originals under the writer lock. The log is flushed before the save finishes, so when power is lost and the marker or its directory disappears, what remains is "a grown log plus derived files with stale stamps", and that combination always leads to regeneration. The cost is one regeneration at the next read.

Tests pin this reasoning (`tests/dirty_batches.rs`).

| Test | Situation it imitates | What it checks |
| --- | --- | --- |
| `an_empty_marker_left_by_a_crash_repairs_the_next_listing` | Stopping in the same process just before a derived file is published | The next listing reads the originals and fixes the row |
| `a_marker_lost_to_power_loss_never_serves_a_stale_listing_or_context` | A marker that a power cut prevented from being flushed, or the whole marker directory disappearing | Both the listing head and the conversation context reach the newest original, and the next save works too |
| `a_writer_killed_inside_a_save_leaves_originals_the_next_reader_serves` | Killing the writer process with `abort` inside a save batch (both with the marker left and with it deleted) | The originals of the dead save remain in the log and the reader returns them as well |

The previous contract treated the marker as an original and committed it with a directory fsync. The stamp comparison above already provided the same guarantee, and this document already said "losing that entry costs one rebuilt listing row", so the fsync was redundant. The marker that import (`legacy_publish.rs`) creates is an ordering hint before the originals appear, so it is still flushed.

A batch commit flushes the log once. Appends inside the batch write all their original bytes but defer the flush, and `flush_pending_log` at the end of the batch commits the file once. Selection state files including `local-head.json` are written only after that flush succeeds. When the flush fails, the save returns an error and no selection state remains, so resending the same events puts each event into `events.jsonl` exactly once. Blobs are still flushed one at a time.

`edit-stats/` is a reserved child of the root that holds ledger data rather than sessions, and the catalog skips it explicitly (`reader.rs::EDIT_STATS_DIR`).

### One event record

`events.jsonl` holds one canonical JSON object per line, ending with a newline (`schema.rs::EventEnvelope`).

| Field | Meaning |
| --- | --- |
| `schema_version` | Record schema version, currently `1` |
| `session_id` | Validated session identifier. Only ASCII letters, digits, `.`, `-` and `_` are allowed; it must start with a letter or digit, cannot start or end with a dot, is at most 160 characters, and may not be a Windows device name |
| `event_id` | UUIDv4 string, reserved before any I/O |
| `parent_event_id` | The previous event on this branch. Absent only at the root |
| `origin_id` | UUID of the installed HOME. Identifies the writer, not the user |
| `timestamp` | UTC RFC3339 with nanosecond precision. Part of the hash, therefore part of the identity |
| `kind` | Event kind string |
| `payload` | Inline JSON body, mutually exclusive with `payload_ref` |
| `payload_ref` | `{sha256, size, encoding}` of the externalized body |
| `content_hash` | SHA-256 over every other field |

The line is canonical JSON. There is no whitespace, object keys are recursively sorted by UTF-8 byte order, and array order is preserved (`schema.rs::canonicalize_value`, `canonical_json`). `content_hash` is computed over the envelope with `content_hash` emptied, and `timestamp` always serializes in the same fixed format, so the same instant can never hash differently.

**Externalization happens before hashing.** A payload that would push an inline record past the 8 MiB per-record limit (`MAX_RECORD_BYTES`) is written to `blobs/<sha256>.json` and replaced by `payload_ref`. The record is then hashed in that final form (`schema.rs::EventDraft::seal`). An inline record and a blob-reference record are therefore different identities, and a reader never has to guess which one it is looking at.

Decoding validates in this order: the line parses, the version is supported, the payload representation is exactly one of the two, the `payload_ref` hash/size/encoding is valid, the typed payload matches its kind, and `content_hash` matches the recomputed value. A record that fails any of these is corruption, not something to skip.

### Event kinds

This build projects eight kinds. Unknown kinds are preserved as they are and refused wherever a projection has to interpret them (`schema.rs::KnownEventKind`).

| Kind | Payload | Meaning |
| --- | --- | --- |
| `session_created` | `SessionCreatedPayload` | Complete durable metadata. `branch_root`/`reset_provenance` mark an explicit reset root |
| `message_appended` | `MessageAppendedPayload` | One visible message |
| `message_revised` | `MessageRevisedPayload` | Replacement of an existing append, named by event id |
| `metadata_changed` | `MetadataChangedPayload` | Complete small metadata. Not a delta |
| `compacted` | `CompactedPayload` | A summary, bound to the original range it covers |
| `compaction_blocked` | `CompactionBlockedPayload` | Why compaction was refused or stopped |
| `archive_changed` | `ArchiveChangedPayload` | Hide or unhide. Archiving never deletes |
| `auxiliary_recorded` | `AuxiliaryRecordedPayload` | Environment snapshots, memory injection, replay events, preserved legacy auxiliary data |

`SessionMetadata` deliberately leaves out messages, compaction, environment snapshots, memory injection, replay events, every runtime cache, PIDs and socket fields. Unknown old keys stay in `legacy_extensions` and never come back as runtime state.

### Branches and heads

The log is an immutable parent graph. `EventGraph` validates it on open: a parent must exist, ids must be unique, cycles and conflicting identities are rejected, and identical records are deduplicated (`heads.rs::EventGraph`). A head is an event with no children.

- The selected head is **host-local** and is never exchanged between instances.
- With one head, execution runs automatically. With several, execution is refused until a head is chosen explicitly.
- A revision appends `message_revised`. A rewind chooses an earlier head and appends a new branch. `clear` starts a new empty branch root with reset provenance. No operation deletes an original.

### Compaction records

`CompactedPayload` is the resume contract, and every field exists so that a stale or partial summary can be detected (`schema.rs::CompactedPayload`).

| Field | Meaning |
| --- | --- |
| `summary_text` | Human-oriented summary. It may be empty for a valid provider-native window |
| `source_head_id` | The branch head when the work started |
| `covered_through_event_id` | The last original event the summary actually covers |
| `source_prefix_digest` | Canonical digest of the covered prefix |
| `first_kept_event_id` | The first event kept as tail instead of summarized |
| `prior_compaction_id` | The previously applied compaction, used for incremental summaries |
| `provider` / `model` / `route` / `settings_digest` / `generation` | What produced it, under which settings |
| `input_tokens` / `output_tokens` | Usage, when known |
| `outcome` | `summary`, `native`, `lossy_emergency`, `handoff` |
| `native_window` | Provider-native window entries, in order, preserved whole |
| `restoration` | Bounded `{path, kind, note}` references plus a byte budget. Not file bodies. Written by a structured summary and by a `handoff` boundary alike |

`handoff` marks a boundary written from a generated handoff document rather than a summary (`crates/app-core/src/agent/handoff_compaction.rs`). The document is stored as `summary_text`, and the files its covered prefix read are stored in `restoration` under the same bounds as a structured summary. It is never empty and always has a `Goal` heading: an empty document, or one without that heading, is refused before it is written. No other section is checked, so a document missing one is still a valid `handoff`. A failed, cancelled or refused generation writes no record at all, so the transcript, the previous boundary and the message count stay exactly as they were.

A compaction that cannot make progress appends `compaction_blocked` with its reason instead of pretending the summary succeeded (`context_recovery_blocked`, `context_budget_too_small`, `provider_error`, `overflow_no_progress`, `stale_source`). A length notice or an empty structural summary is never recorded as a successful `summary`. A valid native window with an empty `summary_text` is recorded.

The strategy that owns compaction is `strategy` under `[compaction]`. It is one of `structured` (the default), `handoff`, `openai_auto` and `openai_explicit`. `handoff` runs the ordinary compaction job and stores a generated handoff document as the boundary, tagged `handoff` so the next request introduces it as the successor's own earlier writing (`crates/base/src/compaction/handoff_provider.rs`). A handoff that cannot be written falls back to a structured summary for that same job. `[compaction] method_order` may name that chain explicitly (`["handoff", "structured"]`); when it is absent or rejected the strategy alone decides, and a provider-native strategy ignores it (`docs/runtime/TURNS.md`, `crates/base/src/compaction/method_order.rs`). When `[provider] openai_native_compaction_mode` is set explicitly to `auto`/`explicit`, it is honored only when `strategy` is unset. Absent, empty and `off` all resolve to `structured`. The budget defaults in the same section are `recent_tokens` 20,000, `reserve_tokens` 16,384 raised up to `min(window * 0.04, 49,152)`, and a window-graded trigger ratio running from 0.45 at 16k to 0.80 above 512k (`crates/compaction-core/src/lib.rs`).

The model that produces a structured summary can be changed with `summary_model` in the same section (environment variable `HORON_AGENT_COMPACTION_SUMMARY_MODEL`). Empty, or equal to the session model, uses the session model. When set, the session provider is forked and the summary is requested with that model, and if that call fails with a provider error the request is retried once with the session model. When the provider rejects the model, only a warning is left and the session model is used. It does not apply to provider-native compaction paths and does not enter job identity (the path identifier and settings digest) (`crates/base/src/compaction/summary.rs` `generate_structured_summary_with_model`).

### Derived caches

- `checkpoint.json` holds the selected head, the source stamp (log epoch/inode, committed offset, anchor hash), the current metadata, the last compaction and the retained tail. A normal append does not rewrite the prefix.
- `sparse-index.jsonl` holds every 128th event plus message offsets, so old events can be fetched by byte range without a full replay.
- `summary.json` and `.catalog.index.jsonl` are listing rows. Listing never parses a transcript.
- `facade-cache.json` holds the session facade record and its hash. A session above the size cap keeps no cache and is rebuilt from the originals.
- A cache hit reads only `checkpoint.json` and the tail. A size decrease, an inode/epoch change, an anchor mismatch or a same-length edit forces a full replay. Finding an arbitrary modification inside an unchanged prefix is `doctor`'s job, and a warm connection does not claim to hash the whole prefix.
- The last commit of `horon run` leaves its derived cache to the next reader. The commit still writes its originals, its runtime binding and its selected head, but it skips the derived rebuild that would be paid for a process about to exit. The next reader finds a checkpoint whose stamp no longer matches the log and rebuilds it under the writer lock, exactly as it would after a crash. The deferral is armed once and spent by that commit whatever its outcome, and a long-lived session never sets it.

### Locks and the writer pool

Each session's single `BlockingWriter` holds an `flock` on `.writer.lock` for the session's lifetime. A second writer, including one in another process, is answered `busy`. Catalog writes take `.catalog.lock`, and the catalog lock is never held while taking a session lock. A dirty session is rebuilt by whoever reads it, so a stale listing cannot outlive a failed cache refresh for long.

Commit I/O does not create a dedicated thread per session. A process-wide writer pool creates `clamp(available_parallelism, 2, 4)` workers (`session-writer-0` and so on) once, and a session is pinned to one worker by the hash of its `registry_key` (the canonicalized session directory path). The key is canonicalized first so two spellings of the same session directory never land on different workers and open two `BlockingWriter`s. The hasher yields the same value for the life of the process, so a session stays on the same worker for its whole lifetime.

The pool keeps the contract of the previous structure.

- **Submission order**: jobs for one session run on its worker in submission order. `call()` waits on a response rendezvous until the job finishes, so the caller signature and the blocking semantics are unchanged.
- **Lock lifetime**: `.writer.lock` is held by the `BlockingWriter` owned by the worker's session entry. The entry disappears when the `Close` queued by the last facade's drop arrives, so the drop drains every earlier job before releasing the lock. The `BlockingWriter` is declared before the slab lease in the entry, so a waiter reopening the same session wakes after the lock is released.
- **Panic isolation**: jobs are wrapped in `catch_unwind`. A panic removes only that session's entry, which releases the lock, and the other sessions on the same worker keep committing. Waiting callers of that session lose their response and receive a "reply lost" error, and the process stays alive.
- **Reentrancy**: calling or dropping a worker thread's own writer re-entrantly becomes an error instead of a deadlock. The open path returns an error for the same reason when it is called on a worker.
- **Head-of-line blocking**: one save reads the event log three times, so saving a large session can hold that worker for hundreds of milliseconds, and saves of other sessions pinned to the same worker wait that long. Sessions on other workers are unaffected. This is the price of decoupling thread count from session count, and it is measured together with the per-session thread and RSS slope.

### Replication transport

Two instances exchange **originals only**: canonical event records and original blobs. Checkpoints, indexes, PIDs, leases, local head selections and local runtime bindings are not sent, and a local file offset is never used as a remote cursor (`crates/session-store/src/replication*.rs`).

- `protocol_version` is `1`. A page carries at most 256 events (`MAX_PAGE_EVENTS`) and at most 256 known ids, and is limited to 16 MiB of actual escaped wire bytes (`MAX_WIRE_BYTES`).
- `CanonicalEventRecord { event_id, content_hash, canonical_json }` carries the canonical event JSON as a **string**. A client must not parse it and re-serialize it. `u64` values above 2^53, `1.0` and `-0.0` do not survive a round trip through JavaScript numbers. The Rust store parses, canonicalizes and validates the string and checks it against the outer id and hash.
- Export walks the parent/reference closure of a pinned head with parents first. The first page pins the head. A cursor that holds a different head or does not describe the page and its closure is `invalid_cursor`.
- A cursor is a self-checking base64url object. It carries the version, session, head, closure digest and the next canonical phase index. Parent ties are broken by event id.
- Import validates the whole page before committing anything: schema, hashes, ids, paths, parents, blob and compaction references. The same id with the same hash is a no-op, the same id with a different hash is `hash_conflict`, and a missing parent or blob is `missing_dependency`. An I/O failure can leave a synced prefix, because a page is not an atomic batch. `accepted_ids` and `retry_cursor` say what to send next, and resending is idempotent.
- Branched heads are all preserved and reported as `conflict_heads`. No timestamp decides a winner.
- An import into a session with active writes answers `busy` and is retried later. The state of a running session is never changed behind its back.
- Blobs move in contiguous 1 MiB chunks (`BLOB_CHUNK_BYTES`) and are verified by SHA-256 and size before publication. Chunks are staged as `.blob-staging/<sha256>/<offset>.chunk` with a `size` manifest, so an interrupted transfer resumes at the first missing offset. The same bytes at the same offset are idempotent, different bytes are `conflict`, and an offset beyond the durable prefix is `missing_offset`. A blob reference is never imported before its bytes are verified.
- When a canonical record itself is too large to carry inline on the wire, the page lists it in `record_refs` at its logical position and the bytes move as `RecordBlobChunk` values through `.record-transfer/`. This is only a transport mechanism; the stored log is never rewritten.

### Harness API surface (protocol 1.12)

The store is reached through the harness API. The major version stays at `1` and the minor version moved from 11 to 12. A client negotiates the handshake and then checks the capabilities it needs before sending anything.

| Capability | What it gates |
| --- | --- |
| `session_event_store_v1` | `list_session_heads`, `select_session_head` |
| `session_replication_v1` | `export_session_events`, `import_session_events`, `export_session_blob`, `import_session_blob`, `get_session_blob_status`, and the record blob methods |
| `session_history_cursor_v1` | `get_history_page` |
| `structured_compaction_v1` | Reading the structured summary fields of a `compacted` record. No method is gated by this capability |

All four are advertised by the bridge itself (`BRIDGE_CAPABILITIES` in `crates/harness-api-server/src/lib.rs`) and are not detected from the daemon. They are values describing what this build's store and compaction engine produce. The first three also gate SDK methods, so a client sending one of them without the capability receives `unsupported` naming it. The fourth gates no method. It is a signal a client can read before interpreting a `compacted` payload, so no SDK call is refused because of it.

The new requests are `ListSessionHeads`, `SelectSessionHead`, `GetHistoryPage`, `ExportSessionEvents`, `ImportSessionEvents`, `ExportSessionBlob`, `ImportSessionBlob`, `GetSessionBlobStatus`, and the whole-record pair `ExportSessionRecordBlob` / `ImportSessionRecordBlob` plus `GetSessionRecordBlobStatus`. The responses are `SessionHeads`, `SessionHistoryPage`, `SessionEventPage`, `SessionImportResult`, `SessionBlobChunk`, `SessionBlobStatus` and `SessionRecordBlobStatus`.

`GetHistory` keeps its meaning: the whole visible transcript. `GetHistoryPage` is the new bounded read and takes `head_event_id` (defaulting to the selected head), the opaque continuation `before_event_id`, and `limit` with a default of 50 and a cap of 200. Within a page, messages come back oldest first, and a pinned-head cursor keeps pointing at the same history after the selection changes.

Older clients keep working. They need no new request, the existing `ErrorCode` values (`unsupported_version`, `unknown_request`, `unknown_session`, `invalid_request`, `internal`) are unchanged, and finer causes go into an optional `details` object (`store_code`, `retryable`, `accepted_ids`, `retry_cursor`, `conflict_heads`) that older clients simply ignore. `store_code` values come from `StoreErrorCode` in `crates/session-store/src/error.rs` (for example `hash_conflict`, `missing_dependency`, `conflict_heads`, `busy`, `invalid_cursor`, `record_too_large`, `migration_required`). `unknown_session` is used only for a session that does not exist. `busy` and I/O failures map to `internal`, and `retryable` is set when a retry helps.

Status is assembled from the current host runtime, not from a past PID. A stored PID is legacy auxiliary data, and no reader revives it as live state.

### Legacy layout and migration

A session written by an older build exists as `sessions/<id>.json` with an optional `sessions/<id>.journal.jsonl`, and `sessions/<id>.bak` is a recoverable snapshot. Both layouts are found by one enumeration rule (`reader.rs`), and the layout is reported per session.

Migration is offline and non-destructive (`legacy.rs`).

- Legacy bytes are replayed into the current state, including `.bak` recovery and the journal tail, and unknown metadata and auxiliary data are preserved.
- The source is hashed before and after, a live legacy writer is refused, and the prepared copy carries the new log and a source-hash manifest. Publication happens only after sync and verification finish.
- Old files, SQLite databases, WAL, SHM, `sdk-archive.json`, to-do state and edit statistics are neither deleted nor modified.
- Migrating the same source again is a deterministic no-op (deterministic origin id and event ids). A different source hash is `source_conflict`.
- When the new log verifies, it is read first. Otherwise the legacy originals are read and a non-destructive migration is offered.
- Display metadata that exists only in the old SQLite index is imported through an explicit, read-only, opt-in path. `session migrate --apply --legacy-index` reads a protected prepared copy inside one consistent transaction. That path exists only in builds with the `legacy-session-index-import` feature, and `--legacy-index` is hidden from `--help` in a build without it (`src/cli/args.rs`). The default build opens no database and refuses with `migration_required`. Normal operation opens no database, and a database whose display metadata was not imported emits the same `migration_required` diagnostic instead of a listing whose titles quietly vanished.
- Every surfaced refusal names the build that can finish the job: `session migrate`, `session inspect` and `session doctor` print the same remedy — install a binary compiled with `--features legacy-session-index-import`, then re-run `--legacy-index`. The release build carries `pdf` only (`.github/workflows/release.yml`), so a released binary says "install such a build" rather than telling the operator to re-run a flag it hides and refuses.
- Passing `--legacy-index` to a build without the feature fails before any session is read, with that same remedy and without touching the database.

#### SQLite that is not a session is out of scope

SQLite is not part of the session store. It survives only as the **legacy** recent-session index, reached through the `legacy-session-index-import` cargo feature and the `--legacy-index` flag. Other SQLite state (to-dos, edit statistics, archives, telemetry) is never read, written or converted by the event store, and the store's normal read and write paths link no database handle.

### Operator commands

`session inspect` reports the layout, event count, heads, selection, message count, title, archive state, torn tail bytes and corruption, read-only. `session doctor` verifies every original (or one) and does not repair. `session migrate` is a dry run without `--apply`, and `--from` reads another HOME without modifying it. `session export` / `import` move a bundle. `export-legacy` rewrites the current projection into another HOME's legacy layout before a downgrade. `recover-to` salvages the verified prefix into a new root without overwriting the originals. `archive` / `restore` append `ArchiveChanged` records. The retention policy hides sessions and never deletes them. The subcommand group is `session` with `sessions` as an alias.

`session delete` is the destructive operation, and it is not archive. It removes the transcript directory, the legacy JSON and journal, the artifacts, and every file keyed by the session id: the five to-do files, the goal attachment under `goals/sessions/`, `pending-soft-interrupts/`, `session_leases/`, the three PID markers, `context-notes/`, `durable-runtime/`, `failover-budget/`, both edit-statistics files and the catalog dirty marker. A daemon that is running also removes its reload context and reload recovery record. Process-wide state is not a session's and stays: `managed-skills`, `models`, `bin`, `logs`, `cache`, `memory`, approvals, `generated-images`, `safety`, `devices.json`, `servers.json`, `config.toml` and the usage database.

The delete is idempotent — a second delete of the same id removes nothing and still succeeds — and it refuses an id the store would not accept as a path component. Through the daemon it also refuses a session a turn is running for, one another process is streaming, or one still attached to a client: deleting under a live writer would race that writer. An idle session the daemon holds between turns, including one kept for its reconnect grace after its client left, is closed first and then deleted. `session delete --yes` goes through the daemon when one is running and edits the store directly otherwise.

### Limits

| Limit | Value | Owner |
| --- | --- | --- |
| Record bytes | 8 MiB before externalization | `schema.rs::MAX_RECORD_BYTES` |
| Session id | 160 characters | `schema.rs::MAX_SESSION_ID_LEN` |
| Restoration entries | 10 per compaction | `schema.rs::MAX_RESTORATION_ENTRIES` |
| Page events / known ids | 256 each | `replication.rs` |
| Page wire bytes | 16 MiB, escaped envelope included | `replication.rs::MAX_WIRE_BYTES` |
| Blob chunk | 1 MiB | `blob_exchange.rs::BLOB_CHUNK_BYTES` |
| History page | Default 50, maximum 200 | `GetHistoryPage` |
| Catalog index rewrite | Dead bytes above twice the live bytes, or above 64 MiB | `catalog.rs::needs_compaction` |
| Marker-log window for a new session | The marker becomes durable before the first log line. If the process dies in between, only the marker remains and the next listing sees that session as `busy`. Reopening the session and saving clears it. Making the log durable first would reverse the order on every append, so this window stays as a limit of the current structure | `writer.rs::dirty`, `catalog.rs::list` |

## Durable runtime state journal

This document describes the per-session durable state journal implemented in the current tree. It leaves the conversation originals (the session log) alone and records separately only the **run state** that has to survive a restart.

### Why record it separately

The conversation log holds "what was said". Restart recovery needs a different question: did we tell the client the input was accepted, did execution actually start, did it finish, is the result unknown? Without that distinction, a restart resends the same input or silently loses input that already ran.

### Location

`crates/base/src/durable_runtime/` owns it: `mod.rs`, `record.rs` (records and state), `replay.rs` (scan, apply, cleanup), `store.rs` (files, fsync, compaction) and `tests.rs`.

### Record kinds

`RecordKind` has sixteen variants. Each transition is expressed by one `Payload`.

| Group | Kinds |
| --- | --- |
| Input | `input.accepted`, `input.started`, `input.committed`, `input.cancelled`, `input.returned`, `input.in_doubt` |
| Run | `run.started`, `run.stopped` |
| Control | `control.applying`, `control.terminal` |
| Monitor | `monitor.upsert`, `monitor.checkpoint`, `monitor.stopped` |
| Wake | `wake.pending`, `wake.acked` |
| Other | `checkpoint` |

`input.accepted` keeps the input body once, and later transitions reference only the id.

### Invariants

- One transition has one sequence number (seq). No state is published before its sync finishes.
- Two processes appending to the same session concurrently lose no sequence number and do not collide.
- `RuntimeState` is the same before and after compaction. No retained entry is lost.
- The canonical bytes of the conversation graph do not change because of this journal.
- Opening the journal flushes the parent of every directory level it creates (`store.rs::ensure_private_dir`). The first record flushes the journal directory to commit the `runtime.jsonl` entry, because a journal committed together with a directory whose own entry never reached the parent is lost after a power cut. An existing directory is not flushed, so appends do not add cost. The tests are in `durable_runtime/directory_tests.rs`.

### Limits

The `JournalLimits` defaults (`store.rs`).

| Constant | Value | Meaning |
| --- | --- | --- |
| `MAX_JOURNAL_BYTES` | 32 MiB | Journal file cap |
| `LIFECYCLE_RESERVE_BYTES` | 1 MiB | Reserve that admission cannot enter. Lifecycle rows still can |
| `COMPACT_AFTER_BYTES` | 4 MiB | Compact above this size |
| `COMPACT_AFTER_RECORDS` | 4,096 | Compact above this record count |
| `MAX_LIVE_INPUTS` | 1,000 | Cap on live inputs |
| `MAX_LIVE_INPUT_BYTES` | 16 MiB | Cap on the combined live input bodies |
| `MAX_INPUT_PAYLOAD_BYTES` | 1 MiB | Cap on one input body |
| `TERMINAL_RUN_CAP` | 256 | Finished run records retained |
| `TERMINAL_RECEIPT_CAP` | 1,024 | Finished control receipts retained |

### Recovery

`RuntimeState` holds six tables: inputs, input receipts, runs, controls, monitors and wakes. What the journal layer **expresses** and the **enforcement** that applies those tables to the queue are separate. Of the rules below, the first three are checked by the journal layer (`crates/base/src/durable_runtime/`) and the queue application is owned by `crates/app-core`, verified by the `durable_input` tests.

- An input left at `accepted` is restored as **pending**. Restoring the same input id twice puts only one copy into the queue.
- An input at `started` with no terminal row is sealed as **in_doubt**. It is never injected again automatically. Work whose execution is unknown is recorded as neither success nor failure.
- A run left at `started` is `interrupted_unknown`. It is neither a completion nor a panic (`StopCause::InterruptedUnknown` in `crates/agent-runtime/src/turn_state.rs`).
- `input.in_doubt` can be sealed only from the `started` state. Arriving from any other phase is rejected as a transition violation.
- A torn tail is reported separately (`RecoveryReport`). A file with no records at all and a corrupted file are different events.

### Sessions that had no journal

The existing `<session>.json` queue is imported once. The deterministic input id mapping is recorded, and a second start does not import again. After a normal transition the legacy JSON is mirrored with fsync. A mirror failure does not roll back admission into the new journal. It surfaces as a recovery warning and as a reason for `export-legacy` to refuse.

### What this journal does not guarantee

- Exactly-once external side effects. It does not stop a tool from running twice.
- Monitor sharing across devices.
- Production-scale throughput.
- The speed of provider calls.

### Failure injection

The per-store-instance fsync failure hook (`JournalIoPoint`) is enabled only under `test-support`. So that a test never mistakes a fake fsync success for durability evidence, the tests record the OS call order together with the result after a restart.