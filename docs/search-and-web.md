# Searching code, the web and past work

## Pick the tool

| You want | Tool |
| --- | --- |
| Lines containing text or a regex | `agentgrep` (default `mode: grep`; `regex: true` for a regex) |
| Files by name | `agentgrep` `mode: find` |
| A file's structure (symbols, headings) | `agentgrep` `mode: outline` with `file` |
| Where code for a plain-language description lives | `agentgrep` `mode: locate` with `query` |
| Relationships between symbols | `agentgrep` `mode: trace` |
| Code by syntax shape (`foo($A)`) | `ast_grep` |
| Definitions, references, hover, symbols, diagnostics, rename, code actions | `lsp` |
| A web search | `websearch` |
| One web page | `webfetch` |
| Something said in an earlier session | `session_search` |
| Something earlier in this conversation (after compaction) | `conversation_search` |
| Horon's own docs | `horon`, then `read horon://<page>` |

## `agentgrep`

- `before`/`after` (up to 20) show context lines; `paths_only: true` lists files only.
- Inside a git repository it honors `.gitignore`; outside one it walks everything, including `node_modules`. Pass `path` and `glob` to narrow it.
- Each output line is capped at 240 bytes (`[line cut]` marks a cut line); the whole result is capped at 48 KiB, with the rest in an `artifact://` address.
- `locate` is local and deterministic; its result starts with `confidence: ok|low|none`.

## `ast_grep`

`pattern`, optional `lang`, and `path` (or `paths`; one string or an array; default the working directory). Metavariables work in Python, Go, Rust, TypeScript and the other supported languages. A search that finds nothing says where it looked and how many files of a supported language it scanned, so a zero from the wrong folder is visible.

## `lsp`

The language server must be on `PATH` and the project's own toolchain must be installed.

| Error tail | Fix |
| --- | --- |
| `Could not find a valid TypeScript installation` | `npm i -D typescript` in the project |
| `Unknown binary 'rust-analyzer'` | `rustup component add rust-analyzer` (the `~/.cargo/bin` file is only a proxy) |
| `pyright-langserver` not found | `npm i -g pyright` |

When a server fails to start, stops responding or times out, the error quotes its last stderr lines (up to 8). `action: status` lists running servers. `horon diagnostics <files>` gives the same diagnostics from the shell.

## `websearch`

- Engines: `duckduckgo`, `bing`, `searxng`, `custom` (`[websearch] custom`, an HTTP JSON endpoint). With no `engine` the configured order is tried.
- `num_results` defaults to 8, at most 20. `bing_market` (or `HORON_AGENT_BING_MARKET`) picks the Bing market.
- A zero-result answer names the engines tried; try another engine before concluding nothing exists.

## `webfetch`

Downloads up to 5 MB and returns at most 40,000 bytes of text, with the full body in an `artifact://` address. Loopback and private addresses are refused.

## `session_search`

Searches earlier sessions; the current session and tool noise are hidden by default. `role` filters `user`, `assistant`, `metadata`; `source` can include other agents' histories (`claude`, `codex`, `pi`, `opencode`, `cursor`). Hits are excerpts; `history://` reads only this session and the sessions it spawned ([read-addresses.md](read-addresses.md)).
