//! An isolated `horon-agent serve` daemon for the ACP conformance tests.
//!
//! The daemon owns its scratch root, HOME, runtime dir, socket and scripted
//! provider, so a test never touches the developer's `~/.horon-agent` or a real
//! daemon: `HORON_AGENT_SOCKET`/`HORON_AGENT_API_SOCKET` are removed from the
//! child environment and replaced with the isolated socket, and the child is
//! killed and its scratch root removed on drop.
//!
//! Include it from a test target together with the modules it names:
//!
//! ```ignore
//! #[path = "support/acp_schema.rs"] mod acp_schema;
//! #[path = "support/acp_client.rs"] mod acp_client;
//! #[path = "support/acp_daemon.rs"] mod acp_daemon;
//! #[path = "support/lifecycle.rs"] mod lifecycle;
//! ```
#![allow(dead_code)]

use crate::lifecycle::{
    BINARY, ScriptedProvider, scratch_root, set_mode, wait_for_socket, write_config, write_scripted_provider_config,
};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

pub const STARTUP_DEADLINE: Duration = Duration::from_secs(120);

pub struct AcpDaemon {
    pub root: PathBuf,
    pub home: PathBuf,
    pub runtime: PathBuf,
    pub socket: PathBuf,
    pub provider: Arc<ScriptedProvider>,
    profile: String,
    child: Child,
}

impl AcpDaemon {
    /// `profile` is the tool profile the daemon runs with (`"acp"` for the ACP
    /// conformance suite).
    pub fn start(profile: &str) -> Self {
        let root = scratch_root("acpdaemon");
        let home = root.join("home");
        let runtime = root.join("rt");
        std::fs::create_dir_all(&home).expect("acp daemon home");
        std::fs::create_dir_all(&runtime).expect("acp daemon runtime");
        set_mode(&root, 0o700);
        set_mode(&home, 0o700);
        set_mode(&runtime, 0o700);
        write_config(&home);
        let socket = runtime.join("horon-agent.sock");
        let _ = std::fs::remove_file(&socket);
        let provider = ScriptedProvider::start();
        let env = isolated_env(&root, &home, &runtime, &socket, profile);
        write_scripted_provider_config(&home, provider.base_url());
        let daemon = Self {
            child: spawn(&env, &runtime.join("daemon.log")),
            root,
            home,
            runtime,
            socket,
            provider,
            profile: profile.to_string(),
        };
        wait_for_socket(&daemon.socket, STARTUP_DEADLINE);
        daemon
    }

    fn spawn_child(&self) -> Child {
        spawn(&self.env_pairs(), &self.runtime.join("daemon.log"))
    }

    /// The complete isolated environment, as `--agent-env K=V` pairs for the
    /// TCK and as the child environment for [`crate::acp_client::AcpClient`].
    pub fn env_pairs(&self) -> Vec<(String, String)> {
        isolated_env(&self.root, &self.home, &self.runtime, &self.socket, &self.profile)
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn logs(&self) -> String {
        std::fs::read_to_string(self.runtime.join("daemon.log")).unwrap_or_default()
    }

    pub fn restart(&mut self) {
        self.stop();
        self.child = self.spawn_child();
        wait_for_socket(&self.socket, STARTUP_DEADLINE);
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for AcpDaemon {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn isolated_env(root: &Path, home: &Path, runtime: &Path, socket: &Path, profile: &str) -> Vec<(String, String)> {
    vec![
        ("HORON_AGENT_HOME".to_string(), home.display().to_string()),
        ("HORON_AGENT_RUNTIME_DIR".to_string(), runtime.display().to_string()),
        ("HOME".to_string(), root.display().to_string()),
        ("XDG_CONFIG_HOME".to_string(), root.join("config").display().to_string()),
        ("XDG_CACHE_HOME".to_string(), root.join("cache").display().to_string()),
        ("OPENAI_API_KEY".to_string(), "t17-scripted-key".to_string()),
        ("HORON_AGENT_OPENAI_TRANSPORT".to_string(), "https".to_string()),
        ("HORON_AGENT_NO_AUTO_UPDATE".to_string(), "1".to_string()),
        ("HORON_AGENT_MEMORY_ENABLED".to_string(), "0".to_string()),
        ("HORON_AGENT_MEMORY_SIDECAR_ENABLED".to_string(), "0".to_string()),
        ("HORON_AGENT_NON_INTERACTIVE".to_string(), "1".to_string()),
        ("HORON_AGENT_TOOL_PROFILE".to_string(), profile.to_string()),
        ("HORON_AGENT_SOCKET".to_string(), socket.display().to_string()),
    ]
}

fn spawn(env: &[(String, String)], log_path: &Path) -> Child {
    let log = std::fs::File::create(log_path).expect("acp daemon log");
    let errors = log.try_clone().expect("acp daemon log clone");
    let mut command = Command::new(BINARY);
    command.args(["serve", "--no-update"]).env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    command
        .env_remove("HORON_AGENT_API_SOCKET")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(Stdio::from(errors))
        .spawn()
        .expect("spawn acp daemon")
}
