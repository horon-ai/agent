//! How the transcript accumulates deltas, tool calls, and reasoning into items.

use super::*;

#[test]
fn deltas_accumulate_and_a_new_message_id_splits() {
    let mut transcript = Transcript::new();
    transcript.text_delta(Some("m1"), "one ");
    transcript.text_delta(Some("m1"), "two");
    transcript.text_delta(Some("m2"), "three");
    transcript.finish_stream();
    assert_eq!(
        transcript.items(),
        &[
            Item::Assistant("one two".to_string()),
            Item::Assistant("three".to_string())
        ]
    );
}

#[test]
fn empty_message_is_not_published() {
    let mut transcript = Transcript::new();
    transcript.text_delta(Some("m1"), "draft");
    transcript.text_replace(Some("m1"), "");
    transcript.finish_stream();
    assert!(transcript.items().is_empty());
}

#[test]
fn recent_assistant_text_keeps_answers_only() {
    let mut transcript = Transcript::new();
    transcript.push_user("look at https://user.example/");
    transcript.push_assistant("first https://one.example/");
    transcript.push_assistant("second https://two.example/");
    transcript.text_delta(Some("m9"), "streaming https://three.example/");
    assert_eq!(
        transcript.recent_assistant_text(2).collect::<Vec<_>>(),
        vec!["streaming https://three.example/", "second https://two.example/"]
    );
    assert_eq!(transcript.recent_assistant_text(8).count(), 3);
}

#[test]
fn tool_done_updates_the_running_line() {
    let mut transcript = Transcript::new();
    transcript.tool_start("c1", "bash");
    transcript.tool_done("c1", "bash", None, None);
    let items = transcript.items();
    assert_eq!(items.len(), 1, "{items:?}");
    let Item::Tool(call) = &items[0] else {
        panic!("expected one tool line: {items:?}");
    };
    assert_eq!(call.name, "bash");
    assert_eq!(call.state, ToolState::Done);
}

#[test]
fn open_text_reports_the_message_until_it_is_committed() {
    let mut transcript = Transcript::new();
    assert_eq!(transcript.open_text(), None, "nothing is open yet");

    transcript.text_delta(Some("m1"), "partial");
    assert_eq!(transcript.open_text(), Some("partial"));

    transcript.text_delta(Some("m1"), " more");
    assert_eq!(transcript.open_text(), Some("partial more"));

    // The committed item is the same text, so the renderer must stop
    // drawing the open one or the reply would appear twice.
    transcript.finish_stream();
    assert_eq!(transcript.open_text(), None);
    assert_eq!(transcript.items(), &[Item::Assistant("partial more".to_string())]);
}

#[test]
fn open_text_hides_blank_and_retracted_messages() {
    let mut transcript = Transcript::new();
    transcript.text_delta(Some("m1"), "  \n\t");
    assert_eq!(transcript.open_text(), None, "whitespace is never published");

    // Deltas append, so the whitespace that was hidden on its own is now
    // part of the message; drawing anything but the raw buffer would make
    // the open row differ from the item `finish_stream` commits.
    transcript.text_delta(Some("m1"), "draft");
    assert_eq!(transcript.open_text(), Some("  \n\tdraft"));
    transcript.text_replace(Some("m1"), "");
    assert_eq!(transcript.open_text(), None, "a retraction must not be drawn");
}

#[test]
fn reasoning_is_dropped_while_the_setting_is_off() {
    let mut transcript = Transcript::new();
    transcript.reasoning_delta("thinking about it");
    assert_eq!(
        transcript.open_reasoning(),
        None,
        "off means the client keeps nothing to draw"
    );
    transcript.reasoning_done();
    assert!(
        !transcript.items().iter().any(|item| matches!(item, Item::Reasoning(_))),
        "and nothing reaches the transcript: {:?}",
        transcript.items()
    );
}

#[test]
fn reasoning_is_kept_apart_from_the_answer_when_the_setting_is_on() {
    let mut transcript = Transcript::new();
    transcript.set_show_reasoning(true);
    transcript.reasoning_delta("first I look");
    transcript.reasoning_delta(" at the file");
    assert_eq!(transcript.open_reasoning(), Some("first I look at the file"));
    transcript.reasoning_done();
    assert_eq!(transcript.open_reasoning(), None, "a boundary commits it");

    transcript.text_delta(None, "here is the answer");
    assert_eq!(
        transcript.open_text(),
        Some("here is the answer"),
        "the reasoning never joins the answer"
    );
    transcript.text_done();

    let items = transcript.items();
    assert_eq!(
        items,
        &[
            Item::Reasoning("first I look at the file".to_string()),
            Item::Assistant("here is the answer".to_string()),
        ],
        "reasoning precedes its answer in display order"
    );
}

#[test]
fn a_tool_call_commits_the_reasoning_that_preceded_it() {
    let mut transcript = Transcript::new();
    transcript.set_show_reasoning(true);
    transcript.reasoning_delta("I need to read the file");
    transcript.tool_start("c1", "read");
    assert_eq!(transcript.open_reasoning(), None);
    assert_eq!(
        transcript.items().first(),
        Some(&Item::Reasoning("I need to read the file".to_string())),
        "the thought belongs above the call it caused: {:?}",
        transcript.items()
    );
}

#[test]
fn turning_reasoning_off_retracts_a_half_written_thought() {
    let mut transcript = Transcript::new();
    transcript.set_show_reasoning(true);
    transcript.reasoning_delta("half a thought");
    transcript.set_show_reasoning(false);
    assert_eq!(transcript.open_reasoning(), None);
    transcript.reasoning_done();
    assert!(
        !transcript.items().iter().any(|item| matches!(item, Item::Reasoning(_))),
        "nothing half-finished stays on screen: {:?}",
        transcript.items()
    );
}

#[test]
fn blank_reasoning_is_never_published() {
    let mut transcript = Transcript::new();
    transcript.set_show_reasoning(true);
    transcript.reasoning_delta("   ");
    transcript.reasoning_done();
    assert!(
        transcript.items().is_empty(),
        "whitespace alone is not a thought: {:?}",
        transcript.items()
    );
}

#[test]
fn tool_done_without_a_start_still_renders() {
    let mut transcript = Transcript::new();
    transcript.tool_done("c1", "read", None, Some("no such file"));
    let items = transcript.items();
    assert_eq!(items.len(), 1, "{items:?}");
    let Item::Tool(call) = &items[0] else {
        panic!("expected one tool line: {items:?}");
    };
    assert_eq!(call.name, "read");
    assert_eq!(call.state, ToolState::Failed("no such file".to_string()));
}
