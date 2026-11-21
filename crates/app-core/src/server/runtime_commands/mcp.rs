use std::path::Path;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::mcp::{McpClient, McpConfig, McpServerConfig};

pub(in crate::server) async fn inspect(working_dir: Option<&Path>, arguments: &[String]) -> Result<(Value, String)> {
    // Use the daemon's normal workspace-trust path for each request. A server
    // removed or denied since the list was shown must not be started by a probe.
    let config = McpConfig::load_for_dir(working_dir);
    match arguments {
        [] => Ok(list(config)),
        [verb, name] if verb == "probe" => {
            let Some(server) = config.servers.get(name) else {
                bail!(
                    "Server is not in the trusted MCP configuration. Use /mcp to see available servers and check workspace trust."
                );
            };
            if !server.is_enabled() {
                bail!("This MCP server is disabled. Enable it in the trusted configuration before probing it.");
            }
            probe(name, server, working_dir).await
        }
        _ => bail!("Usage: /mcp or /mcp probe <server>"),
    }
}

fn list(config: McpConfig) -> (Value, String) {
    let mut servers: Vec<_> = config.servers.into_iter().collect();
    servers.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    let mut message = String::from("Trusted MCP servers");
    if servers.is_empty() {
        message.push_str("\nNo servers in the trusted configuration.");
    }
    let rows: Vec<_> = servers
        .into_iter()
        .map(|(name, server)| {
            let name = display_name(&name);
            let transport = transport_label(&server);
            let enabled = server.is_enabled();
            let status = if enabled { "enabled" } else { "disabled" };
            message.push('\n');
            message.push_str(&name);
            message.push_str(" · ");
            message.push_str(transport);
            message.push_str(" · ");
            message.push_str(status);
            json!({"name": name, "transport": transport, "enabled": enabled})
        })
        .collect();
    (json!({"servers": rows}), message)
}

async fn probe(name: &str, server: &McpServerConfig, working_dir: Option<&Path>) -> Result<(Value, String)> {
    let transport = transport_label(server);
    let result = McpClient::connect_in_dir(name.to_owned(), server, working_dir).await;
    let mut client = match result {
        Ok(client) => client,
        Err(error) => {
            // Connection errors can carry URLs or subprocess arguments. Keep
            // them off the client-facing surface, and name the local remedy.
            let guidance = if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
            {
                "Check that the configured executable exists."
            } else if server.is_stdio() {
                "Check that its executable is installed and its working directory is readable."
            } else {
                "Check its URL, credentials and reachability in the MCP configuration."
            };
            bail!("MCP connection over {transport} failed. {guidance}");
        }
    };
    let capabilities = client.handle().server_capabilities();
    let count = client.tools().len();
    client.shutdown().await;
    let name = display_name(name);
    let tools = capabilities.tools.is_some();
    let resources = capabilities.resources.is_some();
    let prompts = capabilities.prompts.is_some();
    let message = format!(
        "MCP probe connected: {name}\nTransport: {transport}\nTools: {tools} ({count} available)\nResources: {resources}\nPrompts: {prompts}"
    );
    let data = json!({
        "server": name,
        "transport": transport,
        "capabilities": {"tools": tools, "resources": resources, "prompts": prompts},
        "tool_count": count,
    });
    Ok((data, message))
}

fn transport_label(server: &McpServerConfig) -> &'static str {
    if server.is_stdio() {
        "stdio"
    } else {
        server
            .remote_transport()
            .map_or("unknown", |transport| transport.label())
    }
}

fn display_name(name: &str) -> String {
    name.chars().filter(|ch| !ch.is_control()).take(100).collect()
}
