//! Attach/detach soak: 1,000 attach/detach cycles plus repeated turn/close
//! cycles, sampled only at quiescence.
//!
//! The retained slope is the point, not the total. A leak of one descriptor per
//! ten cycles adds ten per 100-cycle window, so every counter is compared
//! window-to-window and the largest single-window growth is checked separately
//! from end-to-end retention: ending 12 FDs up but flat after cycle 100 is
//! warm-up, adding 3 FDs per window is a leak.
//!
//! `shared_context` has no removal path (`crates/app-core/src/server.rs:771`), so
//! it is measured and reported but never asserted.

use crate::test_support::*;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Default run length. Measured on an Apple M1 Max: 2.7-3.1 s per cycle (100
/// cycles in 267 s, 200 in 628 s), so 200 cycles costs about 9-10 minutes. With
/// `TARGET_WINDOWS = 10` a window is 20 cycles, which resolves a leak of one
/// descriptor per 20 cycles - enough for a per-request or per-session leak,
/// the only kind that can grow here.
const CYCLES: usize = 200;
/// The run is cut into this many equal windows so every run yields a slope, even
/// a short one: `window = max(1, cycles / TARGET_WINDOWS)`.
const TARGET_WINDOWS: usize = 10;

fn cycles_from_env() -> usize {
    std::env::var("HORON_AGENT_SOAK_CYCLES")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(CYCLES)
}

/// A session is reused only after seven others, so a cycle never races the
/// disconnect cleanup of the session it attaches to.
const ROTATING_SESSIONS: usize = 8;

const FD_RETENTION_BUDGET: f64 = 8.0;
const THREAD_RETENTION_BUDGET: f64 = 4.0;
/// Counts of things that must be gone once every client has detached.
const GAUGE_BUDGET: f64 = 0.0;
/// Cumulative totals may not grow at all across the run.
const CUMULATIVE_GROWTH_BUDGET: f64 = 0.0;
/// The tool round sleeps 250 ms against a 25 ms poll, so one round legitimately
/// adds about ten wakeups; this bound leaves scheduler slack and still catches a
/// poll that runs away.
const INTERRUPT_POLL_GROWTH_BUDGET: f64 = 200.0;

#[derive(Default)]
struct ProviderState {
    /// How many tool calls the next assistant message carries; 0 is a plain turn.
    tool_calls_in_round: AtomicUsize,
    /// Silent stretch before the first event, in milliseconds. A provider call or
    /// a slow tool step is exactly this, and it must not fail a read deadline.
    first_event_delay_ms: AtomicUsize,
    tool_calls: AtomicUsize,
    total_calls: AtomicUsize,
}

impl ProviderState {
    fn begin_turn(&self, tool_calls_in_round: usize) {
        self.tool_calls_in_round.store(tool_calls_in_round, Ordering::SeqCst);
        self.first_event_delay_ms.store(0, Ordering::SeqCst);
        self.tool_calls.store(0, Ordering::SeqCst);
    }

    fn tool_calls(&self) -> usize {
        self.tool_calls.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct SoakProvider {
    state: Arc<ProviderState>,
    tool_fixture: String,
}

#[async_trait]
impl Provider for SoakProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume: Option<&str>,
    ) -> Result<EventStream> {
        self.state.total_calls.fetch_add(1, Ordering::SeqCst);
        let delay_ms = self.state.first_event_delay_ms.load(Ordering::SeqCst);
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms as u64)).await;
        }
        let round_calls = self.state.tool_calls_in_round.load(Ordering::SeqCst);
        if round_calls > 0 && self.state.tool_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            // Each call sleeps 250 ms, which is long enough for several 25 ms
            // interrupt-poll ticks. The call count matters: a one-member group
            // runs inline in the daemon and never reaches the polling group
            // runner, so the count is what the wakeup number is really about.
            let mut events = Vec::new();
            for index in 0..round_calls {
                // `read` is a Concurrent tool, so two of them form ONE multi-member
                // group. Two `bash` calls would be two one-member groups, which the
                // daemon runs inline and which therefore never poll.
                events.push(Ok(StreamEvent::ToolUseStart {
                    id: format!("soak-read-{index}"),
                    name: "read".to_string(),
                }));
                events.push(Ok(StreamEvent::ToolInputDelta(
                    json!({"file_path": self.tool_fixture, "intent": "soak tool round"}).to_string(),
                )));
                events.push(Ok(StreamEvent::ToolUseEnd));
            }
            events.push(Ok(StreamEvent::MessageEnd {
                stop_reason: Some("tool_use".to_string()),
            }));
            return Ok(Box::pin(stream::iter(events)));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::TextDelta("soak reply".to_string())),
            Ok(StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            }),
        ])))
    }

    fn name(&self) -> &str {
        "soak"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct SoakSample {
    cycle: usize,
    fd_count: Option<u64>,
    thread_count: Option<u64>,
    /// False means the stage counters were switched off, so their zeros are "not
    /// measured" rather than "measured zero".
    stage_counters_enabled: bool,
    queue_depth_high_water: u64,
    pending_requests: u64,
    interrupt_poll_wakeups: u64,
    background_tasks: u64,
    shared_context_entries: u64,
    live_sessions: u64,
    connected_clients: u64,
    rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct TurnRound {
    index: usize,
    /// Tool calls the round carried. 1 takes the daemon's inline path, 2 or more
    /// the polling group path.
    tool_calls_in_round: usize,
    elapsed_ms: u128,
    provider_calls: usize,
    outcome: String,
    /// Name and error of every tool the round completed, so a round that never
    /// dispatched is visible instead of being counted as one that ran.
    tool_outcomes: Vec<String>,
    /// Wakeup growth measured across this one turn.
    wakeup_delta: u64,
    /// Taken while the tool round is still open, to show that a mid-flight read
    /// is not the quiescent one.
    mid_flight: Option<SoakSample>,
}

struct CounterSeries {
    name: &'static str,
    unit: &'static str,
    /// Cycles each window covers, so the slope is always quoted per 100 cycles
    /// regardless of how the run was sliced.
    window_cycles: f64,
    values: Vec<Option<f64>>,
}

impl CounterSeries {
    fn new(
        samples: &[SoakSample],
        name: &'static str,
        unit: &'static str,
        window_cycles: usize,
        pick: impl Fn(&SoakSample) -> Option<u64>,
    ) -> Self {
        Self {
            name,
            unit,
            window_cycles: window_cycles.max(1) as f64,
            values: samples
                .iter()
                .map(|sample| pick(sample).map(|value| value as f64))
                .collect(),
        }
    }

    fn complete(&self) -> bool {
        self.values.iter().all(Option::is_some)
    }

    fn baseline(&self) -> Option<f64> {
        self.values.first().copied().flatten()
    }

    fn final_value(&self) -> Option<f64> {
        self.values.last().copied().flatten()
    }

    fn window_growth(&self) -> Vec<Option<f64>> {
        self.values
            .windows(2)
            .map(|pair| match (pair[0], pair[1]) {
                (Some(previous), Some(next)) => Some(next - previous),
                _ => None,
            })
            .collect()
    }

    fn max_window_growth(&self) -> Option<f64> {
        self.window_growth()
            .into_iter()
            .flatten()
            .fold(None, |max: Option<f64>, growth| {
                Some(max.map_or(growth, |max| max.max(growth)))
            })
    }

    fn slope_per_100_cycles(&self) -> Option<f64> {
        let (baseline, final_value) = (self.baseline()?, self.final_value()?);
        let windows = self.values.len().checked_sub(1)?;
        (windows > 0).then(|| (final_value - baseline) / windows as f64 * (100.0 / self.window_cycles))
    }

    fn describe(&self) -> String {
        let baseline = self
            .baseline()
            .map_or_else(|| "n/a".to_string(), |value| format!("{value}"));
        let final_value = self
            .final_value()
            .map_or_else(|| "n/a".to_string(), |value| format!("{value}"));
        let slope = self
            .slope_per_100_cycles()
            .map_or_else(|| "n/a".to_string(), |value| format!("{value:+.3}"));
        let max_growth = self
            .max_window_growth()
            .map_or_else(|| "n/a".to_string(), |value| format!("{value:+.1}"));
        format!(
            "{} ({}): baseline {baseline} -> final {final_value}, slope {slope}/100 cycles, largest window {max_growth}",
            self.name, self.unit
        )
    }
}

/// Baseline-relative check for a level that starts at the process's own value:
/// a descriptor or thread count may warm up, but it may not keep climbing. The
/// sustained-rise rule separates a slow leak from warm-up, because warm-up spends
/// its growth in the first window and then flattens.
fn growth_offenders(series: &CounterSeries, retention_budget: f64) -> Vec<String> {
    if !series.complete() {
        return vec![format!(
            "{}: the platform reported no value, so return-to-baseline is unproven",
            series.name
        )];
    }
    let total = series.final_value().unwrap_or(0.0) - series.baseline().unwrap_or(0.0);
    let growth = series.window_growth();
    let windows = growth.len();
    let rising = growth.iter().flatten().filter(|growth| **growth > 0.0).count();
    let sustained = total > 0.0 && windows > 0 && rising * 2 >= windows;
    if total > retention_budget || sustained {
        let slope = series.slope_per_100_cycles().unwrap_or(0.0);
        let max_window = series.max_window_growth().unwrap_or(0.0);
        return vec![format!(
            "{} did not return to baseline: retained {total:+.0} (budget {retention_budget:+.0}), slope {slope:+.3}/100 cycles, rose in {rising} of {windows} windows, largest window {max_window:+.0}",
            series.name
        )];
    }
    Vec::new()
}

/// Absolute check for a count of live things that must be gone at quiescence.
fn gauge_offenders(series: &CounterSeries, budget: f64) -> Vec<String> {
    let final_value = series.final_value().unwrap_or(-1.0);
    if final_value > budget {
        return vec![format!(
            "{} is not empty at quiescence: final {final_value:.0} (budget {budget:.0})",
            series.name
        )];
    }
    Vec::new()
}

/// Growth check for a total that legitimately starts above zero, such as tasks
/// spawned since process start. Asking such a counter to reach zero would fail a
/// run in which it never moved, so only its growth is a leak.
fn cumulative_offenders(series: &CounterSeries, growth_budget: f64) -> Vec<String> {
    let (Some(baseline), Some(final_value)) = (series.baseline(), series.final_value()) else {
        return vec![format!("{}: no value reported, so growth is unproven", series.name)];
    };
    let growth = final_value - baseline;
    if growth > growth_budget {
        return vec![format!(
            "{} grew by {growth:+.0} across the run (baseline {baseline:.0} -> final {final_value:.0}, growth budget {growth_budget:+.0})",
            series.name
        )];
    }
    Vec::new()
}

async fn sample(debug_socket_path: &Path, cycle: usize) -> Result<SoakSample> {
    let memory = debug_run_command_json(debug_socket_path.to_path_buf(), "server:memory", None).await?;
    let incident = debug_run_command_json(debug_socket_path.to_path_buf(), "server:memory-incident", None).await?;
    let process = incident.get("process").cloned().unwrap_or_else(|| json!({}));
    let counters = process.get("stage_counters").cloned().unwrap_or_else(|| json!({}));
    let counters = if counters.is_object() { counters } else { json!({}) };
    Ok(SoakSample {
        cycle,
        fd_count: process.get("fd_count").and_then(serde_json::Value::as_u64),
        thread_count: process.get("thread_count").and_then(serde_json::Value::as_u64),
        stage_counters_enabled: counters
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        queue_depth_high_water: counters
            .get("queue_depth_high_water")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        pending_requests: counters
            .get("pending_requests")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        interrupt_poll_wakeups: counters
            .get("interrupt_poll_wakeups")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        background_tasks: memory
            .pointer("/background/task_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        shared_context_entries: memory
            .pointer("/swarm/shared_context_entry_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        live_sessions: memory
            .pointer("/sessions/live_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        connected_clients: memory
            .pointer("/clients/connected_clients")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        rss_bytes: process.get("rss_bytes").and_then(serde_json::Value::as_u64),
    })
}

/// Sample the process counters only once two consecutive reads agree.
///
/// `fd_count` and `thread_count` are process-wide, and this test shares the
/// process with the rest of the e2e binary, so one instant can count a descriptor
/// that another task is still closing (the previous sample's debug connection, a
/// harness output file, a socket whose peer just went away). Settling on two equal
/// reads isolates the measurement from that churn: a descriptor that closes
/// disappears from the reading, while a leaked one stays and is still counted.
/// The budgets and the sustained-rise rule are unchanged.
async fn settled_sample(debug_socket_path: &Path, cycle: usize) -> Result<SoakSample> {
    const SETTLE_READS: usize = 10;
    const SETTLE_INTERVAL: Duration = Duration::from_millis(20);
    let mut taken = sample(debug_socket_path, cycle).await?;
    for _ in 1..SETTLE_READS {
        tokio::time::sleep(SETTLE_INTERVAL).await;
        let next = sample(debug_socket_path, cycle).await?;
        let settled = next.fd_count == taken.fd_count && next.thread_count == taken.thread_count;
        taken = next;
        if settled {
            break;
        }
    }
    Ok(taken)
}

/// Wakeup count and counter-enabled flag from one debug round-trip, for the
/// per-turn delta; `sample` costs two commands and a turn boundary needs one.
async fn wakeup_counters(debug_socket_path: &Path) -> Result<(bool, u64)> {
    let incident = debug_run_command_json(debug_socket_path.to_path_buf(), "server:memory-incident", None).await?;
    let counters = incident
        .pointer("/process/stage_counters")
        .cloned()
        .unwrap_or_else(|| json!({}));
    Ok((
        counters
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        counters
            .get("interrupt_poll_wakeups")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
    ))
}

async fn wait_for_server_ready_bounded(socket_path: &Path, debug_socket_path: &Path, deadline: Duration) -> Result<()> {
    // The shared helper pings with a bare read_line and no timer, so a server
    // that accepts the connection but never answers parks the whole test with
    // nothing printed. Bound the phase and name it when it times out.
    timeout(deadline, wait_for_server_ready(socket_path, debug_socket_path))
        .await
        .map_err(|_| anyhow::anyhow!("server readiness exceeded {deadline:?}"))?
}

async fn wait_for_quiescence(debug_socket_path: &Path, deadline: Duration) -> Result<()> {
    let end = Instant::now() + deadline;
    let mut last_observation = "no observation".to_string();
    while Instant::now() < end {
        let memory = debug_run_command_json(debug_socket_path.to_path_buf(), "server:memory", None).await?;
        let clients = memory
            .pointer("/clients/connected_clients")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let sessions = memory
            .pointer("/sessions/live_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if clients == 0 && sessions == 0 {
            return Ok(());
        }
        last_observation = format!("clients={clients} live_sessions={sessions}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("server never reached quiescence within {deadline:?}: {last_observation}")
}

/// Each read waits out the whole remaining turn budget. Capping a single read
/// would fail a healthy turn: a provider call or a tool step can legitimately be
/// silent for seconds between events, and the turn deadline is what is real.
async fn wait_for_done(client: &mut server::Client, target_id: u64, what: &str) -> Result<Vec<ServerEvent>> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut events = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = timeout(remaining, client.read_event())
            .await
            .map_err(|_| anyhow::anyhow!("{what}: no event for request {target_id} within 30s"))??;
        let done = matches!(event, ServerEvent::Done { id } if id == target_id);
        let failure = match &event {
            ServerEvent::Error { id, message, .. } if *id == target_id => Some(message.clone()),
            _ => None,
        };
        events.push(event);
        if let Some(message) = failure {
            anyhow::bail!("{what} failed for request {target_id}: {message}");
        }
        if done {
            return Ok(events);
        }
    }
    anyhow::bail!(
        "{what} timed out waiting for done {target_id} after {} events within 30s",
        events.len()
    )
}

async fn attach_once(socket_path: &Path, target_session_id: &str) -> Result<()> {
    // `connect_with_path` and `subscribe_with_info` write without a timer, so a
    // server that accepts the socket and never answers would park the cycle with
    // nothing printed. Name the phase instead of hanging.
    let mut client = timeout(
        Duration::from_secs(10),
        server::Client::connect_with_path(socket_path.to_path_buf()),
    )
    .await
    .map_err(|_| anyhow::anyhow!("attach connect exceeded 10s"))??;
    let subscribe_id = timeout(
        Duration::from_secs(10),
        client.subscribe_with_info(None, None, Some(target_session_id.to_string()), false, false),
    )
    .await
    .map_err(|_| anyhow::anyhow!("subscribe exceeded 10s for {target_session_id}"))??;
    let events = wait_for_done(&mut client, subscribe_id, "soak attach").await?;
    anyhow::ensure!(
        events
            .iter()
            .any(|event| matches!(event, ServerEvent::History { id, .. } if *id == subscribe_id)),
        "attach to {target_session_id} returned no history event"
    );
    drop(client);
    Ok(())
}

/// The detach is the connection drop. A persisted session takes the immediate
/// cleanup path, so a transient rejection means the previous cycle's cleanup is
/// still settling, not that the soak is broken.
async fn attach_detach_once(socket_path: &Path, target_session_id: &str) -> Result<()> {
    let mut last_error = None;
    for attempt in 0..4 {
        match attach_once(socket_path, target_session_id).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(10 * (attempt + 1))).await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("attach never ran")))
}

async fn run_turn_cycle(
    socket_path: &Path,
    debug_socket_path: &Path,
    unique_suffix: &str,
    index: usize,
    window_cycles: usize,
    tool_calls_in_round: usize,
    state: &ProviderState,
) -> Result<TurnRound> {
    let mut session = Session::create_with_id(
        format!("session_soak_turn_{index}_{unique_suffix}"),
        None,
        Some(format!("Soak Turn {index}")),
    );
    session.model = Some("soak-model".to_string());
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: format!("soak turn {index}"),
            cache_control: None,
        }],
    );
    session.save()?;
    let session_id = session.id.clone();

    state.begin_turn(tool_calls_in_round);
    let wakeups_before = wakeup_counters(debug_socket_path)
        .await
        .map(|(_, wakeups)| wakeups)
        .unwrap_or(0);
    let started = Instant::now();
    let mut client = server::Client::connect_with_path(socket_path.to_path_buf()).await?;
    let subscribe_id = client
        .subscribe_with_info(None, None, Some(session_id.clone()), false, false)
        .await?;
    wait_for_done(&mut client, subscribe_id, "soak turn attach").await?;
    let message_id = client.send_message(&format!("soak turn {index}")).await?;

    let mid_flight = if tool_calls_in_round > 0 {
        // The tool sleeps 250 ms, so this lands inside an open tool round: the
        // queue holds one interrupt and the sample is taken while it does.
        tokio::time::sleep(Duration::from_millis(60)).await;
        let queued = debug_run_command(
            debug_socket_path.to_path_buf(),
            &format!("queue_interrupt:soak interrupt {index}"),
            Some(&session_id),
        )
        .await;
        let taken = sample(debug_socket_path, index * window_cycles).await.ok();
        if let Err(error) = queued {
            eprintln!("soak mid-flight interrupt not queued: {error}");
        }
        taken
    } else {
        None
    };

    let events = wait_for_done(&mut client, message_id, "soak turn").await?;
    let tool_outcomes = events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::ToolDone { name, error, .. } => Some(match error {
                Some(error) => format!("{name}: error {error}"),
                None => format!("{name}: ok"),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    let wakeup_delta = wakeup_counters(debug_socket_path)
        .await
        .map(|(_, wakeups)| wakeups.saturating_sub(wakeups_before))
        .unwrap_or(0);
    let elapsed_ms = started.elapsed().as_millis();
    let provider_calls = state.tool_calls();
    drop(client);
    Ok(TurnRound {
        index,
        tool_calls_in_round,
        elapsed_ms,
        provider_calls,
        outcome: "completed".to_string(),
        tool_outcomes,
        wakeup_delta,
        mid_flight,
    })
}

fn create_rotating_sessions(unique_suffix: &str) -> Result<Vec<String>> {
    let mut session_ids = Vec::with_capacity(ROTATING_SESSIONS);
    for idx in 0..ROTATING_SESSIONS {
        let mut session = Session::create_with_id(
            format!("session_soak_attach_{idx}_{unique_suffix}"),
            None,
            Some(format!("Soak Attach {idx}")),
        );
        session.model = Some("soak-model".to_string());
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("soak attach {idx}"),
                cache_control: None,
            }],
        );
        session.add_message(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: format!("soak reply {idx}"),
                cache_control: None,
            }],
        );
        session.save()?;
        session_ids.push(session.id);
    }
    Ok(session_ids)
}

/// A provider that is silent for longer than one read window is healthy, not
/// stuck. The first version of this soak capped every single read at 2 s and
/// failed here with tokio's `deadline has elapsed`; the turn budget is the only
/// deadline that may end a turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_first_event_turn_still_completes() -> Result<()> {
    let _env = setup_test_env()?;
    let runtime = tempfile::tempdir()?;
    let socket_path = runtime.path().join("server.sock");
    let debug_socket_path = runtime.path().join("debug.sock");
    let state = Arc::new(ProviderState::default());
    let provider: Arc<dyn Provider> = Arc::new(SoakProvider {
        state: Arc::clone(&state),
        tool_fixture: runtime.path().join("fixture.txt").to_string_lossy().into_owned(),
    });
    let server = server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone())
        .with_idle_reconnect_grace(std::time::Duration::ZERO);
    let handle = tokio::spawn(async move { server.run().await });
    let result = async {
        wait_for_server_ready_bounded(&socket_path, &debug_socket_path, Duration::from_secs(60)).await?;
        let mut session = Session::create_with_id(
            format!("session_soak_slow_first_{}", std::process::id()),
            None,
            Some("Soak Slow First Event".to_string()),
        );
        session.model = Some("soak-model".to_string());
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "slow first event".to_string(),
                cache_control: None,
            }],
        );
        session.save()?;

        state.begin_turn(0);
        state.first_event_delay_ms.store(3_000, Ordering::SeqCst);

        let mut client = timeout(
            Duration::from_secs(10),
            server::Client::connect_with_path(socket_path.clone()),
        )
        .await
        .map_err(|_| anyhow::anyhow!("connect exceeded 10s"))??;
        let subscribe_id = client
            .subscribe_with_info(None, None, Some(session.id.clone()), false, false)
            .await?;
        wait_for_done(&mut client, subscribe_id, "slow first event attach").await?;
        let message_id = client.send_message("slow first event").await?;
        let events = wait_for_done(&mut client, message_id, "slow first event turn").await?;
        anyhow::ensure!(
            events
                .iter()
                .any(|event| matches!(event, ServerEvent::TextDelta { text } if text == "soak reply")),
            "a turn silent for 3 s must still deliver its reply, not fail a read deadline"
        );
        Ok(())
    }
    .await;
    abort_server_and_cleanup(&handle, &socket_path, &debug_socket_path);
    result
}

/// The long run is opt-in: at the measured 3.14 s per cycle (200 cycles in
/// 628 s on an Apple M1 Max) 1,000 cycles costs about 52 minutes, which is a
/// nightly job, not a gate. Run it with
/// `cargo test -p horon-agent --test e2e soak_attach_detach_1000_cycles -- --ignored --exact --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "about 52 minutes at the measured 3.14 s per cycle; nightly/manual long run"]
async fn soak_attach_detach_1000_cycles() -> Result<()> {
    horon_agent::env::set_var("HORON_AGENT_SOAK_CYCLES", "1000");
    run_soak().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn soak_attach_detach_returns_every_counter_to_baseline() -> Result<()> {
    run_soak().await
}

async fn run_soak() -> Result<()> {
    let _env = setup_test_env()?;

    let runtime_dir = short_runtime_dir(format!(
        "horon-agent-soak-attach-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir)?;
    let unique_suffix = runtime_dir
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("soak")
        .to_string();
    let socket_path: PathBuf = runtime_dir.join("horon-agent.sock");
    let debug_socket_path: PathBuf = runtime_dir.join("horon-agent-debug.sock");

    let targets = create_rotating_sessions(&unique_suffix)?;
    let state = Arc::new(ProviderState::default());
    let tool_fixture = runtime_dir.join("soak-tool-fixture.txt");
    std::fs::write(&tool_fixture, "soak tool fixture\n")?;
    let provider: Arc<dyn Provider> = Arc::new(SoakProvider {
        state: Arc::clone(&state),
        tool_fixture: tool_fixture.to_string_lossy().into_owned(),
    });
    let server_instance = server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone())
        .with_idle_reconnect_grace(std::time::Duration::ZERO);
    let server_handle = tokio::spawn(async move { server_instance.run().await });

    let result = async {
        eprintln!("soak: server spawned, waiting for readiness");
        wait_for_server_ready_bounded(&socket_path, &debug_socket_path, Duration::from_secs(60)).await?;
        eprintln!("soak: server ready, waiting for quiescence");
        wait_for_quiescence(&debug_socket_path, Duration::from_secs(30)).await?;

        let cycles = cycles_from_env();
        let window = (cycles / TARGET_WINDOWS).max(1);
        let windows = cycles.div_ceil(window);
        eprintln!("soak: quiescent, starting {cycles} cycles in {windows} windows of {window}");
        let mut samples = Vec::with_capacity(windows + 1);
        let mut turn_rounds = Vec::with_capacity(windows);
        let mut quiescence_misses: Vec<String> = Vec::new();
        samples.push(settled_sample(&debug_socket_path, 0).await?);

        for window_index in 1..=windows {
            let window_started = Instant::now();
            let first_cycle = (window_index - 1) * window;
            let last_cycle = (first_cycle + window).min(cycles);
            for cycle in first_cycle..last_cycle {
                attach_detach_once(&socket_path, &targets[cycle % ROTATING_SESSIONS]).await?;
            }
            let attach_ms = window_started.elapsed().as_millis();
            let completed_cycles = last_cycle - first_cycle;

            // Window 1 exercises the one-call inline path, the last window the
            // two-call polling group path, and every other window a plain turn.
            let tool_calls_in_round = if window_index == windows {
                2
            } else if window_index == 1 {
                1
            } else {
                0
            };
            let round = match run_turn_cycle(
                &socket_path,
                &debug_socket_path,
                &unique_suffix,
                window_index,
                window,
                tool_calls_in_round,
                &state,
            )
            .await
            {
                Ok(round) => round,
                Err(error) => TurnRound {
                    index: window_index,
                    tool_calls_in_round,
                    elapsed_ms: 0,
                    provider_calls: state.tool_calls(),
                    outcome: format!("error: {error}"),
                    tool_outcomes: Vec::new(),
                    wakeup_delta: 0,
                    mid_flight: None,
                },
            };
            if let Some(error) = round.outcome.strip_prefix("error: ") {
                eprintln!("soak window {window_index}/{windows}: turn failed: {error}");
            }
            turn_rounds.push(round);

            // A turn that failed may have left a session behind, so a quiescence
            // miss is recorded and the soak keeps sampling rather than losing the
            // rest of the run to one stuck turn.
            if let Err(error) = wait_for_quiescence(&debug_socket_path, Duration::from_secs(30)).await {
                eprintln!("soak window {window_index}/{windows}: quiescence missed: {error}");
                quiescence_misses.push(format!("window {window_index}: {error}"));
            }
            let taken = settled_sample(&debug_socket_path, last_cycle).await?;
            eprintln!(
                "soak window {window_index}/{windows} (cycle {last_cycle}): fds={:?} threads={:?} queue_hw={} pending={} wakeups={} tasks={} shared_ctx={} ms_per_cycle={:.2}",
                taken.fd_count,
                taken.thread_count,
                taken.queue_depth_high_water,
                taken.pending_requests,
                taken.interrupt_poll_wakeups,
                taken.background_tasks,
                taken.shared_context_entries,
                attach_ms as f64 / completed_cycles as f64,
            );
            samples.push(taken);
        }

        let fds = CounterSeries::new(&samples, "fd_count", "descriptors", window, |sample| sample.fd_count);
        let threads = CounterSeries::new(&samples, "thread_count", "threads", window, |sample| sample.thread_count);
        let queue_high_water = CounterSeries::new(&samples, "queue_depth_high_water", "entries", window, |sample| {
            Some(sample.queue_depth_high_water)
        });
        let pending = CounterSeries::new(&samples, "pending_requests", "requests", window, |sample| {
            Some(sample.pending_requests)
        });
        let wakeups = CounterSeries::new(&samples, "interrupt_poll_wakeups", "wakeups", window, |sample| {
            Some(sample.interrupt_poll_wakeups)
        });
        let tasks = CounterSeries::new(&samples, "background_tasks", "tasks", window, |sample| {
            Some(sample.background_tasks)
        });
        let shared_context = CounterSeries::new(&samples, "shared_context_entries", "entries", window, |sample| {
            Some(sample.shared_context_entries)
        });
        let live_sessions = CounterSeries::new(&samples, "live_sessions", "sessions", window, |sample| {
            Some(sample.live_sessions)
        });
        let clients = CounterSeries::new(&samples, "connected_clients", "clients", window, |sample| {
            Some(sample.connected_clients)
        });

        let series = [
            &fds,
            &threads,
            &queue_high_water,
            &pending,
            &wakeups,
            &tasks,
            &shared_context,
            &live_sessions,
            &clients,
        ];

        let mut table = String::from("cycle\tfds\tthreads\tqueue_hw\tpending\twakeups\ttasks\tshared_ctx\trss_bytes\n");
        for sample in &samples {
            table.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                sample.cycle,
                sample
                    .fd_count
                    .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
                sample
                    .thread_count
                    .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
                sample.queue_depth_high_water,
                sample.pending_requests,
                sample.interrupt_poll_wakeups,
                sample.background_tasks,
                sample.shared_context_entries,
                sample
                    .rss_bytes
                    .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
            ));
        }

        let failed_rounds: Vec<String> = turn_rounds
            .iter()
            .filter(|round| round.outcome != "completed")
            .map(|round| format!("window {}: {}", round.index, round.outcome))
            .collect();
        let mut offenders = growth_offenders(&fds, FD_RETENTION_BUDGET);
        offenders.extend(growth_offenders(&threads, THREAD_RETENTION_BUDGET));

        for series in [&pending, &live_sessions, &clients] {
            offenders.extend(gauge_offenders(series, GAUGE_BUDGET));
        }

        offenders.extend(cumulative_offenders(&tasks, CUMULATIVE_GROWTH_BUDGET));
        offenders.extend(quiescence_misses.iter().map(|miss| format!("quiescence missed: {miss}")));
        offenders.extend(failed_rounds.iter().map(|round| format!("turn failed: {round}")));
        offenders.extend(cumulative_offenders(&wakeups, INTERRUPT_POLL_GROWTH_BUDGET));

        // The 25 ms poll is installed only where an interrupt watch exists, and the
        // daemon installs one only for a multi-member group
        // (`turn_streaming_mpsc.rs`, the `group.len() > 1` branch): a one-member
        // group runs inline and `poll_for_interrupt` parks on `pending()`. So a
        // zero is correct when nothing watched, and a defect when something did.
        let group_rounds: Vec<&TurnRound> = turn_rounds
            .iter()
            .filter(|round| round.tool_calls_in_round > 1 && round.outcome == "completed")
            .collect();
        let inline_rounds: Vec<&TurnRound> = turn_rounds
            .iter()
            .filter(|round| round.tool_calls_in_round == 1 && round.outcome == "completed")
            .collect();
        let group_wakeups: u64 = group_rounds.iter().map(|round| round.wakeup_delta).sum();
        let inline_wakeups: u64 = inline_rounds.iter().map(|round| round.wakeup_delta).sum();
        let watch_configured_rounds = group_rounds.len();
        // Reported, never asserted. The counter reads 0 for two structural
        // reasons, and neither is a defect: `poll_for_interrupt` parks on
        // `pending()` unless a watch exists, and the `select!` is biased toward
        // completion, so a group whose members finish inside one 25 ms tick
        // never reaches the poll branch. The wiring itself is proven by
        // `agent::tool_round_tests::interrupt_poll_records_a_wakeup_per_interval_while_a_member_runs`,
        // which gates a member open across several ticks.
        let interrupt_poll_report = if watch_configured_rounds == 0 {
            "interrupt_poll_wakeups: no multi-member tool round ran, so no interrupt watch was configured and the 25 ms interval never ticked".to_string()
        } else {
            format!(
                "interrupt_poll_wakeups: {group_wakeups} across {watch_configured_rounds} watched group round(s) and {inline_wakeups} across {} one-member round(s); a group whose members finish inside one 25 ms tick records nothing, so the timer costs nothing in the common case",
                inline_rounds.len()
            )
        };

        // The high-water mark only rises, so the check is that the second half of
        // the run stopped raising it.
        let growth = queue_high_water.window_growth();
        let late_growth = growth
            .into_iter()
            .skip(queue_high_water.values.len().saturating_sub(1) / 2)
            .flatten()
            .fold(0.0_f64, f64::max);
        if late_growth > 0.0 {
            offenders.push(format!(
                "queue_depth_high_water kept rising in the second half of the run: largest late window {late_growth:+.0}"
            ));
        }

        let shared_context_grew = shared_context
            .final_value()
            .zip(shared_context.baseline())
            .is_some_and(|(final_value, baseline)| final_value > baseline);
        let interrupt_poll_wakeups = wakeups.final_value().unwrap_or(0.0);
        let report = json!({
            "cycles": cycles,
            "window": window,
            "windows": windows,
            "rotating_sessions": ROTATING_SESSIONS,
            "turn_rounds": turn_rounds,
            "failed_rounds": failed_rounds,
            "quiescence_misses": quiescence_misses,
            "samples": samples,
            "counters": series.iter().map(|series| json!({
                "name": series.name,
                "unit": series.unit,
                "baseline": series.baseline(),
                "final": series.final_value(),
                "slope_per_100_cycles": series.slope_per_100_cycles(),
                "max_window_growth": series.max_window_growth(),
                "window_growth": series.window_growth(),
                "summary": series.describe(),
            })).collect::<Vec<_>>(),
            "budgets": {
                "fd_retention": FD_RETENTION_BUDGET,
                "thread_retention": THREAD_RETENTION_BUDGET,
                "gauge": GAUGE_BUDGET,
                "sustained_rule": "fails when the counter retains above budget, or rises in at least half the windows",
            },
            "interrupt_poll_wakeups_total": interrupt_poll_wakeups,
            "interrupt_poll_wakeup_slope_per_100_cycles": wakeups.slope_per_100_cycles(),
            "interrupt_poll_report": interrupt_poll_report,
            "interrupt_poll_by_path": {
                "watch_configured_rounds": watch_configured_rounds,
                "group_round_wakeups": group_wakeups,
                "group_round_tool_calls": group_rounds.first().map(|round| round.tool_calls_in_round),
                "inline_round_wakeups": inline_wakeups,
                "inline_round_tool_calls": inline_rounds.first().map(|round| round.tool_calls_in_round),
                "finding": "the 25 ms interrupt poll costs nothing in the common case: poll_for_interrupt parks on pending() unless a watch exists, a one-member group runs inline in crates/app-core/src/agent/turn_streaming_mpsc.rs and never reaches run_group, and a watched group whose members finish inside one tick never reaches the biased select branch. It only ticks when a multi-member group has a member in flight past 25 ms, which is the number todo 16 should weigh against a notification.",
            },
            "shared_context_grew": shared_context_grew,
            "shared_context_note": "shared_context has no removal path (crates/app-core/src/server.rs:771); measured and reported, never asserted",
            "non_returning": offenders,
        });

        // Read by scripts/perf/check_structural.py: FD and thread retention at a
        // short cycle count, with the number of counters that did not return.
        eprintln!(
            "STRUCTURAL_PERF section=soak cycles={} fd_retention={} thread_retention={} non_returning={}",
            cycles,
            fds.final_value().unwrap_or(0.0) as i64 - fds.baseline().unwrap_or(0.0) as i64,
            threads.final_value().unwrap_or(0.0) as i64 - threads.baseline().unwrap_or(0.0) as i64,
            offenders.len()
        );
        eprintln!("soak_attach_detach_table\n{table}");
        for series in series {
            eprintln!("soak_attach_detach_counter={}", series.describe());
        }
        eprintln!(
            "soak_attach_detach_metrics={}",
            serde_json::to_string_pretty(&report)?
        );

        assert!(
            offenders.is_empty(),
            "soak: {} counter(s) did not return to baseline: {}",
            offenders.len(),
            offenders.join(" | ")
        );
        Ok(())
    }
    .await;

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);
    let _ = std::fs::remove_dir_all(&runtime_dir);
    result
}
