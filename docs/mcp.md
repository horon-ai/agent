# MCP servers

External tools come in over the Model Context Protocol. Horon connects to servers defined in JSON files and exposes their tools through `mcp_search` and `mcp_call` (or directly, when the provider loads deferred tools natively).

## Where servers are defined

The first definition of a name wins, in this order:

| Order | File | On when |
| --- | --- | --- |
| 1 | Project `.horon/mcp.json`, else `.mcp.json`, else `.claude/mcp.json` | The working folder is trusted |
| 2 | `~/.horon/mcp.json` (`<home>/mcp.json`) | Always |
| 3 | `~/.claude.json` (this project, then global) and `~/.claude/mcp.json` | `[mcp] claude_user = true` |
| 4 | `[mcp_servers.*]` in `~/.codex/config.toml` | `[mcp] codex_user = true` |

Each file takes the map under `mcpServers` or `servers`:

```json
{
  "mcpServers": {
    "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] },
    "hub":   { "type": "http", "url": "http://127.0.0.1:3425/mcp/hub" }
  }
}
```

An entry with `url` and no `command` is remote (`type: "http"`, Streamable HTTP with SSE fallback); `headers` sends static headers. `${VAR}` expands from the environment. When a name has both a local and a remote definition, the local one wins.

## Workspace trust

A project-local file is skipped until its digest is trusted, so cloning a repository never starts its servers on its own.

| Need | How |
| --- | --- |
| Trust this folder once | `horon --trust-workspace` (records the digest) |
| Decide for one process | `HORON_AGENT_WORKSPACE_TRUST=trust\|deny` |
| The file changed | Its digest no longer matches; trust again |

## Calling server tools

| Step | Call |
| --- | --- |
| Find a tool | `mcp_search` with a name or purpose; returns callable names and input schemas |
| Call it | `mcp_call` with `server`, `tool`, `arguments` |
| Connect, reconnect, list | `mcp` actions such as `connect`, `reload`, `list` |
| Resources and prompts | `mcp` `resources`, `read_resource`, `prompts`, `get_prompt` |

A connect, reload or list result names at most 20 tools per server and counts the rest; find the others with `mcp_search`.

## Remote OAuth

`horon mcp list` shows servers and local auth state without connecting. `horon mcp test <server>`, `login <server>`, `logout <server>` manage one remote server.

## Problems

| Symptom | Cause |
| --- | --- |
| A project server never appears | The folder is not trusted |
| `HOSTED_POLICY` refusal | A hosted shared worker refuses every MCP server ([hosting.md](hosting.md)) |
| A server asks for `roots/list` and gets an error | `[mcp] expose_roots` is off (default); turning it on tells servers the working directory |
