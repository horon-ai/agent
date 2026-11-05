//! Shared test doubles: an in-memory directory and a scriptable delivery double.

//! Bus behaviour tests with an in-memory directory and delivery double.

use crate::IrcBus;
use crate::message::{
    Deliver, DeliverResult, Directory, Endpoint, IrcError, IrcLimits, IrcMessage, SendRequest, WaitFilter, WaitOutcome,
};
use futures::future::BoxFuture;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy)]
pub(crate) enum Mode {
    Injected,
    Woken,
    Errored,
    Unavailable,
}

pub(crate) struct Inner {
    agents: Vec<Endpoint>,
    handles: HashMap<String, String>,
    modes: HashMap<String, Mode>,
    calls: Vec<(String, bool)>,
    pending: HashSet<String>,
    responders: HashSet<String>,
    broadcast: Vec<Endpoint>,
    delay_ms: u64,
    bus: Option<Arc<IrcBus>>,
    reply_on_pending: Option<(String, String, String)>,
}

pub(crate) struct World {
    pub(crate) inner: Mutex<Inner>,
}

impl World {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                agents: Vec::new(),
                handles: HashMap::new(),
                modes: HashMap::new(),
                calls: Vec::new(),
                pending: HashSet::new(),
                responders: HashSet::new(),
                broadcast: Vec::new(),
                delay_ms: 0,
                bus: None,
                reply_on_pending: None,
            }),
        })
    }

    fn mode_of(inner: &Inner, id: &str) -> Mode {
        inner.modes.get(id).copied().unwrap_or(Mode::Woken)
    }

    pub(crate) fn result(mode: Mode) -> DeliverResult {
        match mode {
            Mode::Injected => DeliverResult::Injected,
            Mode::Woken => DeliverResult::Woken,
            Mode::Errored => DeliverResult::Errored("hand-off failed".into()),
            Mode::Unavailable => DeliverResult::Unavailable("no live session".into()),
        }
    }
}

impl Directory for World {
    fn resolve<'a>(&'a self, _sender: &'a str, target: &'a str) -> BoxFuture<'a, Result<Endpoint, String>> {
        Box::pin(async move {
            let inner = self.inner.lock().unwrap();
            if let Some(agent) = inner.agents.iter().find(|agent| agent.id.as_ref() == target) {
                return Ok(clone_endpoint(agent));
            }
            let id = inner
                .handles
                .get(target)
                .ok_or_else(|| format!("unknown \"{target}\""))?;
            let agent = inner
                .agents
                .iter()
                .find(|agent| agent.id.as_ref() == id)
                .ok_or_else(|| format!("unknown \"{target}\""))?;
            Ok(clone_endpoint(agent))
        })
    }

    fn broadcast_targets<'a>(&'a self, sender: &'a str) -> BoxFuture<'a, Result<Vec<Endpoint>, String>> {
        Box::pin(async move {
            let inner = self.inner.lock().unwrap();
            let root = inner
                .agents
                .iter()
                .any(|agent| agent.id.as_ref() == sender && agent.parent.is_none());
            if !root {
                return Err("only a root session can broadcast".into());
            }
            Ok(inner.broadcast.iter().map(clone_endpoint).collect())
        })
    }

    fn roster<'a>(&'a self, _sender: &'a str) -> BoxFuture<'a, Vec<Endpoint>> {
        Box::pin(async move {
            let inner = self.inner.lock().unwrap();
            inner.agents.iter().map(clone_endpoint).collect()
        })
    }
}

impl Deliver for World {
    fn deliver<'a>(
        &'a self,
        target: &'a Endpoint,
        _message: &'a IrcMessage,
        from_parent: bool,
    ) -> BoxFuture<'a, DeliverResult> {
        Box::pin(async move {
            let (mode, delay) = {
                let mut inner = self.inner.lock().unwrap();
                inner.calls.push((target.id.to_string(), from_parent));
                (World::mode_of(&inner, target.id.as_ref()), inner.delay_ms)
            };
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            World::result(mode)
        })
    }

    fn has_pending_input<'a>(&'a self, agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let (pending, reply, bus) = {
                let mut inner = self.inner.lock().unwrap();
                let pending = inner.pending.contains(agent);
                let reply = inner.reply_on_pending.take();
                (pending, reply, inner.bus.clone())
            };
            if let Some((from, to, body)) = reply
                && let Some(bus) = bus
            {
                let sent = bus
                    .send(SendRequest {
                        from: from.into(),
                        to,
                        body,
                        reply_to: None,
                        progress: false,
                        wake_relay: false,
                    })
                    .await;
                assert!(sent.is_ok(), "injected reply must be accepted");
            }
            pending
        })
    }

    fn can_respond<'a>(&'a self, agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move { self.inner.lock().unwrap().responders.contains(agent) })
    }
}

fn clone_endpoint(endpoint: &Endpoint) -> Endpoint {
    Endpoint {
        id: Arc::clone(&endpoint.id),
        handle: endpoint.handle.as_ref().map(Arc::clone),
        parent: endpoint.parent.as_ref().map(Arc::clone),
        status: Arc::clone(&endpoint.status),
    }
}

fn agent(id: &str) -> Endpoint {
    Endpoint {
        id: Arc::from(id),
        handle: None,
        parent: None,
        status: Arc::from("running"),
    }
}

pub(crate) struct Harness {
    pub(crate) world: Arc<World>,
    pub(crate) bus: Arc<IrcBus>,
}

pub(crate) fn harness(limits: IrcLimits) -> Harness {
    let world = World::new();
    let bus = IrcBus::new(
        limits,
        Arc::clone(&world) as Arc<dyn Directory>,
        Arc::clone(&world) as Arc<dyn Deliver>,
    );
    world.inner.lock().unwrap().bus = Some(Arc::clone(&bus));
    Harness { world, bus }
}

impl Harness {
    pub(crate) fn add(self, id: &str) -> Self {
        self.add_endpoint(agent(id))
    }

    pub(crate) fn add_endpoint(self, endpoint: Endpoint) -> Self {
        self.world.inner.lock().unwrap().agents.push(endpoint);
        self
    }

    pub(crate) fn handle(self, id: &str, handle: &str) -> Self {
        self.world
            .inner
            .lock()
            .unwrap()
            .handles
            .insert(handle.to_string(), id.to_string());
        self
    }

    pub(crate) fn parent(self, id: &str, parent: &str) -> Self {
        let mut inner = self.world.inner.lock().unwrap();
        if let Some(agent) = inner.agents.iter_mut().find(|agent| agent.id.as_ref() == id) {
            agent.parent = Some(Arc::from(parent));
        }
        drop(inner);
        self
    }

    pub(crate) fn mode(self, id: &str, mode: Mode) -> Self {
        self.world.inner.lock().unwrap().modes.insert(id.to_string(), mode);
        self
    }

    pub(crate) fn responder(self, id: &str, can: bool) -> Self {
        let mut inner = self.world.inner.lock().unwrap();
        if can {
            inner.responders.insert(id.to_string());
        } else {
            inner.responders.remove(id);
        }
        drop(inner);
        self
    }

    pub(crate) fn pending(self, id: &str) -> Self {
        self.world.inner.lock().unwrap().pending.insert(id.to_string());
        self
    }

    pub(crate) fn reply_on_pending(self, from: &str, to: &str, body: &str) -> Self {
        self.world.inner.lock().unwrap().reply_on_pending = Some((from.to_string(), to.to_string(), body.to_string()));
        self
    }

    pub(crate) fn broadcast(self, targets: &[&str]) -> Self {
        self.world.inner.lock().unwrap().broadcast = targets.iter().map(|id| agent(id)).collect();
        self
    }

    pub(crate) fn delay(self, ms: u64) -> Self {
        self.world.inner.lock().unwrap().delay_ms = ms;
        self
    }

    pub(crate) fn calls(&self) -> Vec<(String, bool)> {
        self.world.inner.lock().unwrap().calls.clone()
    }

    pub(crate) fn clear_calls(&self) {
        self.world.inner.lock().unwrap().calls.clear();
    }

    pub(crate) async fn send(&self, from: &str, to: &str, body: &str) -> Result<crate::SendReport, IrcError> {
        self.bus
            .send(SendRequest {
                from: Arc::from(from),
                to: to.to_string(),
                body: body.to_string(),
                reply_to: None,
                progress: false,
                wake_relay: false,
            })
            .await
    }

    pub(crate) async fn send_progress(&self, from: &str, to: &str, body: &str) -> Result<crate::SendReport, IrcError> {
        self.bus
            .send(SendRequest {
                from: Arc::from(from),
                to: to.to_string(),
                body: body.to_string(),
                reply_to: None,
                progress: true,
                wake_relay: false,
            })
            .await
    }

    pub(crate) async fn send_broadcast(&self, from: &str, body: &str) -> Result<crate::SendReport, IrcError> {
        self.bus
            .send(SendRequest {
                from: Arc::from(from),
                to: "all".to_string(),
                body: body.to_string(),
                reply_to: None,
                progress: false,
                wake_relay: false,
            })
            .await
    }
}

pub(crate) fn direct() -> WaitFilter {
    WaitFilter {
        from: None,
        direct_only: true,
    }
}

pub(crate) fn describe(outcome: &WaitOutcome) -> &'static str {
    match outcome {
        WaitOutcome::Message(_) => "message",
        WaitOutcome::Timeout => "timeout",
        WaitOutcome::NothingToWaitFor => "nothing",
        WaitOutcome::Unanswerable(_) => "unanswerable",
        WaitOutcome::PendingInput => "pending",
        WaitOutcome::Cancelled => "cancelled",
    }
}

pub(crate) fn from_peer(peer: &str) -> WaitFilter {
    WaitFilter {
        from: Some(Arc::from(peer)),
        direct_only: true,
    }
}

pub(crate) fn never() -> impl Future<Output = ()> {
    std::future::pending()
}
