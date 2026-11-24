//! `/effort`, the effort cycle key and the model key: the ladder the client
//! offers, what a chosen level means, and the report that puts the level on
//! the status line.
//!
//! The ladder is the client's list rather than the provider's. Which levels a
//! provider has is its own decision, so the client only refuses a name that is
//! not on the ladder at all; one the provider does not have comes back as an
//! error the transcript shows, and the status line keeps the level it had.

use super::{Action, App};
use crate::overlays::picker::{Kind, Picker, Row};

/// The levels the client offers, from the least reasoning to the most.
const LADDER: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The ladder as one line, for the refusal.
fn ladder_text() -> String {
    LADDER.join(", ")
}

/// What `/effort` says about a name the ladder does not hold.
fn outside(name: &str) -> String {
    format!("unknown reasoning effort `{name}`; use one of {}", ladder_text())
}

/// One line saying what a level asks the model to do, for a picker row.
fn describe(level: &str) -> &'static str {
    match level {
        "none" => "no reasoning before the answer",
        "minimal" => "the least reasoning this provider allows",
        "low" => "a little reasoning before answering",
        "medium" => "the provider's own balance",
        "high" => "more reasoning, and slower answers",
        "xhigh" => "a lot of reasoning for hard questions",
        "max" => "the most reasoning this provider allows",
        _ => "",
    }
}

impl App {
    /// `/effort`: the list when no level is typed, the level itself otherwise.
    ///
    /// A name that is not on the ladder is refused here, before anything leaves
    /// the client: the harness would only fail a request the user can see is
    /// impossible, and the refusal lists what is possible.
    pub(crate) fn run_effort(&mut self, arguments: &str) -> Option<Action> {
        let typed = arguments.trim();
        if typed.is_empty() {
            return self.open_efforts();
        }
        // The levels are lower case, and a level typed in capitals is the same
        // level: the client spells it back the one way the ladder does.
        let wanted = typed.to_ascii_lowercase();
        let Some(level) = LADDER.iter().copied().find(|entry| *entry == wanted) else {
            self.warn(outside(typed));
            return None;
        };
        self.ask_effort(level)
    }

    /// Open the ladder as a list, with the level in force marked and under the
    /// cursor.
    ///
    /// The rows are the client's own, so the list is complete the moment it
    /// opens: no request leaves and nothing says it is still loading. The
    /// cursor starts on the marked row because `enter` confirms the row it is
    /// on: opening anywhere else would move the session to a level the reader
    /// never chose. A session that reports no level, or a level the ladder does
    /// not hold, opens on the first row.
    fn open_efforts(&mut self) -> Option<Action> {
        let current = self.reasoning_effort.as_deref();
        let start = current
            .and_then(|level| LADDER.iter().position(|entry| *entry == level))
            .unwrap_or(0);
        let rows = LADDER
            .iter()
            .map(|level| {
                let marked = current == Some(*level);
                Row::new(*level, *level)
                    .marked(marked)
                    .with_preview(vec![describe(level).to_string()])
            })
            .collect();
        self.picker = Some(Picker::at(
            Kind::Efforts,
            "Select reasoning effort",
            rows,
            "the ladder is empty",
            start,
        ));
        None
    }

    /// `app.cycle_effort`: the next level up, wrapping past the last one.
    ///
    /// A level the client does not know — a provider may name its own — starts
    /// the walk at the first entry rather than refusing the key: the key is the
    /// quick way to change the effort, and an unknown level is exactly when the
    /// user has no list to step through.
    pub(crate) fn cycle_effort(&mut self) -> Option<Action> {
        // A level already waiting for the turn to end is the base, so two
        // presses during one turn walk two levels; then the last level asked
        // for, then the one the harness reports.
        let current = self
            .effort_after_turn
            .as_deref()
            .or(self.effort_requested.as_deref())
            .or(self.reasoning_effort.as_deref());
        let next = match current.and_then(|level| LADDER.iter().position(|entry| *entry == level)) {
            Some(index) => LADDER[(index + 1) % LADDER.len()],
            None => LADDER[0],
        };
        self.ask_effort(next)
    }

    /// The one way an effort change leaves the client.
    ///
    /// `/effort`, the cycle key and a chosen row of the list all come through
    /// here, so the lost connection and the running turn are answered in one
    /// place instead of three.
    ///
    /// A level asked for during a turn waits for it rather than being sent: the
    /// daemon answers an effort request only after the running turn releases
    /// the agent, so a request sent now would hold the bridge thread — every
    /// event and the cancel key with it — until the turn ended, and the SDK's
    /// request deadline would report a failure the daemon applies later anyway.
    pub(crate) fn ask_effort(&mut self, level: &str) -> Option<Action> {
        if self.disconnected {
            self.warn("the harness connection is gone; the reasoning effort cannot change");
            return None;
        }
        if self.pending_turn {
            // A notice, like every other answer to a command during a turn: the
            // level was accepted, it is the sending that waits.
            self.effort_after_turn = Some(level.to_string());
            self.transcript.push_notice(format!(
                "the turn is still running; the reasoning effort changes to {level} once it ends"
            ));
            return None;
        }
        self.effort_requested = Some(level.to_string());
        Some(Action::Effort(level.to_string()))
    }

    /// The one way a model change leaves the client.
    ///
    /// `/model <name>` and a chosen row of the list both come through here. A
    /// name asked for during a turn waits for it, for the reason
    /// [`App::ask_effort`] describes: the daemon answers after the turn releases
    /// the agent, and a request sent now would freeze the stream until then.
    /// A newer name replaces a waiting one.
    pub(crate) fn ask_model(&mut self, name: &str) -> Option<Action> {
        if self.pending_turn {
            self.model_after_turn = Some(name.to_string());
            self.transcript.push_notice(format!(
                "the turn is still running; the model changes to {name} once it ends"
            ));
            return None;
        }
        // The cycle keys step from here until an answer names the model: the
        // harness has not switched yet, and the status line must keep the model
        // it confirmed.
        self.model_requested = Some(name.to_string());
        Some(Action::Model(name.to_string()))
    }

    /// `app.pick_model`: the list `/model` opens.
    ///
    /// A key that opened a list asking the harness for rows over a connection
    /// that is gone would sit there swallowing keys, so the loss is reported
    /// instead.
    pub(crate) fn pick_model(&mut self) -> Option<Action> {
        if self.disconnected {
            self.warn("the harness connection is gone; there is no model to pick");
            return None;
        }
        self.open_models()
    }

    /// Fold one runtime report into the state the status line reads.
    ///
    /// `level` is the reasoning effort the report confirms, when it answers a
    /// `/effort`: the line that says the level was set is printed here, from
    /// the answer that carries it, so a request the harness refused never
    /// prints one. A report that names no effort still leaves the confirmed
    /// level on the status line, because the harness stored it before it was
    /// asked what it holds.
    ///
    /// The level a request asked for is dropped only by an answer naming that
    /// same level: a report about some other level says nothing about a request
    /// still waiting to be answered.
    pub fn runtime_info(
        &mut self,
        provider: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        level: Option<String>,
    ) {
        if let Some(provider) = provider {
            // The catalog belongs to the provider that listed it: a report
            // naming another one makes the entries the cycle checked against
            // the old list meaningless.
            if self.provider.as_deref() != Some(provider.as_str()) {
                self.model_catalog.clear();
                self.model_catalog_loaded = false;
                self.model_shortlist_skipped.clear();
            }
            self.provider = Some(provider);
        }
        if let Some(model) = model {
            // Only a report naming the model the client asked for is the answer
            // to that request; one naming another model says nothing about a
            // switch still on its way, so the cycle keys keep stepping from it.
            if self.model_requested.as_deref() == Some(model.as_str()) {
                self.model_requested = None;
            }
            self.model = Some(model);
        }
        if let Some(effort) = effort {
            self.reasoning_effort = Some(effort);
        } else if let Some(level) = &level {
            self.reasoning_effort = Some(level.clone());
        }
        if let Some(level) = level {
            // Only the answer that names the level the client asked for is the
            // answer to that request, so only then does the cycle key step from
            // the reported level again; a report confirming some other level
            // says nothing about a request still in flight.
            if self.effort_requested.as_deref() == Some(level.as_str()) {
                self.effort_requested = None;
            }
            self.transcript.push_notice(format!("reasoning effort set to {level}"));
        }
    }
}
