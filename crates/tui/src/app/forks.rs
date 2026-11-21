//! `/fork`: choosing a recorded boundary to branch from, and the way back to
//! the session the branch was cut from.
//!
//! The rows come from the harness's own stored history, so a row names the
//! message by its stored id. The daemon resolves that id against the session's
//! head when the fork is requested; a boundary that has since left the head is
//! refused and the client stays on the session it was showing. The branch and
//! its original are both stored, so returning is an ordinary attach.

use super::{Action, App};
use crate::overlays::picker::{Kind, Picker, Row};
use crate::sdk_bridge::ForkRow;

/// A session a branch was forked from, while the client follows the branch.
///
/// The origin is what the fork picker offers as the way back: both sessions
/// stay stored, so returning is an ordinary attach and the branch is still
/// there afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForkOrigin {
    /// The session the branch was cut from.
    pub session_id: String,
    /// What the session was called when the fork was made, for the row.
    pub label: String,
}

/// What the fork list is drawn with.
const TITLE: &str = "Fork from a message";

/// What the list says when the session has no recorded user message.
const EMPTY: &str = "no recorded messages to fork from";

/// Prefix a return row's value with, so the picker can tell a session id from a
/// stored message id. Session ids never contain this punctuation.
pub(super) const RETURN_TO: &str = "session:";

impl App {
    /// Open the fork list and ask the bridge for the session's boundaries.
    ///
    /// The change waits for a running turn for the same reason a session switch
    /// does: the answer moves the client to another session, and the turn's
    /// events belong to the one being left.
    pub(crate) fn open_forks(&mut self) -> Option<Action> {
        if self.session_id.is_none() {
            self.warn("there is no session to fork");
            return None;
        }
        if !self.session_change_allowed("fork") {
            return None;
        }
        self.picker = Some(Picker::loading(Kind::Forks, TITLE, EMPTY));
        Some(Action::Forks)
    }

    /// Fill the open fork list: the way back first, then the newest boundary.
    ///
    /// The preview is built by the bridge; only the message text is searchable,
    /// because the rest of the preview repeats the same two sentences on every
    /// row and a filter on them would keep the whole list.
    pub fn show_forks(&mut self, rows: Vec<ForkRow>) {
        let mut listed: Vec<Row> = Vec::with_capacity(rows.len() + 1);
        if let Some(back) = self.return_row() {
            listed.push(back);
        }
        for row in rows {
            let ForkRow {
                boundary,
                label,
                detail,
                preview,
            } = row;
            let search = preview.get(1).cloned().into_iter().collect();
            listed.push(
                Row::new(label, boundary)
                    .with_detail(detail)
                    .with_preview(preview)
                    .with_search(search),
            );
        }
        self.fill(Kind::Forks, listed);
    }

    /// What a chosen fork row means: the way back, or a fork to ask for.
    ///
    /// The origin is recorded before the request leaves, so the answer can say
    /// which session the branch came from. A refusal clears it again in
    /// `request_failed`, which is what keeps a failed fork from claiming an
    /// origin the client never moved to.
    pub(super) fn choose_fork(&mut self, value: &str, row_label: &str) -> Option<Action> {
        if let Some(origin) = value.strip_prefix(RETURN_TO) {
            self.transcript.push_notice(format!("attaching to {origin}"));
            return Some(Action::Attach(origin.to_string()));
        }
        let origin = self.session_id.clone()?;
        let label = self
            .session_title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| origin.clone());
        self.transcript
            .push_notice(format!("forking before {row_label}"));
        self.fork_pending = Some(ForkOrigin {
            session_id: origin,
            label,
        });
        Some(Action::Fork(value.to_string()))
    }

    /// Take the fork the client asked for and has not seen answered.
    pub(crate) fn take_pending_fork(&mut self) -> Option<ForkOrigin> {
        self.fork_pending.take()
    }

    /// Record the session a just-attached branch was forked from.
    ///
    /// An answer that names the origin itself is not a branch: the fork was
    /// refused or answered with the source, and the way back would point at the
    /// session in force.
    pub(crate) fn forked_from(&mut self, origin: ForkOrigin, branch: &str) {
        if origin.session_id != branch {
            self.fork_origin = Some(origin);
        }
    }

    /// The row that returns to the session this branch was cut from.
    fn return_row(&self) -> Option<Row> {
        let origin = self.fork_origin.as_ref()?;
        if self.session_id.as_deref() == Some(origin.session_id.as_str()) {
            return None;
        }
        Some(
            Row::new(format!("back to {}", origin.label), format!("{RETURN_TO}{}", origin.session_id))
                .with_detail("original")
                .with_preview(vec![
                    "return to the session this branch came from".to_string(),
                    format!("session {}", origin.session_id),
                    "both this branch and the original stay stored".to_string(),
                ]),
        )
    }
}
