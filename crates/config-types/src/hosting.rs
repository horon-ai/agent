//! Hosting mode, tenant identity and the policy a hosted daemon runs under.
//!
//! A process either serves a single operator as it always has, owns one
//! customer inside its own container, or runs as a shared worker. The
//! environment is the only authority for that choice: files inside a tenant's
//! volume are writable by that tenant, so a `[hosting]` section in
//! `config.toml` may only narrow the policy and never widen it.
//!
//! The types here are pure: parsing, validation and derivation. Reading the
//! environment, inspecting the filesystem and caching the resolved policy for
//! the process belong to the application layer (`horon_agent_base::hosting`).

use serde::{Deserialize, Serialize};

/// The hosting modes an operator can select.
///
/// `None` is the ordinary single-operator process. `PerUserContainer` is one
/// customer per container with a dedicated home volume. `SharedWorker` is a
/// single process serving several customers, which the MCP and child-process
/// gate treats as untrusted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostingMode {
    /// No hosting: the process serves the operator's own home.
    #[default]
    None,
    /// One customer per container with a dedicated `HORON_HOME` volume.
    PerUserContainer,
    /// One process shared by several customers; no child processes.
    SharedWorker,
}

impl HostingMode {
    /// The environment value that selects this mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::PerUserContainer => "per-user-container",
            Self::SharedWorker => "shared-worker",
        }
    }

    /// The values an operator may write, in the order the error names them.
    pub const fn selectable() -> [Self; 2] {
        [Self::PerUserContainer, Self::SharedWorker]
    }

    /// Parse an environment value. `None` is not spellable: an absent variable
    /// selects it, and writing `none` is a mistake this reports.
    pub fn parse(raw: &str) -> Result<Self, HostingConfigError> {
        match raw.trim() {
            "per-user-container" => Ok(Self::PerUserContainer),
            "shared-worker" => Ok(Self::SharedWorker),
            other => Err(HostingConfigError::UnknownMode(other.to_string())),
        }
    }

    /// Whether this mode serves anyone other than the operator.
    pub const fn is_hosted(self) -> bool {
        !matches!(self, Self::None)
    }
}

impl std::fmt::Display for HostingMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Maximum length of a tenant id, in bytes.
pub const MAX_TENANT_ID_LEN: usize = 63;

/// A tenant identifier used as a directory name and an audit label.
///
/// The charset is deliberately narrower than a filesystem allows: the id names
/// a directory under the volume root, so `.`, `..`, path separators, absolute
/// paths and non-ASCII look-alikes are all refused before any path is joined.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TenantId(String);

impl TenantId {
    /// Validate a raw tenant id.
    pub fn parse(raw: &str) -> Result<Self, HostingConfigError> {
        let invalid = |reason: &'static str| HostingConfigError::InvalidTenantId {
            value: raw.to_string(),
            reason,
        };
        if raw.is_empty() {
            return Err(invalid("it is empty"));
        }
        if raw.len() > MAX_TENANT_ID_LEN {
            return Err(invalid("it is longer than 63 bytes"));
        }
        if raw.starts_with('-') || raw.ends_with('-') {
            return Err(invalid("it must start and end with a letter or digit"));
        }
        if !raw
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(invalid("only lowercase ASCII letters, digits and hyphens are allowed"));
        }
        Ok(Self(raw.to_string()))
    }

    /// The validated id.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for TenantId {
    type Error = HostingConfigError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<TenantId> for String {
    fn from(value: TenantId) -> Self {
        value.0
    }
}

/// The `[hosting]` section of `config.toml`.
///
/// Only narrowing is honoured. `mode` is captured so the reader can warn that
/// it is ignored: the environment is the sole authority for the mode, because a
/// tenant can write its own `config.toml` and must not be able to leave its
/// container by editing it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostingSection {
    /// `false` refuses shared children even in a mode that would allow them.
    /// `true` never widens a mode that forbids them.
    pub allow_shared_children: Option<bool>,
    /// Names of MCP servers this process may run. Absent leaves every server
    /// allowed; an empty list refuses every server. Exact match, narrowing only.
    pub allowed_mcp_servers: Option<Vec<String>>,
    /// Ignored, with one warning. Present so the reader can name the mistake.
    pub mode: Option<String>,
}

/// A hosting mode together with the tenant it serves and the narrowed policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostingPolicy {
    mode: HostingMode,
    tenant_id: Option<TenantId>,
    shared_children_allowed: bool,
    allowed_mcp_servers: Option<Vec<String>>,
}

impl Default for HostingPolicy {
    fn default() -> Self {
        Self {
            mode: HostingMode::None,
            tenant_id: None,
            shared_children_allowed: true,
            allowed_mcp_servers: None,
        }
    }
}

impl HostingPolicy {
    /// Combine a mode, the tenant from the environment, and the config section.
    pub fn new(
        mode: HostingMode,
        tenant_id: Option<TenantId>,
        section: &HostingSection,
    ) -> Result<Self, HostingConfigError> {
        if mode.is_hosted() && tenant_id.is_none() {
            return Err(HostingConfigError::MissingTenantId(mode));
        }
        // A tenant identifier is an audit label for a hosted process; outside
        // hosting it is not part of the process identity at all.
        let tenant_id = if mode.is_hosted() { tenant_id } else { None };
        let shared_children_allowed = match mode {
            HostingMode::PerUserContainer => section.allow_shared_children.unwrap_or(true),
            // A shared worker never runs a child process for a customer,
            // whatever the file says; the gate reads this value.
            HostingMode::SharedWorker | HostingMode::None => false,
        };
        // Normalized once so a blank entry cannot match a real server and an
        // empty list keeps its meaning: refuse every server.
        let allowed_mcp_servers = section.allowed_mcp_servers.as_ref().map(|servers| {
            let mut servers: Vec<String> = servers
                .iter()
                .map(|server| server.trim().to_string())
                .filter(|server| !server.is_empty())
                .collect();
            servers.sort_unstable();
            servers.dedup();
            servers
        });
        Ok(Self {
            mode,
            tenant_id,
            shared_children_allowed,
            allowed_mcp_servers,
        })
    }

    /// The selected mode.
    pub fn mode(&self) -> HostingMode {
        self.mode
    }

    /// The validated tenant id, when this process is hosted.
    pub fn tenant_id(&self) -> Option<&TenantId> {
        self.tenant_id.as_ref()
    }

    /// Whether this process serves a hosted customer at all.
    pub fn is_hosted(&self) -> bool {
        self.mode.is_hosted()
    }

    /// Whether this process owns exactly one customer's container.
    pub fn is_per_user_container(&self) -> bool {
        matches!(self.mode, HostingMode::PerUserContainer)
    }

    /// Whether this process is shared between customers.
    pub fn is_shared_worker(&self) -> bool {
        matches!(self.mode, HostingMode::SharedWorker)
    }

    /// Whether MCP servers and extensions may spawn child processes.
    ///
    /// True only for a per-user container whose config did not narrow it off.
    /// The shared-worker gate consumes this value.
    pub fn allows_shared_children(&self) -> bool {
        self.shared_children_allowed
    }

    /// Whether the process may reuse the operator's login files.
    pub fn inherits_operator_logins(&self) -> bool {
        !self.is_hosted()
    }

    /// The narrowed MCP server names, `None` when the section did not narrow.
    pub fn allowed_mcp_servers(&self) -> Option<&[String]> {
        self.allowed_mcp_servers.as_deref()
    }

    /// Whether this policy permits the MCP server `server`.
    ///
    /// Absent leaves every server permitted, an empty list permits none, and
    /// the match is exact against the name a source or an editor registered.
    pub fn permits_mcp_server(&self, server: &str) -> bool {
        match &self.allowed_mcp_servers {
            None => true,
            Some(allowed) => allowed.iter().any(|name| name == server),
        }
    }
}

/// Why a hosting value was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostingConfigError {
    /// `HORON_AGENT_HOSTING_MODE` held a value that names no mode.
    UnknownMode(String),
    /// A hosted mode was selected without a tenant id.
    MissingTenantId(HostingMode),
    /// A tenant id could escape its volume root or confuse an audit log.
    InvalidTenantId { value: String, reason: &'static str },
}

impl std::fmt::Display for HostingConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownMode(value) => write!(
                formatter,
                "unknown hosting mode '{value}'; allowed values: per-user-container, shared-worker"
            ),
            Self::MissingTenantId(mode) => write!(
                formatter,
                "hosting mode '{mode}' requires a tenant id; set HORON_AGENT_TENANT_ID"
            ),
            Self::InvalidTenantId { value, reason } => {
                write!(formatter, "invalid tenant id '{value}': {reason}")
            }
        }
    }
}

impl std::error::Error for HostingConfigError {}

#[cfg(test)]
#[path = "hosting_tests.rs"]
mod tests;
