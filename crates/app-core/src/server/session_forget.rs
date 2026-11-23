//! Forgetting every per-session registry a removed server session owned.

use super::unregister_session_event_sink;
use crate::protocol::ServerEvent;
use horon_agent_irc_core::AgentId;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, Weak};
use tokio::sync::mpsc;

/// The counters one delivery layer keeps for its live relays.
type RelayMap = StdMutex<HashMap<AgentId, usize>>;

/// Sessions with a wake relay still in flight, and how many each one has.
///
/// A relay task outlives the session record it was started for: it waits for
/// the woken turn's final answer and only then drops its count. Each delivery
/// layer owns one of these maps, and [`register_pending_relays`] lets removal
/// clear the entry of a session that disappears mid-relay, so a reused session
/// id never inherits a stale "still answering" mark.
pub(crate) type PendingRelays = Arc<RelayMap>;

/// Every live delivery layer's counters, held weakly so a retired one is not
/// kept alive just to be forgotten from.
static REGISTERED_RELAYS: LazyLock<StdMutex<Vec<Weak<RelayMap>>>> = LazyLock::new(|| StdMutex::new(Vec::new()));

/// Track `relays` so session removal can clear a session's counters in it.
pub(crate) fn register_pending_relays(relays: &PendingRelays) {
    let mut registered = match REGISTERED_RELAYS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    registered.retain(|weak| weak.strong_count() > 0);
    let entry = Arc::downgrade(relays);
    if !registered.iter().any(|weak| weak.ptr_eq(&entry)) {
        registered.push(entry);
    }
}

/// Drop the relay counters a removed session still owned.
///
/// A relay that finishes afterwards finds no entry and leaves the map alone,
/// so clearing here never panics or pushes a count below zero.
fn forget_pending_relays(session_id: &str) {
    let registered = match REGISTERED_RELAYS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    for weak in registered.iter() {
        let Some(relays) = weak.upgrade() else {
            continue;
        };
        let mut relays = match relays.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        relays.remove(session_id);
    }
}

/// Forget the per-session registries a removed session owned: its event sink,
/// its metrics, its recorded reasoning effort, and its IRC bus state, including
/// the wake relays still in flight for it.
///
/// Split out from `remove_session_entry` for callers that already hold
/// the `sessions` write guard: re-entering `remove_session_entry` there would
/// take the same lock twice. `retired_sink` is the channel the event-sink
/// registry held before the removal; `None` means no sink was registered.
///
/// The presence marker is deliberately not part of this: `remove_session_entry`
/// owns it, and the two resume paths that call this directly never cleared it.
pub(crate) fn forget_removed_session(session_id: &str, retired_sink: Option<&mpsc::UnboundedSender<ServerEvent>>) {
    if let Some(retired) = retired_sink {
        unregister_session_event_sink(session_id, retired);
    }
    crate::session_metrics::forget(session_id);
    crate::session_effort::forget_session_effort(session_id);
    // Per-session caches that would otherwise outlive the session: the monitor
    // runtime (and its sampler thread), the observation record, and the durable
    // input store. Without this a daemon that opens and closes sessions forever
    // keeps every one of them, and their threads, alive.
    crate::tool::forget_session_caches(session_id);
    crate::agent::tool_result_trim::clear_policy(session_id);
    crate::plan_mode::clear_state(session_id);
    crate::tool::run_approval::clear_floor(session_id);
    crate::agent::Agent::clear_appended_system_text(session_id);
    crate::extensions::observer::forget_session(session_id);
    horon_agent_provider_core::degradation::forget_session(session_id);
    crate::server::session_observation::forget_session(session_id);
    crate::server::session_notes::forget_session(session_id);
    crate::durable_input_runtime::forget_session(session_id);
    crate::server::control_receipts::forget_session(session_id);
    crate::agent::hosted_notice::clear_session(session_id);
    if let Some(bus) = horon_agent_irc_core::global::get() {
        bus.forget(session_id);
    }
    forget_pending_relays(session_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::agent::tool_result_trim::{self, TrimPolicy};
    use crate::config::ApprovalMode;
    use crate::message::{Message, ToolDefinition};
    use crate::plan_mode::{self, PlanState, SubmittedPlan};
    use crate::provider::{EventStream, Provider};
    use crate::session::Session;
    use crate::tool::{Registry, run_approval};
    use async_trait::async_trait;
    use std::ffi::OsString;
    use std::sync::Arc;

    struct StubProvider;

    #[async_trait]
    impl Provider for StubProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> anyhow::Result<EventStream> {
            anyhow::bail!("stub provider must never be called")
        }

        fn name(&self) -> &str {
            "stub"
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(StubProvider)
        }
    }

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<OsString>,
        _dir: tempfile::TempDir,
    }

    impl EnvGuard {
        fn new() -> Self {
            let lock = crate::storage::lock_test_env();
            let dir = tempfile::tempdir().expect("isolated HORON_HOME");
            let prev = std::env::var_os("HORON_HOME");
            crate::env::set_var("HORON_HOME", dir.path());
            Self {
                _lock: lock,
                prev,
                _dir: dir,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(prev) => crate::env::set_var("HORON_HOME", prev),
                None => crate::env::remove_var("HORON_HOME"),
            }
        }
    }

    #[test]
    fn forget_removed_session_clears_policy_plan_floor_and_appended_prompt() {
        let _env = EnvGuard::new();
        let session = Session::create(None, None);
        let target_id = session.id.clone();
        let other_id = "session-forget-retained-peer";

        let mut agent = Agent::new_with_session(Arc::new(StubProvider), Registry::empty(), session, None);
        agent
            .set_appended_system_text("custom-forget-prompt-marker")
            .expect("append prompt text");
        assert!(agent.system_prompt_for_debug().contains("custom-forget-prompt-marker"));

        let policy = TrimPolicy::new(1_000).expect("valid trim policy");
        tool_result_trim::set_policy(&target_id, policy);
        tool_result_trim::set_policy(other_id, policy);

        plan_mode::set_state(&target_id, PlanState::Planning);
        plan_mode::record_submission(
            &target_id,
            SubmittedPlan {
                request_id: "req-1".to_string(),
                text: "plan body".to_string(),
                files: vec!["a.rs".to_string()],
            },
        );
        plan_mode::approve_pending_plan(&target_id);
        plan_mode::record_submission(
            &target_id,
            SubmittedPlan {
                request_id: "req-2".to_string(),
                text: "pending body".to_string(),
                files: vec!["b.rs".to_string()],
            },
        );
        plan_mode::allow_unattended_approval(&target_id);

        plan_mode::set_state(other_id, PlanState::Planning);

        run_approval::set_floor(&target_id, ApprovalMode::All);
        run_approval::set_floor(other_id, ApprovalMode::Write);

        forget_removed_session(&target_id, None);

        assert_eq!(tool_result_trim::policy_for(&target_id), None);
        assert_eq!(plan_mode::state_for(&target_id), PlanState::Off);
        assert_eq!(plan_mode::pending_plan(&target_id), None);
        assert_eq!(plan_mode::approved_plan(&target_id), None);
        assert_eq!(plan_mode::approve_if_opted_in(&target_id).expect("opt-in check"), None);
        assert_eq!(
            run_approval::stricter(&target_id, ApprovalMode::Yolo),
            ApprovalMode::Yolo
        );
        assert!(
            !agent.system_prompt_for_debug().contains("custom-forget-prompt-marker"),
            "appended system text must be cleared when session is forgotten"
        );

        // Unrelated peer session remains intact.
        assert_eq!(tool_result_trim::policy_for(other_id), Some(policy));
        assert_eq!(plan_mode::state_for(other_id), PlanState::Planning);
        assert_eq!(
            run_approval::stricter(other_id, ApprovalMode::Yolo),
            ApprovalMode::Write
        );

        forget_removed_session(other_id, None);
    }

    #[test]
    fn forget_removed_session_clears_queued_hosted_notices_without_touching_peer() {
        let target_id = "session-forget-hosted-notice-target";
        let peer_id = "session-forget-hosted-notice-peer";

        crate::agent::hosted_notice::push(target_id, "target mcp refusal".to_string());
        crate::agent::hosted_notice::push(peer_id, "peer mcp refusal".to_string());

        forget_removed_session(target_id, None);

        assert_eq!(crate::agent::hosted_notice::take(target_id), None);
        let peer_notice = crate::agent::hosted_notice::take(peer_id).expect("peer notice remains queued");
        assert!(peer_notice.contains("- peer mcp refusal"), "{peer_notice}");
    }
}
