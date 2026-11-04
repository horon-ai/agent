# Vendored ACP v2 schema

The ACP v2 conformance tests validate every frame the agent writes to stdout
against the stable schema released for `schema-v2.0.0-alpha.10`. The copies here
are committed so the test suite needs no network access.

| file | sha256 | source |
|---|---|---|
| `schema-v2.0.0-alpha.10.json` | `28c7251e820511da257cdb55d30e2c30bbbe59226305b04e3b751dc655e0778d` | https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/schema-v2.0.0-alpha.10/schema/v2/schema.json |
| `meta-v2.0.0-alpha.10.json` | `ad94c01f2736416776fd53d66e3aaf89242ab72d99832664f39d6ab41e049736` | https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/schema-v2.0.0-alpha.10/schema/v2/meta.json |

- Release tag: `schema-v2.0.0-alpha.10`
- Source commit: `97fcb0f5b88567dd91d585ea6885761365a6aa7b` (agent-client-protocol, the peeled `^{}` commit of that tag)
- Schema dialect: `https://json-schema.org/draft/2020-12/schema`
- `schema-v2.0.0-alpha.10.json` is 328,217 bytes; `meta-v2.0.0-alpha.10.json` is 769 bytes.
- Downloaded on 2026-10-27, byte for byte, without reformatting.

The file vendored here is the STABLE v2 schema, `schema/v2/schema.json`, not
`schema/v2/schema.unstable.json`. The two are different documents at this tag,
and the tests must validate against the stable one.

`meta-v2.0.0-alpha.10.json` lists the wire method names per side
(`agentMethods`, `clientMethods`, `protocolMethods`): 11 agent methods
(`initialize`, `auth/login`, `auth/logout`, `session/new`, `session/list`,
`session/resume`, `session/close`, `session/prompt`, `session/cancel`,
`session/set_config_option`, `session/delete`), 4 client methods
(`session/request_permission`, `session/update`, `elicitation/create`,
`elicitation/complete`), and 1 protocol method (`$/cancel_request`).

Note for the conformance tool: `acp-tck` at commit
`b7f40fb3c003c45926547be5a1f7e2b374035815` validates v2 against its own vendored
UNSTABLE alpha.9 schema (spec commit
`beac4ba96190018e9f6d82e148ff275084c5aa3e`), so a TCK verdict is not a verdict
on the stable alpha.10 document vendored here.

Verify a copy with:

```sh
shasum -a 256 tests/fixtures/acp/schema-v2.0.0-alpha.10.json
shasum -a 256 tests/fixtures/acp/meta-v2.0.0-alpha.10.json
```
