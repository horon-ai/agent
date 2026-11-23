//! The hosting gate for every package and extension child process.
//!
//! A package's MCP stdio server, an extension tool's child, a declared command
//! and an observer all start through
//! [`PackageExecutable::start_with_arguments`](super::PackageExecutable::start_with_arguments),
//! so one check there covers a package's whole process surface. The decision and
//! its audit record live in [`crate::mcp::hosted`]; this file only names the
//! operation the gate decides on.

use crate::mcp::hosted::{self, HostedRequest};

/// Refuse to start `package_id`'s child when the hosting policy forbids it.
///
/// The error names the policy and the operation, and the refusal is already in
/// the audit log by the time it is returned.
pub(crate) fn ensure(package_id: &str) -> anyhow::Result<()> {
    hosted::check_current(&HostedRequest::package(package_id))?;
    Ok(())
}
