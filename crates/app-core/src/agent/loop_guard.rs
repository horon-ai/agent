//! Loop detection for repeated tool calls.
//!
//! The guard keeps one rolling window of the most recent tool calls in the
//! current turn and answers a single question at each dispatch decision: is
//! this call a repeat of what the model has already done, and how hard should
//! the agent push back?
//!
//! # When the guard is consulted
//!
//! Once per DISPATCH decision, never inside a running group: before a
//! concurrent group starts and before each ordered call. Every member of a
//! group is recorded at that single decision, so a group of four identical
//! `read` calls advances the ladder to four before any of them runs, and the
//! member whose verdict refuses it is simply not dispatched while the rest of
//! the group still runs. The caller decides dispatch boundaries; this module
//! never observes a call mid-group and knows nothing about concurrency.
//!
//! # The one escalation rule
//!
//! An identical run of three emits a warning and continues; the fourth and
//! later identical calls are refused; two refusals of the same tool with the
//! same signature end the turn. A cycle climbs the same ladder, keyed on the
//! cycle itself rather than on one call: its first detection warns, the next is
//! refused, and the second refusal ends the turn. That is the whole ladder, and
//! it is applied in exactly one place, [`LoopGuard::record_and_decide`]. The
//! detectors only decide whether a run is long enough to be interesting:
//!
//! - identical run: three or more consecutive repeats of the same tool with the
//!   same byte-identical signature,
//! - cycle: a period of two to six calls repeated three or more times, unless
//!   every member of the cycle targets a distinct identity (alternating reads
//!   of two different files are comparison, not a loop).
//!
//! A cycle that is only warned about repeats forever: a daemon stuck in
//! `read a.rs -> bash -> read a.rs` would collect one reminder per dispatch and
//! never stop. The warning is the first rung, not the ceiling. The cycle's key
//! is its own set of member signatures under a phase rotation, so the rotation
//! of the same loop (`read -> bash`, then `bash -> read`) stays one cycle
//! instead of restarting the ladder.
//!
//! # Polls stop at the warning rung, and waiting is capped
//!
//! The guard compares inputs, and one family of calls has a legitimately
//! constant input: a reader of a resource that moves on its own. A `bg` status
//! or wait, the `*_output` readers of a background task, and a status-style tool
//! reached through `mcp_call` are all asked the same way every time, and the
//! answer is what changes. Repeating one of them is waiting, not being stuck,
//! so those calls are never refused: the first repeat at the warning tier
//! leaves one reminder, and later repeats keep running until the separate
//! polling cap below bounds the run. Refusing them would end the turn that is
//! waiting for the very result it asked about and orphan the work behind it.
//! A cycle made only of polls is treated the same way. A call whose name
//! suggests it changes something - an MCP write, a `bg` cancel - is not a poll
//! and keeps the full ladder. Neither is a `bg` watch, delivery, or subscribe:
//! each of those rewrites the task's delivery settings (and may re-arm the
//! stall watchdog) on every call, so repeating one is a write loop, not a wait.
//!
//! The ladder's silence is not the whole policy, because it is silent on
//! purpose: with no second bound a model that polls a resource which never
//! changes pays one full request per poll for as long as the daemon runs. The
//! second bound is [`POLLING_CALL_CAP`]: a run of consecutive polls is counted
//! per turn, and the call past the cap ends the turn with a notice. It is
//! deliberately coarser than the ladder - it counts polls, not identical polls,
//! because changing one field on every status call still costs a request - and
//! it never refuses a call, so a short wait behaves exactly as it did before.
//! Any non-poll ends the run, and the turn boundary clears the count.
//!
//! There is no "similar" tier between those two. A run that differs from the
//! newest call only in its arguments is what ordinary work looks like (the next
//! page, the next file), so catching it would take a similarity score; a run
//! that also matches the signature is already an identical run. No similarity
//! scoring exists anywhere in this module.
//!
//! State lives in memory for the current turn only. Nothing is written to
//! disk, and the guard never mutates the tool input it is given.

use serde_json::Value;
use std::collections::{HashMap, VecDeque};

/// Records kept for the current turn. Long enough to hold every cycle the
/// detector can recognise (six periods times three repetitions) plus the
/// surrounding run, short enough that a stale loop cannot survive a long
/// stretch of unrelated work.
const WINDOW_CAPACITY: usize = 64;

/// An identical run reaches this length on the call that emits the warning.
const IDENTICAL_WARN_AT: usize = 3;

/// From this length on, an identical call is refused instead of executed.
const IDENTICAL_REFUSE_AT: usize = 4;

const CYCLE_MIN_PERIOD: usize = 2;
const CYCLE_MAX_PERIOD: usize = 6;
const CYCLE_MIN_REPEATS: usize = 3;

/// Refusals of one tool with one signature that end the turn.
const STOP_AFTER_REFUSALS: u32 = 2;

/// Consecutive polls one turn may make before the guard ends it.
///
/// The poll exemption is right about a single wait and silent about a long one:
/// a resource that never changes turns "wait for it" into one paid request per
/// poll, forever, which is the failure an unattended daemon hits. This cap is
/// the second net. It is generous enough that any real wait finishes inside it
/// (twenty-four status reads is minutes of polling, not a loop) and small enough
/// that a stuck turn costs a bounded number of requests instead of an unbounded
/// number. Reaching it ends the turn rather than refusing the call: the work
/// behind the poll is still worth resuming in a new turn, and the notice says so.
const POLLING_CALL_CAP: usize = 24;

/// `bg` actions whose result can change while the input stays identical.
///
/// `cancel` and `cleanup` change state and stay on the full ladder. So do
/// `watch`, `delivery`, and `subscribe`: all three run one arm in the tool
/// (`tool/bg.rs`, `"watch" | "delivery" | "subscribe"`) that calls
/// `update_delivery` on every invocation and re-arms the stall watchdog when
/// `stall_wake_seconds` is present. A call that rewrites state is not a wait,
/// so repeating one is refused and then stops the turn.
const POLLING_BG_ACTIONS: &[&str] = &["status", "wait", "output", "tail", "list"];

/// Words that mark a remote tool as a status-style read: its answer describes a
/// resource that moves on its own, so asking again with the same input is how
/// the caller learns whether it moved.
///
/// These classify a remote tool's name, the only signal the guard has for it,
/// so they are kept apart from [`POLLING_BG_ACTIONS`]: `bg`'s `watch`,
/// `delivery`, and `subscribe` are excluded there because the tool's own code
/// shows they write, and no such evidence exists for a remote tool. A remote
/// name carrying one of these words therefore keeps the exemption even when the
/// tool behind it writes - `subscribe_events` and `watch_job` are classified as
/// polls. That is a known limit rather than a decision: a name is all there is.
const POLLING_TOOL_WORDS: &[&str] = &[
    "status",
    "poll",
    "wait",
    "await",
    "output",
    "tail",
    "progress",
    "watch",
    "result",
    "results",
    "state",
    "logs",
    "log",
    "info",
    "list",
    "jobs",
    "job",
    "tasks",
    "task",
    "delivery",
    "subscribe",
];

/// Words that mark a remote tool as changing something. They veto the polling
/// reading of a name that also names its subject, so `create_job` and
/// `write_file` keep the full ladder while `job_status` does not.
const MUTATING_TOOL_WORDS: &[&str] = &[
    "write",
    "set",
    "update",
    "create",
    "delete",
    "remove",
    "cancel",
    "cleanup",
    "clean",
    "kill",
    "restart",
    "start",
    "stop",
    "run",
    "exec",
    "execute",
    "push",
    "send",
    "post",
    "put",
    "patch",
    "apply",
    "commit",
    "merge",
    "move",
    "rename",
    "copy",
    "upload",
    "save",
    "edit",
    "modify",
    "drop",
    "reset",
    "install",
    "uninstall",
    "enable",
    "disable",
    "retry",
    "submit",
    "approve",
    "reject",
    // Verbs that change a record or a buffer without naming a file. Without
    // them `assign_task`, `mark_task_done`, `add_task`, `append_log` and
    // `rotate_logs` read as status calls and ran forever, while `create_task`
    // and `delete_task` were refused - the same kind of call, two verdicts.
    "add",
    "insert",
    "append",
    "assign",
    "mark",
    "clear",
    "flush",
    "purge",
    "rotate",
    "replace",
    "truncate",
    "ack",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct CallRecord {
    tool_name: String,
    /// Canonical form of the tool input: sorted object keys, `intent` removed.
    signature: String,
    /// What the call acts on, when the tool has a recognised target field.
    /// `None` never counts as a distinct target for the cycle detector.
    target: Option<String>,
    /// The call reads a resource that can change while its input stays
    /// identical, so repeating it is waiting rather than being stuck.
    polling: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detection {
    None,
    Identical { repeats: usize },
    Cycle { period: usize, repeats: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoopGuardVerdict {
    /// Execute the call as requested.
    Allow,
    /// Execute the call, then carry the message as a system reminder.
    Warn(String),
    /// Do not execute the call; return the message as an error tool result.
    Refuse(String),
    /// Do not execute the call and end the turn; surface the message.
    Stop(String),
}

/// Per-turn loop-detection state owned by the agent.
///
/// The guard is reset at the start of every turn, so a loop the model broke
/// last turn starts clean this turn.
#[derive(Debug, Default)]
pub(crate) struct LoopGuard {
    window: VecDeque<CallRecord>,
    /// Refusals per tool and signature within the turn for an identical run.
    /// Kept beside the window rather than inside it so the count survives
    /// unrelated calls between refusals. Keyed on the tool too, so one tool's
    /// refusal never climbs another tool's ladder into a stop.
    refusals: HashMap<(String, String), u32>,
    /// Ladder state of the cycle currently being repeated, if any.
    ///
    /// One contiguous cycle is tracked at a time: the warning is emitted once
    /// per cycle run, the next detections refuse, and a broken cycle drops the
    /// state so a later, different loop warns first instead of inheriting a
    /// refusal. Keying the warning on the cycle rather than on a single call is
    /// what lets a rotation of several calls climb the ladder at all.
    cycle_ladder: Option<CycleLadder>,
    /// Polls made since the last call that was not a poll.
    ///
    /// Counted here rather than derived from the window because the window is
    /// capped at [`WINDOW_CAPACITY`] and a run longer than it would forget its
    /// own length; the cap must be reachable no matter how long the run is.
    /// Any non-poll resets it, so alternating a status read with real work is
    /// never capped, and [`Self::reset`] clears it with the rest of the turn.
    consecutive_polls: usize,
}

/// Escalation state for one contiguous repeating cycle.
#[derive(Debug)]
struct CycleLadder {
    /// The cycle this state belongs to, phase-normalized.
    key: String,
    warned: bool,
    refusals: u32,
}

impl LoopGuard {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Start a fresh turn: no history, no refusal counts, no cycle ladder, and
    /// a full poll allowance.
    pub(crate) fn reset(&mut self) {
        self.window.clear();
        self.refusals.clear();
        self.cycle_ladder = None;
        self.consecutive_polls = 0;
    }

    /// Record one call about to be dispatched and return the escalation verdict.
    ///
    /// The call is recorded even when the verdict refuses it, so a model that
    /// repeats the same input after a refusal advances the ladder to the stop
    /// instead of restarting it.
    pub(crate) fn record_and_decide(&mut self, tool_name: &str, input: &Value) -> LoopGuardVerdict {
        let signature = canonical_signature(input);
        let target = target_identity(tool_name, input);
        let polling = polling_call(tool_name, input);
        self.push(CallRecord {
            tool_name: tool_name.to_string(),
            signature: signature.clone(),
            target,
            polling,
        });

        // The coarse bound, decided before any ladder: it applies to every poll
        // shape - an identical run, a cycle, or a sequence whose inputs all
        // differ - because each of them costs one request per poll. A call that
        // is not a poll ends the run, so only waiting is counted.
        if polling {
            self.consecutive_polls += 1;
            if self.consecutive_polls > POLLING_CALL_CAP {
                return LoopGuardVerdict::Stop(poll_cap_stop_text(tool_name, self.consecutive_polls));
            }
        } else {
            self.consecutive_polls = 0;
        }

        match detect(&self.window) {
            Detection::Identical { repeats } => {
                self.cycle_ladder = None;
                if polling {
                    return poll_verdict(tool_name, repeats);
                }
                if repeats >= IDENTICAL_REFUSE_AT {
                    let key = (tool_name.to_string(), signature.clone());
                    let refusal_count = {
                        let count = self.refusals.entry(key).or_insert(0);
                        *count += 1;
                        *count
                    };
                    if refusal_count >= STOP_AFTER_REFUSALS {
                        LoopGuardVerdict::Stop(stop_text(tool_name, repeats))
                    } else {
                        LoopGuardVerdict::Refuse(refusal_text(tool_name, &signature, repeats))
                    }
                } else {
                    LoopGuardVerdict::Warn(identical_warning_text(tool_name, repeats))
                }
            }
            Detection::Cycle { period, repeats } => {
                let key = self.cycle_key(period);
                let members = self.cycle_members(period);
                let polling = self.cycle_is_polling(period);
                let ladder = match &mut self.cycle_ladder {
                    Some(ladder) if ladder.key == key => ladder,
                    slot => {
                        *slot = Some(CycleLadder {
                            key,
                            warned: false,
                            refusals: 0,
                        });
                        slot.as_mut().expect("the cycle ladder was just installed")
                    }
                };
                if !ladder.warned {
                    ladder.warned = true;
                    if polling {
                        LoopGuardVerdict::Warn(polling_cycle_warning_text(&members, repeats))
                    } else {
                        LoopGuardVerdict::Warn(cycle_warning_text(&members, repeats))
                    }
                } else if polling {
                    // A cycle of polls is waiting in a loop shape: one reminder
                    // is the whole response, because refusing it would end the
                    // turn that is waiting for a changing answer.
                    LoopGuardVerdict::Allow
                } else {
                    ladder.refusals += 1;
                    if ladder.refusals >= STOP_AFTER_REFUSALS {
                        LoopGuardVerdict::Stop(cycle_stop_text(&members))
                    } else {
                        LoopGuardVerdict::Refuse(cycle_refusal_text(&members))
                    }
                }
            }
            Detection::None => {
                // No cycle at the tail: any tracked cycle is broken, so a later
                // one starts again at the warning tier.
                self.cycle_ladder = None;
                LoopGuardVerdict::Allow
            }
        }
    }

    fn push(&mut self, record: CallRecord) {
        if self.window.len() == WINDOW_CAPACITY {
            self.window.pop_front();
        }
        self.window.push_back(record);
    }

    /// Whether every member of the cycle at the tail is a poll.
    fn cycle_is_polling(&self, period: usize) -> bool {
        let start = self.window.len().saturating_sub(period);
        self.window.iter().skip(start).all(|record| record.polling)
    }

    /// The `period` distinct calls of the cycle at the tail, oldest first.
    fn cycle_members(&self, period: usize) -> Vec<String> {
        let start = self.window.len().saturating_sub(period);
        self.window
            .iter()
            .skip(start)
            .map(|record| record.tool_name.clone())
            .collect()
    }

    /// Identity of the cycle at the tail: every member's tool and canonical
    /// signature, reduced to its lexicographically smallest rotation.
    ///
    /// The rotation makes the key phase-invariant - `read -> bash` and
    /// `bash -> read` are the same loop and must not restart the ladder - while
    /// keeping the order, so two different orderings of the same calls stay
    /// distinct. A single call's signature cannot key this: each member of a
    /// cycle has its own.
    fn cycle_key(&self, period: usize) -> String {
        let start = self.window.len().saturating_sub(period);
        let members: Vec<String> = self
            .window
            .iter()
            .skip(start)
            .map(|record| format!("{}:{}", record.tool_name, record.signature))
            .collect();
        let mut best: Option<Vec<String>> = None;
        for offset in 0..members.len() {
            let rotated: Vec<String> = members[offset..]
                .iter()
                .chain(members[..offset].iter())
                .cloned()
                .collect();
            if best.as_ref().is_none_or(|current| rotated < *current) {
                best = Some(rotated);
            }
        }
        best.unwrap_or_default().join("\n")
    }

    #[cfg(test)]
    pub(crate) fn window_len(&self) -> usize {
        self.window.len()
    }
}

/// The run of trailing records that repeat the newest call: the same tool with
/// the same signature. Two different tools aimed at one path carry the same
/// canonical input, so comparing the signature alone counted them as one run
/// and warned that a tool had been "called 3 times in a row" when it had not.
fn identical_run(window: &VecDeque<CallRecord>) -> usize {
    let Some(newest) = window.back() else {
        return 0;
    };
    window
        .iter()
        .rev()
        .take_while(|record| record.tool_name == newest.tool_name && record.signature == newest.signature)
        .count()
}

/// The shortest period (two to six) repeated at least three times at the tail,
/// unless every member of that period targets a distinct identity.
fn cycle_run(window: &VecDeque<CallRecord>) -> Option<(usize, usize)> {
    for period in CYCLE_MIN_PERIOD..=CYCLE_MAX_PERIOD {
        let needed = period * CYCLE_MIN_REPEATS;
        if window.len() < needed {
            continue;
        }
        let members: Vec<&CallRecord> = window.iter().skip(window.len() - period).collect();
        // A period whose members are all the same call is an identical run,
        // which the identical detector owns.
        if members
            .windows(2)
            .all(|pair| pair[0].tool_name == pair[1].tool_name && pair[0].signature == pair[1].signature)
        {
            continue;
        }
        let matches = (0..needed).all(|index| {
            let record = &window[window.len() - needed + index];
            let member = members[index % period];
            record.tool_name == member.tool_name && record.signature == member.signature
        });
        if !matches {
            continue;
        }
        let mut targets: Vec<&str> = members.iter().filter_map(|member| member.target.as_deref()).collect();
        let every_member_has_a_target = targets.len() == members.len();
        targets.sort_unstable();
        targets.dedup();
        if every_member_has_a_target && targets.len() == members.len() {
            continue;
        }
        return Some((period, CYCLE_MIN_REPEATS));
    }
    None
}

fn detect(window: &VecDeque<CallRecord>) -> Detection {
    let identical = identical_run(window);
    if identical >= IDENTICAL_WARN_AT {
        return Detection::Identical { repeats: identical };
    }
    if let Some((period, repeats)) = cycle_run(window) {
        return Detection::Cycle { period, repeats };
    }
    Detection::None
}

/// Serialize a tool input with sorted object keys and the `intent` field
/// removed, so two calls that differ only in their stated reason are equal.
pub(crate) fn canonical_signature(input: &Value) -> String {
    let mut out = String::new();
    match input {
        Value::Object(map) => write_object(map, true, &mut out),
        other => write_value(other, &mut out),
    }
    out
}

fn write_object(map: &serde_json::Map<String, Value>, skip_intent: bool, out: &mut String) {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_unstable();
    out.push('{');
    let mut first = true;
    for key in keys {
        if skip_intent && key == "intent" {
            continue;
        }
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&Value::String(key.clone()).to_string());
        out.push(':');
        write_value(&map[key], out);
    }
    out.push('}');
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => write_object(map, false, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// What a call acts on, when the tool has a recognised target field.
///
/// `read` includes the requested range so paginating one file never looks like
/// a repeat. The range is the raw requested fields rather than a normalized
/// offset/limit pair: normalization lives in the read tool, and duplicating it
/// here would let the two drift.
fn target_identity(tool_name: &str, input: &Value) -> Option<String> {
    match tool_name {
        "read" => read_target(input),
        "lsp_diagnostics" => first_string_field(input, &["file_path", "path"]),
        "bash_output" | "task_output" | "bg" => {
            first_string_field(input, &["task_id", "id", "shell_id", "background_id"])
        }
        _ => None,
    }
}

fn read_target(input: &Value) -> Option<String> {
    let path = input.get("file_path")?.as_str()?;
    let range_field = |name: &str| match input.get(name) {
        None | Some(Value::Null) => String::from("-"),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    };
    Some(format!(
        "{path}#start_line={}#end_line={}#offset={}#limit={}",
        range_field("start_line"),
        range_field("end_line"),
        range_field("offset"),
        range_field("limit")
    ))
}

fn first_string_field(input: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| input.get(*name).and_then(Value::as_str).map(str::to_string))
}

/// Whether repeating this call is waiting rather than being stuck.
///
/// The guard compares inputs, so a call whose result can change under a
/// constant input looks identical to a call the model keeps repeating by
/// mistake. These are the calls that read a resource which moves on its own:
/// a background task's status, wait, or accumulated output, and a status-style
/// remote tool. They stop at the warning rung instead of being refused.
fn polling_call(tool_name: &str, input: &Value) -> bool {
    match tool_name {
        "bash_output" | "task_output" => true,
        "bg" => bg_action_is_polling(input),
        "mcp_call" => input.get("tool").and_then(Value::as_str).is_some_and(names_a_poll),
        name => native_mcp_tool(name).is_some_and(names_a_poll),
    }
}

/// `bg` decides its action from the `action` field, which its schema requires;
/// an omitted action is inferred from `intent` or rejected, and this guard stays
/// conservative there rather than duplicating that inference.
fn bg_action_is_polling(input: &Value) -> bool {
    input
        .get("action")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .is_some_and(|action| POLLING_BG_ACTIONS.contains(&action.as_str()))
}

/// The tool of a native `mcp__<server>__<tool>` call name.
fn native_mcp_tool(tool_name: &str) -> Option<&str> {
    tool_name.strip_prefix("mcp__")?.rsplit("__").next()
}

/// Whether a remote tool's name reads a resource that changes on its own.
fn names_a_poll(tool: &str) -> bool {
    let words: Vec<String> = tool
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if words.iter().any(|word| MUTATING_TOOL_WORDS.contains(&word.as_str())) {
        return false;
    }
    words.iter().any(|word| POLLING_TOOL_WORDS.contains(&word.as_str()))
}

/// The ladder for a poll: one reminder at the warning tier, then the guard
/// steps aside.
///
/// A poll's input is identical by definition and its answer is not, so a
/// refusal here would block the call that is waiting for the answer and the
/// stop after it would end the turn and orphan the work. The warning survives
/// because a poll whose answer never changes is still worth one nudge.
fn poll_verdict(tool_name: &str, repeats: usize) -> LoopGuardVerdict {
    if repeats == IDENTICAL_WARN_AT {
        LoopGuardVerdict::Warn(polling_warning_text(tool_name, repeats))
    } else {
        LoopGuardVerdict::Allow
    }
}

/// The notice that ends a turn whose polls ran past [`POLLING_CALL_CAP`].
///
/// It names the cap and the count, and it names the caller's next move: a
/// loop-guard stop emits no error event, so the SDK's retry policy never sees
/// it and the client or daemon that owns the session must start the next turn.
fn poll_cap_stop_text(tool_name: &str, polls: usize) -> String {
    format!(
        "Loop guard ended the turn: \"{tool_name}\" has been called {polls} times in a row to read a resource that can change between calls. Waiting is bounded: the cap is {POLLING_CALL_CAP} consecutive polls in one turn, and the cap never refused a call. Nothing resumes this turn automatically; the client or daemon that owns this session must start the next turn to keep waiting. If the answer is not changing, take a different step instead of asking again."
    )
}

fn identical_warning_text(tool_name: &str, repeats: usize) -> String {
    format!(
        "<system-reminder>Loop guard: \"{tool_name}\" has now been called {repeats} times in a row with identical input. Change the arguments or take a different approach; the next identical call will be refused.</system-reminder>"
    )
}

fn polling_warning_text(tool_name: &str, repeats: usize) -> String {
    format!(
        "<system-reminder>Loop guard: \"{tool_name}\" has now been called {repeats} times in a row with identical input. This call reads a resource that can change between calls, so the guard will not refuse it; if the answer is not changing, wait for it or take a different step instead of asking again.</system-reminder>"
    )
}

fn polling_cycle_warning_text(members: &[String], repeats: usize) -> String {
    let sequence = members.join(" -> ");
    format!(
        "<system-reminder>Loop guard: the call sequence {sequence} has repeated {repeats} times. Every step reads a resource that can change between calls, so the guard will not refuse it; if nothing is changing, wait for it or take a different step instead of repeating the sequence.</system-reminder>"
    )
}

fn cycle_warning_text(members: &[String], repeats: usize) -> String {
    let sequence = members.join(" -> ");
    format!(
        "<system-reminder>Loop guard: the call sequence {sequence} has repeated {repeats} times without changing the target. Break the cycle by changing the input or the plan before continuing; the next repetition of this cycle will be refused.</system-reminder>"
    )
}

fn cycle_refusal_text(members: &[String]) -> String {
    let sequence = members.join(" -> ");
    format!(
        "Loop guard refused the cycle {sequence}: these calls keep repeating without changing their target. Change the input of at least one step (a different file, range, command, or query) or take a different approach. A second refusal of this same cycle ends the turn."
    )
}

fn cycle_stop_text(members: &[String]) -> String {
    let sequence = members.join(" -> ");
    format!(
        "Loop guard ended the turn: the cycle {sequence} kept repeating without changing its target and was refused twice."
    )
}

fn refusal_text(tool_name: &str, signature: &str, repeats: usize) -> String {
    format!(
        "Loop guard refused \"{tool_name}\": this exact input has been sent {repeats} times in a row: {signature}. Change the arguments (a different file, range, command, or query) or use a different tool. A second refusal of this exact input ends the turn."
    )
}

fn stop_text(tool_name: &str, repeats: usize) -> String {
    format!("Loop guard ended the turn: \"{tool_name}\" repeated the same input {repeats} times and was refused twice.")
}
