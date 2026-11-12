---
title: Configuration
description: Provider profiles, model roles and the settings whose defaults are part of the contract.
section: Start
order: 3
---

The configuration file is `~/.horon/config.toml` (`<home>/config.toml` when
`HORON_HOME` is set).

## Provider profiles

Endpoints are `[providers.<name>]` tables selected with `[provider]
default_provider` or `--provider-profile`.

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
| `type` | `"openai-compatible"` — the only value the model path uses in this build. |
| `base_url` | Required. The address including the API prefix, e.g. `/v1`. |
| `api_key_env` | Environment variable holding the key, checked before `env_file`. |
| `env_file` | Key file kept in the config directory, one `NAME=value` per line. |
| `auth` | `bearer` (default), `header` (name from `auth_header`, defaults to `api-key`), or `none`. |
| `default_model` | The profile's default model id. |
| `models` | Optional array; `id` is required and `context_window`, `input`, `reasoning`, `reasoning_effort` may accompany it. |
| `headers` | Extra HTTP headers on every request. |
| `extra_body` | Top-level JSON object merged into the request body, for backend-only fields. |
| `requires_api_key` | Set `false` where there is no key, as with a local endpoint. |
| `prompt_cache_key` | `true` always sends a lineage-stable key, `false` never; empty sends it only for `api.openai.com` and `openrouter.ai` hosts. |

The request URL is `base_url` with `/chat/completions` appended, and tool
definitions travel in the same request body as `tools`. Nothing relays between
the agent and the endpoint.

## Model roles

A role maps a workload to a model other than the session's own. Roles are
configuration only; a file without a role table resolves models exactly as
before.

| Role | Workload |
| --- | --- |
| `default` | Every turn unless a more specific role applies. |
| `plan` | Planning turns that produce a plan before execution hands off. |
| `aux` | Cheap auxiliary work: compaction summaries, titles, classification, memory extraction. |
| `judge` | Typed judgments: one bounded call answering a `bool`, `choice`, or `score` question. |
| `image` | Image generation and editing. The only role whose model does not speak Chat Completions, and the only one with no text fallback. |

## Settings whose defaults are the contract

| Setting | Default | Other values |
| --- | --- | --- |
| Tool profile (`--tool-profile`, `HORON_AGENT_TOOL_PROFILE`, `[tools] profile`) | `core` | `full`, `acp`, `minimal`/`lite`, `none` |
| `[acp] profile` | `standard` | `extended`, `full` |
| `[tools] redo`, `hashline_edit` | `false` | `true` |
| `[tools] ask_policy` | `recommended` | `error` |
| `[tools] bash_pty` | `auto` | `off`, `always` |
| `[tools] irc` | `false` | `true`; `HORON_AGENT_IRC` overrides it per process |
| `[features] crash_bundles` | `true` | `false` |
| `[features] turn_recovery` | `conservative` | `aggressive`, `off` |
| `[features] foreign_rules`, `stream_rules` | `false` | `true` |
| `[features] effort_keywords` | `true` | `false` |
| `[prewalk] enabled` | `false` | `true`, which then requires `into` |
| `[storage] sync` | `full` | `fast` |
| `[features] pricing_refresh` | `false` | `true` |
| `[server] idle_hibernation_secs` | `1800` | `0` disables the sweep |
| Update check on `serve` | not run | `--no-update` skips it everywhere |

`[storage] sync` names a durability level, not a syscall: `full` is power-loss
durable (`F_FULLFSYNC` on macOS) and `fast` is the cheapest crash-durable flush.
Originals are confirmed with it; derived caches are rebuilt instead.

`[server] idle_hibernation_secs` is the idle window before the daemon releases a
session's derived caches (the provider projection and the tool-output index). It
writes nothing, so the next turn rebuilds both from resident state without a disk
read. `0` disables the sweep; the daemon checks every ten seconds.

## Environment-only settings

- `HORON_HOME` — data directory, default `~/.horon`.
- `HORON_AGENT_TOOL_PROFILE` — overrides `[tools] profile` for one process.
- `HORON_AGENT_STORAGE_SYNC` — overrides `[storage] sync`; an unknown value falls
  back to `full` with one warning.
- `HORON_AGENT_WORKSPACE_TRUST` — `trust` or `deny`. Project-local MCP config is
  skipped unless its digest was trusted; `--trust-workspace` records the decision.
- `HORON_AGENT_JOBS_ARTIFACT_RETENTION` — `rotate` (default) reclaims the oldest
  background task artifacts when the budget is full; `preserve` refuses to start
  a new task instead.
- `HORON_AGENT_NO_AUTO_UPDATE` — set by the npm launcher so the pinned binaries
  are not replaced by the agent's own updater.
