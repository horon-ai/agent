# Turns

A turn starts with a message and ends when the model answers without calling more tools, or when something stops it.

## How a turn proceeds

1. The request carries the system prompt, the session's history and the tool list fixed for the session.
2. The model streams text and tool calls. Calls in one response are grouped; read-only calls may run together, and results return in request order.
3. Tool results go back to the model; repeat until it stops calling tools.

## When it stops early

| Situation | What Horon does |
| --- | --- |
| Rate limit, context limit, incomplete or empty response | Retries or continues within caps (`[features] turn_recovery = conservative`; `aggressive` or `off` change it) |
| The configured model fails and a fallback is set | Moves to the fallback for this turn and restores the session model later |
| `horon run` stops on a guardrail, `length`, or an empty turn | Exits `1` |
| You want to steer a running turn | Send a message: it is queued and joins the turn at the next boundary without cancelling it (`esc` sends queued messages now) |
| You want to stop it | `ctrl+c` (hard cancel) |
| A failed turn should continue | `/retry` or `alt+r` |

## The loop guard

| Same call (same tool, same arguments) in one turn | Result |
| --- | --- |
| 3rd time | Runs, with a warning |
| 4th time and later | Refused, not run |
| Refused twice for the same signature | The turn ends |
| A repeating cycle of calls | Warned, then refused, then the turn ends |

Change the arguments (another file, another query) or the approach instead of retrying. For `toolbox run` without `args`, see [tools.md](tools.md).

## Context

| Need | How |
| --- | --- |
| Summarize older history in place | `/compact`, or automatic at the threshold; the summary uses the `aux` role model when set |
| Start the next turn from a clean context, keeping notes | `context_notes` to write notes, then `new_context` (needs `[tools] context_notes`) |
| Mark a point and fold back to it | `checkpoint`, then `rewind` with a `report` of what was learned (`dry_run: true` previews without queuing) |
| Read what compaction removed | `read history://current` or `conversation_search` |

Old large tool results are shortened in batches once the prompt cache has gone cold, so the cache is not broken every turn.

## Effort and model switching

- `shift+tab` or `/effort` sets the session's reasoning effort. Prompt words like "think hard" raise it for one turn (`[features] effort_keywords`).
- `[prewalk] enabled = true` with `into = "<model>"` or `"role:<name>"` plans on the session model, then switches once to the execution model.

## Goals and todos

- `todo` keeps the session's task list; `horon run` continues up to 6 extra turns while todos stay unfinished.
- `goal` (`create`, `list`, `show`, `resume`, `update`) keeps a durable objective. An active goal continues the run turn after turn up to a cap; `budget_tokens` stops it when the session's spend reaches the budget.

## Spend limits

`horon run --max-cost <USD>` stops at that estimated spend (an unpriced remote route is refused up front); `--warn-cost <USD>` warns once on stderr.
