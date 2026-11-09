//! Resource and prompt actions of the `mcp` tool.
//!
//! Bodies follow the same path as tool results: one token budget unless the
//! caller opted into large output, and the redaction pass inside the base
//! renderers. Nothing here injects a prompt into the conversation: the text is
//! returned to the caller like any other tool output.

use super::{McpManagementTool, McpToolInput};
use crate::mcp::{
    McpManager, McpPromptDef, McpResourceDef, ResourceContent, render_prompt_result, render_resource_body,
    unsupported_capability_message,
};
use crate::tool::ToolOutput;
use anyhow::Result;

/// Shown when a listing was asked for without a server and none is connected.
const NO_CONNECTED_SERVERS: &str =
    "No MCP servers are connected. Use action=connect with a configured server name first.";

impl McpManagementTool {
    /// `resources`: list the resources of one server, or of every connected one.
    pub(super) async fn resources_action(&self, params: McpToolInput) -> Result<ToolOutput> {
        let manager = self.manager.read().await;
        let servers = listing_targets(&manager, params.server.as_deref()).await;
        if servers.is_empty() {
            return Ok(ToolOutput::new(NO_CONNECTED_SERVERS).with_title("MCP: Resources"));
        }
        let mut out = String::new();
        for server in servers {
            let handle = manager.server_handle(&server).await?;
            out.push_str(&format!("## {server}\n"));
            if !handle.supports_resources() {
                out.push_str(&format!(
                    "  {}\n\n",
                    unsupported_capability_message("resources", &server)
                ));
                continue;
            }
            let resources = handle.list_resources().await?;
            if resources.is_empty() {
                out.push_str("  (no resources)\n\n");
                continue;
            }
            for resource in &resources {
                out.push_str(&format!("  - {}\n", describe_resource(resource)));
            }
            out.push('\n');
        }
        Ok(ToolOutput::new(out.trim_end().to_string()).with_title("MCP: Resources"))
    }

    /// `read_resource`: fetch one resource body by URI.
    pub(super) async fn read_resource_action(
        &self,
        params: McpToolInput,
        accept_large_output: bool,
    ) -> Result<ToolOutput> {
        let server = params
            .server
            .clone()
            .ok_or_else(|| anyhow::anyhow!("'server' is required for action=read_resource"))?;
        let uri = params
            .uri
            .clone()
            .ok_or_else(|| anyhow::anyhow!("'uri' is required for action=read_resource"))?;
        let manager = self.manager.read().await;
        let handle = manager.server_handle(&server).await?;
        if !handle.supports_resources() {
            return Ok(ToolOutput::new(unsupported_capability_message("resources", &server))
                .with_title(format!("MCP: {server}")));
        }
        let result = handle.read_resource(&uri).await?;
        let body = render_contents(&result.contents);
        Ok(ToolOutput::new(fitted(body, accept_large_output)).with_title(format!("mcp:{server}:{uri}")))
    }

    /// `prompts`: list the prompts of one server, or of every connected one.
    pub(super) async fn prompts_action(&self, params: McpToolInput) -> Result<ToolOutput> {
        let manager = self.manager.read().await;
        let servers = listing_targets(&manager, params.server.as_deref()).await;
        if servers.is_empty() {
            return Ok(ToolOutput::new(NO_CONNECTED_SERVERS).with_title("MCP: Prompts"));
        }
        let mut out = String::new();
        for server in servers {
            let handle = manager.server_handle(&server).await?;
            out.push_str(&format!("## {server}\n"));
            if !handle.supports_prompts() {
                out.push_str(&format!("  {}\n\n", unsupported_capability_message("prompts", &server)));
                continue;
            }
            let prompts = handle.list_prompts().await?;
            if prompts.is_empty() {
                out.push_str("  (no prompts)\n\n");
                continue;
            }
            for prompt in &prompts {
                out.push_str(&format!("  - {}\n", describe_prompt(prompt)));
            }
            out.push('\n');
        }
        Ok(ToolOutput::new(out.trim_end().to_string()).with_title("MCP: Prompts"))
    }

    /// `get_prompt`: expand one prompt on the server and return its text.
    pub(super) async fn get_prompt_action(
        &self,
        params: McpToolInput,
        accept_large_output: bool,
    ) -> Result<ToolOutput> {
        let server = params
            .server
            .clone()
            .ok_or_else(|| anyhow::anyhow!("'server' is required for action=get_prompt"))?;
        let name = params
            .name
            .clone()
            .ok_or_else(|| anyhow::anyhow!("'name' is required for action=get_prompt"))?;
        let manager = self.manager.read().await;
        let handle = manager.server_handle(&server).await?;
        if !handle.supports_prompts() {
            return Ok(ToolOutput::new(unsupported_capability_message("prompts", &server))
                .with_title(format!("MCP: {server}")));
        }
        let result = handle.get_prompt(&name, params.arguments.clone()).await?;
        let text = render_prompt_result(&result);
        let text = if text.is_empty() {
            "The server returned an empty prompt.".to_string()
        } else {
            text
        };
        Ok(ToolOutput::new(fitted(text, accept_large_output)).with_title(format!("mcp:{server}:{name}")))
    }
}

/// Servers a listing covers: the named one, or every connected one in a stable
/// order so repeated calls are comparable.
async fn listing_targets(manager: &McpManager, server: Option<&str>) -> Vec<String> {
    match server {
        Some(server) => vec![server.to_string()],
        None => {
            let mut connected = manager.connected_servers().await;
            connected.sort();
            connected
        }
    }
}

/// One line for a listed resource: URI, name, MIME type, description.
fn describe_resource(resource: &McpResourceDef) -> String {
    let mut line = resource.uri.clone();
    if !resource.name.is_empty() {
        line.push_str(&format!(" ({})", resource.name));
    }
    if let Some(mime) = &resource.mime_type {
        line.push_str(&format!(" [{mime}]"));
    }
    if let Some(description) = &resource.description {
        line.push_str(&format!(": {description}"));
    }
    line
}

/// One line for a listed prompt; a required argument carries a trailing `*`.
fn describe_prompt(prompt: &McpPromptDef) -> String {
    let mut line = prompt.name.clone();
    if !prompt.arguments.is_empty() {
        let arguments: Vec<String> = prompt
            .arguments
            .iter()
            .map(|argument| {
                if argument.required {
                    format!("{}*", argument.name)
                } else {
                    argument.name.clone()
                }
            })
            .collect();
        line.push_str(&format!("({})", arguments.join(", ")));
    }
    if let Some(description) = &prompt.description {
        line.push_str(&format!(": {description}"));
    }
    line
}

/// Render every body of one `resources/read` result.
fn render_contents(contents: &[ResourceContent]) -> String {
    if contents.is_empty() {
        return "The server returned no content for this resource.".to_string();
    }
    contents.iter().map(render_resource_body).collect::<Vec<_>>().join("\n")
}

/// Apply the tool-result token budget unless the caller opted into large output.
fn fitted(text: String, accept_large_output: bool) -> String {
    if accept_large_output {
        return text;
    }
    crate::mcp::budget::fit_to_budget(&text, crate::mcp::budget::MCP_RESULT_TOKEN_BUDGET)
}
