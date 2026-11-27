# Sub-agents (`swarm`)

`swarm` starts child sessions in the daemon, gives them work, and collects their results. `horon run` starts a daemon on demand for its first swarm request.

## Common tasks

| Need | Call |
| --- | --- |
| One child | `swarm` `action: "spawn"` with `label`, `initial_message` (or `prompt`), optional `agent`, `model`, `effort`, `isolation`, `output_schema` |
| Several children in one call | `action: "spawn_many"` with `tasks: [{label, initial_message, ...}]` and optional shared `context`; labels must be unique |
| What is running | `action: "list"` or `"status"`; `/swarm` in the terminal client |
| Wait for children | `action: "await_members"`, or `bg` `wait` |
| Read a finished child's result | `read agent://<id>` or `read history://<id>` |
| Talk to a child | `action: "message"`, `"dm"`, `"broadcast"`, or the `irc` tool ([irc.md](irc.md)) |
| Stop or clean up | `action: "stop"`, `"cleanup"` |
| Named roles available | `action: "list_agent_defs"`, or `horon agents` |
| Models a child can use | `action: "list_models"` |

## Isolation

| `isolation` | The child works in |
| --- | --- |
| `none` | The same folder as the parent |
| `worktree` | Its own git worktree |
| `clone` | Its own clone |

Use `worktree` or `clone` when children edit files in parallel.

## Model a child runs

| Spawn says | Child uses |
| --- | --- |
| Nothing | The parent's provider profile and model, including gateway route ids |
| `model: "<id>"` | That model on the parent's profile |
| `model: "<profile>:<id>"` | That profile and model |

If the inherited model cannot be used, the error names the model and inherited profile and says to pass `model` as `<profile>:<id>`.

## Named agent definitions

A spawn can say `agent: "<name>"` instead of restating a role. Definitions are Markdown files with YAML frontmatter in `<workspace>/.horon/agents/*.md` (first) and `~/.horon/agents/*.md`:

```markdown
---
name: reviewer
description: Reviews a diff for bugs
tools: read, agentgrep, bash
model: my-model
effort: high
---
You review the change you are given and report concrete defects.
```

Explicit spawn arguments override the definition.

## Plans and task graphs

For larger work a coordinator can run a plan: `propose_plan`, `approve_plan`/`reject_plan`, `run_plan`, `plan_status`, `assign_task`/`assign_next`/`fill_slots`, `expand_node`, `complete_node`, `inject_gap`. Children report with `report`; the parent reads `summary`. A hard cap on live members prevents runaway fan-out.

## Rules for the parent

- Give each child a self-contained task: the child does not see the parent's conversation.
- Do not hand out the same files to two children without isolation.
- Verify children's claimed results before reporting them.
