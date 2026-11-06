//! ACP session lifecycle: `session/new`, `session/load`, `session/resume`,
//! `session/close` and `session/delete`, plus the session setup parameters
//! they share (`cwd`, `additionalDirectories`, `mcpServers`).

use super::v2::negotiate::{ACP_V2_PROTOCOL_VERSION, AcpVersion};
use super::v2::state::{AttachSnapshot, Control, Resumed};
use super::*;
use crate::protocol::{AcpClientCapabilities, AcpSubscribeOptions, HistoryMessage, SessionMcpServer};
use crate::session::RenderedImage;
use std::path::Path;

/// MCP transports besides stdio. Advertised in `initialize` and enforced on
/// `mcpServers`; stdio is always supported. The daemon connects both remote
/// transports per session (`tool::session_mcp`), proven against local
/// Streamable HTTP and legacy SSE stubs (`session_mcp_http_stub_connects`,
/// `session_mcp_sse_stub_connects`).
pub(super) struct McpTransports {
    pub(super) http: bool,
    pub(super) sse: bool,
}

pub(super) const MCP_REMOTE_TRANSPORTS: McpTransports = McpTransports { http: true, sse: true };

/// Bound for daemon replies to session configuration requests. These never
/// wait on a model turn, so a missing reply means the daemon does not serve
/// the request and the session continues without it. A reply that arrives is
/// `Ack`, `Done` or `Error` (`wait_for_accepted`), never only `Done`.
const DAEMON_CONFIG_REPLY_BOUND: Duration = Duration::from_secs(30);

/// The safety bound on one attach attempt's wait for the daemon to name the
/// session (its `History`) or refuse it.
///
/// It is not how a refusal is detected: the daemon answers a Subscribe it
/// cannot bind with an `Error` frame (client_lifecycle.rs, the target-aware
/// subscribe branch), and a closed connection fails the wait at once. The
/// bound only lets a daemon that hangs outright still be answered, so it sits
/// far above a slow restore: a session of 2001 stored messages restores in
/// ~9 s, and 300 s leaves room for sessions some thirty times that size.
#[derive(Clone, Copy, Debug)]
pub(super) struct AttachBounds {
    pub(super) reply: Duration,
}

impl Default for AttachBounds {
    fn default() -> Self {
        Self {
            reply: Duration::from_secs(300),
        }
    }
}

/// The longest `retry_after_secs` a busy refusal can make the one retry wait.
const ATTACH_RETRY_AFTER_CAP: Duration = Duration::from_secs(2);

/// How long a v2 attach waits after its `Done` for the daemon's foreground
/// snapshot, which the daemon queues right behind that `Done`
/// (client_lifecycle.rs, the subscribe branch). A daemon that sends none is
/// answered with the state this connection knows, idle.
const ATTACH_SNAPSHOT_BOUND: Duration = Duration::from_secs(10);

/// What an attach does with the target's History.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttachMode {
    /// v1: `session/load` replays it to the client, `session/resume` does not.
    V1 { replay: bool },
    /// v2: it is handed back with the foreground snapshot, and the session's
    /// stream takes over from the attach (`session/resume` replays it).
    V2,
}

/// An attached session, and on v2 what its `session/resume` reports.
struct Attached {
    session: DaemonSession,
    v2: Option<V2Attach>,
}

/// The History and foreground state a v2 attach read, for its resume.
#[derive(Default)]
struct V2Attach {
    messages: Vec<HistoryMessage>,
    images: Vec<RenderedImage>,
    snapshot: Option<AttachSnapshot>,
    leftovers: Vec<ServerEvent>,
}

/// Why one attach attempt ended, and whether a fresh connection may pass.
enum AttachFailure {
    /// The daemon refused the attach because another owner still held the
    /// session's store writer, before anything reached the client. `delay` is
    /// the daemon's `retry_after_secs`, if it gave one.
    Busy { error: RpcError, delay: Option<Duration> },
    /// Every other end: a refusal a retry would meet again, a closed or hung
    /// daemon, a failure after part of the history was replayed (a retry would
    /// duplicate it), or a client write failure.
    Fatal(RpcError),
}

impl AttachFailure {
    fn into_error(self) -> RpcError {
        match self {
            Self::Busy { error, .. } | Self::Fatal(error) => error,
        }
    }
}

/// Whether an attach refusal is the store's busy writer, the one refusal worth
/// another connection.
///
/// `StoreError::busy` is always retryable and its `Display` reads
/// `store error [internal:busy] retryable ...`
/// (crates/session-store/src/error.rs:207-215, :284); for the attach that is
/// `session writer is already owned` while a previous owner (a deleted or
/// closed attachment) is still letting the writer go. A live-attach conflict
/// (`... could not be shared safely`, `retry_after_secs: 1`) is another client
/// holding the session and is not retried.
fn is_retryable_refusal(message: &str) -> bool {
    message.contains("[internal:busy] retryable")
}

#[derive(Debug, PartialEq)]
pub(super) struct SessionSetup {
    pub(super) cwd: PathBuf,
    pub(super) additional_dirs: Vec<String>,
    pub(super) mcp_servers: Vec<SessionMcpServer>,
}

pub(super) fn session_setup_from_params(params: &Value) -> std::result::Result<SessionSetup, String> {
    Ok(SessionSetup {
        cwd: cwd_from_params(params)?,
        additional_dirs: additional_dirs_from_params(params)?,
        mcp_servers: mcp_servers_from_params(params)?,
    })
}

pub(super) fn cwd_from_params(params: &Value) -> std::result::Result<PathBuf, String> {
    match params.get("cwd") {
        Some(Value::String(cwd)) if Path::new(cwd).is_absolute() => Ok(PathBuf::from(cwd)),
        Some(Value::String(cwd)) => Err(format!("cwd must be an absolute path: {cwd:?}")),
        None | Some(Value::Null) => Err("Missing required cwd".to_string()),
        Some(_) => Err("cwd must be a string".to_string()),
    }
}

fn additional_dirs_from_params(params: &Value) -> std::result::Result<Vec<String>, String> {
    let directories = match params.get("additionalDirectories") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(directories)) => directories,
        Some(_) => return Err("additionalDirectories must be an array".to_string()),
    };
    directories
        .iter()
        .map(|directory| match directory.as_str() {
            Some(path) if Path::new(path).is_absolute() => Ok(path.to_string()),
            Some(path) => Err(format!(
                "additionalDirectories entries must be absolute paths: {path:?}"
            )),
            None => Err("additionalDirectories entries must be strings".to_string()),
        })
        .collect()
}

pub(super) fn mcp_servers_from_params(params: &Value) -> std::result::Result<Vec<SessionMcpServer>, String> {
    match params.get("mcpServers") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(servers)) => servers.iter().map(mcp_server_from_value).collect(),
        Some(_) => Err("mcpServers must be an array".to_string()),
    }
}

fn mcp_server_from_value(server: &Value) -> std::result::Result<SessionMcpServer, String> {
    if !server.is_object() {
        return Err("mcpServers entries must be objects".to_string());
    }
    let name = required_string(server, "name", "mcpServers entry")?;
    let transport = match server.get("type") {
        None | Some(Value::Null) => "stdio",
        Some(Value::String(kind)) => kind.as_str(),
        Some(_) => return Err(format!("MCP server {name:?}: type must be a string")),
    };
    match transport {
        "stdio" => Ok(SessionMcpServer {
            command: Some(required_string(server, "command", &format!("MCP server {name:?}"))?),
            args: string_array(server, "args", &name)?,
            env: name_value_pairs(server, "env", &name)?,
            name,
            transport: "stdio".to_string(),
            url: None,
            headers: Vec::new(),
        }),
        "http" | "sse" => {
            let advertised = if transport == "http" {
                MCP_REMOTE_TRANSPORTS.http
            } else {
                MCP_REMOTE_TRANSPORTS.sse
            };
            if !advertised {
                return Err(format!(
                    "MCP server {name:?}: the {transport} transport is not supported by this agent"
                ));
            }
            Ok(SessionMcpServer {
                url: Some(required_string(server, "url", &format!("MCP server {name:?}"))?),
                headers: name_value_pairs(server, "headers", &name)?,
                name,
                transport: transport.to_string(),
                command: None,
                args: Vec::new(),
                env: Vec::new(),
            })
        }
        other => Err(format!("MCP server {name:?}: unknown transport type {other:?}")),
    }
}

fn required_string(object: &Value, field: &str, owner: &str) -> std::result::Result<String, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{owner}: {field} must be a string"))
}

fn string_array(server: &Value, field: &str, name: &str) -> std::result::Result<Vec<String>, String> {
    match server.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("MCP server {name:?}: {field} entries must be strings"))
            })
            .collect(),
        Some(_) => Err(format!("MCP server {name:?}: {field} must be an array")),
    }
}

fn name_value_pairs(server: &Value, field: &str, name: &str) -> std::result::Result<Vec<(String, String)>, String> {
    match server.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|pair| {
                let key = pair.get("name").and_then(Value::as_str);
                let value = pair.get("value").and_then(Value::as_str);
                match (key, value) {
                    (Some(key), Some(value)) => Ok((key.to_string(), value.to_string())),
                    _ => Err(format!(
                        "MCP server {name:?}: {field} entries must be {{name, value}} strings"
                    )),
                }
            })
            .collect(),
        Some(_) => Err(format!("MCP server {name:?}: {field} must be an array")),
    }
}

/// The ACP options a session Subscribe carries, from the capabilities the
/// client advertised in `initialize`.
pub(super) fn acp_subscribe_options(client_capabilities: &Value, additional_dirs: Vec<String>) -> AcpSubscribeOptions {
    let flag = |pointer: &str| client_capabilities.pointer(pointer).and_then(Value::as_bool) == Some(true);
    AcpSubscribeOptions {
        client_capabilities: AcpClientCapabilities {
            fs_read_text_file: flag("/fs/readTextFile"),
            fs_write_text_file: flag("/fs/writeTextFile"),
            terminal: flag("/terminal"),
        },
        permission_requests: true,
        additional_dirs,
        protocol_version: None,
    }
}

/// `elicitation.form` must be an object; `{}` at `elicitation` is not form
/// support.
pub(super) fn supports_elicitation_form(client_capabilities: &Value) -> bool {
    client_capabilities
        .pointer("/elicitation/form")
        .is_some_and(Value::is_object)
}

/// A daemon session can run turns when it reports a model.
pub(super) fn model_is_usable(provider_model: Option<&str>, available_models: &[String]) -> bool {
    provider_model.is_some_and(|model| !model.trim().is_empty()) || !available_models.is_empty()
}

fn internal(context: &str, err: &anyhow::Error) -> RpcError {
    RpcError::new(JSONRPC_INTERNAL_ERROR, format!("{context}: {err:#}"))
}

/// IS-1: a v2 connection needs the History ack the daemon echoes to a
/// connection that subscribed with `protocol_version: 2`. Without it the
/// daemon predates this adapter's v2 support: it reuses the running process,
/// ignores the Subscribe's version, and never sends `UserMessageInserted`, so
/// every prompt of this connection would be answered cancelled after it ran.
/// The daemon is not killed here — other clients may be attached to it.
fn require_v2_daemon(acp_protocol_version: Option<u32>) -> std::result::Result<(), RpcError> {
    if acp_protocol_version == Some(ACP_V2_PROTOCOL_VERSION as u32) {
        return Ok(());
    }
    Err(RpcError::new(
        JSONRPC_INTERNAL_ERROR,
        "The running HoronAgent daemon is older than this adapter and does not support ACP v2. \
         Restart the HoronAgent daemon and retry.",
    ))
}

/// A Subscribe for this ACP connection. `acp` is `None` only for the
/// short-lived connections that inspect or archive a session.
pub(super) fn subscribe_request(
    id: u64,
    cwd: Option<&Path>,
    target_session_id: Option<String>,
    acp: Option<AcpSubscribeOptions>,
    supports_ask_requests: bool,
) -> Request {
    Request::Subscribe {
        system_prompt: None,
        supports_pdf_panels: false,
        supports_user_response: false,
        supports_ask_requests,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        id,
        working_dir: cwd.map(|cwd| cwd.display().to_string()),
        target_session_id,
        client_instance_id: Some("acp".to_string()),
        client_has_local_history: false,
        allow_session_takeover: false,
        terminal_env: crate::terminal_launch::snapshot_client_terminal_env(),
        acp,
    }
}

impl AcpRuntime {
    async fn ensure_daemon(&self) -> Result<()> {
        if dispatch::server_is_running().await {
            return Ok(());
        }
        dispatch::spawn_server(
            &self.provider_choice,
            self.model.as_deref(),
            self.provider_profile.as_deref(),
        )
        .await
    }

    pub(super) async fn connect_daemon(&self) -> Result<(ReadHalf, WriteHalf)> {
        // A unit test scripts the daemon end instead of dialing the socket.
        if let Some(connector) = self.daemon_connector.clone() {
            return connector().await;
        }
        self.ensure_daemon().await?;
        let stream = crate::server::connect_socket(&crate::server::socket_path()).await?;
        Ok(stream.into_split())
    }

    fn subscribe_for_session(&self, id: u64, setup: &SessionSetup, target_session_id: Option<String>) -> Request {
        let capabilities = self.client_capabilities();
        let acp = acp_subscribe_options(&capabilities, setup.additional_dirs.clone());
        subscribe_request(
            id,
            Some(&setup.cwd),
            target_session_id,
            Some(self.for_surface(acp)),
            supports_elicitation_form(&capabilities),
        )
    }

    /// IS-3: a v2 connection never asks the client for file or terminal access
    /// (v2 removed it), so the daemon sees no such capability and is told which
    /// protocol version to address this connection with. A v1 Subscribe is
    /// passed through untouched.
    fn for_surface(&self, options: AcpSubscribeOptions) -> AcpSubscribeOptions {
        if self.spoken_version() != Some(AcpVersion::V2) {
            return options;
        }
        AcpSubscribeOptions {
            client_capabilities: AcpClientCapabilities {
                fs_read_text_file: false,
                fs_write_text_file: false,
                terminal: false,
            },
            permission_requests: options.permission_requests,
            additional_dirs: options.additional_dirs,
            protocol_version: Some(ACP_V2_PROTOCOL_VERSION as u32),
        }
    }

    pub(super) async fn handle_session_new(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let setup = match session_setup_from_params(&message.params) {
            Ok(setup) => setup,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        match self.create_new_session(setup).await {
            Ok(session) => {
                let session_id = session.session_id.clone();
                let state = session.ui_state.lock().await.clone();
                let mode = *session.mode.lock().await;
                let session = Arc::new(session);
                self.sessions
                    .lock()
                    .await
                    .insert(session_id.clone(), Arc::clone(&session));
                self.start_v2_stream(&session);
                let mut result = json!({ "sessionId": session_id });
                insert_session_configuration(&mut result, &state, mode);
                self.write_result(id, result).await?;
                self.write_available_commands(&session_id).await
            }
            Err(error) => self.write_rpc_error(id, error).await,
        }
    }

    /// IS-12 and IS-13: `session/new` on v2. `mcpServers` is optional and each
    /// entry needs `type` `stdio` or `http`; the response carries the session's
    /// options and commands, and no `modes` and no `available_commands_update`
    /// follow it.
    pub(super) async fn handle_session_new_v2(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let setup = match session_setup_v2(&message.params) {
            Ok(setup) => setup,
            Err(error) => return self.write_rpc_error(id, error).await,
        };
        match self.create_new_session(setup).await {
            Ok(session) => {
                let session_id = session.session_id.clone();
                let session = Arc::new(session);
                self.sessions
                    .lock()
                    .await
                    .insert(session_id.clone(), Arc::clone(&session));
                self.start_v2_stream(&session);
                let mut result = json!({ "sessionId": session_id });
                self.insert_setup_v2(&mut result, &session).await;
                self.write_result(id, result).await
            }
            Err(error) => self.write_rpc_error(id, error).await,
        }
    }

    /// IS-5, IS-6 and IS-13: `session/resume` on v2. A bad `replayFrom` is
    /// refused before anything is attached. Then, in order: the replay when
    /// `replayFrom` is `{type: "start"}`, the session info, one current
    /// `state_update` from the daemon's attach snapshot, and the response.
    pub(super) async fn handle_session_resume_v2(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        let setup = match session_setup_v2(&message.params) {
            Ok(setup) => setup,
            Err(error) => return self.write_rpc_error(id, error).await,
        };
        let replay = match v2::replay::replay_from(&message.params) {
            Ok(replay) => replay,
            Err(error) => return self.write_rpc_error(id, error).await,
        };
        let cwd = setup.cwd.clone();
        let active = self.sessions.lock().await.get(&session_id).cloned();
        let (session, attached) = match active {
            Some(session) => match self.refresh_active_session(&session, setup, false).await {
                Ok((messages, images)) => (
                    session,
                    V2Attach {
                        messages,
                        images,
                        ..V2Attach::default()
                    },
                ),
                Err(error) => return self.write_rpc_error(id, error).await,
            },
            None => match self
                .attach_existing_session(session_id.clone(), setup, AttachMode::V2)
                .await
            {
                Ok(Attached { session, v2 }) => {
                    let session = Arc::new(session);
                    self.sessions
                        .lock()
                        .await
                        .insert(session_id.clone(), Arc::clone(&session));
                    (session, v2.unwrap_or_default())
                }
                Err(error) => return self.write_rpc_error(id, error).await,
            },
        };
        let mut updates = if replay {
            v2::replay::replay_updates(&attached.messages, &attached.images, &cwd)
        } else {
            Vec::new()
        };
        updates.extend(self.session_info_update(&session_id).await);
        let resumed = Resumed {
            updates,
            snapshot: attached.snapshot,
            position: v2::replay::resume_position(&attached.messages),
            leftovers: attached.leftovers,
        };
        self.synchronize_resumed(&session, resumed).await;
        let mut result = json!({});
        self.insert_setup_v2(&mut result, &session).await;
        self.write_result(id, result).await
    }

    /// Hand the resume's updates to the session's stream, which writes them in
    /// order with its live updates, and wait until they are written. A stream
    /// started here reads them before any daemon event.
    async fn synchronize_resumed(&self, session: &Arc<DaemonSession>, resumed: Resumed) {
        let Some(v2) = session.v2.get() else {
            return;
        };
        let (done, written) = tokio::sync::oneshot::channel();
        if v2.send_control(Control::Resumed {
            resumed: Box::new(resumed),
            done,
        }) {
            self.start_v2_stream(session);
            let _ = written.await;
        }
    }

    /// IS-12: a v2 setup response's options and commands (omitted when empty).
    async fn insert_setup_v2(&self, result: &mut Value, session: &DaemonSession) {
        let state = session.ui_state.lock().await.clone();
        let mode = *session.mode.lock().await;
        insert_session_configuration_v2(result, &state, mode);
        insert_available_commands_v2(result, &acp_available_commands());
    }

    /// The current title and last activity of the session, from the session
    /// catalog; `None` when the catalog has no row for it (or its row is busy),
    /// since an update with no field would say nothing.
    async fn session_info_update(&self, session_id: &str) -> Option<Value> {
        let lookup_id = session_id.to_string();
        let info = match self.session_catalog.clone() {
            Some(catalog) => catalog(&lookup_id),
            None => tokio::task::spawn_blocking(move || catalog_info(&lookup_id))
                .await
                .map_err(anyhow::Error::from)
                .and_then(|info| info)
                .unwrap_or_else(|err| {
                    crate::logging::warn(&format!("ACP: no session info for {session_id}: {err:#}"));
                    None
                }),
        };
        let info = info?;
        let mut update = json!({ "sessionUpdate": "session_info_update", "updatedAt": info.updated_at });
        if let Some(title) = info.title {
            update["title"] = json!(title);
        }
        Some(update)
    }

    /// Subscribe a new, unprompted session and read its History.
    ///
    /// One exchange, no retry. The daemon answers for a session it already
    /// holds even when the home has no sessions store root yet (its persisted
    /// history fallback synthesizes the empty snapshot instead of failing), so
    /// a disconnected daemon here is a real failure worth reporting rather than
    /// hiding behind a reconnect.
    pub(super) async fn subscribe_new_session(&self, subscribe: &Request) -> Result<(DaemonSession, ServerEvent)> {
        let subscribe_id = subscribe.id();
        let (reader, writer) = self.connect_daemon().await?;
        let session = DaemonSession::new(String::new(), reader, writer, subscribe_id + 1);
        let mut subscribed = session.request(subscribe_id, subscribe).await?;
        wait_for_done(&mut subscribed, subscribe_id).await?;
        drop(subscribed);
        let history = request_history(&session).await?;
        Ok((session, history))
    }

    async fn create_new_session(&self, setup: SessionSetup) -> std::result::Result<DaemonSession, RpcError> {
        let subscribe = self.subscribe_for_session(1, &setup, None);
        let (mut session, created) = self
            .subscribe_new_session(&subscribe)
            .await
            .map_err(|err| internal("Failed to create HoronAgent session", &err))?;
        let ServerEvent::History {
            session_id,
            provider_name,
            provider_model,
            available_models,
            reasoning_effort,
            acp_protocol_version,
            ..
        } = created
        else {
            unreachable!("request_history only returns history")
        };
        // A daemon older than this adapter ignores the Subscribe's
        // `protocol_version` and never sends the v2-only events a prompt is
        // answered with, so every prompt would be rejected although it ran.
        if self.spoken_version() == Some(AcpVersion::V2)
            && let Err(error) = require_v2_daemon(acp_protocol_version)
        {
            return Err(error);
        }
        // Dropping `session` on the error path closes the daemon connection,
        // which discards the unprompted session.
        if !self.record_readiness(provider_model.as_deref(), &available_models) {
            return Err(RpcError::auth_required());
        }
        session.session_id = session_id;
        session.ui_state = Arc::new(Mutex::new(SessionUiState::from_history_fields(
            provider_name,
            provider_model,
            available_models,
            reasoning_effort,
        )));
        session.cwd = Arc::new(Mutex::new(Some(setup.cwd)));
        if !setup.mcp_servers.is_empty() {
            configure_session_mcp(&session, setup.mcp_servers).await;
        }
        set_session_approval_mode(&session, AcpMode::Default.approval_mode()).await;
        self.prepare_v2_session(&session);
        self.install_session_bridge(&session);
        Ok(session)
    }

    pub(super) fn record_readiness(&self, provider_model: Option<&str>, available_models: &[String]) -> bool {
        let ready = model_is_usable(provider_model, available_models);
        if ready {
            self.ready.store(true, Ordering::SeqCst);
        }
        ready
    }

    pub(super) async fn handle_session_load(&self, message: IncomingRequest, replay_history: bool) -> Result<()> {
        let id = message.id;
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        let setup = match session_setup_from_params(&message.params) {
            Ok(setup) => setup,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };

        let active = self.sessions.lock().await.get(&session_id).cloned();
        let session = match active {
            Some(session) => match self.refresh_active_session(&session, setup, replay_history).await {
                Ok(_) => session,
                Err(error) => return self.write_rpc_error(id, error).await,
            },
            None => match self
                .attach_existing_session(session_id.clone(), setup, AttachMode::V1 { replay: replay_history })
                .await
            {
                Ok(Attached { session, .. }) => {
                    let session = Arc::new(session);
                    self.sessions
                        .lock()
                        .await
                        .insert(session_id.clone(), Arc::clone(&session));
                    self.start_v2_stream(&session);
                    session
                }
                Err(error) => return self.write_rpc_error(id, error).await,
            },
        };
        let state = session.ui_state.lock().await.clone();
        let mode = *session.mode.lock().await;
        let mut result = json!({});
        insert_session_configuration(&mut result, &state, mode);
        self.write_result(id, result).await?;
        self.write_available_commands(&session_id).await
    }

    /// `session/load` or `session/resume` of a session already active on this
    /// connection reuses its daemon connection instead of attaching again.
    /// Returns the History it read, which a v2 resume replays itself.
    async fn refresh_active_session(
        &self,
        session: &DaemonSession,
        setup: SessionSetup,
        replay_history: bool,
    ) -> std::result::Result<(Vec<HistoryMessage>, Vec<RenderedImage>), RpcError> {
        let history = request_history(session)
            .await
            .map_err(|err| internal("Failed to read the session history", &err))?;
        let ServerEvent::History {
            messages,
            images,
            provider_name,
            provider_model,
            available_models,
            reasoning_effort,
            acp_protocol_version,
            ..
        } = history
        else {
            unreachable!("request_history only returns history")
        };
        if self.spoken_version() == Some(AcpVersion::V2)
            && let Err(error) = require_v2_daemon(acp_protocol_version)
        {
            return Err(error);
        }
        *session.ui_state.lock().await =
            SessionUiState::from_history_fields(provider_name, provider_model, available_models, reasoning_effort);
        if replay_history {
            self.replay_history(&session.session_id, &messages, &images, &setup.cwd)
                .await
                .map_err(|err| internal("Failed to replay the session history", &err))?;
        }
        *session.cwd.lock().await = Some(setup.cwd);
        configure_session_mcp(session, setup.mcp_servers).await;
        let mode = *session.mode.lock().await;
        set_session_approval_mode(session, mode.approval_mode()).await;
        Ok((messages, images))
    }

    /// `session/load` or `session/resume` of a session this connection does
    /// not hold: attach it on a fresh daemon connection, once more if the
    /// daemon refused the first try because the session's store writer was
    /// still owned elsewhere.
    async fn attach_existing_session(
        &self,
        target_session_id: String,
        setup: SessionSetup,
        mode: AttachMode,
    ) -> std::result::Result<Attached, RpcError> {
        match self.attach_once(&target_session_id, &setup, mode).await {
            Ok(session) => Ok(session),
            Err(AttachFailure::Fatal(error)) => Err(error),
            Err(AttachFailure::Busy { error, delay }) => {
                // Nothing reached the client yet, and the holder the daemon
                // named lets the writer go on its own: one more connection
                // either gets the session or answers the daemon's own reason.
                crate::logging::info(&format!(
                    "ACP: attaching {target_session_id} once more: {}",
                    error.message
                ));
                if let Some(delay) = delay {
                    tokio::time::sleep(delay.min(ATTACH_RETRY_AFTER_CAP)).await;
                }
                self.attach_once(&target_session_id, &setup, mode)
                    .await
                    .map_err(AttachFailure::into_error)
            }
        }
    }

    /// One attach attempt. It ends at the daemon's answer: the target's
    /// `History` and `Done`, a refusal `Error`, or the connection closing.
    /// [`AttachBounds::reply`] only bounds a daemon that hangs before naming
    /// the session; the replay that follows and the closing `Done` are this
    /// side's own writes and get no deadline.
    async fn attach_once(
        &self,
        target_session_id: &str,
        setup: &SessionSetup,
        mode: AttachMode,
    ) -> std::result::Result<Attached, AttachFailure> {
        let answer_bound = self.attach_bounds.reply;
        let (reader, writer) = self
            .connect_daemon()
            .await
            .map_err(|err| AttachFailure::Fatal(internal("Failed to reach the HoronAgent daemon", &err)))?;
        let mut session = DaemonSession::new(String::new(), reader, writer, 2);
        let resume_id = 1;
        let subscribe = self.subscribe_for_session(resume_id, setup, Some(target_session_id.to_string()));
        let mut events = session
            .foreground_request(Some(resume_id), &subscribe)
            .await
            .map_err(|err| AttachFailure::Fatal(internal("Failed to attach HoronAgent session", &err)))?;

        // The daemon answers an unknown target by silently creating a new
        // session; a History for any other id means the target does not exist.
        let deadline = tokio::time::Instant::now() + answer_bound;
        let mut found = false;
        let mut ui_state = SessionUiState::default();
        let mut v2 = V2Attach::default();
        loop {
            let next = events.next();
            let answered = if found {
                next.await
            } else {
                match tokio::time::timeout_at(deadline, next).await {
                    Ok(result) => result,
                    Err(_) => {
                        return Err(AttachFailure::Fatal(RpcError::new(
                            JSONRPC_INTERNAL_ERROR,
                            format!(
                                "Failed to attach HoronAgent session '{target_session_id}': the HoronAgent \
                                 daemon did not answer within {answer_bound:?}"
                            ),
                        )));
                    }
                }
            };
            match answered.map_err(|err| AttachFailure::Fatal(internal("Failed to attach HoronAgent session", &err)))? {
                ServerEvent::Ack { .. } => {}
                ServerEvent::History {
                    session_id,
                    messages,
                    images,
                    provider_name,
                    provider_model,
                    available_models,
                    reasoning_effort,
                    acp_protocol_version,
                    ..
                } => {
                    found = session_id == target_session_id;
                    if !found {
                        continue;
                    }
                    if matches!(mode, AttachMode::V2)
                        && let Err(error) = require_v2_daemon(acp_protocol_version)
                    {
                        return Err(AttachFailure::Fatal(error));
                    }
                    self.record_readiness(provider_model.as_deref(), &available_models);
                    ui_state = SessionUiState::from_history_fields(
                        provider_name,
                        provider_model,
                        available_models,
                        reasoning_effort,
                    );
                    match mode {
                        AttachMode::V1 { replay: true } => {
                            self.replay_history(&session_id, &messages, &images, &setup.cwd)
                                .await
                                .map_err(|err| {
                                    AttachFailure::Fatal(internal("Failed to replay the session history", &err))
                                })?;
                        }
                        AttachMode::V1 { replay: false } => {}
                        AttachMode::V2 => {
                            v2.messages = messages;
                            v2.images = images;
                        }
                    }
                }
                ServerEvent::Done { id } if id == resume_id => break,
                ServerEvent::Error {
                    id,
                    message,
                    retry_after_secs,
                } if id == resume_id => {
                    if message.contains("not found") {
                        return Err(AttachFailure::Fatal(RpcError::session_not_found(target_session_id)));
                    }
                    let error = RpcError::new(
                        JSONRPC_INTERNAL_ERROR,
                        format!("Failed to attach HoronAgent session '{target_session_id}': {message}"),
                    );
                    // Once the target's History arrived the replay has begun,
                    // and a retry would send it to the client twice.
                    return Err(if !found && is_retryable_refusal(&message) {
                        AttachFailure::Busy {
                            error,
                            delay: retry_after_secs.map(Duration::from_secs),
                        }
                    } else {
                        AttachFailure::Fatal(error)
                    });
                }
                other if found && self.profile.is_extended() => {
                    self.write_horon_agent_extension_event(target_session_id, &other)
                        .await
                        .map_err(|err| AttachFailure::Fatal(internal("Failed to write a session event", &err)))?;
                }
                _ => {}
            }
        }
        if !found {
            return Err(AttachFailure::Fatal(RpcError::session_not_found(target_session_id)));
        }
        let v2 = if mode == AttachMode::V2 {
            // The session's v2 stream opens while the attach still holds the
            // foreground, so every event after the snapshot reaches one of the
            // two, and the hand-over returns what the attach left unread.
            self.prepare_v2_session(&session);
            let (snapshot, mut leftovers) = read_attach_snapshot(&mut events, target_session_id)
                .await
                .map_err(|err| AttachFailure::Fatal(internal("Failed to attach HoronAgent session", &err)))?;
            leftovers.extend(events.hand_over());
            Some(V2Attach {
                snapshot,
                leftovers,
                ..v2
            })
        } else {
            drop(events);
            None
        };
        session.session_id = target_session_id.to_string();
        session.ui_state = Arc::new(Mutex::new(ui_state));
        session.cwd = Arc::new(Mutex::new(Some(setup.cwd.clone())));
        if !setup.mcp_servers.is_empty() {
            configure_session_mcp(&session, setup.mcp_servers.clone()).await;
        }
        set_session_approval_mode(&session, AcpMode::Default.approval_mode()).await;
        self.install_session_bridge(&session);
        Ok(Attached { session, v2 })
    }

    /// Write the stored conversation to this connection only, before the
    /// caller answers `session/load`.
    pub(super) async fn replay_history(
        &self,
        session_id: &str,
        messages: &[crate::protocol::HistoryMessage],
        images: &[crate::session::RenderedImage],
        cwd: &Path,
    ) -> Result<()> {
        let options = MapperOptions::new(&self.client_capabilities(), Some(cwd.to_path_buf()));
        for update in replay_updates(messages, images, &options) {
            self.write_notification("session/update", json!({ "sessionId": session_id, "update": update }))
                .await?;
        }
        Ok(())
    }

    pub(super) async fn handle_session_close(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        if !self.close_active_session(&session_id).await {
            return self.write_rpc_error(id, RpcError::session_not_found(&session_id)).await;
        }
        self.write_result(id, json!({})).await
    }

    /// Cancel any in-flight prompt, wait (bounded) until it has answered
    /// `cancelled`, then release the session's daemon connection. False when
    /// the session is not active on this connection.
    pub(super) async fn close_active_session(&self, session_id: &str) -> bool {
        if self.spoken_version() == Some(AcpVersion::V2) {
            return self.close_active_session_v2(session_id).await;
        }
        if !self.sessions.lock().await.contains_key(session_id) {
            return false;
        }
        let prompts = self.unanswered_prompts(session_id);
        self.cancel_session_turn(session_id).await;
        // `session/cancel` withdraws the turn's calls only when a turn is in
        // flight; closing the session ends every call the client still has
        // open, including the ones no prompt asked for.
        self.withdraw_session_calls(session_id).await;
        for prompt in prompts {
            if tokio::time::timeout(PROMPT_CANCEL_BOUND, prompt.answered())
                .await
                .is_err()
            {
                crate::logging::warn(&format!(
                    "ACP: closing {session_id} before its prompt answered within {}s",
                    PROMPT_CANCEL_BOUND.as_secs()
                ));
            }
        }
        self.sessions.lock().await.remove(session_id);
        true
    }

    pub(super) async fn handle_session_delete(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        self.close_active_session(&session_id).await;
        let lookup_id = session_id.clone();
        let lookup = tokio::task::spawn_blocking(move || catalog_lookup(&lookup_id))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|lookup| lookup);
        let cwd = match lookup {
            Ok(CatalogLookup::Present { cwd }) => cwd,
            // Never created, or already deleted: succeed silently.
            Ok(CatalogLookup::Missing | CatalogLookup::Archived) => return self.write_result(id, json!({})).await,
            Err(err) => {
                return self
                    .write_rpc_error(id, internal("Failed to read the session catalog", &err))
                    .await;
            }
        };
        match self.archive_session(&session_id, cwd.as_deref()).await {
            Ok(()) => self.write_result(id, json!({})).await,
            Err(error) => self.write_rpc_error(id, error).await,
        }
    }

    /// Hide a session from listings through the daemon, which owns the
    /// session's writer. Nothing is deleted from disk.
    async fn archive_session(&self, session_id: &str, cwd: Option<&str>) -> std::result::Result<(), RpcError> {
        let (reader, writer) = self
            .connect_daemon()
            .await
            .map_err(|err| internal("Failed to reach the HoronAgent daemon", &err))?;
        let session = DaemonSession::new(session_id.to_string(), reader, writer, 2);
        let attach_id = 1;
        let subscribe = subscribe_request(attach_id, cwd.map(Path::new), Some(session_id.to_string()), None, false);
        let mut attached = session
            .request(attach_id, &subscribe)
            .await
            .map_err(|err| internal("Failed to attach the session to delete", &err))?;
        wait_for_done(&mut attached, attach_id)
            .await
            .map_err(|err| open_elsewhere(&err))?;
        drop(attached);

        let archive_id = session.next_id();
        let archive = Request::ArchiveSession {
            id: archive_id,
            archived: true,
            reason: Some("acp session/delete".to_string()),
        };
        let mut archived = session
            .request(archive_id, &archive)
            .await
            .map_err(|err| internal("Failed to delete the session", &err))?;
        // The daemon answers ArchiveSession with Done, or Error when the
        // session is busy in, or owned by, another client.
        wait_for_done(&mut archived, archive_id)
            .await
            .map_err(|err| open_elsewhere(&err))
    }
}

/// IS-13: the setup parameters of a v2 `session/new` or `session/resume`.
/// `mcpServers` entries need a `type`, and `sse` is gone.
fn session_setup_v2(params: &Value) -> std::result::Result<SessionSetup, RpcError> {
    let invalid = |message: String| RpcError::new(JSONRPC_INVALID_PARAMS, message);
    Ok(SessionSetup {
        cwd: cwd_from_params(params).map_err(invalid)?,
        additional_dirs: additional_dirs_from_params(params).map_err(invalid)?,
        mcp_servers: v2::shapes::mcp_servers(params, MCP_REMOTE_TRANSPORTS.http)?,
    })
}

/// Read a v2 attach's stream after its `Done` up to the foreground snapshot.
/// Every other event read on the way is returned, in order, for the session's
/// stream to map after the snapshot.
async fn read_attach_snapshot(
    events: &mut EventStream,
    session_id: &str,
) -> Result<(Option<AttachSnapshot>, Vec<ServerEvent>)> {
    let deadline = tokio::time::Instant::now() + ATTACH_SNAPSHOT_BOUND;
    let mut leftovers = Vec::new();
    loop {
        let Ok(event) = tokio::time::timeout_at(deadline, events.next()).await else {
            crate::logging::warn(&format!(
                "ACP: no foreground snapshot for {session_id} within {}s; reporting it idle",
                ATTACH_SNAPSHOT_BOUND.as_secs()
            ));
            return Ok((None, leftovers));
        };
        match event? {
            ServerEvent::ForegroundState {
                snapshot: true,
                running,
                run_id,
                start_message_id,
                awaiting_user,
                ..
            } => {
                let snapshot = AttachSnapshot {
                    running,
                    awaiting_user,
                    run_id,
                    start_message_id,
                };
                return Ok((Some(snapshot), leftovers));
            }
            other => leftovers.push(other),
        }
    }
}

fn open_elsewhere(err: &anyhow::Error) -> RpcError {
    RpcError::new(
        JSONRPC_INTERNAL_ERROR,
        format!("session is open in another HoronAgent client: {err:#}"),
    )
}

/// Send the session's MCP servers to the daemon. A daemon that does not
/// serve the request yet answers with an error; the session continues
/// without the servers.
async fn configure_session_mcp(session: &DaemonSession, servers: Vec<SessionMcpServer>) {
    let id = session.next_id();
    let request = Request::ConfigureSessionMcp { id, servers };
    settle_config_request(session, id, &request, "configure_session_mcp").await;
}

/// Resync the daemon's approval override with the session's recorded mode.
async fn set_session_approval_mode(session: &DaemonSession, mode: Option<&str>) {
    let id = session.next_id();
    let request = Request::SetSessionApprovalMode {
        id,
        mode: mode.map(str::to_string),
    };
    settle_config_request(session, id, &request, "set_session_approval_mode").await;
}

async fn settle_config_request(session: &DaemonSession, id: u64, request: &Request, name: &str) {
    let mut reply = match session.request(id, request).await {
        Ok(reply) => reply,
        Err(err) => {
            crate::logging::warn(&format!("ACP: {name} not sent for {}: {err:#}", session.session_id));
            return;
        }
    };
    match tokio::time::timeout(DAEMON_CONFIG_REPLY_BOUND, wait_for_accepted(&mut reply, id)).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => crate::logging::warn(&format!(
            "ACP: {name} not applied for {}; continuing without it: {err:#}",
            session.session_id
        )),
        Err(_) => crate::logging::warn(&format!(
            "ACP: {name} for {} got no reply within {}s; continuing without it",
            session.session_id,
            DAEMON_CONFIG_REPLY_BOUND.as_secs()
        )),
    }
}
