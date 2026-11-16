//! The conversation as it is drawn: user messages, streamed assistant text,
//! and one line per tool call.
//!
//! The transcript owns the streaming buffer rather than the app, because the
//! rules about it (a delta for a new message closes the previous one, an empty
//! replacement retracts text) are the same everywhere and are easy to get wrong
//! at the call site.

use std::time::Instant;

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
}

impl ToolState {
    /// The short word shown after the tool name.
    pub fn label(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed(_) => "failed",
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
    /// Whether reasoning is kept. Off by default, as
    /// `transcript.show_reasoning` resolves; the option is read at the event,
    /// so flipping it affects the next delta rather than only the next turn.
    show_reasoning: bool,
    /// Rows of a finished call's output kept for the preview, as
    /// `transcript.tool_output_lines` resolves. Zero hides the preview.
    tool_output_lines: usize,
    /// When each call started, keyed by call id and dropped when it ends.
    starts: Vec<Start>,
}

/// When one call started.
#[derive(Debug)]
struct Start {
    call_id: String,
    name: String,
    at: Instant,
}

impl Default for Transcript {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            streaming: Streaming::default(),
            reasoned: Reasoned::default(),
            show_reasoning: false,
            tool_output_lines: DEFAULT_OUTPUT_LINES,
            starts: Vec::new(),
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

    /// Keep, or stop keeping, the model's reasoning.
    ///
    /// Turning it off drops reasoning already being assembled, because a
    /// setting the user just switched off must not leave half a thought on
    /// screen; turning it on keeps what the answer has shown so far.
    pub fn set_show_reasoning(&mut self, show: bool) {
        self.show_reasoning = show;
        if !show {
            self.reasoned = Reasoned::default();
        }
    }

    /// Whether reasoning is being kept.
    pub fn show_reasoning(&self) -> bool {
        self.show_reasoning
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

    /// Record a user message, closing any partly streamed reply first.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.finish_stream();
        self.items.push(Item::User(text.into()));
    }

    /// Record an informational line.
    pub fn push_notice(&mut self, text: impl Into<String>) {
        self.finish_stream();
        self.items.push(Item::Notice(text.into()));
    }

    /// Record a warning line.
    pub fn push_warning(&mut self, text: impl Into<String>) {
        self.finish_stream();
        self.items.push(Item::Warning(text.into()));
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
    pub fn tool_done(&mut self, call_id: &str, name: &str, output: Option<&str>, error: Option<&str>) {
        let state = match error {
            Some(message) if !message.is_empty() => ToolState::Failed(message.to_string()),
            _ => ToolState::Done,
        };
        let elapsed = self.take_start(call_id, name);
        let preview = output.and_then(|text| tool_line::cut_output(text, self.tool_output_lines));
        let Some(call) = self.call_for(call_id, name) else {
            let mut call = ToolCall::new(call_id, name);
            call.state = state;
            call.duration_ms = elapsed;
            call.output = preview;
            self.items.push(Item::Tool(call));
            return;
        };
        // A call finishes once, but the same result can reach the client twice
        // (a reconnect replays it). The second one must not overwrite the first:
        // it arrives after the start mark was taken, so its duration is empty,
        // and a repeat of a success would erase a recorded failure.
        let keep_failure = matches!(call.state, ToolState::Failed(_)) && !matches!(state, ToolState::Failed(_));
        if !keep_failure {
            call.state = state;
        }
        if call.duration_ms.is_none() {
            call.duration_ms = elapsed;
        }
        if call.output.is_none() {
            call.output = preview;
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
