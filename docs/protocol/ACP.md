# ACP support (v1 and v2)

Language: **English** | [한국어](ACP.ko.md)

The horon agent supports Agent Client Protocol (ACP) v1 and v2 together on one endpoint so an editor can attach it as an external agent. Running `horon acp` turns it into an ACP agent that exchanges JSON-RPC messages over standard input and output. v1 is the stable version and its messages follow the official `schema-v1.26.0`. v2 follows the stable portion of `schema-v2.0.0-alpha.13`. The v1 sections of this document describe how a v1 connection behaves; the v2 differences are collected in [ACP v2](#acp-v2).

## Running

```bash
horon acp            # handle ACP v1 over standard input and output
```

`acp` writes nothing but ACP messages to stdout (line-delimited UTF-8 JSON). Logs go to stderr, and the process exits as soon as stdin closes. A request still being handled never gets its response.

Model calls go straight to the `/chat/completions` endpoint of the OpenAI-compatible endpoint configured in `[providers.<name>]`, so that profile has to be ready first. Configuration and API keys follow [MODEL.md](../provider/MODEL.md).

`acp` connects to the agent daemon. When no daemon is running, `acp` starts one itself and asks for the tool profile in `[acp] tool_profile` (default `acp`). A daemon that is already running keeps its own settings. A `--tool-profile` you pass directly wins over both.

### Editor setup

Every editor puts the setting somewhere else and names the keys differently. What they share is registering the command `horon` (or `horon-agent`) with the arguments `["acp"]` as an external agent. Below are examples for Zed and JetBrains. Check each editor's documentation for the exact key names.

Zed (`settings.json`):

```json
{
  "agent_servers": {
    "horon agent": {
      "type": "custom",
      "command": "/absolute/path/horon",
      "args": ["acp"]
    }
  }
}
```

JetBrains (`~/.jetbrains/acp.json`):

```json
{
  "agent_servers": {
    "horon agent": {
      "command": "/absolute/path/horon",
      "args": ["acp"]
    }
  }
}
```

The ACP plugins for Neovim and VS Code take the same two values (command, args). The setting key differs per plugin.

## Authentication

The `authMethods` in the `initialize` response are:

| id | Offered when | What it does |
|---|---|---|
| `horon-agent-credentials` | Always | Checks whether the credentials of a `[providers.<name>]` profile can be used to call models |

Register a profile with `horon provider add`. Even when the client advertises `auth.terminal`, there is only one method. This build has no login command for the editor to run in a terminal.

`authenticate` succeeds with `{}` when the daemon reports a usable model. With no model it returns `-32000`. `session/new` also returns `-32000` when no model is usable. An unknown `methodId` is `-32602`.

`logout` is not advertised, because there is no way to erase credentials.

## Advertised capabilities

`initialize` always answers with `protocolVersion: 1`, even when the client sends 65535. A `protocolVersion` that is missing or not an integer in the 0–65535 range is `-32602`. Any request sent before `initialize` is `-32600`. The client's `clientCapabilities` and `clientInfo` are stored per connection and replaced when `initialize` is sent again.

| Item | Value |
|---|---|
| `loadSession` | `true` |
| `promptCapabilities` | `image: true`, `embeddedContext: true`, `audio: false` |
| `mcpCapabilities` | `http: true`, `sse: true` (stdio is always supported) |
| `sessionCapabilities` | `close`, `resume`, `list`, `delete`, `additionalDirectories` |

A method that calls an unadvertised capability is refused with `-32601`. An unknown request starting with `_` is also `-32601`, and unknown notifications are ignored. Notifications never get a response.

Error codes follow these rules.

| Code | Used for |
|---|---|
| `-32000` | Authentication required (no usable model) |
| `-32002` | Unknown or inactive session (includes `data.sessionId`) |
| `-32600` | Request before `initialize` |
| `-32601` | Unsupported method |
| `-32602` | Bad parameters, including a second prompt sent to a session whose prompt is still running |
| `-32603` | Internal error |
| `-32800` | Cancelled request |

## Sessions

| Method | Behaviour |
|---|---|
| `session/new` | `cwd` is required and must be an absolute path, otherwise `-32602`. Accepts `additionalDirectories` and `mcpServers` |
| `session/load` | Opens a stored session. Replays the history as `session/update` before responding |
| `session/resume` | Opens the session without replaying history |
| `session/close` | Cancels the running prompt and releases the session connection. That prompt ends as `cancelled` |
| `session/list` | Lists unarchived sessions by most recent activity |
| `session/delete` | Hides the session from the list |

`session/list` filters by `cwd` and returns 50 sessions at a time. The next page comes from `nextCursor`. A bad cursor is `-32602`, and no results is an empty array.

`session/delete` does not delete the session file from disk. The daemon archives the session so it drops out of the list. Sending it again for an already archived session still succeeds, so repeating the call gives the same result. When another client is using that session it is refused with `-32603` (open in another horon agent client).

When `session/load` or `session/resume` attaches a session whose store the daemon has not yet released to another connection, the daemon sends the store's refusal (`store error [internal:busy] retryable session writer is already owned`) as an error frame. The adapter retries only that refusal once on a fresh connection (waiting up to 2 seconds when the daemon reports `retry_after_secs`), and when the retry is refused too it answers immediately with `-32603` carrying the daemon's reason. It does not retry once history replay has started. Other errors, a refusal saying another client owns the session, and a dropped connection are answered immediately without retrying. A session whose history is large enough to make restore slow waits as long as the daemon takes to send the history; only a daemon that has stopped entirely is answered as an unresponsive request at the 300-second safety limit of each attempt.

### load versus resume

`load` replays the history and then responds. `resume` responds immediately without replaying. Sending `load` or `resume` again for a session already open on this connection reuses that connection instead of attaching again.

`load` replays the stored history as `session/update` before it responds. User prompts arrive as `user_message_chunk`, reasoning as `agent_thought_chunk` and answers as `agent_message_chunk`. Reasoning appears in the stored transcript, and therefore in the replay, only when `reasoning_display` is `full`. `full` is the default, so reasoning is replayed under default settings as well. Each tool call replays as a pair: a completed `tool_call` and a `tool_call_update` carrying the stored text. For an edit tool that text contains a unified diff. Structured diff content is not stored, so it is not replayed. A failed tool leaves no error marker in the record and therefore replays as completed. The final plan is restored from the `todo` tool input. Replayed messages carry a `messageId` of the form `replay-<row number>`.

`compaction_update` is replayed only when the client advertised `session.compaction`. Clients that did not advertise it are not sent compaction information. For a compacted session the daemon replays only the 64 most recent rows it shows. Replay goes only to the connection that sent `load`; `session/resume` never replays.

## The flow of one prompt

`session/prompt` accepts `text`, `resource_link`, `resource` (text and blob) and `image` blocks. `audio` is not advertised, so it is refused.

Every `session/update` of that turn goes out before the response. The `stopReason` in the response is the actual cause the daemon reported.

| stopReason | Meaning |
|---|---|
| `end_turn` | Normal completion |
| `max_tokens` | Hit the output limit |
| `max_turn_requests` | Exceeded the request count allowed in one turn |
| `refusal` | The model refused |
| `cancelled` | Cancelled through `session/cancel`, `$/cancel_request` or `session/close` |

A cancelled turn always answers `cancelled`, whatever shape the daemon stream ends in. Interrupting the current turn externally also gives `cancelled`. Daemon events left over from the cancelled turn (late text, tool, plan and diff frames and that turn's termination signal) are drained by the adapter, so they never leak into the next `session/prompt`. The `session/prompt` after a cancelled turn asks the daemon whether the session is still busy before sending the message. That wait is at most about 5 seconds. If the session is still busy, the message is sent anyway and a daemon refusal is answered with `-32603`. A daemon event that arrives about 10 seconds or more after the cancelled turn responded cannot be drained and may mix into the next prompt's updates. Token usage is carried in `_meta["horon-agent"].usage` of the response.

`session/cancel` is a notification. Sent when no prompt is running, it does nothing. `$/cancel_request` cancels requests in both directions. Pointed at a running prompt, that prompt receives one response and ends. Any other running request is answered `-32800`. Unknown ids are ignored.

## What the editor sees

During a turn, these flow out as `session/update`.

| Update | Content |
|---|---|
| `agent_message_chunk` | Answer text. A new `messageId` starts after each tool execution round |
| `agent_thought_chunk` | The model's reasoning |
| `plan` | The to-do list |
| `tool_call` | Start of a tool call. Carries the kind and title |
| `tool_call_update` | Input (`rawInput`) and absolute `locations`, result text, file diff (absolute paths, `oldText`, `newText`), terminal output |
| `usage_update` | Context usage |
| `session_info_update` | Session title change |
| `available_commands_update` | Slash command list |
| `config_option_update`, `current_mode_update` | Settings or mode changed |

The `tool_call_update` that ends a call also names the call's verdict, as `"_meta": {"horon-agent": {"verdict": "<name>"}}`. The names are `ok`, `partial`, `truncated`, `withheld`, `rejected`, `failed` and `interrupted`, with the meanings listed in [Tool result verdicts](../runtime/TOOLS.md#tool-result-verdicts). Every profile sends it. When the daemon reported no verdict for the call there is no `_meta` key and the frame is byte for byte what it was before. `status` is not derived from the verdict: it is still `failed` when the call returned an error and `completed` otherwise. `load` replays a stored verdict the same way on the replayed `tool_call_update`, and a result stored without a verdict replays without `_meta`.

`notice` and `compaction_update` are sent only when the client announced `session.notices` and `session.compaction` in `clientCapabilities`. Without that announcement they are not sent. After compaction finishes the `compactionId` has the form `<turn id>-cmp-<sequence>`.

There are three slash commands: `/model`, `/models` and `/effort`. With one leading space the line is not interpreted as a command and goes to the model untouched.

`plan` appears only when the `todo` tool is available. Under default settings that tool exists only when `horon acp` started the daemon with the `acp` profile. A daemon started first by the desktop app or by `serve` runs the `core` profile, so no plan updates appear.

### The acp tool profile

The `acp` profile allows only 12 tools: `agentgrep`, `apply_patch`, `ask`, `bash`, `batch`, `edit`, `ls`, `mcp`, `read`, `replace`, `todo`, `write`.

## Permission modes

A session has three modes.

| Mode id | Behaviour | Daemon approval setting it maps to |
|---|---|---|
| `default` | Follow the daemon setting (`approval_mode`) as it is | Clears the session override |
| `ask-commands` | Asks before shell, network and file-mutating tools | `write` |
| `auto` | Runs without asking | `off` |

The mode appears in two places: `modes` and the first entry of `configOptions` (`mode`, category `mode`). The mode can change while a prompt is running through `session/set_mode` or `session/set_config_option`, and the change emits `current_mode_update` and `config_option_update`. An unknown mode id is `-32602`.

When a tool needs approval, `session/request_permission` goes to the editor and the tool waits for the answer. Overlapping calls for the same invocation share one question.

| optionId | Result |
|---|---|
| `allow-once` | Allow this once |
| `allow-always` | Allow. The same tool name is not asked again for the session |
| `allow-path` | v2 only. Allow this file from now on, recorded as a scoped grant |
| `allow-folder` | v2 only. Allow this file's folder from now on, recorded as a scoped grant |
| `reject-once` | Reject this once |
| `reject-always` | Reject. The same tool name is not asked again for the session |

A v1 session gets the four options it always had, in the same order, so an existing editor is unaffected. A v2 session gets `allow-path` and `allow-folder` only when the call names exactly one filesystem path: one answer must not decide several places the user was never shown. Neither scoped option touches the per-tool memory that `allow-always` writes, because the grant is about a place, not a tool. The daemon stores the grant itself (`docs/runtime/TOOLS.md`, "Scoped grants") and repeats the single-path check, so a client cannot widen its own answer.

A cancellation result, an unknown optionId, a malformed response and a client error are all treated as rejection. When another view decides first, the editor's question is withdrawn with `$/cancel_request`.

## Config options

| id | category | Value |
|---|---|---|
| `mode` | `mode` | `default`, `ask-commands`, `auto`. Always first |
| `model` | `model` | Models the provider offers |
| `reasoning_effort` | `thought_level` | Reasoning effort |

`model` and `reasoning_effort` appear only when both a current value and choices exist. `session/set_config_option` accepts only selectable values and returns the full option list. A boolean or any non-string value, an unknown id, a value outside the choices, and a change of `model` or `reasoning_effort` while a prompt is running are `-32602`.

For older clients, `session/set_model` (`modelId` or `model`) and `session/set_reasoning_effort` (`effort` or `reasoningEffort`) are accepted too.

## Delegating to editor features

A request goes to the client only when the client advertised that capability in `clientCapabilities`. Unadvertised features are never requested, and the fallback in the table applies.

| Client capability | When advertised | When not advertised |
|---|---|---|
| `fs.readTextFile`, `fs.writeTextFile` | Text reads and writes of `read`, `write`, `edit`, `replace` and `apply_patch` go out as `fs/read_text_file` and `fs/write_text_file`. Unsaved buffers are read too | Reads and writes local disk directly |
| `terminal` | A foreground `bash` (neither PTY nor background) runs through `terminal/create`, `terminal/wait_for_exit`, `terminal/output` and `terminal/release`. On timeout `terminal/kill` is sent. The `tool_call` carries the terminal | Runs locally |
| `elicitation.form` | `ask` questions go out as an `elicitation/create` form (mode `form`) | `ask` follows `[tools] ask_policy` |

`accept` from the form makes the chosen entry the answer. `decline`, `cancel`, a malformed response and a timeout become an empty answer, and `ask_policy` decides what happens next. `recommended` picks the recommended entry, `error` makes the `ask` tool fail. Questions are never used for secret input such as passwords, and URL-style forms are not used.

Timeouts for requests to the client are 60 seconds for files, 30 seconds for terminal control and 300 seconds for forms. Terminal `wait_for_exit` has no separate limit. On timeout `$/cancel_request` is sent. File and terminal requests are reported to the daemon as `-32800`, and forms become an empty answer. Cancelling a prompt or closing a session withdraws open client requests the same way. `terminal/kill` and `terminal/release` are not withdrawn.

## Per-session MCP servers

The `mcpServers` of `session/new`, `session/load` and `session/resume` are used only for that session and that ACP connection. `stdio`, `http` and `sse` are supported. When the session closes or the connection drops, stdio child processes end too, and nothing is shared with other sessions. An empty `type` means `stdio`. An unknown `type` or an entry missing a required field is `-32602`.

## Isolation from other clients

ACP-only events (client requests, permission questions, plans, diffs) leave only through the transport the ACP connection owns. Desktop, web, `run` and `serve` clients attached to the same daemon do not receive them. Files, terminals and permission requests from those clients' turns never reach the editor either. Once the ACP connection is gone, no ACP mode remains on the session.

## ACP v2

v2 ships through the same `horon acp` as v1. There is no separate command or port. Each connection picks v1 or v2 from `protocolVersion` in `initialize`, and only that side's messages travel on the connection. A v1 client receives the same responses as before v2 existed.

### Version negotiation

A `protocolVersion` that is missing, not an integer, or above 65535 is `-32602`. Other values are chosen as follows.

| `protocolVersion` | v2 on (default) | v2 off |
|---|---|---|
| `1` | v1 | v1 |
| `2` or higher | v2 (larger values are answered as 2) | v1 |
| `0` | `-32022` with `data: {requested: 0, supported: [1, 2]}`. The connection stays uninitialized | v1 |

A second `initialize` on a v2 connection is accepted only before any other request and replaces the last values. When the repeat fails with `-32022` or `-32602` the earlier initialization stands. When the repeat carries 1 the connection switches to v1. A request choosing v2 without `info` is refused.

### Turning v2 off

If v2 negotiation causes problems, you can fall back to v1 only.

```toml
[acp]
protocol_v2 = false
```

The environment variable `HORON_AGENT_ACP_PROTOCOL_V2=0` does the same. Use `1` or `true` to turn it on. When it is off, every request is answered as a v1 request. Sending 0 or 65535 still returns `protocolVersion: 1`. A new `horon acp` has to be started for a changed setting to apply.

### Advertised capabilities (v2)

The v2 `initialize` response consists of `protocolVersion: 2`, `info` (`name: "horon-agent"`, `title: "HoronAgent"`, `version`), `capabilities` and `authMethods`.

| Item | Value |
|---|---|
| `capabilities.session.prompt` | `image: {}`, `audio: {}`, `embeddedContext: {}` |
| `capabilities.session.mcp` | `stdio: {}`. `http: {}` is added only when the v1 adapter advertises `mcpCapabilities.http` |
| `capabilities.session` | `delete: {}`, `additionalDirectories: {}` |
| `capabilities._meta["horon-agent"]` | `profile` and `extensions`. Present only under the `extended` or `full` profile |

Unlike v1 there are no `true`/`false` markers and no `loadSession`, and `list`/`resume`/`close` are not advertised entries either. The `info` and `capabilities` the client sends are stored per connection.

A v2 connection accepts `initialize`, `auth/login`, `auth/logout`, `session/new`, `session/list`, `session/resume`, `session/close`, `session/prompt`, `session/cancel`, `session/set_config_option`, `session/delete`, `tool_call/cancel`, `terminal/cancel` and `$/cancel_request`. `session/load`, `session/set_mode`, `authenticate`, `logout`, `session/set_model`, `session/set_reasoning_effort` and every other method not starting with `_` are `-32601`. Requests starting with `_` are `-32601` as well, and notifications get no answer. A request with `id: null` is answered with `id: null` and `-32600`. On a connection negotiated as v2, a JSON array frame is handled as a JSON-RPC batch and each entry takes the normal request path.

### What the editor sees (v2)

**Prompt acknowledgement.** `session/prompt` answers `{messageId}` as soon as the daemon reports it stored the user message. It does not wait for the turn to finish. `messageId` is the message id the daemon stored. When the submission is refused before storing, it produces no updates. An unknown session is `-32002`, bad content is `-32602`, no usable model or a logged-out connection is `-32000`, and a daemon that is re-reading its configuration is `-32603`. A prompt sent while a turn is running is not refused. When an earlier prompt on this connection is still being handled, the adapter holds prompts in the order received and sends them to the daemon once the session can accept the next one; when a turn from another view is running, the daemon stores the message after that turn. Either way the response comes after storage. If `$/cancel_request`, `session/cancel` or `session/close` arrives before storage, the answer is `-32800` and the held or waiting message is withdrawn. A failure after storage is reported not as a response but as an idle `state_update` carrying `stopReason: "error"` and `error: {code, message}`.

**Input.** A prompt may use `text`, `image`, `audio`, `resource` and `resource_link` blocks. `audio` must have a `mimeType` of `audio/wav`, `audio/mpeg` or `audio/mp3`, and reaches the model request as an `input_audio` part. Any other audio format is `-32602` before anything is sent to the daemon. A v1 connection does not advertise `audio` and refuses it as before. Under v2 an empty `prompt` array is `-32602`; v1 still accepts an empty array.

**State updates.** The session's foreground turn is reported through `state_update` whether this editor started it, another view did, or the agent started it by itself. The order is `running` at the start, `requires_action` while waiting for a permission or question answer, `running` again before the first event after the answer, and `idle` at the end. `idle` carries `stopReason` (`end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled`, `error`) and omits it when the cause is unknown. `idle` arrives exactly once, after every other update of that turn. `session/cancel` sends the remaining updates and then ends with `idle` (`cancelled`). When the adapter knows the run id of the turn it is reporting, it sends `cancel_turn{run_id}` to the daemon, so a turn another view started in the meantime is not stopped. When the run id is not known yet (before this editor's prompt reported `running`), the previous `cancel` is sent. `session/close` cancels the running prompt the same way and sends the last updates and `idle` (`cancelled`) before the close response. Closing a prompt that was accepted but has not reported `running` follows the same order. `usage_update` goes out at the position where the daemon reported usage, never mixed into another update.

**Message ids.** Every `*_message_chunk` and whole-message update has a `messageId`. The user message of a prompt this editor sent is reported once as a single `user_message`, with the same id as the `messageId` in the prompt response. User messages of turns started in another view are reported too. Only rows stored without a display role qualify, and `[auto]` system notices are not user messages. A soft interrupt injected mid-turn arrives as a `user_message` whose `messageId` is the row id, as soon as the daemon stores the row. Answer and reasoning ids are built from the stored id of the message that started the turn in the form `<id>:a<n>` and `<id>:t<n>`. `n` grows with each tool execution round. Replay computes ids by the same rule, so live and replayed ids match.

**Tool calls.** v2 has no `tool_call` update. The first `tool_call_update` for a `toolCallId` creates the call and carries `title`, `name`, `kind` and `status: "pending"`. As input completes, `rawInput`, absolute `locations` and the bash command fill in through later updates. Content arrives as `tool_call_content_chunk`, and a field left out does not change. A call that started executing advertises `capabilities: {cancel: {}}` and withdraws it with `capabilities: null` once it reaches a final state. `tool_call/cancel {sessionId, toolCallId}` stops only that advertised call and answers `{}` as soon as the daemon reports the cancellation started. The call ends with `status: "cancelled"` and the turn continues. An unknown call, an already finished call and an unadvertised call are answered `-32602` without contacting the daemon. Tools running when the turn is cancelled end with `status: "cancelled"`. When `session/close` fails to stop the turn within its limit, open tools still end as `cancelled`, and a dropped daemon connection ends them as `failed` followed by an idle `error`.

**Tool verdict.** The `tool_call_update` that moves a call to its final status carries the verdict as `"_meta": {"horon-agent": {"verdict": "<name>"}}`, with the same names and rules as v1: every profile sends it, a call without a verdict has no `_meta` key, and `status` is not derived from it. A call stopped with `tool_call/cancel` got its final `status: "cancelled"` update when the cancellation started, and the adapter sends no second update when the tool later finishes, so that call's verdict is not reported. `session/resume` with `replayFrom: {type: "start"}` puts a stored verdict on the replayed completed `tool_call_update`.

**Structured diff.** A file edit goes out as `{type: "diff", changes: [{operation, path, oldPath, fileType: "text"}], patch: {format: "git_patch", text}}`. `operation` is `add`, `modify`, `delete` or `move`, and `oldPath` is present only for `move`. A file that did not exist before the call, such as `apply_patch`'s `Add File`, is `add`, and the patch contains `new file mode` and `--- /dev/null`. Paths are absolute without `a/` or `b/` prefixes, and the missing side is `/dev/null`. Unlike v1, `oldText`/`newText` are not sent.

**Display terminal.** A foreground `bash` appears as a display-only terminal owned by the agent. Its content carries `{type: "terminal", terminalId}`; once the command is known `terminal_update {terminalId, command, cwd}` arrives, and output during execution arrives live as `terminal_output_chunk {terminalId, data}`. At the end `terminal_update {terminalId, output: {data}, exitStatus: {exitCode, signal}}` arrives. `data` is base64-encoded. The editor is never asked to create a terminal.

**Background work.** Work that keeps running in the session after a tool call has handed control back to the model is attached to every `state_update` of that session (`running`, `requires_action`, `idle` and the current state in `session/resume`) as a `background` list. Without such work it is `background: []`. A `bash` that moved to the background is listed as a terminal (`{type: "terminal", terminalId}`) and any other tool as its call (`{type: "tool_call", toolCallId}`). Only work owned by the daemon's background work manager is listed; session-management work such as monitors, scheduled ambient runs and swarm waits is not. An entry is announced with `terminal_update` or `tool_call_update` before it joins the list, and its final state (`exitStatus` or `completed`/`failed`/`cancelled`) is sent before it leaves. When the list changes, the same state is sent again with the changed list and no new `stopReason`. Removing an entry while the session is `idle` is deferred by up to 250 ms. If a new turn starts in the meantime, one `running` with the entry removed is sent; otherwise an `idle` with the entry removed is sent. A background terminal advertises `capabilities: {cancel: {}}`, and `terminal/cancel {sessionId, terminalId}` stops that process and then answers `{}`. A terminal that is not listed or has already finished is answered `-32602` without contacting the daemon.

**Permission requests.** A tool needing approval asks through `session/request_permission {sessionId, title, description, subject, options}`. `subject` is `{type: "command", command, cwd, toolCallId}` for `bash` and `{type: "tool_call", toolCall}` for other tools. The choices and the way allow-always and reject-always are remembered match v1, and a cancellation, unknown result or unknown optionId never counts as approval.

**Plan.** The to-do list arrives as `plan_update {plan: {type: "items", planId: "todo", entries}}`, and each update replaces all entries.

**Config options and slash commands.** Options use `configId` and `groupId`, and `mode` (category `mode`) is always first. `session/set_config_option` accepts `type: "id"` with a string value or `type: "boolean"` with a boolean, and returns the full list. There is no boolean option, so a boolean value is `-32602`. A change is announced with `config_option_update`, not `current_mode_update`. Slash commands carry `type: "command"` and `input: {type: "text", hint}`. The `session/new` and `session/resume` responses include `configOptions` and `availableCommands` (omitting the field when empty) and have no `modes`.

**Session settings.** `session/new` and `session/resume` take an absolute `cwd` plus the optional `additionalDirectories` (the full list every time) and `mcpServers`. Each `mcpServers` entry must have a `type` of `stdio` or `http`. `sse`, a missing `type` and an unadvertised `http` are `-32602`. In `session/resume`, a missing or `null` `replayFrom` attaches without history, and `{type: "start"}` replays the whole conversation before responding as `user_message`, `agent_thought`, `agent_message` and completed `tool_call_update`. Any other cursor is `-32602`. `session/list` returns `{sessions: [{sessionId, cwd, title?, updatedAt?, additionalDirectories?}], nextCursor?}`, and a bad cursor is `-32602`. Sending an unknown session to a method that takes `sessionId` gives `-32002` with `data.sessionId`. Reopening a v1 session as v2 replays it under the stored ids. When the daemon already running is an older build that does not know v2, `session/new` and `session/resume` answer `-32603` telling you to restart the daemon. The daemon is not stopped directly, because another client may be using it.

**Request ids.** A request reusing the id of a request that has not been answered yet is `-32600`. The same holds for duplicate ids inside a batch, and a batch response array always goes out with one answer per entry. A `session/prompt` arriving after `session/close` started is answered `-32603` without being forwarded to the daemon.

**Questions.** An `ask` question goes out as `elicitation/create` (form style, with `sessionId`) only when the client advertised `capabilities.elicitation.form` as an object, and `requires_action` is reported while it waits. When the daemon first reports the `ask` call that raised the question, `toolCallId` is included too. `accept` becomes the answer, while `decline`, `cancel` and an unknown action follow `[tools] ask_policy` as in v1.

### Per-connection `auth/logout`

`authMethods` contains only `horon-agent-credentials` with `type: "agent"`. Even when the client advertises `capabilities.auth.terminal`, no terminal method is attached. `auth/login {methodId}` behaves like `authenticate` in v1. With no usable model it is `-32000`, and an unknown `methodId` is `-32602`.

`auth/logout {}` ends only that connection's authentication state. Stored credentials are not erased; a test confirms the credential file of an isolated home stays byte-identical. Until `auth/login` succeeds again on the same connection, `session/new`, `session/resume` and `session/prompt` answer `-32000`.

### What v2 does not do

- It sends no `fs/*` or `terminal/*` requests to the client, including `terminal/create`. In the stable v2 schema the agent sends the client only four methods: `session/request_permission`, `session/update`, `elicitation/create` and `elicitation/complete`. To read and write editor files, attach the editor's MCP server through `mcpServers`; terminals show up only as the display terminal described above.
- Methods and features that exist only in draft or unstable form, such as `session/fork` or `providers/*`, are not supported. They are not advertised, so they answer `-32601`.
- URL-style questions (`elicitation.url`) are not sent. The daemon's `ask` questions are all form questions choosing among options, so there is no question to send by URL. That is also why `elicitation/complete`, which announces a finished URL question, is never sent.
- The tool status `paused` is not sent. The daemon has no state that parks a started tool.
- An empty `prompt` array is `-32602` under v2 only. v1 accepts it as before.
- SSE MCP servers are refused with `-32602`.

### Known v2 limitations

- Downgrading horon-agent to an older build and upgrading again makes `session/resume` replay treat every user row as the start of a turn until the session cache is rebuilt. For turns stored by the older build, the `messageId` produced by the replay can differ from the id seen live.
- When the model endpoint does not support streaming, no partial text arrives before the answer finishes. The answer therefore may look as if it arrived all at once.
- The conditional v2 items of the official acp-tck (patch, permission, terminal, plan paths) are skipped because the TCK only ever sends `"hi"`. `tests/acp_stdio_v2.rs` verifies those paths instead.

### v2 conformance testing

```bash
cargo test --locked -p horon-agent --test acp_stdio_v2     # v2 integration scenarios
bash scripts/test/acp_tck.sh --protocol-version 2               # official acp-tck v2
cargo test --locked -p horon-agent --test acp_tck -- --ignored --test-threads=1   # v1 and v2 TCK
```

`tests/acp_stdio_v2.rs` runs v2 scenarios IS-1 through IS-21 and the v1/v2 side-by-side scenarios, and validates frames against `tests/fixtures/acp/schema-v2.0.0-alpha.13.json`. IS-16 covers `tool_call/cancel`, IS-17 covers `terminal/cancel` and the `background` list, IS-18 covers `terminal_output_chunk`, IS-19 covers an audio prompt, IS-20 covers a live soft interrupt and IS-21 covers holding a prompt during work, all against a real daemon. `scripts/test/acp_tck.sh` takes `--protocol-version 1|2` and defaults to v1. The official acp-tck judges against its own copy of the v2 schema (alpha.9, unstable), so its result is not evidence of alpha.13 conformance. Before the switch to alpha.13, both TCK tests (v1, v2) passed on the integration branch and both verdicts were conformant. At that point the v2 mandatory items passed 24, the capability items passed 49 with 16 skipped and no failures. The v1 mandatory items passed 31.

## Configuration

`[acp]` in `config.toml`:

```toml
[acp]
profile = "standard"   # standard | extended | full
tool_profile = "acp"   # tool profile requested when acp starts the daemon itself
protocol_v2 = true     # false answers every client with v1 only
```

`protocol_v2` decides whether v2 is negotiated. The default is `true`.

`standard` sends only messages that fit the spec. `extended` and `full` also send `_horon_agent/*` extension notifications that clients may ignore. The environment variables `HORON_AGENT_ACP_PROFILE`, `HORON_AGENT_ACP_TOOL_PROFILE` and `HORON_AGENT_ACP_PROTOCOL_V2` override the same values.

## Known limitations

- A v1 connection does not support `logout`. For `auth/logout` on a v2 connection see [Per-connection `auth/logout`](#per-connection-authlogout).
- A v1 connection does not advertise `audio` input. The `systemPrompt` session feature is advertised on neither v1 nor v2.
- The v2 limitations are listed separately under [Known v2 limitations](#known-v2-limitations).
- URL-style elicitation is not used.
- Terminal delegation applies only to foreground, non-PTY, non-background `bash` calls.
- When the model endpoint does not support streaming, no partial deltas arrive before the answer finishes. The answer therefore may look as if it arrived all at once.
- `session/delete` only archives, so the file stays on disk.
- When stdin closes, a request still in flight loses its response.

## Conformance testing

`cargo test --locked -p horon-agent --test acp_stdio` runs 15 tests, one per integration scenario IS-1 through IS-15. Every frame is validated against `tests/fixtures/acp/schema-v1.26.0.json`.

```bash
# official acp-tck v1 suite (pinned commit, isolated daemon)
bash scripts/test/acp_tck.sh
ACP_TCK_REQUIRE_PASS=<requirement id,...> bash scripts/test/acp_tck.sh
```

`scripts/test/acp_tck.sh` needs `uv`, `git`, `cargo` and `python3`, and downloads the suite on first use. It exits 0 only when the report's verdict is conformant, authentication and version mismatches blocked nothing, mandatory (MANDATORY) items neither failed nor went unrun, and no capability (CAPABILITY) item failed. Listing requirement ids in `ACP_TCK_REQUIRE_PASS` (or `--require-pass`) additionally requires every one of them to PASS.

The same judgement can run as a Rust test.

```bash
cargo test --locked -p horon-agent --test acp_tck -- --ignored --nocapture
```

Session lifecycle and authentication are covered by `tests/acp_lifecycle.rs`, and the test support code by `tests/acp_infra.rs`. The copies of the official schemas and their provenance are in `tests/fixtures/acp/README.md`.