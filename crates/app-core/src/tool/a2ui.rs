//! Capability-gated `a2ui` response tool for A2UI v1.0 UI emission.
use super::{Tool, ToolContext, ToolEffects, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_tool_types::a2ui::{
    A2uiSessionCapabilities, summarize_a2ui_for_model, validate_agent_to_renderer_envelopes,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Built-in tool name registered only for sessions whose client negotiated A2UI.
pub(crate) const A2UI_TOOL: &str = "a2ui";
/// Key inside `ToolOutput.metadata` carrying validated A2UI envelopes.
pub(crate) const A2UI_METADATA_KEY: &str = "a2ui";

pub(crate) struct A2uiTool {
    description: String,
    capabilities: A2uiSessionCapabilities,
}

impl A2uiTool {
    pub(crate) fn new(capabilities: A2uiSessionCapabilities) -> Self {
        let catalogs = capabilities.effective_catalog_ids().join(", ");
        let version = if capabilities.version.trim().is_empty() {
            "1.0"
        } else {
            capabilities.version.trim()
        };
        let description = format!(
            "Respond with interactive A2UI v{version} UI envelopes (createSurface, updateComponents, \
             updateDataModel, deleteSurface, callRendererFunction, agentFunctionResponse). \
             Every envelope must carry \"version\": \"{version}\" and use an allowed catalogId ({catalogs}). \
             Provide a concise fallback_text for non-UI readers. Keep final_response true (default) when this UI \
             completes your answer and do not duplicate it in plain prose."
        );
        Self {
            description,
            capabilities,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    envelopes: Vec<Value>,
    fallback_text: Option<String>,
    final_response: Option<bool>,
    #[serde(rename = "intent")]
    _intent: Option<String>,
    #[serde(rename = "accept_large_output")]
    _accept_large_output: Option<bool>,
}

#[async_trait]
impl Tool for A2uiTool {
    fn name(&self) -> &str {
        A2UI_TOOL
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::read()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "envelopes": {
                    "type": "array",
                    "minItems": 1,
                    "description": "Ordered array of A2UI v1.0 envelopes (each with version and one action key).",
                    "items": { "type": "object" }
                },
                "fallback_text": {
                    "type": "string",
                    "description": "Concise plain-text summary for text-only logs and non-UI history views."
                },
                "final_response": {
                    "type": "boolean",
                    "description": "Set false only when additional tool calls must run after emitting this UI (default true)."
                },
                "intent": super::intent_schema_property()
            },
            "required": ["envelopes"]
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let input: Input = serde_json::from_value(input)?;
        let validated = validate_agent_to_renderer_envelopes(&input.envelopes, &self.capabilities)?;
        let final_response = input.final_response.unwrap_or(true);
        let summary = summarize_a2ui_for_model(&validated, input.fallback_text.as_deref());
        Ok(ToolOutput::new(summary)
            .with_title("A2UI response")
            .with_metadata(json!({
                A2UI_METADATA_KEY: {
                    "envelopes": validated,
                    "fallback_text": input.fallback_text,
                    "final_response": final_response,
                }
            })))
    }
}

impl super::Registry {
    /// Register or remove the capability-gated `a2ui` tool for this session's registry.
    pub(crate) async fn set_a2ui_capabilities(&self, capabilities: Option<A2uiSessionCapabilities>) {
        if let Some(caps) = capabilities {
            self.register(A2UI_TOOL.into(), Arc::new(A2uiTool::new(caps))).await;
        } else {
            self.unregister(A2UI_TOOL).await;
        }
    }

    /// Fetch the currently registered `a2ui` definition when A2UI is enabled.
    pub(crate) async fn a2ui_tool_definition(&self) -> Option<horon_agent_message_types::ToolDefinition> {
        let tools = self.tools.read().await;
        tools.get(A2UI_TOOL).map(|tool| tool.to_definition())
    }
}
