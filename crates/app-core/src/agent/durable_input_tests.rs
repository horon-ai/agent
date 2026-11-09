#![cfg_attr(test, allow(clippy::await_holding_lock))]

use std::sync::Arc;

use horon_agent_agent_runtime::{SoftInterruptMessage, SoftInterruptSource};
use horon_agent_base::durable_input::{DurableInputStore, ResolveAction};
use horon_agent_base::durable_runtime::InputPhase;

use super::Agent;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;

struct NoopProvider;

#[async_trait::async_trait]
impl Provider for NoopProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        Err(anyhow::anyhow!("the durable input tests never reach the provider"))
    }

    fn name(&self) -> &str {
        "durable-input-test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(NoopProvider)
    }
}

struct Sandbox {
    _dir: tempfile::TempDir,
    home: std::path::PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let home = dir.path().to_path_buf();
        crate::env::set_var("HORON_HOME", &home);
        Self { _dir: dir, home }
    }

    fn journal_root(&self) -> std::path::PathBuf {
        self.home.join(crate::durable_runtime::JOURNAL_DIR_NAME)
    }

    fn store(&self, session_id: &str) -> DurableInputStore {
        DurableInputStore::open_in(&self.journal_root(), session_id).expect("open the input store")
    }

    #[cfg(unix)]
    fn mirror_dir(&self) -> std::path::PathBuf {
        self.home.join("pending-soft-interrupts")
    }
}

fn message(content: &str) -> SoftInterruptMessage {
    SoftInterruptMessage {
        content: content.to_string(),
        images: Vec::new(),
        urgent: false,
        source: SoftInterruptSource::User,
        client_id: None,
        input_id: None,
    }
}

async fn test_agent() -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(NoopProvider);
    let registry = Registry::new(provider.clone()).await;
    Agent::new(provider, registry)
}

#[tokio::test]
async fn durable_input_accept_then_restart_restores_pending_exactly_once() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    let _session = "ses_app_durable_accept";
    let mut agent = test_agent().await;
    agent.session.save_prepared().expect("save the session");
    let session_id = agent.session_id().to_string();

    let accepted = crate::durable_input_runtime::accept(&session_id, message("restore me once"))
        .await
        .expect("accept the input");
    assert!(!accepted.already_live);

    let provider: Arc<dyn Provider> = Arc::new(NoopProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut restored = Agent::new(provider, registry);
    restored.restore_session(&session_id).expect("restore the session");
    assert_eq!(
        restored.soft_interrupt_count(),
        1,
        "the accepted input restores as pending"
    );

    let mut second = Agent::new(Arc::new(NoopProvider), Registry::new(Arc::new(NoopProvider)).await);
    second.restore_session(&session_id).expect("restore again");
    assert_eq!(
        second.soft_interrupt_count(),
        1,
        "restoring twice yields one queue entry, not a duplicate"
    );

    let mut store = sandbox.store(&session_id);
    assert_eq!(store.live_messages().expect("live").len(), 1);
}

#[tokio::test]
async fn durable_input_started_without_terminal_is_in_doubt_and_not_reinjected() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    let _session = "ses_app_durable_in_doubt";
    let mut agent = test_agent().await;
    agent.session.save_prepared().expect("save the session");
    let session_id = agent.session_id().to_string();

    let accepted = crate::durable_input_runtime::accept(&session_id, message("maybe it ran"))
        .await
        .expect("accept");
    let started = crate::durable_input_runtime::start(&session_id, vec![accepted.input_id.clone()], None).await;
    assert_eq!(started, vec![accepted.input_id.clone()], "started is synced");

    let provider: Arc<dyn Provider> = Arc::new(NoopProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut restored = Agent::new(provider, registry);
    restored.restore_session(&session_id).expect("restore the session");

    assert_eq!(
        restored.soft_interrupt_count(),
        0,
        "an input with no terminal row is never auto-reinjected"
    );
    let mut store = sandbox.store(&session_id);
    assert_eq!(
        store.phase_of(&accepted.input_id).expect("phase"),
        InputPhase::InDoubt,
        "recovery seals it in doubt instead of guessing"
    );
    let report = store.recover(&[]).expect("recover again");
    assert!(report.pending.is_empty());
    assert_eq!(report.already_in_doubt, vec![accepted.input_id.clone()]);
}

#[tokio::test]
async fn durable_input_explicit_resolve_retry_and_cancel() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    let _session = "ses_app_durable_resolve";
    let mut agent = test_agent().await;
    agent.session.save_prepared().expect("save the session");
    let session_id = agent.session_id().to_string();

    let retry_me = crate::durable_input_runtime::accept(&session_id, message("retry me"))
        .await
        .expect("accept retry");
    let cancel_me = crate::durable_input_runtime::accept(&session_id, message("cancel me"))
        .await
        .expect("accept cancel");
    crate::durable_input_runtime::start(&session_id, vec![retry_me.input_id.clone()], None).await;
    crate::durable_input_runtime::start(&session_id, vec![cancel_me.input_id.clone()], None).await;

    let provider: Arc<dyn Provider> = Arc::new(NoopProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut restored = Agent::new(provider, registry);
    restored.restore_session(&session_id).expect("restore");
    assert_eq!(restored.soft_interrupt_count(), 0, "nothing is auto-reinjected");

    let retried =
        crate::durable_input_runtime::resolve(&session_id, &retry_me.input_id, ResolveAction::Retry, |_| true)
            .await
            .expect("resolve retry");
    let requeued = retried.requeued_as.expect("retry requeues the message");

    let cancelled =
        crate::durable_input_runtime::resolve(&session_id, &cancel_me.input_id, ResolveAction::Cancel, |_| false)
            .await
            .expect("resolve cancel");
    assert!(cancelled.requeued_as.is_none());

    let live = crate::durable_input_runtime::live_messages(&session_id).await;
    assert_eq!(live.len(), 1, "only the retried copy is live, got {live:?}");
    assert_eq!(live[0].content, "retry me");
    assert_eq!(live[0].input_id.as_deref(), Some(requeued.as_str()));

    let mut store = sandbox.store(&session_id);
    assert!(!store.state().expect("state").inputs.contains_key(&cancel_me.input_id));
}

#[cfg(unix)]
#[tokio::test]
async fn durable_input_mirror_failure_does_not_roll_back_the_accept() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    let _session = "ses_app_durable_mirror";
    let mut agent = test_agent().await;
    agent.session.save_prepared().expect("save the session");
    let session_id = agent.session_id().to_string();

    let accepted = crate::durable_input_runtime::accept(&session_id, message("accepted anyway"))
        .await
        .expect("accept survives a mirror problem");

    std::fs::create_dir_all(sandbox.mirror_dir()).expect("mirror dir");
    let mut permissions = std::fs::metadata(sandbox.mirror_dir()).expect("metadata").permissions();
    let original = permissions.clone();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o555);
    std::fs::set_permissions(sandbox.mirror_dir(), permissions).expect("make the mirror read-only");

    let mut store = sandbox.store(&session_id);
    let mirror = store.mirror();
    std::fs::set_permissions(sandbox.mirror_dir(), original).expect("restore permissions");
    assert!(mirror.is_err(), "the mirror write fails under a read-only directory");

    assert_eq!(
        store.phase_of(&accepted.input_id).expect("phase"),
        InputPhase::Accepted,
        "a mirror failure never rolls back a successful journal accept"
    );
    let refusal = store.export_legacy().expect("export call runs");
    let refused = refusal.expect_err("export refuses while the mirror is stale");
    assert!(
        refused.reasons.iter().any(|reason| reason.contains("stale")),
        "the mirror failure is an export refusal reason, got {:?}",
        refused.reasons
    );
}

#[tokio::test]
async fn durable_input_export_legacy_refuses_unless_quiescent() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    let _session = "ses_app_durable_export";
    let mut agent = test_agent().await;
    agent.session.save_prepared().expect("save the session");
    let session_id = agent.session_id().to_string();

    let accepted = crate::durable_input_runtime::accept(&session_id, message("in flight"))
        .await
        .expect("accept");
    crate::durable_input_runtime::start(&session_id, vec![accepted.input_id.clone()], None).await;

    let refused = crate::durable_input_runtime::export_legacy(&session_id)
        .await
        .expect("export call runs")
        .expect_err("an in-flight input blocks the export");
    assert!(
        refused.iter().any(|reason| reason.contains("running")),
        "got {refused:?}"
    );

    let mut store = sandbox.store(&session_id);
    store
        .seal_in_doubt(&accepted.input_id, "test_seal")
        .expect("seal in doubt");
    let refused = crate::durable_input_runtime::export_legacy(&session_id)
        .await
        .expect("export call runs")
        .expect_err("an in-doubt input blocks the export");
    assert!(
        refused.iter().any(|reason| reason.contains("in doubt")),
        "got {refused:?}"
    );

    crate::durable_input_runtime::resolve(&session_id, &accepted.input_id, ResolveAction::Cancel, |_| false)
        .await
        .expect("cancel the undetermined input");
    let exported = crate::durable_input_runtime::export_legacy(&session_id)
        .await
        .expect("export call runs")
        .expect("a quiescent session exports");
    assert_eq!(exported.exported, 0, "nothing is pending after the cancel");
}

#[tokio::test]
async fn durable_input_cancel_by_client_id_and_clear() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    let _session = "ses_app_durable_cancel";
    let mut agent = test_agent().await;
    agent.session.save_prepared().expect("save the session");
    let session_id = agent.session_id().to_string();

    let mut first = message("cancel by id");
    first.client_id = Some("client-a".to_string());
    let first = crate::durable_input_runtime::accept(&session_id, first)
        .await
        .expect("accept first");
    let second = crate::durable_input_runtime::accept(&session_id, message("clear me"))
        .await
        .expect("accept second");

    let (cancelled, not_live) =
        crate::durable_input_runtime::cancel_by_client_ids(&session_id, vec!["client-a".to_string()]).await;
    assert_eq!(cancelled, vec!["client-a".to_string()]);
    assert!(not_live.is_empty());

    let cleared = crate::durable_input_runtime::clear_pending(&session_id).await;
    assert_eq!(cleared, 1, "the remaining pending input is cleared");

    let mut store = sandbox.store(&session_id);
    assert!(store.live_messages().expect("live").is_empty());
    assert!(!store.state().expect("state").inputs.contains_key(&first.input_id));
    assert!(!store.state().expect("state").inputs.contains_key(&second.input_id));
}

#[tokio::test]
async fn durable_input_commit_clears_the_in_flight_input() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    let _session = "ses_app_durable_commit";
    let mut agent = test_agent().await;
    agent.session.save_prepared().expect("save the session");
    let session_id = agent.session_id().to_string();

    let accepted = crate::durable_input_runtime::accept(&session_id, message("commit me"))
        .await
        .expect("accept");
    crate::durable_input_runtime::start(&session_id, vec![accepted.input_id.clone()], None).await;
    crate::durable_input_runtime::commit(&session_id, vec![accepted.input_id.clone()], None).await;

    let mut store = sandbox.store(&session_id);
    let state = store.state().expect("state").clone();
    assert!(!state.inputs.contains_key(&accepted.input_id));
    assert_eq!(
        state
            .input_receipts
            .get(&accepted.input_id)
            .map(|receipt| receipt.phase),
        Some(horon_agent_base::durable_runtime::InputReceiptPhase::Committed)
    );
    let report = store.recover(&[]).expect("recover");
    assert!(report.pending.is_empty(), "a committed input is not restored");
}
