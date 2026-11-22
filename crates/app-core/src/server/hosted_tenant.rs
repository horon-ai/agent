//! The startup boundary a hosted tenant must pass.
//!
//! The daemon calls [`enforce_at_startup`] before it binds a socket. In a
//! per-user container it refuses to run unless the process owns an owner-only
//! home volume, a separate runtime directory, a tenant id and a disabled TCP
//! gateway, because the alternative is serving a customer from the operator's
//! own home or through a socket that reaches outside the container.
//!
//! A shared worker starts even with a hostile-looking environment: its own
//! child-process gate is what refuses the operations it must not run, and that
//! gate reads [`crate::hosting::HostingPolicy::allows_shared_children`].

use crate::hosting::{self, HostingInputs, HostingPaths, HostingPolicy};

/// Resolve the hosting policy and, for a per-user container, confirm its boundary.
///
/// Inputs are arguments rather than globals so a test drives the whole decision
/// without an environment lock; [`enforce_at_startup`] is the production caller.
pub fn enforce_startup(inputs: &HostingInputs, paths: &HostingPaths) -> anyhow::Result<HostingPolicy> {
    let policy = hosting::resolve(inputs)?;
    hosting::confirm_container_boundary(&policy, paths)?;
    Ok(policy)
}

/// The daemon startup hook: enforce the boundary or report why it was refused.
pub fn enforce_at_startup() -> anyhow::Result<()> {
    let inputs = HostingInputs::from_environment();
    let paths = hosting::boundary_paths(crate::config::config().gateway.enabled);
    let policy = enforce_startup(&inputs, &paths)?;
    if policy.is_hosted() {
        crate::logging::info(&format!(
            "Hosting boundary confirmed: mode={} tenant={}",
            policy.mode(),
            policy.tenant_id().map(|tenant| tenant.as_str()).unwrap_or("(none)")
        ));
    }
    if policy.is_shared_worker() {
        crate::logging::warn(
            "Shared hosting worker: MCP servers and extension child processes are refused for every customer",
        );
    }
    Ok(())
}
