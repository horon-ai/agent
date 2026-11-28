# Memory

Memory keeps facts, preferences and lessons across sessions. Each memory is one markdown page under `<home>/memory/wiki/` (the source of truth) with a SQLite index beside it that can always be rebuilt. No embeddings, no network.

## Scopes

| Scope | Holds | Visible to |
| --- | --- | --- |
| `project` | Memories of one working folder (keyed by its canonical path) | Sessions in that folder |
| `global` | User-level memories | Every session |
| `all` | Read-only union of both | — |

## The `memory` tool

| Action | Use |
| --- | --- |
| `remember` | Store a new memory (`content`, optional `category`, `tags`, `scope`) |
| `learn` | Store a lesson; `scope: "global"` for user-level; refused with `all` |
| `recall`, `search` | Find memories; `search` matches substrings with case and separators folded (`side-panel` finds `side_panel`) |
| `list` | Newest first |
| `update` | Change `content`, `category` or `tags` of one id in place; the id, creation time and links stay |
| `tag`, `link`, `related` | Organize and follow links between memories |
| `forget` | Delete one memory |

Read one in full with `read memory://<id>`, or list with `read memory://` ([read-addresses.md](read-addresses.md)). `write` and `edit` never change memory.

## When to write a memory

| Write | Do not write |
| --- | --- |
| A preference the user stated | Facts the code or repository already shows |
| A correction the user made, so it is not repeated | One-off task details |
| A procedure that worked and will be needed again | Secrets or credentials |

## Command line

| Command | Purpose |
| --- | --- |
| `horon memory list\|search\|stats` | Inspect stored memories |
| `horon memory export <output> [--scope ...]` | Export to a file; recognizable credentials are masked, file mode `0600` |
| `horon memory import <file>` | Import a JSON file |
| `horon memory migrate [--dry-run]` | Import pre-wiki `memory/*.json` files (they are never modified) |

`horon volume export|import` backs up memory together with the rest of the user's data ([hosting.md](hosting.md)).
