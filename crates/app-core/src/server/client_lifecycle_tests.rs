#![cfg_attr(test, allow(clippy::await_holding_lock))]
use super::*;
use crate::message::{ContentBlock, Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use async_trait::async_trait;
use futures::stream;
use horon_agent_agent_runtime::SoftInterruptQueue;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct IsolatedRuntimeDir {
    _prev_runtime: Option<std::ffi::OsString>,
    _temp: tempfile::TempDir,
}

struct IsolatedReloadRecoveryEnv {
    prev_home: Option<std::ffi::OsString>,
    prev_runtime: Option<std::ffi::OsString>,
    _home: tempfile::TempDir,
    _runtime: tempfile::TempDir,
}

#[test]
fn cancel_soft_interrupts_by_id_reports_each_requested_id_exactly_once() {
    let queue = SoftInterruptQueue::new();
    let control = SessionControlHandle::cancel_only("session_cancel_by_id_test", queue.clone(), InterruptSignal::new());
    let user = SoftInterruptSource::User;
    for (content, id) in [("a", Some("f1")), ("b", None), ("c", Some("f2")), ("d", Some("f3"))] {
        assert!(control.queue_soft_interrupt_with_id(content.into(), Vec::new(), false, user, id.map(str::to_string),));
    }
    assert_eq!(control.pending_soft_interrupt_count(), 4);

    let requested: Vec<String> = ["f3", "gone", "f1", "f3"].map(String::from).to_vec();
    let (cancelled, not_queued) = control.cancel_soft_interrupts_by_id(&requested);
    assert_eq!(cancelled, vec!["f3".to_string(), "f1".to_string()]);
    assert_eq!(not_queued, vec!["gone".to_string()]);

    let remaining: Vec<String> = queue
        .lock()
        .expect("queue lock")
        .iter()
        .map(|message| message.content.clone())
        .collect();
    assert_eq!(remaining, vec!["b".to_string(), "c".to_string()]);

    // Once drained (injected), the same id can no longer be cancelled.
    queue.lock().expect("queue lock").clear();
    let (cancelled, not_queued) = control.cancel_soft_interrupts_by_id(&["f2".to_string()]);
    assert!(cancelled.is_empty());
    assert_eq!(not_queued, vec!["f2".to_string()]);
}

#[tokio::test]
async fn session_control_handle_does_not_wait_for_busy_agent_lock() {
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));

    let queue = SoftInterruptQueue::new();
    let background_signal = InterruptSignal::new();
    let stop_signal = InterruptSignal::new();
    let control = SessionControlHandle::new(
        "session_control_test",
        queue.clone(),
        background_signal.clone(),
        stop_signal.clone(),
    );

    let _busy_agent_lock = agent.lock().await;

    tokio::time::timeout(Duration::from_millis(100), async {
        assert!(control.queue_soft_interrupt("please stop".to_string(), Vec::new(), true, SoftInterruptSource::User,));
        control.request_cancel();
        assert!(control.request_background_current_tool());
        control.clear_soft_interrupts();
    })
    .await
    .expect("lock-free control operations should not wait for the agent mutex");

    assert!(stop_signal.is_set());
    assert!(background_signal.is_set());
    assert!(queue.lock().expect("queue lock").is_empty());
}

#[tokio::test]
async fn refreshed_session_control_handle_does_not_wait_for_busy_agent_lock() {
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session = crate::session::Session::create_with_id("session_busy_control_refresh".to_string(), None, None);
    session.model = Some("panic-on-fork".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)));

    let stop_signal = InterruptSignal::new();
    let soft_interrupt_queue = SoftInterruptQueue::new();
    let shutdown_signals = Arc::new(RwLock::new(HashMap::from([(
        "session_busy_control_refresh".to_string(),
        stop_signal.clone(),
    )])));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::from([(
        "session_busy_control_refresh".to_string(),
        soft_interrupt_queue,
    )])));

    let _busy_agent_lock = agent.lock().await;

    tokio::time::timeout(Duration::from_millis(100), async {
        let control = refresh_session_control_handle(
            "session_busy_control_refresh",
            &agent,
            &shutdown_signals,
            &soft_interrupt_queues,
        )
        .await;
        control.request_cancel();
    })
    .await
    .expect("refreshing a session control handle must not wait for the busy agent mutex");

    assert!(stop_signal.is_set());
}

#[tokio::test]
async fn busy_session_background_tool_signal_fires_via_registry_fallback() {
    // Regression: pressing Alt+B/Ctrl+B while a turn owns the agent mutex (e.g.
    // running `await_members`) used to silently no-op because the lock-free
    // `cancel_only` control handle dropped the background-tool signal
    // (BACKGROUND_TOOL_SIGNAL_FIRE result=no_signal_handle). Building a full
    // SessionControlHandle now registers the signal in a process-global registry
    // so the cancel-only fallback can still fire it without the agent lock.
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let session_id = "session_busy_background_signal_registry";
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some("panic-on-fork".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)));

    let background_signal = {
        let agent_guard = agent.lock().await;
        agent_guard.background_tool_signal()
    };

    // Build a full control handle once (registers the background signal), then
    // simulate the busy-turn reconnect path which yields a cancel-only handle.
    let stop_signal = InterruptSignal::new();
    let soft_interrupt_queue = SoftInterruptQueue::new();
    let _full = SessionControlHandle::new(
        session_id,
        soft_interrupt_queue.clone(),
        background_signal.clone(),
        stop_signal.clone(),
    );

    let cancel_only = SessionControlHandle::cancel_only(session_id, soft_interrupt_queue, stop_signal);

    // The cancel-only handle has no directly-held background signal, yet it must
    // still fire the registered one.
    assert!(cancel_only.request_background_current_tool());
    assert!(background_signal.is_set());

    // Cleanup so the global registry does not leak across tests.
    crate::server::state::remove_background_tool_signal(session_id);
}

#[tokio::test]
async fn busy_agent_request_rejection_does_not_wait_for_agent_lock() {
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();

    let busy_agent_lock = agent.lock().await;
    let rejected = tokio::time::timeout(Duration::from_millis(100), async {
        reject_if_agent_busy_for_request(
            17,
            "rename_session",
            "session_busy_reject",
            true,
            &agent,
            &client_event_tx,
        )
        .await
    })
    .await
    .expect("busy-agent request rejection must not wait for the agent mutex");
    assert!(rejected);
    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Error {
            id: 17,
            retry_after_secs: Some(1),
            ..
        })
    ));

    drop(busy_agent_lock);
    assert!(
        !reject_if_agent_busy_for_request(
            18,
            "rename_session",
            "session_busy_reject",
            false,
            &agent,
            &client_event_tx,
        )
        .await
    );
    assert!(client_event_rx.try_recv().is_err());
}

async fn assert_busy_error(
    client_event_rx: &mut mpsc::UnboundedReceiver<ServerEvent>,
    expected_id: u64,
    request_kind: &str,
) {
    match tokio::time::timeout(Duration::from_secs(5), client_event_rx.recv())
        .await
        .expect("the busy decision must answer")
    {
        Some(ServerEvent::Error {
            id,
            message,
            retry_after_secs: Some(1),
        }) => {
            assert_eq!(id, expected_id);
            assert_eq!(
                message,
                format!(
                    "Cannot handle {request_kind} while the session is busy. Try again after the current turn finishes."
                )
            );
        }
        other => panic!("expected the busy error for request {expected_id}, got {other:?}"),
    }
}

async fn archive_agent(session_id: &str, provider: Arc<dyn Provider>, persist: bool) -> Arc<Mutex<Agent>> {
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some("panic-on-fork".to_string());
    if persist {
        // Archiving records an event at the session's current head, so the
        // session needs a durable event graph first. A plain save skips an
        // unchanged fresh session, so force the root event. Callers keep the
        // isolated HORON_HOME that this write lands in.
        session
            .save_prepared()
            .expect("persist the session before archiving it");
    }
    Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)))
}

/// A resume spawns the model prefetch, which holds the agent lock for a moment
/// while no turn runs. ACP `session/delete` sends `ArchiveSession` right after
/// the subscribe `Done`, so the archive used to answer "Cannot handle ...
/// busy" for that transient hold. The busy decision must read turn state, not
/// the lock.
#[tokio::test(start_paused = true)]
async fn archive_request_completes_while_background_work_holds_the_agent_lock() {
    let _guard = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_archive_background_lock";
    let agent = archive_agent(
        session_id,
        Arc::new(PanicOnForkProvider {
            forked: Arc::new(AtomicBool::new(false)),
        }),
        true,
    )
    .await;
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();

    // Prefetch-like holder: takes the lock, reports that it holds it, releases
    // after a bounded pause. Paused time fixes the ordering - ready tasks run
    // before the clock advances - so the dispatch below reaches its busy check
    // while the lock is still held.
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let holder_agent = Arc::clone(&agent);
    let holder = tokio::spawn(async move {
        let guard = holder_agent.lock().await;
        let _ = held_tx.send(());
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(guard);
    });
    held_rx.await.expect("prefetch-like holder should take the agent lock");

    let dispatch_agent = Arc::clone(&agent);
    let dispatch_tx = client_event_tx.clone();
    let dispatch = tokio::spawn(async move {
        // Exactly what the `Request::ArchiveSession` arm does.
        if reject_if_agent_busy_for_request(41, "archive_session", session_id, false, &dispatch_agent, &dispatch_tx)
            .await
        {
            return;
        }
        handle_archive_session(41, true, None, &dispatch_agent, session_id, &dispatch_tx).await;
    });

    let event = tokio::time::timeout(Duration::from_secs(5), client_event_rx.recv())
        .await
        .expect("archive must answer while background work holds the lock")
        .expect("archive must emit a response event");
    assert!(
        matches!(event, ServerEvent::Done { id: 41 }),
        "expected Done for the archive request, got {event:?}"
    );
    assert!(
        client_event_rx.try_recv().is_err(),
        "the archive request must answer exactly once"
    );

    dispatch.await.expect("dispatch task");
    holder.await.expect("holder task");
}

/// A running turn still gets the busy answer, and it gets it without waiting
/// for the agent lock: the lock stays free here, so only turn state can decide.
#[tokio::test]
async fn archive_request_is_rejected_while_a_turn_is_active() {
    let _guard = crate::storage::lock_test_env();
    let _env = IsolatedRuntimeDir::new();
    let session_id = "session_archive_active_turn";
    let agent = archive_agent(
        session_id,
        Arc::new(PanicOnForkProvider {
            forked: Arc::new(AtomicBool::new(false)),
        }),
        false,
    )
    .await;
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();

    // This connection is streaming a turn.
    assert!(reject_if_agent_busy_for_request(42, "archive_session", session_id, true, &agent, &client_event_tx,).await);
    assert_busy_error(&mut client_event_rx, 42, "archive_session").await;

    // A turn registered outside this connection (server-initiated or resumed).
    let active_turn = crate::turn_cancel_registry::register_active_turn(session_id, InterruptSignal::new());
    assert!(
        reject_if_agent_busy_for_request(43, "archive_session", session_id, false, &agent, &client_event_tx,).await
    );
    assert_busy_error(&mut client_event_rx, 43, "archive_session").await;
    drop(active_turn);

    // Nothing is running any more: the same request proceeds.
    assert!(
        !reject_if_agent_busy_for_request(44, "archive_session", session_id, false, &agent, &client_event_tx,).await
    );
    assert!(client_event_rx.try_recv().is_err());
}

/// A lock that outlives the bound may be guarding a turn that has not registered
/// yet, or one that already unregistered. The request must get the busy answer
/// instead of parking on the mutex, because parking stalls the sequential request
/// loop - which is also how Cancel is read.
#[tokio::test(start_paused = true)]
async fn archive_request_is_rejected_when_the_agent_lock_outlives_the_wait() {
    let _guard = crate::storage::lock_test_env();
    let _env = IsolatedRuntimeDir::new();
    let session_id = "session_archive_lock_outlives_wait";
    let agent = archive_agent(
        session_id,
        Arc::new(PanicOnForkProvider {
            forked: Arc::new(AtomicBool::new(false)),
        }),
        false,
    )
    .await;
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();

    // No turn is registered, and the holder keeps the lock for the whole
    // request, so only the bound can decide the answer.
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let holder_agent = Arc::clone(&agent);
    let holder = tokio::spawn(async move {
        let _held = holder_agent.lock().await;
        let _ = held_tx.send(());
        std::future::pending::<()>().await;
    });
    held_rx.await.expect("holder should take the agent lock");

    let dispatch_agent = Arc::clone(&agent);
    let dispatch_tx = client_event_tx.clone();
    let dispatch = tokio::spawn(async move {
        if reject_if_agent_busy_for_request(45, "archive_session", session_id, false, &dispatch_agent, &dispatch_tx)
            .await
        {
            return;
        }
        handle_archive_session(45, true, None, &dispatch_agent, session_id, &dispatch_tx).await;
    });

    assert_busy_error(&mut client_event_rx, 45, "archive_session").await;
    dispatch
        .await
        .expect("the refusal must not park the request on the agent mutex");
    assert!(
        client_event_rx.try_recv().is_err(),
        "the refused archive must not also answer Done"
    );
    holder.abort();
}

#[tokio::test]
async fn context_message_persists_without_starting_turn() {
    let _guard = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_context_only_no_reply";
    let forked = Arc::new(AtomicBool::new(false));
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::clone(&forked),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some("panic-on-fork".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let before = agent.lock().await.message_count();

    append_context_message(
        77,
        "remember this context",
        vec![("image/png".to_string(), "AAA".to_string())],
        session_id,
        false,
        &agent,
        &client_event_tx,
    )
    .await;

    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::ContextMessageAdded { id: 77 })
    ));
    assert!(client_event_rx.try_recv().is_err());
    assert!(!forked.load(Ordering::SeqCst));

    let persisted = crate::session::Session::load(session_id).expect("persisted session");
    assert_eq!(persisted.active_context().len(), before + 1);
    let message = persisted.active_context().last().unwrap();
    assert_eq!(format!("{:?}", message.role), "User");
    assert!(matches!(
        &message.content[0],
        ContentBlock::Image { media_type, data }
            if media_type == "image/png" && data == "AAA"
    ));
    assert!(matches!(
        &message.content[1],
        ContentBlock::Text { text, .. } if text == "remember this context"
    ));
}

#[tokio::test]
async fn context_message_rejects_while_busy_without_waiting_for_agent_lock() {
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let _busy_agent_lock = agent.lock().await;

    tokio::time::timeout(Duration::from_millis(100), async {
        append_context_message(
            78,
            "too busy",
            Vec::new(),
            "session_context_busy",
            true,
            &agent,
            &client_event_tx,
        )
        .await;
    })
    .await
    .expect("busy rejection must not wait for the agent mutex");

    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Error {
            id: 78,
            retry_after_secs: Some(1),
            ..
        })
    ));
}

#[tokio::test]
async fn cancel_without_local_task_still_signals_session_control() {
    let soft_interrupt_queue = SoftInterruptQueue::new();
    let stop_signal = InterruptSignal::new();
    let control =
        SessionControlHandle::cancel_only("session_detached_cancel", soft_interrupt_queue, stop_signal.clone());
    // The point of this path is a turn this connection does not own (attach
    // after reload, server-initiated turn). Without a registered active turn
    // the cancel is a deliberate no-op, because arming the signal with nothing
    // running only kills the *next* message.
    let _active_turn =
        crate::turn_cancel_registry::register_active_turn("session_detached_cancel", InterruptSignal::new());
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let mut client_is_processing = true;
    let mut message_id = Some(99);
    let mut session_id = Some("session_detached_cancel".to_string());
    let mut task = None;

    cancel_processing_message(
        &mut ProcessingState {
            client_is_processing: &mut client_is_processing,
            message_id: &mut message_id,
            session_id: &mut session_id,
            task: &mut task,
            turn_acquired: &Arc::new(AtomicBool::new(false)),
        },
        &control,
        &client_event_tx,
        &SwarmStatusRefs {
            members: &swarm_members,
            swarms_by_id: &swarms_by_id,
            event_history: &event_history,
            event_counter: &event_counter,
            event_tx: &swarm_event_tx,
        },
        Some(99),
        None,
        None,
    )
    .await;

    assert!(stop_signal.is_set());
    assert!(!client_is_processing);
    assert!(message_id.is_none());
    assert!(session_id.is_none());
    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::TurnStopped {
            reason: crate::protocol::TurnStopReason::Interrupted,
            ..
        })
    ));
    assert!(matches!(client_event_rx.recv().await, Some(ServerEvent::Interrupted)));
    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Done { id: 99 })
    ));
}

/// Regression for issue #428: the detached-turn cancel path schedules a
/// deferred reset of the shared stop signal. That reset must be epoch-guarded:
/// if a newer cancel fires during the reset window (rapid repeated Esc), the
/// stale timer must not clear it, otherwise the running turn never observes
/// the interrupt and keeps generating.
#[tokio::test]
async fn deferred_cancel_reset_does_not_erase_newer_cancel() {
    let soft_interrupt_queue = SoftInterruptQueue::new();
    let stop_signal = InterruptSignal::new();
    let control = SessionControlHandle::cancel_only(
        "session_detached_cancel_race",
        soft_interrupt_queue.clone(),
        stop_signal.clone(),
    );
    // A turn owned by another connection is what makes this the signalling
    // path rather than the idle no-op; see the sibling test.
    let _active_turn =
        crate::turn_cancel_registry::register_active_turn("session_detached_cancel_race", InterruptSignal::new());
    let (client_event_tx, _client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);

    let cancel_via_no_task_path = async |request_id: u64| {
        let mut client_is_processing = true;
        let mut message_id = Some(request_id);
        let mut session_id = Some("session_detached_cancel_race".to_string());
        let mut task = None;
        cancel_processing_message(
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut session_id,
                task: &mut task,
                turn_acquired: &Arc::new(AtomicBool::new(false)),
            },
            &control,
            &client_event_tx,
            &SwarmStatusRefs {
                members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                event_history: &event_history,
                event_counter: &event_counter,
                event_tx: &swarm_event_tx,
            },
            Some(request_id),
            None,
            None,
        )
        .await;
    };

    // First Esc: fires the signal and schedules a 500ms deferred reset.
    cancel_via_no_task_path(1).await;
    assert!(stop_signal.is_set());

    // 400ms later the user presses Esc again (turn still hasn't stopped).
    tokio::time::sleep(Duration::from_millis(400)).await;
    cancel_via_no_task_path(2).await;
    assert!(stop_signal.is_set());

    // The first press's timer expires now. It must NOT clear the second
    // press's still-unobserved cancel.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        stop_signal.is_set(),
        "stale deferred reset erased a newer cancel (issue #428)"
    );

    // The second press's own timer may still clear it afterwards.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !stop_signal.is_set(),
        "the newest cancel's deferred reset should eventually clear the flag"
    );
}

impl IsolatedRuntimeDir {
    fn new() -> Self {
        let temp = tempfile::TempDir::new().expect("runtime dir");
        let prev_runtime = std::env::var_os("HORON_AGENT_RUNTIME_DIR");
        crate::env::set_var("HORON_AGENT_RUNTIME_DIR", temp.path());
        crate::server::clear_reload_marker();
        Self {
            _prev_runtime: prev_runtime,
            _temp: temp,
        }
    }
}

impl IsolatedReloadRecoveryEnv {
    fn new() -> Self {
        let home = tempfile::TempDir::new().expect("horon-agent home");
        let runtime = tempfile::TempDir::new().expect("runtime dir");
        let prev_home = std::env::var_os("HORON_HOME");
        let prev_runtime = std::env::var_os("HORON_AGENT_RUNTIME_DIR");
        crate::env::set_var("HORON_HOME", home.path());
        crate::env::set_var("HORON_AGENT_RUNTIME_DIR", runtime.path());
        crate::server::clear_reload_marker();
        Self {
            prev_home,
            prev_runtime,
            _home: home,
            _runtime: runtime,
        }
    }
}

impl Drop for IsolatedReloadRecoveryEnv {
    fn drop(&mut self) {
        crate::server::clear_reload_marker();
        if let Some(prev_home) = self.prev_home.take() {
            crate::env::set_var("HORON_HOME", prev_home);
        } else {
            crate::env::remove_var("HORON_HOME");
        }
        if let Some(prev_runtime) = self.prev_runtime.take() {
            crate::env::set_var("HORON_AGENT_RUNTIME_DIR", prev_runtime);
        } else {
            crate::env::remove_var("HORON_AGENT_RUNTIME_DIR");
        }
    }
}

impl Drop for IsolatedRuntimeDir {
    fn drop(&mut self) {
        crate::server::clear_reload_marker();
        if let Some(prev_runtime) = self._prev_runtime.take() {
            crate::env::set_var("HORON_AGENT_RUNTIME_DIR", prev_runtime);
        } else {
            crate::env::remove_var("HORON_AGENT_RUNTIME_DIR");
        }
    }
}

/// Regression for issue #428: a turn actively streaming in this session but
/// NOT owned by the cancelling connection (no local task handle: post-reload
/// reattach, server-initiated wake turns, headless recovery) must abort
/// promptly even when the control handle's stop signal is a *different
/// instance* from the streaming agent's own `graceful_shutdown` signal.
///
/// Before the fix, `cancel_processing_message` hit the NO_LOCAL_TASK branch,
/// fired the stale handle-local signal (which nothing was listening to),
/// emitted `Interrupted` immediately, and the provider stream kept generating
/// for minutes ("Interrupting..." disappears, model keeps going, eventually
/// "Interrupted [x66]").
#[test]
fn cancel_aborts_detached_streaming_turn_with_stale_stop_signal() -> anyhow::Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_detached_streaming_cancel_428";

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(NeverEndingStreamProvider);
        let registry = Registry::new(Arc::clone(&provider)).await;
        let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
        session.model = Some("never-ending-stream".to_string());
        let agent = Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)));

        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<ServerEvent>();

        // Start the turn the way server-initiated paths do: no entry in any
        // connection's processing-task map.
        let turn_agent = Arc::clone(&agent);
        let turn = tokio::spawn(async move {
            process_message_streaming_mpsc(turn_agent, "stream forever", Vec::new(), None, event_tx).await
        });

        // Wait until the provider stream is actively producing output.
        loop {
            match tokio::time::timeout(Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(ServerEvent::TextDelta { .. })) => break,
                Ok(Some(_)) => continue,
                Ok(None) => panic!("event channel closed before streaming started"),
                Err(_) => panic!("turn never started streaming"),
            }
        }

        // Esc arrives on a connection that does not own the task. Its control
        // handle holds a stop signal instance that is NOT the streaming
        // agent's graceful_shutdown signal (stale/lost registration).
        let stale_stop_signal = InterruptSignal::new();
        let control =
            SessionControlHandle::cancel_only(session_id, SoftInterruptQueue::new(), stale_stop_signal.clone());
        let (client_event_tx, _client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
        let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
        let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (swarm_event_tx, _) = broadcast::channel(8);
        let mut client_is_processing = false;
        let mut message_id = None;
        let mut cancel_session_id = None;
        let mut task = None;

        cancel_processing_message(
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut cancel_session_id,
                task: &mut task,
                turn_acquired: &Arc::new(AtomicBool::new(false)),
            },
            &control,
            &client_event_tx,
            &SwarmStatusRefs {
                members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                event_history: &event_history,
                event_counter: &event_counter,
                event_tx: &swarm_event_tx,
            },
            Some(1),
            None,
            None,
        )
        .await;

        // The turn now keeps the agent until its idle state was forwarded; this
        // test plays the session fanout's part by reading that state off the raw
        // channel and reporting it forwarded.
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
                .await
                .expect("the cancelled turn reports its idle state")
                .expect("the event channel stays open");
            if let ServerEvent::ForegroundState {
                running: false, run_id, ..
            } = event
            {
                crate::server::foreground_state::note_forwarded(&run_id);
                break;
            }
        }

        // The streaming turn must observe the cancel and stop promptly, not
        // minutes later when the provider happens to finish (issue #428).
        let result = tokio::time::timeout(Duration::from_secs(2), turn)
            .await
            .expect("streaming turn must abort promptly after cancel (issue #428)")
            .expect("turn task join");
        result.expect("cancelled turn should checkpoint cleanly");

        // The turn is over, so its cancel registration must be gone and the
        // agent's own signal must be reset so the *next* turn is not aborted
        // by the consumed cancel.
        assert!(
            crate::turn_cancel_registry::active_turn_signals(session_id).is_empty(),
            "finished turn must unregister its cancel signal"
        );
        let agent_signal = {
            let agent_guard = agent.lock().await;
            agent_guard.graceful_shutdown_signal()
        };
        assert!(
            !agent_signal.is_set(),
            "consumed cancel must not leak into the next turn"
        );
    });
    Ok(())
}

/// A streaming turn (`process_message_streaming_mpsc`: jade relay, comm
/// sessions, reload continuations) keeps the agent until its idle state was
/// forwarded. Releasing it with the turn let the next turn's `running` precede
/// that idle state, and a cancel-held idle lost its `cancelled` reason.
#[test]
fn streaming_turn_holds_the_agent_until_its_idle_is_forwarded() -> anyhow::Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_streaming_turn_holds_agent";

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
        let registry = Registry::new(Arc::clone(&provider)).await;
        let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
        session.model = Some("complete-immediately".to_string());
        let agent = Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)));

        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let turn_agent = Arc::clone(&agent);
        let turn = tokio::spawn(async move {
            process_message_streaming_mpsc(turn_agent, "quick", Vec::new(), None, event_tx).await
        });

        // The idle state is reportable as soon as the turn is over; the turn
        // itself must not return (nor release the agent) before it is forwarded.
        let run_id = loop {
            let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
                .await
                .expect("the turn reports its idle state")
                .expect("the event channel stays open");
            if let ServerEvent::ForegroundState {
                running: false, run_id, ..
            } = event
            {
                break run_id;
            }
        };
        assert!(
            agent.try_lock().is_err(),
            "the turn keeps the agent until its idle state is forwarded"
        );
        crate::server::foreground_state::note_forwarded(&run_id);
        tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .expect("the turn returns once its idle state was forwarded")
            .expect("the turn task does not panic")?;
        assert!(agent.try_lock().is_ok(), "the turn releases the agent");
        Ok(())
    })
}

/// A cancel that arrives while the session is idle must not arm the cancel
/// signal at all.
///
/// The no-local-task branch cannot tell an idle session from one whose turn
/// another connection owns, so it used to fire the signal and clear it on a
/// 500ms timer. Any message sent inside that window began with the flag
/// already set and was aborted the instant it started: no reply, no error,
/// just a message that vanished. Pressing Esc on an idle prompt and typing
/// immediately is an ordinary thing to do, so this must be a true no-op.
#[test]
fn idle_cancel_does_not_arm_the_signal_for_the_next_turn() -> anyhow::Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_idle_cancel_noop";

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let stop_signal = InterruptSignal::new();
        let control = SessionControlHandle::cancel_only(session_id, SoftInterruptQueue::new(), stop_signal.clone());
        let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
        let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
        let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (swarm_event_tx, _) = broadcast::channel(8);
        let mut client_is_processing = false;
        let mut message_id = None;
        let mut cancel_session_id = None;
        let mut task = None;

        assert!(
            !crate::turn_cancel_registry::has_active_turn(session_id),
            "test precondition: the session must be idle"
        );

        cancel_processing_message(
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut cancel_session_id,
                task: &mut task,
                turn_acquired: &Arc::new(AtomicBool::new(false)),
            },
            &control,
            &client_event_tx,
            &SwarmStatusRefs {
                members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                event_history: &event_history,
                event_counter: &event_counter,
                event_tx: &swarm_event_tx,
            },
            Some(1),
            None,
            None,
        )
        .await;

        assert!(
            !stop_signal.is_set(),
            "an idle cancel must not arm the stop signal; the next turn would die instantly"
        );
        // The client still learns the cancel was handled, so a UI showing
        // "Interrupting..." resolves rather than hanging.
        match client_event_rx.try_recv() {
            Ok(ServerEvent::Interrupted) => {}
            other => panic!("idle cancel must still report Interrupted, got {other:?}"),
        }
    });
    Ok(())
}

struct PanicOnForkProvider {
    forked: Arc<AtomicBool>,
}

/// Streams text deltas forever (one every 20ms) until dropped. Stands in for
/// a live provider stream that only stops when the turn observes a cancel.
struct NeverEndingStreamProvider;

#[async_trait]
impl Provider for NeverEndingStreamProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(stream::unfold(0u64, |n| async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Some((Ok(StreamEvent::TextDelta(format!("token{} ", n))), n + 1))
        })))
    }

    fn name(&self) -> &str {
        "never-ending-stream"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

#[derive(Clone, Default)]
struct CompleteImmediatelyProvider;

#[async_trait]
impl Provider for CompleteImmediatelyProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(stream::iter(vec![Ok(StreamEvent::MessageEnd {
            stop_reason: None,
        })])))
    }

    fn name(&self) -> &str {
        "complete-immediately"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

#[derive(Clone, Default)]
struct FanoutStreamProvider;

#[async_trait]
impl Provider for FanoutStreamProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(stream::unfold(0_u8, |step| async move {
            match step {
                0 => Some((Ok(StreamEvent::TextDelta("before attach".to_string())), 1)),
                1 => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Some((Ok(StreamEvent::TextDelta("after attach".to_string())), 2))
                }
                2 => Some((
                    Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    }),
                    3,
                )),
                _ => None,
            }
        })))
    }

    fn name(&self) -> &str {
        "fanout-stream"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

#[async_trait]
impl Provider for PanicOnForkProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        panic!("complete should never run in lightweight control test")
    }

    fn name(&self) -> &str {
        "panic-on-fork"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        self.forked.store(true, Ordering::SeqCst);
        panic!("fork should not run for lightweight control requests")
    }
}

#[test]
fn ping_request_is_lightweight_control_request() {
    assert!((Request::Ping { id: 1 }).is_lightweight_control_request());
}

fn subscribe_request(working_dir: Option<&str>) -> Request {
    Request::Subscribe {
        system_prompt: None,
        supports_user_response: false,
        supports_ask_requests: false,
        id: 1,
        working_dir: working_dir.map(str::to_string),
        target_session_id: None,
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
        acp: None,
    }
}

#[test]
fn initial_subscribe_requires_an_absolute_client_working_dir() {
    for invalid in [None, Some(""), Some("relative/project")] {
        let error = initial_subscribe_working_dir(&subscribe_request(invalid))
            .expect_err("invalid client cwd must be rejected before session creation");
        assert!(error.contains("working_dir") || error.contains("working directory"));
    }

    let absolute = std::env::temp_dir().join("horon-agent-client-project");
    assert_eq!(
        initial_subscribe_working_dir(&subscribe_request(absolute.to_str())).expect("absolute client cwd"),
        absolute.to_string_lossy()
    );

    let error = initial_subscribe_working_dir(&Request::GetState { id: 2 })
        .expect_err("stateful requests must not create an unbound session");
    assert!(error.contains("must Subscribe"));
}

#[test]
fn remote_subscribe_requires_an_existing_server_directory() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("not-a-directory");
    std::fs::write(&file, "file")?;
    let missing = directory.path().join("missing");
    for path in [&file, &missing] {
        let mut request = subscribe_request(path.to_str());
        assert!(
            initial_subscribe_working_dir(&request).is_ok(),
            "local subscription behavior remains unchanged"
        );
        if let Request::Subscribe {
            continue_on_disconnect, ..
        } = &mut request
        {
            *continue_on_disconnect = true;
        }
        assert!(
            initial_subscribe_working_dir(&request)
                .unwrap_err()
                .contains("must exist and be a directory on the server")
        );
    }
    assert_eq!(
        validated_subscribe_working_dir(directory.path().to_str(), true).expect("existing directory"),
        directory.path().to_str().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn new_client_agent_stamps_client_cwd_into_initial_context() {
    let provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let client_cwd = std::env::temp_dir().join("horon-agent-authoritative-client-project");
    let client_cwd = client_cwd.to_string_lossy();
    let agent = Agent::new_with_initial_working_dir(provider, registry, Some(&client_cwd));

    assert_eq!(agent.working_dir(), Some(client_cwd.as_ref()));
    let context = agent.messages()[0].content_preview();
    assert!(
        context.contains(&format!("Working directory: {client_cwd}")),
        "initial context must be created from the client cwd: {context}"
    );
}

#[test]
fn server_reload_starting_is_true_only_for_recent_starting_marker() {
    let _guard = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();

    assert!(!server_reload_starting());

    crate::server::write_reload_state(
        "reload-lifecycle-test",
        "test-hash",
        crate::server::ReloadPhase::Starting,
        Some("session_test_reload".to_string()),
    );
    assert!(server_reload_starting());

    crate::server::write_reload_state(
        "reload-lifecycle-test",
        "test-hash",
        crate::server::ReloadPhase::SocketReady,
        Some("session_test_reload".to_string()),
    );
    assert!(!server_reload_starting());
}

#[test]
fn reload_starting_rejects_new_turn_without_spawning_processing_task() {
    let _guard = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    crate::server::write_reload_state(
        "reload-lifecycle-starting",
        "test-hash",
        crate::server::ReloadPhase::Starting,
        Some("session_guard".to_string()),
    );

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let forked = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
            forked: Arc::clone(&forked),
        });
        let registry = Registry::new(Arc::clone(&provider)).await;
        let mut session = crate::session::Session::create_with_id("session_guard".to_string(), None, None);
        session.model = Some("panic-on-fork".to_string());
        let agent = Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)));

        let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let (processing_done_tx, mut processing_done_rx) = mpsc::unbounded_channel();
        let mut client_is_processing = false;
        let mut processing_message_id = None;
        let mut processing_session_id = None;
        let mut processing_task = None;
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
        let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
        let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (swarm_event_tx, _) = broadcast::channel(8);

        start_processing_message(
            ProcessingMessage {
                id: 42,
                content: "do not start during reload".to_string(),
                images: Vec::new(),
                system_reminder: None,
                active_skill: None,
                v2: None,
            },
            "session_guard",
            None,
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut processing_message_id,
                session_id: &mut processing_session_id,
                task: &mut processing_task,
                turn_acquired: &Arc::new(AtomicBool::new(false)),
            },
            &agent,
            &client_event_tx,
            &processing_done_tx,
            Vec::new(),
            &SwarmStatusRefs {
                members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                event_history: &event_history,
                event_counter: &event_counter,
                event_tx: &swarm_event_tx,
            },
        )
        .await;

        let event = client_event_rx
            .recv()
            .await
            .expect("reload event should be sent to client");
        assert!(matches!(event, ServerEvent::Reloading { new_socket: None }));
        assert!(
            client_event_rx.try_recv().is_err(),
            "reload guard should only emit the reload notification"
        );
        assert!(!client_is_processing);
        assert_eq!(processing_message_id, None);
        assert_eq!(processing_session_id, None);
        assert!(processing_task.is_none());
        assert!(processing_done_rx.try_recv().is_err());
        assert!(
            !forked.load(Ordering::SeqCst),
            "rejecting during reload should not fork or invoke provider work"
        );
    });
}

#[tokio::test]
async fn client_initiated_turn_fans_out_stream_and_terminal_events_to_live_attachments() {
    let _guard = crate::storage::lock_test_env();
    // The turn persists its session through the event store, so the agent home
    // must be isolated too; a shared runtime dir alone leaves it in the ambient home.
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_live_attachment_fanout";

    let provider: Arc<dyn Provider> = Arc::new(FanoutStreamProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some("fanout-stream".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)));

    let (origin_tx, mut origin_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let (attached_tx, mut attached_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        session_id.to_string(),
        SwarmMember {
            session_id: session_id.to_string(),
            event_tx: origin_tx.clone(),
            event_txs: HashMap::from([("origin".to_string(), origin_tx.clone())]),
            working_dir: None,
            swarm_id: None,
            swarm_enabled: false,
            status: "ready".to_string(),
            detail: None,
            task_label: None,
            friendly_name: None,
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: "agent".to_string(),
            joined_at: Instant::now(),
            last_status_change: Instant::now(),
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
        },
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let (processing_done_tx, mut processing_done_rx) = mpsc::unbounded_channel();
    let mut client_is_processing = false;
    let mut processing_message_id = None;
    let mut processing_session_id = None;
    let mut processing_task = None;

    start_processing_message(
        ProcessingMessage {
            id: 479,
            content: "stream to every attachment".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            v2: None,
        },
        session_id,
        None,
        &mut ProcessingState {
            client_is_processing: &mut client_is_processing,
            message_id: &mut processing_message_id,
            session_id: &mut processing_session_id,
            task: &mut processing_task,
            turn_acquired: &Arc::new(AtomicBool::new(false)),
        },
        &agent,
        &origin_tx,
        &processing_done_tx,
        Vec::new(),
        &SwarmStatusRefs {
            members: &swarm_members,
            swarms_by_id: &swarms_by_id,
            event_history: &event_history,
            event_counter: &event_counter,
            event_tx: &swarm_event_tx,
        },
    )
    .await;

    loop {
        let event = tokio::time::timeout(Duration::from_secs(2), origin_rx.recv())
            .await
            .expect("origin should receive the initial stream event promptly")
            .expect("origin event channel should remain open");
        if matches!(event, ServerEvent::TextDelta { ref text } if text == "before attach") {
            break;
        }
    }

    crate::server::register_session_event_sender(&swarm_members, session_id, "attached", attached_tx).await;

    for rx in [&mut origin_rx, &mut attached_rx] {
        let mut saw_post_attach_delta = false;
        let mut saw_message_end = false;
        let mut saw_done = false;
        while !saw_post_attach_delta || !saw_message_end || !saw_done {
            let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("attachment should receive streamed event promptly")
                .expect("attachment event channel should remain open");
            saw_post_attach_delta |= matches!(
                event,
                ServerEvent::TextDelta { ref text } if text == "after attach"
            );
            if matches!(event, ServerEvent::MessageEnd { .. }) {
                assert!(!saw_done, "MessageEnd must precede the terminal Done event");
                saw_message_end = true;
            }
            saw_done |= matches!(event, ServerEvent::Done { id: 479 });
        }
    }

    let (done_id, result, _) = tokio::time::timeout(Duration::from_secs(2), processing_done_rx.recv())
        .await
        .expect("processing should complete promptly")
        .expect("processing completion channel should remain open");
    assert_eq!(done_id, 479);
    result.expect("turn should complete successfully");

    if let Some(handle) = processing_task.take() {
        handle.await.expect("processing task join");
    }
}

#[test]
fn accepted_reload_recovery_continuation_marks_intent_delivered() -> anyhow::Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_accepted_reload_recovery";
    let continuation = "stored continuation accepted by server";

    super::super::reload_recovery::persist_intent(
        "reload-accepted-continuation",
        session_id,
        super::super::reload_recovery::ReloadRecoveryRole::InterruptedPeer,
        crate::protocol::ReloadRecoveryDirective {
            reconnect_notice: Some("stored notice".to_string()),
            continuation_message: continuation.to_string(),
        },
        "synthetic accepted continuation test",
    )?;
    assert!(super::super::reload_recovery::has_pending_for_session(session_id));

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
        let registry = Registry::new(Arc::clone(&provider)).await;
        let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
        session.model = Some("complete-immediately".to_string());
        let agent = Arc::new(Mutex::new(Agent::new_with_session(provider, registry, session, None)));

        let (client_event_tx, _client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let (processing_done_tx, mut processing_done_rx) = mpsc::unbounded_channel();
        let mut client_is_processing = false;
        let mut processing_message_id = None;
        let mut processing_session_id = None;
        let mut processing_task = None;
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
        let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
        let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (swarm_event_tx, _) = broadcast::channel(8);

        start_processing_message(
            ProcessingMessage {
                id: 77,
                content: "continue after reload".to_string(),
                images: Vec::new(),
                system_reminder: Some(continuation.to_string()),
                active_skill: None,
                v2: None,
            },
            session_id,
            None,
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut processing_message_id,
                session_id: &mut processing_session_id,
                task: &mut processing_task,
                turn_acquired: &Arc::new(AtomicBool::new(false)),
            },
            &agent,
            &client_event_tx,
            &processing_done_tx,
            Vec::new(),
            &SwarmStatusRefs {
                members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                event_history: &event_history,
                event_counter: &event_counter,
                event_tx: &swarm_event_tx,
            },
        )
        .await;

        assert!(client_is_processing);
        assert_eq!(processing_message_id, Some(77));
        assert_eq!(processing_session_id.as_deref(), Some(session_id));
        assert!(processing_task.is_some());
        assert!(
            !super::super::reload_recovery::has_pending_for_session(session_id),
            "server acceptance of the exact hidden continuation should consume the durable intent"
        );

        let (done_id, result, _report) =
            tokio::time::timeout(std::time::Duration::from_secs(5), processing_done_rx.recv())
                .await
                .expect("processing task should finish")
                .expect("processing task should report completion");
        assert_eq!(done_id, 77);
        result?;
        if let Some(handle) = processing_task.take() {
            handle.await.expect("processing task join");
        }
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

#[test]
fn reload_starting_rejects_new_turns_for_multiple_sessions() {
    let _guard = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    crate::server::write_reload_state(
        "reload-lifecycle-multi-starting",
        "test-hash",
        crate::server::ReloadPhase::Starting,
        Some("session_alpha".to_string()),
    );

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let forked = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
            forked: Arc::clone(&forked),
        });
        let registry = Registry::new(Arc::clone(&provider)).await;
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
        let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
        let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (swarm_event_tx, _) = broadcast::channel(8);

        for (message_id, session_id) in [(101, "session_alpha"), (102, "session_beta"), (103, "session_gamma")] {
            let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
            session.model = Some("panic-on-fork".to_string());
            let agent = Arc::new(Mutex::new(Agent::new_with_session(
                Arc::clone(&provider),
                registry.clone(),
                session,
                None,
            )));

            let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
            let (processing_done_tx, mut processing_done_rx) = mpsc::unbounded_channel();
            let mut client_is_processing = false;
            let mut processing_message_id = None;
            let mut processing_session_id = None;
            let mut processing_task = None;

            start_processing_message(
                ProcessingMessage {
                    id: message_id,
                    content: format!("do not start {session_id} during reload"),
                    images: Vec::new(),
                    system_reminder: None,
                    active_skill: None,
                    v2: None,
                },
                session_id,
                None,
                &mut ProcessingState {
                    client_is_processing: &mut client_is_processing,
                    message_id: &mut processing_message_id,
                    session_id: &mut processing_session_id,
                    task: &mut processing_task,
                    turn_acquired: &Arc::new(AtomicBool::new(false)),
                },
                &agent,
                &client_event_tx,
                &processing_done_tx,
                Vec::new(),
                &SwarmStatusRefs {
                    members: &swarm_members,
                    swarms_by_id: &swarms_by_id,
                    event_history: &event_history,
                    event_counter: &event_counter,
                    event_tx: &swarm_event_tx,
                },
            )
            .await;

            let event = tokio::time::timeout(std::time::Duration::from_millis(250), client_event_rx.recv())
                .await
                .expect("reload guard should emit promptly for every session")
                .expect("reload event should be sent to client");
            assert!(
                matches!(event, ServerEvent::Reloading { new_socket: None }),
                "expected Reloading event for {session_id}, got {event:?}"
            );
            assert!(
                client_event_rx.try_recv().is_err(),
                "reload guard should only emit one reload notification for {session_id}"
            );
            assert!(
                !client_is_processing,
                "{session_id} should not enter processing during reload"
            );
            assert_eq!(processing_message_id, None);
            assert_eq!(processing_session_id, None);
            assert!(
                processing_task.is_none(),
                "{session_id} should not spawn a processing task during reload"
            );
            assert!(processing_done_rx.try_recv().is_err());
        }

        assert!(
            !forked.load(Ordering::SeqCst),
            "rejecting multiple sessions during reload should not fork or invoke provider work"
        );
    });
}

fn decode_request_or_event(line: &str) -> ServerEvent {
    serde_json::from_str(line.trim()).expect("decode server event")
}

#[test]
fn soft_interrupt_dispatch_starts_idle_session_and_queues_busy_session() {
    assert!(should_start_idle_soft_interrupt(false, false, false));
    assert!(!should_start_idle_soft_interrupt(true, false, false));
    assert!(!should_start_idle_soft_interrupt(false, true, false));
    assert!(!should_start_idle_soft_interrupt(false, false, true));
}

#[derive(Clone, Default)]
struct SdkCallbackProvider {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait]
impl Provider for SdkCallbackProvider {
    async fn complete(&self, _: &[Message], tools: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read");
        assert_eq!(tools[0].description, "SDK read callback");
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = if call.is_multiple_of(2) {
            vec![
                // The event store requires each tool-use id to be unique within a
                // session, so a repeated turn must not reuse one id.
                StreamEvent::ToolUseStart {
                    id: format!("model-call-{call}"),
                    name: "read".into(),
                },
                StreamEvent::ToolInputDelta("{}".into()),
                StreamEvent::ToolUseEnd,
                StreamEvent::MessageEnd {
                    stop_reason: Some("tool_use".into()),
                },
            ]
        } else {
            vec![
                StreamEvent::TextDelta("callback completed".into()),
                StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                },
            ]
        };
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
    fn name(&self) -> &str {
        "sdk-callback-test"
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[tokio::test]
async fn sdk_socket_configure_list_callback_and_busy_rejection() {
    sdk_socket_workflow(false).await;
}

#[tokio::test]
async fn user_response_socket_registration_follows_explicit_capability() {
    sdk_socket_workflow(true).await;
}

async fn sdk_socket_workflow(response_only: bool) {
    let _lock = crate::storage::lock_test_env();
    let home = IsolatedReloadRecoveryEnv::new();
    let provider_template: Arc<dyn Provider> = Arc::new(SdkCallbackProvider::default());

    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let global_session_id = Arc::new(RwLock::new(String::new()));
    let client_count = Arc::new(RwLock::new(0usize));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let plan_proposals = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let file_touch = FileTouchService::new();
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let (_global_event_tx, _) = broadcast::channel(8);
    let global_is_processing = Arc::new(RwLock::new(false));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let connect = || {
        let (server_stream, client_stream) = crate::transport::Stream::pair().expect("socket pair");
        let server_task = tokio::spawn(handle_client(
            server_stream,
            Arc::clone(&sessions),
            _global_event_tx.clone(),
            provider_template.clone(),
            global_is_processing.clone(),
            global_session_id.clone(),
            client_count.clone(),
            Arc::clone(&client_connections),
            swarm_members.clone(),
            swarms_by_id.clone(),
            plan_proposals.clone(),
            swarm_plans.clone(),
            swarm_coordinators.clone(),
            file_touch.clone(),
            event_history.clone(),
            event_counter.clone(),
            swarm_event_tx.clone(),
            "horon-agent-test".to_string(),
            "🧪".to_string(),
            mcp_pool.clone(),
            shutdown_signals.clone(),
            soft_interrupt_queues.clone(),
            AwaitMembersRuntime::default(),
            SwarmMutationRuntime::default(),
            std::time::Duration::ZERO,
        ));
        (server_task, client_stream)
    };
    let (server_task, client_stream) = connect();

    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);

    async fn send(writer: &mut crate::transport::WriteHalf, value: serde_json::Value) {
        writer.write_all(format!("{value}\n").as_bytes()).await.unwrap();
    }
    async fn until(
        reader: &mut BufReader<crate::transport::ReadHalf>,
        predicate: impl Fn(&ServerEvent) -> bool,
    ) -> ServerEvent {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).await.unwrap() > 0);
                let event: ServerEvent = serde_json::from_str(&line).unwrap();
                if predicate(&event) {
                    return event;
                }
            }
        })
        .await
        .expect("daemon event timeout")
    }
    use serde_json::json;
    send(
        &mut client_writer,
        json!({"type":"subscribe","id":1,"working_dir":home._home.path()}),
    )
    .await;
    until(&mut client_reader, |e| matches!(e, ServerEvent::Done { id: 1 })).await;
    if response_only {
        // Exercise actual Desktop/TUI Subscribe snapshots, including disabling
        // the tool again on a repeated subscription rather than retaining it.
        for (index, capability) in [false, true, false].into_iter().enumerate() {
            let id = 100 + index as u64 * 2;
            send(
                &mut client_writer,
                json!({
                    "type": "subscribe",
                    "id": id,
                    "working_dir": home._home.path(),
                    "supports_user_response": capability,
                }),
            )
            .await;
            until(
                &mut client_reader,
                |e| matches!(e, ServerEvent::Done { id: done } if *done == id),
            )
            .await;
            send(&mut client_writer, json!({"type":"list_tools","id":id + 1})).await;
            let ServerEvent::Tools { tools, .. } = until(
                &mut client_reader,
                |e| matches!(e, ServerEvent::Tools { id: listed, .. } if *listed == id + 1),
            )
            .await
            else {
                panic!("expected tool list")
            };
            assert_eq!(tools.iter().any(|t| t.name == "user_response"), capability);
        }
        // This test exercises catalog gating, not the 30-second retention of
        // idle unsaved sessions. Remove the isolated fixture before detaching.
        sessions.write().await.clear();
        drop(client_writer);
        drop(client_reader);
        tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        return;
    }
    send(&mut client_writer, json!({"type":"configure_tools","id":2,"tools":{"enabled":[],"custom":[{"name":"read","description":"SDK read callback","parameters":{"type":"object"}}]}})).await;
    let configured = until(&mut client_reader, |e| {
        matches!(e, ServerEvent::Ack { id: 2 } | ServerEvent::Error { id: 2, .. })
    })
    .await;
    assert!(matches!(configured, ServerEvent::Ack { .. }), "{configured:?}");
    send(&mut client_writer, json!({"type":"list_tools","id":3})).await;
    let ServerEvent::Tools { tools, .. } = until(&mut client_reader, |e| {
        matches!(e, ServerEvent::Tools { id: 3, .. } | ServerEvent::Error { id: 3, .. })
    })
    .await
    else {
        panic!("expected tools")
    };
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read");
    send(
        &mut client_writer,
        json!({"type":"message","id":4,"content":"call read"}),
    )
    .await;
    let ServerEvent::ToolCall { call_id, name, .. } =
        until(&mut client_reader, |e| matches!(e, ServerEvent::ToolCall { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(name, "read");
    send(&mut client_writer, json!({"type":"configure_tools","id":5,"tools":{}})).await;
    let busy = until(&mut client_reader, |e| {
        matches!(e, ServerEvent::Ack { id: 5 } | ServerEvent::Error { id: 5, .. })
    })
    .await;
    assert!(
        matches!(busy, ServerEvent::Error { .. }),
        "must not ack invalid/busy config: {busy:?}"
    );
    send(
        &mut client_writer,
        json!({"type":"tool_result","id":6,"call_id":call_id,"output":"SDK read output"}),
    )
    .await;
    let acknowledged = until(&mut client_reader, |e| {
        matches!(e, ServerEvent::Ack { id: 6 } | ServerEvent::Error { id: 6, .. })
    })
    .await;
    assert!(matches!(acknowledged, ServerEvent::Ack { .. }), "{acknowledged:?}");
    until(&mut client_reader, |e| matches!(e, ServerEvent::Done { id: 4 })).await;

    let session_a = sessions.read().await.keys().next().unwrap().clone();
    let (observer_task, observer_stream) = connect();
    let (observer_reader, mut observer_writer) = observer_stream.into_split();
    let mut observer_reader = BufReader::new(observer_reader);
    send(
        &mut observer_writer,
        json!({"type":"subscribe","id":20,"working_dir":home._home.path(),"target_session_id":session_a}),
    )
    .await;
    until(&mut observer_reader, |e| matches!(e, ServerEvent::Done { id: 20 })).await;

    // Explicit detach releases SDK ownership even while the socket stays open.
    send(&mut client_writer, json!({"type":"prepare_disconnect","id":21})).await;
    until(&mut client_reader, |e| matches!(e, ServerEvent::Done { id: 21 })).await;
    let tools = json!({"enabled":[],"custom":[{"name":"read","description":"SDK read callback","parameters":{"type":"object"}}]});
    send(
        &mut observer_writer,
        json!({"type":"configure_tools","id":22,"tools":tools}),
    )
    .await;
    let ack = until(&mut observer_reader, |e| {
        matches!(e, ServerEvent::Ack { id: 22 } | ServerEvent::Error { id: 22, .. })
    })
    .await;
    assert!(
        matches!(ack, ServerEvent::Ack { .. }),
        "detach must release owner: {ack:?}"
    );

    // Switching the owner to B must leave A unavailable, not route A's
    // callback to B. The first client keeps A's Agent alive throughout.
    let mut other_session = crate::session::Session::create_with_id("sdk_other_session".into(), None, None);
    other_session.working_dir = Some(home._home.path().to_string_lossy().into_owned());
    other_session.save().unwrap();
    let other_registry = Registry::new(provider_template.clone()).await;
    sessions.write().await.insert(
        "sdk_other_session".into(),
        Arc::new(Mutex::new(Agent::new_with_session(
            provider_template.clone(),
            other_registry,
            other_session,
            None,
        ))),
    );
    send(
        &mut observer_writer,
        json!({"type":"resume_session","id":23,"session_id":"sdk_other_session"}),
    )
    .await;
    until(&mut observer_reader, |e| matches!(e, ServerEvent::Done { id: 23 })).await;
    send(
        &mut client_writer,
        json!({"type":"message","id":24,"content":"call read after owner switch"}),
    )
    .await;
    until(&mut client_reader, |e| {
        assert!(
            !matches!(e, ServerEvent::ToolCall { .. }),
            "disconnected owner must not receive callback"
        );
        matches!(e, ServerEvent::Done { id: 24 })
    })
    .await;
    send(&mut observer_writer, json!({"type":"list_tools","id":25})).await;
    until(&mut observer_reader, |e| {
        assert!(
            !matches!(e, ServerEvent::ToolCall { .. }),
            "A callback must never reach B's owner connection"
        );
        matches!(e, ServerEvent::Tools { id: 25, .. })
    })
    .await;

    // A new owner can configure retained A after the prior owner switches.
    send(
        &mut client_writer,
        json!({"type":"configure_tools","id":26,"tools":tools}),
    )
    .await;
    let ack = until(&mut client_reader, |e| {
        matches!(e, ServerEvent::Ack { id: 26 } | ServerEvent::Error { id: 26, .. })
    })
    .await;
    assert!(
        matches!(ack, ServerEvent::Ack { .. }),
        "session switch must release owner: {ack:?}"
    );
    send(
        &mut client_writer,
        json!({"type":"message","id":7,"content":"call read again"}),
    )
    .await;
    let event = until(&mut client_reader, |e| matches!(e, ServerEvent::ToolCall { .. })).await;
    assert!(matches!(event, ServerEvent::ToolCall { session_id, .. } if session_id == session_a));
    drop(observer_writer);
    drop(observer_reader);
    // The observer resumed an unsaved, idle session that has nothing on disk.
    // Its disconnect therefore takes the documented reconnect-grace path
    // (`IDLE_RECONNECT_GRACE`): the live Agent is retained until a successor
    // attachment claims the session or the grace expires. Nothing reconnects
    // here, so the deadline must cover that whole window, not just the
    // disconnect work that follows it.
    tokio::time::timeout(Duration::from_secs(40), observer_task)
        .await
        .expect("observer lifecycle should finish within the reconnect grace")
        .unwrap()
        .unwrap();
    // Disconnect with a pending callback must not wait for the 120s tool-callback
    // deadline; it may only wait for the idle-reconnect grace described above.
    drop(client_writer);
    drop(client_reader);
    tokio::time::timeout(Duration::from_secs(40), server_task)
        .await
        .expect("client lifecycle should finish within the reconnect grace")
        .unwrap()
        .unwrap();
}

/// An excluded built-in must never run its constructor.
#[tokio::test]
async fn loading_deferred_setup_excluded_builtin_is_not_constructed() {
    let skills = crate::tool::Registry::shared_skills_registry();
    let mut allow = std::collections::HashSet::new();
    allow.insert("read".to_string());
    allow.insert("bash".to_string());

    let built = crate::tool::Registry::base_tools(&skills, Some(&allow));
    let names: Vec<&str> = built.keys().map(String::as_str).collect();
    assert!(names.contains(&"read"), "an allowed tool must be present: {names:?}");
    assert!(names.contains(&"bash"), "an allowed tool must be present: {names:?}");
    for excluded in ["selfdev", "desktop_selfdev"] {
        assert!(
            !names.contains(&excluded),
            "{excluded} was constructed despite being excluded: {names:?}"
        );
    }

    // The unrestricted path still yields the full built-in surface, so the
    // filter cannot silently shrink the unrestricted profile.
    let all = crate::tool::Registry::base_tools(&skills, None);
    assert!(
        all.len() > built.len(),
        "the unrestricted surface must stay larger: {} vs {}",
        all.len(),
        built.len()
    );
}

/// The core profile keeps its eighteen tools, including the ones task 11 added.
#[tokio::test]
async fn loading_deferred_setup_core_profile_keeps_skill_and_mcp() {
    let skills = crate::tool::Registry::shared_skills_registry();
    let core: std::collections::HashSet<String> = horon_agent_base::config::CORE_TOOLS
        .iter()
        .map(|n| (*n).to_string())
        .collect();
    let built = crate::tool::Registry::base_tools(&skills, Some(&core));
    // Every core tool that `base_tools` owns must survive the allow-list. `undo`
    // and `ask` are per-session tools built in `Registry::new`, not here, so they
    // are asserted against CORE_TOOLS below instead of against this map.
    for required in [
        "skill_manage",
        "monitor",
        "bash",
        "read",
        "webfetch",
        "websearch",
        "edit",
    ] {
        assert!(
            built.contains_key(required),
            "{required} must survive the core allow-list: {:?}",
            built.keys().collect::<Vec<_>>()
        );
    }
    for session_tool in ["undo", "ask", "mcp"] {
        assert!(
            horon_agent_base::config::CORE_TOOLS.contains(&session_tool),
            "{session_tool} must stay in CORE_TOOLS"
        );
    }
    assert_eq!(
        horon_agent_base::config::CORE_TOOLS.len(),
        18,
        "CORE_TOOLS itself must stay at eighteen"
    );
}

/// Closing a session must release its per-session caches.
///
/// Each of these is a process-wide map keyed by session id. Without a teardown a
/// daemon that opens and closes sessions forever keeps every one of them, plus
/// the monitor sampler thread that belongs to it.
#[tokio::test]
async fn loading_deferred_setup_session_teardown_releases_the_caches() {
    let session = format!("teardown-{}", std::process::id());

    let before = crate::server::session_observation::observation_for(&session);
    before.append_message(crate::protocol::HistoryMessage {
        turn_start: false,
        message_id: None,
        response_stats: None,
        role: "user".to_string(),
        content: "a message that keeps the record alive".to_string(),
        tool_calls: None,
        ..Default::default()
    });

    crate::server::forget_removed_session(&session, None);

    // A forgotten session must get a fresh owner: a brand-new record has no
    // history and a seq that restarted, which is what proves the old one went.
    let after = crate::server::session_observation::observation_for(&session);
    assert_eq!(
        after.seq(),
        0,
        "a forgotten session must get a fresh observation owner, not the cached one"
    );
}

#[derive(Clone)]
struct AcpBridgeTurnProvider {
    open: Arc<tokio::sync::watch::Sender<bool>>,
}

impl AcpBridgeTurnProvider {
    fn new() -> Self {
        Self {
            open: Arc::new(tokio::sync::watch::channel(false).0),
        }
    }

    fn hold_turn(&self) -> Arc<tokio::sync::watch::Sender<bool>> {
        Arc::clone(&self.open)
    }
}

#[async_trait]
impl Provider for AcpBridgeTurnProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        use futures::StreamExt as _;
        let mut open = self.open.subscribe();
        Ok(Box::pin(
            stream::once(async move { Ok(StreamEvent::TextDelta("turn started".to_string())) }).chain(stream::once(
                async move {
                    loop {
                        if *open.borrow_and_update() {
                            break;
                        }
                        if open.changed().await.is_err() {
                            break;
                        }
                    }
                    Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    })
                },
            )),
        ))
    }

    fn name(&self) -> &str {
        "acp-bridge-turn"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

/// An ACP subscription must carry its editor a client request while its own turn
/// runs, resolve the daemon-side call with the answer, refuse answers from other
/// connections, and stop routing the moment the turn ends.
#[tokio::test]
async fn acp_session_bridge_round_trip_and_desktop_turn_uses_local_fs() {
    let _lock = crate::storage::lock_test_env();
    let home = IsolatedReloadRecoveryEnv::new();
    let provider = AcpBridgeTurnProvider::new();
    let turn_gate = provider.hold_turn();
    let provider_template: Arc<dyn Provider> = Arc::new(provider);

    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let global_session_id = Arc::new(RwLock::new(String::new()));
    let client_count = Arc::new(RwLock::new(0usize));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let plan_proposals = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let file_touch = FileTouchService::new();
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let (_global_event_tx, _) = broadcast::channel(8);
    let global_is_processing = Arc::new(RwLock::new(false));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let connect = || {
        let (server_stream, client_stream) = crate::transport::Stream::pair().expect("socket pair");
        let server_task = tokio::spawn(handle_client(
            server_stream,
            Arc::clone(&sessions),
            _global_event_tx.clone(),
            provider_template.clone(),
            global_is_processing.clone(),
            global_session_id.clone(),
            client_count.clone(),
            Arc::clone(&client_connections),
            swarm_members.clone(),
            swarms_by_id.clone(),
            plan_proposals.clone(),
            swarm_plans.clone(),
            swarm_coordinators.clone(),
            file_touch.clone(),
            event_history.clone(),
            event_counter.clone(),
            swarm_event_tx.clone(),
            "horon-agent-test".to_string(),
            "🧪".to_string(),
            mcp_pool.clone(),
            shutdown_signals.clone(),
            soft_interrupt_queues.clone(),
            AwaitMembersRuntime::default(),
            SwarmMutationRuntime::default(),
            std::time::Duration::ZERO,
        ));
        (server_task, client_stream)
    };

    async fn send(writer: &mut crate::transport::WriteHalf, value: serde_json::Value) {
        writer.write_all(format!("{value}\n").as_bytes()).await.unwrap();
    }
    async fn until(
        reader: &mut BufReader<crate::transport::ReadHalf>,
        predicate: impl Fn(&ServerEvent) -> bool,
    ) -> ServerEvent {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).await.unwrap() > 0);
                let event: ServerEvent = serde_json::from_str(&line).unwrap();
                if predicate(&event) {
                    return event;
                }
            }
        })
        .await
        .expect("daemon event timeout")
    }
    use serde_json::json;

    let (server_task, client_stream) = connect();
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);

    send(
        &mut client_writer,
        json!({
            "type": "subscribe",
            "id": 1,
            "working_dir": home._home.path(),
            "acp": {"client_capabilities": {"fs_read_text_file": true}},
        }),
    )
    .await;
    until(&mut client_reader, |event| matches!(event, ServerEvent::Done { id: 1 })).await;

    let session_id = sessions
        .read()
        .await
        .keys()
        .next()
        .expect("a subscribed session")
        .clone();
    assert!(crate::tool::acp_session::is_acp(&session_id));
    assert!(
        crate::tool::client_bridge::for_session(&session_id).is_none(),
        "no turn is running, so nothing routes to the editor yet"
    );

    let (observer_task, observer_stream) = connect();
    let (observer_reader, mut observer_writer) = observer_stream.into_split();
    let mut observer_reader = BufReader::new(observer_reader);
    send(
        &mut observer_writer,
        json!({
            "type": "subscribe",
            "id": 20,
            "working_dir": home._home.path(),
            "target_session_id": session_id,
        }),
    )
    .await;
    until(&mut observer_reader, |event| {
        matches!(event, ServerEvent::Done { id: 20 })
    })
    .await;

    send(&mut client_writer, json!({"type":"message","id":2,"content":"start"})).await;
    until(
        &mut client_reader,
        |event| matches!(event, ServerEvent::TextDelta { text } if text == "turn started"),
    )
    .await;
    let bridge = crate::tool::client_bridge::for_session(&session_id)
        .expect("the ACP connection's running turn routes fs calls to the editor");

    let caller = {
        let bridge = Arc::clone(&bridge);
        tokio::spawn(async move {
            bridge
                .call(
                    "fs/read_text_file",
                    json!({"path": "/tmp/unsaved-buffer.txt"}),
                    Some("tool-call-1".to_string()),
                    Duration::from_secs(10),
                )
                .await
        })
    };
    let (request_id, method, params, tool_call_id) = match until(&mut client_reader, |event| {
        matches!(event, ServerEvent::ClientRequest { .. })
    })
    .await
    {
        ServerEvent::ClientRequest {
            request_id,
            method,
            params,
            tool_call_id,
        } => (request_id, method, params, tool_call_id),
        other => panic!("expected a client request, got {other:?}"),
    };
    assert_eq!(method, "fs/read_text_file");
    assert_eq!(params["path"], "/tmp/unsaved-buffer.txt");
    assert_eq!(tool_call_id.as_deref(), Some("tool-call-1"));

    send(
        &mut observer_writer,
        json!({
            "type": "client_response",
            "id": 21,
            "request_id": request_id,
            "result": {"content": "answered by the wrong connection"},
        }),
    )
    .await;
    let rejected = until(&mut observer_reader, |event| {
        matches!(event, ServerEvent::Ack { id: 21 } | ServerEvent::Error { id: 21, .. })
    })
    .await;
    match rejected {
        ServerEvent::Error { message, .. } => assert!(
            message.contains("not served by this connection"),
            "another connection must be told why it cannot answer: {message}"
        ),
        other => panic!("a response from another connection must be rejected, got {other:?}"),
    }

    send(
        &mut client_writer,
        json!({
            "type": "client_response",
            "id": 3,
            "request_id": request_id,
            "result": {"content": "unsaved buffer"},
        }),
    )
    .await;
    let ack = until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Ack { id: 3 } | ServerEvent::Error { id: 3, .. })
    })
    .await;
    assert!(matches!(ack, ServerEvent::Ack { .. }), "{ack:?}");
    let answer = caller
        .await
        .expect("the call task must not panic")
        .expect("the call resolves");
    assert_eq!(answer["content"], "unsaved buffer");

    send(
        &mut client_writer,
        json!({
            "type": "client_response",
            "id": 4,
            "request_id": request_id,
            "result": {"content": "answered twice"},
        }),
    )
    .await;
    let repeated = until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Ack { id: 4 } | ServerEvent::Error { id: 4, .. })
    })
    .await;
    match repeated {
        ServerEvent::Error { message, .. } => assert_eq!(message, "unknown client request"),
        other => panic!("a second answer to the same request must be an error, got {other:?}"),
    }

    turn_gate.send(true).expect("the turn gate stays open");
    until(&mut client_reader, |event| matches!(event, ServerEvent::Done { id: 2 })).await;
    assert!(
        crate::tool::client_bridge::for_session(&session_id).is_none(),
        "a finished ACP turn must leave no owner, so a later daemon turn uses local fs"
    );

    drop(bridge);
    sessions.write().await.clear();
    drop(client_writer);
    drop(client_reader);
    tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(observer_writer);
    drop(observer_reader);
    tokio::time::timeout(Duration::from_secs(10), observer_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[path = "client_lifecycle_tests/acp_background_work.rs"]
mod acp_background_work;
#[path = "client_lifecycle_tests/acp_permission.rs"]
mod acp_permission;
#[path = "client_lifecycle_tests/acp_plan_diff.rs"]
mod acp_plan_diff;
#[path = "client_lifecycle_tests/acp_session_mcp.rs"]
mod acp_session_mcp;
#[path = "client_lifecycle_tests/acp_tool_cancel.rs"]
mod acp_tool_cancel;
#[path = "client_lifecycle_tests/acp_v2_events.rs"]
mod acp_v2_events;
#[path = "client_lifecycle_tests/lightweight_comm.rs"]
mod lightweight_comm;
#[path = "client_lifecycle_tests/reattach_init.rs"]
mod reattach_init;
#[path = "client_lifecycle_tests/system_prompt.rs"]
mod system_prompt;
