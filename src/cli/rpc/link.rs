//! One connection to the HoronAgent daemon, opened for the rpc mode.
//!
//! The daemon socket is read by a single background task. A decoded event whose
//! request id has a registered waiter goes to that waiter; every other event
//! belongs to the running turn and goes to the foreground queue, which the
//! adapter owns and selects on while a prompt is running. A turn's terminal
//! (`Done`/`Error`) carries the id of the `message` request that started the
//! turn, and that request registers no waiter, so the terminal arrives on the
//! foreground queue where the turn loop looks for it.

use crate::cli::dispatch;
use crate::cli::provider_init::ProviderChoice;
use crate::protocol::{Request, ServerEvent};
use crate::transport::{ReadHalf, WriteHalf};
use anyhow::Result;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;

/// How this client identifies itself to the daemon.
const CLIENT_INSTANCE_ID: &str = "rpc";

pub(super) const DISCONNECTED: &str = "HoronAgent daemon disconnected";

/// One connection to the daemon plus its request-id allocator.
pub(super) struct Daemon {
    writer: Arc<Mutex<WriteHalf>>,
    next_id: AtomicU64,
    replies: Arc<Mutex<HashMap<u64, oneshot::Sender<ServerEvent>>>>,
    pump: JoinHandle<()>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

impl Daemon {
    pub(super) fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Write one request.
    pub(super) async fn send(&self, request: &Request) -> Result<()> {
        let mut json = serde_json::to_string(request)?;
        json.push('\n');
        let mut writer = self.writer.lock().await;
        writer.write_all(json.as_bytes()).await?;
        writer.flush().await?;
        Ok(())
    }

    async fn register(&self, id: u64) -> oneshot::Receiver<ServerEvent> {
        let (tx, rx) = oneshot::channel();
        self.replies.lock().await.insert(id, tx);
        rx
    }

    /// Send `request` (its id is already set) and wait for its reply.
    pub(super) async fn request(&self, request: &Request) -> Result<ServerEvent> {
        let id = request.id();
        let reply = self.register(id).await;
        if let Err(error) = self.send(request).await {
            self.replies.lock().await.remove(&id);
            return Err(error);
        }
        reply
            .await
            .map_err(|_| anyhow::anyhow!("{DISCONNECTED} (request {id})"))
    }
}

/// Where the daemon end of the connection comes from. A unit test scripts it
/// instead of dialing the socket.
pub(super) type Connector = Arc<dyn Fn() -> Connect + Send + Sync>;
pub(super) type Connect = Pin<Box<dyn Future<Output = Result<(ReadHalf, WriteHalf)>> + Send>>;

/// Open the connection, subscribe, and read the session's history.
///
/// `resume` names a session to attach to; when it is absent the daemon creates a
/// new one for this connection.
pub(super) async fn connect(
    provider_choice: &ProviderChoice,
    model: Option<&str>,
    provider_profile: Option<&str>,
    cwd: Option<&std::path::Path>,
    resume: Option<&str>,
    connector: Option<Connector>,
) -> Result<(Daemon, ServerEvent, mpsc::UnboundedReceiver<ServerEvent>)> {
    let (reader, writer) = match connector {
        Some(connector) => connector().await?,
        None => {
            if !dispatch::server_is_running().await {
                dispatch::spawn_server(provider_choice, model, provider_profile).await?;
            }
            crate::server::connect_socket(&crate::server::socket_path())
                .await?
                .into_split()
        }
    };

    let (fg_tx, fg_rx) = mpsc::unbounded_channel();
    let replies: Arc<Mutex<HashMap<u64, oneshot::Sender<ServerEvent>>>> = Arc::default();
    let pump = tokio::spawn(pump_events(reader, fg_tx, Arc::clone(&replies)));

    let daemon = Daemon {
        writer: Arc::new(Mutex::new(writer)),
        next_id: AtomicU64::new(1),
        replies,
        pump,
    };

    let subscribe = subscribe_request(daemon.next_id(), cwd, resume);
    daemon.request(&subscribe).await?;

    let history_request = Request::GetHistory { id: daemon.next_id() };
    let history_reply = daemon.request(&history_request).await?;
    if !matches!(history_reply, ServerEvent::History { .. }) {
        anyhow::bail!("unexpected reply to get_history: {history_reply:?}");
    }
    Ok((daemon, history_reply, fg_rx))
}

fn subscribe_request(id: u64, cwd: Option<&std::path::Path>, resume: Option<&str>) -> Request {
    Request::Subscribe {
        id,
        system_prompt: None,
        supports_user_response: false,
        supports_ask_requests: false,
        working_dir: cwd.map(|cwd| cwd.display().to_string()),
        target_session_id: resume.map(str::to_string),
        client_instance_id: Some(CLIENT_INSTANCE_ID.to_string()),
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: crate::terminal_launch::snapshot_client_terminal_env(),
        acp: None,
    }
}

async fn pump_events(
    reader: ReadHalf,
    foreground: mpsc::UnboundedSender<ServerEvent>,
    replies: Arc<Mutex<HashMap<u64, oneshot::Sender<ServerEvent>>>>,
) {
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) => {
                crate::logging::warn(&format!("rpc: daemon read failed: {err}"));
                break;
            }
        }
        let Ok(event) = serde_json::from_str::<ServerEvent>(&line) else {
            crate::logging::warn(&format!(
                "rpc: skipping undecodable daemon event: {}",
                crate::logging::truncate_for_log(line.trim_end(), 200)
            ));
            continue;
        };
        let waiter = match reply_id(&event) {
            Some(id) => replies.lock().await.remove(&id),
            None => None,
        };
        if let Some(waiter) = waiter {
            // A dropped receiver means the requester already gave up.
            drop(waiter.send(event));
            continue;
        }
        if foreground.send(event).is_err() {
            break;
        }
    }
    replies.lock().await.clear();
}

/// The request id an event answers, if it answers one.
fn reply_id(event: &ServerEvent) -> Option<u64> {
    match event {
        ServerEvent::Ack { id }
        | ServerEvent::BackgroundWorkList { id, .. }
        | ServerEvent::CompactResult { id, .. }
        | ServerEvent::Done { id }
        | ServerEvent::Error { id, .. }
        | ServerEvent::History { id, .. }
        | ServerEvent::ModelChanged { id, .. }
        | ServerEvent::ReasoningEffortChanged { id, .. }
        | ServerEvent::State { id, .. }
        | ServerEvent::ToolCallCancelResult { id, .. }
        | ServerEvent::UserMessageInserted { id, .. } => Some(*id),
        _ => None,
    }
}
