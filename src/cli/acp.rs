//! ACP (Agent Client Protocol) adapter over stdio, bridged to the HoronAgent
//! daemon. `connection` owns the JSON-RPC connection, `pump` the per-session
//! daemon reader, and this module the ACP session methods.

use super::dispatch;
use super::provider_init::ProviderChoice;
use crate::protocol::{Request, ServerEvent};
use crate::transport::{ReadHalf, WriteHalf};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

mod auth;
mod client_router;
mod connection;
mod elicitation;
mod lifecycle;
mod mapper;
mod modes;
mod permission;
mod prompt;
mod pump;
mod replay;
mod rpc;
mod session_configuration;
mod session_list;
#[cfg(test)]
mod tests;
mod util;
mod v2;

use auth::*;
pub(crate) use auth::{AcpEntry, acp_entry, run_acp_login};
use client_router::*;
use connection::*;
use elicitation::*;
use lifecycle::*;
use mapper::*;
use modes::*;
use permission::*;
use pump::*;
use replay::*;
use rpc::*;
use session_configuration::*;
use session_list::*;
use util::*;
use v2::negotiate::AcpVersion;

/// After a cancel is recorded, a prompt that sees no Done, Error, TurnStopped
/// or daemon EOF within this bound still answers `cancelled`.
const PROMPT_CANCEL_BOUND: Duration = Duration::from_secs(10);

impl AcpRuntime {
    /// `session/cancel` sent as a request rather than a notification. Some
    /// hosts do this; it cancels the same way and answers `{}`.
    async fn handle_session_cancel_request(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await?;
                return Ok(());
            }
        };
        self.cancel_session_turn(&session_id).await;
        self.write_result(id, json!({})).await
    }

    /// `session/set_config_option`, value-id variant only: no boolean option
    /// is ever offered, so the boolean variant is invalid params. The mode
    /// changes at any time; model and effort keep refusing mid-prompt.
    ///
    /// On v2 (IS-12) the value must carry `type: "id"` (or `"boolean"`, which
    /// no option accepts), the answer lists the options in v2 shapes, and a
    /// mode change reports only `config_option_update`.
    async fn handle_set_config_option(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let v2 = self.spoken_version() == Some(AcpVersion::V2);
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        let change = if v2 {
            match v2::shapes::parse_set_config_option(&message.params) {
                Ok((config_id, v2::shapes::ConfigValue::Id(value))) => Ok((config_id, value)),
                Ok((config_id, v2::shapes::ConfigValue::Boolean(_))) => Err(format!(
                    "config option {config_id:?} is not a boolean option; this agent offers no boolean options"
                )),
                Err(error) => return self.write_rpc_error(id, error).await,
            }
        } else {
            config_value_from_params(&message.params)
        };
        let (config_id, value) = match change {
            Ok(change) => change,
            Err(err) => return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await,
        };
        let session = self.sessions.lock().await.get(&session_id).cloned();
        let Some(session) = session else {
            return self.write_rpc_error(id, RpcError::session_not_found(&session_id)).await;
        };
        let offered = current_config_options(&session.ui_state, &session.mode).await;
        if let Err(err) = check_config_value(&offered, &config_id, &value) {
            return self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await;
        }

        let applied = if config_id == CONFIG_ID_MODE {
            match parse_mode_id(&value) {
                Ok(mode) => apply_mode(&session, mode).await,
                Err(error) => Err(error),
            }
        } else if session.prompt_running.load(Ordering::SeqCst) {
            Err(RpcError::new(
                JSONRPC_INVALID_PARAMS,
                format!("{config_id} cannot change while a prompt is running"),
            ))
        } else {
            let result = match config_id.as_str() {
                CONFIG_ID_MODEL => apply_model(&session, value).await,
                _ => apply_effort(&session, value).await,
            };
            result.map_err(|err| RpcError::new(JSONRPC_INTERNAL_ERROR, format!("Failed to set {config_id}: {err:#}")))
        };
        if let Err(error) = applied {
            return self.write_rpc_error(id, error).await;
        }

        let config_options = current_config_options(&session.ui_state, &session.mode).await;
        if v2 {
            let config_options = v2::shapes::config_options(&config_options);
            self.write_result(id, json!({ "configOptions": config_options }))
                .await?;
            // v2 has no `current_mode_update`: the mode is a config option.
            return self.write_config_option_update(&session).await;
        }
        self.write_result(id, json!({ "configOptions": config_options }))
            .await?;
        if config_id == CONFIG_ID_MODE {
            self.write_mode_updates(&session).await
        } else {
            self.write_config_option_update(&session).await
        }
    }

    /// Compatibility entry points used by ACP hosts that implemented the
    /// pre-configOptions model and reasoning controls. Normalize them through
    /// the standard config option path so both interfaces stay in sync.
    async fn handle_compat_config_option(
        &self,
        mut message: IncomingRequest,
        config_id: &str,
        value_fields: &[&str],
        method: &str,
    ) -> Result<()> {
        let value = match compatibility_option_value(&message.params, value_fields, method) {
            Ok(value) => value,
            Err(error) => {
                self.write_error_value(message.id, JSONRPC_INVALID_PARAMS, error)
                    .await?;
                return Ok(());
            }
        };
        let Some(params) = message.params.as_object_mut() else {
            self.write_error_value(
                message.id,
                JSONRPC_INVALID_PARAMS,
                format!("{method} params must be an object"),
            )
            .await?;
            return Ok(());
        };
        params.insert("configId".to_string(), Value::String(config_id.to_string()));
        params.insert("value".to_string(), Value::String(value));
        self.handle_set_config_option(message).await
    }

    /// v1 only: a v2 setup response carries the commands itself (IS-12).
    async fn write_available_commands(&self, session_id: &str) -> Result<()> {
        let commands = acp_available_commands();
        self.write_notification(
            "session/update",
            json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "available_commands_update",
                    "availableCommands": commands,
                }
            }),
        )
        .await
    }

    async fn run_session_command(&self, session: &DaemonSession, command: AcpSlashCommand) -> Result<String> {
        match command {
            AcpSlashCommand::Model(None) => {
                let state = session.ui_state.lock().await;
                Ok(match state.model.as_deref() {
                    Some(model) => format!("Current model: `{model}`"),
                    None => "The daemon did not report a current model.".to_string(),
                })
            }
            AcpSlashCommand::Model(Some(model)) => {
                apply_model(session, model.clone()).await?;
                self.write_config_option_update(session).await?;
                let selected = session.ui_state.lock().await.model.clone().unwrap_or(model);
                Ok(format!("Switched model to `{selected}`."))
            }
            AcpSlashCommand::Models => {
                let event = request_model_catalog(session).await?;
                let ServerEvent::History {
                    provider_name,
                    provider_model,
                    available_models,
                    ..
                } = event
                else {
                    unreachable!("request_model_catalog only returns history")
                };
                let (current, models) = {
                    let mut state = session.ui_state.lock().await;
                    if provider_name.is_some() {
                        state.provider_name = provider_name;
                    }
                    if provider_model.is_some() {
                        state.model = provider_model;
                    }
                    state.available_models = available_models;
                    (state.model.clone(), state.available_models.clone())
                };
                self.write_config_option_update(session).await?;
                Ok(format_model_catalog(current.as_deref(), &models))
            }
            AcpSlashCommand::Effort(None) => {
                let state = session.ui_state.lock().await;
                let current = state.reasoning_effort.as_deref().unwrap_or("provider default");
                let available = available_efforts(&state);
                if available.is_empty() {
                    Ok(format!("Current reasoning effort: `{current}`."))
                } else {
                    Ok(format!(
                        "Current reasoning effort: `{current}`. Available: {}.",
                        available
                            .iter()
                            .map(|effort| format!("`{effort}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                }
            }
            AcpSlashCommand::Effort(Some(effort)) => {
                apply_effort(session, effort.clone()).await?;
                self.write_config_option_update(session).await?;
                let selected = session.ui_state.lock().await.reasoning_effort.clone().unwrap_or(effort);
                Ok(format!("Set reasoning effort to `{selected}`."))
            }
        }
    }

    async fn write_config_option_update(&self, session: &DaemonSession) -> Result<()> {
        let config_options = current_config_options(&session.ui_state, &session.mode).await;
        let update = if self.spoken_version() == Some(AcpVersion::V2) {
            v2::mapper::config_option_update(&config_options)
        } else {
            json!({
                "sessionUpdate": "config_option_update",
                "configOptions": config_options,
            })
        };
        self.write_notification(
            "session/update",
            json!({
                "sessionId": session.session_id,
                "update": update,
            }),
        )
        .await
    }

    async fn write_horon_agent_extension_event(&self, session_id: &str, event: &ServerEvent) -> Result<()> {
        self.write_notification(
            "_horon_agent/server_event",
            json!({
                "sessionId": session_id,
                "event": serde_json::to_value(event).unwrap_or(Value::Null),
            }),
        )
        .await
    }
}

pub(crate) async fn run_acp_command(
    provider_choice: ProviderChoice,
    model: Option<String>,
    provider_profile: Option<String>,
    explicit_tool_profile: bool,
) -> Result<()> {
    crate::env::set_var("HORON_AGENT_NON_INTERACTIVE", "1");
    let acp_config = crate::config::config().acp.clone();
    if !explicit_tool_profile {
        crate::env::set_var("HORON_AGENT_TOOL_PROFILE", acp_config.tool_profile.trim());
        crate::config::invalidate_config_cache();
    }
    let profile = AcpProfile::parse(&acp_config.profile);
    let runtime = AcpRuntime::new(
        profile,
        provider_choice,
        model,
        provider_profile,
        Box::new(tokio::io::stdout()),
    );
    // The kill switch: with `[acp] protocol_v2 = false` (or
    // HORON_AGENT_ACP_PROTOCOL_V2=0) every client is answered exactly as v1.
    runtime.lock_connection().protocol_v2 = acp_config.protocol_v2;
    runtime.run().await
}
