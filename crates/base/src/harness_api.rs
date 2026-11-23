//! Resolve how the harness API is exposed: the local socket only, or a TCP
//! listener that authenticates every connection with a bearer credential.
//!
//! The API bridge is a short-lived process started by a client, an SDK or a
//! container entrypoint, so it reads this once at startup instead of through
//! the cached [`crate::config::Config`]. That keeps the `[harness_api]` table
//! and the environment from widening anything behind the daemon's back, which
//! is the same shape `[hosting]` uses (#663).
//!
//! - `HORON_AGENT_API_BIND` (`IP:PORT`) and `--api-bind` name the TCP address.
//! - `HORON_AGENT_API_INSECURE_BEHIND_PROXY` and `--api-insecure-behind-proxy`
//!   acknowledge that a TLS-terminating proxy fronts that address.
//! - The `[harness_api]` section supplies both when neither is given.
//!
//! Resolution is pure: [`resolve`] takes the already-merged values and decides.
//! The safety rule lives here so the CLI, the bridge binary and tests cannot
//! each invent their own: a non-loopback bind needs at least one issued
//! credential *and* the proxy acknowledgement, and a loopback TCP bind needs a
//! credential too because every TCP connection authenticates.

pub use horon_agent_config_types::HarnessApiConfig;
use std::net::SocketAddr;

/// Environment override for the TCP address, e.g. `0.0.0.0:7946`.
pub const API_BIND_ENV: &str = "HORON_AGENT_API_BIND";
/// Environment override for the proxy acknowledgement (`1`/`true`).
pub const API_INSECURE_BEHIND_PROXY_ENV: &str = "HORON_AGENT_API_INSECURE_BEHIND_PROXY";

/// The resolved TCP exposure of the harness API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpExposure {
    bind: SocketAddr,
    insecure_behind_proxy: bool,
}

impl TcpExposure {
    /// The address the bridge binds.
    pub fn bind(&self) -> SocketAddr {
        self.bind
    }

    /// Whether the operator acknowledged a TLS-terminating proxy in front.
    pub fn insecure_behind_proxy(&self) -> bool {
        self.insecure_behind_proxy
    }

    /// Whether this address stays on the local machine.
    pub fn is_loopback(&self) -> bool {
        self.bind.ip().is_loopback()
    }
}

/// Everything resolution reads, injected so a test needs no environment.
#[derive(Debug, Clone, Default)]
pub struct ExposureInputs {
    /// `--api-bind` or `HORON_AGENT_API_BIND`; the CLI flag wins before this is built.
    pub bind: Option<String>,
    /// `--api-insecure-behind-proxy` or its environment variable; `None` defers to the file.
    pub insecure_behind_proxy: Option<bool>,
    /// The `[harness_api]` section of `config.toml`.
    pub section: HarnessApiConfig,
    /// Whether at least one API credential exists in this home.
    pub credentials_available: bool,
}

impl ExposureInputs {
    /// Read the values this process was started with.
    ///
    /// The CLI flag wins over the environment, and both win over the file.
    pub fn from_environment(flag_bind: Option<String>, flag_insecure_behind_proxy: bool) -> Self {
        let env_bind = env_value(API_BIND_ENV);
        let env_insecure = match env_value(API_INSECURE_BEHIND_PROXY_ENV) {
            Some(raw) => parse_env_bool(&raw),
            None => None,
        };
        Self {
            bind: flag_bind.or(env_bind),
            insecure_behind_proxy: if flag_insecure_behind_proxy {
                Some(true)
            } else {
                env_insecure
            },
            section: section_from_config_file(),
            credentials_available: false,
        }
    }
}

/// A variable's text, or `None` when it is unset or not UTF-8.
fn env_value(key: &str) -> Option<String> {
    std::env::var_os(key)?.to_str().map(str::to_string)
}

/// Read `1`/`true`/`yes`/`on` and `0`/`false`/`no`/`off`; anything else is unset.
fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Resolve the TCP exposure, or `None` when only the local socket is served.
///
/// Refusals name every unmet requirement, so an operator reading one line knows
/// both why the bind was refused and what to run next.
pub fn resolve(inputs: &ExposureInputs) -> Result<Option<TcpExposure>, HarnessApiExposureError> {
    let requested = match non_empty(inputs.bind.as_deref()).or_else(|| non_empty(inputs.section.bind.as_deref())) {
        Some(raw) => raw,
        None => return Ok(None),
    };
    let bind: SocketAddr =
        requested
            .parse()
            .map_err(|error: std::net::AddrParseError| HarnessApiExposureError::InvalidBind {
                value: requested.to_string(),
                reason: error.to_string(),
            })?;
    let acknowledged = inputs
        .insecure_behind_proxy
        .unwrap_or(inputs.section.insecure_behind_proxy);
    let loopback = bind.ip().is_loopback();
    let missing_acknowledgement = !loopback && !acknowledged;
    let missing_credential = !inputs.credentials_available;
    if missing_acknowledgement || missing_credential {
        return Err(HarnessApiExposureError::Refused {
            bind,
            loopback,
            missing_acknowledgement,
            missing_credential,
        });
    }
    Ok(Some(TcpExposure {
        bind,
        insecure_behind_proxy: acknowledged,
    }))
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|raw| !raw.is_empty())
}

/// Read the `[harness_api]` section from the same file the daemon config uses.
///
/// Parse errors are the config loader's to report; this reader contributes
/// nothing rather than turning a typo into a second failure.
fn section_from_config_file() -> HarnessApiConfig {
    let Some(path) = crate::config::Config::path() else {
        return HarnessApiConfig::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HarnessApiConfig::default();
    };
    if !text.contains("harness_api") && !text.contains("\\u") && !text.contains("\\U") {
        return HarnessApiConfig::default();
    }
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct ConfigHarnessApiSlice {
        harness_api: Option<HarnessApiConfig>,
    }
    match toml::from_str::<ConfigHarnessApiSlice>(&text) {
        Ok(slice) => slice.harness_api.unwrap_or_else(HarnessApiConfig::default),
        Err(_) => {
            let Ok(document) = text.parse::<toml::Value>() else {
                return HarnessApiConfig::default();
            };
            let Some(table) = document.get("harness_api") else {
                return HarnessApiConfig::default();
            };
            match table.clone().try_into::<HarnessApiConfig>() {
                Ok(section) => section,
                Err(error) => {
                    crate::logging::warn(&format!(
                        "Ignoring [harness_api] section in {}: {error}",
                        path.display()
                    ));
                    HarnessApiConfig::default()
                }
            }
        }
    }
}

/// Why a requested exposure was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessApiExposureError {
    /// The bind value is not a literal socket address.
    InvalidBind { value: String, reason: String },
    /// At least one safety requirement is missing.
    Refused {
        bind: SocketAddr,
        loopback: bool,
        missing_acknowledgement: bool,
        missing_credential: bool,
    },
}

impl std::fmt::Display for HarnessApiExposureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBind { value, reason } => write!(
                formatter,
                "'{value}' is not a harness API bind address ({reason}); write IP:PORT with a literal IPv4 or IPv6 address, for example 127.0.0.1:7946 or [::1]:7946"
            ),
            Self::Refused {
                bind,
                loopback,
                missing_acknowledgement,
                missing_credential,
            } => {
                write!(formatter, "refusing to serve the harness API on {bind}: ")?;
                let mut separator = "";
                if *missing_credential {
                    write!(
                        formatter,
                        "no API credential exists; issue one with `horon-agent api-credentials issue`"
                    )?;
                    separator = " and ";
                }
                if *missing_acknowledgement {
                    write!(
                        formatter,
                        "{separator}a non-loopback bind sends the bearer token in cleartext and built-in TLS is not implemented; terminate TLS in front and pass --api-insecure-behind-proxy (or write insecure_behind_proxy = true in [harness_api])"
                    )?;
                } else if *missing_credential && *loopback {
                    write!(
                        formatter,
                        "; every TCP connection authenticates, so the listener would refuse them all"
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for HarnessApiExposureError {}

#[cfg(test)]
#[path = "harness_api_tests.rs"]
mod tests;
