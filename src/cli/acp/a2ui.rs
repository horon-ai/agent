use std::fmt;

use crate::protocol::a2ui::{
    A2UI_MIME_TYPE, A2UI_URI_SCHEME, A2UI_VERSION_V0_9, A2UI_VERSION_V1_0, A2uiSessionCapabilities,
    render_a2ui_text_card, validate_renderer_to_agent_envelope,
};
use serde_json::{Map, Value, json};

const A2UI_TOOL_NAME: &str = "a2ui";

/// Validation or structural error while extracting an inbound A2UI event from `session/prompt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct A2uiPromptError {
    pub(super) message: String,
}

impl A2uiPromptError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for A2uiPromptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for A2uiPromptError {}

/// Validated A2UI renderer-to-agent event extracted from ACP `session/prompt` parameters.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct InboundA2uiEvent {
    pub(super) envelope: Value,
    pub(super) data_model: Option<Value>,
}

/// Buffered A2UI payload observed in a replayed `tool` row waiting for its turn's assistant row.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PendingA2uiMessage {
    pub(super) tool_call_id: String,
    pub(super) envelopes: Vec<Value>,
    pub(super) expected_card: String,
}

pub(super) fn is_a2ui_tool_name(name: &str) -> bool {
    name == A2UI_TOOL_NAME
}

pub(super) fn parse_initialize_a2ui_capabilities(
    params: &Value,
    client_capabilities: &Value,
) -> Option<A2uiSessionCapabilities> {
    if let Some(meta) = client_capabilities.get("_meta")
        && meta_mentions_a2ui(meta)
    {
        return parse_capabilities_from_meta(meta);
    }
    if let Some(meta) = params.get("_meta")
        && meta_mentions_a2ui(meta)
    {
        return parse_capabilities_from_meta(meta);
    }
    None
}

pub(super) fn parse_capabilities_from_caps_value(client_capabilities: &Value) -> Option<A2uiSessionCapabilities> {
    let meta = client_capabilities.get("_meta")?;
    parse_capabilities_from_meta(meta)
}

fn meta_mentions_a2ui(meta: &Value) -> bool {
    let has_a2ui = matches!(meta.get("a2ui"), Some(val) if !val.is_null());
    let has_renderer = matches!(meta.get("a2uiRendererCapabilities"), Some(val) if !val.is_null());
    has_a2ui || has_renderer
}

fn parse_capabilities_from_meta(meta: &Value) -> Option<A2uiSessionCapabilities> {
    let obj = meta.as_object()?;
    if let Some(a2ui_val) = obj.get("a2ui")
        && !a2ui_val.is_null()
    {
        return parse_capability_entry(a2ui_val);
    }
    if let Some(renderer_val) = obj.get("a2uiRendererCapabilities")
        && !renderer_val.is_null()
    {
        return parse_capability_entry(renderer_val);
    }
    None
}

fn parse_capability_entry(value: &Value) -> Option<A2uiSessionCapabilities> {
    match value {
        Value::Bool(true) => Some(default_a2ui_capabilities(A2UI_VERSION_V1_0)),
        Value::Bool(false) => None,
        Value::Object(obj) => {
            if obj.get("enabled").and_then(Value::as_bool) == Some(false) {
                return None;
            }
            if let Some((version_key, nested_obj)) = find_nested_version_table(obj) {
                if nested_obj.get("enabled").and_then(Value::as_bool) == Some(false) {
                    return None;
                }
                let mut caps = parse_capability_object(nested_obj, normalize_version(version_key));
                if caps.supported_catalog_ids.is_empty() {
                    caps.supported_catalog_ids = parse_catalog_ids(obj);
                }
                if !caps.accepts_inline_catalogs {
                    caps.accepts_inline_catalogs = parse_inline_catalogs(obj);
                }
                return Some(caps);
            }
            let raw_version = obj
                .get("version")
                .and_then(Value::as_str)
                .map(normalize_version)
                .unwrap_or(A2UI_VERSION_V1_0);
            Some(parse_capability_object(obj, raw_version))
        }
        _ => None,
    }
}

fn find_nested_version_table(obj: &Map<String, Value>) -> Option<(&'static str, &Map<String, Value>)> {
    for key in ["v1.0", "1.0", "v0.9"] {
        if let Some(nested) = obj.get(key).and_then(Value::as_object) {
            return Some((key, nested));
        }
    }
    None
}

fn normalize_version(raw: &str) -> &str {
    match raw.trim() {
        "v1.0" | "1.0" | "" => A2UI_VERSION_V1_0,
        "v0.9" | "0.9" => A2UI_VERSION_V0_9,
        other => other,
    }
}

fn default_a2ui_capabilities(version: &str) -> A2uiSessionCapabilities {
    A2uiSessionCapabilities {
        version: version.to_string(),
        supported_catalog_ids: Vec::new(),
        accepts_inline_catalogs: false,
    }
}

fn parse_capability_object(obj: &Map<String, Value>, default_version: &str) -> A2uiSessionCapabilities {
    let version = match obj.get("version").and_then(Value::as_str) {
        Some(raw) => normalize_version(raw).to_string(),
        None => default_version.to_string(),
    };
    A2uiSessionCapabilities {
        version,
        supported_catalog_ids: parse_catalog_ids(obj),
        accepts_inline_catalogs: parse_inline_catalogs(obj),
    }
}

fn parse_catalog_ids(obj: &Map<String, Value>) -> Vec<String> {
    let raw_list = obj
        .get("supportedCatalogIds")
        .or_else(|| obj.get("supported_catalog_ids"))
        .and_then(Value::as_array);
    let Some(items) = raw_list else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for item in items {
        if let Some(s) = item.as_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() && !ids.iter().any(|existing| existing == trimmed) {
                ids.push(trimmed.to_string());
            }
        }
    }
    ids
}

fn parse_inline_catalogs(obj: &Map<String, Value>) -> bool {
    obj.get("acceptsInlineCatalogs")
        .or_else(|| obj.get("accepts_inline_catalogs"))
        .and_then(Value::as_bool)
        == Some(true)
}

pub(super) fn attach_initialize_a2ui_capability(
    result: &mut Value,
    capabilities_field: &str,
    negotiated: Option<&A2uiSessionCapabilities>,
) {
    let Some(cap) = negotiated else {
        return;
    };
    let Some(root) = result.as_object_mut() else {
        return;
    };
    let caps_entry = root
        .entry(capabilities_field)
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(caps_obj) = caps_entry.as_object_mut() else {
        return;
    };
    let meta_entry = caps_obj.entry("_meta").or_insert_with(|| Value::Object(Map::new()));
    let Some(meta_obj) = meta_entry.as_object_mut() else {
        return;
    };
    meta_obj.insert("a2ui".to_string(), server_capability_value(cap));
}

fn server_capability_value(cap: &A2uiSessionCapabilities) -> Value {
    let version = if cap.version.trim().is_empty() {
        A2UI_VERSION_V1_0
    } else {
        cap.version.as_str()
    };
    let catalogs: Vec<Value> = cap
        .effective_catalog_ids()
        .into_iter()
        .map(|id| Value::String(id.to_string()))
        .collect();
    json!({
        "supported": true,
        "version": version,
        "supportedCatalogIds": catalogs,
        "acceptsInlineCatalogs": cap.accepts_inline_catalogs,
        "mimeType": A2UI_MIME_TYPE,
    })
}

fn a2ui_envelope_uri(envelope: &Value, tool_call_id: &str, sequence: u32) -> String {
    let raw_id = extract_surface_or_call_id(envelope, tool_call_id, sequence);
    let encoded = percent_encode_uri_segment(&raw_id);
    format!("{A2UI_URI_SCHEME}://surface/{encoded}")
}

pub(super) fn a2ui_embedded_resource(envelope: &Value, tool_call_id: &str, sequence: u32) -> Value {
    let text = match serde_json::to_string(envelope) {
        Ok(serialized) => serialized,
        Err(_) => "{}".to_string(),
    };
    json!({
        "type": "resource",
        "resource": {
            "uri": a2ui_envelope_uri(envelope, tool_call_id, sequence),
            "mimeType": A2UI_MIME_TYPE,
            "text": text,
        }
    })
}

fn extract_surface_or_call_id(envelope: &Value, tool_call_id: &str, sequence: u32) -> String {
    if let Some(surface_id) = envelope.get("surfaceId").and_then(Value::as_str)
        && !surface_id.trim().is_empty()
    {
        return surface_id.trim().to_string();
    }
    const PAYLOAD_KEYS: [&str; 9] = [
        "createSurface",
        "updateComponents",
        "updateDataModel",
        "deleteSurface",
        "callRendererFunction",
        "surfaceUpdate",
        "dataModelUpdate",
        "beginRendering",
        "agentFunctionResponse",
    ];
    for key in PAYLOAD_KEYS {
        if let Some(payload) = envelope.get(key) {
            for field in ["surfaceId", "functionCallId", "id"] {
                if let Some(candidate) = payload.get(field).and_then(Value::as_str)
                    && !candidate.trim().is_empty()
                {
                    return candidate.trim().to_string();
                }
            }
        }
    }
    if !tool_call_id.trim().is_empty() {
        return tool_call_id.trim().to_string();
    }
    format!("envelope-{sequence}")
}

fn percent_encode_uri_segment(input: &str) -> String {
    if input.is_empty() {
        return "default".to_string();
    }
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let is_unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~');
        if is_unreserved {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn is_a2ui_resource_block(block: &Value) -> bool {
    if block.get("type").and_then(Value::as_str) != Some("resource") {
        return false;
    }
    let Some(resource) = block.get("resource") else {
        return false;
    };
    is_a2ui_resource_contents(resource)
}

fn is_a2ui_resource_contents(resource: &Value) -> bool {
    let mime_matches = match resource
        .get("mimeType")
        .or_else(|| resource.get("mime_type"))
        .and_then(Value::as_str)
    {
        Some(raw) => {
            let media_type = match raw.split(';').next() {
                Some(part) => part.trim(),
                None => raw.trim(),
            };
            media_type.eq_ignore_ascii_case(A2UI_MIME_TYPE)
        }
        None => false,
    };
    if mime_matches {
        return true;
    }
    let prefix = format!("{A2UI_URI_SCHEME}://");
    match resource.get("uri").and_then(Value::as_str) {
        Some(uri) => uri.trim().starts_with(&prefix),
        None => false,
    }
}

pub(super) fn extract_inbound_a2ui_event(
    params: &Value,
) -> std::result::Result<Option<InboundA2uiEvent>, A2uiPromptError> {
    let params_data_model = parse_optional_data_model(
        params.pointer("/_meta/a2uiRendererDataModel"),
        "params._meta.a2uiRendererDataModel",
    )?;
    let meta_envelope = match params.pointer("/_meta/a2uiEvent") {
        Some(raw) if !raw.is_null() => Some(parse_envelope_value(raw, "params._meta.a2uiEvent")?),
        _ => None,
    };

    let mut prompt_events: Vec<(Value, Option<Value>)> = Vec::new();
    if let Some(blocks) = params.get("prompt").and_then(Value::as_array) {
        for block in blocks {
            if !is_a2ui_resource_block(block) {
                continue;
            }
            let resource = match block.get("resource") {
                Some(res) => res,
                None => {
                    return Err(A2uiPromptError::new("A2UI resource block is missing resource object"));
                }
            };
            let text = match resource.get("text").and_then(Value::as_str) {
                Some(text) if !text.trim().is_empty() => text,
                _ => {
                    return Err(A2uiPromptError::new(
                        "A2UI resource block requires non-empty JSON text payload",
                    ));
                }
            };
            let envelope = parse_envelope_str(text, "prompt[].resource.text")?;
            let resource_dm = parse_optional_data_model(
                resource.pointer("/_meta/a2uiRendererDataModel"),
                "prompt[].resource._meta.a2uiRendererDataModel",
            )?;
            let block_dm = match resource_dm {
                Some(dm) => Some(dm),
                None => parse_optional_data_model(
                    block.pointer("/_meta/a2uiRendererDataModel"),
                    "prompt[]._meta.a2uiRendererDataModel",
                )?,
            };
            prompt_events.push((envelope, block_dm));
        }
    }

    let (envelope, data_model) = match meta_envelope {
        Some(env) => {
            if prompt_events.len() > 1 || (prompt_events.len() == 1 && prompt_events[0].0 != env) {
                return Err(A2uiPromptError::new(
                    "conflicting A2UI events in params._meta.a2uiEvent and prompt resource blocks",
                ));
            }
            let fallback_dm = if prompt_events.len() == 1 {
                prompt_events.swap_remove(0).1
            } else {
                None
            };
            (env, params_data_model.or(fallback_dm))
        }
        None => {
            if prompt_events.is_empty() {
                return Ok(None);
            }
            if prompt_events.len() > 1 {
                return Err(A2uiPromptError::new(
                    "multiple A2UI resource blocks in a single session/prompt are not supported",
                ));
            }
            let (env, block_dm) = prompt_events.swap_remove(0);
            (env, block_dm.or(params_data_model))
        }
    };

    if let Err(err) = validate_renderer_to_agent_envelope(&envelope, data_model.as_ref()) {
        return Err(A2uiPromptError::new(format!("invalid A2UI renderer event: {err}")));
    }

    Ok(Some(InboundA2uiEvent { envelope, data_model }))
}

fn parse_envelope_value(raw: &Value, label: &str) -> std::result::Result<Value, A2uiPromptError> {
    match raw {
        Value::Object(_) => Ok(raw.clone()),
        Value::String(s) => parse_envelope_str(s, label),
        _ => Err(A2uiPromptError::new(format!("{label} must be a JSON object"))),
    }
}

fn parse_envelope_str(raw: &str, label: &str) -> std::result::Result<Value, A2uiPromptError> {
    let parsed: Value =
        serde_json::from_str(raw).map_err(|err| A2uiPromptError::new(format!("{label} must be valid JSON: {err}")))?;
    if !parsed.is_object() {
        return Err(A2uiPromptError::new(format!("{label} must decode to a JSON object")));
    }
    Ok(parsed)
}

fn parse_optional_data_model(raw: Option<&Value>, label: &str) -> std::result::Result<Option<Value>, A2uiPromptError> {
    let Some(val) = raw else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(None);
    }
    match val {
        Value::Object(_) => Ok(Some(val.clone())),
        Value::String(s) => {
            let parsed: Value = serde_json::from_str(s)
                .map_err(|err| A2uiPromptError::new(format!("{label} must be valid JSON: {err}")))?;
            if !parsed.is_object() {
                return Err(A2uiPromptError::new(format!("{label} must decode to a JSON object")));
            }
            Ok(Some(parsed))
        }
        _ => Err(A2uiPromptError::new(format!("{label} must be a JSON object"))),
    }
}

pub(super) fn extract_replay_a2ui_message(
    tool_call_id: &str,
    input: &Value,
    content: &str,
) -> Option<PendingA2uiMessage> {
    if content.trim_start().starts_with("Error:") {
        return None;
    }
    let envelopes: Vec<Value> = match input.get("envelopes").and_then(Value::as_array) {
        Some(arr) if !arr.is_empty() => arr.clone(),
        _ => {
            let single = input.get("envelope").filter(|v| v.is_object())?;
            vec![single.clone()]
        }
    };
    let fallback_text = input.get("fallback_text").and_then(Value::as_str);
    let expected_card = render_a2ui_text_card(&envelopes, fallback_text);
    Some(PendingA2uiMessage {
        tool_call_id: tool_call_id.to_string(),
        envelopes,
        expected_card,
    })
}

pub(super) fn split_assistant_text_and_pending_a2ui(
    content: &str,
    pending: &mut Vec<PendingA2uiMessage>,
) -> (String, Vec<PendingA2uiMessage>) {
    if pending.is_empty() {
        return (content.to_string(), Vec::new());
    }
    let mut remaining = content.to_string();
    let mut matched = Vec::new();
    let mut kept = Vec::new();
    for entry in pending.drain(..) {
        let trimmed_card = entry.expected_card.trim();
        if !trimmed_card.is_empty() && strip_card_once(&mut remaining, trimmed_card) {
            matched.push(entry);
        } else {
            kept.push(entry);
        }
    }
    *pending = kept;
    if matched.is_empty() {
        return (content.to_string(), Vec::new());
    }
    (remaining.trim().to_string(), matched)
}

fn strip_card_once(haystack: &mut String, needle: &str) -> bool {
    let Some(idx) = haystack.find(needle) else {
        return false;
    };
    let end = idx + needle.len();
    let before = haystack[..idx].trim_end_matches('\n');
    let after = haystack[end..].trim_start_matches('\n');
    let joined = match (before.is_empty(), after.is_empty()) {
        (true, true) => String::new(),
        (true, false) => after.to_string(),
        (false, true) => before.to_string(),
        (false, false) => format!("{before}\n\n{after}"),
    };
    *haystack = joined;
    true
}

pub(super) fn embedded_resources_for_pending(pending: &[PendingA2uiMessage]) -> Vec<Value> {
    let mut resources = Vec::new();
    for entry in pending {
        for (idx, envelope) in entry.envelopes.iter().enumerate() {
            let seq = u32::try_from(idx).unwrap_or(u32::MAX);
            resources.push(a2ui_embedded_resource(envelope, &entry.tool_call_id, seq));
        }
    }
    resources
}
