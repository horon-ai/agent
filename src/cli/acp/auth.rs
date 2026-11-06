//! ACP authentication: the advertised `authMethods` and `authenticate` as a
//! readiness check against the daemon.

use super::*;

pub(super) const AUTH_METHOD_CREDENTIALS: &str = "horon-agent-credentials";

/// The one method: the profile credentials the daemon already resolves. A
/// terminal method would need a login command this build does not have, so no
/// client capability ever adds a second entry.
pub(super) fn auth_methods() -> Vec<Value> {
    vec![json!({
        "id": AUTH_METHOD_CREDENTIALS,
        "name": "Use HoronAgent login",
        "description": "Uses the OpenAI-compatible provider profile configured in [providers.<name>]. Run 'horon-agent provider add' to create one.",
    })]
}

impl AcpRuntime {
    pub(super) async fn handle_authenticate(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        match message.params.get("methodId").and_then(Value::as_str) {
            Some(AUTH_METHOD_CREDENTIALS) => {}
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
            // Unknown ids keep v1 `authenticate`'s answers.
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
