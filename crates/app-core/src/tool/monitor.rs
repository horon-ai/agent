//! The `monitor` tool: persistent file and command watches with durable wakes.

use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_base::monitor::{
    Clock, CommandPolicy, MonitorError, MonitorKind, MonitorRecord, MonitorRuntime, MonitorSpec, MonitorStatus,
    RestartPolicy, SoftInterruptWakeSink, SystemClock, WakeSink,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// The one tool name the registry, the profile, and the schema agree on.
pub const MONITOR_TOOL: &str = "monitor";

/// Runtimes this process holds, one per session.
static RUNTIMES: LazyLock<Mutex<HashMap<String, Arc<Mutex<MonitorRuntime>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn runtimes() -> std::sync::MutexGuard<'static, HashMap<String, Arc<Mutex<MonitorRuntime>>>> {
    RUNTIMES.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn journal_root() -> Result<PathBuf, MonitorError> {
    let home =
        crate::storage::horon_agent_dir().map_err(|error| MonitorError::io(format!("no agent home: {error}")))?;
    Ok(home.join(horon_agent_base::durable_runtime::JOURNAL_DIR_NAME))
}

/// The policy a monitored command obeys: the same destructive-command gate the
/// `bash` tool applies, and the same session working directory it resolves.
struct BashCommandPolicy {
    working_dir: Option<PathBuf>,
}

impl CommandPolicy for BashCommandPolicy {
    fn check(&self, command: &str, cwd: Option<&Path>, _session_id: &str) -> Result<(), String> {
        let start = cwd.map(Path::to_path_buf).or_else(|| self.working_dir.clone());
        match super::bash::monitored_command_refusal(command, start) {
            Some(refusal) => Err(refusal),
            None => Ok(()),
        }
    }

    fn working_dir(&self, requested: Option<&Path>, _session_id: &str) -> Option<PathBuf> {
        requested.map(Path::to_path_buf).or_else(|| self.working_dir.clone())
    }
}

fn runtime_for(ctx: &ToolContext) -> Result<Arc<Mutex<MonitorRuntime>>, MonitorError> {
    let session_id = ctx.session_id.clone();
    if let Some(existing) = runtimes().get(&session_id) {
        return Ok(Arc::clone(existing));
    }
    let root = journal_root()?;
    let sink: Arc<dyn WakeSink> = Arc::new(SoftInterruptWakeSink::new(session_id.clone()));
    let policy: Arc<dyn CommandPolicy> = Arc::new(BashCommandPolicy {
        working_dir: ctx.working_dir.clone(),
    });
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let runtime = MonitorRuntime::open_in(&root, &session_id, clock, sink, policy)?;
    let handle = Arc::new(Mutex::new(runtime));
    runtimes().insert(session_id.clone(), Arc::clone(&handle));
    spawn_sampler(session_id, Arc::clone(&handle));
    Ok(handle)
}

/// Sample this session's monitors every second until the runtime is gone.
///
/// The loop is a plain thread, not a Tokio task: every tick reads files and
/// writes the journal, and none of that may run on an async worker. It holds
/// only a weak reference, so dropping the session's runtime ends it.
fn spawn_sampler(session_id: String, runtime: Arc<Mutex<MonitorRuntime>>) {
    let weak = Arc::downgrade(&runtime);
    drop(runtime);
    std::thread::Builder::new()
        .name(format!("monitor-sampler-{session_id}"))
        .spawn(move || {
            loop {
                std::thread::sleep(horon_agent_base::monitor::POLL_INTERVAL);
                let Some(runtime) = weak.upgrade() else {
                    return;
                };
                let outcome = {
                    let mut guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.tick()
                };
                if let Err(error) = outcome {
                    crate::logging::warn(&format!("[monitor] sampling session {session_id} failed: {error}"));
                    return;
                }
            }
        })
        .ok();
}

/// Drop a session's cached runtime, so the next call reopens the journal.
///
/// The sampler thread holds only a weak reference, so removing the entry ends
/// it at the next tick instead of leaving one OS thread per closed session.
pub fn forget_session(session_id: &str) {
    runtimes().remove(session_id);
}

/// Sample one session's monitors once and deliver what the pacer allows.
///
/// The sampler thread calls this on the one-second cadence; a test calls it
/// directly so a tick is an explicit step rather than a wait.
#[cfg(test)]
pub fn tick_for_test(session_id: &str) -> Result<Vec<String>, MonitorError> {
    let runtime = match runtimes().get(session_id) {
        Some(runtime) => Arc::clone(runtime),
        None => {
            return Err(MonitorError::not_found(format!(
                "no monitor runtime for session {session_id}"
            )));
        }
    };
    let mut guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.tick()
}

pub struct MonitorTool;

impl MonitorTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MonitorTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct MonitorInput {
    /// create, list, inspect, cancel, or rearm
    action: String,
    /// file or command
    #[serde(default)]
    kind: Option<String>,
    /// Absolute path of a file monitor
    #[serde(default)]
    path: Option<String>,
    /// Shell command of a command monitor
    #[serde(default)]
    command: Option<String>,
    /// Working directory for a command monitor
    #[serde(default)]
    cwd: Option<String>,
    /// Monitor id for inspect, cancel, and rearm
    #[serde(default)]
    id: Option<String>,
    /// Restart policy; only `never` is accepted
    #[serde(default)]
    restart_policy: Option<String>,
    /// Short label shown in the UI
    #[serde(default)]
    intent: Option<String>,
}

#[async_trait]
impl Tool for MonitorTool {
    fn name(&self) -> &str {
        MONITOR_TOOL
    }

    fn description(&self) -> &str {
        // Always-on prompt cost: the pacing policy and restart rules are host
        // behavior, and `rearm` is named in the action enum.
        "Watch a file or command across restarts; changes wake you instead of polling."
    }

    fn effects(&self) -> super::ToolEffects {
        super::ToolEffects::write()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "inspect", "cancel", "rearm"],
                    "description": "create a monitor, list this session's monitors, inspect one, cancel one, or rearm one"
                },
                "kind": {
                    "type": "string",
                    "enum": ["file", "command"],
                    "description": "What the monitor watches. Required for create."
                },
                "path": {
                    "type": "string",
                    "description": "Absolute path of the file to watch. Required when kind is file."
                },
                "command": {
                    "type": "string",
                    "description": "Shell command to run once. Required when kind is command."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for a command monitor; defaults to the session directory."
                },
                "id": {
                    "type": "string",
                    "description": "Monitor id. Required for inspect, cancel, and rearm."
                },
                "restart_policy": {
                    "type": "string",
                    "enum": ["never"],
                    "description": "Only `never` is supported: a restore records needs_rearm instead of starting a process."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: MonitorInput =
            serde_json::from_value(input).map_err(|error| anyhow::anyhow!("invalid monitor input: {error}"))?;
        let runtime = match runtime_for(&ctx) {
            Ok(runtime) => runtime,
            Err(error) => return Ok(refusal(error)),
        };

        match params.action.as_str() {
            "create" => create(runtime, &ctx, params).await,
            "list" => {
                let guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let monitors: Vec<Value> = guard.list().iter().map(record_value).collect();
                Ok(ToolOutput::new(render_list(&monitors)).with_metadata(json!({
                    "action": "list",
                    "monitors": monitors,
                })))
            }
            "inspect" => {
                let Some(id) = params.id.as_deref() else {
                    return Ok(refusal(MonitorError::invalid("inspect needs an id")));
                };
                let guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                match guard.inspect(id) {
                    Ok(record) => Ok(ToolOutput::new(render_record(&record)).with_metadata(json!({
                        "action": "inspect",
                        "monitor": record_value(&record),
                    }))),
                    Err(error) => Ok(refusal(error)),
                }
            }
            "cancel" => {
                let Some(id) = params.id.as_deref() else {
                    return Ok(refusal(MonitorError::invalid("cancel needs an id")));
                };
                let mut guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                match guard.cancel(id, "cancelled by the agent") {
                    Ok(record) => Ok(ToolOutput::new(format!("cancelled monitor {}\n", record.id))
                        .with_metadata(json!({"action": "cancel", "monitor": record_value(&record)}))),
                    Err(error) => Ok(refusal(error)),
                }
            }
            "rearm" => {
                let Some(id) = params.id.as_deref() else {
                    return Ok(refusal(MonitorError::invalid("rearm needs an id")));
                };
                let (record, kind) = {
                    let mut guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    let current = match guard.inspect(id) {
                        Ok(record) => record,
                        Err(error) => return Ok(refusal(error)),
                    };
                    let kind = current.spec.kind;
                    match guard.rearm(id) {
                        Ok(record) => (record, kind),
                        Err(error) => return Ok(refusal(error)),
                    }
                };
                if kind != MonitorKind::Command {
                    return Ok(ToolOutput::new(format!("rearmed monitor {id}; it is sampling again\n"))
                        .with_metadata(json!({"action": "rearm", "monitor": record_value(&record)})));
                }
                let pid = {
                    let mut guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    match guard.start_command(id) {
                        Ok(pid) => pid,
                        Err(error) => return Ok(refusal(error)),
                    }
                };
                let record = {
                    let guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.inspect(id).unwrap_or(record)
                };
                Ok(ToolOutput::new(format!("rearmed monitor {id}; started pid {pid}\n"))
                    .with_metadata(json!({"action": "rearm", "pid": pid, "monitor": record_value(&record)})))
            }
            other => Ok(refusal(MonitorError::invalid(format!(
                "unknown monitor action {other:?}; use create, list, inspect, cancel, or rearm"
            )))),
        }
    }
}

async fn create(runtime: Arc<Mutex<MonitorRuntime>>, ctx: &ToolContext, params: MonitorInput) -> Result<ToolOutput> {
    let kind = match params.kind.as_deref().map(MonitorKind::parse) {
        Some(Some(kind)) => kind,
        Some(None) => {
            return Ok(refusal(MonitorError::invalid("kind must be `file` or `command`")));
        }
        None => return Ok(refusal(MonitorError::invalid("create needs a kind: file or command"))),
    };
    if let Some(policy) = params.restart_policy.as_deref()
        && RestartPolicy::parse(policy).is_none()
    {
        return Ok(refusal(MonitorError::invalid(
            "restart_policy may only be `never`; a monitor never restarts a command on its own",
        )));
    }

    let spec = match kind {
        MonitorKind::File => {
            let Some(path) = params.path.clone() else {
                return Ok(refusal(MonitorError::invalid("a file monitor needs a path")));
            };
            let resolved = ctx.resolve_path(Path::new(&path));
            MonitorSpec::file(resolved.to_string_lossy().to_string())
        }
        MonitorKind::Command => {
            let Some(command) = params.command.clone() else {
                return Ok(refusal(MonitorError::invalid("a command monitor needs a command")));
            };
            let mut spec = MonitorSpec::command(command);
            spec.cwd = params.cwd.clone();
            spec
        }
    };

    let created = {
        let mut guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.create(spec, params.intent.clone()) {
            Ok(record) => record,
            Err(error) => return Ok(refusal(error)),
        }
    };

    if kind == MonitorKind::Command {
        let started = {
            let mut guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.start_command(&created.id)
        };
        let pid = match started {
            Ok(pid) => pid,
            Err(error) => {
                let mut guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let _ = guard.cancel(&created.id, "the command could not start");
                return Ok(refusal(error));
            }
        };
        let record = {
            let guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.inspect(&created.id).unwrap_or(created)
        };
        return Ok(ToolOutput::new(format!(
            "created monitor {} watching command (pid {pid})\n\n{}\n",
            record.id,
            render_record(&record)
        ))
        .with_metadata(json!({"action": "create", "pid": pid, "monitor": record_value(&record)})));
    }

    let (record, observed) = {
        let mut guard = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let observed = guard.tick().map(|delivered| delivered.len()).unwrap_or(0);
        (guard.inspect(&created.id).unwrap_or(created), observed)
    };
    Ok(ToolOutput::new(format!(
        "created monitor {} watching {}\n\n{}\n",
        record.id,
        record.spec.target(),
        render_record(&record)
    ))
    .with_metadata(json!({
        "action": "create",
        "wakes": observed,
        "monitor": record_value(&record),
    })))
}

fn refusal(error: MonitorError) -> ToolOutput {
    ToolOutput::new(format!("monitor: {}\n", error.message())).with_metadata(json!({
        "error": error.kind().as_str(),
        "message": error.message(),
    }))
}

fn record_value(record: &MonitorRecord) -> Value {
    json!({
        "id": record.id,
        "kind": record.spec.kind.as_str(),
        "target": record.spec.target(),
        "status": record.status.as_str(),
        "pause": record.pause.map(|reason| reason.as_str()),
        "restart_policy": record.spec.restart_policy.as_str(),
        "changes": record.changes,
        "wakes": record.wakes,
        "coalesced": record.coalesced,
        "monitor_seq": record.monitor_seq,
        "last_summary": record.last_summary,
    })
}

fn render_record(record: &MonitorRecord) -> String {
    let mut lines = vec![
        format!("id: {}", record.id),
        format!("kind: {}", record.spec.kind.as_str()),
        format!("target: {}", record.spec.target()),
        format!("status: {}", record.status.as_str()),
        format!("restart_policy: {}", record.spec.restart_policy.as_str()),
        format!("changes: {}", record.changes),
        format!("wakes: {}", record.wakes),
    ];
    if let Some(reason) = record.pause {
        lines.push(format!("pause: {}", reason.as_str()));
    }
    if record.coalesced > 0 {
        lines.push(format!("coalesced: {}", record.coalesced));
    }
    if let Some(summary) = record.last_summary.as_deref() {
        lines.push(format!("last: {summary}"));
    }
    if record.status == MonitorStatus::NeedsRearm {
        lines.push("action needed: this command was interrupted by a restart and did not restart itself; call monitor with action=\"rearm\" to run it again".to_string());
    }
    lines.join("\n")
}

fn render_list(monitors: &[Value]) -> String {
    if monitors.is_empty() {
        return "no monitors in this session\n".to_string();
    }
    let mut lines = Vec::with_capacity(monitors.len());
    for monitor in monitors {
        lines.push(format!(
            "{} {} {} — {} ({} changes, {} wakes)",
            monitor["id"].as_str().unwrap_or_default(),
            monitor["kind"].as_str().unwrap_or_default(),
            monitor["target"].as_str().unwrap_or_default(),
            monitor["status"].as_str().unwrap_or_default(),
            monitor["changes"].as_u64().unwrap_or_default(),
            monitor["wakes"].as_u64().unwrap_or_default(),
        ));
    }
    lines.join("\n") + "\n"
}
