# Harness API

Language: **English** | [한국어](HARNESS_API.ko.md)

This document covers the credential setup and the request and event surfaces of the harness API that desktop and SDK clients use over a Unix socket.

## Credential setup

This build has no interactive login flow. The SDK exposes two things about credentials.

- `HoronAgentClient::set_api_key(provider, key)` (TypeScript `setApiKey`) writes the key into horon-agent's owner-only credential store (a `SetApiKey` request with a `credential_updated` response) and re-reads the provider credentials. The key never appears in a response or a log.
- `notify_auth_changed(provider)` (TypeScript `notifyAuthChanged`) is the notice to re-read the credential files after the user logged in to OAuth with another tool. It sends no credentials itself, and the bridge advertises this through `auth_changed_notification`.

The SDK neither issues nor refreshes OAuth tokens. The launch option `inherit_logins` (on by default) links the user's existing CLI credential files into the instance, and model requests use those files as they are (`crates/agent-sdk/src/launch.rs`). Model calls go only through the OpenAI-compatible profile in `[providers.<name>]`, so this build has no model runtime that authenticates with OAuth. The `openai-oauth:` prefix still exists, but with that runtime gone it fails before the call (`docs/provider/MODEL.md`).

## Durable runtime API (protocol 1.14)

The major version stays at `1` and the minor version moved from 13 to 14. Every surface below is additive. The byte representation of existing requests and the shape of their responses are unchanged, and a client that sends none of the new requests behaves exactly as before.

The bridge advertises four capabilities. A client checks them before sending anything.

| Capability | Surface it gates |
| --- | --- |
| `durable_runtime_v1` | `get_runtime_state`, `apply_control`, `get_control_receipt` |
| `monitor_v1` | None. Monitors start and stop through existing tool calls, so this is a marker telling a client how to read results |
| `snapshot_marker_v1` | `get_snapshot` and the `snapshot_marker` event |
| `ask_receipts_v1` | The optional `answer_id` of `ask_response` |

A request sent without the capability is rejected with a structured `unsupported` error that names the missing capability. It is not treated as success, and it is not a version error. Both SDKs provide methods with the same names (`get_runtime_state`/`getRuntimeState`, `apply_control`/`applyControl`, `get_control_receipt`/`getControlReceipt`, `get_snapshot`/`getSnapshot`, `respond_to_ask_with_id`/`respondToAskWithId`) and reject the request before writing it to the socket when the capability is absent. That is the point where an old server can no longer receive an unknown request and mishandle it.

### State summary

`get_runtime_state` returns a **summary** of runs, queue, receipts and monitors from the session's durable journal. Accepted input bodies, command arguments and command environment variables never appear here. A status panel needs counts, phases and causes, not the sentences sitting in the queue.

| Field | Meaning |
| --- | --- |
| `journal` | Whether the journal exists, its generation, last seq, bytes, record count and whether it is read-only |
| `runs` | Runs that have not finished: `run_id`, `started_at`, optional `input_id` |
| `queue` | Live input count, inputs whose result is not settled, held bytes |
| `receipts` | Completed and cancelled input receipts, newest first: `input_id`, `phase`, `seq` |
| `monitors` | Monitor id, kind, whether it stopped, a summary sentence of the last checkpoint |
| `pending_wakes` | Number of wakes not yet delivered |
| `pending_controls` | Number of controls that have not finished |
| `last_stop` | Cause and time the last run ended |

### Control and receipts

`apply_control` applies a control action under a caller-chosen `operation_id`. Sending the same id again returns the recorded receipt and does not apply the action twice. `get_control_receipt` reads that record. `outcome` is one of `applying`, `terminal`, `replay`, `conflict`, `unknown` and `indeterminate`. `indeterminate` means the action started but its terminal record could not be written, so the result is unknowable, and it is never retried automatically.

### Snapshots and markers

`get_snapshot` returns state plus a bounded history tail at one consistent point in time. Just before the response a `snapshot_marker` event arrives on the same connection first, and both frames carry the same `marker_id`. Every live event that arrives afterwards is newer than that snapshot. The marker is a stream event without `reply_to`, so a client classifies the boundary by that rule instead of guessing from arrival order. `generation` changes on every restart and promises nothing about replaying past `seq` values.

`get_snapshot` takes `tail` (default 20, maximum 100) and `max_bytes` (default 262,144, maximum 1 MiB), and `truncated` becomes true when the budget cuts the tail.

### Answer receipts

`ask_response` gained an optional `answer_id`. Sending the same `answer_id` with the same answer for the same `ask_id` counts as one answer, so a retry after a lost response does not resolve the question twice. A different `answer_id` for the same question is a conflict. The existing path that sends no `answer_id` works as before and needs no capability negotiation.

### New events

Four events were added: `runtime_state`, `control_receipt`, `snapshot_marker` and `snapshot`. All are gated by a capability; the server does not send them to a connection that did not negotiate it, and the SDK discards them after receiving them. Byte-level behaviour of existing connections is unchanged.

### Not yet wired up

The protocol shape and SDK methods of `apply_control`/`get_control_receipt` and `get_snapshot` are settled, but the storage and observation implementation behind them belongs to separate work. Until that work lands, the bridge answers these requests with `unsupported`. From the client's side that is the same "this server does not offer it yet" answer, so no branching changes.

## Session event store and replication (protocol 1.12)

The harness API keeps major version 1 and moved from minor version 11 to 12. Everything below is additive. A client that sends only the requests it already sent keeps working, and reading the conversation history still needs no new request.

A client negotiates the handshake and checks the capabilities it needs before sending anything. Alongside the existing set, the bridge advertises the following.

| Capability | What it controls |
| --- | --- |
| `session_event_store_v1` | `list_session_heads`, `select_session_head` |
| `session_replication_v1` | `export_session_events`, `import_session_events`, `export_session_blob`, `import_session_blob`, `get_session_blob_status`, and the whole-record blob pair |
| `session_history_cursor_v1` | `get_history_page` |
| `structured_compaction_v1` | Reading the structured summary fields of a `compacted` record. No method is gated by this capability |

A request sent without the capability is answered `unsupported` with the missing capability name. That is not a version error. Either update and restart both the daemon and the bridge, or stop using that method against this build.

### Heads

A session can hold several heads after a rewind or a branched import. The selected head is **host-local**. It is not replicated and is not part of the session's permanent identity. `list_session_heads` reports the heads and the current selection, and `selected_head` is absent when the several heads need an explicit choice. Until one is selected, execution is refused, and `select_session_head` answers `busy` when another process holds the write lock. Because the choice changes what the user's next turn continues from, never pick one silently; show the choice to the user.

### History

`get_history` is unchanged and still returns the whole visible conversation history. `get_history_page` is the bounded read a UI wants on long sessions. It takes `session_id`, an optional `head_event_id` (defaulting to the selected head), the opaque continuation value `before_event_id`, and `limit` (default 50, maximum 200). The response is a `SessionHistoryPage` with a fixed `head`, a list of page messages from oldest to newest, and a `next_cursor` to pass back as `before_event_id`. Pin `head_event_id` while the user scrolls. A cursor with a pinned head points at the same history even after the selection changes.

### Replication

`export_session_events` returns the origin for a pinned head, one bounded page at a time with parents first. `import_session_events` validates the whole page before committing and answers with `accepted_ids`, `heads`, `retry_cursor` and `conflict_heads`. Original blobs move separately in 1 MiB chunks and are verified by SHA-256 and size before publication. An interrupted transfer continues from `get_session_blob_status`.

### Markdown transcript

`export_session_markdown` takes `session_id` and replies with a `session_markdown` event carrying the whole readable transcript in `markdown`. Each turn is a `### User` or `### Assistant` heading followed by its content: text verbatim, a tool call as a fenced JSON block, a tool result as a fenced block, and an image or audio block as a short placeholder. Secrets are redacted with the same pattern table the disk path uses, so a token that never reached the event log cannot appear in the transcript. A tool result longer than 4 KiB is cut at a UTF-8 boundary and replaced with a `… (truncated, N bytes total)` marker.

The reply is not part of the replication protocol and needs no `protocol_version`. It requires `session_replication_v1`, the capability that already gates the other stored-session reads. An unknown session answers `unknown_session`, the same error the other reads use. `horon-agent export <session-id> [--out <path>]` writes identical bytes, because both call one renderer.

Two rules a client must not break:

- **Never re-serialize `canonical_json`.** An event's canonical JSON travels as a string. `u64` values above 2^53, `1.0` and `-0.0` do not survive a round trip through JavaScript numbers. Pass the string through untouched.
- **Treat `conflict_heads` as data.** Branched heads are all preserved. No timestamp decides a winner, and an import must not discard a branch the user did not choose.

Re-sending an import is safe. The same id with the same hash does nothing.

### Errors

The five stable `ErrorCode` values are unchanged, so older clients keep decoding them. A finer cause comes in an optional `details` object. It carries `store_code` (for example `hash_conflict`, `missing_dependency`, `conflict_heads`, `busy`, `invalid_cursor`, `record_too_large`), `retryable`, `accepted_ids`, `retry_cursor` and `conflict_heads`. Ignore `details` when you do not use it. Branch on `retryable` before retrying, and never retry a `hash_conflict` unchanged.

### Status

`runtime_info` builds liveness from the current host runtime. A PID recorded in stale session metadata is historical side information, not a live process, and no client should display it as running.

The full storage format, limits and legacy migration rules are in `docs/protocol/SESSION_STORE.md`.

## Harness API request and event surfaces: usage_report, search_sessions, list_artifacts, durable_runtime

This document records the request surfaces added to the harness API. The first part covers the three requests of protocol 1.13 and their response events, and the last part covers the four durable runtime surfaces of 1.14. Field names and types are copied from the definitions in `crates/harness-api/src/requests.rs` and `crates/harness-api/src/events.rs`, and behaviour and limits are the values confirmed in the implementation under `crates/harness-api-server/src/translate/`.

All three requests are additive. A client that sends none of them keeps working, and the bridge advertises `usage_report`, `session_search` and `artifacts` in the `capabilities` of `hello_ok` (`BRIDGE_CAPABILITIES` in `crates/harness-api-server/src/lib.rs`).

### Common rules

- One frame is one JSON object on one line (NDJSON). A request names its kind in `req` and carries an `id`, and a response echoes that `id` in `reply_to`.
- Request bodies serialize with `#[serde(tag = "req", rename_all = "snake_case")]`, so field names are snake_case.
- Unknown fields are ignored, and unknown event kinds are skipped.
- A failure arrives as `{"ev":"error","code":...,"message":...}`. The `ErrorCode` values these surfaces use are `invalid_request`, `unknown_session` and `internal`.
- A request that names a session takes `session_id`. A missing session answers `unknown_session` rather than an empty result, because "this session wrote nothing" and "no such session exists" are different facts.

### usage_report

Returns token usage, the context window and per-model totals for the attached session and for every stored session. The limits field (`limits`) remains but this build has no source for it, so it is always an empty array. The capability name is `usage_report`.

#### Request

```json
{ "v": 1, "id": 7, "req": "usage_report", "session_id": "session_..." }
```

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `session_id` | string | No | Narrows the report to this session. Omitted, it reports every stored session together with the attached session |

An unknown `session_id` is `unknown_session`.

#### Response: `usage_report`

```json
{
  "v": 1,
  "reply_to": 7,
  "ev": "usage_report",
  "session_id": "session_...",
  "sessions": [],
  "totals": {},
  "models": [],
  "context_window": 200000,
  "limits": [],
  "pricing": "unknown",
  "estimated_cost": null
}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `session_id` | string, may be absent | The session the report was narrowed to. Absent in a full report |
| `sessions` | `UsageSession[]` | One row per session in scope, ordered by session id |
| `totals` | `UsageTotals` | The sum across `sessions` |
| `models` | `UsageModel[]` | Per-model totals, ordered by model id |
| `context_window` | number, may be absent | Context window (tokens) of the attached session's active route. Absent means "unknown", not another model's default window |
| `limits` | `UsageLimitInfo[]` | The limit list. This build has no account source that reports limits, so it is always an empty array |
| `pricing` | string | Always `"unknown"`. This repository has no price table |
| `estimated_cost` | number, may be absent | Always `null`. With no price table there is no cost estimate either |

`UsageSession`:

| Field | Type | Meaning |
| --- | --- | --- |
| `session_id` | string | Session id |
| `model` | string, may be absent | The model the session metadata recorded |
| `input` | number, may be absent | Sum of input tokens. Absent when no request recorded any |
| `output` | number, may be absent | Sum of output tokens |
| `cache_read_input` | number, may be absent | Sum of cache-read input tokens |
| `cache_creation_input` | number, may be absent | Sum of cache-write input tokens |
| `requests` | number | Provider requests that recorded usage. Not the number of user turns; one turn issues several requests and each tool round trip counts separately |

`UsageTotals` holds `input`, `output`, `cache_read_input` and `cache_creation_input` (each may be absent), plus `requests` and `sessions` (the number of sessions the sums cover, including sessions without usage).

`UsageModel` holds `model` (`unknown` for a session whose metadata named no model), the same four token sums, `requests` and `sessions`.

`UsageLimitInfo` holds `name`, `used`, `limit`, `unit` (for example `usd`) and `resets_at` (RFC 3339). `used`, `limit` and `resets_at` are present only when the daemon reported them.

#### Behaviour

The report merges three sources in order of authority: the attached session's daemon counters, the `token_usage` records in the stored session source, and the last live `token_usage` event. A metric no request recorded is `null`, not `0`, because a provider not reporting cache tokens and a provider reporting zero are different facts.

This build has no account subscription, so the daemon reports no limits and `limits` is always an empty array. An empty array means "this build has no limit source", not "this account has no limits".

### search_sessions

Searches the transcripts of every stored session, not just the attached one. The capability name is `session_search`. It is needed because filtering to sessions with a session switcher cannot answer "where was that conversation".

#### Request

```json
{ "v": 1, "id": 8, "req": "search_sessions", "query": "zanzibar", "limit": 10 }
```

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `query` | string | Yes | Search term. Whitespace only is `invalid_request` |
| `limit` | number | No | Maximum hits. Default 20, capped at 50 |

`query` is not interpreted as a path, a glob or a SQL fragment. It is searched as the literal string. A `limit` of 0 returns an empty result.

#### Response: `session_search_results`

```json
{
  "v": 1,
  "reply_to": 8,
  "ev": "session_search_results",
  "query": "zanzibar",
  "results": []
}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `query` | string | Echoes the request's search term, so a client that sent several searches can tell which answer is on screen |
| `results` | `SessionSearchHit[]` | The hits |

`SessionSearchHit`:

| Field | Type | Meaning |
| --- | --- | --- |
| `session_id` | string | Stored session the hit belongs to |
| `title` | string, may be absent | The display title of that session |
| `role` | string | `user`, `assistant`, or `metadata` for a title or session-level hit |
| `preview` | string | Plain-text excerpt cut around the match. Cut spans are marked with `...` |
| `at` | string | RFC 3339 time of the matching message. For a metadata hit, the session time |

#### Behaviour

It uses the same engine, the same index and the same scoring as the agent's own session search, so a search from the agent and a search from a client return the same results. With no storage directory the result is empty, and an engine failure is `internal`.

### list_artifacts

Lists the files a conversation produced: images the conversation carried in, and HTML and image files written into the working directory after the session started. The capability name is `artifacts`.

#### Request

```json
{ "v": 1, "id": 9, "req": "list_artifacts", "session_id": "session_..." }
```

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `session_id` | string | Yes | Session to list. Missing, or with no stored record, it is `unknown_session` |

#### Response: `artifacts`

```json
{
  "v": 1,
  "reply_to": 9,
  "ev": "artifacts",
  "session_id": "session_...",
  "artifacts": []
}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `session_id` | string | The requested session |
| `artifacts` | `ArtifactEntry[]` | Entries, newest first |

`ArtifactEntry`:

| Field | Type | Meaning |
| --- | --- | --- |
| `path` | string | Path relative to the session working directory, with `/` separators |
| `kind` | string | `html` or `image` |
| `bytes` | number | File size at response time |
| `mtime_ms` | number | Last modification time in epoch milliseconds |

#### Behaviour

Only metadata is returned. The client reads the bytes separately with `read_file`, so even a 40 MB HTML report costs one `stat` per candidate.

The list is collected from two sources: paths named by the transcript and a scan of the working directory. Both reject paths that leave the session root and reject symbolic links, so a transcript naming `../../.ssh/id_rsa` never puts it on the list. When the session start time is not recorded, the session file's modification time stands in for it, and older files are excluded. The result is sorted by `mtime_ms` descending, by path ascending for equal times, and cut at 200 entries.

### durable_runtime_v1 · monitor_v1 · snapshot_marker_v1 · ask_receipts_v1

The four surfaces added by protocol minor 14. All four are **additive**, so a client that did not negotiate the capability neither sends nor receives these requests. Existing clients behave as before.

| Capability | Requests and events it opens | What it guarantees |
| --- | --- | --- |
| `durable_runtime_v1` | `get_runtime_state`, `apply_control`, `get_control_receipt` | Run state that survives a restart, and control receipts where a resend of the same operation id is applied only once |
| `monitor_v1` | File and command monitors of the `monitor` tool | Survives a restart as `needs_rearm` without re-running the command |
| `snapshot_marker_v1` | `get_snapshot` | `SnapshotMarker` arrives 1:1 immediately before `Snapshot`, and the `seq` of live events continues after it |
| `ask_receipts_v1` | `answer_id` of `ask_response` | The same `question_id`+`answer_id`+`digest` replays the stored result, and a different digest is a conflict |

#### Boundary rules

- A `SnapshotMarker` and a `Snapshot` never go to different connections. Only the connection that received the marker receives the snapshot.
- Live events after a snapshot carry a `seq` issued by the observation owner. A client can prove "everything after N is newer" itself instead of guessing order.
- `apply_control` has the server build the fingerprint. The same id with the same fingerprint replays the stored result, the same id with a different fingerprint is `conflict`, and a concurrent `applying` is `pending` without a second execution. Anything still applying at restart is sealed as `indeterminate` and never executed again.
- An operation id outside the retention window is `unknown`. The SDK never retries automatically in that case.
- The only supported actions are `abort_run(expected_run_id)` and `cancel_inputs(input_ids)`. Model changes, files, shells and arbitrary commands are not on this surface.
- A request against a capability the bridge does not advertise is unreachable. That is why all four strings are in `BRIDGE_CAPABILITIES`.

### Sources

- Request definitions: `GetRuntimeState`, `ApplyControl`, `GetControlReceipt`, `GetSnapshot` and `AskResponse.answer_id` in `crates/harness-api/src/requests.rs`
- Event definitions: `SnapshotMarker`, `Snapshot`, `RuntimeStateSummary` and `ControlReceipt` in `crates/harness-api/src/events.rs`
- Implementation: `crates/app-core/src/server/control_receipts.rs`, `crates/app-core/src/server/session_observation.rs`, `crates/base/src/durable_runtime/`
- Capability constants: `CAPABILITY_DURABLE_RUNTIME_V1`, `CAPABILITY_MONITOR_V1`, `CAPABILITY_SNAPSHOT_MARKER_V1` and `CAPABILITY_ASK_RECEIPTS_V1` in `crates/harness-api/src/lib.rs`
- Protocol version: `API_VERSION_MAJOR = 1`, `API_VERSION_MINOR = 15`