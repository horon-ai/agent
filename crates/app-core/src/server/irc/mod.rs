//! Server-side IRC wiring: address resolution, delivery into live sessions,
//! wake-result relay, and the process-global bus install.

mod delivery;
mod directory;
mod relay;

use super::Server;
use horon_agent_irc_core::{IrcBus, IrcLimits};
use std::sync::Arc;
use std::time::Duration;

/// Install the process-global IRC bus for this daemon run.
///
/// Called once from the daemon's run path after the listeners are bound, so
/// constructing a `Server` (which tests do in parallel) never publishes state.
pub(super) fn install(server: &Server) {
    let directory = Arc::new(directory::SwarmDirectory::new(Arc::clone(&server.swarm_state.members)));
    let deliver = Arc::new(delivery::SessionDeliver::new(
        Arc::clone(&server.swarm_state.members),
        Arc::clone(&server.sessions),
        Arc::clone(&server.soft_interrupt_queues),
        Arc::clone(&server.swarm_state.swarms_by_id),
        Arc::clone(&server.event_history),
        Arc::clone(&server.event_counter),
        server.swarm_event_tx.clone(),
    ));
    horon_agent_irc_core::global::install(IrcBus::new(limits(), directory, deliver));
}

fn limits() -> IrcLimits {
    let config = crate::config::config().irc.clamped();
    IrcLimits {
        mailbox_cap: config.mailbox_cap,
        max_body_bytes: config.max_body_bytes,
        wait_max: Duration::from_secs(config.wait_max_secs),
        broadcast_concurrency: config.broadcast_concurrency,
    }
}

#[cfg(test)]
#[path = "test_support_tests.rs"]
mod test_support;

#[cfg(test)]
#[path = "directory_tests.rs"]
mod directory_tests;

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod delivery_tests;

#[cfg(test)]
#[path = "relay_tests.rs"]
mod relay_tests;
