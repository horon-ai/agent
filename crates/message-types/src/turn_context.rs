//! Append-only reminders: context that changes during a session reaches the
//! model as a reminder appended to the transcript, never as an edit of an
//! earlier message.
//!
//! A request then always extends the previous one, which is what keeps a
//! provider's prompt cache, and a gateway that continues one upstream
//! conversation per session, on the same conversation. A newer reminder
//! supersedes an older one with the same heading; nothing earlier is rewritten.

use crate::{ContentBlock, Message, Role};

/// Heading that opens the turn-context reminder a session keeps in its
/// transcript.
const TURN_CONTEXT_HEADING: &str = "# Turn context";

const SYSTEM_REMINDER_OPEN: &str = "<system-reminder>\n";

/// Everything a turn-context reminder carries before its body.
const TURN_CONTEXT_OPEN: &str = "<system-reminder>\n# Turn context\nThis replaces any earlier turn context.\n\n";

/// Everything a turn-context reminder carries after its body.
const TURN_CONTEXT_CLOSE: &str = "\n</system-reminder>";

/// The heading line of a `<system-reminder>` text: its first line inside the tag.
fn reminder_heading(text: &str) -> Option<&str> {
    let body = text.strip_prefix(SYSTEM_REMINDER_OPEN)?;
    body.lines().next().filter(|line| !line.trim().is_empty())
}

/// Text of the newest `<system-reminder>` user message whose heading is `heading`.
fn latest_reminder<'a>(messages: &'a [Message], heading: &str) -> Option<&'a str> {
    messages.iter().rev().find_map(|message| {
        if message.role != Role::User {
            return None;
        }
        let [ContentBlock::Text { text, .. }] = message.content.as_slice() else {
            return None;
        };
        (reminder_heading(text) == Some(heading)).then_some(text.as_str())
    })
}

/// The message that brings a transcript's newest reminder of the same heading
/// up to date, or `None` when it already carries `reminder`.
pub fn reminder_update(messages: &[Message], reminder: &str) -> Option<Message> {
    let heading = reminder_heading(reminder)?;
    (latest_reminder(messages, heading) != Some(reminder)).then(|| Message::user(reminder))
}

/// The turn-context reminder to append for this request's dynamic system
/// context, or `None` when the transcript's newest turn context already says
/// the same.
///
/// Context that was present and is now gone is cleared with a `(none)` turn
/// context, so the model does not keep acting on a stale one.
pub fn turn_context_update(messages: &[Message], system_dynamic: &str) -> Option<Message> {
    let body = system_dynamic.trim();
    let latest = latest_reminder(messages, TURN_CONTEXT_HEADING);
    if body.is_empty() && latest.is_none() {
        return None;
    }
    let body = if body.is_empty() { "(none)" } else { body };
    // Compared in place: an unchanged context, the common case, is answered
    // without building the reminder it would have sent.
    let unchanged = latest.is_some_and(|text| {
        text.strip_prefix(TURN_CONTEXT_OPEN)
            .and_then(|rest| rest.strip_suffix(TURN_CONTEXT_CLOSE))
            == Some(body)
    });
    if unchanged {
        return None;
    }
    let mut reminder = String::with_capacity(TURN_CONTEXT_OPEN.len() + body.len() + TURN_CONTEXT_CLOSE.len());
    reminder.push_str(TURN_CONTEXT_OPEN);
    reminder.push_str(body);
    reminder.push_str(TURN_CONTEXT_CLOSE);
    Some(Message::user(&reminder))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTES_A: &str = "# Context Notes\n\nstep one done";
    const NOTES_B: &str = "# Context Notes\n\nstep two done";

    fn turn_context(body: &str) -> String {
        format!(
            "<system-reminder>\n# Turn context\nThis replaces any earlier turn context.\n\n{body}\n</system-reminder>"
        )
    }

    fn assert_user_text(message: &Message, text: &str) {
        assert_eq!(message.role, Role::User);
        match message.content.as_slice() {
            [ContentBlock::Text { text: actual, .. }] => assert_eq!(actual, text),
            other => panic!("expected one text block, got {other:?}"),
        }
    }

    #[test]
    fn turn_context_is_appended_once_and_again_only_when_it_changes() {
        let mut transcript = vec![Message::user("start")];
        let first = turn_context_update(&transcript, NOTES_A).expect("new context is sent");
        assert_user_text(&first, &turn_context(NOTES_A));
        transcript.push(first);
        transcript.push(Message::assistant_text("working"));

        assert!(
            turn_context_update(&transcript, &format!("  {NOTES_A}\n")).is_none(),
            "the same context is not sent twice"
        );

        let changed = turn_context_update(&transcript, NOTES_B).expect("changed context is sent");
        assert_user_text(&changed, &turn_context(NOTES_B));
    }

    #[test]
    fn turn_context_compares_with_the_newest_copy_only() {
        let transcript = vec![
            Message::user(&turn_context(NOTES_A)),
            Message::user(&turn_context(NOTES_B)),
        ];
        let back = turn_context_update(&transcript, NOTES_A).expect("A is not the context in force");
        assert_user_text(&back, &turn_context(NOTES_A));
    }

    #[test]
    fn turn_context_is_cleared_only_after_one_was_sent() {
        assert!(turn_context_update(&[Message::user("start")], " \n").is_none());

        let mut transcript = vec![Message::user(&turn_context(NOTES_A))];
        let cleared = turn_context_update(&transcript, "").expect("stale context is cleared");
        assert_user_text(&cleared, &turn_context("(none)"));
        transcript.push(cleared);
        assert!(turn_context_update(&transcript, "").is_none(), "cleared once");
    }

    #[test]
    fn reminders_with_other_headings_do_not_count_as_turn_context() {
        let transcript = vec![
            Message::user(&turn_context(NOTES_A)),
            Message::user("<system-reminder>\n# Memory Summary\n\nfacts\n</system-reminder>"),
        ];
        assert!(turn_context_update(&transcript, NOTES_A).is_none());

        let memory = "<system-reminder>\n# Memory Summary\n\nfacts\n</system-reminder>";
        assert!(
            reminder_update(&transcript, memory).is_none(),
            "the same summary is not sent twice"
        );
        let newer = "<system-reminder>\n# Memory Summary\n\nmore facts\n</system-reminder>";
        assert_user_text(
            &reminder_update(&transcript, newer).expect("a changed summary is sent"),
            newer,
        );
    }
}
