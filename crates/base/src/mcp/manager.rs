//! MCP Manager - manages MCP server connections for a single session.
//!
//! In daemon mode with a shared pool, servers marked `shared: true` (the default)
//! are managed by the pool and reused across sessions. Servers marked `shared: false`
//! (e.g., Playwright with browser state) are spawned per-session.

use super::client::{McpClient, McpHandle, TRANSPORT_CLOSED_NOTIFICATION};
use super::pool::SharedMcpPool;
use super::protocol::{McpConfig, McpServerConfig, McpToolDef, ToolCallResult};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, mpsc, watch};

/// Bound on how long a tool call will wait for a not-yet-connected MCP server
/// to come up before failing with a clean tool error. Keeps a slow/hanging
/// server from blocking a single tool call forever (and never blocks spawn).
const CONNECT_ON_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Reconnect ladder delays after a transport closes. Four attempts span ~7.5s
/// of downtime, which covers a server restart without hammering it.
const RECONNECT_BACKOFF: [Duration; 4] = [
    Duration::from_millis(500),
    Duration::from_millis(1000),
    Duration::from_millis(2000),
    Duration::from_millis(4000),
];

/// Coalesce a burst of `notifications/tools/list_changed` into one refresh.
const TOOLS_CHANGED_DEBOUNCE: Duration = Duration::from_millis(250);

/// A server that records more than this many connect attempts inside
/// [`BREAKER_WINDOW`] without one succeeding is suspended.
const BREAKER_MAX_ATTEMPTS: usize = 5;
const BREAKER_WINDOW: Duration = Duration::from_secs(30);
const BREAKER_SUSPENSION: Duration = Duration::from_secs(60);

/// The MCP method a server sends when its tool catalog changed.
const TOOLS_LIST_CHANGED: &str = "notifications/tools/list_changed";

type OwnedConnectionResult = Option<std::result::Result<(), String>>;
type OwnedConnectionSender = watch::Sender<OwnedConnectionResult>;
type OwnedConnecting = Arc<Mutex<HashMap<String, OwnedConnectionSender>>>;

#[derive(Default)]
struct BreakerState {
    /// Connect attempts inside the rolling window.
    attempts: Vec<Instant>,
    suspended_until: Option<Instant>,
}

/// Callback the agent registry installs to re-publish a server whose tool
/// catalog changed. Called from the watcher task, so it must not block.
type ToolsChangedCallback = Arc<dyn Fn(String) + Send + Sync>;

/// State shared with the per-connection watcher and reconnect tasks.
///
/// They outlive the call that spawned them, so they cannot borrow the
/// manager. Every field they touch is behind an `Arc`, including the two
/// connection maps: a reconnect publishes its fresh client into the same map
/// the manager reads.
struct McpWatchState {
    on_tools_changed: StdMutex<Option<ToolsChangedCallback>>,
    refresh_locks: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
    /// One connect at a time per server, whichever path asked for it: the
    /// on-demand connect and the background reconnect ladder must not both
    /// spawn a process for the same server.
    connect_locks: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
    breaker: StdMutex<HashMap<String, BreakerState>>,
    /// Servers whose transport just closed on purpose. The watcher must not
    /// reconnect those.
    shutting_down: StdMutex<HashSet<String>>,
    /// Servers with a reconnect ladder in flight, so one close yields one
    /// ladder.
    reconnecting: StdMutex<HashSet<String>>,
    owned_clients: Arc<RwLock<HashMap<String, McpClient>>>,
    pool_handles: Arc<RwLock<HashMap<String, McpHandle>>>,
    config: StdMutex<McpConfig>,
    project_dir: Option<std::path::PathBuf>,
    pool: Option<Arc<SharedMcpPool>>,
}

impl McpWatchState {
    /// Per-server connect lock, shared by every path that can spawn a process.
    fn connect_lock(&self, server: &str) -> Arc<Mutex<()>> {
        let mut locks = self
            .connect_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(locks.entry(server.to_string()).or_default())
    }

    fn config_snapshot(&self) -> McpConfig {
        self.config
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn notify_tools_changed(&self, server: &str) {
        let callback = self
            .on_tools_changed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(callback) = callback {
            callback(server.to_string());
        }
    }

    /// Remaining suspension for a server, in whole seconds.
    fn suspension_seconds(&self, server: &str) -> Option<u64> {
        let mut breaker = self.breaker.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = breaker.get_mut(server)?;
        let until = state.suspended_until?;
        let now = Instant::now();
        if until <= now {
            state.suspended_until = None;
            return None;
        }
        Some(until.duration_since(now).as_secs().max(1))
    }

    /// Record a connect attempt and suspend the server when the window shows
    /// repeated failure. Returns the suspension message when it just tripped.
    fn record_attempt(&self, server: &str) -> Option<String> {
        let now = Instant::now();
        let mut breaker = self.breaker.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = breaker.entry(server.to_string()).or_default();
        state
            .attempts
            .retain(|attempt| now.duration_since(*attempt) < BREAKER_WINDOW);
        state.attempts.push(now);
        if state.attempts.len() > BREAKER_MAX_ATTEMPTS {
            state.attempts.clear();
            state.suspended_until = Some(now + BREAKER_SUSPENSION);
            return Some(BREAKER_SUSPENSION.as_secs().to_string());
        }
        None
    }

    fn record_success(&self, server: &str) {
        let mut breaker = self.breaker.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(state) = breaker.get_mut(server) {
            state.attempts.clear();
            state.suspended_until = None;
        }
    }

    fn reset_breaker(&self) {
        self.breaker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    fn mark_shutting_down(&self, server: &str) {
        self.shutting_down
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(server.to_string());
    }

    fn clear_shutting_down(&self, server: &str) {
        self.shutting_down
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(server);
    }

    fn is_shutting_down(&self, server: &str) -> bool {
        self.shutting_down
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(server)
    }

    /// The live handle for a server, from either connection map.
    async fn handle_for(&self, server: &str) -> Option<McpHandle> {
        if let Some(handle) = self
            .pool_handles
            .read()
            .await
            .get(server)
            .filter(|handle| !handle.is_closed())
        {
            return Some(handle.clone());
        }
        self.owned_clients
            .read()
            .await
            .get(server)
            .filter(|client| !client.handle().is_closed())
            .map(|client| client.handle())
    }

    async fn is_connected(&self, server: &str) -> bool {
        self.handle_for(server).await.is_some()
    }
}

/// Meter a completed tool call for partner-discovery provenance. No-op for
/// servers without discovery provenance (the overwhelmingly common case) and
/// whenever `sponsors.enabled` is false. Counts only; never content.
fn meter_provenance_call(server: &str, result: &Result<ToolCallResult>) {
    let is_error = match result {
        Ok(res) => res.is_error,
        Err(_) => true,
    };
    crate::sponsors::provenance::on_tool_call(server, is_error);
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct McpManagerMemoryProfile {
    pub shared_pool_enabled: bool,
    pub configured_servers: usize,
    pub connected_servers: usize,
    pub pooled_handles: usize,
    pub owned_clients: usize,
    pub available_tools: usize,
    pub configured_json_bytes: usize,
    pub tool_schema_estimate_bytes: usize,
}

/// Manages MCP server connections for a session.
///
/// In daemon mode, shared servers delegate to the SharedMcpPool while
/// non-shared (stateful) servers are owned per-session.
pub struct McpManager {
    pool: Option<Arc<SharedMcpPool>>,
    /// Handles from the shared pool (shared servers). Shared with the
    /// connection watchers, which publish reconnections here.
    pool_handles: Arc<RwLock<HashMap<String, McpHandle>>>,
    /// Per-session owned clients (non-shared / stateful servers).
    owned_clients: Arc<RwLock<HashMap<String, McpClient>>>,
    /// In-flight owned-server handshakes, keyed by server name. This prevents
    /// background startup and a manual connect from spawning two processes for
    /// the same session server at the same time.
    connecting: OwnedConnecting,
    config: McpConfig,
    session_id: String,
    /// Project directory used to resolve project-local MCP config. `None`
    /// loads only global config and never consults the process working directory.
    project_dir: Option<std::path::PathBuf>,
    /// Notification handling, reconnection and the failure breaker.
    watch: Arc<McpWatchState>,
}

struct OwnedConnectionAttemptGuard {
    connecting: OwnedConnecting,
    name: String,
    sender: OwnedConnectionSender,
    armed: bool,
}

impl OwnedConnectionAttemptGuard {
    fn new(connecting: OwnedConnecting, name: &str, sender: OwnedConnectionSender) -> Self {
        Self {
            connecting,
            name: name.to_string(),
            sender,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for OwnedConnectionAttemptGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let connecting = Arc::clone(&self.connecting);
        let name = self.name.clone();
        let sender = self.sender.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };

        handle.spawn(async move {
            let mut connecting = connecting.lock().await;
            let is_current = connecting
                .get(&name)
                .is_some_and(|current| current.same_channel(&sender));
            if is_current {
                connecting.remove(&name);
                let _ = sender.send(Some(Err(format!(
                    "MCP server '{}' connection attempt was cancelled",
                    name
                ))));
            }
        });
    }
}

impl McpManager {
    /// Create a new manager in owned in-process mode (used by tests and local harnesses).
    pub fn new() -> Self {
        let project_dir = std::env::current_dir().ok();
        let config = McpConfig::load_for_dir(project_dir.as_deref());
        Self::build(None, config, "owned".to_string(), project_dir)
    }

    /// Create a manager backed by a shared pool (daemon mode)
    pub fn with_shared_pool(pool: Arc<SharedMcpPool>, session_id: String) -> Self {
        Self::with_shared_pool_for_dir(pool, session_id, None)
    }

    /// Create a manager backed by a shared pool, resolving project-local MCP
    /// config against `project_dir` instead of the server process cwd
    /// (issue #420: remote/client sessions must use the session working dir).
    pub fn with_shared_pool_for_dir(
        pool: Arc<SharedMcpPool>,
        session_id: String,
        project_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let config = McpConfig::load_for_dir(project_dir.as_deref());
        Self::build(Some(pool), config, session_id, project_dir)
    }

    /// Create manager with specific config (no sharing)
    pub fn with_config(config: McpConfig) -> Self {
        Self::build(None, config, "owned".to_string(), None)
    }

    fn build(
        pool: Option<Arc<SharedMcpPool>>,
        config: McpConfig,
        session_id: String,
        project_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let pool_handles = Arc::new(RwLock::new(HashMap::new()));
        let owned_clients = Arc::new(RwLock::new(HashMap::new()));
        let watch = Arc::new(McpWatchState {
            on_tools_changed: StdMutex::new(None),
            refresh_locks: StdMutex::new(HashMap::new()),
            connect_locks: StdMutex::new(HashMap::new()),
            breaker: StdMutex::new(HashMap::new()),
            shutting_down: StdMutex::new(HashSet::new()),
            reconnecting: StdMutex::new(HashSet::new()),
            owned_clients: Arc::clone(&owned_clients),
            pool_handles: Arc::clone(&pool_handles),
            config: StdMutex::new(config.clone()),
            project_dir: project_dir.clone(),
            pool: pool.clone(),
        });
        Self {
            pool,
            pool_handles,
            owned_clients,
            connecting: Arc::new(Mutex::new(HashMap::new())),
            config,
            session_id,
            project_dir,
            watch,
        }
    }

    /// Install the hook that re-publishes a server's tool surface when the
    /// server reports a changed catalog. The agent registry uses this to
    /// refresh its proxies without a manual `reload`.
    pub fn set_on_tools_changed(&self, callback: ToolsChangedCallback) {
        *self
            .watch
            .on_tools_changed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(callback);
    }

    /// Watch a connection for server notifications: a changed tool catalog is
    /// refreshed (debounced, single-flight) and a closed transport starts the
    /// reconnect ladder. Taking the receiver consumes it, so this is a no-op
    /// for a connection another watcher already owns.
    fn watch_connection(&self, name: &str, handle: &McpHandle) {
        let Some(receiver) = handle.take_notifications() else {
            return;
        };
        self.watch.clear_shutting_down(name);
        spawn_connection_watcher(name.to_string(), receiver, Arc::clone(&self.watch));
    }

    /// Whether this manager has a shared pool available
    pub fn is_shared(&self) -> bool {
        self.pool.is_some()
    }

    /// Connect to all configured servers.
    /// Shared servers go to the pool, non-shared are spawned per-session.
    #[expect(
        clippy::collapsible_if,
        reason = "MCP connect flow keeps shared-pool and owned-server paths explicit"
    )]
    pub async fn connect_all(&self) -> Result<(usize, Vec<(String, String)>)> {
        let mut total_successes = 0;
        let mut total_failures = Vec::new();

        // Disabled servers stay in config (so they can be connected on demand
        // by name) but are never auto-spawned (issue #436).
        // Split the rest into shared vs owned.
        let (shared_servers, owned_servers): (Vec<_>, Vec<_>) = self
            .config
            .servers
            .iter()
            .filter(|(_, config)| config.is_enabled())
            .partition(|(_, config)| config.shared && self.pool.is_some());

        // Connect shared servers via pool
        if let Some(pool) = &self.pool {
            if !shared_servers.is_empty() {
                let (successes, failures) = pool.connect_all().await;
                total_successes += successes;
                total_failures.extend(failures);

                // Acquire handles for shared servers only
                let all_handles = pool.acquire_handles(&self.session_id).await;
                let shared_names: std::collections::HashSet<&String> =
                    shared_servers.iter().map(|(name, _)| *name).collect();
                let mut watched = Vec::new();
                let pooled_count = {
                    let mut pool_handles = self.pool_handles.write().await;
                    for (name, handle) in all_handles {
                        if shared_names.contains(&name) {
                            watched.push((name.clone(), handle.clone()));
                            pool_handles.insert(name, handle);
                        }
                    }
                    pool_handles.len()
                };
                for (name, handle) in watched {
                    self.watch_connection(&name, &handle);
                }

                // If pool already had servers connected, count those as successes
                if total_successes == 0 && pooled_count > 0 {
                    total_successes = pooled_count;
                }
            }
        }

        // Connect non-shared servers per-session
        if !owned_servers.is_empty() {
            let connect_futures = owned_servers.into_iter().map(|(name, config)| {
                let name = name.clone();
                let config = config.clone();
                async move {
                    let result = self.connect(&name, &config).await;
                    (name, result)
                }
            });

            for (name, result) in futures::future::join_all(connect_futures).await {
                match result {
                    Ok(()) => total_successes += 1,
                    Err(e) => {
                        let error_msg = format!("{:#}", e);
                        crate::logging::error(&format!("Failed to connect to MCP server '{}': {}", name, error_msg));
                        total_failures.push((name, error_msg));
                    }
                }
            }
        }

        Ok((total_successes, total_failures))
    }

    /// Connect to a specific server
    #[expect(
        clippy::collapsible_if,
        reason = "MCP connect flow keeps shared-pool and owned-server paths explicit"
    )]
    pub async fn connect(&self, name: &str, config: &McpServerConfig) -> Result<()> {
        // Partner-discovery provenance: if this server's command matches a
        // setup the agent saw in a integration_tools listing, tag it so calls to
        // it are metered coarsely (counts only; see sponsors::provenance).
        if let Some(sponsor) = crate::sponsors::provenance::on_server_connected(name, &config.command, &config.args) {
            crate::logging::info(&format!(
                "MCP: '{name}' connected via integration discovery (provider: {sponsor}); \
                 coarse usage counts are shared per the disclosed policy"
            ));
        }
        if config.shared {
            if let Some(pool) = &self.pool {
                pool.connect_server(name, config).await?;
                if let Some(handle) = pool.get_handle(name).await {
                    self.pool_handles.write().await.insert(name.to_string(), handle.clone());
                    self.watch.record_success(name);
                    self.watch_connection(name, &handle);
                }
                return Ok(());
            }
        }

        // Owned (non-shared or no pool available). Coordinate concurrent
        // callers so background startup and a manual connect share one
        // handshake instead of spawning duplicate child processes.
        self.connect_owned(name, config).await
    }

    #[allow(clippy::await_holding_lock)]
    async fn connect_owned(&self, name: &str, config: &McpServerConfig) -> Result<()> {
        if self
            .owned_clients
            .read()
            .await
            .get(name)
            .is_some_and(|client| !client.handle().is_closed())
        {
            return Ok(());
        }
        if let Some(seconds) = self.watch.suspension_seconds(name) {
            anyhow::bail!(
                "MCP server '{name}' is suspended after repeated connection failures; \
                 retry in {seconds}s or run the mcp tool with action=reload."
            );
        }

        let (mut waiter, leader, sender) = {
            let mut connecting = self.connecting.lock().await;
            if let Some(sender) = connecting.get(name) {
                (Some(sender.subscribe()), false, None)
            } else {
                if self
                    .owned_clients
                    .read()
                    .await
                    .get(name)
                    .is_some_and(|client| !client.handle().is_closed())
                {
                    return Ok(());
                }
                let (sender, _receiver) = watch::channel(None);
                connecting.insert(name.to_string(), sender.clone());
                (None, true, Some(sender))
            }
        };

        if !leader {
            let receiver = waiter
                .as_mut()
                .expect("owned MCP connection waiter must have a receiver");
            receiver
                .changed()
                .await
                .with_context(|| format!("MCP server '{}' connection attempt was cancelled", name))?;
            return receiver
                .borrow()
                .clone()
                .unwrap_or_else(|| Err(format!("MCP server '{}' connection did not finish", name)))
                .map_err(anyhow::Error::msg);
        }

        let sender = sender.expect("owned MCP connection leader must have a sender");
        let mut attempt_guard = OwnedConnectionAttemptGuard::new(Arc::clone(&self.connecting), name, sender.clone());
        self.watch.record_attempt(name);
        // A background reconnect ladder can be handshaking this same server
        // right now. Serialize with it and reuse whatever it produced instead
        // of spawning a second process (the tests count `initialize` frames).
        let connect_lock = self.watch.connect_lock(name);
        let _connect_guard = connect_lock.lock().await;
        let already_connected = self
            .owned_clients
            .read()
            .await
            .get(name)
            .is_some_and(|client| !client.handle().is_closed());
        let connection = if already_connected {
            None
        } else {
            Some(McpClient::connect_in_dir(name.to_string(), config, self.project_dir.as_deref()).await)
        };
        drop(_connect_guard);

        let mut connecting = self.connecting.lock().await;
        let mut clients = self.owned_clients.write().await;
        let (result, handle) = match connection {
            Some(Ok(client)) => {
                let handle = client.handle();
                clients.insert(name.to_string(), client);
                (Ok(()), Some(handle))
            }
            Some(Err(error)) => (Err(format!("{:#}", error)), None),
            None => (Ok(()), None),
        };

        if connecting
            .get(name)
            .is_some_and(|current| current.same_channel(&sender))
        {
            connecting.remove(name);
            let _ = sender.send(Some(result.clone()));
        }
        attempt_guard.disarm();
        drop(clients);
        drop(connecting);

        if let Some(handle) = handle {
            self.watch.record_success(name);
            self.watch_connection(name, &handle);
        }

        result.map_err(anyhow::Error::msg)
    }

    /// Disconnect from a server
    pub async fn disconnect(&self, name: &str) -> Result<()> {
        // An intentional disconnect must not trip the reconnect ladder; the
        // watcher checks this flag when the transport-closed signal arrives.
        self.watch.mark_shutting_down(name);
        // Check if it's a pool handle
        {
            let mut handles = self.pool_handles.write().await;
            if handles.remove(name).is_some() {
                if let Some(pool) = &self.pool {
                    pool.release_handles(&self.session_id, &[name.to_string()]).await;
                }
                return Ok(());
            }
        }

        // Otherwise it's owned
        let mut clients = self.owned_clients.write().await;
        if let Some(mut client) = clients.remove(name) {
            client.shutdown().await;
        }
        Ok(())
    }

    /// Disconnect from all servers
    pub async fn disconnect_all(&self) {
        // Session is ending: flush any pending partner-discovery usage
        // aggregates (best effort) so short sessions still report.
        crate::sponsors::provenance::flush_now();
        // Release pool handles
        {
            let mut handles = self.pool_handles.write().await;
            let names: Vec<String> = handles.keys().cloned().collect();
            for name in &names {
                self.watch.mark_shutting_down(name);
            }
            handles.clear();
            if let Some(pool) = &self.pool {
                pool.release_handles(&self.session_id, &names).await;
            }
        }

        // Shutdown owned clients
        {
            let mut clients = self.owned_clients.write().await;
            for (name, mut client) in clients.drain() {
                self.watch.mark_shutting_down(&name);
                client.shutdown().await;
            }
        }
    }

    /// Get list of connected server names
    pub async fn connected_servers(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .pool_handles
            .read()
            .await
            .iter()
            .filter(|(_, handle)| !handle.is_closed())
            .map(|(name, _)| name.clone())
            .collect();
        names.extend(
            self.owned_clients
                .read()
                .await
                .iter()
                .filter(|(_, client)| !client.handle().is_closed())
                .map(|(name, _)| name.clone()),
        );
        names
    }

    /// Get all available tools from all connected servers
    pub async fn all_tools(&self) -> Vec<(String, McpToolDef)> {
        let mut tools = Vec::new();

        // Pool handles
        for (server_name, handle) in self.pool_handles.read().await.iter() {
            if handle.is_closed() {
                continue;
            }
            for tool in handle.tools() {
                tools.push((server_name.clone(), tool));
            }
        }

        // Owned clients
        for (server_name, client) in self.owned_clients.read().await.iter() {
            if client.handle().is_closed() {
                continue;
            }
            for tool in client.tools() {
                tools.push((server_name.clone(), tool));
            }
        }

        tools
    }

    /// Get the best available MCP tool catalog for deferred discovery.
    ///
    /// Live definitions win, while the schema cache fills in enabled servers
    /// that are still connecting. This preserves advertise-early behavior for
    /// the fixed `mcp_search` surface without changing `all_tools()`, whose
    /// callers intentionally operate on connected servers only.
    pub async fn searchable_tools(&self) -> Vec<(String, McpToolDef)> {
        let mut tools: BTreeMap<(String, String), McpToolDef> = BTreeMap::new();
        let schema_cache = super::McpSchemaCache::load_cached();

        for (server, config) in &self.config.servers {
            if !config.is_enabled() {
                continue;
            }
            if let Some(cached) = schema_cache.tools_for(server, config) {
                for tool in cached {
                    tools.insert((server.clone(), tool.name.clone()), tool.clone());
                }
            }
        }

        // Insert live definitions last so schema changes discovered during this
        // session replace stale cache entries immediately.
        for (server, tool) in self.all_tools().await {
            tools.insert((server.clone(), tool.name.clone()), tool);
        }

        tools.into_iter().map(|((server, _), tool)| (server, tool)).collect()
    }

    /// Call a tool on a specific server.
    ///
    /// Connect-on-first-call: if the server is configured but not yet connected
    /// (e.g. because we advertised its tools early from the on-disk schema cache
    /// while the background connection was still settling), this connects it
    /// first, bounded by `CONNECT_ON_CALL_TIMEOUT`. This is the latency we
    /// deliberately deferred from spawn — paid only when a tool is actually
    /// used, never blocking startup.
    pub async fn call_tool(&self, server: &str, tool: &str, arguments: serde_json::Value) -> Result<ToolCallResult> {
        if let Some(seconds) = self.watch.suspension_seconds(server) {
            anyhow::bail!(
                "MCP server '{server}' is suspended after repeated connection failures; \
                 retry in {seconds}s or run the mcp tool with action=reload."
            );
        }

        // Whether a live connection existed before this attempt. A call that
        // started on a live handle may already have reached the server, so its
        // failure is never replayed; only a server that was already gone can be
        // reconnected and retried.
        let had_live_handle = {
            let live = self
                .pool_handles
                .read()
                .await
                .get(server)
                .is_some_and(|handle| !handle.is_closed());
            live || self.owned_handle(server).await.is_some()
        };
        let first = self.call_tool_once(server, tool, arguments.clone()).await;
        let result = match first {
            // One reconnect-and-retry, and only when the failure happened
            // before the request was written: replaying a call the server may
            // already have executed is how duplicates happen.
            Err(error) if !had_live_handle && is_presend_transport_failure(&error) => {
                crate::logging::info(&format!(
                    "MCP: transport to '{server}' was gone before the request was sent; reconnecting once"
                ));
                match self.reconnect_now(server).await {
                    Ok(()) => self.call_tool_once(server, tool, arguments).await,
                    Err(reconnect_error) => {
                        Err(error.context(format!("MCP reconnect for '{server}' failed: {reconnect_error:#}")))
                    }
                }
            }
            other => other,
        };
        // The metering lives inside `call_tool_once`, which is the only place
        // that sees each attempt; a second meter here would count every call
        // twice for a server whose report aggregates by sponsor.
        result
    }

    async fn call_tool_once(&self, server: &str, tool: &str, arguments: serde_json::Value) -> Result<ToolCallResult> {
        // Fast path: already connected via pool handle.
        {
            let handles = self.pool_handles.read().await;
            if let Some(handle) = handles.get(server)
                && !handle.is_closed()
            {
                return handle.call_tool(tool, arguments).await;
            }
        }
        // Fast path: already connected via owned client. The call runs on a
        // cloned handle so the client map is not locked while the server
        // answers: a disconnect must be able to stop a server whose call is
        // still in flight instead of waiting out the request timeout.
        if let Some(handle) = self.owned_handle(server).await {
            let result = handle.call_tool(tool, arguments).await;
            meter_provenance_call(server, &result);
            return result;
        }

        // Not connected yet. If the server is configured, connect-on-first-call.
        if let Some(config) = self.config.servers.get(server).cloned() {
            crate::logging::info(&format!(
                "MCP: connecting to '{server}' on first tool call (connect-on-first-call)"
            ));
            let connect = self.connect(server, &config);
            match tokio::time::timeout(CONNECT_ON_CALL_TIMEOUT, connect).await {
                Ok(Ok(())) => {
                    // Retry once now that we should be connected.
                    {
                        let handles = self.pool_handles.read().await;
                        if let Some(handle) = handles.get(server)
                            && !handle.is_closed()
                        {
                            return handle.call_tool(tool, arguments).await;
                        }
                    }
                    if let Some(handle) = self.owned_handle(server).await {
                        let result = handle.call_tool(tool, arguments).await;
                        meter_provenance_call(server, &result);
                        return result;
                    }
                    anyhow::bail!("MCP server '{server}' connected but exposed no handle for tool '{tool}'");
                }
                Ok(Err(err)) => {
                    anyhow::bail!("MCP server '{server}' failed to connect: {err:#}");
                }
                Err(_) => {
                    anyhow::bail!(
                        "MCP server '{server}' did not connect within {}s; tool '{tool}' is \
                         unavailable right now",
                        CONNECT_ON_CALL_TIMEOUT.as_secs()
                    );
                }
            }
        }

        anyhow::bail!("MCP server '{}' not connected", server)
    }

    /// Handle of the open owned client for `server`. A handle is a cheap clone,
    /// so no lock is held while it is used.
    async fn owned_handle(&self, server: &str) -> Option<McpHandle> {
        self.owned_clients
            .read()
            .await
            .get(server)
            .map(McpClient::handle)
            .filter(|handle| !handle.is_closed())
    }

    /// Reconnect a server once, replacing a closed connection. Used by the
    /// pre-send retry path; the background ladder handles unprompted recovery.
    async fn reconnect_now(&self, server: &str) -> Result<()> {
        let Some(config) = self.config.servers.get(server).cloned() else {
            anyhow::bail!("MCP server '{server}' is not configured");
        };
        if let Some(seconds) = self.watch.suspension_seconds(server) {
            anyhow::bail!(
                "MCP server '{server}' is suspended after repeated connection failures; \
                 retry in {seconds}s or run the mcp tool with action=reload."
            );
        }
        // Drop only a *closed* connection, so connect does not short-circuit on
        // it while a live one restored by the ladder survives.
        {
            let mut clients = self.owned_clients.write().await;
            if clients.get(server).is_some_and(|client| client.handle().is_closed()) {
                clients.remove(server);
            }
        }
        {
            let mut handles = self.pool_handles.write().await;
            if handles.get(server).is_some_and(McpHandle::is_closed) {
                handles.remove(server);
            }
        }
        self.watch.record_attempt(server);
        tokio::time::timeout(CONNECT_ON_CALL_TIMEOUT, self.connect(server, &config))
            .await
            .context("reconnect did not finish in time")??;
        if let Some(handle) = self.watch.handle_for(server).await {
            self.watch.record_success(server);
            self.watch_connection(server, &handle);
        }
        Ok(())
    }

    /// Ensure a configured server is connected, bounded by `timeout`. No-op if
    /// already connected or not configured. Used to warm a server proactively.
    pub async fn ensure_server_connected(&self, server: &str, timeout: std::time::Duration) -> Result<()> {
        if self.connected_servers().await.iter().any(|s| s == server) {
            return Ok(());
        }
        let Some(config) = self.config.servers.get(server).cloned() else {
            anyhow::bail!("MCP server '{server}' is not configured");
        };
        match tokio::time::timeout(timeout, self.connect(server, &config)).await {
            Ok(result) => result,
            Err(_) => anyhow::bail!("MCP server '{server}' did not connect within {}s", timeout.as_secs()),
        }
    }

    /// Reload config and reconnect to servers
    pub async fn reload(&mut self) -> Result<(usize, Vec<(String, String)>)> {
        // Disconnect all (releases pool handles, shuts down owned)
        self.disconnect_all().await;

        // Reload config
        self.config = McpConfig::load_for_dir(self.project_dir.as_deref());
        *self
            .watch
            .config
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self.config.clone();
        // An explicit reload is the escape hatch the suspension message points
        // at, so it must clear the breaker.
        self.watch.reset_breaker();
        self.watch
            .shutting_down
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();

        // If we have a pool, reload it too (reconnects shared servers)
        if let Some(pool) = &self.pool {
            pool.reload().await;
        }

        // Reconnect everything
        self.connect_all().await
    }

    /// Get config
    pub fn config(&self) -> &McpConfig {
        &self.config
    }

    /// Add or replace one server in this manager's own config. Nothing is
    /// written to disk and nothing connects until [`Self::connect`] is called.
    pub fn insert_server(&mut self, name: String, config: McpServerConfig) {
        self.config.servers.insert(name, config);
    }

    /// Run owned stdio servers in `project_dir` (when it exists) instead of
    /// inheriting the daemon's working directory.
    pub fn with_project_dir(mut self, project_dir: Option<std::path::PathBuf>) -> Self {
        self.project_dir = project_dir;
        self
    }

    /// Load a fresh copy of the config from disk, resolved against this
    /// manager's project directory (or the process cwd when unset).
    pub fn load_fresh_config(&self) -> McpConfig {
        McpConfig::load_for_dir(self.project_dir.as_deref())
    }

    pub fn debug_memory_profile(&self) -> McpManagerMemoryProfile {
        let pooled_handles = self.pool_handles.try_read().map(|handles| handles.len()).unwrap_or(0);
        let owned_clients = self.owned_clients.try_read().map(|clients| clients.len()).unwrap_or(0);

        let mut available_tools = 0usize;
        let mut tool_schema_estimate_bytes = 0usize;

        if let Ok(handles) = self.pool_handles.try_read() {
            for handle in handles.values() {
                for tool in handle.tools() {
                    available_tools += 1;
                    tool_schema_estimate_bytes += estimate_tool_bytes(&tool);
                }
            }
        }

        if let Ok(clients) = self.owned_clients.try_read() {
            for client in clients.values() {
                for tool in client.tools() {
                    available_tools += 1;
                    tool_schema_estimate_bytes += estimate_tool_bytes(&tool);
                }
            }
        }

        McpManagerMemoryProfile {
            shared_pool_enabled: self.pool.is_some(),
            configured_servers: self.config.servers.len(),
            connected_servers: pooled_handles + owned_clients,
            pooled_handles,
            owned_clients,
            available_tools,
            configured_json_bytes: crate::process_memory::estimate_json_bytes(&self.config),
            tool_schema_estimate_bytes,
        }
    }

    /// Check if any servers are connected
    pub async fn has_connections(&self) -> bool {
        !self.pool_handles.read().await.is_empty() || !self.owned_clients.read().await.is_empty()
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

fn estimate_tool_bytes(tool: &McpToolDef) -> usize {
    tool.name.len()
        + tool.description.as_ref().map(|value| value.len()).unwrap_or(0)
        + crate::process_memory::estimate_json_bytes(&tool.input_schema)
}

/// Whether a failed MCP call is safe to retry.
///
/// Only failures that happened before the request was written qualify: a
/// connect error, a handle that was already closed, or a transport that died
/// while the request was still queued. A request the server received and then
/// failed or timed out is never replayed — the server may have executed it.
fn is_presend_transport_failure(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_ascii_lowercase();
    if text.contains("request timeout") {
        return false;
    }
    const NEEDLES: &[&str] = &[
        "econnrefused",
        "connection refused",
        "econnreset",
        "connection reset",
        "epipe",
        "broken pipe",
        "transport closed",
        "not connected",
        "connection closed",
        "failed to send request",
        // Remote: an HTTP failure on session initialization means nothing was
        // accepted yet.
        "http 404",
        "http 502",
        "http 503",
        "status 404",
        "status 502",
        "status 503",
    ];
    NEEDLES.iter().any(|needle| text.contains(needle))
}

/// What a debounced notification burst ended with.
enum Burst {
    /// The debounce window elapsed: refresh now.
    Elapsed,
    /// The channel closed with no transport signal; the watcher is done.
    Closed,
    /// The transport closed inside the window.
    TransportClosed,
}

/// Coalesce a burst of notifications so a server that rewrites its catalog
/// repeatedly causes one refresh, not one per notification.
async fn drain_notification_burst(receiver: &mut mpsc::UnboundedReceiver<String>) -> Burst {
    let deadline = tokio::time::Instant::now() + TOOLS_CHANGED_DEBOUNCE;
    loop {
        match tokio::time::timeout_at(deadline, receiver.recv()).await {
            Ok(Some(method)) if method == TRANSPORT_CLOSED_NOTIFICATION => return Burst::TransportClosed,
            Ok(Some(_)) => continue,
            Ok(None) => return Burst::Closed,
            Err(_) => return Burst::Elapsed,
        }
    }
}

fn spawn_connection_watcher(server: String, receiver: mpsc::UnboundedReceiver<String>, state: Arc<McpWatchState>) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    runtime.spawn(async move {
        watch_connection(server, receiver, state).await;
    });
}

/// One watcher per connection: refresh the catalog when the server says it
/// changed, and reconnect when the transport dies.
async fn watch_connection(server: String, mut receiver: mpsc::UnboundedReceiver<String>, state: Arc<McpWatchState>) {
    loop {
        let Some(method) = receiver.recv().await else {
            // Every sender is gone: the connection was dropped for good.
            return;
        };
        if method == TRANSPORT_CLOSED_NOTIFICATION {
            if state.is_shutting_down(&server) {
                return;
            }
            schedule_reconnect(server, state);
            return;
        }
        if method != TOOLS_LIST_CHANGED {
            crate::logging::debug(&format!("MCP [{server}]: ignoring notification {method}"));
            continue;
        }
        match drain_notification_burst(&mut receiver).await {
            Burst::Elapsed => refresh_tools_after_notification(&server, &state).await,
            Burst::Closed => return,
            Burst::TransportClosed => {
                if state.is_shutting_down(&server) {
                    return;
                }
                schedule_reconnect(server, state);
                return;
            }
        }
    }
}

async fn refresh_tools_after_notification(server: &str, state: &Arc<McpWatchState>) {
    // Single-flight per server: a burst that survived the debounce and a
    // second notification arriving meanwhile must not run two refreshes.
    let lock = {
        let mut locks = state
            .refresh_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(locks.entry(server.to_string()).or_default())
    };
    let _guard = lock.lock().await;
    let Some(handle) = state.handle_for(server).await else {
        return;
    };
    match handle.refresh_tools().await {
        Ok(()) => {
            crate::logging::info(&format!("MCP [{server}]: applied notifications/tools/list_changed"));
            state.notify_tools_changed(server);
        }
        Err(error) => crate::logging::warn(&format!(
            "MCP [{server}]: refreshing tools after list_changed failed: {error:#}"
        )),
    }
}

fn schedule_reconnect(server: String, state: Arc<McpWatchState>) {
    {
        let mut reconnecting = state
            .reconnecting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !reconnecting.insert(server.clone()) {
            return;
        }
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        state
            .reconnecting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&server);
        return;
    };
    runtime.spawn(async move {
        reconnect_ladder(&server, &state).await;
        state
            .reconnecting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&server);
    });
}

async fn reconnect_ladder(server: &str, state: &Arc<McpWatchState>) {
    for delay in RECONNECT_BACKOFF {
        tokio::time::sleep(delay).await;
        if state.is_connected(server).await {
            return;
        }
        if state.suspension_seconds(server).is_some() {
            crate::logging::debug(&format!("MCP [{server}]: reconnect suspended by the failure breaker"));
            return;
        }
        match reconnect_once(server, state).await {
            Ok(handle) => {
                crate::logging::info(&format!("MCP [{server}]: reconnected after the transport closed"));
                state.record_success(server);
                state.notify_tools_changed(server);
                if let Some(receiver) = handle.take_notifications() {
                    spawn_connection_watcher(server.to_string(), receiver, Arc::clone(state));
                }
                return;
            }
            Err(error) => crate::logging::debug(&format!("MCP [{server}]: reconnect attempt failed: {error:#}")),
        }
    }
    crate::logging::warn(&format!(
        "MCP [{server}]: reconnecting failed after {} attempts",
        RECONNECT_BACKOFF.len()
    ));
}

async fn reconnect_once(server: &str, state: &Arc<McpWatchState>) -> Result<McpHandle> {
    // Same lock the on-demand connect path takes: if it already restored the
    // server, reuse that connection instead of starting a second process.
    let connect_lock = state.connect_lock(server);
    let _connect_guard = connect_lock.lock().await;
    if let Some(handle) = state.handle_for(server).await {
        return Ok(handle);
    }
    let config = state
        .config_snapshot()
        .servers
        .get(server)
        .cloned()
        .with_context(|| format!("MCP server '{server}' is no longer configured"))?;
    state.record_attempt(server);
    if config.shared
        && let Some(pool) = &state.pool
    {
        pool.connect_server(server, &config).await?;
        let handle = pool
            .get_handle(server)
            .await
            .context("the pool exposed no handle after reconnecting")?;
        state
            .pool_handles
            .write()
            .await
            .insert(server.to_string(), handle.clone());
        return Ok(handle);
    }
    let client = McpClient::connect_in_dir(server.to_string(), &config, state.project_dir.as_deref()).await?;
    let handle = client.handle();
    state.owned_clients.write().await.insert(server.to_string(), client);
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn empty_config() -> McpConfig {
        McpConfig::default()
    }

    #[test]
    fn issue_790_owned_manager_keeps_its_initial_config_directory() {
        let _guard = crate::storage::lock_test_env();
        let original_cwd = std::env::current_dir().expect("current cwd");
        let previous_home = std::env::var_os("HORON_HOME");
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let elsewhere = tempfile::tempdir().expect("elsewhere tempdir");
        crate::env::set_var("HORON_HOME", home.path());
        std::fs::write(
            project.path().join(".mcp.json"),
            r#"{"mcpServers":{"project-only":{"command":"project-server"}}}"#,
        )
        .expect("write project config");
        crate::workspace_trust::trust_for_tests(project.path());

        std::env::set_current_dir(project.path()).expect("set project cwd");
        let manager = McpManager::new();
        std::env::set_current_dir(elsewhere.path()).expect("set different cwd");
        let fresh = manager.load_fresh_config();

        std::env::set_current_dir(original_cwd).expect("restore cwd");
        if let Some(previous_home) = previous_home {
            crate::env::set_var("HORON_HOME", previous_home);
        } else {
            crate::env::remove_var("HORON_HOME");
        }

        assert!(fresh.servers.contains_key("project-only"));
    }

    #[tokio::test]
    async fn call_tool_unconfigured_server_bails_cleanly() {
        let manager = McpManager::with_config(empty_config());
        let err = manager
            .call_tool("ghost", "do_thing", serde_json::json!({}))
            .await
            .expect_err("calling an unknown server must error");
        assert!(
            err.to_string().contains("ghost"),
            "error should name the missing server: {err}"
        );
    }

    #[tokio::test]
    async fn ensure_server_connected_unconfigured_errors() {
        let manager = McpManager::with_config(empty_config());
        let err = manager
            .ensure_server_connected("ghost", Duration::from_millis(50))
            .await
            .expect_err("ensuring an unconfigured server must error");
        assert!(err.to_string().contains("not configured"), "{err}");
    }

    #[tokio::test]
    async fn connect_all_skips_disabled_servers() {
        // Issue #436: disabled servers stay in config but are never
        // auto-spawned. This config's command would fail to connect (and thus
        // produce a failure entry) if it were attempted at all.
        let mut config = McpConfig::default();
        config.servers.insert(
            "off".to_string(),
            McpServerConfig {
                command: "true".to_string(),
                args: vec![],
                env: HashMap::new(),
                shared: false,
                transport: None,
                url: None,
                headers: std::collections::HashMap::new(),
                enabled: Some(false),
                disabled: None,
                timeout_secs: None,
            },
        );
        let manager = McpManager::with_config(config);
        let (successes, failures) = manager.connect_all().await.expect("connect_all");
        assert_eq!(successes, 0, "disabled server must not be spawned");
        assert!(
            failures.is_empty(),
            "disabled server must not be attempted: {failures:?}"
        );
        assert!(manager.connected_servers().await.is_empty());
        // Still present in config so it can be connected on demand by name.
        assert!(manager.config().servers.contains_key("off"));
    }

    #[tokio::test]
    async fn connect_on_first_call_fails_cleanly_for_broken_server() {
        // A configured server whose command exits immediately and never speaks
        // MCP. connect-on-first-call must surface a clean, bounded tool error
        // (connection failure) rather than hanging or panicking.
        let mut config = McpConfig::default();
        config.servers.insert(
            "broken".to_string(),
            McpServerConfig {
                // `true` exits 0 immediately: the stdio handshake gets EOF, so
                // connect fails fast instead of waiting on the initialize bound.
                command: "true".to_string(),
                args: vec![],
                env: HashMap::new(),
                shared: false,
                transport: None,
                url: None,
                headers: std::collections::HashMap::new(),
                enabled: None,
                disabled: None,
                timeout_secs: None,
            },
        );
        let manager = McpManager::with_config(config);

        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(40),
            manager.call_tool("broken", "anything", serde_json::json!({})),
        )
        .await;
        let inner = result.expect("call_tool must return, not hang");
        assert!(inner.is_err(), "broken server must yield a tool error");
        let msg = inner.unwrap_err().to_string();
        assert!(msg.contains("broken"), "tool error should name the server: {msg}");
        assert!(
            started.elapsed() < Duration::from_secs(35),
            "connect-on-first-call must be bounded"
        );
    }

    #[tokio::test]
    async fn cancelled_owned_connection_notifies_waiters_and_clears_entry() {
        let manager = McpManager::with_config(empty_config());
        let (sender, _receiver) = watch::channel(None);
        manager
            .connecting
            .lock()
            .await
            .insert("cancelled".to_string(), sender.clone());
        let mut waiter = sender.subscribe();

        drop(OwnedConnectionAttemptGuard::new(
            Arc::clone(&manager.connecting),
            "cancelled",
            sender,
        ));

        tokio::time::timeout(Duration::from_secs(1), waiter.changed())
            .await
            .expect("cancellation cleanup must notify waiters")
            .expect("cancellation sender must remain alive");
        assert_eq!(
            waiter
                .borrow()
                .as_ref()
                .and_then(|result| result.as_ref().err())
                .map(String::as_str),
            Some("MCP server 'cancelled' connection attempt was cancelled")
        );
        assert!(!manager.connecting.lock().await.contains_key("cancelled"));
    }

    #[cfg(unix)]
    #[tokio::test]
    // `disconnect_all` flushes process-global sponsor provenance counters, so
    // hold the test-env lock to avoid draining another test's pending reports.
    #[allow(clippy::await_holding_lock)]
    async fn concurrent_owned_connects_spawn_only_one_server() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;

        let _env_lock = crate::storage::lock_test_env();

        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("counted-mcp.sh");
        let count = temp.path().join("starts");
        std::fs::write(
            &script,
            r##"#!/bin/sh
printf 'started\n' >> "$MCP_START_COUNT"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"counted","version":"0.0.1"}}}'
      ;;
    *'"tools/list"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"tools":[]}}'
      ;;
    *'"shutdown"'*)
      exit 0
      ;;
  esac
done
"##,
        )
        .expect("write MCP script");
        let mut permissions = std::fs::metadata(&script).expect("script metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("make script executable");

        let mut config = McpConfig::default();
        let mut env = HashMap::new();
        env.insert("MCP_START_COUNT".to_string(), count.display().to_string());
        config.servers.insert(
            "counted".to_string(),
            McpServerConfig {
                command: script.display().to_string(),
                args: Vec::new(),
                env,
                shared: false,
                transport: None,
                url: None,
                headers: HashMap::new(),
                enabled: None,
                disabled: None,
                timeout_secs: Some(10),
            },
        );
        let config = config.servers.get("counted").cloned().expect("server config");
        let manager = Arc::new(McpManager::with_config({
            let mut cfg = McpConfig::default();
            cfg.servers.insert("counted".to_string(), config.clone());
            cfg
        }));

        let background = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move { manager.connect_all().await })
        };
        let manual = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move { manager.connect("counted", &config).await })
        };

        let (_, failures) = background
            .await
            .expect("background task join")
            .expect("background connect_all");
        assert!(failures.is_empty(), "background failures: {failures:?}");
        manual.await.expect("manual task join").expect("manual connect");

        let starts = std::fs::read_to_string(&count).expect("read start count");
        assert_eq!(starts.lines().count(), 1, "server starts: {starts:?}");
        manager.disconnect_all().await;
    }

    #[test]
    fn breaker_suspends_after_more_than_five_attempts_in_the_window() {
        let manager = McpManager::with_config(empty_config());
        for _ in 0..BREAKER_MAX_ATTEMPTS {
            assert!(
                manager.watch.record_attempt("flaky").is_none(),
                "the breaker must not trip inside the allowed attempt count"
            );
        }
        assert!(
            manager.watch.record_attempt("flaky").is_some(),
            "the sixth attempt inside the window must suspend the server"
        );
        assert!(
            manager
                .watch
                .suspension_seconds("flaky")
                .is_some_and(|seconds| seconds <= BREAKER_SUSPENSION.as_secs()),
            "a suspended server reports the remaining wait"
        );

        // A different server is unaffected.
        assert!(manager.watch.suspension_seconds("healthy").is_none());

        // An explicit reload is the documented escape hatch.
        manager.watch.reset_breaker();
        assert!(manager.watch.suspension_seconds("flaky").is_none());
    }

    #[test]
    fn a_successful_connect_clears_the_breaker_history() {
        let manager = McpManager::with_config(empty_config());
        for _ in 0..BREAKER_MAX_ATTEMPTS {
            manager.watch.record_attempt("flapping");
        }
        manager.watch.record_success("flapping");
        for _ in 0..BREAKER_MAX_ATTEMPTS {
            assert!(manager.watch.record_attempt("flapping").is_none());
        }
    }

    #[tokio::test]
    async fn a_suspended_server_fails_the_tool_call_with_the_reload_hint() {
        let manager = McpManager::with_config(empty_config());
        for _ in 0..=BREAKER_MAX_ATTEMPTS {
            manager.watch.record_attempt("suspended");
        }
        let error = manager
            .call_tool("suspended", "anything", serde_json::json!({}))
            .await
            .expect_err("a suspended server must refuse the call");
        let message = error.to_string();
        assert!(
            message.starts_with("MCP server 'suspended' is suspended after repeated connection failures; retry in "),
            "{message}"
        );
        assert!(message.contains("action=reload"), "{message}");
    }

    #[test]
    fn only_pre_send_transport_failures_are_retryable() {
        for message in [
            "MCP server 'x' exited (connection closed)",
            "Failed to send request",
            "transport closed",
            "connect error: Connection refused (os error 111)",
            "MCP server 'x' not connected",
            "HTTP 502 Bad Gateway",
        ] {
            assert!(
                is_presend_transport_failure(&anyhow::anyhow!(message)),
                "{message} happened before the request was written"
            );
        }
        for message in [
            "Request timeout after 30s (raise `timeout_secs` for MCP server 'x' if its tools legitimately run longer)",
            "MCP error -32602: invalid arguments",
        ] {
            assert!(
                !is_presend_transport_failure(&anyhow::anyhow!(message)),
                "{message} may have already run on the server"
            );
        }
    }
}

#[cfg(all(test, unix))]
mod provenance_integration_tests {
    use super::*;
    use std::io::Write;

    /// Write a minimal stdio MCP server as a shell script: answers
    /// initialize, tools/list, and tools/call with canned JSON-RPC replies.
    fn write_fake_mcp_server(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("fake-mcp-server.sh");
        let script = r##"#!/bin/bash
while IFS= read -r line; do
  id=$(echo "$line" | grep -o '"id":[0-9]*' | grep -o '[0-9]*' | head -1)
  case "$line" in
    *'"initialize"'*)
      echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"0.0.1"}}}'
      ;;
    *'"tools/list"'*)
      echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"tools":[{"name":"create_card","description":"fake card","inputSchema":{"type":"object"}}]}}'
      ;;
    *'"tools/call"'*)
      echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"content":[{"type":"text","text":"card created"}],"isError":false}}'
      ;;
    *'"shutdown"'*)
      exit 0
      ;;
  esac
done
"##;
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        drop(file);
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    /// Full loop: discovery records a setup, connecting a matching server
    /// tags provenance and counts the connect, real MCP tool calls through
    /// the manager are metered, non-matching servers are not.
    #[tokio::test]
    // The test-env mutex must intentionally stay held across awaits so the
    // HORON_HOME/config mutation stays serialized for the whole test.
    #[allow(clippy::await_holding_lock)]
    async fn discovery_provenance_end_to_end_with_real_mcp_server() {
        let env_guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().unwrap();
        crate::env::set_var("HORON_HOME", temp.path());
        std::fs::write(temp.path().join("config.toml"), "[sponsors]\nenabled = true\n").unwrap();
        crate::config::Config::invalidate_cache();
        crate::sponsors::provenance::reset_for_tests();

        let server_path = write_fake_mcp_server(temp.path());
        let command = server_path.to_string_lossy().to_string();

        // 1. Discovery listing recorded this setup.
        crate::sponsors::provenance::record_discovered_setups(vec![crate::sponsors::provenance::DiscoveredSetup {
            sponsor: "agentcard".into(),
            command: command.clone(),
            args: vec![],
        }]);

        // 2. Agent connects the matching server through the real manager.
        let mut config = McpConfig::default();
        config.servers.insert(
            "agentcard".to_string(),
            McpServerConfig {
                command: command.clone(),
                args: vec![],
                env: HashMap::new(),
                shared: false,
                transport: None,
                url: None,
                headers: std::collections::HashMap::new(),
                enabled: None,
                disabled: None,
                timeout_secs: None,
            },
        );
        let manager = McpManager::with_config(config.clone());
        let server_config = config.servers.get("agentcard").unwrap().clone();
        manager
            .connect("agentcard", &server_config)
            .await
            .expect("fake MCP server must connect");
        assert!(crate::sponsors::provenance::is_tagged("agentcard"));

        // 3. Real tool calls through the manager are metered.
        let result = manager
            .call_tool("agentcard", "create_card", serde_json::json!({}))
            .await
            .expect("tool call through fake server");
        assert!(!result.is_error);

        // 4. A second, non-discovered server with a different command is
        // never tagged.
        assert!(!crate::sponsors::provenance::is_tagged("other"));

        // 5. Pending aggregates hold exactly the connect + the call.
        let reports = crate::sponsors::provenance::drain_pending_for_tests();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].sponsor, "agentcard");
        assert_eq!(reports[0].connects, 1);
        assert_eq!(reports[0].calls, 1);
        assert_eq!(reports[0].errors, 0);

        manager.disconnect_all().await;
        drop(env_guard);
    }
}
