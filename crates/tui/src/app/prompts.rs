//! Prompts, approvals and questions: what is on screen, what waits behind
//! it, and the words the status line uses for a pending decision.

use crate::transcript::Decision;

use super::App;
use crate::prompts::Prompt;

impl App {
    /// Drop the questions a turn was parked on.
    ///
    /// The `ask` tool waits on the connection for as long as its turn runs, so a
    /// question outliving its turn can no longer be answered: the daemon has
    /// already stopped waiting. Approvals are different and are left alone —
    /// their request stays queued with its own deadline.
    pub(crate) fn withdraw_asks(&mut self, reason: &str) {
        let mut withdrawn = 0;
        if matches!(self.prompt, Some(Prompt::Ask(_))) {
            self.prompt = None;
            withdrawn += 1;
        }
        let before = self.waiting_prompts.len();
        self.waiting_prompts.retain(|prompt| !matches!(prompt, Prompt::Ask(_)));
        withdrawn += before - self.waiting_prompts.len();
        if self.prompt.is_none() {
            self.prompt = self.waiting_prompts.pop_front();
        }
        if withdrawn == 0 {
            return;
        }
        let plural = if withdrawn == 1 { "" } else { "s" };
        self.transcript
            .push_notice(format!("{reason}; {withdrawn} question{plural} withdrawn"));
    }

    /// The lines the active prompt wants drawn, if any.
    ///
    /// The hints come from the key map in force, so a rebound answer key is the
    /// key the prompt names. A line under the prompt counts the decisions still
    /// waiting behind it: answering the one on screen shows the next, and
    /// without the count a second approval would look like a repeat of the
    /// first.
    pub fn prompt_lines(&self) -> Vec<String> {
        let Some(prompt) = &self.prompt else {
            return Vec::new();
        };
        let mut lines = prompt.lines_with(&self.bindings);
        // The arguments come from the one line whose failure names this request,
        // so a prompt can only ever show the call it is asking about. No match
        // means the name alone: a guess here already put one call's command on
        // another call's decision.
        if let Prompt::Approval(approval) = prompt
            && let Some(target) = self.transcript.argument_summary_for_request(&approval.request_id)
        {
            lines[0] = format!("! approval needed: {}: {target}", approval.action);
        }
        let waiting = self.waiting_prompts.len();
        if waiting > 0 {
            let plural = if waiting == 1 { "" } else { "s" };
            lines.push(format!("  {waiting} more prompt{plural} waiting"));
        }
        lines
    }

    /// Put a prompt in front of the user, or behind the one already there.
    ///
    /// A prompt takes every key, so the slash popup goes: a list still on screen
    /// under a prompt would claim keys it no longer gets, and its rows would sit
    /// between the decision and the draft it is about.
    pub(crate) fn show_prompt(&mut self, prompt: Prompt) {
        self.slash.dismiss();
        match &self.prompt {
            Some(_) => self.waiting_prompts.push_back(prompt),
            None => self.prompt = Some(prompt),
        }
    }

    /// Take the prompt on screen down and show the next one, if any.
    ///
    /// Returns the prompt that was on screen, which is what the confirmation
    /// line names.
    pub(crate) fn take_prompt(&mut self) -> Option<Prompt> {
        let taken = self.prompt.take();
        self.prompt = self.waiting_prompts.pop_front();
        taken
    }

    /// Drop the prompts a turn was parked on.
    ///
    /// A prompt belongs to the turn that asked for it. Once that turn is gone
    /// (answered, cancelled, failed, or a lost connection) the decision has no
    /// waiting call behind it, and answering it would be a claim about a run
    /// that no longer exists. The queue goes with it for the same reason.
    pub(crate) fn withdraw_prompts(&mut self, reason: &str) {
        if self.prompt.is_none() && self.waiting_prompts.is_empty() {
            return;
        }
        let shown = usize::from(self.prompt.is_some()) + self.waiting_prompts.len();
        // A withdrawn approval was never answered, and its row has to say so:
        // left waiting it would claim a decision that is never coming.
        let withdrawn: Vec<String> = self
            .prompt
            .iter()
            .chain(self.waiting_prompts.iter())
            .filter_map(|prompt| match prompt {
                Prompt::Approval(approval) => Some(approval.request_id.clone()),
                Prompt::Ask(_) => None,
            })
            .collect();
        self.prompt = None;
        self.waiting_prompts.clear();
        for request in withdrawn {
            self.transcript.decide_request(&request, Decision::Unanswered);
        }
        // The rows the withdrawn approvals were drawn on changed height.
        self.scroll.reflow();
        let plural = if shown == 1 { "" } else { "s" };
        self.transcript
            .push_notice(format!("{reason}; {shown} prompt{plural} withdrawn"));
    }

    /// The pending-decision wording for the `pending` segment.
    pub(crate) fn prompt_label(&self) -> Option<&'static str> {
        match &self.prompt {
            Some(Prompt::Approval(_)) => Some("approval needed"),
            Some(Prompt::Ask(_)) => Some("question waiting"),
            None => None,
        }
    }
}
