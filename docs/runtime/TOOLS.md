# Tools

Language: **English** | [한국어](TOOLS.ko.md)

This covers tool loading tiers, effects and approval tiers, edit safety, the ancestor `AGENTS.md` attached to `read`, skill reads (`skill://`), memory reads (`memory://`) and memory updates, where MCP configuration comes from, condensing large tool output and `read` deduplication, aging of old results, and the one-shot auxiliary call.

## Tool discovery and deferred loading

The `full` profile exposes every built-in tool but keeps the schema of non-core tools out of the request's cached prefix. The default `core` profile also carries schemas for only seven of the fifteen core tools; the rest are named in a system-prompt index. Three `LoadMode` tiers decide whether each tool is sent eagerly, deferred, or dropped, and `toolbox` is the window onto tools that are deferred or index-only. This document describes the behaviour of the current tree.

### Three tiers

`LoadMode` is defined in `crates/base/src/config.rs:825`.

| Value | Meaning |
| --- | --- |
| `essential` | Sent eagerly and callable with no discovery step. This is the default. |
| `discoverable` | Sent as a deferred definition. The schema reaches the model once a tool reference names it. Providers without native deferred loading drop the definition instead, and in a `core` session the index names it. |
| `off` | Not sent at all. |

`discoverable` reuses the provider-native deferred loading that already carries MCP tools. The definition travels in the request's tool catalog but stays outside the cached system-prompt prefix, so it costs no prefix bytes and adding or removing a tool never invalidates the prompt cache.

A per-tool tier is overridden with the `[tools.load_mode]` table. Names are normalized, and a name that does not exist in this build has no effect.

```toml
[tools.load_mode]
batch = "off"
memory = "essential"
```

### Default layout

- The seven tools in `ESSENTIAL_TOOLS` (`read`, `write`, `edit`, `apply_patch`, `bash`, `ls`, `agentgrep`) are `essential` in every profile (`config.rs:803` `ESSENTIAL_TOOLS`). Almost every coding turn touches them, so their schemas stay in the cached prefix.
- `CORE_TOOLS` still holds fifteen entries and is the allow-list of the `core` profile (`config.rs:779` `CORE_TOOLS`). The list sets reachability; whether tools are sent eagerly depends on whether the session is a listing session.
- A `core` session becomes a listing session under three conditions: the profile is the untouched default `core`, no allow-list is given explicitly, and `toolbox` is reachable (`crates/app-core/src/agent.rs:657`, `config.rs:1237` `ToolConfig::toolbox_reachable`). That session sends only the seven tools and `toolbox` with schemas, and the other eight core tools (`replace`, `webfetch`, `websearch`, `skill_manage`, `mcp`, `monitor`, `undo`, `ask`) become `discoverable` and are named in the index (`config.rs:889` `ToolLoadTiers::tier`).
- Sessions that are not listing sessions send every core tool as `essential`. This covers `acp`, `minimal`/`lite`, `none`, an explicit `enabled` list, an SDK `enabled` list, and an allow-list passed to `new_with_session`. Their tool lists and tiers are unchanged and they have no index. When an SDK `enabled` list is attached to a listing `core` session, the client has named tools exactly, so listing mode turns off and every named tool is sent eagerly (`crates/app-core/src/agent/turn_execution.rs:800` `apply_builtin_load_tiers`).
- `mcp_search` and `mcp_call` are sent eagerly by default (`config.rs:861` `ALWAYS_EAGER_TOOLS`). They are the path that discovers MCP schemas, so the provider-native deferred route still sends them eagerly. On providers without native deferred loading, a listing `core` session that has no active MCP server configured when it locks its first tool list does not send the two tools eagerly; they go into the index and are called through `toolbox`. With at least one server they are sent eagerly as before. This decision also never changes mid-session. A session pinned to a native provider has neither tool in its frozen index, so switching to a non-native provider later leaves both tools eager (so as not to advertise a name the index does not carry). The closing sentence of `mcp list`, of connection results, and of new-MCP-tool announcements that trims the rest past 32 items points at `toolbox` only in sessions where the two tools are in the index, and otherwise announces a direct call (`turn_execution.rs:648` `freeze_mcp_surface`, `turn_execution.rs:681` `mcp_surface_listed`). With `mcp_tools = "eager"` per-server definitions are shipped and this rule does not apply.
- `toolbox` is always `essential`. An explicit `discoverable` entry is promoted to `essential`, and an `off` entry removes the tool (`config.rs:889` `ToolLoadTiers::tier`).
- Deferred defaults turn on only when the explicit `full` profile has no allow-list. `acp`, `minimal`/`lite`, `none`, an explicit `enabled` list, and an unset profile send the tools that pass eagerly (`config.rs:1210` `ToolConfig::load_tiers`).
- A session fixes its tiers once at start. Reloading configuration never moves a tool between the eager and deferred sets mid-session.

`irc` is a tool outside the core set. With `[tools] irc = true` it joins a non-empty allow-list such as `core`, `acp`, `minimal`, or an explicit `enabled` list, and a `full` session without an allow-list sees it as it always did. With the switch off the registration stays but execution is refused, so it is unusable even in a `full` session. An empty allow-list (`none`) stays empty even with the switch on. Mailbox behaviour and configuration are in [`IRC.md`](IRC.md).

#### Forcing a tool back to eager

- Writing `[tools.load_mode] ask = "essential"` ships that tool with its schema even in a listing session. An explicit entry outranks the default layout.
- `monitor = "off"` removes that tool from both the request and the index.
- `toolbox = "off"`, or adding `toolbox` to `[tools] disabled`, turns listing mode off and puts all fifteen core tools back on the eager surface. The point is to avoid creating a tool with no way to reach it.

### The listing index

The static part of a listing `core` session's system prompt gains an `# Additional tools` section. One short guidance sentence is followed by `- <name>: <one-line description>` rows in name order (`crates/base/src/prompt.rs:483` `build_additional_tools_section`). The guidance sentence says only how to call a listed tool through the `run` action of `toolbox`. A row's description is the first sentence of the tool description, capped at 60 characters, so the model fetches parameters separately with `toolbox describe`.

- The same index is used regardless of provider. Providers with native deferred loading receive this index alongside the deferred definitions. Providers without it drop the deferred definitions, so this index is the only thing that names them.
- Rows are filtered by the same rule the session sees from `toolbox list`. `mcp__*` proxies and `toolbox` itself are left out, and a tool the session policy would have `toolbox` refuse (outside the allow-list, disabled, or turned off by the SDK) is not listed (`turn_execution.rs:897` `build_listed_tools_index`, `crates/app-core/src/tool/toolbox.rs:232` `session_can_reach`).
- The index freezes once, when the first tool list is locked. A warm-up request before MCP servers register does not freeze it; it is rebuilt once at lock time and then pinned. Unlocking tools, compaction, MCP connections, provider switches, and configuration reloads afterwards do not change it (`turn_execution.rs:834` `ensure_listed_tools_index`).
- Only a session restore resets the index and rebuilds it for the new session. Clearing the conversation with `/clear` is the same session, so the index survives (`crates/app-core/src/agent.rs:1192` `reset_listed_tools_index`).
- When the SDK replaces the system prompt wholesale, the index is appended after it (`crates/app-core/src/agent/prompting.rs:175`). An overridden session still keeps the names of listed tools.
- A row uses only the first sentence of the tool description and cuts at 60 characters with an ellipsis when that sentence is longer. Index rows and `toolbox list` rows share one helper, so both lists show the same wording (`one_line_description` in `crates/base/src/prompt.rs`). A model that picks a tool from the index gets its schema with `toolbox describe` and runs it with `toolbox run`.
- The `todo` guidance sentence is appended only for sessions whose allow-list contains `todo` (`acp` and the like), and never for a listing `core` session, which has no `todo` (`prompting.rs:233` `append_todo_hint`). When both are appended, the guidance sentence comes before the index.

### toolbox

`toolbox` is registered under the `full` and `core` profiles (`crates/app-core/src/tool/toolbox.rs:55` `enabled`). Other profiles, including `acp`, keep a closed allow-list, so `toolbox` is absent. In a `full` session with an allow-list, that list narrows reach again. In a session that is neither listing nor `full`, such as a swarm worker, the allow-list leaves `toolbox` out (`without_unreachable_dispatcher` in `agent.rs`).

There are three actions.

- `list`: shows the names and one-line descriptions of tools deferred or indexed for this session. It leaves out itself and the MCP surface (`mcp__*`, plus the eagerly sent `mcp_search`/`mcp_call`), and lists only tools reachable in this session, in name order (`toolbox.rs:282` `list`).
- `describe`: normalizes the name and returns the description and input schema as text (`toolbox.rs:318` `describe`). This is how the model learns parameters on providers whose tool list does not carry the schema.
- `run`: executes the target tool through `Registry::execute` (`toolbox.rs:336` `run`). `toolbox` itself cannot be run, and `args` must be a JSON object.

#### Policy applies once, to the target tool

`toolbox run` gets the same policy as a direct call, but it applies once, to the target tool rather than to the outer call. Plan mode, approval requests, and the `pre_tool`/`post_tool` hooks run inside the inner `Registry::execute` with the target tool's name and input. The outer `toolbox` call skips those steps, so approval is never asked twice and a hook never receives the name `toolbox` (`crates/app-core/src/tool/mod.rs:2144`). The allow-list and disabled-list checks remain on the outside to decide whether `toolbox` itself is reachable.

Plan mode's test for a read-only call also follows the target. `list` and `describe` are read-only, `run` uses the effects the target tool declares, and an unknown target or a malformed action counts as a write barrier (`toolbox.rs:132` `effects_for_input`). So in plan mode `toolbox run write` is refused like a direct `write`, and `toolbox run ls` passes.

Tools at the `off` tier, tools outside the session allow-list, and tools on the disabled list do not appear in `list`, and `describe` and `run` refuse them.

#### References and the next turn

`describe` and `run` put `tool_references` into the result metadata. Transcript conversion turns it into a single `ContentBlock::ToolReference`. Providers with native deferred loading use that reference to load the deferred schema on the next turn, which makes a direct call possible. Providers without it get no such promotion; the model simply keeps calling through `toolbox run`.

#### How clients see it

In a listing `core` daemon, listed tools such as `replace`, `webfetch`, and `ask` appear as `toolbox` calls. ACP editors pick a kind from the tool name, and `toolbox` belongs to no category, so it renders as `other` (`src/cli/acp/util.rs:286` `tool_kind`). Only an `acp` profile session can expect the editor to receive `ask` as a direct call and render it.

#### Hosted search

Model requests go straight to an OpenAI-compatible `/chat/completions` and carry no hosted tools in the body, so nothing attaches OpenAI's hosted `web_search`. `websearch` works only as a local tool.

### Why core stays closed

`core` is an allow-list. It pins the default headless surface to the fifteen core tools plus `toolbox`, keeps the other modules in the tree, and leaves them unexposed. The allow-list is as closed as it was; only the delivery changed. Seven tools come with schemas, eight are reached through the index and `toolbox`. The most recent addition, `monitor`, watches files and commands across restarts. The choice preserves three things.

- The default surface's size does not change, so the request prefix and the prompt cache stay stable.
- The default permission envelope, writes and shell included, stays predictable.
- New tools do not quietly widen the headless default.

Widening it takes an explicit `--tool-profile full`, `HORON_AGENT_TOOL_PROFILE=full`, or `[tools] profile = "full"`.

### Why there is no promote

`toolbox` only loads a schema by reference. It does not edit the session's tool definitions, does not write to the transcript, and does not promote a deferred tool into the eager set. A promotion would change the tool list mid-session, invalidate the prompt cache, and make the `full` profile's deferred layout pointless. Hence the three actions `list`, `describe`, and `run`, and no `promote`.

## Tool effects, plan mode, and approval tiers

This explains the bit mask that declares what one tool touches in a round, the rule that reads that declaration to decide concurrent execution, the plan-mode state machine, and when approval tiers step in. All three devices share one declaration. A tool's declared effects justify concurrent execution, justify a plan-mode refusal, and drive the approval tier.

### Effect bits

`ToolEffects` is a `u8` wrapper over five bits (`crates/tool-core/src/lib.rs:166`).

| Bit | Value | Meaning |
| --- | --- | --- |
| `READ` | `1 << 0` | Reads a file or state. |
| `WRITE` | `1 << 1` | Overwrites a file. |
| `APPEND` | `1 << 2` | Appends to existing content. |
| `NETWORK` | `1 << 3` | Sends a request outward. |
| `PROCESS` | `1 << 4` | Spawns a process. |

`BARRIER` is `WRITE | APPEND | PROCESS` (`crates/tool-core/src/lib.rs:176`). `NETWORK` is deliberately excluded. Two lookups do not interfere with each other, and a paid call count is a budget question rather than a safety one. A call declaring only reads and network therefore overlaps with others in the same round (`parallel_safe`, `crates/tool-core/src/lib.rs:223`).

### Concurrency classes

`ConcurrencyClass` has two values, `Ordered` and `Concurrent` (`crates/tool-core/src/lib.rs:151`). The class is derived from effects and never declared separately (`crates/tool-core/src/lib.rs:270`).

The `Tool` trait's default `effects` is the write barrier (`crates/tool-core/src/lib.rs:255`). A tool that declares nothing is assumed to be unable to overlap. The two mistakes cost differently.

| Wrong declaration | Result |
| --- | --- |
| `Concurrent` when the tool actually writes | Two overlapping writes to one file, or two overlapping shells in one work tree, overwrite each other's changes. The last writer silently wins. User data disappears. |
| `Ordered` when the tool only reads | Calls still run. They just run alone, which is slower. |

Only read-only, non-mutating tools declare `Concurrent`.

`irc` writes no files but does not override `effects()`, so it declares the same default write barrier. Multiple `irc` calls in one round therefore run one at a time in order, which keeps one sender's message order intact. Its concurrency class is not `Concurrent` either.

### Effects that depend on input

When a tool's footprint depends on its arguments, it overrides `effects_for_input` (`crates/tool-core/src/lib.rs:260`). The default implementation returns `effects()` unchanged.

The registry asks for effects with the call's input, not with the name alone.

- `declared_effects` calls `effects_for_input(input)` (`crates/app-core/src/tool/mod.rs:1036`).
- `declared_effects_sync` reads only `effects()` (`crates/app-core/src/tool/mod.rs:1044`). It runs before a round is dispatched and cannot await the map, so it returns `None` while the lock is held.
- Both treat `None` as "cannot classify". Callers read that value as the write barrier. A read-only gate never opens because of a tool whose footprint it cannot see.

### Two gates

The plan-mode check exists in two places because the two paths call tools differently.

| Path | Location | What it does |
| --- | --- | --- |
| Registry | `Registry::execute` (`crates/app-core/src/tool/mod.rs:1450`) | Resolves the name, checks session policy, passes the plan-mode gate (`:1513`), takes the tool, then passes the approval gate (`:1561`) and the `pre_tool` hook (`:1582`) in order. |
| Agent | `Agent::validate_tool_allowed` (`crates/app-core/src/agent/turn_execution.rs:1066`) | Checks one name before the round dispatches any call. The plan-mode gate is at `:1081`. |

The agent path puts the plan-mode check at the top of the function because of the SDK branch below it. SDK-enabled tools and callback tools exit straight to `Ok` from the allow-list check. A gate after that branch would let through exactly the calls whose footprint cannot be seen. So the plan-mode check runs before the branch, and the registry path uses the same default for the same reason.

Both paths log a `TOOL_LIFECYCLE` warning on refusal and record `block_reason` as `plan_mode` or `approval` (`crates/app-core/src/tool/mod.rs:1517`, `:1573`).

### The plan-mode state machine

`PlanState` has four values (`crates/app-core/src/plan_mode.rs:16`).

| State | Stored name | Read-only? | Meaning |
| --- | --- | --- | --- |
| `Off` | `off` | No | Plan mode is unused. Every call runs as usual. |
| `Planning` | `planning` | Yes | The model is drafting a plan. |
| `PendingApproval` | `pending_approval` | Yes | A plan was submitted and the user has not decided. |
| `Approved` | `approved` | No | The plan is approved, so effects are allowed again. |

`is_read_only` decides read-only status (`crates/app-core/src/plan_mode.rs:50`). The gate decides with `allows_effects`, and in a read-only state it lets through only `parallel_safe` calls (`crates/app-core/src/plan_mode.rs:164`). A tool that declares nothing falls back to the trait's write barrier and is refused.

State lives in a process-global map keyed by session id. It is not a field on the registry because `Registry::Clone` copies per call; a field would split state across clones.

- `state_for` returns `Off` for a session with no entry (`crates/app-core/src/plan_mode.rs:83`).
- `set_state` stores no `Off`; it removes the entry (`crates/app-core/src/plan_mode.rs:94`).
- `clear_state` removes the session entry (`crates/app-core/src/plan_mode.rs:105`).
- `transition` changes the state and records the transition in the session replay timeline (`crates/app-core/src/plan_mode.rs:185`).

`block_message` writes the refusal text (`crates/app-core/src/plan_mode.rs:173`). One paragraph says what was refused, that nothing changed, and the two ways out. It also notes that resending the same call gives the same result.

Plan mode is off by default. The configuration field defaults to `false` (`crates/base/src/config.rs:919`), and the default configuration file also sets `plan_mode = false` (`crates/base/src/config/default_file.rs:455`). A session that never touches configuration has no entry in the state map, so it always evaluates as `Off`.

### Approval tiers

`ApprovalMode` has three values, `off`, `write`, and `all` (`crates/config-types/src/lib.rs:511`). The default is `off` (`crates/base/src/config.rs:922`), which is the same non-asking behaviour as before.

`evaluate` decides (`crates/app-core/src/approval.rs:70`). Order matters.

1. A danger class in `dual_confirm_classes` requires approval regardless of mode. That is exactly why a class goes into this list: so no mode can auto-approve it.
2. If the mode is `off`, return auto-approval right here. This comes before the read-only branch. Reversing the order would make a mode that should change nothing start writing an audit line per call.
3. A read-only call is never asked in any mode (`crates/app-core/src/approval.rs:43`).
4. `write` mode auto-approves only file mutations. A process or network component requires approval.
5. `all` mode auto-approves every call that is not read-only.

Danger classes are not reclassified; the existing command risk classifier's verdict is used as is (`crates/app-core/src/approval.rs:52`). The only class with a name is `destructive-shell` (`crates/app-core/src/approval.rs:20`), because shell commands are the only thing whose footprint needs weighing.

`dual_confirm_classes` defaults to an empty array (`crates/base/src/config.rs:940`, `crates/base/src/config/default_file.rs:476`). Empty leaves every decision to the mode.

The gate sits before the `pre_tool` hook (`crates/app-core/src/tool/mod.rs:1561`). The order keeps an outside gate from observing a call that should have been asked about as if it had already been dispatched. The hard refusal of dangerous commands inside a shell remains. Approval tiers only decide whether the user is asked.

`irc` has the write bit and no process or network bit, so `write` mode auto-approves it like a file mutation, and `all` mode does not ask either. It is not a tool that raises an approval prompt.

### Audit records

Auto-approval writes an audit record. It creates no new file or queue and uses the existing log event `TOOL_APPROVAL` (`crates/app-core/src/approval.rs:139`). The fields are tool name, mode, reason, and call id (`crates/app-core/src/approval.rs:125`).

`off` mode writes no audit line, because logs must match the era before approval tiers existed. Read-only calls are not audited either. That is why `off` returns before the read-only branch.

## Edit safety

A tool that writes a file records before and after snapshots of that call's changes in the edit journal once the write succeeds. `undo` reverts the most recent unit, and `redo` re-applies the unit the last `undo` removed. `read`'s hashline anchors and `hashline_edit` tag line contents so an edit can name target lines. This document describes the scope and limits of that behaviour in the current tree.

### The journal

The journal belongs to the `horon-agent-edit-safety` crate (`crates/edit-safety/src/store.rs:162` `SnapshotStore`). File contents are addressed by SHA-256, and identical bytes are stored once (`crates/edit-safety/src/blob.rs:17` `BlobId`).

- One tool call is one mutation unit. A call touching several files holds per-file before/after in one unit.
- When one call touches the same path twice, only the first before and the last after are kept, as one net change (`crates/app-core/src/tool/undo.rs:95` `record`).
- Recording happens only after the write succeeds. A failed write leaves no unit.
- `commit` moves the unit into the undo history, and `abort` discards the unit and its bytes (`store.rs:228`, `store.rs:261`).
- History is one per session and lives only for the process lifetime. A restart starts empty.
- The budget is 64 MiB (`store.rs:25` `DEFAULT_BYTE_BUDGET`). When usage exceeds it at commit time, the oldest units are dropped whole. A unit just committed is never dropped by its own commit, so a unit larger than the budget survives while it is the newest (`store.rs:228` `commit`).
- An active unit's bytes count toward store usage, but budget pruning happens only at commit.

### undo

`undo` targets only the newest committed unit in the history (`crates/app-core/src/tool/undo.rs:271` `UndoTool`).

1. Compare each file's current bytes with the unit's after state.
2. If any differs, the call is refused by default. With `force: true` it overwrites and lists the overwritten paths in the result.
3. If the recorded state holds a file that could not be read, the whole unit is refused. A partial restore is never reported as success (`store.rs:277` `undo`).
4. On success, files go back to their before state. A file that did not exist is deleted; a state with bytes writes those bytes.

The journal never touches files itself. It returns the bytes to restore, and the calling `undo` tool writes them. That split lets crate-level tests run without a file system.

### redo

`redo` is registered only when `[tools] redo = true` (`crates/app-core/src/tool/undo.rs:133` `redo_enabled`). The default is `false`. Even registered, it must pass the profile's allow-list to reach the model, so in the `core` profile it also needs `full` or an explicit allow-list.

- It rewrites the after state of the unit the last `undo` moved.
- It first checks that each file's current bytes equal the unit's before state. On a mismatch it refuses; with `force: true` it overwrites and lists the paths.
- On success it commits that change as a new mutation unit, so it can be undone again.
- The redo queue lives in per-session module state and disappears when the process ends.

### hashline anchors

Passing `hashline: true` to `read` prefixes each line with `{line:>5}#{tag}:{body}` (`crates/app-core/src/tool/read.rs:153`). The tag is the first two bytes of the line's SHA-256 written as four lowercase hex digits (`crates/edit-safety/src/hashline.rs:27` `tag_for`). A 16-bit value is not a cryptographic commitment; it is a cheap marker for the line the model read.

- A line longer than 2000 bytes is cut and receives the `----` tag (`read.rs:13` `MAX_LINE_LEN`). That anchor names no content, so it cannot be used for an edit.
- `hashline_edit` is registered only when `[tools] hashline_edit = true`. The default is `false`, and it is outside the core set.
- Input is `file_path` plus an `ops` array. Each op has `start_anchor`, an optional `end_anchor`, and `new_text`.
- `ops` may be omitted when the call names a file-level effect: `delete: true` removes the file, and `move_to: "<path>"` writes the file's final content to that path and then removes the source. Both run after any line ops, in the same call and the same mutation unit, so one `undo` restores the old content and the old path.
- A move refuses an existing destination instead of overwriting it; `apply_patch` remains the way to replace a destination deliberately. `move_to` and `delete` cannot be combined.
- The path policy and the generated-file guard apply to the source and to the move destination, and `delete` also passes the protected-path check.
- Every anchor is checked against the current on-disk content first. One stale anchor refuses the whole call and writes nothing (`crates/app-core/src/tool/hashline_edit.rs:128` `resolve_ops`).
- Line numbers start at 1, spans move downward only, and overlapping spans are refused.
- An empty `new_text` deletes the span. A single trailing newline counts as a separator rather than content.
- The write path matches `edit`: per-path locking, one mutation unit, and the same file-touch events.

### Limits and failure modes

| Situation | Behaviour |
| --- | --- |
| Process restart | History is empty, so `undo` refuses and says there is nothing to revert. |
| Another process changed the file after recording | `undo` refuses. With `force: true` it overwrites and lists the paths. |
| A file could not be read when recorded | `undo` for that whole unit is refused. |
| Budget exceeded | The oldest units disappear and can no longer be reverted. |
| A unit larger than the budget | It survives while it is the newest. The budget is not a hard cap. |
| Stale hashline anchor | The whole call is refused and the file is untouched. |
| `----` anchor | Refused with an error saying it cannot be used for an edit. |
| Overlapping spans | The whole call is refused. |

The crate never reads the file system, so it cannot detect outside changes by itself. Detection happens when the caller compares current bytes with the recorded state through `BlobId::of`. History never leaves the process and is not shared with another process or a restarted session.

## Executable files that write creates

When `write` creates a new file whose content starts with `#!`, it also grants the execute permission on Unix (`crates/app-core/src/tool/write_shebang.rs` `make_executable`). The point is to stop a freshly written script from burning a turn on a permission error when it is next run as `bash ./script.sh`.

- Permissions change only on creation. Overwriting an existing file keeps that file's mode, because the owner set the mode and this call changed only the content.
- The target mode is the creation mode plus the execute bit. The creation mode is `0o666 & !umask`, so the execute bit is added only for readable classes. With umask 022, 644 becomes 755; with 077, 600 becomes 700 (`script_mode`).
- `umask()` is never called. That value is process-wide and would race another thread creating a file. Instead the mode of the just-created file is read and the execute bit is computed from it.
- It applies to creation only. Content written to the ACP client buffer does not exist on disk, so no execute bit is touched, and `edit`, `apply_patch`, and `hashline_edit` are unchanged.
- The result body gains a sentence saying the file was made executable. It is absent when permissions were not changed.
- Windows has no execute bit, so nothing happens. The same-named function under `#[cfg(not(unix))]` returns `false`.

## The blocked state in todo

The `todo` tool accepts `blocked` as a status. Writing a task that cannot proceed for an external reason as `pending` or `in_progress` loses the reason, and the list alone no longer says what is being awaited.

- `blocked` requires a `blocker` string. It allows up to 500 characters and refuses an empty or whitespace-only value (`crates/app-core/src/tool/todo_blocked.rs` `normalize`). A block with no reason is not stored.
- A `blocker` on an item that is not `blocked` is dropped. Left behind after a status change, it would read like a note that is still the live reason.
- Surrounding whitespace is trimmed before saving.
- Completion checks do not treat `blocked` as done. A list whose only remaining item is `blocked` is not complete, and the automatic nudge counts that item as remaining work (`crates/todo-policy/src/followup.rs` `is_incomplete_todo`). It also does not count as `in_progress` for the rule that allows only one in-progress item.
- ACP plan entries have only three states, so `blocked` is exported as `pending` (`crates/app-core/src/tool/todo.rs` `plan_entries`). That means an open item nobody is working on right now, and the reason stays on the item's own `blocker`.
- The tool result JSON includes `blocker`, so whoever reads the list can read the reason.

## Ancestor AGENTS.md attached to read results

When `read` returns a file body, it looks for `AGENTS.md` in directories above that file and appends them to the end of the result. The point is that a session reading a directory sees that directory's rules together with the file. This document describes the behaviour of the current tree. The code is `inject_for_read` in `crates/app-core/src/tool/nested_agents.rs`, called from `execute` in `crates/app-core/src/tool/read.rs`.

### What it does

The injection target is the tool result, not the system prompt. After `read`'s `execute` builds the text body, it calls `nested_agents::inject_for_read(&path, output.len(), &ctx)` and appends the returned string to the result with `push_str`.

The empty-body decision is measured before that call with `let base_is_empty = output.is_empty();`. That keeps the `(empty file)` marker from being pushed aside by the injection text and still attaches ancestor contracts to an empty file (the `if base_is_empty` branch appends the injection too). Injection only appends. It never parses, merges, or enforces file contents as rules.

### Which files are attached

#### Ancestor search rule

The search is `find_agents_md_up`. It starts at the directory holding the file being read, walks upward, and stops at the session's working directory. The `while let Some(dir) = current` loop `break`s on `dir == root || !dir.starts_with(root)`. Found files are ordered root to leaf with `found.reverse()` before attaching, so a nearer directory's contract comes later and more specific rules are read last.

Three cases are excluded.

- The working directory's own `AGENTS.md` is not attached, because the `break` above ends the search the moment it reaches the working directory. That file is already handled by the system prompt layers (layer 4 in [`docs/runtime/PROMPT.md`](PROMPT.md)).
- When the file being read is that directory's own `AGENTS.md`, it is not included. The `canonical != start` condition in `find_agents_md_up` skips that case. Only ancestor contracts are attached.
- An ancestor directory with no `AGENTS.md` is skipped. Only existing files are collected, in order.

#### Boundary enforcement

Both the injection target path and the working directory are canonicalized before comparison. `contain` runs `std::fs::canonicalize` on both and returns `None` when `file == root || !file.starts_with(&root)`. Nothing is attached then.

Because of the canonicalization, a symlink inside the working directory that actually points outside it is refused. The link's name alone never passes. The `contain_rejects_symlink_escaping_root` test pins this case.

#### Size ceilings

There are three ceilings, and the first two count the bytes actually appended to the result, not just the file body. Headers and truncation notices are inside them.

| Constant | Value | What it stops |
| --- | --- | --- |
| `MAX_BYTES_PER_FILE` | 16 KiB | The whole block one ancestor `AGENTS.md` produces (header, body, truncation notice) |
| `MAX_BYTES_PER_READ` | 32 KiB | The sum of every block one `read` attaches |
| `MAX_BASE_OUTPUT_BYTES` | 32 KiB | A read result at or above this size gets no attachment at all. This is not where the guard starts; it is a conservative margin so the attachment cannot push a still-acceptable result past the guard's real limit |

When building a block, the header and truncation notice lengths are subtracted first and only the remainder goes into the body, so no block exceeds its ceiling. A cut body is cut at a UTF-8 character boundary and leaves an `[Note: content truncated ...]` line at the end of that block. `MAX_BYTES_PER_READ` likewise caps every block that read attaches plus the skipped notice.

When even the next ancestor's header does not fit, or the notice for a truncated body does not fit, injection stops there and appends a `[Note: N more ancestor AGENTS.md files skipped: ...]` notice to the end of the result. To leave room for that notice, a slice of the read budget is held back when sizing each block (`SKIPPED_MARKER_RESERVE`). If the notice still does not fit, it is dropped. Nothing is appended past a ceiling.

### When nothing is attached

If any of the following holds, the result is the plain body.

- The `[tools] nested_agents_md` setting is off. `inject_for_read` reads `crate::config::config().tools.nested_agents_md` first.
- The session has no working directory. If `ctx.working_dir.as_deref()` is `None` it returns immediately and logs `nested_agents: no working_dir; skipping ancestor AGENTS.md injection`.
- The read file is outside the working directory, or a symlink points outward.
- No `AGENTS.md` exists anywhere above.
- The read result is already 32 KiB or larger (`base_output_bytes >= MAX_BASE_OUTPUT_BYTES`). This ceiling is not where the tool-result guard starts. The guard's real limit is `min(budget × 0.30, 50,000)` tokens from `single_output_max_tokens` (`crates/app-core/src/tool/mod.rs:1201-1203`, constants defined at `:1361` and `:1372`), converted at four characters per token (`APPROX_CHARS_PER_TOKEN = 4`, `crates/core/src/util.rs:17`), so it settles at 200,000 characters once the budget reaches about 166,667 tokens. In a session whose context is nearly full, the remaining headroom binds before that (`mod.rs:1668-1671`). At the context-window scale of real models, 32 KiB falls far short of that limit. The ceiling exists so the attachment cannot push a still-acceptable result past the guard. The cost is that a result between 32 KiB and the guard's real limit returns its body with no contracts and no marker saying so. Past the guard's limit the result itself is withheld and comes back truncated only when the caller passes `accept_large_output`.
- The path was already injected in this session. See the session-scoped cache below.
- `read` does not reach the text branch. `agent://` references return from `is_agent_url`, images from `is_image_file`, PDFs from `is_pdf_file`, and binaries from `is_binary_file` before the injection point.

### Configuration

The switch is in the `[tools]` table of `config.toml`. It defaults to on (`crates/base/src/config.rs:902-905`, `crates/base/src/config.rs:934`).

```toml
[tools]
nested_agents_md = true
```

The environment override is `HORON_AGENT_NESTED_AGENTS_MD`. Values like `0` or `false` parse as off (`crates/base/src/config/env_overrides.rs:161-165`). The default configuration file carries the same key and description (`crates/base/src/config/default_file.rs:435-441`).

### Session-scoped cache

The same `AGENTS.md` is never attached twice in one session. Injected paths are collected per session id, and an existing path is skipped (`already_injected` and `mark_injected` on `InjectedSessions`).

- The cache is process-global and tracks at most 64 sessions (`MAX_TRACKED_SESSIONS`). `ensure_session` registers a session, then drops the earliest-registered session's record while `order.len() > MAX_TRACKED_SESSIONS`.
- A session whose record was pushed out receives the same path again. The `inserting_more_than_max_sessions_evicts_the_oldest` test pins that order.
- Changing a file's content within a session does not re-attach it. Only the path is remembered.

### Output examples

A block is two newlines, then a bracketed header, then the file body verbatim (`block_header`'s `format!("\n\n[Directory Context: {}]\n", ...)` in `inject_for_read`).

```text
    1	use crate::config;

[Directory Context: /Users/me/repo/crates/base/AGENTS.md]
crates/base rules body

[Directory Context: /Users/me/repo/crates/base/config/AGENTS.md]
config subdirectory rules body
```

A truncated body gets `[Note: content truncated to save context window; read the file directly for the rest: <path>]` at the end of its block. When budget ran out and ancestor files were skipped, `[Note: N more ancestor AGENTS.md files skipped: this read's context budget is full; read them directly]` is appended to the result instead.

### Known limits

- Compaction does not re-attach anything. The cache looks only at session id and path, and nothing clears records on compaction.
- The cache is process memory only. A fresh process starts empty and re-attaches on that session's first `read`.
- Past 64 sessions, the oldest session's record is pushed out, so that session receives the same contracts once more.
- Injection happens in `read` only. It is not attached to `agentgrep`, `ls`, or `bash` output.
- Rules are not enforced. An attached body is text the model reads, and no check blocks a violation.
- The working directory's own `AGENTS.md` never arrives this way. That file and the global `~/AGENTS.md` belong to the prompt layers.
- A read result of 32 KiB or more gets no contracts and the result does not say so. A reader cannot tell "there is no AGENTS.md here" from "the body was too large, so contracts were skipped". Until the guard's real limit, the body comes back normally, so the result alone cannot distinguish the two. The constant responsible for that silent omission is `MAX_BASE_OUTPUT_BYTES`.

## Reading skills

Where skills come from and what happens when names collide is in the "Skills" section of [`PROMPT.md`](PROMPT.md). This section covers only how the tool reads a skill.

### `read skill://`

- `skill://<name>` returns that skill's body. The result starts with `## Skill: <name>` and `Base directory: <skill folder>`; frontmatter is dropped.
- `skill://<name>/<path>` reads a file relative to the skill folder. When the name carries a namespace (`claude/review`), the longest registered prefix becomes the name and the rest is the path.
- Line selectors (`skill://qa-skill/references/a.md:20-40`) and `offset`/`limit` behave as in a file read.
- The name is resolved against this session's effective list, which is user skills plus the skills in that session's working folder, so project skills from another repository are invisible.
- The path is canonicalized and must lie inside the canonical skill folder. `..`, absolute paths, and outward symlinks fail with `skill:// path resolves outside the skill directory`. An unknown skill fails with `skill:// names no installed skill`.

### Relationship to path policy

The path-denial policy (`path_policy`) never treats the literal `skill://` as a working-folder path. Instead it applies the existing denial rules to the real file that `skill://<name>/<path>` names, so a file the policy blocks cannot be read through a skill path either. In a session whose policy blocks everything outside the working folder, a sub-file of a user-level skill such as `~/.horon/skills` hits that rule. Such sessions read user-level skills with `skill_manage` (action `load`).

## Managing skills

`create`, `update`, and `delete` in `skill_manage` touch only skills the model wrote itself. Those live at `~/.horon/managed-skills/<name>/SKILL.md`, and a human-written skill with the same name always wins. So none of the three actions can edit or delete a human-written skill.

| Action | What it does | When it refuses |
| --- | --- | --- |
| `create` | Writes a new managed skill. | Refuses when a skill with the same name is already loaded or exists in the managed directory. |
| `update` | Changes a managed skill's description and body. | Says to use `create` when no managed skill exists. |
| `delete` | Removes the managed skill directory and reloads the skill list. | When the same name is loaded as a human-written skill, names its source and deletes no file. |

`create` and `update` require `[tools] managed_skills` to be on. `delete` ignores that switch, because managed skills left behind after turning it off still need cleaning up.

The safety rules for `delete` live in one place, `remove` in `crates/base/src/managed_skills.rs`.

- The name is revalidated, so `..` or a path separator cannot reach outside the managed directory.
- A symlink in the managed directory is deleted as a link only. The directory it points at and the files inside it stay.
- A real directory is canonicalized, checked to be inside the managed directory, and then deleted. Recursive deletion does not follow inner symlinks and removes only the links.
- A missing name produces a not-found error.
- After deletion the shared skill registry is reloaded, so the next turn's list and loads see it immediately. If the reload fails, the result says so alongside the fact that the files are already gone.

Refusal text names the source a name belongs to. A skill from a working folder's `.agents/skills`, for example, reads "a project skill from the agents source (<directory>)". The text is built by reading the skill source table (`SOURCES` in `crates/base/src/skill/sources.rs`), so it never invents a directory the table does not have.

In plan mode all three actions are refused. `skill_manage` declares the write effect, and the plan-mode gate blocks calls with the write effect before execution. That gate is described in [Tool effects, plan mode, and approval tiers](#tool-effects-plan-mode-and-approval-tiers).

## Reading memory

Memory injected into the prompt is truncated, and seeing the full content used to require a separate call to the `memory` tool. `read` now handles `memory://` references directly, with the same tool and the same paging rules as files.

### `read memory://`

- `memory://<id>` returns the full content of that one memory. The output is an `id`, `category`, `scope`, `tags`, `created`, `updated`, `links` header plus a `content:` body. `links` holds the target ids of graph edges to other memories (related, supersedes, contradicts, derived); tag and cluster edges are metadata and are left out.
- `memory://` with no id lists stored memories newest first. Each row is `- <id> [<scope>/<category>] <one-line preview>` with the preview capped at 80 characters of the first line. Memories stored at the same instant are ordered by id, so the order is stable.
- Line selectors (`memory://<id>:1-3`) and `start_line`/`end_line`/`offset`/`limit` work exactly as in a file read, including the next-page hint.
- An unknown id fails with `memory:// record not found: <id>`. Memory stored by another project is outside this session's scope: it is not listed and cannot be read by id.
- The readable scope is the working folder's project store from the call that made the `read`, plus the user-level global store.
- The reference is read-only and never becomes a file path. The path-denial policy does not treat the literal `memory://` as a working-folder path, so memory reads work in a session with that policy.
- No path writes to `memory://` through `write` or `edit`. Changing memory goes through the `memory` tool.

### `memory` action `update`

- `{"action": "update", "id": "...", "content": "...", "category": "...", "tags": [...]}` edits one memory in place. All three fields are optional and only supplied fields change.
- The id, creation time (`created_at`), access count, reinforcement records, and graph edges stay. Only `updated_at` moves to the new time.
- Changing the body drops the stored embedding, so the next recall recomputes it from the new body. Changing tags also updates the graph's tag edges.
- The body goes through secrets masking before it is stored, with the same rules as saving session history.
- An unknown id returns `Not found: <id>`. Supplying no field, or a whitespace-only body, is refused as a tool error.

### `memory` action `learn`

- `{"action": "learn", "memory": "...", "context": "...", "scope": "project" | "global", "skill": {...}}` records one lesson: a non-obvious fix, a procedure, a project convention worth reusing. It is the same call omp's `learn` makes.
- The lesson text goes in `memory`, not `lesson`. A call that sends `lesson` is refused ("Unknown field: `lesson`. The lesson text goes in `memory`."), because serde would otherwise ignore the field and store nothing.
- `memory` is capped at 2,000 characters and `context` at 400; over the cap is a tool error. `context` is stored with the lesson as a `Context:` line.
- `scope` is `project` (default) or `global`, landing in the same stores as `remember`. `all` is a read scope and is refused.
- The record's category is `lesson`, not `fact`, so recall can surface it as a lesson and rank it above a fact of the same age. `from_extracted` maps `lesson` and `learning` to that category too, so an extracted lesson is no longer folded into `fact`.
- The text is masked with the same secret rules as saved history before it is stored, and the repeat of an identical lesson reinforces the stored record instead of adding a second one.
- With `skill` given, `{"action": "create" | "update", "name": "...", "description": "...", "body": "..."}` writes a managed skill through the same code as `skill_manage create`/`update`. `name` is required: a `skill` object without it is reported as "`skill.name` is required" and nothing is written. A skill failure is reported on a second line of the result and never rolls back the lesson: the lesson is already stored. Unlike `skill_manage`, the skills list is not reloaded, so a new skill appears after the next `reload_all` or restart.
- Authoring a skill still requires `[tools] managed_skills = true`; with it off, the lesson is stored and the result says the skill was not written.

## Cross-session memory consolidation

Extraction stores memories one session at a time and never merges across sessions, so the same subject piles up with drifting wording and there is no compact digest to inject. Consolidation is the second phase: it runs immediately after an extraction, inside the extraction task, never on the turn path.

- Entries that share a subject merge into one active entry. The survivor keeps its id and content; the others are kept on disk and marked superseded (`superseded_by` plus a `Supersedes` edge), so they are still readable by id and nothing is deleted.
- Two memories share a subject when they share at least two content terms and at least 60% of their combined term set, or when they share a tag. The pass prefers merging over keeping near-duplicates: a wrong merge costs a ranking slot, not data, and every merge is logged with the superseded ids and the session or tool that stored each.
- A `correction` outranks every other category on its subject, because it records what the user said is true; a `fact` extracted later does not overwrite it. Within one category the newest entry wins.
- Both scopes consolidate: the working folder's project store and the user-level global store, each with its own pass marker and summary (`crates/base/src/memory/consolidation.rs`).
- Each scope's summary is written next to that scope's memory file and injected as memory guidance on the next fresh user turn, in the same slot as the per-turn relevance payload. It contributes nothing until a pass has run.
- `[agents] memory_consolidation_min_new` (Horon default `20`) is how many memories must have been stored since the previous pass before it runs. A pass reloads the graph, merges and rewrites it, so it is batched instead of run per session.
- `[agents] memory_consolidation_summary_bytes` (Horon default `4096`) caps the injected summary. Over the cap the text is cut at a character boundary, never dropped.

## Where MCP server configuration comes from

MCP server definitions are merged in the order given by `crates/base/src/mcp/sources.rs`. Earlier sources come first, and the first definition of a name wins.

| Order | Source | File | When it is on |
| --- | --- | --- | --- |
| 1 | Working folder | `.horon/mcp.json` > `.mcp.json` > `.claude/mcp.json` | When the working folder is trusted (`docs/safety/SECURITY.md`) |
| 2 | horon-agent | `~/.horon/mcp.json` | Always |
| 3 | Claude Code | `~/.claude.json` (this project's entry > global `mcpServers`) > `~/.claude/mcp.json` | `[mcp] claude_user = true` |
| 4 | Codex CLI | `[mcp_servers.*]` in `~/.codex/config.toml` | `[mcp] codex_user = true` |

- There is one exception. When the same name has a local stdio definition and a remote (HTTP, SSE) definition, the stdio definition survives regardless of source order and a line is logged. A remote twin of a working local server usually needs its own login (issue #653).
- User settings from other hosts (Claude Code, Codex) are off by default. Those servers were reviewed elsewhere, and loading them starts processes here. When an off file holds servers, each process logs one line with the count and the switch that turns it on. `HORON_AGENT_DISABLE_CLAUDE_MCP` keeps the Claude source unread even with `claude_user` on.
- Codex configuration is read live every time. It is not copied into `~/.horon/mcp.json` on first run as it once was, so environment values (which may be secret) are not duplicated and a server removed in Codex disappears on the next load.
- The project entry in `~/.claude.json` is selected by the canonical working folder path, so one project's servers never mix into another repository's session.
- A user-level file is not re-parsed while its `FileStamp` holds; a process cache (up to 16 files) is used instead. A session start with no changed user settings reads no file at all. Working-folder files are not cached, because they must be parsed from the same single read that produced the trust decision.
- Environment expansion (`${VAR}`) and removal of unlaunchable entries happen after merging, every time.

### Resources and prompts

Servers that publish resources or prompts are reached through the same `mcp` tool, so the core tool list and the deferred-loading layout do not change.

| Action | Arguments | Result |
| --- | --- | --- |
| `resources` | optional `server` | One line per resource (URI, name, MIME type, description) for the named server, or for every connected server when no name is given. |
| `read_resource` | `server`, `uri` | The resource body. Text goes through the same secret redaction as tool results and is fitted to the MCP result budget unless `accept_large_output: true`; a binary body is summarized by MIME type and decoded size. |
| `prompts` | optional `server` | One line per prompt with its arguments; a trailing `*` marks a required argument. |
| `get_prompt` | `server`, `name`, optional `arguments` | The prompt expanded by the server, as text. It is returned to the caller like any tool output and is not injected into the conversation. |

- A server that did not advertise the `resources` or `prompts` capability gets `MCP resources are not supported by '<server>' …` and no JSON-RPC request is written for it.
- `notifications/resources/list_changed` and `notifications/prompts/list_changed` drop the cached list, so the next `resources` or `prompts` call refetches it. The cache belongs to the connection and starts empty again after a reconnect.
- Resource subscriptions (`resources/subscribe`), sampling, and roots are not implemented.

## Large tool output and context saving

Three devices reduce how much tool output enters the model's context. None of them discards the original. The model gets the reduced result together with an address for reading the original back.

### Session artifacts and `artifact://`

The full body of output over budget is saved under the agent home's `artifacts/<session id>/<id>.<tool>.log` (`crates/base/src/artifacts.rs`). The id is the next number above the largest id already in that directory, so a restart continues after existing artifacts instead of reusing an id a transcript may still point at. The session id is validated against the alphabet the session store uses. The text is masked with the same history secrets masking before it is written.

- The model reads the original page by page with `read file_path="artifact://<id>"` plus `start_line`/`end_line`. Another session's artifacts cannot be read.
- One artifact keeps the first and last 8 MiB; the elided middle is replaced by an `[ARTIFACT TRUNCATED: ...]` notice on a line boundary.
- When a session's artifacts exceed 256 MiB, the lowest ids are deleted first until the session is back under the cap.
- They sit outside the session store (`sessions/<id>/`), whose layout `doctor` and cloning assume is fixed.

### Condensing rules

Output over budget becomes a head, a signal line for the omitted region, a tail, and the address of the original. Head and tail are 40% of the budget each and are cut on line boundaries. Lines in the omitted region that contain words such as `error`, `warning`, `failed`, `panic`, `exception`, `traceback`, `fatal`, or `assert` are carried over within the remaining budget as `L<original line>: <line>` (300 bytes per line).

| Point | Budget |
| --- | --- |
| `bash` | 30,000 bytes |
| `webfetch` | 40,000 bytes (the 5 MB download cap is unchanged) |
| PDF text from `read` | 64 KiB |
| Other tools (MCP, `agentgrep`, and the rest) | 48 KiB, applied in `Registry::execute` just before the context guard |

A call given `accept_large_output: true` and `read`'s text files are not condensed. A text `read` returns at most 64 KiB at once and points at the next `start_line` (or `offset`). The file is on disk, so nothing is stored separately. SDK tool results never pass through the registry and get only the history cap (512 KiB, by bytes).

### `read` deduplication

Reading a file that was already read in the same session compares content hashes (`crates/app-core/src/tool/read_state.rs`).

- When the content matches and the requested lines are inside a range already shown, the body is replaced by one line: `[read: <path> lines a-b are unchanged since your earlier read (tool call <id>) ...]`.
- When the content changed, the earlier read covered the whole file, and this read also covers the whole file, a unified diff against the earlier read is returned. If the diff is larger than 40% of the file, the whole file is sent. The baseline snapshot is kept only for files up to 1 MiB and is not readable through `artifact://`.
- `full: true` always sends the body. When the `hashline` format differs from the earlier read, it is rendered again.
- Applying compaction, rewinding or editing the conversation, urgent truncation, aging old results, and ending the session all drop that session's records. The deduplication is only correct while the earlier output is still in the context.

The setting is `[tools] read_dedup = true` (the default), with environment override `HORON_AGENT_READ_DEDUP`.

### Aging batches of old results

Editing earlier messages breaks the prompt cache from that point on, so results are reduced in batches rather than one per turn (`crates/app-core/src/agent/tool_result_aging.rs`). Targets are tool results over 8 KiB that sit outside the most recent 24 messages and behind the compaction boundary.

- The batch runs when the cache lifetime since the last provider response has passed (300 seconds when the path does not say). The cache is already cold, so there is no rewrite cost.
- While the cache is alive it runs only when the context is at or above 75% of the compaction threshold and the targets total at least 64 KiB.
- It does not run during a compaction or on a provider-specific compaction path.

An aged result starts with `[horon: condensed older tool output: ...]` and keeps 1 KiB at each end plus the `artifact://` address of the original. A result with this prefix is never aged again. Each batch records "tool result aging" as a documented invalidation in the KV cache monitor and saves the session once. The setting is `[tools] tool_result_aging = true` (the default), with environment override `HORON_AGENT_TOOL_RESULT_AGING`.

### Instrumentation

`process.stage_counters` in `horon debug 'server:memory'` reports `tool_output_spills`, `tool_output_spill_original_bytes`, `tool_output_spill_kept_bytes`, `read_unchanged_hits`, `read_delta_hits`, `read_bytes_avoided`, `tool_result_aging_batches`, `tool_result_aged_results`, and `tool_result_aged_bytes`. The `context` section of the structural gate (`scripts/perf/check_structural.py`) checks that a 1 MiB output lands inside 48 KiB, that a repeated read stays under 512 bytes, and that crossing the pressure band twice produces exactly one aging batch.

## Auxiliary completion and the secret round trip

This explains the contract of one side call that does not touch the conversation, and the round trip in which a credential leaves for the provider and returns to the tool as its real value. The two devices address different problems but draw their boundary the same way: what the caller may and may not decide is split by types, and values crossing the boundary are refused with named errors.

### The auxiliary completion contract

`CompletionRequest` has exactly four fields (`crates/base/src/aux_completion.rs:90`): the prompt, optional system text, an optional output token cap, and an optional response schema. There is no field, setter, or argument that picks a model, an endpoint, credentials, or tools. So the caller cannot change which model runs, where the request goes, who pays, or what the model can do. Those four stay with the session that dispatches the request.

| Item | Limit | Location |
| --- | --- | --- |
| `prompt` | 65,536 bytes (64 KiB) | `crates/base/src/aux_completion.rs:70` |
| `system` | 16,384 bytes (16 KiB) | `crates/base/src/aux_completion.rs:73` |
| `max_tokens` | 1 to 4,096 inclusive | `crates/base/src/aux_completion.rs:76`, `:79` |

Limits are measured in bytes, not characters. Both strings are compared with `len()` (`crates/base/src/aux_completion.rs:101`, `:118`). A Korean prompt with the same character count is three times the bytes, so a character-based limit would quietly triple.

A request outside the contract is refused where the value is built, so a problem never surfaces only after a model call has already been paid for.

| Error prefix | When | Location |
| --- | --- | --- |
| `AUX_COMPLETION_INPUT` | The prompt or system text exceeds its limit, or `max_tokens` is out of range | `crates/base/src/aux_completion.rs:64` |
| `AUX_COMPLETION_SCHEMA` | The schema is not an object, or the response violates the schema | `crates/base/src/aux_completion.rs:67` |

Without system text, a fixed sentence is used instead (`crates/base/src/aux_completion.rs:82`). It instructs the model to answer in text only and call no tools. Caller-supplied text replaces it.

### The schema subset

Schema checking is a small validator inside this module rather than the JSON Schema crate (`crates/base/src/aux_completion.rs:207`). The `base` crate is at the bottom of the dependency graph, with `agent-sdk` depending on `base`. Pulling in a full validator would reverse that direction or add a new external dependency for one field.

Exactly five keywords are supported.

| Keyword | Meaning |
| --- | --- |
| `type` | `object`, `array`, `string`, `number`, `integer`, `boolean`, `null`, or an array of those names |
| `required` | Property names an object must carry |
| `properties` | Per-property subschemas, checked only when the property is present |
| `items` | The subschema every array element must satisfy |
| `enum` | The values a response may take |

Every other keyword is ignored. `additionalProperties`, `minLength`, `pattern`, `format`, numeric ranges, `oneOf`/`anyOf`/`allOf`, and `$ref` all fall in that group. A schema relying on them is weaker than it looks, so the subset is documented rather than implied.

### Tool registration

The `aux_complete` tool is registered only when `[tools] aux_completion` is on (`crates/app-core/src/tool/mod.rs:2425`). The registration function reads the setting and returns immediately when it is off. Off means the tool does not appear in the tool list and cannot be found by name.

It is not in `CORE_TOOLS` (`crates/base/src/config.rs:756`). The core profile's closed list does not grow. Every call spends one more side model request, which is why the switch defaults to off (`crates/base/src/config.rs:930`).

The tool is a thin adapter (`crates/app-core/src/tool/aux_complete.rs:99`). It converts model arguments into the contract type and returns text; it performs neither limit checks nor schema validation. Checks in two places would let rules diverge per surface.

### Secret placeholders

When a credential leaves for the provider, a placeholder goes instead of the real value. Its shape is `«SECRET:<label>:<base>»`. `label` is the detection rule's name, and `base` is a 12-character string that corresponds to one real value. `base` is the first 8 bytes of an HMAC-SHA256 computed with the 32-byte key stored in `secret-placeholder.key` in the horon-agent home, written in base 36 (`A`–`Z`, `0`–`9`). The key file is created with mode 0600 and recreated when missing. If it cannot be created, each process uses a new temporary key and logs a warning.

The same real value always receives the same placeholder. A value appearing several times in a conversation keeps its placeholder, so the cached prompt prefix does not shift. Because the key stays on disk, a session reopened after a restart restores the earlier placeholders too. Building a session vault pre-registers environment variables whose names contain `KEY`, `SECRET`, `TOKEN`, `PASSWORD`, `PASS`, `AUTH`, `CREDENTIAL`, or `PRIVATE` and whose value is at least 8 characters, so credentials from the environment also resolve after a restart.

There are three modes (`crates/config-types/src/lib.rs:556`).

| Mode | Behaviour |
| --- | --- |
| `off` | Sends text unchanged. This is the default. |
| `obfuscate` | Replaces detected credentials with stable placeholders and gives tools that need the real value that value back. |
| `block` | Refuses calls that would send a credential outward. |

Mode names are read in kebab case, and an unknown value falls back to the default (`crates/config-types/src/lib.rs:589`). That keeps one bad entry from making the rest of the configuration unreadable. The setting is `secrets_mode` (`crates/base/src/config.rs:933`), and the default configuration file also writes `secrets_mode = "off"` (`crates/base/src/config/default_file.rs:470`).

Detection rules are not new: the same table as the existing masking is used. The pattern list behind `redact_secrets` on the disk path is the wire path's list (`crates/base/src/message.rs:51`). With one table, a pattern added for disk applies to the wire automatically. Already-masked text is never masked again, because wrapping a placeholder in another pattern means restoring the outer layer returns only the inner placeholder.

### Three wiring points

The round trip happens in three places.

1. Messages leaving for the provider. Only the request copy carries placeholders. In `off` mode no copy is made and the held messages are sent unchanged.
2. Tool input. Placeholders are restored to real values before reaching the gate.
3. Tool output. Values are masked back into placeholders before entering history.

Restoration running before the gate is the design's invariant. An approver who saw a placeholder would approve a command nobody will execute. The tool receives the restored input and executes it, so what the gate saw must equal what runs. The two steps are bound in one function so a call site cannot reorder them.

The mapping between placeholders and real values lives only in process memory. A per-session vault holds the real-to-placeholder table, the reverse table, and a per-name counter, and writes nothing to disk. It disappears when the process ends. Debug output prints item counts, not values, so a log line or a crash bundle cannot leak credentials through the vault itself.

`block` mode refuses with an error starting `SECRET_BLOCK`, naming how many hits matched which label and how to switch to `obfuscate`.