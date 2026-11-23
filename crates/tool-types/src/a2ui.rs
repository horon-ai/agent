//! A2UI v1.0 wire validation, security filtering, and projection helpers.
//!
//! Covers outbound `agent_to_renderer` envelopes (`createSurface`,
//! `updateComponents`, `updateDataModel`, `deleteSurface`,
//! `callRendererFunction`, `agentFunctionResponse`) and inbound
//! `renderer_to_agent` envelopes (`action`, `callAgentFunction`,
//! `rendererFunctionResponse`, `error`) using only `serde` and `serde_json`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fmt;

/// Current canonical A2UI specification wire version.
pub const A2UI_VERSION_V1_0: &str = "1.0";
/// Earlier pre-release draft version accepted when a renderer negotiates it.
pub const A2UI_VERSION_V0_9: &str = "v0.9";
/// Official Basic Catalog URI for A2UI v1.0.
pub const A2UI_BASIC_CATALOG_V1_0: &str = "https://a2ui.org/specification/v1_0/catalogs/basic/catalog.json";
/// Standard MIME type for A2UI envelopes in ACP and A2A payloads.
pub const A2UI_MIME_TYPE: &str = "application/a2ui+json";
/// Standard URI scheme used for `EmbeddedResource` surface carriers.
pub const A2UI_URI_SCHEME: &str = "a2ui";

/// Maximum number of envelopes permitted in a single `a2ui` emission.
pub const MAX_A2UI_ENVELOPES_PER_CALL: usize = 64;
/// Maximum number of components permitted per surface message.
pub const MAX_A2UI_COMPONENTS_PER_SURFACE: usize = 500;
/// Maximum serialized byte size of a single A2UI envelope (256 KiB).
pub const MAX_A2UI_ENVELOPE_BYTES: usize = 262_144;
/// Maximum JSON nesting depth accepted for A2UI payloads.
pub const MAX_A2UI_JSON_DEPTH: usize = 32;

const OUTBOUND_KINDS: &[&str] = &[
    "createSurface",
    "updateComponents",
    "updateDataModel",
    "deleteSurface",
    "callRendererFunction",
    "agentFunctionResponse",
];

const INBOUND_KINDS: &[&str] = &["action", "callAgentFunction", "rendererFunctionResponse", "error"];

const BLOCKED_RENDERER_FUNCTIONS: &[&str] = &[
    "eval",
    "exec",
    "execute",
    "runScript",
    "requestClipboard",
    "requestGeolocation",
];

const URI_PROPERTY_KEYS: &[&str] = &["url", "href", "src", "uri", "actionurl", "link", "iconurl", "imageurl"];

fn default_a2ui_version() -> String {
    A2UI_VERSION_V1_0.to_string()
}

/// Client-negotiated A2UI renderer capabilities for a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2uiSessionCapabilities {
    /// Negotiated A2UI specification version (`"1.0"` by default).
    #[serde(default = "default_a2ui_version")]
    pub version: String,
    /// Allow-listed catalog URIs the renderer supports. Empty means Basic v1.0 only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_catalog_ids: Vec<String>,
    /// Whether the renderer accepts inline custom catalog definitions.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepts_inline_catalogs: bool,
}

impl Default for A2uiSessionCapabilities {
    fn default() -> Self {
        Self {
            version: default_a2ui_version(),
            supported_catalog_ids: vec![A2UI_BASIC_CATALOG_V1_0.to_string()],
            accepts_inline_catalogs: false,
        }
    }
}

impl A2uiSessionCapabilities {
    /// Catalog identifiers permitted for outgoing UI envelopes on this session.
    pub fn effective_catalog_ids(&self) -> Vec<&str> {
        if self.supported_catalog_ids.is_empty() {
            vec![A2UI_BASIC_CATALOG_V1_0]
        } else {
            self.supported_catalog_ids.iter().map(String::as_str).collect()
        }
    }

    fn normalized_version(&self) -> &str {
        if self.version.trim().is_empty() {
            A2UI_VERSION_V1_0
        } else {
            self.version.trim()
        }
    }
}

/// Validation failure returned for untrusted model or renderer A2UI payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum A2uiValidationError {
    EmptyEnvelopeBatch,
    TooManyEnvelopes(usize),
    EnvelopeTooLarge { bytes: usize, limit: usize },
    DepthExceeded(usize),
    InvalidEnvelopeShape(&'static str),
    UnsupportedVersion(String),
    InvalidIdentifier { field: &'static str, value: String },
    UnallowedCatalog(String),
    ForbiddenComponentName,
    TooManyComponents(usize),
    DuplicateComponentId(String),
    MissingRootComponent(String),
    CyclicComponentGraph(String),
    DuplicateSurfaceCreate(String),
    UnsafeUri { key: String, uri: String },
    ForbiddenRendererFunction(String),
}

impl fmt::Display for A2uiValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyEnvelopeBatch => write!(f, "envelopes must contain at least one A2UI message"),
            Self::TooManyEnvelopes(n) => {
                write!(
                    f,
                    "too many A2UI envelopes in one call ({n} > {MAX_A2UI_ENVELOPES_PER_CALL})"
                )
            }
            Self::EnvelopeTooLarge { bytes, limit } => {
                write!(f, "A2UI envelope exceeds size limit ({bytes} > {limit} bytes)")
            }
            Self::DepthExceeded(limit) => write!(f, "A2UI payload exceeds max JSON depth of {limit}"),
            Self::InvalidEnvelopeShape(msg) => write!(f, "invalid A2UI envelope shape: {msg}"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported A2UI version '{v}'"),
            Self::InvalidIdentifier { field, value } => {
                write!(f, "invalid A2UI identifier for '{field}': '{value}'")
            }
            Self::UnallowedCatalog(id) => write!(f, "A2UI catalogId '{id}' is not in supportedCatalogIds"),
            Self::ForbiddenComponentName => write!(f, "component name 'Surface' is reserved and cannot be used"),
            Self::TooManyComponents(n) => {
                write!(
                    f,
                    "too many components on surface ({n} > {MAX_A2UI_COMPONENTS_PER_SURFACE})"
                )
            }
            Self::DuplicateComponentId(id) => write!(f, "duplicate component id '{id}' in message"),
            Self::MissingRootComponent(surface) => {
                write!(
                    f,
                    "createSurface '{surface}' with components must include component id 'root'"
                )
            }
            Self::CyclicComponentGraph(surface) => {
                write!(f, "component hierarchy on surface '{surface}' contains a cycle")
            }
            Self::DuplicateSurfaceCreate(surface) => {
                write!(f, "surfaceId '{surface}' cannot be created twice without deleteSurface")
            }
            Self::UnsafeUri { key, uri } => write!(f, "unsafe URI scheme in '{key}': '{uri}'"),
            Self::ForbiddenRendererFunction(name) => write!(f, "renderer function '{name}' is not allowed"),
        }
    }
}

impl std::error::Error for A2uiValidationError {}

/// Classification of a validated inbound `renderer_to_agent` message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererEventKind {
    Action,
    CallAgentFunction,
    RendererFunctionResponse,
    Error,
}

/// Validated inbound event sent by an A2UI renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedRendererEvent {
    pub kind: RendererEventKind,
    pub prompt_text: String,
    pub needs_reply: bool,
}

/// Tracks batch-level invariants across multiple streamed A2UI envelopes.
#[derive(Debug, Default)]
pub struct OutboundBatchValidator {
    active_surfaces: HashSet<String>,
    count: usize,
}

impl OutboundBatchValidator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate a single `agent_to_renderer` envelope against capabilities and batch state.
    pub fn validate_one(
        &mut self,
        envelope: &Value,
        caps: &A2uiSessionCapabilities,
    ) -> Result<(), A2uiValidationError> {
        if self.count >= MAX_A2UI_ENVELOPES_PER_CALL {
            return Err(A2uiValidationError::TooManyEnvelopes(self.count + 1));
        }
        check_size_and_depth(envelope)?;
        let expected_version = caps.normalized_version();
        let (kind, payload) = extract_envelope(envelope, expected_version, OUTBOUND_KINDS)?;
        let allowed_catalogs = caps.effective_catalog_ids();

        match kind {
            "createSurface" => {
                let obj = expect_object(payload, "createSurface must be an object")?;
                let surface_id = require_safe_id(obj, "surfaceId")?;
                if expected_version == A2UI_VERSION_V1_0 && obj.contains_key("theme") {
                    return Err(A2uiValidationError::InvalidEnvelopeShape(
                        "v1.0 createSurface does not accept 'theme'",
                    ));
                }
                if !self.active_surfaces.insert(surface_id.to_string()) {
                    return Err(A2uiValidationError::DuplicateSurfaceCreate(surface_id.to_string()));
                }
                if let Some(catalog) = optional_non_empty_str(obj, "catalogId")? {
                    check_catalog(catalog, &allowed_catalogs, caps.accepts_inline_catalogs)?;
                }
                if let Some(components_val) = obj.get("components") {
                    let components = components_val
                        .as_array()
                        .ok_or(A2uiValidationError::InvalidEnvelopeShape(
                            "createSurface.components must be an array",
                        ))?;
                    validate_component_list(
                        surface_id,
                        components,
                        &allowed_catalogs,
                        caps.accepts_inline_catalogs,
                        !components.is_empty(),
                    )?;
                }
            }
            "updateComponents" => {
                let obj = expect_object(payload, "updateComponents must be an object")?;
                let surface_id = require_safe_id(obj, "surfaceId")?;
                let components = obj
                    .get("components")
                    .and_then(Value::as_array)
                    .filter(|arr| !arr.is_empty())
                    .ok_or(A2uiValidationError::InvalidEnvelopeShape(
                        "updateComponents requires a non-empty components array",
                    ))?;
                validate_component_list(
                    surface_id,
                    components,
                    &allowed_catalogs,
                    caps.accepts_inline_catalogs,
                    false,
                )?;
            }
            "updateDataModel" => {
                let obj = expect_object(payload, "updateDataModel must be an object")?;
                require_safe_id(obj, "surfaceId")?;
                if let Some(path_val) = obj.get("path") {
                    let path = path_val.as_str().ok_or(A2uiValidationError::InvalidEnvelopeShape(
                        "updateDataModel.path must be a string",
                    ))?;
                    if !path.is_empty() && !path.starts_with('/') {
                        return Err(A2uiValidationError::InvalidEnvelopeShape(
                            "updateDataModel.path must be empty or a JSON pointer starting with '/'",
                        ));
                    }
                    if path.chars().any(char::is_control) {
                        return Err(A2uiValidationError::InvalidEnvelopeShape(
                            "updateDataModel.path must not contain control characters",
                        ));
                    }
                }
                if expected_version == A2UI_VERSION_V1_0 && !obj.contains_key("value") {
                    return Err(A2uiValidationError::InvalidEnvelopeShape(
                        "v1.0 updateDataModel requires a 'value' field",
                    ));
                }
            }
            "deleteSurface" => {
                let obj = expect_object(payload, "deleteSurface must be an object")?;
                let surface_id = require_safe_id(obj, "surfaceId")?;
                self.active_surfaces.remove(surface_id);
            }
            "callRendererFunction" => {
                let obj = expect_object(payload, "callRendererFunction must be an object")?;
                if obj.contains_key("surfaceId") {
                    require_safe_id(obj, "surfaceId")?;
                }
                require_safe_id(obj, "functionCallId")?;
                let call = obj.get("callFunction").and_then(Value::as_object).ok_or(
                    A2uiValidationError::InvalidEnvelopeShape("callRendererFunction.callFunction must be an object"),
                )?;
                let call_name = call
                    .get("@call")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .ok_or(A2uiValidationError::InvalidEnvelopeShape(
                        "callFunction.@call must be a non-empty string",
                    ))?;
                let catalog_id = optional_non_empty_str(call, "catalogId")?.ok_or(
                    A2uiValidationError::InvalidEnvelopeShape("callFunction.catalogId is required"),
                )?;
                check_catalog(catalog_id, &allowed_catalogs, caps.accepts_inline_catalogs)?;
                if call.get("requiresUserActivation").and_then(Value::as_bool) == Some(true)
                    || BLOCKED_RENDERER_FUNCTIONS
                        .iter()
                        .any(|blocked| call_name.eq_ignore_ascii_case(blocked))
                {
                    return Err(A2uiValidationError::ForbiddenRendererFunction(call_name.to_string()));
                }
            }
            "agentFunctionResponse" => {
                let obj = expect_object(payload, "agentFunctionResponse must be an object")?;
                require_safe_id(obj, "functionCallId")?;
                if !obj.contains_key("value") && !obj.contains_key("error") {
                    return Err(A2uiValidationError::InvalidEnvelopeShape(
                        "agentFunctionResponse requires 'value' or 'error'",
                    ));
                }
            }
            _ => return Err(A2uiValidationError::InvalidEnvelopeShape("unknown envelope kind")),
        }

        scan_unsafe_uris(payload, &allowed_catalogs)?;
        self.count += 1;
        Ok(())
    }
}

/// Validate a complete batch of `agent_to_renderer` envelopes from the model.
pub fn validate_agent_to_renderer_envelopes(
    envelopes: &[Value],
    caps: &A2uiSessionCapabilities,
) -> Result<Vec<Value>, A2uiValidationError> {
    if envelopes.is_empty() {
        return Err(A2uiValidationError::EmptyEnvelopeBatch);
    }
    if envelopes.len() > MAX_A2UI_ENVELOPES_PER_CALL {
        return Err(A2uiValidationError::TooManyEnvelopes(envelopes.len()));
    }
    let mut validator = OutboundBatchValidator::new();
    for env in envelopes {
        validator.validate_one(env, caps)?;
    }
    Ok(envelopes.to_vec())
}

/// Validate a single `renderer_to_agent` envelope and optional `a2uiRendererDataModel`.
pub fn validate_renderer_to_agent_envelope(
    envelope: &Value,
    data_model: Option<&Value>,
) -> Result<ValidatedRendererEvent, A2uiValidationError> {
    check_size_and_depth(envelope)?;
    if let Some(dm) = data_model {
        check_size_and_depth(dm)?;
        if !dm.is_object() {
            return Err(A2uiValidationError::InvalidEnvelopeShape(
                "a2uiRendererDataModel must be a JSON object",
            ));
        }
    }

    let version = envelope
        .get("version")
        .and_then(Value::as_str)
        .ok_or(A2uiValidationError::InvalidEnvelopeShape("missing string version"))?;
    if version != A2UI_VERSION_V1_0 && version != A2UI_VERSION_V0_9 {
        return Err(A2uiValidationError::UnsupportedVersion(version.to_string()));
    }

    let (kind_key, payload) = extract_envelope(envelope, version, INBOUND_KINDS)?;
    let (kind, needs_reply) =
        match kind_key {
            "action" => {
                let obj = expect_object(payload, "action must be an object")?;
                if obj
                    .get("name")
                    .and_then(Value::as_str)
                    .is_none_or(|s| s.trim().is_empty())
                {
                    return Err(A2uiValidationError::InvalidEnvelopeShape("action.name is required"));
                }
                require_safe_id(obj, "surfaceId")?;
                require_safe_id(obj, "sourceComponentId")?;
                (RendererEventKind::Action, true)
            }
            "callAgentFunction" => {
                let obj = expect_object(payload, "callAgentFunction must be an object")?;
                if obj.contains_key("surfaceId") {
                    require_safe_id(obj, "surfaceId")?;
                }
                require_safe_id(obj, "functionCallId")?;
                let call = obj.get("callFunction").and_then(Value::as_object).ok_or(
                    A2uiValidationError::InvalidEnvelopeShape("callAgentFunction.callFunction must be an object"),
                )?;
                if call
                    .get("@call")
                    .and_then(Value::as_str)
                    .is_none_or(|s| s.trim().is_empty())
                {
                    return Err(A2uiValidationError::InvalidEnvelopeShape(
                        "callFunction.@call must be a non-empty string",
                    ));
                }
                (RendererEventKind::CallAgentFunction, true)
            }
            "rendererFunctionResponse" => {
                let obj = expect_object(payload, "rendererFunctionResponse must be an object")?;
                require_safe_id(obj, "functionCallId")?;
                if !obj.contains_key("value") && !obj.contains_key("error") {
                    return Err(A2uiValidationError::InvalidEnvelopeShape(
                        "rendererFunctionResponse requires 'value' or 'error'",
                    ));
                }
                (RendererEventKind::RendererFunctionResponse, false)
            }
            "error" => {
                let obj = expect_object(payload, "error must be an object")?;
                if obj
                    .get("code")
                    .and_then(Value::as_str)
                    .is_none_or(|s| s.trim().is_empty())
                {
                    return Err(A2uiValidationError::InvalidEnvelopeShape("error.code is required"));
                }
                (RendererEventKind::Error, false)
            }
            _ => return Err(A2uiValidationError::InvalidEnvelopeShape("unknown inbound kind")),
        };

    let prompt_text = format_inbound_a2ui_message(kind_key, payload, data_model);
    Ok(ValidatedRendererEvent {
        kind,
        prompt_text,
        needs_reply,
    })
}

/// Render a compact summary of A2UI blocks for LLM conversation replay.
pub fn summarize_a2ui_for_model(envelopes: &[Value], fallback_text: Option<&str>) -> String {
    let mut lines = Vec::new();
    lines.push("[A2UI response]".to_string());
    if let Some(text) = fallback_text.map(str::trim).filter(|t| !t.is_empty()) {
        lines.push(format!("Summary: {text}"));
    }
    for env in envelopes {
        if let Some(obj) = env.get("createSurface").and_then(Value::as_object) {
            let sid = obj.get("surfaceId").and_then(Value::as_str).unwrap_or("?");
            let count = obj.get("components").and_then(Value::as_array).map_or(0, Vec::len);
            lines.push(format!("createSurface surfaceId={sid} components={count}"));
        } else if let Some(obj) = env.get("updateComponents").and_then(Value::as_object) {
            let sid = obj.get("surfaceId").and_then(Value::as_str).unwrap_or("?");
            let ids: Vec<&str> = obj
                .get("components")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|c| c.get("id").and_then(Value::as_str))
                .take(12)
                .collect();
            lines.push(format!("updateComponents surfaceId={sid} ids=[{}]", ids.join(", ")));
        } else if let Some(obj) = env.get("updateDataModel").and_then(Value::as_object) {
            let sid = obj.get("surfaceId").and_then(Value::as_str).unwrap_or("?");
            let path = obj.get("path").and_then(Value::as_str).unwrap_or("/");
            lines.push(format!("updateDataModel surfaceId={sid} path={path}"));
        } else if let Some(obj) = env.get("deleteSurface").and_then(Value::as_object) {
            let sid = obj.get("surfaceId").and_then(Value::as_str).unwrap_or("?");
            lines.push(format!("deleteSurface surfaceId={sid}"));
        } else if let Some(obj) = env.get("callRendererFunction").and_then(Value::as_object) {
            let call = obj
                .get("callFunction")
                .and_then(|c| c.get("@call"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            lines.push(format!("callRendererFunction @call={call}"));
        } else if let Some(obj) = env.get("agentFunctionResponse").and_then(Value::as_object) {
            let call_id = obj.get("functionCallId").and_then(Value::as_str).unwrap_or("?");
            lines.push(format!("agentFunctionResponse functionCallId={call_id}"));
        }
    }
    lines.join("\n")
}

/// Render a readable plain-text card for terminals, markdown exports, and non-UI readers.
pub fn render_a2ui_text_card(envelopes: &[Value], fallback_text: Option<&str>) -> String {
    let mut lines = Vec::new();
    let mut surface_ids: Vec<&str> = Vec::new();
    let mut component_lines: Vec<String> = Vec::new();
    let mut deleted: Vec<&str> = Vec::new();

    for env in envelopes {
        if let Some(obj) = env.get("createSurface").and_then(Value::as_object) {
            if let Some(sid) = obj.get("surfaceId").and_then(Value::as_str)
                && !surface_ids.contains(&sid)
            {
                surface_ids.push(sid);
            }
            if let Some(arr) = obj.get("components").and_then(Value::as_array) {
                collect_component_card_lines(arr, &mut component_lines);
            }
        } else if let Some(obj) = env.get("updateComponents").and_then(Value::as_object) {
            if let Some(sid) = obj.get("surfaceId").and_then(Value::as_str)
                && !surface_ids.contains(&sid)
            {
                surface_ids.push(sid);
            }
            if let Some(arr) = obj.get("components").and_then(Value::as_array) {
                collect_component_card_lines(arr, &mut component_lines);
            }
        } else if let Some(obj) = env.get("deleteSurface").and_then(Value::as_object)
            && let Some(sid) = obj.get("surfaceId").and_then(Value::as_str)
        {
            deleted.push(sid);
        }
    }

    if !surface_ids.is_empty() {
        lines.push(format!("[A2UI surface: {}]", surface_ids.join(", ")));
    } else if !deleted.is_empty() {
        lines.push(format!("[A2UI closed: {}]", deleted.join(", ")));
    } else {
        lines.push("[A2UI response]".to_string());
    }

    if let Some(text) = fallback_text.map(str::trim).filter(|t| !t.is_empty()) {
        lines.push(text.to_string());
    }
    for item in component_lines {
        if !lines.iter().any(|existing| existing == &item) {
            lines.push(item);
        }
    }
    lines.join("\n")
}

fn collect_component_card_lines(components: &[Value], out: &mut Vec<String>) {
    for comp in components {
        let Some(obj) = comp.as_object() else {
            continue;
        };
        let kind = obj.get("component").and_then(Value::as_str).unwrap_or("");
        let id = obj.get("id").and_then(Value::as_str).unwrap_or("");
        match kind {
            "Text" => {
                if let Some(text) = obj.get("text").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    out.push(text.to_string());
                }
            }
            "TextField" | "CheckBox" | "Slider" | "ChoicePicker" | "DateTimeInput" => {
                if let Some(label) = obj.get("label").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    out.push(format!("- {kind}: {label}"));
                } else if !id.is_empty() {
                    out.push(format!("- {kind}: #{id}"));
                }
            }
            "Button" => {
                let action = obj
                    .get("action")
                    .and_then(|a| a.get("event").and_then(|e| e.get("name")))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        obj.get("action")
                            .and_then(|a| a.get("functionCall").and_then(|f| f.get("@call")))
                            .and_then(Value::as_str)
                    })
                    .unwrap_or(id);
                let child = obj.get("child").and_then(Value::as_str).unwrap_or(id);
                out.push(format!("[Button {child} -> {action}]"));
            }
            _ => {}
        }
    }
}

fn format_inbound_a2ui_message(kind: &str, payload: &Value, data_model: Option<&Value>) -> String {
    let mut sections = Vec::new();
    if kind == "action"
        && let Some(user_msg) = payload
            .get("userMessage")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        sections.push(user_msg.to_string());
    }
    let payload_json = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
    sections.push(format!("[A2UI {kind}]\n{payload_json}"));
    if let Some(dm) = data_model {
        let dm_json = serde_json::to_string(dm).unwrap_or_else(|_| "{}".to_string());
        sections.push(format!("[A2UI renderer dataModel]\n{dm_json}"));
    }
    sections.join("\n\n")
}

fn check_size_and_depth(value: &Value) -> Result<(), A2uiValidationError> {
    let encoded = serde_json::to_vec(value)
        .map_err(|_| A2uiValidationError::InvalidEnvelopeShape("envelope cannot be serialized"))?;
    if encoded.len() > MAX_A2UI_ENVELOPE_BYTES {
        return Err(A2uiValidationError::EnvelopeTooLarge {
            bytes: encoded.len(),
            limit: MAX_A2UI_ENVELOPE_BYTES,
        });
    }
    check_depth(value, 1)
}

fn check_depth(value: &Value, depth: usize) -> Result<(), A2uiValidationError> {
    if depth > MAX_A2UI_JSON_DEPTH {
        return Err(A2uiValidationError::DepthExceeded(MAX_A2UI_JSON_DEPTH));
    }
    match value {
        Value::Array(items) => {
            for item in items {
                check_depth(item, depth + 1)?;
            }
        }
        Value::Object(map) => {
            for val in map.values() {
                check_depth(val, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn extract_envelope<'a>(
    value: &'a Value,
    expected_version: &str,
    allowed_kinds: &[&str],
) -> Result<(&'a str, &'a Value), A2uiValidationError> {
    let map = value.as_object().ok_or(A2uiValidationError::InvalidEnvelopeShape(
        "envelope must be a JSON object",
    ))?;
    let version = map
        .get("version")
        .and_then(Value::as_str)
        .ok_or(A2uiValidationError::InvalidEnvelopeShape("missing string 'version'"))?;
    if version != expected_version {
        return Err(A2uiValidationError::UnsupportedVersion(version.to_string()));
    }
    if map.len() != 2 {
        return Err(A2uiValidationError::InvalidEnvelopeShape(
            "envelope must have exactly 'version' and one action key",
        ));
    }
    for (key, val) in map {
        if key == "version" {
            continue;
        }
        if allowed_kinds.contains(&key.as_str()) {
            return Ok((key.as_str(), val));
        }
        return Err(A2uiValidationError::InvalidEnvelopeShape(
            "unrecognized A2UI envelope action key",
        ));
    }
    Err(A2uiValidationError::InvalidEnvelopeShape("missing A2UI action key"))
}

fn expect_object<'a>(
    value: &'a Value,
    msg: &'static str,
) -> Result<&'a serde_json::Map<String, Value>, A2uiValidationError> {
    value.as_object().ok_or(A2uiValidationError::InvalidEnvelopeShape(msg))
}

fn require_safe_id<'a>(
    map: &'a serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, A2uiValidationError> {
    let raw = map
        .get(field)
        .and_then(Value::as_str)
        .ok_or(A2uiValidationError::InvalidEnvelopeShape("missing required identifier"))?;
    validate_safe_id(field, raw)?;
    Ok(raw)
}

fn validate_safe_id(field: &'static str, raw: &str) -> Result<(), A2uiValidationError> {
    if raw.is_empty()
        || raw.len() > 128
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
    {
        return Err(A2uiValidationError::InvalidIdentifier {
            field,
            value: raw.to_string(),
        });
    }
    Ok(())
}

fn optional_non_empty_str<'a>(
    map: &'a serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<Option<&'a str>, A2uiValidationError> {
    match map.get(field) {
        None => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.as_str())),
        Some(_) => Err(A2uiValidationError::InvalidEnvelopeShape(
            "optional string field must be non-empty",
        )),
    }
}

fn check_catalog(catalog_id: &str, allowed_catalogs: &[&str], accepts_inline: bool) -> Result<(), A2uiValidationError> {
    if accepts_inline && catalog_id.starts_with("inline:") {
        return Ok(());
    }
    if allowed_catalogs.contains(&catalog_id) {
        return Ok(());
    }
    Err(A2uiValidationError::UnallowedCatalog(catalog_id.to_string()))
}

fn validate_component_list(
    surface_id: &str,
    components: &[Value],
    allowed_catalogs: &[&str],
    accepts_inline: bool,
    require_root: bool,
) -> Result<(), A2uiValidationError> {
    if components.len() > MAX_A2UI_COMPONENTS_PER_SURFACE {
        return Err(A2uiValidationError::TooManyComponents(components.len()));
    }
    let mut ids = HashSet::with_capacity(components.len());
    let mut edges: HashMap<&str, Vec<&str>> = HashMap::with_capacity(components.len());
    let mut has_root = false;

    for comp in components {
        let obj = comp.as_object().ok_or(A2uiValidationError::InvalidEnvelopeShape(
            "every component entry must be a JSON object",
        ))?;
        let id = require_safe_id(obj, "id")?;
        if !ids.insert(id) {
            return Err(A2uiValidationError::DuplicateComponentId(id.to_string()));
        }
        if id == "root" {
            has_root = true;
        }
        let kind = obj
            .get("component")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or(A2uiValidationError::InvalidEnvelopeShape(
                "component must declare a non-empty 'component' type",
            ))?;
        if kind == "Surface" {
            return Err(A2uiValidationError::ForbiddenComponentName);
        }
        if let Some(catalog) = optional_non_empty_str(obj, "@catalogId")? {
            check_catalog(catalog, allowed_catalogs, accepts_inline)?;
        }
        let children = collect_child_refs(obj);
        edges.insert(id, children);
    }

    if require_root && !has_root {
        return Err(A2uiValidationError::MissingRootComponent(surface_id.to_string()));
    }
    detect_component_cycle(surface_id, &edges)
}

fn collect_child_refs(obj: &serde_json::Map<String, Value>) -> Vec<&str> {
    let mut refs = Vec::new();
    for key in ["child", "contentChild", "entryPointChild"] {
        if let Some(id) = obj.get(key).and_then(Value::as_str) {
            refs.push(id);
        }
    }
    match obj.get("children") {
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(id) = item.as_str() {
                    refs.push(id);
                }
            }
        }
        Some(Value::Object(child_obj)) => {
            if let Some(Value::Array(items)) = child_obj.get("explicitList") {
                for item in items {
                    if let Some(id) = item.as_str() {
                        refs.push(id);
                    }
                }
            }
        }
        _ => {}
    }
    refs
}

fn detect_component_cycle<'a>(
    surface_id: &str,
    edges: &HashMap<&'a str, Vec<&'a str>>,
) -> Result<(), A2uiValidationError> {
    // 0 = unvisited, 1 = visiting, 2 = done
    let mut state: HashMap<&'a str, u8> = HashMap::with_capacity(edges.len());
    for &node in edges.keys() {
        if state.get(node).copied().unwrap_or(0) == 0 && has_cycle(node, edges, &mut state) {
            return Err(A2uiValidationError::CyclicComponentGraph(surface_id.to_string()));
        }
    }
    Ok(())
}

fn has_cycle<'a>(node: &'a str, edges: &HashMap<&'a str, Vec<&'a str>>, state: &mut HashMap<&'a str, u8>) -> bool {
    state.insert(node, 1);
    if let Some(children) = edges.get(node) {
        for &child in children {
            match state.get(child).copied().unwrap_or(0) {
                1 => return true,
                0 if edges.contains_key(child) && has_cycle(child, edges, state) => return true,
                _ => {}
            }
        }
    }
    state.insert(node, 2);
    false
}

fn scan_unsafe_uris(value: &Value, allowed_catalogs: &[&str]) -> Result<(), A2uiValidationError> {
    match value {
        Value::Object(map) => {
            let is_open_url = map.get("@call").and_then(Value::as_str) == Some("openUrl");
            for (k, v) in map {
                let lower = k.to_ascii_lowercase();
                if URI_PROPERTY_KEYS.contains(&lower.as_str())
                    && let Some(raw) = v.as_str()
                {
                    validate_uri_string(k, raw, allowed_catalogs)?;
                }
                if is_open_url
                    && k == "args"
                    && let Some(url) = v.get("url").and_then(Value::as_str)
                {
                    validate_uri_string("openUrl.args.url", url, allowed_catalogs)?;
                }
                scan_unsafe_uris(v, allowed_catalogs)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                scan_unsafe_uris(item, allowed_catalogs)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_uri_string(key: &str, raw: &str, allowed_catalogs: &[&str]) -> Result<(), A2uiValidationError> {
    if allowed_catalogs.contains(&raw) {
        return Ok(());
    }
    // Strip ASCII whitespace and control chars before checking schemes so `java\nscript:` cannot bypass.
    let normalized: String = raw
        .chars()
        .filter(|c| !c.is_ascii_control() && !c.is_ascii_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();

    let safe = normalized.starts_with("https://")
        || normalized.starts_with("a2ui://")
        || normalized.starts_with("${")
        || normalized == "#"
        || (normalized.starts_with('#') && !normalized.contains(':'))
        || (normalized.starts_with('/') && !normalized.starts_with("//"))
        || normalized.starts_with("./")
        || normalized.starts_with("../");

    if !safe {
        return Err(A2uiValidationError::UnsafeUri {
            key: key.to_string(),
            uri: raw.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_v1_create_update_and_delete_batch_passes() {
        let caps = A2uiSessionCapabilities::default();
        let envelopes = vec![
            json!({
                "version": "1.0",
                "createSurface": {
                    "surfaceId": "status_card",
                    "catalogId": A2UI_BASIC_CATALOG_V1_0,
                    "sendDataModel": true,
                    "components": [
                        { "id": "root", "component": "Column", "children": ["title", "btn"] },
                        { "id": "title", "component": "Text", "text": "Build Status" },
                        { "id": "btn", "component": "Button", "child": "btn_lbl", "action": { "event": { "name": "refresh" } } }
                    ],
                    "dataModel": { "status": "ok" }
                }
            }),
            json!({
                "version": "1.0",
                "updateDataModel": {
                    "surfaceId": "status_card",
                    "path": "/status",
                    "value": "done"
                }
            }),
            json!({
                "version": "1.0",
                "deleteSurface": { "surfaceId": "status_card" }
            }),
        ];

        let out = validate_agent_to_renderer_envelopes(&envelopes, &caps).expect("valid batch");
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn rejects_unallowed_catalog_and_v09_theme_field() {
        let caps = A2uiSessionCapabilities::default();
        let bad_catalog = vec![json!({
            "version": "1.0",
            "createSurface": {
                "surfaceId": "s1",
                "catalogId": "https://evil.example/catalog.json"
            }
        })];
        assert!(matches!(
            validate_agent_to_renderer_envelopes(&bad_catalog, &caps),
            Err(A2uiValidationError::UnallowedCatalog(_))
        ));

        let with_theme = vec![json!({
            "version": "1.0",
            "createSurface": {
                "surfaceId": "s1",
                "theme": { "primaryColor": "#000000" }
            }
        })];
        assert!(matches!(
            validate_agent_to_renderer_envelopes(&with_theme, &caps),
            Err(A2uiValidationError::InvalidEnvelopeShape(_))
        ));
    }

    #[test]
    fn rejects_unsafe_uris_cycles_surface_component_and_privileged_functions() {
        let caps = A2uiSessionCapabilities::default();
        let js_uri = vec![json!({
            "version": "1.0",
            "updateComponents": {
                "surfaceId": "s1",
                "components": [
                    { "id": "img", "component": "Image", "url": "  java\nscript:alert(1)" }
                ]
            }
        })];
        assert!(matches!(
            validate_agent_to_renderer_envelopes(&js_uri, &caps),
            Err(A2uiValidationError::UnsafeUri { .. })
        ));

        let html_data_uri = vec![json!({
            "version": "1.0",
            "callRendererFunction": {
                "functionCallId": "fn1",
                "callFunction": {
                    "@call": "openUrl",
                    "catalogId": A2UI_BASIC_CATALOG_V1_0,
                    "args": { "url": "data:text/html,<script>alert(1)</script>" }
                }
            }
        })];
        assert!(matches!(
            validate_agent_to_renderer_envelopes(&html_data_uri, &caps),
            Err(A2uiValidationError::UnsafeUri { .. })
        ));

        let cyclic = vec![json!({
            "version": "1.0",
            "updateComponents": {
                "surfaceId": "s1",
                "components": [
                    { "id": "a", "component": "Column", "children": ["b"] },
                    { "id": "b", "component": "Column", "children": ["a"] }
                ]
            }
        })];
        assert!(matches!(
            validate_agent_to_renderer_envelopes(&cyclic, &caps),
            Err(A2uiValidationError::CyclicComponentGraph(_))
        ));

        let surface_component = vec![json!({
            "version": "1.0",
            "updateComponents": {
                "surfaceId": "s1",
                "components": [{ "id": "root", "component": "Surface" }]
            }
        })];
        assert_eq!(
            validate_agent_to_renderer_envelopes(&surface_component, &caps),
            Err(A2uiValidationError::ForbiddenComponentName)
        );

        let user_act_fn = vec![json!({
            "version": "1.0",
            "callRendererFunction": {
                "functionCallId": "fn2",
                "callFunction": {
                    "@call": "copyToClipboard",
                    "catalogId": A2UI_BASIC_CATALOG_V1_0,
                    "requiresUserActivation": true
                }
            }
        })];
        assert!(matches!(
            validate_agent_to_renderer_envelopes(&user_act_fn, &caps),
            Err(A2uiValidationError::ForbiddenRendererFunction(_))
        ));
    }

    #[test]
    fn rejects_duplicate_create_without_delete_and_allows_after_delete() {
        let caps = A2uiSessionCapabilities::default();
        let dup = vec![
            json!({"version": "1.0", "createSurface": {"surfaceId": "s1"}}),
            json!({"version": "1.0", "createSurface": {"surfaceId": "s1"}}),
        ];
        assert_eq!(
            validate_agent_to_renderer_envelopes(&dup, &caps),
            Err(A2uiValidationError::DuplicateSurfaceCreate("s1".into()))
        );

        let recreate = vec![
            json!({"version": "1.0", "createSurface": {"surfaceId": "s1"}}),
            json!({"version": "1.0", "deleteSurface": {"surfaceId": "s1"}}),
            json!({"version": "1.0", "createSurface": {"surfaceId": "s1"}}),
        ];
        assert!(validate_agent_to_renderer_envelopes(&recreate, &caps).is_ok());
    }

    #[test]
    fn validates_inbound_action_and_error_and_renders_text_card() {
        let action = json!({
            "version": "1.0",
            "action": {
                "name": "approve_deploy",
                "surfaceId": "deploy_panel",
                "sourceComponentId": "approve_btn",
                "userMessage": "Approve production deploy",
                "context": { "env": "staging" }
            }
        });
        let data_model = json!({ "version": "1.0", "surfaces": { "deploy_panel": { "confirm": true } } });
        let parsed = validate_renderer_to_agent_envelope(&action, Some(&data_model)).expect("valid action");
        assert_eq!(parsed.kind, RendererEventKind::Action);
        assert!(parsed.needs_reply);
        assert!(parsed.prompt_text.contains("Approve production deploy"));
        assert!(parsed.prompt_text.contains("approve_deploy"));
        assert!(parsed.prompt_text.contains("dataModel"));

        let err_env = json!({
            "version": "1.0",
            "error": {
                "code": "VALIDATION_FAILED",
                "surfaceId": "deploy_panel",
                "path": "/components/0",
                "message": "missing label"
            }
        });
        let parsed_err = validate_renderer_to_agent_envelope(&err_env, None).expect("valid error");
        assert_eq!(parsed_err.kind, RendererEventKind::Error);
        assert!(!parsed_err.needs_reply);

        let card = render_a2ui_text_card(
            &[json!({
                "version": "1.0",
                "createSurface": {
                    "surfaceId": "deploy_panel",
                    "components": [
                        { "id": "root", "component": "Text", "text": "Ready to deploy" },
                        { "id": "btn", "component": "Button", "child": "Go", "action": { "event": { "name": "go" } } }
                    ]
                }
            })],
            Some("Deployment confirmation panel"),
        );
        assert!(card.contains("[A2UI surface: deploy_panel]"));
        assert!(card.contains("Deployment confirmation panel"));
        assert!(card.contains("Ready to deploy"));
        assert!(card.contains("[Button Go -> go]"));
    }
}
