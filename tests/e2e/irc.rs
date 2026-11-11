//! Real-daemon IRC mailbox bus: agent-to-agent send, idle wake, and relay.
//!
//! Every test drives the daemon's own socket with the same JSON requests a
//! Desktop client sends, and every assertion reads the durable session record
//! instead of the text a scripted mock response returned.
//!
//! Each test takes `setup_test_env`, whose guard holds the process-wide HOME
//! lock for the whole scenario. That serializes these tests against every other
//! e2e test in this binary, which matters because `Server::run` installs one
//! process-global IRC bus and the last daemon to start would otherwise replace
//! the bus the running scenario is using.

#![cfg(unix)]

use crate::test_support::*;
use horon_agent::config::Config;
use horon_agent::message::ContentBlock;
use horon_agent::provider::Provider;
use horon_agent::session::Session;
use serde_json::json;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const WAIT_LIMIT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

fn isolated_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HORON_HOME").expect("setup_test_env sets HORON_HOME"))
}

fn write_config(body: &str) {
    std::fs::write(isolated_home().join("config.toml"), body).expect("write config.toml");
    Config::invalidate_cache();
}

fn irc_switch_on_config() -> &'static str {
    "[tools]\nprofile = \"core\"\nirc = true\n"
}

fn irc_switch_off_config() -> &'static str {
    "[tools]\nprofile = \"core\"\n"
}

struct RunningDaemon {
    _runtime_dir: tempfile::TempDir,
    socket: PathBuf,
    debug_socket: PathBuf,
    handle: tokio::task::JoinHandle<Result<()>>,
}

async fn start_daemon(provider: Arc<MockProvider>) -> Result<RunningDaemon> {
    let runtime_dir = tempfile::tempdir()?;
    let socket = runtime_dir.path().join("server.sock");
    let debug_socket = runtime_dir.path().join("debug.sock");
    let provider: Arc<dyn Provider> = provider;
    let server = server::Server::new_with_paths(provider, socket.clone(), debug_socket.clone());
    let handle = tokio::spawn(async move { server.run().await });
    wait_for_server_ready(&socket, &debug_socket).await?;
    Ok(RunningDaemon {
        _runtime_dir: runtime_dir,
        socket,
        debug_socket,
        handle,
    })
}

impl RunningDaemon {
    fn shutdown(&self) {
        abort_server_and_cleanup(&self.handle, &self.socket, &self.debug_socket);
    }
}

/// A raw NDJSON connection, the same wire a Desktop client speaks.
struct RawClient {
    reader: BufReader<horon_agent::transport::ReadHalf>,
    writer: horon_agent::transport::WriteHalf,
    next_id: u64,
}

impl RawClient {
    async fn connect(path: &std::path::Path) -> Result<Self> {
        let connection = server::connect_socket(path).await?;
        let (reader, writer) = connection.into_split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
        })
    }

    async fn write_request(&mut self, mut request: serde_json::Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        request["id"] = json!(id);
        self.writer.write_all(format!("{request}\n").as_bytes()).await?;
        Ok(id)
    }

    async fn read_event(&mut self) -> Result<ServerEvent> {
        let mut line = String::new();
        anyhow::ensure!(self.reader.read_line(&mut line).await? > 0, "server disconnected");
        Ok(serde_json::from_str(&line)?)
    }

    /// Wait for one request's terminal event, collecting everything seen.
    async fn collect_until<F>(&mut self, id: u64, mut is_terminal: F) -> Result<Vec<ServerEvent>>
    where
        F: FnMut(&ServerEvent) -> bool,
    {
        let deadline = Instant::now() + WAIT_LIMIT;
        let mut events = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            anyhow::ensure!(
                !remaining.is_zero(),
                "timed out waiting for request {id}; seen: {events:?}"
            );
            let event = match timeout(remaining, self.read_event()).await {
                Ok(result) => result?,
                Err(_) => anyhow::bail!("timed out waiting for request {id}; seen: {events:?}"),
            };
            let terminal = is_terminal(&event);
            events.push(event);
            if terminal {
                return Ok(events);
            }
        }
    }

    /// Wait for the done/error event of a message or subscribe request.
    async fn collect_until_terminal(&mut self, id: u64) -> Result<Vec<ServerEvent>> {
        self.collect_until(id, |event| {
            matches!(event, ServerEvent::Done { id: done } if *done == id)
                || matches!(event, ServerEvent::Error { id: failed, .. } if *failed == id)
        })
        .await
    }

    /// `subscribe` a root session and return its id.
    async fn subscribe_root(&mut self) -> Result<String> {
        let working_dir = std::env::current_dir()?.to_string_lossy().into_owned();
        let id = self
            .write_request(json!({
                "type": "subscribe",
                "working_dir": working_dir,
                "crash_on_disconnect": false,
                "continue_on_disconnect": false,
            }))
            .await?;
        let events = self.collect_until_terminal(id).await?;
        for event in &events {
            if let ServerEvent::SessionId { session_id } = event {
                return Ok(session_id.clone());
            }
        }
        anyhow::bail!("subscribe did not identify the session: {events:?}")
    }

    /// Spawn an idle in-process child that inherits the caller's swarm.
    async fn spawn_idle_child(&mut self, parent: &str) -> Result<String> {
        let id = self
            .write_request(json!({
                "type": "comm_spawn",
                "session_id": parent,
                "spawn_mode": "inline",
            }))
            .await?;
        let events = self
            .collect_until(id, |event| match event {
                ServerEvent::CommSpawnResponse { id: response, .. } => *response == id,
                ServerEvent::Error { id: failed, .. } => *failed == id,
                _ => false,
            })
            .await?;
        for event in &events {
            if let ServerEvent::CommSpawnResponse { new_session_id, .. } = event {
                return Ok(new_session_id.clone());
            }
            if let ServerEvent::Error { message, .. } = event {
                anyhow::bail!("comm_spawn failed: {message}");
            }
        }
        anyhow::bail!("comm_spawn returned no response: {events:?}")
    }

    /// Send a plain user turn and return the message request id.
    async fn send_user_message(&mut self, content: &str) -> Result<u64> {
        self.write_request(json!({
            "type": "message",
            "content": content,
        }))
        .await
    }
}

fn text_block(text: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta(text.to_string()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        },
    ]
}

fn irc_tool_call(call_id: &str, target: &str, message: &str) -> Vec<StreamEvent> {
    let input = json!({
        "action": "send",
        "to": target,
        "message": message,
    })
    .to_string();
    vec![
        StreamEvent::ToolUseStart {
            id: call_id.to_string(),
            name: "irc".to_string(),
        },
        StreamEvent::ToolInputDelta(input),
        StreamEvent::ToolUseEnd,
        StreamEvent::MessageEnd {
            stop_reason: Some("tool_use".to_string()),
        },
    ]
}

fn irc_wait_call(call_id: &str, timeout_secs: u64) -> Vec<StreamEvent> {
    let input = json!({
        "action": "wait",
        "timeout_secs": timeout_secs,
    })
    .to_string();
    vec![
        StreamEvent::ToolUseStart {
            id: call_id.to_string(),
            name: "irc".to_string(),
        },
        StreamEvent::ToolInputDelta(input),
        StreamEvent::ToolUseEnd,
        StreamEvent::MessageEnd {
            stop_reason: Some("tool_use".to_string()),
        },
    ]
}

/// Script one assistant message with several tool calls and a `tool_use` stop.
fn tool_round(mut events: Vec<StreamEvent>) -> Vec<StreamEvent> {
    if matches!(events.last(), Some(StreamEvent::MessageEnd { .. })) {
        events.pop();
    }
    events.push(StreamEvent::MessageEnd {
        stop_reason: Some("tool_use".to_string()),
    });
    events
}

fn session_texts(session_id: &str) -> Vec<String> {
    Session::load(session_id)
        .map(|session| {
            session
                .active_context()
                .iter()
                .flat_map(|message| message.content.iter())
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every block's text: plain text plus tool results, because a relayed answer can
/// reach the sender as an injected message or through a parked `irc wait` call.
fn session_block_texts(session_id: &str) -> Vec<String> {
    Session::load(session_id)
        .map(|session| {
            session
                .active_context()
                .iter()
                .flat_map(|message| message.content.iter())
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.clone()),
                    ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn session_tool_results(session_id: &str) -> Vec<(String, bool)> {
    Session::load(session_id)
        .map(|session| {
            session
                .active_context()
                .iter()
                .flat_map(|message| message.content.iter())
                .filter_map(|block| match block {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error, ..
                    } => Some((format!("{tool_use_id}: {content}"), is_error.unwrap_or(false))),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn envelope_attribute(block: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = block.find(&needle)? + needle.len();
    let rest = &block[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn short_name(session_id: &str) -> Option<String> {
    Session::load(session_id).ok().and_then(|session| session.short_name)
}

async fn poll_until<T, F>(mut read: F, what: &str) -> Result<T>
where
    F: FnMut() -> Option<T>,
{
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        if let Some(value) = read() {
            return Ok(value);
        }
        anyhow::ensure!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Watch a negative condition over a bounded window instead of a single sleep.
async fn assert_absent<F>(mut read: F, what: &str) -> Result<()>
where
    F: FnMut() -> Option<String>,
{
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if let Some(hit) = read() {
            anyhow::bail!("{what}: {hit}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    Ok(())
}

#[tokio::test]
async fn idle_child_is_woken_and_its_answer_is_relayed() -> Result<()> {
    let _env = setup_test_env()?;
    write_config(irc_switch_on_config());

    let provider = Arc::new(MockProvider::new());
    let daemon = start_daemon(Arc::clone(&provider)).await?;
    let result = async {
        let mut client = RawClient::connect(&daemon.socket).await?;
        let parent = client.subscribe_root().await?;
        let child = client.spawn_idle_child(&parent).await?;

        // The spawn response is sent after the member is registered as `ready`
        // with no initial turn, so the child is idle here. The parent's first
        // message sends the ping and then parks in `irc wait`, so the woken
        // child's relay has a live waiter to land on whatever order the shared
        // mock queue hands out the identical `ok` answers.
        provider.queue_response(tool_round(
            [
                irc_tool_call("call_ping", &child, "ping"),
                irc_wait_call("call_wait", 10),
            ]
            .concat(),
        ));
        for _ in 0..6 {
            provider.queue_response(text_block("ok"));
        }

        let message_id = client.send_user_message("ping the child").await?;
        let _ = client.collect_until_terminal(message_id).await?;

        // The child record holds the sender's envelope with the body.
        let envelope = poll_until(
            || {
                session_texts(&child)
                    .into_iter()
                    .find(|text| text.contains("<irc ") && text.contains("ping"))
            },
            "the child's IRC envelope",
        )
        .await?;
        assert_eq!(
            envelope_attribute(&envelope, "from").as_deref(),
            Some(parent.as_str()),
            "the envelope must name the parent as its sender"
        );
        let handle = envelope_attribute(&envelope, "handle");
        let child_name = short_name(&child);
        assert_ne!(
            handle.as_deref(),
            Some(child.as_str()),
            "handle must not be the child's id"
        );
        if let Some(child_name) = child_name.as_deref() {
            assert_ne!(
                handle.as_deref(),
                Some(child_name),
                "handle must not be the child's own name"
            );
        }
        if let Some(handle) = handle.as_deref()
            && let Some(parent_name) = short_name(&parent)
        {
            assert_eq!(handle, parent_name, "handle must be the parent's alias");
        }

        // The woken child's last answer is relayed back to the parent.
        let relayed = poll_until(
            || {
                session_block_texts(&parent)
                    .into_iter()
                    .find(|text| text.contains("wake_relay=\"true\""))
            },
            "the parent's relayed wake answer",
        )
        .await?;
        assert_eq!(
            envelope_attribute(&relayed, "from").as_deref(),
            Some(child.as_str()),
            "the relay must arrive from the child"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    daemon.shutdown();
    result
}

#[tokio::test]
async fn irc_tool_is_refused_when_switch_is_off() -> Result<()> {
    let _env = setup_test_env()?;
    write_config(irc_switch_off_config());

    let provider = Arc::new(MockProvider::new());
    let daemon = start_daemon(Arc::clone(&provider)).await?;
    let result = async {
        let mut client = RawClient::connect(&daemon.socket).await?;
        let parent = client.subscribe_root().await?;
        let child = client.spawn_idle_child(&parent).await?;

        provider.queue_response(irc_tool_call("call_off", &child, "ping"));
        provider.queue_response(text_block("ok"));

        let message_id = client.send_user_message("call irc while it is off").await?;
        let events = client.collect_until_terminal(message_id).await?;
        let errors: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                ServerEvent::Error { message, .. } => Some(message.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            !errors.is_empty(),
            "the turn must fail when the tool is not on the allow-list: {events:?}"
        );
        assert!(
            errors
                .iter()
                .any(|message| message.contains("irc") && message.contains("not allowed")),
            "expected the allow-list refusal, saw: {errors:?}"
        );

        // No envelope ever reaches the child.
        assert_absent(
            || session_texts(&child).into_iter().find(|text| text.contains("<irc ")),
            "the child received an envelope",
        )
        .await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    daemon.shutdown();
    result
}

#[tokio::test]
async fn send_to_unknown_agent_reports_failed_receipt() -> Result<()> {
    let _env = setup_test_env()?;
    write_config(irc_switch_on_config());

    let provider = Arc::new(MockProvider::new());
    let daemon = start_daemon(Arc::clone(&provider)).await?;
    let result = async {
        let mut client = RawClient::connect(&daemon.socket).await?;
        let parent = client.subscribe_root().await?;

        provider.queue_response(irc_tool_call("call_unknown", "no-such-agent", "hello"));
        provider.queue_response(text_block("ok"));

        let message_id = client.send_user_message("send to a missing peer").await?;
        let _ = client.collect_until_terminal(message_id).await?;

        let results = poll_until(
            || {
                let results = session_tool_results(&parent);
                results.iter().any(|(_, is_error)| *is_error).then_some(results)
            },
            "the failed irc tool result",
        )
        .await?;
        let failed: Vec<&str> = results
            .iter()
            .filter(|(_, is_error)| *is_error)
            .map(|(content, _)| content.as_str())
            .collect();
        assert!(
            failed
                .iter()
                .any(|content| content.contains("Unknown agent") && content.contains("no-such-agent")),
            "expected the unknown-agent receipt failure, saw: {failed:?}"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    daemon.shutdown();
    result
}
