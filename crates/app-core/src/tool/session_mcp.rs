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

use super::{Registry, Tool, acp_session};
use crate::mcp::{McpConfig, McpManager, McpServerConfig, McpToolDef};
use crate::protocol::{ServerEvent, SessionMcpServer};
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinSet;

/// Turn the editor's server list into manager configs, refusing the whole
/// list on the first invalid entry so a session never runs half of it.
pub(crate) fn server_configs(servers: Vec<SessionMcpServer>) -> Result<Vec<(String, McpServerConfig)>, String> {
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

#[cfg(test)]
#[path = "session_mcp_tests.rs"]
pub(crate) mod tests;
