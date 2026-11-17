//! The history search: the drafts sent before this one, offered as a list.
//!
//! `up` walks the drafts one at a time, which stops working once the one the
//! user wants is a hundred lines back. The list is the same picker the model and
//! session commands use, so its filter and its keys are already known, and it
//! reports a row by the row's place in the walk rather than by its text: the
//! same message can be sent twice, and a multi-line draft is one entry.
//!
//! A chosen draft goes back into the editor and is not sent. A draft is normally
//! recalled to be changed, and sending it on the key that fills it would leave
//! the user with nothing to edit.

use super::App;
use crate::overlays::picker::{Kind, Picker, Row};

/// The title the list is drawn with.
const TITLE: &str = "Search history";

/// What the list says when nothing was sent from this editor yet.
const EMPTY: &str = "no earlier drafts";

impl App {
    /// Open the list of drafts sent before this one, newest first.
    ///
    /// The drafts are the client's own, so the list is complete the moment it
    /// opens: nothing is asked of the harness and the list never loads.
    pub(crate) fn open_history_search(&mut self) {
        let rows: Vec<Row> = self
            .history
            .drafts()
            .enumerate()
            .map(|(position, draft)| draft_row(position, draft))
            .collect();
        self.picker = Some(Picker::new(Kind::History, TITLE, rows, EMPTY));
    }

    /// Put the draft a chosen row names back in the editor.
    ///
    /// The walk ends here: the draft on screen is no longer the one the walk
    /// stood on, so a later `down` must not replace the recalled text with the
    /// draft the walk was going to return to. A value the history no longer
    /// holds is ignored rather than put back as some other draft.
    pub(crate) fn recall_draft(&mut self, value: &str) {
        let Ok(position) = value.parse::<usize>() else {
            return;
        };
        let Some(draft) = self.history.draft_at(position) else {
            return;
        };
        let draft = draft.to_string();
        self.history.reset();
        self.input.set_text(&draft);
    }
}

/// One row for a draft: the first line names it, the line count follows when it
/// has more than one, and the whole draft is what the preview shows.
fn draft_row(position: usize, draft: &str) -> Row {
    // Split the way the editor counts lines, so a draft that ends with a
    // newline still says it has the empty line after it.
    let lines: Vec<&str> = draft.split('\n').collect();
    let label = lines.first().copied().unwrap_or("").to_string();
    let preview = lines.iter().map(|line| line.to_string()).collect();
    let mut row = Row::new(label, position.to_string()).with_preview(preview);
    if lines.len() > 1 {
        row = row.with_detail(format!("{} lines", lines.len()));
    }
    row
}
