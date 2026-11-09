# External integrations

Language: **English** | [한국어](INTEGRATIONS.ko.md)

Remote MCP servers and custom web search.

## Remote MCP servers

HoronAgent connects to local stdio servers, Streamable HTTP servers, and legacy
HTTP+SSE servers. Existing stdio configuration keeps working.

### Configuration

Remote servers are added to `~/.horon/mcp.json` or a project's `.mcp.json`.

```json
{
  "mcpServers": {
    "remote": {
      "type": "http",
      "url": "https://mcp.example.com/mcp"
    },
    "remote-with-token": {
      "type": "http",
      "url": "https://mcp.example.com/mcp",
      "headers": {
        "Authorization": "Bearer ${MCP_TOKEN}"
      }
    },
    "legacy": {
      "type": "sse",
      "url": "https://mcp.example.com/sse"
    }
  }
}
```

`http` and `streamable-http` select Streamable HTTP. An entry with only a URL and
no command or explicit type selects Streamable HTTP as well. `sse` explicitly
selects the legacy transport. If initialization shows that the endpoint does not
support Streamable HTTP, the HTTP connection can fall back to legacy SSE. Unknown
transport types and remote entries without a URL are not run.

URLs and headers use the same `${VARIABLE}` and `${VARIABLE:-default}`
expansion as stdio configuration. Setting an Authorization header takes precedence
over stored OAuth credentials. Real credentials never go into a
committed configuration file.

HoronAgent also reads supported external MCP configuration sources. When the same
server name has both a local stdio definition and a remote definition,
the existing local definition is kept. Use a different name to
configure a remote server alongside it.

### Commands

```sh
horon mcp list
horon mcp test remote
horon mcp login remote
horon mcp login remote --no-browser
horon mcp logout remote
```

`test` connects to the configured server, initializes the protocol, lists tools, and
exits. It exits with failure if the connection or handshake fails. It does not call an LLM
or execute the server's tools.

`login` is explicit and interactive. It discovers OAuth metadata, registers a
public client when supported, and completes the authorization-code flow
with PKCE and a loopback callback. `--no-browser` prints the authorization URL
instead of opening the system browser. The browser still has to be able to reach
this machine's loopback callback. If the authorization server does not support
dynamic client registration, set an Authorization header.

Tokens are stored in `mcp-oauth.json` in the HoronAgent home directory with restricted
permissions. Tokens are bound to the remote resource scope and
refresh without opening a browser. Background startup and ordinary connection attempts do not
start an interactive login. When credentials are refused or missing, it names
the corresponding explicit login command instead of waiting for a human response.

MCP management tools accept ad-hoc remote connections through `url`, `transport`, and `headers`,
or connect to a named configured server without those fields. `command`, `args`, and `env` are
still the local stdio connection fields.

### Startup, connection waits, and prompt caching

Session tool registration does not wait for remote network traffic. Management,
search, and dispatch tools register immediately. Valid cached tool schemas are
known before background connections start. Each successful connection publishes that
server's tools independently, so a slow server cannot hide a fast peer's tools or
block management and calls for that peer.
An empty live catalog can also replace a stale cached schema instead of announcing
deleted tools on the next start.

The whole remote handshake has a 30-second deadline. It covers credential
refresh, initialization, legacy fallback, and tool listing. Individual tool
requests also default to 30 seconds. `timeout_secs` can raise a legitimately slow
tool request, but it does not raise the overall remote handshake deadline. The first
call to an offline server waits up to 30 seconds for a connection. Cancel and shutdown
remove waiters and stop the response streams they own. No ordinary tool call or
startup path waits for an interactive OAuth login.

There is a separate existing headless behavior. Before the first turn, `horon run`
waits up to 5 seconds for active servers that have no valid cached schema.
With a warm cache it skips that wait. `HORON_AGENT_RUN_MCP_WAIT_MS=0` turns it off, and a smaller
millisecond budget can be set. Inactive servers consume no budget, and the
readiness check uses the exact server identifier rather than a normalized tool-name prefix.

In `auto` and `deferred` MCP modes, connect, disconnect, and tool discovery
keep the immediately registered tool prefixes and the static system prompt stable.
Provider-specific deferred definitions live outside those prefixes. KV cache telemetry now hashes and
counts only the immediate definitions, so a deferred catalog change
does not show up incorrectly as a harness-created cache miss. Explicit `eager` mode with a cold or
changed schema cache still deliberately rebuilds the tool snapshot and
can cause prompt cache misses. A warm cached schema avoids that rebuild when it has not changed.

#### Verification performed on 2026-10-13

- A real TCP test holds back a slow server's initialize response, and during that time
  asserts that registration returns, a fast peer becomes discoverable, and management, search, and
  fast tool calls remain usable. It covers both the shared-connection and owned-connection paths
  and includes a warm-cache restart in explicit eager mode.
- Two-server registration measured 357/241 microseconds cold/warm for the owned connection
  and 126/219 microseconds for the shared connection
  (`justrust` run `20261013-072941134-560607`). These values measure
  registry work inside the test, not full application startup or internet connection latency.
- In a later local run under heavy concurrent CPU load, the owned connection measured 7.159/0.565
  milliseconds cold/warm and the shared connection 15.871/0.521 milliseconds
  (`20261013-074441115-730779`). Registration still returned
  before the held-back network response. Different machines and load
  conditions are not evidence of an implementation speedup or a universal latency ceiling.
- A real native Expo OAuth login succeeded in an isolated HoronAgent home. A native
  initialize/list/shutdown smoke test returned 39 tools in 2.404 seconds. With no credentials, the
  same endpoint returned login guidance in 0.228 seconds without opening a browser.
- Regression tests compare the real agent's immediate schemas, static prompt,
  conversation, cache tracker, and KV monitor before and after HTTP connect,
  disconnect, and reconnect for native and fallback deferred providers. This verifies the
  request prefix relevant to caching, not a paid provider's cache-hit invoice.
- A hosted replay of the pinned final tree passed 113 base MCP tests, 54 app-core MCP
  tests, 79 broader agent tests, 11 root library MCP CLI tests, and a real
  stdio collision integration. Some suites overlap. The earlier local verification
  came after a confirmed hosted filesystem failure and was not an attempt
  to bypass hosted build accounting.
- A freshly built pinned binary passed a ten-case public CLI HTTP/SSE/OAuth
  replay and discovered 39 tools from a real Expo HTTPS MCP service. In an isolated negative control,
  removing five safety fixes made the expected regression assertions
  fail. Restoring them passed the same checks.
  The requirement observation record was cleaned up from the repository, and reproduction uses the tests above.

### Lifecycle and safety

Remote requests preserve the JSON-RPC request ID, the negotiated protocol version,
and the session ID the server assigns. A legacy SSE endpoint must stay on
the configured server's origin. HTTP redirects are not followed with MCP credentials attached.
Shutdown closes pending requests and stops the workers the connection owns.

Use HTTPS for remote servers that require authentication. Loopback HTTP is
acceptable for local development and integration tests. OAuth endpoint discovery and callback
validation enforce secure URLs, resource binding, PKCE, and state validation.

## Custom HTTP JSON web search

Any HTTP endpoint that returns JSON is configured in `~/.horon/config.toml`.

```toml
[websearch]
engine = "custom"
fallback_engines = []

[websearch.custom]
endpoint = "https://search.example.org/api"
method = "GET"
query_param = "q"
count_param = "count"
params = { format = "json" }
headers = { Accept = "application/json" }
header_env = { Authorization = "SEARCH_AUTH" }
results_pointer = "/data/results"
title_pointer = "/title"
url_pointer = "/url"
snippet_pointer = "/snippet"
```

This build has no setting that routes a local engine to provider-native search.
`engine = "native"` remains only as a selectable value and is left out of the search order. So with that value and
an empty `fallback_engines`, search uses the default DuckDuckGo and Bing.
`engine = "custom"` searches only the configured endpoint. The existing DuckDuckGo and Bing
defaults stay the same. On error or an empty result, `fallback_engines` is tried in order.
The default fallback engine is Bing. Set it to `[]` when private queries must not go anywhere else. A user-defined
engine can also serve as a fallback engine.

### Requests

`endpoint` must be an absolute HTTP(S) URL without user information and without a fragment.
Use HTTPS for remote services. Only GET and POST are supported, and the setting is case-insensitive.
The default is GET. `params` are static URL query parameters that apply to both methods and override
endpoint query parameters with the same name. For GET, the query and the optional count become URL
parameters. Dynamic values override endpoint parameters with the same name as the static values. Every
parameter is URL-encoded.

For POST, `body` is a static JSON object (default `{}`). The query goes into the `query_param` slot as a string
and the count into the `count_param` slot as a JSON integer. These two are top-level keys of the body,
not pointers, and they override body keys that already exist. For example:

```toml
[websearch.custom]
endpoint = "https://search.example.org/api"
method = "POST"
query_param = "search"
count_param = "limit"
params = { version = "1" }
body = { options = { safe = true } }
```

`query_param` defaults to `q`. Omit `count_param` to send no count parameter.
When present it must be non-empty and must differ from `query_param`. Results are still limited locally by
`num_results` (tool default 8, maximum 20).

`headers` supplies static header values. For credentials, `header_env` is the better choice. It maps header
names to environment variable names and overrides static headers case-insensitively. The environment variable holds the
**full** value, including `Bearer ` where needed. A missing or empty variable, or a malformed
header value, fails before any network access. Credentials are not copied into diagnostics.
When header authentication is available, do not put credentials in the URL or the body.

Custom requests time out at 30 seconds and do not follow redirects, including same-host
redirects, so credentials are not passed to another destination. Request errors strip the reqwest URL.
HTTP errors show only the status and do not show the response body, the configured URL, or the headers. These
protections do not replace trusting the endpoint you configure. Plain HTTP sends credentials
without encrypting them.

### JSON result mapping

Pointers follow JSON Pointer (RFC 6901). `/` is escaped as `~1` and `~` as `~0`.
`results_pointer` defaults to `/results` and must resolve to an array. For a top-level array, use `""`.
The title, URL, and snippet pointers are relative to each array item and default to `/title`,
`/url`, and `/snippet`. Nested paths such as `/link/href` are supported too. An item whose URL is missing, is not a
string, or is empty is skipped. When the title is missing, not a string, or empty, the URL is used instead;
a snippet that is missing or not a string becomes empty. Broken JSON, a malformed pointer
expression, a missing result mapping, or a mapping that is not an array fails clearly and the configured fallback
engine is used. A valid empty array means no results.