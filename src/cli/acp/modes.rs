//! ACP session modes: `default`, `ask-commands` and `auto`, each one of the
//! daemon's per-session approval overrides (`set_session_approval_mode`).
//!
//! The mapping follows the approval gate exactly as it behaves
//! (crates/app-core/src/approval.rs): `default` clears the override so the
//! daemon configuration decides, `ask-commands` is `write` (plain file edits
//! run, shell and network calls ask), and `auto` is `off` (nothing asks).
//! The mode is offered twice, as `modes` and as the first `configOptions`
//! entry (category `mode`), and both setters go through [`apply_mode`].

use super::*;

pub(super) const CONFIG_ID_MODE: &str = "mode";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum AcpMode {
    #[default]
    Default,
    AskCommands,
    Auto,
}

impl AcpMode {
    pub(super) const ALL: [Self; 3] = [Self::Default, Self::AskCommands, Self::Auto];

    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AskCommands => "ask-commands",
            Self::Auto => "auto",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::AskCommands => "Ask before commands",
            Self::Auto => "Auto",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Default => "Use the approval mode from HoronAgent config",
            Self::AskCommands => "Ask before shell and network tools; file edits run",
            Self::Auto => "Run tools without asking",
        }
    }

    /// The `set_session_approval_mode` value this mode sends to the daemon.
    pub(super) fn approval_mode(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::AskCommands => Some("write"),
            Self::Auto => Some("off"),
        }
    }

    pub(super) fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.id() == id)
    }
}

pub(super) fn session_modes(current: AcpMode) -> Value {
    json!({
        "currentModeId": current.id(),
        "availableModes": AcpMode::ALL
            .iter()
            .map(|mode| json!({ "id": mode.id(), "name": mode.name(), "description": mode.description() }))
            .collect::<Vec<_>>(),
    })
}

/// The mode as a select config option; always the first `configOptions` entry.
pub(super) fn mode_config_option(current: AcpMode) -> Value {
    json!({
        "type": "select",
        "id": CONFIG_ID_MODE,
        "name": "Mode",
        "category": "mode",
        "currentValue": current.id(),
        "options": AcpMode::ALL
            .iter()
            .map(|mode| json!({ "value": mode.id(), "name": mode.name(), "description": mode.description() }))
            .collect::<Vec<_>>(),
    })
}

fn unknown_mode(mode_id: &str) -> RpcError {
    let known: Vec<&str> = AcpMode::ALL.iter().map(|mode| mode.id()).collect();
    RpcError::new(
        JSONRPC_INVALID_PARAMS,
        format!("Unknown mode id {mode_id:?}; available: {}", known.join(", ")),
    )
}

pub(super) fn parse_mode_id(mode_id: &str) -> std::result::Result<AcpMode, RpcError> {
    AcpMode::from_id(mode_id).ok_or_else(|| unknown_mode(mode_id))
}

impl AcpRuntime {
    /// `session/set_mode {sessionId, modeId}`. Works while a prompt runs: the
    /// daemon reads the override at each gate decision.
    pub(super) async fn handle_set_mode(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        let Some(mode_id) = message.params.get("modeId").and_then(Value::as_str) else {
            return self
                .write_error_value(
                    id,
                    JSONRPC_INVALID_PARAMS,
                    "session/set_mode requires a string modeId".to_string(),
                )
                .await;
        };
        let mode = match parse_mode_id(mode_id) {
            Ok(mode) => mode,
            Err(error) => return self.write_rpc_error(id, error).await,
        };
        let session = self.sessions.lock().await.get(&session_id).cloned();
        let Some(session) = session else {
            return self.write_rpc_error(id, RpcError::session_not_found(&session_id)).await;
        };
        if let Err(error) = apply_mode(&session, mode).await {
            return self.write_rpc_error(id, error).await;
        }
        self.write_result(id, json!({})).await?;
        self.write_mode_updates(&session).await
    }

    pub(super) async fn write_mode_updates(&self, session: &DaemonSession) -> Result<()> {
        let current = *session.mode.lock().await;
        self.write_notification(
            "session/update",
            json!({
                "sessionId": session.session_id,
                "update": {
                    "sessionUpdate": "current_mode_update",
                    "currentModeId": current.id(),
                }
            }),
        )
        .await?;
        self.write_config_option_update(session).await
    }
}

/// Bound for the daemon's answer to `set_session_approval_mode`; the request
/// never waits on a model turn.
const MODE_REPLY_BOUND: Duration = Duration::from_secs(30);

/// Switch the daemon's approval override for this connection's turns, then
/// record the mode. The mode is recorded only after the daemon accepted it.
pub(super) async fn apply_mode(session: &DaemonSession, mode: AcpMode) -> std::result::Result<(), RpcError> {
    let request_id = session.next_id();
    let request = Request::SetSessionApprovalMode {
        id: request_id,
        mode: mode.approval_mode().map(str::to_string),
    };
    let failed = |detail: String| {
        RpcError::new(
            JSONRPC_INTERNAL_ERROR,
            format!("Failed to switch to mode {}: {detail}", mode.id()),
        )
    };
    let mut reply = session
        .request(request_id, &request)
        .await
        .map_err(|err| failed(format!("{err:#}")))?;
    match tokio::time::timeout(MODE_REPLY_BOUND, wait_for_accepted(&mut reply, request_id)).await {
        Ok(Ok(())) => {
            *session.mode.lock().await = mode;
            Ok(())
        }
        Ok(Err(err)) => Err(failed(format!("{err:#}"))),
        Err(_) => Err(failed(format!(
            "the daemon did not answer within {}s",
            MODE_REPLY_BOUND.as_secs()
        ))),
    }
}
