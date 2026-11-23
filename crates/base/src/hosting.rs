//! Resolve the process-wide hosting policy and confirm its execution boundary.
//!
//! [`resolve`] turns the environment values and the `[hosting]` section into a
//! [`HostingPolicy`]; it touches nothing but its arguments, so a test drives it
//! without a global environment. [`confirm_container_boundary`] is the startup
//! refusal for a per-user container: a hosted daemon must prove it owns an
//! owner-only home volume, a separate runtime directory, no TCP gateway and a
//! tenant id, or it exits instead of serving a customer from the operator's
//! home.
//!
//! [`current`] caches the resolved policy for the process. Production callers
//! use it; tests call the pieces with explicit inputs.

pub use horon_agent_config_types::hosting::{
    HostingConfigError, HostingMode, HostingPolicy, HostingSection, MAX_TENANT_ID_LEN, TenantId,
};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

/// The only authority for the hosting mode.
pub const HOSTING_MODE_ENV: &str = "HORON_AGENT_HOSTING_MODE";
/// The tenant id this process serves. An audit label; never a session lookup key.
pub const TENANT_ID_ENV: &str = "HORON_AGENT_TENANT_ID";

/// Everything resolution reads, injected so tests need no global state.
#[derive(Debug, Clone, Default)]
pub struct HostingInputs {
    /// Raw `HORON_AGENT_HOSTING_MODE`, absent when unset.
    pub mode: Option<String>,
    /// Raw `HORON_AGENT_TENANT_ID`, absent when unset.
    pub tenant_id: Option<String>,
    /// The `[hosting]` section; may only narrow.
    pub section: HostingSection,
}

impl HostingInputs {
    /// Read the values this process was started with.
    pub fn from_environment() -> Self {
        Self {
            mode: env_value(HOSTING_MODE_ENV),
            tenant_id: env_value(TENANT_ID_ENV),
            section: section_from_config_file(),
        }
    }
}

/// A variable's value, or `None` when it is unset or not UTF-8.
///
/// Reading `var_os` first keeps "unset" and "not text" the same outcome without
/// collapsing a set-but-empty value, which the resolver rejects by name.
fn env_value(key: &str) -> Option<String> {
    std::env::var_os(key)?.to_str().map(str::to_string)
}

/// Resolve the policy from raw values. Absent mode means no hosting.
pub fn resolve(inputs: &HostingInputs) -> Result<HostingPolicy, HostingConfigError> {
    let Some(raw_mode) = inputs.mode.as_deref() else {
        return HostingPolicy::new(HostingMode::None, None, &inputs.section);
    };
    let mode = HostingMode::parse(raw_mode)?;
    let tenant_id = match inputs.tenant_id.as_deref() {
        Some(raw) => Some(TenantId::parse(raw)?),
        None => None,
    };
    HostingPolicy::new(mode, tenant_id, &inputs.section)
}

/// Paths and daemon settings the container boundary inspects.
#[derive(Debug, Clone)]
pub struct HostingPaths {
    /// `HORON_HOME` as set, `None` when the variable is absent.
    pub home: Option<PathBuf>,
    /// `HORON_AGENT_RUNTIME_DIR` as set, `None` when absent.
    pub runtime_dir: Option<PathBuf>,
    /// `$HOME/.horon` for the user that owns the process image.
    pub operator_home: PathBuf,
    /// Whether the TCP gateway from `[gateway]` is enabled.
    pub gateway_enabled: bool,
}

/// The operator home a hosted container must not use.
pub fn operator_default_home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".horon")
}

/// Read the boundary inputs from the environment and the resolved config.
pub fn boundary_paths(gateway_enabled: bool) -> HostingPaths {
    HostingPaths {
        home: std::env::var_os("HORON_HOME").map(PathBuf::from),
        runtime_dir: std::env::var_os("HORON_AGENT_RUNTIME_DIR").map(PathBuf::from),
        operator_home: operator_default_home(),
        gateway_enabled,
    }
}

/// Refuse to start a per-user container whose boundary is not provable.
///
/// Only `PerUserContainer` is checked here. A shared worker starts so its own
/// MCP and child-process gate can refuse the operations it must not run.
pub fn confirm_container_boundary(policy: &HostingPolicy, paths: &HostingPaths) -> Result<(), HostingBoundaryError> {
    if !policy.is_per_user_container() {
        return Ok(());
    }
    if policy.tenant_id().is_none() {
        return Err(HostingBoundaryError::MissingTenantId);
    }

    let home = paths.home.as_deref().ok_or(HostingBoundaryError::MissingHome)?;
    if !home.is_absolute() {
        return Err(HostingBoundaryError::HomeNotAbsolute(home.to_path_buf()));
    }
    let home_metadata =
        std::fs::symlink_metadata(home).map_err(|_| HostingBoundaryError::HomeMissing(home.to_path_buf()))?;
    if home_metadata.file_type().is_symlink() {
        return Err(HostingBoundaryError::HomeIsSymlink(home.to_path_buf()));
    }
    if !home_metadata.is_dir() {
        return Err(HostingBoundaryError::HomeNotADirectory(home.to_path_buf()));
    }
    if !horon_agent_platform::fs::is_private_dir(&home_metadata) {
        return Err(HostingBoundaryError::HomeNotOwnerOnly {
            home: home.to_path_buf(),
            mode: horon_agent_platform::fs::mode_bits(&home_metadata),
        });
    }
    let resolved_home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let resolved_operator = std::fs::canonicalize(&paths.operator_home).unwrap_or_else(|_| paths.operator_home.clone());
    if resolved_home == resolved_operator {
        return Err(HostingBoundaryError::OperatorHome(home.to_path_buf()));
    }

    let runtime_dir = paths
        .runtime_dir
        .as_deref()
        .ok_or(HostingBoundaryError::MissingRuntimeDir)?;
    if !runtime_dir.is_absolute() {
        return Err(HostingBoundaryError::RuntimeNotAbsolute(runtime_dir.to_path_buf()));
    }
    if runtime_dir == home || runtime_dir.starts_with(home) {
        return Err(HostingBoundaryError::RuntimeInsideHome(runtime_dir.to_path_buf()));
    }
    match std::fs::symlink_metadata(runtime_dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(HostingBoundaryError::RuntimeIsSymlink(runtime_dir.to_path_buf()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(HostingBoundaryError::RuntimeUnusable {
                runtime: runtime_dir.to_path_buf(),
                reason: error.to_string(),
            });
        }
    }
    let candidate_runtime = resolve_candidate_path(runtime_dir);
    if candidate_runtime == resolved_home || candidate_runtime.starts_with(&resolved_home) {
        return Err(HostingBoundaryError::RuntimeInsideHome(runtime_dir.to_path_buf()));
    }
    std::fs::create_dir_all(runtime_dir).map_err(|error| HostingBoundaryError::RuntimeUnusable {
        runtime: runtime_dir.to_path_buf(),
        reason: error.to_string(),
    })?;
    let runtime_metadata =
        std::fs::symlink_metadata(runtime_dir).map_err(|error| HostingBoundaryError::RuntimeUnusable {
            runtime: runtime_dir.to_path_buf(),
            reason: error.to_string(),
        })?;
    if runtime_metadata.file_type().is_symlink() {
        return Err(HostingBoundaryError::RuntimeIsSymlink(runtime_dir.to_path_buf()));
    }
    let resolved_runtime = std::fs::canonicalize(runtime_dir).unwrap_or_else(|_| runtime_dir.to_path_buf());
    if resolved_runtime == resolved_home || resolved_runtime.starts_with(&resolved_home) {
        return Err(HostingBoundaryError::RuntimeInsideHome(runtime_dir.to_path_buf()));
    }
    if !runtime_metadata.is_dir() || !horon_agent_platform::fs::is_private_dir(&runtime_metadata) {
        horon_agent_platform::fs::set_dir_permissions_owner_only(runtime_dir).map_err(|error| {
            HostingBoundaryError::RuntimeUnusable {
                runtime: runtime_dir.to_path_buf(),
                reason: error.to_string(),
            }
        })?;
        let runtime_metadata =
            std::fs::symlink_metadata(runtime_dir).map_err(|error| HostingBoundaryError::RuntimeUnusable {
                runtime: runtime_dir.to_path_buf(),
                reason: error.to_string(),
            })?;
        if !runtime_metadata.is_dir() || !horon_agent_platform::fs::is_private_dir(&runtime_metadata) {
            return Err(HostingBoundaryError::RuntimeNotOwnerOnly {
                runtime: runtime_dir.to_path_buf(),
                mode: horon_agent_platform::fs::mode_bits(&runtime_metadata),
            });
        }
    }

    if paths.gateway_enabled {
        return Err(HostingBoundaryError::GatewayEnabled);
    }
    Ok(())
}

/// Resolve a candidate path before creating missing leaf directories so symlinked
/// ancestors cannot place `runtime_dir` inside `HORON_HOME`.
fn resolve_candidate_path(path: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(part) => {
                resolved.push(part);
                if let Ok(canonical) = std::fs::canonicalize(&resolved) {
                    resolved = canonical;
                }
            }
        }
    }
    resolved
}

/// Derive the 12-hex-digit runtime directory digest for a tenant home path.
///
/// Uses the first 12 lowercase hex characters of SHA-256 over the raw path
/// bytes so both `horon-agent-sdk` and `scripts/hosting/instance.sh` derive the
/// identical `horon-tenant-<12hex>` runtime directory.
pub fn tenant_runtime_digest(home: &Path) -> String {
    let digest = Sha256::digest(home.as_os_str().as_encoded_bytes());
    hex::encode(&digest[..6])
}

/// The process-wide policy, resolved once.
///
/// A daemon resolves it at startup and #668/#669 read it from here. An invalid
/// value is returned every time rather than cached as a panic, so the caller
/// reports it and exits.
pub fn current() -> Result<&'static HostingPolicy, HostingConfigError> {
    static CURRENT: LazyLock<Result<HostingPolicy, HostingConfigError>> =
        LazyLock::new(|| resolve(&HostingInputs::from_environment()));
    CURRENT.as_ref().map_err(Clone::clone)
}

/// Read the `[hosting]` section, warning once about an ignored `mode`.
///
/// The file is read through the same path the daemon config uses. Parse errors
/// are the config loader's to report; this reader contributes nothing rather
/// than turning a typo into a second failure.
fn section_from_config_file() -> HostingSection {
    let Some(path) = crate::config::Config::path() else {
        return HostingSection::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HostingSection::default();
    };
    if !text.contains("hosting") && !text.contains("\\u") && !text.contains("\\U") {
        return HostingSection::default();
    }
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct ConfigHostingSlice {
        hosting: Option<HostingSection>,
    }
    match toml::from_str::<ConfigHostingSlice>(&text) {
        Ok(slice) => {
            let Some(section) = slice.hosting else {
                return HostingSection::default();
            };
            if section.mode.is_some() {
                crate::logging::warn(&format!(
                    "Ignoring [hosting] mode in {}: {HOSTING_MODE_ENV} is the only authority for the hosting mode",
                    path.display()
                ));
            }
            section
        }
        Err(_) => {
            let Ok(document) = text.parse::<toml::Value>() else {
                return HostingSection::default();
            };
            let Some(table) = document.get("hosting") else {
                return HostingSection::default();
            };
            if table.get("mode").is_some() {
                crate::logging::warn(&format!(
                    "Ignoring [hosting] mode in {}: {HOSTING_MODE_ENV} is the only authority for the hosting mode",
                    path.display()
                ));
            }
            match table.clone().try_into::<HostingSection>() {
                Ok(section) => section,
                Err(error) => {
                    crate::logging::warn(&format!("Ignoring [hosting] section in {}: {error}", path.display()));
                    HostingSection::default()
                }
            }
        }
    }
}

/// Why a per-user container refused to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostingBoundaryError {
    MissingTenantId,
    MissingHome,
    HomeNotAbsolute(PathBuf),
    HomeMissing(PathBuf),
    HomeIsSymlink(PathBuf),
    HomeNotADirectory(PathBuf),
    HomeNotOwnerOnly { home: PathBuf, mode: Option<u32> },
    OperatorHome(PathBuf),
    MissingRuntimeDir,
    RuntimeNotAbsolute(PathBuf),
    RuntimeInsideHome(PathBuf),
    RuntimeIsSymlink(PathBuf),
    RuntimeNotOwnerOnly { runtime: PathBuf, mode: Option<u32> },
    RuntimeUnusable { runtime: PathBuf, reason: String },
    GatewayEnabled,
}

fn describe_mode(mode: Option<u32>) -> String {
    match mode {
        Some(bits) => format!("{bits:04o}"),
        None => "unknown".to_string(),
    }
}

impl std::fmt::Display for HostingBoundaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingTenantId => write!(formatter, "hosted container has no tenant id; set {TENANT_ID_ENV}"),
            Self::MissingHome => write!(
                formatter,
                "hosted container has no HORON_HOME; point it at the tenant's home volume"
            ),
            Self::HomeNotAbsolute(home) => write!(
                formatter,
                "hosted HORON_HOME must be an absolute path, got {}",
                home.display()
            ),
            Self::HomeMissing(home) => write!(
                formatter,
                "hosted HORON_HOME does not exist or cannot be read: {}",
                home.display()
            ),
            Self::HomeIsSymlink(home) => write!(
                formatter,
                "hosted HORON_HOME must not be a symbolic link: {}",
                home.display()
            ),
            Self::HomeNotADirectory(home) => {
                write!(formatter, "hosted HORON_HOME must be a directory: {}", home.display())
            }
            Self::HomeNotOwnerOnly { home, mode } => write!(
                formatter,
                "hosted HORON_HOME must be owner-only (0700), found mode {}: {}",
                describe_mode(*mode),
                home.display()
            ),
            Self::OperatorHome(home) => write!(
                formatter,
                "hosted HORON_HOME must not be the operator's own home: {}",
                home.display()
            ),
            Self::MissingRuntimeDir => write!(
                formatter,
                "hosted container has no HORON_AGENT_RUNTIME_DIR; keep sockets off the home volume"
            ),
            Self::RuntimeNotAbsolute(runtime) => write!(
                formatter,
                "hosted runtime directory must be an absolute path, got {}",
                runtime.display()
            ),
            Self::RuntimeInsideHome(runtime) => write!(
                formatter,
                "hosted runtime directory must be separate from HORON_HOME, got {}",
                runtime.display()
            ),
            Self::RuntimeIsSymlink(runtime) => write!(
                formatter,
                "hosted runtime directory must not be a symbolic link: {}",
                runtime.display()
            ),
            Self::RuntimeNotOwnerOnly { runtime, mode } => write!(
                formatter,
                "hosted runtime directory must be owner-only (0700), found mode {}: {}",
                describe_mode(*mode),
                runtime.display()
            ),
            Self::RuntimeUnusable { runtime, reason } => write!(
                formatter,
                "hosted runtime directory {} is unusable: {reason}",
                runtime.display()
            ),
            Self::GatewayEnabled => write!(
                formatter,
                "the TCP gateway must be disabled in a hosted container; it binds an address outside the container's socket boundary"
            ),
        }
    }
}

impl std::error::Error for HostingBoundaryError {}

#[cfg(all(test, unix))]
#[path = "hosting_tests.rs"]
mod tests;
