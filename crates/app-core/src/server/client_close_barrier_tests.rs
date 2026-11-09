//! Close-barrier mechanics: what a published gate holds, who may wait on it,
//! and how the guard is released. The end-to-end resume paths live beside the
//! resume harness in `client_session_tests/resume/close_barrier.rs`.

use super::*;
use crate::protocol::ServerEvent;
use tokio::time::timeout;

fn connection(name: &str, session_id: &str) -> ClientConnectionInfo {
    let (disconnect_tx, _) = mpsc::unbounded_channel();
    ClientConnectionInfo {
        client_id: name.into(),
        session_id: session_id.into(),
        client_instance_id: None,
        connected_at: std::time::Instant::now(),
        last_seen: std::time::Instant::now(),
        is_processing: false,
        current_tool_name: None,
        terminal_env: vec![],
        disconnect_tx,
    }
}

/// Runs `body` on a fresh runtime while the process-wide test env lock is held
/// on this thread. These tests observe process-global barrier state, so they
/// must stay serialized, but the `std` guard belongs outside the async body
/// rather than held across an await point.
fn run_with_env_lock(body: impl Future<Output = ()>) {
    let _lock = crate::storage::lock_test_env();
    tokio::runtime::Runtime::new().expect("runtime").block_on(body);
}

#[test]
fn waiter_blocks_until_the_owning_guard_is_dropped() {
    run_with_env_lock(waiter_blocks_until_the_owning_guard_is_dropped_inner());
}

async fn waiter_blocks_until_the_owning_guard_is_dropped_inner() {
    let session_id = "session_barrier_waiter_blocks";
    let guard = SessionCloseGuard::register(session_id).await;

    let reached = close_barrier_probe::observe(|| close_barrier_probe::waits(session_id) > 0);
    let waiter = tokio::spawn({
        let session_id = session_id.to_string();
        async move { await_pending_close(&session_id).await }
    });
    // The wait is recorded before the await, so this proves the resume reached
    // the barrier instead of merely having started.
    timeout(Duration::from_secs(2), reached)
        .await
        .expect("the waiter must reach the barrier");
    assert!(!waiter.is_finished(), "a published gate must hold the waiter");

    drop(guard);
    timeout(Duration::from_secs(2), waiter)
        .await
        .expect("dropping the guard releases the waiter")
        .expect("waiter join");
    assert_eq!(
        close_barrier_probe::published_gates(),
        0,
        "a released guard leaves no entry behind"
    );
}

#[test]
fn unregistered_session_waits_for_nothing() {
    run_with_env_lock(unregistered_session_waits_for_nothing_inner());
}

async fn unregistered_session_waits_for_nothing_inner() {
    let session_id = "session_barrier_no_gate";
    timeout(Duration::from_secs(1), await_pending_close(session_id))
        .await
        .expect("a session with no close in flight must not wait");
    assert_eq!(close_barrier_probe::waits(session_id), 0);
}

#[test]
fn replaced_guard_does_not_remove_the_successor_gate() {
    run_with_env_lock(replaced_guard_does_not_remove_the_successor_gate_inner());
}

async fn replaced_guard_does_not_remove_the_successor_gate_inner() {
    let session_id = "session_barrier_replaced";
    let first = SessionCloseGuard::register(session_id).await;
    let second = SessionCloseGuard::register(session_id).await;

    // The first owner finishes while the second still holds its own gate.
    drop(first);
    assert_eq!(
        close_barrier_probe::published_gates(),
        1,
        "the replaced guard must not delete the entry that replaced it"
    );
    let reached = close_barrier_probe::observe(|| close_barrier_probe::waits(session_id) > 0);
    let waiter = tokio::spawn({
        let session_id = session_id.to_string();
        async move { await_pending_close(&session_id).await }
    });
    timeout(Duration::from_secs(2), reached)
        .await
        .expect("the successor gate is still published");
    assert!(
        !waiter.is_finished(),
        "waiters of the successor gate must still be held"
    );

    drop(second);
    timeout(Duration::from_secs(2), waiter)
        .await
        .expect("the successor guard releases its own waiters")
        .expect("waiter join");
    assert_eq!(close_barrier_probe::published_gates(), 0);
}

#[test]
fn cancelled_close_leaves_no_entry_behind() {
    run_with_env_lock(cancelled_close_leaves_no_entry_behind_inner());
}

async fn cancelled_close_leaves_no_entry_behind_inner() {
    let session_id = "session_barrier_cancelled";
    let published = close_barrier_probe::observe(|| close_barrier_probe::published_gates() > 0);
    let task = tokio::spawn({
        let session_id = session_id.to_string();
        async move {
            let _guard = SessionCloseGuard::register(&session_id).await;
            std::future::pending::<()>().await;
        }
    });
    timeout(Duration::from_secs(2), published)
        .await
        .expect("the close published its gate");

    task.abort();
    let _ = task.await;
    assert_eq!(
        close_barrier_probe::published_gates(),
        0,
        "a cancelled close must not accumulate a gate"
    );
}

#[test]
fn blocking_session_io_runs_directly_without_a_runtime() {
    let on_caller_thread = std::thread::current().id();
    let observed = blocking_session_io(|| std::thread::current().id());
    assert_eq!(observed, on_caller_thread);
}

#[tokio::test]
async fn blocking_session_io_runs_directly_on_a_current_thread_runtime() {
    // `block_in_place` panics on a current-thread runtime, so reaching the
    // assertion at all is what proves the helper chose the direct branch.
    let on_caller_thread = std::thread::current().id();
    assert_eq!(blocking_session_io(|| std::thread::current().id()), on_caller_thread);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn blocking_session_io_hands_the_worker_to_other_tasks() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (progress_tx, progress_rx) = std::sync::mpsc::channel();
    let other = tokio::spawn(async move {
        started_rx.await.expect("the blocking task starts");
        progress_tx.send(()).expect("the blocking task waits for progress");
    });

    // Spawn onto the sole worker: the test's own block_on thread is not a worker.
    // A direct call would starve `other` until the channel deadline expires.
    let blocking = tokio::spawn(async move {
        blocking_session_io(|| {
            started_tx.send(()).expect("notify the other task");
            progress_rx.recv_timeout(Duration::from_secs(5)).is_ok()
        })
    });
    let observed = timeout(Duration::from_secs(10), blocking)
        .await
        .expect("the blocking task finishes")
        .expect("blocking task join");
    other.await.expect("other task join");
    assert!(
        observed,
        "blocking session I/O must let another task run on the same worker"
    );
}

#[test]
fn cold_claim_serializes_holders_and_keeps_one_entry() {
    run_with_env_lock(cold_claim_serializes_holders_and_keeps_one_entry_inner());
}

async fn cold_claim_serializes_holders_and_keeps_one_entry_inner() {
    let session_id = "session_cold_claim_serialize";
    let first = claim_cold_resume(session_id).await;
    let entry_before = close_barrier_probe::cold_resume_claim_ptr(session_id);
    assert!(entry_before.is_some());
    let second_reached = close_barrier_probe::observe(|| close_barrier_probe::claim_waits(session_id) >= 2);
    let waiter = tokio::spawn({
        let session_id = session_id.to_string();
        async move { claim_cold_resume(&session_id).await }
    });
    timeout(Duration::from_secs(2), second_reached)
        .await
        .expect("the second attempt reaches the shared entry before the holder releases it");
    assert!(
        !waiter.is_finished(),
        "a second cold claim cannot be granted while the first holder lives"
    );

    drop(first);
    let second = timeout(Duration::from_secs(2), waiter)
        .await
        .expect("the claim is handed over when the holder releases it")
        .expect("waiter join");
    assert_eq!(
        close_barrier_probe::cold_resume_claim_ptr(session_id),
        entry_before,
        "a handoff must reuse the same entry, never a second lock for one id"
    );

    let third_reached = close_barrier_probe::observe(|| close_barrier_probe::claim_waits(session_id) >= 3);
    let third = tokio::spawn({
        let session_id = session_id.to_string();
        async move { claim_cold_resume(&session_id).await }
    });
    timeout(Duration::from_secs(2), third_reached)
        .await
        .expect("a third entrant queues on the handed-over entry");
    assert!(!third.is_finished(), "the third entrant waits for the handoff holder");

    drop(second);
    let third_guard = timeout(Duration::from_secs(2), third)
        .await
        .expect("the handed-over claim serves the third entrant")
        .expect("third join");
    assert_eq!(
        close_barrier_probe::cold_resume_claim_ptr(session_id),
        entry_before,
        "a third entrant after the handoff still sees the same entry"
    );
    drop(third_guard);
    assert_eq!(
        close_barrier_probe::cold_resume_claim_entries(),
        0,
        "the last holder removes the entry"
    );
}

#[test]
fn last_waiter_cancelled_after_holder_release_leaves_no_entry() {
    run_with_env_lock(last_waiter_cancelled_after_holder_release_leaves_no_entry_inner());
}

async fn last_waiter_cancelled_after_holder_release_leaves_no_entry_inner() {
    let session_id = "session_cold_claim_cancel_after_release";
    let holder = claim_cold_resume(session_id).await;
    let mut waiter = Box::pin(claim_cold_resume(session_id));
    assert!(
        futures::poll!(waiter.as_mut()).is_pending(),
        "the second attempt queues on the live entry"
    );
    drop(holder);
    // The holder released the mutex but the woken waiter has not been polled
    // again. Dropping it here is the cancelled-after-release case, and the entry
    // itself must retire the key from its own Drop.
    drop(waiter);
    assert_eq!(
        close_barrier_probe::cold_resume_claim_entries(),
        0,
        "an entry whose last reference died has to remove its own registry key"
    );
    let again = claim_cold_resume(session_id).await;
    assert_eq!(close_barrier_probe::cold_resume_claim_entries(), 1);
    drop(again);
    assert_eq!(close_barrier_probe::cold_resume_claim_entries(), 0);
}

#[test]
fn cancelled_waiter_leaves_no_stale_claim() {
    run_with_env_lock(cancelled_waiter_leaves_no_stale_claim_inner());
}

async fn cancelled_waiter_leaves_no_stale_claim_inner() {
    let session_id = "session_cold_claim_cancel";
    let holder = claim_cold_resume(session_id).await;
    let mut queued = Box::pin(claim_cold_resume(session_id));
    assert!(
        futures::poll!(queued.as_mut()).is_pending(),
        "the queued claim must not be granted while the first holder lives"
    );
    drop(queued);
    assert_eq!(close_barrier_probe::cold_resume_claim_entries(), 1);

    drop(holder);
    assert_eq!(
        close_barrier_probe::cold_resume_claim_entries(),
        0,
        "a cancelled waiter must not leave a stale claim behind"
    );
    let again = claim_cold_resume(session_id).await;
    assert_eq!(close_barrier_probe::cold_resume_claim_entries(), 1);
    drop(again);
}

#[test]
fn close_replacement_keeps_the_waiter_blocked() {
    run_with_env_lock(close_replacement_keeps_the_waiter_blocked_inner());
}

async fn close_replacement_keeps_the_waiter_blocked_inner() {
    let session_id = "session_close_replacement_wait";
    let first = SessionCloseGuard::register(session_id).await;
    let first_reached = close_barrier_probe::observe(|| close_barrier_probe::waits(session_id) > 0);
    let waiter = tokio::spawn({
        let session_id = session_id.to_string();
        async move { await_pending_close(&session_id).await }
    });
    timeout(Duration::from_secs(2), first_reached)
        .await
        .expect("the waiter reaches the first close");

    let replacement = SessionCloseGuard::register(session_id).await;
    let replacement_reached = close_barrier_probe::observe(|| close_barrier_probe::waits(session_id) >= 2);
    drop(first);
    timeout(Duration::from_secs(2), replacement_reached)
        .await
        .expect("the waiter restarts on the replacement close instead of a stale clone");
    assert!(!waiter.is_finished(), "a replacement close still holds the waiter");

    drop(replacement);
    timeout(Duration::from_secs(2), waiter)
        .await
        .expect("the waiter is released once no close is pending")
        .expect("waiter join");
    assert_eq!(close_barrier_probe::published_gates(), 0);
}

/// A removal that finds nothing publishes nothing and leaves the rest of the
/// identity cleanup inside the connection region, so it cannot erase a
/// successor's state.
#[test]
fn cleanup_without_a_removal_publishes_no_gate_and_keeps_other_sessions() {
    run_with_env_lock(cleanup_without_a_removal_publishes_no_gate_and_keeps_other_sessions_inner());
}

async fn cleanup_without_a_removal_publishes_no_gate_and_keeps_other_sessions_inner() {
    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => crate::env::set_var("HORON_HOME", value),
                None => crate::env::remove_var("HORON_HOME"),
            }
        }
    }
    let home = tempfile::TempDir::new().expect("isolated home");
    let _home = RestoreHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let connections = Arc::new(RwLock::new(HashMap::from([(
        "conn_other".to_string(),
        connection("conn_other", "session_untouched_owner"),
    )])));
    let members = Arc::new(RwLock::new(HashMap::from([(
        "session_untouched_owner".to_string(),
        SwarmMember {
            session_id: "session_untouched_owner".to_string(),
            event_tx: mpsc::unbounded_channel().0,
            event_txs: HashMap::new(),
            working_dir: None,
            swarm_id: Some("swarm_untouched".to_string()),
            swarm_enabled: false,
            status: "ready".into(),
            detail: None,
            task_label: None,
            friendly_name: None,
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: "agent".into(),
            joined_at: std::time::Instant::now(),
            last_status_change: std::time::Instant::now(),
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: Default::default(),
        },
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        "swarm_untouched".to_string(),
        HashSet::from(["session_untouched_owner".to_string()]),
    )])));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::new()));
    let file_touch = FileTouchService::new();
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::new()));
    let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::new()));
    let (events, _events_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let (swarm_events, _) = broadcast::channel(8);
    let mut processing_task = None;

    cleanup_client_connection(
        &sessions,
        "session_already_gone",
        false,
        &mut processing_task,
        tokio::spawn(std::future::pending()),
        &members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        &file_touch,
        &channel_subscriptions,
        &channel_subscriptions_by_session,
        &connections,
        "conn_gone",
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(std::collections::VecDeque::new())),
        &Arc::new(std::sync::atomic::AtomicU64::new(0)),
        &swarm_events,
        &events,
        Duration::ZERO,
    )
    .await
    .expect("cleanup of an absent session reports no error");

    assert_eq!(
        close_barrier_probe::published_gates(),
        0,
        "nothing was removed, so no close barrier may be published"
    );
    assert!(
        members.read().await.contains_key("session_untouched_owner"),
        "a successor's swarm member must survive an absent-session cleanup"
    );
    assert!(connections.read().await.contains_key("conn_other"));
}
