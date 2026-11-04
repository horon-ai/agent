# Vendored ACP v1 schema

The ACP v1 conformance tests validate every frame the agent writes to stdout
against the official schema released for `schema-v1.26.0`. The copies here are
committed so the test suite needs no network access.

| file | sha256 | source |
|---|---|---|
| `schema-v1.26.0.json` | `4bcd1d2051ff7ed379bd9b234fb9b09c5ab9245478698e9fab805bd9b52a33a6` | https://github.com/agentclientprotocol/agent-client-protocol/releases/download/schema-v1.26.0/schema.json |
| `meta-v1.26.0.json` | `061edb6efa8fb2aa2792459a86ec7268de5fe665bba48b2ffe7939df01481f88` | https://github.com/agentclientprotocol/agent-client-protocol/releases/download/schema-v1.26.0/meta.json |

- Release tag: `schema-v1.26.0`
- Source commit: `e2d83349bc9e28a4059ea3acefa1c91c966f74e2` (agent-client-protocol)
- Schema dialect: `https://json-schema.org/draft/2020-12/schema`
- `schema-v1.26.0.json` is 255,170 bytes; `meta-v1.26.0.json` is 1,159 bytes.

`meta-v1.26.0.json` lists the wire method names per side (`agentMethods`,
`clientMethods`, `protocolMethods`). The schema itself carries the `x-method`
and `x-side` annotations that `tests/support/acp_schema.rs` uses to pick the
`$defs` entry for one message, so the validator never hand-copies a table from
the specification.

Verify a copy with:

```sh
shasum -a 256 tests/fixtures/acp/schema-v1.26.0.json
shasum -a 256 tests/fixtures/acp/meta-v1.26.0.json
```

# The scripted provider the turns reach

The isolated test homes run every model turn against a loopback scripted
provider. Each home writes a `[providers.scripted]` profile of
`type = "openai-compatible"` into its `config.toml` and selects it with
`[provider] default_provider = "scripted"`, so the agent calls the provider's
`/v1/chat/completions` directly. No plugin artifact or separate build step is
needed.

`tests/support/lifecycle.rs` serves the scripted endpoint. It streams Chat
Completions SSE chunks: text deltas, `reasoning_content` deltas, tool calls
split across chunks, and a terminal `finish_reason` followed by `[DONE]`. A
turn that holds the stream open omits `Content-Length` so the client can read
the first delta while the turn is still open and cancellation is exercised
rather than raced.

`scripts/test/acp_tck.sh` starts its own Python provider with the same wire
shape and config profile for the external TCK run.
