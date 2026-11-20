//! Foreground state and replay behavior.

use super::*;
use tokio::sync::oneshot;

fn idle(hint: &str) -> ServerEvent {
    ServerEvent::ForegroundState {
        session_id: "s".into(),
        running: false,
        run_id: "r".into(),
        start_message_id: None,
        user_content: None,
        awaiting_user: false,
        stop_reason: Some(hint.into()),
        snapshot: false,
    }
}

fn settle(events: Vec<ServerEvent>, hint: &str) -> String {
    let mut observer = StopObserver::default();
    for mut event in events {
        observer.apply(&mut event);
    }
    let mut idle = idle(hint);
    observer.apply(&mut idle);
    let ServerEvent::ForegroundState { stop_reason, .. } = idle else {
        unreachable!()
    };
    stop_reason.expect("the observer always settles a reason")
}

fn stopped(reason: TurnStopReason, provider: Option<&str>) -> ServerEvent {
    ServerEvent::TurnStopped {
        reason,
        message: "m".into(),
        provider_stop_reason: provider.map(str::to_string),
    }
}

fn started_turn(session_id: &str) -> (ForegroundGuard, mpsc::UnboundedReceiver<ServerEvent>) {
    started_run(session_id, "run-1")
}

fn started_run(session_id: &str, run_id: &str) -> (ForegroundGuard, mpsc::UnboundedReceiver<ServerEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let guard = ForegroundGuard::new(&tx);
    let mut report = guard.report(None);
    report.inserted(session_id, "message-1");
    report.started(&tx, session_id, run_id, None);
    (guard, rx)
}

fn idle_reason(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Option<Option<String>> {
    let mut reason = None;
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::ForegroundState {
            running: false,
            stop_reason,
            ..
        } = event
        {
            reason = Some(stop_reason);
        }
    }
    reason
}

/// A History row says it started a turn only to a v2 connection and only
/// when it did; every other row and every other client gets exactly the
/// bytes it got before the marker existed.
#[test]
fn history_rows_mark_turn_starts_for_v2_only() {
    use crate::message::{ContentBlock, Role};
    let stored = |id: &str, turn_start: bool| crate::session::StoredMessage {
        original_input: None,
        arg_repairs: None,
        degradations: None,
        turn_start,
        id: id.to_string(),
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: id.to_string(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    };
    let stored = [stored("m1", true), stored("si1", false)];
    let rendered = |index: usize| crate::session::RenderedMessage {
        response_stats: None,
        role: "user".to_string(),
        content: stored[index].id.clone(),
        tool_calls: Vec::new(),
        tool_data: None,
        stored_index: Some(index),
        stored_id: Some(stored[index].id.clone()),
    };
    let row = |index: usize, include_message_ids: bool| {
        serde_json::to_string(&history_message(rendered(index), &stored, include_message_ids)).unwrap()
    };
    assert_eq!(row(0, false), r#"{"role":"user","content":"m1"}"#);
    assert_eq!(row(1, false), r#"{"role":"user","content":"si1"}"#);
    assert_eq!(
        row(0, true),
        r#"{"message_id":"m1","turn_start":true,"role":"user","content":"m1"}"#
    );
    assert_eq!(row(1, true), r#"{"message_id":"si1","role":"user","content":"si1"}"#);
}

/// A compacted session renders the whole history while the resident context
/// holds only its tail, so a row's `stored_index` can point at the wrong
/// message. The id rides on the row itself, copied while rendering, and must
/// name that original rather than whatever sits at the index.
#[test]
fn a_history_row_names_the_message_it_was_rendered_from_not_the_index() {
    use crate::message::{ContentBlock, Role};
    let stored = |id: &str| crate::session::StoredMessage {
        original_input: None,
        arg_repairs: None,
        degradations: None,
        turn_start: false,
        id: id.to_string(),
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: id.to_string(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    };
    // The resident context is the tail; the rendered row came from an
    // earlier message whose index is not in it.
    let stored = [stored("resident")];
    let row = history_message(
        crate::session::RenderedMessage {
            response_stats: None,
            role: "assistant".to_string(),
            content: "earlier".to_string(),
            tool_calls: Vec::new(),
            tool_data: None,
            stored_index: Some(0),
            stored_id: Some("compacted-away".to_string()),
        },
        &stored,
        true,
    );
    assert_eq!(row.message_id.as_deref(), Some("compacted-away"));
}

#[test]
fn a_cancel_hold_delays_idle_and_marks_cancelled_only_when_asked() {
    // Released without a stop (a cancel that returned early): the turn's
    // own end stands.
    let (mut guard, mut rx) = started_turn("hold-session-released");
    let hold = defer_idle_for_cancel("hold-session-released").expect("a running turn");
    guard.finish(true);
    drop(guard);
    assert_eq!(idle_reason(&mut rx), None, "no idle while the hold is alive");
    drop(hold);
    assert_eq!(idle_reason(&mut rx), Some(Some("end_turn".to_string())));

    // A cancel that stopped the turn marks it.
    let (mut guard, mut rx) = started_turn("hold-session-stopped");
    let hold = defer_idle_for_cancel("hold-session-stopped").expect("a running turn");
    hold.mark_cancelled();
    guard.finish(true);
    drop(guard);
    drop(hold);
    assert_eq!(idle_reason(&mut rx), Some(Some("cancelled".to_string())));
    assert!(
        defer_idle_for_cancel("hold-session-stopped").is_none(),
        "nothing runs any more"
    );
}

/// An owner aborted while a cancel holds its idle state (todo 16) does not
/// release its lock with the task: the lock stays parked until the held
/// idle state went out and a fanout task reported it forwarded.
#[tokio::test]
async fn an_aborted_owner_keeps_its_lock_until_the_held_idle_is_forwarded() {
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    let (guard, mut rx) = started_run("abort-session", "abort-run");
    let kept = guard.keep_until_forwarded(Arc::clone(&lock).lock_owned().await);
    let hold = defer_idle_for_cancel("abort-session").expect("a running turn");
    hold.mark_cancelled();
    let (parked_tx, parked) = oneshot::channel();
    let owner = tokio::spawn(async move {
        let _owner = (guard, kept);
        let _ = parked_tx.send(());
        std::future::pending::<()>().await;
    });
    parked.await.expect("the owner runs");
    owner.abort();
    assert!(owner.await.expect_err("the owner was aborted").is_cancelled());

    assert!(lock.try_lock().is_err(), "the aborted owner's lock is parked");
    assert_eq!(idle_reason(&mut rx), None, "no idle while the cancel holds it");
    drop(hold);
    assert_eq!(idle_reason(&mut rx), Some(Some("cancelled".to_string())));
    assert!(
        lock.try_lock().is_err(),
        "the lock stays parked until the idle state was forwarded"
    );
    note_forwarded("abort-run");
    let _released = tokio::time::timeout(Duration::from_secs(5), lock.lock())
        .await
        .expect("the lock is released once the idle state was forwarded");
}

/// The owner that closes its turn normally releases its lock only after
/// the idle state was forwarded.
#[tokio::test]
async fn close_releases_the_lock_after_the_idle_is_forwarded() {
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    let (mut guard, mut rx) = started_run("close-session", "close-run");
    let kept = guard.keep_until_forwarded(Arc::clone(&lock).lock_owned().await);
    guard.finish(true);
    let closing = tokio::spawn(guard.close(kept));
    // The idle state is sent before close waits, so its arrival means close
    // is waiting for the forward.
    let idle = loop {
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("close sends the idle state")
            .expect("the sender is alive");
        if let ServerEvent::ForegroundState {
            running: false,
            stop_reason,
            ..
        } = event
        {
            break stop_reason;
        }
    };
    assert_eq!(idle, Some("end_turn".to_string()));
    assert!(lock.try_lock().is_err(), "close holds the lock until the forward");
    note_forwarded("close-run");
    tokio::time::timeout(Duration::from_secs(5), closing)
        .await
        .expect("close returns once the idle state was forwarded")
        .expect("close does not panic");
    assert!(lock.try_lock().is_ok());
}

/// An owner aborted while it waits in `close` (the cancel path's
/// cooperative timeout) does not release its lock with the task: the lock
/// stays parked until the idle state was forwarded, so the next turn cannot
/// report `running` before that idle state is out.
#[tokio::test]
async fn an_owner_aborted_while_closing_keeps_its_lock_until_the_idle_is_forwarded() {
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    let (mut guard, mut rx) = started_run("close-abort-session", "close-abort-run");
    let kept = guard.keep_until_forwarded(Arc::clone(&lock).lock_owned().await);
    guard.finish(true);
    let closing = tokio::spawn(guard.close(kept));
    // The idle state is sent before close waits for its forward: once it
    // arrived, the owner is in (or about to enter) that wait.
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("close sends the idle state")
            .expect("the sender is alive");
        if let ServerEvent::ForegroundState {
            running: false,
            stop_reason,
            ..
        } = event
        {
            assert_eq!(stop_reason.as_deref(), Some("end_turn"));
            break;
        }
    }
    closing.abort();
    assert!(closing.await.expect_err("the owner was aborted").is_cancelled());

    assert!(
        lock.try_lock().is_err(),
        "the aborted owner's lock stays parked until the idle state was forwarded"
    );
    note_forwarded("close-abort-run");
    let _released = tokio::time::timeout(Duration::from_secs(5), lock.lock())
        .await
        .expect("the lock is released once the idle state was forwarded");
}

#[test]
fn stop_reasons_follow_the_v1_table() {
    assert_eq!(settle(vec![], "end_turn"), "end_turn");
    assert_eq!(settle(vec![], "error"), "error");
    assert_eq!(settle(vec![], "cancelled"), "cancelled");
    assert_eq!(
        settle(
            vec![stopped(TurnStopReason::LimitReached, Some("max_tokens"))],
            "end_turn"
        ),
        "max_tokens"
    );
    assert_eq!(
        settle(vec![stopped(TurnStopReason::LimitReached, None)], "end_turn"),
        "max_turn_requests"
    );
    assert_eq!(
        settle(vec![stopped(TurnStopReason::ProviderGuardrail, None)], "end_turn"),
        "refusal"
    );
    assert_eq!(
        settle(vec![stopped(TurnStopReason::Failure, None)], "end_turn"),
        "error"
    );
    assert_eq!(settle(vec![ServerEvent::Interrupted], "end_turn"), "cancelled");
    assert_eq!(
        settle(vec![stopped(TurnStopReason::LimitReached, Some("length"))], "cancelled"),
        "cancelled"
    );
}

#[test]
fn a_provider_stop_reason_matches_whatever_case_the_wire_spells_it() {
    // Gemini reports `MAX_TOKENS` where the OpenAI wires report
    // `max_tokens`, and both must reach the same ACP stop reason.
    for reason in ["max_tokens", "MAX_TOKENS", "Max_Tokens"] {
        assert_eq!(
            settle(vec![stopped(TurnStopReason::LimitReached, Some(reason))], "end_turn"),
            "max_tokens",
            "{reason}"
        );
    }
    for reason in [
        "max_output_tokens",
        "MAX_OUTPUT_TOKENS",
        "length",
        "LENGTH",
        "incomplete",
    ] {
        assert_eq!(
            settle(vec![stopped(TurnStopReason::LimitReached, Some(reason))], "end_turn"),
            "max_tokens",
            "{reason}"
        );
    }
    // A stop that is not an output limit still reports the request limit.
    for reason in ["end_turn", "STOP", "tool_use"] {
        assert_eq!(
            settle(vec![stopped(TurnStopReason::LimitReached, Some(reason))], "end_turn"),
            "max_turn_requests",
            "{reason}"
        );
    }
    // The guardrail check follows the same rule.
    for reason in ["refusal", "REFUSAL", "content_filter", "Content_Filter"] {
        assert_eq!(
            settle(
                vec![ServerEvent::ProviderGuardrail {
                    stop_reason: Some(reason.to_string()),
                    message: "blocked".to_string(),
                }],
                "end_turn"
            ),
            "refusal",
            "{reason}"
        );
    }
}
