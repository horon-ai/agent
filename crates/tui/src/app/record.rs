//! Opt-in local recording and timed replay of the visible conversation.
//!
//! `/record` starts a recording and `/replay` plays one back. Both are explicit:
//! nothing is written, and nothing is replayed, unless the user asks. While a
//! replay runs it owns the transcript pane, because the conversation on screen
//! is the recording rather than the live session; the live conversation is held
//! aside and comes back when the replay is stopped or when live activity ends
//! it.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::ApiEvent;

use super::App;
use crate::recording::{self, Event, Outcome, Player, Recorder, Recording};
use crate::transcript::{ToolState, Transcript};

/// How far one seek key moves the replay.
const SEEK_STEP_MS: i64 = 5_000;

/// What a replay keeps while it owns the transcript.
pub(crate) struct ReplayState {
    player: Player,
    /// The line the pane starts with, drawn again after a seek rebuilds it.
    header: String,
    /// The conversation the client was showing before the replay started.
    live: Transcript,
    /// When the last tick was counted, so a frame loop's idle time is not lost.
    last_tick: Instant,
    /// Whether the end of the recording was already reported.
    announced_end: bool,
}

impl App {
    /// Start a recording at `path`, or say why it cannot.
    pub(crate) fn start_recording(&mut self, path: PathBuf) {
        if self.recorder.is_some() {
            self.warn("a recording is already running; /record stop ends it");
            return;
        }
        match Recorder::create(&path, recording::MAX_BYTES) {
            Ok(recorder) => {
                self.recorder = Some(recorder);
                let shown = path.display().to_string();
                self.transcript.push_notice(format!(
                    "recording to {shown}; approval answers, unsent drafts and tool bodies stay out; /record stop ends it"
                ));
            }
            Err(error) => self.warn(format!("recording: {error}")),
        }
    }

    /// Stop the recording in force and say where it went.
    pub(crate) fn stop_recording(&mut self) {
        let Some(recorder) = self.recorder.take() else {
            self.transcript.push_notice("no recording is running");
            return;
        };
        let path = recorder.path().display().to_string();
        let events = recorder.events();
        self.transcript
            .push_notice(format!("recording saved to {path} ({events} events)"));
    }

    /// Answer the recorder's questions: what it is writing, or that nothing is.
    fn recording_status(&mut self) {
        let status = match &self.recorder {
            Some(recorder) => format!(
                "recording to {} ({} events)",
                recorder.path().display(),
                recorder.events()
            ),
            None => "no recording is running".to_string(),
        };
        self.transcript.push_notice(status);
    }

    /// Append one event, stopping the recording when the file is full or broken.
    ///
    /// A recorder that cannot keep the next event is turned off rather than
    /// retried on every later event: the warning says where the file stopped,
    /// which is what the user needs to know to keep what it holds.
    pub(super) fn record_event(&mut self, event: Event) {
        let Some(recorder) = self.recorder.as_mut() else {
            return;
        };
        if let Err(error) = recorder.push(&event) {
            let path = recorder.path().display().to_string();
            let events = recorder.events();
            self.recorder = None;
            self.warn(format!("recording stopped at {path} ({events} events): {error}"));
        }
    }

    /// Record every answer the conversation published since the last scan.
    ///
    /// An answer is published by whichever event or key closes the stream, and
    /// the transcript commits it to the item list at that moment. Answers are
    /// counted rather than addressed by index: a clear or a session switch
    /// replaces the conversation, and a count that went down is a retraction,
    /// not new text. Reading the new answers back is what keeps a recorded
    /// answer the text the screen showed, including a message the provider
    /// split in two.
    ///
    /// A replay owns the transcript while it runs, so its scratch conversation
    /// never moves the count: the live conversation is the one being recorded.
    pub(super) fn record_new_assistants(&mut self) {
        if self.replay.is_some() {
            return;
        }
        let revision = self.transcript.revision();
        let published = self
            .transcript
            .items()
            .iter()
            .filter(|item| matches!(item, crate::transcript::Item::Assistant(_)))
            .count();
        if self.recorder.is_none() {
            self.record_scan = (revision, published);
            return;
        }
        if revision != self.record_scan.0 || published <= self.record_scan.1 {
            self.record_scan = (revision, published);
            return;
        }
        let fresh: Vec<String> = self
            .transcript
            .items()
            .iter()
            .filter_map(|item| match item {
                crate::transcript::Item::Assistant(text) => Some(text.clone()),
                _ => None,
            })
            .skip(self.record_scan.1)
            .collect();
        self.record_scan = (revision, published);
        for text in fresh {
            self.record_event(Event::Assistant(text));
        }
    }

    /// `/record`: start, stop or report the recording.
    pub(crate) fn run_record(&mut self, arguments: &str) {
        match arguments.trim() {
            "stop" => self.stop_recording(),
            "status" => self.recording_status(),
            "" | "start" => self.start_recording(recording::new_recording_path()),
            path => self.start_recording(recording::expand_path(path)),
        }
    }

    /// `/replay`: play a recording, or the newest one, or stop the replay.
    pub(crate) fn run_replay(&mut self, arguments: &str) {
        let argument = arguments.trim();
        if argument == "stop" {
            self.stop_replay("stopped");
            return;
        }
        if self.replay.is_some() {
            self.warn("a replay is already playing; /replay stop ends it");
            return;
        }
        // A turn that is still running would end the replay on its first event,
        // which reads as a failure rather than as "not now".
        if self.pending_turn {
            self.warn("the turn is still running; /replay works once it ends");
            return;
        }
        // A prompt is a decision about the live session: a replay that covered it
        // would leave it unanswered and unreachable until the replay stopped.
        if self.prompt.is_some() {
            self.warn("answer the prompt before replaying a recording");
            return;
        }
        let path = if argument.is_empty() {
            match recording::newest_recording_path() {
                Some(path) => path,
                None => {
                    let dir = recording::recordings_dir();
                    self.warn(format!("no recordings under {}", dir.display()));
                    return;
                }
            }
        } else {
            recording::expand_path(argument)
        };
        self.start_replay(&path);
    }

    /// Load `path` and put its events on screen.
    fn start_replay(&mut self, path: &Path) {
        let recording = match Recording::load(path) {
            Ok(recording) => recording,
            Err(error) => {
                self.warn(format!("replay: {error}"));
                return;
            }
        };
        if recording.events.is_empty() {
            self.warn(format!("replay: {} holds no events", path.display()));
            return;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let header = format!(
            "replaying {name}: {} events over {}; space pauses, left/right seek 5s, +/- speed, esc stops",
            recording.events.len(),
            millis(recording.duration_ms),
        );
        // A command popup over the recording would offer keys the replay owns.
        self.slash.dismiss();
        let live = std::mem::take(&mut self.transcript);
        let mut transcript = Transcript::new();
        transcript.set_show_reasoning(live.show_reasoning());
        transcript.set_tool_output_lines(live.tool_output_lines());
        transcript.push_notice(header.clone());
        self.transcript = transcript;
        let mut player = Player::new(recording);
        let due = player.advance(0);
        self.replay = Some(ReplayState {
            player,
            header,
            live,
            last_tick: Instant::now(),
            announced_end: false,
        });
        for event in &due {
            self.apply_replay_event(event);
        }
        // The counted heights describe the conversation that was on screen, not
        // the recording that replaced it.
        self.scroll.invalidate();
        self.scroll.to_bottom();
    }

    /// Whether a replay is on screen.
    pub(crate) fn replaying(&self) -> bool {
        self.replay.is_some()
    }

    /// Stop the replay and put the live conversation back.
    pub(crate) fn stop_replay(&mut self, reason: &str) {
        let Some(state) = self.replay.take() else {
            return;
        };
        let (position, duration, played, total) = (
            state.player.position_ms(),
            state.player.duration_ms(),
            state.player.played(),
            state.player.total(),
        );
        self.transcript = state.live;
        self.transcript.push_notice(format!(
            "replay {reason} at {} of {} ({played}/{total} events)",
            millis(position),
            millis(duration),
        ));
        self.scroll.invalidate();
        self.scroll.to_bottom();
    }

    /// Handle one key while a replay plays, answering whether it was consumed.
    ///
    /// The replay owns every key except the two that leave the client: a
    /// letter must not reach the draft behind the recording, so nothing typed
    /// here can be sent by a key meant for the playback.
    pub(crate) fn replay_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
        {
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.stop_replay("stopped"),
            KeyCode::Char(' ') => self.toggle_replay_pause(),
            KeyCode::Left => self.seek_replay(-SEEK_STEP_MS),
            KeyCode::Right => self.seek_replay(SEEK_STEP_MS),
            KeyCode::Home => self.seek_replay_to(0),
            KeyCode::End => self.seek_replay_to(u64::MAX),
            KeyCode::Char('+') | KeyCode::Char('=') => self.change_replay_speed(true),
            KeyCode::Char('-') | KeyCode::Char('_') => self.change_replay_speed(false),
            _ => {}
        }
        true
    }

    /// Count the wall clock since the last frame and play what became due.
    ///
    /// Called once per frame loop. It reports whether the screen changed, so an
    /// idle pause costs nothing and an event that landed gets a frame.
    pub(crate) fn tick_replay(&mut self) -> bool {
        let Some(state) = self.replay.as_ref() else {
            return false;
        };
        if state.player.paused() {
            return false;
        }
        let now = Instant::now();
        let elapsed = now
            .saturating_duration_since(state.last_tick)
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if elapsed == 0 {
            return false;
        }
        self.advance_replay(elapsed)
    }

    /// Advance the replay by `real_ms` and draw the events that became due.
    ///
    /// The frame loop feeds it the time between frames; a test feeds it the time
    /// it means, which is what keeps playback deterministic without a thread.
    pub(crate) fn advance_replay(&mut self, real_ms: u64) -> bool {
        let due = match self.replay.as_mut() {
            Some(state) => {
                if state.player.paused() {
                    return false;
                }
                let due = state.player.advance(real_ms);
                state.last_tick = Instant::now();
                due
            }
            None => return false,
        };
        let changed = !due.is_empty();
        for event in &due {
            self.apply_replay_event(event);
        }
        // The end is reported once, and the replay stays on screen so the last
        // rows can still be read and seeked back over.
        let ended = self
            .replay
            .as_ref()
            .is_some_and(|state| state.player.is_finished() && !state.announced_end);
        if ended {
            if let Some(state) = self.replay.as_mut() {
                state.announced_end = true;
            }
            let (position, duration, played, total) = self.replay_progress();
            self.transcript.push_notice(format!(
                "replay ended at {} of {} ({played}/{total} events); /replay stop returns to the conversation",
                millis(position),
                millis(duration),
            ));
        }
        changed || ended
    }

    /// Turn the replay's clock on or off, saying where it stopped.
    pub(crate) fn toggle_replay_pause(&mut self) {
        let paused = match self.replay.as_mut() {
            Some(state) => {
                let paused = state.player.toggle_pause();
                state.last_tick = Instant::now();
                paused
            }
            None => return,
        };
        let (position, duration, played, total) = self.replay_progress();
        let word = if paused { "paused" } else { "resumed" };
        self.transcript.push_notice(format!(
            "replay {word} at {} of {} ({played}/{total} events)",
            millis(position),
            millis(duration),
        ));
        self.scroll.to_bottom();
    }

    /// Move the replay by `delta_ms`, forwards or backwards.
    pub(crate) fn seek_replay(&mut self, delta_ms: i64) {
        let Some(state) = self.replay.as_ref() else {
            return;
        };
        let position = state.player.position_ms();
        let target = if delta_ms < 0 {
            position.saturating_sub(delta_ms.unsigned_abs())
        } else {
            position.saturating_add(delta_ms as u64)
        };
        self.seek_replay_to(target);
    }

    /// Jump the replay to `target_ms` and rebuild the screen at that point.
    ///
    /// Rebuilding rather than patching is what makes a seek backwards exact: the
    /// pane shows the recording as it stood at that time, with no rows left over
    /// from a position the user has left.
    pub(crate) fn seek_replay_to(&mut self, target_ms: u64) {
        let due = match self.replay.as_mut() {
            Some(state) => {
                let due = state.player.seek(target_ms);
                state.last_tick = Instant::now();
                state.announced_end = state.player.is_finished();
                due
            }
            None => return,
        };
        self.rebuild_replay(&due);
        let (position, duration, played, total) = self.replay_progress();
        self.transcript.push_notice(format!(
            "replay seeked to {} of {} ({played}/{total} events)",
            millis(position),
            millis(duration),
        ));
        self.scroll.to_bottom();
    }

    /// Change the replay speed one step, saying the speed now in force.
    pub(crate) fn change_replay_speed(&mut self, faster: bool) {
        let speed = match self.replay.as_mut() {
            Some(state) => {
                if faster {
                    state.player.speed_up()
                } else {
                    state.player.speed_down()
                }
            }
            None => return,
        };
        self.transcript.push_notice(format!("replay speed {speed}x"));
    }

    /// Where the replay stands: position, length, played and total events.
    fn replay_progress(&self) -> (u64, u64, usize, usize) {
        match self.replay.as_ref() {
            Some(state) => (
                state.player.position_ms(),
                state.player.duration_ms(),
                state.player.played(),
                state.player.total(),
            ),
            None => (0, 0, 0, 0),
        }
    }

    /// Replace the pane with the replay's rows up to the position a seek chose.
    fn rebuild_replay(&mut self, due: &[Event]) {
        let Some(state) = self.replay.as_ref() else {
            return;
        };
        let header = state.header.clone();
        let mut transcript = Transcript::new();
        transcript.set_show_reasoning(state.live.show_reasoning());
        transcript.set_tool_output_lines(state.live.tool_output_lines());
        transcript.push_notice(header);
        self.transcript = transcript;
        for event in due {
            self.apply_replay_event(event);
        }
        self.scroll.invalidate();
    }

    /// Draw one recorded event the way the live conversation drew it.
    fn apply_replay_event(&mut self, event: &Event) {
        match event {
            Event::User(text) => self.transcript.push_user(text.clone()),
            Event::Assistant(text) => self.transcript.push_assistant(text.clone()),
            Event::Warning => self.transcript.push_recorded_warning(),
            Event::Tool { name, outcome } => self.transcript.push_recorded_tool(name, *outcome == Outcome::Failed),
        }
    }
}

/// Whether a live event ends the replay: anything that changes the conversation
/// means the session is doing something the recording cannot show.
pub(crate) fn ends_replay(event: &ApiEvent) -> bool {
    matches!(
        event,
        ApiEvent::TextDelta { .. }
            | ApiEvent::TextDone { .. }
            | ApiEvent::TextReplace { .. }
            | ApiEvent::ReasoningDelta { .. }
            | ApiEvent::ReasoningDone { .. }
            | ApiEvent::ToolStart { .. }
            | ApiEvent::ToolInputDelta { .. }
            | ApiEvent::ToolDone { .. }
            | ApiEvent::TurnDone { .. }
            | ApiEvent::TurnStopped { .. }
            | ApiEvent::BackgroundOutput { .. }
            | ApiEvent::ApprovalPending { .. }
            | ApiEvent::ApprovalResolved { .. }
            | ApiEvent::AskRequested { .. }
            | ApiEvent::Error { .. }
    )
}

/// A duration in milliseconds as seconds, to one decimal.
fn millis(ms: u64) -> String {
    format!("{:.1}s", ms as f64 / 1000.0)
}

/// The outcome a finished call records, when the recording keeps it.
///
/// A call the approval gate held is left out: it is an approval matter, and the
/// default recording keeps no part of one.
pub(crate) fn recorded_outcome(state: &ToolState) -> Option<Outcome> {
    match state {
        ToolState::Done => Some(Outcome::Done),
        ToolState::Failed(message) if !message.contains(crate::tool_line::GATE_REFUSAL) => Some(Outcome::Failed),
        _ => None,
    }
}
