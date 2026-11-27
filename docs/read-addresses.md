# What `read` can open

Besides ordinary files, `read` resolves these addresses. Line selectors (`:40-80`, `:raw`) and `start_line`/`end_line`/`offset`/`limit` work on all of them as on a file.

| Address | Returns | Notes |
| --- | --- | --- |
| `skill://<name>` | The skill's `SKILL.md`, whole, without line numbers, headed by its base directory | An unknown name fails with the list of installed names. See [skills.md](skills.md) |
| `skill://<name>/<path>` | A file inside the skill folder | Paths leaving the folder are refused |
| `horon://` | The list of these documentation pages | |
| `horon://<page>` | One page of this documentation, whole, without line numbers (`horon://tools.md`; `.md` and letter case may be left out) | Built into the binary, so it matches the running version |
| `memory://` | Stored memories, newest first | Only this project's and the global store |
| `memory://<id>` | One memory with its metadata | Change memories with the `memory` tool, never `write` |
| `artifact://<id>` | The full output a tool result was cut from | Only this session's artifacts; kept per session up to 256 MiB, oldest first deleted |
| `history://current` | This session's stored transcript, including what compaction removed from the model's view | Needs `[tools] context_notes = true` (default) |
| `history://<session-id>` | The transcript of a session this one spawned | Not other sessions |
| `agent://<id>` | The result a finished child agent published | See [swarm.md](swarm.md) |
| `<file>.zip`, `.tar`, `.tar.gz`, `.tgz` | Member list; `archive.zip:dir/file.txt` reads one member | Members over 8 MiB, absolute paths and `..` are refused |
| `<file>.db`, `.sqlite`, `.sqlite3`, `.db3` | Tables with row counts; `app.db:users` shows the schema and rows, `?limit=&offset=` pages them | Opened read-only |
| `<file>.pdf` | Extracted text, up to 64 KiB per call | |
| PNG, JPEG, GIF, WebP | Sent to the model as an image when it accepts images | PNG and JPEG are decoded first: a corrupt image is refused with `the image data is corrupt` instead of breaking the conversation |
| BMP, ICO, TIFF | Converted to PNG, then sent | |

## Cases

| Situation | What to do |
| --- | --- |
| A tool result ends with an `artifact://` address | `read` it with a line range for the part you need |
| You need text compaction removed | `read history://current` with a selector, or `conversation_search` |
| A child agent finished | `read agent://<id>` |
| A question about Horon itself | `horon` tool to search, then `read horon://<page>` |
| `history://` refused | `[tools] context_notes` is off |
