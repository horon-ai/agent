//! A short, opt-in record of the work a turn is doing.
//!
//! The bridge already forwards the facts this needs: `ToolStart`/`ToolDone` and
//! `BackgroundProgress`. This module turns them into a few live notes and two
//! counters, and nothing else. A note is drawn above the open stream and never
//! becomes a message, a harness request, or a recorded transcript row, so
//! turning the setting on cannot change what the model sees.
//!
//! A turn shows at most [`MAX_LIVE_NOTES`] live notes. Later ones are counted,
//! not drawn, and the closing line reports how many were omitted. The closing
//! line itself is always the last thing a turn adds when notes are on, so the
//! display ends with accurate totals instead of an unexplained silence.
//!
//! The switch is checked before any event is read, so the default-off client
//! copies no call id, inserts into no set, and allocates nothing per event.

use std::collections::{BTreeSet, VecDeque};

use horon_agent_sdk::api::ApiEvent;

/// Live notes one turn may show before later ones are only counted.
pub(crate) const MAX_LIVE_NOTES: u32 = 4;

/// Background jobs tracked at once. A busy session can start more than the
/// screen could ever show, and the counters keep reporting what it did.
const MAX_JOBS: usize = 8;

/// Longest note line, in characters.
const LINE_CHARS: usize = 160;

/// One background job's latest state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Job {
    task_id: String,
    label: String,
    summary: String,
    done: bool,
}

/// What one turn has done so far, and what the screen was allowed to show.
#[derive(Debug, Default)]
pub(crate) struct WorkNotes {
    /// The session the counters describe; a different one replaces them.
    session: Option<String>,
    /// Whether `transcript.work_notes` asked for the summary.
    enabled: bool,
    tools_running: BTreeSet<String>,
    tools_finished: u64,
    jobs: VecDeque<Job>,
    jobs_finished: u64,
    shown: u32,
    omitted: u64,
}

impl WorkNotes {
    /// Forget the turn's counters but keep the switch as it is.
    pub(crate) fn reset(&mut self) {
        let enabled = self.enabled;
        *self = Self {
            enabled,
            ..Self::default()
        };
    }

    /// Turn the summary on or off. Turning it off drops the partial turn too,
    /// so turning it on again cannot show totals from before the switch.
    pub(crate) fn set_enabled(&mut self, on: bool) {
        if on {
            self.enabled = true;
        } else {
            *self = Self::default();
        }
    }

    /// Read one event, returning the line to draw when there is one.
    pub(crate) fn observe(&mut self, event: &ApiEvent) -> Option<String> {
        if !self.enabled {
            return None;
        }
        match event {
            ApiEvent::ToolStart { session_id, call_id, .. } => {
                if self.belongs(session_id) {
                    self.tools_running.insert(call_id.clone());
                }
                None
            }
            ApiEvent::ToolDone { session_id, call_id, .. } => {
                if self.belongs(session_id) && self.tools_running.remove(call_id) {
                    self.tools_finished += 1;
                }
                None
            }
            ApiEvent::BackgroundProgress {
                session_id,
                task_id,
                label,
                summary,
                done,
                ..
            } => {
                if !self.belongs(session_id) || !self.update_job(task_id, label, summary, *done) {
                    return None;
                }
                let line = if *done {
                    format!("work · {label} · finished")
                } else {
                    format!("work · {label} · {summary}")
                };
                self.admit(line)
            }
            ApiEvent::TurnDone { session_id, .. } | ApiEvent::TurnStopped { session_id, .. } => {
                if !self.belongs(session_id) {
                    return None;
                }
                self.finish()
            }
            _ => None,
        }
    }

    /// Keep the counters on one session: a different id starts them over.
    fn belongs(&mut self, session_id: &str) -> bool {
        if session_id.is_empty() {
            return false;
        }
        if self.session.as_deref() != Some(session_id) {
            self.reset();
            self.session = Some(session_id.to_string());
        }
        true
    }

    /// Apply one progress tick, answering whether it changed anything visible.
    fn update_job(&mut self, task_id: &str, label: &str, summary: &str, done: bool) -> bool {
        if let Some(job) = self.jobs.iter_mut().find(|job| job.task_id == task_id) {
            let changed = job.summary != summary || job.done != done || job.label != label;
            if done && !job.done {
                self.jobs_finished += 1;
            }
            job.label = label.to_string();
            job.summary = summary.to_string();
            job.done = done;
            return changed;
        }
        if done {
            self.jobs_finished += 1;
        }
        if self.jobs.len() == MAX_JOBS {
            self.jobs.pop_front();
        }
        self.jobs.push_back(Job {
            task_id: task_id.to_string(),
            label: label.to_string(),
            summary: summary.to_string(),
            done,
        });
        true
    }

    /// Take one live line if the turn's budget still has room.
    fn admit(&mut self, line: String) -> Option<String> {
        if self.shown >= MAX_LIVE_NOTES {
            self.omitted += 1;
            return None;
        }
        self.shown += 1;
        Some(bounded(&line))
    }

    /// Close the turn with its counters, then start the next one empty.
    fn finish(&mut self) -> Option<String> {
        let running = self.tools_running.len() as u64;
        self.tools_running.clear();
        self.tools_finished += running;
        let line = if self.tools_finished > 0
            || self.jobs_finished > 0
            || !self.jobs.is_empty()
            || self.omitted > 0
        {
            let mut text = format!(
                "work · {} tool calls · {} background jobs finished",
                self.tools_finished, self.jobs_finished
            );
            if self.omitted > 0 {
                text.push_str(&format!(" · {} notes omitted", self.omitted));
            }
            Some(bounded(&text))
        } else {
            None
        };
        self.tools_finished = 0;
        self.jobs_finished = 0;
        self.omitted = 0;
        self.shown = 0;
        self.jobs.clear();
        line
    }
}

/// One line, cut to [`LINE_CHARS`] characters with an ellipsis when cut.
fn bounded(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= LINE_CHARS {
        return flat;
    }
    let mut cut: String = flat.chars().take(LINE_CHARS.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

impl crate::app::App {
    /// Feed one event to the work summary, drawing the line it produces when
    /// the setting asks for it. A replay is not the turn happening now, so its
    /// events never move the summary.
    pub(crate) fn note_work(&mut self, event: &ApiEvent) {
        if self.replay.is_some() {
            return;
        }
        if let Some(line) = self.work_notes.observe(event) {
            self.transcript.push_live_notice(line);
        }
    }
}

#[cfg(test)]
#[path = "work_notes_tests.rs"]
mod tests;
