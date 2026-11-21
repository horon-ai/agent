//! The attached session's swarm children, as `/swarm` lists them.
//!
//! The rows come from the session records the harness itself annotates with
//! swarm ownership (`parent_session_id`, `agent_label`, `swarm_status`), and
//! the list is narrowed here to two things: the sessions whose swarm parent is
//! the attached session, and that parent itself. Every other stored session is
//! dropped before a row exists, so the picker can never name a session outside
//! this session's swarm. Being in the harness's session list is not the
//! permission to attach either: a row carries the id of a child the harness
//! reported, and selecting it leaves through the same attach request `/switch`
//! uses, which the harness answers for the session it actually serves.
//!
//! A row's state is the last lifecycle status the harness persisted for the
//! member, not a liveness probe. The list only has to order live work before
//! finished work, so the few words that ordering needs are read here as strings
//! rather than through a second copy of the daemon's enum: a word this build
//! does not know sorts last and is shown as the harness wrote it.
//!
//! A child that finished its turn is asked to close with a short final
//! response after it reports completion, and that response is the summary a
//! reader can see. It is read from the stored conversation through the same
//! page/tail boundary `/switch` uses, for the most recently finished rows and
//! up to [`MAX_REPORT_READS`] of them, because the list opens on a keystroke.

use horon_agent_sdk::{Error, HoronAgentClient, SessionInfo};

use super::{activity, cut, moment, sessions};

/// How many lines one row's preview may carry, the last of which is always the
/// line saying what `enter` does.
///
/// The preview pane draws at most eight lines (`PREVIEW_MAX` in `picker_draw`),
/// so the seven a row can build here — state, task, report, activity,
/// directory, session, footer — are what the pane is sized for.
const MAX_PREVIEW_LINES: usize = 7;

/// Longest preview line, in characters.
const PREVIEW_CHARS: usize = 160;

/// Longest label, in characters.
const LABEL_CHARS: usize = 80;

/// How many finished children one list reads a reported answer from.
///
/// Each read is one history request, and the list is opened by a keystroke, so
/// the reads are capped rather than issued once per member. The cap is spent in
/// display order, which puts the children that finished most recently first.
const MAX_REPORT_READS: usize = 8;

/// What a row's state reads as when the harness recorded none.
const NO_STATE: &str = "(state not reported)";

/// What a row's directory reads as when the session names none.
const NO_DIRECTORY: &str = "(no directory)";

/// What the preview says of a parent the session list no longer carries.
const PARENT_NOT_LISTED: &str = "not listed";

/// One row of the `/swarm` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmRow {
    /// The session id the row attaches to.
    pub id: String,
    /// What the row reads as: the task label the harness holds, the title, or
    /// the session id, in that order.
    pub label: String,
    /// The second column: the last reported lifecycle state.
    pub detail: String,
    /// The task the child was spawned or assigned for, when the harness
    /// recorded one.
    ///
    /// It is one of the lines the filter matches: a child is often known by
    /// what it was told to do rather than by the label a row starts with.
    pub task: Option<String>,
    /// The short final response a finished child closed its turn with.
    ///
    /// Read after the list is built, so a row for a child the harness no longer
    /// has a conversation for simply carries none.
    pub report: Option<String>,
    /// The lines the picker's preview pane shows while the row is selected.
    pub preview: Vec<String>,
    /// Whether this row is the way back to the parent rather than a child.
    ///
    /// Only children are asked for a reported answer: the parent is a
    /// conversation the client may return to, not work this session spawned.
    pub parent: bool,
}

/// The swarm rows of `current`: its children, then the parent it returns to.
///
/// The list request is the harness's own; a failure travels back so the picker
/// closes with the sentence saying why instead of showing an empty swarm. A
/// history read that fails only leaves that row without a report.
pub(super) fn rows(client: &HoronAgentClient, current: &str) -> Result<Vec<SwarmRow>, Error> {
    let listed = client.list_sessions()?;
    let mut rows = list(&listed, current);
    read_reports(client, &mut rows);
    Ok(rows)
}

/// Which of `listed` belong to `current`'s swarm, in the order they draw.
///
/// Direct children only: a grandchild is reached by attaching to its parent and
/// opening `/swarm` there, which keeps one list to one level and makes the
/// parent row a real way back instead of a jump to the root.
fn list(listed: &[SessionInfo], current: &str) -> Vec<SwarmRow> {
    let parent_id = listed
        .iter()
        .find(|session| session.session_id == current)
        .and_then(|session| session.parent_session_id.as_deref())
        .map(str::trim)
        .filter(|parent| !parent.is_empty() && *parent != current)
        .map(str::to_string);

    // The sort key is taken once per child, not once per comparison.
    let mut children: Vec<(u8, Option<i64>, &SessionInfo)> = listed
        .iter()
        .filter(|session| session.session_id != current)
        .filter(|session| session.parent_session_id.as_deref().map(str::trim) == Some(current))
        .map(|session| (group(recorded(session)), activity(session), session))
        .collect();
    // Live work first, finished work after it, an unrecognised state last so a
    // newer harness's word is visible rather than hidden or mistaken for done.
    // Within a group the newest activity leads, and the id breaks a tie so two
    // rows written in the same millisecond keep a stable order across draws.
    children.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| left.2.session_id.cmp(&right.2.session_id))
    });

    let mut rows: Vec<SwarmRow> = children.into_iter().map(|(_, _, session)| child_row(session)).collect();
    if let Some(parent_id) = parent_id {
        let parent = listed.iter().find(|session| session.session_id == parent_id);
        rows.push(parent_row(&parent_id, parent));
    }
    rows
}

/// The lifecycle word the harness recorded for a session, trimmed.
///
/// A record that names no state, or only whitespace, has none: the row then
/// says so and sorts with the states this build does not know.
fn recorded(session: &SessionInfo) -> Option<&str> {
    session
        .swarm_status
        .as_deref()
        .map(str::trim)
        .filter(|word| !word.is_empty())
}

/// The state a row's second column says.
///
/// The harness's persisted swarm status is the one that describes the work; the
/// session's own `status` names only this connection's attachment, so it is not
/// used here. A state this client does not know is shown as the harness wrote
/// it, because a newer daemon may have a word this build has never seen.
fn state(session: &SessionInfo) -> String {
    match recorded(session) {
        Some(word) => word.to_string(),
        None => NO_STATE.to_string(),
    }
}

/// Whether the state says the child is still producing an answer.
///
/// These are the words a turn goes through while it works (`streaming` and
/// `thinking` included), plus the plan words a queued member carries. A child
/// in one of them has no finished answer to report yet.
fn producing(state: &str) -> bool {
    matches!(
        state,
        "spawned" | "running" | "running_stale" | "queued" | "blocked" | "pending" | "todo" | "streaming" | "thinking"
    )
}

/// Where a row sorts: live work, then finished work, then an unknown state.
///
/// A ready worker is live: it finished its turn and is waiting for the next
/// assignment, which is a session this client can still use.
fn group(state: Option<&str>) -> u8 {
    match state {
        Some(word) if producing(word) || word == "ready" => 0,
        Some("completed" | "done" | "failed" | "stopped" | "crashed") => 1,
        _ => 2,
    }
}

/// The task the harness recorded for what the child was told to do.
fn task(session: &SessionInfo) -> Option<String> {
    let text = session
        .agent_label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            session
                .title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })?;
    Some(collapse(text, PREVIEW_CHARS))
}

/// The task label a row starts with: the record's, the title, or the id.
fn label(session: &SessionInfo) -> String {
    let text = session
        .agent_label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            session
                .title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or(&session.session_id);
    collapse(text, LABEL_CHARS)
}

/// One line of text, whitespace collapsed and cut on a character boundary.
///
/// A stored label or report may hold a whole prompt's line breaks, and a row
/// that carried them would be drawn as a paragraph the list never planned for.
/// The cut ends in an ellipsis so a shortened text does not read as the whole
/// one.
fn collapse(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let mut kept: String = collapsed.chars().take(max.saturating_sub(1)).collect();
    kept.truncate(kept.trim_end().len());
    kept.push('…');
    kept
}

/// When the session was last written, as the local date and time.
fn last_active(session: &SessionInfo) -> String {
    match activity(session).and_then(moment) {
        Some(time) => time,
        None => "(not reported)".to_string(),
    }
}

/// The directory a session reports, cut to one preview line.
///
/// A session that names no directory, or only whitespace, shows the empty state
/// the preview pane has always used for it.
fn directory(session: &SessionInfo) -> String {
    let Some(directory) = session
        .working_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return NO_DIRECTORY.to_string();
    };
    cut(directory)
}

/// One row for a child of the attached session.
fn child_row(session: &SessionInfo) -> SwarmRow {
    let task = task(session);
    let footer = "enter attaches to this child; /swarm there lists its own children";
    SwarmRow {
        id: session.session_id.clone(),
        label: label(session),
        detail: state(session),
        preview: preview(session, task.as_deref(), footer),
        task,
        report: None,
        parent: false,
    }
}

/// The row that returns to the swarm parent of the attached session.
///
/// The parent is named from the attached session's own ownership record, so the
/// row exists even when the session list no longer carries that parent: the id
/// is the harness's answer about the spawn tree, not a guess, and the harness
/// still decides whether it can serve the attach.
fn parent_row(parent_id: &str, parent: Option<&SessionInfo>) -> SwarmRow {
    let Some(parent) = parent else {
        return SwarmRow {
            id: parent_id.to_string(),
            label: format!("parent: {}", cut(parent_id)),
            detail: PARENT_NOT_LISTED.to_string(),
            task: None,
            report: None,
            preview: vec![
                format!("state: {PARENT_NOT_LISTED} in this session list"),
                format!("session: {parent_id}"),
                "enter still asks the harness to attach to it".to_string(),
            ],
            parent: true,
        };
    };
    let task = task(parent);
    SwarmRow {
        id: parent.session_id.clone(),
        label: format!("parent: {}", label(parent)),
        detail: state(parent),
        preview: preview(parent, task.as_deref(), "enter returns to the parent session"),
        task,
        report: None,
        parent: true,
    }
}

/// The bounded preview lines of one session.
///
/// The footer is the one line that must survive the cap: it says what `enter`
/// does, and a long directory must not be able to push it out.
fn preview(session: &SessionInfo, task: Option<&str>, footer: &str) -> Vec<String> {
    let mut lines = vec![format!("state: {}", state(session))];
    if let Some(task) = task {
        lines.push(format!("task: {task}"));
    }
    lines.push(format!("last active: {}", last_active(session)));
    lines.push(format!("directory: {}", directory(session)));
    lines.push(format!("session: {}", session.session_id));
    lines.truncate(MAX_PREVIEW_LINES - 1);
    lines.push(footer.to_string());
    lines
}

/// Ask the rows that finished a turn for the answer they closed it with.
///
/// A row that is still producing an answer has nothing to show and does not
/// spend the read budget; a row the harness has no readable conversation for
/// simply stays without one. The parent row is skipped: it is the conversation
/// being returned to, not work this session spawned.
fn read_reports(client: &HoronAgentClient, rows: &mut [SwarmRow]) {
    let mut reads = 0usize;
    for row in rows.iter_mut() {
        if reads == MAX_REPORT_READS {
            break;
        }
        if row.parent || producing(&row.detail) {
            continue;
        }
        reads += 1;
        let messages = sessions::peek(client, &row.id);
        if let Some(report) = sessions::last_assistant_text(&messages) {
            apply_report(row, &report);
        }
    }
}

/// Put a finished child's reported answer into its row.
///
/// The line goes directly under the task, because that is the pair a reader
/// compares — what the child was told to do, and what it said it did. The
/// report is bounded like every other preview line, and it becomes a filter
/// line so a child can also be found by a word from its report.
fn apply_report(row: &mut SwarmRow, report: &str) {
    let report = collapse(report, PREVIEW_CHARS);
    if report.is_empty() {
        return;
    }
    let at = if row.task.is_some() { 2 } else { 1 };
    let at = at.min(row.preview.len().saturating_sub(1));
    row.preview.insert(at, format!("reported: {report}"));
    row.report = Some(report);
}

#[cfg(test)]
#[path = "swarm_tests.rs"]
mod tests;
