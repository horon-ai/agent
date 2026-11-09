//! Resume commits once: the environment event and restored state land together,
//! and `session_start` is the success signal that follows that commit.

use super::*;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use anyhow::Result;
use async_trait::async_trait;

struct ResumeTestProvider;

#[async_trait]
impl Provider for ResumeTestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!("the resume tests never complete a model call"))
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> String {
        "resume-test-model".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

/// Isolates the process-global session store for one test.
struct ResumeTestEnv {
    _home: tempfile::TempDir,
    _runtime: tempfile::TempDir,
    previous_home: Option<std::ffi::OsString>,
    previous_runtime: Option<std::ffi::OsString>,
}

impl ResumeTestEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create temp home");
        let runtime = tempfile::tempdir().expect("create temp runtime dir");
        let previous_home = std::env::var_os("HORON_HOME");
        let previous_runtime = std::env::var_os("HORON_AGENT_RUNTIME_DIR");
        crate::env::set_var("HORON_HOME", home.path());
        crate::env::set_var("HORON_AGENT_RUNTIME_DIR", runtime.path());
        crate::storage::create_store_dir(&home.path().join("sessions"), false).expect("create sessions dir");
        Self {
            _home: home,
            _runtime: runtime,
            previous_home,
            previous_runtime,
        }
    }
}

impl Drop for ResumeTestEnv {
    fn drop(&mut self) {
        for (key, value) in [
            ("HORON_HOME", self.previous_home.take()),
            ("HORON_AGENT_RUNTIME_DIR", self.previous_runtime.take()),
        ] {
            if let Some(value) = value {
                crate::env::set_var(key, value);
            } else {
                crate::env::remove_var(key);
            }
        }
    }
}

fn write_resume_fixture(session_id: &str) {
    let mut session = Session::create_with_id(session_id.to_string(), None, Some("resume fixture".to_string()));
    session.add_message(
        Role::User,
        vec![crate::message::ContentBlock::Text {
            text: "resume this session".to_string(),
            cache_control: None,
        }],
    );
    session.save().expect("save resume fixture");
}

fn resume_test_agent() -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(ResumeTestProvider);
    let owner_session = Session::create_with_id("agent_owner_loading_live_resume".to_string(), None, None);
    Agent::new_with_session(provider, Registry::empty(), owner_session, None)
}

fn event_log_path(session_id: &str) -> std::path::PathBuf {
    Session::load(session_id)
        .expect("load session for its event log")
        .event_log_path()
        .expect("event log path")
}

/// Metadata commits currently durable in the session's event log.
///
/// `Session::save` is outside this scope to instrument, so a durable commit is
/// counted where it is observable: one `metadata_changed` record per save.
fn durable_metadata_commits(session_id: &str) -> usize {
    let log = event_log_path(session_id);
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|record| record.get("kind").and_then(|kind| kind.as_str()).map(str::to_owned))
                .is_some_and(|kind| kind == "metadata_changed")
        })
        .count()
}

#[tokio::test]
async fn loading_live_resume_persists_env_event_in_one_save_then_fires_session_start() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeTestEnv::new();
    let session_id = "session_loading_live_resume_happy";
    write_resume_fixture(session_id);
    let mut agent = resume_test_agent();

    let commits_before = durable_metadata_commits(session_id);
    let status = agent.restore_session(session_id).expect("resume session");
    let commits_after = durable_metadata_commits(session_id);
    assert_eq!(
        commits_after - commits_before,
        1,
        "resume must fold the environment event and restored state into one durable commit"
    );

    let attempts = resume_probe::attempts_for(session_id);
    assert_eq!(attempts.len(), 1, "one resume attempt is recorded");
    let attempt = &attempts[0];
    assert_eq!(attempt.save_succeeded, Some(true), "the coalesced save must succeed");
    assert!(
        attempt.session_start_fired,
        "session_start fires once the coalesced save has landed"
    );

    let reloaded = Session::load(session_id).expect("reload resumed session");
    let resume_snapshots = reloaded
        .env_snapshots
        .iter()
        .filter(|snapshot| snapshot.reason == "resume")
        .count();
    assert_eq!(resume_snapshots, 1, "the resume environment event survives a restart");
    assert_eq!(
        reloaded.status,
        crate::session::SessionStatus::Active,
        "the resumed state landed in the same commit as the environment event"
    );
    assert_eq!(status, crate::session::SessionStatus::Active);
    assert_eq!(reloaded.active_context().len(), 1);
}

#[tokio::test]
async fn loading_live_resume_refuses_success_and_session_start_when_the_save_fails() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeTestEnv::new();
    let session_id = "session_loading_live_resume_save_failure";
    write_resume_fixture(session_id);
    let mut agent = resume_test_agent();

    crate::storage::write_session_lease(&crate::storage::SessionLease {
        session_id: session_id.to_string(),
        epoch: 1,
        away_host: Some("cloud".to_string()),
        repo_root: None,
        last_sync_snapshot: None,
        last_sync_head: None,
        updated_at: "test".to_string(),
    })
    .expect("write a lease that refuses persistence on this machine");

    agent
        .restore_session(session_id)
        .expect_err("a failed resume save must not return a successfully restored session");

    let attempts = resume_probe::attempts_for(session_id);
    assert_eq!(attempts.len(), 1, "one resume attempt is recorded");
    let attempt = &attempts[0];
    assert_eq!(
        attempt.save_succeeded,
        Some(false),
        "a session that moved away must refuse to persist"
    );
    assert!(
        !attempt.session_start_fired,
        "a failed save must not announce session_start for state that is not durable"
    );

    crate::storage::remove_session_lease(session_id);
    let reloaded = Session::load(session_id).expect("reload after the failed save");
    assert!(
        reloaded
            .env_snapshots
            .iter()
            .all(|snapshot| snapshot.reason != "resume"),
        "the environment event must not reach disk when its commit failed"
    );
}

#[tokio::test]
async fn deferred_resume_serves_saved_history_and_blocks_tools_until_its_commit_lands() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeTestEnv::new();
    let session_id = "session_deferred_resume_order";
    write_resume_fixture(session_id);
    let mut agent = resume_test_agent();
    let commits_before = durable_metadata_commits(session_id);

    agent
        .prepare_restored_session(session_id, None)
        .expect("prepare the resumed session");
    // The transcript an attachment delivers is already readable here, and the
    // commit it will pay for afterwards has not run yet.
    assert_eq!(
        agent.get_history(false).len(),
        1,
        "saved History is available before the commit"
    );
    assert_eq!(
        durable_metadata_commits(session_id),
        commits_before,
        "preparing must not commit the resumed state"
    );
    assert!(crate::agent::resume_commit_owed(session_id));
    // A direct tool run cannot commit (it only borrows the agent), so it refuses.
    assert!(
        agent
            .execute_tool("read", serde_json::json!({"file_path": "/dev/null"}))
            .await
            .is_err(),
        "a direct tool run must not run ahead of the resume commit"
    );
    assert_eq!(durable_metadata_commits(session_id), commits_before);

    agent.finish_resume_persistence().expect("persist the resumed session");
    assert_eq!(durable_metadata_commits(session_id) - commits_before, 1);
    assert!(!crate::agent::resume_commit_owed(session_id));
    let attempts = resume_probe::attempts_for(session_id);
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].save_succeeded, Some(true));
    assert!(
        attempts[0].session_start_fired,
        "session_start follows the durable commit"
    );
}

#[tokio::test]
async fn message_in_restore_window_commits_then_runs() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeTestEnv::new();
    let session_id = "session_message_in_restore_window";
    write_resume_fixture(session_id);
    let mut agent = resume_test_agent();

    agent
        .prepare_restored_session(session_id, None)
        .expect("prepare the resumed session");
    agent
        .append_user_context_message("hello", Vec::new())
        .expect("a message during the restore window lands the commit and then runs");

    let attempts = resume_probe::attempts_for(session_id);
    assert_eq!(attempts.len(), 1, "the message paid for exactly one resume commit");
    assert_eq!(attempts[0].save_succeeded, Some(true));
    assert!(attempts[0].session_start_fired, "session_start fires once");
    assert!(agent.pending_resume.is_none());
    assert!(!crate::agent::resume_commit_owed(session_id));
}

#[tokio::test]
async fn failed_resume_commit_is_retried_by_the_next_entry() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeTestEnv::new();
    let session_id = "session_deferred_resume_failure";
    write_resume_fixture(session_id);
    let mut agent = resume_test_agent();

    crate::storage::write_session_lease(&crate::storage::SessionLease {
        session_id: session_id.to_string(),
        epoch: 1,
        away_host: Some("cloud".to_string()),
        repo_root: None,
        last_sync_snapshot: None,
        last_sync_head: None,
        updated_at: "test".to_string(),
    })
    .expect("write a lease that refuses persistence on this machine");

    agent
        .prepare_restored_session(session_id, None)
        .expect("prepare the resumed session");
    let refused = agent
        .finish_resume_persistence()
        .expect_err("a deferred commit that cannot land must report the failure");
    assert!(
        refused.downcast_ref::<crate::agent::ResumeCommitFailed>().is_some(),
        "the failure is typed so clients get a retry hint: {refused:#}"
    );
    let attempts = resume_probe::attempts_for(session_id);
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].save_succeeded, Some(false));
    assert!(
        !attempts[0].session_start_fired,
        "a failed commit must not announce session_start"
    );
    assert!(crate::agent::resume_commit_owed(session_id), "the barrier stays closed");

    crate::storage::remove_session_lease(session_id);
    agent
        .append_user_context_message("hello", Vec::new())
        .expect("the next entry retries the commit and then runs");
    let attempts = resume_probe::attempts_for(session_id);
    assert_eq!(attempts.len(), 2, "the retry is a second commit attempt");
    assert_eq!(attempts[1].save_succeeded, Some(true));
    assert_eq!(
        attempts.iter().filter(|attempt| attempt.session_start_fired).count(),
        1,
        "session_start fires once, for the commit that landed"
    );
    assert!(!crate::agent::resume_commit_owed(session_id));
}

#[tokio::test]
async fn rename_while_commit_owed_commits_once_and_keeps_the_title() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeTestEnv::new();
    let session_id = "session_rename_while_owed";
    write_resume_fixture(session_id);
    let mut agent = resume_test_agent();

    agent
        .prepare_restored_session(session_id, None)
        .expect("prepare the resumed session");
    agent
        .rename_session_title(Some("x".to_string()))
        .expect("rename lands the owed commit");

    let attempts = resume_probe::attempts_for(session_id);
    assert_eq!(attempts.len(), 1, "the rename save is the resume commit");
    assert!(attempts[0].session_start_fired);
    assert!(!crate::agent::resume_commit_owed(session_id));
    let reloaded = Session::load(session_id).expect("reload renamed session");
    assert_eq!(reloaded.display_title(), Some("x"));
}

#[tokio::test]
async fn closing_with_an_owed_commit_never_fires_session_start() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeTestEnv::new();
    let session_id = "session_close_while_owed";
    write_resume_fixture(session_id);
    let mut agent = resume_test_agent();

    agent
        .prepare_restored_session(session_id, None)
        .expect("prepare the resumed session");
    agent.mark_closed();

    let attempts = resume_probe::attempts_for(session_id);
    assert!(
        attempts.iter().all(|attempt| !attempt.session_start_fired),
        "closing is not the resume commit"
    );
    assert!(!crate::agent::resume_commit_owed(session_id));
    let reloaded = Session::load(session_id).expect("reload closed session");
    assert_eq!(reloaded.status, crate::session::SessionStatus::Closed);
}

#[tokio::test]
async fn resume_env_snapshot_is_recorded_once_inside_the_commit() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeTestEnv::new();
    let session_id = "session_resume_env_snapshot_once";
    write_resume_fixture(session_id);
    let mut agent = resume_test_agent();
    crate::storage::write_session_lease(&crate::storage::SessionLease {
        session_id: session_id.to_string(),
        epoch: 1,
        away_host: Some("cloud".to_string()),
        repo_root: None,
        last_sync_snapshot: None,
        last_sync_head: None,
        updated_at: "test".to_string(),
    })
    .expect("write a lease that refuses persistence on this machine");

    agent
        .prepare_restored_session(session_id, None)
        .expect("prepare the resumed session");
    assert!(
        !agent.env_snapshot_reasons().contains(&"resume"),
        "preparing must not pay for the environment probe"
    );
    agent.finish_resume_persistence().expect_err("the first commit fails");
    crate::storage::remove_session_lease(session_id);
    agent.finish_resume_persistence().expect("the retried commit lands");

    let in_memory = agent
        .env_snapshot_reasons()
        .iter()
        .filter(|reason| **reason == "resume")
        .count();
    assert_eq!(in_memory, 1, "a retried commit records the event once");
    let reloaded = Session::load(session_id).expect("reload resumed session");
    assert_eq!(
        reloaded
            .env_snapshots
            .iter()
            .filter(|snapshot| snapshot.reason == "resume")
            .count(),
        1
    );
}
