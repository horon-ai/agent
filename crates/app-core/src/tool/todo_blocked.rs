//! Validation for the `blocked` todo status.
//!
//! A blocked item names what it waits on. The note is what makes the status
//! useful: without it the reader only learns that something stopped, which
//! reads as work the agent abandoned. Everything here runs before the list is
//! saved, so a stored todo never carries a blocked status without a reason or a
//! reason without a blocked status.

use super::TodoItem;
use crate::todo::TODO_BLOCKER_MAX_CHARS;
use anyhow::{Result, bail};

/// Trim every incoming blocker note, drop it off an item that is not blocked,
/// and reject a blocked item that carries no note.
///
/// A note on an item that moved on would outlive the block it describes and
/// read as a live reason, so the status alone decides whether a note is stored.
pub(super) fn normalize(todos: &mut Option<Vec<TodoItem>>) -> Result<()> {
    let Some(todos) = todos.as_mut() else {
        return Ok(());
    };
    for todo in todos.iter_mut() {
        let note = todo
            .blocker
            .take()
            .map(|note| note.trim().to_string())
            .filter(|note| !note.is_empty());
        if !crate::todo::todo_status_is_blocked(&todo.status) {
            continue;
        }
        let Some(note) = note else {
            bail!(
                "todo {:?} is blocked without a `blocker`: say what the item waits on, or use pending",
                todo.id
            );
        };
        let length = note.chars().count();
        if length > TODO_BLOCKER_MAX_CHARS {
            bail!(
                "todo {:?} has a {length}-character `blocker`; the limit is {TODO_BLOCKER_MAX_CHARS}",
                todo.id
            );
        }
        todo.blocker = Some(note);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo::{FollowUpKind, FollowUpPolicy, TodoItem, TodoPlan, TodoSnapshot};
    use crate::tool::Tool;
    use serde_json::json;

    fn parse(input: serde_json::Value) -> Result<super::super::TodoInput> {
        super::super::parse_todo_input(input)
    }

    fn item(id: &str, status: &str, blocker: Option<&str>) -> TodoItem {
        TodoItem {
            id: id.to_string(),
            content: format!("task {id}"),
            status: status.to_string(),
            priority: "high".to_string(),
            blocker: blocker.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn todo_blocked_item_with_a_blocker_validates() {
        let parsed = parse(json!({
            "todos": [{
                "content": "ship the release",
                "status": "blocked",
                "priority": "high",
                "id": "t1",
                "confidence": "plausible",
                "blocker": "  the CI queue is down  "
            }]
        }))
        .expect("a blocked item with a blocker should validate");

        let todos = parsed.todos.expect("todos present");
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].status, "blocked");
        assert_eq!(todos[0].blocker.as_deref(), Some("the CI queue is down"));
    }

    #[test]
    fn todo_blocked_without_a_blocker_is_rejected() {
        for blocker in [None, Some(""), Some("   "), Some("null")] {
            let mut fields = json!({
                "content": "ship the release",
                "status": "blocked",
                "priority": "high",
                "id": "t1",
                "confidence": "plausible"
            });
            match blocker {
                // A literal "null" string is a note; only a real null is absent.
                Some(value) if value != "null" => fields["blocker"] = json!(value),
                Some(_) => fields["blocker"] = json!(null),
                None => {}
            }
            let error = parse(json!({ "todos": [fields] }))
                .err()
                .unwrap_or_else(|| panic!("blocker {blocker:?} must be rejected"));
            let message = error.to_string();
            assert!(
                message.contains("is blocked without a `blocker`"),
                "blocker {blocker:?} gave {message}"
            );
        }
    }

    #[test]
    fn todo_blocked_blocker_is_dropped_off_a_non_blocked_item() {
        for status in ["pending", "in_progress", "completed", "cancelled"] {
            let parsed = parse(json!({
                "todos": [{
                    "content": "task",
                    "status": status,
                    "priority": "high",
                    "id": "t1",
                    "confidence": "plausible",
                    "blocker": "the CI queue is down"
                }]
            }))
            .unwrap_or_else(|error| panic!("{status} with a stray blocker must parse: {error}"));

            let todos = parsed.todos.expect("todos present");
            assert_eq!(todos[0].status, status);
            assert_eq!(todos[0].blocker, None, "{status} must not keep a blocker");
        }
    }

    #[test]
    fn todo_blocked_blocker_length_is_bounded() {
        let longest = "x".repeat(TODO_BLOCKER_MAX_CHARS);
        parse(json!({
            "todos": [{
                "content": "task",
                "status": "blocked",
                "priority": "high",
                "id": "t1",
                "confidence": "plausible",
                "blocker": longest
            }]
        }))
        .expect("a note at the limit should validate");

        let too_long = "x".repeat(TODO_BLOCKER_MAX_CHARS + 1);
        let error = parse(json!({
            "todos": [{
                "content": "task",
                "status": "blocked",
                "priority": "high",
                "id": "t1",
                "confidence": "plausible",
                "blocker": too_long
            }]
        }))
        .err()
        .expect("a note over the limit must be rejected");
        assert!(error.to_string().contains("the limit is 500"), "{error}");
    }

    #[test]
    fn todo_blocked_output_shows_the_blocker_and_the_status() {
        let todos = vec![
            item("t1", "completed", None),
            item("t2", "blocked", Some("the CI queue is down")),
        ];
        let output = super::super::build_todo_output(todos, TodoPlan::default(), Vec::new(), None, None, Vec::new())
            .expect("todo output should build");

        assert!(output.output.contains("\"status\": \"blocked\""), "{}", output.output);
        assert!(
            output.output.contains("\"blocker\": \"the CI queue is down\""),
            "{}",
            output.output
        );
        // The blocked item still counts as remaining work in the title.
        assert_eq!(output.title.as_deref(), Some("1 todos"));
    }

    #[test]
    fn todo_blocked_item_reaches_the_client_plan_as_pending() {
        let todos = vec![
            item("t1", "completed", None),
            item("t2", "blocked", Some("the CI queue is down")),
        ];
        let entries = super::super::plan_entries(&todos);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].status, "pending");
        // A blocked item is never the single in-progress entry a client draws.
        assert!(entries.iter().all(|entry| entry.status != "in_progress"));
    }

    #[test]
    fn todo_blocked_item_is_not_done_for_completion_or_follow_up() {
        let blocked = item("t1", "blocked", Some("the CI queue is down"));
        let todos = vec![blocked, item("t2", "completed", None)];

        // Completion: a list whose only open item is blocked is not finished.
        assert!(crate::todo::is_incomplete_todo(&todos[0]));
        assert!(!crate::todo::todo_status_is_completed("blocked"));

        // Follow-up: the policy pokes for the blocked item instead of
        // announcing that every todo is done.
        let mut policy = FollowUpPolicy::new(true);
        let decision = policy.decide(&TodoSnapshot {
            todos,
            ..Default::default()
        });
        let kind = decision.follow_up.map(|follow_up| follow_up.kind);
        assert_eq!(kind, Some(FollowUpKind::IncompleteTodos));
    }

    #[test]
    fn todo_blocked_schema_advertises_the_status_and_its_note() {
        let schema = super::super::TodoTool::new().parameters_schema();
        let properties = &schema["properties"]["todos"]["items"]["properties"];

        let statuses = properties["status"]["enum"]
            .as_array()
            .expect("status enum should be a list");
        assert!(
            statuses.iter().any(|value| value == "blocked"),
            "status enum should offer blocked: {statuses:?}"
        );

        let blocker = properties
            .get("blocker")
            .unwrap_or_else(|| panic!("todo schema should advertise `blocker`: {properties}"));
        assert_eq!(blocker["type"], "string");
        assert_eq!(blocker["maxLength"], TODO_BLOCKER_MAX_CHARS);

        // The note is required only while the item is blocked, so it stays out
        // of the unconditional required list.
        let required = schema["properties"]["todos"]["items"]["required"]
            .as_array()
            .expect("todo item required list");
        assert!(!required.iter().any(|value| value == "blocker"));
    }

    #[test]
    fn todo_blocked_rejects_an_unknown_status_with_the_new_vocabulary() {
        let error = parse(json!({
            "todos": [{
                "content": "task",
                "status": "waiting",
                "priority": "high",
                "id": "t1",
                "confidence": "plausible"
            }]
        }))
        .err()
        .expect("an unknown status must be rejected");

        let message = error.to_string();
        assert!(message.contains("invalid todo status \"waiting\""), "{message}");
        assert!(
            message.contains("pending, in_progress, blocked, completed, cancelled"),
            "{message}"
        );
    }
}
