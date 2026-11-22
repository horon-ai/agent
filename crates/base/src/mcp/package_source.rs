//! Package MCP servers as a session registers them.
//!
//! A package declares servers ([`crate::packages::PackageMcpServer`]), and this
//! module turns them into the configs a session connects, after the user's own
//! MCP sources were merged:
//!
//! - every name carries the `plugin_<id>_` prefix, and a name the user sources
//!   already define keeps the user's connection ([`sources::merge_package_lower`]
//!   has no stdio exception);
//! - a `stdio` entry becomes a config whose command is the package's installed
//!   executable, which the host starts through package supervision instead of
//!   `Command`;
//! - a `remote` entry becomes the user's own config for the server it names, so
//!   the package adds no endpoint and no credential. A reference the user does
//!   not define is skipped with a log line: the package cannot conjure a
//!   connection the user never configured;
//! - a package connection is never shared (`shared: false`), so revoking the
//!   package closes exactly its own connection and neither the user's server of
//!   the same target nor another package's connection.

use std::collections::HashMap;

use super::protocol::McpServerConfig;
use super::sources::{self, Servers};
use crate::packages::{PackageMcpKind, PackageMcpServer};

/// One package server as the session will register it.
#[derive(Debug, Clone)]
pub struct PackageServerRegistration {
    /// The declared server, including the executable or reference it names.
    pub server: PackageMcpServer,
    /// The config to connect, already merged with the user's sources.
    pub config: McpServerConfig,
}

/// The configurations of `servers`, merged after `merged`.
///
/// Only package entries are returned: a name the user sources already define is
/// never replaced by a package entry, and a remote reference the user does not
/// define yields nothing.
pub fn package_servers(
    merged: &HashMap<String, McpServerConfig>,
    servers: &[PackageMcpServer],
) -> Vec<PackageServerRegistration> {
    let mut candidates = Servers::new();
    for server in servers {
        if let Some(config) = config_for(server, merged) {
            candidates.insert(server.public_name.clone(), config);
        }
    }
    let mut after = merged.clone();
    sources::merge_package_lower(&mut after, candidates);
    servers
        .iter()
        .filter(|server| !merged.contains_key(&server.public_name))
        .filter_map(|server| {
            after.get(&server.public_name).map(|config| PackageServerRegistration {
                server: server.clone(),
                config: config.clone(),
            })
        })
        .collect()
}

/// The config one declared server is connected with, or `None` when the
/// declaration must not register.
fn config_for(server: &PackageMcpServer, merged: &HashMap<String, McpServerConfig>) -> Option<McpServerConfig> {
    match &server.kind {
        PackageMcpKind::Stdio {
            executable,
            mode,
            args,
            timeout_secs,
        } => {
            let mut arguments = Vec::with_capacity(args.len() + 1);
            arguments.push(mode.clone());
            arguments.extend(args.iter().cloned());
            Some(McpServerConfig {
                command: executable.to_string_lossy().into_owned(),
                args: arguments,
                env: HashMap::new(),
                // The package owns this child; the shared pool must never hand
                // it to another session or keep it past a revocation.
                shared: false,
                transport: Some("stdio".to_string()),
                url: None,
                headers: HashMap::new(),
                enabled: None,
                disabled: None,
                timeout_secs: *timeout_secs,
            })
        }
        PackageMcpKind::Remote { server: target } => match merged.get(target) {
            Some(config) => {
                let mut config = config.clone();
                config.shared = false;
                Some(config)
            }
            None => {
                crate::logging::info(&format!(
                    "MCP: Skipping package server '{}': it refers to '{}', which no user MCP source defines",
                    server.public_name, target
                ));
                None
            }
        },
    }
}
