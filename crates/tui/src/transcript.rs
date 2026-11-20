//! The conversation as it is drawn: user messages, streamed assistant text,
//! and one line per tool call.
//!
//! The transcript owns the streaming buffer rather than the app, because the
//! rules about it (a delta for a new message closes the previous one, an empty
//! replacement retracts text) are the same everywhere and are easy to get wrong
//! at the call site.

use std::time::Instant;

use crate::tool_expand::{self, ExpandState};
use crate::tool_line::{self, DEFAULT_OUTPUT_LINES, OutputHead};

/// What a tool call line says about its call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolState {
    /// Started, no outcome yet.
    Running,
    /// Finished without an error.
    Done,
    /// Finished with an error the model will see.
    Failed(String),
    /// Refused by the approval gate, with the decision still to come.
    ///
    /// The gate answers a call it may not run with a sentence naming the
    /// request, and that sentence is kept here so the line can say what the
    /// request was about once it is decided. Drawing it as a failure meanwhile
    /// says the call ran and broke, which is not what happened.
    Waiting(String),
    /// Refused by the approval gate, with the decision recorded on the row.
    ///
    /// This is not a failure: the gate held the call, and the model has to
    /// issue it again. It is not a wait either: the prompt is gone, so nothing
    /// will change the row again.
    Settled(Decision, String),
}

/// What became of a request the approval gate refused a call with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Answered yes for this call.
    Allowed,
    /// Answered yes, and for every later call like it.
    AllowedAlways,
    /// Denied, or the prompt was dismissed.
    Denied,
    /// The prompt went away without an answer.
    Unanswered,
}

impl Decision {
    /// The word the row wears for this outcome.
    pub fn label(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::AllowedAlways => "allowed always",
            Self::Denied => "denied",
            Self::Unanswered => "not answered",
        }
    }
}

impl ToolState {
    /// The short word shown after the tool name.
    pub fn label(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed(_) => "failed",
            Self::Waiting(_) => "needs approval",
            Self::Settled(decision, _) => decision.label(),
        }
    }
}

/// What a tool line says when the turn ended before its result arrived.
///
/// Neutral on purpose: a cancelled turn and a dead connection end calls the
/// same way, and neither says anything about the tool itself.
pub const TURN_ENDED: &str = "the turn ended before this call reported";

/// One tool call, as far as the client knows it.
///
/// The call id is kept because parallel calls of the same tool are ordinary:
/// two `bash` calls interleave their input and finish in either order, and only
/// the id says which line a result belongs to. An empty id is what an older
/// daemon sends, and it means "the call that started most recently".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// The id the daemon gave this call, empty when it gave none.
    pub call_id: String,
    /// The tool's name.
    pub name: String,
    /// Where the call stands.
    pub state: ToolState,
    /// The argument JSON seen so far, capped at [`tool_line::MAX_INPUT_BYTES`].
    pub input: String,
    /// When the call finished and how long it took, in milliseconds.
    pub duration_ms: Option<u64>,
    /// The head of the call's output, kept only as far as the preview needs.
    pub output: Option<OutputHead>,
}

impl ToolCall {
    fn new(call_id: &str, name: &str) -> Self {
        Self {
            call_id: call_id.to_string(),
            name: name.to_string(),
            state: ToolState::Running,
            input: String::new(),
            duration_ms: None,
            output: None,
        }
    }

    /// Whether this is the call `call_id` names.
    ///
    /// An empty id matches nothing: with no id there is no identity, and the
    /// caller falls back to "the oldest running call of this name".
    fn is_call(&self, call_id: &str) -> bool {
        !call_id.is_empty() && self.call_id == call_id
    }
}

/// What one completed turn cost, drawn under its last line.
///
/// The figures are taken once, when the turn ends, and kept with the item: the
/// renderer never reads a clock or asks the turn again, so a frame costs the
/// same whether or not the footer is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnMetrics {
    /// Wall time from the message being sent to the turn ending, in
    /// milliseconds.
    pub elapsed_ms: u64,
    /// Input and output tokens the turn's provider calls reported, added
    /// together. `None` when no call reported usage, which is not the same as
    /// a turn that spent none.
    pub tokens: Option<(u64, u64)>,
}

/// One line of conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A message the user sent.
    User(String),
    /// A message typed while a turn was running: kept, drawn, and sent when
    /// the turn ends.
    Queued(String),
    /// A completed assistant message.
    Assistant(String),
    /// A tool call, addressed by its call id while it runs.
    Tool(ToolCall),
    /// Session-level information, not model output.
    Notice(String),
    /// Something the user needs to see and act on.
    Warning(String),
    /// A thought the model published before its answer, drawn apart from it.
    Reasoning(String),
    /// What fills the context window, as `/context` reported it.
    ///
    /// Boxed because the report is the widest thing an item can carry and the
    /// variant is rare, so the enum keeps the footprint of its common cases.
    Context(Box<crate::context::ContextReport>),
    /// Another runtime command's answer, drawn as labelled rows.
    ///
    /// Boxed for the same reason as [`Item::Context`]: the payload behind it is
    /// large, and most items carry a message instead.
    Report(Box<crate::report::Report>),
    /// What one completed turn cost, drawn under it when
    /// `transcript.turn_usage` is on.
    TurnUsage(TurnMetrics),
    /// A request boundary the provider had to serve without its cached prefix.
    ///
    /// Drawn under the request it belongs to, and only for an avoidable miss:
    /// see [`CacheMiss::from_report`] for what counts as one.
    CacheMiss(CacheMiss),
}

/// Which part of the cached prompt prefix the harness changed.
///
/// The three are exactly the reasons the daemon classifies as harness-caused
/// (`MissReason::harness_caused` in `crates/base/src/kv_cache_monitor.rs`): the
/// prompt is the harness's to build, so a change to it there was avoidable. A
/// provider switch, an expiry or a documented reload is not, and never reaches
/// this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheCause {
    /// The system prompt's static part.
    System,
    /// The tool definitions sent with the request.
    Tools,
    /// A message that was already part of the prefix.
    Prefix,
}

impl CacheCause {
    /// The cause the daemon's stable reason id names, `None` for any other id.
    ///
    /// The ids mirror `MissReason::id`; one this client does not know is left
    /// unclassified rather than drawn as an alarm, because the marker would
    /// then claim a cause the client cannot check.
    fn from_reason(reason: &str) -> Option<Self> {
        match reason {
            "system_changed" => Some(Self::System),
            "tools_changed" => Some(Self::Tools),
            "prefix_changed" => Some(Self::Prefix),
            _ => None,
        }
    }

    /// How the marker names the cause.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "the system prompt changed",
            Self::Tools => "the tool set changed",
            Self::Prefix => "an earlier message changed",
        }
    }
}

/// An avoidable prompt-cache miss, as the transcript marks it.
///
/// Only the classification and the size of the resend are kept. The prompt text
/// and the cache's identity never reach the client, and nothing here could hold
/// them: a mark is the reader's summary of one request boundary, not a record
/// of what was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheMiss {
    /// Which part of the request prefix the harness changed.
    pub cause: CacheCause,
    /// Tokens the provider had to be sent again.
    pub missed_tokens: u64,
}

impl CacheMiss {
    /// The mark for one daemon report, or `None` when the report is not an
    /// avoidable, classified miss.
    ///
    /// `harness_caused` and the reason id are both required: the daemon derives
    /// one from the other, so a report where they disagree cannot be trusted to
    /// describe an avoidable miss. A documented cause is an intentional
    /// invalidation (a config or skill reload) and is not an alarm either.
    pub fn from_report(
        reason: &str,
        harness_caused: bool,
        documented_cause: Option<&str>,
        missed_tokens: u64,
    ) -> Option<Self> {
        let cause = CacheCause::from_reason(reason)?;
        if !harness_caused || documented_cause.is_some_and(|cause| !cause.trim().is_empty()) {
            return None;
        }
        Some(Self { cause, missed_tokens })
    }

    /// The one line the marker draws.
    pub fn summary(&self) -> String {
        format!(
            "{}, ~{} tokens resent",
            self.cause.label(),
            compact_tokens(self.missed_tokens)
        )
    }
}

/// A token count at the width the marker can spare, as the daemon's own summary
/// writes it.
fn compact_tokens(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{}K", value / 1_000)
    } else {
        value.to_string()
    }
}

/// The streaming message being assembled.
#[derive(Debug, Default)]
struct Streaming {
    message_id: Option<String>,
    text: String,
    open: bool,
}

/// The reasoning being assembled, kept apart from the answer.
///
/// A provider streams reasoning and assistant text on the same message id, in
/// order, interleaved with tool calls. Accumulating both into one buffer would
/// splice a thought into the middle of the answer it precedes, so reasoning has
/// its own slot and is committed on its own boundary.
#[derive(Debug, Default)]
struct Reasoned {
    text: String,
    open: bool,
}

/// The transcript buffer.
#[derive(Debug)]
pub struct Transcript {
    items: Vec<Item>,
    streaming: Streaming,
    reasoned: Reasoned,
    /// The value `transcript.show_reasoning` last resolved to.
    setting_show_reasoning: bool,
    /// A choice made for this session, by `--hide-thinking` or the toggle key.
    ///
    /// It outranks the setting until that setting itself changes, so an
    /// unrelated settings reload does not undo the toggle.
    session_show_reasoning: Option<bool>,
    /// Whether reasoning is kept, as the setting and the session choice
    /// resolve. Off by default; the option is read at the event, so flipping
    /// it affects the next delta rather than only the next turn.
    show_reasoning: bool,
    /// Rows of a finished call's output kept for the preview, as
    /// `transcript.tool_output_lines` resolves. Zero hides the preview.
    tool_output_lines: usize,
    /// When each call started, keyed by call id and dropped when it ends.
    starts: Vec<Start>,
    /// The whole output of the calls whose preview was cut, and which of them is
    /// drawn in full. Kept here rather than on the items so a line stays a plain
    /// description of a call, and bounded in `tool_expand`.
    expand: ExpandState,
    /// The requests this session has decided, newest last.
    ///
    /// A refusal can reach the client after its prompt was answered, and the
    /// row still has to say what happened to the call. Bounded because it is
    /// only ever read for the refusal in hand.
    decided: Vec<(String, Decision)>,
    /// The turn being measured, between the message being sent and the turn
    /// ending. `None` when the client is not the one measuring, which is why a
    /// turn another surface started draws no footer.
    turn: Option<Turn>,
    /// Whether a finished turn's time and usage are drawn, as
    /// `transcript.turn_usage` resolves.
    turn_usage: bool,
}

/// When one call started.
#[derive(Debug)]
struct Start {
    call_id: String,
    name: String,
    at: Instant,
}

/// The turn in progress, while the client is measuring it.
#[derive(Debug)]
struct Turn {
    /// When the message was handed to the harness.
    started: Instant,
    /// Input tokens the turn's provider calls reported so far.
    ///
    /// Added as they arrive rather than read as a difference of session totals,
    /// so a counter that resets or moves backwards can never subtract here.
    input: u64,
    /// Output tokens the turn's provider calls reported so far.
    output: u64,
    /// Whether any call reported usage. A turn that reported none draws no
    /// token field rather than a misleading zero.
    reported: bool,
}

impl Default for Transcript {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            streaming: Streaming::default(),
            reasoned: Reasoned::default(),
            setting_show_reasoning: false,
            session_show_reasoning: None,
            show_reasoning: false,
            tool_output_lines: DEFAULT_OUTPUT_LINES,
            starts: Vec::new(),
            expand: ExpandState::default(),
            decided: Vec::new(),
            turn: None,
            turn_usage: false,
        }
    }
}

impl Transcript {
    /// An empty transcript.
    pub fn new() -> Self {
        Self::default()
    }

    /// Keep at most `lines` rows of each finished call's output, or none.
    ///
    /// Applies to calls that finish from now on: the transcript holds only the
    /// preview, so a smaller number cannot shorten a line already drawn.
    pub fn set_tool_output_lines(&mut self, lines: usize) {
        self.tool_output_lines = lines.min(tool_line::MAX_OUTPUT_LINES);
    }

    /// How many rows of a finished call's output the preview keeps.
    pub fn tool_output_lines(&self) -> usize {
        self.tool_output_lines
    }

    /// Apply the value `transcript.show_reasoning` resolves to.
    ///
    /// A session choice outranks it, so an unrelated settings reload does not
    /// undo the toggle; a change of the setting itself is a newer choice and
    /// ends the session choice.
    pub fn apply_settings_show_reasoning(&mut self, show: bool) {
        if self.setting_show_reasoning != show {
            self.setting_show_reasoning = show;
            self.session_show_reasoning = None;
        }
        self.reconcile_show_reasoning();
    }

    /// Keep, or stop keeping, the model's reasoning for the rest of the session.
    ///
    /// The choice outranks `transcript.show_reasoning` until that setting
    /// value changes. Turning reasoning off drops the thought already being
    /// assembled, because a switch the user just flipped off must not leave
    /// half a thought on screen; turning it on starts from the next delta,
    /// because the deltas dropped meanwhile were never kept. The reasoning
    /// effort the model is asked for is not touched.
    pub fn set_show_reasoning(&mut self, show: bool) {
        self.session_show_reasoning = Some(show);
        self.reconcile_show_reasoning();
    }

    /// Whether reasoning is being kept.
    pub fn show_reasoning(&self) -> bool {
        self.show_reasoning
    }

    /// Apply the setting and the session choice to what the transcript keeps.
    fn reconcile_show_reasoning(&mut self) {
        let show = self.session_show_reasoning.unwrap_or(self.setting_show_reasoning);
        if self.show_reasoning == show {
            return;
        }
        self.show_reasoning = show;
        if !show {
            self.reasoned = Reasoned::default();
        }
    }

    /// Keep, or stop keeping, a finished turn's time and token usage.
    ///
    /// Read when the turn ends, so a turn already running takes the new value
    /// and a turn that already ended keeps the footer it was drawn with.
    pub fn set_turn_usage(&mut self, show: bool) {
        self.turn_usage = show;
    }

    /// Start measuring the turn the client is about to send.
    ///
    /// A turn already being measured is replaced: one message is in flight at a
    /// time, so the only way to reach this twice without an end is a send that
    /// never produced a turn.
    pub fn begin_turn(&mut self) {
        self.turn = Some(Turn {
            started: Instant::now(),
            input: 0,
            output: 0,
            reported: false,
        });
    }

    /// Add one provider call's usage to the turn being measured.
    ///
    /// Counts are added with saturation and never subtracted, so a provider
    /// that reports a smaller number than an earlier call, or a counter a
    /// reconnect restarted, cannot make the turn's usage negative. A call that
    /// reported no usage adds nothing; a turn with no calls at all keeps its
    /// token field empty.
    pub fn note_usage(&mut self, input: u64, output: u64) {
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        turn.input = turn.input.saturating_add(input);
        turn.output = turn.output.saturating_add(output);
        turn.reported = true;
    }

    /// Close the turn being measured and draw what it cost under it.
    ///
    /// Nothing is drawn for a turn this client did not start, or while
    /// `transcript.turn_usage` is off. The elapsed time is taken once here and
    /// stored with the item, so drawing it later reads no clock.
    pub fn end_turn(&mut self) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        if !self.turn_usage {
            return;
        }
        // The answer is committed before the footer, so the footer reads as the
        // last line of the turn rather than sitting above streaming text.
        self.finish_stream();
        let elapsed_ms = u64::try_from(turn.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.items.push(Item::TurnUsage(TurnMetrics {
            elapsed_ms,
            tokens: turn.reported.then_some((turn.input, turn.output)),
        }));
    }

    /// Stop measuring the turn without drawing anything.
    ///
    /// Used when there is no completed turn to describe: the send never
    /// reached the harness, the connection died, or the session changed.
    pub fn discard_turn(&mut self) {
        self.turn = None;
    }

    /// Every item in display order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The assistant text still streaming, if any.
    ///
    /// `None` once the message is committed by [`finish_stream`](Self::finish_stream),
    /// retracted by [`text_replace`](Self::text_replace), or when the open
    /// message holds only whitespace, which `finish_stream` would drop rather
    /// than publish. The renderer draws this as its newest entry, so a message
    /// appears while it streams instead of all at once when it ends.
    pub fn open_text(&self) -> Option<&str> {
        if !self.streaming.open || self.streaming.text.trim().is_empty() {
            return None;
        }
        Some(&self.streaming.text)
    }

    /// The newest assistant answers, including any text still streaming.
    pub(crate) fn recent_assistant_text(&self, messages: usize) -> impl Iterator<Item = &str> {
        let committed = self.items.iter().rev().filter_map(|item| match item {
            Item::Assistant(text) => Some(text.as_str()),
            _ => None,
        });
        self.open_text().into_iter().chain(committed).take(messages)
    }

    /// Record a user message, closing any partly streamed reply first.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.finish_stream();
        self.items.push(Item::User(text.into()));
    }

    /// Record a completed assistant message that was not streamed here.
    ///
    /// Reading a stored session back goes through this: the daemon hands over
    /// whole messages, so there are no deltas to accumulate.
    pub fn push_assistant(&mut self, text: impl Into<String>) {
        self.finish_stream();
        self.items.push(Item::Assistant(text.into()));
    }

    /// Empty the conversation, keeping the display settings.
    ///
    /// Resuming another session replaces the conversation rather than the
    /// reader's settings: the output preview length and the reasoning switch
    /// were chosen for this terminal, not for the session that was open.
    pub fn clear(&mut self) {
        self.items.clear();
        self.streaming = Streaming::default();
        self.reasoned = Reasoned::default();
        self.starts.clear();
        // The kept outputs belong to the calls just removed: a later call can
        // reuse an id, and text from the old conversation must not be drawn
        // under it.
        self.expand.clear();
        // The turn being measured belongs to the conversation being replaced,
        // and the rows its footer would sit under are gone with it.
        self.turn = None;
    }

    /// The kept outputs, for the pane to draw the calls they belong to.
    pub(crate) fn expand_state(&self) -> &ExpandState {
        &self.expand
    }

    /// Show, or hide again, the whole output of the newest cut call.
    ///
    /// Answers whether there was anything to toggle: the caller says so when
    /// there was not, rather than leaving the key silently doing nothing.
    pub(crate) fn toggle_expand(&mut self) -> bool {
        self.expand.toggle()
    }

    /// Record an informational line.
    pub fn push_notice(&mut self, text: impl Into<String>) {
        self.finish_stream();
        self.items.push(Item::Notice(text.into()));
    }

    /// Display a live notice immediately before any open streaming message.
    pub(crate) fn push_live_notice(&mut self, text: String) {
        self.items.push(Item::Notice(text));
    }

    /// Record the `/context` breakdown.
    ///
    /// One item rather than a notice per row: the rows are one block, they are
    /// measured together, and a held view scrolls past them as one unit.
    pub fn push_context(&mut self, report: crate::context::ContextReport) {
        self.finish_stream();
        self.items.push(Item::Context(Box::new(report)));
    }

    /// Record another runtime command's answer.
    ///
    /// One item for the same reasons as [`Transcript::push_context`]: the rows
    /// are one block, measured together and scrolled past as one unit.
    pub fn push_report(&mut self, report: crate::report::Report) {
        self.finish_stream();
        self.items.push(Item::Report(Box::new(report)));
    }

    /// Record a warning line.
    pub fn push_warning(&mut self, text: impl Into<String>) {
        self.finish_stream();
        self.items.push(Item::Warning(text.into()));
    }

    /// Record an avoidable cache miss at the boundary of the request it belongs
    /// to.
    ///
    /// The daemon reports the miss immediately after the usage of the request
    /// that missed, so the caller hands it over while that request's output is
    /// the newest thing drawn; committing the open stream first is what puts the
    /// mark under the answer that cost the resend rather than over it.
    ///
    /// The same mark twice in a row is one event delivered twice — a replayed
    /// tail after a reconnect — not two requests: a request that misses always
    /// draws its own output in between, so nothing in between means nothing
    /// happened.
    pub fn push_cache_miss(&mut self, miss: CacheMiss) {
        self.finish_stream();
        let item = Item::CacheMiss(miss);
        if self.items.last() == Some(&item) {
            return;
        }
        self.items.push(item);
    }

    /// Record a message waiting for the running turn to end.
    ///
    /// It is a line of its own rather than a warning, because it is not a
    /// problem: the text is already accepted and will be sent.
    pub fn push_queued(&mut self, text: impl Into<String>) {
        self.finish_stream();
        self.items.push(Item::Queued(text.into()));
    }

    /// Turn the oldest queued line for `text` into the message that was sent.
    ///
    /// The queued line is promoted rather than replaced, so the conversation
    /// keeps the order the messages were typed in.
    pub fn promote_queued(&mut self, text: &str) {
        let found = self
            .items
            .iter()
            .position(|item| matches!(item, Item::Queued(queued) if queued == text));
        if let Some(index) = found {
            self.items[index] = Item::User(text.to_string());
        }
    }

    /// Take back the line drawn for a message the harness refused.
    ///
    /// The newest line with this text is the one the failed send drew: one
    /// message is in flight at a time, and a submit during a turn draws a
    /// waiting line rather than a sent one, so retrying cannot stack a second
    /// copy of a line that was never delivered. A waiting line for another
    /// message with the same text is left alone.
    pub fn retract_user(&mut self, text: &str) {
        let found = self
            .items
            .iter()
            .rposition(|item| matches!(item, Item::User(sent) if sent == text));
        if let Some(index) = found {
            self.items.remove(index);
        }
    }

    /// End every tool line that never reported a result.
    ///
    /// A turn can end while a call is still running: it was cancelled, the
    /// provider failed, or the connection died. The line would then say
    /// `running` for the rest of the session, so it is closed here with a
    /// sentence that blames neither the tool nor the model.
    pub fn end_running_tools(&mut self) {
        for item in &mut self.items {
            if let Item::Tool(call) = item
                && call.state == ToolState::Running
            {
                call.state = ToolState::Failed(TURN_ENDED.to_string());
            }
        }
        // The call can never finish now, so its start mark is dead weight; a
        // late replay of its result must not invent a duration either.
        let items = &self.items;
        self.starts.retain(|start| {
            items.iter().any(|item| {
                matches!(item, Item::Tool(call) if call.state == ToolState::Running
                    && ((!start.call_id.is_empty() && call.call_id == start.call_id)
                        || (start.call_id.is_empty() && call.name == start.name)))
            })
        });
    }

    /// Append streamed assistant text.
    ///
    /// A delta that carries a different message id than the open message starts
    /// a new one, which is how one turn's several messages stay separate
    /// without waiting for `TextDone`.
    pub fn text_delta(&mut self, message_id: Option<&str>, text: &str) {
        self.finish_reasoning();
        if self.streaming.open && self.streaming.message_id.as_deref() != message_id {
            self.finish_stream();
        }
        self.streaming.open = true;
        self.streaming.message_id = message_id.map(str::to_string);
        self.streaming.text.push_str(text);
    }

    /// Replace the open message's text.
    ///
    /// An empty replacement retracts it, including one that was already
    /// completed: providers retry, and the client must not keep text the model
    /// withdrew.
    pub fn text_replace(&mut self, message_id: Option<&str>, text: &str) {
        if self.streaming.open && self.streaming.message_id.as_deref() != message_id {
            self.finish_stream();
        }
        if text.is_empty() {
            let retracts_open = self.streaming.open && self.streaming.message_id.as_deref() == message_id;
            if retracts_open {
                self.streaming = Streaming::default();
                return;
            }
            if let Some(index) = self.last_assistant_index() {
                self.items.remove(index);
            }
            return;
        }
        self.streaming.open = true;
        self.streaming.message_id = message_id.map(str::to_string);
        self.streaming.text.clear();
        self.streaming.text.push_str(text);
    }

    /// A message ended; the next delta starts a new one.
    pub fn text_done(&mut self) {
        self.finish_stream();
    }

    /// Reasoning text arrived for the message being assembled.
    ///
    /// Dropped when the setting is off, and dropped rather than buffered: a
    /// client that keeps it anyway pays for tokens it will never draw.
    pub fn reasoning_delta(&mut self, text: &str) {
        if !self.show_reasoning {
            return;
        }
        // Accumulate. Reasoning commits at a boundary (`reasoning_done`, a tool
        // call, or the first answer delta), never on the next delta: a provider
        // streams a thought as many deltas, and committing each one would leave
        // one item per chunk instead of one per thought.
        self.reasoned.text.push_str(text);
        self.reasoned.open = true;
    }

    /// Reasoning ended for the current step.
    pub fn reasoning_done(&mut self) {
        self.finish_reasoning();
    }

    /// Reasoning still streaming, for the renderer to draw dimmed.
    pub fn open_reasoning(&self) -> Option<&str> {
        if !self.reasoned.open || self.reasoned.text.trim().is_empty() {
            return None;
        }
        Some(&self.reasoned.text)
    }

    /// Commit the open reasoning, dropping an empty one.
    fn finish_reasoning(&mut self) {
        if !self.reasoned.open {
            return;
        }
        let reasoned = std::mem::take(&mut self.reasoned);
        if !reasoned.text.trim().is_empty() {
            self.items.push(Item::Reasoning(reasoned.text));
        }
    }

    /// A tool call started.
    ///
    /// `call_id` is how the result will find this line again; the start time
    /// goes beside the items rather than into one, because an [`Item`] must stay
    /// comparable and an [`Instant`] is not.
    pub fn tool_start(&mut self, call_id: &str, name: &str) {
        self.finish_reasoning();
        self.finish_stream();
        // A start can arrive after the result that ended it, when frames from a
        // resumed stream are reordered. The line is already there and already
        // says how the call ended, so this records nothing: a second line with
        // the same id would show the same call twice.
        let known = !call_id.is_empty()
            && self
                .items
                .iter()
                .any(|item| matches!(item, Item::Tool(call) if call.is_call(call_id)));
        if known {
            return;
        }
        self.starts.push(Start {
            call_id: call_id.to_string(),
            name: name.to_string(),
            at: Instant::now(),
        });
        self.items.push(Item::Tool(ToolCall::new(call_id, name)));
    }

    /// Whether a refusal line names `request_id`.
    ///
    /// The approval gate writes the id into the sentence it refused the call
    /// with, and that is the only link between a parked request and its line: a
    /// prompt finds its arguments through it, and the line is drawn as waiting
    /// for as long as it holds. One predicate, so the two can never disagree
    /// about which call a request is about.
    pub fn names_request(message: &str, request_id: &str) -> bool {
        !request_id.is_empty() && message.contains(request_id)
    }

    /// The argument summary of the call the request `request_id` is asking about.
    ///
    /// An approval arrives with the tool name alone, so a prompt that named only
    /// the name could not tell two parallel `bash` calls apart — and pairing by
    /// name alone showed a prompt the arguments of an unrelated call from an
    /// earlier turn, which is worse than showing nothing. The refusal the gate
    /// writes into the tool line carries the request id
    /// (`crates/app-core/src/tool/mod.rs`, `Registry::approval_refusal`: "... .
    /// Request {request_id} is queued; answer it through the approval surface.
    /// Nothing was executed."), and that id is unique to one call, so the line
    /// whose failure text names this request is the one this prompt covers.
    ///
    /// Every other line is excluded by construction: a call that ran without a
    /// prompt has a result that names something else, a line from an earlier
    /// turn names an earlier request, and a call still streaming its input names
    /// no request yet. `None` means the prompt shows the action alone.
    pub fn argument_summary_for_request(&self, request_id: &str) -> Option<String> {
        if request_id.is_empty() {
            return None;
        }
        self.items.iter().find_map(|item| match item {
            // A parked line is drawn as waiting and still names its request,
            // which is what a prompt reads its arguments through.
            Item::Tool(call) => match &call.state {
                ToolState::Failed(message) | ToolState::Waiting(message) | ToolState::Settled(_, message)
                    if Self::names_request(message, request_id) =>
                {
                    tool_line::summarize(&call.input)
                }
                _ => None,
            },
            _ => None,
        })
    }

    /// Draw the line naming `request_id` as waiting for its decision.
    ///
    /// The call was refused because the mode asks first, not because it broke,
    /// so until the request is decided the line says it is waiting. A line that
    /// already carries its decision keeps it, and nothing happens when no line
    /// names the id yet: `decide_request` settles a row that arrives later.
    pub fn park_request(&mut self, request_id: &str) {
        self.restate_request(request_id, |state| match state {
            ToolState::Failed(message) => Some(ToolState::Waiting(message)),
            _ => None,
        });
    }

    /// Record what became of `request_id` on every line that names it.
    ///
    /// This is the only way a gate line stops saying it is waiting: the three
    /// outcomes say what happened to the request, and none of them is "failed",
    /// because the call did not run. The decision is remembered for a line that
    /// arrives after it.
    pub fn decide_request(&mut self, request_id: &str, decision: Decision) {
        if request_id.is_empty() {
            return;
        }
        // The daemon's resolved event carries `approved` alone, so it cannot say
        // whether the answer was `a` or `y`: the "always" the local key recorded
        // must not be weakened by the event that merely confirms it.
        let decision = match (self.decided_decision(request_id), decision) {
            (Some(Decision::AllowedAlways), Decision::Allowed) => Decision::AllowedAlways,
            _ => decision,
        };
        self.restate_request(request_id, |state| match state {
            ToolState::Failed(message) | ToolState::Waiting(message) | ToolState::Settled(_, message) => {
                Some(ToolState::Settled(decision, message))
            }
            _ => None,
        });
        // One entry per request: a second answer to the same request is refused
        // by the daemon and must not reorder what the row says.
        self.decided.retain(|(id, _)| id != request_id);
        self.decided.push((request_id.to_string(), decision));
        const KEPT: usize = 64;
        if self.decided.len() > KEPT {
            self.decided.remove(0);
        }
    }

    /// The decision already recorded for `request_id`, if any.
    fn decided_decision(&self, request_id: &str) -> Option<Decision> {
        self.decided
            .iter()
            .rev()
            .find(|(id, _)| id == request_id)
            .map(|(_, decision)| *decision)
    }

    /// The decision already recorded for the refusal in `state`, if any.
    fn decided_for_name(&self, state: &ToolState) -> Option<Decision> {
        match state {
            ToolState::Failed(message) => self.decided_for(message),
            _ => None,
        }
    }

    /// The decision recorded for a request the message names, if any.
    fn decided_for(&self, message: &str) -> Option<Decision> {
        self.decided
            .iter()
            .rev()
            .find(|(id, _)| Self::names_request(message, id))
            .map(|(_, decision)| *decision)
    }

    /// Apply `restate` to every line naming `request_id`.
    ///
    /// A line that becomes parked or decided draws no output at all, so the text
    /// kept for it goes too: the key must not report a press that changes
    /// nothing on screen.
    fn restate_request(&mut self, request_id: &str, restate: impl Fn(ToolState) -> Option<ToolState>) {
        for item in &mut self.items {
            if let Item::Tool(call) = item
                && let ToolState::Failed(message) | ToolState::Waiting(message) | ToolState::Settled(_, message) =
                    &call.state
                && Self::names_request(message, request_id)
            {
                let state = call.state.clone();
                if let Some(next) = restate(state) {
                    call.state = next;
                }
            }
        }
        // A second pass, after the states are settled: only a line that ended up
        // parked or decided loses its kept text, and the loop above cannot also
        // touch the table while it holds the items.
        for item in &self.items {
            if let Item::Tool(call) = item
                && Self::settled(&call.state)
            {
                self.expand.forget(&call.call_id);
            }
        }
    }

    /// Argument bytes arrived for a call still streaming its input.
    ///
    /// The delta is appended to the call's line rather than replacing it: input
    /// streams as JSON fragments, and only the joined result parses. An empty
    /// id means the call that started most recently, which is what the daemon
    /// says when it sends no id.
    pub fn tool_input(&mut self, call_id: &str, delta: &str) {
        let target = if call_id.is_empty() {
            self.starts.last().map(|start| start.call_id.clone())
        } else {
            Some(call_id.to_string())
        };
        let Some(target) = target else {
            return;
        };
        let Some(call) = self.items.iter_mut().find_map(|item| match item {
            Item::Tool(call) if call.is_call(&target) => Some(call),
            _ => None,
        }) else {
            return;
        };
        tool_line::append_input(&mut call.input, delta);
    }

    /// A tool call finished, with its output and the error text when it failed.
    ///
    /// The output is cut to the configured preview here, not at the renderer:
    /// only the preview is ever kept, so a tool returning megabytes cannot grow
    /// the transcript.
    ///
    /// A preview that hid part of the output keeps the whole of it, so the key
    /// can show what the cut took away. A failure is kept as much as a success:
    /// a tool that exits non-zero still produced output, and a note naming it
    /// says nothing about how much the call wrote.
    ///
    /// Answers whether a line the pane already counted now draws different
    /// rows: keeping this output folds the call that was open, and a held view
    /// would otherwise keep the height it measured while that call was open. A
    /// cut result with nothing open changes heights only on its own line, which
    /// the scrollback re-measures as the newest item.
    pub fn tool_done(&mut self, call_id: &str, name: &str, output: Option<&str>, error: Option<&str>) -> bool {
        let state = match error {
            Some(message) if !message.is_empty() => ToolState::Failed(message.to_string()),
            _ => ToolState::Done,
        };
        let elapsed = self.take_start(call_id, name);
        let preview = output.and_then(|text| tool_line::cut_output(text, self.tool_output_lines));
        let hides = output
            .zip(preview.as_ref())
            .is_some_and(|(text, head)| tool_expand::cut(text, head));
        // A refusal can arrive after its prompt was answered, and the row still
        // has to say what happened to the request rather than that it failed.
        let state = match (&state, self.decided_for_name(&state)) {
            (ToolState::Failed(message), Some(decision)) => ToolState::Settled(decision, message.clone()),
            _ => state,
        };
        let Some(call) = self.call_for(call_id, name) else {
            let mut call = ToolCall::new(call_id, name);
            call.state = state;
            call.duration_ms = elapsed;
            call.output = preview;
            // A line parked or decided by the gate draws no output at all, so
            // its text is never kept: a key that expanded nothing would be a key
            // that did nothing.
            let draws = !Self::settled(&call.state);
            self.items.push(Item::Tool(call));
            return draws && hides && self.keep_output(call_id, output);
        };
        // A call finishes once, but the same result can reach the client twice
        // (a reconnect replays it). The second one must not overwrite the first:
        // it arrives after the start mark was taken, so its duration is empty,
        // and a repeat of a success would erase a recorded failure.
        let keep_failure = matches!(
            call.state,
            ToolState::Failed(_) | ToolState::Waiting(_) | ToolState::Settled(_, _)
        ) && !matches!(state, ToolState::Failed(_));
        if !keep_failure {
            call.state = state;
        }
        if call.duration_ms.is_none() {
            call.duration_ms = elapsed;
        }
        let stored = call.output.is_none();
        if stored {
            call.output = preview;
        }
        let draws = !Self::settled(&call.state);
        stored && draws && hides && self.keep_output(call_id, output)
    }

    /// Whether a call's line draws no output at all.
    fn settled(state: &ToolState) -> bool {
        matches!(state, ToolState::Waiting(_) | ToolState::Settled(_, _))
    }

    /// Keep the whole text behind a cut preview, for the key to show.
    ///
    /// Answers whether a line already on screen changed its rows, which is what
    /// a held scroll view has to recount.
    fn keep_output(&mut self, call_id: &str, output: Option<&str>) -> bool {
        match output {
            Some(text) => self.expand.record(call_id, text),
            None => false,
        }
    }

    /// The call `call_id` names, or the oldest running call when there is no id.
    ///
    /// A daemon that sends no id still sends the name, and its calls are
    /// sequential, so the oldest unfinished call of that name is the one the
    /// result belongs to; the newest is wrong, because that is the call still
    /// starting.
    fn call_for(&mut self, call_id: &str, name: &str) -> Option<&mut ToolCall> {
        let found = self
            .items
            .iter()
            .position(|item| matches!(item, Item::Tool(call) if call.is_call(call_id)));
        let index: Option<usize> = match found {
            Some(index) => Some(index),
            // With no id there is no identity, and calls without ids are
            // sequential, so the oldest unfinished call of that name is the one
            // this result belongs to.
            None if call_id.is_empty() => self.items.iter().position(|item| match item {
                Item::Tool(call) => call.state == ToolState::Running && call.name == name,
                _ => false,
            }),
            None => None,
        };
        let index = index?;
        match self.items.get_mut(index) {
            Some(Item::Tool(call)) => Some(call),
            _ => None,
        }
    }

    /// How long the call that just finished took, removing its start mark.
    fn take_start(&mut self, call_id: &str, name: &str) -> Option<u64> {
        let index = self.starts.iter().position(|start| {
            if call_id.is_empty() {
                start.name == name
            } else {
                start.call_id == call_id
            }
        })?;
        let start = self.starts.remove(index);
        tool_line::elapsed_since(Some(start.at))
    }

    /// Commit the open streaming message, dropping an empty one.
    pub fn finish_stream(&mut self) {
        self.finish_reasoning();
        if !self.streaming.open {
            return;
        }
        let streaming = std::mem::take(&mut self.streaming);
        if !streaming.text.trim().is_empty() {
            self.items.push(Item::Assistant(streaming.text));
        }
    }

    fn last_assistant_index(&self) -> Option<usize> {
        self.items.iter().rposition(|item| matches!(item, Item::Assistant(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_accumulate_and_a_new_message_id_splits() {
        let mut transcript = Transcript::new();
        transcript.text_delta(Some("m1"), "one ");
        transcript.text_delta(Some("m1"), "two");
        transcript.text_delta(Some("m2"), "three");
        transcript.finish_stream();
        assert_eq!(
            transcript.items(),
            &[
                Item::Assistant("one two".to_string()),
                Item::Assistant("three".to_string())
            ]
        );
    }

    #[test]
    fn empty_message_is_not_published() {
        let mut transcript = Transcript::new();
        transcript.text_delta(Some("m1"), "draft");
        transcript.text_replace(Some("m1"), "");
        transcript.finish_stream();
        assert!(transcript.items().is_empty());
    }

    #[test]
    fn recent_assistant_text_keeps_answers_only() {
        let mut transcript = Transcript::new();
        transcript.push_user("look at https://user.example/");
        transcript.push_assistant("first https://one.example/");
        transcript.push_assistant("second https://two.example/");
        transcript.text_delta(Some("m9"), "streaming https://three.example/");
        assert_eq!(
            transcript.recent_assistant_text(2).collect::<Vec<_>>(),
            vec!["streaming https://three.example/", "second https://two.example/"]
        );
        assert_eq!(transcript.recent_assistant_text(8).count(), 3);
    }

    #[test]
    fn tool_done_updates_the_running_line() {
        let mut transcript = Transcript::new();
        transcript.tool_start("c1", "bash");
        transcript.tool_done("c1", "bash", None, None);
        let items = transcript.items();
        assert_eq!(items.len(), 1, "{items:?}");
        let Item::Tool(call) = &items[0] else {
            panic!("expected one tool line: {items:?}");
        };
        assert_eq!(call.name, "bash");
        assert_eq!(call.state, ToolState::Done);
    }

    #[test]
    fn open_text_reports_the_message_until_it_is_committed() {
        let mut transcript = Transcript::new();
        assert_eq!(transcript.open_text(), None, "nothing is open yet");

        transcript.text_delta(Some("m1"), "partial");
        assert_eq!(transcript.open_text(), Some("partial"));

        transcript.text_delta(Some("m1"), " more");
        assert_eq!(transcript.open_text(), Some("partial more"));

        // The committed item is the same text, so the renderer must stop
        // drawing the open one or the reply would appear twice.
        transcript.finish_stream();
        assert_eq!(transcript.open_text(), None);
        assert_eq!(transcript.items(), &[Item::Assistant("partial more".to_string())]);
    }

    #[test]
    fn open_text_hides_blank_and_retracted_messages() {
        let mut transcript = Transcript::new();
        transcript.text_delta(Some("m1"), "  \n\t");
        assert_eq!(transcript.open_text(), None, "whitespace is never published");

        // Deltas append, so the whitespace that was hidden on its own is now
        // part of the message; drawing anything but the raw buffer would make
        // the open row differ from the item `finish_stream` commits.
        transcript.text_delta(Some("m1"), "draft");
        assert_eq!(transcript.open_text(), Some("  \n\tdraft"));
        transcript.text_replace(Some("m1"), "");
        assert_eq!(transcript.open_text(), None, "a retraction must not be drawn");
    }

    #[test]
    fn reasoning_is_dropped_while_the_setting_is_off() {
        let mut transcript = Transcript::new();
        transcript.reasoning_delta("thinking about it");
        assert_eq!(
            transcript.open_reasoning(),
            None,
            "off means the client keeps nothing to draw"
        );
        transcript.reasoning_done();
        assert!(
            !transcript.items().iter().any(|item| matches!(item, Item::Reasoning(_))),
            "and nothing reaches the transcript: {:?}",
            transcript.items()
        );
    }

    #[test]
    fn reasoning_is_kept_apart_from_the_answer_when_the_setting_is_on() {
        let mut transcript = Transcript::new();
        transcript.set_show_reasoning(true);
        transcript.reasoning_delta("first I look");
        transcript.reasoning_delta(" at the file");
        assert_eq!(transcript.open_reasoning(), Some("first I look at the file"));
        transcript.reasoning_done();
        assert_eq!(transcript.open_reasoning(), None, "a boundary commits it");

        transcript.text_delta(None, "here is the answer");
        assert_eq!(
            transcript.open_text(),
            Some("here is the answer"),
            "the reasoning never joins the answer"
        );
        transcript.text_done();

        let items = transcript.items();
        assert_eq!(
            items,
            &[
                Item::Reasoning("first I look at the file".to_string()),
                Item::Assistant("here is the answer".to_string()),
            ],
            "reasoning precedes its answer in display order"
        );
    }

    #[test]
    fn a_tool_call_commits_the_reasoning_that_preceded_it() {
        let mut transcript = Transcript::new();
        transcript.set_show_reasoning(true);
        transcript.reasoning_delta("I need to read the file");
        transcript.tool_start("c1", "read");
        assert_eq!(transcript.open_reasoning(), None);
        assert_eq!(
            transcript.items().first(),
            Some(&Item::Reasoning("I need to read the file".to_string())),
            "the thought belongs above the call it caused: {:?}",
            transcript.items()
        );
    }

    #[test]
    fn turning_reasoning_off_retracts_a_half_written_thought() {
        let mut transcript = Transcript::new();
        transcript.set_show_reasoning(true);
        transcript.reasoning_delta("half a thought");
        transcript.set_show_reasoning(false);
        assert_eq!(transcript.open_reasoning(), None);
        transcript.reasoning_done();
        assert!(
            !transcript.items().iter().any(|item| matches!(item, Item::Reasoning(_))),
            "nothing half-finished stays on screen: {:?}",
            transcript.items()
        );
    }

    #[test]
    fn blank_reasoning_is_never_published() {
        let mut transcript = Transcript::new();
        transcript.set_show_reasoning(true);
        transcript.reasoning_delta("   ");
        transcript.reasoning_done();
        assert!(
            transcript.items().is_empty(),
            "whitespace alone is not a thought: {:?}",
            transcript.items()
        );
    }

    #[test]
    fn tool_done_without_a_start_still_renders() {
        let mut transcript = Transcript::new();
        transcript.tool_done("c1", "read", None, Some("no such file"));
        let items = transcript.items();
        assert_eq!(items.len(), 1, "{items:?}");
        let Item::Tool(call) = &items[0] else {
            panic!("expected one tool line: {items:?}");
        };
        assert_eq!(call.name, "read");
        assert_eq!(call.state, ToolState::Failed("no such file".to_string()));
    }
}
