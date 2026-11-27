# Messages between agents (`irc`)

`irc` lets sessions in one daemon exchange short messages. It is on by default (`[tools] irc = false` or `HORON_AGENT_IRC=0` removes it) and never touches disk or network. A session run without a daemon (`horon run` alone) gets `IRC is unavailable`.

## Actions

| Action | Arguments | Use |
| --- | --- | --- |
| `list` | — | Agents you can see, with session ids and handles |
| `send` | `to` (session id, unique handle, or `all`), `message`, optional `reply_to`, `progress` | Send a message |
| `wait` | optional `from`, `timeout_secs` (1–300, default 30), `direct_only` | Block until a reply arrives |
| `inbox` | optional `from` | Read waiting messages without blocking |

## Who can see whom

| Relationship | Visible |
| --- | --- |
| Parent and its children, siblings in one swarm | Yes |
| Unrelated root sessions | No, unless they share `HORON_AGENT_SWARM_ID` |
| `to: "all"` | Only from a root session, to members of its swarm |

An ambiguous handle is refused; use the session id. An unknown name says to run `irc action=list`.

## Delivery

| Receiver state | Delivery |
| --- | --- |
| Waiting in `wait` | Handed over at once |
| In a turn | Injected at the next boundary between a model response and a tool round |
| Idle | Woken into a new turn |

Each mailbox holds 100 messages and drops the oldest past that; a message body is capped at 64 KiB. Messages from one sender keep their order.

## Replies

A direct message (not a broadcast) creates a reply obligation: answer it with `send` and `reply_to`. A `progress: true` message reports progress without settling the obligation. When a question cannot be answered, reply saying so rather than staying silent.
