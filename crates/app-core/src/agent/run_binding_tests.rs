//! The binding a run row carries: what the row states, which publish it lets
//! land by rename, and what a reload reads back from either copy.

use super::*;

use horon_agent_agent_runtime::{StopDisposition, StopProvenance};
use horon_agent_base::durable_runtime::{
    Binding, DurableRuntimeStore, JournalOptions, Payload, StopCause, read_journal_records,
};

use crate::provider::EventStream;
use crate::tool::Registry;

/// A provider no test here calls: the run boundaries are driven directly.
struct IdleProvider;

#[async_trait::async_trait]
impl Provider for IdleProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        Err(anyhow::anyhow!("no request in a run-boundary test"))
    }

    fn name(&self) -> &str {
        "run-binding-probe"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

/// An agent with one visible message, so closing it persists the session.
fn probe_agent() -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(IdleProvider);
    let mut agent = Agent::new(provider, Registry::empty());
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "go".into(),
            cache_control: None,
        }],
    );
    agent
}

fn home() -> std::path::PathBuf {
    horon_agent_base::storage::horon_agent_dir().expect("the sandbox home resolves")
}

fn journal_root() -> std::path::PathBuf {
    home().join(horon_agent_base::durable_runtime::JOURNAL_DIR_NAME)
}

fn journal_path(session_id: &str) -> std::path::PathBuf {
    journal_root()
        .join(session_id)
        .join(horon_agent_base::durable_runtime::JOURNAL_FILE_NAME)
}

fn mirror_path(session_id: &str) -> std::path::PathBuf {
    home().join("sessions").join(session_id).join("local-runtime.json")
}

/// Every binding the session's run rows state, oldest row first.
fn row_bindings(session_id: &str) -> Vec<Binding> {
    read_journal_records(&journal_path(session_id))
        .expect("the journal is readable")
        .into_iter()
        .filter_map(|record| match record.payload {
            Payload::RunStarted(row) => row.binding,
            Payload::RunStopped(row) => row.binding,
            _ => None,
        })
        .collect()
}

fn mirror(session_id: &str) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(mirror_path(session_id)).expect("the mirror exists"))
        .expect("the mirror parses")
}

fn mirror_rev(session_id: &str) -> u64 {
    mirror(session_id)["rev"].as_u64().expect("the mirror states a rev")
}

/// Drive one complete run boundary, the way a turn does.
async fn boundary(agent: &mut Agent) -> String {
    let run = agent.begin_run();
    agent.settle_run_start(&run).await;
    agent.finish_run(run.run_id(), StopProvenance::Completed).await;
    run.run_id().to_string()
}

/// A closing process states the binding its close will publish, and the close
/// then reuses that row's version instead of flushing one of its own.
#[tokio::test]
async fn closing_agent_states_the_close_binding_and_the_close_reuses_its_rev() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let mut agent = probe_agent().closing_after_run();
    let session_id = agent.session.id.clone();

    boundary(&mut agent).await;
    let stopped = row_bindings(&session_id).pop().expect("the stop row states a binding");
    assert_eq!(stopped.status, SessionStatus::Closed);
    assert_eq!(stopped.last_pid, agent.session.last_pid);
    assert_eq!(stopped.working_dir, agent.session.working_dir);
    assert_eq!(stopped.provider_session_id, agent.session.provider_session_id);

    agent.mark_closed();
    assert_eq!(
        mirror_rev(&session_id),
        stopped.rev,
        "the close publishes the version its own stop row states"
    );
    assert_eq!(mirror(&session_id)["status"], serde_json::json!("Closed"));

    let reloaded = Session::load(&session_id).expect("the closed session loads");
    assert_eq!(reloaded.status, SessionStatus::Closed);
}

/// A daemon session stays open after the turn, so its stop row states nothing
/// and the later close has to flush a version no row answers.
#[tokio::test]
async fn daemon_stop_row_states_nothing_and_the_close_flushes_a_new_rev() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let mut agent = probe_agent();
    let session_id = agent.session.id.clone();

    boundary(&mut agent).await;
    let rows = row_bindings(&session_id);
    assert_eq!(rows.len(), 1, "only the start row states a binding: {rows:?}");
    let started = rows[0].rev;

    agent.mark_closed();
    let rev = mirror_rev(&session_id);
    assert!(
        rev > started,
        "a close no row confirms takes a version above the last row: row={started} mirror={rev}"
    );
}

/// A row that never lands confirms nothing, so the close keeps the flushing
/// path instead of publishing a version the journal does not answer.
#[tokio::test]
async fn failed_run_row_write_keeps_the_close_on_the_flushing_path() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let mut agent = probe_agent().closing_after_run();
    let session_id = agent.session.id.clone();

    // A journal path that cannot be opened refuses every row from here on.
    let path = journal_path(&session_id);
    std::fs::create_dir_all(&path).expect("the journal path is replaced by a directory");

    let run = agent.begin_run();
    agent.settle_run_start(&run).await;
    let disposition = agent.finish_run(run.run_id(), StopProvenance::Completed).await;

    assert_eq!(disposition, StopDisposition::Current);
    let stop = agent.current_stop().expect("the in-memory stop is kept");
    assert!(!stop.persisted(), "a row that never landed is not persisted");
    assert!(
        row_bindings(&session_id).is_empty(),
        "no row landed, so none states a binding"
    );

    agent.mark_closed();
    assert_eq!(mirror(&session_id)["status"], serde_json::json!("Closed"));
    let reloaded = Session::load(&session_id).expect("the closed session loads");
    assert_eq!(reloaded.status, SessionStatus::Closed);
}

/// The four end-of-run states resolve to the copy that holds the higher
/// version, and a session no copy describes falls back to its defaults.
#[tokio::test]
async fn end_of_run_crash_states_load_the_higher_version() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    for state in [State::Both, State::RowOnly, State::MirrorOnly, State::Neither] {
        let id = state.id();
        let mut session = Session::create_with_id(id.to_string(), None, None);
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "prompt".into(),
                cache_control: None,
            }],
        );
        session.save().expect("the session saves");
        let live = session.status.clone();
        let live_mirror = std::fs::read(mirror_path(id)).expect("the live mirror exists");
        session.mark_closed();
        session.save().expect("the close saves");
        let closed = session.status.clone();
        let closed_rev = mirror_rev(id);
        drop(session);

        // The stop row states the closed binding at the version the close wrote.
        let mut store = DurableRuntimeStore::open_in_with(
            &journal_root(),
            id,
            JournalOptions {
                limits: Default::default(),
                hooks: None,
            },
        )
        .expect("the journal opens");
        store
            .append_run_stopped(
                "run-1",
                StopCause::Completed,
                None,
                Some(Binding::new(closed_rev, SessionStatus::Closed)),
            )
            .expect("the stop row lands");
        drop(store);

        match state {
            State::Both => {}
            State::RowOnly => std::fs::remove_file(mirror_path(id)).expect("the mirror is removable"),
            State::MirrorOnly => {
                std::fs::write(mirror_path(id), &live_mirror).expect("the older mirror is restored");
                std::fs::remove_file(journal_path(id)).expect("the journal is removable");
            }
            State::Neither => {
                std::fs::remove_file(journal_path(id)).expect("the journal is removable");
                std::fs::remove_file(mirror_path(id)).expect("the mirror is removable");
            }
        }

        let reloaded = Session::load(id).expect("the session loads");
        let expected = if matches!(state, State::MirrorOnly | State::Neither) {
            live.clone()
        } else {
            closed.clone()
        };
        assert_eq!(reloaded.status, expected, "{state:?} reads the newer copy");
    }
}

#[derive(Clone, Copy, Debug)]
enum State {
    Both,
    RowOnly,
    MirrorOnly,
    Neither,
}

impl State {
    fn id(self) -> &'static str {
        match self {
            State::Both => "binding_both_copies",
            State::RowOnly => "binding_row_only",
            State::MirrorOnly => "binding_mirror_only",
            State::Neither => "binding_neither",
        }
    }
}
