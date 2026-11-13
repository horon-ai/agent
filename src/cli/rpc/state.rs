//! Session state and the commands that read or change it.
//!
//! The daemon is the source of truth for the model, the history, and the
//! session's identity; this module keeps the projection the host asks for and
//! turns each host command into the one daemon request that answers it. Anything
//! the daemon cannot answer is derived here and marked partial in
//! `docs/protocol/RPC.md`.

use anyhow::Result;
use serde_json::{Value, json};

use crate::protocol::{Request, ServerEvent};

use super::adapter::Adapter;
use super::frame;

/// The session projection the host reads through `get_state`.
pub(super) struct SessionState {
    pub(super) session_id: String,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) thinking_level: Option<String>,
    pub(super) todo_phases: Value,
    pub(super) context_window: u64,
    pub(super) message_count: usize,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) cache_read: u64,
    pub(super) cache_write: u64,
    pub(super) tool_calls: u64,
    pub(super) user_messages: u64,
    pub(super) assistant_messages: u64,
}

impl SessionState {
    pub(super) fn from_history(history: &ServerEvent, model_override: Option<&str>) -> Self {
        let ServerEvent::History {
            session_id,
            messages,
            provider_name,
            provider_model,
            reasoning_effort,
            token_usage_totals,
            ..
        } = history
        else {
            return Self::empty(model_override);
        };
        let (input_tokens, output_tokens, cache_read, cache_write) = match token_usage_totals {
            Some(totals) => (
                totals.input_tokens,
                totals.output_tokens,
                totals.cache_read_input_tokens,
                totals.cache_creation_input_tokens,
            ),
            None => (0, 0, 0, 0),
        };
        Self {
            session_id: session_id.clone(),
            provider: provider_name.clone().unwrap_or_else(|| "horon".to_string()),
            model: model_override
                .map(str::to_string)
                .unwrap_or_else(|| provider_model.clone().unwrap_or_else(|| "unknown".to_string())),
            thinking_level: reasoning_effort.clone(),
            todo_phases: json!([]),
            context_window: default_context_window(),
            message_count: messages.len(),
            input_tokens,
            output_tokens,
            cache_read,
            cache_write,
            tool_calls: messages
                .iter()
                .filter(|message| message.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty()))
                .count() as u64,
            user_messages: messages.iter().filter(|message| message.role == "user").count() as u64,
            assistant_messages: messages.iter().filter(|message| message.role == "assistant").count() as u64,
        }
    }

    fn empty(model_override: Option<&str>) -> Self {
        Self {
            session_id: String::new(),
            provider: "horon".to_string(),
            model: model_override.unwrap_or("unknown").to_string(),
            thinking_level: None,
            todo_phases: json!([]),
            context_window: default_context_window(),
            message_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read: 0,
            cache_write: 0,
            tool_calls: 0,
            user_messages: 0,
            assistant_messages: 0,
        }
    }
}

/// What the host is told when neither the config nor the daemon names a window.
/// The value matches the daemon's own generic default.
fn default_context_window() -> u64 {
    200_000
}

impl Adapter {
    pub(super) fn state_frame(&self) -> Value {
        json!({
            "model": {"provider": self.state.provider, "id": self.state.model},
            "thinkingLevel": self.state.thinking_level,
            "isStreaming": self.prompt_in_flight,
            "isCompacting": false,
            "steeringMode": "all",
            "followUpMode": "all",
            "interruptMode": "immediate",
            "sessionId": self.state.session_id,
            "sessionName": Value::Null,
            "autoCompactionEnabled": true,
            "fastModeEnabled": false,
            "fastModeActive": false,
            "tokensPerSecond": Value::Null,
            "messageCount": self.state.message_count,
            "queuedMessageCount": 0,
            "todoPhases": self.state.todo_phases,
            "contextUsage": {
                "tokens": self.state.input_tokens + self.state.output_tokens,
                "contextWindow": self.state.context_window,
                "percent": context_percent(self.state.input_tokens + self.state.output_tokens, self.state.context_window),
            },
        })
    }

    /// Ask the daemon for the catalog and project it into the host's model shape.
    pub(super) async fn available_models(&mut self) -> std::result::Result<Value, String> {
        let request = Request::GetModelCatalog {
            id: self.daemon.next_id(),
            subscribe_usage_updates: false,
        };
        let reply = match self.daemon.request(&request).await {
            Ok(reply) => reply,
            Err(error) => return Err(error.to_string()),
        };
        let ServerEvent::History {
            provider_name,
            provider_model,
            available_models,
            available_model_routes,
            ..
        } = reply
        else {
            return Err("unexpected reply to get_model_catalog".to_string());
        };
        let provider = provider_name.unwrap_or_else(|| self.state.provider.clone());
        let profile = crate::config::config().providers.get(&provider).cloned();
        let base_url = profile
            .as_ref()
            .map(|profile| profile.base_url.clone())
            .unwrap_or_default();

        // A route names the endpoint that serves the model; a bare model name
        // from an older daemon does not, so the profile's own name is used.
        let routes: Vec<(String, String)> = if available_model_routes.is_empty() {
            available_models
                .into_iter()
                .map(|model| (model, provider.clone()))
                .collect()
        } else {
            let mut routes: Vec<(String, String)> = Vec::new();
            for route in available_model_routes.into_iter().filter(|route| route.available) {
                if routes.iter().any(|(model, _)| model == &route.model) {
                    continue;
                }
                routes.push((route.model, route.provider));
            }
            routes
        };

        let models: Vec<Value> = routes
            .into_iter()
            .map(|(model, route_provider)| {
                let context_window = profile
                    .as_ref()
                    .and_then(|profile| {
                        profile
                            .models
                            .iter()
                            .find(|entry| entry.id == model)
                            .and_then(|entry| entry.context_window)
                    })
                    .unwrap_or_else(|| default_context_window() as usize);
                let thinking = profile
                    .as_ref()
                    .and_then(|profile| profile.models.iter().find(|entry| entry.id == model))
                    .and_then(|entry| entry.reasoning_effort.clone());
                json!({
                    "provider": route_provider,
                    "id": model,
                    "name": model,
                    "api": frame::API_METHOD,
                    "baseUrl": base_url,
                    "contextWindow": context_window,
                    "maxTokens": 8192,
                    "input": ["text"],
                    "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                    "thinkingLevels": thinking.map(|level| vec![level]).unwrap_or_default(),
                })
            })
            .collect();

        let mut models = models;
        // The active model must be in the catalog, or a host that echoes the
        // list back cannot select it.
        if let Some(active) = provider_model
            && !models
                .iter()
                .any(|model| model.get("id").and_then(Value::as_str) == Some(active.as_str()))
        {
            models.push(json!({
                "provider": provider,
                "id": active,
                "name": active,
                "api": frame::API_METHOD,
                "baseUrl": base_url,
                "contextWindow": default_context_window(),
                "maxTokens": 8192,
                "input": ["text"],
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            }));
        }
        Ok(json!({"models": models}))
    }

    pub(super) async fn set_model(&mut self, model: &str) -> std::result::Result<Value, String> {
        let request = Request::SetModel {
            id: self.daemon.next_id(),
            model: model.to_string(),
        };
        match self.daemon.request(&request).await {
            Ok(ServerEvent::ModelChanged {
                model: active, error, ..
            }) => {
                if let Some(error) = error {
                    return Err(error);
                }
                self.state.model = active.clone();
                Ok(json!({"provider": self.state.provider, "id": active}))
            }
            Ok(ServerEvent::Error { message, .. }) => Err(message),
            Ok(other) => Err(format!("unexpected reply to set_model: {other:?}")),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(super) async fn set_thinking_level(&mut self, level: &str) -> std::result::Result<(), String> {
        let request = Request::SetReasoningEffort {
            id: self.daemon.next_id(),
            effort: level.to_string(),
            target_session_id: None,
        };
        match self.daemon.request(&request).await {
            Ok(ServerEvent::ReasoningEffortChanged { effort, error, .. }) => {
                if let Some(error) = error {
                    return Err(error);
                }
                self.state.thinking_level = effort;
                Ok(())
            }
            Ok(ServerEvent::Error { message, .. }) => Err(message),
            Ok(other) => Err(format!("unexpected reply to set_reasoning_effort: {other:?}")),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(super) async fn set_session_name(&mut self, name: &str) -> std::result::Result<(), String> {
        let request = Request::RenameSession {
            id: self.daemon.next_id(),
            title: Some(name.to_string()),
        };
        match self.daemon.request(&request).await {
            Ok(ServerEvent::Error { message, .. }) => Err(message),
            Ok(_) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Store the host's todo phases in the session's todo file.
    ///
    /// The host's `blocked` status has no counterpart here, so it lands as
    /// `abandoned` and the blocker text is dropped; the document says so.
    pub(super) fn set_todos(&mut self, phases: &Value) -> std::result::Result<Value, String> {
        let todos = phases_to_todos(phases);
        let previous = crate::todo::load_todos(&self.state.session_id).unwrap_or_default();
        crate::todo::save_todos(&self.state.session_id, &todos).map_err(|error| error.to_string())?;
        crate::todo::update_todo_review_cycle(&self.state.session_id, &previous, &todos)
            .map_err(|error| error.to_string())?;
        let todo_phases = todos_to_phases(&todos);
        self.state.todo_phases = todo_phases.clone();
        Ok(json!({"todoPhases": todo_phases}))
    }

    pub(super) async fn compact(&mut self) -> std::result::Result<Value, String> {
        let request = Request::Compact {
            id: self.daemon.next_id(),
            method: None,
        };
        match self.daemon.request(&request).await {
            Ok(ServerEvent::CompactResult { message, success, .. }) => {
                if success {
                    Ok(json!({"summary": message}))
                } else {
                    Err(message)
                }
            }
            Ok(ServerEvent::Error { message, .. }) => Err(message),
            Ok(other) => Err(format!("unexpected reply to compact: {other:?}")),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(super) async fn session_stats(&mut self) -> Result<Value> {
        let messages = self.history_messages().await?;
        let total = messages.len() as u64;
        Ok(json!({
            "sessionId": self.state.session_id,
            "userMessages": self.state.user_messages,
            "assistantMessages": self.state.assistant_messages,
            "toolCalls": self.state.tool_calls,
            "toolResults": self.state.tool_calls,
            "totalMessages": total,
            "tokens": {
                "input": self.state.input_tokens,
                "output": self.state.output_tokens,
                "reasoning": 0,
                "cacheRead": self.state.cache_read,
                "cacheWrite": self.state.cache_write,
                "total": self.state.input_tokens + self.state.output_tokens,
            },
            "premiumRequests": 0,
            "cost": 0,
            "contextUsage": {
                "tokens": self.state.input_tokens + self.state.output_tokens,
                "contextWindow": self.state.context_window,
                "percent": context_percent(
                    self.state.input_tokens + self.state.output_tokens,
                    self.state.context_window,
                ),
            },
        }))
    }

    pub(super) async fn messages_frame(&mut self) -> Result<Value> {
        let messages = self.history_messages().await?;
        Ok(json!({"messages": messages, "totalMessages": messages.len()}))
    }

    /// The session's stored messages, in the host's `AgentMessage` shapes.
    async fn history_messages(&mut self) -> Result<Vec<Value>> {
        let request = Request::GetHistory {
            id: self.daemon.next_id(),
        };
        let reply = self.daemon.request(&request).await?;
        let ServerEvent::History { messages, .. } = reply else {
            anyhow::bail!("unexpected reply to get_history");
        };
        Ok(messages
            .into_iter()
            .map(|message| {
                let timestamp = frame::now_ms();
                match message.role.as_str() {
                    "user" => json!({
                        "role": "user",
                        "content": [frame::text_block(&message.content)],
                        "attribution": "user",
                        "timestamp": timestamp,
                    }),
                    "assistant" => json!({
                        "role": "assistant",
                        "content": [frame::text_block(&message.content)],
                        "api": frame::API_METHOD,
                        "provider": self.state.provider,
                        "model": self.state.model,
                        "usage": frame::usage(0, 0, 0, 0),
                        "stopReason": "stop",
                        "timestamp": timestamp,
                    }),
                    "tool" => {
                        let call = message.tool_data.clone().unwrap_or_default();
                        json!({
                            "role": "toolResult",
                            "toolCallId": call.id,
                            "toolName": call.name,
                            "content": [frame::text_block(&message.content)],
                            "details": {},
                            "timestamp": timestamp,
                        })
                    }
                    _ => json!({
                        "role": "user",
                        "content": [frame::text_block(&message.content)],
                        "attribution": "user",
                        "timestamp": timestamp,
                    }),
                }
            })
            .collect())
    }

    /// Run one shell command for the host. Horon has no request that runs a
    /// command outside a turn, so the adapter runs it itself.
    pub(super) async fn run_bash(&mut self, command: &str) -> Result<Value> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let started = std::time::Instant::now();
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&cwd)
            .output()
            .await?;
        let _ = started;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        let total_lines = text.lines().count();
        let total_bytes = text.len();
        Ok(json!({
            "output": text,
            "exitCode": output.status.code(),
            "cancelled": false,
            "timedOut": false,
            "truncated": false,
            "totalLines": total_lines,
            "totalBytes": total_bytes,
            "outputLines": total_lines,
            "outputBytes": total_bytes,
            "workingDir": cwd.display().to_string(),
        }))
    }
}

fn context_percent(tokens: u64, window: u64) -> f64 {
    if window == 0 {
        return 0.0;
    }
    (tokens as f64 / window as f64) * 100.0
}

/// Flatten the host's phases into the session's todo rows, one group per phase.
fn phases_to_todos(phases: &Value) -> Vec<crate::todo::TodoItem> {
    let mut todos = Vec::new();
    let Some(phases) = phases.as_array() else {
        return todos;
    };
    for phase in phases {
        let group = phase
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let Some(tasks) = phase.get("tasks").and_then(Value::as_array) else {
            continue;
        };
        for (index, task) in tasks.iter().enumerate() {
            let content = task
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if content.is_empty() {
                continue;
            }
            let (status, blocker) = match task.get("status").and_then(Value::as_str) {
                Some("in_progress") => ("in_progress", None),
                Some("completed") => ("completed", None),
                Some("abandoned") => ("abandoned", None),
                // Horon keeps a blocked todo and the reason it waits on, so the
                // host's `blocker` travels with the item.
                Some("blocked") => (
                    "blocked",
                    task.get("blocker").and_then(Value::as_str).map(str::to_string),
                ),
                _ => ("pending", None),
            };
            todos.push(crate::todo::TodoItem {
                content,
                status: status.to_string(),
                priority: "medium".to_string(),
                id: format!("{group}-{index}"),
                group: (!group.is_empty()).then(|| group.clone()),
                blocker,
                ..Default::default()
            });
        }
    }
    todos
}

/// Rebuild the host's phases from the session's todo rows.
fn todos_to_phases(todos: &[crate::todo::TodoItem]) -> Value {
    let mut phases: Vec<(String, Vec<Value>)> = Vec::new();
    for todo in todos {
        let name = todo.group.clone().unwrap_or_else(|| "Tasks".to_string());
        let task = json!({"content": todo.content, "status": todo.status});
        match phases.iter_mut().find(|(phase, _)| *phase == name) {
            Some((_, tasks)) => tasks.push(task),
            None => phases.push((name, vec![task])),
        }
    }
    Value::Array(
        phases
            .into_iter()
            .map(|(name, tasks)| json!({"name": name, "tasks": tasks}))
            .collect(),
    )
}
