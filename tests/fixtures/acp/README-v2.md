# Vendored ACP v2 schema

The ACP v2 conformance tests validate every frame the agent writes to stdout
against the stable schema released for `schema-v2.0.0-alpha.16`. The copies here
are committed so the test suite needs no network access.

| file | sha256 | bytes | source |
|---|---|---|---|
| `schema-v2.0.0-alpha.16.json` | `22a631a896e2df061f64f81f3414949d46f1d25151ceb3f74bda2255ad36773d` | 373,941 | https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/schema-v2.0.0-alpha.16/schema/v2/schema.json |
| `meta-v2.0.0-alpha.16.json` | `5eac58a28615d399af1db8e3d636767a7e404f2df38c9668a1ba00b1989e0714` | 855 | https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/schema-v2.0.0-alpha.16/schema/v2/meta.json |

- Release tag: `schema-v2.0.0-alpha.16`
- Source commit: `13b765828f961812f0c78ae577220e994209fa2f` (agent-client-protocol)
- Schema dialect: `https://json-schema.org/draft/2020-12/schema`
- Downloaded on 2026-11-16 with
  `gh release download schema-v2.0.0-alpha.16 -R agentclientprotocol/agent-client-protocol -p schema.json -p meta.json`,
  saved byte for byte, without reformatting.

The file vendored here is the STABLE v2 schema, `schema/v2/schema.json`, not
`schema/v2/schema.unstable.json`. The two are different documents at this tag,
and the tests must validate against the stable one.

`meta-v2.0.0-alpha.16.json` lists the wire method names per side
(`agentMethods`, `clientMethods`, `protocolMethods`): 13 agent methods
(`initialize`, `auth/login`, `auth/logout`, `session/new`, `session/list`,
`session/resume`, `session/close`, `session/prompt`, `session/cancel`,
`session/set_config_option`, `session/delete`, `tool_call/cancel`,
`terminal/cancel`), 4 client methods (`session/request_permission`,
`session/update`, `elicitation/create`, `elicitation/complete`), and 1 protocol
method (`$/cancel_request`).

Note for the conformance tool: `acp-tck` validates v2 against its own vendored
unstable alpha.9 schema, so a TCK verdict is not a verdict on the stable
alpha.16 document vendored here.

Verify a copy with:

```sh
shasum -a 256 tests/fixtures/acp/schema-v2.0.0-alpha.16.json
shasum -a 256 tests/fixtures/acp/meta-v2.0.0-alpha.16.json
```
