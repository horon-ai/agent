//! The `/swarm` rows: whose children become rows, the way back to the parent,
//! the order the list draws in, and the bound on what one row says.
//!
//! The rows are built from session records, so the tests hand the builder the
//! records a local harness annotates — lineage, task label and last persisted
//! lifecycle state — and check what a reader ends up seeing. No daemon is
//! reached: the one list request that fills the rows is a single SDK call, and
//! the report a finished child closes its turn with is applied here from the
//! text a caller would have read, so what these tests pin is the row a reader
//! gets.

use horon_agent_sdk::SessionInfo;

use super::{apply_report, list};

/// A session record as the harness serialises one.
fn session(id: &str, parent: Option<&str>, swarm_status: Option<&str>, updated_at_ms: Option<i64>) -> SessionInfo {
    let mut record = serde_json::json!({"session_id": id, "status": "idle"});
    if let Some(parent) = parent {
        record["parent_session_id"] = serde_json::json!(parent);
    }
    if let Some(status) = swarm_status {
        record["swarm_status"] = serde_json::json!(status);
    }
    if let Some(updated) = updated_at_ms {
        record["updated_at_ms"] = serde_json::json!(updated);
    }
    serde_json::from_value(record).expect("a session record")
}

#[test]
fn only_direct_children_of_the_attached_session_become_rows() {
    let sessions = [
        session("root", None, Some("ready"), Some(10)),
        session("child", Some("root"), Some("running"), Some(20)),
        session("grandchild", Some("child"), Some("running"), Some(30)),
        session("stranger", None, Some("running"), Some(40)),
        session("elsewhere", Some("other-root"), Some("running"), Some(50)),
    ];

    let rows = list(&sessions, "root");

    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(
        ids,
        ["child"],
        "the list must hold this session's children and nothing else: {ids:?}"
    );
}

#[test]
fn the_parent_row_returns_to_the_session_the_child_reports_back_to() {
    let sessions = [
        session("root", None, Some("ready"), Some(10)),
        session("child", Some("root"), Some("running"), Some(20)),
        session("grandchild", Some("child"), Some("completed"), Some(30)),
    ];

    let rows = list(&sessions, "child");

    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(
        ids,
        ["grandchild", "root"],
        "the children come first and the way back comes last: {ids:?}"
    );
    assert!(!rows[0].parent, "a child row is not the way back");
    let parent = rows.last().expect("a parent row");
    assert!(parent.parent, "the way back must be marked as the parent row");
    assert_eq!(parent.label, "parent: root", "the row must name itself as the way back");
    assert_eq!(
        parent.preview.last().map(String::as_str),
        Some("enter returns to the parent session")
    );
}

#[test]
fn live_children_sort_before_finished_ones_and_the_newest_leads() {
    let sessions = [
        session("root", None, None, Some(1)),
        session("running-old", Some("root"), Some("running"), Some(100)),
        session("running-new", Some("root"), Some("running"), Some(200)),
        session("done-new", Some("root"), Some("completed"), Some(300)),
        session("failed-old", Some("root"), Some("failed"), Some(50)),
        session("paused", Some("root"), Some("paused"), Some(400)),
    ];

    let rows = list(&sessions, "root");

    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(
        ids,
        ["running-new", "running-old", "done-new", "failed-old", "paused"],
        "live work leads, finished work follows, an unknown state comes last: {ids:?}"
    );
}

#[test]
fn a_ready_worker_still_sorts_with_live_work() {
    let sessions = [
        session("root", None, None, Some(1)),
        session("ready", Some("root"), Some("ready"), Some(100)),
        session("done", Some("root"), Some("completed"), Some(200)),
    ];

    let rows = list(&sessions, "root");

    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(
        ids,
        ["ready", "done"],
        "a worker waiting for more work is live, not finished: {ids:?}"
    );
}

#[test]
fn a_task_label_is_collapsed_and_cut_with_an_ellipsis() {
    let long = format!(
        "Fix the parser\n\n{}",
        "and then the whole authentication flow ".repeat(10)
    );
    let mut record = session("child", Some("root"), Some("running"), Some(2_000));
    record.agent_label = Some(long.clone());

    let rows = list(&[session("root", None, None, Some(1)), record], "root");

    let row = rows.first().expect("the child row");
    assert!(row.label.chars().count() <= super::LABEL_CHARS, "{}", row.label);
    assert!(
        row.label.ends_with('…'),
        "a cut label must say it was cut: {}",
        row.label
    );
    assert!(!row.label.contains('\n'), "a label is one line: {:?}", row.label);
    let task = row.task.as_deref().expect("the recorded task");
    assert!(
        task.chars().count() <= super::PREVIEW_CHARS,
        "the task is bounded: {}",
        task.chars().count()
    );
    assert!(row.preview.len() <= super::MAX_PREVIEW_LINES, "{:?}", row.preview);
    assert_eq!(
        row.preview.last().map(String::as_str),
        Some("enter attaches to this child; /swarm there lists its own children")
    );
}

#[test]
fn a_reported_answer_goes_under_the_task_and_stays_bounded() {
    let mut child = session("child", Some("root"), Some("completed"), Some(2));
    child.agent_label = Some("Fix the parser".to_string());
    let mut rows = list(&[session("root", None, None, Some(1)), child], "root");
    let row = rows.first_mut().expect("the child row");

    apply_report(row, &format!("  done in {}\n", "two files ".repeat(60)));

    assert_eq!(row.preview[1], "task: Fix the parser");
    let reported = row.report.as_deref().expect("the report");
    assert!(
        reported.chars().count() <= super::PREVIEW_CHARS,
        "the report is bounded: {}",
        reported.chars().count()
    );
    assert!(!reported.contains('\n'), "a report is one line: {reported:?}");
    assert_eq!(row.preview[2], format!("reported: {reported}"));
    assert!(row.preview.len() <= super::MAX_PREVIEW_LINES, "{:?}", row.preview);
    assert_eq!(
        row.preview.last().map(String::as_str),
        Some("enter attaches to this child; /swarm there lists its own children"),
        "the line saying what enter does must survive the report"
    );
}

#[test]
fn a_reported_answer_lands_under_the_state_when_there_is_no_task() {
    let mut rows = list(
        &[
            session("root", None, None, Some(1)),
            session("child", Some("root"), Some("completed"), Some(2)),
        ],
        "root",
    );
    let row = rows.first_mut().expect("the child row");
    assert!(row.task.is_none(), "the fixture gained a task: {:?}", row.task);

    apply_report(row, "did the thing");

    assert_eq!(row.preview[1], "reported: did the thing");
}

#[test]
fn a_blank_report_leaves_the_row_as_it_was() {
    let mut rows = list(
        &[
            session("root", None, None, Some(1)),
            session("child", Some("root"), Some("completed"), Some(2)),
        ],
        "root",
    );
    let row = rows.first_mut().expect("the child row");
    let before = row.preview.clone();

    apply_report(row, "   \n\t ");

    assert_eq!(row.preview, before);
    assert!(row.report.is_none());
}

#[test]
fn a_child_without_a_reported_state_says_so_and_an_unknown_word_survives() {
    let sessions = [
        session("root", None, None, Some(1)),
        session("silent", Some("root"), None, Some(2)),
        session("novel", Some("root"), Some("paused"), Some(3)),
    ];

    let rows = list(&sessions, "root");

    let silent = rows.iter().find(|row| row.id == "silent").expect("a row");
    assert_eq!(silent.detail, "(state not reported)");
    assert_eq!(
        silent.preview.first().map(String::as_str),
        Some("state: (state not reported)")
    );
    let novel = rows.iter().find(|row| row.id == "novel").expect("a row");
    assert_eq!(
        novel.detail, "paused",
        "a state this build does not know is shown as the harness wrote it"
    );
}

#[test]
fn a_parent_the_session_list_no_longer_carries_still_offers_the_way_back() {
    let sessions = [
        session("child", Some("root"), Some("running"), Some(20)),
        session("grandchild", Some("child"), Some("running"), Some(30)),
    ];

    let rows = list(&sessions, "child");

    let parent = rows.last().expect("a parent row");
    assert!(parent.parent, "the row did not mark itself as the parent");
    assert_eq!(parent.id, "root", "the id comes from the child's own ownership record");
    assert_eq!(parent.detail, "not listed");
    assert_eq!(
        parent.preview.last().map(String::as_str),
        Some("enter still asks the harness to attach to it")
    );
}

#[test]
fn a_reported_answer_trims_trailing_space_before_ellipsis_and_handles_multibyte() {
    let mut rows = list(
        &[
            session("root", None, None, Some(1)),
            session("child", Some("root"), Some("completed"), Some(2)),
        ],
        "root",
    );
    let row = rows.first_mut().expect("the child row");

    let prefix = "가".repeat(super::PREVIEW_CHARS - 2);
    apply_report(row, &format!("  {prefix}   rest  "));
    assert_eq!(row.report.as_deref(), Some(format!("{prefix}…").as_str()));

    let exact = "가".repeat(super::PREVIEW_CHARS);
    apply_report(row, &exact);
    assert_eq!(row.report.as_deref(), Some(exact.as_str()));
}
