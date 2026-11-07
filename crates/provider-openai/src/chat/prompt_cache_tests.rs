//! Prompt cache policy tests for the direct Chat Completions provider.
//!
//! Key derivation and the per-host default are body-level rules, so these
//! tests build bodies directly instead of driving a server.

use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Instant;

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

// ---------------------------------------------------------------------------
// Memoized derivation

#[test]
fn prompt_cache_memo_reuses_the_key_without_recomputing() {
    let provider = openai_provider("gpt-5.1");
    let tools = vec![prompt_tool()];
    let context = cache_context("root-a");
    let messages = [Message::user("first question")];

    let first = build_body(&provider, &messages, &tools, "You are horon.", Some(&context));
    assert_eq!(provider_derivations(&provider), 1);
    let second = build_body(&provider, &messages, &tools, "You are horon.", Some(&context));

    assert_eq!(first["prompt_cache_key"], second["prompt_cache_key"]);
    assert!(is_prompt_cache_key(&second["prompt_cache_key"]), "{second}");
    assert_eq!(
        provider_derivations(&provider),
        1,
        "a steady-state turn must not hash again"
    );
}

#[test]
fn prompt_cache_memo_recomputes_when_an_input_changes() {
    let provider = openai_provider("gpt-5.1");
    let tools = vec![prompt_tool()];
    let context = cache_context("root-a");
    let messages = [Message::user("hi")];
    let base = build_body(&provider, &messages, &tools, "s", Some(&context));
    assert_eq!(provider_derivations(&provider), 1);

    let other_scope = build_body(&provider, &messages, &tools, "s", Some(&cache_context("root-b")));
    assert_ne!(base["prompt_cache_key"], other_scope["prompt_cache_key"]);
    assert_eq!(provider_derivations(&provider), 2);

    let other_system = build_body(&provider, &messages, &tools, "another system", Some(&context));
    assert_ne!(base["prompt_cache_key"], other_system["prompt_cache_key"]);
    assert_eq!(provider_derivations(&provider), 3);

    let mut changed_tools = tools.clone();
    changed_tools.push(ToolDefinition::new(
        "write",
        "Write a file.",
        json!({ "type": "object" }),
    ));
    let other_tools = build_body(&provider, &messages, &changed_tools, "s", Some(&context));
    println!(
        "tools=1 key={} tools=2 key={} derivations={}",
        base["prompt_cache_key"].as_str().unwrap_or(""),
        other_tools["prompt_cache_key"].as_str().unwrap_or(""),
        provider_derivations(&provider),
    );
    assert_ne!(
        base["prompt_cache_key"], other_tools["prompt_cache_key"],
        "a changed tool list must not reuse the previous key"
    );
    assert_eq!(provider_derivations(&provider), 4);

    // Raising the tool count and then dropping it back never hands out a stale key.
    let back = build_body(&provider, &messages, &tools, "s", Some(&context));
    assert_eq!(base["prompt_cache_key"], back["prompt_cache_key"]);
    assert_eq!(provider_derivations(&provider), 5);
}

#[test]
fn prompt_cache_memo_key_is_byte_identical_to_the_unmemoized_derivation() {
    let provider = openai_provider("gpt-5.1");
    let tools = vec![prompt_tool()];
    let context = cache_context("root-a");
    let body = build_body(
        &provider,
        &[Message::user("hi")],
        &tools,
        "You are horon.",
        Some(&context),
    );

    let expected = unmemoized_prompt_cache_key("root-a", &body);
    assert_eq!(body["prompt_cache_key"], json!(expected));
    assert_eq!(
        provider.prompt_cache_key_for("root-a", &body),
        expected,
        "a memo hit returns the same bytes as a fresh derivation"
    );
}

#[test]
fn prompt_cache_memo_is_empty_after_a_fork() {
    let provider = openai_provider("gpt-5.1");
    let tools = vec![prompt_tool()];
    let context = cache_context("root-a");
    let messages = [Message::user("hi")];
    let body = build_body(&provider, &messages, &tools, "s", Some(&context));
    assert_eq!(provider_derivations(&provider), 1);

    let forked = provider.clone_for_fork();
    assert_eq!(
        forked.prompt_cache_key_derivations(),
        0,
        "a fork must not inherit the parent's memo"
    );
    assert_eq!(body["prompt_cache_key"], forked.prompt_cache_key_for("root-a", &body));
    assert_eq!(forked.prompt_cache_key_derivations(), 1);
    assert_eq!(
        provider_derivations(&provider),
        1,
        "deriving in the fork leaves the parent's memo untouched"
    );
}

#[test]
fn prompt_cache_memo_is_shared_safely_across_threads() {
    let provider = Arc::new(openai_provider("gpt-5.1"));
    let tools = vec![prompt_tool()];
    let context = cache_context("root-a");
    let messages = [Message::user("hi")];

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let provider = Arc::clone(&provider);
            let tools = tools.clone();
            let context = context.clone();
            let messages = messages.clone();
            std::thread::spawn(move || {
                (0..50)
                    .map(|_| {
                        let body = build_body(&provider, &messages, &tools, "s", Some(&context));
                        body["prompt_cache_key"].clone()
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let keys: Vec<Value> = handles
        .into_iter()
        .flat_map(|handle| handle.join().expect("worker"))
        .collect();

    assert!(keys.iter().all(is_prompt_cache_key), "{keys:?}");
    assert!(
        keys.windows(2).all(|pair| pair[0] == pair[1]),
        "every request in one lineage shares one key"
    );
    assert_eq!(
        provider_derivations(&provider),
        1,
        "the first derivation is the only one"
    );
}

/// The derivation exactly as it read the built body before the memo existed,
/// copied verbatim from that version: the memoized path must hand back these
/// same bytes, including the field order and the separator handling.
fn unmemoized_prompt_cache_key(scope: &str, body: &Value) -> String {
    let system = body["messages"]
        .as_array()
        .and_then(|messages| messages.first())
        .filter(|first| first["role"].as_str() == Some("system"))
        .and_then(|first| first["content"].as_str())
        .unwrap_or_default();
    let tools = body.get("tools").map(Value::to_string).unwrap_or_default();

    let mut hasher = Sha256::new();
    hasher.update(scope.as_bytes());
    hasher.update([0]);
    hasher.update(system.as_bytes());
    hasher.update([0]);
    hasher.update(tools.as_bytes());
    let digest = hasher.finalize();

    let mut key = String::with_capacity("pck_".len() + 24);
    key.push_str("pck_");
    for byte in &digest[..12] {
        use std::fmt::Write as _;
        write!(key, "{byte:02x}").expect("writing to a String cannot fail");
    }
    key
}

fn provider_derivations(provider: &OpenAiCompatibleProvider) -> u64 {
    provider.prompt_cache_key_derivations()
}

/// The cacheable prefix of a real request: the static system prompt plus the
/// tool catalog, about 80 KB serialized, the size the pre-memo measurement
/// priced.
fn large_cacheable_prefix() -> (String, Vec<ToolDefinition>) {
    let mut system = String::new();
    for section in 0..110 {
        system.push_str(&format!(
            "## Section {section}\n\
             Keep the established conventions for this area. A request that changes behaviour must name the \
             invariant it preserves, and a test that covers it must fail before the change and pass after. \
             Prefer the smallest edit that removes the cause, and leave the surrounding style untouched. \
             When two callers need the same behaviour, move it beside the data they share instead of copying \
             it, and delete the copy rather than leaving both. State the reason a fallback exists in the code \
             that takes it, because a reader six months later cannot recover it from the call site alone.\n"
        ));
    }
    let tools = (0..7)
        .map(|index| {
            let mut properties = serde_json::Map::new();
            for property in 0..6 {
                properties.insert(
                    format!("argument_{property}"),
                    json!({
                        "type": "string",
                        "description": format!(
                            "Argument {property} of tool {index}. Supply the value the previous step reported. \
                             The tool rejects a path outside the workspace, a pattern that matches nothing, and \
                             a range whose start is after its end."
                        ),
                    }),
                );
            }
            ToolDefinition::new(
                format!("tool_{index}"),
                format!(
                    "Tool number {index}. It performs one measured action over the workspace and returns a \
                     structured result. Prefer this tool over running the equivalent command by hand: it \
                     records the action in the session log, redacts secrets before writing output, and refuses \
                     a target that would leave the repository. A failure returns the reason instead of an \
                     empty result. "
                )
                .repeat(2),
                json!({
                    "type": "object",
                    "properties": properties,
                    "required": ["argument_0"],
                    "additionalProperties": false,
                }),
            )
        })
        .collect();
    (system, tools)
}

/// The conversation below the prefix: completed turns whose tool results sit at
/// the 30 KB output cap, the size a mid-session request reaches.
fn conversation_turns(turns: usize) -> Vec<Message> {
    let mut messages = Vec::new();
    for turn in 0..turns {
        messages.push(Message::user(&format!("Turn {turn}: check the fixture.")));
        messages.push(Message::assistant_text(&format!(
            "Turn {turn} ran the checks; the output follows."
        )));
        let output = format!("turn {turn} output line\n").repeat(1_500);
        messages.push(Message::tool_result(&format!("call_{turn}"), &output, false));
    }
    messages
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|left, right| left.total_cmp(right));
    samples[samples.len() / 2]
}

fn micros<F: FnMut()>(mut run: F) -> f64 {
    let start = Instant::now();
    run();
    start.elapsed().as_secs_f64() * 1e6
}

/// The memo exists to make a steady-state turn cheap next to the work every
/// request already does, so this prices the key against serializing the body the
/// request sends. The fixture is a mid-session request: the ~80 KB cacheable
/// prefix the pre-memo measurement priced, plus eight completed turns with tool
/// results at the 30 KB output cap.
#[test]
#[ignore = "release timing check: run with --release --ignored --nocapture"]
fn prompt_cache_memo_steady_state_beats_body_serialization() {
    let provider = openai_provider("gpt-5.1");
    let (system, tools) = large_cacheable_prefix();
    let context = cache_context("root-timing");
    let messages = conversation_turns(8);

    let body = build_body(&provider, &messages, &tools, &system, Some(&context));
    let body_bytes = serde_json::to_vec(&body).expect("body serializes");
    let prefix_bytes = system.len() + serde_json::to_vec(&body["tools"]).expect("tools serialize").len();
    assert!(
        (70_000..90_000).contains(&prefix_bytes),
        "the fixture must price the ~80 KB prefix the pre-memo cost was measured on, got {prefix_bytes} bytes"
    );

    let first_us = median(
        (0..10)
            .map(|_| {
                let fresh = openai_provider("gpt-5.1");
                micros(|| {
                    std::hint::black_box(fresh.prompt_cache_key_for("root-timing", &body));
                })
            })
            .collect(),
    );
    let steady_us = median(
        (0..200)
            .map(|_| {
                micros(|| {
                    std::hint::black_box(provider.prompt_cache_key_for("root-timing", &body));
                })
            })
            .collect(),
    );
    let body_us = median(
        (0..200)
            .map(|_| {
                micros(|| {
                    std::hint::black_box(serde_json::to_vec(&body).expect("body serializes"));
                })
            })
            .collect(),
    );

    println!(
        "prompt_cache_key: prefix={prefix_bytes} bytes body={} bytes first_us={first_us:.3} steady_us={steady_us:.3} body_us={body_us:.3} ratio={:.4}",
        body_bytes.len(),
        steady_us / body_us
    );
    assert!(
        steady_us / body_us < 0.05,
        "steady-state key cost {steady_us:.3} us is not below 5% of body serialization {body_us:.3} us"
    );
    assert!(
        steady_us < first_us / 10.0,
        "a memo hit ({steady_us:.3} us) must be far cheaper than a fresh derivation ({first_us:.3} us)"
    );
}
