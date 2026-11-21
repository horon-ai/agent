# Model calls

Language: **English** | [한국어](MODEL.ko.md)

The agent sends model requests directly to the endpoint of the profile's wire protocol. A profile speaks one of four wires: Chat Completions, the OpenAI Responses API, Anthropic Messages, or Gemini generateContent. There is no router process or plugin layer in between, and no port is opened for the model. Endpoints are written under `[providers.<name>]` in `config.toml` and selected with `[provider] default_provider` or `--provider-profile`.

## Model path

Model requests leave through a single path.

1. `[provider] default_provider` selects the startup profile name. Given `--provider-profile <name>`, that run selects the `[providers.<name>]` table instead. `-p`/`--provider` takes `auto` (the default, which follows `[provider] default_provider` or `--provider-profile`), `openai-compatible`, or one profile id: either a built-in id such as `openrouter`, or the name of a `[providers.<name>]` table you wrote.
2. The model is chosen from `--model <id>`, the profile's `default_model`, and `[provider] default_model`, whichever is set. If none is set the call ends in an error.
3. The request URL is `base_url` with the path of the profile's `type` appended, such as `/chat/completions` for `openai-compatible`. Write the API prefix such as `/v1` as part of `base_url`.
4. Tool definitions travel as `tools` in the same request body. No separate model server relays the tools.

Calling a model with no profile selected makes the agent stop before it reaches the network, and stderr tells you to add an `openai-compatible` profile under `[providers.<name>]`.

## Wire protocols

A profile's `type` key picks the request and response shape. Four values are accepted.

| `type` | Endpoint | Default auth header | Effort field | `api_method` |
| --- | --- | --- | --- | --- |
| `openai-compatible` (default) | `POST {base_url}/chat/completions` | `Authorization: Bearer <key>` | `reasoning_effort` | `chat_completions` |
| `openai-responses` | `POST {base_url}/responses` | `Authorization: Bearer <key>` | `reasoning.effort` | `responses` |
| `anthropic-compatible` | `POST {base_url}/messages` | `x-api-key: <key>` | `output_config.effort` with adaptive thinking, or `thinking.budget_tokens` | `messages` |
| `gemini-compatible` | `POST {base_url}/models/{model}:streamGenerateContent?alt=sse` | `x-goog-api-key: <key>` | `generationConfig.thinkingConfig` | `generate_content` |

A `base_url` that already ends with the wire's path is used as it is, so a gateway that publishes the full path needs no rewriting. The `anthropic-compatible` wire sends `anthropic-version: 2023-06-01` unless the profile's `headers` set that name itself. `open-router` is rejected at runtime, and the error names the four supported values.

`type` defaults the credential header, and an explicit `auth` always wins. With `auth` omitted, the two OpenAI kinds send `Authorization: Bearer <key>`, `anthropic-compatible` sends `x-api-key`, and `gemini-compatible` sends `x-goog-api-key`. `auth = "header"` without `auth_header` sends `api-key` for the OpenAI kinds, `x-api-key` for Anthropic, and `x-goog-api-key` for Gemini. `auth = "none"` sends no key. A profile written before these wires existed keeps working unchanged, because `openai-compatible` is the default and its defaults did not move.

Effort maps per wire. `openai-compatible` sends `reasoning_effort`, and `openai-responses` sends `reasoning.effort`. `anthropic-compatible` sends `output_config.effort` with adaptive thinking for models that accept it, or `thinking.budget_tokens` for models that take a manual budget (low 4096, medium 10000, high, xhigh and max 32000), and nothing for a non-Claude model. `gemini-compatible` sends `generationConfig.thinkingConfig` with `thinkingBudget` for a `gemini-2.5*` model (minimal 512, low 1024, medium 8192, high and xhigh 24576) and `thinkingLevel` (`MINIMAL` through `XHIGH`) for the rest, always with `includeThoughts: true`.

Each wire appears in the model routes under its own `api_method` value from the table above. The runtime identity stays `openai-compatible` or `openai-compatible:<profile>` for all four, so stored sessions, the harness API and the SDK see one provider name.

`prompt_cache_key` applies to `openai-compatible` and `openai-responses` only. `extra_body` keys are merged into the top level of every request body on all four wires. By default, the model list uses the profile's static `[[providers.<name>.models]]` array.

`horon model list --profile <name> --search <text>` filters the configured list by profile and by a case-insensitive substring in the model ID. Add `--refresh` to fetch `GET /models` for matching `openai-compatible` and `openai-responses` profiles using their configured authentication. The report identifies each profile and whether it refreshed, failed, or uses an unsupported wire. Failed and unsupported profiles retain their configured models in the report. `--json` includes the model source and reason per profile. Refresh results apply to that command and leave `config.toml` unchanged.

### Tool parameter schemas

A tool's `parameters` is sent as JSON Schema narrowed to the selected wire's dialect. Unsupported keywords are reported as `schema_keyword_dropped`. Both OpenAI wires require an object at the root. A root `anyOf` or `oneOf` of object alternatives becomes a single object with the fields the alternatives advertise and the requirements shared by all object alternatives. A property already declared at the root keeps that declaration. A field declared only in the alternatives keeps its different schemas under a property-level `anyOf`. The request uses `strict: false` and reports the removed root combiner. The tool still enforces conditional requirements at execution, such as the `label` required by `swarm`'s `spawn` action.

```toml
# Chat Completions, the default
[providers.my-chat]
type = "openai-compatible"
base_url = "https://llm.example.com/v1"
api_key_env = "MY_CHAT_API_KEY"
default_model = "my-model"

# OpenAI Responses API
[providers.my-responses]
type = "openai-responses"
base_url = "https://llm.example.com/v1"
api_key_env = "MY_RESPONSES_API_KEY"
default_model = "my-model"

# Anthropic Messages; no auth line, so x-api-key carries the key
[providers.my-anthropic]
type = "anthropic-compatible"
base_url = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY"
default_model = "claude-sonnet-4-6"

# Gemini generateContent; no auth line, so x-goog-api-key carries the key
[providers.my-gemini]
type = "gemini-compatible"
base_url = "https://generativelanguage.googleapis.com/v1beta"
api_key_env = "GEMINI_API_KEY"
default_model = "gemini-2.5-pro"
```

## Explicit provider prefixes

Adding the `openai:`, `openai-api:`, or `openai-oauth:` prefix to `--model` or `/model` is taken as naming the built-in OpenAI path explicitly. The prefix is handled before model-name inference and before the active `[providers.<name>]` profile, so a prefixed model never flows into an active compatible profile (`crates/base/src/provider/mod.rs` `set_model`, `crates/provider-core/src/selection.rs` `explicit_model_provider_prefix`). `openai-api:` and `openai-oauth:` also pin API-key and OAuth credential mode respectively (`crates/provider-core/src/auth_mode.rs` `parse_explicit_credential_prefix`).

This build has no built-in OpenAI runtime. So a specification such as `openai:gpt-5` fails before sending a request, and the error names the active profile and says to use `<profile>:<model>` to use that profile. An unprefixed model name stays with the currently active profile. This behavior is pinned by the `explicit_openai_prefix_never_falls_through_to_active_profile` test (`crates/base/src/provider/tests.rs`).

## Configuration

```toml
[provider]
default_provider = "my-gw"

[providers.my-gw]
type = "openai-compatible"
base_url = "https://llm.example.com/v1"
api_key_env = "MY_GW_API_KEY"
env_file = "my-gw.env"
auth = "bearer"
default_model = "my-model"
extra_body = { chat_template_kwargs = { thinking = true } }

[[providers.my-gw.models]]
id = "my-model"
context_window = 128000

[[providers.my-gw.models]]
id = "my-vision-model"
input = ["text", "image"]
```

| Key | Meaning |
| --- | --- |
| `type` | `"openai-compatible"` (default), `"openai-responses"`, `"anthropic-compatible"`, or `"gemini-compatible"`. It picks the wire listed under [Wire protocols](#wire-protocols). |
| `base_url` | Required. The address including the API prefix. `http` is accepted only for a local or private host: `localhost`, a `*.local` name, a loopback, private, link-local or CGNAT address, or an IPv6 loopback, unique-local or link-local address. Every other host must use `https`. |
| `api_key_env` | Name of the environment variable holding the key. It checks the process environment first and reads `env_file` when no value is there. |
| `env_file` | Name of the key file kept in the config directory. Write one per line, such as `MY_GW_API_KEY=...`. |
| `auth` | One of `bearer`, `header`, `none`. Leaving it out applies the `type` default: `bearer` for the two OpenAI kinds, and `header` with the wire's own header for Anthropic and Gemini. `header` takes the header name from `auth_header` and falls back to `api-key`, `x-api-key`, or `x-goog-api-key` by type when it is empty. |
| `default_model` | The profile's default model id. |
| `models` | The `[[providers.<name>.models]]` array. `id` is required, and `context_window`, `input`, `reasoning`, and `reasoning_effort` may accompany it. |
| `headers` | Extra HTTP headers added to every request. |
| `extra_body` | A top-level JSON object merged into the top level of every request body on all four wires. Use it for backend-only fields such as `chat_template_kwargs`. |
| `requires_api_key` | States whether a key is needed. Set it to `false` where there is no key, as with a local endpoint. |
| `prompt_cache_key` | Decides whether a lineage-stable `prompt_cache_key` goes into the request body. `true` always sends it, `false` never does, and leaving it empty sends it only for `api.openai.com` and `openrouter.ai` hosts. Set it to `false` when a gateway with a strict schema returns 400 for the unknown field. |
| `stream_first_event_timeout_secs` | Optional positive seconds from sending an attempt to the first meaningful event. The HTTP response-header timeout also applies. |
| `stream_idle_timeout_secs` | Optional positive seconds without meaningful progress once the stream starts. Each meaningful event restarts the idle timer. |
| `max_in_flight_requests` | Optional maximum concurrent model and image requests in this process for this named profile, from 1 to 65,535. Leave it unset to allow requests without a profile queue. |

Both limits apply to all four wires. Keepalives and usage-only updates do not start a stream or reset its idle timer. Each setting enables its own timer, and the existing retry limits apply to both timeout errors.

The request limit is shared across sessions and models on one profile. When it is full, a request waits before connecting and can be cancelled while waiting. A streaming request holds its slot until the stream ends or is dropped. Restart the agent process to apply a changed limit.

A single `default_model` line is enough to make calls. The `models` array tells the agent context size and input modalities, which helps model selection and compaction estimates.

A model's `reasoning_effort` is applied when that model becomes active. The session reports the level before the first request and the request carries it, so the status line draws `provider/model · high` without anyone running `/effort`, and the field is filled on whichever wire the profile speaks (the second column of [Wire protocols](#wire-protocols)). An explicit `/effort`, `shift+tab`, or a client `set_reasoning_effort` overrides the configured level and stays in force; a switch to another model then adopts the level that model declares, and a model that declares none keeps the effort already in force. A session restored from storage keeps the effort it recorded even when the configuration names a different level.

`horon run --thinking <effort>` overrides the selected model's effort for the run. `--service-tier <tier>` applies to OpenAI Chat Completions and Responses. Use `priority` (also `fast`), `off` to omit the request field, or `ultrafast` for a supported model. The command rejects unsupported values before sending the prompt. These flags leave the profile configuration unchanged.

## Model roles

A model role is a named workload-to-model mapping shared by every feature that needs a model other than the session's own. Roles are configuration only: a file without a role table resolves models exactly as this build does today.

### Role names

| Role | Workload that uses it | Written as |
| --- | --- | --- |
| `default` | Every turn of the session unless a more specific role applies. A bare `--model` or `/model` names this role for the run. | `default` |
| `plan` | Planning turns: the model that writes a plan before execution hands off (#70). | `plan` |
| `aux` | Cheap auxiliary work: compaction summaries, titles, classification, memory extraction. | `aux` |
| `judge` | Typed judgments: one bounded call answering a `bool`, `choice`, or `score` question about text the turn already produced. Today the single caller is unexpected-stop recovery (`docs/runtime/TURNS.md`). | `judge` |
| `image` | Image generation and editing: the one role whose model does not speak Chat Completions. Its single caller is the `generate_image` tool, which posts to `/images/generations` and `/images/edits` on the same profile, and only a profile of type `openai-compatible` or `openai-responses` accepts it. No inherited fallback, because a text model cannot answer an image request. | `image` |

No other role exists. A new role is added only together with the workload that calls it. Reason: a role with no named caller is a second spelling of `default`, and a role that appears before its consumer cannot be reviewed for the model it should point at.

### Where roles live

Roles are a sub-table of the profile that serves them.

```toml
[providers.my-gw]
default_model = "my-model"

[providers.my-gw.roles]
default = "my-model"
plan = "my-planner"
aux = "my-small-model"
```

A role value is a bare model id inside that profile. It is not a route spec: `openai:`, `openai-api:`, `openai-oauth:`, and `<profile>:` prefixes are refused with a configuration error naming the role. A role never selects a second profile, endpoint, or credential, and `[provider] default_provider` and `--provider-profile` stay the only way to choose those. Reason: one session keeps one endpoint and one credential, so a role cannot move a request to another account behind the caller's back, and the fallback order below remains a single chain.

The endpoint stays the only authority on whether a model id exists. The role layer does not check ids against `[[providers.<name>.models]]`, which is metadata for context size and model pickers and already accepts a `default_model` it does not list (`crates/base/src/provider/external.rs`). A role that names an unlisted model is used as written, and the generic context default applies until the array lists it.

### Existing per-workload keys

`[compaction] summary_model`, `[agents] swarm_model`, and `[agents] memory_model` keep their current meaning and are not aliases of a role. Reason: each resolves through its own path and understands values that are not model ids — `inherit` for the two agents keys (`crates/base/src/session/agent_models.rs` `effective_agent_model`, `crates/base/src/sidecar.rs`) — so reading them as role names would turn a sentinel into a model id. When a workload has both its own key and a role, the workload key wins and `roles.aux` fills only workloads whose own key is unset. A configuration file that never mentions `roles` behaves exactly as this build does today.

### Resolution order

A workload that asks for a role resolves it in this order, and the first step that yields a non-empty value wins.

1. The override for this run or turn (`--model` names `default`; a handoff sets `plan` through the existing model switch).
2. The role's own entry under `[providers.<name>.roles]`.
3. The entry of each role this role inherits from, in that role's declared order: `plan` and `aux` inherit `default`, and `judge` inherits `aux` then `default`.
4. The profile's `default_model`.
5. `[provider] default_model`.
6. An error that names the workload and the keys that were empty.

An empty string counts as unset at every step. `aux` walks the same chain, so a profile with only `default_model` uses today's model for auxiliary work. `judge` inherits `aux` before `default` so a profile that already names one cheap model for side work judges with it instead of spending the session model. Reason: steps 1, 4, and 5 are the order the agent already resolves one model today; roles slot in above them, so every existing file resolves to the same model it resolves to now, and an unset role inherits instead of failing.

`image` stops at step 3: it has no inherited roles and does not take a profile or provider text default. An image request sent to a chat model cannot succeed, so an unset `roles.image` means image generation is not configured, which the tool reports, rather than a call that always fails at the endpoint. This is the only role where a text default is not a valid substitute, and `ModelRole::accepts_text_default` states it once.

### Fallback and failover order

The ordered list that #73 adds is `fallback_models = ["m2", "m3"]` on `[providers.<name>]`, not on a role, and it applies to whichever model the role resolved to. Recovery enters these steps in this fixed order, and each is entered only after the one before it is exhausted.

1. Model fallback: the next entry of `fallback_models`, once per entry, after the retry budget for a retryable error is spent.
2. Same-provider account failover (`same_provider_account_failover`, default `true`).
3. Cross-provider failover (`cross_provider_failover`, default `countdown`; unattended runs need `autonomous_failover`, default `false`).

The switch lasts for the rest of the turn and the next turn starts again from the role's model. Non-retryable errors (401, invalid request, context overflow) never start the chain. Reason: the steps differ in what they cost to take. A model switch keeps the endpoint, the credential, and the cache host; an account switch changes the credential; a provider switch changes the endpoint. Cheapest first means a rate limit is absorbed without losing prompt-cache locality, and putting the list on the profile keeps it from naming a model on another endpoint, which the placement rule above forbids.

### Who implements it

#73 writes the shared type: the `roles` table flattened into `NamedProviderConfig` in `crates/config-types/src/named_provider.rs` and one resolution function that every caller uses. #70 adds only its own `[prewalk]` section and calls that function for `into` and `from`, spelling a role as `role:<name>` (`into = "role:aux"`). Reason: #73 changes `NamedProviderConfig` in any case and is a declared dependency of #70 (`depends_on: [73, 112]`), so two implementations would drift; the `role:` prefix keeps a role reference apart from a model id, as the existing `openai:` and `<profile>:` prefixes already do.

An unset role configuration behaves exactly as the build does today: no `roles` table, no new key, the model chosen by the existing chain, and no extra attempt.

### The `image` role and the one model path

The build's rule is that one model path serves models: a turn posts to the endpoint of the profile's `type`, on the profile named by `[provider] default_provider` or `--provider-profile`. The `generate_image` tool is the one documented exception, and it is a route rather than an architecture:

- It posts `POST {base_url}/images/generations` with a JSON body, or `POST {base_url}/images/edits` with a multipart body, on the **same** profile: same base URL, same credential, same `auth`, same `extra_body`.
- Only a profile of type `openai-compatible` or `openai-responses` accepts this route. A profile on the Anthropic or Gemini wire cannot serve it, because neither wire carries an Images endpoint: the tool still registers when `roles.image` names a model, and each call fails with an error naming the profile and its type instead of sending a request to a URL that cannot answer (`crates/base/src/provider/external.rs` `image_endpoint`).
- One provider runtime sends requests directly. `crates/provider-openai/src/images.rs` builds image requests beside `chat/`. Planned user-installed Rust packages sit outside the model path.
- The model comes from `roles.image`, and the tool is registered only when that entry exists **and** `[tools] generate_image = true`. Both conditions are required: the switch alone would advertise a tool whose every call fails, and the role alone would register it for a session that never asked.
- A call declares `WRITE | NETWORK`, so `approval_mode = "write"` asks first and an unattended run does not spend money without approval.
- The model never receives image bytes. Files land in `~/.horon/generated-images/` and the result names paths, sizes and MIME types, which keeps a call at a few hundred tokens instead of megabytes in the context and the stored event log.
- Output is limited to the OpenAI Images shape. A gateway that speaks it works unchanged, including the Horon AI Gateway. The Gemini-native image API and the Responses API's `image_generation` tool are out of scope; the MCP recipe in `docs/provider/INTEGRATIONS.md` covers anything this route does not.

### The `judge` role in practice

`crates/provider-core/src/judge.rs` owns the pure half of a judgment: the prompt (`JudgeRequest::prompt`), the response schema (`response_schema`), the limits (64 questions, 48 KiB of state, 4 KiB of instructions per question, 64 choice labels, 64 score levels), and a strict parser that rejects an invented label, an out-of-range probability, a missing or duplicated id, and any extra field. `crates/base/src/judge.rs` owns the half that needs configuration: `resolve_judge_model` walks the chain above, and `judge_with` forks the caller's provider, selects the resolved model, waits inside the attempt budget, and returns the parsed answers with the provider's usage.

A judgment is an ordinary model call on the session's own profile, so this role adds no endpoint and no credential; that keeps the build's one-model-path rule intact. A model that cannot be selected is an error, never a silent inheritance of the session model: the point of a judgment is a second model's opinion. The attempt budget is `[autojudge] attempt_timeout_ms`: it defaults to 3000, `0` means the default rather than no deadline, and anything above 30 s is clamped. The budget covers the whole attempt, model selection included, because a judgment holds up the turn it is judging.

The role is named `judge` and the `/judge` command is a different workload: `/judge` spawns a reviewer worker inside a swarm and resolves its model through the session agent-model overrides (`/model judge`, `/agents judge <model>`). Both read `[autojudge] model` as their model override, so one setting names the model in both places, but a judgment never spawns a worker and a worker never answers a typed question.

### What the fallback list looks like at runtime

`ModelRoleConfig::fallback_models()` in `crates/config-types/src/model_roles.rs` trims each entry and drops blanks, so an empty string never becomes an attempt.

Only the active named profile has a list this step may spend. `active_profile_fallback_models()` in `crates/app-core/src/agent/model_fallback.rs` resolves the profile from `HORON_AGENT_NAMED_PROVIDER_PROFILE`, then `HORON_AGENT_PROVIDER_PROFILE_NAME`, and returns the entries only when they are non-empty. A session that is not on a named profile behaves exactly as it did before this step existed.

`Agent::try_model_fallback` records an entry in the turn state before applying it, so a switch the provider refuses still spends that entry; otherwise the same refusal could be retried without a bound. `restore_model_fallback` puts the session's own model back at the next turn start (through `begin_turn_state`), on an interrupt, and on a failed turn. The switch never reaches `session.model`, so a reload or a client that reads the session sees the selection the user made.

Two read-only accessors expose the outcome. `Agent::fallback_models()` returns the configured entries, empty when no profile is active. `Agent::last_serving_model()` is set in `Agent::record_model_turn_usage` when a response is stored, so it still names the model that answered after the next turn has restored the session model. `get_runtime_info` reports them as `fallback_models` and `serving_model`, and the catalog push carries the same pair in its `identity` object, so a client that keeps the pushed catalog does not have to poll.

### Prewalk handoff

`[prewalk]` spends one model switch per session on the move from planning to execution: the session runs on the planning model while it plans, then continues on an execution model once the plan exists.

```toml
[prewalk]
enabled = true
into = "exec-model"
from = "plan-model"
```

| Key | Meaning |
| --- | --- |
| `enabled` | Whether a session hands off at all. Off by default. |
| `into` | The execution model: a bare model id, or `role:<name>`. Required when `enabled = true`; a section without a usable value fails config validation with an error naming `prewalk.into`. |
| `from` | Applicability only. When set, the session hands off just if it currently runs this model. It never names a model to return to, and leaving it out applies to every session. |

The handoff point depends on plan mode. With `[tools] plan_mode` off it is the first successful `todo` write that left at least one item; with plan mode on it is plan approval, so a todo list written during planning never switches the model out from under the plan. The switch takes the same path as `/model`, so `session.model` stores the execution model and every later request, including after a resume or a restore, runs on it.

The decision is one replay event, `prewalk` in the stored session, and that event is what makes the handoff one-shot across processes: a session that already carries one never decides again. A refused `into` is recorded the same way — an unknown model, a `role:` name outside `default`, `plan` and `aux`, or a role that resolves to nothing — so the session keeps its current model, one warning is logged, and later rounds do not repeat it. An `into` that resolves to the model the session already runs records the decision without switching.

`role:<name>` resolves through the active profile's role chain described above, which is why a profile that only sets `default_model` can hand off to `into = "role:aux"` and get that same model. `crates/app-core/src/agent/prewalk.rs` holds the decision, and the acceptance cases live in `crates/app-core/src/agent/prewalk_tests.rs`.

## Prompt cache key

A request that sends `prompt_cache_key` always uses the same key for the same session lineage (the root of a forked lineage, or the session id when there is none), the same system prompt, and the same tool list. The key is 28 characters, `pck_` followed by the first 12 bytes of sha256 in hex, and it stays the same even when conversation turns or compaction results change. So after compaction the leading prompt remains in the same cache slot, and a different lineage uses a different slot.

Calls without a session id send no key. `prompt_cache_key = true` sends it regardless of host, but use `false` for an endpoint that rejects unknown fields. The field is a Chat Completions and Responses API feature, so the two OpenAI wires send it and the Anthropic and Gemini wires do not.

The `api.openai.com` host also carries `prompt_cache_retention`. Its value is `HORON_AGENT_OPENAI_PROMPT_CACHE_RETENTION` (`in_memory` or `24h`) when that is set, and otherwise `24h` only when the model supports extended retention. Models that use the default 30-minute retention get no field. Other hosts do not get this field. `extra_body` is merged after both fields, so a profile can override the values.

## API keys and file locations

Keys are looked up in this order.

1. The environment variable `api_key_env` points at.
2. The key file `env_file` points at.
3. For a built-in profile, the environment variable and file that profile defines.

`env_file` and `config.toml` live in different directories.

| File | Location |
| --- | --- |
| `config.toml` | `<home>/config.toml` when `HORON_HOME` is set, otherwise `~/.horon/config.toml` |
| Key file | `<home>/config/horon-agent/<env_file>` when `HORON_HOME` is set, otherwise `horon-agent/<env_file>` under the platform config directory (`~/Library/Application Support/horon-agent/` on macOS, `~/.config/horon-agent/` on Linux) |

Key files get narrowed permissions when read. You can also write the key value directly into `api_key` in `config.toml`, but because that file is easily copied around, storing the key in a key file with `provider add --api-key-stdin` is the better choice.

## Creating a profile

`provider add` writes the same table into `config.toml` and stores the key received on `--api-key-stdin` in the key file.

```bash
printf '%s' "$MY_GW_KEY" | horon provider add my-gw \
  --type openai-compatible \
  --base-url https://llm.example.com/v1 \
  --model my-model \
  --api-key-env MY_GW_API_KEY \
  --api-key-stdin \
  --set-default
```

`--type` takes `openai-compatible`, `openai-responses`, `anthropic-compatible`, or `gemini-compatible` and defaults to `openai-compatible`. Without `--auth` the command writes no `auth` line, so the type's default credential header applies. `--set-default` writes `[provider] default_provider = "my-gw"` and `[provider] default_model` together. To change an existing name, pass `--overwrite`. For a local endpoint that needs no key, use `--auth none`.

Profiles are added through the CLI and the configuration file. Headless mode and TUI mode read the same profiles, and no provider settings screen exists in either.

Check the new profile right away like this.

```bash
horon --provider-profile my-gw --model my-model run "hello"
```

## Checking readiness

```bash
horon provider current --json
```

The output fields are `requested_provider`, `requested_model`, `resolved_provider`, `selected_model`, `ready`, and `unavailable_reason` when it is not ready (`src/cli/commands/report_info.rs`). `ready` being `true` means the current configuration can call the profile's endpoint.

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `Unknown provider profile '<name>'. Add [providers.<name>] to config.toml.` | The name `default_provider` or `--provider-profile` points at is not in the configuration. Match the table name and value. |
| `Provider profile '<name>' has invalid base_url ...` | `base_url` is empty, is not an absolute URL, or is a plain `http` URL for a host that is not local or private. The message says `https://... or http://localhost`; the full `http` allowance is the local and private host list in the `base_url` row above. Include the API prefix. |
| A missing-key error | The error names the environment variable to fill in. The key value never appears in the error. Export the variable or write it to `env_file`. |
| 401 or 403 | The key is wrong or the `auth` style does not match the endpoint. The `type` default is `bearer` for the two OpenAI kinds and the wire's own header for `anthropic-compatible` and `gemini-compatible`; an explicit `auth = "header"` with `auth_header` overrides both. |
| 404 | `base_url` may be missing a prefix such as `/v1`, or it already carries the wire path and the profile names the wrong `type`. The agent appends only the path of the profile's `type` to `base_url`. |
| Model not found | One of `--model`, the profile `default_model`, or `[provider] default_model` names an id the endpoint does not know. Match the `id` in `[[providers.<name>.models]]`. |

## Built-in profiles

`crates/provider-metadata/src/catalog.rs` holds 43 OpenAI-compatible profiles. Ids such as `openrouter`, `zai`, `kimi`, `cerebras`, `ollama`, `lmstudio`, and `nvidia-nim` can be used directly in `[provider] default_provider`, and any of those ids can also be selected with `--provider <id>`. Each profile defines its own `base_url`, key environment variable, and key file name, and most also define a default model. Profiles without a default model are the gateways (`openrouter`, `orcarouter`, `conifer`, `chutes`), the local runtimes (`ollama`, `lmstudio`), the generic `openai-compatible`, and `openai-api` and `anthropic-api`, and for those you have to pass `--model` or set `[provider] default_model`. If you created a `[providers.<name>]` table yourself, that table's `default_model` plays the same role. `--provider-profile` is the flag that selects a `[providers.<name>]` table, so use it only with names you defined. A table with the same name as a built-in profile loses to the built-in profile, so use a different name.

## Context compaction

A direct provider does not produce provider-specific compaction artifacts. Instead horon-agent's structured summary compaction asks the current model on the same endpoint for a summary, so both threshold auto-compaction and manual `/compact` work (`crates/provider-openai/src/chat/mod.rs` `supports_compaction`, `crates/provider-openai/src/chat/tests.rs` `summary_compaction_is_available_for_the_chat_provider`). Setting `[compaction] summary_model` (environment variable `HORON_AGENT_COMPACTION_SUMMARY_MODEL`) forks the session provider and summarizes with that model, and if that call fails it retries once with the session model (`crates/base/src/compaction/summary.rs`). Provider-specific strategies such as `openai_auto` and `openai_explicit` do not exist on this direct path.

## Cleaning up previous releases

The native plugin bundle directory `~/.horon/native-plugin-bundles/` that earlier releases downloaded is no longer used, so you can delete it yourself.

## Model usage metadata

The shared runtime provides `ModelRoute.usage`. The public API and the Rust SDK expose it as
`ModelRouteInfo.usage: Option<ModelUsage>` through `get_runtime_info()` and the pushed
`RuntimeInfo` event. The existing `list_models()` return type does not change. The
TypeScript SDK exposes the same optional field, `usage`.

```json
{
  "count": 12,
  "last_used_unix_secs": 1789210000,
  "tracking_started_unix_secs": 1789209000,
  "selection_count": 8,
  "last_selected_unix_secs": 1789208000
}
```

### Units and scope

- `count` counts **tracked agent turns with at least one stored assistant response on this route**. A turn is pinned
  to a stored input message. Tool continuations, truncated-answer continuations, and reload continuations each count
  once per route. The session journal preserves turn identity across handoffs and restarts. New input starts a new
  turn. Even if a later request fails or is cancelled, the previously stored response stays counted as usage. This
  value does not measure task success.
- `last_used_unix_secs` is the time of the last recorded stored response. A long turn can move forward without
  raising the count while it runs.
- `tracking_started_unix_secs` marks the coverage of this local ledger. There is no value before tracking starts.
  The count is not a retroactive estimate over the whole history. A count of 0 means no turns were recorded in this
  scope, not that the model was never used.
- `selection_count` and `last_selected_unix_secs` come from `model_picker_usage.json`, a legacy file an earlier TUI
  wrote and the current client no longer writes. These values count picker selections, not model requests. Effort
  variants are summed within one route. This counter is maintained separately.
- No `usage` means the metadata is unknown, and older servers fall into that case too. It must not be displayed as
  "never used".

The ledger is stored in `model-usage-v1.sqlite3` under `HORON_HOME` (usually `~/.horon`).
SQLite serializes concurrent writes. The primary key combines the persistent session/input turn ID with the route
identity, so repeated tool continuations do not raise the count. Reads neither create nor modify the ledger. Runtime
recording is used by both the streaming and blocking paths, which covers the desktop, the CLI, and swarm agents.
Debug sessions and hidden auxiliary model calls are not counted. A serving route that cannot be pinned down is not
guessed. Model fallback is attributed to the model that actually served the response.

Old sessions cannot be filled in exactly. Their messages store token counts but not model/route
identity, and only eight environment snapshots are kept. Neither the session's latest model nor its last
modified time provides an exact attribution of past requests. Opening the picker does not
scan the conversation.

### Ranking and recency

`horon_agent_sdk::compare_model_usage` and the TypeScript `compareModelUsage` compare usage from best to worst by
tracked turn count, last-used time, past selection count, and last-selected time, in that order. Search relevance and
explicit current/favorite policy come before this comparator, and a stable model/route tiebreaker follows it.
The earlier TUI used the same comparator and left the existing current/favorite policy in place.

Legacy protocol clients opt in to `model_usage_updated` delta events with
`get_model_catalog.subscribe_usage_updates: true`. This flag defaults to false, so an older client that cannot decode
the new enum variant does not receive the new event kind. The new API bridge opts in. Each delta carries one route,
so it does not rebuild the whole catalog and avoids the busy Agent lock. The bridge updates its cached catalog and
publishes `RuntimeInfo`. Monotonic merging keeps a late snapshot or another session's event from rolling the count
back within one tracking epoch.

### Deployment

Desktop hot reload alone does not update the daemon. First build and verify the runtime on an isolated socket. After
publishing the verified source build, run
`horon server promote <installed-version>` and `horon server reload --json` during a safe idle period.
Do not use `server stop --force` or send a kill signal. A supported reload checkpoints sessions but still signals
in-progress model generation. If a session is processing, activation is deferred.