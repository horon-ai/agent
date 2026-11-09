# Tool scheduling and the loop guard

Language: **English** | [한국어](TOOL_SCHEDULING.ko.md)

Two behaviors decide how a message's tool calls run. The scheduler groups the calls
of one model message and runs read-only calls side by side. The loop guard watches
for repetition in the tool call sequence. This document describes the behavior of the
current tree, where both are wired into the turn loop.

## Grouping a round

One model message can carry several tool calls. That round is split into groups in
request order, and each group runs as one unit.

- A call declares what it touches with an effect bitmask. Reads and network may
  overlap; write, append, and process are barriers.
- A call joins the open group while the open group's accumulated effects and the new
  call's effects satisfy `compatible_with`.
- A call that does not satisfy it closes the open group and opens a new one. A call
  that declares a barrier effect therefore never overlaps anything.
- A call whose name is not in the registry is treated as `ToolEffects::write()`. A
  call whose effects are unknown must never run overlapped.

Because this split is a pure function of the per-call effect list
(`crates/app-core/src/agent/tool_round.rs:134` `plan_groups`), grouping can be tested
without starting an agent. Effects are read per call through
`Tool::effects_for_input`, so only calls that use a network request for their input
are counted even for the same tool (`tool_round.rs:182` `classify_call`). A
session-scoped SDK custom tool is forced to a barrier even when its name matches a
built-in alias, because that call runs a callback rather than the registry tool of the
same name.

Both turn loops call the same splitter. The non-streaming path uses
`plan_tool_round_groups`
(`crates/app-core/src/agent/turn_loops.rs:53` `plan_tool_round_groups`), and the
streaming daemon path calls the same helper
(`crates/app-core/src/agent/turn_streaming_mpsc.rs:1668` `plan_tool_round_groups`), so
grouping rules, caps, and ordering cannot drift between the two.

## Which tools may overlap

The tool itself decides its effects. `Tool::effects` defaults to the barrier
`ToolEffects::write()` (`crates/tool-core/src/lib.rs:255` `Tool::effects`), and a tool
whose effects depend on its input overrides `Tool::effects_for_input`
(`crates/tool-core/src/lib.rs:260` `Tool::effects_for_input`). The concurrency class
is derived from these effects (`crates/tool-core/src/lib.rs:270`
`Tool::concurrency_class`), so the scheduler uses effects directly instead of going
through the class. The barrier default is deliberate. Two overlapping calls that write
the same file can lose user data, while an unnecessary barrier only costs speed.

The tools that declare effects are below.

| Tool | Source |
| --- | --- |
| `read` | `crates/app-core/src/tool/read.rs:131` `effects` |
| `read` (remote path) | `crates/app-core/src/tool/read.rs:135` `effects_for_input` |
| `ls` | `crates/app-core/src/tool/ls.rs:56` `effects` |
| `agentgrep` | `crates/app-core/src/tool/agentgrep.rs:190` `effects` |
| `webfetch` | `crates/app-core/src/tool/webfetch.rs:51` `effects` |
| `websearch` | `crates/app-core/src/tool/websearch.rs:56` `effects` |
| `bash` | `crates/app-core/src/tool/bash.rs:829` `effects` |

The rest stay at the barrier default, namely `write`, `edit`, `replace`,
`apply_patch`, `batch`, `mcp`, and `skill_manage`. Registry lookup resolves provider
aliases and answers `ToolEffects::write()` for a name it does not know
(`crates/app-core/src/tool/mod.rs:1038` `Registry::effects_for_call`).

## Two caps per group

A group stops at either cap, and the two caps are independent
(`crates/app-core/src/agent/tool_round.rs ~:12-18`).

| Cap | Value | Source |
| --- | --- | --- |
| members of one group | 10 | `tool_round.rs:45` `MAX_GROUP_MEMBERS` |
| network members of one group | 4 | `tool_round.rs:52` `MAX_NETWORK_MEMBERS` |

A network member is a call whose effects carry the `NETWORK` bit. No tool-name list is
involved, so a call that uses a network request for its input (a `read` of a remote
path) is counted only for that call. The cap is checked against the accumulated
effects and the accumulated count while the group is open (`tool_round.rs:143`
`active.compatible_with`). A mixed group therefore stops at four network calls even
when a local read would still fit, because every network call spends a paid API
request.

## Results arrive in request order

Group members run at the same time and finish in completion order, but results are
applied by their index within the round. `run_group` sorts the collected results by
index before returning (`tool_round.rs:348` `results.sort_by_key`), and both loops
apply group members in request order, so terminal events, stored durations, and
history messages follow the model's order (`turn_loops.rs:1543` `results.remove`,
`turn_streaming_mpsc.rs:894` `results.insert`). A provider pairs each `tool_use` with
its `tool_result` by id, so a reordered round corrupts the transcript.

Two failure rules hold inside a group.

- Each member runs in its own task, so a panicking tool becomes the error result for
  that call instead of losing the whole group (`tool_round.rs:282` `tokio::spawn`).
- On cancellation the group keeps the results it already has and reports the indexes
  that produced no result. That way the caller writes a skipped result for each of
  those indexes and keeps one `tool_result` per `tool_use` (`tool_round.rs:354`
  `unstarted`).

## Context budget

A tool that ran passes its output through the tool layer's overflow guard
(`crates/app-core/src/tool/mod.rs:1415` `self.guard_context_overflow`, the checks
inside `guard_context_overflow`, `tool/mod.rs:1415` `guard_context_overflow`). That
guard compares the output with `token_budget()` times `CONTEXT_GUARD_THRESHOLD` (0.90,
`tool/mod.rs:1048` `CONTEXT_GUARD_THRESHOLD`, the comparison is `tool/mod.rs:1461`
`needs_truncation`).

The guard reads the compaction manager it holds. `Registry::clone` swaps that manager
for a new one, while `clone_sharing_compaction` shares the parent's manager
(`tool/mod.rs:511` `CompactionManager::new`, `tool/mod.rs:531`
`clone_sharing_compaction`). So the guard sees the real session budget and token count
rather than the floor a fresh manager would report. Both loops pass the sharing clone
into the round that executes tools (`turn_loops.rs:1487` `run_group`,
`turn_streaming_mpsc.rs:1862` `clone_sharing_compaction`, `turn_streaming_mpsc.rs:1658`
`run_group`).

The round also keeps a counter of its own, because the tool-layer guard cannot see the
round it belongs to. `effective_token_count()` computes `max(token_estimate,
observed)` (`crates/base/src/compaction/mod.rs:1554` `effective_token_count`), the
estimate is taken with zero messages (`crates/base/src/compaction/mod.rs:1535`
`token_estimate`), and `observed_input_tokens` moves only after a provider response
comes back (`crates/base/src/compaction/mod.rs:1540`
`update_observed_input_tokens`). So re-reading that counter in the middle of a round
does not show outputs already applied earlier in the same round. Several outputs can
each pass their own guard and still exceed the window together.

`RoundTokenBudget` closes that gap. The round seeds one counter from the agent's own
compaction manager and the session messages
(`crates/app-core/src/agent/round_budget.rs:62` `RoundTokenBudget::seed`) and advances
it by the tokens of what goes back to the model. Admitted output, the refusal put in
its place, and truncated text are all charged (`round_budget.rs:142`
`RoundTokenBudget::admit`). Both loops build it once per round and pass it to every
group, so later groups are charged for what earlier groups already spent
(`turn_loops.rs:1501` `RoundTokenBudget::seed`, `turn_streaming_mpsc.rs:1677`
`RoundTokenBudget::seed`). The round applies the tool layer's rules to its own running
total (`round_budget.rs:160` `projected_tokens`). Output that fits is admitted, and
output that does not is withheld when the caller did not ask to pay and truncated when
it did (`round_budget.rs:171` `single_output_max_tokens`).

Caps, truncation notices, window-exhaustion refusals, and oversized refusals come from
the tool layer rather than copies. `Registry::context_guard_threshold_tokens`
(`tool/mod.rs:1053` `context_guard_threshold_tokens`) turns the budget into the 0.90
ceiling, and `Registry::truncated_output_notice` (`tool/mod.rs:1086`
`truncated_output_notice`) writes the sentence appended after a truncated head. The
round enforces both caps itself instead of leaving the per-output cap to the tool
layer. `admit` returns output only when the projected total fits and this output is
inside the single-output cap (`round_budget.rs:178` `projected_tokens`). So output the
tool layer would refuse as oversized is refused here too, and the cumulative case is
added on top: every member passed on its own, but the sum does not fit.

The round refuses with three different sentences because it handles three different
cases. When the window is already spent (`current >= threshold`) there is no room, so
the round rewrites `Registry::exhausted_context_refusal` (`tool/mod.rs:1069`
`exhausted_context_refusal`) with the round's running total. That sentence says
`accept_large_output` cannot help, which is true in that situation. When room is left
but this output does not fit whole, `ceiling_bound` (`round_budget.rs:211`
`ceiling_bound`) chooses which sentence states the real reason. When the single-output
cap is the stricter of the two (`single_max_tokens < remaining_tokens`), the round
prints the tool layer's `Registry::oversized_output_refusal` (`tool/mod.rs:1242`
`oversized_output_refusal`), so the caller cannot tell which layer refused. Otherwise
the round writes its own `withheld_output_refusal` (`round_budget.rs:290`
`withheld_output_refusal`): the same "CONTEXT LIMIT REACHED" prefix and `/compact`
prescription, but with the `accept_large_output` escape hatch and the number of tokens
that would still fit, because that flag buys the head next round. A caller that
already set the flag is not refused at this point and gets its output truncated to the
stricter of the two caps (`round_budget.rs:171` `single_output_max_tokens`).

## The loop guard

The guard keeps the current turn's recent calls in a rolling window (64 records,
`crates/app-core/src/agent/loop_guard.rs:84` `WINDOW_CAPACITY`) and answers one
question per dispatch decision.

There is one escalation ladder and it is written in one place (`loop_guard.rs:301`
`LoopGuard::record_and_decide`).

| Run length | Verdict | Source |
| --- | --- | --- |
| the same run 3 times | warn, then run the call | `loop_guard.rs:87` `IDENTICAL_WARN_AT`, `loop_guard.rs:344` `identical_warning_text` |
| the same run 4 or more times | refuse and do not run the call | `loop_guard.rs:90` `IDENTICAL_REFUSE_AT`, `loop_guard.rs:341` `refusal_text` |
| one tool of one signature refused twice | stop and end the turn | `loop_guard.rs:97` `STOP_AFTER_REFUSALS`, `loop_guard.rs:339` `stop_text` |
| first cycle detection | warn, then run the call | `loop_guard.rs:367` `cycle_warning_text` |
| the same cycle detected again | refuse and do not run the call | `loop_guard.rs:379` `cycle_refusal_text` |
| the same cycle refused twice | stop and end the turn | `loop_guard.rs:377` `cycle_stop_text` |

A run is the same tool with the same input. `identical_run` compares the tool name and
the signature together (`loop_guard.rs:455` `identical_run`), and the refusal counter
keys on both (`loop_guard.rs:339` `refusals.entry`). So `read`, then
`lsp_diagnostics`, then `read` in one path is three calls and not one run. One tool's
refusals do not raise another tool's ladder either.

A refused call is recorded too, so repeating the same input after a refusal climbs the
ladder instead of starting over (`loop_guard.rs:301` `LoopGuard::record_and_decide`).
Comparison uses the normalized signature. Object keys are sorted and the `intent` field
is dropped, so two calls that differ only in the stated reason count as the same
(`loop_guard.rs:455` `identical_run`, the `intent` skip is `loop_guard.rs:531`
`skip_intent`).

The guard is consulted once per dispatch decision and never inside a running group
(`crates/app-core/src/agent/loop_guard.rs ~:8-16`). One group is one decision. Every
member that reaches a tool is recorded before it starts, so a group holding four
identical `read` calls reaches length 4 at that decision, the fourth member is refused
on the spot, and the first three run unchanged. Invalid calls and calls the SDK
answered never reach a tool, so they are not recorded and not judged. The dispatch
boundary belongs to the caller, and the module knows nothing about concurrency.

Identical runs and cycles climb the same ladder. The cycle detector triggers when a
cycle of 2 to 6 calls repeats three times (`loop_guard.rs:468` `cycle_run`), and the
first detection only warns (`loop_guard.rs:367` `cycle_warning_text`). After that the
same cycle is refused and then stopped (`loop_guard.rs:379` `cycle_refusal_text`). A
warning that repeats forever is not a stop, and a daemon stuck in
`read a.rs -> bash -> read a.rs` would, if left alone, pile up one notification per
dispatch and never end. A cycle's identity is its member tool names and signatures
reduced to the lexicographically smallest rotation (`loop_guard.rs:423` `cycle_key`),
so `read -> bash` and `bash -> read` are one loop and a phase rotation does not restart
the ladder. Ladder state lives in one `Option<CycleLadder>` slot (`loop_guard.rs:262`
`cycle_ladder`, the struct is `loop_guard.rs:275` `CycleLadder`) and is dropped the
moment the tail is no longer that cycle (`loop_guard.rs:327` `cycle_ladder`), so
changing one step's input clears the refusal.

The cycle detector skips a rotation whose members all target different identities, so
work that alternates between different files is never warned about (`loop_guard.rs:492`
`every_member_has_a_target`). That exception is narrower than it sounds.
`target_identity` exists only for `read`, `lsp_diagnostics`, `bash_output`,
`task_output`, and `bg` (`loop_guard.rs:568` `target_identity`), and every other tool
answers `None`, so `every_member_has_a_target` is false and the cycle is not exempt.
Cycling `ls`, `webfetch`, or `agentgrep` three times between two targets therefore does
trigger. Keeping the set narrow is deliberate. A tool that cannot read its target from
the input is exactly what a real two-target loop over that tool looks like, so widening
the exception weakens the guard for that loop. There is no "similar" rung. A run that
differs only in arguments from the latest call is ordinary work (the next page, the
next file), and a run whose signature also matches is already the same run. That is why
the module has no similarity score (`crates/app-core/src/agent/loop_guard.rs ~:55-59`).

### Polling skips the refusal rungs, and waiting is capped

One class of call legitimately has constant input: reading a resource that changes on
its own. `bg`'s status, wait, output, tail, and list actions, the `bash_output` and
`task_output` readers, and stateful remote tools reached through `mcp_call` (or a
native `mcp__<server>__<tool>` call) ask the same way every time while the answer
changes. Such a call is classified once per record (`loop_guard.rs:614` `polling_call`),
and the identical-run ladder never refuses any of them. A run of the same poll leaves
exactly one notification at the warning rung (`loop_guard.rs:685`
`polling_warning_text`, `loop_guard.rs:660` `poll_verdict`), and later calls of that
run all execute up to the separate cap described below. A cycle whose members are all
polling is handled the same way (`loop_guard.rs:691` `polling_cycle_warning_text`,
`loop_guard.rs:378` `LoopGuardVerdict::Allow`). Refusing such a call would end the turn
that is waiting for the very result it asked for, so the ladder never refuses.

Waiting is bounded by a second net rather than the ladder. Consecutive polls are
counted against a cap of 24 (`loop_guard.rs:109` `POLLING_CALL_CAP`), and the unit
counted is deliberately coarse: it counts polls, not the same poll, because changing
one field on each status call still spends a whole request. The first 24 polls of a run
execute, the 25th is not dispatched, and the turn ends there
(`loop_guard.rs:275` `consecutive_polls`, `loop_guard.rs:673` `poll_cap_stop_text`).
The cap never refuses a call. It stops the turn at the 25th poll, and neither the
identical-run ladder nor an all-polling cycle refuses. (A poll that is a member of a
cycle containing a non-polling call can be refused as part of that cycle, which is the
ordinary ladder working.) The stopped call is not executed, and its tool result carries
the stop notice. Every later call in the round is written as a
`[Skipped: the loop guard ended the turn]` result marked as an error, so the transcript
stays balanced (`turn_streaming_mpsc.rs:2263` `LOOP_GUARD_SKIP`). A non-polling call
that actually ran finishes, and the turn boundary clears the count
(`loop_guard.rs:293` `consecutive_polls`), so the next turn starts with the full
allowance. Invalid calls and SDK-answered calls never reach the guard, because both
loops remove them from the dispatch set before consulting it (`turn_loops.rs:1567`
`RoundMember::Execute`, `turn_streaming_mpsc.rs:1818` `consult_loop_guard`), and
neither breaks the run.

On the streaming daemon path the stop is reported as
`TurnStopReason::LimitReached` (`turn_streaming_mpsc.rs:55`
`TurnStopReason::LimitReached`). That is the reason the wire protocol already carries
for "some limit ended this turn" (the non-streaming path records the same notice and
returns without a stop reason). Nothing in the harness restarts that turn. The stop
ends the turn and leaves the next move to the session owner. The SDK's retry policy
gives up on that reason (`crates/agent-sdk/src/turn_retry.rs:407`
`TurnStopReason::LimitReached`), and retries are scheduled only from error events,
which a loop guard stop does not emit (`crates/agent-sdk/src/turn_retry.rs:393`
`observe_error`). The notice therefore says plainly that the client or daemon owning
the session must start the next turn, and nothing inside a stopped turn can expect a
resume. What a new turn gets is a new allowance, because the guard is initialized on
every turn entry. The `/overnight` supervisor (`overnight.rs:374` `run_turn_monitored`)
treats a stopped turn as a completed turn and starts the next one from the continuation
prompt (`overnight.rs:368` `build_continuation_prompt`); that turn reaches `run_turn`
through `run_once_capture` (`turn_execution.rs:108` `run_turn`) and resets the guard on
entry (`turn_loops.rs:426` `loop_guard.reset`). The cap therefore bounds turns, not the
run. On an unattended daemon with no such supervisor, the wait is orphaned until an
operator starts one. That is the intended trade: a finite stop an operator can see
beats an infinite loop nobody sees.

Classification is deliberately narrow so the exception does not become a hole in real
work. `bg` counts only the actions listed above. `cancel`, `cleanup`, `watch`,
`delivery`, and `subscribe` change state and keep the full ladder. The last three all
take one branch of the tool, and that branch rewrites the job's delivery settings
through `update_delivery` on every call and re-arms the stall watchdog when
`stall_wake_seconds` is given, so repeating one is a write loop rather than a wait. A
remote tool is a poll when its name has a state word and no change word, so
`job_status` and `get_job` are polls while `write_file`, `create_job`, and `delete_job`
are not (`loop_guard.rs:641` `names_a_poll`). Everything else, including `read`, `ls`,
`agentgrep`, `bash`, `write`, and `edit`, is unaffected by this rule and keeps the full
ladder.

State lives in memory for the current turn (`loop_guard.rs:248` `LoopGuard`) and
`reset` clears it (`loop_guard.rs:289` `LoopGuard::reset`). It drops the window, the
identical-run refusal counts, and the cycle ladder at once. Both turn entry points call
it, so a loop the model broke in the previous turn starts clean in this one
(`turn_loops.rs:426` `loop_guard.reset`, `turn_streaming_mpsc.rs:67` `loop_guard.reset`).
Nothing is written to disk, and the guard never changes the tool input it receives.

The guard is wired into both loops through `consult_loop_guard` (`turn_loops.rs:79`
`consult_loop_guard`). That function records every call of one dispatch decision and
returns one verdict per call. The `[features]` toggle is checked here too. With
`loop_guard = false` no call is recorded and every verdict is `Allow`
(`turn_loops.rs:86` `features.loop_guard`). The non-streaming loop produces one
decision per group before dispatch (`turn_loops.rs:71` `consult_loop_guard`). The
streaming loop produces one decision before execution for a one-member group and before
any member starts for a multi-member group (`turn_streaming_mpsc.rs:1818`
`consult_loop_guard`, `turn_streaming_mpsc.rs:1818` `consult_loop_guard`).

A refused call leaves the dispatch set but still leaves one terminal event and one tool
result at its index (`turn_loops.rs:1592` `RoundMember::Blocked`,
`turn_streaming_mpsc.rs:1823` `write_loop_guard_result`). A stop writes the remaining
results, records the notice, and ends the turn (`turn_loops.rs:396`
`record_loop_guard_stop`, `turn_streaming_mpsc.rs:1714` `finish_loop_guard_stop`).

### What the guard does not flag

- Paginated reads of one file. A `read` target identity includes the requested range
  fields, so reading one file five times at different offsets is five different targets
  (`loop_guard.rs:568` `target_identity`).
- Alternating reads of two files. A cycle whose members each target a different
  identity is skipped (`loop_guard.rs:492` `every_member_has_a_target`), and `read` is
  one of the five tools with a target identity (`loop_guard.rs:568` `target_identity`).
  Alternating between two targets of `ls`, `webfetch`, or `agentgrep` does not qualify
  and does trigger. See the cycle paragraph above.
- Reads of different paths. Different targets, as above.
- Two tools on one path. A run is one tool with one input, so alternating `read` and
  `lsp_diagnostics` on one file is not warned (`loop_guard.rs:455` `identical_run`).
- Repeated polling of a resource that changes on its own. The ladder warns once and
  neither refuses nor stops. The separate cap ends the turn only after 24 consecutive
  polls (see "Polling skips the refusal rungs, and waiting is capped").
- A different signature after a refusal. Neither refused nor stopped
  (`loop_guard_tests.rs:54` `a_different_signature_after_a_refusal_does_not_stop`).

The negative cases are pinned by tests: `paginated_reads_of_one_file_are_clean`,
`paginating_one_file_in_repeating_pages_is_clean`,
`alternating_reads_of_two_files_are_clean`, `reads_of_distinct_paths_are_clean`
(`crates/app-core/src/agent/loop_guard_tests.rs:129`
`paginated_reads_of_one_file_are_clean`).

## Toggles

Both toggles live in `[features]`, and that section lives in `~/.horon/config.toml`.
Both default to `true`.

| Key | Default | Source |
| --- | --- | --- |
| `parallel_tools` | `true` | `crates/config-types/src/lib.rs:1327` `parallel_tools`, `crates/config-types/src/lib.rs:1367` `parallel_tools` |
| `loop_guard` | `true` | `crates/config-types/src/lib.rs:1330` `loop_guard`, `crates/config-types/src/lib.rs:1368` `loop_guard` |

Turn either one off in the config file.

```toml
[features]
parallel_tools = false
loop_guard = false
```

That section is `[features]` in the config file (`crates/base/src/config.rs:524`
`features`). Neither key has an environment override, an unrelated `[features]` key
does not move the two defaults, and a parsing test pins that
(`crates/base/src/config_tests.rs:237`
`parallel_tools_and_loop_guard_feature_defaults_and_parse`).

With `parallel_tools = false` every group has exactly one member, so a round runs the
same way it did before concurrency existed (`turn_loops.rs:53` `plan_tool_round_groups`).
Both loops read the flag from the loaded config (`turn_loops.rs:1489`
`features.parallel_tools`, `turn_streaming_mpsc.rs:1667` `features.parallel_tools`).
`loop_guard` is read in `consult_loop_guard`, which both loops call (`turn_loops.rs:86`
`features.loop_guard`).

## Limits

### Moving a running call to the background while a group runs

A background tool request (the signal is named Alt+B/Ctrl+B in its own comment,
`crates/app-core/src/server/state.rs:15` `background-tool`) carries no target identity,
and the running tool registry holds one entry per session, so it can point at only one
waiting call. A multi-member group registers no entry (`turn_streaming_mpsc.rs:1655`
`running_tool_registry`), so a request raised while the group runs reports
`moved: false` instead of moving a call (`turn_streaming_mpsc.rs:1658` `run_group`).
The response is computed from the registry entry and the signal
(`crates/app-core/src/server/client_lifecycle.rs:1299` `move_tool_to_background`), and
the event shape carries `moved` without `tool_call_id` or `tool_name`
(`crates/protocol/src/wire/events.rs:298` `BackgroundToolResult`).

The signal is cleared behind the group, so a signal that arrives during a group cannot
move the next one-member call (`turn_streaming_mpsc.rs:1872`
`background_tool_signal.reset`).

A one-member group takes the inline path, and that path registers the waiting call, so
it can still move to the background (`turn_streaming_mpsc.rs:1647`
`running_tool_registry::register`). That covers every `Ordered` call and every call
while `parallel_tools` is off, as long as the call reaches a tool. Invalid calls and
SDK-answered calls resolve without registering.

### Interrupts during a round

The two loops do not respond to a stop the same way, and the difference is deliberate.

`run_turn` has no mid-round interrupt. Its only soft interrupt point runs after the
whole round (`turn_loops.rs:1486` `inject_soft_interrupts`), and the scheduler is called
without a cancel signal: `turn_loops.rs:1487` `run_group` is passed `None`, and the loop
reads `parallel_tools` just above (`turn_loops.rs:1489` `features.parallel_tools`). A
stop that arrives while a group runs is applied after the group finishes, not during it.

The streaming loop responds in two places inside a round. It reads the queue directly
before each group after the first (`turn_streaming_mpsc.rs:1680`
`has_urgent_interrupt`). While a multi-member group runs, it passes a cancel that
watches both the shutdown signal and the interrupt queue to `run_group`
(`turn_streaming_mpsc.rs:2440` `watching_interrupt`, `tool_round.rs:74`
`GroupCancel::new`). That watch is a predicate over the queue, and while members are
executing the group re-reads it every `INTERRUPT_POLL_INTERVAL` (`tool_round.rs:58`
`INTERRUPT_POLL_INTERVAL`, `tool_round.rs:393` `poll_for_interrupt`), so a stop aborts
unfinished members instead of running every paid request to completion. 25 ms is the
polling interval, not a guaranteed delay: a stop is noticed on the next tick, and a
member that finished inside that window reports its result as usual.

Polling exists because the queue cannot wake anything. The soft interrupt queue is an
`Arc<Mutex<Vec<SoftInterruptMessage>>>` (`crates/agent-runtime/src/lib.rs:51`
`SoftInterruptQueue`), a plain mutex around a vector with nothing to wait on. Adding a
notification would mean changing that alias into a struct with one more field, and that
change touches a type every producer and consumer of the queue shares in
`horon-agent-agent-runtime`, all for a wait only running tool groups need. Polling
leaves that shared type alone.

Both entry points end the same way. The group boundary check and the mid-group interrupt
both finish through `finish_streamed_round_interrupt` (`turn_streaming_mpsc.rs:1682`
`finish_streamed_round_interrupt`), so the transcript, client events, and stored session
are the same whichever one noticed the stop.

### Hooks

Three waiting hooks live inside `Registry::execute`, and one process-wide async mutex
serializes them. The input transformer can change a tool input before anything reads it
(`tool/mod.rs:1364` `transform_tool_input`), the `pre_tool` gate can block a call
(`tool/mod.rs:1375` `run_pre_tool_gate`), and `post_tool_feedback` puts text back into a
result (`tool/mod.rs:1193` `apply_post_tool_feedback`). The transformer is the worst one
to overlap, because changing the same input twice races and the surviving input depends
on scheduling. The lock is described at `tool/mod.rs:203` `HOOK_SERIAL`.

Three results matter.

- The lock is taken on every call, not just group members, because `Registry::execute`
  cannot tell whether its caller grouped it.
- The lock is held only for the hook call and never during the tool's own execution, so
  unrelated tools do not queue behind it.
- The lock is process-wide. Two sessions in one daemon queue while one hook call runs.

A detached observer does not lock and may run at the same time. `post_tool`
(`tool/mod.rs:1120` `post_tool_event`) and the turn and session observers go through
`hooks::dispatch_observer`, which spawns a detached child and returns immediately
(`crates/base/src/hooks.rs:324` `dispatch_observer`). Their script body runs after the
call has returned, so no lock in this process can order them.

## Where the behavior is tested

| Area | Test file and pinning tests |
| --- | --- |
| grouping around `Ordered` calls | `crates/app-core/src/agent/tool_round_tests.rs` (`grouping_splits_concurrent_runs_around_ordered_calls`, `an_unclassified_call_is_treated_as_ordered`, `write_effect_closes_the_open_group`) |
| the 10 and 4 caps | `tool_round_tests.rs` (`grouping_caps_a_concurrent_run_at_ten_members`, `grouping_caps_network_calls_at_four_members`, `a_mixed_group_never_carries_more_than_four_network_members`, `network_effect_limits_a_group_without_a_name_list`, `read_and_network_share_a_group_until_the_network_cap`, `the_network_cap_counts_aliased_network_names`) |
| effect resolution and unknown names | `tool_round_tests.rs` (`unknown_tool_effect_is_a_barrier`, `plan_round_reads_the_effects_of_each_call_from_its_input`) |
| ordered results, panic isolation, cancellation | `tool_round_tests.rs` |
| the round-owned budget | `crates/app-core/src/agent/round_budget_tests.rs` (`the_counter_survives_a_group_boundary`, `first_output_is_admitted_and_the_second_that_crosses_is_refused`) |
| the ladder and its boundaries | `crates/app-core/src/agent/loop_guard_tests.rs` (`identical_run_warns_refuses_then_stops`, `a_different_signature_after_a_refusal_does_not_stop`) |
| a run is one tool with one input | `loop_guard_tests.rs` (`alternating_tools_on_one_path_never_warn`, `one_tools_refusal_does_not_stop_another_tool`, `polls_before_a_repeated_read_do_not_soften_it`, `reset_clears_the_poll_ladder`) |
| polls keep running and changing calls do not | `loop_guard_tests.rs` (`identical_bg_polls_warn_once_and_never_refuse_or_stop`, `every_polling_bg_action_keeps_running`, `identical_output_readers_keep_running`, `identical_status_style_mcp_calls_keep_running`, `a_native_mcp_status_call_keeps_running`, `a_mutating_bg_action_keeps_the_full_ladder`, `a_non_status_mcp_tool_keeps_the_full_ladder`, `a_cycle_of_polls_warns_once_and_keeps_running`, `a_cycle_with_a_non_poll_member_still_escalates`) |
| the polling cap that bounds waiting | `loop_guard_tests.rs` (`consecutive_polls_are_capped_and_the_cap_ends_the_turn`, `the_polling_cap_counts_polls_whose_input_changes`) |
| the cycle ladder, its clearing, and per-turn initialization | `loop_guard_tests.rs` (`repeating_cycle_warns_then_refuses_then_stops`, `changing_one_step_of_a_refused_cycle_is_allowed`, `cycle_ladder_is_cleared_by_reset`) |
| serialization of waiting hooks, transformer included | `crates/app-core/src/tool/hook_serialization_tests.rs` (`concurrent_reads_serialize_post_tool_feedback_hooks`, `concurrent_reads_serialize_the_input_transform_hook`, `post_tool_observer_stays_detached`) |
| `intent` normalization, pagination and A/B negative cases, window eviction | `loop_guard_tests.rs` (`intent_does_not_change_the_signature`, `paginated_reads_of_one_file_are_clean`, `paginating_one_file_in_repeating_pages_is_clean`, `alternating_reads_of_two_files_are_clean`, `reads_of_distinct_paths_are_clean`, `window_evicts_and_caps_the_reported_count`) |
| a non-streaming round through real agent state | `crates/app-core/src/agent/turn_loops_round_tests.rs` |
| both toggles, defaults and parsing | `crates/base/src/config_tests.rs:237` `parallel_tools_and_loop_guard_feature_defaults_and_parse` |
| the shared registry clone used for tool execution | `crates/app-core/src/tool/compaction_sharing_tests.rs`, `crates/app-core/src/agent/streaming_clone_compaction_tests.rs` |
| the Alt+B response the client receives | `crates/app-core/src/agent/turn_streaming_mpsc_interrupt_tests.rs` (`a_background_request_during_a_group_is_refused_visibly`, `a_lone_ordered_bash_is_still_detachable`) |

## How citations are written

Every citation in this document puts the target symbol next to the line it is on: the
file, the line, and the symbol inside its own code span. The reader greps the symbol.
The check script used to write this document is not in the repository (`.omo/` is not
tracked). It found the symbol inside the cited file and failed when the symbol sat more
than 15 lines away from the cited line. CI does not run that check now, so when a line
number has drifted, grep the symbol to find it again. Three citations point at a module
doc block rather than a symbol and mark the approximation with `~` before the line
number.