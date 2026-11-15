//! Parsing and auth-resolution tests for `[providers.<name>]` profiles.

use super::{NamedProviderAuth, NamedProviderConfig, NamedProviderType};

fn parse(toml_source: &str) -> NamedProviderConfig {
    toml::from_str(toml_source).expect("profile table must parse")
}

fn profile_with_type(provider_type: &str) -> NamedProviderConfig {
    parse(&format!(
        "type = \"{provider_type}\"\nbase_url = \"https://gateway.example.test/v1\"\n"
    ))
}

#[test]
fn four_type_strings_parse() {
    assert_eq!(
        profile_with_type("openai-compatible").provider_type,
        NamedProviderType::OpenAiCompatible
    );
    assert_eq!(
        profile_with_type("openai-responses").provider_type,
        NamedProviderType::OpenAiResponses
    );
    assert_eq!(
        profile_with_type("anthropic-compatible").provider_type,
        NamedProviderType::AnthropicCompatible
    );
    assert_eq!(
        profile_with_type("gemini-compatible").provider_type,
        NamedProviderType::GeminiCompatible
    );
}

#[test]
fn snake_case_aliases_parse() {
    assert_eq!(
        profile_with_type("openai_responses").provider_type,
        NamedProviderType::OpenAiResponses
    );
    assert_eq!(
        profile_with_type("gemini_compatible").provider_type,
        NamedProviderType::GeminiCompatible
    );
    assert_eq!(
        profile_with_type("openai_compatible").provider_type,
        NamedProviderType::OpenAiCompatible
    );
    assert_eq!(
        profile_with_type("anthropic_compatible").provider_type,
        NamedProviderType::AnthropicCompatible
    );
}

#[test]
fn new_type_strings_serialize_back_to_their_kebab_case_names() {
    let responses = NamedProviderConfig {
        provider_type: NamedProviderType::OpenAiResponses,
        base_url: "https://gateway.example.test/v1".to_string(),
        ..Default::default()
    };
    let encoded = toml::to_string(&responses).expect("profile must serialize");
    assert!(
        encoded.contains("type = \"openai-responses\""),
        "responses profile must write its kebab-case type, got:\n{encoded}"
    );

    let gemini = NamedProviderConfig {
        provider_type: NamedProviderType::GeminiCompatible,
        base_url: "https://gateway.example.test/v1".to_string(),
        ..Default::default()
    };
    let encoded = toml::to_string(&gemini).expect("profile must serialize");
    assert!(
        encoded.contains("type = \"gemini-compatible\""),
        "gemini profile must write its kebab-case type, got:\n{encoded}"
    );
}

#[test]
fn unknown_type_is_rejected_as_unknown_variant() {
    let error =
        toml::from_str::<NamedProviderConfig>("type = \"bogus\"\nbase_url = \"https://gateway.example.test/v1\"\n")
            .expect_err("an unknown provider type must not parse");
    let message = error.to_string();
    assert!(
        message.contains("unknown variant"),
        "expected a serde unknown-variant error, got: {message}"
    );
    assert!(message.contains("bogus"), "the error must name the value: {message}");
}

#[test]
fn absent_auth_resolves_to_the_type_default() {
    let cases = [
        ("openai-compatible", NamedProviderAuth::Bearer),
        ("openai-responses", NamedProviderAuth::Bearer),
        ("open-router", NamedProviderAuth::Bearer),
        ("anthropic-compatible", NamedProviderAuth::Header),
        ("gemini-compatible", NamedProviderAuth::Header),
    ];
    for (type_name, expected) in cases {
        let profile = profile_with_type(type_name);
        assert_eq!(
            profile.resolved_auth(),
            expected,
            "{type_name}: omitted auth must resolve to the type default"
        );
    }
}

#[test]
fn absent_auth_header_defaults_per_type() {
    let cases = [
        ("openai-compatible", None),
        ("openai-responses", None),
        ("anthropic-compatible", Some("x-api-key")),
        ("gemini-compatible", Some("x-goog-api-key")),
    ];
    for (type_name, expected) in cases {
        let profile = profile_with_type(type_name);
        assert_eq!(
            profile.resolved_auth_header().as_deref(),
            expected,
            "{type_name}: default auth must resolve to the documented header"
        );
    }
}

#[test]
fn explicit_bearer_on_anthropic_wins_over_the_type_default() {
    let profile = parse(
        r#"
        type = "anthropic-compatible"
        base_url = "https://gateway.example.test/v1"
        auth = "bearer"
        "#,
    );
    assert_eq!(profile.resolved_auth(), NamedProviderAuth::Bearer);
    assert_eq!(
        profile.resolved_auth_header(),
        None,
        "bearer mode carries no custom header"
    );
}

#[test]
fn explicit_header_without_auth_header_uses_the_type_default_name() {
    let cases = [
        ("openai-compatible", "api-key"),
        ("openai-responses", "api-key"),
        ("anthropic-compatible", "x-api-key"),
        ("gemini-compatible", "x-goog-api-key"),
    ];
    for (type_name, expected) in cases {
        let profile = parse(&format!(
            r#"
            type = "{type_name}"
            base_url = "https://gateway.example.test/v1"
            auth = "header"
            "#
        ));
        assert_eq!(profile.resolved_auth(), NamedProviderAuth::Header);
        assert_eq!(
            profile.resolved_auth_header().as_deref(),
            Some(expected),
            "{type_name}: header mode without auth_header must use the type default"
        );
    }
}

#[test]
fn explicit_auth_header_wins_over_the_type_default() {
    let profile = parse(
        r#"
        type = "anthropic-compatible"
        base_url = "https://gateway.example.test/v1"
        auth = "header"
        auth_header = "x-tenant-token"
        "#,
    );
    assert_eq!(profile.resolved_auth_header().as_deref(), Some("x-tenant-token"));

    // A blank value is not a header name; the type default applies instead.
    let blank = parse(
        r#"
        type = "gemini-compatible"
        base_url = "https://gateway.example.test/v1"
        auth = "header"
        auth_header = "   "
        "#,
    );
    assert_eq!(blank.resolved_auth_header().as_deref(), Some("x-goog-api-key"));
}

#[test]
fn no_auth_resolves_to_no_header() {
    for type_name in [
        "openai-compatible",
        "openai-responses",
        "anthropic-compatible",
        "gemini-compatible",
    ] {
        let profile = parse(&format!(
            r#"
            type = "{type_name}"
            base_url = "https://gateway.example.test/v1"
            auth = "none"
            auth_header = "x-api-key"
            "#
        ));
        assert_eq!(profile.resolved_auth(), NamedProviderAuth::None);
        assert_eq!(
            profile.resolved_auth_header(),
            None,
            "{type_name}: no-auth mode must not report a header"
        );
    }
}

/// A profile written before this change keeps its explicit auth values; the
/// resolution helpers must reproduce them exactly.
#[test]
fn explicit_auth_and_header_written_before_this_change_still_parse_identically() {
    // `auth = "header"` differs from this type's Bearer default, and the
    // custom header differs from the OpenAI default `api-key`, so the resolved
    // pair proves both explicit values survived the schema change.
    let profile = parse(
        r#"
        type = "openai-compatible"
        base_url = "https://llm.example.com/v1/"
        auth = "header"
        auth_header = "x-tenant-key"
        api_key_env = "MY_GATEWAY_API_KEY"
        default_model = "opaque/model@id"
        model_catalog = false

        [[models]]
        id = "opaque/model@id"
        "#,
    );
    assert_eq!(profile.auth_header.as_deref(), Some("x-tenant-key"));
    assert_eq!(profile.resolved_auth(), NamedProviderAuth::Header);
    assert_eq!(profile.resolved_auth_header().as_deref(), Some("x-tenant-key"));
    assert_eq!(profile.api_key_env.as_deref(), Some("MY_GATEWAY_API_KEY"));
    assert_eq!(profile.default_model.as_deref(), Some("opaque/model@id"));
    assert_eq!(profile.models.len(), 1);
}

#[test]
fn capitalised_auth_spellings_still_parse() {
    let profile = parse(
        r#"
        type = "openai-compatible"
        base_url = "https://api.mistral.ai/v1"
        auth = "Bearer"
        "#,
    );
    assert_eq!(profile.resolved_auth(), NamedProviderAuth::Bearer);
}

/// An unset `auth` must stay absent from the written TOML, so a saved config
/// keeps meaning "the type default" instead of pinning today's value.
#[test]
fn omitted_auth_is_not_written_back() {
    let profile = profile_with_type("gemini-compatible");
    let encoded = toml::to_string(&profile).expect("profile must serialize");
    assert!(
        !encoded.contains("auth ="),
        "an unset auth must not be serialized, got:\n{encoded}"
    );
    let reread: NamedProviderConfig = toml::from_str(&encoded).expect("written profile must parse");
    assert_eq!(reread.resolved_auth(), NamedProviderAuth::Header);
}
