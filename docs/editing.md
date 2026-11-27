# Reading and editing files

## Pick the tool

| Change | Tool |
| --- | --- |
| Look at a file or a range | `read` (`start_line`/`end_line`, or `offset`/`limit`; selectors such as `path:40-80`, `path:raw`) |
| List a directory | `ls` |
| Create or overwrite a whole file | `write` |
| Replace exact text in one file | `edit` with `old_string`/`new_string`, or an `edits` array (all apply or none) |
| Same literal or regex replacement in many files | `replace` (all apply or none) |
| Several files or hunks at once, add/delete/move files | `apply_patch` (Codex `*** Begin Patch` envelope or unified diff) |
| Replace line ranges named by anchors | `read` with `hashline: true`, then `hashline_edit` |
| Rewrite by syntax shape | `ast_edit` (ast-grep pattern and template) |
| Take back the last file change | `undo`; `redo` reapplies it |

## `read`

- Text comes with line numbers, at most 64 KiB per call; the result names the next `start_line` when there is more. Default page size is 5,000 lines (`[tools] read_default_limit`).
- Reading a file again: an unchanged range already shown comes back as one line saying so; a whole-file reread of a changed file comes back as a diff. `full: true` always sends the body.
- `read` also opens URLs such as `skill://` and `horon://`, archives, SQLite files, PDFs and images ([read-addresses.md](read-addresses.md)).
- Lines over 2,000 bytes are cut.

## `edit`

| Case | Result |
| --- | --- |
| `old_string` found once | Replaced |
| Found several times | Refused unless `replace_all: true` |
| Not found | Refused with the closest match; nothing changes |
| The file uses CRLF and `old_string` has LF | Matched as CRLF; the file keeps CRLF |
| `edits` given and the shorthand fields are empty strings | The shorthand is ignored |
| `[tools] enforce_seen_line_guard = true` and the lines were not read | Refused until you `read` that range |

## `apply_patch`

1. Every file in the patch is checked before anything is written, against the content earlier hunks leave behind (a file updated twice sees its first update).
2. One hunk that does not apply fails the whole patch: `✗ <path>`, `No files changed: the patch failed preflight.`, verdict `failed`. Nothing is written.
3. Each file keeps its line endings and its final newline or lack of one; in a mixed-ending file, unchanged lines keep their bytes.
4. A write that fails after preflight (I/O error) leaves earlier files written and carries `failed`.

Context and removed lines must match exactly where declared; the applier does not search for a nearby match.

## Hashline edits

`read` with `hashline: true` prints `line#tag` anchors. `hashline_edit` takes `file_path` and `ops` (`start_anchor`, optional `end_anchor`, `new_text`), or `delete: true`, or `move_to: "<path>"`.

| Case | Result |
| --- | --- |
| Any anchor stale (file changed since the read) | Whole call refused, file untouched |
| Spans overlap | Refused |
| Empty `new_text` | Deletes the span |
| `move_to` to an existing path | Refused (use `apply_patch` to replace on purpose) |
| `move_to` or `end_anchor` is `""` | Treated as omitted |
| A `----` anchor (line over 2,000 bytes) | Refused |

## `ast_edit` and `ast_grep`

Metavariables `$A` (one node) and `$$$ARGS` (many) work in every supported language, including Python and Go. Replacing a node keeps the statement terminator after it (`return a - b;` keeps its `;`).

## Undo and redo

- One tool call is one undo unit, even across several files. History is per session and lasts until the process exits; it holds up to 64 MiB, dropping the oldest units.
- `undo` refuses when another process changed the file since; `force: true` overwrites and lists the paths. `redo` checks the same way.
- After a restart there is nothing to undo.

## Guards on every write

- Paths must stay inside the workspace (and `--add-dir` roots), across `..` and symlinks.
- Files marked as generated are refused when `[tools] block_auto_generated = true` (the default).
- Deleting a protected path (the workspace root, home, system folders) is refused ([safety.md](safety.md)).
