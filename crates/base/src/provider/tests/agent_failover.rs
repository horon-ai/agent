fn agent_failover_set_default_model(model: &str) -> anyhow::Result<()> {
    crate::config::Config::set_default_model_only((!model.trim().is_empty()).then_some(model))
}

#[test]
fn agent_failover_same_model_rotation_precedes_config_and_respects_context() {
    with_clean_provider_test_env(|| {
        let rt = enter_test_runtime();
        let _guard = rt.enter();
        let (provider, _attempts, _cleanup) = account_routing_fixture(&[]);
        agent_failover_set_default_model("openai-oauth:gpt-6-astra").unwrap();
        let preserved = provider
            .cross_provider_pool_prompt(
                ActiveProvider::OpenAI,
                ActiveProvider::OpenAI,
                "401 unauthorized".into(),
                512,
                128,
            )
            .unwrap();
        assert_eq!(
            preserved.target_route.as_deref(),
            Some("openai-oauth@openai-fox:gpt-5.4")
        );
        assert!(
            provider
                .cross_provider_pool_prompt(
                    ActiveProvider::OpenAI,
                    ActiveProvider::OpenAI,
                    "401 unauthorized".into(),
                    5_504_580,
                    1_376_145,
                )
                .is_none(),
            "synthetic preserved-model candidates must also fit the input"
        );
    });
}

