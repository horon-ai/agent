//! MCP Client - handles communication with a single MCP server

use super::protocol::*;
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, oneshot};

/// Shared communication handle for an MCP server.
/// Multiple sessions can hold clones of this and send concurrent requests.
/// Request/response correlation by ID ensures no interference.
#[derive(Clone)]
pub struct McpHandle {
    pub(crate) name: String,
    request_id: Arc<AtomicU64>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
    /// Set by the reader on stdout EOF: no reply can arrive, so requests fail fast.
    closed: Arc<AtomicBool>,
    writer_tx: mpsc::Sender<String>,
    server_info: Arc<std::sync::RwLock<Option<ServerInfo>>>,
    capabilities: Arc<std::sync::RwLock<ServerCapabilities>>,
    tools: Arc<std::sync::RwLock<Vec<McpToolDef>>>,
    /// Cached `resources/list` result. `None` means "not fetched or invalidated
    /// since", which is distinct from a server that legitimately has none.
    resources: Arc<std::sync::RwLock<Option<Vec<McpResourceDef>>>>,
    /// Cached `prompts/list` result, with the same `None` = stale rule.
    prompts: Arc<std::sync::RwLock<Option<Vec<McpPromptDef>>>>,
    /// Reply timeout applied to every request on this server.
    request_timeout: std::time::Duration,
    /// Server-initiated notifications, by method. A frame with a `method` and
    /// no `id` lands here instead of being dropped, so a server that changes
    /// its tool list can say so.
    ///
    /// Never read: the sender exists so the channel stays open exactly as long
    /// as the connection does, which is what lets the watcher detect a dropped
    /// connection as a closed channel.
    #[expect(
        dead_code,
        reason = "kept alive so the notification channel outlives the reader task"
    )]
    notifications: mpsc::UnboundedSender<String>,
    /// The single receiving half of [`Self::notifications`]. One consumer per
    /// connection: taking it twice yields `None`, which keeps the watcher count
    /// at one even when several sessions share the handle.
    notifications_rx: NotificationReceiver,
}

/// Method forwarded to the notification sink when the transport can no longer
/// deliver replies. Never sent by a real server: it is how the connection
/// watcher learns it must run the reconnect ladder.
pub const TRANSPORT_CLOSED_NOTIFICATION: &str = "horon-agent/transport_closed";

/// Notification a server sends when its resource list changed.
pub const RESOURCES_LIST_CHANGED_NOTIFICATION: &str = "notifications/resources/list_changed";

/// Notification a server sends when its prompt list changed.
pub const PROMPTS_LIST_CHANGED_NOTIFICATION: &str = "notifications/prompts/list_changed";

/// The single receiving half of a connection's notification channel. Wrapped so
/// the watcher can take it exactly once, however many handles exist.
pub(crate) type NotificationReceiver = Arc<std::sync::Mutex<Option<mpsc::UnboundedReceiver<String>>>>;

fn notification_channel() -> (mpsc::UnboundedSender<String>, NotificationReceiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    (tx, Arc::new(std::sync::Mutex::new(Some(rx))))
}

/// Clear one list cache so the next read refetches it.
fn invalidate_list<T>(cache: &std::sync::RwLock<Option<Vec<T>>>) {
    *cache.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// Removes waiters on timeout, send failure and cancellation of the caller.
struct PendingRequest {
    id: u64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.try_lock() {
            pending.remove(&self.id);
        } else if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let pending = Arc::clone(&self.pending);
            let id = self.id;
            runtime.spawn(async move {
                pending.lock().await.remove(&id);
            });
        }
    }
}

/// Default reply timeout when a server config does not set `timeout_secs`.
pub const DEFAULT_MCP_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A separate budget for the complete remote connection, including OAuth refresh.
const REMOTE_MCP_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Resolve the per-request reply timeout for a server config.
pub fn request_timeout_for(config: &McpServerConfig) -> std::time::Duration {
    config
        .timeout_secs
        .filter(|secs| *secs > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or(DEFAULT_MCP_REQUEST_TIMEOUT)
}

impl McpHandle {
    /// Send a request and wait for response
    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<JsonRpcResponse> {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest::new(id, method, params);

        let msg = serde_json::to_string(&request)? + "\n";
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if self.closed.load(Ordering::SeqCst) {
                anyhow::bail!("MCP server '{}' exited (connection closed)", self.name);
            }
            pending.insert(id, tx);
        }
        let _cleanup = PendingRequest {
            id,
            pending: Arc::clone(&self.pending),
        };
        // Include channel backpressure in the deadline, not just the reply.
        let response = tokio::time::timeout(self.request_timeout, async {
            self.writer_tx.send(msg).await.context("Failed to send request")?;
            rx.await.with_context(|| format!("MCP server '{}' exited (connection closed)", self.name))
        })
        .await
        .with_context(|| format!(
            "Request timeout after {}s (raise `timeout_secs` for MCP server '{}' if its tools legitimately run longer)",
            self.request_timeout.as_secs(), self.name
        ))??;

        if let Some(err) = &response.error {
            anyhow::bail!("MCP error {}: {}", err.code, err.message);
        }

        Ok(response)
    }

    /// Call a tool
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolCallResult> {
        let arguments = if arguments.is_null() {
            Value::Object(serde_json::Map::new())
        } else {
            arguments
        };
        let params = ToolCallParams {
            name: name.to_string(),
            arguments,
        };

        let response = self.request("tools/call", Some(serde_json::to_value(params)?)).await?;

        let result = response.result.context("No result from tool call")?;
        let tool_result: ToolCallResult = serde_json::from_value(result)?;

        Ok(tool_result)
    }

    /// Whether the transport can no longer deliver replies. Closed handles
    /// must be replaced by a fresh connection, never used to replay a call.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst) || self.writer_tx.is_closed()
    }

    /// Take the notification receiver for this connection, if this is the first
    /// caller. `None` means another watcher already owns it.
    pub fn take_notifications(&self) -> Option<mpsc::UnboundedReceiver<String>> {
        self.notifications_rx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    /// Get the server name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get server info
    pub fn server_info(&self) -> Option<ServerInfo> {
        self.server_info
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Get available tools
    pub fn tools(&self) -> Vec<McpToolDef> {
        self.tools
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Refresh the list of available tools
    pub async fn refresh_tools(&self) -> Result<()> {
        let response = self.request("tools/list", None).await?;

        if let Some(result) = response.result {
            let tools_result: ToolsListResult = serde_json::from_value(result)?;
            *self.tools.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = tools_result.tools;
        }

        Ok(())
    }

    /// The capabilities the server declared in its `initialize` result.
    pub fn server_capabilities(&self) -> ServerCapabilities {
        self.capabilities
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Whether the server advertised the `resources` capability.
    pub fn supports_resources(&self) -> bool {
        self.server_capabilities().resources.is_some()
    }

    /// Whether the server advertised the `prompts` capability.
    pub fn supports_prompts(&self) -> bool {
        self.server_capabilities().prompts.is_some()
    }

    /// The server's resource list, taken from the cache when an earlier call
    /// already fetched it. A `resources/list_changed` notification empties the
    /// cache, so this refetches after one.
    pub async fn list_resources(&self) -> Result<Vec<McpResourceDef>> {
        if let Some(cached) = self
            .resources
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            return Ok(cached);
        }
        self.refresh_resources().await
    }

    /// Fetch the resource list and replace the cache.
    pub async fn refresh_resources(&self) -> Result<Vec<McpResourceDef>> {
        self.require_capability(self.supports_resources(), "resources")?;
        let response = self.request("resources/list", None).await?;
        let result = response.result.context("No result from resources/list")?;
        let parsed: ResourcesListResult = serde_json::from_value(result)?;
        let resources = parsed.resources;
        *self.resources.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(resources.clone());
        Ok(resources)
    }

    /// Read one resource body. A server without the `resources` capability is
    /// rejected before any request is written.
    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult> {
        self.require_capability(self.supports_resources(), "resources")?;
        let params = serde_json::to_value(ReadResourceParams { uri })?;
        let response = self.request("resources/read", Some(params)).await?;
        let result = response.result.context("No result from resources/read")?;
        Ok(serde_json::from_value(result)?)
    }

    /// The server's prompt list, with the same cache rule as resources.
    pub async fn list_prompts(&self) -> Result<Vec<McpPromptDef>> {
        if let Some(cached) = self
            .prompts
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            return Ok(cached);
        }
        self.refresh_prompts().await
    }

    /// Fetch the prompt list and replace the cache.
    pub async fn refresh_prompts(&self) -> Result<Vec<McpPromptDef>> {
        self.require_capability(self.supports_prompts(), "prompts")?;
        let response = self.request("prompts/list", None).await?;
        let result = response.result.context("No result from prompts/list")?;
        let parsed: PromptsListResult = serde_json::from_value(result)?;
        let prompts = parsed.prompts;
        *self.prompts.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(prompts.clone());
        Ok(prompts)
    }

    /// Expand one prompt on the server.
    pub async fn get_prompt(&self, name: &str, arguments: Option<Value>) -> Result<GetPromptResult> {
        self.require_capability(self.supports_prompts(), "prompts")?;
        let params = serde_json::to_value(GetPromptParams {
            name: name.to_string(),
            arguments,
        })?;
        let response = self.request("prompts/get", Some(params)).await?;
        let result = response.result.context("No result from prompts/get")?;
        Ok(serde_json::from_value(result)?)
    }

    /// Drop the cached resource list so the next call refetches it.
    pub fn invalidate_resources(&self) {
        invalidate_list(&self.resources);
    }

    /// Drop the cached prompt list so the next call refetches it.
    pub fn invalidate_prompts(&self) {
        invalidate_list(&self.prompts);
    }

    /// Reject a method the server never declared, instead of writing a request
    /// it would answer with a protocol error.
    fn require_capability(&self, supported: bool, capability: &str) -> Result<()> {
        if supported {
            return Ok(());
        }
        anyhow::bail!("{}", unsupported_capability_message(capability, &self.name))
    }
}

/// The message a caller gets for a capability the server never advertised, used
/// both when the client refuses to send the request and when the tool reports
/// the capability as missing.
pub fn unsupported_capability_message(capability: &str, server: &str) -> String {
    format!(
        "MCP {capability} are not supported by '{server}' (the server did not advertise the {capability} capability)"
    )
}

/// MCP Client - owns the child process and provides shared handles.
/// Only one McpClient exists per MCP server process, but many McpHandle
/// clones can be distributed to different sessions.
pub struct McpClient {
    handle: McpHandle,
    backend: Backend,
}

/// What actually carries the JSON-RPC messages for a client.
enum Backend {
    /// Local child process speaking newline-delimited JSON over stdio.
    Stdio(Child),
    /// Remote server over Streamable HTTP or legacy SSE.
    Remote(super::remote::RemoteConnection),
}

impl McpClient {
    /// Connect to an MCP server, inheriting the current process working directory
    pub async fn connect(name: String, config: &McpServerConfig) -> Result<Self> {
        Self::connect_in_dir(name, config, None).await
    }

    /// Connect to an MCP server, optionally running it in `working_dir`.
    ///
    /// The working directory is only applied when it exists; otherwise the
    /// subprocess falls back to inheriting the current process cwd (issue #557).
    pub async fn connect_in_dir(
        name: String,
        config: &McpServerConfig,
        working_dir: Option<&std::path::Path>,
    ) -> Result<Self> {
        if let Some(transport) = config.remote_transport() {
            return Self::connect_remote(name, config, transport).await;
        }
        let working_dir = working_dir.filter(|dir| dir.is_dir());
        crate::logging::info(&format!(
            "MCP: Connecting to '{}' ({} {:?}) cwd={:?}",
            name, config.command, config.args, working_dir
        ));

        // Credentials must be opted into an MCP server explicitly through its
        // config. The long-lived horon-agent daemon contains provider credentials in
        // its process environment, and blindly inheriting them exposes those
        // credentials to every configured MCP executable (issue #771).
        let inherited: HashMap<String, String> = std::env::vars().collect();
        let env = mcp_child_env(inherited, &config.env);

        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .envs(&env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = working_dir {
            command.current_dir(dir);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("Failed to spawn MCP server: {}", config.command))?;

        let stdin = child.stdin.take().context("No stdin")?;
        let stdout = child.stdout.take().context("No stdout")?;
        let stderr = child.stderr.take().context("No stderr")?;

        // Spawn stderr reader
        let server_name = name.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            crate::logging::warn(&format!("MCP [{}] stderr: {}", server_name, trimmed));
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Setup channels
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>> = Arc::new(Mutex::new(HashMap::new()));
        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(32);

        // Spawn writer task
        let mut stdin = stdin;
        tokio::spawn(async move {
            while let Some(msg) = writer_rx.recv().await {
                if stdin.write_all(msg.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        // Spawn reader task
        let pending_clone = Arc::clone(&pending);
        let closed = Arc::new(AtomicBool::new(false));
        let closed_clone = Arc::clone(&closed);
        let reader_name = name.clone();
        let (notifications, notifications_rx) = notification_channel();
        let reader_notifications = notifications.clone();
        // Created before the reader so a list_changed notification clears the
        // same caches the handle readers consult.
        let resources: Arc<std::sync::RwLock<Option<Vec<McpResourceDef>>>> = Arc::new(std::sync::RwLock::new(None));
        let prompts: Arc<std::sync::RwLock<Option<Vec<McpPromptDef>>>> = Arc::new(std::sync::RwLock::new(None));
        let reader_resources = Arc::clone(&resources);
        let reader_prompts = Arc::clone(&prompts);
        let mut reader = BufReader::new(stdout);
        tokio::spawn(async move {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        crate::logging::debug(&format!("MCP [{}]: stdout EOF", reader_name));
                        break;
                    }
                    Ok(_) => match serde_json::from_str::<Value>(&line) {
                        Ok(Value::Object(object)) => {
                            if let Some(method) = object.get("method").and_then(Value::as_str) {
                                // A server-initiated request carries an `id` and
                                // would need a reply; horon-agent advertises no
                                // client capabilities, so only notifications (no
                                // `id`) are acted on.
                                if object.get("id").is_none() {
                                    // The list caches are invalidated in the
                                    // reader, before the notification reaches
                                    // the watcher: a frame that follows this one
                                    // must already see the stale cache cleared.
                                    if method == RESOURCES_LIST_CHANGED_NOTIFICATION {
                                        invalidate_list(&reader_resources);
                                    } else if method == PROMPTS_LIST_CHANGED_NOTIFICATION {
                                        invalidate_list(&reader_prompts);
                                    }
                                    let _ = reader_notifications.send(method.to_string());
                                } else {
                                    crate::logging::debug(&format!(
                                        "MCP [{}]: ignoring server request {}",
                                        reader_name, method
                                    ));
                                }
                                continue;
                            }
                            let response = serde_json::from_value::<JsonRpcResponse>(Value::Object(object));
                            match response {
                                Ok(response) => {
                                    if let Some(id) = response.id {
                                        let mut pending = pending_clone.lock().await;
                                        if let Some(tx) = pending.remove(&id) {
                                            let _ = tx.send(response);
                                        }
                                    }
                                }
                                Err(err) => crate::logging::debug(&format!(
                                    "MCP [{}]: unparseable response: {}",
                                    reader_name, err
                                )),
                            }
                        }
                        _ => {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                crate::logging::debug(&format!("MCP [{}] non-JSON output: {}", reader_name, trimmed));
                            }
                        }
                    },
                    Err(e) => {
                        crate::logging::warn(&format!("MCP [{}] read error: {}", reader_name, e));
                        break;
                    }
                }
            }
            // Server can never reply now: drop in-flight senders, fail later requests fast.
            let mut pending = pending_clone.lock().await;
            closed_clone.store(true, Ordering::SeqCst);
            pending.clear();
            drop(pending);
            // Tell the connection watcher to run the reconnect ladder.
            let _ = reader_notifications.send(TRANSPORT_CLOSED_NOTIFICATION.to_string());
        });

        let handle = McpHandle {
            name: name.clone(),
            request_id: Arc::new(AtomicU64::new(1)),
            pending,
            closed,
            writer_tx,
            server_info: Arc::new(std::sync::RwLock::new(None)),
            capabilities: Arc::new(std::sync::RwLock::new(ServerCapabilities::default())),
            tools: Arc::new(std::sync::RwLock::new(Vec::new())),
            resources,
            prompts,
            request_timeout: request_timeout_for(config),
            notifications,
            notifications_rx,
        };

        let mut client = Self {
            handle,
            backend: Backend::Stdio(child),
        };

        client
            .initialize()
            .await
            .with_context(|| format!("MCP server '{}' failed to initialize", name))?;

        client
            .handle
            .refresh_tools()
            .await
            .with_context(|| format!("MCP server '{}' failed to list tools", name))?;

        crate::logging::info(&format!(
            "MCP: Connected to '{}' with {} tools",
            name,
            client.handle.tools().len()
        ));

        Ok(client)
    }

    #[allow(clippy::too_many_arguments)]
    fn new_handle(
        name: &str,
        config: &McpServerConfig,
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
        closed: Arc<AtomicBool>,
        writer_tx: mpsc::Sender<String>,
        notifications: mpsc::UnboundedSender<String>,
        notifications_rx: NotificationReceiver,
        resources: Arc<std::sync::RwLock<Option<Vec<McpResourceDef>>>>,
        prompts: Arc<std::sync::RwLock<Option<Vec<McpPromptDef>>>>,
    ) -> McpHandle {
        McpHandle {
            name: name.to_string(),
            request_id: Arc::new(AtomicU64::new(1)),
            pending,
            closed,
            writer_tx,
            server_info: Arc::new(std::sync::RwLock::new(None)),
            capabilities: Arc::new(std::sync::RwLock::new(ServerCapabilities::default())),
            tools: Arc::new(std::sync::RwLock::new(Vec::new())),
            resources,
            prompts,
            request_timeout: request_timeout_for(config),
            notifications,
            notifications_rx,
        }
    }

    /// Bound discovery, authentication refresh, initialization, fallback and
    /// tool listing together. `timeout_secs` can extend legitimate tool calls,
    /// but must not turn connecting to a remote server into an indefinite wait.
    async fn connect_remote(name: String, config: &McpServerConfig, transport: RemoteTransport) -> Result<Self> {
        tokio::time::timeout(
            REMOTE_MCP_HANDSHAKE_TIMEOUT,
            Self::connect_remote_handshake(&name, config, transport),
        )
        .await
        .with_context(|| format!("MCP server '{name}' connection timed out after 30s"))?
    }

    /// Connect to a remote (Streamable HTTP or legacy SSE) MCP server. A
    /// Streamable HTTP endpoint that rejects `initialize` with 400/404/405 is
    /// retried once as legacy SSE, mirroring the spec's backwards-compat rule.
    async fn connect_remote_handshake(
        name: &str,
        config: &McpServerConfig,
        transport: RemoteTransport,
    ) -> Result<Self> {
        crate::logging::info(&format!("MCP: Connecting to '{}' ({})", name, config.display_target()));
        match Self::connect_remote_once(name, config, transport).await {
            Ok(client) => Ok(client),
            Err((err, wants_sse)) if wants_sse && transport == RemoteTransport::StreamableHttp => {
                crate::logging::info(&format!(
                    "MCP: '{}' rejected Streamable HTTP ({:#}); retrying as legacy SSE",
                    name, err
                ));
                Self::connect_remote_once(name, config, RemoteTransport::LegacySse)
                    .await
                    .map_err(|(sse_err, _)| {
                        sse_err.context(format!(
                            "MCP server '{name}' failed over both Streamable HTTP ({err:#}) and SSE"
                        ))
                    })
            }
            Err((err, _)) => Err(err),
        }
    }

    async fn connect_remote_once(
        name: &str,
        config: &McpServerConfig,
        transport: RemoteTransport,
    ) -> std::result::Result<Self, (anyhow::Error, bool)> {
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>> = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let (writer_tx, writer_rx) = mpsc::channel::<String>(32);
        let (notifications, notifications_rx) = notification_channel();
        let connection = super::remote::start(
            name,
            config,
            transport,
            writer_rx,
            Arc::clone(&pending),
            Arc::clone(&closed),
            notifications.clone(),
        )
        .await
        .map_err(|err| (err, false))?;

        let handle = Self::new_handle(
            name,
            config,
            pending,
            closed,
            writer_tx,
            notifications,
            notifications_rx,
            Arc::new(std::sync::RwLock::new(None)),
            Arc::new(std::sync::RwLock::new(None)),
        );
        let mut client = Self {
            handle,
            backend: Backend::Remote(connection),
        };

        if let Err(err) = client.initialize().await {
            let wants_sse = matches!(&client.backend, Backend::Remote(c) if c.wants_sse_fallback());
            return Err((
                err.context(format!("MCP server '{name}' failed to initialize")),
                wants_sse,
            ));
        }
        client
            .handle
            .refresh_tools()
            .await
            .with_context(|| format!("MCP server '{name}' failed to list tools"))
            .map_err(|err| (err, false))?;

        crate::logging::info(&format!(
            "MCP: Connected to '{}' with {} tools",
            name,
            client.handle.tools().len()
        ));
        Ok(client)
    }

    /// Get a shareable handle to this client
    pub fn handle(&self) -> McpHandle {
        self.handle.clone()
    }

    /// Initialize the MCP connection
    async fn initialize(&mut self) -> Result<()> {
        // Remote servers get the newest revision we speak; older servers
        // answer with the version they support, which is fine because horon-agent
        // only uses methods common to every revision.
        let protocol_version = match &self.backend {
            Backend::Stdio(_) => "2024-11-05",
            Backend::Remote(connection) if connection.is_legacy() => "2024-11-05",
            Backend::Remote(_) => "2025-06-18",
        };
        let params = InitializeParams {
            protocol_version: protocol_version.to_string(),
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "horon-agent".to_string(),
                version: horon_agent_build_meta::pkg_version().to_string(),
            },
        };

        let response = self
            .handle
            .request("initialize", Some(serde_json::to_value(params)?))
            .await?;

        {
            let result = response.result.context("MCP initialize response has no result")?;
            let init_result: InitializeResult = serde_json::from_value(result)?;
            if let Backend::Remote(connection) = &self.backend {
                if !matches!(
                    init_result.protocol_version.as_str(),
                    "2024-11-05" | "2025-03-26" | "2025-06-18"
                ) {
                    anyhow::bail!(
                        "MCP server negotiated unsupported protocol version: {}",
                        init_result.protocol_version
                    );
                }
                connection.set_protocol_version(&init_result.protocol_version);
            }
            *self
                .handle
                .server_info
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = init_result.server_info;
            *self
                .handle
                .capabilities
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = init_result.capabilities;
        }

        // Send initialized notification
        let notif = JsonRpcNotification::new("notifications/initialized", None);
        let msg = serde_json::to_string(&notif)? + "\n";
        self.handle.writer_tx.send(msg).await?;

        Ok(())
    }

    /// Check if server is still running
    pub fn is_running(&mut self) -> bool {
        match &mut self.backend {
            Backend::Stdio(child) => matches!(child.try_wait(), Ok(None)),
            Backend::Remote(connection) => connection.is_running(),
        }
    }

    /// Whether this client talks to a remote server.
    pub fn is_remote(&self) -> bool {
        matches!(self.backend, Backend::Remote(_))
    }

    /// Shutdown the server
    pub async fn shutdown(&mut self) {
        match &mut self.backend {
            Backend::Stdio(child) => {
                let _ = self
                    .handle
                    .writer_tx
                    .send("{\"jsonrpc\":\"2.0\",\"method\":\"shutdown\"}\n".to_string())
                    .await;

                tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                let _ = child.kill().await;
            }
            Backend::Remote(connection) => connection.shutdown().await,
        }
    }

    // === Legacy compatibility methods that delegate to handle ===

    pub fn name(&self) -> &str {
        &self.handle.name
    }

    pub fn server_info(&self) -> Option<ServerInfo> {
        self.handle.server_info()
    }

    pub fn tools(&self) -> Vec<McpToolDef> {
        self.handle.tools()
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolCallResult> {
        self.handle.call_tool(name, arguments).await
    }

    pub async fn refresh_tools(&self) -> Result<()> {
        self.handle.refresh_tools().await
    }
}

/// Secrets that an MCP child must not receive merely because horon-agent has them.
///
/// This intentionally applies only to inherited values. A server can still be
/// given any of these names through `McpServerConfig::env`.
fn is_sensitive_inherited_env_key(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    key.ends_with("_API_KEY")
        || key.ends_with("_ACCESS_TOKEN")
        || key.ends_with("_AUTH_TOKEN")
        || matches!(
            key.as_str(),
            "AWS_ACCESS_KEY_ID"
                | "AWS_SECRET_ACCESS_KEY"
                | "AWS_SESSION_TOKEN"
                | "AZURE_CLIENT_SECRET"
                | "GOOGLE_APPLICATION_CREDENTIALS"
        )
}

fn mcp_child_env(
    mut inherited: HashMap<String, String>,
    explicit: &HashMap<String, String>,
) -> HashMap<String, String> {
    inherited.retain(|key, _| !is_sensitive_inherited_env_key(key));
    inherited.extend(explicit.clone());
    inherited
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Backend::Stdio(child) = &mut self.backend {
            let _ = child.start_kill();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{McpClient, is_sensitive_inherited_env_key, mcp_child_env};
    use crate::mcp::protocol::McpServerConfig;
    use std::collections::HashMap;

    #[test]
    fn inherited_mcp_env_scrubs_provider_credentials() {
        for key in [
            "ANTHROPIC_API_KEY",
            "openai_api_key",
            "CURSOR_ACCESS_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "GOOGLE_APPLICATION_CREDENTIALS",
        ] {
            assert!(is_sensitive_inherited_env_key(key), "must scrub {key}");
        }
        for key in ["PATH", "HOME", "RUST_LOG", "HORON_AGENT_OPENROUTER_API_KEY_NAME"] {
            assert!(!is_sensitive_inherited_env_key(key), "must preserve {key}");
        }
    }

    #[test]
    fn explicit_mcp_env_can_opt_a_credential_back_in() {
        let inherited = HashMap::from([
            ("PATH".to_string(), "/bin".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "daemon-secret".to_string()),
        ]);
        let explicit = HashMap::from([("ANTHROPIC_API_KEY".to_string(), "server-specific-secret".to_string())]);

        let env = mcp_child_env(inherited, &explicit);
        assert_eq!(env.get("PATH").map(String::as_str), Some("/bin"));
        assert_eq!(
            env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("server-specific-secret")
        );
    }

    /// A minimal fake stdio MCP server (shell script) that reports its own
    /// process cwd as the serverInfo name.
    fn fake_server_config() -> McpServerConfig {
        let script = r#"
while IFS= read -r line; do
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"%s","version":"0"}}}\n' "$PWD"
      ;;
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}\n'
      ;;
  esac
done
"#;
        McpServerConfig {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            env: Default::default(),
            shared: false,
            transport: None,
            url: None,
            headers: std::collections::HashMap::new(),
            enabled: None,
            disabled: None,
            timeout_secs: None,
        }
    }

    #[tokio::test]
    async fn connect_fails_fast_when_server_exits_before_initialize() {
        // A server that prints to stderr and exits before answering
        // `initialize` must fail connect promptly, even with a huge
        // `timeout_secs` (previously the pending request waited it out).
        let config = McpServerConfig {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "echo boom >&2; exit 1".to_string()],
            timeout_secs: Some(86_400),
            ..fake_server_config()
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            McpClient::connect("dead".to_string(), &config),
        )
        .await
        .expect("connect must not hang on a server that exited");
        let err = format!("{:#}", result.err().expect("connect must fail"));
        assert!(err.contains("exited"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn connect_in_dir_sets_subprocess_cwd() {
        // Issue #557: owned MCP servers must run in the session project dir.
        let dir = tempfile::tempdir().expect("tempdir");
        let expected = dir.path().canonicalize().expect("canonicalize");

        let client = McpClient::connect_in_dir("cwd-test".to_string(), &fake_server_config(), Some(dir.path()))
            .await
            .expect("connect");

        let reported = client.server_info().expect("server info").name;
        assert_eq!(
            std::path::Path::new(&reported)
                .canonicalize()
                .expect("canonicalize reported"),
            expected
        );
    }

    #[tokio::test]
    async fn connect_in_dir_missing_dir_falls_back_to_inherited_cwd() {
        let client = McpClient::connect_in_dir(
            "cwd-fallback-test".to_string(),
            &fake_server_config(),
            Some(std::path::Path::new("/nonexistent/horon-agent-557")),
        )
        .await
        .expect("connect should fall back to inherited cwd");

        let reported = client.server_info().expect("server info").name;
        assert!(!reported.is_empty());
    }
}

#[cfg(all(test, unix))]
#[path = "resources_tests.rs"]
mod resources_tests;
