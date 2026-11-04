//! `aux_complete`: one tool-free auxiliary completion, behind `[tools] aux_completion`.
//!
//! The tool is a thin adapter. Everything that decides what a caller may ask
//! for - the field set, the size caps, the schema subset, the dispatch through
//! the session's own sidecar - lives in
//! [`horon_agent_base::aux_completion`]. This module only turns the model's
//! arguments into a request through that contract and hands back the text, so
//! the caps and the schema check cannot drift between the tool surface and any
//! other caller.

use super::{Tool, ToolContext, ToolEffects, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_base::aux_completion::{CompletionRequest, complete};
use serde::Deserialize;
use serde_json::{Value, json};

pub const AUX_COMPLETE_TOOL: &str = "aux_complete";

pub struct AuxCompleteTool;

impl AuxCompleteTool {
    pub fn new() -> Self {
        Self
    }
}

pub(crate) fn enabled() -> bool {
    crate::config::config().tools.aux_completion
}

#[derive(Deserialize)]
struct AuxCompleteInput {
    prompt: String,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    schema: Option<Value>,
}

impl AuxCompleteInput {
    fn into_request(self) -> Result<CompletionRequest> {
        let mut request = CompletionRequest::new(self.prompt)?;
        if let Some(system) = self.system {
            request = request.with_system(system)?;
        }
        if let Some(max_tokens) = self.max_tokens {
            request = request.with_max_tokens(max_tokens)?;
        }
        if let Some(schema) = self.schema {
            request = request.with_schema(schema)?;
        }
        Ok(request)
    }
}

#[async_trait]
impl Tool for AuxCompleteTool {
    fn name(&self) -> &str {
        AUX_COMPLETE_TOOL
    }

    fn description(&self) -> &str {
        "Run one small side completion that never calls tools and never changes which model is used."
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::network()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "intent": super::intent_schema_property(),
                "prompt": {
                    "type": "string",
                    "description": "The text to complete. At most 64 KiB."
                },
                "system": {
                    "type": "string",
                    "description": "Optional system text. At most 16 KiB. Defaults to a text-only instruction."
                },
                "max_tokens": {
                    "type": "integer",
                    "description": "Optional response bound in tokens, 1 to 4096."
                },
                "schema": {
                    "type": "object",
                    "description": "Optional JSON Schema the response must satisfy. Supported keywords: type, required, properties, items, enum."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let request: AuxCompleteInput = serde_json::from_value(input)?;
        let text = complete(request.into_request()?).await?;
        Ok(ToolOutput::new(text))
    }
}
