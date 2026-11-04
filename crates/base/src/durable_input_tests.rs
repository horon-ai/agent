use super::*;

use std::path::Path;

use crate::durable_runtime::InputReceiptPhase;
use horon_agent_agent_runtime::{SoftInterruptMessage, SoftInterruptSource};

struct Sandbox {
    _dir: tempfile::TempDir,
    home: std::path::PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let home = dir.path().to_path_buf();
        Self { _dir: dir, home }
    }

    fn journal_root(&self) -> std::path::PathBuf {
        self.home.join(crate::durable_runtime::JOURNAL_DIR_NAME)
    }

    fn store(&self, session_id: &str) -> DurableInputStore {
        DurableInputStore::open_in(&self.journal_root(), session_id).expect("open the input store")
    }

    fn set_home(&self) {
        crate::env::set_var("HORON_AGENT_HOME", &self.home);
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

fn message_with_client(content: &str, client_id: &str) -> SoftInterruptMessage {
    SoftInterruptMessage {
        client_id: Some(client_id.to_string()),
        ..message(content)
    }
}

fn mirror_file(home: &Path, session_id: &str) -> std::path::PathBuf {
    home.join("pending-soft-interrupts").join(format!("{session_id}.json"))
}

#[test]
fn durable_input_accept_survives_restart_exactly_once() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    sandbox.set_home();
    let session = "ses_durable_input_accept";

    let mut store = sandbox.store(session);
    let mut queued = message("survive the restart");
    let accepted = store.accept(&mut queued).expect("accept the input");
    assert!(!accepted.already_live);
    assert_eq!(queued.input_id.as_deref(), Some(accepted.input_id.as_str()));
    drop(store);

    let mut reopened = sandbox.store(session);
    let first = reopened.recover(&[]).expect("recover after restart");
    assert_eq!(first.pending.len(), 1, "the accepted input restores once");
    assert_eq!(first.pending[0].content, "survive the restart");
    assert_eq!(first.pending[0].input_id.as_deref(), Some(accepted.input_id.as_str()));

    let second = reopened.recover(&[]).expect("recover a second time");
    assert_eq!(
        second.pending.len(),
        1,
        "restoring twice yields one entry, not a duplicate"
    );
    assert_eq!(second.pending[0].input_id, first.pending[0].input_id);
}

#[test]
fn durable_input_started_without_terminal_becomes_in_doubt_and_is_not_reinjected() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    sandbox.set_home();
    let session = "ses_durable_input_in_doubt";

    let mut store = sandbox.store(session);
    let mut queued = message("maybe it ran");
    let accepted = store.accept(&mut queued).expect("accept");
    store.start(&accepted.input_id, None).expect("start");
    drop(store);

    let mut reopened = sandbox.store(session);
    let recovery = reopened.recover(&[]).expect("recover");
    assert!(
        recovery.pending.is_empty(),
        "a started input with no terminal row is not re-injected, got {:?}",
        recovery.pending
    );
    assert_eq!(recovery.sealed_in_doubt, vec![accepted.input_id.clone()]);
    assert_eq!(
        reopened.phase_of(&accepted.input_id).expect("phase"),
        InputPhase::InDoubt
    );

    let again = reopened.recover(&[]).expect("recover again");
    assert!(again.pending.is_empty());
    assert!(
        again.sealed_in_doubt.is_empty(),
        "a second recovery does not reseal an input already in doubt"
    );
    assert_eq!(again.already_in_doubt, vec![accepted.input_id.clone()]);
}

#[test]
fn durable_input_explicit_retry_requeues_and_cancel_drops() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    sandbox.set_home();
    let session = "ses_durable_input_resolve";

    let mut store = sandbox.store(session);
    let mut first = message("retry me");
    let retried = store.accept(&mut first).expect("accept first");
    store.start(&retried.input_id, None).expect("start first");

    let mut second = message("cancel me");
    let cancelled = store.accept(&mut second).expect("accept second");
    store.start(&cancelled.input_id, None).expect("start second");
    drop(store);

    let mut reopened = sandbox.store(session);
    let recovery = reopened.recover(&[]).expect("recover");
    assert!(recovery.pending.is_empty());

    let retry = reopened
        .resolve(&retried.input_id, ResolveAction::Retry, |_| true)
        .expect("retry");
    assert_eq!(retry.previous_phase, InputPhase::InDoubt);
    let requeued = retry.requeued_as.expect("retry requeues the message");
    assert_ne!(requeued, retried.input_id, "the retry uses a fresh id");

    let cancel = reopened
        .resolve(&cancelled.input_id, ResolveAction::Cancel, |_| true)
        .expect("cancel");
    assert!(cancel.requeued_as.is_none(), "cancel requeues nothing");

    let live = reopened.live_messages().expect("live messages");
    assert_eq!(live.len(), 1, "only the retried copy is live, got {live:?}");
    assert_eq!(live[0].content, "retry me");
    assert_eq!(live[0].input_id.as_deref(), Some(requeued.as_str()));

    let state = reopened.state().expect("state").clone();
    assert!(!state.inputs.contains_key(&retried.input_id));
    assert!(!state.inputs.contains_key(&cancelled.input_id));
    assert_eq!(
        state.input_receipts.get(&retried.input_id).map(|receipt| receipt.phase),
        Some(InputReceiptPhase::Cancelled)
    );
}

#[test]
fn durable_input_retry_cleans_the_previously_injected_copy() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    sandbox.set_home();
    let session = "ses_durable_input_retry_copy";

    let mut store = sandbox.store(session);
    let mut queued = message("injected then uncertain");
    let accepted = store.accept(&mut queued).expect("accept");
    store.start(&accepted.input_id, None).expect("start");
    store
        .note_injected_copy(&accepted.input_id, "message-42")
        .expect("record the injected copy");
    drop(store);

    let mut reopened = sandbox.store(session);
    reopened.recover(&[]).expect("recover");

    let mut removed: Vec<String> = Vec::new();
    let resolved = reopened
        .resolve(&accepted.input_id, ResolveAction::Retry, |message_id| {
            removed.push(message_id.to_string());
            true
        })
        .expect("retry");
    assert_eq!(
        removed,
        vec!["message-42".to_string()],
        "retry removes the transcript copy it recorded"
    );
    assert!(resolved.cleaned_injected_copy);
    assert!(resolved.warning.is_none());
}

/// The failure is induced by a read-only directory, which only exists as a
/// concept where the filesystem carries POSIX write bits; Windows accepts the
/// write, so the premise of this test has no Windows form.
#[cfg(unix)]
#[test]
fn durable_input_mirror_failure_keeps_the_accept() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    sandbox.set_home();
    let session = "ses_durable_input_mirror_failure";

    let mut store = sandbox.store(session);
    let mut queued = message("accepted despite a bad mirror");
    let accepted = store.accept(&mut queued).expect("accept the input");

    let mirror_dir = sandbox.home.join("pending-soft-interrupts");
    std::fs::create_dir_all(&mirror_dir).expect("mirror dir");
    let mut permissions = std::fs::metadata(&mirror_dir).expect("metadata").permissions();
    let original = permissions.clone();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o555);
    std::fs::set_permissions(&mirror_dir, permissions).expect("make the mirror dir read-only");

    let mirror = store.mirror();
    std::fs::set_permissions(&mirror_dir, original).expect("restore mirror dir permissions");
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
        "the mirror failure surfaces as an export refusal reason, got {:?}",
        refused.reasons
    );

    store.mirror().expect("a later mirror succeeds");
    let exported = store
        .export_legacy()
        .expect("export call runs")
        .expect("export succeeds");
    assert_eq!(exported.exported, 1);
    assert!(exported.path.exists(), "the legacy file is written");
}

#[test]
fn durable_input_export_legacy_refuses_while_undetermined() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    sandbox.set_home();
    let session = "ses_durable_input_export_refusal";

    let mut store = sandbox.store(session);
    let mut queued = message("still running");
    let accepted = store.accept(&mut queued).expect("accept");
    store.start(&accepted.input_id, None).expect("start");

    let refusal = store.export_legacy().expect("export call runs");
    let refused = refusal.expect_err("export refuses while an input is in flight");
    assert!(
        refused.reasons.iter().any(|reason| reason.contains("running")),
        "a started input blocks the export, got {:?}",
        refused.reasons
    );

    store
        .seal_in_doubt(&accepted.input_id, "test_seal")
        .expect("seal in doubt");
    let refusal = store.export_legacy().expect("export call runs");
    let refused = refusal.expect_err("export refuses while an input is in doubt");
    assert!(
        refused.reasons.iter().any(|reason| reason.contains("in doubt")),
        "an in-doubt input blocks the export, got {:?}",
        refused.reasons
    );

    store
        .resolve(&accepted.input_id, ResolveAction::Cancel, |_| false)
        .expect("cancel the undetermined input");
    let exported = store
        .export_legacy()
        .expect("export call runs")
        .expect("a quiescent session exports");
    assert_eq!(exported.exported, 0, "nothing is pending after the cancel");
}

#[test]
fn durable_input_legacy_baseline_is_imported_once_with_stable_ids() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    sandbox.set_home();
    let session = "ses_durable_input_baseline";

    crate::soft_interrupt_store::overwrite_durable(session, &[message_with_client("from the old build", "old-1")])
        .expect("seed the legacy file");

    let mut store = sandbox.store(session);
    let first = store.recover(&[]).expect("recover");
    assert_eq!(first.imported_from_legacy, 1, "the baseline imports once");
    assert_eq!(first.pending.len(), 1);
    assert_eq!(first.pending[0].client_id.as_deref(), Some("old-1"));
    let imported_id = first.pending[0].input_id.clone().expect("an input id");
    drop(store);

    let mut reopened = sandbox.store(session);
    let second = reopened.recover(&[]).expect("recover again");
    assert_eq!(second.imported_from_legacy, 0, "the baseline is never re-imported");
    assert_eq!(second.pending.len(), 1);
    assert_eq!(
        second.pending[0].input_id.as_deref(),
        Some(imported_id.as_str()),
        "the deterministic mapping keeps the same id"
    );
}

#[test]
fn durable_input_client_id_is_preserved_separately_from_the_input_id() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    sandbox.set_home();
    let session = "ses_durable_input_client_id";

    let mut store = sandbox.store(session);
    let mut queued = message_with_client("keep my caller id", "client-7");
    let accepted = store.accept(&mut queued).expect("accept");
    assert_ne!(
        accepted.input_id.as_str(),
        "client-7",
        "the journal id is never the caller id"
    );

    let live = store.live_messages().expect("live messages");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].client_id.as_deref(), Some("client-7"));
    assert_eq!(live[0].input_id.as_deref(), Some(accepted.input_id.as_str()));
}

#[test]
fn durable_input_commit_and_cancel_leave_receipts() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    sandbox.set_home();
    let session = "ses_durable_input_receipts";

    let mut store = sandbox.store(session);
    let mut committed = message("finish normally");
    let committed = store.accept(&mut committed).expect("accept").input_id;
    store.start(&committed, Some("run-1")).expect("start");
    store.commit(&committed, Some("run-1")).expect("commit");

    let mut cancelled = message("cancel while pending");
    let cancelled = store.accept(&mut cancelled).expect("accept").input_id;
    store.cancel(&cancelled, "user_cleared").expect("cancel");

    let state = store.state().expect("state").clone();
    assert_eq!(
        state.input_receipts.get(&committed).map(|receipt| receipt.phase),
        Some(InputReceiptPhase::Committed)
    );
    assert_eq!(
        state.input_receipts.get(&cancelled).map(|receipt| receipt.phase),
        Some(InputReceiptPhase::Cancelled)
    );
    assert!(state.inputs.is_empty(), "both inputs left live state");

    let mirror = mirror_file(&sandbox.home, session);
    assert!(
        !mirror.exists(),
        "an empty pending set removes the legacy mirror rather than writing []"
    );
}

#[test]
fn durable_input_accept_is_deduplicated_by_input_id() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    sandbox.set_home();
    let session = "ses_durable_input_dedup";

    let mut store = sandbox.store(session);
    let mut first = message("only once");
    let accepted = store.accept(&mut first).expect("accept");

    let mut replay = message("only once");
    replay.input_id = Some(accepted.input_id.clone());
    let again = store.accept(&mut replay).expect("accept the same id");
    assert!(again.already_live, "a replayed id is reported as already live");
    assert_eq!(store.live_messages().expect("live").len(), 1);
}

#[test]
fn durable_input_resolve_seals_a_started_input_that_recovery_never_saw() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    sandbox.set_home();
    let session = "ses_durable_input_resolve_unsealed";

    let mut store = sandbox.store(session);
    let mut queued = message("the process died mid-turn");
    let accepted = store.accept(&mut queued).expect("accept");
    store.start(&accepted.input_id, None).expect("start");

    let resolved = store
        .resolve(&accepted.input_id, ResolveAction::Cancel, |_| false)
        .expect("resolve applies to an input the crash left started");
    assert_eq!(
        resolved.previous_phase,
        InputPhase::Started,
        "the reported previous phase is what the journal held"
    );
    assert!(resolved.requeued_as.is_none());
    assert!(
        store.state().expect("state").inputs.is_empty(),
        "the undetermined input leaves live state"
    );
    store.export_legacy().expect("export call runs").expect("quiescent now");
}

#[test]
fn durable_input_start_returns_a_started_input_to_pending_first() {
    let _guard = crate::storage::lock_test_env();
    let sandbox = Sandbox::new();
    sandbox.set_home();
    let session = "ses_durable_input_reinject";

    let mut store = sandbox.store(session);
    let mut queued = message("inject me twice");
    let accepted = store.accept(&mut queued).expect("accept");
    store.start(&accepted.input_id, None).expect("first start");
    store
        .start(&accepted.input_id, None)
        .expect("a second injection returns it to pending first");
    assert_eq!(store.phase_of(&accepted.input_id).expect("phase"), InputPhase::Started);
}
