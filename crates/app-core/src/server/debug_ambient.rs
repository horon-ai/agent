use crate::ambient_runner::AmbientRunnerHandle;
use crate::provider::Provider;
use crate::safety::RecordDecisionOutcome;
use anyhow::Result;
use std::sync::Arc;

/// Build the operator-facing reply for an approval decision.
///
/// Only a decision this call wrote is reported as the requested action taking
/// effect. When another writer already recorded a decision, the reply states
/// what is actually on record instead of repeating the request.
fn decision_reply(request_id: &str, requested_approved: bool, outcome: &RecordDecisionOutcome) -> String {
    match outcome {
        RecordDecisionOutcome::Recorded(decision) => {
            if decision.approved {
                format!("Approved: {request_id}")
            } else {
                format!("Denied: {request_id}")
            }
        }
        RecordDecisionOutcome::AlreadyDecided { existing } => {
            let recorded = if existing.approved { "approved" } else { "denied" };
            let requested = if requested_approved { "approve" } else { "deny" };
            format!(
                "Already decided: {request_id} was already {recorded} via {} (requested {requested}; no new decision recorded)",
                existing.decided_via
            )
        }
    }
}

pub(super) async fn maybe_handle_ambient_command(
    cmd: &str,
    ambient_runner: &Option<AmbientRunnerHandle>,
    provider: &Arc<dyn Provider>,
) -> Result<Option<String>> {
    if cmd == "ambient:status" {
        let output = if let Some(runner) = ambient_runner {
            runner.status_json().await
        } else {
            serde_json::json!({
                "enabled": false,
                "status": "disabled",
                "message": "Ambient mode is not enabled in config"
            })
            .to_string()
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:queue" {
        let output = if let Some(runner) = ambient_runner {
            runner.queue_json().await
        } else {
            "[]".to_string()
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:trigger" {
        let output = if let Some(runner) = ambient_runner {
            runner.trigger().await;
            "Ambient cycle triggered".to_string()
        } else {
            return Err(anyhow::anyhow!("Ambient mode is not enabled"));
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:log" {
        let output = if let Some(runner) = ambient_runner {
            runner.log_json().await
        } else {
            "[]".to_string()
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:permissions" {
        let output = if let Some(runner) = ambient_runner {
            let _ = runner.safety().expire_dead_session_requests("debug_socket_gc");
            let pending = runner.safety().pending_requests();
            let items: Vec<serde_json::Value> = pending
                .iter()
                .map(|request| {
                    let review_summary = request
                        .context
                        .as_ref()
                        .and_then(|ctx| ctx.get("review"))
                        .and_then(|review| review.get("summary"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&request.description);
                    let review_why = request
                        .context
                        .as_ref()
                        .and_then(|ctx| ctx.get("review"))
                        .and_then(|review| review.get("why_permission_needed"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&request.rationale);
                    serde_json::json!({
                        "id": request.id,
                        "action": request.action,
                        "description": request.description,
                        "rationale": request.rationale,
                        "summary": review_summary,
                        "why_permission_needed": review_why,
                        "urgency": format!("{:?}", request.urgency),
                        "wait": request.wait,
                        "created_at": request.created_at.to_rfc3339(),
                        "context": request.context,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string())
        } else {
            "[]".to_string()
        };
        return Ok(Some(output));
    }

    if cmd.starts_with("ambient:approve:") {
        let request_id = cmd.strip_prefix("ambient:approve:").unwrap_or("").trim();
        if request_id.is_empty() {
            return Err(anyhow::anyhow!("Usage: ambient:approve:<request_id>"));
        }
        let output = if let Some(runner) = ambient_runner {
            let outcome = runner
                .safety()
                .record_decision(request_id, true, "debug_socket", None)?;
            decision_reply(request_id, true, &outcome)
        } else {
            return Err(anyhow::anyhow!("Ambient mode is not enabled"));
        };
        return Ok(Some(output));
    }

    if cmd.starts_with("ambient:deny:") {
        let rest = cmd.strip_prefix("ambient:deny:").unwrap_or("").trim();
        if rest.is_empty() {
            return Err(anyhow::anyhow!("Usage: ambient:deny:<request_id> [reason]"));
        }
        let output = if let Some(runner) = ambient_runner {
            let mut parts = rest.splitn(2, char::is_whitespace);
            let request_id = parts.next().unwrap_or("").trim();
            let message = parts.next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            let outcome = runner
                .safety()
                .record_decision(request_id, false, "debug_socket", message)?;
            decision_reply(request_id, false, &outcome)
        } else {
            return Err(anyhow::anyhow!("Ambient mode is not enabled"));
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:stop" {
        let output = if let Some(runner) = ambient_runner {
            runner.stop().await;
            "Ambient mode stopped".to_string()
        } else {
            return Err(anyhow::anyhow!("Ambient mode is not enabled"));
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:start" {
        let output = if let Some(runner) = ambient_runner {
            if runner.start(Arc::clone(provider)).await {
                "Ambient mode started".to_string()
            } else {
                "Ambient mode is already running".to_string()
            }
        } else {
            return Err(anyhow::anyhow!("Ambient mode is not enabled in config"));
        };
        return Ok(Some(output));
    }

    if cmd == "ambient:help" {
        return Ok(Some(
            r#"Ambient mode debug commands (ambient: prefix):
  ambient:status              - Ambient + schedule runner state, counts, next due items
  ambient:queue               - Scheduled queue contents with target/session metadata
  ambient:trigger             - Manually trigger an ambient cycle
  ambient:log                 - Recent transcript summaries
  ambient:permissions         - List pending permission requests
  ambient:approve:<id>        - Approve a permission request
  ambient:deny:<id> [reason]  - Deny a permission request (optional reason)
  ambient:start               - Start/restart ambient mode
  ambient:stop                - Stop ambient mode"#
                .to_string(),
        ));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, ToolDefinition};
    use crate::provider::EventStream;
    use crate::safety::{SafetySystem, record_permission_via_file};
    use async_trait::async_trait;

    struct StubProvider;

    #[async_trait]
    impl Provider for StubProvider {
        async fn complete(
            &self,
            _: &[Message],
            _: &[ToolDefinition],
            _: &str,
            _: Option<&str>,
        ) -> anyhow::Result<EventStream> {
            panic!("ambient debug commands never reach the provider")
        }

        fn name(&self) -> &str {
            "stub"
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(Self)
        }
    }

    /// Point `HORON_AGENT_HOME` at a temp dir for the life of the guard.
    struct TempHome {
        _dir: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("temp home");
            let previous = std::env::var_os("HORON_AGENT_HOME");
            crate::env::set_var("HORON_AGENT_HOME", dir.path());
            Self { _dir: dir, previous }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
                None => crate::env::remove_var("HORON_AGENT_HOME"),
            }
        }
    }

    fn runner() -> AmbientRunnerHandle {
        AmbientRunnerHandle::new(Arc::new(SafetySystem::new()))
    }

    /// Run one ambient debug command to completion.
    ///
    /// The callers hold the process-wide test-environment lock, which is a std
    /// mutex, so the command is driven from a synchronous test body instead of
    /// an async one.
    fn run_command(command: &str) -> Option<String> {
        let provider: Arc<dyn Provider> = Arc::new(StubProvider);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        runtime
            .block_on(maybe_handle_ambient_command(command, &Some(runner()), &provider))
            .expect("ambient command succeeds")
    }

    #[test]
    fn approve_reports_the_decision_it_recorded() {
        let _guard = crate::storage::lock_test_env();
        let _home = TempHome::new();

        let reply = run_command("ambient:approve:req_t14_happy");

        assert_eq!(reply.as_deref(), Some("Approved: req_t14_happy"));
    }

    #[test]
    fn approve_reports_an_existing_denial_instead_of_claiming_approval() {
        let _guard = crate::storage::lock_test_env();
        let _home = TempHome::new();
        record_permission_via_file(
            "req_t14_denied",
            false,
            "external_approver",
            Some("not this time".to_string()),
        )
        .expect("external denial is recorded");

        let reply = run_command("ambient:approve:req_t14_denied").expect("command is handled");

        assert!(!reply.contains("Approved"), "must not claim approval: {reply}");
        assert!(reply.contains("already denied"), "reports the recorded denial: {reply}");
        assert!(reply.contains("external_approver"), "names the writer: {reply}");
    }

    #[test]
    fn deny_reports_an_existing_approval_instead_of_claiming_denial() {
        let _guard = crate::storage::lock_test_env();
        let _home = TempHome::new();
        record_permission_via_file("req_t14_approved", true, "telegram_reply", None)
            .expect("external approval is recorded");

        let reply = run_command("ambient:deny:req_t14_approved").expect("command is handled");

        assert!(!reply.contains("Denied"), "must not claim denial: {reply}");
        assert!(
            reply.contains("already approved"),
            "reports the recorded approval: {reply}"
        );
        assert!(reply.contains("telegram_reply"), "names the writer: {reply}");
    }
}
