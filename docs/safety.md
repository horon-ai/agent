# Safety and approvals

## Approval mode

`[tools] approval_mode` decides which calls wait for a person.

| Mode | Asks before |
| --- | --- |
| `yolo` (default), `off` | Nothing ordinary; the independent guards below still apply |
| `write` | Calls that can change files (`eval` cells count) |
| `all` | Every call that is not read-only |

In the terminal client a parked call shows a prompt: `y` once, `a` always (for calls like it), `n` deny. `[tools] bash_patterns` rules are checked before the mode; the first match wins. An unattended session that stops for approval is answered with `horon approvals resolve` ([sessions.md](sessions.md)).

## Plan mode

`alt+m` or `/plan on` makes the session read-only while it drafts a plan. Only read-only calls run; writes, shells and network effects are refused with `plan_mode`. Once the plan is approved, effects are allowed again.

## Guards that always apply

| Guard | What it refuses |
| --- | --- |
| Destructive-command gate (`bash`) | Commands with a broad blast radius ask for a reasoned confirmation that a blind retry cannot satisfy; `/`, `$HOME`, credential stores and device nodes are never deleted |
| Protected delete paths | `apply_patch`, `hashline_edit` and other deletes of protected paths |
| Workspace containment | File writes outside the workspace and `--add-dir` roots, including through `..` or symlinks |
| Generated files | Writes to files marked generated (`[tools] block_auto_generated`) |
| Network targets | `webfetch` to loopback or private addresses |
| `git_guard` (opt-in) | `git stash`, `reset --hard` and branch moves in a shared main checkout |

When a guard refuses, change the plan (a narrower path, a different command), do not retry the same call: the loop guard ends the turn after repeated identical refusals ([turns.md](turns.md)).

## Asking the user: `ask`

| Situation | Result |
| --- | --- |
| A client that can answer is attached | Waits up to `timeout_secs` (1–300, default 300) |
| No client attached | Answers at once by `[tools] ask_policy`: `recommended` (default) takes the recommended option; `error` refuses so the model decides alone |
| The wait runs out or the client leaves | Falls back to the policy and says why |

Use `ask` only when the choice cannot be made from the repository, the conversation or memory; set a short `timeout_secs` in unattended runs.

## Secrets

- Transcripts and exports redact recognizable credentials.
- `[tools] secrets_mode`: `off` (default) sends text unchanged; `obfuscate` replaces detected credentials with stable placeholders `«SECRET:<label>:<base>»` toward the provider and restores them for tools; `block` refuses calls that would send a credential outward.
- Never write keys into memory or skills.

## Workspace trust

Project-local MCP configuration does not run until its digest is trusted (`--trust-workspace`, or `HORON_AGENT_WORKSPACE_TRUST=trust|deny`). See [mcp.md](mcp.md).

## Crash bundles

`[features] crash_bundles = true` (default) saves a local bundle when the agent crashes, for a bug report; nothing is uploaded.
