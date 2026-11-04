//! Advisor: an independent second model that watches a session and files
//! narrow notes the main agent weighs at its next round boundary.
//!
//! Opt-in through `[advisor] enabled = true`. On the first completed tool round
//! of a session the main agent forks its provider, creates a child session,
//! restricts an isolated registry to the read-only inspection tools plus
//! [`advise`](AdviseTool), and starts one background worker. The main agent
//! feeds the worker the transcript since its cursor at every round boundary;
//! the worker runs one advisor turn at a time and later deltas accumulate in
//! the channel rather than starting parallel turns.
//!
//! Notes never act on their own. `nit` and `concern` notes merge into the next
//! request's system reminder; a `blocker` queues a soft interrupt unless the
//! session is still inside the configured immune window after an earlier
//! blocker. The advisor never starts a main-agent turn.

use super::{Agent, SoftInterruptSource};
use crate::logging;
use crate::message::{ContentBlock, Role};
use crate::session::{Session, StoredMessage};
use crate::tool::{Tool, ToolContext, ToolEffects, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
#[cfg(test)]
use std::collections::HashMap;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use tokio::sync::mpsc;

/// Registry name of the advisor's only write-side channel.
pub(crate) const ADVISE_TOOL: &str = "advise";

/// The read-only inspection tools the advisor keeps from the session registry.
const ADVISOR_TOOLS: [&str; 3] = ["read", "agentgrep", "ls"];

/// Markers the emission guard drops without telling the advisor why, because a
/// note matching one is an injection attempt rather than a review note.
const QUARANTINE: [&str; 3] = [
    r"(?i)\bignore\s+(?:all\s+)?(?:prior|previous|earlier)\s+(?:user\s+)?instructions\b",
    r"(?i)\brm\s+-[a-z]*r[a-z]*f|\brm\s+-[a-z]*f[a-z]*r",
    r"(?i)\bdeny\s+(?:this|it|the\s+request)\s+if\s+(?:asked|questioned)\b",
];

/// Normalized notes that say nothing worth the main agent's attention.
const BLOCKLIST: [&str; 23] = [
    "stop",
    "stop here",
    "halt",
    "abort",
    "done",
    "task done",
    "complete",
    "finished",
    "ok",
    "okay",
    "no issue",
    "no issues",
    "no concerns",
    "nothing to add",
    "nothing to flag",
    "nothing to report",
    "no notes",
    "lgtm",
    "looks good",
    "all good",
    "on track",
    "continue",
    "carry on",
];

/// How many delivered notes the session remembers for deduplication.
const SEEN_CAPACITY: usize = 4096;

/// Longest tool result the advisor sees from one message.
const TOOL_RESULT_CHARS: usize = 4000;

/// Consecutive failed advisor turns before the advisor is switched off for the
/// session.
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

pub(crate) const ADVISOR_SYSTEM_PROMPT: &str = r#"You are the advisor: an independent reviewer watching another agent work in the session below. You are not that agent and never act on its behalf.

Read the transcript updates you are given. Before you claim anything, verify it with two or three read-only tool calls (read, agentgrep, ls). Then file short notes with the advise tool.

Report only what the main agent cannot see for itself or would plausibly get wrong:
- Never restate an error the transcript already showed the agent.
- Never repeat a note you already filed.
- Never second-guess a decision the agent has committed to unless the transcript or a tool output you inspected shows it will fail.
- Cite only the transcript or tool output you inspected. If you did not verify a claim, do not make it.

At most {max} notes per update may be non-blockers; blockers are exempt from that budget.

Call advise with one of these severities:
- "nit": non-urgent cleanup the agent can skip.
- "concern": a wrong path or a missing constraint.
- "blocker": a defect that will cause data loss, a broken build, or a wrong answer.

If nothing is actionable, do not call advise at all. End your turn with one short sentence naming what you checked."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    Nit,
    Concern,
    Blocker,
}

impl Severity {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Nit => "nit",
            Self::Concern => "concern",
            Self::Blocker => "blocker",
        }
    }

    fn parse(value: Option<&str>) -> Option<Self> {
        match value.unwrap_or("concern").trim().to_ascii_lowercase().as_str() {
            "nit" => Some(Self::Nit),
            "concern" => Some(Self::Concern),
            "blocker" => Some(Self::Blocker),
            _ => None,
        }
    }
}

/// How a note reaches the main agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// A blocker outside the immune window interrupts the turn.
    Interrupt,
    /// Everything else rides the next request's system reminder.
    Reminder,
}

/// A blocker interrupts; every other note waits for the reminder slot. Inside
/// the immune window after an earlier blocker, even a blocker waits: a second
/// interrupt would restart the same alarm the first one already raised.
pub(crate) fn delivery_for(severity: Severity, within_immune_window: bool) -> Delivery {
    if severity == Severity::Blocker && !within_immune_window {
        Delivery::Interrupt
    } else {
        Delivery::Reminder
    }
}

/// The `immune_turns` user turns after a blocker interrupt, counted in turn
/// starts so a round's later requests stay inside the same window.
pub(crate) fn within_immune_window(fired: Option<usize>, turn: usize, immune_turns: u32) -> bool {
    fired.is_some_and(|fired| turn < fired.saturating_add(immune_turns as usize))
}

/// One note the advisor filed for delivery.
#[derive(Debug, Clone)]
pub(crate) struct AdvisoryNote {
    pub(crate) severity: Severity,
    pub(crate) text: String,
}

/// The main agent's ownership of one advisor session.
pub(crate) struct Handle {
    tx: mpsc::UnboundedSender<String>,
    sink: Arc<Sink>,
    disabled: Arc<AtomicBool>,
}

impl Handle {
    fn send(&self, text: String) {
        let _ = self.tx.send(text);
    }

    pub(crate) fn take_pending(&self) -> Vec<AdvisoryNote> {
        self.sink.take_pending()
    }

    fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::SeqCst)
    }
}

/// Shared state between the `advise` tool (inside the child agent) and the
/// main agent's delivery step.
pub(crate) struct Sink {
    session_id: String,
    budget: u32,
    state: Mutex<SinkState>,
}

struct SinkState {
    non_blockers_this_update: u32,
    seen: VecDeque<String>,
    pending: Vec<AdvisoryNote>,
}

impl Sink {
    pub(crate) fn new(session_id: String, budget: u32) -> Self {
        Self {
            session_id,
            budget,
            state: Mutex::new(SinkState {
                non_blockers_this_update: 0,
                seen: VecDeque::new(),
                pending: Vec::new(),
            }),
        }
    }

    fn guard(&self) -> MutexGuard<'_, SinkState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A new advisor turn starts a new update, and the non-blocker budget is
    /// per update rather than per session.
    pub(crate) fn begin_update(&self) {
        self.guard().non_blockers_this_update = 0;
    }

    pub(crate) fn take_pending(&self) -> Vec<AdvisoryNote> {
        std::mem::take(&mut self.guard().pending)
    }

    /// Apply the emission guard and, on success, queue the note.
    pub(crate) fn emit(&self, raw_note: &str, severity: Severity) -> &'static str {
        let note = raw_note.trim();
        if note.is_empty() {
            return "Dropped: empty note.";
        }
        if quarantined(note) {
            logging::warn(&format!(
                "advisor note quarantined for session {}: {}",
                self.session_id,
                crate::util::truncate_str(note, 240)
            ));
            return "Dropped: nothing actionable.";
        }
        let normalized = normalize(note);
        if BLOCKLIST.contains(&normalized.as_str()) {
            return "Dropped: nothing actionable.";
        }
        let mut state = self.guard();
        if state.seen.iter().any(|seen| seen == &normalized) {
            return "Dropped: already raised.";
        }
        if severity != Severity::Blocker && state.non_blockers_this_update >= self.budget {
            return "Dropped: this update's advice budget is spent.";
        }
        if severity != Severity::Blocker {
            state.non_blockers_this_update += 1;
        }
        state.seen.push_back(normalized);
        while state.seen.len() > SEEN_CAPACITY {
            state.seen.pop_front();
        }
        state.pending.push(AdvisoryNote {
            severity,
            text: note.to_string(),
        });
        "Delivered."
    }
}

fn quarantined(note: &str) -> bool {
    static PATTERNS: LazyLock<Vec<regex::Regex>> = LazyLock::new(|| {
        QUARANTINE
            .iter()
            .filter_map(|pattern| regex::Regex::new(pattern).ok())
            .collect()
    });
    PATTERNS.iter().any(|pattern| pattern.is_match(note))
}

/// Lowercase, then collapse every run of non-alphanumeric characters into one
/// space. The comparison form of a note, never the delivered text.
fn normalize(note: &str) -> String {
    let lowered = note.to_lowercase();
    let mut normalized = String::with_capacity(lowered.len());
    let mut pending_space = false;
    for character in lowered.chars() {
        if character.is_alphanumeric() {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            pending_space = false;
            normalized.push(character);
        } else {
            pending_space = true;
        }
    }
    normalized
}

/// Sessions that ARE an advisor worker. Their own round boundaries must never
/// start another advisor.
static ADVISOR_SESSIONS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn advisor_sessions() -> MutexGuard<'static, HashSet<String>> {
    ADVISOR_SESSIONS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn is_advisor_session(session_id: &str) -> bool {
    advisor_sessions().contains(session_id)
}

#[cfg(test)]
struct TestAdvisor {
    sink: Arc<Sink>,
    child_session_id: String,
}

#[cfg(test)]
static TEST_ADVISORS: LazyLock<Mutex<HashMap<String, TestAdvisor>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
fn register_test_advisor(parent_session_id: &str, sink: Arc<Sink>, child_session_id: &str) {
    TEST_ADVISORS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            parent_session_id.to_string(),
            TestAdvisor {
                sink,
                child_session_id: child_session_id.to_string(),
            },
        );
}

/// The notes the advisor has queued but the main agent has not drained yet.
#[cfg(test)]
pub(crate) fn test_pending_notes(parent_session_id: &str) -> Option<Vec<(Severity, String)>> {
    let advisors = TEST_ADVISORS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let advisor = advisors.get(parent_session_id)?;
    Some(
        advisor
            .sink
            .guard()
            .pending
            .iter()
            .map(|note| (note.severity, note.text.clone()))
            .collect(),
    )
}

/// The child session id the advisor runs in, for transcript assertions.
#[cfg(test)]
pub(crate) fn test_child_session_id(parent_session_id: &str) -> Option<String> {
    TEST_ADVISORS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(parent_session_id)
        .map(|advisor| advisor.child_session_id.clone())
}

/// The prompt the advisor session runs with, with its budget filled in.
pub(crate) fn system_prompt(max_notes: u32) -> String {
    ADVISOR_SYSTEM_PROMPT.replace("{max}", &max_notes.to_string())
}

/// Render one note for the main agent's next request.
pub(crate) fn render_note(note: &AdvisoryNote) -> String {
    format!(
        "<advisory severity=\"{}\" guidance=\"weigh, don't blindly obey\">\n{}\n</advisory>",
        note.severity.as_str(),
        note.text
    )
}

/// Render the transcript slice the advisor has not seen yet.
pub(crate) fn render_delta(messages: &[StoredMessage], start: usize, end: usize) -> String {
    let mut rendered = String::from("### Session update\n");
    for (offset, message) in messages[start.min(end)..end].iter().enumerate() {
        let index = start + offset;
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        rendered.push_str(&format!("\n[#{index} {role}]\n"));
        for block in &message.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    rendered.push_str(text.trim_end());
                    rendered.push('\n');
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    let compact = serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string());
                    rendered.push_str(&format!("→ {name} {compact}\n"));
                }
                ContentBlock::ToolResult { content, is_error, .. } => {
                    let marker = if *is_error == Some(true) { "(error) " } else { "" };
                    rendered.push_str(&format!(
                        "← {marker}{}\n",
                        crate::util::truncate_str(content, TOOL_RESULT_CHARS)
                    ));
                }
                _ => {}
            }
        }
    }
    rendered
}

/// Whether this stored message starts a user turn.
///
/// Mirrors the provider projection's fresh-user predicate: text or images only,
/// and an injected `<system-reminder>` is context, not a new turn. Counting it
/// as one would shorten the advisor's immune window.
fn is_turn_start(message: &StoredMessage) -> bool {
    if message.role != Role::User {
        return false;
    }
    let mut saw_user_text = false;
    for block in &message.content {
        match block {
            ContentBlock::Text { text, .. } => {
                let trimmed = text.trim();
                if !trimmed.is_empty() && !trimmed.starts_with("<system-reminder>") {
                    saw_user_text = true;
                }
            }
            ContentBlock::Image { .. } => {}
            _ => return false,
        }
    }
    saw_user_text
}

/// The number of user turns in the transcript, used to bound the advisor's
/// immune window after a blocker interrupt.
pub(crate) fn user_turn_index(messages: &[StoredMessage]) -> usize {
    messages.iter().filter(|message| is_turn_start(message)).count()
}

/// The advisor's single channel back to the main agent.
struct AdviseTool {
    sink: Arc<Sink>,
}

#[async_trait]
impl Tool for AdviseTool {
    fn name(&self) -> &str {
        ADVISE_TOOL
    }

    fn description(&self) -> &str {
        "File one short advisory note for the main agent. Use severity \"nit\" for non-urgent cleanup, \"concern\" for a wrong path or missing constraint, and \"blocker\" for something that will cause data loss, a broken build, or a wrong answer. Notes are reviewed before delivery; a note that says nothing actionable is dropped."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "note": {
                    "type": "string",
                    "description": "The note itself, in one or two sentences."
                },
                "severity": {
                    "type": "string",
                    "enum": ["nit", "concern", "blocker"],
                    "description": "How strongly the main agent should weigh this note. Defaults to concern."
                }
            },
            "required": ["note"]
        })
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::read()
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let note = input
            .get("note")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("advise requires a string `note`"))?;
        let severity = Severity::parse(input.get("severity").and_then(Value::as_str))
            .ok_or_else(|| anyhow::anyhow!("severity must be one of nit, concern, blocker"))?;
        Ok(ToolOutput::new(self.sink.emit(note, severity)))
    }
}

impl Agent {
    /// Create the advisor after the session's first completed tool round.
    ///
    /// Returns false when the advisor could not be prepared, so the caller does
    /// not keep retrying on every round.
    async fn spawn_advisor(&mut self, config: &crate::config::AdvisorConfig) -> bool {
        let budget = config.note_budget();
        // A private map and a private MCP alias index: narrowing the advisor's
        // surface must not re-index the parent's MCP aliases.
        let base_registry = self.registry().with_isolated_tool_subset(&ADVISOR_TOOLS).await;

        let sink = Arc::new(Sink::new(self.session.id.clone(), budget));
        base_registry
            .register(
                ADVISE_TOOL.to_string(),
                Arc::new(AdviseTool {
                    sink: Arc::clone(&sink),
                }) as Arc<dyn Tool>,
            )
            .await;

        let provider = self.provider_fork();
        let model = match &config.model {
            Some(model) => {
                if let Err(error) = provider.set_model(model) {
                    logging::warn(&format!(
                        "advisor model '{}' is not available for session {}: {error}",
                        model, self.session.id
                    ));
                }
                provider.model()
            }
            None => provider.model(),
        };

        let mut session = Session::create(Some(self.session.id.clone()), Some("advisor".to_string()));
        let child_session_id = session.id.clone();
        session.system_prompt = Some(system_prompt(budget));
        session.model = Some(model);
        session.provider_key = self.session.provider_key.clone();
        session.route_api_method = self.session_route_api_method();
        session.working_dir = self.session.working_dir.clone();
        if let Err(error) = session.save() {
            logging::warn(&format!(
                "advisor session for {} could not be saved: {error}",
                self.session.id
            ));
            return false;
        }

        let allowed: HashSet<String> = ADVISOR_TOOLS
            .iter()
            .copied()
            .chain(std::iter::once(ADVISE_TOOL))
            .map(|name| name.to_string())
            .collect();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let disabled = Arc::new(AtomicBool::new(false));
        let parent_session_id = self.session.id.clone();
        advisor_sessions().insert(child_session_id.clone());
        #[cfg(test)]
        register_test_advisor(&parent_session_id, Arc::clone(&sink), &child_session_id);

        let worker_sink = Arc::clone(&sink);
        let worker_disabled = Arc::clone(&disabled);
        let worker_parent = parent_session_id.clone();
        // The blocking turn loop is not `Send` (it runs on an owned thread in the
        // CLI and the swarm paths), so the advisor owns a dedicated thread with
        // its own current-thread runtime instead of a spawned task. One thread
        // also enforces "one advisor turn in flight" for free: later deltas wait
        // in the channel and are merged into the next turn.
        let worker_name = format!("advisor-{}", parent_session_id.chars().take(8).collect::<String>());
        let worker_child_session_id = child_session_id.clone();
        let spawned = std::thread::Builder::new().name(worker_name).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    logging::warn(&format!(
                        "advisor runtime for session {worker_parent} could not start: {error}"
                    ));
                    worker_disabled.store(true, Ordering::SeqCst);
                    advisor_sessions().remove(&worker_child_session_id);
                    return;
                }
            };
            let mut child = Agent::new_with_session(provider, base_registry, session, Some(allowed));
            let mut failures = 0u32;
            while let Some(first) = rx.blocking_recv() {
                let mut delta: String = first;
                while let Ok(next) = rx.try_recv() {
                    delta.push_str("\n\n");
                    delta.push_str(&next);
                }
                worker_sink.begin_update();
                match runtime.block_on(child.run_once_capture(&delta)) {
                    Ok(_) => failures = 0,
                    Err(error) => {
                        failures += 1;
                        logging::warn(&format!("advisor turn failed for session {worker_parent}: {error}"));
                        if failures >= MAX_CONSECUTIVE_FAILURES {
                            logging::warn(&format!("advisor disabled for session {worker_parent}: {error}"));
                            worker_disabled.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                }
            }
            advisor_sessions().remove(&worker_child_session_id);
        });
        if let Err(error) = spawned {
            logging::warn(&format!(
                "advisor worker for session {parent_session_id} could not start: {error}"
            ));
            advisor_sessions().remove(&child_session_id);
            return false;
        }

        self.advisor = Some(Handle { tx, sink, disabled });
        true
    }

    /// Feed the advisor this round's transcript and deliver the notes that are
    /// ready to the current turn.
    ///
    /// Runs at the round boundary, so a note never starts a turn: one that
    /// arrives while the main agent is idle waits for the next turn.
    pub(crate) async fn advisor_after_round(&mut self) {
        let config = crate::config::config().advisor.clone();
        if is_advisor_session(&self.session.id) {
            return;
        }
        if !config.enabled {
            self.advisor = None;
            return;
        }
        if self.advisor.as_ref().is_some_and(Handle::is_disabled) {
            self.advisor = None;
            return;
        }
        if self.advisor.is_none() && !self.spawn_advisor(&config).await {
            return;
        }

        let end = self.session.active_context().len();
        if end > self.advisor_cursor {
            let delta = render_delta(self.session.active_context(), self.advisor_cursor, end);
            let mode = crate::config::config().tools.secrets_mode;
            let delta = crate::secrets::mask_for_history(&delta, &self.session.id, mode);
            self.advisor_cursor = end;
            if let Some(handle) = &self.advisor {
                handle.send(delta);
            }
        }

        self.deliver_advisories(&config);
    }

    fn deliver_advisories(&mut self, config: &crate::config::AdvisorConfig) {
        let Some(handle) = &self.advisor else {
            return;
        };
        let notes = handle.take_pending();
        if notes.is_empty() {
            return;
        }
        let turn = user_turn_index(self.session.active_context());
        let immune = within_immune_window(self.advisor_blocker_turn, turn, config.immune_turns);
        for note in notes {
            let text = render_note(&note);
            if delivery_for(note.severity, immune) == Delivery::Interrupt {
                self.queue_soft_interrupt(text, Vec::new(), false, SoftInterruptSource::System);
                self.advisor_blocker_turn = Some(turn);
            } else {
                Agent::merge_system_reminder(&mut self.current_turn_system_reminder, &text);
            }
        }
    }

    /// Tell a running advisor that the main transcript was rewritten
    /// (compaction, rewind, or reset), so its next update is not a diff
    /// against history that no longer exists.
    pub(crate) fn advisor_history_rewritten(&mut self) {
        let Some(handle) = &self.advisor else {
            return;
        };
        self.advisor_cursor = self.session.active_context().len();
        handle.send("### Session reset".to_string());
    }
}
