//! Runtime identity replies: the fields `get_runtime_info` and a model switch
//! report, including the configured fallback list and the model that actually
//! served the last response.

use super::{BridgeState, Outbound};
use horon_agent_harness_api::ApiEvent;
use serde_json::{Value, json};

fn state_with_session() -> BridgeState {
    BridgeState {
        session_id: Some("s1".into()),
        ..Default::default()
    }
}

/// Ask for runtime info and answer its catalog probe.
///
/// The probe is unconditional now, so every runtime-info assertion has to
/// reply the way the daemon would: a `history` payload with the identity keys.
fn runtime_info_after_probe(state: &mut BridgeState, api_id: u64, catalog: Value) -> ApiEvent {
    let out = state.api_request_to_legacy(&json!({
        "req": "get_runtime_info", "id": api_id, "session_id": "s1"
    }));
    let Outbound::Legacy(probe) = &out[0] else {
        panic!("runtime info must refresh the catalog");
    };
    let mut reply = catalog;
    reply["type"] = json!("history");
    reply["id"] = probe["id"].clone();
    reply["messages"] = json!([]);
    state
        .legacy_event_to_api(&reply)
        .into_iter()
        .find(|frame| frame.reply_to == Some(api_id))
        .map(|frame| frame.event)
        .unwrap_or_else(|| panic!("runtime info reply for request {api_id}"))
}

#[test]
fn runtime_info_reports_the_active_provider_and_complete_route_catalog() {
    let mut state = state_with_session();
    state.legacy_event_to_api(&json!({
        "type": "available_models_updated",
        "provider_name": "anthropic",
        "provider_model": "claude-sonnet",
        "reasoning_effort": "high",
        "resolved_credential": "oauth",
        "available_models": ["claude-sonnet", "gemini-pro"],
        "available_model_routes": [
            {
                "model": "claude-sonnet",
                "provider": "anthropic",
                "api_method": "messages",
                "available": true,
                "detail": "ready"
            },
            {
                "model": "gemini-pro",
                "provider": "gemini",
                "api_method": "generateContent",
                "available": false,
                "detail": "credential missing"
            }
        ]
    }));

    let event = runtime_info_after_probe(
        &mut state,
        4,
        json!({
            "provider_name": "anthropic",
            "provider_model": "claude-sonnet",
            "available_models": ["claude-sonnet", "gemini-pro"]
        }),
    );
    let ApiEvent::RuntimeInfo {
        session_id,
        provider,
        model,
        reasoning_effort,
        auth_method,
        fallback_models,
        serving_model,
        routes,
    } = event
    else {
        panic!("expected runtime info, got {event:?}");
    };
    assert_eq!(session_id, "s1");
    assert_eq!(provider.as_deref(), Some("anthropic"));
    assert_eq!(model.as_deref(), Some("claude-sonnet"));
    assert_eq!(reasoning_effort.as_deref(), Some("high"));
    // The daemon's resolved credential travels with the identity, so a
    // client never has to guess it from a model that has both routes.
    assert_eq!(auth_method.as_deref(), Some("oauth"));
    assert!(fallback_models.is_empty());
    assert!(serving_model.is_none());
    assert_eq!(routes.len(), 2);
    assert_eq!(routes[1].provider, "gemini");
    assert!(!routes[1].available);
}

#[test]
fn model_usage_survives_catalogs_and_live_updates_without_a_round_trip() {
    let mut state = state_with_session();
    let mut route = json!({"model":"test-model","provider":"OpenAI","api_method":"openai-oauth",
        "available":true,"detail":"ready", "usage":{"count":2,"last_used_unix_secs":30,
        "tracking_started_unix_secs":10,"selection_count":5,"last_selected_unix_secs":9}});
    let catalog = json!({"type":"available_models_updated","provider_model":"test-model",
        "available_models":["test-model"],"available_model_routes":[route.clone()]});
    state.legacy_event_to_api(&catalog);
    route["usage"]["count"] = json!(3);
    route["usage"]["last_used_unix_secs"] = json!(40);
    let frames = state.legacy_event_to_api(&json!({"type":"model_usage_updated","route":route}));
    let ApiEvent::RuntimeInfo { routes, .. } = &frames[0].event else {
        panic!("usage must push runtime info");
    };
    assert_eq!(routes[0].usage.as_ref().unwrap().count, 3);
    // A later catalog refresh must not undo a newer usage delta.
    let event = runtime_info_after_probe(
        &mut state,
        88,
        json!({"provider_model":"test-model","available_models":["test-model"]}),
    );
    let ApiEvent::RuntimeInfo { routes, .. } = event else {
        panic!("expected runtime info");
    };
    let usage = routes[0].usage.as_ref().unwrap();
    assert_eq!(usage.count, 3);
    assert_eq!(usage.last_used_unix_secs, Some(40));
    assert_eq!(usage.selection_count, 5);
    assert_eq!(usage.tracking_started_unix_secs, Some(10));
}

#[test]
fn runtime_info_reports_the_model_fallback_list_and_the_serving_model() {
    let mut state = state_with_session();
    // The daemon sends identity just before the catalog reply. `provider_model`
    // is the session's own selection again, while the fallback that answered
    // the last turn is reported separately.
    state.legacy_event_to_api(&json!({
        "type": "available_models_updated",
        "provider_name": "fallback-gw",
        "provider_model": "m1",
        "available_models": ["m1", "m2"],
        "identity": {"fallback_models": ["m2"], "serving_model": "m2"}
    }));

    let event = runtime_info_after_probe(
        &mut state,
        5,
        json!({
            "provider_name": "fallback-gw",
            "provider_model": "m1",
            "available_models": ["m1", "m2"],
            "identity": {"fallback_models": ["m2"], "serving_model": "m2"}
        }),
    );
    let ApiEvent::RuntimeInfo {
        model,
        fallback_models,
        serving_model,
        ..
    } = &event
    else {
        panic!("expected runtime info, got {event:?}");
    };
    assert_eq!(model.as_deref(), Some("m1"));
    assert_eq!(fallback_models, &["m2".to_string()]);
    assert_eq!(serving_model.as_deref(), Some("m2"));
    // A client reads the wire values, so both keys must be on the frame.
    let value = serde_json::to_value(&event).expect("serialize runtime info");
    assert_eq!(value["fallback_models"], json!(["m2"]));
    assert_eq!(value["serving_model"], json!("m2"));
}

#[test]
fn runtime_info_omits_the_model_fallback_fields_when_unset() {
    let mut state = state_with_session();
    let event = runtime_info_after_probe(
        &mut state,
        6,
        json!({"provider_model": "m1", "available_models": ["m1"]}),
    );
    let ApiEvent::RuntimeInfo {
        fallback_models,
        serving_model,
        ..
    } = &event
    else {
        panic!("expected runtime info, got {event:?}");
    };
    assert!(fallback_models.is_empty());
    assert!(serving_model.is_none());
    // An unconfigured list is an absent key, not an empty array, so a client
    // can tell "no fallbacks" apart from "this daemon never reported them".
    let value = serde_json::to_value(&event).expect("serialize runtime info");
    assert!(value.get("fallback_models").is_none());
    assert!(value.get("serving_model").is_none());
}
