//! ACP version selection for one connection (IS-1).

use super::super::rpc::{JSONRPC_INVALID_PARAMS, RpcError};
use serde_json::{Value, json};

/// The v2 protocol version this adapter answers with.
pub(crate) const ACP_V2_PROTOCOL_VERSION: u64 = 2;

/// `Unsupported protocol version`, the v2 initialization error code.
const JSONRPC_UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// The surface a connection speaks after `initialize`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcpVersion {
    V1,
    V2,
}

/// Why `initialize` selected no version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InitError {
    /// The request carried no usable `protocolVersion`, or selected v2 without
    /// the `info` every v2 initialize must send.
    InvalidParams(String),
    /// No version in common with the client.
    Unsupported { requested: u64, supported: Vec<u64> },
}

impl InitError {
    pub(crate) fn to_rpc(&self) -> RpcError {
        match self {
            Self::InvalidParams(message) => RpcError::new(JSONRPC_INVALID_PARAMS, message.clone()),
            Self::Unsupported { requested, supported } => RpcError {
                code: JSONRPC_UNSUPPORTED_PROTOCOL_VERSION,
                message: "Unsupported protocol version".to_string(),
                data: Some(json!({ "requested": requested, "supported": supported })),
            },
        }
    }
}

/// The version `initialize` selects, or the reason it selected none.
///
/// With v2 disabled every usable version is answered as v1 does today, so the
/// switch is a true rollback; with v2 enabled 1 still selects the v1 surface and
/// 0 becomes `Unsupported` instead of being read as v1.
pub(crate) fn select(params: &Value, v2_enabled: bool) -> Result<AcpVersion, InitError> {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_u64)
        .filter(|version| *version <= u64::from(u16::MAX))
        .ok_or_else(|| {
            InitError::InvalidParams("initialize requires an integer protocolVersion between 0 and 65535".to_string())
        })?;
    if !v2_enabled {
        return Ok(AcpVersion::V1);
    }
    match requested {
        0 => Err(InitError::Unsupported {
            requested,
            supported: vec![1, ACP_V2_PROTOCOL_VERSION],
        }),
        1 => Ok(AcpVersion::V1),
        _ => {
            require_v2_info(params)?;
            Ok(AcpVersion::V2)
        }
    }
}

/// A v2 initialize carries `info`; the v1 params it replaces did not.
fn require_v2_info(params: &Value) -> Result<(), InitError> {
    let info = params.get("info").filter(|info| info.is_object());
    let text = |field: &str| info.and_then(|info| info.get(field)).and_then(Value::as_str).is_some();
    if text("name") && text("version") {
        return Ok(());
    }
    Err(InitError::InvalidParams(
        "initialize requires an info object with a string name and version when it selects protocol version 2"
            .to_string(),
    ))
}
