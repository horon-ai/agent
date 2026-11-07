//! Measure IRC bus throughput on the waiter, delivery, and broadcast paths.
//!
//! ```text
//! cargo run --release -p horon-agent-irc-core --example irc_throughput
//! IRC_THROUGHPUT_MESSAGES=10000 cargo run --release -p horon-agent-irc-core --example irc_throughput
//! ```
//!
//! Prints exactly one `IRC_THROUGHPUT` line per scenario, and nothing else on
//! stdout. `direct_waiter` parks 64 receivers on `wait` and hands each of them its
//! share of the messages. `direct_deliver` hands every message to a delivery double
//! that injects immediately. `broadcast64` sends `messages / 64` broadcasts of 64
//! targets each, so its `messages` field counts delivered legs (a multiple of 64).
//!
//! `IRC_THROUGHPUT_MESSAGES` (default 100000) must be a positive integer; anything
//! else exits 2 so a benchmark run cannot silently measure a different workload.

use futures::future::BoxFuture;
use horon_agent_irc_core::{
    AgentId, Deliver, DeliverResult, Directory, Endpoint, IrcBus, IrcLimits, IrcMessage, SendRequest, WaitFilter,
    WaitOutcome,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const DEFAULT_MESSAGES: usize = 100_000;
const BROADCAST_TARGETS: usize = 64;
const WAITER_PAIRS: usize = 64;
const WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(300);

fn main() {
    let messages = match std::env::var("IRC_THROUGHPUT_MESSAGES") {
        Ok(raw) => match parse_messages(&raw) {
            Some(value) => value,
            None => {
                eprintln!("messages must be positive");
                std::process::exit(2);
            }
        },
        Err(_) => DEFAULT_MESSAGES,
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("failed to start the tokio runtime: {error}");
            std::process::exit(1);
        }
    };
    match runtime.block_on(run(messages)) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
        }
        Err(error) => {
            eprintln!("irc_throughput: {error}");
            std::process::exit(1);
        }
    }
}

fn parse_messages(raw: &str) -> Option<usize> {
    match raw.trim().parse::<usize>() {
        Ok(value) if value > 0 => Some(value),
        _ => None,
    }
}

async fn run(messages: usize) -> Result<Vec<String>, String> {
    let (waiter_messages, waiter_elapsed) = direct_waiter(messages).await?;
    let (deliver_messages, deliver_elapsed) = direct_deliver(messages).await?;
    let (broadcast_messages, broadcast_elapsed) = broadcast64(messages).await?;
    Ok(vec![
        report_line("direct_waiter", waiter_messages, waiter_elapsed),
        report_line("direct_deliver", deliver_messages, deliver_elapsed),
        report_line("broadcast64", broadcast_messages, broadcast_elapsed),
    ])
}

fn report_line(scenario: &str, messages: u64, elapsed: Duration) -> String {
    let micros = elapsed.as_micros().max(1) as u64;
    let rate = messages.saturating_mul(1_000_000) / micros;
    format!("IRC_THROUGHPUT scenario={scenario} messages={messages} elapsed_us={micros} msgs_per_sec={rate}")
}

// A `wait` only keeps a target while the caller still owes that target a reply, so
// every receiver re-arms one direct send per received message. The first arm runs
// before the clock starts. Rounds are aligned on `waiters_live` so each message
// meets a parked waiter instead of the delivery double.
async fn direct_waiter(messages: usize) -> Result<(u64, Duration), String> {
    let bus = IrcBus::new(
        IrcLimits::default(),
        Arc::new(MapDirectory::new()),
        Arc::new(InstantDeliver::new()),
    );
    let base = messages / WAITER_PAIRS;
    let extra = messages % WAITER_PAIRS;
    let mut pairs = Vec::with_capacity(WAITER_PAIRS);
    for index in 0..WAITER_PAIRS {
        let receiver: AgentId = Arc::from(format!("receiver-{index}"));
        let sender: AgentId = Arc::from(format!("sender-{index}"));
        direct_send(&bus, &receiver, sender.as_ref(), "reply obligation").await?;
        pairs.push((receiver, sender, base + usize::from(index < extra)));
    }

    let started = Instant::now();
    let work = async {
        let mut receivers = Vec::with_capacity(WAITER_PAIRS);
        for (receiver, sender, rounds) in &pairs {
            receivers.push(tokio::spawn(receiver_loop(
                Arc::clone(&bus),
                receiver.clone(),
                sender.clone(),
                *rounds,
            )));
        }
        let mut remaining: Vec<(usize, usize)> = pairs
            .iter()
            .enumerate()
            .filter(|(_, (_, _, rounds))| *rounds > 0)
            .map(|(index, (_, _, rounds))| (index, *rounds))
            .collect();
        while !remaining.is_empty() {
            wait_until_parked(&bus, remaining.len()).await?;
            let mut still_running = Vec::with_capacity(remaining.len());
            for (index, rounds) in remaining {
                let (receiver, sender, _) = &pairs[index];
                direct_send(&bus, sender, receiver.as_ref(), "message").await?;
                if rounds > 1 {
                    still_running.push((index, rounds - 1));
                }
            }
            remaining = still_running;
        }
        for handle in receivers {
            match handle.await {
                Ok(result) => result?,
                Err(error) => return Err(format!("waiter task failed: {error}")),
            }
        }
        Ok::<(), String>(())
    };
    tokio::time::timeout(SCENARIO_TIMEOUT, work)
        .await
        .map_err(|_| String::from("direct_waiter exceeded its scenario timeout"))??;
    Ok((messages as u64, started.elapsed()))
}

async fn receiver_loop(bus: Arc<IrcBus>, receiver: AgentId, sender: AgentId, rounds: usize) -> Result<(), String> {
    for index in 0..rounds {
        if index > 0 {
            direct_send(&bus, &receiver, sender.as_ref(), "reply obligation").await?;
        }
        let outcome = bus
            .wait(
                receiver.as_ref(),
                WaitFilter {
                    from: Some(sender.clone()),
                    direct_only: true,
                },
                WAIT_TIMEOUT,
                std::future::pending::<()>(),
            )
            .await;
        match outcome {
            WaitOutcome::Message(_) => {}
            other => {
                return Err(format!(
                    "{} stopped waiting early: {}",
                    receiver.as_ref(),
                    describe_wait(&other)
                ));
            }
        }
    }
    Ok(())
}

async fn wait_until_parked(bus: &IrcBus, expected: usize) -> Result<(), String> {
    let deadline = Instant::now() + SCENARIO_TIMEOUT;
    loop {
        if bus.stats().waiters_live >= expected as u64 {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!(
                "only {} of {expected} waiters parked before the scenario deadline",
                bus.stats().waiters_live
            ));
        }
        tokio::task::yield_now().await;
    }
}

async fn direct_deliver(messages: usize) -> Result<(u64, Duration), String> {
    let deliver = Arc::new(InstantDeliver::new());
    let bus = IrcBus::new(
        IrcLimits::default(),
        Arc::new(MapDirectory::new()),
        Arc::clone(&deliver) as Arc<dyn Deliver>,
    );
    let sender: AgentId = Arc::from("sender");

    let started = Instant::now();
    for _ in 0..messages {
        direct_send(&bus, &sender, "receiver", "message").await?;
    }
    let elapsed = started.elapsed();

    let delivered = deliver.calls();
    if delivered != messages as u64 {
        return Err(format!(
            "direct_deliver reached the delivery double {delivered} times, expected {messages}"
        ));
    }
    Ok((messages as u64, elapsed))
}

async fn broadcast64(messages: usize) -> Result<(u64, Duration), String> {
    let bus = IrcBus::new(
        IrcLimits::default(),
        Arc::new(MapDirectory::new()),
        Arc::new(InstantDeliver::new()),
    );
    let sender: AgentId = Arc::from("root");
    let rounds = (messages / BROADCAST_TARGETS).max(1);
    let legs = rounds * BROADCAST_TARGETS;
    let body = String::from("broadcast");

    let started = Instant::now();
    for _ in 0..rounds {
        let report = bus
            .send(SendRequest {
                from: sender.clone(),
                to: String::from("all"),
                body: body.clone(),
                reply_to: None,
                progress: false,
                wake_relay: false,
            })
            .await
            .map_err(|error| format!("broadcast failed: {error}"))?;
        if report.delivered() != BROADCAST_TARGETS {
            return Err(format!(
                "broadcast reached {} targets, expected {BROADCAST_TARGETS}",
                report.delivered()
            ));
        }
    }
    let elapsed = started.elapsed();
    Ok((legs as u64, elapsed))
}

async fn direct_send(bus: &IrcBus, from: &AgentId, to: &str, body: &str) -> Result<(), String> {
    let report = bus
        .send(SendRequest {
            from: from.clone(),
            to: to.to_string(),
            body: body.to_string(),
            reply_to: None,
            progress: false,
            wake_relay: false,
        })
        .await
        .map_err(|error| format!("send to {to} failed: {error}"))?;
    if report.delivered() != 1 {
        return Err(format!(
            "send to {to} delivered to {} targets, expected 1",
            report.delivered()
        ));
    }
    Ok(())
}

fn describe_wait(outcome: &WaitOutcome) -> String {
    match outcome {
        WaitOutcome::Message(message) => format!("unexpected message from {}", message.from),
        WaitOutcome::Timeout => String::from("timeout"),
        WaitOutcome::NothingToWaitFor => String::from("nothing to wait for"),
        WaitOutcome::Unanswerable(targets) => format!("unanswerable by {} targets", targets.len()),
        WaitOutcome::PendingInput => String::from("pending input"),
        WaitOutcome::Cancelled => String::from("cancelled"),
    }
}

fn endpoint(id: &str) -> Endpoint {
    Endpoint {
        id: Arc::from(id),
        handle: None,
        parent: None,
        status: Arc::from("ready"),
    }
}

struct MapDirectory {
    agents: Vec<AgentId>,
}

impl MapDirectory {
    fn new() -> Self {
        let mut agents: Vec<AgentId> = Vec::with_capacity(WAITER_PAIRS * 2 + BROADCAST_TARGETS + 2);
        agents.push(Arc::from("receiver"));
        agents.push(Arc::from("sender"));
        agents.push(Arc::from("root"));
        for index in 0..WAITER_PAIRS {
            agents.push(Arc::from(format!("receiver-{index}")));
            agents.push(Arc::from(format!("sender-{index}")));
        }
        for index in 0..BROADCAST_TARGETS {
            agents.push(Arc::from(format!("peer-{index}")));
        }
        Self { agents }
    }
}

impl Directory for MapDirectory {
    fn resolve<'a>(&'a self, _sender: &'a str, target: &'a str) -> BoxFuture<'a, Result<Endpoint, String>> {
        let known = self.agents.iter().any(|agent| agent.as_ref() == target);
        Box::pin(async move {
            if known {
                Ok(endpoint(target))
            } else {
                Err(format!("unknown target: {target}"))
            }
        })
    }

    fn broadcast_targets<'a>(&'a self, sender: &'a str) -> BoxFuture<'a, Result<Vec<Endpoint>, String>> {
        let targets: Vec<Endpoint> = self
            .agents
            .iter()
            .filter(|agent| agent.as_ref() != sender && agent.starts_with("peer-"))
            .map(|agent| endpoint(agent))
            .collect();
        Box::pin(async move { Ok(targets) })
    }

    fn roster<'a>(&'a self, sender: &'a str) -> BoxFuture<'a, Vec<Endpoint>> {
        let targets: Vec<Endpoint> = self
            .agents
            .iter()
            .filter(|agent| agent.as_ref() != sender)
            .map(|agent| endpoint(agent))
            .collect();
        Box::pin(async move { targets })
    }
}

struct InstantDeliver {
    calls: AtomicU64,
}

impl InstantDeliver {
    fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
        }
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }
}

impl Deliver for InstantDeliver {
    fn deliver<'a>(
        &'a self,
        _target: &'a Endpoint,
        _message: &'a IrcMessage,
        _from_parent: bool,
    ) -> BoxFuture<'a, DeliverResult> {
        let calls = &self.calls;
        Box::pin(async move {
            calls.fetch_add(1, Ordering::Relaxed);
            DeliverResult::Injected
        })
    }

    fn has_pending_input<'a>(&'a self, _agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn can_respond<'a>(&'a self, _agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async { true })
    }
}
