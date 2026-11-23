//! The hosting policy gate for MCP servers and package child processes.
//!
//! A daemon resolves one [`HostingPolicy`] for the process
//! ([`crate::hosting`]). Every place that would start a child process, open a
//! remote MCP connection, or hand a server to the process-global pool asks this
//! module first. The decision is a pure function of the policy, the operation
//! and the server or package name, so the whole matrix is tested without an
//! environment; the thin helpers below read the process policy and write the
//! audit record.
//!
//! A refusal is always explicit. Callers get a typed [`HostedRefusal`] that
//! names the policy and the operation, and [`check`] writes one `HOSTED_POLICY`
//! event with the same facts. Nothing here falls back to shared execution.

use crate::hosting::{HostingMode, HostingPolicy};
use std::fmt;

/// The audit event every refused operation writes.
pub const AUDIT_EVENT: &str = "HOSTED_POLICY";

/// One kind of work a gate is asked to allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostedOperation {
    /// A stdio MCP server child the daemon starts from a configured entry.
    Stdio,
    /// A stdio MCP server an ACP editor supplied for one session.
    SessionStdio,
    /// A stdio server the process-global shared pool would hand to sessions.
    PooledStdio,
    /// A remote (HTTP or SSE) MCP connection.
    Remote,
    /// A package or extension child process, a package's MCP stdio child included.
    PackageChild,
}

impl HostedOperation {
    /// The stable token the audit event and a test name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "mcp-stdio",
            Self::SessionStdio => "session-mcp-stdio",
            Self::PooledStdio => "pooled-mcp-stdio",
            Self::Remote => "mcp-remote",
            Self::PackageChild => "package-child",
        }
    }

    /// What the operation is, for a message a person reads.
    const fn label(self) -> &'static str {
        match self {
            Self::Stdio => "the MCP stdio server",
            Self::SessionStdio => "the session MCP stdio server",
            Self::PooledStdio => "the pooled MCP stdio server",
            Self::Remote => "the remote MCP server",
            Self::PackageChild => "the package child",
        }
    }

    /// Whether the operation starts a process on this host.
    pub const fn starts_a_child(self) -> bool {
        !matches!(self, Self::Remote)
    }
}

/// Why a hosted gate refused an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefusalReason {
    /// A shared worker serves several customers from one process.
    SharedWorker,
    /// `[hosting] allow_shared_children = false` in a per-user container.
    SharedChildrenDisabled,
    /// The server is not in `[hosting] allowed_mcp_servers`.
    NotAllowed,
    /// The process policy could not be resolved.
    Unresolved,
}

impl RefusalReason {
    /// The stable token the audit event carries.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SharedWorker => "shared-worker",
            Self::SharedChildrenDisabled => "children-disabled",
            Self::NotAllowed => "not-allowed",
            Self::Unresolved => "unresolved-policy",
        }
    }
}

/// One refused operation, naming the policy and what would have run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedRefusal {
    mode: Option<HostingMode>,
    tenant: Option<String>,
    operation: HostedOperation,
    subject: Option<String>,
    reason: RefusalReason,
    detail: Option<String>,
}

impl HostedRefusal {
    /// The policy mode, `None` when the policy could not be resolved.
    pub fn mode(&self) -> Option<HostingMode> {
        self.mode
    }

    /// The refused operation.
    pub fn operation(&self) -> HostedOperation {
        self.operation
    }

    /// Why it was refused.
    pub fn reason(&self) -> RefusalReason {
        self.reason
    }

    /// The server or package name, when the gate knew one.
    pub fn subject(&self) -> Option<&str> {
        self.subject.as_deref()
    }

    /// The sentence that explains the reason, without the policy or the subject.
    fn explanation(&self) -> &'static str {
        match (self.reason, self.operation) {
            (RefusalReason::SharedWorker, HostedOperation::Remote) => {
                "a shared worker serves several customers from one process, so its OAuth token store and ${VAR} \
                 environment cannot be proven per user"
            }
            (RefusalReason::SharedWorker, _) => {
                "a shared worker serves several customers from one process and never starts a child process for \
                 one of them"
            }
            (RefusalReason::SharedChildrenDisabled, _) => {
                "[hosting] allow_shared_children = false closes child processes in this container"
            }
            (RefusalReason::NotAllowed, _) => "it is not listed in [hosting] allowed_mcp_servers",
            (RefusalReason::Unresolved, _) => "the hosting policy could not be resolved",
        }
    }
}

impl fmt::Display for HostedRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let policy = self.mode.map(HostingMode::as_str).unwrap_or("unresolved");
        write!(
            formatter,
            "hosting policy '{policy}' refuses {}",
            self.operation.label()
        )?;
        if let Some(subject) = self.subject.as_deref() {
            write!(formatter, " '{subject}'")?;
        }
        write!(formatter, ": {}", self.explanation())?;
        if let Some(detail) = self.detail.as_deref() {
            write!(formatter, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for HostedRefusal {}

/// One operation a gate is asked about.
#[derive(Debug, Clone, Copy)]
pub struct HostedRequest<'a> {
    /// What would run.
    pub operation: HostedOperation,
    /// The MCP server name, when the operation names one.
    pub server: Option<&'a str>,
    /// The package id, when the operation starts a package child.
    pub package: Option<&'a str>,
}

impl<'a> HostedRequest<'a> {
    /// An MCP server operation named `server`.
    pub const fn server(operation: HostedOperation, server: &'a str) -> Self {
        Self {
            operation,
            server: Some(server),
            package: None,
        }
    }

    /// A package child named `package`.
    pub const fn package(package: &'a str) -> Self {
        Self {
            operation: HostedOperation::PackageChild,
            server: None,
            package: Some(package),
        }
    }

    /// The name a refusal and its audit record carry.
    fn subject(&self) -> Option<&'a str> {
        self.server.or(self.package)
    }
}

/// Whether `request` may run under `policy`. Records nothing.
///
/// - No hosting: everything is allowed, which is the behavior every process had
///   before the hosting modes existed.
/// - A shared worker refuses every operation, because both child processes and
///   the remote credential store would belong to one customer while the process
///   serves all of them.
/// - A per-user container allows its own children unless the file narrowed them
///   off with `allow_shared_children = false`; its remote connections are
///   per-user because the home volume and the process environment are.
/// - `[hosting] allowed_mcp_servers` narrows a hosted process only: absent
///   leaves every server reachable, an empty list reaches none. A process
///   without hosting is unchanged, so a key left over on a reused volume never
///   moves the operator's own servers out of reach.
pub fn decide(policy: &HostingPolicy, request: &HostedRequest<'_>) -> Result<(), HostedRefusal> {
    let refusal = |reason: RefusalReason| HostedRefusal {
        mode: Some(policy.mode()),
        tenant: policy.tenant_id().map(|tenant| tenant.as_str().to_string()),
        operation: request.operation,
        subject: request.subject().map(str::to_string),
        reason,
        detail: None,
    };
    match policy.mode() {
        HostingMode::SharedWorker => return Err(refusal(RefusalReason::SharedWorker)),
        HostingMode::PerUserContainer => {
            if request.operation.starts_a_child() && !policy.allows_shared_children() {
                return Err(refusal(RefusalReason::SharedChildrenDisabled));
            }
        }
        HostingMode::None => {}
    }
    if policy.is_hosted()
        && let Some(server) = request.server
        && !policy.permits_mcp_server(server)
    {
        return Err(refusal(RefusalReason::NotAllowed));
    }
    Ok(())
}

/// [`decide`] plus the audit record a refusal earns.
pub fn check(policy: &HostingPolicy, request: &HostedRequest<'_>) -> Result<(), HostedRefusal> {
    let decision = decide(policy, request);
    if let Err(refusal) = &decision {
        audit(refusal);
    }
    decision
}

/// [`check`] against the process policy.
///
/// An unresolvable policy refuses: a value the daemon could not read is a
/// misconfiguration this gate must not guess around.
pub fn check_current(request: &HostedRequest<'_>) -> Result<(), HostedRefusal> {
    match crate::hosting::current() {
        Ok(policy) => check(policy, request),
        Err(error) => {
            let refusal = HostedRefusal {
                mode: None,
                tenant: None,
                operation: request.operation,
                subject: request.subject().map(str::to_string),
                reason: RefusalReason::Unresolved,
                detail: Some(error.to_string()),
            };
            audit(&refusal);
            Err(refusal)
        }
    }
}

/// The hosted refusal inside `error`'s chain, when the error carries one.
///
/// A caller that wraps a refusal with `anyhow` context still finds it here, so
/// a path that reports failures instead of propagating them can report the
/// policy too.
pub fn find(error: &anyhow::Error) -> Option<&HostedRefusal> {
    error.chain().find_map(|cause| cause.downcast_ref::<HostedRefusal>())
}

/// Write the one audit record a refusal earns.
///
/// The record carries the policy, the operation, the server or package name, the
/// reason and the tenant. Never a command argument, an environment value, a
/// header or a URL: those can hold a secret, and a refusal needs none of them.
fn audit(refusal: &HostedRefusal) {
    crate::logging::event_warn(
        AUDIT_EVENT,
        [
            ("policy", refusal.mode.map(HostingMode::as_str).unwrap_or("unresolved")),
            ("operation", refusal.operation.as_str()),
            ("subject", refusal.subject.as_deref().unwrap_or("-")),
            ("reason", refusal.reason.as_str()),
            ("tenant", refusal.tenant.as_deref().unwrap_or("-")),
        ],
    );
}

#[cfg(test)]
#[path = "hosted_tests.rs"]
mod tests;
