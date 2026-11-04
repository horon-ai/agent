//! Todo 16 measurements. These are measurements, not gates: each test prints one
//! JSON line and asserts only that the sample was really taken, because a `null`
//! counter means the number is unknown rather than zero. The decisions built on
//! them live in `.omo/evidence/agent-runtime-performance-2/task-16-agent-runtime-performance-2.md`.
//!
//! Residency is read through the daemon's own `create_session` debug command and
//! subscription cost runs the real `globalEvents` SDK stream against the real
//! harness API bridge in this process, so both are visible in the same
//! `thread_count`/`fd_count` the daemon reports rather than a model of them.

use crate::test_support::*;
use horon_agent_harness_api_server::run_bridge;
use horon_agent_sdk::{ConnectOptions, GlobalEventsOptions, HoronAgentClient};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn runtime_dir(label: &str) -> PathBuf {
    let dir = short_runtime_dir(format!(
        "horon-agent-t16-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Residency {
    live_sessions: u64,
    shared_context_entries: u64,
    debug_jobs: u64,
    swarm_members: u64,
    connected_clients: u64,
    fd_count: Option<u64>,
    thread_count: Option<u64>,
    rss_bytes: Option<u64>,
}

async fn residency(debug_socket_path: &Path) -> Result<Residency> {
    let memory = debug_run_command_json(debug_socket_path.to_path_buf(), "server:memory", None).await?;
    let incident = debug_run_command_json(debug_socket_path.to_path_buf(), "server:memory-incident", None).await?;
    let process = incident.get("process").cloned().unwrap_or_else(|| json!({}));
    let number = |value: Option<&Value>| value.and_then(Value::as_u64).unwrap_or(0);
    Ok(Residency {
        live_sessions: number(memory.pointer("/sessions/live_count")),
        shared_context_entries: number(memory.pointer("/swarm/shared_context_entry_count")),
        debug_jobs: number(memory.pointer("/debug/job_count")),
        swarm_members: number(memory.pointer("/swarm/member_count")),
        connected_clients: number(memory.pointer("/clients/connected_clients")),
        fd_count: process.get("fd_count").and_then(Value::as_u64),
        thread_count: process.get("thread_count").and_then(Value::as_u64),
        rss_bytes: process.get("rss_bytes").and_then(Value::as_u64),
    })
}

fn describe(population: usize, sample: &Residency, create_ms: Option<u128>) -> Value {
    json!({
        "sessions_requested": population,
        "live_sessions": sample.live_sessions,
        "shared_context_entries": sample.shared_context_entries,
        "debug_jobs": sample.debug_jobs,
        "swarm_members": sample.swarm_members,
        "connected_clients": sample.connected_clients,
        "fd_count": sample.fd_count,
        "thread_count": sample.thread_count,
        "rss_bytes": sample.rss_bytes,
        "create_ms_total": create_ms,
    })
}

fn populations() -> Vec<usize> {
    std::env::var("HORON_AGENT_T16_POPULATIONS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .filter_map(|step| step.trim().parse::<usize>().ok())
                .filter(|step| *step > 0)
                .collect::<Vec<_>>()
        })
        .filter(|steps| !steps.is_empty())
        .unwrap_or_else(|| vec![1, 10, 100, 1000])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn measure_session_residency_at_population_steps() -> Result<()> {
    let _env = setup_test_env()?;
    let dir = runtime_dir("residency");
    let socket_path = dir.join("horon-agent.sock");
    let debug_socket_path = dir.join("horon-agent-debug.sock");
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::default());
    let server_instance = server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone());
    let server_handle = tokio::spawn(async move { server_instance.run().await });

    let result = async {
        wait_for_server_ready(&socket_path, &debug_socket_path).await?;
        let baseline = residency(&debug_socket_path).await?;
        eprintln!("t16_residency_baseline={}", describe(0, &baseline, None));

        let mut steps = Vec::new();
        let mut created = 0usize;
        let mut previous_target = 0usize;
        for target in populations() {
            let started = Instant::now();
            let mut failures = 0usize;
            while created < target {
                match debug_run_command(debug_socket_path.clone(), "create_session", None).await {
                    Ok(_) => created += 1,
                    Err(error) => {
                        failures += 1;
                        eprintln!("t16_residency_create_failed at {created}: {error}");
                        break;
                    }
                }
            }
            let elapsed = started.elapsed().as_millis();
            let sample = residency(&debug_socket_path).await?;
            let created_in_step = target - previous_target;
            let mut step = describe(target, &sample, Some(elapsed));
            if let Some(object) = step.as_object_mut() {
                object.insert("created_in_step".into(), json!(created_in_step));
                object.insert(
                    "ms_per_create".into(),
                    json!((elapsed as f64 / created_in_step.max(1) as f64 * 1000.0).round() / 1000.0),
                );
            }
            previous_target = target;
            eprintln!("t16_residency_step={step}");
            steps.push(step);
            if failures > 0 {
                break;
            }
        }

        let baseline_fds = baseline.fd_count.unwrap_or(0) as i64;
        let baseline_threads = baseline.thread_count.unwrap_or(0) as i64;
        let report = json!({
            "measurement": "session residency at 1/10/100/1000 live sessions",
            "command": "cargo test -p horon-agent --test e2e measure_session_residency_at_population_steps -- --exact --nocapture",
            "baseline": describe(0, &baseline, None),
            "steps": steps,
            "derived": {
                "fd_per_session_at_max": steps.last().and_then(|step| {
                    let fds = step.get("fd_count").and_then(Value::as_u64)? as i64;
                    let sessions = step.get("live_sessions").and_then(Value::as_u64)? as f64;
                    (sessions > 0.0).then(|| (fds - baseline_fds) as f64 / sessions)
                }),
                "thread_per_session_at_max": steps.last().and_then(|step| {
                    let threads = step.get("thread_count").and_then(Value::as_u64)? as i64;
                    let sessions = step.get("live_sessions").and_then(Value::as_u64)? as f64;
                    (sessions > 0.0).then(|| (threads - baseline_threads) as f64 / sessions)
                }),
            },
        });
        eprintln!("t16_residency={}", serde_json::to_string_pretty(&report)?);
        anyhow::ensure!(
            steps.last().is_some_and(|step| step
                .get("live_sessions")
                .and_then(Value::as_u64)
                .is_some_and(|count| count > 0)),
            "no live session was measured, so residency is unproven"
        );
        Ok(())
    }
    .await;

    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn persisted_session(index: usize, suffix: &str) -> Result<String> {
    let mut session = Session::create_with_id(
        format!("session_t16_global_{index}_{suffix}"),
        None,
        Some(format!("T16 Global {index}")),
    );
    session.model = Some("mock-model".to_string());
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: format!("t16 global {index}"),
            cache_control: None,
        }],
    );
    session.save()?;
    Ok(session.id)
}

fn sdk_client(api_socket: &Path) -> HoronAgentClient {
    HoronAgentClient::connect(ConnectOptions {
        socket_path: Some(api_socket.to_path_buf()),
        client_name: "t16-measure/1.0".into(),
        request_timeout: Some(Duration::from_secs(30)),
        ensure_runtime: false,
        supports_user_response: false,
    })
    .expect("connect to the harness API bridge")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn measure_global_events_subscription_cost() -> Result<()> {
    let _env = setup_test_env()?;
    let dir = runtime_dir("globalevents");
    let socket_path = dir.join("horon-agent.sock");
    let debug_socket_path = dir.join("horon-agent-debug.sock");
    let api_socket = dir.join("api.sock");
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::default());
    let server_instance = server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone());
    let server_handle = tokio::spawn(async move { server_instance.run().await });
    let bridge_socket = socket_path.clone();
    let bridge_api = api_socket.clone();
    let bridge = tokio::spawn(async move { run_bridge(bridge_api, bridge_socket).await });

    let result = async {
        wait_for_server_ready(&socket_path, &debug_socket_path).await?;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !api_socket.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        anyhow::ensure!(api_socket.exists(), "harness API bridge never bound {}", api_socket.display());

        let suffix = dir.file_name().unwrap().to_string_lossy().into_owned();
        let mut ids = Vec::new();
        for index in 0..20 {
            ids.push(persisted_session(index, &suffix)?);
        }

        let before = residency(&debug_socket_path).await?;
        let client = sdk_client(&api_socket);
        let stream = client.global_events(GlobalEventsOptions {
            discovery_interval: Duration::from_millis(20),
            max_buffered_events: 10_000,
        })?;

        let started = Instant::now();
        let attach_deadline = Instant::now() + Duration::from_secs(60);
        let mut events = 0usize;
        let mut watched = 0usize;
        let mut during = residency(&debug_socket_path).await?;
        while Instant::now() < attach_deadline {
            watched = during.connected_clients.saturating_sub(before.connected_clients) as usize;
            if watched >= ids.len() {
                break;
            }
            match tokio::time::timeout(Duration::from_millis(50), async { stream.next() }).await {
                Ok(Ok(Some(_))) => events += 1,
                Ok(Ok(None)) => break,
                Ok(Err(error)) => anyhow::bail!("global events stream failed: {error}"),
                Err(_) => {}
            }
            during = residency(&debug_socket_path).await?;
        }
        let attach_ms = started.elapsed().as_millis();
        drop(stream);
        drop(client);

        let settle_deadline = Instant::now() + Duration::from_secs(20);
        let mut after = residency(&debug_socket_path).await?;
        while Instant::now() < settle_deadline
            && (after.connected_clients > before.connected_clients || after.fd_count > before.fd_count)
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
            after = residency(&debug_socket_path).await?;
        }

        let watched = watched as f64;
        let fd_delta = during.fd_count.unwrap_or(0) as i64 - before.fd_count.unwrap_or(0) as i64;
        let thread_delta = during.thread_count.unwrap_or(0) as i64 - before.thread_count.unwrap_or(0) as i64;
        let client_delta = during.connected_clients as i64 - before.connected_clients as i64;
        let report = json!({
            "measurement": "per-subscription cost of one globalEvents stream",
            "command": "cargo test -p horon-agent --test e2e measure_global_events_subscription_cost -- --exact --nocapture",
            "sessions_persisted": ids.len(),
            "sessions_watched": watched as usize,
            "events_delivered": events,
            "attach_ms_for_all_children": attach_ms,
            "daemon_before": describe(0, &before, None),
            "daemon_during": describe(0, &during, None),
            "daemon_after_close": describe(0, &after, None),
            "per_subscription": {
                "fd_delta": fd_delta,
                "thread_delta": thread_delta,
                "daemon_client_delta": client_delta,
                "fd_per_watched_session": (watched > 0.0).then(|| fd_delta as f64 / watched),
                "thread_per_watched_session": (watched > 0.0).then(|| thread_delta as f64 / watched),
                "daemon_client_per_watched_session": (watched > 0.0).then(|| client_delta as f64 / watched),
                "attach_ms_per_watched_session": (watched > 0.0).then(|| attach_ms as f64 / watched),
            },
            "protocol_round_trips_per_child": {
                "attach": 1,
                "event_subscribe": 1,
                "list_sessions_per_discovery_pass": 1,
                "source": "crates/agent-sdk/src/client.rs start_global_child: connect + events(Some(id)) + attach_session",
            },
        });
        eprintln!("t16_global_events={}", serde_json::to_string_pretty(&report)?);
        anyhow::ensure!(watched > 0.0, "no child subscription was observed, so the cost is unproven");
        Ok(())
    }
    .await;

    bridge.abort();
    abort_server_and_cleanup(&server_handle, &socket_path, &debug_socket_path);
    let _ = std::fs::remove_dir_all(&dir);
    result
}
