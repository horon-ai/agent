//! Policy refusals a session's next turn must report.
//!
//! A refused MCP server is absent from the tool list, which looks exactly like a
//! server nobody configured. The gate that refuses writes the audit record
//! (`HOSTED_POLICY` in `crates/base/src/mcp/hosted.rs`); this queue carries the
//! same refusal into the next turn's system reminder, so the model can tell the
//! user why the tools are missing instead of the path ending in a log line only.
//!
//! A notice is keyed by session, deduplicated, and bounded: a long-lived daemon
//! cannot grow the queue without limit even if a server is retried every turn.

use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex, PoisonError};

/// Most notices one session keeps before the oldest is dropped.
pub(crate) const MAX_NOTICES: usize = 8;

static NOTICES: LazyLock<Mutex<BTreeMap<String, Vec<String>>>> = LazyLock::new(Default::default);

/// Record one refused operation for `session_id`.
pub(crate) fn push(session_id: &str, notice: String) {
    let mut notices = NOTICES.lock().unwrap_or_else(PoisonError::into_inner);
    let queue = notices.entry(session_id.to_string()).or_default();
    if queue.contains(&notice) {
        return;
    }
    if queue.len() == MAX_NOTICES {
        queue.remove(0);
    }
    queue.push(notice);
}

/// Take this session's notices as the one reminder paragraph they become.
pub(crate) fn take(session_id: &str) -> Option<String> {
    let mut notices = NOTICES.lock().unwrap_or_else(PoisonError::into_inner);
    let queue = notices.remove(session_id)?;
    if queue.is_empty() {
        return None;
    }
    let mut reminder = String::from(
        "<hosted-policy>\nThe hosting policy refused these operations, so their tools are not available. \
         Tell the user when they ask for them.\n",
    );
    for notice in queue {
        reminder.push_str("- ");
        reminder.push_str(&notice);
        reminder.push('\n');
    }
    reminder.push_str("</hosted-policy>");
    Some(reminder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_becomes_one_reminder_that_names_the_policy() {
        push(
            "session-notice-a",
            "MCP server 'plugin_demo_echo' of package demo was not started: hosting policy 'shared-worker' \
             refuses the MCP stdio server 'plugin_demo_echo': one worker, no child"
                .to_string(),
        );
        push("session-notice-a", "a second refusal".to_string());
        let reminder = take("session-notice-a").expect("the queue holds two notices");
        assert!(reminder.starts_with("<hosted-policy>"), "{reminder}");
        assert!(reminder.contains("refused these operations"), "{reminder}");
        assert!(reminder.contains("hosting policy 'shared-worker'"), "{reminder}");
        assert!(reminder.contains("- a second refusal"), "{reminder}");
        assert!(reminder.ends_with("</hosted-policy>"), "{reminder}");
        assert_eq!(take("session-notice-a"), None, "taking drains the queue");
    }

    #[test]
    fn the_same_refusal_is_recorded_once_and_the_queue_is_bounded() {
        let session = "session-notice-b";
        for _ in 0..3 {
            push(session, "the same refusal".to_string());
        }
        let reminder = take(session).expect("one notice");
        assert_eq!(reminder.matches("- the same refusal").count(), 1, "{reminder}");

        for index in 0..MAX_NOTICES + 2 {
            push(session, format!("refusal {index}"));
        }
        let reminder = take(session).expect("a full queue");
        assert!(!reminder.contains("refusal 0"), "the oldest is dropped: {reminder}");
        assert!(reminder.contains(&format!("refusal {}", MAX_NOTICES + 1)), "{reminder}");
    }

    #[test]
    fn a_session_without_a_notice_takes_nothing() {
        assert_eq!(take("session-notice-empty"), None);
    }
}
