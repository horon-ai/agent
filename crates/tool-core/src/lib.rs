use anyhow::Result;
use async_trait::async_trait;
use horon_agent_agent_runtime::InterruptSignal;
use horon_agent_message_types::ToolDefinition;
use horon_agent_tool_types::ToolOutput;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const TOOL_INTENT_DESCRIPTION: &str = "Required short label shown in the UI: why this call is being made.";

/// Input key a caller sets to accept the token cost of an oversized result.
///
/// The context guard withholds any tool result too large for the remaining
/// context and states its token cost. Setting this repeats the call and spends
/// that cost deliberately. Kept in sync with the registry constant of the same
/// name, which reads the flag off raw tool input.
pub const ACCEPT_LARGE_OUTPUT_KEY: &str = "accept_large_output";

/// Deliberately terse: this rides on every tool schema on every request, so
/// each word is paid forever. The full explanation lives in the refusal
/// message, which is only ever shown when it is actually relevant.
pub const ACCEPT_LARGE_OUTPUT_DESCRIPTION: &str = "Re-run accepting the stated token cost of a withheld result.";

pub fn intent_schema_property() -> Value {
    serde_json::json!({
        "type": "string",
        "description": TOOL_INTENT_DESCRIPTION,
    })
}

pub fn accept_large_output_schema_property() -> Value {
    serde_json::json!({
        "type": "boolean",
        "description": ACCEPT_LARGE_OUTPUT_DESCRIPTION,
    })
}

/// Ensure a tool parameter schema declares the shared `intent` property and
/// marks it required. Applied centrally when converting tools to provider
/// definitions so every tool (including MCP proxies) asks the model for an
/// intent without each tool wiring it manually.
///
/// The optional `accept_large_output` escape hatch is added the same way. Any
/// tool can produce a result too large to return, so documenting it per tool
/// would mean editing dozens of schemas and missing MCP proxies entirely.
pub fn ensure_intent_in_schema(mut schema: Value) -> Value {
    let Some(object) = schema.as_object_mut() else {
        return schema;
    };
    // Only touch object-shaped parameter schemas.
    let is_object_schema = object
        .get("type")
        .and_then(|t| t.as_str())
        .map(|t| t == "object")
        .unwrap_or_else(|| object.contains_key("properties"));
    if !is_object_schema {
        return schema;
    }

    let properties = object
        .entry("properties")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Some(properties) = properties.as_object_mut() {
        properties.entry("intent").or_insert_with(intent_schema_property);
        // Optional, so it is deliberately not added to `required`.
        properties
            .entry(ACCEPT_LARGE_OUTPUT_KEY)
            .or_insert_with(accept_large_output_schema_property);
    } else {
        return schema;
    }

    match object.get_mut("required") {
        Some(Value::Array(required)) => {
            if !required.iter().any(|v| v.as_str() == Some("intent")) {
                required.push(Value::String("intent".to_string()));
            }
        }
        _ => {
            object.insert(
                "required".to_string(),
                Value::Array(vec![Value::String("intent".to_string())]),
            );
        }
    }

    schema
}

/// A request for stdin input from a running command.
pub struct StdinInputRequest {
    pub request_id: String,
    pub prompt: String,
    pub is_password: bool,
    pub response_tx: tokio::sync::oneshot::Sender<String>,
}

#[derive(Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub message_id: String,
    pub tool_call_id: String,
    pub working_dir: Option<PathBuf>,
    pub stdin_request_tx: Option<tokio::sync::mpsc::UnboundedSender<StdinInputRequest>>,
    pub graceful_shutdown_signal: Option<InterruptSignal>,
    pub execution_mode: ToolExecutionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionMode {
    AgentTurn,
    Direct,
}

impl ToolContext {
    pub fn for_subcall(&self, tool_call_id: String) -> Self {
        Self {
            session_id: self.session_id.clone(),
            message_id: self.message_id.clone(),
            // Child labels such as `batch-1-compile_remote` repeat across
            // batches. Preserve the parent invocation identity so idempotent
            // services distinguish new jobs while retries keep the same key.
            // Length-prefix the parent to make the composition unambiguous.
            tool_call_id: format!("{}:{}:{}", self.tool_call_id.len(), self.tool_call_id, tool_call_id),
            working_dir: self.working_dir.clone(),
            stdin_request_tx: self.stdin_request_tx.clone(),
            graceful_shutdown_signal: self.graceful_shutdown_signal.clone(),
            execution_mode: self.execution_mode,
        }
    }

    pub fn resolve_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else if let Some(ref base) = self.working_dir {
            base.join(path)
        } else {
            path.to_path_buf()
        }
    }
}

/// Whether a tool may run beside other tools in the same round.
///
/// The two values are not symmetric in cost. A wrong `Concurrent` can lose user
/// data: two overlapping writes to one file, or two shells in one working tree,
/// interleave their changes and the last writer silently wins. A wrong `Ordered`
/// only costs speed, because the call still runs, just alone. Opt in to
/// `Concurrent` only for a tool that reads state and never mutates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrencyClass {
    /// Must not overlap with any other call in the round.
    Ordered,
    /// Read-only, so it may run beside other `Concurrent` calls.
    Concurrent,
}

/// What one tool call may touch, as a bitmask.
///
/// The mask is the single source of truth behind [`ConcurrencyClass`]: a call
/// whose effects carry no [`ToolEffects::BARRIER`] bit may overlap other calls
/// in the same round. `NETWORK` is deliberately outside the barrier because two
/// fetches do not interfere with each other; the number of paid calls is a
/// budget question, not a safety one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolEffects(u8);

impl ToolEffects {
    pub const READ: u8 = 1 << 0;
    pub const WRITE: u8 = 1 << 1;
    pub const APPEND: u8 = 1 << 2;
    pub const NETWORK: u8 = 1 << 3;
    pub const PROCESS: u8 = 1 << 4;

    /// Bits that must never overlap another call in the round.
    pub const BARRIER: u8 = Self::WRITE | Self::APPEND | Self::PROCESS;

    pub const fn read() -> Self {
        Self(Self::READ)
    }

    pub const fn write() -> Self {
        Self(Self::WRITE)
    }

    pub const fn append() -> Self {
        Self(Self::APPEND)
    }

    pub const fn network() -> Self {
        Self(Self::NETWORK)
    }

    pub const fn process() -> Self {
        Self(Self::PROCESS)
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn reads(self) -> bool {
        self.0 & Self::READ != 0
    }

    pub const fn writes(self) -> bool {
        self.0 & Self::WRITE != 0
    }

    pub const fn appends(self) -> bool {
        self.0 & Self::APPEND != 0
    }

    pub const fn networks(self) -> bool {
        self.0 & Self::NETWORK != 0
    }

    pub const fn processes(self) -> bool {
        self.0 & Self::PROCESS != 0
    }

    /// True when the call may run beside another one: read and/or network only.
    pub const fn parallel_safe(self) -> bool {
        self.0 & Self::BARRIER == 0
    }

    /// True when this call and `other` may share one round group.
    pub const fn compatible_with(self, other: Self) -> bool {
        self.union(other).parallel_safe()
    }
}

/// A tool that can be executed by the agent.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (must match what's sent to the API).
    fn name(&self) -> &str;

    /// Original MCP identity, independent of the provider-facing registry alias.
    fn mcp_identity(&self) -> Option<(&str, &str)> {
        None
    }

    /// Human-readable description.
    fn description(&self) -> &str;

    /// JSON Schema for the input parameters.
    fn parameters_schema(&self) -> Value;

    /// What this tool may touch.
    ///
    /// Defaults to a write barrier, so a tool that declares nothing is never
    /// treated as safe to overlap. A read default would silently promote every
    /// undeclared tool to `Concurrent`.
    fn effects(&self) -> ToolEffects {
        ToolEffects::write()
    }

    /// Effects of one concrete call, for a tool whose reach depends on its input.
    fn effects_for_input(&self, _input: &Value) -> ToolEffects {
        self.effects()
    }

    /// Whether this tool may run beside other calls in the same round.
    ///
    /// Derived from [`Tool::effects`]. A wrong `Concurrent` can lose user data:
    /// two overlapping writes to one file, or two shells in one working tree,
    /// interleave their changes and the last writer silently wins. A wrong
    /// `Ordered` only costs speed, because the call still runs, just alone.
    fn concurrency_class(&self) -> ConcurrencyClass {
        if self.effects().parallel_safe() {
            ConcurrencyClass::Concurrent
        } else {
            ConcurrencyClass::Ordered
        }
    }

    /// Execute the tool with the given input.
    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput>;

    /// Convert to API tool definition.
    fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: ensure_intent_in_schema(self.parameters_schema()),
            defer_loading: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BareTool;

    #[async_trait]
    impl Tool for BareTool {
        fn name(&self) -> &str {
            "bare"
        }

        fn description(&self) -> &str {
            "Declares only what the trait requires"
        }

        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
            Ok(ToolOutput::new("ok"))
        }
    }

    #[test]
    fn tool_defaults_to_ordered_concurrency() {
        assert_eq!(BareTool.concurrency_class(), ConcurrencyClass::Ordered);
    }

    #[test]
    fn effects_bitmask_predicates_are_independent() {
        let all = ToolEffects::read()
            .union(ToolEffects::write())
            .union(ToolEffects::append())
            .union(ToolEffects::network())
            .union(ToolEffects::process());
        assert!(all.reads());
        assert!(all.writes());
        assert!(all.appends());
        assert!(all.networks());
        assert!(all.processes());

        assert_eq!(
            ToolEffects::read().union(ToolEffects::network()),
            ToolEffects::network().union(ToolEffects::read()),
            "union is order independent"
        );
        assert!(!ToolEffects::read().writes());
        assert!(!ToolEffects::write().reads());
        assert!(!ToolEffects::append().writes());
        assert!(!ToolEffects::process().networks());
    }

    #[test]
    fn read_effects_are_parallel_safe_and_write_is_not() {
        assert!(ToolEffects::read().parallel_safe());
        assert!(ToolEffects::network().parallel_safe());
        assert!(ToolEffects::read().union(ToolEffects::network()).parallel_safe());
        assert!(!ToolEffects::write().parallel_safe());
        assert!(!ToolEffects::append().parallel_safe());
        assert!(!ToolEffects::process().parallel_safe());
        assert!(!ToolEffects::read().union(ToolEffects::write()).parallel_safe());

        assert!(ToolEffects::read().compatible_with(ToolEffects::read()));
        assert!(!ToolEffects::read().compatible_with(ToolEffects::write()));
        assert!(!ToolEffects::write().compatible_with(ToolEffects::read()));
    }

    #[test]
    fn default_effects_are_a_barrier() {
        let effects = BareTool.effects();
        assert!(effects.writes(), "an undeclared tool must stay a barrier");
        assert!(!effects.parallel_safe());
        assert_eq!(BareTool.effects_for_input(&serde_json::json!({})), effects);
    }

    #[test]
    fn concurrency_class_derives_from_effects() {
        struct ReadingTool;
        struct WritingTool;
        struct NetworkingTool;

        #[async_trait]
        impl Tool for ReadingTool {
            fn name(&self) -> &str {
                "reading"
            }

            fn description(&self) -> &str {
                "Reads only"
            }

            fn parameters_schema(&self) -> Value {
                serde_json::json!({"type": "object"})
            }

            fn effects(&self) -> ToolEffects {
                ToolEffects::read()
            }

            async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
                Ok(ToolOutput::new("ok"))
            }
        }

        #[async_trait]
        impl Tool for WritingTool {
            fn name(&self) -> &str {
                "writing"
            }

            fn description(&self) -> &str {
                "Writes only"
            }

            fn parameters_schema(&self) -> Value {
                serde_json::json!({"type": "object"})
            }

            fn effects(&self) -> ToolEffects {
                ToolEffects::write()
            }

            async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
                Ok(ToolOutput::new("ok"))
            }
        }

        #[async_trait]
        impl Tool for NetworkingTool {
            fn name(&self) -> &str {
                "networking"
            }

            fn description(&self) -> &str {
                "Reads and reaches the network"
            }

            fn parameters_schema(&self) -> Value {
                serde_json::json!({"type": "object"})
            }

            fn effects(&self) -> ToolEffects {
                ToolEffects::read().union(ToolEffects::network())
            }

            async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
                Ok(ToolOutput::new("ok"))
            }
        }

        assert_eq!(ReadingTool.concurrency_class(), ConcurrencyClass::Concurrent);
        assert_eq!(NetworkingTool.concurrency_class(), ConcurrencyClass::Concurrent);
        assert_eq!(WritingTool.concurrency_class(), ConcurrencyClass::Ordered);
    }

    #[test]
    fn effects_for_input_can_widen_a_read_to_network() {
        struct LocalOrRemoteRead;

        #[async_trait]
        impl Tool for LocalOrRemoteRead {
            fn name(&self) -> &str {
                "local_or_remote_read"
            }

            fn description(&self) -> &str {
                "Reads a file, or a URL when the path carries a scheme"
            }

            fn parameters_schema(&self) -> Value {
                serde_json::json!({"type": "object"})
            }

            fn effects(&self) -> ToolEffects {
                ToolEffects::read()
            }

            fn effects_for_input(&self, input: &Value) -> ToolEffects {
                let remote = input
                    .get("path")
                    .and_then(|value| value.as_str())
                    .is_some_and(|path| path.contains("://"));
                if remote {
                    ToolEffects::read().union(ToolEffects::network())
                } else {
                    ToolEffects::read()
                }
            }

            async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
                Ok(ToolOutput::new("ok"))
            }
        }

        let local = LocalOrRemoteRead.effects_for_input(&serde_json::json!({"path": "src/lib.rs"}));
        let remote = LocalOrRemoteRead.effects_for_input(&serde_json::json!({"path": "agent://child-1"}));
        assert_eq!(local, ToolEffects::read());
        assert!(!local.networks());
        assert!(remote.reads());
        assert!(remote.networks());
        assert!(remote.parallel_safe(), "a network read may still overlap");
    }

    #[test]
    fn subcall_ids_are_parent_scoped_and_retry_stable() {
        let mut context = ToolContext {
            session_id: "same-session".into(),
            message_id: "same-message".into(),
            tool_call_id: "first-batch".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::AgentTurn,
        };
        let label = "batch-1-compile_remote".to_string();
        let first = context.for_subcall(label.clone());
        assert_eq!(first.tool_call_id, context.for_subcall(label.clone()).tool_call_id);
        assert_ne!(
            first.tool_call_id,
            context.for_subcall("batch-2-compile_remote".into()).tool_call_id
        );
        context.tool_call_id = "second-batch".into();
        let second = context.for_subcall(label);
        assert_ne!(first.tool_call_id, second.tool_call_id);
        assert_eq!(first.session_id, second.session_id);
        assert_eq!(first.message_id, second.message_id);
    }

    #[test]
    fn ensure_intent_adds_property_and_required() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {"type": "string"}
            }
        });
        let out = ensure_intent_in_schema(schema);
        assert!(out["properties"]["intent"].is_object());
        let required: Vec<_> = out["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"command"));
        assert!(required.contains(&"intent"));
    }

    #[test]
    fn ensure_intent_creates_required_array_when_missing() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {}
        });
        let out = ensure_intent_in_schema(schema);
        assert_eq!(out["required"], serde_json::json!(["intent"]));
    }

    #[test]
    fn ensure_intent_preserves_existing_intent_property() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["intent"],
            "properties": {
                "intent": {"type": "string", "description": "custom"}
            }
        });
        let out = ensure_intent_in_schema(schema);
        assert_eq!(out["properties"]["intent"]["description"], "custom");
        assert_eq!(
            out["required"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v.as_str() == Some("intent"))
                .count(),
            1
        );
    }

    #[test]
    fn ensure_intent_skips_non_object_schemas() {
        let schema = serde_json::json!({"type": "string"});
        let out = ensure_intent_in_schema(schema.clone());
        assert_eq!(out, schema);
    }
}

#[cfg(test)]
mod escape_hatch_tests {
    use super::*;

    #[test]
    fn injects_the_escape_hatch_into_any_object_schema() {
        // MCP tools are built from remote definitions and never edit their own
        // schemas, so they can only advertise the flag if injection is central.
        // A schema shaped like an MCP proxy's proves the mechanism.
        let mcp_shaped = serde_json::json!({
            "type": "object",
            "required": ["path"],
            "properties": { "path": { "type": "string" } }
        });
        let out = ensure_intent_in_schema(mcp_shaped);
        assert_eq!(
            out["properties"][ACCEPT_LARGE_OUTPUT_KEY]["type"], "boolean",
            "every object schema must advertise the escape hatch"
        );
        // Optional by design: requiring it would make the model answer a token
        // budget question on every call.
        let required: Vec<&str> = out["required"]
            .as_array()
            .expect("required array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"intent"));
        assert!(!required.contains(&ACCEPT_LARGE_OUTPUT_KEY));
    }

    #[test]
    fn never_overwrites_a_schema_that_declares_the_flag_itself() {
        let custom = serde_json::json!({
            "type": "object",
            "properties": {
                ACCEPT_LARGE_OUTPUT_KEY: { "type": "boolean", "description": "custom" }
            }
        });
        let out = ensure_intent_in_schema(custom);
        assert_eq!(
            out["properties"][ACCEPT_LARGE_OUTPUT_KEY]["description"], "custom",
            "a tool's own declaration must survive injection"
        );
    }

    #[test]
    fn the_schema_key_matches_what_the_guard_reads() {
        // The registry reads this exact constant off raw tool input. If the two
        // ever diverge, the flag would be advertised but never honored, which is
        // worse than not offering it at all.
        assert_eq!(ACCEPT_LARGE_OUTPUT_KEY, "accept_large_output");
    }
}
