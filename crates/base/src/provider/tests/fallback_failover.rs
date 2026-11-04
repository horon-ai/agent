include!("agent_failover.rs");

#[test]
fn test_active_provider_env_only_seeds_sessions_when_explicitly_selected() {
    with_clean_provider_test_env(|| {
        crate::env::set_var("HORON_AGENT_ACTIVE_PROVIDER", "openai");
        assert_eq!(MultiProvider::initial_provider_from_env(), None);

        crate::provider::activation::select_initial_runtime_provider_key("openai");
        assert_eq!(
            MultiProvider::initial_provider_from_env(),
            Some(ActiveProvider::OpenAI)
        );

        crate::provider::activation::clear_initial_runtime_provider();
        assert_eq!(MultiProvider::initial_provider_from_env(), None);
    });
}

#[test]
fn test_should_failover_on_403_forbidden() {
    let err = anyhow::anyhow!(
        "Copilot token exchange failed (HTTP 403 Forbidden): not accessible by integration"
    );
    assert!(MultiProvider::classify_failover_error(&err).should_failover());
}

#[test]
fn test_should_failover_on_token_exchange_failed() {
    let msg = r#"Copilot token exchange failed (HTTP 403 Forbidden): {"error_details":{"title":"Contact Support"}}"#;
    let err = anyhow::anyhow!("{}", msg);
    assert!(MultiProvider::classify_failover_error(&err).should_failover());
}

#[test]
fn test_should_failover_on_access_denied() {
    let err = anyhow::anyhow!("Access denied: account suspended");
    assert!(MultiProvider::classify_failover_error(&err).should_failover());
}

#[test]
fn test_should_failover_when_status_code_starts_message() {
    let err = anyhow::anyhow!("401 unauthorized");
    assert!(MultiProvider::classify_failover_error(&err).should_failover());
    assert_eq!(
        MultiProvider::classify_failover_error(&err),
        FailoverDecision::RetryAndMarkUnavailable
    );
}

#[test]
fn test_should_not_failover_on_non_independent_status_digits() {
    let err = anyhow::anyhow!("backend returned code 14290");
    assert!(!MultiProvider::classify_failover_error(&err).should_failover());
}

#[test]
fn test_context_limit_error_fails_over_without_marking_provider_unavailable() {
    let err = anyhow::anyhow!("Context length exceeded maximum context window");
    assert!(MultiProvider::classify_failover_error(&err).should_failover());
    assert_eq!(
        MultiProvider::classify_failover_error(&err),
        FailoverDecision::RetryNextProvider
    );
}

#[test]
fn test_should_not_failover_on_generic_error() {
    let err = anyhow::anyhow!("Connection timed out");
    assert!(!MultiProvider::classify_failover_error(&err).should_failover());
}
