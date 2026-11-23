//! Hand one management operation to a running daemon.
//!
//! Only a failure to reach a daemon at all is answered with "no daemon": the
//! command then publishes through the store's file lock, exactly as it does on a
//! machine that never ran a daemon. Once a connection exists, every failure is
//! ambiguous — the daemon may already have applied the operation — so it is
//! reported and never retried against a second writer.

use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::protocol::{PackageControlOperation, PackageControlReceipt, PackageControlRequest, ServerEvent};

/// How long one daemon round trip may take, including hashing a large package.
const DEADLINE: Duration = Duration::from_secs(120);

/// Apply `operation` on the daemon, or report that no daemon was reachable.
///
/// `Ok(None)` means nothing was sent and the caller may publish offline.
/// `Ok(Some(_))` is the daemon's authoritative receipt: the catalog generation
/// it published and the complete catalog at it.
pub(super) async fn apply(operation: PackageControlOperation) -> Result<Option<PackageControlReceipt>> {
    let socket = crate::server::socket_path();
    // One connect decides reachability, and only "nothing is listening" is
    // offline. A permission failure or a transport error is reported: it means a
    // daemon may exist but this command could not reach it, and publishing
    // behind its back would be the second writer this guard exists to prevent.
    let mut client = match crate::server::Client::connect_with_path(socket.clone()).await {
        Ok(client) => client,
        Err(error) if no_daemon_listening(&error) => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("connect to the horon-agent daemon at {}", socket.display()));
        }
    };
    let request = PackageControlRequest {
        operation_id: crate::id::new_id("pkgop"),
        operation,
    };
    let reply = tokio::time::timeout(DEADLINE, client.package_control(request))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "the daemon at {} did not answer within {}s; the change may already be applied, so inspect \
                 `horon package list` before running the command again",
                socket.display(),
                DEADLINE.as_secs()
            )
        })?;
    match reply? {
        ServerEvent::PackageControlResult { receipt, .. } => Ok(Some(receipt)),
        ServerEvent::Error { message, .. } => bail!("the daemon refused the package operation: {message}"),
        other => bail!("unexpected reply to a package operation: {other:?}"),
    }
}

/// Whether a failed connect proves that nothing is listening at that path.
///
/// Only "the socket does not exist" and "the endpoint refused" qualify. Any
/// other failure (a permission error, an exhausted descriptor table, a broken
/// pipe adapter) leaves open that a daemon is there, so the caller must not fall
/// back to the offline writer.
fn no_daemon_listening(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            )
        })
    })
}
