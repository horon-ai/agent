//! The host's own dispatch for one executable package command.
//!
//! A package's executable command is a slash-command surface, and the only
//! caller of it is the daemon: `horon-agent run` has no command input. Rather
//! than start a daemon (which would drag a socket, a bridge and a provider
//! configuration into a test that is about package binaries), this module drives
//! `horon_agent::extensions::invoke::call_command` in the test process, over
//! the same isolated home the CLI installs into. That is exactly the function
//! the daemon's `PackageCommand` runtime command calls, so the child that answers
//! is started by the real host, not by a test stand-in.
//!
//! The child a call starts stays in the host's process-wide registry, which in a
//! daemon lives as long as the session does. Here the call is followed by a
//! revocation so the child this module started is gone before the test asks the
//! process table whether anything still runs.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use horon_agent::agent::Agent;
use horon_agent::extensions::lifecycle::{PackageLifecycleRegistry, RevocationCause};
use horon_agent::message::{Message, ToolDefinition};
use horon_agent::provider::{EventStream, Provider};
use horon_agent::tool::Registry;
use serde_json::{Value, json};

/// The environment keys this module redirects, and their previous values.
const REDIRECTED: [&str; 10] = [
    "HOME",
    "USERPROFILE",
    "HORON_HOME",
    "HORON_AGENT_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "HORON_AGENT_MEMORY_ENABLED",
    "HORON_AGENT_MEMORY_SIDECAR_ENABLED",
    "HORON_AGENT_RUN_MCP",
    "HORON_AGENT_NON_INTERACTIVE",
];

/// A provider that is never asked to complete anything.
struct UnusedProvider;

#[async_trait]
impl Provider for UnusedProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        Err(anyhow::anyhow!("the package end-to-end host never calls a model"))
    }

    fn name(&self) -> &str {
        "unused"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(UnusedProvider)
    }
}

/// The redirected environment, restored on drop.
struct Isolated {
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl Isolated {
    fn enter(home: &Path) -> Self {
        let previous: Vec<(&'static str, Option<OsString>)> =
            REDIRECTED.into_iter().map(|key| (key, std::env::var_os(key))).collect();
        let runtime = home.join("runtime");
        std::fs::create_dir_all(&runtime).expect("create the isolated runtime directory");
        horon_agent::env::set_var("HOME", home);
        horon_agent::env::set_var("USERPROFILE", home);
        horon_agent::env::set_var("HORON_HOME", home);
        horon_agent::env::set_var("HORON_AGENT_RUNTIME_DIR", &runtime);
        horon_agent::env::set_var("XDG_CONFIG_HOME", home.join("config"));
        horon_agent::env::set_var("XDG_CACHE_HOME", home.join("cache"));
        horon_agent::env::set_var("HORON_AGENT_MEMORY_ENABLED", "0");
        horon_agent::env::set_var("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0");
        horon_agent::env::set_var("HORON_AGENT_RUN_MCP", "0");
        horon_agent::env::set_var("HORON_AGENT_NON_INTERACTIVE", "1");
        horon_agent::config::invalidate_config_cache();
        Self { previous }
    }
}

impl Drop for Isolated {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => horon_agent::env::set_var(key, value),
                None => horon_agent::env::remove_var(key),
            }
        }
        horon_agent::config::invalidate_config_cache();
    }
}

/// Run one declared executable command of the installed `package_id` and return
/// the value its child answered.
pub fn call_command(home: &Path, package_id: &str, command: &str) -> Result<Value, String> {
    let isolated = Isolated::enter(home);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("build the host runtime: {error}"))?;
    let outcome = runtime
        .block_on(async {
            let provider: Arc<dyn Provider> = Arc::new(UnusedProvider);
            let registry = Registry::new(Arc::clone(&provider)).await;
            let mut agent = Agent::new(provider, registry);
            agent.enter_package_generation().await;
            let session = agent.session_id().to_owned();
            let generation = agent.package_snapshot().map(|snapshot| snapshot.generation());
            let value = horon_agent::extensions::invoke::call_command(
                &session,
                package_id,
                command,
                json!({ "text": "qa", "args": ["qa"] }),
            )
            .await;
            agent.mark_closed();
            // The command's child stays registered for the life of the session.
            // End it the way a disable does, while this runtime can still run the
            // blocking task that sweeps the process group: the reaping check that
            // follows must see only the binary's own runs. The fired count is the
            // reaping check's business, not this function's.
            if let Some(generation) = generation {
                let _ =
                    PackageLifecycleRegistry::shared().revoke(package_id, generation + 1, RevocationCause::Disabled);
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            value
        })
        .map_err(|error| format!("dispatch package {package_id} command {command}: {error:#}"))?;
    drop(runtime);
    drop(isolated);
    Ok(outcome)
}
