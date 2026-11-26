//! Live output notices for a finite background job (#466).
//!
//! Every test drives the tool the model calls and reads what the owner receives,
//! so nothing here pins the matcher's internals: the pattern is compiled, the
//! notice is published, and the job either runs or does not.

use super::*;
use crate::bus::{BackgroundTaskStatus, Bus, BusEvent};
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

fn notify_ctx(session: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "notify-msg".to_string(),
        tool_call_id: "notify-call".to_string(),
        working_dir: Some(std::path::PathBuf::from("/tmp")),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

/// Start a finite background job and return its task id.
async fn start_job(session: &str, input: serde_json::Value) -> String {
    let output = BashTool::new()
        .execute(input, notify_ctx(session))
        .await
        .expect("the background job should start");
    output.metadata.expect("background metadata")["task_id"]
        .as_str()
        .expect("task id")
        .to_string()
}

/// The next live notice published for `session`, or `None` when the job's
/// notices have been drained.
///
/// The bus is process-wide, so every other event — and every other session's
/// notices — is skipped rather than treated as a failure, and a lagged receiver
/// keeps reading instead of losing the rest of the stream.
async fn next_notice(bus: &mut broadcast::Receiver<BusEvent>, session: &str, wait: Duration) -> Option<String> {
    let deadline = Instant::now() + wait;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, bus.recv()).await {
            Ok(Ok(BusEvent::BackgroundOutput { session_id, text })) => {
                if session_id == session {
                    return Some(text);
                }
            }
            Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return None,
        }
    }
}

/// Collect up to `wanted` notices for `session`, giving up after `wait`.
///
/// The job publishes while it runs and the bus is process-wide, so this waits
/// for the publishes rather than sampling a snapshot: a quiet interval means the
/// notices that exist have all arrived.
async fn collect_notices(
    bus: &mut broadcast::Receiver<BusEvent>,
    session: &str,
    wanted: usize,
    wait: Duration,
) -> Vec<String> {
    let deadline = Instant::now() + wait;
    let mut notices = Vec::new();
    while notices.len() < wanted {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match next_notice(bus, session, remaining).await {
            Some(notice) => notices.push(notice),
            None => break,
        }
    }
    notices
}

/// Wait until the job leaves `Running`, then return its stored output.
async fn finished_output(task_id: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = crate::background::global()
            .status(task_id)
            .await
            .expect("the task status file");
        if status.status != BackgroundTaskStatus::Running {
            return crate::background::global()
                .output(task_id)
                .await
                .expect("the task output file");
        }
        assert!(Instant::now() < deadline, "the background job never finished");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Stop a job through the tool an agent uses, so a test cannot leak a child.
async fn cancel_job(session: &str, task_id: &str) {
    let _ = super::super::bg::BgTool::new()
        .execute(json!({"action": "cancel", "task_id": task_id}), notify_ctx(session))
        .await;
}

/// A pattern that cannot compile is refused before the command exists, so an
/// invalid regex cannot leave a process behind.
#[tokio::test]
async fn an_invalid_pattern_starts_no_process() {
    let marker = std::env::temp_dir().join(format!("horon-notify-invalid-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);

    let error = BashTool::new()
        .execute(
            json!({
                "command": format!("touch {}", marker.display()),
                "run_in_background": true,
                "notify": false,
                "notify_pattern": "READY (unclosed",
            }),
            notify_ctx("notify-invalid"),
        )
        .await
        .expect_err("an uncompilable pattern must be refused");

    assert!(
        error.to_string().contains("notify_pattern"),
        "unexpected error: {error}"
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(!marker.exists(), "the command ran despite the invalid pattern");
}

/// The field only means something for a finite background job, so a foreground
/// call and a named service are both refused rather than silently ignored.
#[tokio::test]
async fn only_a_finite_background_job_accepts_a_pattern() {
    let foreground = BashTool::new()
        .execute(
            json!({"command": "printf ok", "notify_pattern": "ok"}),
            notify_ctx("notify-foreground"),
        )
        .await
        .expect_err("a foreground call cannot deliver live notices");
    assert!(
        foreground.to_string().contains("run_in_background"),
        "unexpected error: {foreground}"
    );

    let service = BashTool::new()
        .execute(
            json!({
                "command": "sleep 30",
                "name": "notify-service",
                "notify": false,
                "notify_pattern": "ready",
            }),
            notify_ctx("notify-service-session"),
        )
        .await
        .expect_err("a named service cannot deliver live notices");
    assert!(
        service.to_string().contains("named service"),
        "unexpected error: {service}"
    );
}

/// An empty pattern or name is a model's way of leaving the argument out, so
/// the call runs as a plain foreground command instead of being refused.
#[tokio::test]
async fn an_empty_pattern_or_name_means_none() {
    let output = BashTool::new()
        .execute(
            json!({"command": "printf ok", "notify_pattern": "", "name": ""}),
            notify_ctx("notify-empty"),
        )
        .await
        .expect("an empty pattern must not refuse a foreground call");
    assert!(output.output.contains("ok"), "unexpected output: {}", output.output);
}

/// A match on a running job reaches its owner before the process exits, names
/// the job and the stream, and wakes nothing on its own.
#[tokio::test]
async fn a_match_reaches_the_owner_while_the_job_runs() {
    let session = "notify-live";
    let mut bus = Bus::global().subscribe();

    let task_id = start_job(
        session,
        json!({
            "command": "printf 'READY 42\\n'; sleep 30",
            "run_in_background": true,
            "notify": false,
            "wake": false,
            "notify_pattern": "READY \\d+",
        }),
    )
    .await;

    let notice = next_notice(&mut bus, session, Duration::from_secs(30))
        .await
        .expect("a live notice before the job exits");
    assert!(notice.contains("READY 42"), "notice: {notice}");
    assert!(notice.contains(&task_id), "the notice names its job: {notice}");
    assert!(notice.contains("stdout"), "the notice names its stream: {notice}");

    // Still running, so this was a live notice rather than a completion report.
    let status = crate::background::global()
        .status(&task_id)
        .await
        .expect("the task status file");
    assert_eq!(status.status, BackgroundTaskStatus::Running);

    cancel_job(session, &task_id).await;
}

#[tokio::test]
async fn a_timed_out_job_keeps_output_and_stops_notices() {
    let session = "notify-timeout";
    let mut bus = Bus::global().subscribe();
    let task_id = start_job(
        session,
        json!({
            "command": "printf 'MATCH-before\\n'; sleep 30; printf 'MATCH-after\\n'",
            "run_in_background": true,
            "notify": false,
            "notify_pattern": "^MATCH-",
            "timeout": 2500,
        }),
    )
    .await;

    let notice = next_notice(&mut bus, session, Duration::from_secs(15))
        .await
        .expect("the first matching line arrives before the timeout");
    assert!(notice.ends_with("MATCH-before"), "{notice}");
    let output = finished_output(&task_id).await;
    assert!(output.contains("MATCH-before"), "{output}");
    assert!(output.contains("timed out"), "{output}");
    assert!(!output.contains("MATCH-after"), "{output}");
    assert!(
        next_notice(&mut bus, session, Duration::from_millis(100))
            .await
            .is_none()
    );
}

/// Both pipes are matched, and a command that ends without a newline still has
/// its last line delivered.
#[tokio::test]
async fn both_streams_and_a_final_partial_line_are_matched() {
    let session = "notify-streams";
    let mut bus = Bus::global().subscribe();

    let task_id = start_job(
        session,
        json!({
            "command": "printf 'MATCH-stdout\\n'; printf 'MATCH-stderr\\n' >&2; printf 'MATCH-partial'",
            "run_in_background": true,
            "notify": false,
            "notify_pattern": "MATCH-",
        }),
    )
    .await;

    let _ = finished_output(&task_id).await;
    let notices = collect_notices(&mut bus, session, 3, Duration::from_secs(30)).await;
    assert_eq!(notices.len(), 3, "one notice per matching line: {notices:#?}");
    assert!(
        notices.iter().any(|notice| notice.contains("stdout: MATCH-stdout")),
        "{notices:#?}"
    );
    assert!(
        notices.iter().any(|notice| notice.contains("stderr: MATCH-stderr")),
        "{notices:#?}"
    );
    assert!(
        notices.iter().any(|notice| notice.contains("stdout: MATCH-partial")),
        "the unterminated final line is a notice too: {notices:#?}"
    );
}

/// A burst of matching lines is capped, while every line still reaches the
/// stored output the job's result points at.
#[tokio::test]
async fn a_matching_burst_stays_within_the_notice_budget() {
    let session = "notify-burst";
    let mut bus = Bus::global().subscribe();
    let command = (1..=32)
        .map(|line| format!("printf 'HIT {line}\\n'; "))
        .collect::<String>();

    let task_id = start_job(
        session,
        json!({
            "command": command,
            "run_in_background": true,
            "notify": false,
            "notify_pattern": "HIT ",
        }),
    )
    .await;

    let stored = finished_output(&task_id).await;
    let notices = collect_notices(&mut bus, session, 33, Duration::from_secs(5)).await;
    assert!(!notices.is_empty(), "the first matching lines are still delivered");
    assert!(
        notices.len() <= 20,
        "a burst is capped at the documented budget, got {}",
        notices.len()
    );
    assert!(notices.len() < 32, "the cap engaged instead of forwarding every line");
    assert!(stored.contains("HIT 1"), "stored output keeps the first line: {stored}");
    assert!(stored.contains("HIT 32"), "stored output keeps the last line: {stored}");
}

/// A notice is process output, never a terminal: escape sequences, a bell and a
/// tab are dropped or folded before the text leaves the tool.
#[tokio::test]
async fn a_notice_strips_terminal_control_bytes() {
    let session = "notify-sanitize";
    let mut bus = Bus::global().subscribe();

    let task_id = start_job(
        session,
        json!({
            "command": "printf 'ALERT \\033[31mred\\033[0m\\a\\tdone\\n'",
            "run_in_background": true,
            "notify": false,
            "notify_pattern": "ALERT",
        }),
    )
    .await;

    let notice = next_notice(&mut bus, session, Duration::from_secs(30))
        .await
        .expect("a live notice");
    let _ = finished_output(&task_id).await;
    assert!(
        notice.contains("ALERT red"),
        "the sequence is dropped whole: {notice:?}"
    );
    assert!(notice.contains("done"), "the rest of the line survives: {notice:?}");
    assert!(
        !notice.chars().any(char::is_control),
        "no control byte reaches the transcript: {notice:?}"
    );
}
