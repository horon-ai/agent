//! Prompt cache policy tests for the direct Chat Completions provider.
//!
//! Key derivation and the per-host default are body-level rules, so these
//! tests build bodies directly instead of driving a server.

use super::*;
use serde_json::{Value, json};

const QA_KEY: &str = "sk-test-secret";

// ---------------------------------------------------------------------------
// Prompt cache policy

pub(super) fn cache_context(root: &str) -> ProviderRequestContext {
    ProviderRequestContext {
        session_id: Some(format!("{root}-session")),
        parent_session_id: None,
        cache_root_id: Some(root.to_string()),
    }
}

pub(super) fn prompt_tool() -> ToolDefinition {
    ToolDefinition::new("read", "Read a file.", json!({ "type": "object" }))
}

fn build_body(
    provider: &OpenAiCompatibleProvider,
    messages: &[Message],
    tools: &[ToolDefinition],
    system: &str,
    context: Option<&ProviderRequestContext>,
) -> Value {
    serde_json::from_slice(&provider.build_body(messages, tools, system, context)).expect("the body is JSON")
}

/// `pck_` plus 24 lowercase hex characters.
pub(super) fn is_prompt_cache_key(value: &Value) -> bool {
    value.as_str().is_some_and(|key| {
        key.strip_prefix("pck_").is_some_and(|digest| {
            digest.len() == 24
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    })
}

fn openai_provider(model: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        "https://api.openai.com/v1",
        ChatAuth::Bearer,
        Some(QA_KEY.to_string()),
        model,
        vec![model.to_string()],
    )
    .expect("provider")
}

#[test]
fn prompt_cache_key_is_stable_for_one_lineage_and_ignores_conversation_turns() {
    let provider = openai_provider("gpt-5.1");
    let tools = vec![prompt_tool()];
    let context = cache_context("root-a");

    let first = build_body(
        &provider,
        &[Message::user("first question")],
        &tools,
        "You are horon.",
        Some(&context),
    );
    let second = build_body(
        &provider,
        &[Message::user("first question"), Message::user("second question")],
        &tools,
        "You are horon.",
        Some(&context),
    );

    assert!(is_prompt_cache_key(&first["prompt_cache_key"]), "{first}");
    assert_eq!(
        first["prompt_cache_key"], second["prompt_cache_key"],
        "a compaction that rewrites the turns keeps the key"
    );
}

#[test]
fn prompt_cache_key_differs_between_lineages_and_scope_kinds() {
    let provider = openai_provider("gpt-5.1");
    let tools = vec![prompt_tool()];
    let messages = [Message::user("hi")];

    let a = build_body(&provider, &messages, &tools, "s", Some(&cache_context("root-a")));
    let b = build_body(&provider, &messages, &tools, "s", Some(&cache_context("root-b")));
    assert_ne!(a["prompt_cache_key"], b["prompt_cache_key"]);
    assert!(is_prompt_cache_key(&a["prompt_cache_key"]));

    // The system prompt and the tool catalog are part of the key, so a
    // different profile prefix does not collide with this lineage.
    let other_system = build_body(
        &provider,
        &messages,
        &tools,
        "another system",
        Some(&cache_context("root-a")),
    );
    assert_ne!(a["prompt_cache_key"], other_system["prompt_cache_key"]);

    // Without a lineage root the session id is the scope.
    let session_only = ProviderRequestContext {
        session_id: Some("session-1".to_string()),
        parent_session_id: None,
        cache_root_id: None,
    };
    let by_session = build_body(&provider, &messages, &tools, "s", Some(&session_only));
    assert!(is_prompt_cache_key(&by_session["prompt_cache_key"]));
}

#[test]
fn prompt_cache_key_is_absent_without_a_scope_or_when_disabled() {
    let provider = openai_provider("gpt-5.1");
    let messages = [Message::user("hi")];
    let empty_scope = ProviderRequestContext::default();

    let no_scope = build_body(&provider, &messages, &[], "s", Some(&empty_scope));
    assert!(no_scope.get("prompt_cache_key").is_none(), "{no_scope}");
    let no_context = build_body(&provider, &messages, &[], "s", None);
    assert!(no_context.get("prompt_cache_key").is_none(), "{no_context}");

    let disabled = provider.with_prompt_cache_key_policy(Some(false));
    let body = build_body(&disabled, &messages, &[], "s", Some(&cache_context("root-a")));
    assert!(body.get("prompt_cache_key").is_none(), "{body}");
}

#[test]
fn prompt_cache_key_defaults_off_for_unknown_hosts_and_on_for_known_ones() {
    let messages = [Message::user("hi")];
    let context = cache_context("root-a");
    let with_host = |host: &str| {
        OpenAiCompatibleProvider::new(
            format!("https://{host}/v1"),
            ChatAuth::None,
            None,
            "m",
            vec!["m".to_string()],
        )
        .expect("provider")
    };

    let openrouter = build_body(&with_host("openrouter.ai"), &messages, &[], "s", Some(&context));
    assert!(is_prompt_cache_key(&openrouter["prompt_cache_key"]), "{openrouter}");

    let unknown = build_body(&with_host("llm.example.com"), &messages, &[], "s", Some(&context));
    assert!(unknown.get("prompt_cache_key").is_none(), "{unknown}");

    // The policy can force the field onto a strict host or off a known one.
    let forced = with_host("llm.example.com").with_prompt_cache_key_policy(Some(true));
    let forced_body = build_body(&forced, &messages, &[], "s", Some(&context));
    assert!(is_prompt_cache_key(&forced_body["prompt_cache_key"]), "{forced_body}");
}

fn retention_resolver() -> PromptCacheRetentionResolver {
    Arc::new(|model: &str| (model == "gpt-5.1").then(|| "24h".to_string()))
}

#[test]
fn prompt_cache_retention_is_sent_only_to_the_official_openai_host() {
    let messages = [Message::user("hi")];
    let body_at = |base_url: &str, model: &str| {
        let provider = OpenAiCompatibleProvider::new(base_url, ChatAuth::None, None, model, vec![model.to_string()])
            .expect("provider")
            .with_prompt_cache_retention_resolver(retention_resolver());
        build_body(&provider, &messages, &[], "s", None)
    };

    assert_eq!(
        body_at("https://api.openai.com/v1", "gpt-5.1")["prompt_cache_retention"],
        json!("24h")
    );
    assert!(
        body_at("https://openrouter.ai/api/v1", "gpt-5.1")
            .get("prompt_cache_retention")
            .is_none(),
        "a gateway is not the official endpoint"
    );
    assert!(
        body_at("https://api.openai.com/v1", "gpt-5.6")
            .get("prompt_cache_retention")
            .is_none(),
        "the resolver decides per model"
    );
    // A switched model re-evaluates the resolver instead of keeping the old value.
    let provider = OpenAiCompatibleProvider::new(
        "https://api.openai.com/v1",
        ChatAuth::None,
        None,
        "gpt-5.6",
        vec!["gpt-5.1".to_string()],
    )
    .expect("provider")
    .with_prompt_cache_retention_resolver(retention_resolver());
    provider.set_model("gpt-5.1").expect("model switch");
    let switched = build_body(&provider, &messages, &[], "s", None);
    assert_eq!(switched["prompt_cache_retention"], json!("24h"), "{switched}");
}

#[test]
fn extra_body_overrides_generated_prompt_cache_fields() {
    let provider = openai_provider("gpt-5.1")
        .with_extra_body(json!({ "prompt_cache_key": "profile-key", "prompt_cache_retention": "in_memory" }))
        .with_prompt_cache_retention_resolver(retention_resolver());
    let body = build_body(
        &provider,
        &[Message::user("hi")],
        &[],
        "s",
        Some(&cache_context("root-a")),
    );
    assert_eq!(body["prompt_cache_key"], json!("profile-key"));
    assert_eq!(body["prompt_cache_retention"], json!("in_memory"));
}
