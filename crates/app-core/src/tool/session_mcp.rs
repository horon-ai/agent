//! MCP servers an ACP client attaches to one session.
//!
//! ACP editors pass `mcpServers` on `session/new`, `session/load` and
//! `session/resume`; the adapter forwards them as `ConfigureSessionMcp`. The
//! daemon connects them here, owned by exactly one (connection, session) pair:
//!
//! - one [`McpManager`] per installation, built from an empty config, so no
//!   user or project `mcp.json` is read, written or trusted on its behalf;
//! - every server is `shared: false`, so the shared pool never hands its
//!   process to another session;
//! - each server connects in its own task, and its proxies go into the
//!   session registry's [`Overlay`], never into the registry's own tools;
//! - [`SessionMcp::shutdown`] cancels pending connects, withdraws the overlay
//!   and stops the processes. The connection calls it on disconnect, on a
//!   re-subscribe and when the editor sends a new server list.
//!
//! The overlay is what keeps the editor's servers to the editor (IS-12). It is
//! visible only while the running turn was started by the ACP connection that
//! installed it ([`acp_session::turn_owned_by`], the rule the client, permission
//! and approval bridges follow); for every other turn, a plain client attached
//! to the same session or a daemon turn, it does not exist. While it is
//! visible, an editor server shadows a configured server of the same name on
//! every path (direct dispatch, `mcp_search`, `mcp_call`). Configured proxies
//! are never touched, so withdrawing the overlay is the whole restore.
//!
//! The overlay proxies hold the manager, so dropping a `SessionMcp` without
//! `shutdown` would leave the processes running; every owner must shut down.

use super::{Registry, Tool, ToolContext, ToolOutput, WeakRegistry, acp_session};
use crate::extensions::lifecycle::{PackageLifecycleHolder, PackageLifecycleRegistry, RevocationCallback};
use crate::extensions::package_snapshot::{PackageSession, lease_owner};
use crate::hosting::HostingPolicy;
use crate::mcp::hosted::{HostedOperation, HostedRequest};
use crate::mcp::{
    ContentBlock, McpClient, McpConfig, McpHandle, McpManager, McpServerConfig, McpToolDef, dispatch_names,
    package_servers, render_resource_content, take_accept_large_output,
};
use crate::packages::{PackageKind, PackageMcpKind, PackageProcess, PackageRecord, PackageState, PackageStore};
use crate::protocol::{ServerEvent, SessionMcpServer};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex, PoisonError};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinSet;

/// Turn the editor's server list into manager configs, refusing the whole
/// list on the first invalid or refused entry so a session never runs half of it.
///
/// The policy is a parameter because the request is gated against it before any
/// connection starts; the caller passes the process policy. The whole request
/// fails when one server is refused, which is the error the ACP client receives:
/// an editor never sees a silently reduced list.
pub(crate) fn server_configs(
    policy: &HostingPolicy,
    servers: Vec<SessionMcpServer>,
) -> Result<Vec<(String, McpServerConfig)>, String> {
    let mut seen = HashSet::new();
    servers
        .into_iter()
        .map(|server| {
            let name = server.name.trim().to_string();
            if name.is_empty() {
                return Err("MCP server name must not be empty".to_string());
            }
            if !seen.insert(name.clone()) {
                return Err(format!("MCP server {name:?} is listed more than once"));
            }
            let config = server_config(&name, server)?;
            let operation = if config.remote_transport().is_some() {
                HostedOperation::Remote
            } else {
                HostedOperation::SessionStdio
            };
            crate::mcp::hosted::check(policy, &HostedRequest::server(operation, &name))
                .map_err(|refusal| refusal.to_string())?;
            Ok((name, config))
        })
        .collect()
}

fn server_config(name: &str, server: SessionMcpServer) -> Result<McpServerConfig, String> {
    let mut config = McpServerConfig {
        command: String::new(),
        args: Vec::new(),
        env: HashMap::new(),
        shared: false,
        transport: None,
        url: None,
        headers: HashMap::new(),
        enabled: None,
        disabled: None,
        timeout_secs: None,
    };
    match server.transport.as_str() {
        "stdio" => {
            let command = server
                .command
                .filter(|command| !command.trim().is_empty())
                .ok_or_else(|| format!("MCP server {name:?}: the stdio transport needs a command"))?;
            config.command = command;
            config.args = server.args;
            config.env = server.env.into_iter().collect();
            config.transport = Some("stdio".to_string());
        }
        transport @ ("http" | "sse") => {
            let url = server
                .url
                .ok_or_else(|| format!("MCP server {name:?}: the {transport} transport needs a url"))?;
            validate_remote_url(name, &url)?;
            config.url = Some(url);
            config.headers = server.headers.into_iter().collect();
            config.transport = Some(transport.to_string());
        }
        other => return Err(format!("MCP server {name:?}: unknown transport {other:?}")),
    }
    Ok(config)
}

fn validate_remote_url(name: &str, raw: &str) -> Result<(), String> {
    let invalid = || format!("MCP server {name:?}: url must be an http(s) URL without embedded credentials");
    let url = url::Url::parse(raw).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid());
    }
    Ok(())
}

/// The editor-provided proxies of one session, kept beside a registry's own
/// tools. Shared by every clone of that registry.
#[derive(Default)]
pub(crate) struct Overlay {
    installed: Option<Installation>,
}

struct Installation {
    token: u64,
    session_id: String,
    connection_id: String,
    servers: HashSet<String>,
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl Overlay {
    /// The overlay as the running turn sees it: `None` unless that turn was
    /// started by the connection that installed it.
    fn active(&self) -> Option<ActiveOverlay> {
        let installed = self.installed.as_ref()?;
        acp_session::turn_owned_by(&installed.session_id, &installed.connection_id).then(|| ActiveOverlay {
            servers: installed.servers.clone(),
            tools: installed.tools.clone(),
        })
    }
}

/// Editor proxies visible to the current turn, and the server names they
/// shadow.
pub(crate) struct ActiveOverlay {
    servers: HashSet<String>,
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ActiveOverlay {
    /// The editor proxy registered as `name`.
    pub(crate) fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Whether the registry's own tool `name` is hidden for this turn: an
    /// editor proxy holds the name, or the tool belongs to a shadowed server.
    pub(crate) fn hides(&self, name: &str, tool: &Arc<dyn Tool>) -> bool {
        self.tools.contains_key(name)
            || tool
                .mcp_identity()
                .is_some_and(|(server, _)| self.servers.contains(server))
    }

    /// Whether `server` is one of the editor's servers.
    pub(crate) fn serves(&self, server: &str) -> bool {
        self.servers.contains(server)
    }

    pub(crate) fn tools(&self) -> &HashMap<String, Arc<dyn Tool>> {
        &self.tools
    }

    /// Original `(server, tool)` of an editor alias.
    pub(crate) fn identity(&self, alias: &str) -> Option<(String, String)> {
        let (server, raw) = self.tools.get(alias)?.mcp_identity()?;
        Some((server.to_string(), raw.to_string()))
    }

    /// The proxy for `raw` on the editor server `server`, with its alias.
    pub(crate) fn find(&self, server: &str, raw: &str) -> Option<(String, Arc<dyn Tool>)> {
        self.tools
            .iter()
            .find(|(_, tool)| tool.mcp_identity() == Some((server, raw)))
            .map(|(alias, tool)| (alias.clone(), Arc::clone(tool)))
    }

    /// `(server, definition, alias)` of every editor tool, sorted by alias.
    pub(crate) fn entries(&self) -> Vec<(String, McpToolDef, String)> {
        let mut entries: Vec<_> = self
            .tools
            .iter()
            .filter_map(|(alias, tool)| {
                let (server, raw) = tool.mcp_identity()?;
                let definition = McpToolDef {
                    name: raw.to_string(),
                    description: Some(tool.description().to_string()),
                    input_schema: tool.parameters_schema(),
                };
                Some((server.to_string(), definition, alias.clone()))
            })
            .collect();
        entries.sort_by(|left, right| left.2.cmp(&right.2));
        entries
    }
}

impl Registry {
    /// Editor proxies the turn running now may see; `None` for every turn
    /// the installing ACP connection did not start.
    pub(crate) fn session_mcp(&self) -> Option<ActiveOverlay> {
        self.session_mcp.read().unwrap_or_else(PoisonError::into_inner).active()
    }

    /// Publish the complete proxy set of installation `token`. A newer
    /// installation (another ACP connection took the session) is never
    /// overwritten by an older one.
    fn publish_session_mcp(
        &self,
        token: u64,
        session_id: &str,
        connection_id: &str,
        servers: HashSet<String>,
        tools: Vec<(String, Arc<dyn Tool>)>,
    ) {
        let mut overlay = self.session_mcp.write().unwrap_or_else(PoisonError::into_inner);
        if overlay
            .installed
            .as_ref()
            .is_some_and(|installed| installed.token > token)
        {
            return;
        }
        overlay.installed = Some(Installation {
            token,
            session_id: session_id.to_string(),
            connection_id: connection_id.to_string(),
            servers,
            tools: tools.into_iter().collect(),
        });
    }

    /// Withdraw installation `token`, and only that one.
    fn withdraw_session_mcp(&self, token: u64) {
        let mut overlay = self.session_mcp.write().unwrap_or_else(PoisonError::into_inner);
        if overlay
            .installed
            .as_ref()
            .is_some_and(|installed| installed.token == token)
        {
            overlay.installed = None;
        }
    }
}

/// The editor-provided servers of one session on one connection.
pub(crate) struct SessionMcp {
    token: u64,
    manager: Arc<RwLock<McpManager>>,
    registry: Shared<BoxFuture<'static, Registry>>,
    connects: JoinSet<()>,
}

impl SessionMcp {
    /// Start connecting `configs` for `session_id` on the ACP connection
    /// `connection_id`. Returns at once; each server publishes its tools when
    /// its own handshake finishes. A server that fails or never answers only
    /// logs: its handshake is bounded by the MCP client's request timeout and
    /// other servers are not held back.
    ///
    /// `registry` resolves to the registry the session's turns use. It is
    /// awaited only after a server connected, so a turn holding the agent
    /// never blocks the connection that sent the request.
    pub(crate) fn start(
        session_id: &str,
        connection_id: &str,
        configs: Vec<(String, McpServerConfig)>,
        working_dir: Option<PathBuf>,
        registry: BoxFuture<'static, Registry>,
        events: mpsc::UnboundedSender<ServerEvent>,
    ) -> Self {
        static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        let mut manager = McpManager::with_config(McpConfig::default()).with_project_dir(working_dir);
        for (name, config) in &configs {
            manager.insert_server(name.clone(), config.clone());
        }
        let manager = Arc::new(RwLock::new(manager));
        let registry = registry.shared();
        let servers: HashSet<String> = configs.iter().map(|(name, _)| name.clone()).collect();
        // Each publish lists every connected server, so publishes are
        // serialized: the last one always sees every finished handshake.
        let publish = Arc::new(Mutex::new(()));
        let mut connects = JoinSet::new();
        for (name, config) in configs {
            let manager = Arc::clone(&manager);
            let registry = registry.clone();
            let events = events.clone();
            let servers = servers.clone();
            let publish = Arc::clone(&publish);
            let session_id = session_id.to_string();
            let connection_id = connection_id.to_string();
            connects.spawn(async move {
                if let Err(error) = manager.read().await.connect(&name, &config).await {
                    crate::logging::warn(&format!(
                        "ACP MCP: session {session_id} server '{name}' did not connect: {error:#}"
                    ));
                    return;
                }
                let registry = registry.await;
                let count = {
                    let _publish = publish.lock().await;
                    let tools = crate::mcp::create_mcp_tools(Arc::clone(&manager)).await;
                    let count = tools
                        .iter()
                        .filter(|(_, tool)| tool.mcp_identity().is_some_and(|(server, _)| server == name))
                        .count();
                    registry.publish_session_mcp(token, &session_id, &connection_id, servers, tools);
                    count
                };
                crate::logging::info(&format!(
                    "ACP MCP: session {session_id} server '{name}' connected with {count} tool(s)"
                ));
                let _ = events.send(ServerEvent::McpStatus {
                    servers: vec![format!("{name}:{count}")],
                });
            });
        }
        Self {
            token,
            manager,
            registry,
            connects,
        }
    }

    /// Cancel pending connects, withdraw this installation's proxies and stop
    /// every server process it started, including one whose tool call is
    /// still in flight.
    pub(crate) async fn shutdown(mut self) {
        // Await the aborted tasks so none of them can publish tools after
        // they were withdrawn below.
        self.connects.abort_all();
        while self.connects.join_next().await.is_some() {}
        if let Some(registry) = self.registry.peek() {
            registry.withdraw_session_mcp(self.token);
        }
        // An in-flight call keeps its proxy, and with it this manager, alive:
        // only an explicit disconnect stops that server now.
        self.manager.read().await.disconnect_all().await;
    }
}

// ---------------------------------------------------------------------------
// Package MCP servers (issue #648)
// ---------------------------------------------------------------------------

/// One tool a package MCP server advertised, as a registry proxy.
struct PackageTool {
    /// Registry alias the model calls.
    alias: String,
    /// Package server that owns it, `plugin_<id>_<name>`.
    server: String,
    /// Name the server itself uses.
    raw_name: String,
    description: String,
    schema: Value,
    handle: McpHandle,
}

#[async_trait]
impl Tool for PackageTool {
    fn name(&self) -> &str {
        &self.alias
    }

    fn mcp_identity(&self) -> Option<(&str, &str)> {
        Some((&self.server, &self.raw_name))
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.schema.clone()
    }

    /// Same contract as a configured MCP tool: `intent` and the large-output
    /// escape hatch are the host's, so they are stripped unless the server
    /// declares them, and the rest is passed through unchanged.
    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let mut input = if input.is_null() {
            Value::Object(serde_json::Map::new())
        } else {
            input
        };
        let server_declares_intent = self
            .schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| properties.contains_key("intent"));
        if !server_declares_intent && let Some(object) = input.as_object_mut() {
            object.remove("intent");
        }
        let accept_large_output = take_accept_large_output(&mut input);
        let result = self.handle.call_tool(&self.raw_name, input).await?;

        let mut output_parts = Vec::new();
        for block in result.content {
            match block {
                ContentBlock::Text { text } => output_parts.push(text),
                ContentBlock::Image { data, mime_type } => {
                    output_parts.push(format!("[Image: {} ({} bytes)]", mime_type, data.len()));
                }
                ContentBlock::Resource { resource } => output_parts.push(render_resource_content(resource)),
            }
        }

        let output = output_parts.join("\n");
        let output = if accept_large_output {
            output
        } else {
            crate::mcp::budget::fit_to_budget(&output, crate::mcp::budget::MCP_RESULT_TOKEN_BUDGET)
        };
        let title = format!("mcp:{}:{}", self.server, self.raw_name);

        if result.is_error {
            Ok(ToolOutput::new(format!("Error: {}", output)).with_title(title))
        } else {
            Ok(ToolOutput::new(output).with_title(title))
        }
    }
}

/// One connected package server of a session, kept so its child can be closed
/// exactly.
struct PackageLive {
    public_name: String,
    client: McpClient,
}

/// The MCP servers one package contributes to one session at one generation.
struct PackageConnection {
    package_id: String,
    /// `record.generation` the definitions were read at.
    generation: u64,
    /// Every name this package planned to register, known before the first
    /// handshake so a stop can withdraw all of them.
    servers: Vec<String>,
    /// Clients that finished their handshake, for an exact shutdown.
    live: Arc<StdMutex<Vec<PackageLive>>>,
    /// Set before teardown, so a handshake that finishes late publishes nothing.
    cancelled: Arc<AtomicBool>,
    /// Task running handshakes; `None` when every server was refused before start.
    task: Option<tokio::task::JoinHandle<()>>,
    /// Unregisters the revocation callback when the connection goes away.
    _holder: Option<PackageLifecycleHolder>,
}

/// One package's servers for one generation, resolved against the user's
/// sources.
struct PackagePlan {
    package_id: String,
    /// `record.generation`, which moves when the copy is replaced.
    generation: u64,
    servers: Vec<crate::mcp::PackageServerRegistration>,
}

/// One session's package MCP state.
struct PackageMcpSession {
    registry: WeakRegistry,
    /// Serializes generation refreshes and teardown so stale unregistrations
    /// never race a newly spawned replacement connection.
    refresh_lock: Mutex<()>,
    /// Catalog generation the entries were planned from, `u64::MAX` before the
    /// first refresh.
    generation: AtomicU64,
    packages: Mutex<BTreeMap<String, PackageConnection>>,
}

/// What one package's connection task needs, kept in one value so the task
/// takes a single argument.
struct PackageRun {
    registry: Registry,
    store: PackageStore,
    /// The session whose next turn reports a refusal.
    session_id: String,
    package_id: String,
    /// Catalog generation this connection belongs to.
    catalog_generation: u64,
    servers: Vec<crate::mcp::PackageServerRegistration>,
    live: Arc<StdMutex<Vec<PackageLive>>>,
    cancelled: Arc<AtomicBool>,
}

static PACKAGE_MCP_SESSIONS: LazyLock<StdMutex<BTreeMap<String, Arc<PackageMcpSession>>>> =
    LazyLock::new(|| StdMutex::new(BTreeMap::new()));

fn lock_package_sessions() -> std::sync::MutexGuard<'static, BTreeMap<String, Arc<PackageMcpSession>>> {
    PACKAGE_MCP_SESSIONS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Bring one session's package MCP servers to the generation its turn holds.
///
/// The turn boundary calls this after the package snapshot was captured. An
/// unchanged catalog generation returns at once, so an idle turn reads no file
/// and starts no process. A package that was replaced or removed loses its own
/// connections and tools, while every other package's connections, and the
/// user's own servers, keep running: a connection is only ever closed by the
/// package that owns it.
pub(crate) async fn refresh_package_mcp(session_id: &str, registry: Registry, session: &Arc<PackageSession>) {
    let Some(snapshot) = session.current() else {
        return;
    };
    let has_installed_plugin = !snapshot.is_empty()
        && snapshot
            .packages()
            .any(|record| record.state == PackageState::Installed && matches!(record.kind, PackageKind::Plugin));
    let store = session.store();
    if !has_installed_plugin || !store.root().exists() {
        let Some(state) = existing_package_state(session_id) else {
            return;
        };
        let _refresh = state.refresh_lock.lock().await;
        if state.generation.load(Ordering::SeqCst) == snapshot.generation() {
            return;
        }
        let stale: Vec<PackageConnection> = {
            let mut packages = state.packages.lock().await;
            let stale = std::mem::take(&mut *packages).into_values().collect();
            state.generation.store(snapshot.generation(), Ordering::SeqCst);
            stale
        };
        for connection in stale {
            stop_connection(connection, Some(registry.clone())).await;
        }
        return;
    }
    let state = package_state(session_id, &registry);
    let _refresh = state.refresh_lock.lock().await;
    if state.generation.load(Ordering::SeqCst) == snapshot.generation() {
        return;
    }
    let store = store.clone();
    let workspace = snapshot.workspace().map(PathBuf::from);
    let records: Vec<PackageRecord> = snapshot.packages().cloned().collect();
    let plan_store = store.clone();
    let planned = tokio::task::spawn_blocking(move || plan_packages(&plan_store, &records, workspace.as_deref())).await;
    let planned = match planned {
        Ok(planned) => planned,
        Err(error) => {
            crate::logging::warn(&format!(
                "MCP: the package server plan for session {session_id} did not finish: {error}"
            ));
            return;
        }
    };

    let stale = {
        let mut packages = state.packages.lock().await;
        let desired: BTreeMap<String, u64> = planned
            .iter()
            .map(|plan| (plan.package_id.clone(), plan.generation))
            .collect();
        let expired: Vec<String> = packages
            .iter()
            .filter(|(id, connection)| desired.get(*id) != Some(&connection.generation))
            .map(|(id, _)| id.clone())
            .collect();
        let mut stale = Vec::with_capacity(expired.len());
        for id in expired {
            if let Some(connection) = packages.remove(&id) {
                stale.push(connection);
            }
        }
        stale
    };
    // Tear down expired or replaced generations before starting replacements so a
    // fast replacement cannot have its registered tools removed by the old
    // connection's `unregister_mcp_server`.
    for connection in stale {
        stop_connection(connection, Some(registry.clone())).await;
    }

    let mut packages = state.packages.lock().await;
    for mut plan in planned {
        if packages
            .get(&plan.package_id)
            .is_some_and(|connection| connection.generation == plan.generation)
        {
            // Another turn published this generation while this one planned.
            continue;
        }
        // Check process hosting policy synchronously before returning so Turn 1
        // observes any `<hosted-policy>` notice via `report_hosted_refusals()`.
        plan.servers.retain(|registration| {
            let public_name = &registration.server.public_name;
            let operation = match &registration.server.kind {
                PackageMcpKind::Stdio { .. } => HostedOperation::Stdio,
                PackageMcpKind::Remote { .. } => HostedOperation::Remote,
            };
            if let Err(refusal) = crate::mcp::hosted::check_current(&HostedRequest::server(operation, public_name)) {
                crate::logging::warn(&format!(
                    "MCP: package {} server '{public_name}' was refused: {refusal}",
                    plan.package_id
                ));
                report_hosted_refusal(session_id, public_name, &refusal);
                return false;
            }
            true
        });
        let catalog_generation = snapshot.generation();
        if plan.servers.is_empty() {
            let id = plan.package_id;
            packages.insert(
                id.clone(),
                PackageConnection {
                    package_id: id,
                    generation: plan.generation,
                    servers: Vec::new(),
                    live: Arc::new(StdMutex::new(Vec::new())),
                    cancelled: Arc::new(AtomicBool::new(false)),
                    task: None,
                    _holder: None,
                },
            );
            continue;
        }
        let Some(holder) = package_revocation_holder(session_id, &plan.package_id, catalog_generation) else {
            continue;
        };
        let connection = start_connection(&registry, store.clone(), session_id, catalog_generation, holder, plan);
        packages.insert(connection.package_id.clone(), connection);
    }
    state.generation.store(snapshot.generation(), Ordering::SeqCst);
}

/// Resolve every package's declared servers, off the async thread: each
/// definition file is read from the installed copy and checked against the
/// digest the catalog recorded, and remote references are resolved against the
/// user's own sources.
fn plan_packages(
    store: &PackageStore,
    records: &[PackageRecord],
    workspace: Option<&std::path::Path>,
) -> Vec<PackagePlan> {
    let mut merged: Option<McpConfig> = None;
    let mut planned = Vec::new();
    for record in records {
        if record.state != PackageState::Installed || !matches!(record.kind, PackageKind::Plugin) {
            continue;
        }
        let servers = match store.mcp_servers(record) {
            Ok(servers) => servers,
            Err(error) => {
                crate::logging::warn(&format!(
                    "MCP: package {} declares no usable MCP server: {error:#}",
                    record.id
                ));
                continue;
            }
        };
        if servers.is_empty() {
            continue;
        }
        let merged = merged.get_or_insert_with(|| McpConfig::load_for_dir(workspace));
        let servers = package_servers(&merged.servers, &servers);
        if servers.is_empty() {
            continue;
        }
        planned.push(PackagePlan {
            package_id: record.id.clone(),
            generation: record.generation,
            servers,
        });
    }
    planned
}

/// Stop one session's servers for one package, and nothing else.
///
/// The lifecycle registry calls this while the daemon publishes a revocation,
/// so it hands the work to the runtime and returns.
pub(crate) fn revoke_package(session_id: &str, package_id: &str) {
    let Some(state) = existing_package_state(session_id) else {
        return;
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let package_id = package_id.to_owned();
    handle.spawn(async move {
        let _refresh = state.refresh_lock.lock().await;
        let connection = state.packages.lock().await.remove(&package_id);
        if let Some(connection) = connection {
            let registry = state.registry.upgrade();
            stop_connection(connection, registry).await;
        }
    });
}

/// Drop one session's package MCP state, called when its agent leaves memory.
pub(crate) fn forget_package_mcp(session_id: &str) {
    let Some(state) = lock_package_sessions().remove(session_id) else {
        return;
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        let _refresh = state.refresh_lock.lock().await;
        let connections: Vec<PackageConnection> = std::mem::take(&mut *state.packages.lock().await)
            .into_values()
            .collect();
        let registry = state.registry.upgrade();
        for connection in connections {
            stop_connection(connection, registry.clone()).await;
        }
    });
}

fn existing_package_state(session_id: &str) -> Option<Arc<PackageMcpSession>> {
    lock_package_sessions().get(session_id).map(Arc::clone)
}

fn package_state(session_id: &str, registry: &Registry) -> Arc<PackageMcpSession> {
    let mut sessions = lock_package_sessions();
    if let Some(existing) = sessions.get(session_id) {
        return Arc::clone(existing);
    }
    let state = Arc::new(PackageMcpSession {
        registry: registry.downgrade(),
        refresh_lock: Mutex::new(()),
        generation: AtomicU64::new(u64::MAX),
        packages: Mutex::new(BTreeMap::new()),
    });
    sessions.insert(session_id.to_owned(), Arc::clone(&state));
    state
}

/// A lease owner suffix that is unique inside this process.
fn next_package_token() -> u64 {
    static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
    NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
}

/// Stop this session's servers for `package_id` when the daemon revokes it.
///
/// `None` when the package was already revoked after the generation this turn
/// captured: nothing may start for it, and the next generation brings its
/// servers back if it is installed again.
fn package_revocation_holder(session_id: &str, package_id: &str, generation: u64) -> Option<PackageLifecycleHolder> {
    let session = session_id.to_owned();
    let id = package_id.to_owned();
    let callback: RevocationCallback = Arc::new(move |event: &crate::extensions::lifecycle::RevocationEvent| {
        if event.generation > generation {
            revoke_package(&session, &id);
        }
    });
    match PackageLifecycleRegistry::shared().register(package_id, generation, callback) {
        Ok(holder) => Some(holder),
        Err(error) => {
            crate::logging::warn(&format!(
                "MCP: package {package_id} serves no MCP server in session {session_id}: {error:#}"
            ));
            None
        }
    }
}

/// Start one generation's connections for one package.
fn start_connection(
    registry: &Registry,
    store: PackageStore,
    session_id: &str,
    catalog_generation: u64,
    holder: PackageLifecycleHolder,
    plan: PackagePlan,
) -> PackageConnection {
    let PackagePlan {
        package_id,
        generation,
        servers,
    } = plan;
    let names: Vec<String> = servers
        .iter()
        .map(|registration| registration.server.public_name.clone())
        .collect();
    let live: Arc<StdMutex<Vec<PackageLive>>> = Arc::new(StdMutex::new(Vec::new()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let task = tokio::spawn(run_package(PackageRun {
        registry: registry.clone(),
        store,
        session_id: session_id.to_string(),
        package_id: package_id.clone(),
        catalog_generation,
        servers,
        live: Arc::clone(&live),
        cancelled: Arc::clone(&cancelled),
    }));
    PackageConnection {
        package_id,
        generation,
        servers: names,
        live,
        cancelled,
        task: Some(task),
        _holder: Some(holder),
    }
}

/// Start each declared server, publish its tools as soon as its handshake
/// finishes, and keep the clients for an exact stop.
async fn run_package(run: PackageRun) {
    let PackageRun {
        registry,
        store,
        session_id,
        package_id,
        catalog_generation,
        servers,
        live,
        cancelled,
    } = run;
    for registration in servers {
        if cancelled.load(Ordering::SeqCst) {
            return;
        }
        let public_name = registration.server.public_name.clone();
        // The server's own name, not the package id: a name the allow list can
        // narrow, and a name the user can act on when it is refused.
        let operation = match &registration.server.kind {
            PackageMcpKind::Stdio { .. } => HostedOperation::Stdio,
            PackageMcpKind::Remote { .. } => HostedOperation::Remote,
        };
        if let Err(refusal) = crate::mcp::hosted::check_current(&HostedRequest::server(operation, &public_name)) {
            crate::logging::warn(&format!(
                "MCP: package {package_id} server '{public_name}' was refused: {refusal}"
            ));
            report_hosted_refusal(&session_id, &public_name, &refusal);
            continue;
        }
        let connected = match &registration.server.kind {
            PackageMcpKind::Stdio { mode, args, .. } => {
                match start_package_child(&store, &package_id, catalog_generation, mode, args).await {
                    Ok(process) => McpClient::connect_package(public_name.clone(), &registration.config, process).await,
                    Err(error) => Err(error),
                }
            }
            PackageMcpKind::Remote { .. } => McpClient::connect(public_name.clone(), &registration.config).await,
        };
        let client = match connected {
            Ok(client) => client,
            Err(error) => {
                crate::logging::warn(&format!(
                    "MCP: package {package_id} server '{public_name}' did not connect: {error:#}"
                ));
                // A refusal the process gate raised rather than this name check.
                if let Some(refusal) = crate::mcp::hosted::find(&error) {
                    report_hosted_refusal(&session_id, &public_name, refusal);
                }
                continue;
            }
        };
        if cancelled.load(Ordering::SeqCst) {
            let mut client = client;
            client.shutdown().await;
            return;
        }
        let tools = package_tools(&public_name, &client.handle());
        let count = tools.len();
        registry
            .refresh_mcp_tools(tools, std::slice::from_ref(&public_name))
            .await;
        crate::logging::info(&format!(
            "MCP: package {package_id} server '{public_name}' connected with {count} tool(s)"
        ));
        live.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(PackageLive { public_name, client });
    }
}

/// Record a hosted refusal where the session's next turn will see it.
///
/// The gate already wrote the audit record; this carries the same refusal into
/// the session, so a refused package MCP server is reported instead of quietly
/// missing from the tool list.
pub(crate) fn report_hosted_refusal(session_id: &str, server: &str, refusal: &crate::mcp::hosted::HostedRefusal) {
    crate::agent::hosted_notice::push(session_id, format!("MCP server '{server}' was not started: {refusal}"));
}

/// Start one package MCP child through the shared package supervision.
///
/// The lease and the executable check happen off the async thread; the child
/// then runs inside the package's own process scope, with the package
/// activation environment and its installed copy as the working directory.
async fn start_package_child(
    store: &PackageStore,
    package_id: &str,
    catalog_generation: u64,
    mode: &str,
    args: &[String],
) -> Result<PackageProcess> {
    if let Some(event) = PackageLifecycleRegistry::shared().revocation(package_id) {
        ensure!(
            event.generation <= catalog_generation,
            "package {package_id} was {} at catalog generation {}; an MCP server from generation {catalog_generation} is stale",
            event.cause.as_str(),
            event.generation
        );
    }
    let mut arguments = Vec::with_capacity(args.len() + 1);
    arguments.push(OsString::from(mode));
    arguments.extend(args.iter().map(OsString::from));
    let store = store.clone();
    let id = package_id.to_owned();
    tokio::task::spawn_blocking(move || -> Result<PackageProcess> {
        let lease = store.lease(&lease_owner(&format!("mcp-{id}"), next_package_token()))?;
        let executable = store.prepare_executable(lease, &id)?;
        executable.start_with_arguments(&arguments)
    })
    .await
    .context("the package MCP start task did not finish")?
}

/// Publish one live server's tools as registry proxies.
fn package_tools(server: &str, handle: &McpHandle) -> Vec<(String, Arc<dyn Tool>)> {
    let defined: Vec<(String, McpToolDef)> = handle
        .tools()
        .into_iter()
        .map(|definition| (server.to_string(), definition))
        .collect();
    let aliases = dispatch_names(&defined);
    defined
        .into_iter()
        .zip(aliases)
        .map(|((server, definition), alias)| {
            let tool = PackageTool {
                alias: alias.clone(),
                server,
                raw_name: definition.name,
                description: definition.description.unwrap_or_else(|| "MCP tool".to_string()),
                schema: definition.input_schema,
                handle: handle.clone(),
            };
            (alias, Arc::new(tool) as Arc<dyn Tool>)
        })
        .collect()
}

/// Stop one package's connections: cancel pending handshakes, withdraw its
/// tools, and shut down exactly the clients this connection started.
async fn stop_connection(connection: PackageConnection, registry: Option<Registry>) {
    connection.cancelled.store(true, Ordering::SeqCst);
    if let Some(task) = connection.task {
        task.abort();
        // The abort above makes a cancelled join the expected answer; anything
        // else means the connection task panicked and is worth a line in the log.
        if let Err(error) = task.await
            && !error.is_cancelled()
        {
            crate::logging::warn(&format!("MCP package connection task failed: {error}"));
        }
    }
    if let Some(registry) = registry {
        for name in &connection.servers {
            registry.unregister_mcp_server(name).await;
        }
    }
    let clients = std::mem::take(&mut *connection.live.lock().unwrap_or_else(PoisonError::into_inner));
    for mut live in clients {
        crate::logging::info(&format!("MCP: closing package server '{}'", live.public_name));
        live.client.shutdown().await;
    }
}

#[cfg(test)]
#[path = "session_mcp_tests.rs"]
pub(crate) mod tests;
