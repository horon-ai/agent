//! `/skills` and the rows it offers: the list the harness reports, the draft a
//! chosen row puts in the editor, and the names the slash popup offers.
//!
//! The list belongs to the session. A switch or a new session clears it, and
//! the bridge sends the new session's list right after the attach, so a name
//! the client left behind is never offered for the session now on screen.
//!
//! A chosen row is a draft, not a send: the invocation is the part the client
//! knows, and the request after it is the user's to write. It goes through the
//! ordinary submit path, so it queues during a turn exactly as typed text does.

use super::{Action, App};
use crate::commands::Skill;
use crate::overlays::picker::{Kind, Picker, Row};
use crate::sdk_bridge::SkillRow;

/// The title the list is drawn with.
const TITLE: &str = "Skills";

/// What the list says when the harness reported no skill at all.
const EMPTY: &str = "the harness reported no skills";

impl App {
    /// `/skills`: the list as last reported, refreshed when the answer comes.
    ///
    /// The rows already known are shown at once rather than behind a loading
    /// line: the list changes only when the harness reports a different set,
    /// and waiting for a round trip would hide names the client already has.
    pub(crate) fn open_skills(&mut self) -> Option<Action> {
        let rows = self.skill_rows();
        self.picker = Some(if rows.is_empty() {
            Picker::loading(Kind::Skills, TITLE, EMPTY)
        } else {
            Picker::new(Kind::Skills, TITLE, rows, EMPTY)
        });
        Some(Action::Skills)
    }

    /// The harness's answer: it replaces the list, and the open list with it.
    ///
    /// The replacement is whole, because the answer names one session's skills.
    /// A list the client is not showing is filled too: `fill` keeps rows for
    /// the list that asked for them and drops the rest.
    pub fn show_skills(&mut self, rows: Vec<SkillRow>) {
        self.commands.set_skills(rows.iter().map(skill_entry).collect());
        self.skills = rows;
        let rows = self.skill_rows();
        self.fill(Kind::Skills, rows);
    }

    /// Put a chosen skill's invocation in the editor, unsent.
    ///
    /// The trailing space is where the request starts: the invocation alone is
    /// a complete line, and every character typed after it lands in the words
    /// the skill is asked to follow.
    pub(crate) fn draft_skill(&mut self, name: &str) {
        self.history.reset();
        self.input.set_text(&format!("/skill:{name} "));
    }

    /// The rows the list draws, in the order the harness reported them.
    fn skill_rows(&self) -> Vec<Row> {
        self.skills.iter().map(skill_row).collect()
    }
}

/// One row of the list: the name, the harness's line about it, and the preview.
fn skill_row(skill: &SkillRow) -> Row {
    // The name is the label and the value; the description is the second
    // column. Neither the path nor the tool list is searchable: a filter on
    // either would keep every skill read from the same directory.
    Row::new(skill.name.clone(), skill.name.clone())
        .with_detail(skill.description.clone())
        .with_preview(skill_preview(skill))
}

/// The lines the preview pane shows while this row is selected.
///
/// The last line is the one the user needs before pressing enter: what the key
/// does with the row, which no other list says because no other list fills the
/// editor rather than acting.
fn skill_preview(skill: &SkillRow) -> Vec<String> {
    let mut lines = Vec::with_capacity(4);
    if !skill.description.is_empty() {
        lines.push(skill.description.clone());
    }
    if let Some(path) = &skill.path {
        lines.push(format!("path: {path}"));
    }
    if !skill.allowed_tools.is_empty() {
        lines.push(format!("allowed tools: {}", skill.allowed_tools.join(", ")));
    }
    lines.push(format!("enter puts /skill:{} in the draft", skill.name));
    lines
}

/// The command-list view of one row, for the resolver and `/help`.
fn skill_entry(skill: &SkillRow) -> Skill {
    Skill {
        name: skill.name.clone(),
        description: skill.description.clone(),
    }
}
