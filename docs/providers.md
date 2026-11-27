# Providers and models

Every model request goes straight to the endpoint of one provider profile. The profile's `type` picks the wire; nothing relays in between.

## Add a profile

```sh
horon provider add my-gw --type openai-compatible --base-url https://llm.example.com/v1 --model my-model
horon provider current --json     # "ready": true when the profile resolves
```

Or write it in `<home>/config.toml`:

```toml
[provider]
default_provider = "my-gw"

[providers.my-gw]
type = "openai-compatible"
base_url = "https://llm.example.com/v1"
api_key_env = "MY_GW_API_KEY"
default_model = "my-model"

[[providers.my-gw.models]]
id = "my-model"
context_window = 128000
```

| `type` | Request | Default auth header |
| --- | --- | --- |
| `openai-compatible` (default) | `POST {base_url}/chat/completions` | `Authorization: Bearer` |
| `openai-responses` | `POST {base_url}/responses` | `Authorization: Bearer` |
| `anthropic-compatible` | `POST {base_url}/messages` | `x-api-key` |
| `gemini-compatible` | `POST {base_url}/models/{model}:streamGenerateContent?alt=sse` | `x-goog-api-key` |

A `base_url` that already ends with the wire path is used as is. `open-router` is not a type; use `openai-compatible` with that host.

## Profile keys

| Key | Meaning |
| --- | --- |
| `base_url` | Required; include the API prefix such as `/v1` |
| `api_key_env` | Variable holding the key, checked first |
| `env_file` | Key file in the config directory, `NAME=value` lines |
| `auth` | `bearer`, `header` (with `auth_header`), or `none`; omitted means the type default |
| `requires_api_key` | `false` for a keyless local endpoint |
| `default_model` | Model used when none is chosen |
| `[[providers.<name>.models]]` | `id` plus optional `context_window`, `input` (`["text","image"]`), `reasoning`, `reasoning_effort` |
| `headers`, `extra_body` | Extra HTTP headers / JSON merged into every request body |
| `session_header` | Header that carries the session id, for gateways that keep one upstream conversation per session |
| `prompt_cache_key` | `true` always, `false` never, empty only for api.openai.com and openrouter.ai |

A gateway that lists its own models (`/models`) needs no `models` entries: `horon model list --refresh --profile <name>` shows what it serves, including route ids such as `group/<name>`.

## Choosing a model

| Need | How |
| --- | --- |
| One run | `horon run -m <id> ...` or `--provider-profile <name>` |
| The current session (TUI) | `/model` (also `/switch`) |
| A named bundle of model, effort and role models | `[model_presets.<name>]`, then `/modelpreset preview <name>` and `/modelpreset apply <name>` |
| A different model for a workload | Model roles (below) |

A model switch keeps the session on the profile that serves the new model, so a spawned swarm worker inherits the same profile and model.

## Model roles

Roles live inside one profile and never select a second endpoint:

```toml
[providers.my-gw.roles]
plan = "my-planner"
aux = "my-cheap-model"      # compaction summaries, titles, memory extraction
judge = "my-judge"          # one bounded bool/choice/score call
image = "my-image-model"    # registers generate_image
```

`horon run --role plan=<model>` overrides one role for a run.

## Effort

Effort maps per wire: `reasoning_effort` (Chat Completions), `reasoning.effort` (Responses), `output_config.effort` or `thinking.budget_tokens` (Anthropic), `generationConfig.thinkingConfig` (Gemini). In the TUI, `shift+tab` cycles the session's effort and the status line shows the chosen level at once. Words such as "think hard" in a prompt raise effort for that turn when `[features] effort_keywords` is on.

## Built-in profiles

43 OpenAI-compatible profiles are built in (`openrouter`, `zai`, `kimi`, `cerebras`, `ollama`, `lmstudio`, `nvidia-nim`, ...). `horon provider list` names them.

## When it fails

| Symptom | Fix |
| --- | --- |
| `Unknown provider profile '<name>'` | `default_provider` or `--provider-profile` names no `[providers.<name>]` table |
| `invalid base_url` | Use an absolute `https://` URL; plain `http` only for local or private hosts |
| Missing-key error | Export the variable the message names, or write it to `env_file` |
| 401 / 403 | Wrong key or wrong `auth` style for the endpoint |
| 404 | `base_url` lacks `/v1`, or the profile has the wrong `type` |
| Model not found | `-m`, `default_model` or `[provider] default_model` names an id the endpoint does not serve |
| No profile at all | The call stops before the network and names the missing table |

`horon provider diagnose --check` queries model lists for each profile.
