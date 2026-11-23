//! Detects pathological repetition of visible prose while a response streams.
//!
//! The guard is deliberately narrow. It fires only when the exact same long
//! sentence appears several times in a row, and it looks only at prose:
//! fenced code, table rows, headings and list items are skipped, and any other
//! sentence between two copies breaks the run. A model that makes the same point twice, repeats a
//! line of code, or fills a table with similar rows never trips it.
//!
//! Text arrives in arbitrary pieces, so the guard keeps the unfinished tail of
//! a sentence between calls and judges a sentence only once it is complete.

/// Only a sentence at least this long can be a repetition: short lines repeat in
/// ordinary prose. A shorter sentence still breaks a run of identical ones.
const MIN_SENTENCE_CHARS: usize = 40;
/// Fragments this short, such as `e.g.`, come from splitting at an abbreviation
/// and are ignored altogether.
const MIN_FRAGMENT_CHARS: usize = 12;
/// The same sentence this many times in a row is treated as a repetition loop.
pub(in crate::agent) const STOP_AFTER: usize = 4;

#[derive(Default)]
pub struct ProseRepeatGuard {
    tail: String,
    cursor: usize,
    scan_pos: usize,
    state: ProseState,
}

#[derive(Default)]
struct ProseState {
    in_fence: bool,
    last: String,
    has_last: bool,
    run: usize,
    fired: bool,
}

impl ProseRepeatGuard {
    /// Discard all buffered text and repetition state (e.g. on a provider retry rollback).
    pub fn reset_all(&mut self) {
        self.tail.clear();
        self.cursor = 0;
        self.scan_pos = 0;
        self.state.in_fence = false;
        self.state.has_last = false;
        self.state.run = 0;
        self.state.fired = false;
    }

    /// Feed the next piece of assistant text. Returns the repeated sentence the
    /// first time it reaches [`STOP_AFTER`] consecutive occurrences, and never
    /// again afterwards.
    pub fn feed(&mut self, delta: &str) -> Option<String> {
        if self.state.fired || delta.is_empty() {
            return None;
        }
        self.tail.push_str(delta);
        while let Some(end) = self.next_boundary() {
            let segment = &self.tail[self.cursor..end];
            self.cursor = end;
            if let Some(sentence) = self.state.observe(segment) {
                self.state.fired = true;
                self.tail.clear();
                self.cursor = 0;
                self.scan_pos = 0;
                return Some(sentence);
            }
        }
        if self.cursor == self.tail.len() {
            self.tail.clear();
            self.cursor = 0;
            self.scan_pos = 0;
        } else if self.cursor >= 4096 {
            self.tail.drain(..self.cursor);
            self.scan_pos = self.scan_pos.saturating_sub(self.cursor);
            self.cursor = 0;
        }
        None
    }

    /// Byte length of the first complete segment in the tail: a line, or a
    /// sentence ended by `.`, `!` or `?` followed by whitespace. A terminator at
    /// the very end of the tail is not a boundary yet, because the next piece
    /// may continue the word.
    fn next_boundary(&mut self) -> Option<usize> {
        let bytes = self.tail.as_bytes();
        while self.scan_pos < bytes.len() {
            let Some(rel) = bytes[self.scan_pos..]
                .iter()
                .position(|&b| matches!(b, b'\n' | b'.' | b'!' | b'?'))
            else {
                self.scan_pos = bytes.len();
                return None;
            };
            let index = self.scan_pos + rel;
            if bytes[index] == b'\n' {
                self.scan_pos = index + 1;
                return Some(index + 1);
            }
            let next_pos = index + 1;
            if next_pos >= bytes.len() {
                self.scan_pos = index;
                return None;
            }
            let next_byte = bytes[next_pos];
            let next_is_ws = if matches!(next_byte, b' ' | b'\n' | b'\r' | b'\t') {
                true
            } else if next_byte < 0x80 && next_byte != 0x0B && next_byte != 0x0C {
                false
            } else {
                self.tail[next_pos..].chars().next().is_some_and(char::is_whitespace)
            };
            self.scan_pos = next_pos;
            if next_is_ws {
                return Some(next_pos);
            }
        }
        None
    }
}

impl ProseState {
    fn observe(&mut self, segment: &str) -> Option<String> {
        let trimmed = segment.trim();
        if trimmed.starts_with("```") {
            self.in_fence = !self.in_fence;
            self.reset();
            return None;
        }
        if self.in_fence || is_structure(trimmed) {
            self.reset();
            return None;
        }
        if trimmed.len() < MIN_FRAGMENT_CHARS {
            return None;
        }
        let length = normalized_char_len(trimmed);
        if length < MIN_FRAGMENT_CHARS {
            return None;
        }
        if length < MIN_SENTENCE_CHARS {
            self.reset();
            return None;
        }
        if self.has_last && matches_normalized(&self.last, trimmed) {
            self.run += 1;
            if self.run >= STOP_AFTER {
                self.has_last = false;
                return Some(std::mem::take(&mut self.last));
            }
            return None;
        }
        self.last.clear();
        write_normalized(&mut self.last, trimmed);
        self.has_last = true;
        self.run = 1;
        None
    }

    fn reset(&mut self) {
        self.has_last = false;
        self.run = 0;
    }
}

fn normalized_char_len(trimmed: &str) -> usize {
    let is_ascii = trimmed.is_ascii();
    let mut len = 0usize;
    for (idx, word) in trimmed.split_whitespace().enumerate() {
        if idx > 0 {
            len += 1;
        }
        len += if is_ascii { word.len() } else { word.chars().count() };
    }
    len
}

fn matches_normalized(last: &str, trimmed: &str) -> bool {
    let mut words = trimmed.split_whitespace();
    let Some(first) = words.next() else {
        return last.is_empty();
    };
    let Some(mut rest) = last.strip_prefix(first) else {
        return false;
    };
    for word in words {
        let Some(after_space) = rest.strip_prefix(' ') else {
            return false;
        };
        let Some(after_word) = after_space.strip_prefix(word) else {
            return false;
        };
        rest = after_word;
    }
    rest.is_empty()
}

fn write_normalized(out: &mut String, trimmed: &str) {
    for (idx, word) in trimmed.split_whitespace().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        out.push_str(word);
    }
}

/// Lines that carry structure rather than prose: table rows, headings, quotes
/// and list items.
fn is_structure(line: &str) -> bool {
    if line.starts_with(['|', '#', '>']) || line.starts_with("- ") || line.starts_with("* ") {
        return true;
    }
    let digits = line.as_bytes().iter().take_while(|b| b.is_ascii_digit()).count();
    digits > 0 && line[digits..].starts_with(['.', ')'])
}

impl super::super::Agent {
    /// Record limit provenance, persist the stop notice, and finalize turn state
    /// while preserving any streamed prose prefix for callers.
    pub(in crate::agent) fn finish_prose_repeat_stop(
        &mut self,
        text_content: &str,
        sentence: &str,
    ) -> anyhow::Result<String> {
        let notice = format!(
            "Stopped: the response repeated the same sentence {} times in a row: \"{sentence}\"",
            STOP_AFTER
        );
        self.observe_stop_cause(horon_agent_agent_runtime::StopProvenance::Limit);
        self.record_stream_rule_stop(&notice);
        self.save_session()?;
        let final_text = if text_content.trim().is_empty() {
            notice.clone()
        } else {
            format!("{text_content}\n\n{notice}")
        };
        self.turn_state.set_final_text(&final_text);
        self.turn_state.finish(super::super::TurnOutcome::Stop)?;
        Ok(notice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::test_sandbox::AuthTestSandbox;
    use crate::message::{Message, StreamEvent, ToolDefinition};
    use crate::provider::{EventStream, Provider};
    use crate::tool::Registry;
    use async_trait::async_trait;
    use horon_agent_agent_runtime::StopProvenance;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    const SENTENCE: &str = "The migration is complete and every check passed without errors. ";

    fn feed_in_pieces(guard: &mut ProseRepeatGuard, text: &str, piece: usize) -> usize {
        let chars: Vec<char> = text.chars().collect();
        chars
            .chunks(piece)
            .filter(|chunk| guard.feed(&chunk.iter().collect::<String>()).is_some())
            .count()
    }

    #[test]
    fn the_same_sentence_across_stream_boundaries_fires_exactly_once() {
        for piece in [1, 7, 31, 500] {
            let mut guard = ProseRepeatGuard::default();
            let text = SENTENCE.repeat(STOP_AFTER + 3);
            assert_eq!(feed_in_pieces(&mut guard, &text, piece), 1, "piece size {piece}");
        }
    }

    #[test]
    fn fewer_repeats_than_the_threshold_do_not_fire() {
        let mut guard = ProseRepeatGuard::default();
        assert_eq!(feed_in_pieces(&mut guard, &SENTENCE.repeat(STOP_AFTER - 1), 9), 0);
    }

    #[test]
    fn code_tables_lists_and_ordinary_talking_points_never_fire() {
        let code = format!(
            "```rust\n{}```\n",
            "let value = compute_the_expensive_thing(input);\n".repeat(10)
        );
        let table = "| name | value | description of the column |\n".repeat(10);
        let list = "- This item restates the identical talking point for the reader\n".repeat(10);
        let numbered = "1. This item restates the identical talking point for the reader\n".repeat(10);
        let varied = format!(
            "{} We checked the parser. {} We checked the writer. {} We checked the reader. {} ",
            SENTENCE.trim(),
            SENTENCE.trim(),
            SENTENCE.trim(),
            SENTENCE.trim()
        );
        for (name, text) in [
            ("code", code),
            ("table", table),
            ("list", list),
            ("numbered", numbered),
            ("varied prose", varied),
        ] {
            let mut guard = ProseRepeatGuard::default();
            assert_eq!(feed_in_pieces(&mut guard, &text, 13), 0, "{name}");
        }
    }

    struct ScriptedProseProvider {
        streams: Mutex<VecDeque<Vec<StreamEvent>>>,
    }

    impl ScriptedProseProvider {
        fn new(events: Vec<StreamEvent>) -> Arc<Self> {
            Arc::new(Self {
                streams: Mutex::new(VecDeque::from([events])),
            })
        }
    }

    #[async_trait]
    impl Provider for ScriptedProseProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> anyhow::Result<EventStream> {
            let events = self.streams.lock().unwrap().pop_front().unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
        }

        fn name(&self) -> &str {
            "prose-repeat-stub"
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(Self {
                streams: Mutex::new(VecDeque::new()),
            })
        }
    }

    fn enable_repeat_guard() -> AuthTestSandbox {
        let sandbox = AuthTestSandbox::new().unwrap();
        std::fs::write(sandbox.root().join("config.toml"), "[features]\nrepeat_guard = true\n").unwrap();
        crate::config::Config::invalidate_cache();
        sandbox
    }

    #[tokio::test]
    async fn streaming_repeat_stop_records_limit_provenance_and_preserves_prefix() {
        let _sandbox = enable_repeat_guard();
        let events = vec![
            StreamEvent::TextDelta("Initial context. ".to_string()),
            StreamEvent::TextDelta(SENTENCE.repeat(STOP_AFTER)),
            StreamEvent::TextDelta("Trailing text after repeat.".to_string()),
            StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            },
        ];
        let mut agent = super::super::super::Agent::new(ScriptedProseProvider::new(events), Registry::empty());
        agent.set_memory_enabled(false);
        let (tx, mut rx) = mpsc::unbounded_channel();

        agent
            .run_once_streaming_mpsc("go", vec![], None, tx)
            .await
            .expect("repeat stop ends turn cleanly");

        let observed: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(observed.iter().any(|ev| matches!(
            ev,
            crate::protocol::ServerEvent::TurnStopped {
                reason: crate::protocol::TurnStopReason::LimitReached,
                message,
                ..
            } if message.contains("repeated the same sentence 4 times")
        )));
        assert_eq!(
            agent.current_stop().map(|stop| stop.provenance()),
            Some(StopProvenance::Limit)
        );
        assert_eq!(
            agent.turn_state().outcome(),
            Some(super::super::super::TurnOutcome::Stop)
        );
        let final_text = agent.turn_state().final_text();
        assert!(final_text.starts_with("Initial context."), "{final_text}");
        assert!(
            final_text.contains("repeated the same sentence 4 times"),
            "{final_text}"
        );
        assert!(!final_text.contains("Trailing text after repeat"), "{final_text}");
    }

    #[tokio::test]
    async fn retry_rollback_resets_prose_repeat_guard_in_streaming_turn() {
        let _sandbox = enable_repeat_guard();
        let events = vec![
            StreamEvent::TextDelta(SENTENCE.repeat(STOP_AFTER - 1)),
            StreamEvent::RetryRollback { attempt: 1, max: 3 },
            StreamEvent::TextDelta(SENTENCE.to_string()),
            StreamEvent::TextDelta("All good after rollback.".to_string()),
            StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            },
        ];
        let mut agent = super::super::super::Agent::new(ScriptedProseProvider::new(events), Registry::empty());
        agent.set_memory_enabled(false);
        let (tx, mut rx) = mpsc::unbounded_channel();

        agent
            .run_once_streaming_mpsc("go", vec![], None, tx)
            .await
            .expect("single copy after rollback must complete normally");

        let observed: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(!observed.iter().any(|ev| matches!(
            ev,
            crate::protocol::ServerEvent::TurnStopped {
                reason: crate::protocol::TurnStopReason::LimitReached,
                ..
            }
        )));
        assert_eq!(
            agent.current_stop().map(|stop| stop.provenance()),
            Some(StopProvenance::Completed)
        );
        assert_eq!(
            agent.turn_state().final_text(),
            format!("{SENTENCE}All good after rollback.")
        );
    }

    #[tokio::test]
    async fn blocking_run_turn_stops_on_repeated_prose_and_resets_on_rollback() {
        let _sandbox = enable_repeat_guard();
        // Part 1: 3 copies + RetryRollback + 1 copy must NOT trigger stop.
        let ok_events = vec![
            StreamEvent::TextDelta(SENTENCE.repeat(STOP_AFTER - 1)),
            StreamEvent::RetryRollback { attempt: 1, max: 3 },
            StreamEvent::TextDelta(SENTENCE.to_string()),
            StreamEvent::TextDelta("Finished cleanly.".to_string()),
            StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            },
        ];
        let mut agent_ok = super::super::super::Agent::new(ScriptedProseProvider::new(ok_events), Registry::empty());
        agent_ok.set_memory_enabled(false);
        let reply = agent_ok.run_once_capture("go").await.expect("completes cleanly");
        assert_eq!(reply, format!("{SENTENCE}Finished cleanly."));
        assert_eq!(
            agent_ok.current_stop().map(|stop| stop.provenance()),
            Some(StopProvenance::Completed)
        );

        // Part 2: 4 copies in blocking run_turn stops early with Limit provenance.
        let repeat_events = vec![
            StreamEvent::TextDelta("Prefix line. ".to_string()),
            StreamEvent::TextDelta(SENTENCE.repeat(STOP_AFTER)),
            StreamEvent::TextDelta("Should not be reached.".to_string()),
            StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            },
        ];
        let mut agent_stop =
            super::super::super::Agent::new(ScriptedProseProvider::new(repeat_events), Registry::empty());
        agent_stop.set_memory_enabled(false);
        let stopped = agent_stop
            .run_once_capture("go")
            .await
            .expect("repeat stop returns partial + notice");
        assert!(stopped.starts_with("Prefix line."), "{stopped}");
        assert!(stopped.contains("repeated the same sentence 4 times"), "{stopped}");
        assert!(!stopped.contains("Should not be reached"), "{stopped}");
        assert_eq!(
            agent_stop.current_stop().map(|stop| stop.provenance()),
            Some(StopProvenance::Limit)
        );
    }
}
