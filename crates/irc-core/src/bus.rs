//! The mailbox bus: send, wait, inbox, and reply obligation tracking.

use crate::mailbox::{State, Waiter, record_mark};
use crate::message::{
    AgentId, Deliver, DeliverResult, Directory, Endpoint, IrcError, IrcLimits, IrcMessage, IrcStats, MessageId,
    Outcome, Receipt, SendReport, SendRequest, Unanswered, WaitFilter, WaitOutcome,
};
use futures::stream::{self, StreamExt};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

/// How often a parked wait re-checks whether it can still be answered.
const RECHECK: Duration = Duration::from_millis(500);

/// Process-local bus shared by every session in one daemon.
pub struct IrcBus {
    limits: IrcLimits,
    directory: Arc<dyn Directory>,
    deliver: Arc<dyn Deliver>,
    state: Mutex<State>,
    counter: AtomicU64,
    waiter_counter: AtomicU64,
}

impl IrcBus {
    pub fn new(limits: IrcLimits, directory: Arc<dyn Directory>, deliver: Arc<dyn Deliver>) -> Arc<Self> {
        Arc::new(Self {
            limits,
            directory,
            deliver,
            state: Mutex::new(State::new()),
            counter: AtomicU64::new(0),
            waiter_counter: AtomicU64::new(0),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn next_id(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Fire-and-forget delivery to one target or, with `to = "all"`, to a broadcast set.
    pub async fn send(&self, req: SendRequest) -> Result<SendReport, IrcError> {
        let body = req.body.trim();
        if body.is_empty() {
            return Err(IrcError::EmptyBody);
        }
        let bytes = body.len();
        if bytes > self.limits.max_body_bytes {
            return Err(IrcError::BodyTooLarge {
                bytes,
                max: self.limits.max_body_bytes,
            });
        }
        let to = req.to.strip_prefix("agent://").unwrap_or(&req.to);
        let broadcast = to == "all";
        let targets = if broadcast {
            if req.progress {
                return Err(IrcError::ProgressBroadcast);
            }
            self.directory
                .broadcast_targets(req.from.as_ref())
                .await
                .map_err(IrcError::Directory)?
        } else {
            let target = self
                .directory
                .resolve(req.from.as_ref(), to)
                .await
                .map_err(IrcError::Directory)?;
            if target.id == req.from {
                return Err(IrcError::SelfSend);
            }
            vec![target]
        };

        let id = MessageId(self.next_id());
        let ts_ms = now_ms();
        let shared: Arc<str> = Arc::from(body);
        {
            let mut state = self.lock();
            state.stats.body_allocations += 1;
            state.stats.sent += 1;
        }

        let legs: Vec<(Endpoint, IrcMessage)> = targets
            .into_iter()
            .map(|target| {
                let message = IrcMessage {
                    id,
                    seq: self.next_id(),
                    from: req.from.clone(),
                    to: target.id.clone(),
                    body: Arc::clone(&shared),
                    ts_ms,
                    reply_to: req.reply_to,
                    broadcast,
                    progress: req.progress,
                    wake_relay: req.wake_relay,
                };
                (target, message)
            })
            .collect();

        let receipts = if broadcast {
            self.deliver_broadcast(legs).await
        } else {
            let mut receipts = Vec::with_capacity(legs.len());
            for (target, message) in legs {
                receipts.push(self.deliver_leg(&target, &message).await);
            }
            receipts
        };

        Ok(SendReport {
            message_id: id,
            receipts,
        })
    }

    async fn deliver_broadcast(&self, legs: Vec<(Endpoint, IrcMessage)>) -> Vec<Receipt> {
        let inflight = Arc::new(AtomicU64::new(0));
        let high_water = Arc::new(AtomicU64::new(0));
        let limit = self.limits.broadcast_concurrency.max(1);
        let mut receipts: Vec<Receipt> = stream::iter(legs)
            .map(|(target, message)| {
                let inflight = Arc::clone(&inflight);
                let high_water = Arc::clone(&high_water);
                async move {
                    let current = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                    high_water.fetch_max(current, Ordering::SeqCst);
                    let receipt = self.deliver_leg(&target, &message).await;
                    inflight.fetch_sub(1, Ordering::SeqCst);
                    receipt
                }
            })
            .buffer_unordered(limit)
            .collect()
            .await;
        let observed = high_water.load(Ordering::SeqCst);
        {
            let mut state = self.lock();
            if observed > state.stats.broadcast_inflight_high_water {
                state.stats.broadcast_inflight_high_water = observed;
            }
        }
        receipts.sort_by(|left, right| left.to.cmp(&right.to));
        receipts
    }

    /// Deliver one message to one target and record the outcome.
    async fn deliver_leg(&self, target: &Endpoint, message: &IrcMessage) -> Receipt {
        let outcome = self.hand_off(target, message).await;
        if !matches!(outcome, Outcome::Failed(_)) {
            let mut state = self.lock();
            if message.progress {
                record_mark(&mut state.last_progress, &message.from, &target.id, message.seq);
            } else {
                record_mark(&mut state.last_sent, &message.from, &target.id, message.seq);
            }
            if !message.broadcast && !message.wake_relay {
                record_mark(&mut state.last_direct, &message.from, &target.id, message.seq);
            }
        }
        Receipt {
            to: target.id.clone(),
            outcome,
        }
    }

    async fn hand_off(&self, target: &Endpoint, message: &IrcMessage) -> Outcome {
        let waiter = {
            let mut state = self.lock();
            state.take_matching_waiter(target.id.as_ref(), message)
        };
        if let Some(waiter) = waiter
            && waiter.tx.send(message.clone()).is_ok()
        {
            return Outcome::Injected;
        }

        let from_parent = target.parent.as_ref().is_some_and(|parent| *parent == message.from);
        match self.deliver.deliver(target, message, from_parent).await {
            DeliverResult::Injected => Outcome::Injected,
            DeliverResult::Woken => Outcome::Woken,
            DeliverResult::Unavailable(reason) => {
                self.forget(target.id.as_ref());
                Outcome::Failed(reason)
            }
            DeliverResult::Errored(reason) => {
                let mut state = self.lock();
                let cap = self.limits.mailbox_cap;
                state.enqueue(message.clone(), cap);
                Outcome::Failed(reason)
            }
        }
    }

    /// Consume the oldest buffered message, optionally only from `from`.
    pub fn take(&self, me: &str, from: Option<&str>) -> Option<IrcMessage> {
        self.lock().take_from_mailbox(me, from)
    }

    /// Consume every buffered message, optionally only from `from`, oldest first.
    pub fn drain(&self, me: &str, from: Option<&str>) -> Vec<IrcMessage> {
        self.lock().drain_from_mailbox(me, from)
    }

    pub fn unread_count(&self, me: &str) -> usize {
        self.lock().unread(me)
    }

    pub fn sent_since(&self, from: &str, to: &str, since_seq: u64) -> bool {
        self.lock().sent_since(from, to, since_seq)
    }

    pub fn unanswered_sends(&self, from: &str) -> Vec<Unanswered> {
        self.lock().unanswered_sends(from)
    }

    /// Drop every trace of `agent`: mailbox, parked waiters, and reply marks.
    pub fn forget(&self, agent: &str) {
        self.lock().forget(agent);
    }

    pub fn limits(&self) -> &IrcLimits {
        &self.limits
    }

    pub fn stats(&self) -> IrcStats {
        self.lock().stats
    }

    pub fn directory(&self) -> &Arc<dyn Directory> {
        &self.directory
    }

    /// Block until a message `filter` accepts arrives, the deadline passes, or `cancel` fires.
    pub async fn wait(
        &self,
        me: &str,
        filter: WaitFilter,
        timeout: Duration,
        cancel: impl Future<Output = ()>,
    ) -> WaitOutcome {
        let timeout = timeout.min(self.limits.wait_max);
        let (waiter_id, mut rx, targets) = {
            let mut state = self.lock();
            if let Some(message) = state.take_accepted(me, &filter) {
                return WaitOutcome::Message(message);
            }
            let unanswered = state.unanswered_sends(me);
            let targets: Vec<AgentId> = match filter.from.as_ref() {
                Some(from) => unanswered
                    .into_iter()
                    .filter(|send| send.to == *from)
                    .map(|send| send.to)
                    .collect(),
                None => unanswered.into_iter().map(|send| send.to).collect(),
            };
            if targets.is_empty() {
                return WaitOutcome::NothingToWaitFor;
            }
            if state.all_targets_wait_on(&targets, me) {
                return WaitOutcome::Unanswerable(targets);
            }
            let id = self.waiter_counter.fetch_add(1, Ordering::Relaxed) + 1;
            let (tx, rx) = oneshot::channel();
            state.register_waiter(
                me,
                Waiter {
                    id,
                    filter,
                    waits_on: targets.clone(),
                    tx,
                },
            );
            (id, rx, targets)
        };

        if self.deliver.has_pending_input(me).await {
            return self.settle(me, waiter_id, &mut rx, WaitOutcome::PendingInput).await;
        }
        if !self.any_target_can_respond(&targets).await {
            return self.settle(me, waiter_id, &mut rx, WaitOutcome::NothingToWaitFor).await;
        }

        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        tokio::pin!(cancel);
        let mut recheck = tokio::time::interval_at(tokio::time::Instant::now() + RECHECK, RECHECK);
        recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                received = &mut rx => {
                    return match received {
                        Ok(message) => WaitOutcome::Message(message),
                        Err(_) => WaitOutcome::NothingToWaitFor,
                    };
                }
                _ = &mut deadline => {
                    return self.settle(me, waiter_id, &mut rx, WaitOutcome::Timeout).await;
                }
                _ = &mut cancel => {
                    return self.settle(me, waiter_id, &mut rx, WaitOutcome::Cancelled).await;
                }
                _ = recheck.tick() => {
                    if self.deliver.has_pending_input(me).await {
                        return self.settle(me, waiter_id, &mut rx, WaitOutcome::PendingInput).await;
                    }
                    if !self.any_target_can_respond(&targets).await {
                        return self.settle(me, waiter_id, &mut rx, WaitOutcome::NothingToWaitFor).await;
                    }
                }
            }
        }
    }

    async fn any_target_can_respond(&self, targets: &[AgentId]) -> bool {
        for target in targets {
            if self.deliver.can_respond(target.as_ref()).await {
                return true;
            }
        }
        false
    }

    /// Unpark the waiter, preferring a message that already reached it over `outcome`.
    async fn settle(
        &self,
        me: &str,
        waiter_id: u64,
        rx: &mut oneshot::Receiver<IrcMessage>,
        outcome: WaitOutcome,
    ) -> WaitOutcome {
        let removed = {
            let mut state = self.lock();
            state.remove_waiter(me, waiter_id)
        };
        if removed {
            return match rx.try_recv() {
                Ok(message) => WaitOutcome::Message(message),
                Err(_) => outcome,
            };
        }
        // A delivery leg claimed the waiter and is handing the message over.
        match rx.await {
            Ok(message) => WaitOutcome::Message(message),
            Err(_) => outcome,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

#[cfg(test)]
#[path = "wait_latency_tests.rs"]
mod wait_latency_tests;
