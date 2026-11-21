//! Wire contract shared by the host and a user-installed Rust extension child.
//!
//! Both package kinds run the same Rust executable, so both sides read this
//! module: JSON-RPC 2.0 frames, one UTF-8 JSON object per newline, the
//! compatibility range and declaration hash compared in `initialize`, and the
//! typed payloads of `tool/call`, `command/invoke`, `observer/notify`, `cancel`
//! and `shutdown`.
//!
//! Nothing here performs I/O or needs a runtime. The bounded reader and writer,
//! the deadlines and the request tracking live in `horon-agent-app-core`'s
//! `extensions::wire`; an author SDK reuses these types and the same limits.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{ExtensionManifest, ObserverEvent, PackageKind};

/// JSON-RPC version written on every frame in both directions.
pub const JSON_RPC_VERSION: &str = "2.0";

/// Largest single frame either side accepts, newline included.
///
/// A frame is refused before it is parsed, so one peer cannot make the other
/// buffer without bound by never sending a newline.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Requests that may wait for a result at once.
pub const MAX_PENDING_REQUESTS: usize = 64;

/// Frames that may queue for the writer.
pub const MAX_QUEUED_FRAMES: usize = MAX_PENDING_REQUESTS * 2;

/// Protocol range the host implements today. Adding a compatible protocol
/// raises only the upper bound.
pub const HOST_PROTOCOL_RANGE: ProtocolRange = ProtocolRange::exactly(ProtocolVersion::new(1, 0));

/// A `major.minor` protocol version, written as text on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion {
    major: u16,
    minor: u16,
}

impl ProtocolVersion {
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Parse the `major.minor` spelling. Both parts are decimal without a
    /// leading zero, so a version has exactly one spelling.
    pub fn parse(raw: &str) -> Option<Self> {
        let (major, minor) = raw.split_once('.')?;
        Some(Self::new(number(major)?, number(minor)?))
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

impl Serialize for ProtocolVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| DeError::custom("protocol version must be major.minor"))
    }
}

fn number(raw: &str) -> Option<u16> {
    if raw.is_empty() || raw.len() > 5 || (raw.len() > 1 && raw.starts_with('0')) {
        return None;
    }
    let mut value: u16 = 0;
    for byte in raw.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u16::from(byte - b'0'))?;
    }
    Some(value)
}

/// An inclusive range of protocol versions one side can speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolRange {
    pub min: ProtocolVersion,
    pub max: ProtocolVersion,
}

impl ProtocolRange {
    pub const fn exactly(version: ProtocolVersion) -> Self {
        Self {
            min: version,
            max: version,
        }
    }

    /// A range is only meaningful when its bounds are ordered.
    pub fn new(min: ProtocolVersion, max: ProtocolVersion) -> Option<Self> {
        (min <= max).then_some(Self { min, max })
    }

    pub fn contains(&self, version: ProtocolVersion) -> bool {
        self.min <= version && version <= self.max
    }

    /// Versions both sides speak, or `None` when the ranges do not touch.
    pub fn intersection(&self, other: &Self) -> Option<Self> {
        Self::new(self.min.max(other.min), self.max.min(other.max))
    }
}

/// One dispatchable feature of the wire. A request names the capability it
/// needs, and `initialize` grants only what both sides declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Capability {
    #[serde(rename = "tool/call")]
    ToolCall,
    #[serde(rename = "command/invoke")]
    CommandInvoke,
    #[serde(rename = "observer/notify")]
    ObserverNotify,
}

impl Capability {
    pub const ALL: [Capability; 3] = [
        Capability::ToolCall,
        Capability::CommandInvoke,
        Capability::ObserverNotify,
    ];
}

/// A set of capabilities, written as a sorted array so both sides produce the
/// same bytes for the same set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilitySet {
    capabilities: BTreeSet<Capability>,
}

impl CapabilitySet {
    pub fn new(capabilities: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            capabilities: capabilities.into_iter().collect(),
        }
    }

    pub fn insert(&mut self, capability: Capability) -> bool {
        self.capabilities.insert(capability)
    }

    pub fn contains(&self, capability: &Capability) -> bool {
        self.capabilities.contains(capability)
    }

    pub fn iter(&self) -> impl Iterator<Item = Capability> {
        self.capabilities.iter().copied()
    }

    pub fn intersection(&self, other: &Self) -> Self {
        Self {
            capabilities: self.capabilities.intersection(&other.capabilities).copied().collect(),
        }
    }
}

/// The methods either side may put on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireMethod {
    #[serde(rename = "initialize")]
    Initialize,
    #[serde(rename = "tool/call")]
    ToolCall,
    #[serde(rename = "command/invoke")]
    CommandInvoke,
    /// A child-initiated notification: it carries no id and expects no reply.
    #[serde(rename = "observer/notify")]
    ObserverNotify,
    #[serde(rename = "cancel")]
    Cancel,
    #[serde(rename = "shutdown")]
    Shutdown,
}

impl WireMethod {
    /// Capability a request needs before the host dispatches it.
    pub const fn capability(self) -> Option<Capability> {
        match self {
            Self::ToolCall => Some(Capability::ToolCall),
            Self::CommandInvoke => Some(Capability::CommandInvoke),
            Self::ObserverNotify => Some(Capability::ObserverNotify),
            Self::Initialize | Self::Cancel | Self::Shutdown => None,
        }
    }
}

/// Envelope version marker, so a frame with another version fails to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JsonRpcVersion {
    #[serde(rename = "2.0")]
    V2,
}

/// A request: the host issues it and the peer answers with `WireResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRequest {
    pub jsonrpc: JsonRpcVersion,
    pub id: u64,
    pub method: WireMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A notification: same shape as a request without an id and without a reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireNotification {
    pub jsonrpc: JsonRpcVersion,
    pub method: WireMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A reply to one request id, carrying exactly one of `result` and `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireResponse {
    pub jsonrpc: JsonRpcVersion,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl WireResponse {
    /// Exactly one of `result` and `error` is the answer; a frame with both or
    /// neither is malformed. Both the parser and the host correlation check use
    /// this one rule.
    pub fn is_exclusive(&self) -> bool {
        matches!((&self.result, &self.error), (Some(_), None) | (None, Some(_)))
    }

    pub fn reply(id: u64, result: Value) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn fail(id: u64, error: RpcError) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            id,
            result: None,
            error: Some(error),
        }
    }

    /// Unwrap the answer after [`Self::is_exclusive`] has held.
    pub fn into_outcome(self) -> Result<Result<Value, RpcError>, FrameError> {
        match (self.result, self.error) {
            (Some(result), None) => Ok(Ok(result)),
            (None, Some(error)) => Ok(Err(error)),
            _ => Err(FrameError::InvalidResponse),
        }
    }
}

/// A JSON-RPC error, as defined by the spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub const METHOD_NOT_FOUND: i32 = -32601;

    pub fn method_not_found(message: impl Into<String>) -> Self {
        Self {
            code: Self::METHOD_NOT_FOUND,
            message: message.into(),
            data: None,
        }
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.message, self.code)
    }
}

/// A parsed frame. `Response` answers one of our ids; `Request` is something
/// the peer asks of us; `Notification` is a one-way report.
#[derive(Debug, Clone, PartialEq)]
pub enum WireFrame {
    Request(WireRequest),
    Notification(WireNotification),
    Response(WireResponse),
}

/// Why a frame was refused. Messages never quote the frame itself, so a log
/// line cannot leak tool output or a secret the peer sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// Only whitespace arrived where a frame was expected.
    Empty,
    TooLarge {
        limit: usize,
    },
    NotUtf8,
    Malformed,
    /// The object is valid JSON but is none of request, notification, response.
    UnknownShape,
    InvalidResponse,
    InvalidParams,
    /// The stream ended in the middle of a frame.
    Unterminated,
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("empty frame"),
            Self::TooLarge { limit } => write!(formatter, "frame exceeds {limit} bytes"),
            Self::NotUtf8 => formatter.write_str("frame is not UTF-8"),
            Self::Malformed => formatter.write_str("malformed frame"),
            Self::UnknownShape => formatter.write_str("frame is neither a request, a notification nor a response"),
            Self::InvalidResponse => formatter.write_str("response carries both or neither of result and error"),
            Self::InvalidParams => formatter.write_str("invalid parameters"),
            Self::Unterminated => formatter.write_str("stream ended inside a frame"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Parse one newline-delimited frame.
///
/// Length is checked here as well so an over-long slice is refused even when a
/// caller already buffered it. The bounded reader that refuses to buffer past
/// [`MAX_FRAME_BYTES`] lives in the host.
pub fn parse_frame(frame: &[u8]) -> Result<WireFrame, FrameError> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { limit: MAX_FRAME_BYTES });
    }
    let text = std::str::from_utf8(frame).map_err(|_| FrameError::NotUtf8)?;
    let text = text.trim_end_matches(['\n', '\r']);
    if text.trim().is_empty() {
        return Err(FrameError::Empty);
    }
    let value: Value = serde_json::from_str(text).map_err(|_| FrameError::Malformed)?;
    let object = value.as_object().ok_or(FrameError::UnknownShape)?;
    let has_id = object.contains_key("id");
    let has_method = object.contains_key("method");
    match (has_method, has_id) {
        (true, true) => deserialize::<WireRequest>(value).map(WireFrame::Request),
        (true, false) => deserialize::<WireNotification>(value).map(WireFrame::Notification),
        (false, true) => {
            let response = deserialize::<WireResponse>(value)?;
            if !response.is_exclusive() {
                return Err(FrameError::InvalidResponse);
            }
            Ok(WireFrame::Response(response))
        }
        (false, false) => Err(FrameError::UnknownShape),
    }
}

fn deserialize<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, FrameError> {
    serde_json::from_value(value).map_err(|_| FrameError::Malformed)
}

/// Encode one frame: the JSON object, one trailing newline.
pub fn encode_frame(frame: &impl Serialize) -> Result<Vec<u8>, FrameError> {
    let mut bytes = serde_json::to_vec(frame).map_err(|_| FrameError::Malformed)?;
    if bytes.len() + 1 > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { limit: MAX_FRAME_BYTES });
    }
    bytes.push(b'\n');
    Ok(bytes)
}

impl WireRequest {
    pub fn new(id: u64, method: WireMethod, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            id,
            method,
            params,
        }
    }

    /// Decode this request's parameters as the payload its method declares.
    pub fn parse_params<T: for<'de> Deserialize<'de>>(&self) -> Result<T, FrameError> {
        let value = self.params.clone().unwrap_or(Value::Null);
        serde_json::from_value(value).map_err(|_| FrameError::InvalidParams)
    }
}

impl WireNotification {
    pub fn new(method: WireMethod, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            method,
            params,
        }
    }

    pub fn parse_params<T: for<'de> Deserialize<'de>>(&self) -> Result<T, FrameError> {
        let value = self.params.clone().unwrap_or(Value::Null);
        serde_json::from_value(value).map_err(|_| FrameError::InvalidParams)
    }
}

/// Payload of `initialize`: what the host offers and which package it expects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitializeParams {
    pub protocol: ProtocolRange,
    pub capabilities: CapabilitySet,
    pub package_id: String,
    pub kind: PackageKind,
}

/// Payload of the `initialize` reply: what the child can speak and which
/// declaration it actually loaded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitializeResult {
    pub protocol: ProtocolRange,
    pub capabilities: CapabilitySet,
    /// SHA-256 of the installed declaration files, recomputed by the child.
    pub declaration_digest: String,
}

/// Payload of `tool/call`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallParams {
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
}

/// Payload of `command/invoke`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInvokeParams {
    pub command: String,
    #[serde(default)]
    pub arguments: Value,
}

/// Payload of `observer/notify`: the minimum metadata of one observed event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserverParams {
    pub event: ObserverEvent,
    #[serde(default)]
    pub metadata: Value,
}

/// Payload of `cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelParams {
    pub request_id: u64,
}

/// Payload of `shutdown`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What `initialize` settled: the one protocol version and the capabilities
/// both sides granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiated {
    pub protocol: ProtocolVersion,
    pub capabilities: CapabilitySet,
}

/// What the host already knows about the installed package it is about to
/// handshake with. Everything here comes from the installation record, never
/// from the child.
#[derive(Debug, Clone, Copy)]
pub struct HostExpectation<'a> {
    /// Protocol range this host build implements.
    pub protocol: ProtocolRange,
    /// Capabilities this host build can dispatch.
    pub capabilities: &'a CapabilitySet,
    /// Capabilities the installed manifest declared.
    pub declared: &'a CapabilitySet,
    pub kind: PackageKind,
    pub package_id: &'a str,
    /// SHA-256 of the installed declaration files.
    pub declaration_digest: &'a str,
}

/// Why an `initialize` exchange was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeError {
    Kind {
        expected: PackageKind,
        observed: PackageKind,
    },
    PackageId {
        expected: String,
        observed: String,
    },
    /// The digest the child reported is not a SHA-256 spelling at all.
    InvalidDigest,
    /// The child loaded something other than what was installed.
    DigestMismatch,
    ProtocolIncompatible {
        host: ProtocolRange,
        extension: ProtocolRange,
    },
    /// The child asked for a capability its own manifest never declared.
    CapabilityNotDeclared(Capability),
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kind { expected, observed } => {
                write!(
                    formatter,
                    "package kind mismatch: installed {expected:?}, child reported {observed:?}"
                )
            }
            Self::PackageId { expected, observed } => {
                write!(
                    formatter,
                    "package id mismatch: installed {expected}, child reported {observed}"
                )
            }
            Self::InvalidDigest => formatter.write_str("declaration digest is not a SHA-256 value"),
            Self::DigestMismatch => formatter.write_str("declaration digest does not match the installed package"),
            Self::ProtocolIncompatible { host, extension } => write!(
                formatter,
                "no shared protocol version: host {}..{}, child {}..{}",
                host.min, host.max, extension.min, extension.max
            ),
            Self::CapabilityNotDeclared(capability) => {
                write!(
                    formatter,
                    "capability {capability:?} is not declared by the installed manifest"
                )
            }
        }
    }
}

impl std::error::Error for HandshakeError {}

/// Whether `raw` is a lowercase SHA-256 hex digest.
pub fn is_sha256_hex(raw: &str) -> bool {
    raw.len() == 64 && raw.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Compare one `initialize` reply against the installation record.
///
/// The negotiated version is the highest version both ranges share, the granted
/// capabilities are the ones both sides offer, and a child that asks for a
/// capability it never declared is refused outright rather than silently
/// limited.
pub fn negotiate(expectation: &HostExpectation<'_>, result: &InitializeResult) -> Result<Negotiated, HandshakeError> {
    if !is_sha256_hex(&result.declaration_digest) {
        return Err(HandshakeError::InvalidDigest);
    }
    if result.declaration_digest != expectation.declaration_digest {
        return Err(HandshakeError::DigestMismatch);
    }
    let shared =
        expectation
            .protocol
            .intersection(&result.protocol)
            .ok_or_else(|| HandshakeError::ProtocolIncompatible {
                host: expectation.protocol,
                extension: result.protocol,
            })?;
    for capability in result.capabilities.iter() {
        if !expectation.declared.contains(&capability) {
            return Err(HandshakeError::CapabilityNotDeclared(capability));
        }
    }
    Ok(Negotiated {
        protocol: shared.max,
        capabilities: result.capabilities.intersection(expectation.capabilities),
    })
}

/// Whether an `initialize` request a child received matches its own record.
pub fn verify_offer(expectation: &HostExpectation<'_>, params: &InitializeParams) -> Result<(), HandshakeError> {
    if params.kind != expectation.kind {
        return Err(HandshakeError::Kind {
            expected: expectation.kind,
            observed: params.kind,
        });
    }
    if params.package_id != expectation.package_id {
        return Err(HandshakeError::PackageId {
            expected: expectation.package_id.to_string(),
            observed: params.package_id.clone(),
        });
    }
    Ok(())
}

/// Monotonic, never repeated request ids.
///
/// Ids are allocated once per session and never recycled, so a cancelled or
/// timed-out id cannot be answered by a frame meant for a later request.
#[derive(Debug)]
pub struct RequestIds {
    next: AtomicU64,
}

impl RequestIds {
    pub const fn starting_at(first: u64) -> Self {
        Self {
            next: AtomicU64::new(first),
        }
    }

    /// Allocate the next id, or `None` once the space is exhausted. Sessions
    /// stop rather than wrap, because a repeated id would make an answer
    /// ambiguous.
    pub fn allocate(&self) -> Option<u64> {
        match self
            .next
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| current.checked_add(1))
        {
            Ok(id) => Some(id),
            Err(_exhausted) => None,
        }
    }
}

impl Default for RequestIds {
    fn default() -> Self {
        Self::starting_at(1)
    }
}

impl ExtensionManifest {
    /// Capabilities this declaration lets the child use. A package that
    /// declares no tools, commands or events grants nothing.
    pub fn capabilities(&self) -> CapabilitySet {
        let mut capabilities = CapabilitySet::default();
        if !self.tools.is_empty() {
            capabilities.insert(Capability::ToolCall);
        }
        if !self.commands.is_empty() {
            capabilities.insert(Capability::CommandInvoke);
        }
        if !self.events.is_empty() {
            capabilities.insert(Capability::ObserverNotify);
        }
        capabilities
    }
}
