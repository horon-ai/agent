# SDKs and the harness API

`horon api-bridge` (alias `api`) serves the harness API, a framed protocol for programs that create, drive and watch sessions. The Rust SDK (`horon-agent-sdk`), the TypeScript SDK (`packages/sdk/typescript`) and the Python SDK (`packages/sdk/python`) are clients of it. The TypeScript and Python SDKs are not published to npm or PyPI; build them from the repository.

## Where it listens

| Setting | Endpoint |
| --- | --- |
| Default | `horon-agent-api.sock` in the runtime directory (named pipe on Windows) |
| `--api-socket <path>` | Another socket path |
| `--stdio` (Unix) | One connection on stdin/stdout, for SSH clients |
| `--api-bind IP:PORT`, `HORON_AGENT_API_BIND`, `[harness_api] bind` | Also a TCP listener (flag beats environment beats file) |

## Exposing it over TCP

| Rule | Detail |
| --- | --- |
| Every TCP connection authenticates | The first line is `Authorization: Bearer <token>`; issue tokens with `horon api-credentials issue`, and `rotate`, `revoke`, `list` them |
| A non-loopback address needs TLS in front | Start with `--api-insecure-behind-proxy` (or `HORON_AGENT_API_INSECURE_BEHIND_PROXY=1`) only when a TLS-terminating proxy fronts it, and with at least one issued credential |
| Literal addresses only | `127.0.0.1:7946` or `[::1]:7946`; host names are refused; only `127.0.0.0/8` and `::1` count as loopback |
| Lifetime | A bridge with `--api-bind` does not exit with its parent or when idle; stop it with its supervisor |

## Credentials for model calls

There is no interactive login. `set_api_key(provider, key)` stores a key in the owner-only credential store; `notify_auth_changed(provider)` asks the daemon to re-read credential files after another tool logged in. The SDK never issues or refreshes OAuth tokens.

## What a client can do

Create, resume and delete sessions; send messages and soft interrupts; receive streamed events (text, tool calls, usage); read history and runtime info; set the model and effort; manage goals, approvals, packages and the durable runtime journal. The full message list is in the developer reference at https://agent.horon.ai/reference/protocol/harness-api.

| Case | Result |
| --- | --- |
| Delete a session a client is attached to | Refused as open; delete it after detaching (a reconnect grace of about 30 s applies) |
| A session id that stored nothing | `unknown session` |
| SDK `enabled` tool list given | Those tools are sent eagerly; `session_search` and `swarm` stay off unless named |
