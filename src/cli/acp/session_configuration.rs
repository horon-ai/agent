//! ACP session configuration: the mode, model and reasoning effort options.

use super::*;

pub(super) const CONFIG_ID_MODEL: &str = "model";
pub(super) const CONFIG_ID_EFFORT: &str = "reasoning_effort";

pub(super) fn acp_available_commands() -> Vec<Value> {
    vec![
        json!({
            "name": "model",
            "description": "Switch the model for this session, or show the current model",
            "input": { "hint": "model id (optional)" },
        }),
        json!({
            "name": "models",
            "description": "List models available from the active provider",
        }),
        json!({
            "name": "effort",
            "description": "Set reasoning effort, or show the current effort",
            "input": { "hint": "none|minimal|low|medium|high|xhigh|max (optional)" },
        }),
    ]
}

pub(super) fn insert_session_configuration(result: &mut Value, state: &SessionUiState, mode: AcpMode) {
    let Some(object) = result.as_object_mut() else {
        return;
    };
    object.insert("modes".to_string(), session_modes(mode));
    object.insert(
        "configOptions".to_string(),
        Value::Array(session_config_options(state, mode)),
    );
}

/// IS-12: a v2 setup response carries the options in v2 shapes and no `modes`
/// object; the mode is the first config option (category `mode`).
pub(super) fn insert_session_configuration_v2(result: &mut Value, state: &SessionUiState, mode: AcpMode) {
    let Some(object) = result.as_object_mut() else {
        return;
    };
    object.insert(
        "configOptions".to_string(),
        Value::Array(v2::shapes::config_options(&session_config_options(state, mode))),
    );
}

/// IS-12: a v2 setup response lists the session's commands in v2 shapes, and
/// leaves `availableCommands` out, never `null` or `[]`, when there are none.
pub(super) fn insert_available_commands_v2(result: &mut Value, v1_commands: &[Value]) {
    let commands = v2::shapes::available_commands(v1_commands);
    if let (false, Some(object)) = (commands.is_empty(), result.as_object_mut()) {
        object.insert("availableCommands".to_string(), Value::Array(commands));
    }
}

pub(super) fn config_value_from_params(params: &Value) -> std::result::Result<(String, String), String> {
    let config_id = params
        .get("configId")
        .and_then(Value::as_str)
        .ok_or("session/set_config_option requires a string configId")?;
    if params.get("type").and_then(Value::as_str) == Some("boolean") {
        return Err(format!(
            "config option {config_id:?} is not a boolean option; this agent offers no boolean options"
        ));
    }
    let value = params
        .get("value")
        .and_then(Value::as_str)
        .ok_or("session/set_config_option requires a string value")?;
    Ok((config_id.to_string(), value.to_string()))
}

pub(super) async fn current_config_options(ui_state: &Mutex<SessionUiState>, mode: &Mutex<AcpMode>) -> Vec<Value> {
    let mode = *mode.lock().await;
    session_config_options(&*ui_state.lock().await, mode)
}

pub(super) fn select_values(option: &Value) -> Vec<&str> {
    option["options"]
        .as_array()
        .map(|options| options.iter().filter_map(|entry| entry["value"].as_str()).collect())
        .unwrap_or_default()
}

/// A `session/set_config_option` value-id request: the option must be one
/// this session offers and the value one of its choices.
pub(super) fn check_config_value(options: &[Value], config_id: &str, value: &str) -> std::result::Result<(), String> {
    let Some(option) = options.iter().find(|option| option["id"] == config_id) else {
        let known: Vec<&str> = options.iter().filter_map(|option| option["id"].as_str()).collect();
        return Err(format!(
            "Unknown config option id {config_id:?}; available: {}",
            known.join(", ")
        ));
    };
    let values = select_values(option);
    if values.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "Unknown value {value:?} for config option {config_id:?}; available: {}",
            values.join(", ")
        ))
    }
}

pub(super) fn available_efforts(state: &SessionUiState) -> Vec<&'static str> {
    crate::provider::inferred_reasoning_efforts(
        state.provider_name.as_deref(),
        state.model.as_deref(),
    )
    .into_iter()
    // `swarm`/`swarm-deep` are TUI sentinels, not provider effort levels.
    .filter(|effort| !effort.starts_with("swarm"))
    .collect()
}

/// Build the ACP `configOptions` array: the mode first, then the model
/// selector and reasoning effort when the daemon reported them. An option
/// without a current value or without choices is left out, so no option ever
/// carries an empty `currentValue`.
pub(super) fn session_config_options(state: &SessionUiState, mode: AcpMode) -> Vec<Value> {
    let mut options = vec![mode_config_option(mode)];

    if let Some(model) = state.model.as_deref().filter(|model| !model.trim().is_empty()) {
        let mut models: Vec<String> = state
            .available_models
            .iter()
            .filter(|candidate| !candidate.trim().is_empty())
            .cloned()
            .collect();
        if !models.iter().any(|candidate| candidate == model) {
            models.insert(0, model.to_string());
        }
        let select_options: Vec<Value> = models
            .iter()
            .map(|name| json!({ "value": name, "name": name }))
            .collect();
        options.push(json!({
            "type": "select",
            "id": CONFIG_ID_MODEL,
            "name": "Model",
            "category": "model",
            "currentValue": model,
            "options": select_options,
        }));
    }

    let efforts = available_efforts(state);
    if !efforts.is_empty() {
        let current = state
            .reasoning_effort
            .as_deref()
            .filter(|effort| efforts.contains(effort))
            .unwrap_or_else(|| {
                if efforts.contains(&"medium") {
                    "medium"
                } else {
                    efforts[0]
                }
            });
        let select_options: Vec<Value> = efforts
            .iter()
            .map(|name| json!({ "value": name, "name": name }))
            .collect();
        options.push(json!({
            "type": "select",
            "id": CONFIG_ID_EFFORT,
            "name": "Reasoning effort",
            "category": "thought_level",
            "currentValue": current,
            "options": select_options,
        }));
    }

    options
}
