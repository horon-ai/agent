# Vendored ACP v2 schema

The ACP v2 conformance tests validate every frame the agent writes to stdout
against the stable schema released for `schema-v2.0.0-alpha.13`. The copies here
are committed so the test suite needs no network access.

| file | sha256 | bytes | source |
|---|---|---|---|
| `schema-v2.0.0-alpha.13.json` | `b6c91055df549d6f1db1d76d692d0561dad433e03d8ed114ea9b5baed9da72c5` | 341,370 | https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/schema-v2.0.0-alpha.13/schema/v2/schema.json |
| `meta-v2.0.0-alpha.13.json` | `5eac58a28615d399af1db8e3d636767a7e404f2df38c9668a1ba00b1989e0714` | 855 | https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/schema-v2.0.0-alpha.13/schema/v2/meta.json |

- Release tag: `schema-v2.0.0-alpha.13`
- Source commit: `0fc18323a9fdad4e5b4a20ec2f1645dde843fd9b` (agent-client-protocol, the peeled `^{}` commit of that tag)
- Schema dialect: `https://json-schema.org/draft/2020-12/schema`
- Downloaded on 2026-11-07 with
  `gh release download schema-v2.0.0-alpha.13 -R agentclientprotocol/agent-client-protocol -p schema.json -p meta.json`,
  saved byte for byte, without reformatting.

The file vendored here is the STABLE v2 schema, `schema/v2/schema.json`, not
`schema/v2/schema.unstable.json`. The two are different documents at this tag,
and the tests must validate against the stable one.

`meta-v2.0.0-alpha.13.json` lists the wire method names per side
(`agentMethods`, `clientMethods`, `protocolMethods`): 13 agent methods
(`initialize`, `auth/login`, `auth/logout`, `session/new`, `session/list`,
`session/resume`, `session/close`, `session/prompt`, `session/cancel`,
`session/set_config_option`, `session/delete`, `tool_call/cancel`,
`terminal/cancel`), 4 client methods (`session/request_permission`,
`session/update`, `elicitation/create`, `elicitation/complete`), and 1 protocol
method (`$/cancel_request`).

Note for the conformance tool: `acp-tck` validates v2 against its own vendored
unstable alpha.9 schema, so a TCK verdict is not a verdict on the stable
alpha.13 document vendored here.

Verify a copy with:

```sh
shasum -a 256 tests/fixtures/acp/schema-v2.0.0-alpha.13.json
shasum -a 256 tests/fixtures/acp/meta-v2.0.0-alpha.13.json
```
