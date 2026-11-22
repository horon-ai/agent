//! Hosted per-tenant instances: one customer, one home volume, one runtime.
//!
//! [`launch_hosted_instance`] starts a daemon whose `HORON_HOME` is a directory
//! on the tenant's volume and whose sockets live on a short path outside that
//! volume. It never inherits the operator's logins, is never ephemeral, and the
//! hosting mode and tenant are fixed by this code rather than by the caller.
//!
//! [`stop_hosted_instance`] is the suspend path: the daemon receives the
//! ordinary termination signal, drains its session saves, and the call returns
//! only once its endpoint stopped accepting. [`delete_hosted_instance`] stops
//! the instance and removes exactly that tenant's directory after proving the
//! path did not leave the volume root through a link.

use crate::errors::{Error, ErrorKind, Result};
use crate::launch::{LaunchOptions, LaunchedInstance, launch_instance, socket_accepts, stop_instance_daemon};
use horon_agent_base::hosting::{HOSTING_MODE_ENV, HostingMode, MAX_TENANT_ID_LEN, TENANT_ID_ENV, TenantId};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Longest socket path this module accepts, in bytes.
///
/// The Unix limit is 104 bytes on macOS and 108 on Linux, and both include the
/// terminating NUL. A generated runtime directory sits under the platform
/// temporary directory, so this bound leaves room for the longest of the two
/// endpoint names.
const MAX_SOCKET_PATH_BYTES: usize = 100;

/// Endpoint name inside the runtime directory.
const API_SOCKET_NAME: &str = "horon-agent-api.sock";

/// How long a stop waits for the endpoint to disappear after the signal.
const STOP_TIMEOUT: Duration = Duration::from_secs(15);

/// Variables a hosted launch owns. A caller cannot override them, because a
/// client that could point `HORON_HOME` elsewhere would defeat the boundary.
const RESERVED_ENV: &[&str] = &[
    "HORON_HOME",
    "HORON_AGENT_RUNTIME_DIR",
    "HORON_AGENT_API_SOCKET",
    "HORON_AGENT_SOCKET",
    "HORON_AGENT_NO_AUTO_UPDATE",
    "HORON_PROFILE",
    HOSTING_MODE_ENV,
    TENANT_ID_ENV,
];

/// How to start one tenant's instance.
#[derive(Debug, Clone)]
pub struct HostedLaunchOptions {
    /// Volume root that holds one directory per tenant, such as `/data`.
    pub root: PathBuf,
    /// Validated tenant id; names `<root>/<tenant>`.
    pub tenant: String,
    /// horon-agent executable. Defaults to `horon-agent` on `$PATH`.
    pub binary: Option<PathBuf>,
    /// Working directory for sessions. Defaults to the tenant home.
    pub working_dir: Option<PathBuf>,
    /// Base for the hash-shortened runtime directory. Defaults to the platform
    /// temporary directory; a test or a container entrypoint may choose `/run`.
    pub runtime_base: Option<PathBuf>,
    /// Extra environment for the instance. Reserved variables are ignored.
    pub env: HashMap<OsString, OsString>,
    /// How long to wait for the API socket to accept connections.
    pub startup_timeout: Duration,
    /// Forward the bridge's stderr instead of capturing it for startup errors.
    pub inherit_stderr: bool,
    /// Client identity used by the SDK client.
    pub client_name: String,
    /// Request timeout used by the SDK client.
    pub request_timeout: Option<Duration>,
}

impl HostedLaunchOptions {
    /// Options for `tenant` under `root` with production defaults.
    pub fn new(root: impl Into<PathBuf>, tenant: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            tenant: tenant.into(),
            binary: None,
            working_dir: None,
            runtime_base: None,
            env: HashMap::new(),
            startup_timeout: Duration::from_secs(30),
            inherit_stderr: false,
            client_name: concat!("horon-agent-sdk-rs/", env!("CARGO_PKG_VERSION")).to_string(),
            request_timeout: Some(Duration::from_secs(30)),
        }
    }
}

/// The paths that belong to one tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedTenantPaths {
    /// `<root>/<tenant>`, the directory deletion removes.
    pub instance: PathBuf,
    /// `<root>/<tenant>/home`, the `HORON_HOME` volume.
    pub home: PathBuf,
    /// Hash-shortened runtime directory, never inside `home`.
    pub runtime_dir: PathBuf,
    /// API endpoint the launch waits on.
    pub api_socket: PathBuf,
}

/// Derive a tenant's paths without touching the filesystem.
///
/// `runtime_base` is the parent of the hash-shortened runtime directory, chosen
/// by the caller so a container can keep sockets under `/run`. The directory
/// name is a digest of the home path, which keeps the socket short and gives
/// every tenant on one host a distinct endpoint.
pub fn tenant_paths(root: &Path, tenant: &str, runtime_base: &Path) -> Result<HostedTenantPaths> {
    let tenant = TenantId::parse(tenant).map_err(|error| {
        Error::new(
            ErrorKind::InvalidInstanceHome,
            format!("invalid tenant id: {error} (maximum {MAX_TENANT_ID_LEN} bytes)"),
        )
    })?;
    if !root.is_absolute() {
        return Err(Error::new(
            ErrorKind::InvalidInstanceHome,
            format!("hosted volume root must be an absolute path, got {}", root.display()),
        ));
    }
    let instance = root.join(tenant.as_str());
    let home = instance.join("home");
    let runtime_dir = runtime_base.join(format!("horon-tenant-{}", tenant_digest(&home)));
    let api_socket = runtime_dir.join(API_SOCKET_NAME);
    let socket_bytes = api_socket.as_os_str().len();
    if socket_bytes > MAX_SOCKET_PATH_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidInstanceHome,
            format!(
                "hosted socket path is {socket_bytes} bytes, over the {MAX_SOCKET_PATH_BYTES}-byte limit: {}",
                api_socket.display()
            ),
        ));
    }
    Ok(HostedTenantPaths {
        instance,
        home,
        runtime_dir,
        api_socket,
    })
}

/// Start the tenant's daemon and API bridge and return once its socket accepts.
pub fn launch_hosted_instance(options: &HostedLaunchOptions) -> Result<LaunchedInstance> {
    let runtime_base = options.runtime_base.clone().unwrap_or_else(std::env::temp_dir);
    let paths = tenant_paths(&options.root, &options.tenant, &runtime_base)?;
    refuse_symlinked_instance(&paths.instance)?;
    for directory in [&paths.instance, &paths.home, &paths.runtime_dir] {
        fs::create_dir_all(directory).map_err(launch_io)?;
        horon_agent_platform::fs::set_dir_permissions_owner_only(directory).map_err(launch_io)?;
    }

    let mut env: HashMap<OsString, OsString> = options
        .env
        .iter()
        .filter(|(key, _)| match key.to_str() {
            Some(key) => !RESERVED_ENV.contains(&key),
            None => true,
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    env.insert(
        OsString::from(HOSTING_MODE_ENV),
        OsString::from(HostingMode::PerUserContainer.as_str()),
    );
    env.insert(OsString::from(TENANT_ID_ENV), OsString::from(options.tenant.as_str()));

    let launch = LaunchOptions {
        horon_agent_home: Some(paths.home.clone()),
        runtime_dir: Some(paths.runtime_dir.clone()),
        working_dir: Some(options.working_dir.clone().unwrap_or_else(|| paths.home.clone())),
        // A tenant must never see the operator's logins or mutable config.
        inherit_logins: false,
        binary: options.binary.clone(),
        env,
        startup_timeout: options.startup_timeout,
        inherit_stderr: options.inherit_stderr,
        client_name: options.client_name.clone(),
        request_timeout: options.request_timeout,
        ..LaunchOptions::default()
    };
    launch_instance(&launch)
}

/// Stop the tenant's daemon, letting it drain session saves, and wait until its
/// endpoint stops accepting. Idempotent when nothing is running.
pub fn stop_hosted_instance(root: &Path, tenant: &str, runtime_base: &Path) -> Result<()> {
    let paths = tenant_paths(root, tenant, runtime_base)?;
    stop_paths(&paths)
}

/// Stop and delete exactly one tenant's instance directory.
///
/// The runtime directory and both socket files are removed too; nothing outside
/// `<root>/<tenant>` and the derived runtime path is touched.
pub fn delete_hosted_instance(root: &Path, tenant: &str, runtime_base: &Path) -> Result<()> {
    let paths = tenant_paths(root, tenant, runtime_base)?;
    match fs::symlink_metadata(&paths.instance) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(Error::new(
                ErrorKind::InvalidInstanceHome,
                format!("refusing to delete a symbolic link: {}", paths.instance.display()),
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(Error::new(
                ErrorKind::InvalidInstanceHome,
                format!("refusing to delete a non-directory: {}", paths.instance.display()),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            remove_runtime_dir(&paths.runtime_dir).map_err(launch_io)?;
            return Ok(());
        }
        Err(error) => return Err(launch_io(error)),
    }

    stop_paths(&paths)?;

    let resolved_root = fs::canonicalize(root).map_err(launch_io)?;
    let resolved_instance = fs::canonicalize(&paths.instance).map_err(launch_io)?;
    if resolved_instance == resolved_root || !resolved_instance.starts_with(&resolved_root) {
        return Err(Error::new(
            ErrorKind::InvalidInstanceHome,
            format!(
                "refusing to delete {}: it does not resolve inside {}",
                paths.instance.display(),
                root.display()
            ),
        ));
    }
    fs::remove_dir_all(&resolved_instance).map_err(launch_io)?;
    remove_runtime_dir(&paths.runtime_dir).map_err(launch_io)?;
    Ok(())
}

fn stop_paths(paths: &HostedTenantPaths) -> Result<()> {
    let daemon_socket = paths.runtime_dir.join("horon-agent.sock");
    let live = socket_accepts(&paths.api_socket) || socket_accepts(&daemon_socket);
    if !live {
        return Ok(());
    }
    // SIGTERM reaches the daemon's drain (session saves are flushed) before it
    // exits, so the caller may treat the returned stop as a durable one.
    stop_instance_daemon(&paths.home, &paths.runtime_dir);
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !socket_accepts(&paths.api_socket) && !socket_accepts(&daemon_socket) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(Error::new(
        ErrorKind::LaunchFailed,
        format!(
            "the hosted instance for {} did not stop within {:?}",
            paths.home.display(),
            STOP_TIMEOUT
        ),
    ))
}

fn remove_runtime_dir(runtime_dir: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(runtime_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Ok(()),
        Ok(_) => fs::remove_dir_all(runtime_dir),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn refuse_symlinked_instance(instance: &Path) -> Result<()> {
    match fs::symlink_metadata(instance) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(Error::new(
            ErrorKind::InvalidInstanceHome,
            format!(
                "hosted instance directory must not be a symbolic link: {}",
                instance.display()
            ),
        )),
        Ok(metadata) if !metadata.is_dir() => Err(Error::new(
            ErrorKind::InvalidInstanceHome,
            format!("hosted instance path must be a directory: {}", instance.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(launch_io(error)),
    }
}

/// A 12-hex-digit digest of the home path.
///
/// FNV-1a is enough here: the value only has to be stable, short and distinct
/// per tenant on one host, and a collision costs a refused socket rather than a
/// boundary bypass because the directory is still derived from one home.
fn tenant_digest(home: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in home.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:012x}", hash & 0x0000_ffff_ffff_ffff)
}

fn launch_io(error: std::io::Error) -> Error {
    Error::new(ErrorKind::LaunchFailed, error.to_string())
}

#[cfg(all(test, unix))]
#[path = "launch_hosted_tests.rs"]
mod tests;
