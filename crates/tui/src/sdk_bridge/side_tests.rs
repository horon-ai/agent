//! The side question's bridge half: the order that makes a fork safe, the way
//! the child's events become the overlay's answer, and the answer that is read
//! back rather than trusted to the stream.
//!
//! Nothing here connects to a daemon. The steps that would are one SDK call
//! each; what these tests pin is the order they must run in and what a frame of
//! the child's stream means. The daemon's own test owns the claim that the
//! stored child is read-only.

use std::cell::RefCell;

use horon_agent_sdk::api::ApiEvent;
use horon_agent_sdk::{Error, ErrorKind, HistoryMessage};

use super::Incoming;
use super::side::{Effect, Preview, Progress, SideLink, observe, open_with, stored_answer};

/// A link that records the calls made on it, and can refuse one by name.
struct Recorder {
    calls: RefCell<Vec<String>>,
    refuse: Option<&'static str>,
}

impl Recorder {
    fn new(refuse: Option<&'static str>) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            refuse,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

/// A failure the recorder returns, so a refused step carries a typed cause.
fn refusal() -> Error {
    Error::new(ErrorKind::Timeout, "no")
}

impl SideLink for Recorder {
    fn attach(&self, session: &str) -> Result<(), Error> {
        self.calls.borrow_mut().push(format!("attach {session}"));
        if self.refuse == Some("attach") {
            return Err(refusal());
        }
        Ok(())
    }

    fn fork(&self, parent: &str, boundary: Option<&str>) -> Result<String, Error> {
        match boundary {
            Some(boundary) => self.calls.borrow_mut().push(format!("fork {parent} at {boundary}")),
            None => self.calls.borrow_mut().push(format!("fork {parent}")),
        }
        if self.refuse == Some("fork") {
            return Err(refusal());
        }
        Ok("child".to_string())
    }

    fn baseline(&self, child: &str) -> Result<usize, Error> {
        self.calls.borrow_mut().push(format!("baseline {child}"));
        if self.refuse == Some("baseline") {
            return Err(refusal());
        }
        Ok(3)
    }

    fn ask(&self, child: &str, question: &str) -> Result<(), Error> {
        self.calls.borrow_mut().push(format!("ask {child} {question}"));
        if self.refuse == Some("ask") {
            return Err(refusal());
        }
        Ok(())
    }
}

fn message(role: &str, content: &str, answer: Option<&str>) -> HistoryMessage {
    HistoryMessage {
        role: role.to_string(),
        content: content.to_string(),
        response_stats: None,
        answer: answer.map(str::to_string),
        message_id: None,
    }
}

#[test]
fn the_child_is_created_narrow_before_the_question_is_sent_and_announced() {
    let link = Recorder::new(None);
    let mut announced = Vec::new();
    let (baseline, child) = open_with(&link, "parent", None, "what changed?", &mut |child: &str| {
        announced.push(child.to_string())
    })
    .expect("the steps succeed");
    assert_eq!(baseline, 3, "the transcript boundary was not read before the question");
    assert_eq!(child, "child");
    assert_eq!(
        link.calls(),
        [
            "attach parent".to_string(),
            "fork parent".to_string(),
            "attach child".to_string(),
            "baseline child".to_string(),
            "ask child what changed?".to_string(),
        ],
        "a question must not reach a child the daemon did not create narrow"
    );
    assert_eq!(announced, ["child"], "the child id was not announced exactly once");
}

#[test]
fn a_refused_fork_leaves_the_child_unannounced_and_unasked() {
    let link = Recorder::new(Some("fork"));
    let mut announced = Vec::new();
    let error = open_with(&link, "parent", None, "what changed?", &mut |child: &str| {
        announced.push(child.to_string())
    })
    .expect_err("the refusal must travel");
    assert!(
        announced.is_empty(),
        "a child that was never created was announced anyway"
    );
    assert_eq!(error.to_string(), "the harness refused a side session: timeout: no");
    assert_eq!(
        link.calls(),
        ["attach parent".to_string(), "fork parent".to_string()],
        "the question was sent without a child"
    );
}

#[test]
fn a_failure_after_the_question_was_sent_names_the_child() {
    let link = Recorder::new(Some("ask"));
    let error = open_with(&link, "parent", None, "why?", &mut |_| {}).expect_err("the refusal must travel");
    assert_eq!(
        error.to_string(),
        "the side question could not be sent: timeout: no; the side session is child"
    );
}

#[test]
fn a_preview_joins_messages_and_rewrites_a_replaced_one() {
    let mut preview = Preview::default();
    preview.delta(Some("a"), "first");
    preview.delta(Some("a"), " answer");
    preview.delta(Some("b"), "second");
    assert_eq!(preview.text(), "first answer\n\nsecond");

    // A retraction of the message still being written must remove its text,
    // not append another copy of the replacement.
    preview.replace(Some("b"), "rewritten");
    assert_eq!(preview.text(), "first answer\n\nrewritten");

    // A retraction of an earlier message rewrites that one in place.
    preview.replace(Some("a"), "");
    assert_eq!(preview.text(), "rewritten");
}

#[test]
fn an_idless_delta_appends_to_the_open_message() {
    let mut preview = Preview::default();
    preview.delta(Some("a"), "one");
    preview.delta(None, " two");
    assert_eq!(preview.text(), "one two", "an id-less delta started a new message");
}

#[test]
fn another_sessions_text_never_reaches_a_side_answer() {
    let mut preview = Preview::default();
    let event = ApiEvent::TextDelta {
        session_id: "someone-else".to_string(),
        text: "not ours".to_string(),
        message_id: None,
    };
    let (effects, progress) = observe(&event, 7, "child", &mut preview);
    assert!(effects.is_empty());
    assert_eq!(progress, Progress::Continue);
    assert_eq!(preview.text(), "");
}

#[test]
fn a_text_delta_reaches_the_overlay_as_the_answer_so_far() {
    let mut preview = Preview::default();
    let event = ApiEvent::TextDelta {
        session_id: "child".to_string(),
        text: "the cache".to_string(),
        message_id: Some("m1".to_string()),
    };
    let (effects, _) = observe(&event, 7, "child", &mut preview);
    assert_eq!(
        effects,
        [Effect::Send(Incoming::SideAnswer {
            id: 7,
            answer: "the cache".to_string()
        })]
    );
}

#[test]
fn a_parked_approval_is_refused_rather_than_granted() {
    let mut preview = Preview::default();
    let request = ApiEvent::PermissionRequest {
        session_id: "child".to_string(),
        request_id: "req-1".to_string(),
        tool_name: "read".to_string(),
        description: "read a restricted path".to_string(),
    };
    let (effects, _) = observe(&request, 7, "child", &mut preview);
    assert_eq!(effects[0], Effect::Deny("req-1".to_string()));
    assert!(
        matches!(&effects[1], Effect::Send(Incoming::SideNotice { line, .. }) if line.contains("read")),
        "the refusal did not say what was refused: {effects:?}"
    );

    let pending = ApiEvent::ApprovalPending {
        session_id: "child".to_string(),
        request_id: "req-2".to_string(),
        action: "write a file".to_string(),
        expires_at: None,
    };
    let (effects, _) = observe(&pending, 7, "child", &mut preview);
    assert_eq!(effects[0], Effect::Deny("req-2".to_string()));
}

#[test]
fn a_stopped_turn_is_reported_and_a_done_turn_finishes() {
    let mut preview = Preview::default();
    let stopped = ApiEvent::TurnStopped {
        session_id: "child".to_string(),
        reason: horon_agent_sdk::api::TurnStopReason::Unknown,
        message: "the provider failed".to_string(),
        provider_stop_reason: None,
    };
    let (effects, progress) = observe(&stopped, 7, "child", &mut preview);
    assert_eq!(progress, Progress::Continue);
    assert_eq!(
        effects,
        [Effect::Send(Incoming::SideNotice {
            id: 7,
            line: "the provider failed".to_string()
        })]
    );

    let done = ApiEvent::TurnDone {
        session_id: "child".to_string(),
        pending_soft_interrupts: None,
    };
    let (effects, progress) = observe(&done, 7, "child", &mut preview);
    assert!(effects.is_empty());
    assert_eq!(progress, Progress::Finished);
}

#[test]
fn the_stored_answer_skips_the_forked_transcript() {
    let messages = [
        message("user", "the parent question", None),
        message("assistant", "the parent answer", Some("the parent answer")),
        message("user", "the side question", None),
        message("assistant", "the side answer", Some("the side answer")),
    ];
    // The forked transcript is three rows; only the fourth is the child's.
    assert_eq!(stored_answer(&messages, 3).as_deref(), Some("the side answer"));
    // A turn that stored no answer must not borrow the parent's.
    assert_eq!(stored_answer(&messages, 4), None);
    assert_eq!(stored_answer(&messages, 2).as_deref(), Some("the side answer"));
}

#[test]
fn the_stored_answer_prefers_the_answer_field_over_the_markup_content() {
    let messages = [
        message("user", "q", None),
        message(
            "assistant",
            "\u{1b}[2mthinking\u{1b}[0m\n\nthe answer",
            Some("the answer"),
        ),
    ];
    assert_eq!(stored_answer(&messages, 1).as_deref(), Some("the answer"));
}
