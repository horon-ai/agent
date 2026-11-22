use super::*;
use crate::auth::test_sandbox::AuthTestSandbox;
use serde_json::json;

#[test]
fn applies_every_choice_and_leaves_it_intact_after_rejected_presets() {
    let _sandbox = AuthTestSandbox::new().unwrap();
    let config = serde_json::from_value(json!({
        "base_url": "http://127.0.0.1:1/v1",
        "auth": "none",
        "default_model": "daily",
        "models": [{"id": "daily"}, {"id": "focus"}]
    }))
    .unwrap();
    let provider = crate::provider::external::build_named_openai_compatible_runtime("fixture", &config).unwrap();
    let mut agent = Agent::new_with_initial_working_dir(provider, crate::tool::Registry::empty(), None);
    let selected = serde_json::from_value(json!({
        "model": "focus",
        "effort": "high",
        "roles": {"swarm": "focus", "review": "daily", "judge": "focus", "memory": "inherit", "ambient": "daily"}
    }))
    .unwrap();
    agent.model_preset(&selected, false).unwrap();
    assert_eq!(agent.provider_model(), "daily");
    assert_eq!(agent.runtime_agent_model_overrides(), json!({}));
    agent.model_preset(&selected, true).unwrap();
    for invalid in [
        json!({"model": "daily", "effort": "low", "roles": {"swarm": "missing"}}),
        json!({"model": "daily", "effort": "low", "roles": {"unknown": "focus"}}),
        json!({"model": "daily", "effort": "missing", "roles": {"swarm": "daily"}}),
    ] {
        assert!(
            agent
                .model_preset(&serde_json::from_value(invalid).unwrap(), true)
                .is_err()
        );
    }
    assert_eq!(agent.provider_model(), "focus");
    assert_eq!(agent.provider_reasoning_effort().as_deref(), Some("high"));
    assert_eq!(
        agent.runtime_agent_model_overrides(),
        json!({
            "swarm": "focus", "review": "daily", "judge": "focus", "memory": "inherit", "ambient": "daily"
        })
    );
}
