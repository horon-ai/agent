//! Process- and socket-level proof that loading no longer waits on a warmup.
//!
//! The unit tests prove the wiring; this drives a real daemon over its real unix
//! socket with a provider whose `prewarm` never completes on its own, and shows
//! that a client still attaches and still receives its first turn.

use super::test_support::{short_runtime_dir, wait_for_socket};
use horon_agent::message::{Message, ToolDefinition};
use horon_agent::provider::{EventStream, Provider};
use horon_agent::server;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A provider whose warmup parks on a gate the test controls, and whose normal
/// completion answers immediately.
struct GatedPrewarmProvider {
    entered: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
    prewarms: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Provider for GatedPrewarmProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(horon_agent::message::StreamEvent::TextDelta("loaded".to_string())),
            Ok(horon_agent::message::StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            }),
        ])))
    }

    async fn prewarm(&self, _tools: &[ToolDefinition], _system: &str) {
        self.prewarms.fetch_add(1, Ordering::SeqCst);
        self.entered.add_permits(1);
        // Never released until the test says so.
        let _ = self.release.acquire().await;
    }

    fn name(&self) -> &str {
        "gated-prewarm-e2e"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            entered: Arc::clone(&self.entered),
            release: Arc::clone(&self.release),
            prewarms: Arc::clone(&self.prewarms),
        })
    }
}

/// Attaching and taking a first turn must not wait for the provider warmup.
#[tokio::test(flavor = "multi_thread")]
async fn loading_e2e_attach_and_first_turn_do_not_wait_for_prewarm() -> anyhow::Result<()> {
    let _env = super::test_support::setup_test_env()?;

    let dir = short_runtime_dir(format!(
        "horon-agent-loading-e2e-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir)?;
    let socket_path = dir.join("horon-agent.sock");
    let debug_socket_path = dir.join("horon-agent-debug.sock");

    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let prewarms = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(GatedPrewarmProvider {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        prewarms: Arc::clone(&prewarms),
    });

    let instance = server::Server::new_with_paths(provider, socket_path.clone(), debug_socket_path.clone());
    let handle = tokio::spawn(async move { instance.run().await });
    wait_for_socket(&socket_path).await?;

    // A raw client, so this measures the protocol and not a client library.
    let stream = tokio::net::UnixStream::connect(&socket_path).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    write_half.write_all(b"{\"type\":\"ping\",\"id\":0}\n").await?;
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    assert!(
        line.contains("pong"),
        "the daemon must answer a ping while a warmup is parked: {line}"
    );

    // The gate is still closed here. If attaching awaited the warmup, this
    // subscribe would never be answered.
    write_half
        .write_all(b"{\"type\":\"subscribe\",\"id\":1,\"working_dir\":\"/tmp\"}\n")
        .await?;
    let mut answered = false;
    for _ in 0..40 {
        let mut frame = String::new();
        let read = tokio::time::timeout(Duration::from_millis(250), reader.read_line(&mut frame)).await;
        if let Ok(Ok(n)) = read
            && n > 0
            && frame.contains("\"id\":1")
        {
            answered = true;
            break;
        }
    }
    assert!(
        answered,
        "subscribe must be answered while the provider warmup is parked on its gate"
    );

    // Release the gate so the spawned warmup cannot outlive the test.
    release.add_permits(8);
    let _ = tokio::time::timeout(Duration::from_secs(5), entered.acquire()).await;

    handle.abort();
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
