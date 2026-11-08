//! IS-5: the foreground state of one ACP v2 session, and the single ordered
//! stream every v2 `session/update` of that session goes out on.
//!
//! A v2 session reports every foreground turn, whoever started it: this
//! editor, another attached surface, or the agent itself. So the session does
//! not hand its daemon events to the prompt that is running, as v1 does; one
//! stream task per session reads every event the pump routes to it
//! (`DaemonSession::open_v2_stream`) and is the only writer of the session's
//! updates. It answers this editor's prompts when the daemon reports their
//! insertion (`prompt.rs`), so the `{messageId}` response, the `user_message`
//! and every later update of the turn leave in daemon order, and it keeps the
//! [`ForegroundTracker`] that decides every `state_update`.

use super::super::*;
use super::background::{self, BackgroundSet, REMOVAL_DEFER};
use super::foreground::ForegroundTracker;
use super::mapper::V2Mapper;
use super::replay::Position;
use super::shapes::{self, ChunkKind, message_chunk, message_ids, state_idle, state_running};
use crate::protocol::TurnStopReason;
use std::sync::Mutex as StdMutex;
use std::sync::Weak;
use tokio::sync::{mpsc, oneshot, watch};

/// The daemon's refusal of a `withdraw_message` whose turn already claimed the
/// message (crates/app-core/src/server/client_lifecycle.rs, `WithdrawMessage`).
/// It is also the answer once that turn has ended.
const WITHDRAW_ALREADY_INSERTED: &str = "already inserted";

/// A test's observation of [`V2Stream::inserted`]: the prompt's daemon id and
/// whether the prompt was still pending at that moment.
#[cfg(test)]
pub(crate) type InsertedProbe = Box<dyn Fn(u64, bool) + Send + Sync>;

/// A message to the stream task from outside the daemon stream.
pub(crate) enum Control {
    /// A permission or elicitation request to this editor opened (`true`) or
    /// was answered (`false`).
    UserAction(bool),
    /// A prompt this adapter handled itself (a slash command): answer it with
    /// a live-only message id and report it as a foreground interval.
    LocalCommand {
        rpc_id: Value,
        content: Vec<Value>,
        text: String,
        done: oneshot::Sender<()>,
    },
    /// The session closes. With `give_up`, a turn still reported is reported
    /// cancelled here. Every later event is dropped.
    Close { give_up: bool, done: oneshot::Sender<()> },
    /// `session/resume` synchronizes the session's current state; `done`
    /// resolves once its updates are written, before any event the stream
    /// reads after them.
    Resumed {
        resumed: Box<Resumed>,
        done: oneshot::Sender<()>,
    },
}

/// The daemon's `ForegroundState { snapshot: true }` for a session this
/// connection just attached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttachSnapshot {
    pub(crate) running: bool,
    pub(crate) awaiting_user: bool,
    pub(crate) run_id: String,
    pub(crate) start_message_id: Option<String>,
}

/// IS-5 and IS-13: what a `session/resume` writes before its response, in
/// order: the replay and the session info, then the one current-state update.
pub(crate) struct Resumed {
    pub(crate) updates: Vec<Value>,
    /// The attach snapshot; `None` for a session already active on this
    /// connection, whose tracker is already current.
    pub(crate) snapshot: Option<AttachSnapshot>,
    /// Where the History leaves the ids of its last turn.
    pub(crate) position: Option<Position>,
    /// Stream events the attach read after its `Done`, in daemon order. They
    /// are mapped after the snapshot, whose run they belong to or follow.
    pub(crate) leftovers: Vec<ServerEvent>,
    /// IS-4: the background work the daemon reports running, listed before
    /// the replay is written so the replay keeps listed calls open.
    pub(crate) background: Vec<crate::protocol::BackgroundWorkItem>,
}

/// One `session/prompt` waiting for its insertion.
pub(crate) struct PendingPrompt {
    pub(crate) rpc_id: Value,
    pub(crate) key: RequestKey,
    /// The prompt's content blocks, written as its `user_message`.
    pub(crate) content: Vec<Value>,
    pub(crate) outcome: oneshot::Sender<PromptOutcome>,
}

/// How one `withdraw_message` ended, decided in daemon event order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Withdrawal {
    /// The message was dropped before the turn stored it.
    Withdrawn,
    /// The turn claimed the message first. `open`: that turn has not reported
    /// idle yet when the refusal is read, so only a daemon `Cancel` stops it; a
    /// turn already over needs none, and a `Cancel` then would stop whatever
    /// turn runs next in the session.
    AlreadyInserted { open: bool },
    /// No answer, or one naming no queued message.
    Unsettled,
}

/// A `withdraw_message` sent and not answered yet.
struct WithdrawWaiter {
    message_request_id: u64,
    verdict: oneshot::Sender<Withdrawal>,
}

/// How a pending prompt was answered.
#[derive(Debug)]
pub(crate) enum PromptOutcome {
    Inserted,
    Rejected,
}

/// Opens or answers a client request that waits on the user, and reports the
/// answer when dropped.
pub(crate) struct UserActionGuard {
    control: mpsc::UnboundedSender<Control>,
}

impl Drop for UserActionGuard {
    fn drop(&mut self) {
        let _ = self.control.send(Control::UserAction(false));
    }
}

/// The side channel's handle on the session's foreground state.
#[derive(Clone)]
pub(crate) struct UserActions {
    control: mpsc::UnboundedSender<Control>,
}

impl UserActions {
    /// A permission or elicitation request to this editor is about to open.
    pub(crate) fn begin(&self) -> UserActionGuard {
        let _ = self.control.send(Control::UserAction(true));
        UserActionGuard {
            control: self.control.clone(),
        }
    }
}

/// The v2 state of one session: its tracker, the prompts not inserted yet, and
/// the stream task that owns both.
pub(crate) struct V2Session {
    pub(crate) tracker: StdMutex<ForegroundTracker>,
    /// IS-4: the background work this session lists on every `state_update`.
    background: StdMutex<BackgroundSet>,
    pending: StdMutex<HashMap<u64, PendingPrompt>>,
    /// `withdraw_message` requests by request id, answered by the stream task.
    withdraws: StdMutex<HashMap<u64, WithdrawWaiter>>,
    /// Held while a prompt registers and writes its `message`, and while a
    /// cancel reads which prompts to withdraw, so every prompt a cancel sees is
    /// already on the daemon socket and every later one sees the cancel.
    pub(crate) submit_gate: Mutex<()>,
    control: mpsc::UnboundedSender<Control>,
    start: StdMutex<Option<(mpsc::UnboundedReceiver<Control>, mpsc::UnboundedReceiver<Routed>)>>,
    task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    /// Bumped on every change a waiter may be waiting for: an idle report, a
    /// prompt answered, the stream closed.
    changes: watch::Sender<u64>,
    closed: std::sync::atomic::AtomicBool,
    /// A test's observation of the window in [`V2Stream::inserted`] between
    /// opening an inserted prompt's turn and taking the prompt.
    #[cfg(test)]
    inserted_probe: StdMutex<Option<InsertedProbe>>,
    /// How long `session/close` waits for the turn to end.
    pub(crate) close_bound: Duration,
}

impl Drop for V2Session {
    fn drop(&mut self) {
        if let Some(task) = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
}

impl V2Session {
    pub(crate) fn new(events: mpsc::UnboundedReceiver<Routed>, close_bound: Duration) -> Self {
        let (control, control_rx) = mpsc::unbounded_channel();
        Self {
            tracker: StdMutex::default(),
            background: StdMutex::default(),
            pending: StdMutex::default(),
            withdraws: StdMutex::default(),
            submit_gate: Mutex::new(()),
            control,
            start: StdMutex::new(Some((control_rx, events))),
            task: StdMutex::default(),
            changes: watch::Sender::new(0),
            closed: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            inserted_probe: StdMutex::default(),
            close_bound,
        }
    }

    pub(crate) fn tracker(&self) -> std::sync::MutexGuard<'_, ForegroundTracker> {
        self.tracker.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn background(&self) -> std::sync::MutexGuard<'_, BackgroundSet> {
        self.background
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_pending(&self) -> std::sync::MutexGuard<'_, HashMap<u64, PendingPrompt>> {
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn user_actions(&self) -> UserActions {
        UserActions {
            control: self.control.clone(),
        }
    }

    pub(crate) fn send_control(&self, control: Control) -> bool {
        self.control.send(control).is_ok()
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn changed(&self) {
        self.changes.send_modify(|epoch| *epoch += 1);
    }

    /// Observe the moment between opening an inserted prompt's turn and taking
    /// the prompt. The test's assertion pins that order: the prompt is still
    /// pending there, so a close never reads the session as settled in between.
    #[cfg(test)]
    pub(crate) fn set_inserted_probe(&self, probe: InsertedProbe) {
        *self
            .inserted_probe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(probe);
    }

    #[cfg(test)]
    fn probe_inserted(&self, daemon_id: u64, pending: bool) {
        if let Some(probe) = self
            .inserted_probe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            probe(daemon_id, pending);
        }
    }

    /// The stream task's receivers, for a test that plays that task itself (or
    /// ends it) instead of starting [`AcpRuntime::start_v2_stream`].
    #[cfg(test)]
    pub(crate) fn take_stream_receivers(
        &self,
    ) -> Option<(mpsc::UnboundedReceiver<Control>, mpsc::UnboundedReceiver<Routed>)> {
        self.start
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Register a prompt under its daemon `message` id. Called with
    /// [`Self::submit_gate`] held. Gives the prompt back when the stream
    /// already closed.
    pub(crate) fn register(&self, daemon_id: u64, prompt: PendingPrompt) -> Result<(), PendingPrompt> {
        if self.is_closed() {
            return Err(prompt);
        }
        self.lock_pending().insert(daemon_id, prompt);
        Ok(())
    }

    /// Take the right to answer a pending prompt. Exactly one caller gets it.
    pub(crate) fn take_pending(&self, daemon_id: u64) -> Option<PendingPrompt> {
        let taken = self.lock_pending().remove(&daemon_id);
        if taken.is_some() {
            self.changed();
        }
        taken
    }

    fn lock_withdraws(&self) -> std::sync::MutexGuard<'_, HashMap<u64, WithdrawWaiter>> {
        self.withdraws.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Register `withdraw_message` `id` for `message_request_id` before it is
    /// written; the stream task answers it when the daemon's reply arrives.
    pub(crate) fn expect_withdraw(&self, id: u64, message_request_id: u64) -> oneshot::Receiver<Withdrawal> {
        let (verdict, answer) = oneshot::channel();
        self.lock_withdraws().insert(
            id,
            WithdrawWaiter {
                message_request_id,
                verdict,
            },
        );
        answer
    }

    pub(crate) fn forget_withdraw(&self, id: u64) {
        self.lock_withdraws().remove(&id);
    }

    fn take_withdraw(&self, id: u64) -> Option<WithdrawWaiter> {
        self.lock_withdraws().remove(&id)
    }

    fn is_pending(&self, daemon_id: u64) -> bool {
        self.lock_pending().contains_key(&daemon_id)
    }

    pub(crate) fn pending_ids(&self) -> Vec<u64> {
        self.lock_pending().keys().copied().collect()
    }

    /// The daemon id of the pending prompt that is client request `key`.
    pub(crate) fn pending_id_of(&self, key: &RequestKey) -> Option<u64> {
        self.lock_pending()
            .iter()
            .find(|(_, prompt)| &prompt.key == key)
            .map(|(id, _)| *id)
    }

    fn take_all_pending(&self) -> Vec<PendingPrompt> {
        let taken: Vec<PendingPrompt> = self.lock_pending().drain().map(|(_, prompt)| prompt).collect();
        if !taken.is_empty() {
            self.changed();
        }
        taken
    }

    /// Wait until `done` holds, re-checking after every change, or until the
    /// deadline. True when it holds.
    pub(crate) async fn wait_until(
        &self,
        deadline: Option<tokio::time::Instant>,
        mut done: impl FnMut(&Self) -> bool,
    ) -> bool {
        let mut changes = self.changes.subscribe();
        loop {
            changes.mark_unchanged();
            if done(self) {
                return true;
            }
            let changed = changes.changed();
            let result = match deadline {
                Some(deadline) => match tokio::time::timeout_at(deadline, changed).await {
                    Ok(result) => result,
                    Err(_) => return done(self),
                },
                None => changed.await,
            };
            if result.is_err() {
                return done(self);
            }
        }
    }

    /// No prompt waits for insertion, no turn is reported, and no turn an
    /// inserted prompt of this editor started is still to report its end.
    pub(crate) fn settled(&self) -> bool {
        self.lock_pending().is_empty() && !self.tracker().has_cancellable_turn()
    }
}

impl AcpRuntime {
    /// Give a session created on a v2 connection its v2 state and route its
    /// stream there. Runs before the session bridge is installed, so the side
    /// channel can report permission and elicitation requests.
    pub(in super::super) fn prepare_v2_session(&self, session: &DaemonSession) {
        if self.spoken_version() != Some(super::negotiate::AcpVersion::V2) {
            return;
        }
        let events = session.open_v2_stream();
        let _ = session.v2.set(Arc::new(V2Session::new(events, PROMPT_CANCEL_BOUND)));
    }

    /// Start the stream task of a session [`Self::prepare_v2_session`]
    /// prepared, once it is shared. A second call does nothing.
    pub(in super::super) fn start_v2_stream(&self, session: &Arc<DaemonSession>) {
        let Some(v2) = session.v2.get() else {
            return;
        };
        let Some((control, events)) = v2
            .start
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let stream = V2Stream {
            runtime: self.clone(),
            session: Arc::downgrade(session),
            v2: Arc::clone(v2),
            session_id: session.session_id.clone(),
            run: None,
            outside: None,
            removal_due: StdMutex::new(None),
        };
        let task = tokio::spawn(stream.run(control, events));
        *v2.task.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
    }

    pub(in super::super) async fn write_v2_update(&self, session_id: &str, update: Value) -> Result<()> {
        self.write_notification("session/update", json!({ "sessionId": session_id, "update": update }))
            .await
    }
}

/// The stream task of one v2 session.
struct V2Stream {
    runtime: AcpRuntime,
    /// Weak, so the task never keeps a closed session's daemon connection.
    session: Weak<DaemonSession>,
    v2: Arc<V2Session>,
    session_id: String,
    /// The mapper of the run being reported, keyed by its start id.
    run: Option<V2Mapper>,
    /// The mapper of events outside any reported run, kept for the session so
    /// its compaction ids never repeat.
    outside: Option<V2Mapper>,
    /// IS-4: a removal made while idle, not yet reported. Its snapshot goes
    /// out at this deadline unless a state update (a follow-up turn's
    /// `running`) carries the removal first.
    removal_due: StdMutex<Option<tokio::time::Instant>>,
}

impl V2Stream {
    async fn run(mut self, mut control: mpsc::UnboundedReceiver<Control>, mut events: mpsc::UnboundedReceiver<Routed>) {
        loop {
            let removal_due = *self
                .removal_due
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let removal = async move {
                match removal_due {
                    Some(due) => tokio::time::sleep_until(due).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                message = control.recv() => {
                    let Some(message) = message else { continue };
                    if let Control::Close { give_up, done } = message {
                        // What the daemon already sent belongs before the close.
                        while let Ok(Routed::Daemon(event)) = events.try_recv() {
                            self.on_event(*event).await;
                        }
                        self.close(give_up).await;
                        let _ = done.send(());
                        return;
                    }
                    self.on_control(message).await;
                }
                event = events.recv() => match event {
                    Some(Routed::Daemon(event)) => self.on_event(*event).await,
                    // Terminal embedding is a v1 client capability.
                    Some(Routed::Local(_)) => {}
                    None => {
                        self.disconnected().await;
                        return;
                    }
                },
                () = removal => {
                    let state = self.v2.tracker().current_state();
                    self.write_all(vec![state]).await;
                }
            }
        }
    }

    /// Write `updates` in order. Every `state_update` carries the session's
    /// current background list, and writing one reports any deferred removal.
    async fn write_all(&self, mut updates: Vec<Value>) {
        {
            let set = self.v2.background();
            for update in &mut updates {
                if background::stamp(update, &set) {
                    *self
                        .removal_due
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                }
            }
        }
        for update in updates {
            if let Err(err) = self.runtime.write_v2_update(&self.session_id, update).await {
                crate::logging::warn(&format!(
                    "ACP: failed to write a v2 update for {}: {err:#}",
                    self.session_id
                ));
            }
        }
    }

    async fn on_control(&mut self, message: Control) {
        match message {
            Control::UserAction(true) => {
                let updates = self.v2.tracker().user_action_started();
                self.write_all(updates).await;
            }
            Control::UserAction(false) => self.v2.tracker().user_action_finished(),
            Control::LocalCommand {
                rpc_id,
                content,
                text,
                done,
            } => {
                self.local_command(rpc_id, &content, &text).await;
                let _ = done.send(());
            }
            Control::Resumed { resumed, done } => {
                self.resumed(*resumed).await;
                let _ = done.send(());
            }
            Control::Close { .. } => unreachable!("handled by the run loop"),
        }
    }

    /// IS-5: replay and session info, the announcement of every background
    /// item newly listed, then one state taken from the attach snapshot (or
    /// the tracker of a session already active here), then what the attach
    /// read after its `Done`. A run attached part-way gets a mapper whose ids
    /// continue the History's, so live and replayed ids match. The list is
    /// filled before the replay is written, so a listed call is replayed open.
    async fn resumed(&mut self, resumed: Resumed) {
        let Resumed {
            mut updates,
            snapshot,
            position,
            leftovers,
            background,
        } = resumed;
        let announcements = {
            let mut set = self.v2.background();
            let announcements = set.hydrate(&background);
            for update in &mut updates {
                background::keep_listed_call_open(update, &set);
            }
            announcements
        };
        updates.extend(announcements);
        self.write_all(updates).await;
        let snapshot_run = snapshot
            .as_ref()
            .filter(|snapshot| snapshot.running)
            .map(|snapshot| snapshot.run_id.clone());
        if let Some(snapshot) = snapshot {
            let start = snapshot
                .start_message_id
                .clone()
                .unwrap_or_else(|| snapshot.run_id.clone());
            if snapshot.running && !snapshot.run_id.is_empty() {
                let cwd = self.cwd().await;
                self.run = Some(match position.filter(|position| position.start == start) {
                    Some(position) => V2Mapper::resumed(position, cwd),
                    None => V2Mapper::new(&start, cwd),
                });
            }
            self.v2
                .tracker()
                .snapshot(snapshot.running, snapshot.awaiting_user, &snapshot.run_id);
        }
        let state = self.v2.tracker().current_state();
        self.write_all(vec![state]).await;
        for event in leftovers {
            // A run that ended before the snapshot was taken is already over in
            // it: its end would report a false idle while the snapshot's run
            // still runs.
            if let (
                Some(snapshot),
                ServerEvent::ForegroundState {
                    running: false,
                    run_id,
                    snapshot: false,
                    ..
                },
            ) = (&snapshot_run, &event)
                && run_id != snapshot
            {
                continue;
            }
            self.on_event(event).await;
        }
    }

    /// IS-4 for a prompt the adapter answers itself: insertion of a live-only
    /// message, then `running` -> output -> `idle(end_turn)` when nothing else
    /// runs, or the output alone inside a turn that keeps running.
    async fn local_command(&self, rpc_id: Value, content: &[Value], text: &str) {
        static NEXT_LOCAL: AtomicU64 = AtomicU64::new(1);
        let message_id = format!("local-{}", NEXT_LOCAL.fetch_add(1, Ordering::Relaxed));
        self.release_prompt();
        if let Err(err) = self
            .runtime
            .write_result(rpc_id, json!({ "messageId": message_id }))
            .await
        {
            crate::logging::warn(&format!("ACP: failed to answer a v2 command: {err:#}"));
        }
        let mut updates = vec![shapes::user_message(&message_id, content)];
        let interval = !self.v2.tracker().is_active();
        if interval {
            updates.push(state_running());
        }
        updates.push(message_chunk(
            ChunkKind::AgentMessage,
            &message_ids(&message_id).agent(),
            text,
        ));
        if interval {
            updates.push(state_idle(Some("end_turn"), None));
        }
        self.write_all(updates).await;
    }

    async fn on_event(&mut self, event: ServerEvent) {
        if self.v2.is_closed() {
            return;
        }
        if self.runtime.profile.is_extended()
            && let Err(err) = self
                .runtime
                .write_horon_agent_extension_event(&self.session_id, &event)
                .await
        {
            crate::logging::warn(&format!("ACP: failed to write a session event: {err:#}"));
        }
        match event {
            ServerEvent::UserMessageInserted { id, message_id } => self.inserted(id, message_id).await,
            ServerEvent::Error { id, message, .. } => {
                if let Some(withdraw) = self.v2.take_withdraw(id) {
                    self.withdraw_refused(withdraw, &message);
                } else if let Some(prompt) = self.v2.take_pending(id) {
                    self.reject(prompt, pre_insert_error(&message)).await;
                } else {
                    self.v2.tracker().record_error(&message);
                }
            }
            // A cancel took the queued message back before the turn stored it.
            ServerEvent::Done { id } => {
                if let Some(withdraw) = self.v2.take_withdraw(id) {
                    if let Some(prompt) = self.v2.take_pending(withdraw.message_request_id) {
                        self.reject(prompt, RpcError::cancelled()).await;
                    }
                    let _ = withdraw.verdict.send(Withdrawal::Withdrawn);
                } else if let Some(prompt) = self.v2.take_pending(id) {
                    self.reject(prompt, RpcError::cancelled()).await;
                }
            }
            ServerEvent::Reloading { .. } => {
                for prompt in self.v2.take_all_pending() {
                    self.reject(
                        prompt,
                        RpcError::new(JSONRPC_INTERNAL_ERROR, "The HoronAgent daemon is reloading"),
                    )
                    .await;
                }
            }
            ServerEvent::ForegroundState {
                running,
                run_id,
                start_message_id,
                user_content,
                awaiting_user,
                stop_reason,
                snapshot,
                ..
            } => {
                if snapshot {
                    // A run first seen at attach is mapped like one seen
                    // starting, so its ids derive from its start id and its
                    // `running: false` ends its open calls. A repeat for the run
                    // already mapped keeps that mapper and its round.
                    let start = start_message_id.as_deref().unwrap_or(&run_id);
                    if running && !run_id.is_empty() && self.run.as_ref().is_none_or(|mapper| mapper.start() != start) {
                        self.run = Some(self.mapper_for(start).await);
                    }
                    self.v2.tracker().snapshot(running, awaiting_user, &run_id);
                } else if running {
                    // IS-5: a run starting while another is reported ends that
                    // one first (its calls, then its idle), so this run reports
                    // `running` and gets a mapper of its own.
                    let superseded = self.v2.tracker().supersede(&run_id);
                    let mut updates = Vec::new();
                    if let Some((ended, own)) = superseded {
                        updates.extend(
                            self.run
                                .take()
                                .map(|mut mapper| mapper.run_ended(None))
                                .unwrap_or_default(),
                        );
                        updates.extend(ended);
                        if own {
                            self.release_prompt();
                        }
                    }
                    let start = start_message_id.as_deref().unwrap_or(&run_id);
                    if self.run.as_ref().is_none_or(|mapper| mapper.start() != start) {
                        self.run = Some(self.mapper_for(start).await);
                    }
                    updates.extend(self.v2.tracker().turn_started(
                        &run_id,
                        start_message_id.as_deref(),
                        user_content.as_deref(),
                    ));
                    self.write_all(updates).await;
                    self.v2.changed();
                } else if !self.v2.tracker().ends_current_run(&run_id) {
                    // The late end of a run already reported over: the run
                    // reported now keeps its state and its mapper.
                    if self.v2.tracker().stale_run_ended(start_message_id.as_deref()) {
                        self.release_prompt();
                    }
                    self.v2.changed();
                } else {
                    // Calls the run leaves open end before its idle update.
                    let mut updates = self
                        .run
                        .take()
                        .map(|mut mapper| mapper.run_ended(stop_reason.as_deref()))
                        .unwrap_or_default();
                    updates.extend({
                        let mut tracker = self.v2.tracker();
                        let own = start_message_id
                            .as_deref()
                            .is_some_and(|start| !tracker.own_run_ended(start));
                        if own {
                            self.release_prompt();
                        }
                        tracker.turn_ended(start_message_id.as_deref(), stop_reason.as_deref())
                    });
                    self.write_all(updates).await;
                    self.v2.changed();
                }
            }
            ServerEvent::TokenUsage {
                input,
                cache_read_input,
                cache_creation_input,
                ..
            } => {
                let Some(session) = self.session.upgrade() else {
                    return;
                };
                let pre = self.v2.tracker().before_mapped_event();
                self.write_all(pre).await;
                if let Err(err) = self
                    .runtime
                    .write_usage_update(&session, input, cache_read_input, cache_creation_input)
                    .await
                {
                    crate::logging::warn(&format!("ACP: failed to write usage_update: {err:#}"));
                }
            }
            // IS-4: one background item started or ended.
            ServerEvent::BackgroundWork(work) => self.background_work(&work).await,
            // IS-5: live output of a terminal, whether its call still runs in
            // the foreground or moved to the background.
            ServerEvent::ToolOutputDelta { id, data } => {
                self.write_all(vec![background::output_chunk(&id, &data)]).await;
            }
            // Replies whose waiter is gone, and the selector the side channel owns.
            ServerEvent::Ack { .. }
            | ServerEvent::History { .. }
            | ServerEvent::State { .. }
            | ServerEvent::ModelChanged { .. }
            | ServerEvent::ReasoningEffortChanged { .. }
            | ServerEvent::BackgroundWorkList { .. }
            | ServerEvent::ToolCallCancelResult { .. } => {}
            other => {
                if let ServerEvent::TurnStopped {
                    reason: TurnStopReason::Failure | TurnStopReason::Crash | TurnStopReason::Unknown,
                    message,
                    ..
                } = &other
                {
                    self.v2.tracker().record_failure_note(message);
                }
                if self.run.is_none() && self.outside.is_none() {
                    self.outside = Some(self.mapper_for(&self.session_id).await);
                }
                let mapped = self
                    .run
                    .as_mut()
                    .or(self.outside.as_mut())
                    .map(|mapper| mapper.map_event(other))
                    .unwrap_or_default();
                if mapped.is_empty() {
                    return;
                }
                let mut updates = self.v2.tracker().before_mapped_event();
                updates.extend(mapped);
                self.write_all(updates).await;
            }
        }
    }

    /// IS-4: announce or finalize the item first, then report the list. A
    /// started tool call stays open in the run's mapper until its own end. A
    /// removal while idle waits for a follow-up turn's `running` to carry it.
    async fn background_work(&mut self, work: &crate::protocol::BackgroundWorkEvent) {
        let transition = self.v2.background().apply(work);
        if let Some(call_id) = &transition.hold_open
            && let Some(mapper) = self.run.as_mut()
        {
            mapper.hold_open(call_id);
        }
        let mut updates = transition.updates;
        if transition.list_changed {
            let removal = work.state != "started";
            if removal && !self.v2.tracker().is_active() {
                self.write_all(updates).await;
                *self
                    .removal_due
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(tokio::time::Instant::now() + REMOVAL_DEFER);
                return;
            }
            updates.push(self.v2.tracker().current_state());
        }
        self.write_all(updates).await;
    }

    async fn mapper_for(&self, start: &str) -> V2Mapper {
        V2Mapper::new(start, self.cwd().await)
    }

    async fn cwd(&self) -> Option<PathBuf> {
        match self.session.upgrade() {
            Some(session) => session.cwd.lock().await.clone(),
            None => None,
        }
    }

    /// IS-4: the prompt is answered `{messageId}` at insertion, and its
    /// `user_message` follows the response before anything else of the turn.
    async fn inserted(&self, daemon_id: u64, message_id: String) {
        if !self.v2.is_pending(daemon_id) {
            return;
        }
        // The turn is open before the prompt stops pending, so a close never
        // reads the session as settled in between.
        self.v2.tracker().own_prompt_inserted(daemon_id, &message_id);
        #[cfg(test)]
        self.v2.probe_inserted(daemon_id, self.v2.is_pending(daemon_id));
        let Some(prompt) = self.v2.take_pending(daemon_id) else {
            return;
        };
        if let Err(err) = self
            .runtime
            .write_result(prompt.rpc_id, json!({ "messageId": message_id }))
            .await
        {
            crate::logging::warn(&format!("ACP: failed to answer a v2 prompt: {err:#}"));
        }
        self.write_all(vec![shapes::user_message(&message_id, &prompt.content)])
            .await;
        let _ = prompt.outcome.send(PromptOutcome::Inserted);
    }

    /// The daemon refused a withdrawal. A refusal read while the prompt is
    /// still waiting for its insertion event, or while the turn it started has
    /// not reported idle, leaves a turn that only a daemon `Cancel` stops.
    fn withdraw_refused(&self, withdraw: WithdrawWaiter, message: &str) {
        let verdict = if message == WITHDRAW_ALREADY_INSERTED {
            let daemon_id = withdraw.message_request_id;
            let open = self.v2.is_pending(daemon_id) || self.v2.tracker().own_turn_open(daemon_id);
            Withdrawal::AlreadyInserted { open }
        } else {
            crate::logging::info(&format!(
                "ACP: prompt {} in {} not withdrawn: {message}",
                withdraw.message_request_id, self.session_id
            ));
            Withdrawal::Unsettled
        };
        let _ = withdraw.verdict.send(verdict);
    }

    /// The session takes its next prompt: cleared before the answer that
    /// tells the client so is written.
    fn release_prompt(&self) {
        if let Some(session) = self.session.upgrade() {
            session.prompt_running.store(false, Ordering::SeqCst);
        }
    }

    async fn reject(&self, prompt: PendingPrompt, error: RpcError) {
        self.release_prompt();
        if let Err(err) = self.runtime.write_rpc_error(prompt.rpc_id, error).await {
            crate::logging::warn(&format!("ACP: failed to answer a v2 prompt: {err:#}"));
        }
        let _ = prompt.outcome.send(PromptOutcome::Rejected);
    }

    async fn close(&mut self, give_up: bool) {
        // A prompt's registration and its `message` share the gate, so once
        // `closed` is set here no prompt can register (it answers an error)
        // and none is written to the daemon after the close. Prompts that
        // already wrote their message are the ones drained below.
        {
            let _gate = self.v2.submit_gate.lock().await;
            self.v2.closed.store(true, Ordering::SeqCst);
        }
        let mut updates = Vec::new();
        if give_up {
            // The run's calls end before its synthesized idle update.
            updates.extend(
                self.run
                    .take()
                    .map(|mut mapper| mapper.run_ended(Some("cancelled")))
                    .unwrap_or_default(),
            );
            updates.extend(self.v2.tracker().synthesize_cancelled_idle());
        }
        self.write_all(updates).await;
        for prompt in self.v2.take_all_pending() {
            self.reject(prompt, RpcError::cancelled()).await;
        }
        self.release_prompt();
        self.v2.lock_withdraws().clear();
        self.v2.changed();
    }

    async fn disconnected(&mut self) {
        // Same as `close`: nothing registers or writes a prompt once the
        // stream is gone.
        {
            let _gate = self.v2.submit_gate.lock().await;
            self.v2.closed.store(true, Ordering::SeqCst);
        }
        for prompt in self.v2.take_all_pending() {
            self.reject(prompt, RpcError::new(JSONRPC_INTERNAL_ERROR, DAEMON_DISCONNECTED))
                .await;
        }
        self.release_prompt();
        // The daemon went away mid-run: its open calls failed, and that
        // precedes the idle `error` update.
        let mut updates = Vec::new();
        updates.extend(
            self.run
                .take()
                .map(|mut mapper| mapper.run_ended(Some("error")))
                .unwrap_or_default(),
        );
        updates.extend(
            self.outside
                .take()
                .map(|mut mapper| mapper.run_ended(Some("error")))
                .unwrap_or_default(),
        );
        updates.extend(self.v2.tracker().disconnected());
        self.write_all(updates).await;
        self.v2.lock_withdraws().clear();
        self.v2.changed();
    }
}

/// IS-4: a daemon refusal of a prompt before insertion.
fn pre_insert_error(message: &str) -> RpcError {
    if message.contains("Already processing a message") {
        return RpcError::new(JSONRPC_INVALID_PARAMS, message);
    }
    RpcError::new(JSONRPC_INTERNAL_ERROR, message)
}
