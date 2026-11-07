//! Notification dispatcher for ambient mode.
//!
//! Sends notifications via:
//! - ntfy.sh (push notifications to phone)
//! - Desktop notifications (notify-send)
//!
//! All sends are fire-and-forget: errors are logged, never block.

use crate::config::{SafetyConfig, config};
use crate::logging;
use crate::safety::AmbientTranscript;

use std::sync::{Arc, OnceLock};

/// Notification priority levels (maps to ntfy priority header).
#[derive(Debug, Clone, Copy)]
pub enum Priority {
    /// Routine cycle summaries
    Default,
    /// Permission requests, errors
    High,
    /// Critical safety issues
    Urgent,
}

impl Priority {
    fn ntfy_value(self) -> &'static str {
        match self {
            Priority::Default => "3",
            Priority::High => "4",
            Priority::Urgent => "5",
        }
    }

    fn ntfy_tags(self) -> &'static str {
        match self {
            Priority::Default => "robot",
            Priority::High => "warning",
            Priority::Urgent => "rotating_light",
        }
    }
}

/// Dispatcher that sends notifications through the configured local channels.
///
/// No HTTP client is held: building one parses the whole TLS trust store, and
/// `src/cli/startup.rs` constructs this dispatcher on every start to register
/// the permission notifier. Only the ntfy path needs a client, so it asks for the
/// process-wide one when it actually sends.
#[derive(Clone)]
pub struct NotificationDispatcher {
    config: SafetyConfig,
}

impl Default for NotificationDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationDispatcher {
    pub fn new() -> Self {
        Self {
            config: config().safety.clone(),
        }
    }

    #[cfg(test)]
    pub fn from_config(config: SafetyConfig) -> Self {
        Self { config }
    }

    /// Send a cycle summary notification (after ambient cycle completes).
    pub fn dispatch_cycle_summary(&self, transcript: &AmbientTranscript) {
        let title = format!(
            "Ambient cycle: {} memories, {} compactions",
            transcript.memories_modified, transcript.compactions
        );
        let safe_body = format_cycle_body_safe(transcript);
        let detailed_body = format_cycle_body_detailed(transcript);

        let priority = if transcript.pending_permissions > 0 {
            Priority::High
        } else {
            Priority::Default
        };

        self.send_all(&title, &safe_body, &detailed_body, priority);
    }

    /// Send a permission request notification (high priority).
    pub fn dispatch_permission_request(&self, action: &str, description: &str, request_id: &str) {
        let title = format!("horon-agent: permission needed ({})", action);
        let safe_body = "An ambient action needs your approval. Open horon-agent to review.".to_string();
        let detailed_body = format!(
            "Action: {}\n{}\n\nRequest ID: {}\nReview in horon-agent to approve or deny.",
            action, description, request_id
        );

        self.send_all(&title, &safe_body, &detailed_body, Priority::High);
    }

    /// Send the outcome of a permission request through the configured channels.
    ///
    /// The three outcomes a person must be able to tell apart are approved,
    /// denied, and "the wait elapsed, so the request default applied" — the last
    /// one is phrased so it never reads like a person said no.
    pub fn dispatch_permission_decision(&self, request_id: &str, approved: bool, via: &str, message: Option<&str>) {
        let (title, safe_body, detailed_body) = format_permission_decision(request_id, approved, via, message);
        self.send_all(&title, &safe_body, &detailed_body, Priority::High);
    }

    /// Tell the person that an unattended resume gave up and needs them.
    ///
    /// The body names the request id and the reason, so acting on it does not
    /// require opening a log file first.
    pub fn dispatch_needs_human_resume(&self, request_id: &str, session_id: &str, reason: &str, attempts: u32) {
        let title = format!("horon-agent: run needs a human (request {})", request_id);
        let safe_body = "An unattended run stopped and needs a person to decide. Open horon-agent.".to_string();
        let detailed_body = format!(
            "Request: {}\nSession: {}\nAttempts: {}\nReason: {}\n\nResolve it with:\n  horon-agent approvals resolve --request-id {} --tool-call-id <id> --verdict retry|skip --note <text>",
            request_id, session_id, attempts, reason, request_id
        );
        self.send_all(&title, &safe_body, &detailed_body, Priority::Urgent);
    }

    /// Send through the configured local notification paths (fire-and-forget).
    ///
    /// `safe_body` is sanitized (no secrets) — used for ntfy (potentially public).
    /// `detailed_body` includes full info — used for desktop (local machine).
    fn send_all(&self, title: &str, safe_body: &str, detailed_body: &str, priority: Priority) {
        // Guard: only dispatch if inside a tokio runtime
        if tokio::runtime::Handle::try_current().is_err() {
            logging::info("Notification skipped: no tokio runtime");
            return;
        }

        // ntfy.sh — uses SAFE body (may be publicly readable)
        if let Some(ref topic) = self.config.ntfy_topic {
            let client = crate::provider::shared_http_client();
            let url = format!("{}/{}", self.config.ntfy_server, topic);
            let title = title.to_string();
            let body = safe_body.to_string();
            tokio::spawn(async move {
                if let Err(e) = send_ntfy(&client, &url, &title, &body, priority).await {
                    logging::error(&format!("ntfy notification failed: {}", e));
                }
            });
        }

        // Desktop notification — uses DETAILED body (local machine, private)
        if self.config.desktop_notifications {
            let title = title.to_string();
            let body = detailed_body.to_string();
            let urgency = match priority {
                Priority::Default => "normal",
                Priority::High | Priority::Urgent => "critical",
            };
            tokio::spawn(async move {
                send_desktop(&title, &body, urgency);
            });
        }
    }
}

/// The `decided_via` value base safety records when a request's wait elapsed
/// and its default was applied (the private `TIMEOUT_DEFAULT_VIA` in
/// `crates/base/src/safety.rs`).
const TIMEOUT_DEFAULT_VIA: &str = "timeout_default";

/// Build the (title, safe body, detailed body) of a decision notification.
///
/// `safe_body` is what can reach a public channel (ntfy), so it names no
/// message text; `detailed_body` is for private channels and carries the
/// recorded message verbatim.
fn format_permission_decision(
    request_id: &str,
    approved: bool,
    via: &str,
    message: Option<&str>,
) -> (String, String, String) {
    let timed_out = via == TIMEOUT_DEFAULT_VIA;
    let default = if approved { "approve" } else { "deny" };

    let (title, safe_body) = if timed_out {
        (
            format!("horon-agent: approval timed out (default {})", default),
            format!(
                "Permission request {} timed out. No person answered; the request default ({}) was applied.",
                request_id, default
            ),
        )
    } else if approved {
        (
            "horon-agent: approval granted".to_string(),
            format!("Permission request {} was approved.", request_id),
        )
    } else {
        (
            "horon-agent: approval denied".to_string(),
            format!("Permission request {} was denied.", request_id),
        )
    };

    let outcome = if timed_out {
        format!("timed out; the request default ({}) applied", default)
    } else if approved {
        "approved".to_string()
    } else {
        "denied".to_string()
    };
    let mut detailed_body = format!("Request: {}\nOutcome: {}\nDecided via: {}", request_id, outcome, via);
    if let Some(message) = message.filter(|message| !message.trim().is_empty()) {
        detailed_body.push_str(&format!("\nMessage: {}", message));
    }

    (title, safe_body, detailed_body)
}

/// A channel sink for permission decisions and resume escalations.
///
/// Safety records decisions without depending on this crate, so the
/// notification layer registers an adapter on
/// [`crate::safety::register_decision_notifier`]. This trait is the seam that
/// lets tests inject a recording or failing sink without touching safety.
///
/// An error means the sink could not deliver; callers log it and continue, so a
/// broken notification channel never blocks the decision itself.
pub trait DecisionNotifierSink: Send + Sync {
    fn permission_decision(
        &self,
        request_id: &str,
        approved: bool,
        via: &str,
        message: Option<&str>,
    ) -> anyhow::Result<()>;

    fn needs_human_resume(&self, request_id: &str, session_id: &str, reason: &str, attempts: u32)
    -> anyhow::Result<()>;
}

impl DecisionNotifierSink for NotificationDispatcher {
    fn permission_decision(
        &self,
        request_id: &str,
        approved: bool,
        via: &str,
        message: Option<&str>,
    ) -> anyhow::Result<()> {
        self.dispatch_permission_decision(request_id, approved, via, message);
        Ok(())
    }

    fn needs_human_resume(
        &self,
        request_id: &str,
        session_id: &str,
        reason: &str,
        attempts: u32,
    ) -> anyhow::Result<()> {
        self.dispatch_needs_human_resume(request_id, session_id, reason, attempts);
        Ok(())
    }
}

/// Register `sink` as one of the safety decision callbacks.
///
/// Safety keeps every registered callback, so this never displaces another
/// subscriber — the resume runner registers its own callback on the same hook.
/// A sink error is logged and never reaches `record_decision`.
pub fn register_decision_notifier_sink(sink: Arc<dyn DecisionNotifierSink>) {
    crate::safety::register_decision_notifier(Arc::new(move |decision: &crate::safety::Decision| {
        if let Err(error) = sink.permission_decision(
            &decision.request_id,
            decision.approved,
            &decision.decided_via,
            decision.message.as_deref(),
        ) {
            logging::error(&format!(
                "permission decision notification failed for {}: {error:#}",
                decision.request_id
            ));
        }
    }));
}

/// Sink that resolves the configured dispatcher per notification, so a config
/// reload takes effect without restarting the process.
struct ConfiguredNotifications;

impl DecisionNotifierSink for ConfiguredNotifications {
    fn permission_decision(
        &self,
        request_id: &str,
        approved: bool,
        via: &str,
        message: Option<&str>,
    ) -> anyhow::Result<()> {
        NotificationDispatcher::new().dispatch_permission_decision(request_id, approved, via, message);
        Ok(())
    }

    fn needs_human_resume(
        &self,
        request_id: &str,
        session_id: &str,
        reason: &str,
        attempts: u32,
    ) -> anyhow::Result<()> {
        NotificationDispatcher::new().dispatch_needs_human_resume(request_id, session_id, reason, attempts);
        Ok(())
    }
}

static DEFAULT_DECISION_NOTIFIER: OnceLock<()> = OnceLock::new();

/// Install the process-wide decision notifier exactly once.
///
/// Startup can run more than once in a process (embedded launches, tests). The
/// `OnceLock` keeps the second call from registering a duplicate callback, so a
/// single decision never produces two notifications.
pub fn install_default_decision_notifier() {
    if DEFAULT_DECISION_NOTIFIER.set(()).is_ok() {
        register_decision_notifier_sink(Arc::new(ConfiguredNotifications));
        crate::safety::register_decision_notifier(Arc::new(|decision: &crate::safety::Decision| {
            crate::plan_mode::observe_decision(&decision.request_id, decision.approved);
        }));
    }
}

/// Report that an unattended resume exhausted its attempts and a person must
/// step in. The resume runner calls this once per request when it gives up.
pub fn report_needs_human_resume(
    sink: &dyn DecisionNotifierSink,
    request_id: &str,
    session_id: &str,
    reason: &str,
    attempts: u32,
) -> anyhow::Result<()> {
    logging::error(&format!(
        "run needs a human resume: request={request_id} session={session_id} attempts={attempts} reason={reason}"
    ));
    sink.needs_human_resume(request_id, session_id, reason, attempts)
}

/// Fire-and-forget escalation through the configured channels.
pub fn notify_needs_human_resume(request_id: &str, session_id: &str, reason: &str, attempts: u32) {
    let dispatcher = NotificationDispatcher::new();
    if let Err(error) = report_needs_human_resume(&dispatcher, request_id, session_id, reason, attempts) {
        logging::error(&format!("needs-human notification failed for {request_id}: {error:#}"));
    }
}

// ---------------------------------------------------------------------------
// ntfy.sh
// ---------------------------------------------------------------------------

async fn send_ntfy(
    client: &reqwest::Client,
    url: &str,
    title: &str,
    body: &str,
    priority: Priority,
) -> anyhow::Result<()> {
    let resp = client
        .post(url)
        .header("Title", title)
        .header("Priority", priority.ntfy_value())
        .header("Tags", priority.ntfy_tags())
        .body(body.to_string())
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("ntfy returned {}: {}", status, text);
    }

    logging::info(&format!("ntfy notification sent: {}", title));
    Ok(())
}

// ---------------------------------------------------------------------------
// Desktop (cross-platform, fire-and-forget)
// ---------------------------------------------------------------------------

fn reap_notification_child(mut child: std::process::Child) {
    let _ = std::thread::Builder::new()
        .name("horon-agent-notification-child".to_string())
        .spawn(move || {
            let _ = child.wait();
        });
}

/// Send a local desktop notification without blocking.
///
/// Uses Notification Center via `osascript` on macOS and `notify-send` on
/// Linux. The child process is reaped on a background thread; failures are
/// ignored (a missing notifier is not an error).
pub fn send_desktop_notification(title: &str, body: &str) {
    send_desktop_notification_rich(title, None, body, None);
}

/// Send a local desktop notification with optional macOS subtitle and sound.
///
/// `subtitle` renders as a second bold line on macOS (ignored elsewhere).
/// `sound` is a Notification Center sound name such as "Glass" or "Ping"
/// (macOS only). Both are best-effort; a missing notifier is not an error.
pub fn send_desktop_notification_rich(title: &str, subtitle: Option<&str>, body: &str, sound: Option<&str>) {
    #[cfg(target_os = "macos")]
    {
        fn applescript_escape(s: &str) -> String {
            let mut out = String::with_capacity(s.len());
            for ch in s.chars() {
                match ch {
                    '\\' => out.push_str("\\\\"),
                    '"' => out.push_str("\\\""),
                    '\n' => out.push_str("\\n"),
                    '\r' => {}
                    _ => out.push(ch),
                }
            }
            out
        }
        let mut script = format!(
            "display notification \"{}\" with title \"{}\"",
            applescript_escape(body),
            applescript_escape(title)
        );
        if let Some(subtitle) = subtitle.filter(|s| !s.trim().is_empty()) {
            script.push_str(&format!(" subtitle \"{}\"", applescript_escape(subtitle)));
        }
        if let Some(sound) = sound.filter(|s| !s.trim().is_empty()) {
            script.push_str(&format!(" sound name \"{}\"", applescript_escape(sound)));
        }
        if let Ok(child) = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            reap_notification_child(child);
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = (subtitle, sound);
        if let Ok(child) = std::process::Command::new("notify-send")
            .arg("--app-name=horon-agent")
            .arg(title)
            .arg(body)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            reap_notification_child(child);
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (title, subtitle, body, sound);
    }
}

// ---------------------------------------------------------------------------
// Desktop (notify-send)
// ---------------------------------------------------------------------------

fn send_desktop(title: &str, body: &str, urgency: &str) {
    // On macOS notify-send does not exist; route through Notification Center.
    #[cfg(target_os = "macos")]
    {
        let _ = urgency;
        send_desktop_notification(title, body);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let result = std::process::Command::new("notify-send")
            .arg("--app-name=horon-agent")
            .arg(format!("--urgency={}", urgency))
            .arg("--icon=dialog-information")
            .arg(title)
            .arg(body)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match result {
            Ok(status) if status.success() => {
                logging::info(&format!("Desktop notification sent: {}", title));
            }
            Ok(status) => {
                logging::warn(&format!("notify-send exited with {}", status));
            }
            Err(e) => {
                // notify-send not available - not an error, just skip
                logging::info(&format!("notify-send unavailable: {}", e));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Sanitized body for potentially public channels (ntfy.sh).
/// Only includes counts and status — no model-generated text.
fn format_cycle_body_safe(transcript: &AmbientTranscript) -> String {
    let mut lines = Vec::new();

    lines.push(format!("Status: {:?}", transcript.status));
    lines.push(format!("Memories modified: {}", transcript.memories_modified));
    lines.push(format!("Compactions: {}", transcript.compactions));

    if transcript.pending_permissions > 0 {
        lines.push(format!(
            "{} permission request(s) pending",
            transcript.pending_permissions
        ));
    }

    lines.push("Check horon-agent for full details.".to_string());
    lines.join("\n")
}

/// Full detailed body for the desktop notification path.
/// Includes the model-generated summary and provider info.
fn format_cycle_body_detailed(transcript: &AmbientTranscript) -> String {
    let mut lines = Vec::new();

    if let Some(ref summary) = transcript.summary {
        lines.push("# Summary".to_string());
        lines.push(String::new());
        lines.push(summary.clone());
        lines.push(String::new());
    }

    lines.push("---".to_string());
    lines.push(String::new());
    lines.push(format!(
        "**Status:** {:?} · **Provider:** {} ({}) · **Memories:** {} · **Compactions:** {}",
        transcript.status, transcript.provider, transcript.model, transcript.memories_modified, transcript.compactions,
    ));

    if transcript.pending_permissions > 0 {
        lines.push(String::new());
        lines.push(format!(
            "**⚠ {} permission request(s) pending** — review in horon-agent",
            transcript.pending_permissions
        ));
    }

    // Include full conversation transcript if available
    if let Some(ref conversation) = transcript.conversation {
        lines.push(String::new());
        lines.push("---".to_string());
        lines.push(String::new());
        lines.push("# Full Transcript".to_string());
        lines.push(String::new());
        lines.push(conversation.clone());
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_cycle_body_safe() {
        let transcript = AmbientTranscript {
            session_id: "test_001".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: crate::safety::TranscriptStatus::Complete,
            provider: "claude".to_string(),
            model: "claude-sonnet-4".to_string(),
            actions: Vec::new(),
            pending_permissions: 0,
            summary: Some("Cleaned up 3 stale memories.".to_string()),
            compactions: 1,
            memories_modified: 3,
            conversation: None,
        };

        let body = format_cycle_body_safe(&transcript);
        assert!(body.contains("Memories modified: 3"));
        assert!(body.contains("Compactions: 1"));
        assert!(body.contains("Check horon-agent for full details"));
        // Safe body must NOT include model-generated summary
        assert!(!body.contains("Cleaned up"));
        assert!(!body.contains("permission"));
    }

    #[test]
    fn test_format_cycle_body_detailed() {
        let transcript = AmbientTranscript {
            session_id: "test_001".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: crate::safety::TranscriptStatus::Complete,
            provider: "claude".to_string(),
            model: "claude-sonnet-4".to_string(),
            actions: Vec::new(),
            pending_permissions: 0,
            summary: Some("Cleaned up 3 stale memories.".to_string()),
            compactions: 1,
            memories_modified: 3,
            conversation: Some("### User\n\nBegin cycle.\n\n### Assistant\n\nDone.\n".to_string()),
        };

        let body = format_cycle_body_detailed(&transcript);
        // Detailed body SHOULD include the summary
        assert!(body.contains("Cleaned up 3 stale memories."));
        assert!(body.contains("**Memories:** 3"));
        assert!(body.contains("claude"));
        // Should include conversation transcript
        assert!(body.contains("# Full Transcript"));
        assert!(body.contains("### User"));
        assert!(body.contains("Begin cycle."));
    }

    #[test]
    fn test_format_cycle_body_with_pending_permissions() {
        let transcript = AmbientTranscript {
            session_id: "test_002".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: crate::safety::TranscriptStatus::Complete,
            provider: "claude".to_string(),
            model: "claude-sonnet-4".to_string(),
            actions: Vec::new(),
            pending_permissions: 2,
            summary: None,
            compactions: 0,
            memories_modified: 0,
            conversation: None,
        };

        let safe = format_cycle_body_safe(&transcript);
        assert!(safe.contains("2 permission request(s) pending"));
        assert!(safe.contains("Check horon-agent for full details"));

        let detailed = format_cycle_body_detailed(&transcript);
        assert!(detailed.contains("2 permission request(s) pending"));
    }

    #[test]
    fn test_priority_values() {
        assert_eq!(Priority::Default.ntfy_value(), "3");
        assert_eq!(Priority::High.ntfy_value(), "4");
        assert_eq!(Priority::Urgent.ntfy_value(), "5");
    }

    #[test]
    fn test_dispatcher_creation() {
        // Just verify it doesn't panic
        let cfg = SafetyConfig::default();
        let _dispatcher = NotificationDispatcher::from_config(cfg);
    }

    /// One recorded decision: `(request_id, approved, via, message)`.
    type RecordedDecision = (String, bool, String, Option<String>);
    /// One recorded escalation: `(request_id, session_id, reason, attempts)`.
    type RecordedEscalation = (String, String, String, u32);

    /// Records what it is asked to deliver and, when `fail` is set, returns an
    /// error the way an unreachable channel would.
    #[derive(Default)]
    struct RecordingSink {
        decisions: std::sync::Mutex<Vec<RecordedDecision>>,
        needs_human: std::sync::Mutex<Vec<RecordedEscalation>>,
        fail: bool,
    }

    impl RecordingSink {
        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::default()
            }
        }

        fn decision_count(&self, request_id: &str) -> usize {
            self.decisions
                .lock()
                .expect("decisions lock")
                .iter()
                .filter(|(id, ..)| id == request_id)
                .count()
        }
    }

    impl DecisionNotifierSink for RecordingSink {
        fn permission_decision(
            &self,
            request_id: &str,
            approved: bool,
            via: &str,
            message: Option<&str>,
        ) -> anyhow::Result<()> {
            self.decisions.lock().expect("decisions lock").push((
                request_id.to_string(),
                approved,
                via.to_string(),
                message.map(str::to_string),
            ));
            if self.fail {
                anyhow::bail!("notification channel unavailable");
            }
            Ok(())
        }

        fn needs_human_resume(
            &self,
            request_id: &str,
            session_id: &str,
            reason: &str,
            attempts: u32,
        ) -> anyhow::Result<()> {
            self.needs_human.lock().expect("needs-human lock").push((
                request_id.to_string(),
                session_id.to_string(),
                reason.to_string(),
                attempts,
            ));
            if self.fail {
                anyhow::bail!("notification channel unavailable");
            }
            Ok(())
        }
    }

    /// Isolates `~/.horon-agent` for one test and serializes env mutation.
    struct TestHome {
        previous: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl TestHome {
        fn new() -> Self {
            let lock = crate::storage::lock_test_env();
            let dir = tempfile::TempDir::new().expect("temp home");
            let previous = std::env::var_os("HORON_AGENT_HOME");
            crate::env::set_var("HORON_AGENT_HOME", dir.path());
            Self {
                previous,
                _dir: dir,
                _lock: lock,
            }
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
                None => crate::env::remove_var("HORON_AGENT_HOME"),
            }
        }
    }

    #[test]
    fn permission_decisions_reach_the_sink_exactly_once_per_kind() {
        let _home = TestHome::new();
        let sink = Arc::new(RecordingSink::default());
        register_decision_notifier_sink(sink.clone());
        let safety = crate::safety::SafetySystem::new();

        safety
            .record_decision("req_task10_approved", true, "cli", Some("looks right".to_string()))
            .expect("approved decision records");
        safety
            .record_decision("req_task10_denied", false, "cli", Some("not now".to_string()))
            .expect("denied decision records");
        safety
            .record_decision(
                "req_task10_timeout",
                false,
                TIMEOUT_DEFAULT_VIA,
                Some("No decision arrived before the wait elapsed".to_string()),
            )
            .expect("timeout decision records");

        let decisions = sink.decisions.lock().expect("decisions lock");
        for request_id in ["req_task10_approved", "req_task10_denied", "req_task10_timeout"] {
            let matching: Vec<_> = decisions.iter().filter(|(id, ..)| id == request_id).collect();
            assert_eq!(matching.len(), 1, "{request_id} must be delivered exactly once");
        }
        let approved = decisions
            .iter()
            .find(|(id, ..)| id == "req_task10_approved")
            .expect("approved decision");
        assert!(approved.1);
        assert_eq!(approved.2, "cli");
        assert_eq!(approved.3.as_deref(), Some("looks right"));
        let denied = decisions
            .iter()
            .find(|(id, ..)| id == "req_task10_denied")
            .expect("denied decision");
        assert!(!denied.1);
        let timed_out = decisions
            .iter()
            .find(|(id, ..)| id == "req_task10_timeout")
            .expect("timeout decision");
        assert_eq!(timed_out.2, TIMEOUT_DEFAULT_VIA);
    }

    #[test]
    fn a_failing_sink_does_not_stop_record_decision() {
        let _home = TestHome::new();
        let sink = Arc::new(RecordingSink::failing());
        register_decision_notifier_sink(sink.clone());
        let safety = crate::safety::SafetySystem::new();

        let outcome = safety
            .record_decision("req_task10_failing_sink", true, "cli", None)
            .expect("a failing notification sink must not fail the decision");

        assert!(outcome.decision().approved);
        assert_eq!(sink.decision_count("req_task10_failing_sink"), 1);
        assert!(
            safety
                .decision_for("req_task10_failing_sink")
                .expect("the decision is persisted despite the sink error")
                .approved
        );
    }

    #[test]
    fn a_second_registered_sink_also_receives_the_decision() {
        let _home = TestHome::new();
        let first = Arc::new(RecordingSink::default());
        let second = Arc::new(RecordingSink::default());
        register_decision_notifier_sink(first.clone());
        register_decision_notifier_sink(second.clone());
        let safety = crate::safety::SafetySystem::new();

        safety
            .record_decision("req_task10_two_sinks", true, "desktop", None)
            .expect("decision records");

        assert_eq!(first.decision_count("req_task10_two_sinks"), 1);
        assert_eq!(second.decision_count("req_task10_two_sinks"), 1);
    }

    #[test]
    fn needs_human_resume_is_reported_exactly_once() {
        let sink = RecordingSink::default();

        report_needs_human_resume(
            &sink,
            "req_task10_needs_human",
            "sess_task10",
            "resume attempts exhausted (2/2)",
            2,
        )
        .expect("needs-human report");

        let reports = sink.needs_human.lock().expect("needs-human lock");
        assert_eq!(reports.len(), 1, "the needs-human path must report exactly once");
        let (request_id, session_id, reason, attempts) = &reports[0];
        assert_eq!(request_id, "req_task10_needs_human");
        assert_eq!(session_id, "sess_task10");
        assert!(reason.contains("attempts exhausted"));
        assert_eq!(*attempts, 2);
    }

    #[test]
    fn a_timeout_decision_does_not_read_like_a_human_denial() {
        let (title, safe_body, detailed_body) =
            format_permission_decision("req_timeout_text", false, TIMEOUT_DEFAULT_VIA, Some("wait elapsed"));

        assert!(title.contains("timed out"), "{title}");
        assert!(safe_body.contains("timed out"), "{safe_body}");
        assert!(safe_body.contains("default (deny)"), "{safe_body}");
        assert!(!safe_body.contains("was denied"), "{safe_body}");
        assert!(detailed_body.contains("timed out; the request default (deny) applied"));

        let (denied_title, denied_body, _) = format_permission_decision("req_denied_text", false, "cli", None);
        assert!(denied_title.contains("denied"), "{denied_title}");
        assert!(denied_body.contains("was denied"), "{denied_body}");

        let (approved_title, approved_body, _) = format_permission_decision("req_approved_text", true, "cli", None);
        assert!(approved_title.contains("granted"), "{approved_title}");
        assert!(approved_body.contains("was approved"), "{approved_body}");
    }
}

#[cfg(all(test, target_os = "linux"))]
mod notification_process_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/support/notification_reaping.rs"
    ));
}
