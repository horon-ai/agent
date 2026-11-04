//! ACP authentication: the advertised `authMethods`, `authenticate` as a
//! readiness check against the daemon, and `horon-agent acp --login`.

use super::*;

pub(super) const AUTH_METHOD_CREDENTIALS: &str = "horon-agent-credentials";
pub(super) const AUTH_METHOD_TERMINAL: &str = "horon-agent-terminal-login";

/// The agent method is always offered; the terminal method only to a client
/// that advertised `auth.terminal`, as the spec requires.
pub(super) fn auth_methods(client_capabilities: &Value) -> Vec<Value> {
    let mut methods = vec![json!({
        "id": AUTH_METHOD_CREDENTIALS,
        "name": "Use HoronAgent login",
        "description": "Uses the provider login stored by horon-agent. Run 'horon-agent login' first if none exists.",
    })];
    if client_capabilities.pointer("/auth/terminal").and_then(Value::as_bool) == Some(true) {
        methods.push(json!({
            "id": AUTH_METHOD_TERMINAL,
            "name": "Log in from the terminal",
            "type": "terminal",
            "args": ["--login"],
            "env": {},
        }));
    }
    methods
}

impl AcpRuntime {
    pub(super) async fn handle_authenticate(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        match message.params.get("methodId").and_then(Value::as_str) {
            Some(AUTH_METHOD_CREDENTIALS) => {}
            Some(AUTH_METHOD_TERMINAL) => {
                return self
                    .write_error_value(
                        id,
                        JSONRPC_INVALID_PARAMS,
                        format!("{AUTH_METHOD_TERMINAL} runs in a terminal, not through authenticate"),
                    )
                    .await;
            }
            Some(other) => {
                return self
                    .write_error_value(
                        id,
                        JSONRPC_INVALID_PARAMS,
                        format!("Unknown authentication method: {other}"),
                    )
                    .await;
            }
            None => {
                return self
                    .write_error_value(
                        id,
                        JSONRPC_INVALID_PARAMS,
                        "authenticate requires a string methodId".to_string(),
                    )
                    .await;
            }
        }
        match self.check_readiness().await {
            Ok(true) => self.write_result(id, json!({})).await,
            Ok(false) => self.write_rpc_error(id, RpcError::auth_required()).await,
            Err(err) => {
                self.write_error_value(
                    id,
                    JSONRPC_INTERNAL_ERROR,
                    format!("Failed to check the HoronAgent login: {err:#}"),
                )
                .await
            }
        }
    }

    /// Whether the daemon reports a usable model. A positive verdict from any
    /// session History is cached for the life of this process.
    ///
    /// The daemon refuses every request that precedes a Subscribe
    /// (crates/app-core/src/server/client_subscribe_context.rs:21), so a
    /// History exists only on a subscribed connection. Without a cached
    /// verdict the check subscribes once, reads History and disconnects
    /// without prompting; the daemon writes nothing to disk for an unprompted
    /// session.
    async fn check_readiness(&self) -> Result<bool> {
        if self.ready.load(Ordering::SeqCst) {
            return Ok(true);
        }
        let cwd = std::env::current_dir()
            .ok()
            .filter(|cwd| cwd.is_absolute())
            .unwrap_or_else(|| PathBuf::from("/"));
        let (_probe, history) = self
            .subscribe_new_session(&subscribe_request(1, Some(&cwd), None, None, false))
            .await?;
        let ServerEvent::History {
            provider_model,
            available_models,
            ..
        } = history
        else {
            unreachable!("request_history only returns history")
        };
        Ok(self.record_readiness(provider_model.as_deref(), &available_models))
    }

    /// IS-15: `auth/login {methodId}` behaves like v1 `authenticate`, and a
    /// success ends this connection's logged-out state.
    pub(super) async fn handle_auth_login(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id.clone();
        if message.params.get("methodId").and_then(Value::as_str) != Some(AUTH_METHOD_CREDENTIALS) {
            // The terminal method and unknown ids keep v1 `authenticate`'s answers.
            return self.handle_authenticate(message).await;
        }
        match self.check_readiness().await {
            Ok(true) => {
                self.lock_connection().logged_out = false;
                self.write_result(id, json!({})).await
            }
            Ok(false) => self.write_rpc_error(id, RpcError::auth_required()).await,
            Err(err) => {
                self.write_error_value(
                    id,
                    JSONRPC_INTERNAL_ERROR,
                    format!("Failed to check the HoronAgent login: {err:#}"),
                )
                .await
            }
        }
    }

    /// IS-15: `auth/logout` ends this connection's authenticated state only. It
    /// deletes no credential and touches no other surface's login.
    pub(super) async fn handle_auth_logout(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        self.lock_connection().logged_out = true;
        self.write_result(id, json!({})).await
    }
}

/// What `horon-agent acp` runs: the stdio adapter, or only the interactive
/// login when `--login` is given.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AcpEntry {
    Stdio,
    Login,
}

pub(crate) fn acp_entry(login: bool) -> AcpEntry {
    if login { AcpEntry::Login } else { AcpEntry::Stdio }
}

/// The terminal auth method: the same provider login the CLI bootstrap runs,
/// without starting the stdio loop. Prompts go to stderr, so stdout stays
/// empty.
pub(crate) async fn run_acp_login() -> Result<()> {
    let provider = super::super::provider_init::prompt_login_provider_selection(
        &crate::provider_catalog::server_bootstrap_login_providers(),
        "Log in to use HoronAgent from your editor.\n\nChoose a provider:",
    )?;
    super::super::login::run_login_provider(provider, None, super::super::login::LoginOptions::default()).await?;
    super::super::provider_init::apply_login_provider_profile_env(provider);
    Ok(())
}
