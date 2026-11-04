use super::*;

#[test]
fn simple_completion_usage_keeps_latest_reported_values() {
    let mut usage = SimpleCompletionUsage::default();
    assert!(usage.is_empty());
    // Start event: input only.
    usage.observe(Some(1_000), None, Some(700), None);
    // Final event: output and a refreshed input snapshot.
    usage.observe(Some(1_050), Some(90), None, Some(12));
    assert_eq!(usage.input_tokens, Some(1_050));
    assert_eq!(usage.output_tokens, Some(90));
    assert_eq!(usage.cache_read_input_tokens, Some(700));
    assert_eq!(usage.cache_creation_input_tokens, Some(12));
    assert!(!usage.is_empty());
}

#[test]
fn metered_estimate_computes_reference_cost() {
    let estimate = RouteCheapnessEstimate::metered(
        RouteCostSource::Heuristic,
        RouteCostConfidence::Low,
        2_000_000,
        8_000_000,
        None,
        None,
    );
    assert_eq!(estimate.estimated_reference_cost_micros, Some(90_000));
}

#[test]
fn shared_http_client_reuses_builder() {
    let _a = shared_http_client();
    let _b = shared_http_client();
}

#[test]
fn fresh_transport_client_builds_distinct_clients() {
    // Each call must produce a brand-new client (new connection pool), not
    // a cached one: the whole point is that a retry after a transport
    // fault (e.g. TLS BadRecordMac) never reuses a possibly-poisoned
    // pooled connection.
    let _a = fresh_transport_client();
    let _b = fresh_transport_client();
}

#[test]
fn canonical_user_agent_identifies_horon_agent() {
    assert!(HORON_AGENT_USER_AGENT.starts_with("horon-agent/"));
}

#[test]
fn model_route_provider_label_matching_uses_aliases_without_substring_false_positives() {
    assert!(model_route_provider_labels_match("Anthropic", "Claude"));
    assert!(model_route_provider_labels_match("auto", "OpenRouter"));
    assert!(model_route_provider_labels_match("GitHub Copilot", "Copilot"));
    assert!(model_route_provider_labels_match("AWS Bedrock", "Bedrock"));
    assert!(!model_route_provider_labels_match("OpenRouter/OpenAI", "OpenAI"));
    assert!(!model_route_provider_labels_match("OpenAI", "OpenRouter"));
    assert!(!model_route_provider_labels_match("", ""));
    assert!(!model_route_provider_labels_related("OpenAI", ""));
}

#[test]
fn model_route_provider_key_matching_prefers_explicit_route_key() {
    assert!(model_route_provider_matches_key(
        Some("cerebras"),
        "Cerebras Cloud",
        "CEREBRAS"
    ));
    assert!(model_route_provider_matches_key(None, "Anthropic", "Claude"));
    assert!(!model_route_provider_matches_key(Some("cerebras"), "Cerebras", "groq"));
}

struct SnapshotTestProvider;

#[async_trait]
impl Provider for SnapshotTestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        unreachable!("snapshot test does not call complete")
    }

    fn name(&self) -> &str {
        "snapshot-provider"
    }

    fn model(&self) -> String {
        "snapshot-model".to_string()
    }

    fn available_models_display(&self) -> Vec<String> {
        vec!["snapshot-model".to_string()]
    }

    fn model_routes(&self) -> Vec<ModelRoute> {
        vec![ModelRoute {
            model: "snapshot-model".to_string(),
            provider: "Snapshot".to_string(),
            api_method: "snapshot-api".to_string(),
            available: true,
            detail: "test route".to_string(),
            usage: None,
            cheapness: None,
        }]
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(SnapshotTestProvider)
    }
}

#[test]
fn model_catalog_snapshot_materializes_provider_catalog_contract() {
    assert_eq!(SnapshotTestProvider.catalog_sharing_identity(), None);
    let snapshot = ModelCatalogSnapshot::from_provider(&SnapshotTestProvider);

    assert_eq!(snapshot.provider_name.as_deref(), Some("snapshot-provider"));
    assert_eq!(snapshot.provider_model.as_deref(), Some("snapshot-model"));
    assert_eq!(snapshot.available_models, ["snapshot-model"]);
    assert!(snapshot.has_routes());
    assert_eq!(snapshot.model_routes[0].api_method, "snapshot-api");
}

#[test]
fn runtime_key_distinguishes_openrouter_from_direct_compatible_profile() {
    assert_eq!(
        RuntimeKey::from_api_method(&ModelRouteApiMethod::parse("openrouter"), "auto"),
        RuntimeKey::OpenRouter
    );
    assert_eq!(
        RuntimeKey::from_api_method(
            &ModelRouteApiMethod::parse("openai-compatible:nvidia-nim"),
            "NVIDIA NIM",
        ),
        RuntimeKey::OpenAiCompatible {
            profile_id: Some("nvidia-nim".to_string())
        }
    );
}

#[test]
fn route_selection_preserves_runtime_identity_from_model_route() {
    let selection = RouteSelection::from_model_route(&ModelRoute {
        model: "openrouter/owl-alpha".to_string(),
        provider: "OpenRouter".to_string(),
        api_method: "openrouter".to_string(),
        available: true,
        detail: "https://openrouter.ai/api/v1".to_string(),
        usage: None,
        cheapness: None,
    });
    assert_eq!(selection.model, "openrouter/owl-alpha");
    assert_eq!(selection.runtime_key, RuntimeKey::OpenRouter);
    assert_eq!(selection.api_method, "openrouter");

    let selection = RouteSelection::from_model_route(&ModelRoute {
        model: "nvidia/example".to_string(),
        provider: "NVIDIA NIM".to_string(),
        api_method: "openai-compatible:nvidia-nim".to_string(),
        available: true,
        detail: "https://integrate.api.nvidia.com/v1".to_string(),
        usage: None,
        cheapness: None,
    });
    assert_eq!(
        selection.runtime_key,
        RuntimeKey::OpenAiCompatible {
            profile_id: Some("nvidia-nim".to_string())
        }
    );
    assert_eq!(selection.provider_label, "NVIDIA NIM");
}

#[test]
fn grok_build_route_selection_is_a_first_class_runtime() {
    let selection = RouteSelection::from_model_route(&ModelRoute {
        model: "grok-4.6".to_string(),
        provider: "Grok Build".to_string(),
        api_method: "grok-build-acp".to_string(),
        available: true,
        detail: "Grok Build subscription via HoronAgent-managed ACP".to_string(),
        cheapness: None,
        usage: None,
    });
    assert_eq!(selection.runtime_key, RuntimeKey::GrokBuild);
    assert_eq!(selection.runtime_key.stable_id(), "grok-build");
    assert_eq!(selection.routed_model_spec(), "grok-build:grok-4.6");

    let prefixed = RouteSelection::from_model_route(&ModelRoute {
        model: "grok-build:grok-4.6".to_string(),
        provider: "Grok Build".to_string(),
        api_method: "grok-build-acp".to_string(),
        available: true,
        detail: String::new(),
        cheapness: None,
        usage: None,
    });
    assert_eq!(prefixed.routed_model_spec(), "grok-build:grok-4.6");
}

#[test]
fn runtime_key_other_is_internally_tagged_wire_safe() {
    // Internally tagged newtype `Other(String)` cannot be serialized by
    // serde. The struct variant is the wire form used by SetRoute.
    let key = RuntimeKey::Other {
        method: "custom-acp".to_string(),
    };
    let json = serde_json::to_value(&key).expect("Other must serialize");
    assert_eq!(json["kind"], "other");
    assert_eq!(json["method"], "custom-acp");
    let decoded: RuntimeKey = serde_json::from_value(json).expect("Other must deserialize");
    assert_eq!(
        decoded,
        RuntimeKey::Other {
            method: "custom-acp".to_string()
        }
    );
}

#[test]
fn grok_build_runtime_key_is_internally_tagged_wire_safe() {
    let json = serde_json::to_value(&RuntimeKey::GrokBuild).expect("GrokBuild must serialize");
    assert_eq!(json, serde_json::json!({"kind": "grok-build"}));
    let decoded: RuntimeKey = serde_json::from_value(json).expect("GrokBuild must deserialize");
    assert_eq!(decoded, RuntimeKey::GrokBuild);
}
