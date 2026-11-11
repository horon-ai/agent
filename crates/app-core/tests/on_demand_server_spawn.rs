//! The on-demand server contract from `docs/architecture/SERVER.md`: only the
//! one-shot `run` path registers a spawner, registering it starts nothing, the
//! first tool that needs a server starts exactly one, and a failed start is
//! paid once.
//!
//! This runs as its own test binary because the spawner is process-wide: a
//! failing spawner registered here would otherwise leak into every other test
//! in the `app-core` library binary, where the swarm transport reads the same
//! registration.

use horon_agent_app_core::env;
use horon_agent_app_core::server_spawn::{
    ensure_on_demand_server, on_demand_server_spawner_registered, register_on_demand_server_spawner,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn a_run_spawns_one_server_on_demand_and_pays_a_failure_once() {
    let home = tempfile::tempdir().expect("a temporary home");
    let previous = std::env::var_os("HORON_HOME");
    env::set_var("HORON_HOME", home.path());

    // Client and interactive paths register no spawner, so a tool that cannot
    // reach a server keeps the original error instead of starting one behind
    // the client's back.
    assert!(
        !on_demand_server_spawner_registered(),
        "an interactive or client run installs no on-demand spawner"
    );

    let attempts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&attempts);
    register_on_demand_server_spawner(Box::new(move || {
        counted.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { anyhow::bail!("no server in this test") })
    }));
    assert!(on_demand_server_spawner_registered());
    assert_eq!(
        attempts.load(Ordering::Relaxed),
        0,
        "registering a spawner starts nothing: a run that never uses swarm pays nothing"
    );

    let first = ensure_on_demand_server().await.expect_err("the spawner fails here");
    assert!(first.to_string().contains("no server in this test"), "{first}");
    let second = ensure_on_demand_server()
        .await
        .expect_err("the failure stays latched for this run");
    assert!(second.to_string().contains("already failed"), "{second}");
    assert_eq!(
        attempts.load(Ordering::Relaxed),
        1,
        "a run that cannot start a server pays for the attempt once"
    );

    match previous {
        Some(value) => env::set_var("HORON_HOME", value),
        None => env::remove_var("HORON_HOME"),
    }
}
