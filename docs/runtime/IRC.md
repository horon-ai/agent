# The IRC mailbox bus

Language: **English** | [한국어](IRC.ko.md)

This describes how agents inside one `horon-agent` daemon exchange messages through the `irc` tool. It covers the mailbox cap, the waiter FIFO, seq ordering, the reply obligation, the unanswerable verdict, wake-result relay, configuration, and the performance gate.

## The IRC mailbox bus

### At a glance

The `irc` tool is the channel for short conversations between agents. A sender hands over a message with `send`, and the receiver gets it delivered to a waiter immediately, injected at a turn boundary, or woken into a new turn from idle, depending on its state. A received message appears in front of the model as an `<irc …>` envelope, and the envelope's `id` is returned as `reply_to` to answer.

The bus works only inside the process. It uses the daemon's own tokio tasks and session state and never goes through disk or network. The bus core sits in a separate runtime-agnostic crate, `horon-agent-irc-core`, and the server wiring is in `crates/app-core/src/server/irc/`.

### Correspondence with oh-my-pi

oh-my-pi's `IrcBus` (commit 88a69582, https://github.com/can1357/oh-my-pi/tree/88a69582c755bd000e8dc0290981fc787a4f400c/packages/coding-agent/src/irc) is the original contract. What was ported and what changed:

| Contract | Implementation here |
| --- | --- |
| Mailbox cap 100, dropping the oldest on overflow | `IrcLimits` in `crates/irc-core/src/message.rs` and the `enqueue` call in `crates/irc-core/src/bus.rs`, `State::enqueue` in `crates/irc-core/src/mailbox.rs` |
| Fill a waiter first; a successful delivery stays out of the mailbox | `hand_off` in `crates/irc-core/src/bus.rs` |
| A monotonic `seq` per message, ordered by `seq` even within one millisecond | `next_id` in `crates/irc-core/src/bus.rs` and `IrcMessage::seq` in `crates/irc-core/src/message.rs` |
| Allocate a body once and share it across every broadcast leg | the `Arc<str>` body and `body_allocations` counter in `crates/irc-core/src/bus.rs` |
| Track the reply obligation and decide unanswerable | `unanswered_sends`, `can_respond`, `WaitOutcome::Unanswerable` |
| Urgent injection of a direct message from a parent | the `urgent` branch in `crates/app-core/src/server/irc/delivery.rs` |
| Idle wake and turn-result relay | `crates/app-core/src/server/irc/relay.rs` |
| Per-leg receipts | `SendReport`, `Receipt`, `Outcome` |
| Escaping so a body cannot forge prompt structure | `crates/irc-core/src/envelope.rs` |

Six things changed.

- It is a separate `irc` tool rather than a layer on the existing `swarm` tool. Only four actions are exposed: `send`, `wait`, `inbox`, `list`.
- There is a body cap, `max_body_bytes` (default 65536), which the original lacks.
- Broadcast concurrency is limited by `broadcast_concurrency` (default 16).
- Reviving a gone session is replaced by waking an idle one. When no session exists, that leg fails.
- The peer roster appears only through the `list` action. The system prompt carries no peer roster.
- The existing `swarm` completion report does not count as an IRC reply. The reply obligation applies only to direct messages sent by the `irc` tool.

### Addressing and visibility

Recipient `to` takes a session id, a human-readable unique name (handle), or `all`. An `agent://` prefix is stripped before resolution.

- An exactly matching session id selects that member.
- Otherwise a member whose unique name matches exactly is found. Two or more matches are refused with `handle "…" is ambiguous; use the session id`.
- Neither produces `Unknown agent "…" - use irc action=list`.

Visibility holds when both members have a `swarm_id` and it matches, or when one side's `report_back_to_session_id` is the other's session id. A child can see its parent, a parent can see its child, and sessions sharing the same `HORON_AGENT_SWARM_ID` can see each other. Any other combination is refused with `agent "…" is not visible to this session`.

A root session has no parent, so it represents its own swarm. Setting `HORON_AGENT_SWARM_ID` lets several root sessions share one swarm id. Only a root session may use `all`, and the targets are members of the same swarm, excluding the sender, that are not in `stopped`, `failed`, or `completed`. A child session using `all` is refused with `only a root session can broadcast`.

### Delivery path

`send` creates one leg per target and delivers in this order. When a step succeeds, later steps do not run.

| Receiver state | What happens | Receipt |
| --- | --- | --- |
| A waiter is ready to take the message | Handed to the oldest matching waiter as a `oneshot`. Not put in the mailbox and not injected | `injected` |
| External wake mode and a live session exists | Sends only a `WakeRequested` event (reason `irc_delivery`, notification body is the envelope). No soft interrupt and no relay | `woken` |
| Idle session | Starts a new live turn. A direct message schedules a relay | `woken` |
| Busy and the sender is that target's parent | Injected at a turn boundary as an urgent soft interrupt | `injected` |
| Busy and any other relationship | Injected at a turn boundary as a normal soft interrupt | `injected` |
| No live session | Recorded as a failure and that target's bus state is cleared | `failed` |
| The soft interrupt queue refuses | Recorded as a failure and kept in the mailbox | `failed` |

External wake mode is a client setting in which the client wakes the session itself. In that mode a live session receives only `WakeRequested`, with no fallback to a soft interrupt. With no session at all, the failure path above applies.

Parenthood is decided by the target's `Endpoint::parent`, meaning whether its `report_back_to_session_id` equals the sender's session id. Broadcast and `progress` messages are not urgent injections even from a parent.

The injection point is not where a turn ends but the boundary between a model response and a tool round. `turn boundary` in the table above means this boundary, not turn termination. The streaming path injects where a model response ends without tool calls and that response is handled (`handle_streaming_no_tool_calls` in `crates/app-core/src/agent/interrupts.rs`), after a tool round ends and before the next model call (`crates/app-core/src/agent/turn_streaming_mpsc.rs`), and at a tool group boundary when the urgent flag is set. The non-streaming turn loop injects only when a tool round ends (`crates/app-core/src/agent/turn_loops.rs`), so a turn that ends without tools has no injection point. A message that arrives after a busy receiver's turn has finished its last model response and passed the injection check for that response therefore does not enter that turn and stays queued. It is preserved rather than discarded (`crates/app-core/src/server/live_turn.rs`), and the server reports only the remaining count with a `SoftInterruptQueue` event and starts no new turn (`crates/app-core/src/server/client_lifecycle.rs`). On the next turn, injection again happens not at turn start but at the boundary where the first model response is handled. If the first response leads to tool calls, injection happens after that round; if it ends without tool calls, right after that response. The existing `swarm` DM also uses the same `queue_soft_interrupt_for_session` path (`crates/app-core/src/server/client_comm_message.rs`). When the receiver is waiting in `irc wait`, the message goes straight to the waiter as in the first row, so a sender that needs an answer is better off relying on `wait`.

The reason for a failed leg is carried in the receipt. The tool reports success when any leg is delivered, and errors when legs exist but all failed, or when a broadcast has no targets at all. Partial failure is recorded only in the result body, to avoid duplicates from a resend.

### Ordering and the mailbox cap

One `send` produces one message id and a per-leg `seq`. The id and `seq` come from the same monotonic counter, so sends arriving in the same millisecond are still ordered by `seq`. The legs of one broadcast also differ in `seq` and follow the increase.

Consecutive calls from the same sender run one at a time within a round, because the tool's effect class is the default write barrier. Messages from one sender therefore arrive in the order sent. There is no global ordering across different senders.

The mailbox is the queue that holds messages whose live delivery failed. Each target has one `VecDeque`, and past `mailbox_cap` (default 100) the oldest are dropped while `mailbox_dropped` counts them. Messages handed straight to a waiter and messages injected successfully stay out of the mailbox. `inbox` pops oldest first.

When live delivery ends as `Errored`, the same lock looks for a waiter again. A waiter takes the message without it entering the mailbox and the receipt becomes `injected`; only without a waiter does it go into the mailbox and get recorded as `failed`.

When a waiting `wait` is interrupted and its tool task is cancelled, the waiter registration is removed. A message handed over in between but never read is passed to another waiter under the same lock, or put back into the mailbox at its `seq` position. Overflow past the cap again drops the oldest and counts `mailbox_dropped`.

### The reply obligation and wait

The bus records the last `seq` sent between each sender-receiver pair in three tables.

- `last_sent`: a successful send that is not `progress`.
- `last_direct`: a send that is neither a broadcast nor a `wake_relay`. `progress` counts here too.
- `last_progress`: only sends with `progress = true`.

The reply obligation is the pair left in `last_direct` where the other side has not updated `last_sent` since. When the other side sent only `progress`, `last_sent` is unchanged so the obligation remains, and once `last_progress` passes that `seq` a `promised` mark is added. The tool's `wait` uses this obligation to decide whom to wait for.

`wait` decides in this order. ① If the mailbox holds a message matching the filter, pop it and return it immediately. ② With nothing to wait for, `NothingToWaitFor`. ③ If every target has already registered a waiter on me, it resolves immediately as `Unanswerable`. ④ Otherwise register a waiter. After registering: ⑤ if this session has new input pending, `PendingInput`; ⑥ if none of the targets can answer going forward, `NothingToWaitFor`; ⑦ otherwise wait for whichever comes first among arrival, deadline, cancellation, and a 500-millisecond recheck.

`wait` has six outcomes.

| Outcome | Meaning |
| --- | --- |
| `Message` | A message matching the filter arrived, whether from the mailbox or while waiting. |
| `Timeout` | The deadline passed. The tool's default wait is 30 seconds and never exceeds `wait_max_secs`. |
| `NothingToWaitFor` | No reply is owed, or every target that owes one can no longer answer. |
| `Unanswerable` | Every target to wait for is waiting on me. This unblocks one side of a mutual wait to prevent deadlock. |
| `PendingInput` | This session has new input to handle. A user message or another message injected at a step boundary (IRC envelopes included) is the cause. |
| `Cancelled` | The session termination signal ended the wait. |

With no reply obligation it returns `NothingToWaitFor` before even checking pending input. Blocking with no reason to block is not the behaviour.

A waiter resolves the moment something arrives. The 500-millisecond recheck is not a timer period but a secondary check for noticing late that the other side can no longer answer. If an answer arrives first, the waiter finishes as `Message` without waiting for a recheck tick. Waiters that end by timeout or cancellation are removed from the queue. If a message arrived while a waiter was being removed, that message wins.

`can_respond` means that session can still send a message in this process. It is true while the member state is `running` or `spawned`, and also while new input or a relay is pending. It is false for `completed`, `stopped`, or `failed` with no such work.

### Wake result relay

When a turn woken by a direct message to an idle session does not answer directly, that turn's final assistant text is relayed to the sender as a `wake_relay`.

- The relay counts scheduled relays per target and decrements one when the turn ends. If session cleanup already cleared the counter, nothing happens.
- If the target sent a message in the meantime (`last_sent` or `last_progress` above the original message's `seq`), no relay happens.
- The relay body is cut at a UTF-8 boundary so it does not exceed `max_body_bytes`. `reply_to` is the original message id, `progress` is false, and `wake_relay` is true.
- A turn woken by a broadcast, `progress`, or `wake_relay` message schedules no relay. A relay is itself a `wake_relay`, so it produces no further relay. That is the ping-pong guard.
- External wake mode schedules no relay. The result of a client-woken turn does not travel over the IRC path.

### Tool surface

The `irc` tool takes four actions, `send`, `wait`, `inbox`, and `list`.

| Argument | Form | Actions that use it |
| --- | --- | --- |
| `action` | `send` \| `wait` \| `inbox` \| `list`, required | All |
| `intent` | string, a UI label saying why the tool is called | All. It is required in the tool definition the model sees. The `irc` definition source marks only `action` required, and the shared layer adds `intent` to the required list. |
| `to` | string | `send` |
| `message` | string | `send` |
| `reply_to` | integer 0 or more | `send` |
| `progress` | boolean | `send` |
| `from` | string | `wait`, `inbox` |
| `timeout_secs` | integer from 1 to `wait_max_secs`, default 30 | `wait` |
| `direct_only` | boolean, default false | `wait` |

Results per action:

- `send`: the first line is `message {id} sent`, then one `{to}: injected|woken|failed ({reason})` line per target. With targets but every leg failing, that shape becomes a tool error. When a broadcast has no targets and no leg is created, those lines are skipped and only `no recipients: there is no visible agent to broadcast to` is returned as a tool error.
- `wait`: a message result returns the envelope as is. Otherwise one of `no message within {n}s`, `nothing to wait for: no one who owes you a reply can still answer`, `unanswerable: {ids} are waiting on you; send them what they need`, `interrupted: new input is queued for this session`, or `cancelled`.
- `inbox`: stored envelopes oldest first, newline-separated. Empty gives `inbox is empty`.
- `list`: one `{id}\t{handle or -}\t{status}\t{parent or -}` line per target, with a final `unread: {n}` line.

An unknown `action` ends as a tool error listing the available actions. `send` without `to` or without `message` reports that each argument is required.

The tool description tells the model that sending does not block, that the delivery path depends on the receiver's state, that `to` accepts a session id, a unique name, and `all`, that only the same swarm is visible, that intermediate acknowledgement goes through `progress` and does not count as an answer, and that `wait` is for when there is nothing else to do rather than polling.

With the switch off, execution is refused with `IRC is off for this runtime. Set irc = true under [tools] in config.toml.` When running standalone without a daemon and therefore without the global bus, it is refused with `IRC is unavailable: this session is not hosted by a horon-agent daemon`.

### Configuration

Configuration splits into the switch under `[tools]` and the caps under `[irc]`. The default configuration file is `crates/base/src/config/default_file.rs`; the types belong to `crates/config-types/src/irc.rs`.

```toml
[tools]
irc = false

[irc]
mailbox_cap = 100
max_body_bytes = 65536
wait_max_secs = 300
broadcast_concurrency = 16
```

- `[tools] irc` is off by default. Opening a message channel between sessions is why the default is off.
- Turning the switch on adds `irc` to a non-empty allow-list such as `core`, `acp`, `minimal`, or an explicit `enabled` list. An empty list such as profile `none` stays empty, and `full` without an allow-list already exposes every tool.
- With the switch off, execution is refused even in a session without an allow-list such as `full`. That is why the tool registration itself is not filtered out.
- The `HORON_AGENT_IRC` environment variable overrides `[tools] irc`. Its value is read by the same rule as other boolean environment variables, and unrecognized values are ignored.
- All four caps are clamped to a floor of 1, because a 0 would make the mailbox discard every message or a broadcast make no progress.

### Approval and plan mode

`irc` writes no files but does not override `effects()`, so it declares the trait default write barrier. It has no process or network bit. That declaration drives both gates unchanged.

Plan mode refuses write-barrier calls, so `irc` calls are blocked with the `plan_mode` reason in a session with plan mode on. They work again after the plan is approved.

Approval tiers are decided from the tool's effect bits (`crates/app-core/src/approval.rs`). `off` never asks, `write` auto-approves effects that correspond to a file mutation, and `all` auto-approves every call that is not read-only. `irc` has the write bit and no process or network bit, so all three modes classify it as auto-approved. That means `irc` is treated like an ordinary file mutation under `approval_mode = "write"` and raises no approval prompt. It also hits no danger class in `dual_confirm_classes` (`danger_class` in `crates/app-core/src/approval.rs` classifies shell commands only).

### Performance ceilings and the gate

The `irc` section of the structural gate checks on every PR that the bus stays bounded. The measurement is printed by `irc_bus_stays_bounded` in `crates/irc-core/src/perf_tests.rs`, and the gate reads that line.

| Counter | Bound | Meaning |
| --- | --- | --- |
| `mailbox_high_water` | 100 or less | The mailbox never exceeds its cap. |
| `mailbox_dropped` | exactly 900 out of 1000 messages | Only the overflow is dropped, oldest first. |
| `broadcast_body_allocations` | 1 | One 64-target broadcast allocates the body once. |
| `waiters_live_after_churn` | 0 | No waiter remains after timeouts and cancellations. |
| `broadcast_inflight_high_water` | 16 or less | Concurrent broadcast legs never exceed the configured cap. |

The bound values live in `scripts/perf/structural_budget.json`, and the gate implementation is in `scripts/perf/check_structural.py`. To run one section:

```bash
python3 scripts/perf/check_structural.py --only irc
```

Throughput is measured by a separate example. Its three scenarios are direct delivery to a waiter, a fake delivery path that returns an immediate injection, and a 64-target broadcast.

```bash
cargo run --locked --release -p horon-agent-irc-core --example irc_throughput
```

The environment variable `IRC_THROUGHPUT_MESSAGES` changes the message count. The default is 100000.

### Limits

- It works only inside one daemon. Messages do not go to another daemon or a remote host.
- The mailbox is not persisted. Bus state disappears when the daemon ends.
- Removing a session deletes its mailbox, waiters, and reply marks. Messages left in a removed session are not recovered.
- Session ids never change, so renaming a unique name does not affect messages already sent. Names are resolved at send time, so after a rename the old name finds no target.
- There are no channels, topics, subscriptions, or message TTL.
- A session running standalone without a daemon has no bus and the tool is refused.
- `wait` waits for message arrival inside the process, not for a model call. The other side's turn must produce a model response within `timeout_secs`.
- A message that arrives after a busy session's turn has finished its last model response and passed the injection check for that response does not enter that turn and stays queued. It is neither discarded nor a reason to start a new turn, and it is injected only at the first model response boundary of the next turn, so delivery is delayed as long as the receiver does not continue to another turn.