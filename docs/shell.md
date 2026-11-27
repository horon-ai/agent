# Running commands

## Pick the tool

| You want | Tool |
| --- | --- |
| Run a command and wait for it | `bash` |
| A long job or a server that keeps running | `bash` with `name` (or `run_in_background: true`), then `bg` |
| Be told when a file changes or a one-off command ends | `monitor` |
| Python with state kept between cells | `eval` |
| A script that calls several tools with loops or filters | `codemode` (JavaScript) |
| Several independent tool calls at once | `batch` |

## `bash`

| Argument | Meaning |
| --- | --- |
| `command` | The shell command |
| `cwd` | Directory for this call; later calls reuse it |
| `timeout` | Milliseconds, default 120,000, max 600,000, `0` disables |
| `run_in_background` | Return at once; the task id is in the result |
| `name` | 1–48 chars of `a-z0-9._-`. Naming always runs in the background and ignores `timeout`. `""` counts as omitted |
| `ready` | For a named service: `log` regex and/or `port` to wait for, `timeout_secs` (default 30) |
| `notify_pattern` | Regex on output lines of a background task; a match notifies the session. `""` counts as omitted |

- Output over 30,000 bytes becomes head, tail and the lines that mention errors, with the full text at an `artifact://` address.
- `HORON_AGENT_SCRATCH_DIR` and `TMPDIR` point at the session's scratch folder; `eval` cells get the same variables.
- A command that looks destructive (for example `rm -rf` of a broad path) goes through the destructive-command gate ([safety.md](safety.md)).
- `[tools] bash_pty = auto` gives commands a terminal when they need one.

## Background tasks: `bg`

| Action | Use |
| --- | --- |
| `wait` (`wait_mode` `any`, `all`, `first_failure`) | Block until tasks finish; prefer this to polling or `sleep` |
| `list`, `status` | What is running |
| `output`, `tail` | Read output so far |
| `cancel` | Stop a task (`task_id` or the `name` of a named service) |
| `watch` / `delivery` / `subscribe` | Get a notice when it ends |
| `cleanup` (`dry_run`, `max_age_hours`), `retention` | Reclaim old task artifacts |

When the artifact budget is full, `HORON_AGENT_JOBS_ARTIFACT_RETENTION=rotate` (default) deletes the oldest; `preserve` refuses new tasks.

## `monitor`

`action: create` with `kind: file` (absolute `path`) or `kind: command` (`command`, `cwd`); `list`, `inspect`, `cancel`, `rearm`. A restored session marks monitors `needs_rearm` instead of restarting processes.

## `eval`

- Needs Python 3.10+ on `PATH`; one cell per call (`code`, `timeout` 1–3600 s, default 30, `reset: true` for a fresh kernel). State persists while the kernel lives and the directory stays the same.
- Each cell is approved like a `bash` command.
- Inside a cell: `await tool.read(file_path="x")`, `await tool.bash(command="...")`; a refusal raises `RuntimeError`.
- Inline output up to 50 KiB; more is kept at an `artifact://` address.

## `codemode`

Runs a sandboxed JavaScript program that can call any tool, including MCP tools, as `await tools.<name>({...})`. A call that fails, or a command that exits non-zero, throws; catch it to continue. Use it to loop over or filter many tool calls in one step.

## `batch`

`tool_calls`: an array of independent calls run in parallel. Do not put calls that depend on each other's results in one batch.
