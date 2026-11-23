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

use crate::hosting::{self, HostingPaths, HostingPolicy};

/// Confirm the resolved hosting policy against container paths when needed and
/// record the active hosting audit notice.
pub(crate) fn enforce_policy_with_paths(
    policy: &HostingPolicy,
    paths: impl FnOnce() -> HostingPaths,
) -> anyhow::Result<()> {
    if policy.is_per_user_container() {
        let paths = paths();
        hosting::confirm_container_boundary(policy, &paths)?;
    }
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

/// The daemon startup hook: enforce the boundary or report why it was refused.
pub fn enforce_at_startup() -> anyhow::Result<()> {
    let policy = hosting::current()?;
    enforce_policy_with_paths(policy, || {
        hosting::boundary_paths(crate::config::config().gateway.enabled)
    })
}
