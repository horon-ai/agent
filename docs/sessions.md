# Sessions

A session is the conversation, its todos, goal, artifacts and runtime journal. It is stored as an append-only event log at `<home>/sessions/<id>/events.jsonl` with large payloads stored by content hash. Compaction, rewind and archive add records; they never delete an original.

## Common tasks

| Need | How |
| --- | --- |
| Continue the latest session | `horon --continue` |
| Continue a specific one | `horon --resume <id>`, or `/resume [id]` in the terminal client |
| See the current session's details | `/session` |
| Start fresh in the same folder | `/new` |
| Branch from an earlier point into a new session | `/fork` |
| Rename | `/rename <title>` or `horon session rename` |
| Hide from the list without deleting | `horon session archive`; bring back with `horon session restore` or `/archived` |
| Delete for good | `horon session delete <id> --yes`, or `/session delete` for the current one |
| A readable transcript | `horon export <id>` (Markdown) or `--html` (offline HTML); secrets redacted, tool results over 4 KiB cut |
| Move a session to another machine | `horon session export <id> --output <dir>` then `horon session import <dir>` (original events and blobs) |
| Check for damage | `horon session inspect <id> [--json]` (read-only), `horon session doctor` |

## Deleting

| Case | Result |
| --- | --- |
| `horon session delete <id> --yes` | Transcript, todos, goal attachment, artifacts and runtime journal are removed |
| A running daemon has the session open | Refused with `session is open` |
| A client just left it | The daemon keeps it open for a reconnect grace (about 30 s); `/session delete` retries in the background until it can delete |
| The session never stored a message | Nothing to remove; reported as deleted |

## What a resumed session contains

- The full stored history, including tool calls and results.
- Older parts that compaction summarized: the model sees the summary; the original stays readable through `read history://current`.
- Undo history does not survive a restart ([editing.md](editing.md)).
- The session keeps its provider profile and model; `/model` changes them.

## Unattended sessions

Sessions with no client attached (swarm workers, scheduled work) keep running in the daemon. When one stops for a decision:

| Situation | Command |
| --- | --- |
| An input with an unknown outcome | `horon runtime-state show --session <id>`, then `resolve-input ... --action retry\|cancel` |
| Waiting for a human approval | `horon approvals resolve --request-id <id> --tool-call-id <id> --verdict retry\|skip` |

## Storage durability

`[storage] sync = "full"` (default) flushes originals so they survive power loss; `fast` survives a crash but not a power loss. Derived caches are rebuilt, not flushed.
