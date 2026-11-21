//! `/jobs`: the background work this session is still running, and the one
//! cancellation the list offers.
//!
//! The list is a live read, never a replay of the transcript: a client that
//! reattached after a disconnect asks the daemon what survived instead of
//! guessing from the output notices it saw. Every row the daemon reports is
//! running, so the second column says `kind · running`; a job that ends leaves
//! the next answer rather than lingering as a memory of itself.
//!
//! `delete` cancels the selected row, and only a background terminal: the
//! request names one tool call, so the session and every other job keep
//! running. A tool-call row is listed but refused here, because the daemon
//! answers such a request with `not_cancellable`, and a refusal the client can
//! predict is better said than sent.

use crossterm::event::{KeyCode, KeyEvent};

use super::{Action, App};
use crate::overlays::picker::{Kind, Outcome, Picker, Row};
use crate::sdk_bridge::BackgroundJobRow;

/// The title the list is drawn with.
const TITLE: &str = "Background jobs";

/// What the list says when the session is running nothing.
const EMPTY: &str = "this session is running no background work";

/// Output lines a row's preview shows, within what the bridge already keeps.
const OUTPUT_LINES: usize = 4;

/// Longest command line a row keeps.
const LABEL_CHARS: usize = 80;

/// Longest field a preview line keeps.
const FIELD_CHARS: usize = 160;

impl App {
    /// `/jobs`: ask the daemon what this session is running.
    ///
    /// The list opens empty rather than from a remembered answer: a job may
    /// have ended, or started on another client, since the last read, and a
    /// stale row would offer a cancellation that can only be refused.
    pub(crate) fn open_jobs(&mut self) -> Option<Action> {
        self.picker = Some(Picker::loading(Kind::Jobs, TITLE, EMPTY));
        Some(Action::BackgroundJobs)
    }

    /// The daemon's answer: it replaces the list, and the open list with it.
    ///
    /// A list the client is not showing replaces the state anyway, so a
    /// cancellation answered after the picker closed still updates the rows
    /// without opening a list nobody asked for.
    pub fn show_jobs(&mut self, jobs: Vec<BackgroundJobRow>) {
        self.background_jobs = jobs;
        let rows = self.job_rows();
        self.fill(Kind::Jobs, rows);
    }

    /// One cancellation's verdict, and the list as it stands after it.
    ///
    /// A refusal is the daemon's own reason, and a refresh that could not be
    /// read is not the cancellation's failure: the verdict still shows, the
    /// rows already on screen stay, and the failure explains why they may be
    /// stale.
    pub fn job_cancelled(
        &mut self,
        call_id: &str,
        accepted: bool,
        reason: Option<&str>,
        jobs: Result<Vec<BackgroundJobRow>, String>,
    ) {
        if accepted {
            self.transcript
                .push_notice(format!("cancelling background job {call_id}"));
        } else {
            let refusal = refusal_text(reason);
            self.transcript
                .push_notice(format!("could not cancel background job {call_id}: {refusal}"));
        }
        match jobs {
            Ok(jobs) => self.show_jobs(jobs),
            Err(error) => self.warn(format!(
                "could not refresh the background jobs; the rows may be stale: {error}"
            )),
        }
    }

    /// One keystroke inside the job list.
    ///
    /// The list is the generic one with one key of its own: `delete` cancels
    /// the selected terminal. `enter` is not a choice — the rows describe work
    /// rather than offer a value — so it changes nothing and the list stays
    /// open. Everything else is the list's own filter and movement, and `esc`
    /// closes without cancelling anything.
    pub(crate) fn job_panel_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.picker = None;
                None
            }
            KeyCode::Enter => None,
            KeyCode::Delete => self.cancel_selected_job(),
            _ => {
                let mut picker = self.picker.take()?;
                if picker.on_key(key) == Outcome::Handled {
                    self.picker = Some(picker);
                }
                None
            }
        }
    }

    /// Forget the jobs of a session the client is leaving.
    ///
    /// A call id is only meaningful in the session that started it, so neither
    /// the rows nor an open list may survive a switch.
    pub(crate) fn clear_background_jobs(&mut self) {
        self.background_jobs.clear();
        if self.picker.as_ref().is_some_and(|picker| picker.kind() == Kind::Jobs) {
            self.picker = None;
        }
    }

    /// The action that cancels the selected job, when it can be cancelled.
    fn cancel_selected_job(&mut self) -> Option<Action> {
        let (label, index) = {
            let picker = self.picker.as_ref()?;
            let row = picker.selected_row()?;
            let index = match row.value.parse::<usize>() {
                Ok(index) => index,
                // The value is this module's own row index; a value that is not
                // one cannot name a job, so the key changes nothing.
                Err(_) => return None,
            };
            (row.label.clone(), index)
        };
        let Some(job) = self.background_jobs.get(index) else {
            return None;
        };
        // Read before the refusal below: a warning changes the transcript, and
        // the row's borrow cannot stay alive across it.
        let call_id = job.call_id.clone();
        let cancellable = job.cancellable();
        if !cancellable {
            self.warn(format!(
                "job {label} is a tool call; only a background terminal can be cancelled here"
            ));
            return None;
        }
        Some(Action::CancelBackgroundTerminal(call_id))
    }

    /// The rows the list draws, in the order the daemon reported them.
    fn job_rows(&self) -> Vec<Row> {
        self.background_jobs
            .iter()
            .enumerate()
            .map(|(index, job)| {
                // The value is the row's place in `background_jobs`, not the
                // call id: the selection has to name the job even when two
                // jobs share a command line.
                Row::new(job_label(job), index.to_string())
                    .with_detail(format!("{} · running", job.kind))
                    .with_preview(job_preview(job))
                    .with_search(job_search(job))
            })
            .collect()
    }
}

/// What the daemon's refusal means, in a sentence rather than a code.
fn refusal_text(reason: Option<&str>) -> String {
    match reason {
        Some("unknown") => "this session does not know that job".to_string(),
        Some("final") => "the job already ended".to_string(),
        Some("not_cancellable") => "the job is not a background terminal".to_string(),
        Some(other) if !other.trim().is_empty() => other.trim().to_string(),
        _ => "the daemon refused the cancellation".to_string(),
    }
}

/// What a row reads as: the command when the daemon named one, else its task.
fn job_label(job: &BackgroundJobRow) -> String {
    let text = match &job.command {
        Some(command) if !command.is_empty() => command.as_str(),
        _ => &job.task_id,
    };
    cut(text, LABEL_CHARS)
}

/// The lines the preview pane shows while this row is selected.
///
/// Kind, state and the ids share one line, and the output keeps at most the
/// four lines the pane has room for, so the last line — the key that acts on
/// the row — is never the one cut off.
fn job_preview(job: &BackgroundJobRow) -> Vec<String> {
    let mut lines = Vec::with_capacity(OUTPUT_LINES + 4);
    if let Some(command) = &job.command {
        lines.push(format!("command: {}", cut(command, FIELD_CHARS)));
    }
    if let Some(cwd) = &job.cwd {
        lines.push(format!("cwd: {}", cut(cwd, FIELD_CHARS)));
    }
    lines.push(format!(
        "{} · running · call {} · task {}",
        job.kind,
        cut(&job.call_id, LABEL_CHARS),
        cut(&job.task_id, LABEL_CHARS)
    ));
    if job.output.is_empty() {
        lines.push("no output yet".to_string());
    } else {
        for line in job.output.iter().take(OUTPUT_LINES) {
            lines.push(format!("  {line}"));
        }
    }
    lines.push(if job.cancellable() {
        "delete cancels this terminal; the session and other jobs keep running".to_string()
    } else {
        "a tool-call job cannot be cancelled from this list".to_string()
    });
    lines
}

/// The lines the filter matches besides the label and the second column.
///
/// A call id and a task id are what a user copies out of a log to find the job
/// again; the command is matched through the label already.
fn job_search(job: &BackgroundJobRow) -> Vec<String> {
    vec![job.call_id.clone(), job.task_id.clone()]
}

/// The first `limit` characters of a value, with control characters removed.
///
/// The command, the directory and the output all come from the daemon and are
/// drawn as text: a control character left in one could reach the terminal.
fn cut(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(limit)
        .collect()
}
