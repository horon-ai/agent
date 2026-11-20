//! What the transcript marks when the harness made a request miss the prompt
//! cache.
//!
//! The daemon reports one classified miss per completed provider request, right
//! after that request's usage. Each test drives a real [`App`] through those
//! reports and pins what the reader is shown: one mark under the request that
//! cost the resend, and none for a report the client cannot classify or that
//! describes another conversation.

use std::sync::mpsc;

use horon_agent_sdk::api::ApiEvent;

use super::App;
use crate::sdk_bridge::Incoming;
use crate::transcript::{CacheCause, CacheMiss, Item};

fn app() -> App {
    let (_sender, receiver) = mpsc::channel::<Incoming>();
    App::new("socket · s1".to_string(), receiver)
}

/// One provider request's assistant message, as the stream and its end arrive.
fn answer(app: &mut App, text: &str) {
    app.apply_event(&ApiEvent::TextDelta {
        session_id: "s1".into(),
        text: text.into(),
        message_id: Some("m1".into()),
    });
    app.apply_event(&ApiEvent::TextDone {
        session_id: "s1".into(),
        message_id: Some("m1".into()),
    });
}

/// The usage report that precedes a miss for the same request.
fn usage(app: &mut App) {
    app.apply_event(&ApiEvent::TokenUsage {
        session_id: "s1".into(),
        input: 50_000,
        output: 10,
        cache_read_input: Some(4_000),
        cache_creation_input: None,
    });
}

/// One miss report, with the fields the client does not read left at what the
/// daemon sends.
fn miss(app: &mut App, reason: &str, harness_caused: bool, documented_cause: Option<&str>) {
    app.apply_event(&ApiEvent::KvCacheMiss {
        session_id: "s1".into(),
        reason: reason.into(),
        harness_caused,
        missed_tokens: 46_000,
        expected_tokens: 50_000,
        read_tokens: 4_000,
        documented_cause: documented_cause.map(str::to_string),
        message: "KV cache miss: ~46K tokens resent".into(),
    });
}

/// The marks the transcript holds, in display order.
fn marks(app: &App) -> Vec<&CacheMiss> {
    app.transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::CacheMiss(miss) => Some(miss),
            _ => None,
        })
        .collect()
}

#[test]
fn an_avoidable_miss_is_marked_under_the_request_it_belongs_to() {
    let mut app = app();
    answer(&mut app, "ANSWER");
    usage(&mut app);
    miss(&mut app, "prefix_changed", true, None);

    let items = app.transcript.items();
    assert_eq!(
        items,
        &[
            Item::Assistant("ANSWER".to_string()),
            Item::CacheMiss(CacheMiss {
                cause: CacheCause::Prefix,
                missed_tokens: 46_000,
            }),
        ],
        "the mark must follow the answer that cost the resend: {items:?}"
    );
}

#[test]
fn a_replayed_miss_is_marked_once() {
    let mut app = app();
    answer(&mut app, "ANSWER");
    usage(&mut app);
    miss(&mut app, "tools_changed", true, None);
    miss(&mut app, "tools_changed", true, None);

    assert_eq!(marks(&app).len(), 1);
}

#[test]
fn each_request_that_misses_gets_its_own_mark() {
    let mut app = app();
    answer(&mut app, "FIRST");
    usage(&mut app);
    miss(&mut app, "system_changed", true, None);
    answer(&mut app, "SECOND");
    usage(&mut app);
    miss(&mut app, "system_changed", true, None);

    assert_eq!(
        marks(&app).len(),
        2,
        "a second request lost its mark: {:?}",
        app.transcript.items()
    );
}

#[test]
fn an_unavoidable_or_unclassified_miss_is_not_marked() {
    let cases = [
        // A provider or model switch is the provider's behaviour, not the
        // harness's mistake.
        ("provider_switch", false, None),
        ("expired", false, None),
        ("low_read", false, None),
        // A documented invalidation is an intentional reload.
        ("prefix_changed", true, Some("config reload")),
        // A cause this client does not know is left unclassified rather than
        // drawn as an alarm.
        ("added_later", true, None),
        // The daemon derives one from the other, so a report where they
        // disagree cannot be trusted.
        ("system_changed", false, None),
    ];
    for (reason, harness_caused, documented) in cases {
        let mut app = app();
        answer(&mut app, "ANSWER");
        usage(&mut app);
        miss(&mut app, reason, harness_caused, documented);
        assert!(
            marks(&app).is_empty(),
            "{reason} (harness_caused={harness_caused}, documented={documented:?}) was marked: {:?}",
            app.transcript.items()
        );
    }
}

#[test]
fn a_miss_for_another_session_is_not_marked() {
    let mut app = app();
    answer(&mut app, "ANSWER");
    usage(&mut app);
    // Neither the usage nor the stream ever named this session: it is a frame
    // already queued when the client moved to the conversation on screen.
    app.session_id = Some("other".into());
    miss(&mut app, "prefix_changed", true, None);

    assert!(
        marks(&app).is_empty(),
        "another conversation's miss was drawn: {:?}",
        app.transcript.items()
    );
}
