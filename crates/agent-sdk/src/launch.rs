//! Runtime startup and private horon-agent instances.
//!
//! [`ensure_runtime`] attaches applications to the user's shared runtime.
//! [`launch_instance`] instead creates an isolated runtime directory and state
//! home which the returned owner tears down on drop.

use crate::errors::{Error, ErrorKind, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const CREDENTIAL_FILES: &[&str] = &[
    "auth.json",
    "openai-auth.json",
    "antigravity_oauth.json",
    "gemini_oauth.json",
    "config.toml",
];

const EXTERNAL_CREDENTIAL_FILES: &[&str] = &[
    ".claude/.credentials.json",
    ".codex/auth.json",
    ".gemini/oauth_creds.json",
    ".cursor/auth.json",
    ".config/cursor/auth.json",
    "AppData/Roaming/Cursor/auth.json",
    ".config/Cursor/User/globalStorage/state.vscdb",
    ".config/cursor/User/globalStorage/state.vscdb",
    "Library/Application Support/Cursor/User/globalStorage/state.vscdb",
    "Library/Application Support/cursor/User/globalStorage/state.vscdb",
    "AppData/Roaming/Cursor/User/globalStorage/state.vscdb",
    "AppData/Roaming/cursor/User/globalStorage/state.vscdb",
    ".config/github-copilot/hosts.json",
    ".config/github-copilot/apps.json",
    ".copilot/config.json",
    ".hermes/auth.json",
    ".pi/agent/auth.json",
    ".openclaw/agent/auth.json",
    ".openclaw/credentials/oauth.json",
    ".local/share/opencode/auth.json",
];

/// Operator ownership mode for autonomous wake requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WakeMode {
    #[default]
    Internal,
    External,
}

impl WakeMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::External => "external",
        }
    }
}

/// Options for starting a private horon-agent instance.
pub struct LaunchOptions {
    /// State directory for the instance. A temporary, drop-cleaned directory is
    /// used when omitted; an explicit directory is persistent.
    pub horon_agent_home: Option<PathBuf>,
    /// Working directory inherited by sessions created in the instance.
    pub working_dir: Option<PathBuf>,
    /// Share the user's provider login files. Enabled by default.
    pub inherit_logins: bool,
    /// horon-agent executable. Defaults to `horon-agent` on `$PATH`.
    pub binary: Option<PathBuf>,
    /// Extra environment variables for the private instance.
    pub env: HashMap<OsString, OsString>,
    /// Default model for spawned swarm workers, unless overridden per spawn.
    /// `inherit` defaults to the coordinator's model and auth route.
    /// This is applied as the operator-level
    /// `HORON_AGENT_SWARM_MODEL` override and takes precedence over `env`.
    pub swarm_model: Option<String>,
    /// Who executes autonomous wake requests. Applied as the operator-level
    /// `HORON_AGENT_WAKE_MODE` override and takes precedence over `env`.
    pub wake_mode: Option<WakeMode>,
    /// How long to wait for the API socket to accept connections.
    pub startup_timeout: Duration,
    /// Forward the bridge's stderr instead of capturing it for startup errors.
    pub inherit_stderr: bool,
    /// Maximum time spent removing a temporary home which is still being
    /// written by background work.
    pub cleanup_timeout: Duration,
    /// Client identity used by [`crate::HoronAgentClient::launch`].
    pub client_name: String,
    /// Request timeout used by [`crate::HoronAgentClient::launch`].
    pub request_timeout: Option<Duration>,
    /// Socket override used only by [`ensure_runtime`]. Kept here for backwards
    /// compatibility with the original shared-runtime launcher.
    pub api_socket: Option<PathBuf>,
    /// Shared-runtime startup timeout used only by [`ensure_runtime`].
    ///
    /// This retains the original Rust SDK field while `startup_timeout` matches
    /// private-instance launch terminology.
    pub start_timeout: Duration,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            horon_agent_home: None,
            working_dir: None,
            inherit_logins: true,
            binary: None,
            env: HashMap::new(),
            swarm_model: None,
            wake_mode: None,
            startup_timeout: Duration::from_secs(30),
            inherit_stderr: false,
            cleanup_timeout: Duration::from_secs(30),
            client_name: concat!("horon-agent-sdk-rs/", env!("CARGO_PKG_VERSION")).to_string(),
            request_timeout: Some(Duration::from_secs(30)),
            api_socket: None,
            start_timeout: Duration::from_secs(30),
        }
    }
}

/// A running private instance. Dropping it stops both bridge and daemon, then
/// removes an SDK-created temporary home. Caller-supplied homes are preserved.
pub struct LaunchedInstance {
    pub socket_path: PathBuf,
    pub horon_agent_home: PathBuf,
    runtime_dir: PathBuf,
    child: Mutex<Option<Child>>,
    ephemeral: bool,
    cleanup_timeout: Duration,
    stopped: AtomicBool,
}

impl LaunchedInstance {
    fn shutdown(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }

        stop_instance_daemon(&self.horon_agent_home, &self.runtime_dir);
        if let Ok(mut slot) = self.child.lock()
            && let Some(mut child) = slot.take()
        {
            horon_agent_platform::process::terminate_child(&mut child);
        }
        if self.ephemeral {
            remove_ephemeral_home(&self.horon_agent_home, self.cleanup_timeout);
        }
    }
}

impl Drop for LaunchedInstance {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Start an isolated daemon and API bridge and return once its socket accepts.
/// The instance runs the launching binary and never auto-updates, so it stays on that horon-agent version.
pub fn launch_instance(options: &LaunchOptions) -> Result<LaunchedInstance> {
    let ephemeral = options.horon_agent_home.is_none();
    let horon_agent_home = match &options.horon_agent_home {
        Some(path) => {
            fs::create_dir_all(path).map_err(launch_io)?;
            path.clone()
        }
        None => tempfile::Builder::new()
            .prefix("horon-agent-sdk-instance-")
            .tempdir()
            .map_err(launch_io)?
            .keep(),
    };
    set_owner_only_dir(&horon_agent_home)?;

    let runtime_dir = horon_agent_home.join("run");
    fs::create_dir_all(&runtime_dir).map_err(launch_io)?;
    set_owner_only_dir(&runtime_dir)?;
    let socket_path = runtime_dir.join("horon-agent-api.sock");
    for stale in [
        "horon-agent-api.sock",
        "horon-agent.sock",
        "horon-agent-debug.sock",
        "horon-agent.sock.hash",
    ] {
        let _ = fs::remove_file(runtime_dir.join(stale));
    }

    let cleanup_on_error = || {
        if ephemeral {
            remove_ephemeral_home(&horon_agent_home, Duration::ZERO);
        }
    };
    if options.inherit_logins
        && let Err(error) = inherit_credentials(&user_horon_agent_home(), &horon_agent_home)
    {
        cleanup_on_error();
        return Err(error);
    }

    // A caller-supplied home may already register a daemon for this runtime
    // directory. Record it so a failed launch never stops a daemon it did not start.
    let registered_before = registered_daemon_pid(&horon_agent_home, &runtime_dir);

    let binary = options.binary.clone().unwrap_or_else(|| PathBuf::from("horon-agent"));
    let mut command = Command::new(&binary);
    horon_agent_platform::process::hide_console_window(&mut command);
    command
        // `--no-update` turns off the bridge's own background update check, which
        // also runs for source builds.
        .args([
            OsStr::new("--no-update"),
            OsStr::new("api-bridge"),
            OsStr::new("--api-socket"),
        ])
        .arg(&socket_path)
        .current_dir(
            options
                .working_dir
                .as_deref()
                .unwrap_or_else(|| Path::new(".")),
        )
        .env("HORON_AGENT_HOME", &horon_agent_home)
        .env("HORON_AGENT_RUNTIME_DIR", &runtime_dir)
        .env("HORON_AGENT_API_SOCKET", &socket_path)
        .env("HORON_AGENT_SOCKET", runtime_dir.join("horon-agent.sock"))
        // An empty private home always looks out of date, so the release updater
        // would replace and restart the runtime mid-session: the client's turn fails
        // and the restarted server is orphaned. The variable is inherited by every
        // horon-agent process the instance spawns (argv is not), so it keeps the release
        // auto-installer off in those too. The shared runtime still updates.
        .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
        .envs(options.env.iter())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(if options.inherit_stderr {
            Stdio::inherit()
        } else {
            Stdio::piped()
        });
    if let Some(swarm_model) = options.swarm_model.as_deref() {
        command.env("HORON_AGENT_SWARM_MODEL", swarm_model);
    }
    if let Some(wake_mode) = options.wake_mode {
        command.env("HORON_AGENT_WAKE_MODE", wake_mode.as_str());
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(cause) => {
            cleanup_on_error();
            let message = if cause.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "could not run `{}`: horon-agent is not installed, or not on PATH. Install it from https://horon-agent.sh, or pass `binary` with its full path.",
                    binary.display()
                )
            } else {
                format!("could not run `{}`: {cause}", binary.display())
            };
            return Err(Error::new(ErrorKind::HoronAgentNotFound, message));
        }
    };
    let stderr = std::sync::Arc::new(Mutex::new(String::new()));
    let mut stderr_reader = child.stderr.take().map(|mut pipe| {
        let stderr = std::sync::Arc::clone(&stderr);
        std::thread::spawn(move || {
            let mut chunk = [0_u8; 1024];
            while let Ok(count) = pipe.read(&mut chunk) {
                if count == 0 {
                    break;
                }
                if let Ok(mut captured) = stderr.lock() {
                    captured.push_str(&String::from_utf8_lossy(&chunk[..count]));
                    if captured.len() > 4000 {
                        let mut keep_from = captured.len() - 4000;
                        while !captured.is_char_boundary(keep_from) {
                            keep_from += 1;
                        }
                        captured.drain(..keep_from);
                    }
                }
            }
        })
    });

    let deadline = Instant::now() + options.startup_timeout;
    while Instant::now() < deadline {
        if socket_accepts(&socket_path) {
            return Ok(LaunchedInstance {
                socket_path,
                horon_agent_home,
                runtime_dir,
                child: Mutex::new(Some(child)),
                ephemeral,
                cleanup_timeout: options.cleanup_timeout,
                stopped: AtomicBool::new(false),
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // The daemon is a separate process the bridge spawned, so the bridge
                // exiting does not stop it. Stop it before joining stderr: a daemon
                // that inherited the pipe would keep the reader blocked.
                stop_daemon_started_by_failed_launch(&horon_agent_home, &runtime_dir, registered_before);
                if let Some(reader) = stderr_reader.take() {
                    let _ = reader.join();
                }
                let stderr = stderr.lock().map(|value| value.clone()).unwrap_or_default();
                cleanup_on_error();
                return Err(Error::new(
                    ErrorKind::StartupFailed,
                    format!("horon-agent exited during startup ({status}){}", stderr_suffix(&stderr)),
                ));
            }
            Ok(None) => {}
            Err(cause) => {
                stop_daemon_started_by_failed_launch(&horon_agent_home, &runtime_dir, registered_before);
                horon_agent_platform::process::terminate_child(&mut child);
                cleanup_on_error();
                return Err(Error::new(ErrorKind::StartupFailed, cause.to_string()));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    stop_daemon_started_by_failed_launch(&horon_agent_home, &runtime_dir, registered_before);
    horon_agent_platform::process::terminate_child(&mut child);
    if let Some(reader) = stderr_reader.take() {
        let _ = reader.join();
    }
    let stderr = stderr.lock().map(|value| value.clone()).unwrap_or_default();
    cleanup_on_error();
    Err(Error::new(
        ErrorKind::StartupTimeout,
        format!(
            "horon-agent did not create its API socket at {} within {:?}{}",
            socket_path.display(),
            options.startup_timeout,
            stderr_suffix(&stderr)
        ),
    ))
}

/// Resolve the user's normal horon-agent home before a private instance overrides it.
pub fn user_horon_agent_home() -> PathBuf {
    std::env::var_os("HORON_AGENT_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".horon-agent"))
}

/// Share rotating login records with an instance and copy mutable config.
/// Returns the instance-relative paths which were inherited.
pub fn inherit_credentials(from_home: &Path, to_home: &Path) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(to_home).map_err(launch_io)?;
    let metadata = fs::symlink_metadata(to_home).map_err(launch_io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::new(
            ErrorKind::InvalidInstanceHome,
            format!(
                "instance home must be a real directory, not a link or file: {}",
                to_home.display()
            ),
        ));
    }
    if from_home.exists() && fs::canonicalize(from_home).ok() == fs::canonicalize(to_home).ok() {
        return Err(Error::new(
            ErrorKind::InvalidInstanceHome,
            "instance home must be different from the user's horon-agent home",
        ));
    }

    let mut inherited = Vec::new();
    for name in CREDENTIAL_FILES {
        let source = from_home.join(name);
        if !source.is_file() {
            continue;
        }
        let destination = to_home.join(name);
        if *name == "config.toml" {
            fs::copy(&source, &destination).map_err(launch_io)?;
            set_owner_only_file(&destination)?;
        } else {
            link_credential_file(&source, to_home, Path::new(name))?;
        }
        inherited.push(PathBuf::from(name));
    }

    // Provider env files live in the platform config directory. Link exact
    // files, never the directory, so cleanup cannot traverse into user data.
    if let Ok(entries) = fs::read_dir(user_app_config_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().ends_with(".env") || !entry.path().is_file() {
                continue;
            }
            let relative = PathBuf::from("config").join("horon-agent").join(&name);
            link_credential_file(&entry.path(), to_home, &relative)?;
            inherited.push(relative);
        }
    }

    let home = home_dir();
    for relative in EXTERNAL_CREDENTIAL_FILES {
        let source = home.join(relative);
        if !source.is_file() {
            continue;
        }
        let instance_relative = PathBuf::from("external").join(relative);
        link_credential_file(&source, to_home, &instance_relative)?;
        inherited.push(instance_relative);
    }

    // OpenClaw's current credential store is per-agent. Enumerate only one
    // agent-id level and only the two filenames its loader recognizes.
    if let Ok(agents) = fs::read_dir(home.join(".openclaw/agents")) {
        for agent in agents.flatten().filter(|entry| entry.path().is_dir()) {
            for name in ["auth-profiles.json", "auth.json"] {
                let relative = PathBuf::from(".openclaw")
                    .join("agents")
                    .join(agent.file_name())
                    .join("agent")
                    .join(name);
                let source = home.join(&relative);
                if source.is_file() {
                    let instance_relative = PathBuf::from("external").join(&relative);
                    link_credential_file(&source, to_home, &instance_relative)?;
                    inherited.push(instance_relative);
                }
            }
        }
    }
    Ok(inherited)
}

fn link_credential_file(source: &Path, root: &Path, relative: &Path) -> Result<()> {
    let parent_relative = relative.parent().unwrap_or_else(|| Path::new(""));
    let parent = ensure_instance_directory(root, parent_relative)?;
    let destination = parent.join(relative.file_name().unwrap_or_default());
    let _ = fs::remove_file(&destination);
    link_file(source, &destination)
}

fn link_file(source: &Path, destination: &Path) -> Result<()> {
    // Preserve rotating credentials rather than silently creating a stale copy.
    // Windows may require Developer Mode or symlink privileges; the adapter
    // reports that error to the caller instead of copying.
    horon_agent_platform::fs::symlink_file(source, destination).map_err(launch_io)
}

fn ensure_instance_directory(root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(Error::new(
            ErrorKind::InvalidInstanceHome,
            format!("unsafe instance path: {}", relative.display()),
        ));
    }
    let mut current = root.to_path_buf();
    for part in relative.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                fs::remove_file(&current).map_err(launch_io)?;
                fs::create_dir(&current).map_err(launch_io)?;
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(Error::new(
                    ErrorKind::InvalidInstanceHome,
                    format!("instance credential path is not a directory: {}", current.display()),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(launch_io)?;
            }
            Err(error) => return Err(launch_io(error)),
        }
        set_owner_only_dir(&current)?;
    }
    Ok(current)
}

/// Progress while a shared runtime comes up.
pub type Progress<'a> = dyn Fn(&str) + 'a;

/// Whether anything is listening on a socket path.
pub fn socket_accepts(path: &Path) -> bool {
    horon_agent_transport::is_socket_path(path)
}

/// Locate a sibling executable next to our own, falling back to `$PATH`.
fn sibling_exe(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Start the user's shared daemon and API bridge if necessary.
pub fn ensure_runtime(options: &LaunchOptions, progress: &Progress<'_>) -> Result<()> {
    let api = options
        .api_socket
        .clone()
        .unwrap_or_else(horon_agent_harness_api::api_socket_path);
    if socket_accepts(&api) {
        return Ok(());
    }
    let legacy = horon_agent_harness_api::legacy_socket_path();
    let horon_agent = options.binary.clone().unwrap_or_else(|| sibling_exe("horon-agent"));
    if !socket_accepts(&legacy) {
        progress("starting horon-agent runtime...");
        spawn_detached(&horon_agent, &["serve"]).map_err(|error| {
            Error::new(
                ErrorKind::LaunchFailed,
                format!("could not start the horon-agent runtime: {error}"),
            )
        })?;
        wait_for_socket(&legacy, "horon-agent runtime", options.start_timeout)?;
    }
    progress("starting harness API bridge...");
    let standalone_bridge = sibling_exe("horon-agent-harness-api-bridge");
    let (bridge, bridge_args): (&Path, &[&str]) = if standalone_bridge.is_file() {
        (&standalone_bridge, &[])
    } else {
        // Current HoronAgent distributions host the bridge as a CLI subcommand so a
        // desktop bundle only needs to ship one companion executable. Keep the
        // standalone lookup above for compatibility with older installations.
        (&horon_agent, &["api-bridge"])
    };
    spawn_detached(bridge, bridge_args).map_err(|error| {
        Error::new(
            ErrorKind::LaunchFailed,
            format!("could not start the harness API bridge: {error}"),
        )
    })?;
    wait_for_socket(&api, "harness API bridge", options.start_timeout)
}

fn spawn_detached(program: &Path, args: &[&str]) -> std::io::Result<()> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // A GUI host (HoronAgent Desktop) has no console to share, so each
    // console-subsystem child would open its own window on Windows; there the
    // adapter gives the daemon a new process group with a hidden console. On
    // Unix the child stays in the caller's session, as the SDK has always
    // launched it, and the caller does not wait for it.
    drop(horon_agent_platform::spawn_background_consoleless(&mut command)?);
    Ok(())
}

/// Block until `path` accepts connections, or the timeout expires.
pub fn wait_for_socket(path: &Path, what: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    // Runtime startup normally takes only a few milliseconds after the first
    // failed dial. A fixed 100ms sleep therefore imposed almost exactly 100ms
    // of artificial latency on every cold desktop launch. Start aggressively,
    // then back off so a genuinely broken runtime still costs little CPU while
    // we wait for the diagnostic timeout.
    let mut retry_delay = Duration::from_millis(2);
    const MAX_RETRY_DELAY: Duration = Duration::from_millis(25);
    while Instant::now() < deadline {
        if socket_accepts(path) {
            return Ok(());
        }
        std::thread::sleep(retry_delay.min(deadline.saturating_duration_since(Instant::now())));
        retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
    }
    Err(Error::new(
        ErrorKind::LaunchFailed,
        format!("timed out waiting for the {what} at {}", path.display()),
    ))
}

fn read_daemon_pid(home: &Path, runtime_dir: &Path) -> Option<i32> {
    let raw = fs::read(home.join("servers.json")).ok()?;
    let registry: Value = serde_json::from_slice(&raw).ok()?;
    let socket = runtime_dir.join("horon-agent.sock");
    registry.as_object()?.values().find_map(|entry| {
        let entry = entry.as_object()?;
        if Path::new(entry.get("socket")?.as_str()?) != socket {
            return None;
        }
        let pid = i32::try_from(entry.get("pid")?.as_i64()?).ok()?;
        (pid > 1).then_some(pid)
    })
}

fn stop_instance_daemon(home: &Path, runtime_dir: &Path) {
    if let Some(pid) = read_daemon_pid(home, runtime_dir) {
        stop_daemon_pid(pid);
    }
}

/// Pid of a daemon already registered for this runtime directory before the
/// launch started. A failed launch must not stop that daemon.
fn registered_daemon_pid(home: &Path, runtime_dir: &Path) -> Option<i32> {
    read_daemon_pid(home, runtime_dir)
}

/// Stop the daemon a failed launch registered. A daemon that was already
/// registered before the launch belongs to someone else and is left alone.
fn stop_daemon_started_by_failed_launch(home: &Path, runtime_dir: &Path, registered_before: Option<i32>) {
    if let Some(pid) = read_daemon_pid(home, runtime_dir)
        && Some(pid) != registered_before
    {
        stop_daemon_pid(pid);
    }
}

fn stop_daemon_pid(pid: i32) {
    let pid = pid as u32;
    // Ask politely, then force: the adapter sends the termination signal, waits
    // out the grace period, and escalates to a kill the daemon cannot ignore.
    if horon_agent_platform::terminate_process_group(pid, Duration::from_secs(2)) {
        return;
    }
    // The group signal reaches nothing when the recorded daemon is not a group
    // leader - one a supervisor started, or an older build's process. Stop that
    // process itself, which is what this caller did before the adapter owned
    // process control; the shared group call stays honest about the group it
    // could not reach.
    horon_agent_platform::terminate_process(pid, Duration::from_secs(5));
}

fn remove_ephemeral_home(home: &Path, timeout: Duration) {
    let Some(name) = home.file_name().and_then(OsStr::to_str) else {
        return;
    };
    if home.parent() != Some(std::env::temp_dir().as_path()) || !name.starts_with("horon-agent-sdk-instance-") {
        return;
    }
    let deadline = Instant::now() + timeout;
    loop {
        let _ = fs::remove_dir_all(home);
        if !home.exists() || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn stderr_suffix(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(":\n{trimmed}")
    }
}

fn home_dir() -> PathBuf {
    // Both variables are plain environment entries, so reading them needs no
    // operating-system branch; Windows sets only the second one.
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Resolve the user's platform horon-agent config directory.
///
/// The per-platform layout (macOS application support, Windows roaming data,
/// the XDG convention elsewhere) belongs to the platform adapter; a process with
/// no home directory falls back to a relative path, as it always has.
pub fn user_app_config_dir() -> PathBuf {
    horon_agent_platform::paths::user_config_dir("horon-agent")
        .unwrap_or_else(|| PathBuf::from(".").join("horon-agent"))
}

fn set_owner_only_dir(path: &Path) -> Result<()> {
    horon_agent_platform::fs::set_dir_permissions_owner_only(path).map_err(launch_io)
}

fn set_owner_only_file(path: &Path) -> Result<()> {
    horon_agent_platform::fs::set_file_permissions_owner_only(path).map_err(launch_io)
}

fn launch_io(error: std::io::Error) -> Error {
    Error::new(ErrorKind::LaunchFailed, error.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn swarm_model_reaches_runtime_and_overrides_generic_environment() {
        use std::os::unix::fs::PermissionsExt;

        let sandbox = tempfile::tempdir().expect("sandbox");
        let binary = sandbox.path().join("capture-env");
        let captured = sandbox.path().join("swarm-model.txt");
        fs::write(
            &binary,
            "#!/bin/sh\nprintf '%s' \"$HORON_AGENT_SWARM_MODEL\" > \"$CAPTURE_PATH\"\nexit 1\n",
        )
        .expect("write fake runtime");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).expect("make fake runtime executable");

        let mut options = LaunchOptions {
            horon_agent_home: Some(sandbox.path().join("instance")),
            inherit_logins: false,
            binary: Some(binary),
            swarm_model: Some("inherit".to_string()),
            startup_timeout: Duration::from_secs(2),
            ..LaunchOptions::default()
        };
        options
            .env
            .insert(OsString::from("CAPTURE_PATH"), captured.as_os_str().to_owned());
        options.env.insert(
            OsString::from("HORON_AGENT_SWARM_MODEL"),
            OsString::from("unwanted-model"),
        );

        assert!(launch_instance(&options).is_err(), "fake runtime should fail startup");
        assert_eq!(fs::read_to_string(captured).expect("captured model"), "inherit");
    }

    #[test]
    fn wake_mode_reaches_runtime_and_overrides_generic_environment() {
        use std::os::unix::fs::PermissionsExt;

        let sandbox = tempfile::tempdir().expect("sandbox");
        let binary = sandbox.path().join("capture-env");
        let captured = sandbox.path().join("wake-mode.txt");
        fs::write(
            &binary,
            "#!/bin/sh\nprintf '%s' \"$HORON_AGENT_WAKE_MODE\" > \"$CAPTURE_PATH\"\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let mut options = LaunchOptions {
            horon_agent_home: Some(sandbox.path().join("instance")),
            inherit_logins: false,
            binary: Some(binary),
            wake_mode: Some(WakeMode::External),
            startup_timeout: Duration::from_secs(2),
            ..LaunchOptions::default()
        };
        options
            .env
            .insert("CAPTURE_PATH".into(), captured.as_os_str().to_owned());
        options.env.insert("HORON_AGENT_WAKE_MODE".into(), "internal".into());
        assert!(launch_instance(&options).is_err());
        assert_eq!(fs::read_to_string(captured).unwrap(), "external");
    }

    #[test]
    fn private_instances_disable_auto_update() {
        use std::os::unix::fs::PermissionsExt;

        let sandbox = tempfile::tempdir().expect("sandbox");
        let binary = sandbox.path().join("capture-env");
        let captured = sandbox.path().join("auto-update.txt");
        fs::write(
            &binary,
            "#!/bin/sh\nprintf '%s|%s' \"${HORON_AGENT_NO_AUTO_UPDATE-unset}\" \"$*\" > \"$CAPTURE_PATH\"\nexit 1\n",
        )
        .expect("write fake runtime");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).expect("make fake runtime executable");

        let mut options = LaunchOptions {
            horon_agent_home: Some(sandbox.path().join("instance")),
            inherit_logins: false,
            binary: Some(binary),
            startup_timeout: Duration::from_secs(2),
            ..LaunchOptions::default()
        };
        options
            .env
            .insert(OsString::from("CAPTURE_PATH"), captured.as_os_str().to_owned());

        assert!(launch_instance(&options).is_err(), "fake runtime should fail startup");
        assert_eq!(
            fs::read_to_string(captured).expect("captured bridge invocation"),
            format!(
                "1|--no-update api-bridge --api-socket {}",
                sandbox.path().join("instance/run/horon-agent-api.sock").display()
            )
        );
    }

    #[test]
    fn dropping_an_ephemeral_instance_stops_its_bridge_and_removes_its_home() {
        let home = tempfile::Builder::new()
            .prefix("horon-agent-sdk-instance-")
            .tempdir()
            .expect("temp home")
            .keep();
        let runtime_dir = home.join("run");
        fs::create_dir(&runtime_dir).expect("runtime dir");
        let child = Command::new("sleep").arg("30").spawn().expect("sleep child");
        let pid = child.id() as i32;
        let instance = LaunchedInstance {
            socket_path: runtime_dir.join("horon-agent-api.sock"),
            horon_agent_home: home.clone(),
            runtime_dir,
            child: Mutex::new(Some(child)),
            ephemeral: true,
            cleanup_timeout: Duration::ZERO,
            stopped: AtomicBool::new(false),
        };

        drop(instance);

        assert!(
            !horon_agent_platform::is_process_running(pid as u32),
            "owned bridge process survived Drop"
        );
        assert!(!home.exists(), "ephemeral instance home survived Drop");
    }

    /// A daemon owned by the test. Its reaper thread reaps the child as soon as it
    /// exits, so a stopped daemon is never left as a zombie that still answers
    /// `kill(pid, 0)`. The reaper runs on its own thread because `launch_instance`
    /// stops daemons synchronously, before it returns to the test.
    struct StandInDaemon {
        pid: i32,
        exited: std::sync::mpsc::Receiver<()>,
    }

    impl StandInDaemon {
        fn spawn() -> Self {
            let mut child = Command::new("sleep")
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("stand-in daemon");
            let pid = child.id() as i32;
            let (exited_tx, exited) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    match child.try_wait() {
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(25));
                        }
                        Ok(Some(_)) => break,
                        // Never leak the stand-in, even when nothing stopped it.
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                    }
                }
                let _ = exited_tx.send(());
            });
            Self { pid, exited }
        }

        fn is_running(&self) -> bool {
            matches!(self.exited.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty))
        }

        fn wait_for_exit(&self, timeout: Duration) -> bool {
            self.exited.recv_timeout(timeout).is_ok()
        }
    }

    impl Drop for StandInDaemon {
        fn drop(&mut self) {
            if self.is_running() {
                // SAFETY: the reaper has not reaped the child yet, so this pid still names it.
                unsafe {
                    libc::kill(self.pid, libc::SIGKILL);
                }
            }
        }
    }

    /// Registers the stand-in daemon for this runtime, as a real bridge's daemon would.
    const REGISTER_DAEMON: &str = r#"printf '{"x":{"socket":"%s","pid":%s}}' "$HORON_AGENT_RUNTIME_DIR/horon-agent.sock" "$DAEMON_PID" > "$HORON_AGENT_HOME/servers.json""#;

    /// Launch a fake `horon-agent` whose body is `script`; `DAEMON_PID` names `daemon`.
    fn launch_with_fake_runtime(
        sandbox: &Path,
        script: &str,
        daemon: &StandInDaemon,
        horon_agent_home: Option<PathBuf>,
        startup_timeout: Duration,
    ) -> Result<LaunchedInstance> {
        use std::os::unix::fs::PermissionsExt;

        let binary = sandbox.join("fake-horon-agent");
        fs::write(&binary, format!("#!/bin/sh\n{script}\n")).expect("write fake runtime");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).expect("make fake runtime executable");
        let mut options = LaunchOptions {
            horon_agent_home,
            inherit_logins: false,
            binary: Some(binary),
            startup_timeout,
            ..LaunchOptions::default()
        };
        options
            .env
            .insert(OsString::from("DAEMON_PID"), OsString::from(daemon.pid.to_string()));
        launch_instance(&options)
    }

    #[test]
    fn failed_startup_stops_daemon_when_horon_agent_exits() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let daemon = StandInDaemon::spawn();
        let error = launch_with_fake_runtime(
            sandbox.path(),
            &format!("{REGISTER_DAEMON}\nexit 1"),
            &daemon,
            None,
            Duration::from_secs(5),
        )
        .err()
        .expect("a runtime that exits should fail startup");
        assert_eq!(error.kind, ErrorKind::StartupFailed);
        assert!(
            daemon.wait_for_exit(Duration::from_secs(2)),
            "daemon registered by the failed runtime survived startup failure"
        );
    }

    #[test]
    fn failed_startup_stops_daemon_when_socket_never_appears() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let daemon = StandInDaemon::spawn();
        let error = launch_with_fake_runtime(
            sandbox.path(),
            &format!("{REGISTER_DAEMON}\nexec sleep 30"),
            &daemon,
            None,
            Duration::from_secs(2),
        )
        .err()
        .expect("a runtime without an API socket should time out");
        assert_eq!(error.kind, ErrorKind::StartupTimeout);
        assert!(
            daemon.wait_for_exit(Duration::from_secs(2)),
            "daemon registered by the timed-out runtime survived startup failure"
        );
    }

    #[test]
    fn failed_startup_leaves_a_daemon_registered_before_the_launch_running() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let home = sandbox.path().join("instance");
        let runtime_dir = home.join("run");
        fs::create_dir_all(&runtime_dir).expect("runtime dir");
        let daemon = StandInDaemon::spawn();
        // A caller-supplied home can already register a daemon from an earlier run.
        let registry = serde_json::json!({
            "x": {
                "socket": runtime_dir.join("horon-agent.sock").display().to_string(),
                "pid": daemon.pid,
            }
        });
        fs::write(home.join("servers.json"), registry.to_string()).expect("registry");
        let error = launch_with_fake_runtime(sandbox.path(), "exit 1", &daemon, Some(home), Duration::from_secs(5))
            .err()
            .expect("a runtime that exits should fail startup");
        assert_eq!(error.kind, ErrorKind::StartupFailed);
        assert!(daemon.is_running(), "failed launch stopped a daemon it did not start");
    }
}
