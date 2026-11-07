//! Bus state: mailboxes, parked waiters, and reply-obligation marks.

use crate::message::{AgentId, IrcMessage, IrcStats, Unanswered, WaitFilter};
use std::collections::{HashMap, VecDeque};
use tokio::sync::oneshot;

/// One parked `wait`, removable by id so a timeout or cancel leaves no trace.
pub(crate) struct Waiter {
    pub id: u64,
    pub filter: WaitFilter,
    pub waits_on: Vec<AgentId>,
    pub tx: oneshot::Sender<IrcMessage>,
}

type MarkTable = HashMap<(AgentId, AgentId), u64>;

/// Everything the bus guards with one mutex. No field is touched across an await.
pub(crate) struct State {
    pub mailboxes: HashMap<AgentId, VecDeque<IrcMessage>>,
    pub waiters: HashMap<AgentId, VecDeque<Waiter>>,
    pub last_sent: MarkTable,
    pub last_direct: MarkTable,
    pub last_progress: MarkTable,
    pub stats: IrcStats,
}

impl State {
    pub fn new() -> Self {
        Self {
            mailboxes: HashMap::new(),
            waiters: HashMap::new(),
            last_sent: HashMap::new(),
            last_direct: HashMap::new(),
            last_progress: HashMap::new(),
            stats: IrcStats::default(),
        }
    }

    /// Remove the oldest parked waiter for `to` whose filter accepts `message`.
    pub fn take_matching_waiter(&mut self, to: &str, message: &IrcMessage) -> Option<Waiter> {
        let queue = self.waiters.get_mut(to)?;
        let index = queue.iter().position(|waiter| waiter.filter.accepts(message))?;
        let found = queue.remove(index)?;
        if queue.is_empty() {
            self.waiters.remove(to);
        }
        self.sync_waiters_live();
        Some(found)
    }

    /// Hand `message` to the oldest matching waiter, skipping waiters whose
    /// receiver is already gone. Returns whether a waiter took the message.
    pub fn deliver_to_waiter(&mut self, message: &IrcMessage) -> bool {
        loop {
            let Some(waiter) = self.take_matching_waiter(message.to.as_ref(), message) else {
                return false;
            };
            if waiter.tx.send(message.clone()).is_ok() {
                return true;
            }
        }
    }

    pub fn register_waiter(&mut self, me: &str, waiter: Waiter) {
        self.waiters.entry(me.into()).or_default().push_back(waiter);
        self.sync_waiters_live();
    }

    /// Take the oldest buffered message that `filter` accepts.
    pub fn take_accepted(&mut self, me: &str, filter: &WaitFilter) -> Option<IrcMessage> {
        let mailbox = self.mailboxes.get_mut(me)?;
        let index = mailbox.iter().position(|message| filter.accepts(message))?;
        let message = mailbox.remove(index)?;
        if mailbox.is_empty() {
            self.mailboxes.remove(me);
        }
        Some(message)
    }

    /// Remove one parked waiter; returns whether this call removed it.
    pub fn remove_waiter(&mut self, me: &str, id: u64) -> bool {
        let removed = match self.waiters.get_mut(me) {
            Some(queue) => {
                let found = queue.iter().position(|waiter| waiter.id == id).is_some();
                if found {
                    queue.retain(|waiter| waiter.id != id);
                }
                if queue.is_empty() {
                    self.waiters.remove(me);
                }
                found
            }
            None => false,
        };
        self.sync_waiters_live();
        removed
    }

    fn sync_waiters_live(&mut self) {
        self.stats.waiters_live = self.waiters.values().map(VecDeque::len).sum::<usize>() as u64;
    }

    /// Whether every target already has a waiter that is itself waiting on `me`.
    pub fn all_targets_wait_on(&self, targets: &[AgentId], me: &str) -> bool {
        !targets.is_empty()
            && targets.iter().all(|target| {
                self.waiters.get(target).is_some_and(|queue| {
                    queue
                        .iter()
                        .any(|waiter| waiter.waits_on.iter().any(|waiting| waiting.as_ref() == me))
                })
            })
    }

    pub fn unread(&self, me: &str) -> usize {
        self.mailboxes.get(me).map_or(0, VecDeque::len)
    }

    /// Buffer a message whose live hand-off failed, dropping the oldest beyond `cap`.
    pub fn enqueue(&mut self, message: IrcMessage, cap: usize) {
        let mailbox = self.mailboxes.entry(message.to.clone()).or_default();
        mailbox.push_back(message);
        if mailbox.len() > cap {
            mailbox.pop_front();
            self.stats.mailbox_dropped += 1;
        }
        let len = mailbox.len() as u64;
        if len > self.stats.mailbox_high_water {
            self.stats.mailbox_high_water = len;
        }
    }

    /// Buffer a recovered message at its sequence position so a sender's
    /// messages keep their order and `inbox` still reads oldest first.
    ///
    /// The cap still drops from the front, so the oldest message wins over the
    /// newest when the mailbox is full.
    pub fn enqueue_recovered(&mut self, message: IrcMessage, cap: usize) {
        let mailbox = self.mailboxes.entry(message.to.clone()).or_default();
        let index = mailbox
            .iter()
            .position(|queued| queued.seq > message.seq)
            .unwrap_or(mailbox.len());
        mailbox.insert(index, message);
        if mailbox.len() > cap {
            mailbox.pop_front();
            self.stats.mailbox_dropped += 1;
        }
        let len = mailbox.len() as u64;
        if len > self.stats.mailbox_high_water {
            self.stats.mailbox_high_water = len;
        }
    }

    pub fn take_from_mailbox(&mut self, me: &str, from: Option<&str>) -> Option<IrcMessage> {
        let mailbox = self.mailboxes.get_mut(me)?;
        let index = mailbox
            .iter()
            .position(|message| from.is_none_or(|sender| message.from.as_ref() == sender))?;
        let message = mailbox.remove(index)?;
        if mailbox.is_empty() {
            self.mailboxes.remove(me);
        }
        Some(message)
    }

    pub fn drain_from_mailbox(&mut self, me: &str, from: Option<&str>) -> Vec<IrcMessage> {
        let Some(mailbox) = self.mailboxes.remove(me) else {
            return Vec::new();
        };
        let mut kept = VecDeque::new();
        let mut taken = Vec::new();
        for message in mailbox {
            if from.is_none_or(|sender| message.from.as_ref() == sender) {
                taken.push(message);
            } else {
                kept.push_back(message);
            }
        }
        if !kept.is_empty() {
            self.mailboxes.insert(me.into(), kept);
        }
        taken
    }

    /// Targets `from` messaged directly that have not answered since.
    pub fn unanswered_sends(&self, from: &str) -> Vec<Unanswered> {
        let mut unanswered = Vec::new();
        for ((sender, to), seq) in &self.last_direct {
            if sender.as_ref() != from {
                continue;
            }
            let key = (to.clone(), sender.clone());
            let answered = self.last_sent.get(&key).is_some_and(|reply| reply > seq);
            if answered {
                continue;
            }
            let promised = self.last_progress.get(&key).is_some_and(|promise| promise > seq);
            unanswered.push(Unanswered {
                to: to.clone(),
                seq: *seq,
                promised,
            });
        }
        unanswered.sort_by(|left, right| left.to.cmp(&right.to));
        unanswered
    }

    /// Whether `from` sent `to` anything, progress included, after `since_seq`.
    pub fn sent_since(&self, from: &str, to: &str, since_seq: u64) -> bool {
        let key = |table: &MarkTable| table.get(&(from.into(), to.into())).is_some_and(|seq| *seq > since_seq);
        key(&self.last_sent) || key(&self.last_progress)
    }

    pub fn forget(&mut self, agent: &str) {
        self.mailboxes.remove(agent);
        self.waiters.remove(agent);
        let keep = |(from, to): &(AgentId, AgentId)| from.as_ref() != agent && to.as_ref() != agent;
        self.last_sent.retain(|key, _| keep(key));
        self.last_direct.retain(|key, _| keep(key));
        self.last_progress.retain(|key, _| keep(key));
        self.sync_waiters_live();
    }
}

/// Record a send mark, keeping the newer sequence when legs settle out of order.
pub(crate) fn record_mark(table: &mut MarkTable, from: &AgentId, to: &AgentId, seq: u64) {
    let entry = table.entry((from.clone(), to.clone())).or_insert(0);
    if seq > *entry {
        *entry = seq;
    }
}
